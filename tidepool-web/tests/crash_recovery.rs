//! Crash-recovery acceptance: SIGKILL the real `tidepool-selfharness`
//! process mid-answerer-turn, restart it against the same durable state, and
//! confirm the loop resumes from the persisted checkpoint rather than
//! `initialState` — through the production binary, not a hand-wired driver.
//!
//! The binary is located via `env!("CARGO_BIN_EXE_tidepool-selfharness")`
//! (Cargo sets this for integration tests in the crate that defines the
//! bin). Durable state is isolated per run via `XDG_CACHE_HOME`, which
//! `tidepool_runtime::paths::cache_dir` resolves every state/transcript/log
//! path through — both children below and this test's own process (which
//! needs the same cache dir to check the persisted checkpoint) share one
//! override, set once at the top of the test.
//!
//! The scripted replay content is content-agnostic by design: `Harness.hs`'s
//! `loop` transitions `Mode` on every call to `runLLMTurn` regardless of the
//! `Decision`'s field values, so the SAME two recorded replies serve any
//! cycle — what distinguishes "resumed from checkpoint" from "restarted at
//! `initialState`" is the loop COUNT reached before the replay queue
//! (rebuilt fresh from the log file on every process start) runs out, not
//! the reply content. The loop count itself is a runtime fact — carried in
//! the checkpoint ENVELOPE's `iteration` field, not in `State`
//! (`plans/self-iterating-harness/15-generic-surface-wave.md`, "Runtime
//! context is the runtime's job").
//!
//! Assertions read the persisted checkpoint only through
//! `SelfHarnessDriver::checkpoint_path`'s public accessor (never a
//! hard-coded filename) plus `persistence::load_checkpoint`'s public
//! `Checkpoint` record — `generation`, incremented once per committed
//! cycle, is the direct claim: the restarted process's checkpoint
//! generation continuing past the crashed process's is what "resumed from
//! the checkpoint rather than `initialState`" means, mechanically.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Event as LogEvent, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Role, Usage};
use tidepool_harness::replay::ReplayProvider;
use tidepool_harness::selfharness::persistence;
use tidepool_harness::{answerer_decls, Harness, LogObserver, NodeId, SelfHarnessDriver};

/// How long a wait tolerates NO new durable-log event before treating the
/// child as genuinely stuck rather than slow under load — sized above the
/// worst single-compile gap observed under real contention (a full
/// boot+turn cycle ran 555s end to end on a contended box in dogfood), with
/// margin.
const STALL_WINDOW: Duration = Duration::from_secs(500);

/// Absolute ceiling for [`wait_for_turn_start_count`] — needs one full
/// committed cycle's worth of compiles (2 boot + 1 turn) to complete before
/// the second cycle's turn even starts.
const TURN_START_WAIT_CEILING: Duration = Duration::from_secs(600);

/// Absolute ceiling for [`wait_for_exit`] on the restarted process — needs
/// 2 more full committed cycles (see the module doc) before the loop
/// naturally exhausts the replay queue and exits.
const EXIT_WAIT_CEILING: Duration = Duration::from_secs(800);

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-web has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> PathBuf {
    repo_root().join("haskell/lib")
}

fn harness_path() -> PathBuf {
    repo_root().join("examples/harness/Harness.hs")
}

/// A `finalize @Decision (...)` block, exactly the shape
/// `tidepool-harness/tests/selfharness_persistence.rs` already proves the
/// answerer's scoped `[AskUser, Fork, Finalize]` row accepts. Content is
/// interchangeable across cycles (see module doc) — every recorded reply
/// uses the same one.
fn decision_block() -> String {
    "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
     (finalize @Decision (Decision { action = \"act\", rationale = \"because\", \
     confidence = Medium }) :: M ())\n```"
        .to_string()
}

/// Write a fresh replay log at `path` with `replies` recorded assistant
/// turns, in the raw `tidepool_harness::log` wire format `--replay` reads —
/// what `ReplayProvider::from_log` filters for is `TurnDelta{role:
/// Assistant, ..}`; `node`/`turn` are unread by the filter (replay is
/// order-only, see `replay.rs`'s module doc), so a fixed placeholder is
/// fine.
fn build_replay_log(path: &Path, replies: u64) {
    let header = LogHeader {
        prelude_hash: "crash-recovery".into(),
        extract_fingerprint: "crash-recovery".into(),
        harness_version: "test".into(),
    };
    let mut writer = LogWriter::create(path, &header).expect("create replay log");
    for turn in 0..replies {
        writer
            .append(LogEvent::TurnDelta {
                node: NodeId(0),
                turn,
                role: Role::Assistant,
                content: decision_block(),
                usage: Some(Usage {
                    input_tokens: 50,
                    output_tokens: 10,
                }),
                reasoning: None,
            })
            .expect("append recorded reply");
    }
}

/// The most recently created `log-*.jsonl` under `dir` (the answerer's own
/// durable per-node log — a fresh timestamped file every process start, per
/// `tidepool-selfharness`'s main), or `None` if the process hasn't created
/// one yet. Unix-timestamp filenames of equal digit width sort correctly as
/// strings.
fn newest_log_file(dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("log-") && n.ends_with(".jsonl"))
        })
        .collect();
    entries.sort();
    entries.pop()
}

