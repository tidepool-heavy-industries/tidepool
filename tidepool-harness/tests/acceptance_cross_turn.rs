//! Cross-turn binding persistence on a single node: a declaration turn
//! accumulates into the node's `Tidepool.Session.Lib.G<g>` module, and a
//! later expression turn compiles session-aware (importing that module), so
//! a prior turn's declaration resolves as a live binding.
//!
//! Drives that through the REAL turn path (`drive_turn` → `run_block`) via
//! the record-replay provider: turn 1 declares `steps`, turn 2 (a follow-up)
//! computes from `steps`. The multi-block test packs the same story into ONE
//! reply (every ```haskell block runs, in order) and pins the
//! failure-mid-sequence contract.
//!
//! GHC-heavy tier: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::{NodeId, NodeState};
use tidepool_harness::{Harness, TurnOutcome};

fn prelude_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| PathBuf::from("haskell/lib"))
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "cross-turn".into(),
        extract_fingerprint: "cross-turn".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 100,
        output_tokens: 20,
        cached_input_tokens: None,
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: content.to_string(),
        usage: usage(),
    }
}

/// Turn 1 DECLARES `steps`; turn 2 (a follow-up) COMPUTES from it. The value
/// only survives if the node accumulated the declaration across turns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declaration_persists_into_the_next_turn() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("cross_turn.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![
        // Turn 1: a top-level declaration (a value binding). Classified as a
        // decl, it accumulates on the node's decl plane instead of running as an
        // expression.
        reply("I'll record the steps.\n\n```haskell\nsteps = [1, 2, 3] :: [Int]\n```"),
        // Turn 2: an expression that references the prior turn's `steps`. Only
        // resolves if turn 1's declaration persisted.
        reply("Now I'll sum them.\n\n```haskell\npure (toJSON (sum steps))\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("cross-turn root", "Record steps, then use them.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    // Turn 1: the declaration turn completes (nothing to render — it declared).
    let turn1 = harness
        .run_to_hole_or_done(root)
        .await
        .expect("turn 1 (declaration) drives to completion");
    assert!(
        matches!(turn1, TurnOutcome::Completed { .. }),
        "the declaration turn should complete, got {}",
        outcome_tag(&turn1)
    );
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));

    // Turn 2: reopen the node and reference `steps` — it must resolve as a live
    // binding carried over from turn 1.
    let turn2 = harness
        .follow_up(root, "Use the steps you recorded.")
        .await
        .expect("turn 2 (reference) drives to completion");
    match turn2 {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains('6'),
                "turn 2 must compute sum([1,2,3]) = 6 from the persisted `steps`, got: {rendered}"
            );
        }
        other => panic!(
            "turn 2 should complete with the summed value, got {}",
            outcome_tag(&other)
        ),
    }
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));
}

/// The multi-block contract on the REAL turn path: every ```haskell block in
/// one reply runs, in order, as one sequence (decl block feeding the expr
/// block — the packing that previously cost a model round per block); a
/// failure mid-sequence keeps the blocks that ran (the failed reply's OWN
/// decl block persists into the corrective retry) and feeds back a
/// sequence-context corrective naming the failed block and the resume point.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_block_reply_runs_in_order_and_fails_with_resume_point() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("multi_block.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![
        // Reply 1: TWO blocks — a declaration block, then an expression block
        // using it. One model round, both run.
        reply(
            "Declare, then use:\n\n```haskell\nnums = [1, 2, 3] :: [Int]\n\
             double x = x * (2 :: Int)\n```\n\n\
             ```haskell\npure (toJSON (sum (map double nums)))\n```",
        ),
        // Reply 2 (follow-up): block 1 declares; block 2 fails to compile.
        // Block 1 must persist despite block 2's failure.
        reply(
            "```haskell\ntripled = [3, 6, 9] :: [Int]\n```\n\n\
             ```haskell\npure (toJSON (thisNameDoesNotExist))\n```",
        ),
        // Reply 3: the corrective retry — references reply 2's SURVIVING
        // block-1 declaration. Only resolves if the failed sequence kept it.
        reply("```haskell\npure (toJSON (sum tripled))\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root(
            "multi-block root",
            "Pack a decl and its use into one reply.",
        )
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    // Reply 1: both blocks run in one drive — the sequence completes with the
    // LAST block's value, computed through the FIRST block's declarations.
    let turn1 = harness
        .run_to_hole_or_done(root)
        .await
        .expect("multi-block turn drives to completion");
    match &turn1 {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains("12"),
                "sum (map double nums) = 12 must come from the same reply's decl block, got: {rendered}"
            );
        }
        other => panic!("expected Completed, got {}", outcome_tag(other)),
    }
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));

    // Replies 2+3: block 2 of reply 2 fails; the corrective retry (reply 3)
    // computes from reply 2's surviving block-1 declaration.
    let turn2 = harness
        .follow_up(root, "Now triple them.")
        .await
        .expect("corrective retry drives to completion");
    match &turn2 {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains("18"),
                "sum tripled = 18 must come from the FAILED reply's surviving decl block, got: {rendered}"
            );
        }
        other => panic!("expected Completed, got {}", outcome_tag(other)),
    }

    // The corrective user turn carried the sequence context: which block
    // failed, what ran and persists, and where to resume.
    let (_hdr, events) = LogReader::open(&log_path).unwrap();
    let corrective = events
        .filter_map(|e| match e.expect("readable log record").event {
            Event::TurnDelta {
                role: tidepool_harness::provider::Role::User,
                content,
                ..
            } => Some(content),
            _ => None,
        })
        .find(|c| c.contains("Block 2 of 2 failed"))
        .expect("a corrective user turn carries the sequence-failure context");
    assert!(
        corrective.contains("block 1 (tripled = [3, 6, 9] :: [Int])"),
        "the corrective names the surviving block: {corrective}"
    );
    assert!(
        corrective.contains("Continue from block 2"),
        "the corrective states the resume point: {corrective}"
    );
}

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}
