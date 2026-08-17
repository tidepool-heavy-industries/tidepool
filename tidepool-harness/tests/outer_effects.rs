//! Acceptance for S1-L1 outer-row servicing
//! (`plans/self-iterating-harness/20-exomonad-v3-prd.md`): the AUTHORED loop
//! calls `say` (Console), `createWorktree` (Worktree), `run` (Exec), a
//! `withHandler`/`headChanged` subscribe-drain-unsubscribe cycle (RepoEvent),
//! an `after`/`nextEvent` blocking deadline wait (RepoEvent's `RepoEventAwait`
//! suspension — the headline verb `nextEvent`/`after`/`awaitSubscription` all
//! send), and `record` (Journal), and the driver services each resulting
//! suspension through its driver-owned handler set — suspension-serviced, the
//! outer session's handled prefix staying EMPTY on the shared machine.
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
    acquire_lease, answerer_decls, load_harness_source, DriverError, Harness, LogObserver,
    SelfHarnessDriver,
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
/// `withHandler`/(`after`+`nextEvent`) → five suspensions → driver-owned
/// handlers → resumed continuation → durable `State`.
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
    assert_eq!(
        state.get("tickObserved").and_then(|v| v.as_bool()),
        Some(true),
        "`after 50 >>= nextEvent` must round-trip a Tick through the driver \
         (RepoEventAwait), got {state:?}"
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

/// A `state_json` array field, read out as `Vec<&str>` for a plain assertion
/// against `vec!["alpha", "beta"]`-shaped expectations.
fn text_array<'a>(state: &'a serde_json::Value, field: &str) -> Vec<&'a str> {
    state
        .get(field)
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("{field} must be an array, got {state:?}"))
        .iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_else(|| panic!("{field} entry must be a string"))
        })
        .collect()
}

