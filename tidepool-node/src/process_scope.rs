//! Opt-in, blocking service-process ownership. The caller runs these bounded
//! operations outside its asynchronous actor loop. Namespace-init exit and
//! direct monitor wait are separate facts; only both yield a cleanup receipt.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fs::{Mode, OFlags};
use rustix::io::{Errno, FdFlags};
use serde::{Deserialize, Serialize};

use super::{ProcessInvocation, ProcessMountBoundary};

const INFO_LIMIT: usize = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ServiceScopeError {
    #[error("service scope executable must be absolute")]
    ExecutableNotAbsolute,
    #[error("service scope is unsupported: {0}")]
    Unsupported(&'static str),
    #[error("service process was not spawned: {0}")]
    NotSpawned(#[source] std::io::Error),
    #[error("service init identity could not be established: {0}")]
    IdentityUnconfirmed(String),
    #[error("service scope operation is invalid in its current phase")]
    WrongPhase,
    #[error("service payload release is unconfirmed: {0}")]
    ReleaseUnconfirmed(String),
    #[error("service scope cleanup is unconfirmed: {0}")]
    CleanupUnconfirmed(String),
    #[error("service scope io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<Errno> for ServiceScopeError {
    fn from(error: Errno) -> Self {
        Self::Io(error.into())
    }
}

/// Host-resolved changes; explicit unsets take precedence over supplied sets.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServiceEnvironment {
    pub set: BTreeMap<String, String>,
    pub unset: BTreeSet<String>,
}

/// One-shot reservation for a process launch. It deliberately is not Clone:
/// spawning consumes the reservation, so no second caller can launch the same
/// command under a copied claim.
pub struct LaunchReservation {
    boundary: ProcessMountBoundary,
    bubblewrap: PathBuf,
    command: ProcessInvocation,
    view: Option<RetainedProcessView>,
    #[cfg(test)]
    omit_sync_hold: bool,
}

/// An existing workspace view entered before the supervisor creates a PID scope.
#[derive(Debug, Serialize, Deserialize)]
pub struct RetainedProcessView {
    pub entry: crate::NamespaceEntry,
    pub directory: PathBuf,
}

impl LaunchReservation {
    pub(super) fn new(
        boundary: ProcessMountBoundary,
        bubblewrap: PathBuf,
        command: ProcessInvocation,
    ) -> Result<Self, ServiceScopeError> {
        if !bubblewrap.is_absolute() {
            return Err(ServiceScopeError::ExecutableNotAbsolute);
        }
        Ok(Self {
            boundary,
            bubblewrap,
            command,
            view: None,
            #[cfg(test)]
            omit_sync_hold: false,
        })
    }

    /// Keep the workspace's mount policy and add exact PID/process supervision.
    pub fn in_view(mut self, view: RetainedProcessView) -> Result<Self, ServiceScopeError> {
        if !view.directory.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "view directory must be absolute",
            )
            .into());
        }
        self.view = Some(view);
        Ok(self)
    }

    /// Spawn only the blocked wrapper. Successful return immediately transfers
    /// its Child and every gate/witness resource into the noncloneable owner.
    /// The caller must not independently reap this private direct child or set
    /// SIGCHLD to automatic reaping while its init identity is being acquired.
    ///
    /// The executable must be host-trusted bubblewrap compatible with 0.11.0:
    /// default PID-namespace init, blocking block-fd and init-retained sync-fd.
    /// Cleanup relies on Linux ordering audited at v6.12.63: namespace init
    /// drains namespace processes before publishing exit/pidfd readiness.
    /// These prerequisites are not established by checking an executable path
    /// or kernel version string. This API does not attest either binary.
    pub fn spawn(
        self,
        environment: ServiceEnvironment,
        output: File,
    ) -> Result<ScopeCapability, ServiceScopeError> {
        self.spawn_with_stdio(environment, ServiceStdio::Captured(output))
    }

    /// Spawn using either an owned diagnostic file or the helper's inherited
    /// terminal. The latter keeps the supervisor out of the TUI byte stream.
    pub fn spawn_with_stdio(
        self,
        environment: ServiceEnvironment,
        stdio: ServiceStdio,
    ) -> Result<ScopeCapability, ServiceScopeError> {
        let proc = checked_proc()?;
        let (gate_read, gate_write) = private_pipe()?;
        let gate_hold = rustix::io::fcntl_dupfd_cloexec(&gate_write, 3)?;
        let (info_read, info_write) = private_pipe()?;
        // Only the info reader is nonblocking. EAGAIN on bwrap's gate would
        // release the command because bwrap ignores that read's return value.
        rustix::fs::fcntl_setfl(&info_read, OFlags::NONBLOCK)?;
        #[allow(unused_mut)] // Mutated only by the test-only regression seam.
        let mut inherited = vec![
            gate_read.as_raw_fd(),
            gate_hold.as_raw_fd(),
            info_write.as_raw_fd(),
        ];
        #[allow(unused_mut)] // Mutated only by the test-only regression seam.
        let mut options = vec![
            "--unshare-pid".into(),
            "--proc".into(),
            "/proc".into(),
            "--block-fd".into(),
            inherited[0].to_string(),
            "--sync-fd".into(),
            inherited[1].to_string(),
            "--info-fd".into(),
            inherited[2].to_string(),
        ];
        // Mutation exists only in the test build, never as a runtime fallback.
        #[cfg(test)]
        if self.omit_sync_hold {
            inherited.remove(1);
            options.drain(5..7);
        }
        let mut command = match self.view {
            Some(view) => {
                // Open the host's descriptors before PID isolation hides its PID.
                // Preserve the already-prepared mounts, including read-only policy.
                let mut command = view
                    .entry
                    .command(&view.directory, self.bubblewrap.as_os_str())?;
                command.args(["--bind", "/", "/"]);
                // A normal bind marks device mounts nodev. Native subprocess
                // setup opens /dev/null and PTYs even with inherited TUI stdio.
                command.args(["--dev-bind", "/dev", "/dev"]);
                command.args(&options).arg("--chdir").arg(&view.directory);
                command
                    .arg("--")
                    .arg(self.command.program)
                    .args(self.command.args);
                command
            }
            None => {
                let invocation = self.boundary.wrap_with_options(
                    self.bubblewrap.to_string_lossy().into_owned(),
                    self.command,
                    &options,
                );
                let mut command = Command::new(invocation.program);
                command.args(invocation.args);
                command
            }
        };
        let terminal = match stdio {
            ServiceStdio::InheritedTerminal => TerminalCustody::acquire()?,
            ServiceStdio::Captured(_) => None,
        };
        command.envs(environment.set);
        match stdio {
            ServiceStdio::Captured(output) => {
                command
                    .stdin(Stdio::null())
                    .stdout(output.try_clone()?)
                    .stderr(output);
            }
            ServiceStdio::InheritedTerminal => {
                command
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit());
                // The helper keeps its original process group while the exact
                // payload gets the foreground group. Terminal-generated
                // signals therefore reach the application, never its owner.
                command.process_group(0);
            }
        }
        for name in environment.unset {
            command.env_remove(name);
        }
        let terminal_handoff = terminal.is_some();
        // SAFETY: no allocation/locking after fork. FDs are owned by this stack
        // until spawn returns; only the three distinct intended child ends lose
        // CLOEXEC. Their numbers are above stdio and cannot be concurrently reused.
        // process_group(0) runs before this closure; an interactive child then
        // installs that new group as foreground before exec can start bwrap.
        unsafe {
            command.pre_exec(move || {
                for &raw in &inherited {
                    rustix::io::fcntl_setfd(BorrowedFd::borrow_raw(raw), FdFlags::empty())?;
                }
                if terminal_handoff {
                    child_take_terminal()?;
                }
                Ok(())
            });
        }
        let monitor = command.spawn().map_err(ServiceScopeError::NotSpawned)?;
        Ok(ScopeCapability {
            monitor,
            proc,
            info: info_read,
            info_bytes: Vec::new(),
            info_record: None,
            gate: Some(gate_write),
            phase: Phase::Blocked,
            init: None,
            monitor_status: None,
            cleanup: None,
            terminal,
        })
    }
}

