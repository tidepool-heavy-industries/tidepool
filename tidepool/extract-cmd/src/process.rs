//! Process-lifetime policy for the extractor chain.
//!
//! A direct compile has three OS processes (`caller -> frontend -> GHC
//! worker`), while daemon mode has `daemon -> resident GHC worker`.  Every
//! edge uses the same Linux parent-death contract so killing a test, compiler,
//! or daemon cannot leave its expensive child behind.  Keep this here beside
//! the one extractor launcher; callers must not reproduce process-tree policy.

use std::io;
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

/// Construct a `Command` for a long-lived extractor child. This is the one
/// allowed `std::process::Command::new` call in the crate; every other site
/// routes through it instead of constructing its own. Callers still arm the
/// parent-death contract themselves with [`child_dies_with_parent`] once the
/// command is otherwise configured.
#[allow(
    clippy::disallowed_methods,
    reason = "launcher: the crate's single allowed Command::new call (tidepool/extract-cmd/src/process.rs)"
)]
pub(crate) fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    Command::new(program)
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
pub(crate) fn child_dies_with_parent(command: &mut Command) {
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
    fn parent_exit_before_signal_setup_refuses_the_child() {
        let work = tempfile::tempdir().unwrap();
        #[allow(
            clippy::disallowed_methods,
            reason = "test: isolated parent-transition fixture"
        )]
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "process::tests::parent_transition_helper",
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
    #[allow(
        clippy::zombie_processes,
        reason = "fixture exits before child signal setup to exercise reparenting"
    )]
    fn parent_transition_helper() {
        let Some(work) = std::env::var_os(TRANSITION_ENV) else {
            return;
        };
        let work = std::path::PathBuf::from(work);
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
                namespace.args(["sh", "-c", "\"$1\" process::tests::namespace_parent_helper --exact --nocapture; result=$?; exit \"$result\"", "sh"])
                    .arg(&executable);
            } else {
                namespace.arg(&executable).args([
                    "process::tests::namespace_parent_helper",
                    "--exact",
                    "--nocapture",
                ]);
            }
            assert!(namespace.status().unwrap().success());
        }
    }

    #[test]
    fn namespace_parent_helper() {
        if std::env::var_os(NAMESPACE_ENV).is_none() {
            return;
        }
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
    #[allow(
        clippy::zombie_processes,
        reason = "the helper must exit without waiting to test parent-death cleanup"
    )]
    fn parent_death_helper() {
        if std::env::var_os(HELPER_ENV).is_none() {
            return;
        }
        let pid_file = std::env::var_os(PID_FILE_ENV).expect("helper pid file is required");
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: throwaway helper child, exercises child_dies_with_parent directly"
        )]
        let mut command = Command::new("sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        child_dies_with_parent(&mut command);
        let child = command.spawn().expect("spawn helper child");
        std::fs::write(pid_file, child.id().to_string()).expect("write helper child pid");
        // Dropping `Child` deliberately does not wait or kill. Exiting this
        // helper process is the behavior under test.
    }
}
