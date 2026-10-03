use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tidepool_bridge_effects::Proc;

// ============================================================================
// Tag 5: Exec (shell commands)
// ============================================================================

// ExecReq, ExecError, DescribeEffect and the EffectHandler dispatch are
// GENERATED from the `tidepool-protocol` schema — re-exported
// here so the public paths (`tidepool_handlers::ExecReq`) are unchanged. Only
// the handler struct and the per-verb method bodies below are hand-written.
pub use crate::generated::exec::{ExecError, ExecReq};

/// **Exec is not filesystem-sandboxed** — see `bridge/handlers/CLAUDE.md`'s
/// Sandboxing section for the full statement. `root` sets only the initial
/// working directory of a spawned command (and bounds `runIn`'s `dir`
/// argument); the command itself runs as an ordinary unrestricted host
/// process. Containment here is process-level, not filesystem-level: bounded
/// streaming output capture and a timeout with process-group termination
/// (below) — not a landlock/seccomp/namespace boundary.
#[derive(Clone)]
pub struct ExecHandler {
    root: PathBuf,
}

impl ExecHandler {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    const MAX_EXEC_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

    /// Default exec timeout in seconds — aligned with `tidepool-mcp`'s
    /// eval-timeout default (`EVAL_TIMEOUT_SECS` = 600s, see
    /// `bridge/mcp/src/lib.rs`): long enough for an ordinary build/test
    /// command to finish comfortably inside one eval's own timeout interval,
    /// short enough that hung execution or output draining cannot wedge a
    /// resident turn forever. Override with `TIDEPOOL_EXEC_TIMEOUT_SECS`.
    const DEFAULT_EXEC_TIMEOUT_SECS: u64 = 600;

    /// Resolved once per call (not cached) so a changed env var takes effect
    /// on the next `run`/`runIn`/`runArgv` without a restart. An unset,
    /// non-positive, or unparseable value falls back to the default.
    fn exec_timeout() -> Duration {
        let secs = std::env::var("TIDEPOOL_EXEC_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&s| s > 0)
            .unwrap_or(Self::DEFAULT_EXEC_TIMEOUT_SECS);
        Duration::from_secs(secs)
    }

    /// Bounds which directory `runIn`'s `dir` argument may resolve to — it
    /// says nothing about what the spawned command can subsequently touch
    /// (Exec has no filesystem sandbox; see the struct doc above).
    fn resolve_dir(&self, rel: &str) -> Result<PathBuf, ExecError> {
        let target = self.root.join(rel);
        let canonical_root = self
            .root
            .canonicalize()
            .map_err(|e| ExecError::ExecBadDir(e.to_string()))?;
        let canonical = target
            .canonicalize()
            .map_err(|e| ExecError::ExecBadDir(format!("Cannot resolve directory: {}", e)))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(ExecError::ExecBadDir(format!(
                "Path escapes sandbox: {}",
                rel
            )));
        }
        Ok(canonical)
    }

    fn run_command(&self, cmd: &str, dir: &std::path::Path) -> Result<Proc, ExecError> {
        // Exec's whole point is running an arbitrary, caller-named host
        // command (see this crate's CLAUDE.md, "Exec is NOT
        // filesystem-sandboxed") — it is not a fixed process this crate owns
        // the lifecycle of. `spawn_and_capture` gives it its own
        // process-group timeout/kill, which is this handler's substitute for
        // the launcher's die-with-owner guarantee; `exomonad_node`'s
        // `host_command::command` is still the one place that builds the
        // `Command`, matching every other caller in this shape.
        let mut command: Command = exomonad_node::host_command::command("sh");
        command.arg("-c").arg(cmd).current_dir(dir);
        Self::spawn_and_capture(command, Self::exec_timeout())
    }

    /// Spawn `command`, capture stdout/stderr with the [`MAX_EXEC_OUTPUT_BYTES`]
    /// cap enforced DURING the read (bytes past the cap are drained, never
    /// retained — so a runaway producer cannot balloon our memory before the
    /// cap can apply), and kill the whole process group if execution or output
    /// draining outlives `timeout`.
    ///
    /// [`MAX_EXEC_OUTPUT_BYTES`]: Self::MAX_EXEC_OUTPUT_BYTES
    fn spawn_and_capture(command: Command, timeout: Duration) -> Result<Proc, ExecError> {
        Self::spawn_and_capture_with(command, timeout, |child| child.try_wait())
    }