/// The only supported stdio policies. Interactive supervision inherits the
/// pane terminal directly; no broker reads or rewrites terminal traffic.
pub enum ServiceStdio {
    Captured(File),
    InheritedTerminal,
}

/// Retained terminal state for the helper's inherited pane. The duplicated
/// descriptor is never passed to the payload. Foreground restoration is part
/// of exact cleanup, after namespace drain and direct monitor wait.
struct TerminalCustody {
    terminal: OwnedFd,
    owner_group: rustix::process::Pid,
    attributes: rustix::termios::Termios,
    restored: bool,
}

impl TerminalCustody {
    fn acquire() -> Result<Option<Self>, ServiceScopeError> {
        // Unit/marker consumers may deliberately have no controlling terminal;
        // the production tmux path always does. In that case stdio is still
        // inherited, but there is no terminal authority to transfer.
        let stdin = unsafe { BorrowedFd::borrow_raw(0) };
        let attributes = match rustix::termios::tcgetattr(stdin) {
            Ok(attributes) => attributes,
            Err(Errno::NOTTY) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let owner_group = rustix::termios::tcgetpgrp(stdin)?;
        let terminal = rustix::io::fcntl_dupfd_cloexec(stdin, 3)?;
        Ok(Some(Self {
            terminal,
            owner_group,
            attributes,
            restored: false,
        }))
    }

    fn restore(&mut self) -> Result<(), ServiceScopeError> {
        if self.restored {
            return Ok(());
        }
        with_sigttou_blocked(|| {
            rustix::termios::tcsetpgrp(&self.terminal, self.owner_group)?;
            rustix::termios::tcsetattr(
                &self.terminal,
                rustix::termios::OptionalActions::Now,
                &self.attributes,
            )
        })?;
        self.restored = true;
        Ok(())
    }
}

/// Called only from Command::pre_exec after process_group(0). All operations
/// are async-signal-safe and use stack-owned sigset_t storage.
unsafe fn child_take_terminal() -> std::io::Result<()> {
    let mut blocked = rustix::runtime::KernelSigSet::empty();
    blocked.insert(rustix::process::Signal::TTOU);
    let previous =
        rustix::runtime::kernel_sigprocmask(rustix::runtime::How::BLOCK, Some(&blocked))?;
    let stdin = BorrowedFd::borrow_raw(0);
    let handoff = rustix::termios::tcsetpgrp(stdin, rustix::process::getpgrp());
    let restore =
        rustix::runtime::kernel_sigprocmask(rustix::runtime::How::SETMASK, Some(&previous));
    restore?;
    handoff.map_err(Into::into)
}

fn with_sigttou_blocked<T>(operation: impl FnOnce() -> Result<T, Errno>) -> Result<T, Errno> {
    let mut blocked = rustix::runtime::KernelSigSet::empty();
    blocked.insert(rustix::process::Signal::TTOU);
    // SAFETY: this changes only the calling thread's mask, includes no
    // libc-reserved signal, and restores the exact previous mask.
    unsafe {
        let previous =
            rustix::runtime::kernel_sigprocmask(rustix::runtime::How::BLOCK, Some(&blocked))?;
        let result = operation();
        rustix::runtime::kernel_sigprocmask(rustix::runtime::How::SETMASK, Some(&previous))?;
        result
    }
}

fn private_pipe() -> Result<(OwnedFd, OwnedFd), ServiceScopeError> {
    let (read, write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
    // Also correct when the embedding application's stdio was originally closed.
    Ok((
        rustix::io::fcntl_dupfd_cloexec(&read, 3)?,
        rustix::io::fcntl_dupfd_cloexec(&write, 3)?,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Blocked,
    Pinned,
    Released,
    ReleaseUnconfirmed,
    Stopping,
    Cleaned,
}

#[derive(Deserialize, Clone, Debug)]
struct InitInfo {
    #[serde(rename = "child-pid")]
    pid: u32,
    #[serde(rename = "pid-namespace")]
    namespace: u64,
}

struct InitWitness {
    pidfd: OwnedFd,
}

/// Noncloneable process owner. Errors borrow and retain its resources. Dropping
/// an uncertain owner attempts identity-safe termination but yields no receipt;
/// before pinning it may leave a blocked init. Its self-held sync writer prevents
/// host gate closure from accidentally launching the payload in that case.
pub struct ScopeCapability {
    monitor: Child,
    proc: OwnedFd,
    info: OwnedFd,
    info_bytes: Vec<u8>,
    info_record: Option<InitInfo>,
    gate: Option<OwnedFd>,
    phase: Phase,
    init: Option<InitWitness>,
    monitor_status: Option<ExitStatus>,
    cleanup: Option<ServiceScopeCleanup>,
    terminal: Option<TerminalCustody>,
}

impl ScopeCapability {
    pub fn pin_init(&mut self, deadline: Instant) -> Result<(), ServiceScopeError> {
        if self.phase == Phase::Pinned {
            return Ok(());
        }
        if self.phase != Phase::Blocked {
            return Err(ServiceScopeError::WrongPhase);
        }
        let info = self.read_info(deadline)?;
        self.pin_record(&info)?;
        self.phase = Phase::Pinned;
        Ok(())
    }

    fn read_info(&mut self, deadline: Instant) -> Result<InitInfo, ServiceScopeError> {
        if let Some(info) = &self.info_record {
            return Ok(info.clone());
        }
        loop {
            if self.info_bytes.len() >= INFO_LIMIT {
                return Err(ServiceScopeError::IdentityUnconfirmed(
                    "oversized info record".into(),
                ));
            }
            let mut bytes = [0u8; 1024];
            match rustix::io::read(&self.info, &mut bytes) {
                Ok(0) => {
                    let info: InitInfo =
                        serde_json::from_slice(&self.info_bytes).map_err(|error| {
                            ServiceScopeError::IdentityUnconfirmed(error.to_string())
                        })?;
                    self.info_record = Some(info.clone());
                    return Ok(info);
                }
                Ok(count) => self.info_bytes.extend_from_slice(&bytes[..count]),
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) => wait_readable(&self.info, deadline)
                    .map_err(|error| ServiceScopeError::IdentityUnconfirmed(error.to_string()))?,
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn pin_record(&mut self, info: &InitInfo) -> Result<(), ServiceScopeError> {
        let pid = rustix::process::Pid::from_raw(
            info.pid
                .try_into()
                .map_err(|_| ServiceScopeError::IdentityUnconfirmed("invalid init pid".into()))?,
        )
        .ok_or_else(|| ServiceScopeError::IdentityUnconfirmed("zero init pid".into()))?;
        let directory = rustix::fs::openat(
            &self.proc,
            info.pid.to_string(),
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let pidfd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())?;
        // This open/read MUST follow pidfd acquisition and remain relative to
        // the retained proc directory, whose inode refers to the original pid.
        let status = read_status(&directory)?;
        if status.pid != info.pid
            || status.parent != self.monitor.id()
            || status.namespace_pids != [info.pid, 1]
        {
            return Err(ServiceScopeError::IdentityUnconfirmed(
                "init parent/PID namespace mismatch".into(),
            ));
        }
        let namespace = rustix::fs::openat(
            &directory,
            "ns/pid",
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        if rustix::fs::fstat(&namespace)?.st_ino != info.namespace {
            return Err(ServiceScopeError::IdentityUnconfirmed(
                "namespace inode mismatch".into(),
            ));
        }
        self.init = Some(InitWitness { pidfd });
        Ok(())
    }

    /// Commit the launch exactly once. The receipt proves that the release byte
    /// entered the private blocking pipe; it does not claim that the payload
    /// started, remained alive, or later stopped.
    pub fn release_command(&mut self) -> Result<LaunchRelease, ServiceScopeError> {
        if self.phase != Phase::Pinned {
            return Err(ServiceScopeError::WrongPhase);
        }
        // Commit intent before the write; an error is not automatically retried.
        self.phase = Phase::ReleaseUnconfirmed;
        let gate = self.gate.as_ref().ok_or_else(|| {
            ServiceScopeError::ReleaseUnconfirmed("private release gate is unavailable".into())
        })?;
        match rustix::io::write(gate, b"R") {
            Ok(1) => {
                self.phase = Phase::Released;
                self.gate.take();
                let receipt = LaunchRelease { _private: () };
                Ok(receipt)
            }
            Ok(_) => Err(ServiceScopeError::ReleaseUnconfirmed(
                "private release gate accepted a short write".into(),
            )),
            Err(error) => Err(ServiceScopeError::ReleaseUnconfirmed(error.to_string())),
        }
    }

    /// Report facts retained by this exact owner without changing lifecycle.
    /// These observations never authorize release, retry, or resource cleanup.
    pub fn observation(&self) -> Result<ScopeObservation, ServiceScopeError> {
        Ok(match self.phase {
            Phase::Blocked => ScopeObservation::Blocked,
            Phase::Pinned => ScopeObservation::Pinned,
            Phase::Released => ScopeObservation::Released,
            Phase::ReleaseUnconfirmed => ScopeObservation::ReleaseUnconfirmed,
            Phase::Stopping => ScopeObservation::Stopping,
            Phase::Cleaned => ScopeObservation::ProcessStopped(self.cleanup.ok_or_else(|| {
                ServiceScopeError::CleanupUnconfirmed(
                    "cleaned scope lost its terminal receipt; owner retained".into(),
                )
            })?),
        })
    }

    /// Nonblocking exact-exit refresh for an already released/stopping scope.
    /// A terminal receipt is still issued only after namespace-init readiness
    /// and direct monitor wait have both been observed.
    pub fn refresh_terminal(
        &mut self,
        deadline: Instant,
    ) -> Result<Option<ServiceScopeCleanup>, ServiceScopeError> {
        if let Some(receipt) = self.cleanup {
            return Ok(Some(receipt));
        }
        if !matches!(self.phase, Phase::Released | Phase::Stopping) {
            return Ok(None);
        }
        let Some(init) = &self.init else {
            return Ok(None);
        };
        let mut polls = [PollFd::new(&init.pidfd, PollFlags::IN)];
        let ready = rustix::event::poll(
            &mut polls,
            Some(&Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            }),
        )?;
        if ready == 0
            || !polls[0]
                .revents()
                .intersects(PollFlags::IN | PollFlags::HUP)
        {
            return Ok(None);
        }
        self.terminate_and_wait(deadline).map(Some)
    }

    pub fn terminate_and_wait(
        &mut self,
        deadline: Instant,
    ) -> Result<ServiceScopeCleanup, ServiceScopeError> {
        if let Some(receipt) = &self.cleanup {
            return Ok(*receipt);
        }
        let init = self.init.as_ref().ok_or_else(|| {
            ServiceScopeError::CleanupUnconfirmed(
                "init has no validated lifetime witness; owner retained".into(),
            )
        })?;
        self.phase = Phase::Stopping;
        match rustix::process::pidfd_send_signal(&init.pidfd, rustix::process::Signal::KILL) {
            Ok(()) | Err(Errno::SRCH) => {}
            Err(error) => return Err(ServiceScopeError::CleanupUnconfirmed(error.to_string())),
        }
        wait_readable(&init.pidfd, deadline)
            .map_err(|error| ServiceScopeError::CleanupUnconfirmed(error.to_string()))?;
        // Exact init exit follows namespace drain; it is not our wait/reap of
        // init. Collect the separate owned monitor status without reading EOF
        // from pipes which may be inherited by external tasks.
        loop {
            if let Some(status) = self.monitor_status {
                let receipt = ServiceScopeCleanup {
                    monitor_status: status,
                };
                self.gate.take();
                if let Some(terminal) = &mut self.terminal {
                    terminal.restore().map_err(|error| {
                        ServiceScopeError::CleanupUnconfirmed(error.to_string())
                    })?;
                }
                self.cleanup = Some(receipt);
                self.phase = Phase::Cleaned;
                return Ok(receipt);
            }
            self.monitor_status = self
                .monitor
                .try_wait()
                .map_err(|error| ServiceScopeError::CleanupUnconfirmed(error.to_string()))?;
            if self.monitor_status.is_none() {
                let remaining =
                    deadline
                        .checked_duration_since(Instant::now())
                        .ok_or_else(|| {
                            ServiceScopeError::CleanupUnconfirmed(
                                "monitor wait deadline elapsed".into(),
                            )
                        })?;
                std::thread::sleep(remaining.min(Duration::from_millis(2)));
            }
        }
    }
}

impl Drop for ScopeCapability {
    fn drop(&mut self) {
        if self.cleanup.is_some() {
            return;
        }
        if let Some(init) = &self.init {
            let _ = rustix::process::pidfd_send_signal(&init.pidfd, rustix::process::Signal::KILL);
        }
        // Child::kill targets the still-owned direct child, never a rediscovered
        // PID. This is emergency best effort, not a cleanup receipt.
        let _ = self.monitor.kill();
        let _ = self.monitor.try_wait();
        if let Some(terminal) = &mut self.terminal {
            let _ = terminal.restore();
        }
    }
}

/// Evidence that the private launch gate accepted its unique release byte.
/// This is status, not authority: only the retained ScopeCapability owns the
/// process and can establish terminal cleanup.
#[derive(Debug)]
pub struct LaunchRelease {
    _private: (),
}

/// Typed reporting view of the exact capability's process facts.
#[derive(Debug, Clone, Copy)]
pub enum ScopeObservation {
    Blocked,
    Pinned,
    Released,
    ReleaseUnconfirmed,
    Stopping,
    ProcessStopped(ServiceScopeCleanup),
}

#[derive(Debug, Clone, Copy)]
pub struct ServiceScopeCleanup {
    monitor_status: ExitStatus,
}

// Compatibility spellings for current host consumers. The concrete owners are
// the explicitly named reservation and capability types above; aliases cannot
// add cloning or a second launch path.
pub type PreparedServiceScope = LaunchReservation;
pub type ServiceScope = ScopeCapability;
impl ServiceScopeCleanup {
    pub fn monitor_status(&self) -> ExitStatus {
        self.monitor_status
    }
}

pub(crate) fn wait_readable(fd: &OwnedFd, deadline: Instant) -> std::io::Result<()> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "scope observation deadline elapsed",
                )
            })?;
        let timeout = Timespec {
            tv_sec: remaining.as_secs().try_into().unwrap_or(i64::MAX),
            tv_nsec: remaining.subsec_nanos().into(),
        };
        let mut polls = [PollFd::new(fd, PollFlags::IN)];
        match rustix::event::poll(&mut polls, Some(&timeout)) {
            Err(Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "scope observation timed out",
                ));
            }
            Ok(_) => {
                let events = polls[0].revents();
                if events.intersects(PollFlags::ERR | PollFlags::NVAL) {
                    return Err(std::io::Error::other("invalid scope descriptor event"));
                }
                if events.intersects(PollFlags::IN | PollFlags::HUP) {
                    return Ok(());
                }
                return Err(std::io::Error::other("unexpected scope descriptor event"));
            }
        }
    }
}

