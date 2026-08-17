//! F3 regression: node decl-plane directories must be scoped per Harness
//! run, not process-global.
//!
//! Before the fix, `node_decl_plane` rooted every node's declaration plane
//! at `<cache>/harness-sessions/node-<id>` — a path that depends only on the
//! node id, not on which `Harness` created it. Node ids are numbered from 0
//! independently per `Harness` (`forcing.rs`'s `next_node_id`), so two
//! `Harness` instances sharing one cache root (e.g. a dogfood session
//! running beside an acceptance test, or two `Harness`es constructed in one
//! process) both create a `node-0`, and `node_decl_plane`'s
//! `remove_dir_all` at FORCE time means the SECOND instance's node-0 wipes
//! the FIRST instance's already-live, already-declared decl plane out from
//! under it — breaking cross-turn declaration accumulation for the
//! survivor.
//!
//! This test reproduces the collision directly through the real entry point
//! (`Harness::force`/`drive_turn`, not the private path-construction
//! helpers): harness A declares a value on its root node (turn 1) and
//! confirms it resolves (turn 2 — proving the declaration genuinely landed
//! on disk and validated through GHC, not just in memory). THEN harness B,
//! sharing A's cache root, forces its OWN root node — node 0, the exact
//! collision trigger. Turn 3 on A re-references the same declared value; it
//! must still resolve. Under the OLD shared-path layout this test fails
//! (turn 3 can't find the declaration — A's decl-plane directory was
//! deleted by B's force call); under the run-scoped fix, A and B resolve to
//! different `harness-sessions/<run_id>/node-0` directories and can never
//! collide.
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

fn header(tag: &str) -> LogHeader {
    LogHeader {
        prelude_hash: format!("decl-plane-run-scoping-{tag}"),
        extract_fingerprint: format!("decl-plane-run-scoping-{tag}"),
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

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

/// Two `Harness` instances sharing one cache root (`XDG_CACHE_HOME`) each
/// force a root node (node 0). Instance A's decl plane, live and populated
/// BEFORE instance B is even constructed, must survive instance B forcing
/// its own node 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_harnesses_do_not_delete_each_others_decl_plane() {
    support::require_extract();

    // Both harnesses below resolve `tidepool_runtime::paths::cache_dir()`
    // through this SAME root — the exact sharing condition F3 describes.
    let shared_cache = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_CACHE_HOME", shared_cache.path());
    let logs = tempfile::tempdir().unwrap();

    // --- Harness A: declares `guardedValue`, confirms it resolves. ---
    let cfg_a = EngineConfig::standard(prelude_dir(), None).expect("engine config a");
    let replies_a = vec![
        // Turn 1: a top-level declaration — accumulates on A's node-0 decl
        // plane instead of running as an expression.
        reply("I'll record a value.\n\n```haskell\nguardedValue = 42 :: Int\n```"),
        // Turn 2: reference it — only resolves if turn 1's declaration is
        // live on disk and validated.
        reply("Reading it back.\n\n```haskell\npure (toJSON guardedValue)\n```"),
        // Turn 3 (after B's collision, below): reference it again.
        reply("Reading it back again.\n\n```haskell\npure (toJSON guardedValue)\n```"),
    ];
    let provider_a: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies_a));
    let writer_a = LogWriter::create(logs.path().join("a.jsonl"), &header("a")).unwrap();
    let harness_a = Arc::new(Harness::new(writer_a, cfg_a, provider_a).expect("harness a boots"));

    let root_a = harness_a
        .create_root("harness a root", "Record a value, then use it.")
        .unwrap();
    harness_a.force(root_a, Actor::Operator).unwrap();

    let turn1 = harness_a
        .run_to_hole_or_done(root_a)
        .await
        .expect("harness a turn 1 (declaration) drives to completion");
    assert!(
        matches!(turn1, TurnOutcome::Completed { .. }),
        "harness a's declaration turn should complete, got {}",
        outcome_tag(&turn1)
    );
    assert_eq!(harness_a.tree().state(root_a), Some(NodeState::Done));

    let turn2 = harness_a
        .follow_up(root_a, "Use the value you recorded.")
        .await
        .expect("harness a turn 2 (reference) drives to completion");
    match &turn2 {
        TurnOutcome::Completed { rendered } => assert!(
            rendered.contains("42"),
            "harness a turn 2 must read back guardedValue = 42, got: {rendered}"
        ),
        other => panic!(
            "harness a turn 2 should complete with the recorded value, got {}",
            outcome_tag(other)
        ),
    }

    // --- Harness B: constructed AFTER A already has a live, populated
    // node-0 decl plane. Forces its OWN node 0 — the collision trigger. ---
    let cfg_b = EngineConfig::standard(prelude_dir(), None).expect("engine config b");
    let replies_b = vec![reply("Hello.\n\n```haskell\npure (toJSON (1 :: Int))\n```")];
    let provider_b: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies_b));
    let writer_b = LogWriter::create(logs.path().join("b.jsonl"), &header("b")).unwrap();
    let harness_b = Arc::new(Harness::new(writer_b, cfg_b, provider_b).expect("harness b boots"));

    let root_b = harness_b
        .create_root("harness b root", "Say hello.")
        .unwrap();
    // Both harnesses' first node is NodeId(0) — this is the exact moment the
    // pre-fix code would `remove_dir_all` harness A's node-0 decl plane.
    assert_eq!(root_b, NodeId(0));
    harness_b.force(root_b, Actor::Operator).unwrap();

    let turn_b = harness_b
        .run_to_hole_or_done(root_b)
        .await
        .expect("harness b turn drives to completion");
    assert!(
        matches!(turn_b, TurnOutcome::Completed { .. }),
        "harness b's turn should complete, got {}",
        outcome_tag(&turn_b)
    );

    // --- Back on harness A: the declaration must STILL resolve. ---
    let turn3 = harness_a
        .follow_up(root_a, "Use the value again.")
        .await
        .expect("harness a turn 3 (post-collision reference) drives to completion");
    match &turn3 {
        TurnOutcome::Completed { rendered } => assert!(
            rendered.contains("42"),
            "F3 regression: harness b forcing its own node 0 destroyed harness a's live \
             decl plane — turn 3 could not read back guardedValue = 42, got: {rendered}"
        ),
        other => panic!(
            "harness a turn 3 should still complete with the recorded value \
             (decl plane must survive harness b's node-0 creation), got {}",
            outcome_tag(other)
        ),
    }

    std::env::remove_var("XDG_CACHE_HOME");
}
