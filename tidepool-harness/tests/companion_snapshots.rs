//! PRD 21 lane C2 §4 — frozen post-coalgebra context snapshots: digest
//! identity over the exact prefix `assemble_request` re-emits, branches that
//! share that prefix BYTE-STABLY, snapshot immutability under compaction, and
//! receipts that record the provider cache-metric gap instead of faking it.
//!
//! # Why byte-stability is ASSERTED, not inspected
//!
//! The claim "children forked from one snapshot share the frozen prefix
//! byte-stably" is only worth anything if something recomputes it. So
//! [`siblings_share_one_frozen_prefix_byte_stably`] takes each CHILD's own
//! live context, runs it through [`engine::assemble_request`] — the same
//! function that builds the request a provider is actually sent — and
//! re-digests the first `|prefix|` messages of the result. That digest must
//! equal the parent's frozen digest. Nothing here reads a stored copy of the
//! parent's prefix and compares it to itself.
//!
//! # This suite pays NO extract compile
//!
//! Every replayed reply is PROSE — no fenced `haskell` block — so
//! `Harness::drive_turn` returns `NoBlock` before it ever compiles or touches
//! a session machine, and `Harness::force` is lazy by contract
//! (`tests/acceptance_lazy_boot.rs` pins that). Freezing, forking, and
//! compacting are all transcript-level operations. That is deliberate: this
//! file is about context identity, and the suite's wall-clock budget should
//! not pay a GHC compile for an assertion about bytes. `support::isolate_cache`
//! is still called — the harness writes run dirs and a KV path under the cache
//! home — but `support::require_extract` is not, because a test that needs no
//! extract must not claim it does.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_harness::engine::{self, EngineConfig};
use tidepool_harness::log::{Actor, Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Message, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::snapshot::{digest_messages, SnapshotDigest};
use tidepool_harness::tree::NodeId;
use tidepool_harness::Harness;

fn prelude_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .join("haskell/lib")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "companion-snapshots".into(),
        extract_fingerprint: "companion-snapshots".into(),
        harness_version: "test".into(),
    }
}

/// A PROSE reply — no fenced haskell block, so the turn is `NoBlock` and no
/// compile happens. See this module's docs.
fn prose(content: &str) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: content.to_string(),
        usage: Usage {
            input_tokens: 100,
            output_tokens: 20,
            // The whole point of the receipt assertion below: the replay
            // provider reports NOTHING about caching, so this stays `None`.
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

struct Fixture {
    harness: Arc<Harness>,
    log_path: PathBuf,
    _dir: tempfile::TempDir,
    _cache: tempfile::TempDir,
}

fn fixture(replies: Vec<RecordedReply>) -> Fixture {
    let cache = support::isolate_cache();
    let dir = tempfile::tempdir().expect("scratch dir");
    let log_path = dir.path().join("snapshots.jsonl");
    let writer = LogWriter::create(&log_path, &header()).expect("log writer");
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));
    Fixture {
        harness,
        log_path,
        _dir: dir,
        _cache: cache,
    }
}

fn events(path: &std::path::Path) -> Vec<Event> {
    let (_header, iter) = LogReader::open(path).expect("log opens");
    iter.map(|r| r.expect("event parses").event).collect()
}

/// Re-derive the digest of `node`'s OWN assembled request prefix, through the
/// production assembly path, for its first `transcript_len` transcript
/// messages (plus the system message assembly prepends). This is the
/// independent recomputation the byte-stability claim rests on.
fn redigest_assembled_prefix(
    harness: &Harness,
    node: NodeId,
    transcript_len: usize,
) -> SnapshotDigest {
    let (transcript, framing) = harness
        .node_context(node)
        .expect("node has a live convo (forced, not terminated)");
    let assembled: Vec<Message> =
        engine::assemble_request(&transcript, None, framing.as_deref()).messages;
    // +1: `assemble_request` is `[system(framing)] ++ transcript`, so the
    // frozen prefix of N transcript messages is N+1 assembled messages.
    digest_messages(&assembled[..transcript_len + 1])
}

/// Drive `node` one prose turn, so its transcript grows by a real
/// assistant message through the production turn path.
async fn prose_turn(harness: &Harness, node: NodeId) {
    let outcome = harness.drive_turn(node).await.expect("prose turn drives");
    assert!(
        matches!(outcome, tidepool_harness::TurnOutcome::NoBlock { .. }),
        "this suite's replies are prose by design — a turn that reached a \
         compile means the fixture grew a fenced haskell block"
    );
}

