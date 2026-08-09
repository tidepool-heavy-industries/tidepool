//! Acceptance: an EFFECTFUL value bind persists across turns.
//!
//! The motivating case the decl plane cannot express: turn 1 binds a value
//! via a FORK (`steps <- runLLMTurnFork @[Int] …`) — an effectful bind that
//! SUSPENDS at the fork — and turn 2 references `steps` as a live typed binding.
//! `acceptance_cross_turn` only covers the weaker decl-plane property (a pure
//! `steps = [1,2,3]` CAF); this covers the value plane end to end through the
//! real drive_turn → run_bind → answer_fork → resume_bind → materialize path.
//! If the harness ever stopped materializing an `x <- e` bind, turn 2's
//! `sum steps` would fail to resolve and this test would catch it.
//!
//! GHC-heavy tier: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
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
        prelude_hash: "value-bind".into(),
        extract_fingerprint: "value-bind".into(),
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

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

/// Turn 1 binds `steps` through a FORK; turn 2 sums it. The value only survives
/// if the effectful bind materialized into the node's value plane on resume.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_effectful_fork_bind_persists_into_the_next_turn() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("value_bind.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![
        // Turn 1: an EFFECTFUL bind — the RHS forks, so the bind suspends and
        // materializes only when the fork is answered.
        reply("I'll ask a sub-agent for the steps.\n\n```haskell\nsteps <- runLLMTurnFork @[Int] \"give me [1,2,3]\"\n```"),
        // The fork answerer (runs against the suspended parent): the raw [Int].
        reply("```haskell\nresume ([1, 2, 3] :: [Int])\n```"),
        // Turn 2: a pure bind that references the PERSISTED `steps`.
        reply("Now I'll total them.\n\n```haskell\ntotal <- pure (sum steps)\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("value-bind root", "Fork for steps, then use them.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    // Turn 1 suspends at the fork.
    let turn1 = harness
        .run_to_hole_or_done(root)
        .await
        .expect("turn 1 drives to the fork hole");
    assert!(
        matches!(turn1, TurnOutcome::Suspended { .. }),
        "the fork bind should suspend, got {}",
        outcome_tag(&turn1)
    );

    // Answer the fork → resume_bind materializes `steps` → turn 1 completes.
    harness
        .answer_fork(root, Actor::Operator)
        .await
        .expect("fork answered end to end");
    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the parent completes once the [Int] resumes the bind"
    );

    // Turn 2 references `steps` — resolves only if the effectful bind persisted.
    let turn2 = harness
        .follow_up(root, "Total the steps you fetched.")
        .await
        .expect("turn 2 drives to completion");
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
