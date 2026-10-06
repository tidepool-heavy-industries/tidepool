//! Process-lifetime policy for the extractor chain.
//!
//! A direct compile has three OS processes (`caller -> frontend -> GHC
//! worker`), while daemon mode has `daemon -> resident GHC worker`.  Every
//! edge uses the same Linux parent-death contract so killing a test, compiler,
//! or daemon cannot leave its expensive child behind.  Keep this here beside
//! the one extractor launcher; callers must not reproduce process-tree policy.

use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};
use std::process::Command;

#[cfg(target_os = "linux")]
const PR_SET_PDEATHSIG: std::os::raw::c_int = 1;
#[cfg(target_os = "linux")]
const SIGKILL: std::os::raw::c_int = 9;
#[cfg(target_os = "linux")]
const EINTR: std::os::raw::c_int = 4;

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn getppid() -> std::os::raw::c_int;
    fn kill(pid: std::os::raw::c_int, signal: std::os::raw::c_int) -> std::os::raw::c_int;
    fn prctl(option: std::os::raw::c_int, ...) -> std::os::raw::c_int;
}

/// Construct an extractor child with the parent-death contract already armed.
/// This is the one production `std::process::Command::new` owner in the crate;
/// worker requests and metadata probes share the same lifecycle contract.
#[allow(
    clippy::disallowed_methods,
    reason = "launcher: the crate's single allowed Command::new call (tidepool/extract-cmd/src/process.rs)"
)]
pub(crate) fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    child_dies_with_parent(&mut command);
    command
}

/// Read a control handshake without blocking beyond its owner deadline.
/// The caller owns the pipe's only reader, so readiness cannot be consumed
/// by another thread between poll and read. Cancellation remains observable
/// even when terminating the child fails to close its inherited output pipe.
pub(crate) fn read_exact_until(
    reader: &mut (impl Read + AsRawFd),
    mut bytes: &mut [u8],
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> io::Result<()> {
    while !bytes.is_empty() {
        if cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "compiler handshake cancelled",
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "compiler handshake deadline exceeded",
            ));
        }
        let timeout = remaining.min(Duration::from_millis(50)).as_millis().max(1) as libc::c_int;
        let mut descriptor = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd refers to a descriptor retained by
        // this reader for the entire call; libc owns the platform's poll ABI.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        if cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "compiler handshake cancelled",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "compiler handshake deadline exceeded",
            ));
        }
        if descriptor.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "compiler handshake pipe closed",
            ));
        }
        match reader.read(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "compiler handshake truncated",
                ))
            }
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    if cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "compiler handshake cancelled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "compiler handshake deadline exceeded",
        ));
    }
    Ok(())
}

pub(crate) fn kill_process(pid: u32) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: the worker remains owned and unreaped by the caller while
        // this PID is used, so it cannot have been recycled for another process.
        if unsafe { kill(pid as std::os::raw::c_int, SIGKILL) } == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "worker cancellation requires Linux",
        ))
    }
}

/// Fence the currently owned descendant tree before an orderly daemon stop.
/// PID reuse after reap cannot turn an unrelated process into our descendant.
pub(crate) fn descendant_snapshot(pid: u32) -> io::Result<Vec<(u32, u64)>> {
    let mut pending = vec![pid];
    let mut snapshot = Vec::new();
    while let Some(parent) = pending.pop() {
        let tasks = match std::fs::read_dir(format!("/proc/{parent}/task")) {
            Ok(tasks) => tasks,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let mut children = std::collections::BTreeSet::new();
        for task in tasks {
            let path = task?.path().join("children");
            let contents = match std::fs::read_to_string(path) {
                Ok(contents) => contents,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            children.extend(contents.split_whitespace().map(str::to_owned));
        }
        for child in children {
            let child = child
                .parse::<u32>()
                .map_err(|_| io::Error::other("invalid owned descendant PID"))?;
            if let Some(start) = process_start_ticks(child)? {
                snapshot.push((child, start));
                pending.push(child);
                if snapshot.len() > 1024 {
                    return Err(io::Error::other(
                        "owned compiler descendant snapshot exceeds bound",
                    ));
                }
            }
        }
    }
    Ok(snapshot)
}

pub(crate) fn process_start_ticks(pid: u32) -> io::Result<Option<u64>> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let start = stat
        .rsplit_once(") ")
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| io::Error::other("invalid owned descendant start ticks"))?;
    Ok(Some(start))
}

/// Arm the current frontend/daemon process to die if its launcher disappears.
///
/// Linux clears `PDEATHSIG` across `fork`, so this is deliberately paired
/// with [`child_dies_with_parent`] at every child edge.
pub(crate) fn current_process_dies_with_parent() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // A live launcher can be PID 1 inside a build's PID namespace.
        // Capture its identity rather than treating that PID as an orphan.
        // SAFETY: getppid takes no arguments or pointers.
        let parent = unsafe { getppid() };
        arm_for_parent(parent)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(())
    }
}

