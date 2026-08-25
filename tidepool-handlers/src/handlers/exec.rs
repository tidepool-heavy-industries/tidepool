use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tidepool_bridge_effects::Proc;

// ============================================================================
// Tag 4: Exec (shell commands)
// ============================================================================

// ExecReq, ExecError, DescribeEffect and the EffectHandler dispatch are
// GENERATED from the `tidepool-protocol` schema — re-exported
// here so the public paths (`tidepool_handlers::ExecReq`) are unchanged. Only
// the handler struct and the per-verb method bodies below are hand-written.
pub use crate::generated::exec::{ExecError, ExecReq};

/// **Exec is not filesystem-sandboxed** — see `tidepool-handlers/CLAUDE.md`'s
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
    /// `tidepool-mcp/src/lib.rs`): long enough for an ordinary build/test
    /// command to finish comfortably inside one eval's own timeout interval,
    /// short enough that a hung command cannot wedge a resident turn
    /// forever. Override with `TIDEPOOL_EXEC_TIMEOUT_SECS` (whole seconds).
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
        let mut command = Command::new("sh");
        command.arg("-c").arg(cmd).current_dir(dir);
        Self::spawn_and_capture(command, Self::exec_timeout())
    }

    /// Spawn `command`, capture stdout/stderr with the [`MAX_EXEC_OUTPUT_BYTES`]
    /// cap enforced DURING the read (bytes past the cap are drained, never
    /// retained — so a runaway producer cannot balloon our memory before the
    /// cap can apply), and kill the whole process group if the command
    /// outlives `timeout`.
    ///
    /// [`MAX_EXEC_OUTPUT_BYTES`]: Self::MAX_EXEC_OUTPUT_BYTES
    fn spawn_and_capture(mut command: Command, timeout: Duration) -> Result<Proc, ExecError> {
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
        let stdout_thread = std::thread::spawn(move || read_capped(stdout_pipe));
        let stderr_thread = std::thread::spawn(move || read_capped(stderr_pipe));

        let deadline = Instant::now() + timeout;
        let exited = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                // A `try_wait` failure post-spawn is not a documented failure
                // mode of this API in practice; treat it the same as a
                // deadline miss (kill and report) rather than panicking.
                Err(_) => break None,
            }
        };

        match exited {
            Some(status) => {
                let stdout = stdout_thread.join().unwrap_or_default();
                let stderr = stderr_thread.join().unwrap_or_default();
                let exit_code = status.code().unwrap_or(-1) as i64;
                Ok(build_proc(stdout, stderr, exit_code))
            }
            None => {
                Self::kill_process_group(pid);
                // The kill closes every process's copy of the pipe write
                // ends in the group, which unblocks the reader threads with
                // EOF; `wait()` then reaps the zombie.
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                Err(ExecError::ExecTimeout(format!(
                    "command exceeded its {}s timeout and was killed",
                    timeout.as_secs()
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
/// and discarded.
fn read_capped(mut r: impl Read) -> CapturedStream {
    let mut out = CapturedStream::default();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let free = ExecHandler::MAX_EXEC_OUTPUT_BYTES.saturating_sub(out.buf.len());
                let take = free.min(n);
                out.buf.extend_from_slice(&chunk[..take]);
                if take < n {
                    out.truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    out
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
    // timeout — `Err` is ExecSpawn, ExecBadDir, or ExecTimeout.
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
        let mut command = Command::new(&argv[0]);
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

    /// Bundles `exec_run_in_bad_dir_is_typed_left_execbaddir` (#335 end-to-end
    /// acceptance: `runIn` with a bad/escaping directory is a typed
    /// `Left (ExecBadDir _)` the eval pattern-matches, never an abort) +
    /// `exec_run_existing_command_is_right` (the happy path still threads
    /// through the Either: `run cmd >>= liftEither` yields the Proc) into one
    /// tidepool-extract compile. Returns the list of FAILED check names
    /// (empty on success) — see
    /// `tidepool-runtime/tests/generic_form_roundtrip.rs`'s `check` helper.
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
