//! Acceptance for S1-L1 outer-row servicing
//! (`plans/self-iterating-harness/20-exomonad-v3-prd.md`): the AUTHORED loop
//! calls `say` (Console), `createWorktree` (Worktree), `run` (Exec), and a
//! `withHandler`/`headChanged` subscribe-drain-unsubscribe cycle (RepoEvent),
//! and the driver services each resulting suspension through its
//! driver-owned handler set — suspension-serviced, the outer session's
//! handled prefix staying EMPTY on the shared machine.
//!
//! ONE fixture, ONE compile, every assertion off the single resulting
//! `State` (family-bundle discipline — a new suspension kind joins this
//! bundle rather than paying its own extract compile).
//!
//! RepoEvent is wired over a scripted [`ObservationSource`] (never
//! `MonitorObservations`) — exactly the substitution
//! `RepoEventHandler::with_source`'s own doc names for an acceptance harness:
//! it reads no real git movement and always reports none, so this proves the
//! subscribe/drain/unsubscribe SUSPENSION-SERVICING path without depending on
//! `WorktreeMonitor::register` (a separate concern from S1-L1).
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

use std::sync::Arc;

mod support;

use tidepool_bridge_effects::{EvRepositoryEvent, WtWorktreeId};
use tidepool_handlers::{
    ConsoleHandler, EventConfig, EventError, ExecHandler, ObservationSource, RepoEventHandler,
    WorktreeHandler,
};
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::DynModelProvider;
use tidepool_harness::replay::ReplayProvider;
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, LogObserver, SelfHarnessDriver,
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
        prelude_hash: "outer-effects".into(),
        extract_fingerprint: "outer-effects".into(),
        harness_version: "test".into(),
    }
}

/// Always reports no movement — the acceptance-harness substitution
/// `RepoEventHandler::with_source`'s doc names, in place of a real
/// `WorktreeMonitor`. Proves the subscribe/drain/unsubscribe suspension path
/// without needing the worktree registered against a monitor baseline.
struct NoOpSource;

impl ObservationSource for NoOpSource {
    fn observe(
        &mut self,
        _worktrees: &[WtWorktreeId],
    ) -> Result<Vec<EvRepositoryEvent>, EventError> {
        Ok(Vec::new())
    }
}

/// The full round trip: authored `loop` → `say`/`createWorktree`/`run`/
/// `withHandler` → four suspensions → driver-owned handlers → resumed
/// continuation → durable `State`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outer_loop_effects_round_trip_through_the_driver() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    // The nested answerer config is required by the driver's constructor but
    // never exercised here — the fixture loop opens no model holes.
    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(Vec::new()));
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("outer-effects-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));

    // Worktree's target: a real (temporary) git repo, plus registry/worktree
    // roots OUTSIDE it — same shape `outer_subagent.rs` uses for the memory
    // store.
    let store = TestRepo::init().expect("git init the source repo");
    store
        .writer()
        .commit_file("README.md", "seed\n", "seed the repo")
        .expect("seed commit");
    let roots = tempfile::TempDir::new().expect("substrate roots");
    let worktree_root = roots.path().join("worktrees");
    std::fs::create_dir_all(&worktree_root).expect("worktree root");

    let worktree_handler = WorktreeHandler::new(
        roots.path().join("registry"),
        worktree_root.clone(),
        store.path().to_path_buf(),
    )
    .expect("worktree handler opens");
    let exec_handler = ExecHandler::new(worktree_root);
    let event_handler = RepoEventHandler::with_source(Box::new(NoOpSource), EventConfig::default());

    driver.set_console_handler(ConsoleHandler);
    driver.set_worktree_handler(worktree_handler);
    driver.set_exec_handler(exec_handler);
    driver.set_event_handler(event_handler);

    let source = load_harness_source(&fixtures_dir().join("OuterEffectsHarness.hs"))
        .expect("fixture harness loads");
    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("one cycle: say/createWorktree/run/withHandler all serviced");

    let state = &outcome.state_json;
    assert_eq!(
        state.get("runs").and_then(|v| v.as_i64()),
        Some(1),
        "the loop completed exactly once, got {state:?}"
    );
    assert_eq!(
        state.get("lastError").and_then(|v| v.as_str()),
        Some(""),
        "createWorktree must succeed (no rendered WorktreeError), got {state:?}"
    );
    assert_eq!(
        state
            .get("execOutput")
            .and_then(|v| v.as_str())
            .map(str::trim),
        Some("outer-effects-probe"),
        "exec's stdout must cross into durable state, got {state:?}"
    );
}

/// Without a wired Worktree handler, a Worktree suspension fails LOUDLY with
/// the wiring instruction — never a hang, never a silent drop. Mirrors
/// `outer_subagent.rs`'s `outer_spawn_without_handler_errors_legibly`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outer_worktree_without_handler_errors_legibly() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(Vec::new()));
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("outer-effects-nh-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    // Console IS wired (it's `say`d before `createWorktree` in the fixture
    // loop) so the failure under test is specifically Worktree's, not
    // Console's.
    driver.set_console_handler(ConsoleHandler);

    let source = load_harness_source(&fixtures_dir().join("OuterEffectsHarness.hs"))
        .expect("fixture harness loads");
    let err = driver
        .run_one_cycle(&source, None)
        .await
        .expect_err("a Worktree suspension with no handler must error");
    let msg = err.to_string();
    assert!(
        msg.contains("set_worktree_handler"),
        "the error names the wiring seam, got: {msg}"
    );
}
