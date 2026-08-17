//! Acceptance for S1-L1 outer-row servicing
//! (`plans/self-iterating-harness/20-exomonad-v3-prd.md`): the AUTHORED loop
//! calls `say` (Console), `createWorktree` (Worktree), `run` (Exec), a
//! `withHandler`/`headChanged` subscribe-drain-unsubscribe cycle (RepoEvent),
//! and `record` (Journal), and the driver services each resulting suspension
//! through its driver-owned handler set — suspension-serviced, the outer
//! session's handled prefix staying EMPTY on the shared machine.
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
    load_journal, ConsoleHandler, EventConfig, EventError, ExecHandler, JournalHandler,
    ObservationSource, RepoEventHandler, WorktreeHandler,
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
    let journal_path = std::env::temp_dir().join(format!(
        "outer-effects-journal-{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&journal_path);
    let journal_handler = JournalHandler::new(journal_path.clone());

    driver.set_console_handler(ConsoleHandler);
    driver.set_worktree_handler(worktree_handler);
    driver.set_exec_handler(exec_handler);
    driver.set_event_handler(event_handler);
    driver.set_journal_handler(journal_handler);

    let source = load_harness_source(&fixtures_dir().join("OuterEffectsHarness.hs"))
        .expect("fixture harness loads");
    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("one cycle: say/createWorktree/run/withHandler/record all serviced");

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

    // Tidepool.Async (PRD 20 S1-L4) — the green-thread scheduler.
    assert_eq!(
        state.get("asyncOne").and_then(|v| v.as_i64()),
        Some(21),
        "wait must return the spawned thread's own value, got {state:?}"
    );
    let winner = state
        .get("asyncRaceWinner")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        winner == "A" || winner == "B",
        "waitEither must pick one of the two raced threads, got {state:?}"
    );
    let loser_val = state.get("asyncLoserVal").and_then(|v| v.as_i64());
    let expected_loser_val = if winner == "A" { 2 } else { 1 };
    assert_eq!(
        loser_val,
        Some(expected_loser_val),
        "waitEither's loser (the thread that did NOT win) must still be joinable \
         afterward — never cancelled, unlike race — got {state:?}"
    );
    assert_eq!(
        state.get("asyncCancelled").and_then(|v| v.as_bool()),
        Some(true),
        "waitCatch on a cancelled thread must report Left AsyncCancelled, got {state:?}"
    );
    assert_eq!(
        state.get("asyncMapResults"),
        // mapWork n = sumTo n * 10: sumTo 3=6, sumTo 1=1, sumTo 2=3.
        Some(&serde_json::json!([60, 10, 30])),
        "mapConcurrently must return results in ORIGINAL list order ([3,1,2], each a \
         differing-length recursive sum), regardless of completion order, got {state:?}"
    );

    // Journal: the loop's `record "outer-effects" "probe" ...` call must have
    // landed durably in the journal file — read it back through JournalHandler's
    // own fold API, not just trust the loop completed.
    let entries = load_journal(&journal_path).expect("journal file loads");
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one recorded journal entry, got {entries:?}"
    );
    assert_eq!(entries[0].kind, "outer-effects");
    assert_eq!(entries[0].key, "probe");
    assert_eq!(
        entries[0]
            .payload
            .get("exec")
            .and_then(|v| v.as_str())
            .map(str::trim),
        Some("outer-effects-probe"),
        "the recorded payload must carry the exec output, got {:?}",
        entries[0].payload
    );

    let _ = std::fs::remove_file(&journal_path);
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

/// Without a wired Journal handler, a `record` suspension fails LOUDLY with
/// the wiring instruction — never a hang, never a silent drop. Console,
/// Worktree, Exec, and RepoEvent are all wired here (the fixture loop
/// reaches `record` only after they all succeed), so the failure under test
/// is specifically Journal's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outer_journal_without_handler_errors_legibly() {
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
        std::env::temp_dir().join(format!("outer-effects-nj-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));

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
    // Journal deliberately left unwired.

    let source = load_harness_source(&fixtures_dir().join("OuterEffectsHarness.hs"))
        .expect("fixture harness loads");
    let err = driver
        .run_one_cycle(&source, None)
        .await
        .expect_err("a Journal suspension with no handler must error");
    let msg = err.to_string();
    assert!(
        msg.contains("set_journal_handler"),
        "the error names the wiring seam, got: {msg}"
    );
}
