//! W1b acceptance: cross-turn binding persistence on a single node.
//!
//! The harness used to bootstrap a FRESH Haskell module per turn, so a value
//! introduced in turn N was gone in turn N+1. W1b turned on the shared
//! persistent-session core's decl plane per node: a declaration turn accumulates
//! into the node's `Tidepool.Session.Lib.G<g>` module, and a later expression
//! turn compiles session-aware (importing that module), so a prior turn's
//! declaration resolves as a live binding.
//!
//! This drives that through the REAL turn path (`drive_turn` → `run_block`) via
//! the record-replay provider: turn 1 declares `steps`, turn 2 (a follow-up)
//! computes from `steps`. On pre-W1b code turn 2's fresh module does not know
//! `steps`, so the reference fails to compile and the turn never completes with
//! the value — this test would fail.
//!
//! GHC-heavy tier: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::{NodeId, NodeState};
use tidepool_harness::{Harness, TurnOutcome};

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
        prelude_hash: "cross-turn".into(),
        extract_fingerprint: "cross-turn".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 100,
        output_tokens: 20,
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
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)");
        return;
    }

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

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}
