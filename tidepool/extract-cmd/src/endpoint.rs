use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
#[cfg(test)]
use std::io::Read;
use std::io::{self, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

use crate::{daemon, process, ExtractCmd, ExtractRun, SpawnError};

static PHYSICAL_REQUEST_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

pub(crate) const BOUND_ENDPOINT_FLAG: &str = "--compiler-endpoint-v1";
pub(crate) const IDENTITY_MAGIC: &[u8; 8] = b"TPCID002";
const DIRECT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerIdentity {
    producer: [u8; 32],
    consumed_worker: [u8; 32],
    endpoint: [u8; 32],
}

impl CompilerIdentity {
    pub(crate) fn direct(producer: [u8; 32], consumed_worker: [u8; 32]) -> Self {
        Self {
            producer,
            consumed_worker,
            endpoint: producer,
        }
    }

    pub(crate) fn daemon(producer: [u8; 32], consumed_worker: [u8; 32], epoch: [u8; 32]) -> Self {
        let mut hasher = blake3::Hasher::new();
        frame(&mut hasher, b"tidepool-daemon-endpoint-v1");
        frame(&mut hasher, &producer);
        frame(&mut hasher, &epoch);
        Self {
            producer,
            consumed_worker,
            endpoint: *hasher.finalize().as_bytes(),
        }
    }

    /// Identity of the exact bound endpoint, including a daemon's boot epoch.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.endpoint
    }

    /// Stable producer identity used by the deploy handshake.
    pub fn producer_bytes(&self) -> &[u8; 32] {
        &self.producer
    }

    /// Digest of the exact worker executable bytes pinned by this endpoint.
    pub fn consumed_worker_bytes(&self) -> &[u8; 32] {
        &self.consumed_worker
    }

    pub fn to_hex(&self) -> String {
        hex(&self.endpoint)
    }

    pub fn producer_hex(&self) -> String {
        hex(&self.producer)
    }
}

impl std::fmt::Display for CompilerIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

pub(crate) fn producer_identity(frontend: &[u8], worker: &[u8], ghc_libdir: &OsStr) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    // Executable locations are deployment provenance. Retaining identical
    // compiler bytes for a run must preserve its configured authority and
    // certified module products. The GHC directory still selects package inputs.
    frame(&mut hasher, b"tidepool-compiler-producer-v2");
    frame(&mut hasher, frontend);
    frame(&mut hasher, worker);
    frame(&mut hasher, ghc_libdir.as_encoded_bytes());
    *hasher.finalize().as_bytes()
}

fn frame(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[derive(Debug)]
pub(crate) struct LaunchSpec {
    pub(crate) program: OsString,
    pub(crate) prefix: Vec<OsString>,
}

impl LaunchSpec {
    pub(crate) fn direct(program: OsString) -> Self {
        Self {
            program,
            prefix: Vec::new(),
        }
    }

    pub(crate) fn nix(flake_root: &Path) -> Self {
        Self {
            program: "nix".into(),
            prefix: vec![
                "run".into(),
                format!("{}#{}", flake_root.display(), crate::DEFAULT_BIN).into(),
                "--".into(),
            ],
        }
    }
}

#[derive(Debug)]
pub struct CompilerEndpoint {
    identity: CompilerIdentity,
    transport: Transport,
}

/// A completed action and its independent compiler lifecycle observation.
/// Cleanup uncertainty never replaces the action or authorizes its replay.
#[must_use]
#[derive(Debug)]
pub struct CompilerTransactionOutcome<T> {
    pub action: T,
    pub close: CompilerTransactionClose,
}

#[must_use]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerTransactionClose {
    NotStarted,
    Clean,
    Unconfirmed(CompilerTransactionCloseEvidence),
}

impl CompilerTransactionClose {
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Clean)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerTransactionCloseEvidence {
    pub reason: CompilerTransactionCloseReason,
    pub retirement: CompilerTransactionRetirement,
    pub earlier: Vec<CompilerTransactionCloseEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerTransactionCloseReason {
    AdmissionAborted(CompilerTransactionClosePhase),
    AdmissionFailed(CompilerTransactionCloseFailure),
    FailedRequest,
    Abandoned,
    Cancelled,
    EndFailed(CompilerTransactionCloseFailure),
    FrontendExitUnsuccessful,
    FrontendRetirementUnconfirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompilerTransactionClosePhase {
    IdentityHandshake,
    BeginHandshake,
    EndWrite,
    DaemonEndAcknowledgement,
    DaemonDisconnect,
    FrontendWait,
    FrontendKill,
}

/// Equality preserves the identity of the retained IO cause across clones.
#[derive(Clone, Debug)]
pub struct CompilerTransactionCloseFailure {
    pub phase: CompilerTransactionClosePhase,
    pub source: Arc<io::Error>,
}

impl PartialEq for CompilerTransactionCloseFailure {
    fn eq(&self, other: &Self) -> bool {
        self.phase == other.phase && Arc::ptr_eq(&self.source, &other.source)
    }
}
impl Eq for CompilerTransactionCloseFailure {}

impl CompilerTransactionCloseFailure {
    fn new(phase: CompilerTransactionClosePhase, source: io::Error) -> Self {
        Self {
            phase,
            source: Arc::new(source),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerTransactionRetirement {
    DaemonUnobserved {
        disconnect: Option<Result<(), CompilerTransactionCloseFailure>>,
    },
    Direct(DirectCompilerRetirement),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectCompilerRetirement {
    pub exit: Result<std::process::ExitStatus, CompilerTransactionCloseFailure>,
    pub termination: CompilerTermination,
    _retained_child: Option<RetainedCompilerChild>,
}

/// Retains the exact unreaped child when retirement could not be confirmed.
#[derive(Clone)]
struct RetainedCompilerChild(Arc<Mutex<Child>>);
impl std::fmt::Debug for RetainedCompilerChild {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("RetainedCompilerChild")
            .field(
                &self
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .id(),
            )
            .finish()
    }
}
impl PartialEq for RetainedCompilerChild {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for RetainedCompilerChild {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerTermination {
    NotRequested,
    Requested,
    Failed(CompilerTransactionCloseFailure),
}

/// A bounded lease on one compiler worker. Requests execute in order against
/// one resident GHC transaction and dropping the lease releases admission.
#[derive(Debug)]
pub struct CompilerTransaction {
    identity: CompilerIdentity,
    transport: Option<TransactionTransport>,
    failed: bool,
    cancellation: Option<CompilerTransactionCancellation>,
}

/// A cloneable cancellation edge for a scoped compiler transaction. It owns
/// only the exact direct child or duplicated daemon connection armed by that
/// scope, so cancellation cannot affect a later transaction or a reused PID.
#[derive(Clone, Debug)]
pub struct CompilerTransactionCancellation {
    state: Arc<Mutex<CancellationState>>,
}

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: bool,
    target: Option<CancellationTarget>,
}

#[derive(Debug)]
enum CancellationTarget {
    Direct(Arc<Mutex<Child>>),
    Daemon(UnixStream),
}

#[derive(Debug)]
enum TransactionTransport {
    Direct(DirectEndpoint),
    Daemon {
        transaction: daemon::DaemonTransaction,
        socket: PathBuf,
    },
}

#[derive(Debug)]
enum Transport {
    Direct(DirectEndpoint),
    Daemon { socket: PathBuf, epoch: [u8; 32] },
    Scoped,
}

impl Transport {
    fn name(&self) -> &'static str {
        match self {
            Self::Direct(_) => "direct",
            Self::Daemon { .. } => "daemon",
            Self::Scoped => "transaction",
        }
    }
}

#[derive(Debug)]
struct DirectEndpoint {
    child: Arc<Mutex<Child>>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    program: OsString,
    retired: bool,
    admission_phase: CompilerTransactionClosePhase,
}

impl DirectEndpoint {
    fn read_handshake(
        &mut self,
        bytes: &mut [u8],
        deadline: Instant,
        cancellation: Option<&CompilerTransactionCancellation>,
    ) -> io::Result<()> {
        let stdout = self.stdout.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "bound endpoint stdout is closed")
        })?;
        process::read_exact_until(stdout, bytes, deadline, || {
            cancellation.is_some_and(CompilerTransactionCancellation::is_cancelled)
        })
    }

    fn abort(&mut self) -> DirectCompilerRetirement {
        drop(self.stdin.take());
        drop(self.stdout.take());
        self.retired = true;
        retire_owned_child(&self.child, true)
    }

    fn stdout_mut(&mut self) -> Result<&mut ChildStdout, SpawnError> {
        self.stdout.as_mut().ok_or_else(|| {
            SpawnError::indeterminate(
                self.program.clone(),
                io::Error::new(io::ErrorKind::BrokenPipe, "bound endpoint is closed"),
            )
        })
    }
}

impl CompilerTransactionCancellation {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(CancellationState::default())),
        }
    }

    /// Interrupt the currently armed transaction, if any. Repeated calls are
    /// harmless and an arm installed after cancellation is interrupted
    /// immediately.
    pub fn cancel(&self) {
        let target = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.cancelled = true;
            state.target.take()
        };
        cancel_target(target);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancelled
    }

    pub(crate) fn arm_daemon(&self, stream: &UnixStream) -> io::Result<()> {
        self.arm(CancellationTarget::Daemon(stream.try_clone()?));
        Ok(())
    }

    fn arm(&self, target: CancellationTarget) {
        let target = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.cancelled {
                Some(target)
            } else {
                state.target = Some(target);
                None
            }
        };
        cancel_target(target);
    }

    pub(crate) fn disarm(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .target = None;
    }
}

