//! B2 widen: the elaboration flow (F1's exception path) for an operator
//! `dialogAsk` hole. Record-replay, CI-shaped, zero live calls — same
//! production-path discipline as `golden_path.rs` / `acceptance_return_control.rs`.
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH
//! (`--ignore-default-filter` to run; see `tests/golden_path.rs` for the env
//! recipe).
//!
//! Coverage (plans/harness-r0/WIDEN.md §B2):
//!   - a dialog submission with NON-EMPTY PROSE routes to the calling model
//!     as elaborator instead of passing the raw submission through as the
//!     resume Value (the R0 spike's stubbed behavior) — the elaborator's
//!     first attempt is deliberately ill-typed (`resume (42 :: Int)` against
//!     `resume :: Value -> M Value`), exercising the GHC-verbatim retry, same
//!     discipline a fork/return-control answerer uses.
//!   - CONFIRM: the corrected proposal is run and resumes the hole; the node
//!     completes.
//!   - REJECT: a staged proposal is discarded WITHOUT running it — the hole
//!     stays exactly `Suspended` on the SAME hole id, and a subsequent
//!     MECHANICAL answer (empty prose + known key) still resumes it to
//!     completion, proving the continuation was untouched by the rejection.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, AnswerOutcome, Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::{NodeId, NodeState};
use tidepool_harness::{Harness, HoleRouting, TurnOutcome};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn prelude_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| PathBuf::from("haskell/lib"))
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "elaboration".into(),
        extract_fingerprint: "elaboration".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 60,
        output_tokens: 15,
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

/// The one-hole root program every test in this file shares: a single
/// `dialogAsk` confirm, then completion. No fork, no return-control — the
/// ONLY suspension is the operator-routed Dialog hole B2 targets.
fn root_turn() -> RecordedReply {
    reply(
        "I'll confirm with the operator, then finish.\n\n\
         ```haskell\n\
         do\n\
         \x20 _ <- dialogAsk (toJSON (card \"Confirm\" [choice \"Proceed?\" \
         [(\"yes\", \"Yes\"), (\"no\", \"No\")]]))\n\
         \x20 pure (toJSON (1 :: Int))\n\
         ```",
    )
}

fn events_for(log_path: &std::path::Path, node: NodeId) -> Vec<Event> {
    let (_h, events) = LogReader::open(log_path).expect("open log");
    events
        .map(|r| r.expect("well-formed record").event)
        .filter(|e| event_node(e) == Some(node))
        .collect()
}

fn event_node(e: &Event) -> Option<NodeId> {
    match e {
        Event::NodeCreated { node, .. }
        | Event::Forced { node, .. }
        | Event::TurnStart { node, .. }
        | Event::Effect { node, .. }
        | Event::HolePublished { node, .. }
        | Event::HoleAnswerAttempt { node, .. }
        | Event::HoleConsumed { node, .. }
        | Event::NodeDone { node, .. }
        | Event::NodeCancelled { node, .. }
        | Event::TurnDelta { node, .. }
        | Event::TurnForked { node, .. }
        | Event::TurnSpliced { node, .. } => Some(*node),
    }
}

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

/// Force the root and drive it to its (only) Dialog hole.
async fn force_to_dialog_hole(harness: &Arc<Harness>, root: NodeId) {
    harness.force(root, Actor::Operator).unwrap();
    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to a hole");
    match &outcome {
        TurnOutcome::Suspended { classified, .. } => {
            assert!(
                matches!(classified.routing, HoleRouting::Dialog { .. }),
                "root should suspend on a Dialog hole, got {:?}",
                classified.routing
            );
        }
        other => panic!(
            "root should suspend at dialogAsk, got a different outcome: {}",
            outcome_tag(other)
        ),
    }
}

/// A non-empty-prose submission routes to elaboration (NOT the raw-submission
/// pass-through the R0 spike stubbed) — the elaborator's ill-typed first
/// attempt retries GHC-verbatim, the corrected one is STAGED (not consumed),
/// and `confirm_proposal` is what actually resumes the hole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prose_submission_elaborates_then_confirm_consumes() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("elaboration-confirm.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![
        root_turn(),
        // Deliberately ill-typed: an Int where the elaborator's fixed
        // `resume :: Value -> M Value` wants a Value.
        reply("```haskell\nresume (42 :: Int)\n```"),
        // Corrected.
        reply("Right, it must be a Value.\n\n```haskell\nresume (toJSON True)\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("elaboration confirm", "Confirm, finish.")
        .unwrap();
    force_to_dialog_hole(&harness, root).await;

    // A non-empty-prose submission — must NOT pass through as the raw resume
    // Value (the old stub behavior); no proposal is staged yet.
    assert!(harness.pending_proposal_source(root).is_none());
    harness
        .answer_dialog(
            root,
            json!({ "values": {}, "prose": "please just approve it" }),
        )
        .await
        .expect("elaboration produces a staged proposal");

    // The node is NOT done and NOT consumed — the proposal is merely staged.
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));
    let proposal = harness
        .pending_proposal_source(root)
        .expect("a GHC-valid proposal is staged, shown before consume");
    assert!(
        proposal.contains("toJSON"),
        "the staged proposal is the corrected elaborator expr, got: {proposal}"
    );

    // CONFIRM: runs the already-validated expr and resumes the hole.
    harness
        .confirm_proposal(root)
        .await
        .expect("confirm runs the staged proposal and resumes the hole");
    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the program completes once the confirmed proposal resumes the hole"
    );
    assert!(
        harness.pending_proposal_source(root).is_none(),
        "the proposal is cleared once confirmed"
    );

    // --- durable-log assertions -------------------------------------------
    let events = events_for(&log_path, root);

    let published: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::HolePublished { .. }))
        .collect();
    assert_eq!(
        published.len(),
        1,
        "exactly one hole is ever published — elaboration must not re-publish or mint a new hole"
    );

    let attempts: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::HoleAnswerAttempt { .. }))
        .collect();
    assert_eq!(
        attempts.len(),
        3,
        "one Rejected (ill-typed elaborator attempt), one Proposed, one Consumed, got {attempts:?}"
    );
    assert!(matches!(
        attempts[0],
        Event::HoleAnswerAttempt {
            outcome: AnswerOutcome::Rejected { .. },
            ..
        }
    ));
    let Event::HoleAnswerAttempt {
        outcome: AnswerOutcome::Proposed { source },
        ..
    } = attempts[1]
    else {
        panic!("second attempt must be Proposed, got {:?}", attempts[1]);
    };
    assert!(source.contains("toJSON"));
    assert!(matches!(
        attempts[2],
        Event::HoleAnswerAttempt {
            outcome: AnswerOutcome::Consumed,
            ..
        }
    ));

    let consumed: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::HoleConsumed { .. }))
        .collect();
    assert_eq!(consumed.len(), 1, "hole consumed exactly once, by confirm");

    assert!(events.iter().any(|e| matches!(e, Event::NodeDone { .. })));

    // All 3 scripted replies were consumed (root turn + the two elaborator
    // attempts) — the mechanical path never enters into this test.
    let replay = ReplayProvider::from_log(&log_path).expect("replay from log");
    assert_eq!(replay.remaining(), 3);
}

