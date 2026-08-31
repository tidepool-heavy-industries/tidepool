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
unsafe extern "C" {
    fn getppid() -> std::os::raw::c_int;
    fn prctl(option: std::os::raw::c_int, ...) -> std::os::raw::c_int;
}

/// Arm the current frontend/daemon process to die if its launcher disappears.
///
/// Linux clears `PDEATHSIG` across `fork`, so this is deliberately paired
/// with [`child_dies_with_parent`] at every child edge.
pub(crate) fn current_process_dies_with_parent() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        set_parent_death_signal()?;
        reject_already_orphaned()
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

        // SAFETY: the closure runs after fork and before exec and calls only
        // async-signal-safe kernel/libc entry points. It allocates no memory
        // and touches no shared Rust state.
        unsafe {
            command.pre_exec(|| {
                set_parent_death_signal()?;
                reject_already_orphaned()
            });
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
fn reject_already_orphaned() -> io::Result<()> {
    // There is an unavoidable interval between `fork` and `prctl`. If the
    // parent died inside it, no signal was delivered; observing init as the
    // parent closes that race by refusing to continue into `exec`.
    // SAFETY: `getppid` takes no arguments and has no memory-safety contract.
    if unsafe { getppid() } == 1 {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "extractor parent exited before child initialization",
        ))
    } else {
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    const HELPER_ENV: &str = "TIDEPOOL_PARENT_DEATH_HELPER";
    const PID_FILE_ENV: &str = "TIDEPOOL_PARENT_DEATH_PID_FILE";

    #[test]
    fn configured_child_dies_when_its_parent_exits() {
        let pid_file = std::env::temp_dir().join(format!(
            "tidepool-parent-death-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_file(&pid_file);

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
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !proc_entry.exists(),
            "child {pid} survived after its configured parent exited"
        );
        let _ = std::fs::remove_file(pid_file);
    }

    #[test]
    fn parent_death_helper() {
        if std::env::var_os(HELPER_ENV).is_none() {
            return;
        }
        let pid_file = std::env::var_os(PID_FILE_ENV).expect("helper pid file is required");
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