impl Default for CompilerTransactionCancellation {
    fn default() -> Self {
        Self::new()
    }
}

fn cancel_target(target: Option<CancellationTarget>) {
    match target {
        Some(CancellationTarget::Direct(child)) => {
            if let Ok(mut child) = child.lock() {
                // The Child remains unreaped in its owning DirectEndpoint,
                // so its PID cannot be reused before that owner observes the
                // cancellation and waits it.
                if let Err(error) = child.kill() {
                    tracing::warn!(%error, "failed to kill cancelled direct-mode compiler worker");
                }
            }
        }
        Some(CancellationTarget::Daemon(stream)) => {
            // best-effort: the daemon connection may already be closed by
            // the peer or by a concurrent cancellation.
            stream.shutdown(Shutdown::Both).ok();
        }
        None => {}
    }
}

/// Bound a direct-mode worker's exit the same way `OwnedSocket::retire`
/// bounds the daemon's own shutdown: an orderly exit is given a fixed
/// window, past which the child is killed outright rather than left to
/// wedge whoever is dropping this endpoint (this function is reachable from
/// `Drop`, where blocking forever is not an option).
fn wait_for_owned_child(child: &Arc<Mutex<Child>>) -> DirectCompilerRetirement {
    retire_owned_child(child, false)
}