/// Freeze a root's prefix, fork three siblings off it, and prove — by
/// recomputation, not inspection — that each sibling's own assembled request
/// prefix hashes to the parent's frozen digest, and that every sibling
/// reports that same parent digest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn siblings_share_one_frozen_prefix_byte_stably() {
    let fx = fixture(vec![prose("Understood — surveying the ground.")]);
    let harness = fx.harness.clone();

    let root = harness
        .create_root("snapshot root", "Establish the shared context.")
        .expect("create root");
    harness.force(root, Actor::Operator).expect("force root");
    prose_turn(&harness, root).await;

    let digest = harness.freeze_snapshot(root).expect("freeze");
    let snap = harness.snapshot(&digest).expect("digest resolves");
    assert_eq!(
        snap.messages.len(),
        2,
        "the frozen prefix is the opening user turn plus the assistant reply"
    );
    assert_eq!(snap.digest, digest);

    let briefs = ["Branch A: take the left fork.", "Branch B.", "C — third."];
    let children: Vec<NodeId> = briefs
        .iter()
        .map(|b| harness.fork_from_snapshot(&digest, b).expect("fork"))
        .collect();

    for child in &children {
        assert_eq!(
            harness.branch_snapshot(*child),
            Some(digest.clone()),
            "every sibling reports the SAME parent snapshot digest"
        );
        harness.force(*child, Actor::Operator).expect("force child");
    }

    // The acceptance: each child's OWN assembled prefix, recomputed through
    // `engine::assemble_request`, digests to the parent's frozen digest.
    for child in &children {
        assert_eq!(
            redigest_assembled_prefix(&harness, *child, snap.messages.len()),
            digest,
            "child {child:?}'s assembled prefix must be byte-stable with the frozen root"
        );
    }

    // The parent's own next request still re-emits the same prefix, so the
    // digest describes the shared root, not a child-only artifact.
    assert_eq!(
        redigest_assembled_prefix(&harness, root, snap.messages.len()),
        digest
    );

    // The branches genuinely DIVERGE past the prefix — otherwise the equality
    // above would be trivially true of identical transcripts.
    let suffixes: Vec<String> = children
        .iter()
        .map(|c| {
            let (transcript, _) = harness.node_context(*c).expect("live convo");
            assert_eq!(
                transcript.len(),
                snap.messages.len() + 1,
                "a branch is the frozen prefix plus exactly its own brief"
            );
            transcript[snap.messages.len()].content.clone()
        })
        .collect();
    assert_eq!(suffixes, briefs, "each branch carries its own brief");

    // One freeze, one receipt, naming the prefix it actually froze.
    let frozen: Vec<Event> = events(&fx.log_path)
        .into_iter()
        .filter(|e| matches!(e, Event::SnapshotFrozen { .. }))
        .collect();
    assert_eq!(frozen.len(), 1, "one freeze, one receipt");
    assert_eq!(
        frozen[0],
        Event::SnapshotFrozen {
            node: root,
            digest: digest.clone(),
            messages: snap.messages.len() as u64,
            prefix_bytes: snap.prefix_bytes(),
        }
    );
}

/// Freezing is idempotent: an unchanged transcript freezes to the same digest,
/// interns no second entry, and writes no second receipt. A CHANGED transcript
/// is a different cache root, and the old one keeps resolving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn freeze_is_idempotent_and_a_changed_transcript_is_a_new_root() {
    let fx = fixture(vec![prose("First."), prose("Second.")]);
    let harness = fx.harness.clone();

    let root = harness
        .create_root("idempotence", "Begin.")
        .expect("create root");
    harness.force(root, Actor::Operator).expect("force");
    prose_turn(&harness, root).await;

    let first = harness.freeze_snapshot(root).expect("freeze");
    let again = harness.freeze_snapshot(root).expect("re-freeze");
    assert_eq!(
        first, again,
        "an unchanged transcript freezes to one digest"
    );
    assert!(Arc::ptr_eq(
        &harness.snapshot(&first).expect("resolves"),
        &harness.snapshot(&again).expect("resolves"),
    ));

    let after_first = events(&fx.log_path)
        .iter()
        .filter(|e| matches!(e, Event::SnapshotFrozen { .. }))
        .count();
    assert_eq!(
        after_first, 1,
        "a repeat freeze is a lookup — it must not duplicate the receipt"
    );

    // A real turn changes the transcript, so the next freeze is a NEW root.
    prose_turn(&harness, root).await;
    let third = harness.freeze_snapshot(root).expect("freeze after a turn");
    assert_ne!(
        third, first,
        "a changed transcript is a different cache root"
    );
    assert!(
        harness.snapshot(&first).is_some(),
        "the older root still resolves — snapshots are never evicted"
    );
    assert_eq!(
        harness.snapshot(&first).expect("resolves").messages.len() + 1,
        harness.snapshot(&third).expect("resolves").messages.len(),
        "the newer root carries the one additional assistant turn"
    );
    assert_eq!(
        events(&fx.log_path)
            .iter()
            .filter(|e| matches!(e, Event::SnapshotFrozen { .. }))
            .count(),
        2
    );
}