#[derive(Debug)]
struct ProcStatus {
    pid: u32,
    parent: u32,
    namespace_pids: Vec<u32>,
}
fn read_status(directory: &OwnedFd) -> Result<ProcStatus, ServiceScopeError> {
    let fd = rustix::fs::openat(
        directory,
        "status",
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut text = String::new();
    File::from(fd).take(64 * 1024).read_to_string(&mut text)?;
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .ok_or_else(|| {
                ServiceScopeError::IdentityUnconfirmed("missing proc status field".into())
            })
    };
    let number = |text: &str| {
        text.trim().parse::<u32>().map_err(|_| {
            ServiceScopeError::IdentityUnconfirmed("malformed proc status number".into())
        })
    };
    Ok(ProcStatus {
        pid: number(field("Pid:")?)?,
        parent: number(field("PPid:")?)?,
        namespace_pids: field("NSpid:")?
            .split_whitespace()
            .map(number)
            .collect::<Result<_, _>>()?,
    })
}
fn checked_proc() -> Result<OwnedFd, ServiceScopeError> {
    checked_proc_at("/proc")
}

fn checked_proc_at(path: &str) -> Result<OwnedFd, ServiceScopeError> {
    let proc = rustix::fs::open(
        path,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    if rustix::fs::fstatfs(&proc)?.f_type != rustix::fs::PROC_SUPER_MAGIC {
        return Err(ServiceScopeError::Unsupported("/proc is not procfs"));
    }
    let own = rustix::fs::openat(
        &proc,
        "self",
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let status = read_status(&own)?;
    if status.pid != std::process::id() || status.namespace_pids != [std::process::id()] {
        return Err(ServiceScopeError::Unsupported(
            "procfs PID namespace view differs from caller",
        ));
    }
    Ok(proc)
}

#[cfg(test)]
#[path = "process_scope_tests.rs"]
mod tests;
