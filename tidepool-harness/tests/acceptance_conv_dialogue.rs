//! The free-monad `Conv` modality end to end: the model authors, in ONE turn, a
//! speculative dialogue subtree (`runConv (do …)`); the harness walks it one
//! operator interaction per node — with NO model round-trip between nodes —
//! following the `menu` branch the operator picks, until `done`.
//!
//! This proves the amortization payoff: three operator interactions (drink →
//! "anything else?" → nights) all flow from a SINGLE model turn. It also proves
//! the free monad compiles + runs on the JIT and that `runConv` threads answers
//! through its continuations across the dialog suspend/resume boundary.
//!
//! GHC-heavy tier: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::{NodeId, NodeState};
use tidepool_harness::Harness;

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
        prelude_hash: "conv".into(),
        extract_fingerprint: "conv".into(),
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

/// One authored turn, three operator interactions, no round-trip between them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_speculative_dialogue_subtree_walks_locally() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("conv.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    // ONE model turn authors the whole likely subtree. `import Tidepool.Conv` is
    // peeled to the turn's import list; `runConv (do …)` is the M-typed block.
    let block = "Let me run the intake.\n\n\
        ```haskell\n\
        import Tidepool.Conv\n\
        \n\
        runConv (do\n\
        \x20 narrate \"Welcome to the Rusty Tankard.\"\n\
        \x20 drink <- pick \"What'll you have?\" [\"Ale\", \"Water\"]\n\
        \x20 menu \"Anything else?\"\n\
        \x20   [ (\"A room\", do\n\
        \x20       nights <- pick \"How many nights?\" [\"1\", \"2\", \"3+\"]\n\
        \x20       done (\"Poured \" <> drink <> \"; room for \" <> nights <> \" nights.\"))\n\
        \x20   , (\"Just the drink\", done (\"Poured \" <> drink <> \". Enjoy.\")) ])\n\
        ```";
    let replies = vec![reply(block)];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("tavern", "Run a branching intake.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    // Drive to the FIRST node's dialog hole (the `pick` for the drink; the
    // `narrate` beat is folded into its card).
    harness
        .run_to_hole_or_done(root)
        .await
        .expect("drives to node 1");
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));

    // Node 1: choose the drink. Resume advances the compiled walk to node 2 — no
    // model round-trip.
    harness
        .answer_dialog(root, json!({ "values": { "f0": "Ale" }, "prose": "" }))
        .await
        .expect("answer node 1 (drink)");
    // Node 2: the `menu` branch — pick "A room".
    harness
        .answer_dialog(root, json!({ "values": { "f0": "A room" }, "prose": "" }))
        .await
        .expect("answer node 2 (branch)");
    // Node 3: nights. This resume reaches `done`, completing the turn.
    harness
        .answer_dialog(root, json!({ "values": { "f0": "1" }, "prose": "" }))
        .await
        .expect("answer node 3 (nights)");

    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the whole subtree walked to `done` from ONE authored turn"
    );

    // The `done` summary threaded both answers (drink + nights) through the
    // free-monad continuations across the suspend/resume boundary.
    let (_h, events) = LogReader::open(&log_path).expect("open log");
    let rendered = events
        .filter_map(Result::ok)
        .find_map(|r| match r.event {
            Event::NodeDone {
                node,
                result_rendered,
            } if node == root => Some(result_rendered),
            _ => None,
        })
        .expect("root NodeDone in log");
    assert!(
        rendered.contains("Poured Ale") && rendered.contains("room for 1 nights"),
        "the summary carries both answers threaded through the walk, got: {rendered}"
    );
}

/// `finish` carries the collected answers back as STRUCTURED data (a `Value`),
/// so a later turn can read them as fields instead of re-parsing a summary
/// string. Same one-turn walk, but the terminal is `finish (object [...])`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finish_returns_structured_answers() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("conv_finish.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    // `object`/`.=` come from the auto-imported `Tidepool.Prelude` — no extra
    // import beyond `Tidepool.Conv`.
    let block = "Collecting the order as data.\n\n\
        ```haskell\n\
        import Tidepool.Conv\n\
        \n\
        runConv (do\n\
        \x20 drink <- pick \"What'll you have?\" [\"Ale\", \"Water\"]\n\
        \x20 nights <- pick \"How many nights?\" [\"1\", \"2\", \"3+\"]\n\
        \x20 finish (object [\"drink\" .= drink, \"nights\" .= nights]))\n\
        ```";
    let replies = vec![reply(block)];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness.create_root("order", "Collect an order.").unwrap();
    harness.force(root, Actor::Operator).unwrap();

    harness
        .run_to_hole_or_done(root)
        .await
        .expect("drives to node 1");
    harness
        .answer_dialog(root, json!({ "values": { "f0": "Ale" }, "prose": "" }))
        .await
        .expect("answer drink");
    harness
        .answer_dialog(root, json!({ "values": { "f0": "2" }, "prose": "" }))
        .await
        .expect("answer nights");

    assert_eq!(harness.tree().state(root), Some(NodeState::Done));

    let (_h, events) = LogReader::open(&log_path).expect("open log");
    let rendered = events
        .filter_map(Result::ok)
        .find_map(|r| match r.event {
            Event::NodeDone {
                node,
                result_rendered,
            } if node == root => Some(result_rendered),
            _ => None,
        })
        .expect("root NodeDone in log");
    // The result is a structured object under `result` — the caller reads
    // `drink`/`nights` as fields, not out of a prose summary.
    assert!(
        rendered.contains("result")
            && rendered.contains("drink")
            && rendered.contains("Ale")
            && rendered.contains("nights")
            && rendered.contains('2'),
        "finish carries the collected answers as structured data, got: {rendered}"
    );
}