/// PRD 21 locked decision 2, pinned: compaction of a node that has a frozen
/// snapshot mints a NEW cache root and leaves the existing one — and every
/// child already forked from it — completely untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compaction_mints_a_new_cache_root_and_leaves_children_untouched() {
    let fx = fixture(vec![prose("Groundwork laid.")]);
    let harness = fx.harness.clone();

    let root = harness
        .create_root("compaction root", "Establish the shared context.")
        .expect("create root");
    harness.force(root, Actor::Operator).expect("force");
    prose_turn(&harness, root).await;

    let original = harness.freeze_snapshot(root).expect("freeze");
    let snap = harness.snapshot(&original).expect("resolves");
    let prefix_len = snap.messages.len();

    let left = harness
        .fork_from_snapshot(&original, "Left branch.")
        .expect("fork left");
    let right = harness
        .fork_from_snapshot(&original, "Right branch.")
        .expect("fork right");
    for c in [left, right] {
        harness.force(c, Actor::Operator).expect("force child");
    }
    let before: Vec<(Vec<Message>, Option<String>)> = [left, right]
        .iter()
        .map(|c| harness.node_context(*c).expect("live convo"))
        .collect();

    // Compaction: destructive to the PARENT's live transcript, by design.
    harness
        .replace_transcript_with_summary(root, "Everything above, in one line.")
        .expect("compaction");

    // 1. The original root still resolves, byte for byte.
    let still = harness
        .snapshot(&original)
        .expect("the original cache root still resolves after compaction");
    assert_eq!(
        still.messages.as_ref(),
        snap.messages.as_ref(),
        "a frozen prefix is immutable — compaction must not have rewritten it"
    );
    assert_eq!(still.digest, original);

    // 2. Both children still report it, and their transcripts are unchanged.
    for (c, was) in [left, right].iter().zip(before.iter()) {
        assert_eq!(harness.branch_snapshot(*c), Some(original.clone()));
        let now = harness.node_context(*c).expect("live convo");
        assert_eq!(
            &now, was,
            "a child's transcript is not touched by the parent's compaction"
        );
        assert_eq!(
            redigest_assembled_prefix(&harness, *c, prefix_len),
            original,
            "and it still re-digests to the ORIGINAL root"
        );
    }

    // 3. Compaction itself minted the new root — no second explicit freeze
    //    needed — and re-freezing the parent now agrees with it.
    let after = harness
        .freeze_snapshot(root)
        .expect("freeze after compaction");
    assert_ne!(
        after, original,
        "compaction mints a NEW cache root, not a rewritten one"
    );
    assert_eq!(
        harness.snapshot(&after).expect("resolves").messages.len(),
        1,
        "the compacted context is the single summary message"
    );
    let frozen_digests: Vec<SnapshotDigest> = events(&fx.log_path)
        .into_iter()
        .filter_map(|e| match e {
            Event::SnapshotFrozen { digest, .. } => Some(digest),
            _ => None,
        })
        .collect();
    assert_eq!(
        frozen_digests,
        vec![original, after],
        "two receipts: the original root, then the one compaction minted \
         (the post-compaction re-freeze is idempotent and adds none)"
    );
}