    fn spawn_and_capture_with(
        mut command: Command,
        timeout: Duration,
        mut try_wait: impl FnMut(&mut Child) -> io::Result<Option<ExitStatus>>,
    ) -> Result<Proc, ExecError> {
        command
            // Never inherit our own stdin: the resident server may be
            // reading it for its own transport, and a command with no input
            // of its own should see EOF, not block forever.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // New process group (pgid = the child's own pid): isolates the
            // command — and, for `sh -c`, everything it forks — from our own
            // group so a timeout kill can target the whole tree without
            // touching the resident server itself.
            command.process_group(0);
        }

        let mut child = command
            .spawn()
            .map_err(|e| ExecError::ExecSpawn(format!("exec failed: {}", e)))?;
        let pid = child.id();

        // `.take()` cannot genuinely miss (both were just set to `piped()`
        // above) — routed through the typed error rather than
        // `expect`/`unwrap` to stay total, matching this crate's
        // `#![warn(clippy::expect_used)]`.
        let stdout_pipe = child.stdout.take().ok_or_else(|| {
            ExecError::ExecSpawn("exec failed: spawned child has no stdout pipe".to_string())
        })?;
        let stderr_pipe = child.stderr.take().ok_or_else(|| {
            ExecError::ExecSpawn("exec failed: spawned child has no stderr pipe".to_string())
        })?;
        if let Err(error) = set_pipe_nonblocking(&stdout_pipe) {
            Self::kill_process_group(pid);
            child.wait().ok();
            return Err(ExecError::ExecOutput(format!(
                "could not configure stdout reader: {error}"
            )));
        }
        if let Err(error) = set_pipe_nonblocking(&stderr_pipe) {
            Self::kill_process_group(pid);
            child.wait().ok();
            return Err(ExecError::ExecOutput(format!(
                "could not configure stderr reader: {error}"
            )));
        }
        let cancel_readers = Arc::new(AtomicBool::new(false));
        let stdout_thread = spawn_reader(stdout_pipe, cancel_readers.clone());
        let stderr_thread = spawn_reader(stderr_pipe, cancel_readers.clone());

        let deadline = Instant::now() + timeout;
        let outcome = loop {
            match try_wait(&mut child) {
                Ok(Some(status)) => break WaitOutcome::Exited(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        break WaitOutcome::TimedOut;
                    }
                    // `spawn_and_capture` is a synchronous method invoked
                    // from `EffectHandler` dispatch on its own dedicated
                    // thread, not from async code on a shared executor —
                    // see `.clippy.toml`'s dedicated-sync-thread exemption.
                    #[allow(
                        clippy::disallowed_methods,
                        reason = "poll loop on a dedicated sync handler thread, not an async executor thread"
                    )]
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => break WaitOutcome::WaitFailed(error),
            }
        };

        match outcome {
            WaitOutcome::Exited(status) => {
                #[cfg(unix)]
                if !wait_for_readers(&stdout_thread, &stderr_thread, deadline) {
                    Self::kill_process_group(pid);
                    child.wait().ok();
                    cancel_readers.store(true, Ordering::Relaxed);
                    let _ = join_readers(stdout_thread, stderr_thread);
                    return Err(ExecError::ExecTimeout(format!(
                        "command output streams remained open past the {}s timeout; process group was killed",
                        timeout.as_secs()
                    )));
                }
                let (stdout, stderr) = join_readers(stdout_thread, stderr_thread)?;
                let exit_code = status.code().unwrap_or(-1) as i64;
                Ok(build_proc(stdout, stderr, exit_code))
            }
            WaitOutcome::TimedOut => {
                Self::kill_process_group(pid);
                // Killing the process group closes its pipe write ends, then
                // wait reaps the child. Cancellation also releases readers if
                // a process escaped the group while retaining a pipe.
                child.wait().ok();
                cancel_readers.store(true, Ordering::Relaxed);
                let _ = join_readers(stdout_thread, stderr_thread);
                Err(ExecError::ExecTimeout(format!(
                    "command or output draining exceeded its {}s timeout; process group was killed",
                    timeout.as_secs()
                )))
            }
            WaitOutcome::WaitFailed(error) => {
                // Preserve process ownership and cleanup even when status
                // collection fails: terminate the group, reap the child, and
                // join both readers before returning the distinct wait error.
                Self::kill_process_group(pid);
                child.wait().ok();
                cancel_readers.store(true, Ordering::Relaxed);
                let _ = join_readers(stdout_thread, stderr_thread);
                Err(ExecError::ExecWait(format!(
                    "waiting for command failed: {error}"
                )))
            }
        }
    }

    #[cfg(unix)]
    fn kill_process_group(pid: u32) {
        // SAFETY: `kill(2)` is always safe to call; a negative pid targets
        // the whole process group rather than a single process. Best-effort:
        // ESRCH (the group already exited on its own) is not an error worth
        // surfacing.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }

    #[cfg(not(unix))]
    fn kill_process_group(pid: u32) {
        // No process-group primitive off Unix; best effort on the direct
        // child only. `pid` is otherwise unused on this platform.
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    pipe: R,
    cancelled: Arc<AtomicBool>,
) -> std::thread::JoinHandle<io::Result<CapturedStream>> {
    #[cfg(unix)]
    {
        std::thread::spawn(move || read_capped_with_cancel(pipe, &cancelled))
    }
    #[cfg(not(unix))]
    {
        let _ = cancelled;
        std::thread::spawn(move || read_capped(pipe))
    }
}

#[cfg(unix)]
fn wait_for_readers(
    stdout: &std::thread::JoinHandle<io::Result<CapturedStream>>,
    stderr: &std::thread::JoinHandle<io::Result<CapturedStream>>,
    deadline: Instant,
) -> bool {
    while !stdout.is_finished() || !stderr.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "reader completion is supervised on the dedicated sync handler thread"
        )]
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

