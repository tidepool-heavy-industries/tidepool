//! Acceptance for the outer-row Subagent seam: the AUTHORED loop calls
//! `spawnAgent @CuratorReceipt`, the driver
//! services the resulting `Subagent` suspension through its driver-owned
//! `SubagentHandler` (suspension-serviced — the outer session's handled
//! prefix stays EMPTY on the shared machine), and the typed receipt crosses
//! back into the resumed continuation and out through durable `State`.
//!
//! Backend tier: `MockBackend` — no live model, no tokens, ever. The real
//! saga still runs end to end (worktree allocated from a real temporary git
//! repo, binding table written, payload decoded by the Haskell-side
//! `FromJSON`).
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

use std::sync::Arc;

mod support;

use tidepool_agent::backend::mock::MockBackend;
use tidepool_agent::seam::CycleResultPayload;
use tidepool_handlers::SubagentHandler;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::DynModelProvider;
use tidepool_harness::replay::ReplayProvider;
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Harness, LogObserver, SelfHarnessDriver,
};
use tidepool_worktree::testing::TestRepo;

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn fixtures_dir() -> std::path::PathBuf {
    repo_root().join("tidepool-harness/tests/fixtures")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "outer-subagent".into(),
        extract_fingerprint: "outer-subagent".into(),
        harness_version: "test".into(),
    }
}

/// The full round trip: authored `loop` → `spawnAgent` → Subagent suspension
/// → driver-owned handler (real saga, mock backend) → typed
/// `CuratorReceipt` decode → resumed continuation → durable `State`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outer_loop_spawn_agent_round_trips_through_the_driver() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    // The nested answerer config is required by the driver's constructor but
    // never exercised here — the fixture loop opens no model holes.
    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(Vec::new()));
    let writer = tidepool_harness::log::LogWriter::create(
        support::unique_temp_log_path("outer-subagent"),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));

    // The curator's target: a real (temporary) git repo — the memory store's
    // stand-in — plus registry/worktree/binding roots OUTSIDE it.
    let store = TestRepo::init().expect("git init the memory store");
    store
        .writer()
        .commit_file("MEMORY.md", "seed digest\n", "seed the store")
        .expect("seed commit");
    let roots = tempfile::TempDir::new().expect("substrate roots");
    let backend = MockBackend::completing(CycleResultPayload::Structured(serde_json::json!({
        "digest": "one memory: the operator prefers typed options",
        "summary": "filed 1 intention",
    })));
    let handler = SubagentHandler::new(
        roots.path().join("registry"),
        roots.path().join("worktrees"),
        roots.path().join("bindings"),
        store.path().to_path_buf(),
        Box::new(backend),
    )
    .expect("subagent handler opens");
    driver.set_subagent_handler(handler);

    let source = load_harness_source(&fixtures_dir().join("SubagentHarness.hs"))
        .expect("fixture harness loads");
    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("one cycle: loop spawns, driver services, receipt crosses back");

    let state = &outcome.state_json;
    assert_eq!(
        state.get("runs").and_then(|v| v.as_i64()),
        Some(1),
        "the loop completed exactly one spawn, got {state:?}"
    );
    assert_eq!(
        state.get("lastDigest").and_then(|v| v.as_str()),
        Some("one memory: the operator prefers typed options"),
        "the typed receipt's digest field must cross into durable state, got {state:?}"
    );
    assert_eq!(
        state.get("lastError").and_then(|v| v.as_str()),
        Some(""),
        "the spawn must succeed (no rendered SpawnError), got {state:?}"
    );
}

/// Without a wired handler, a Subagent suspension fails LOUDLY with the
/// wiring instruction — never a hang, never a silent drop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outer_spawn_without_handler_errors_legibly() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(Vec::new()));
    let writer = tidepool_harness::log::LogWriter::create(
        support::unique_temp_log_path("outer-subagent-nh"),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));

    let source = load_harness_source(&fixtures_dir().join("SubagentHarness.hs"))
        .expect("fixture harness loads");
    let err = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect_err("a Subagent suspension with no handler must error");
    let msg = err.to_string();
    assert!(
        msg.contains("set_subagent_handler"),
        "the error names the wiring seam, got: {msg}"
    );
}