/// The receipt contract, including the one thing that must NOT be faked: the
/// `ReplayProvider` reports no cache metric, so `cached_input_tokens` is
/// absent — never `0` dressed up as a measurement.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branch_invocation_records_bytes_and_an_absent_cache_metric() {
    let fx = fixture(vec![
        prose("Root turn."),
        prose("Branch turn."),
        // A SECOND branch turn: the receipt must not fire again on it.
        prose("Branch turn two."),
    ]);
    let harness = fx.harness.clone();

    let root = harness
        .create_root("receipt root", "Establish the shared context.")
        .expect("create root");
    harness.force(root, Actor::Operator).expect("force");
    prose_turn(&harness, root).await;

    let digest = harness.freeze_snapshot(root).expect("freeze");
    let snap = harness.snapshot(&digest).expect("resolves");
    let brief = "Take it from here, branch.";
    let branch = harness
        .fork_from_snapshot(&digest, brief)
        .expect("fork branch");
    harness
        .force(branch, Actor::Operator)
        .expect("force branch");
    prose_turn(&harness, branch).await;

    let log = events(&fx.log_path);

    let (node, snapshot, shared, suffix, input, cached) = log
        .iter()
        .find_map(|e| match e {
            Event::BranchInvocation {
                node,
                snapshot,
                shared_prefix_bytes,
                branch_suffix_bytes,
                input_tokens,
                cached_input_tokens,
            } => Some((
                *node,
                snapshot.clone(),
                *shared_prefix_bytes,
                *branch_suffix_bytes,
                *input_tokens,
                *cached_input_tokens,
            )),
            _ => None,
        })
        .expect("the branch's first turn wrote a BranchInvocation receipt");

    assert_eq!(node, branch);
    assert_eq!(snapshot, digest, "the receipt names the frozen parent root");
    assert_eq!(
        shared,
        snap.prefix_bytes(),
        "shared-prefix bytes are the frozen prefix's, exactly"
    );
    assert_eq!(
        suffix,
        brief.len() as u64,
        "branch-suffix bytes are what THIS branch added past the prefix"
    );
    assert!(shared > 0 && suffix > 0);
    assert_eq!(
        input, 100,
        "the provider's OWN input token count, unaltered"
    );

    // The anti-pattern this pins: not reported must stay not reported.
    assert_eq!(
        cached, None,
        "the ReplayProvider reports no cache metric — the receipt must say \
         NOTHING, never 0-as-if-measured"
    );
    let line = log
        .iter()
        .find(|e| matches!(e, Event::BranchInvocation { .. }))
        .map(|e| serde_json::to_string(e).expect("event serializes"))
        .expect("receipt present");
    assert!(
        !line.contains("cached_input_tokens"),
        "an unreported cache metric is ABSENT on the wire, not zero: {line}"
    );

    // One receipt per branch, at its FIRST turn only.
    prose_turn(&harness, branch).await;
    assert_eq!(
        events(&fx.log_path)
            .iter()
            .filter(|e| matches!(e, Event::BranchInvocation { .. }))
            .count(),
        1,
        "the branch-invocation receipt is one-shot"
    );
    assert_eq!(
        harness.branch_snapshot(branch),
        Some(digest),
        "the branch keeps reporting its root after the receipt is written"
    );
}

/// A `Usage` written before `cached_input_tokens` existed still deserializes —
/// the additive-widening contract that keeps existing `log.jsonl` readable.
#[test]
fn a_pre_widening_usage_still_deserializes() {
    let old: Usage = serde_json::from_str(r#"{"input_tokens":120,"output_tokens":8}"#)
        .expect("a pre-widening Usage still deserializes");
    assert_eq!(old.input_tokens, 120);
    assert_eq!(old.output_tokens, 8);
    assert_eq!(
        old.cached_input_tokens, None,
        "a log that never recorded a cache metric reports NONE, not zero"
    );

    // And a whole pre-widening TurnDelta line, which is how it actually
    // appears on disk.
    let line = r#"{"ev":"turn_delta","node":0,"turn":1,"role":"assistant","content":"hi","usage":{"input_tokens":120,"output_tokens":8}}"#;
    let ev: Event = serde_json::from_str(line).expect("a pre-widening log line still deserializes");
    match ev {
        Event::TurnDelta { usage, .. } => {
            assert_eq!(usage.expect("usage present").cached_input_tokens, None)
        }
        other => panic!("expected TurnDelta, got {other:?}"),
    }
}

/// A digest this harness never minted is a typed refusal, not a panic or a
/// silently-empty branch.
#[test]
fn forking_from_an_unknown_digest_is_a_typed_error() {
    let fx = fixture(Vec::new());
    let err = fx
        .harness
        .fork_from_snapshot(&SnapshotDigest("deadbeef".to_string()), "nope")
        .expect_err("an unminted digest must be refused");
    assert!(
        matches!(
            err,
            tidepool_harness::HarnessError::UnknownSnapshot(ref d) if d.as_str() == "deadbeef"
        ),
        "expected UnknownSnapshot, got {err}"
    );
}