#[cfg(unix)]
fn set_pipe_nonblocking(pipe: &impl std::os::fd::AsRawFd) -> io::Result<()> {
    // SAFETY: fcntl only reads and updates flags on this live pipe descriptor.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: same live descriptor; preserve its current flags while enabling O_NONBLOCK.
    let result = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_pipe_nonblocking(_pipe: &impl Read) -> io::Result<()> {
    Ok(())
}

enum WaitOutcome {
    Exited(ExitStatus),
    TimedOut,
    WaitFailed(io::Error),
}

/// One captured stream: bytes retained up to the cap, plus whether more
/// arrived beyond it.
#[derive(Default)]
struct CapturedStream {
    buf: Vec<u8>,
    truncated: bool,
}

/// Read `r` to EOF, retaining at most `ExecHandler::MAX_EXEC_OUTPUT_BYTES`.
/// Reading never stops early at the cap — a full child pipe backs the child
/// up and can wedge it — only RETENTION does; bytes past the cap are read
/// and discarded. Interrupted reads are retried.
#[cfg(any(test, not(unix)))]
fn read_capped(r: impl Read) -> io::Result<CapturedStream> {
    read_capped_inner(r, None)
}

#[cfg(unix)]
fn read_capped_with_cancel(r: impl Read, cancelled: &AtomicBool) -> io::Result<CapturedStream> {
    read_capped_inner(r, Some(cancelled))
}

fn read_capped_inner(
    mut r: impl Read,
    cancelled: Option<&AtomicBool>,
) -> io::Result<CapturedStream> {
    let mut out = CapturedStream::default();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Ok(out);
        }
        match r.read(&mut chunk) {
            Ok(0) => return Ok(out),
            Ok(n) => {
                let free = ExecHandler::MAX_EXEC_OUTPUT_BYTES.saturating_sub(out.buf.len());
                let take = free.min(n);
                out.buf.extend_from_slice(&chunk[..take]);
                if take < n {
                    out.truncated = true;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock && cancelled.is_some() => {
                #[allow(
                    clippy::disallowed_methods,
                    reason = "nonblocking pipe reader yields while waiting for more output"
                )]
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error),
        }
    }
}

