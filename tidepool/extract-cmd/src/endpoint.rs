use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

use crate::{daemon, process, CompileWorkload, ExtractCmd, ExtractRun, SpawnError};

static PHYSICAL_REQUEST_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

pub(crate) const BOUND_ENDPOINT_FLAG: &str = "--compiler-endpoint-v1";
pub(crate) const IDENTITY_MAGIC: &[u8; 8] = b"TPCID002";

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

/// A bounded lease on one compiler worker. Requests execute in order against
/// one resident GHC transaction and dropping the lease releases admission.
#[derive(Debug)]
pub struct CompilerTransaction {
    workload: CompileWorkload,
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
}

impl DirectEndpoint {
    fn abort(&mut self) {
        drop(self.stdin.take());
        drop(self.stdout.take());
        if let Ok(mut child) = self.child.lock() {
            if let Err(error) = child.kill() {
                tracing::warn!(%error, "failed to kill aborted direct-mode compiler worker");
            }
            if let Err(error) = child.wait() {
                tracing::warn!(%error, "failed to reap aborted direct-mode compiler worker");
            }
        }
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
fn wait_for_owned_child(child: &Arc<Mutex<Child>>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let finished = child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok())
            .flatten()
            .is_some();
        if finished {
            return;
        }
        // Sync context: this poll loop bounds a Drop-reachable path, which
        // cannot be async.
        #[allow(
            clippy::disallowed_methods,
            reason = "sync poll loop reachable from Drop, which cannot await"
        )]
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Ok(mut child) = child.lock() {
        if let Err(error) = child.kill() {
            tracing::warn!(%error, "failed to kill unresponsive direct-mode compiler worker");
        }
        if let Err(error) = child.try_wait() {
            tracing::warn!(%error, "failed to reap unresponsive direct-mode compiler worker");
        }
    }
}

impl Drop for DirectEndpoint {
    fn drop(&mut self) {
        self.abort();
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
        Self::bind_direct(cmd)
    }

    pub(crate) fn bind_direct(cmd: &ExtractCmd) -> Result<Self, SpawnError> {
        Self::bind_launch(LaunchSpec::direct(cmd.program.clone()))
    }

    pub(crate) fn bind_nix(flake_root: &Path) -> Result<Self, SpawnError> {
        Self::bind_launch(LaunchSpec::nix(flake_root))
    }