/// Configure `command` so the spawned process dies when this process exits.
fn child_dies_with_parent(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;

        let parent = std::process::id() as std::os::raw::c_int;
        // SAFETY: the closure runs after fork and before exec and calls only
        // async-signal-safe kernel/libc entry points. It allocates no memory
        // and touches no shared Rust state.
        unsafe {
            command.pre_exec(move || arm_for_parent(parent));
        }
    }
}

#[cfg(target_os = "linux")]
fn set_parent_death_signal() -> io::Result<()> {
    // SAFETY: `prctl(PR_SET_PDEATHSIG, signal)` has no pointer arguments. The
    // trailing zeroes are supplied explicitly because `prctl` is variadic.
    let result = unsafe { prctl(PR_SET_PDEATHSIG, SIGKILL, 0usize, 0usize, 0usize) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn arm_for_parent(parent: std::os::raw::c_int) -> io::Result<()> {
    set_parent_death_signal()?;
    // Parent death before prctl delivers no signal. Refuse a changed parent
    // after arming, including reparenting to a subreaper other than init.
    // SAFETY: `getppid` takes no arguments and has no memory-safety contract.
    if unsafe { getppid() } != parent {
        // Avoid allocating in the child hook after fork.
        Err(io::Error::from_raw_os_error(EINTR))
    } else {
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    const HELPER_ENV: &str = "TIDEPOOL_PARENT_DEATH_HELPER";
    const PID_FILE_ENV: &str = "TIDEPOOL_PARENT_DEATH_PID_FILE";
    const TRANSITION_ENV: &str = "TIDEPOOL_PARENT_TRANSITION_DIRECTORY";
    const EXPECTED_PARENT_ENV: &str = "TIDEPOOL_EXPECTED_PARENT";
    const NAMESPACE_ENV: &str = "TIDEPOOL_PARENT_NAMESPACE_HELPER";

    #[test]
    fn handshake_cancellation_interrupts_a_retained_open_writer() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                cancelled.store(true, std::sync::atomic::Ordering::Release);
            });
            let started = Instant::now();
            let error = read_exact_until(
                &mut reader,
                &mut [0],
                started + Duration::from_secs(1),
                || cancelled.load(std::sync::atomic::Ordering::Acquire),
            ).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert!(started.elapsed() < Duration::from_millis(500));
            // EOF cannot explain the interruption: the peer remains owned.
            drop(writer);
        });
    }

    #[test]
    fn handshake_partial_reply_then_eof_is_not_success() {
        use std::io::Write;
        let (mut reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        writer.write_all(&[1, 2]).unwrap();
        drop(writer);
        let mut bytes = [0; 4];
        let error = read_exact_until(
            &mut reader,
            &mut bytes,
            Instant::now() + Duration::from_secs(1),
            || false,
        ).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(&bytes[..2], &[1, 2]);
    }

    #[test]
    fn handshake_ready_or_final_byte_after_deadline_is_refused() {
        use std::io::Write;
        struct DelayedReadyPipe {
            stream: std::os::unix::net::UnixStream,
            delay_before_poll: bool,
        }
        impl AsRawFd for DelayedReadyPipe {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                if self.delay_before_poll {
                    std::thread::sleep(Duration::from_millis(100));
                }
                self.stream.as_raw_fd()
            }
        }
        impl Read for DelayedReadyPipe {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if self.delay_before_poll {
                    panic!("expired readiness must refuse before reading");
                }
                std::thread::sleep(Duration::from_millis(100));
                self.stream.read(bytes)
            }
        }
        for delay_before_poll in [true, false] {
            let (stream, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
            writer.write_all(&[1]).unwrap();
            let mut reader = DelayedReadyPipe { stream, delay_before_poll };
            let error = read_exact_until(
                &mut reader,
                &mut [0],
                Instant::now() + Duration::from_millis(50),
                || false,
            ).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        }
    }

    #[test]
    fn handshake_deadline_is_not_renewed_by_partial_bytes() {
        use std::io::Write;
        let (mut reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                for _ in 0..5 {
                    if writer.write_all(&[1]).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(80));
                }
            });
            let started = Instant::now();
            let mut bytes = [0; 5];
            let error = read_exact_until(
                &mut reader,
                &mut bytes,
                started + Duration::from_millis(150),
                || false,
            ).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert!(started.elapsed() < Duration::from_millis(500));
            drop(reader);
        });
    }

    #[test]
    fn parent_exit_before_signal_setup_refuses_the_child() {
        let work = tempfile::tempdir().unwrap();
        #[allow(
            clippy::disallowed_methods,
            reason = "test: isolated parent-transition fixture"
        )]
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "process::tests::parent_transition_helper",
                "--ignored",
                "--exact",
                "--nocapture",
            ])
            .env(TRANSITION_ENV, work.path())
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let result = work.path().join("result");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !result.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(result).unwrap(), "refused");
    }

    #[test]
    #[ignore = "private subprocess entry for parent-transition acceptance"]
    #[allow(
        clippy::zombie_processes,
        reason = "fixture exits before child signal setup to exercise reparenting"
    )]
    fn parent_transition_helper() {
        let work = std::env::var_os(TRANSITION_ENV)
            .filter(|value| !value.is_empty())
            .expect("private transition helper requires its parent's scratch directory");
        let work = std::path::PathBuf::from(work);
        assert!(
            work.is_dir(),
            "private transition helper scratch directory must exist"
        );
        if let Some(parent) = std::env::var_os(EXPECTED_PARENT_ENV) {
            let parent = parent.to_str().unwrap().parse().unwrap();
            std::fs::write(work.join("ready"), b"ready").unwrap();
            // EOF follows fixture-parent exit, before this child arms its signal.
            std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while unsafe { getppid() } == parent && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_ne!(unsafe { getppid() }, parent);
            assert_eq!(
                arm_for_parent(parent).unwrap_err().kind(),
                io::ErrorKind::Interrupted
            );
            std::fs::write(work.join("result"), b"refused").unwrap();
            return;
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "test: simulate the fork-to-prctl interval without an armed signal"
        )]
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "process::tests::parent_transition_helper",
                "--ignored",
                "--exact",
                "--nocapture",
            ])
            .env(TRANSITION_ENV, &work)
            .env(EXPECTED_PARENT_ENV, std::process::id().to_string())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !work.join("ready").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !work.join("ready").exists() {
            drop(input);
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("parent-transition child did not become ready");
        }
        // Exit closes the pipe and reparents the child before it arms the signal.
        std::process::exit(0);
    }

    #[test]
    #[ignore = "requires unshare and unprivileged PID namespaces"]
    fn live_namespace_pid_one_can_own_frontend_and_child() {
        let executable = std::env::current_exe().unwrap();
        for through_shell in [false, true] {
            #[allow(
                clippy::disallowed_methods,
                reason = "test: launch a fresh PID namespace"
            )]
            let mut namespace = Command::new("unshare");
            namespace
                .args([
                    "--user",
                    "--map-root-user",
                    "--pid",
                    "--fork",
                    "--mount-proc",
                ])
                .env(NAMESPACE_ENV, "1");
            if through_shell {
                namespace.args(["sh", "-c", "\"$1\" process::tests::namespace_parent_helper --ignored --exact --nocapture; result=$?; exit \"$result\"", "sh"])
                    .arg(&executable);
            } else {
                namespace.arg(&executable).args([
                    "process::tests::namespace_parent_helper",
                    "--ignored",
                    "--exact",
                    "--nocapture",
                ]);
            }
            assert!(namespace.status().unwrap().success());
        }
    }

    #[test]
    #[ignore = "private subprocess entry for PID-namespace acceptance"]
    fn namespace_parent_helper() {
        assert_eq!(
            std::env::var(NAMESPACE_ENV)
                .expect("private namespace helper requires its parent marker"),
            "1"
        );
        let parent = unsafe { getppid() };
        assert_eq!(parent, if std::process::id() == 1 { 0 } else { 1 });
        eprintln!("namespace process={} parent={parent}", std::process::id());
        current_process_dies_with_parent().unwrap();
        #[allow(
            clippy::disallowed_methods,
            reason = "test: exercise the owned child hook in the PID namespace"
        )]
        let mut child = Command::new("true");
        child_dies_with_parent(&mut child);
        assert!(child.status().unwrap().success());
    }

    #[test]
    fn configured_child_dies_when_its_parent_exits() {
        let pid_file = std::env::temp_dir().join(format!(
            "tidepool-parent-death-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        // best-effort: test cleanup of a temp path from a prior run.
        std::fs::remove_file(&pid_file).ok();

        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: re-execs this test binary as a parent-death helper, not a production launch site"
        )]
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("process::tests::parent_death_helper")
            .arg("--ignored")
            .arg("--exact")
            .arg("--nocapture")
            .env(HELPER_ENV, "1")
            .env(PID_FILE_ENV, &pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "parent-death helper failed: {status}");

        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let proc_entry = std::path::PathBuf::from(format!("/proc/{pid}"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while proc_entry.exists() && Instant::now() < deadline {
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync poll loop waiting for the helper child to exit"
            )]
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !proc_entry.exists(),
            "child {pid} survived after its configured parent exited"
        );
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(pid_file).ok();
    }

    #[test]
    #[ignore = "private subprocess entry for parent-death acceptance"]
    #[allow(
        clippy::zombie_processes,
        reason = "the helper must exit without waiting to test parent-death cleanup"
    )]
    fn parent_death_helper() {
        assert_eq!(
            std::env::var(HELPER_ENV)
                .expect("private parent-death helper requires its parent marker"),
            "1"
        );
        let pid_file = std::env::var_os(PID_FILE_ENV).expect("helper pid file is required");
        let mut command = command("sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().expect("spawn helper child");
        std::fs::write(pid_file, child.id().to_string()).expect("write helper child pid");
        // Dropping `Child` deliberately does not wait or kill. Exiting this
        // helper process is the behavior under test.
    }
}