fn join_readers(
    stdout_thread: std::thread::JoinHandle<io::Result<CapturedStream>>,
    stderr_thread: std::thread::JoinHandle<io::Result<CapturedStream>>,
) -> Result<(CapturedStream, CapturedStream), ExecError> {
    // Join both before propagating either failure so a bad stream never leaks
    // the other reader thread.
    let stdout = stdout_thread.join();
    let stderr = stderr_thread.join();
    let stdout = stdout
        .map_err(|_| ExecError::ExecOutput("stdout reader panicked".to_string()))?
        .map_err(|error| ExecError::ExecOutput(format!("stdout reader failed: {error}")))?;
    let stderr = stderr
        .map_err(|_| ExecError::ExecOutput("stderr reader panicked".to_string()))?
        .map_err(|error| ExecError::ExecOutput(format!("stderr reader failed: {error}")))?;
    Ok((stdout, stderr))
}

/// Decode both captured streams (lossily — a cap boundary landing mid
/// multi-byte character is replaced, not hand-walked back to the nearest
/// char boundary; simpler than the old post-hoc truncation and just as
/// safe) and build the resulting `Proc`.
fn build_proc(stdout: CapturedStream, stderr: CapturedStream, exit_code: i64) -> Proc {
    let mut stdout_s = String::from_utf8_lossy(&stdout.buf).into_owned();
    if stdout.truncated {
        stdout_s.push_str("\n...[truncated at 2MB]");
    }
    let mut stderr_s = String::from_utf8_lossy(&stderr.buf).into_owned();
    if stderr.truncated {
        stderr_s.push_str("\n...[truncated at 2MB]");
    }
    Proc {
        exit_code,
        stdout: stdout_s,
        stderr: stderr_s,
    }
}

impl ExecHandler {
    // `pub(crate)` rather than private: the generated dispatch arm lives in a
    // sibling module (`crate::generated::exec`) now, not expanded inline here.
    // Errors-tagged verbs: total in `ExecError`, no `cx` — the dispatch arm
    // wraps the `Result` via `cx.respond` (Ok→Right, Err→Left). See #335. A
    // nonzero EXIT is not a failure: `run_command`/`exec_run_argv` return
    // `Ok(Proc { .. })` once the process spawns and finishes within its
    // timeout — `Err` is ExecSpawn, ExecBadDir, ExecTimeout, ExecOutput, or ExecWait.
    pub(crate) fn exec_run(&mut self, cmd: String) -> Result<Proc, ExecError> {
        self.run_command(&cmd, &self.root.clone())
    }

    pub(crate) fn exec_run_in(&mut self, dir: String, cmd: String) -> Result<Proc, ExecError> {
        let target = self.resolve_dir(&dir)?;
        self.run_command(&cmd, &target)
    }

    pub(crate) fn exec_run_argv(&mut self, argv: Vec<String>) -> Result<Proc, ExecError> {
        if argv.is_empty() {
            return Err(ExecError::ExecSpawn("runArgv: empty argv".to_string()));
        }
        // Same reasoning as `run_command` above — an arbitrary caller-named
        // command with its own process-group timeout/kill, built through the
        // launcher's synchronous `command` constructor.
        let mut command: Command = exomonad_node::host_command::command(&argv[0]);
        command.args(&argv[1..]).current_dir(&self.root);
        Self::spawn_and_capture(command, Self::exec_timeout())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    /// A command well within its timeout returns normally.
    #[test]
    fn exec_run_completes_before_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let mut handler = ExecHandler::new(dir.path().to_path_buf());
        let proc = handler.exec_run("echo hi".to_string()).unwrap();
        assert_eq!(proc.stdout, "hi\n");
        assert_eq!(proc.exit_code, 0);
    }

    /// A hung command must not wedge the caller forever: with a short
    /// injected timeout (`TIDEPOOL_EXEC_TIMEOUT_SECS`), a command that
    /// outlives it comes back as a typed `Left (ExecTimeout _)` — and the
    /// WHOLE process group is killed, not just the top-level `sh`. A
    /// backgrounded grandchild (sharing the group via `process_group(0)`,
    /// since a non-interactive `sh` does no job-control pgrp reassignment)
    /// must die too, proven by its recorded pid going unsignalable shortly
    /// after the call returns.
    #[test]
    fn exec_timeout_kills_process_group_including_grandchild() {
        std::env::set_var("TIDEPOOL_EXEC_TIMEOUT_SECS", "1");
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let mut handler = ExecHandler::new(dir.path().to_path_buf());

        let cmd = format!("sleep 30 & echo $! > {} ; wait", pid_file.display());
        let started = std::time::Instant::now();
        let result = handler.exec_run(cmd);
        let elapsed = started.elapsed();
        std::env::remove_var("TIDEPOOL_EXEC_TIMEOUT_SECS");

        match result {
            Err(ExecError::ExecTimeout(_)) => {}
            Err(other) => panic!("expected Left (ExecTimeout _), got Err({other:?})"),
            Ok(proc) => panic!(
                "expected Left (ExecTimeout _), got Ok(Proc {{ exit_code: {}, .. }})",
                proc.exit_code
            ),
        }
        // Returned well inside the 30s the grandchild would otherwise sleep for.
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "took {elapsed:?}"
        );