    fn bind_launch(spec: LaunchSpec) -> Result<Self, SpawnError> {
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
        let stdin = child.stdin.take().ok_or_else(|| {
            SpawnError::not_submitted(
                spec.program.clone(),
                io::Error::other("bound endpoint stdin was not piped"),
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            SpawnError::not_submitted(
                spec.program.clone(),
                io::Error::other("bound endpoint stdout was not piped"),
            )
        })?;
        // Construct the owner before reading the handshake so every failure
        // path closes and reaps the child rather than leaking a half-bound
        // compiler process.
        let mut direct = DirectEndpoint {
            child: Arc::new(Mutex::new(child)),
            stdin: Some(stdin),
            stdout: Some(stdout),
            program: spec.program.clone(),
        };
        let mut magic = [0u8; 8];
        let mut producer = [0u8; 32];
        let mut consumed_worker = [0u8; 32];
        direct
            .stdout_mut()?
            .read_exact(&mut magic)
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
            .stdout_mut()?
            .read_exact(&mut producer)
            .map_err(|source| SpawnError::not_submitted(spec.program.clone(), source))?;
        direct
            .stdout_mut()?
            .read_exact(&mut consumed_worker)
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
        self.transaction_with_cancellation(CompileWorkload::Foreground, None)
    }

    pub fn transaction_for_workload(
        self,
        workload: CompileWorkload,
    ) -> Result<CompilerTransaction, SpawnError> {
        self.transaction_with_cancellation(workload, None)
    }

    fn transaction_with_cancellation(
        self,
        workload: CompileWorkload,
        cancellation: Option<CompilerTransactionCancellation>,
    ) -> Result<CompilerTransaction, SpawnError> {
        let identity = self.identity.clone();
        let transport_name = self.transport.name();
        let admission_started = Instant::now();
        let transport = match self.transport {
            Transport::Direct(mut endpoint) => {
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
                    .stdout_mut()?
                    .read_exact(&mut accepted)
                    .map_err(|source| {
                        SpawnError::indeterminate(endpoint.program.clone(), source)
                    })?;
                if accepted != [1] {
                    return Err(SpawnError::indeterminate(
                        endpoint.program.clone(),
                        io::Error::new(io::ErrorKind::InvalidData, "compiler rejected transaction"),
                    ));
                }
                if let Some(cancellation) = &cancellation {
                    cancellation.arm(CancellationTarget::Direct(Arc::clone(&endpoint.child)));
                }
                TransactionTransport::Direct(endpoint)
            }
            Transport::Daemon { socket, epoch } => {
                let transaction = daemon::begin_transaction_for_workload(
                    &socket,
                    &epoch,
                    workload,
                    cancellation.as_ref(),
                )
                .map_err(|error| {
                    if matches!(error, daemon::DaemonError::Busy) {
                        return SpawnError::capacity(socket.as_os_str());
                    }
                    let source = io::Error::other(error.to_string());
                    if error.permits_rebind() {
                        SpawnError::not_submitted(socket.as_os_str(), source)
                    } else {
                        SpawnError::indeterminate(socket.as_os_str(), source)
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
            workload,
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
    workload: CompileWorkload,
    transaction: Option<CompilerTransaction>,
    program: Option<OsString>,
    cancellation: Option<CompilerTransactionCancellation>,
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
    let workload = TRANSACTION_SCOPE
        .with(|scope| scope.borrow().as_ref().map(|state| state.workload))
        .unwrap_or(CompileWorkload::Foreground);
    let transaction = CompilerEndpoint::bind_unscoped(cmd)?
        .transaction_with_cancellation(workload, cancellation)?;
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

struct TransactionScopeGuard;

impl Drop for TransactionScopeGuard {
    fn drop(&mut self) {
        let transaction = TRANSACTION_SCOPE.with(|scope| {
            scope
                .borrow_mut()
                .take()
                .and_then(|state| state.transaction)
        });
        if let Some(transaction) = transaction {
            if let Err(error) = transaction.finish() {
                tracing::warn!(%error, "compiler transaction did not close cleanly on scope exit");
            }
        }
    }
}

/// Run synchronous compiler preparation calls against one pinned worker.
/// The transaction is created lazily by the first `ExtractCmd::bind` and is
/// always closed before this function returns or unwinds.
pub fn with_compiler_transaction<T>(action: impl FnOnce() -> T) -> T {
    with_compiler_transaction_inner(CompileWorkload::Foreground, None, action)
}

/// As [`with_compiler_transaction`], with an external cancellation edge that
/// may be triggered when the async owner of the blocking preparation is
/// dropped.
pub fn with_compiler_transaction_cancellable<T>(
    cancellation: CompilerTransactionCancellation,
    action: impl FnOnce() -> T,
) -> T {
    with_compiler_transaction_inner(CompileWorkload::Foreground, Some(cancellation), action)
}

/// Declare the workload before the first bind admits the pinned transaction.
/// Invoke inside the blocking compiler task; the typed class is not propagated
/// implicitly across async or OS-thread boundaries.
pub fn with_compiler_transaction_for_workload<T>(
    workload: CompileWorkload,
    action: impl FnOnce() -> T,
) -> T {
    with_compiler_transaction_inner(workload, None, action)
}

pub fn with_compiler_transaction_cancellable_for_workload<T>(
    workload: CompileWorkload,
    cancellation: CompilerTransactionCancellation,
    action: impl FnOnce() -> T,
) -> T {
    with_compiler_transaction_inner(workload, Some(cancellation), action)
}

fn with_compiler_transaction_inner<T>(
    workload: CompileWorkload,
    cancellation: Option<CompilerTransactionCancellation>,
    action: impl FnOnce() -> T,
) -> T {
    TRANSACTION_SCOPE.with(|scope| {
        assert!(
            scope.borrow().is_none(),
            "compiler transaction scopes cannot nest"
        );
        *scope.borrow_mut() = Some(TransactionScope {
            workload,
            transaction: None,
            program: None,
            cancellation,
        });
    });
    let guard = TransactionScopeGuard;
    let result = action();
    drop(guard);
    result
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
        let mut cmd = cmd.clone();
        cmd.request.set_workload(self.workload);
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

    pub fn finish(mut self) -> Result<(), SpawnError> {
        self.close()
    }

    fn close(&mut self) -> Result<(), SpawnError> {
        let close_started = self.transport.as_ref().map(|_| Instant::now());
        let result = match self.transport.take() {
            None => Ok(()),
            Some(mut transport) => match &mut transport {
                TransactionTransport::Direct(endpoint) => {
                    if self.failed {
                        endpoint.abort();
                        Ok(())
                    } else {
                        let result = if let Some(stdin) = endpoint.stdin.as_mut() {
                            stdin
                                .write_all(&[daemon::TRANSACTION_END])
                                .and_then(|()| stdin.flush())
                                .map_err(|source| {
                                    SpawnError::indeterminate(endpoint.program.clone(), source)
                                })
                        } else {
                            Ok(())
                        };
                        drop(endpoint.stdin.take());
                        // Nothing more is read from this endpoint once
                        // `TRANSACTION_END` is sent (the worker owes it no
                        // reply). Closing the read end too, before waiting,
                        // lets a worker still blocked writing to a full
                        // stdout pipe get EPIPE and exit instead of wedging
                        // this wait.
                        drop(endpoint.stdout.take());
                        wait_for_owned_child(&endpoint.child);
                        result
                    }
                }
                TransactionTransport::Daemon {
                    transaction,
                    socket,
                } => {
                    if self.failed {
                        // best-effort: the transaction already failed; the
                        // peer may already have closed its end.
                        transaction.stream.shutdown(Shutdown::Both).ok();
                        Ok(())
                    } else {
                        daemon::end_transaction(transaction).map_err(|error| {
                            SpawnError::indeterminate(
                                socket.as_os_str(),
                                io::Error::other(error.to_string()),
                            )
                        })
                    }
                }
            },
        };
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.disarm();
        }
        if let Some(started) = close_started {
            tracing::debug!(
                phase = "compiler_transaction_close",
                elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                success = result.is_ok(),
                "compiler transaction closed"
            );
        }
        result
    }
}

impl Drop for CompilerTransaction {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            tracing::warn!(%error, "compiler transaction did not close cleanly on drop");
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
            workload: CompileWorkload::Foreground,
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
        wait_for_owned_child(&child);
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