fn retire_owned_child(child: &Arc<Mutex<Child>>, abort: bool) -> DirectCompilerRetirement {
    let mut termination = CompilerTermination::NotRequested;
    let mut deadline = Instant::now() + Duration::from_secs(5);
    if abort {
        termination = request_child_termination(child);
    }
    loop {
        let observed = child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_wait();
        match observed {
            Ok(Some(status)) => {
                return DirectCompilerRetirement {
                    exit: Ok(status),
                    termination,
                    _retained_child: None,
                }
            }
            Err(source) => {
                if matches!(termination, CompilerTermination::NotRequested) {
                    termination = request_child_termination(child);
                }
                return DirectCompilerRetirement {
                    exit: Err(CompilerTransactionCloseFailure::new(
                        CompilerTransactionClosePhase::FrontendWait,
                        source,
                    )),
                    termination,
                    _retained_child: Some(RetainedCompilerChild(Arc::clone(child))),
                };
            }
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            if matches!(termination, CompilerTermination::NotRequested) {
                termination = request_child_termination(child);
                deadline = Instant::now() + Duration::from_secs(5);
            } else {
                return DirectCompilerRetirement {
                    exit: Err(CompilerTransactionCloseFailure::new(
                        CompilerTransactionClosePhase::FrontendWait,
                        io::Error::new(
                            io::ErrorKind::TimedOut,
                            "direct compiler retirement remains unconfirmed",
                        ),
                    )),
                    termination,
                    _retained_child: Some(RetainedCompilerChild(Arc::clone(child))),
                };
            }
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "sync compiler owner polls bounded retirement"
        )]
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn request_child_termination(child: &Arc<Mutex<Child>>) -> CompilerTermination {
    match child
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .kill()
    {
        Ok(()) => CompilerTermination::Requested,
        Err(source) => CompilerTermination::Failed(CompilerTransactionCloseFailure::new(
            CompilerTransactionClosePhase::FrontendKill,
            source,
        )),
    }
}

impl Drop for DirectEndpoint {
    fn drop(&mut self) {
        if self.retired {
            return;
        }
        let retirement = self.abort();
        TRANSACTION_SCOPE.with(|scope| {
            if let Ok(mut scope) = scope.try_borrow_mut() {
                if let Some(scope) = scope.as_mut() {
                    scope
                        .admission_close
                        .push(CompilerTransactionCloseEvidence {
                            reason: CompilerTransactionCloseReason::AdmissionAborted(
                                self.admission_phase,
                            ),
                            retirement: CompilerTransactionRetirement::Direct(retirement.clone()),
                            earlier: Vec::new(),
                        });
                }
            }
        });
        if retirement.exit.is_err() {
            tracing::warn!(
                ?retirement,
                "direct compiler abandonment remains unconfirmed"
            );
        }
    }
}

impl CompilerEndpoint {
    pub(crate) fn bind(cmd: &ExtractCmd) -> Result<Self, SpawnError> {
        if TRANSACTION_SCOPE.with(|scope| scope.borrow().is_some()) {
            let identity = ensure_scoped_transaction(cmd)?;
            return Ok(Self {
                identity,
                transport: Transport::Scoped,
            });
        }
        Self::bind_unscoped(cmd)
    }

    fn bind_unscoped(cmd: &ExtractCmd) -> Result<Self, SpawnError> {
        Self::bind_unscoped_with_cancellation(cmd, None)
    }

    fn bind_unscoped_with_cancellation(
        cmd: &ExtractCmd,
        cancellation: Option<&CompilerTransactionCancellation>,
    ) -> Result<Self, SpawnError> {
        let required = std::env::var_os(crate::REQUIRED_DAEMON_ENDPOINT_ENV);
        if let Some(socket) = std::env::var_os(crate::DAEMON_SOCKET_ENV) {
            let socket = PathBuf::from(socket);
            if let Ok(binding) = daemon::preflight(&socket) {
                let identity = CompilerIdentity::daemon(
                    binding.producer,
                    binding.consumed_worker,
                    binding.epoch,
                );
                if required
                    .as_ref()
                    .is_some_and(|expected| expected != &OsString::from(identity.to_hex()))
                {
                    return Err(SpawnError::not_submitted(
                        &socket,
                        io::Error::other("required compiler daemon epoch or producer changed"),
                    ));
                }
                return Ok(Self {
                    identity: CompilerIdentity::daemon(
                        binding.producer,
                        binding.consumed_worker,
                        binding.epoch,
                    ),
                    transport: Transport::Daemon {
                        socket,
                        epoch: binding.epoch,
                    },
                });
            }
        }
        if required.is_some() {
            return Err(SpawnError::not_submitted(
                &cmd.program,
                io::Error::other(
                    "required owned compiler daemon is unavailable; direct fallback is forbidden",
                ),
            ));
        }
        Self::bind_launch_with_cancellation(
            LaunchSpec::direct(cmd.program.clone()),
            cancellation,
            DIRECT_HANDSHAKE_TIMEOUT,
        )
    }

    pub(crate) fn bind_direct(cmd: &ExtractCmd) -> Result<Self, SpawnError> {
        Self::bind_launch(LaunchSpec::direct(cmd.program.clone()))
    }

    pub(crate) fn bind_nix(flake_root: &Path) -> Result<Self, SpawnError> {
        Self::bind_launch(LaunchSpec::nix(flake_root))
    }

    fn bind_launch(spec: LaunchSpec) -> Result<Self, SpawnError> {
        Self::bind_launch_with_cancellation(spec, None, DIRECT_HANDSHAKE_TIMEOUT)
    }

    fn bind_launch_with_cancellation(
        spec: LaunchSpec,
        cancellation: Option<&CompilerTransactionCancellation>,
        timeout: Duration,
    ) -> Result<Self, SpawnError> {
        let mut command = process::command(&spec.program);
        command
            .args(&spec.prefix)
            .arg(BOUND_ENDPOINT_FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command
            .spawn()
            .map_err(|source| SpawnError::not_submitted(spec.program.clone(), source))?;
        // Own and arm the child before any identity IO can block. The
        // cancellation target is the same unreaped child retained by this
        // endpoint, including during an abandoned async caller's binding.
        let mut direct = DirectEndpoint {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            child: Arc::new(Mutex::new(child)),
            program: spec.program.clone(),
            retired: false,
            admission_phase: CompilerTransactionClosePhase::IdentityHandshake,
        };
        if let Some(cancellation) = cancellation {
            cancellation.arm(CancellationTarget::Direct(Arc::clone(&direct.child)));
        }
        let deadline = Instant::now() + timeout;
        let mut magic = [0u8; 8];
        let mut producer = [0u8; 32];
        let mut consumed_worker = [0u8; 32];
        direct
            .read_handshake(&mut magic, deadline, cancellation)
            .map_err(|source| SpawnError::not_submitted(spec.program.clone(), source))?;
        if &magic != IDENTITY_MAGIC {
            return Err(SpawnError::not_submitted(
                spec.program.clone(),
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid compiler endpoint identity",
                ),
            ));
        }
        direct
            .read_handshake(&mut producer, deadline, cancellation)
            .map_err(|source| SpawnError::not_submitted(spec.program.clone(), source))?;
        direct
            .read_handshake(&mut consumed_worker, deadline, cancellation)
            .map_err(|source| SpawnError::not_submitted(spec.program.clone(), source))?;
        Ok(Self {
            identity: CompilerIdentity::direct(producer, consumed_worker),
            transport: Transport::Direct(direct),
        })
    }

    pub fn identity(&self) -> &CompilerIdentity {
        &self.identity
    }

    pub fn transaction(self) -> Result<CompilerTransaction, SpawnError> {
        self.transaction_with_cancellation(None)
    }

    fn transaction_with_cancellation(
        self,
        cancellation: Option<CompilerTransactionCancellation>,
    ) -> Result<CompilerTransaction, SpawnError> {
        self.transaction_with_cancellation_timeout(cancellation, DIRECT_HANDSHAKE_TIMEOUT)
    }

    fn transaction_with_cancellation_timeout(
        self,
        cancellation: Option<CompilerTransactionCancellation>,
        timeout: Duration,
    ) -> Result<CompilerTransaction, SpawnError> {
        let identity = self.identity.clone();
        let transport_name = self.transport.name();
        let admission_started = Instant::now();
        let transport = match self.transport {
            Transport::Direct(mut endpoint) => {
                endpoint.admission_phase = CompilerTransactionClosePhase::BeginHandshake;
                if let Some(cancellation) = &cancellation {
                    cancellation.arm(CancellationTarget::Direct(Arc::clone(&endpoint.child)));
                }
                let deadline = Instant::now() + timeout;
                let stdin = endpoint.stdin.as_mut().ok_or_else(|| {
                    SpawnError::indeterminate(
                        endpoint.program.clone(),
                        io::Error::new(io::ErrorKind::BrokenPipe, "bound endpoint is closed"),
                    )
                })?;
                stdin
                    .write_all(daemon::DIRECT_TRANSACTION)
                    .map_err(|source| {
                        SpawnError::indeterminate(endpoint.program.clone(), source)
                    })?;
                stdin.flush().map_err(|source| {
                    SpawnError::indeterminate(endpoint.program.clone(), source)
                })?;
                let mut accepted = [0u8; 1];
                endpoint
                    .read_handshake(&mut accepted, deadline, cancellation.as_ref())
                    .map_err(|source| {
                        SpawnError::indeterminate(endpoint.program.clone(), source)
                    })?;
                if accepted != [1] {
                    return Err(SpawnError::indeterminate(
                        endpoint.program.clone(),
                        io::Error::new(io::ErrorKind::InvalidData, "compiler rejected transaction"),
                    ));
                }
                TransactionTransport::Direct(endpoint)
            }
            Transport::Daemon { socket, epoch } => {
                let transaction = daemon::begin_transaction_with_cancellation(
                    &socket,
                    &epoch,
                    cancellation.as_ref(),
                )
                .map_err(|error| {
                    if matches!(error, daemon::DaemonError::Busy) {
                        return SpawnError::capacity(socket.as_os_str());
                    }
                    let permits_rebind = error.permits_rebind();
                    let source = io::Error::other(error);
                    if permits_rebind {
                        SpawnError::not_submitted(socket.as_os_str(), source)
                    } else {
                        let failure = CompilerTransactionCloseFailure::new(
                            CompilerTransactionClosePhase::BeginHandshake,
                            source,
                        );
                        TRANSACTION_SCOPE.with(|scope| {
                            if let Some(scope) = scope.borrow_mut().as_mut() {
                                scope
                                    .admission_close
                                    .push(CompilerTransactionCloseEvidence {
                                        reason: CompilerTransactionCloseReason::AdmissionFailed(
                                            failure.clone(),
                                        ),
                                        retirement:
                                            CompilerTransactionRetirement::DaemonUnobserved {
                                                disconnect: None,
                                            },
                                        earlier: Vec::new(),
                                    });
                            }
                        });
                        SpawnError::indeterminate(
                            socket.as_os_str(),
                            io::Error::new(failure.source.kind(), Arc::clone(&failure.source)),
                        )
                    }
                })?;
                TransactionTransport::Daemon {
                    transaction,
                    socket,
                }
            }
            Transport::Scoped => {
                return Err(SpawnError::not_submitted(
                    "compiler transaction",
                    io::Error::other("compiler transaction is already scoped"),
                ));
            }
        };
        tracing::info!(
            transport = transport_name,
            phase = "compiler_transaction_admission",
            producer = %identity.producer_hex(),
            endpoint = %identity,
            elapsed_ms = u64::try_from(admission_started.elapsed().as_millis()).unwrap_or(u64::MAX),
            queue_ms = u64::try_from(admission_started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "compiler transaction admitted"
        );
        Ok(CompilerTransaction {
            identity,
            transport: Some(transport),
            failed: false,
            cancellation,
        })
    }

    /// Execute `cmd` through the producer captured by this endpoint.
    pub fn execute(mut self, cmd: &ExtractCmd) -> Result<ExtractRun, SpawnError> {
        let cwd = std::env::current_dir()
            .map_err(|source| SpawnError::not_submitted("current directory", source))?;
        // Both sides retain the same input digest. Daemon acceptance adds
        // an exact invocation identity in the transport-owned request event.
        let physical_execution = if matches!(self.transport, Transport::Direct(_)) {
            Some(format!(
                "{}:{}",
                std::process::id(),
                PHYSICAL_REQUEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ))
        } else {
            None
        };
        let span = tracing::info_span!(
            "compile_request",
            compile_request = %daemon::compile_request_correlation(&cwd, &cmd.request.worker_argv()),
            request_mode = %cmd.request.mode(),
            execution_layer = match self.transport { Transport::Scoped => "transaction_wrapper", Transport::Direct(_) => "physical", Transport::Daemon { .. } => "endpoint_submission" },
            physical_execution = physical_execution.as_deref(),
            transport = self.transport.name(),
            producer = %self.identity.producer_hex(),
            endpoint = %self.identity,
        );
        let _entered = span.enter();
        let start = Instant::now();
        let output = match &mut self.transport {
            Transport::Direct(endpoint) => {
                let request = daemon::encode_request(&cwd, &cmd.request.worker_argv());
                let mut stdin = endpoint.stdin.take().ok_or_else(|| {
                    SpawnError::indeterminate(
                        endpoint.program.clone(),
                        io::Error::new(io::ErrorKind::BrokenPipe, "bound endpoint is closed"),
                    )
                })?;
                stdin.write_all(&request).map_err(|source| {
                    SpawnError::indeterminate(endpoint.program.clone(), source)
                })?;
                stdin.flush().map_err(|source| {
                    SpawnError::indeterminate(endpoint.program.clone(), source)
                })?;
                drop(stdin);
                crate::EXTRACT_SPAWNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                daemon::decode_output(endpoint.stdout_mut()?).map_err(|error| {
                    SpawnError::indeterminate(
                        endpoint.program.clone(),
                        io::Error::other(error.to_string()),
                    )
                })?
            }
            Transport::Daemon { socket, epoch } => {
                match daemon::execute(socket, epoch, &cwd, &cmd.request.worker_argv()) {
                    Ok(output) => {
                        crate::EXTRACT_SPAWNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        output
                    }
                    Err(error) => {
                        if error.was_accepted() {
                            crate::EXTRACT_SPAWNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        if matches!(error, daemon::DaemonError::Busy) {
                            return Err(SpawnError::capacity(socket.as_os_str()));
                        }
                        let source = io::Error::other(error.to_string());
                        if error.permits_rebind() {
                            return Err(SpawnError::not_submitted(socket.as_os_str(), source));
                        } else {
                            return Err(SpawnError::indeterminate(socket.as_os_str(), source));
                        }
                    }
                }
            }
            Transport::Scoped => TRANSACTION_SCOPE.with(|scope| {
                let mut scope = scope.borrow_mut();
                let transaction = scope
                    .as_mut()
                    .and_then(|state| state.transaction.as_mut())
                    .ok_or_else(|| {
                        SpawnError::indeterminate(
                            "compiler transaction",
                            io::Error::other("compiler transaction scope ended before execution"),
                        )
                    })?;
                transaction.execute(cmd).map(|run| run.output)
            })?,
        };
        Ok(ExtractRun {
            output,
            elapsed: start.elapsed(),
        })
    }
}

struct TransactionScope {
    transaction: Option<CompilerTransaction>,
    program: Option<OsString>,
    cancellation: Option<CompilerTransactionCancellation>,
    admission_close: Vec<CompilerTransactionCloseEvidence>,
}

thread_local! {
    static TRANSACTION_SCOPE: RefCell<Option<TransactionScope>> = const { RefCell::new(None) };
}

fn ensure_scoped_transaction(cmd: &ExtractCmd) -> Result<CompilerIdentity, SpawnError> {
    if let Some((identity, program)) = TRANSACTION_SCOPE.with(|scope| {
        let scope = scope.borrow();
        let state = scope.as_ref()?;
        Some((
            state.transaction.as_ref()?.identity.clone(),
            state.program.clone(),
        ))
    }) {
        if program.as_ref() != Some(&cmd.program) {
            return Err(SpawnError::not_submitted(
                &cmd.program,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "compiler transaction cannot switch compiler producers",
                ),
            ));
        }
        return Ok(identity);
    }
    let cancellation = TRANSACTION_SCOPE.with(|scope| {
        scope
            .borrow()
            .as_ref()
            .and_then(|state| state.cancellation.clone())
    });
    if cancellation
        .as_ref()
        .is_some_and(CompilerTransactionCancellation::is_cancelled)
    {
        return Err(SpawnError::indeterminate(
            "compiler transaction",
            io::Error::new(
                io::ErrorKind::Interrupted,
                "compiler transaction was cancelled",
            ),
        ));
    }
    let transaction =
        CompilerEndpoint::bind_unscoped_with_cancellation(cmd, cancellation.as_ref())?
            .transaction_with_cancellation(cancellation)?;
    let identity = transaction.identity.clone();
    TRANSACTION_SCOPE.with(|scope| -> Result<(), SpawnError> {
        let mut scope = scope.borrow_mut();
        let state = scope.as_mut().ok_or_else(|| {
            SpawnError::indeterminate(
                "compiler transaction",
                io::Error::other("compiler transaction scope ended while binding"),
            )
        })?;
        state.transaction = Some(transaction);
        state.program = Some(cmd.program.clone());
        Ok(())
    })?;
    Ok(identity)
}

fn retain_earlier_close(
    close: CompilerTransactionClose,
    mut earlier: Vec<CompilerTransactionCloseEvidence>,
) -> CompilerTransactionClose {
    if earlier.is_empty() {
        return close;
    }
    let mut evidence = match close {
        CompilerTransactionClose::Unconfirmed(evidence) => evidence,
        settled @ (CompilerTransactionClose::Clean | CompilerTransactionClose::NotStarted) => {
            let Some(evidence) = earlier.pop() else {
                return settled;
            };
            evidence
        }
    };
    evidence.earlier.extend(earlier);
    CompilerTransactionClose::Unconfirmed(evidence)
}

struct TransactionScopeGuard;

impl TransactionScopeGuard {
    fn take_scope() -> Option<TransactionScope> {
        TRANSACTION_SCOPE.with(|scope| scope.borrow_mut().take())
    }

    fn finish(self) -> CompilerTransactionClose {
        let Some(scope) = Self::take_scope() else {
            return CompilerTransactionClose::NotStarted;
        };
        let close = match scope.transaction {
            Some(transaction) => transaction.finish(),
            None => {
                if let Some(cancellation) = scope.cancellation {
                    cancellation.disarm();
                }
                CompilerTransactionClose::NotStarted
            }
        };
        retain_earlier_close(close, scope.admission_close)
    }
}

impl Drop for TransactionScopeGuard {
    fn drop(&mut self) {
        if let Some(mut transaction) = Self::take_scope().and_then(|scope| scope.transaction) {
            let close = transaction.abandon();
            tracing::warn!(?close, "compiler transaction abandoned during unwind");
        }
    }
}

/// Run synchronous compiler preparation calls against one pinned worker.
/// The transaction is created lazily by the first `ExtractCmd::bind` and is
/// always closed before this function returns or unwinds.
pub fn with_compiler_transaction<T>(action: impl FnOnce() -> T) -> CompilerTransactionOutcome<T> {
    with_compiler_transaction_inner(None, action)
}

/// As [`with_compiler_transaction`], with an external cancellation edge that
/// may be triggered when the async owner of the blocking preparation is
/// dropped.
pub fn with_compiler_transaction_cancellable<T>(
    cancellation: CompilerTransactionCancellation,
    action: impl FnOnce() -> T,
) -> CompilerTransactionOutcome<T> {
    with_compiler_transaction_inner(Some(cancellation), action)
}

fn with_compiler_transaction_inner<T>(
    cancellation: Option<CompilerTransactionCancellation>,
    action: impl FnOnce() -> T,
) -> CompilerTransactionOutcome<T> {
    TRANSACTION_SCOPE.with(|scope| {
        assert!(
            scope.borrow().is_none(),
            "compiler transaction scopes cannot nest"
        );
        *scope.borrow_mut() = Some(TransactionScope {
            transaction: None,
            program: None,
            cancellation,
            admission_close: Vec::new(),
        });
    });
    let guard = TransactionScopeGuard;
    let action = action();
    let close = guard.finish();
    CompilerTransactionOutcome { action, close }
}

impl CompilerTransaction {
    pub fn identity(&self) -> &CompilerIdentity {
        &self.identity
    }

    pub fn execute(&mut self, cmd: &ExtractCmd) -> Result<ExtractRun, SpawnError> {
        if self.failed {
            return Err(SpawnError::indeterminate(
                "compiler transaction",
                io::Error::other("compiler transaction cannot continue after a failed request"),
            ));
        }
        let result = self.execute_inner(cmd);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn execute_inner(&mut self, cmd: &ExtractCmd) -> Result<ExtractRun, SpawnError> {
        let cwd = std::env::current_dir()
            .map_err(|source| SpawnError::indeterminate("current directory", source))?;
        let start = Instant::now();
        // The surrounding transaction already crossed its acceptance fence.
        // Count the logical request before transport so a lost response (or
        // an ambiguous partial write) cannot disappear from structural
        // compiler-request accounting.
        crate::EXTRACT_SPAWNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let transport = self.transport.as_mut().ok_or_else(|| {
            SpawnError::indeterminate(
                "compiler transaction",
                io::Error::other("compiler transaction is closed"),
            )
        })?;
        let span = tracing::info_span!(
            "compile_request",
            compile_request = %daemon::compile_request_correlation(&cwd, &cmd.request.worker_argv()),
            request_mode = %cmd.request.mode(),
            execution_layer = if matches!(transport, TransactionTransport::Direct(_)) { "physical" } else { "endpoint_submission" },
            physical_execution = if matches!(transport, TransactionTransport::Direct(_)) { Some(format!("{}:{}", std::process::id(), PHYSICAL_REQUEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed))) } else { None }.as_deref(),
            transport = match transport {
                TransactionTransport::Direct(_) => "direct",
                TransactionTransport::Daemon { .. } => "daemon",
            },
            transaction = true,
            producer = %self.identity.producer_hex(),
            endpoint = %self.identity,
        );
        let _entered = span.enter();
        let output = match transport {
            TransactionTransport::Direct(endpoint) => {
                let request = daemon::encode_request(&cwd, &cmd.request.worker_argv());
                let stdin = endpoint.stdin.as_mut().ok_or_else(|| {
                    SpawnError::indeterminate(
                        endpoint.program.clone(),
                        io::Error::new(io::ErrorKind::BrokenPipe, "compiler transaction is closed"),
                    )
                })?;
                stdin
                    .write_all(&[daemon::TRANSACTION_REQUEST])
                    .and_then(|()| stdin.write_all(&request))
                    .and_then(|()| stdin.flush())
                    .map_err(|source| {
                        SpawnError::indeterminate(endpoint.program.clone(), source)
                    })?;
                daemon::decode_output(endpoint.stdout_mut()?).map_err(|error| {
                    SpawnError::indeterminate(
                        endpoint.program.clone(),
                        io::Error::other(error.to_string()),
                    )
                })?
            }
            TransactionTransport::Daemon {
                transaction,
                socket,
            } => daemon::execute_transaction_request(transaction, &cwd, &cmd.request.worker_argv())
                .map_err(|error| {
                    SpawnError::indeterminate(
                        socket.as_os_str(),
                        io::Error::other(error.to_string()),
                    )
                })?,
        };
        Ok(ExtractRun {
            output,
            elapsed: start.elapsed(),
        })
    }

    pub fn finish(mut self) -> CompilerTransactionClose {
        self.close(false)
    }

    fn abandon(&mut self) -> CompilerTransactionClose {
        self.close(true)
    }

    fn close(&mut self, abandoned: bool) -> CompilerTransactionClose {
        let Some(transport) = self.transport.take() else {
            return CompilerTransactionClose::NotStarted;
        };
        let cancelled = self
            .cancellation
            .as_ref()
            .is_some_and(CompilerTransactionCancellation::is_cancelled);
        let prior = if abandoned {
            Some(CompilerTransactionCloseReason::Abandoned)
        } else if self.failed {
            Some(CompilerTransactionCloseReason::FailedRequest)
        } else if cancelled {
            Some(CompilerTransactionCloseReason::Cancelled)
        } else {
            None
        };
        let close = match transport {
            TransactionTransport::Direct(mut endpoint) => {
                if let Some(reason) = prior {
                    CompilerTransactionClose::Unconfirmed(CompilerTransactionCloseEvidence {
                        reason,
                        retirement: CompilerTransactionRetirement::Direct(endpoint.abort()),
                        earlier: Vec::new(),
                    })
                } else {
                    let end = endpoint
                        .stdin
                        .as_mut()
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "direct compiler stdin is closed",
                            )
                        })
                        .and_then(|stdin| {
                            stdin
                                .write_all(&[daemon::TRANSACTION_END])
                                .and_then(|()| stdin.flush())
                        });
                    drop(endpoint.stdin.take());
                    drop(endpoint.stdout.take());
                    let retirement = wait_for_owned_child(&endpoint.child);
                    endpoint.retired = true;
                    let clean_exit = matches!(&retirement.exit, Ok(status) if status.success())
                        && matches!(retirement.termination, CompilerTermination::NotRequested);
                    if end.is_ok() && clean_exit {
                        CompilerTransactionClose::Clean
                    } else {
                        let reason = match end {
                            Err(source) => CompilerTransactionCloseReason::EndFailed(
                                CompilerTransactionCloseFailure::new(
                                    CompilerTransactionClosePhase::EndWrite,
                                    source,
                                ),
                            ),
                            Ok(()) if retirement.exit.is_err() => {
                                CompilerTransactionCloseReason::FrontendRetirementUnconfirmed
                            }
                            Ok(()) => CompilerTransactionCloseReason::FrontendExitUnsuccessful,
                        };
                        CompilerTransactionClose::Unconfirmed(CompilerTransactionCloseEvidence {
                            reason,
                            retirement: CompilerTransactionRetirement::Direct(retirement),
                            earlier: Vec::new(),
                        })
                    }
                }
            }
            TransactionTransport::Daemon {
                mut transaction,
                socket: _,
            } => {
                if let Some(reason) = prior {
                    let disconnect =
                        transaction
                            .stream
                            .shutdown(Shutdown::Both)
                            .map_err(|source| {
                                CompilerTransactionCloseFailure::new(
                                    CompilerTransactionClosePhase::DaemonDisconnect,
                                    source,
                                )
                            });
                    CompilerTransactionClose::Unconfirmed(CompilerTransactionCloseEvidence {
                        reason,
                        retirement: CompilerTransactionRetirement::DaemonUnobserved {
                            disconnect: Some(disconnect),
                        },
                        earlier: Vec::new(),
                    })
                } else {
                    match daemon::end_transaction(&mut transaction) {
                        Ok(()) => CompilerTransactionClose::Clean,
                        Err(error) => CompilerTransactionClose::Unconfirmed(
                            CompilerTransactionCloseEvidence {
                                reason: CompilerTransactionCloseReason::EndFailed(
                                    CompilerTransactionCloseFailure::new(
                                        CompilerTransactionClosePhase::DaemonEndAcknowledgement,
                                        io::Error::other(error),
                                    ),
                                ),
                                retirement: CompilerTransactionRetirement::DaemonUnobserved {
                                    disconnect: Some(
                                        transaction.stream.shutdown(Shutdown::Both).map_err(
                                            |source| {
                                                CompilerTransactionCloseFailure::new(
                                                    CompilerTransactionClosePhase::DaemonDisconnect,
                                                    source,
                                                )
                                            },
                                        ),
                                    ),
                                },
                                earlier: Vec::new(),
                            },
                        ),
                    }
                }
            }
        };
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.disarm();
        }
        close
    }
}