        let grandchild_pid: i32 = std::fs::read_to_string(&pid_file)
            .expect("backgrounded grandchild should have written its pid")
            .trim()
            .parse()
            .expect("pid file should contain a plain pid");
        // Give the SIGKILL a brief moment to land, then confirm the
        // grandchild — not just the top-level `sh` — is actually gone.
        let mut alive = true;
        #[allow(
            clippy::disallowed_methods,
            reason = "short synchronous probe in a test: poll `kill -0` for the \
                      grandchild's death, not a long-lived child needing the launcher"
        )]
        for _ in 0..40 {
            let status = std::process::Command::new("kill")
                .args(["-0", &grandchild_pid.to_string()])
                .status()
                .unwrap();
            if !status.success() {
                alive = false;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            !alive,
            "grandchild pid {grandchild_pid} is still alive after the process-group kill"
        );
    }

    /// The argv (shell-free) spawn path times out and is killed the same way.
    #[test]
    fn exec_run_argv_timeout_returns_typed_error() {
        std::env::set_var("TIDEPOOL_EXEC_TIMEOUT_SECS", "1");
        let dir = tempfile::tempdir().unwrap();
        let mut handler = ExecHandler::new(dir.path().to_path_buf());
        let started = std::time::Instant::now();
        let result = handler.exec_run_argv(vec!["sleep".to_string(), "30".to_string()]);
        let elapsed = started.elapsed();
        std::env::remove_var("TIDEPOOL_EXEC_TIMEOUT_SECS");

        match result {
            Err(ExecError::ExecTimeout(_)) => {}
            Err(other) => panic!("expected Left (ExecTimeout _), got Err({other:?})"),
            Ok(proc) => panic!(
                "expected Left (ExecTimeout _), got Ok(Proc {{ exit_code: {}, .. }})",
                proc.exit_code
            ),
        }
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "took {elapsed:?}"
        );
    }

    /// A runaway producer past `MAX_EXEC_OUTPUT_BYTES` is truncated, and the
    /// truncation marker is present — the streaming cap fired during the
    /// read (bytes past the cap were drained, never retained), not after
    /// unbounded buffering.
    #[test]
    fn exec_output_cap_enforced_during_stream() {
        let dir = tempfile::tempdir().unwrap();
        let mut handler = ExecHandler::new(dir.path().to_path_buf());
        // ~5MB of 'x', well past the 2MiB cap.
        let proc = handler
            .exec_run("head -c 5000000 /dev/zero | tr '\\0' 'x'".to_string())
            .expect("a nonzero-output command is not itself a failure");
        assert!(
            proc.stdout.len()
                <= ExecHandler::MAX_EXEC_OUTPUT_BYTES + "\n...[truncated at 2MB]".len()
        );
        assert!(proc.stdout.ends_with("...[truncated at 2MB]"));
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("injected read failure"))
        }
    }

    struct InterruptOnceReader {
        pending: &'static [u8],
        interrupted: bool,
    }

    impl Read for InterruptOnceReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            let n = self.pending.len().min(buf.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending = &self.pending[n..];
            Ok(n)
        }
    }

    #[test]
    fn exec_reader_retries_interrupted_read() {
        let captured = read_capped(InterruptOnceReader {
            pending: b"captured",
            interrupted: false,
        })
        .unwrap();
        assert_eq!(captured.buf, b"captured");
    }

    struct ObservedReader(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl Read for ObservedReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(0)
        }
    }

    #[cfg(target_os = "linux")]
    struct DetachedChildCleanup {
        pid_file: PathBuf,
        pid: Option<i32>,
        terminated: bool,
    }

    #[cfg(target_os = "linux")]
    impl DetachedChildCleanup {
        fn recorded_pid(&mut self) -> Option<i32> {
            self.pid = self.pid.or_else(|| {
                std::fs::read_to_string(&self.pid_file)
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            });
            self.pid
        }

        fn terminate(&mut self) -> Option<i32> {
            let pid = self.recorded_pid();
            if !self.terminated {
                if let Some(pid) = pid {
                    // SAFETY: this test owns the detached helper PID written by its shell fixture.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
                self.terminated = true;
            }
            pid
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for DetachedChildCleanup {
        fn drop(&mut self) {
            self.terminate();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exec_leader_exit_with_detached_pipe_writer_is_bounded_and_cancellable() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("detached.pid");
        let mut cleanup = DetachedChildCleanup {
            pid_file: pid_file.clone(),
            pid: None,
            terminated: false,
        };
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "setsid sleep 5 & echo $! > {}; exit 0",
            pid_file.display()
        ));
        let started = Instant::now();
        let result =
            ExecHandler::spawn_and_capture_with(command, Duration::from_millis(250), |child| {
                child.try_wait()
            });
        let detached_pid = cleanup.recorded_pid();
        let escaped_session_alive = detached_pid.is_some_and(|pid| {
            // SAFETY: signal zero probes the fixture PID without changing it.
            unsafe { libc::getsid(pid) == pid }
        });
        cleanup.terminate();

        assert!(matches!(
            result,
            Err(ExecError::ExecTimeout(detail)) if detail.contains("output streams remained open")
        ));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            escaped_session_alive,
            "fixture child did not escape the process group"
        );
    }

    #[test]
    fn exec_reader_error_is_typed_and_both_readers_are_joined() {
        let stderr_finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stdout_thread = std::thread::spawn(|| read_capped(FailingReader));
        let stderr_thread = {
            let finished = stderr_finished.clone();
            std::thread::spawn(move || read_capped(ObservedReader(finished)))
        };

        let result = join_readers(stdout_thread, stderr_thread);

        assert!(matches!(
            result,
            Err(ExecError::ExecOutput(detail)) if detail.contains("stdout reader failed")
        ));
        assert!(stderr_finished.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn exec_reader_panic_is_typed() {
        let stdout_thread = std::thread::spawn(|| -> io::Result<CapturedStream> {
            panic!("injected reader panic")
        });
        let stderr_thread = std::thread::spawn(|| read_capped(io::empty()));

        let result = join_readers(stdout_thread, stderr_thread);

        assert!(matches!(
            result,
            Err(ExecError::ExecOutput(detail)) if detail == "stdout reader panicked"
        ));
    }

    #[test]
    fn exec_wait_failure_is_typed_and_terminates_child() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("sleep 30");
        let result =
            ExecHandler::spawn_and_capture_with(command, std::time::Duration::from_secs(5), |_| {
                Err(io::Error::other("injected wait failure"))
            });

        assert!(matches!(
            result,
            Err(ExecError::ExecWait(detail)) if detail.contains("injected wait failure")
        ));
    }

    /// Bundles `exec_run_in_bad_dir_is_typed_left_execbaddir` (#335 end-to-end
    /// acceptance: `runIn` with a bad/escaping directory is a typed
    /// `Left (ExecBadDir _)` the eval pattern-matches, never an abort) +
    /// `exec_run_existing_command_is_right` (the happy path still threads
    /// through the Either: `run cmd >>= liftEither` yields the Proc) into one
    /// tidepool-extract compile. Returns the list of FAILED check names
    /// (empty on success) — see
    /// `tidepool/runtime/tests/generic_form_roundtrip.rs`'s `check` helper.
    #[tokio::test]
    async fn test_jit_exec_family() {
        let result = jit_eval(&[
            "let check nm ok = if ok then [] else [nm]",
            "badDir <- runIn \"../../nope-335\" \"echo hi\"",
            "let badDirOk = case badDir of { Left (ExecBadDir _) -> True; _ -> False }",
            "p <- run \"echo hi\" >>= liftEither",
            "let c1 = check \"exec-run-in-bad-dir-is-typed-left-execbaddir\" badDirOk",
            "let c2 = check \"exec-run-existing-command-is-right\" (ok p)",
            "pure (concat [c1, c2])",
        ]);
        assert_eq!(result, serde_json::json!([]), "failed checks: {result}");
    }
}
