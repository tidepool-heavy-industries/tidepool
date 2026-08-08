//! W3 targeted coverage for local-file persistence + restart-reload
//! (`plans/self-iterating-harness/08-wave1-correctness.md` D5: "State-to-disk
//! + restart-reload — no writer, no reader; State survives in-process
//! only"). Drives TWO `render -> loop -> runLLMTurn -> finalize -> render`
//! cycles through the production entry point
//! (`SelfHarnessDriver::run_one_cycle`, mirroring `acceptance_selfharness.rs`'s
//! direct-cycle-driving style — the frozen sync contract's acceptance path),
//! persisting `State` to json after the first cycle via
//! `tidepool_harness::selfharness::persistence::save_state` (what
//! `SelfHarnessDriver::run_loop` calls internally), then constructs a FRESH
//! `SelfHarnessDriver` over a FRESH `Harness` — simulating a killed and
//! restarted process — and confirms it resumes the second cycle from the
//! persisted `State` (advancing `loopCount`/`mode` further), not from
//! `initialState`. Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on
//! PATH — run inside `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::selfharness::persistence;
use tidepool_harness::tree::NodeId;
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, HarnessSource, LogObserver, SelfHarnessDriver,
};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn repo_root() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> std::path::PathBuf {
    repo_root().join("haskell/lib")
}

fn examples_harness_dir() -> std::path::PathBuf {
    repo_root().join("examples/harness")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "selfharness-persistence".into(),
        extract_fingerprint: "selfharness-persistence".into(),
        harness_version: "test".into(),
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: content.to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
        },
    }
}

fn decision_reply(action: &str, confidence: &str) -> RecordedReply {
    reply(&format!(
        "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
         (finalize @Decision (Decision {{ action = \"{action}\", rationale = \"because\", \
         confidence = {confidence} }}) :: M ())\n```"
    ))
}

/// A fresh [`SelfHarnessDriver`] over a fresh [`Harness`] scripted with
/// `replies` — a new one each call mirrors what a restarted PROCESS would
/// construct (a brand-new agent orchestrator with no memory of the prior
/// run's nodes), distinct from just reusing the same driver across cycles.
fn fresh_driver(replies: Vec<RecordedReply>, log_tag: &str) -> SelfHarnessDriver {
    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let writer = tidepool_harness::log::LogWriter::create(
        &std::env::temp_dir().join(format!(
            "selfharness-persistence-{log_tag}-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    SelfHarnessDriver::new(agent, Arc::new(LogObserver))
}

fn source() -> HarnessSource {
    load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads")
}

/// State persists to json after a cycle, and a FRESH driver (simulating a
/// restart) restores it — advancing loopCount/mode from where the killed
/// process left off, rather than starting over from `initialState`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_resumes_from_persisted_state_not_initial_state() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let state_path = std::env::temp_dir().join(format!(
        "selfharness-persistence-state-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&state_path);

    // No file yet: load_state must be Ok(None), not an error — the very
    // first-ever run has nothing to restore.
    assert_eq!(
        persistence::load_state(&state_path).expect("load_state on a missing file"),
        None,
        "a never-persisted path must restore to None, not an error"
    );

    let harness_source = source();

    // --- "Process 1": cycle 1, from initialState (no prior file). ---
    let mut driver1 = fresh_driver(vec![decision_reply("observe", "Medium")], "process1");
    let outcome1 = driver1
        .run_one_cycle(&harness_source, None)
        .expect("cycle 1 (initialState)");
    assert_eq!(
        outcome1
            .state_json
            .get("loopCount")
            .and_then(|v| v.as_i64()),
        Some(1)
    );
    assert_eq!(
        outcome1.state_json.get("mode").and_then(|v| v.as_str()),
        Some("Deciding"),
        "Observing -> Deciding after cycle 1"
    );

    // What `SelfHarnessDriver::run_loop` does after every cycle: persist the
    // returned State to the configured path.
    persistence::save_state(&state_path, &outcome1.state_json).expect("save_state after cycle 1");

    // --- "restart": brand-new driver, brand-new agent, nothing in-process
    // carried over except the file on disk. ---
    let mut driver2 = fresh_driver(vec![decision_reply("act", "High")], "process2");
    driver2.set_state_path(state_path.clone());
    assert_eq!(driver2.state_path(), state_path.as_path());

    let restored = persistence::load_state(driver2.state_path())
        .expect("load_state after restart")
        .expect("cycle 1's State was persisted to disk");
    assert_eq!(
        restored, outcome1.state_json,
        "restored JSON must equal exactly what was persisted"
    );

    // What `SelfHarnessDriver::run_loop` does on start: restore, then run the
    // next cycle against the restored State instead of `None`.
    let outcome2 = driver2
        .run_one_cycle(&harness_source, Some(&restored))
        .expect("cycle 2 (from restored state)");

    // The PRE-loop render for cycle 2 must reflect the RESTORED mode
    // (Deciding), proving `prior_state` reached `render`, not initialState's
    // Observing.
    assert!(
        outcome2.prompt_before.contains("deciding what to do next"),
        "restart must resume from the persisted Deciding mode, not \
         initialState's Observing, got:\n{}",
        outcome2.prompt_before
    );
    assert!(
        !outcome2.prompt_before.contains("No decision made yet"),
        "restart must resume with cycle 1's decision already recorded, got:\n{}",
        outcome2.prompt_before
    );

    // loopCount/mode continue advancing from the RESTORED values (1/Deciding),
    // not reset to 0/Observing.
    assert_eq!(
        outcome2
            .state_json
            .get("loopCount")
            .and_then(|v| v.as_i64()),
        Some(2),
        "loopCount must continue from the persisted 1, not reset to 0 after 1, \
         got {:?}",
        outcome2.state_json
    );
    assert_eq!(
        outcome2.state_json.get("mode").and_then(|v| v.as_str()),
        Some("Acting"),
        "Deciding -> Acting after cycle 2, continuing the restored mode chain"
    );
    let notes = outcome2
        .state_json
        .get("notes")
        .and_then(|v| v.as_array())
        .expect("notes must be an array");
    assert!(
        notes.iter().any(|n| n.as_str() == Some("observe")),
        "cycle 1's note must survive the restart (carried in restored State), \
         got {notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.as_str() == Some("act")),
        "cycle 2's own note must also be present, got {notes:?}"
    );

    let _ = std::fs::remove_file(&state_path);
}

/// `SelfHarnessDriver`'s default `state_path` is a stable, non-empty path
/// under the runtime cache dir (not e.g. accidentally empty/relative to
/// whatever the current directory happens to be at construction) — a cheap
/// sanity check independent of `TIDEPOOL_EXTRACT`.
#[test]
fn default_state_path_is_under_the_cache_dir() {
    let agent_cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(vec![]));
    let writer = tidepool_harness::log::LogWriter::create(
        &std::env::temp_dir().join(format!(
            "selfharness-persistence-default-path-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    assert!(driver.state_path().ends_with("selfharness/state.json"));
}