/// How many `TurnStart` events the log at `path` currently holds. Tolerant
/// of a file mid-write (missing header, a torn trailing line): both read as
/// zero so the poll loop just retries.
fn count_turn_starts(path: &Path) -> usize {
    let Ok((_header, events)) = LogReader::open(path) else {
        return 0;
    };
    events
        .filter_map(Result::ok)
        .filter(|record| matches!(record.event, LogEvent::TurnStart { .. }))
        .count()
}

/// How many events of ANY kind the newest log file under `dir` currently
/// holds — finer-grained than [`count_turn_starts`] alone
/// (`NodeCreated`/`Forced`/`HolePublished`/`HoleConsumed`/`NodeDone` all
/// land between one `TurnStart` and the next), used as the progress signal
/// a stall watchdog polls. `0` if no log file exists yet or it's mid-write.
fn total_log_events(dir: &Path) -> usize {
    let Some(path) = newest_log_file(dir) else {
        return 0;
    };
    let Ok((_header, events)) = LogReader::open(&path) else {
        return 0;
    };
    events.filter_map(Result::ok).count()
}

/// Poll `poll` at a fixed interval until it returns `Some(_)`. Fails loudly
/// — rather than hang — the moment EITHER bound trips: `progress()`'s
/// return value hasn't changed for `stall_after` (genuinely stuck, not just
/// slow: a wall-clock-only deadline can't tell those apart under the
/// 4-6x compile-time inflation a contended box produces, since a legitimate
/// multi-compile wait needs more wall time than a single compile does, but
/// a REAL hang produces no new durable-log event at all), or `timeout`, the
/// absolute backstop, elapses. `on_stall`/`on_timeout` render a
/// self-diagnosing message from (time in that state, last progress value
/// observed) — naming which bound tripped, not just that time ran out.
fn poll_with_stall_watchdog<T>(
    mut poll: impl FnMut() -> Option<T>,
    mut progress: impl FnMut() -> usize,
    stall_after: Duration,
    timeout: Duration,
    on_stall: impl FnOnce(Duration, usize) -> String,
    on_timeout: impl FnOnce(Duration, usize) -> String,
) -> T {
    let start = Instant::now();
    let mut last_value = progress();
    let mut last_change = start;
    loop {
        if let Some(result) = poll() {
            return result;
        }
        let now = Instant::now();
        let current = progress();
        if current != last_value {
            last_value = current;
            last_change = now;
        }
        if now.duration_since(last_change) >= stall_after {
            panic!("{}", on_stall(now.duration_since(last_change), last_value));
        }
        if now.duration_since(start) >= timeout {
            panic!("{}", on_timeout(now.duration_since(start), last_value));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Block until the newest log file under `dir` holds at least `target`
/// `TurnStart` events. `TurnStart` is logged with the model's extracted
/// Haskell block already in hand but BEFORE that block is compiled and run
/// (`harness.rs::drive_turn`) — a real GHC extract call sits between this
/// marker and the turn actually finishing, which is the window this test
/// kills inside.
fn wait_for_turn_start_count(dir: &Path, target: usize, stall_after: Duration, timeout: Duration) {
    poll_with_stall_watchdog(
        || {
            let path = newest_log_file(dir)?;
            (count_turn_starts(&path) >= target).then_some(())
        },
        || total_log_events(dir),
        stall_after,
        timeout,
        |stalled_for, events| {
            format!(
                "no new durable-log event for {stalled_for:?} (stuck at {events} total \
                 events) while waiting for turn_start #{target} under {}",
                dir.display()
            )
        },
        |elapsed, events| {
            format!(
                "hit the {timeout:?} absolute ceiling waiting for turn_start #{target} \
                 under {} ({elapsed:?} elapsed, {events} events observed — still making \
                 progress, just too slowly)",
                dir.display()
            )
        },
    )
}

/// Block until `child` (named `label` for a self-diagnosing panic message)
/// exits, watching `progress` the same way [`wait_for_turn_start_count`]
/// does. A panic here unwinds through the caller's owning [`ChildGuard`],
/// whose `Drop` reaps `child` — this function itself does not kill it.
fn wait_for_exit(
    child: &mut ChildGuard,
    label: &str,
    progress: impl FnMut() -> usize,
    stall_after: Duration,
    timeout: Duration,
) -> std::process::ExitStatus {
    poll_with_stall_watchdog(
        || child.try_wait().expect("poll child process status"),
        progress,
        stall_after,
        timeout,
        |stalled_for, events| {
            format!(
                "{label}: no new durable-log progress for {stalled_for:?} (stuck at \
                 {events} events) — treating as genuinely stuck, not just slow under load"
            )
        },
        |elapsed, events| {
            format!(
                "{label}: hit the {timeout:?} absolute ceiling ({elapsed:?} elapsed, \
                 {events} events observed — still making progress, just too slowly); \
                 still running when killed"
            )
        },
    )
}

/// No `.tmp` sibling survives under `dir` — the atomic-write discipline
/// every persisted-checkpoint format in this wave uses (`.tmp` + rename)
/// never leaves one behind, regardless of which format is current.
fn assert_no_tmp_files(dir: &Path, when: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("tmp") {
            panic!(
                "{when}: found a leftover {} — the checkpoint write is not atomic",
                path.display()
            );
        }
    }
}

/// A driver over an inert answerer `Harness` (never run — its own
/// `ReplayProvider` is empty), built only for its `checkpoint_path()`
/// accessor under the CURRENT `XDG_CACHE_HOME` — the public way to
/// discover where the checkpoint lives without naming its file.
fn checker_driver() -> SelfHarnessDriver {
    let agent_cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(vec![]));
    let writer = LogWriter::create(
        std::env::temp_dir().join(format!(
            "crash-recovery-checker-{}.jsonl",
            std::process::id()
        )),
        &LogHeader {
            prelude_hash: "crash-recovery-checker".into(),
            extract_fingerprint: "crash-recovery-checker".into(),
            harness_version: "test".into(),
        },
    )
    .expect("checker log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("checker harness boots"));
    SelfHarnessDriver::new(agent, Arc::new(LogObserver))
}

fn tail(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Owns a spawned child so a panic anywhere after it's spawned (an
/// assertion failure, a timeout) still reaps it — a leaked
/// `tidepool-selfharness` holds real GHC work and memory on a box shared
/// with sibling worktrees.
struct ChildGuard(std::process::Child);

impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &std::process::Child {
        &self.0
    }
}

impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut std::process::Child {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// This is the only test function in this binary, so its worst case is the
/// binary's worst case against nextest's 1800s (`.config/nextest.toml`
/// profile.default `slow-timeout`) hard-kill. The two waits below are each
/// bounded by an absolute ceiling — [`TURN_START_WAIT_CEILING`] (600s) then
/// [`EXIT_WAIT_CEILING`] (800s) — plus the single `checker_driver` boot
/// compile between them (uncapped, but one real `tidepool-extract` call;
/// generously ~150s under contention) sums to roughly 1550s, leaving a few
/// hundred seconds of real margin rather than running up against the
/// ceiling. [`STALL_WINDOW`] (500s, smaller than either absolute ceiling)
/// is what actually fires first on a genuine hang; the absolute ceilings
/// are the backstop for "technically still progressing, just never
/// finishing."
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_mid_answerer_turn_resumes_from_checkpoint_and_completes() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let scratch = tempfile::tempdir().expect("scratch tempdir");
    let cache_home = scratch.path().join("xdg-cache");
    std::fs::create_dir_all(&cache_home).expect("create XDG_CACHE_HOME");
    // Isolates every path `tidepool_runtime::paths::cache_dir` resolves —
    // for the two spawned children (inherited env) AND this test's own
    // in-process `checker_driver` calls below — from the real
    // `~/.cache/tidepool`.
    std::env::set_var("XDG_CACHE_HOME", &cache_home);
    let selfharness_dir = cache_home.join("tidepool").join("selfharness");

    let log_path = scratch.path().join("replay.jsonl");
    build_replay_log(&log_path, 2);

    let bin = env!("CARGO_BIN_EXE_tidepool-selfharness");
    let harness = harness_path();

    // --- process 1: run cycle 1 to completion, then into cycle 2's answerer
    // turn, then SIGKILL before it commits. ---
    let stdout1 = std::fs::File::create(scratch.path().join("child1.stdout.log")).unwrap();
    let stderr1_path = scratch.path().join("child1.stderr.log");
    let stderr1 = std::fs::File::create(&stderr1_path).unwrap();
    let mut child1 = ChildGuard(
        Command::new(bin)
            .arg("--replay")
            .arg(&log_path)
            .arg("--harness")
            .arg(&harness)
            .env("XDG_CACHE_HOME", &cache_home)
            .stdin(Stdio::null())
            .stdout(stdout1)
            .stderr(stderr1)
            .spawn()
            .expect("spawn tidepool-selfharness (process 1)"),
    );

    // Two TurnStart events: cycle 1's (committed) answerer turn, then cycle
    // 2's answerer turn genuinely starting.
    wait_for_turn_start_count(&selfharness_dir, 2, STALL_WINDOW, TURN_START_WAIT_CEILING);

    // Kill only this test's own child PID — never a pattern match.
    child1.kill().expect("SIGKILL process 1");
    child1.wait().expect("reap process 1");

    assert_no_tmp_files(&selfharness_dir, "immediately after the SIGKILL");

    // The persisted checkpoint must reflect cycle 1's COMMITTED generation,
    // not cycle 2's in-flight (never-committed) work — proving the crash
    // landed between commits, not mid-write.
    let checker = checker_driver();
    let checkpoint_after_crash = persistence::load_checkpoint(checker.checkpoint_path())
        .expect("checkpoint parses cleanly right after the crash")
        .expect("cycle 1 committed a checkpoint before the crash");
    assert_eq!(
        checkpoint_after_crash.generation, 1,
        "checkpoint after the crash must be generation 1 (cycle 1's commit), not \
         generation 0 (no commit yet) or 2 (cycle 2 also committed); got \
         {checkpoint_after_crash:?}"
    );
    assert_eq!(
        checkpoint_after_crash.iteration, 1,
        "generation 1's envelope must carry cycle 1's committed iteration count (1); got \
         {checkpoint_after_crash:?}"
    );

    // --- process 2: same binary, same args, same cache dir — the
    // production restart path. ---
    let stdout2 = std::fs::File::create(scratch.path().join("child2.stdout.log")).unwrap();
    let stderr2_path = scratch.path().join("child2.stderr.log");
    let stderr2 = std::fs::File::create(&stderr2_path).unwrap();
    let mut child2 = ChildGuard(
        Command::new(bin)
            .arg("--replay")
            .arg(&log_path)
            .arg("--harness")
            .arg(&harness)
            .env("XDG_CACHE_HOME", &cache_home)
            .stdin(Stdio::null())
            .stdout(stdout2)
            .stderr(stderr2)
            .spawn()
            .expect("spawn tidepool-selfharness (process 2, restart)"),
    );

    // The replay queue is rebuilt fresh (2 replies) on every process start,
    // so process 2 — if it resumes from the persisted checkpoint — runs 2
    // more committed cycles (iteration 1 -> 2 -> 3) before a 3rd attempt
    // finds the queue empty and the loop exits. This is the test's own
    // designed termination signal, not a claim about the harness's normal
    // shutdown behavior.
    let status2 = wait_for_exit(
        &mut child2,
        "process 2 (restart)",
        || total_log_events(&selfharness_dir),
        STALL_WINDOW,
        EXIT_WAIT_CEILING,
    );
    assert!(
        !status2.success(),
        "process 2 was expected to run out of scripted replies and exit non-zero; \
         stderr:\n{}",
        tail(&stderr2_path)
    );

    assert_no_tmp_files(&selfharness_dir, "after the restarted process exits");

    let checkpoint_after_restart = persistence::load_checkpoint(checker.checkpoint_path())
        .expect("checkpoint parses cleanly after the restarted process exits")
        .expect("a checkpoint still exists after the restart run");
    // The direct durability claim: the restarted process committed
    // generation 2 then 3, continuing from the generation 1 the killed
    // process left — generation 2 alone would mean it restarted from
    // generation 0 (initialState) instead of the persisted checkpoint.
    assert_eq!(
        checkpoint_after_restart.generation,
        3,
        "the restarted process must continue from generation 1 through 2 more \
         committed cycles to reach generation 3; got {checkpoint_after_restart:?}\n\
         stderr:\n{}",
        tail(&stderr2_path)
    );
    assert_eq!(
        checkpoint_after_restart.iteration, 3,
        "generation 3's envelope must carry iteration 3, continuing from cycle 1's \
         persisted iteration (1) through 2 more committed cycles; got \
         {checkpoint_after_restart:?}"
    );
    assert_eq!(
        checkpoint_after_restart
            .state
            .get("mode")
            .and_then(|v| v.as_str()),
        Some("Observing"),
        "Deciding -> Acting -> Observing across the 2 post-restart cycles, continuing \
         the mode chain from cycle 1's persisted Deciding; got {checkpoint_after_restart:?}"
    );
}