impl Drop for CompilerTransaction {
    fn drop(&mut self) {
        if self.transport.is_some() {
            let close = self.abandon();
            tracing::warn!(
                ?close,
                "compiler transaction abandoned without explicit finish"
            );
        }
    }
}

pub(crate) fn write_identity(
    mut writer: impl Write,
    producer: &[u8; 32],
    consumed_worker: &[u8; 32],
) -> io::Result<()> {
    writer.write_all(IDENTITY_MAGIC)?;
    writer.write_all(producer)?;
    writer.write_all(consumed_worker)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BinSource, ExtractRequest};
    use std::time::{Duration, Instant};

    fn direct_handshake_fixture(directory: &Path, phase: u8) -> LaunchSpec {
        let source = directory.join("endpoint.rs");
        std::fs::write(&source, include_str!("test_fixtures/control_ack_worker.rs")).unwrap();
        std::fs::write(directory.join("phase"), [phase]).unwrap();
        let executable = directory.join("endpoint");
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compile the existing immutable transport worker"
        )]
        let built = Command::new("rustc")
            .arg(source)
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap();
        assert!(built.success());
        LaunchSpec::direct(executable.into_os_string())
    }

    fn owned_direct_child(
        cancellation: &CompilerTransactionCancellation,
    ) -> Option<Arc<Mutex<Child>>> {
        let state = cancellation.state.lock().unwrap();
        match state.target.as_ref() {
            Some(CancellationTarget::Direct(child)) => Some(Arc::clone(child)),
            _ => None,
        }
    }

    fn settle_handshake_fixture<T>(
        binding: std::thread::ScopedJoinHandle<'_, T>,
        cancellation: &CompilerTransactionCancellation,
        deadline: Instant,
    ) -> T {
        while !binding.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !binding.is_finished() {
            // Keep a removed-deadline regression bounded through the same
            // exact child owner, never a PID reconstructed from a fixture.
            cancellation.cancel();
        }
        binding.join().unwrap()
    }

    fn cancel_stalled_direct_handshake(phase: u8) {
        let directory = tempfile::tempdir().unwrap();
        let spec = direct_handshake_fixture(directory.path(), phase);
        let cancellation = CompilerTransactionCancellation::new();
        let reader_cancellation = cancellation.clone();
        std::thread::scope(|scope| {
            let binding = scope.spawn(move || {
                CompilerEndpoint::bind_launch_with_cancellation(
                    spec,
                    Some(&reader_cancellation),
                    Duration::from_secs(5),
                )
                .and_then(|endpoint| {
                    endpoint.transaction_with_cancellation(Some(reader_cancellation))
                })
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !directory.path().join("stalled").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if !directory.path().join("stalled").exists() {
                cancellation.cancel();
                let _ = binding.join();
                panic!("fixture did not reach the actual handshake barrier");
            }
            let child = owned_direct_child(&cancellation);
            let started = Instant::now();
            // This is the same token edge invoked by the actor's async-drop guard.
            cancellation.cancel();
            let error =
                settle_handshake_fixture(binding, &cancellation, started + Duration::from_secs(2))
                    .unwrap_err();
            let child = child.expect("direct child must be armed before the barrier");
            assert_eq!(error.source.kind(), io::ErrorKind::Interrupted);
            assert_eq!(error.permits_rebind(), phase == 3);
            assert!(started.elapsed() < Duration::from_secs(2));
            let status = child
                .lock()
                .unwrap()
                .try_wait()
                .unwrap()
                .expect("binding owner must reap");
            assert!(!status.success());
        });
    }

    #[test]
    fn direct_identity_handshake_is_cancellable_before_transaction_admission() {
        cancel_stalled_direct_handshake(3);
    }

    #[test]
    fn direct_begin_handshake_is_cancellable_before_acknowledgement() {
        cancel_stalled_direct_handshake(4);
    }

    fn timeout_stalled_direct_handshake(phase: u8) {
        let directory = tempfile::tempdir().unwrap();
        let spec = direct_handshake_fixture(directory.path(), phase);
        let cancellation = CompilerTransactionCancellation::new();
        let reader_cancellation = cancellation.clone();
        let started = Instant::now();
        let error = std::thread::scope(|scope| {
            let binding = scope.spawn(move || {
                CompilerEndpoint::bind_launch_with_cancellation(
                    spec,
                    Some(&reader_cancellation),
                    Duration::from_secs(1),
                )
                .and_then(|endpoint| {
                    endpoint.transaction_with_cancellation_timeout(
                        Some(reader_cancellation),
                        Duration::from_secs(1),
                    )
                })
            });
            settle_handshake_fixture(binding, &cancellation, started + Duration::from_secs(3))
                .unwrap_err()
        });
        assert_eq!(error.source.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.permits_rebind(), phase == 3);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(directory.path().join("stalled").exists());
        let child = owned_direct_child(&cancellation).expect("direct child must be armed");
        let status = child
            .lock()
            .unwrap()
            .try_wait()
            .unwrap()
            .expect("timeout owner must reap");
        assert!(!status.success());
    }

    #[test]
    fn direct_identity_handshake_deadline_terminates_and_reaps_the_child() {
        timeout_stalled_direct_handshake(3);
    }

    #[test]
    fn direct_begin_handshake_deadline_terminates_and_reaps_the_child() {
        timeout_stalled_direct_handshake(4);
    }

    #[test]
    fn direct_handshake_success_keeps_one_worker_for_ordered_requests() {
        let directory = tempfile::tempdir().unwrap();
        let spec = direct_handshake_fixture(directory.path(), 5);
        let cancellation = CompilerTransactionCancellation::new();
        let endpoint = CompilerEndpoint::bind_launch_with_cancellation(
            spec,
            Some(&cancellation),
            Duration::from_secs(5),
        )
        .unwrap();
        let child = owned_direct_child(&cancellation).expect("successful bind must arm child");
        let mut transaction = endpoint
            .transaction_with_cancellation(Some(cancellation.clone()))
            .unwrap();
        let command = ExtractCmd {
            program: "fixture".into(),
            bin_source: BinSource::Explicit,
            request: ExtractRequest::default(),
        };
        for expected in [b"1", b"2"] {
            let response = transaction.execute(&command).unwrap();
            assert!(response.success());
            assert_eq!(response.output.stdout.as_slice(), expected.as_slice());
        }
        assert!(transaction.finish().is_clean());
        assert!(child
            .lock()
            .unwrap()
            .try_wait()
            .unwrap()
            .expect("close must reap")
            .success());
        assert!(cancellation.state.lock().unwrap().target.is_none());
    }

    fn scoped_transport_fixture(
        phase: u8,
        primary_failure: bool,
    ) -> CompilerTransactionOutcome<Result<Vec<u8>, &'static str>> {
        let directory = tempfile::tempdir().unwrap();
        let spec = direct_handshake_fixture(directory.path(), phase);
        with_compiler_transaction(|| {
            let endpoint = CompilerEndpoint::bind_launch(spec).unwrap();
            let identity = endpoint.identity.clone();
            let transaction = endpoint.transaction().unwrap();
            // Install the genuinely admitted transport in the same private scope
            // populated by lazy bind; this fixture never issues compiler authority.
            TRANSACTION_SCOPE
                .with(|scope| scope.borrow_mut().as_mut().unwrap().transaction = Some(transaction));
            let command = ExtractCmd {
                program: "fixture".into(),
                bin_source: BinSource::Explicit,
                request: ExtractRequest::default(),
            };
            let response = CompilerEndpoint {
                identity,
                transport: Transport::Scoped,
            }
            .execute(&command)
            .unwrap();
            if primary_failure {
                Err("primary action refusal")
            } else {
                Ok(response.output.stdout)
            }
        })
    }

    #[test]
    fn completed_success_survives_unsuccessful_frontend_close() {
        let outcome = scoped_transport_fixture(6, false);
        assert_eq!(outcome.action, Ok(b"1".to_vec()));
        let CompilerTransactionClose::Unconfirmed(evidence) = outcome.close else {
            panic!("close failure must remain separate");
        };
        assert_eq!(
            evidence.reason,
            CompilerTransactionCloseReason::FrontendExitUnsuccessful
        );
        let CompilerTransactionRetirement::Direct(retirement) = evidence.retirement else {
            panic!("exact direct retirement required");
        };
        assert_eq!(retirement.exit.unwrap().code(), Some(17));
        assert_eq!(retirement.termination, CompilerTermination::NotRequested);
    }

    #[test]
    fn primary_failure_survives_unsuccessful_frontend_close() {
        let outcome = scoped_transport_fixture(6, true);
        assert_eq!(outcome.action, Err("primary action refusal"));
        assert!(matches!(
            outcome.close,
            CompilerTransactionClose::Unconfirmed(CompilerTransactionCloseEvidence {
                reason: CompilerTransactionCloseReason::FrontendExitUnsuccessful,
                ..
            })
        ));
    }

    #[test]
    fn clean_scope_explicitly_closes_before_returning_completed_action() {
        let outcome = scoped_transport_fixture(5, false);
        assert_eq!(outcome.action, Ok(b"1".to_vec()));
        assert_eq!(outcome.close, CompilerTransactionClose::Clean);
    }

    #[test]
    fn abandonment_quarantines_and_reaps_exact_direct_transport() {
        let directory = tempfile::tempdir().unwrap();
        let cancellation = CompilerTransactionCancellation::new();
        let endpoint = CompilerEndpoint::bind_launch_with_cancellation(
            direct_handshake_fixture(directory.path(), 5),
            Some(&cancellation),
            Duration::from_secs(5),
        )
        .unwrap();
        let child = owned_direct_child(&cancellation).unwrap();
        let mut transaction = endpoint
            .transaction_with_cancellation(Some(cancellation.clone()))
            .unwrap();
        let close = transaction.abandon();
        assert!(
            transaction.transport.is_none(),
            "quarantined transport cannot serve another request"
        );
        assert!(matches!(
            close,
            CompilerTransactionClose::Unconfirmed(CompilerTransactionCloseEvidence {
                reason: CompilerTransactionCloseReason::Abandoned,
                ..
            })
        ));
        assert!(child.lock().unwrap().try_wait().unwrap().is_some());
        assert!(cancellation.state.lock().unwrap().target.is_none());
    }

    #[test]
    fn begin_refusal_retains_actual_retirement_in_scoped_close_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let spec = direct_handshake_fixture(directory.path(), 4);
        let outcome = with_compiler_transaction(|| {
            CompilerEndpoint::bind_launch(spec).and_then(|endpoint| {
                endpoint.transaction_with_cancellation_timeout(None, Duration::from_millis(100))
            })
        });
        let primary = outcome.action.unwrap_err();
        assert_eq!(primary.source.kind(), io::ErrorKind::TimedOut);
        assert!(!primary.permits_rebind());
        let CompilerTransactionClose::Unconfirmed(evidence) = outcome.close else {
            panic!("owned BEGIN cannot become NotStarted");
        };
        assert_eq!(
            evidence.reason,
            CompilerTransactionCloseReason::AdmissionAborted(
                CompilerTransactionClosePhase::BeginHandshake
            )
        );
        let CompilerTransactionRetirement::Direct(retirement) = evidence.retirement else {
            panic!("actual direct retirement required");
        };
        assert!(
            retirement.exit.is_ok(),
            "refusal must retain actual reap evidence"
        );
    }

    #[test]
    fn clean_final_close_does_not_erase_earlier_admission_retirement_uncertainty() {
        let refused = tempfile::tempdir().unwrap();
        let refused_spec = direct_handshake_fixture(refused.path(), 8);
        let healthy = tempfile::tempdir().unwrap();
        let healthy_spec = direct_handshake_fixture(healthy.path(), 5);
        let outcome = with_compiler_transaction(|| {
            let refusal = CompilerEndpoint::bind_launch(refused_spec).unwrap_err();
            assert!(
                refusal.permits_rebind(),
                "identity refusal submitted no compiler request"
            );
            let endpoint = CompilerEndpoint::bind_launch(healthy_spec).unwrap();
            let identity = endpoint.identity.clone();
            let transaction = endpoint.transaction().unwrap();
            TRANSACTION_SCOPE
                .with(|scope| scope.borrow_mut().as_mut().unwrap().transaction = Some(transaction));
            let command = ExtractCmd {
                program: "fixture".into(),
                bin_source: BinSource::Explicit,
                request: ExtractRequest::default(),
            };
            CompilerEndpoint {
                identity,
                transport: Transport::Scoped,
            }
            .execute(&command)
            .unwrap()
            .output
            .stdout
        });
        assert_eq!(outcome.action, b"1".to_vec());
        assert!(matches!(
            outcome.close,
            CompilerTransactionClose::Unconfirmed(CompilerTransactionCloseEvidence {
                reason: CompilerTransactionCloseReason::AdmissionAborted(
                    CompilerTransactionClosePhase::IdentityHandshake
                ),
                ..
            })
        ));
    }

    #[test]
    fn producer_identity_retains_each_compiler_input() {
        let identity = producer_identity(b"frontend", b"worker", OsStr::new("/ghc/lib"));
        for changed in [
            producer_identity(b"changed", b"worker", OsStr::new("/ghc/lib")),
            producer_identity(b"frontend", b"changed", OsStr::new("/ghc/lib")),
            producer_identity(b"frontend", b"worker", OsStr::new("/other/ghc/lib")),
        ] {
            assert_ne!(identity, changed);
        }
    }

    #[test]
    fn a_failed_transaction_refuses_later_requests() {
        let (stream, peer) = UnixStream::pair().unwrap();
        drop(peer);
        let mut transaction = CompilerTransaction {
            identity: CompilerIdentity::direct([1; 32], [2; 32]),
            transport: Some(TransactionTransport::Daemon {
                transaction: daemon::DaemonTransaction::for_test(stream),
                socket: "/tmp/compiler.sock".into(),
            }),
            failed: false,
            cancellation: None,
        };
        let command = ExtractCmd {
            program: "unused".into(),
            bin_source: BinSource::Explicit,
            request: ExtractRequest::default(),
        };

        assert!(transaction.execute(&command).is_err());
        let second = transaction.execute(&command).unwrap_err().to_string();
        assert!(second.contains("cannot continue after a failed request"));
    }

    #[test]
    fn cancellation_interrupts_only_the_owned_direct_child() {
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: throwaway child to test cancellation, not a production launch site"
        )]
        let child = Arc::new(Mutex::new(Command::new("sleep").arg("30").spawn().unwrap()));
        let cancellation = CompilerTransactionCancellation::new();
        cancellation.arm(CancellationTarget::Direct(Arc::clone(&child)));
        let started = Instant::now();
        cancellation.cancel();
        let status = child.lock().unwrap().wait().unwrap();
        assert!(!status.success());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn cancellation_disconnects_the_owned_daemon_transaction() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let cancellation = CompilerTransactionCancellation::new();
        cancellation.arm(CancellationTarget::Daemon(stream));
        cancellation.cancel();
        let mut byte = [0u8; 1];
        assert_eq!(peer.read(&mut byte).unwrap(), 0);
    }

    #[test]
    fn wait_for_owned_child_does_not_wait_forever_for_an_unresponsive_child() {
        // A child that never exits on its own (nothing closes its stdio,
        // nothing sends it a signal) must not wedge whoever is waiting on
        // it — `wait_for_owned_child` is reachable from `Drop`.
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: throwaway unresponsive child, not a production launch site"
        )]
        let child = Arc::new(Mutex::new(Command::new("sleep").arg("60").spawn().unwrap()));
        let started = Instant::now();
        let retirement = wait_for_owned_child(&child);
        assert!(matches!(retirement.exit, Ok(status) if !status.success()));
        assert_eq!(retirement.termination, CompilerTermination::Requested);
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "wait_for_owned_child did not bound its wait: {:?}",
            started.elapsed()
        );
        // `kill()` is asynchronous: give the kernel a moment to make the
        // process waitable before asserting it was reaped.
        let reap_deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            if let Some(status) = child.lock().unwrap().try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < reap_deadline,
                "the deadline branch did not kill and reap the child"
            );
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync poll loop waiting for the child to be reaped"
            )]
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(!status.success());
    }
}