/// Acceptance for PRD 20 S1-L5 wave 1 (`plans/self-iterating-harness/
/// 20-s1-l5-resume.md`): the driver-side boot fold. `ResumeHarness.hs`
/// declares BOTH `loop` (walks the first two of three steps — a run a crash
/// caught with one step still to go) and `resumeLoop` (walks every step
/// against the injected fold, skipping what is already recorded) so entry
/// SELECTION is what each assertion below actually exercises.
///
/// (a) FRESH: `acquire_lease` mints, the (nonexistent) journal folds to
/// nothing, one cycle compiles the ordinary `loop` entry and records two
/// steps.
/// (b) RESUMED: a SECOND driver over the SAME log dir. `acquire_lease`
/// reports the same run id/journal; folding it now returns 2; one cycle
/// compiles `resumeLoop`, skips the two already-recorded steps, and appends
/// exactly one new entry (`gamma`, `seq` 2 — continuing past the fresh run's
/// seq numbers rather than restarting at 0, proving `JournalHandler::resuming`
/// seeded the counter from the fold).
/// (c) REFUSED: a non-empty fold against `OuterEffectsHarness.hs` (declares
/// no `resumeLoop`) fails `run_one_cycle` with `DriverError::ResumeEntryMissing`
/// before any cycle runs — no extract compile paid for this leg.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_boot_fold_fresh_then_resumed_appends_only_the_delta() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let resume_source = load_harness_source(&fixtures_dir().join("ResumeHarness.hs"))
        .expect("fixture harness loads");

    // --- (a) FRESH boot: no lease on disk yet, nothing folded -------------
    let log_dir = tempfile::TempDir::new().expect("log dir");

    let fresh_agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let fresh_provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(Vec::new()));
    let fresh_writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("resume-fold-fresh-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let fresh_agent = Arc::new(
        Harness::new(fresh_writer, fresh_agent_cfg, fresh_provider).expect("agent harness boots"),
    );
    let mut fresh_driver = SelfHarnessDriver::new(fresh_agent, Arc::new(LogObserver));
    // This test drives each driver by hand (no restart-from-checkpoint
    // involved) and keeps the two boots' checkpoints deliberately apart —
    // the fresh boot's own file, distinct from the resumed boot's below.
    fresh_driver.set_checkpoint_path(log_dir.path().join("checkpoint-fresh.json"));

    let fresh_lease = acquire_lease(log_dir.path()).expect("mint the lease");
    assert!(
        !fresh_lease.resumed,
        "no lease on disk yet — this boot must mint one"
    );
    let fresh_folded = fresh_driver
        .open_run_journal(&fresh_lease.lease)
        .expect("open the (nonexistent) journal");
    assert_eq!(fresh_folded, 0, "a fresh journal folds nothing");

    let fresh_outcome = fresh_driver
        .run_one_cycle(&resume_source, None)
        .await
        .expect("fresh cycle: loop walks the first two of three steps");
    let fresh_state = &fresh_outcome.state_json;
    assert_eq!(
        text_array(fresh_state, "recorded"),
        vec!["alpha", "beta"],
        "the fresh loop must record exactly the first two steps, got {fresh_state:?}"
    );
    assert_eq!(
        text_array(fresh_state, "skipped"),
        Vec::<&str>::new(),
        "a fresh run skips nothing, got {fresh_state:?}"
    );
    assert_eq!(
        fresh_state.get("sawResume").and_then(|v| v.as_bool()),
        Some(false),
        "a fresh boot enters through loop, not resumeLoop, got {fresh_state:?}"
    );

    let fresh_entries =
        load_journal(&fresh_lease.lease.journal).expect("the fresh run's journal loads");
    assert_eq!(
        fresh_entries.len(),
        2,
        "the fresh run must have journaled exactly two steps, got {fresh_entries:?}"
    );
    assert!(fresh_entries.iter().all(|e| e.kind == "step"));

    // --- (b) RESUMED boot: a second driver over the SAME log dir ----------
    let resumed_agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let resumed_provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(Vec::new()));
    let resumed_writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("resume-fold-resumed-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let resumed_agent = Arc::new(
        Harness::new(resumed_writer, resumed_agent_cfg, resumed_provider)
            .expect("agent harness boots"),
    );
    let mut resumed_driver = SelfHarnessDriver::new(resumed_agent, Arc::new(LogObserver));
    resumed_driver.set_checkpoint_path(log_dir.path().join("checkpoint-resumed.json"));

    let resumed_lease = acquire_lease(log_dir.path()).expect("resume the lease");
    assert!(
        resumed_lease.resumed,
        "a lease is already on disk — this boot must resume it"
    );
    assert_eq!(resumed_lease.lease.run_id, fresh_lease.lease.run_id);
    assert_eq!(resumed_lease.lease.journal, fresh_lease.lease.journal);

    let resumed_folded = resumed_driver
        .open_run_journal(&resumed_lease.lease)
        .expect("fold the fresh boot's journal");
    assert_eq!(
        resumed_folded, 2,
        "the resumed boot must fold both of the fresh boot's steps"
    );

    let resumed_outcome = resumed_driver
        .run_one_cycle(&resume_source, None)
        .await
        .expect("resumed cycle: resumeLoop walks every step against the fold");
    let resumed_state = &resumed_outcome.state_json;
    assert_eq!(
        text_array(resumed_state, "skipped"),
        vec!["alpha", "beta"],
        "the resumed run must skip the two steps the fold already accounts for, got {resumed_state:?}"
    );
    assert_eq!(
        text_array(resumed_state, "recorded"),
        vec!["gamma"],
        "the resumed run must record only the delta, got {resumed_state:?}"
    );
    assert_eq!(
        resumed_state.get("sawResume").and_then(|v| v.as_bool()),
        Some(true),
        "the resumed boot must have entered through resumeLoop, got {resumed_state:?}"
    );

    let resumed_entries =
        load_journal(&resumed_lease.lease.journal).expect("the resumed run's journal loads");
    assert_eq!(
        resumed_entries.len(),
        3,
        "appended only the delta — nothing rewritten, got {resumed_entries:?}"
    );
    assert_eq!(resumed_entries[2].key, "gamma");
    assert_eq!(
        resumed_entries[2].seq, 2,
        "the resumed handler must continue past the fresh run's seq numbers, \
         proving JournalHandler::resuming seeded the counter from the fold"
    );

    // --- (c) REFUSED boot: a non-empty fold, no resumeLoop entry ----------
    // Deliberately a SEPARATE log dir/journal — the fold here is hand-seeded,
    // not derived from (a)/(b)'s run.
    let refused_log_dir = tempfile::TempDir::new().expect("log dir");
    let refused_lease = acquire_lease(refused_log_dir.path()).expect("mint the lease");
    std::fs::write(
        &refused_lease.lease.journal,
        "{\"seq\":0,\"kind\":\"step\",\"key\":\"alpha\",\"payload\":{}}\n",
    )
    .expect("hand-seed a non-empty journal");

    let refused_agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let refused_provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(Vec::new()));
    let refused_writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("resume-fold-refused-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let refused_agent = Arc::new(
        Harness::new(refused_writer, refused_agent_cfg, refused_provider)
            .expect("agent harness boots"),
    );
    let mut refused_driver = SelfHarnessDriver::new(refused_agent, Arc::new(LogObserver));

    let refused_folded = refused_driver
        .open_run_journal(&refused_lease.lease)
        .expect("fold the hand-seeded journal");
    assert_eq!(refused_folded, 1);

    let no_resume_source = load_harness_source(&fixtures_dir().join("OuterEffectsHarness.hs"))
        .expect("fixture harness loads");
    let err = refused_driver
        .run_one_cycle(&no_resume_source, None)
        .await
        .expect_err("a non-empty fold against a harness with no resumeLoop must refuse at boot");
    assert!(
        matches!(err, DriverError::ResumeEntryMissing { .. }),
        "expected ResumeEntryMissing, got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("OuterEffectsHarness.hs"),
        "the refusal must name the harness file, got: {msg}"
    );
    assert!(
        msg.contains(&refused_lease.lease.journal.display().to_string()),
        "the refusal must name the journal file, got: {msg}"
    );
}
