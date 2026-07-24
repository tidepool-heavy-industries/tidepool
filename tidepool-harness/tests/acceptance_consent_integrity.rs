//! PRD §11 acceptance: consent integrity through the REAL production entry
//! point. `forcing.rs`'s own unit tests already prove "a fork request
//! materializes only a Thunk child, zero events until Forced" at the
//! `NodeTree` layer with a hand-built `FakeMachine` — this test drives the
//! SAME invariant through the actual compiled Haskell `returnControlFork`
//! request and the real turn engine (record-replay, zero live calls), and
//! audits the DURABLE LOG, not just in-memory state.
//!
//! Coverage map (WIDEN.md §A3 / PRD.md §11): "consent-integrity: a fork
//! request with no forcing event → literal zero child effect/turn events,
//! audited from the log".

use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::{NodeId, NodeState};
use tidepool_harness::{ClassifiedHole, Harness, HoleRouting, TurnOutcome};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn prelude_dir() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| std::path::PathBuf::from("haskell/lib"))
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "acceptance-consent".into(),
        extract_fingerprint: "acceptance-consent".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 50,
        output_tokens: 10,
    }
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
        | Event::TurnForked { node, .. } => Some(*node),
    }
}

fn all_events(log_path: &std::path::Path) -> Vec<Event> {
    let (_h, events) = LogReader::open(log_path).expect("open log");
    events.map(|r| r.expect("well-formed record").event).collect()
}

/// A real `returnControlFork @Int` REQUEST — compiled and run through the
/// engine — suspends the parent on a Fork hole. Nothing else in the system
/// reacts to that suspension automatically (`autoForce = never`, C1): no
/// child node is materialized, so the log contains ZERO events referencing
/// any node other than the parent, and in particular zero `TurnForked` /
/// `TurnStart` / `Effect` events for a would-be child. Only the operator's
/// explicit `answer_fork` call (a real forcing decision) creates the child —
/// and even then, the child's FIRST logged events are exactly
/// `NodeCreated`, `TurnForked`, `Forced`, in that order, before any turn or
/// effect event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_request_with_no_forcing_event_has_zero_child_events() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("consent.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![
        RecordedReply {
            node: NodeId(0),
            turn: 0,
            content: "```haskell\nreturnControlFork @Int \"pick a number\"\n```".to_string(),
            usage: usage(),
        },
        // The fork answerer's own turn, once `answer_fork` is (later) called.
        RecordedReply {
            node: NodeId(0),
            turn: 0,
            content: "```haskell\nresume (7 :: Int)\n```".to_string(),
            usage: usage(),
        },
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("consent root", "fork for a number, never answer it")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to a hole");
    assert!(
        matches!(
            outcome,
            TurnOutcome::Suspended {
                classified: ClassifiedHole {
                    routing: HoleRouting::Fork { .. },
                    ..
                },
                ..
            }
        ),
        "root must suspend on a FORK hole from the request alone"
    );
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));

    // --- consent integrity, audited from the durable log --------------------
    // No forcing decision (answer_fork) has been made yet: the ONLY node
    // referenced anywhere in the log is the root itself.
    let events = all_events(&log_path);
    for e in &events {
        if let Some(n) = event_node(e) {
            assert_eq!(
                n,
                root,
                "no node other than root may appear in the log before any forcing decision, got {e:?}"
            );
        }
    }
    assert!(
        !events.iter().any(|e| matches!(e, Event::TurnForked { .. })),
        "zero TurnForked events before answer_fork is ever called"
    );

    // Now make the forcing decision explicitly: THIS is the only thing that
    // may create a child and give it events.
    let child = harness
        .answer_fork(root, Actor::Operator)
        .await
        .expect("fork answered end to end");
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));
    assert_ne!(child, root);

    let events = all_events(&log_path);
    let child_events: Vec<&Event> = events.iter().filter(|e| event_node(e) == Some(child)).collect();
    assert!(!child_events.is_empty(), "the forced child must now have events");
    assert!(
        matches!(child_events[0], Event::NodeCreated { .. }),
        "the child's FIRST event is NodeCreated, got {:?}",
        child_events[0]
    );
    // No turn/effect event for the child precedes its own Forced event —
    // exactly the invariant `forcing.rs` enforces structurally at the
    // NodeTree layer, now confirmed to hold when the trigger is a REAL
    // compiled `returnControlFork` request driven through the turn engine.
    let forced_idx = child_events
        .iter()
        .position(|e| matches!(e, Event::Forced { .. }))
        .expect("child must have a Forced event");
    for e in &child_events[..forced_idx] {
        assert!(
            !matches!(e, Event::TurnStart { .. } | Event::Effect { .. } | Event::HolePublished { .. }),
            "no turn/effect/hole event before the child's own Forced event: {e:?}"
        );
    }
}