/// Rejecting a staged proposal discards it WITHOUT running it — the hole
/// stays exactly `Suspended` on the SAME id, and the continuation is provably
/// untouched: a subsequent MECHANICAL answer (empty prose + known key) still
/// resumes it to completion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reject_discards_without_running_and_leaves_continuation_untouched() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("elaboration-reject.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![
        root_turn(),
        // Valid on the first attempt — reject must discard this WITHOUT
        // ever running it (if it ran, the node would complete; it must not).
        reply("```haskell\nresume (toJSON False)\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("elaboration reject", "Confirm, finish.")
        .unwrap();
    force_to_dialog_hole(&harness, root).await;
    let hole_id_before = match harness.tree().state(root) {
        Some(NodeState::Suspended { hole }) => hole,
        other => panic!("root should be Suspended, got {other:?}"),
    };

    harness
        .answer_dialog(root, json!({ "values": {}, "prose": "sure, go for it" }))
        .await
        .expect("elaboration produces a staged proposal");
    assert!(harness.pending_proposal_source(root).is_some());

    harness
        .reject_proposal(root)
        .expect("reject discards the staged proposal");

    // Untouched: same hole, still Suspended, no completion.
    assert!(
        harness.pending_proposal_source(root).is_none(),
        "the rejected proposal is cleared"
    );
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));
    let hole_after_reject = harness
        .pending_hole(root)
        .expect("root is still suspended after the rejection");
    assert!(
        matches!(&hole_after_reject.routing, HoleRouting::Dialog { .. }),
        "the SAME dialog hole remains pending after a reject"
    );
    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Suspended {
            hole: hole_id_before.clone()
        }),
        "the exact same hole id survives the reject"
    );

    // The continuation is genuinely intact: a fresh MECHANICAL answer (empty
    // prose + known key — the path B2 must not change) still resumes it.
    harness
        .answer_dialog(root, json!({ "values": { "yes": true }, "prose": "" }))
        .await
        .expect("mechanical answer resumes the untouched continuation");
    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the program completes via the mechanical answer after the reject"
    );

    // --- durable-log assertions -------------------------------------------
    let events = events_for(&log_path, root);

    let published: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::HolePublished { .. }))
        .collect();
    assert_eq!(published.len(), 1, "the hole is never re-published across the reject");
    let Event::HolePublished {
        hole: published_hole,
        ..
    } = published[0]
    else {
        unreachable!()
    };
    assert_eq!(published_hole.0, hole_id_before.0);

    let attempts: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::HoleAnswerAttempt { .. }))
        .collect();
    // `resume_parent` is the SOLE logger of a Consumed attempt (invariant: one
    // successful resume, one Consumed record — `answer_dialog`'s mechanical
    // branch no longer logs its own, so the follow-up mechanical answer here
    // logs exactly one). So: Proposed, ProposalDiscarded, then the single
    // Consumed from the follow-up mechanical answer.
    assert_eq!(
        attempts.len(),
        3,
        "one Proposed, one ProposalDiscarded, one Consumed, got {attempts:?}"
    );
    assert!(matches!(
        attempts[0],
        Event::HoleAnswerAttempt {
            outcome: AnswerOutcome::Proposed { .. },
            ..
        }
    ));
    assert!(matches!(
        attempts[1],
        Event::HoleAnswerAttempt {
            outcome: AnswerOutcome::ProposalDiscarded,
            ..
        }
    ));
    assert!(matches!(
        attempts[2],
        Event::HoleAnswerAttempt {
            outcome: AnswerOutcome::Consumed,
            ..
        }
    ));

    let consumed: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::HoleConsumed { .. }))
        .collect();
    assert_eq!(
        consumed.len(),
        1,
        "the hole consumes exactly once — the rejected proposal must never consume it"
    );

    // Only 2 assistant turns were ever recorded (root + the one elaborator
    // turn) — the mechanical follow-up answer costs no model turn.
    let replay = ReplayProvider::from_log(&log_path).expect("replay from log");
    assert_eq!(replay.remaining(), 2);
}
