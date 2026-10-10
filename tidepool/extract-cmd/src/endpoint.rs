use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
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
    input_files: CompilerInputFiles,
}

impl CompilerTransactionCloseEvidence {
    pub fn new(
        reason: CompilerTransactionCloseReason,
        retirement: CompilerTransactionRetirement,
    ) -> Self {
        Self {
            reason,
            retirement,
            earlier: Vec::new(),
            input_files: CompilerInputFiles::default(),
        }
    }
}

/// Physical input custody only; artifact selection and validation belong to the
/// caller. Keeping the original descriptor preserves its published /proc path.
#[derive(Clone, Debug, Default)]
struct CompilerInputFiles(BTreeMap<RawFd, Arc<File>>);

impl CompilerInputFiles {
    fn insert(&mut self, file: Arc<File>) {
        self.0.entry(file.as_raw_fd()).or_insert(file);
    }

    fn extend(&mut self, files: Self) {
        for file in files.0.into_values() {
            self.insert(file);
        }
    }
}

impl PartialEq for CompilerInputFiles {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self.0.iter().all(|(fd, file)| {
                other
                    .0
                    .get(fd)
                    .is_some_and(|other| Arc::ptr_eq(file, other))
            })
    }
}
impl Eq for CompilerInputFiles {}

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
    FrontendReportedFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompilerTransactionClosePhase {
    IdentityHandshake,
    BeginHandshake,
    EndWrite,
    EndRead,
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
    pub worker_report: Option<CompilerFrontendCloseReport>,
    _retained_child: Option<RetainedCompilerChild>,
}

/// Facts reported by the paired frontend; its own reap remains independent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerFrontendCloseReport {
    pub worker: CompilerWorkerRetirement,
    pub scratch: CompilerScratchRetirement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerWorkerRetirement {
    Reaped(std::process::ExitStatus),
    WaitUnconfirmed(CompilerIoCause),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerScratchRetirement {
    NotObserved,
    Confirmed,
    Unconfirmed(Vec<CompilerScratchFailure>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerScratchFailure {
    pub path: PathBuf,
    pub phase: crate::frontend::ScratchCleanupPhase,
    pub cause: CompilerIoCause,
}

/// The exact OS code and diagnostic observation cross the process boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerIoCause {
    pub raw_os_error: Option<i32>,
    pub kind: io::ErrorKind,
    pub message: String,
}

impl From<&io::Error> for CompilerIoCause {
    fn from(error: &io::Error) -> Self {
        Self {
            raw_os_error: error.raw_os_error(),
            kind: error.kind(),
            message: error.to_string(),
        }
    }
}

const FAILURE_END_MAGIC: &[u8; 8] = b"TPCEND01";
const MAX_FAILURE_END_BYTES: usize = 64 * 1024;
const MAX_SCRATCH_FAILURES: usize = 128;
const DIRECT_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

fn invalid_close(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl CompilerFrontendCloseReport {
    fn validate(&self) -> io::Result<()> {
        match (&self.worker, &self.scratch) {
            (
                CompilerWorkerRetirement::WaitUnconfirmed(_),
                CompilerScratchRetirement::NotObserved,
            ) => Ok(()),
            (CompilerWorkerRetirement::Reaped(status), CompilerScratchRetirement::Confirmed)
                if !status.success() =>
            {
                Ok(())
            }
            (
                CompilerWorkerRetirement::Reaped(_),
                CompilerScratchRetirement::Unconfirmed(failures),
            ) if !failures.is_empty() && failures.len() <= MAX_SCRATCH_FAILURES => Ok(()),
            _ => Err(invalid_close("contradictory compiler failure END report")),
        }
    }
}

fn append_close_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_FAILURE_END_BYTES.saturating_sub(output.len()) {
        return Err(invalid_close(
            "compiler failure END report exceeds total budget",
        ));
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn append_close_field(output: &mut Vec<u8>, bytes: &[u8]) -> io::Result<()> {
    let size =
        u32::try_from(bytes.len()).map_err(|_| invalid_close("oversized compiler close field"))?;
    append_close_bytes(output, &size.to_le_bytes())?;
    append_close_bytes(output, bytes)
}

// Non-OS IO failures use a bounded typed vocabulary. OS errors retain their
// exact errno; ErrorKind is derived by the matching frontend/caller platform.
const CLOSE_IO_KINDS: &[io::ErrorKind] = &[
    io::ErrorKind::Other,
    io::ErrorKind::NotFound,
    io::ErrorKind::PermissionDenied,
    io::ErrorKind::ConnectionRefused,
    io::ErrorKind::ConnectionReset,
    io::ErrorKind::ConnectionAborted,
    io::ErrorKind::NotConnected,
    io::ErrorKind::AddrInUse,
    io::ErrorKind::AddrNotAvailable,
    io::ErrorKind::BrokenPipe,
    io::ErrorKind::AlreadyExists,
    io::ErrorKind::WouldBlock,
    io::ErrorKind::InvalidInput,
    io::ErrorKind::InvalidData,
    io::ErrorKind::TimedOut,
    io::ErrorKind::WriteZero,
    io::ErrorKind::Interrupted,
    io::ErrorKind::UnexpectedEof,
    io::ErrorKind::Unsupported,
    io::ErrorKind::OutOfMemory,
];

fn append_close_cause(output: &mut Vec<u8>, cause: &CompilerIoCause) -> io::Result<()> {
    match cause.raw_os_error {
        Some(code) => {
            if io::Error::from_raw_os_error(code).kind() != cause.kind {
                return Err(invalid_close("compiler close errno/kind mismatch"));
            }
            append_close_bytes(output, &[1])?;
            append_close_bytes(output, &code.to_le_bytes())?;
        }
        None => {
            let kind = CLOSE_IO_KINDS
                .iter()
                .position(|kind| *kind == cause.kind)
                .ok_or_else(|| invalid_close("unsupported non-OS compiler close IO kind"))?;
            append_close_bytes(output, &[0, kind as u8])?;
        }
    }
    append_close_field(output, cause.message.as_bytes())
}

fn encode_failure_end(report: &CompilerFrontendCloseReport) -> io::Result<Vec<u8>> {
    report.validate()?;
    let mut body = Vec::new();
    match &report.worker {
        CompilerWorkerRetirement::Reaped(status) => {
            append_close_bytes(&mut body, &[1])?;
            append_close_bytes(&mut body, &status.into_raw().to_le_bytes())?;
        }
        CompilerWorkerRetirement::WaitUnconfirmed(cause) => {
            append_close_bytes(&mut body, &[2])?;
            append_close_cause(&mut body, cause)?;
        }
    }
    match &report.scratch {
        CompilerScratchRetirement::NotObserved => append_close_bytes(&mut body, &[0])?,
        CompilerScratchRetirement::Confirmed => append_close_bytes(&mut body, &[1])?,
        CompilerScratchRetirement::Unconfirmed(failures) => {
            append_close_bytes(&mut body, &[2])?;
            append_close_bytes(&mut body, &(failures.len() as u32).to_le_bytes())?;
            for failure in failures {
                let phase = match failure.phase {
                    crate::frontend::ScratchCleanupPhase::Products => 1,
                    crate::frontend::ScratchCleanupPhase::EmptyNamespace => 2,
                };
                append_close_bytes(&mut body, &[phase])?;
                append_close_field(&mut body, failure.path.as_os_str().as_bytes())?;
                append_close_cause(&mut body, &failure.cause)?;
            }
        }
    }
    Ok(body)
}

pub(crate) fn write_failure_end(
    writer: &mut impl Write,
    report: &CompilerFrontendCloseReport,
) -> io::Result<()> {
    let body = encode_failure_end(report)?;
    writer.write_all(FAILURE_END_MAGIC)?;
    writer.write_all(&(body.len() as u32).to_le_bytes())?;
    writer.write_all(&body)?;
    writer.flush()
}

struct CloseDecoder<'a>(&'a [u8]);
impl<'a> CloseDecoder<'a> {
    fn take(&mut self, size: usize) -> io::Result<&'a [u8]> {
        if size > self.0.len() {
            return Err(invalid_close("truncated compiler close field"));
        }
        let (value, rest) = self.0.split_at(size);
        self.0 = rest;
        Ok(value)
    }
    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn word(&mut self) -> io::Result<[u8; 4]> {
        self.take(4)?
            .try_into()
            .map_err(|_| invalid_close("truncated compiler close word"))
    }
    fn field(&mut self) -> io::Result<&'a [u8]> {
        let size = u32::from_le_bytes(self.word()?) as usize;
        self.take(size)
    }
    fn cause(&mut self) -> io::Result<CompilerIoCause> {
        let (raw_os_error, kind) = match self.byte()? {
            1 => {
                let code = i32::from_le_bytes(self.word()?);
                (Some(code), io::Error::from_raw_os_error(code).kind())
            }
            0 => {
                let code = self.byte()? as usize;
                let kind = *CLOSE_IO_KINDS
                    .get(code)
                    .ok_or_else(|| invalid_close("unknown compiler close IO kind"))?;
                (None, kind)
            }
            _ => return Err(invalid_close("unknown compiler close IO cause")),
        };
        let message = std::str::from_utf8(self.field()?)
            .map_err(|_| invalid_close("non-UTF8 compiler IO diagnostic"))?
            .to_owned();
        Ok(CompilerIoCause {
            raw_os_error,
            kind,
            message,
        })
    }
}

fn decode_failure_end(body: &[u8]) -> io::Result<CompilerFrontendCloseReport> {
    if body.len() > MAX_FAILURE_END_BYTES {
        return Err(invalid_close("oversized compiler failure END report"));
    }
    let mut decoder = CloseDecoder(body);
    let worker = match decoder.byte()? {
        1 => CompilerWorkerRetirement::Reaped(std::process::ExitStatus::from_raw(
            i32::from_le_bytes(decoder.word()?),
        )),
        2 => CompilerWorkerRetirement::WaitUnconfirmed(decoder.cause()?),
        _ => return Err(invalid_close("unknown compiler worker retirement")),
    };
    let scratch = match decoder.byte()? {
        0 => CompilerScratchRetirement::NotObserved,
        1 => CompilerScratchRetirement::Confirmed,
        2 => {
            let count = u32::from_le_bytes(decoder.word()?) as usize;
            if count == 0 || count > MAX_SCRATCH_FAILURES {
                return Err(invalid_close("invalid scratch failure count"));
            }
            let mut failures = Vec::with_capacity(count);
            for _ in 0..count {
                let phase = match decoder.byte()? {
                    1 => crate::frontend::ScratchCleanupPhase::Products,
                    2 => crate::frontend::ScratchCleanupPhase::EmptyNamespace,
                    _ => return Err(invalid_close("unknown scratch cleanup phase")),
                };
                let path = PathBuf::from(OsString::from_vec(decoder.field()?.to_owned()));
                let cause = decoder.cause()?;
                failures.push(CompilerScratchFailure { path, phase, cause });
            }
            CompilerScratchRetirement::Unconfirmed(failures)
        }
        _ => return Err(invalid_close("unknown scratch retirement")),
    };
    if !decoder.0.is_empty() {
        return Err(invalid_close("trailing compiler failure END fields"));
    }
    let report = CompilerFrontendCloseReport { worker, scratch };
    report.validate()?;
    Ok(report)
}

fn read_failure_end(
    reader: &mut (impl Read + AsRawFd),
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> io::Result<Option<CompilerFrontendCloseReport>> {
    let accept_eof = || {
        if cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "compiler close cancelled",
            ))
        } else if Instant::now() >= deadline {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "compiler close deadline exceeded",
            ))
        } else {
            Ok(())
        }
    };
    let mut magic = [0u8; 8];
    match process::read_exact_until(reader, &mut magic[..1], deadline, &cancelled) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            accept_eof()?;
            return Ok(None);
        }
        result => result?,
    }
    process::read_exact_until(reader, &mut magic[1..], deadline, &cancelled)?;
    if &magic != FAILURE_END_MAGIC {
        return Err(invalid_close("invalid compiler failure END magic"));
    }
    let mut length = [0u8; 4];
    process::read_exact_until(reader, &mut length, deadline, &cancelled)?;
    let size = u32::from_le_bytes(length) as usize;
    if size > MAX_FAILURE_END_BYTES {
        return Err(invalid_close(
            "compiler failure END exceeds allocation budget",
        ));
    }
    let mut body = vec![0u8; size];
    process::read_exact_until(reader, &mut body, deadline, &cancelled)?;
    let report = decode_failure_end(&body)?;
    let mut trailing = [0u8; 1];
    match process::read_exact_until(reader, &mut trailing, deadline, &cancelled) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            accept_eof()?;
            Ok(Some(report))
        }
        Err(error) => Err(error),
        Ok(()) => Err(invalid_close("trailing bytes after compiler failure END")),
    }
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
    retired: bool,
    admission_phase: CompilerTransactionClosePhase,
    #[cfg(test)]
    wait_observation_fault: bool,
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
        #[cfg(test)]
        if self.wait_observation_fault {
            return retire_owned_child_observing(&self.child, true, || {
                Err(io::Error::other("injected retirement observation failure"))
            });
        }
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
    retire_owned_child_observing(child, abort, || {
        child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_wait()
    })
}

fn retire_owned_child_observing(
    child: &Arc<Mutex<Child>>,
    abort: bool,
    mut observe: impl FnMut() -> io::Result<Option<std::process::ExitStatus>>,
) -> DirectCompilerRetirement {
    let mut termination = CompilerTermination::NotRequested;
    let mut deadline = Instant::now() + Duration::from_secs(5);
    if abort {
        termination = request_child_termination(child);
    }
    loop {
        match observe() {
            Ok(Some(status)) => {
                return DirectCompilerRetirement {
                    worker_report: None,
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
                    worker_report: None,
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
                    worker_report: None,
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
                            input_files: CompilerInputFiles::default(),
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
    #[tracing::instrument(
        target = "exomonad_harness::timing",
        name = "compiler_endpoint.bind",
        level = "debug",
        skip_all,
        fields(inclusive = true)
    )]
    pub(crate) fn bind(cmd: &ExtractCmd) -> Result<Self, SpawnError> {
        if TRANSACTION_SCOPE.with(|scope| scope.borrow().is_some()) {
            let identity = bind_scoped_endpoint(cmd, |cancellation| {
                Self::bind_unscoped_with_cancellation(cmd, cancellation)
            })?;
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
            #[cfg(test)]
            wait_observation_fault: false,
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
        self.transaction_with_cancellation_timeout(workload, cancellation, DIRECT_HANDSHAKE_TIMEOUT)
    }

    #[tracing::instrument(
        target = "exomonad_harness::timing",
        name = "compiler_transaction.begin",
        level = "debug",
        skip_all,
        fields(inclusive = true)
    )]
    fn transaction_with_cancellation_timeout(
        self,
        workload: CompileWorkload,
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
                    if matches!(error, daemon::DaemonError::CapacityRefusal(_)) {
                        return SpawnError::capacity_refusal(socket.as_os_str(), error.to_string());
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
                                        input_files: CompilerInputFiles::default(),
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
                        if matches!(error, daemon::DaemonError::CapacityRefusal(_)) {
                            return Err(SpawnError::capacity_refusal(
                                socket.as_os_str(),
                                error.to_string(),
                            ));
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
            Transport::Scoped => {
                ensure_scoped_transaction(cmd, &self.identity)?;
                TRANSACTION_SCOPE.with(|scope| {
                    let mut scope = scope.borrow_mut();
                    let Some(ScopedCompiler::Active { transaction, .. }) =
                        scope.as_mut().map(|state| &mut state.compiler)
                    else {
                        return Err(SpawnError::indeterminate(
                            "compiler transaction",
                            io::Error::other("compiler transaction scope ended before execution"),
                        ));
                    };
                    transaction.execute(cmd).map(|run| run.output)
                })?
            }
        };
        Ok(ExtractRun {
            output,
            elapsed: start.elapsed(),
        })
    }
}

/// A daemon preflight retains identity without acquiring a worker or CPU grant.
/// Direct identity handshakes own a child and keep their existing admission path.
#[derive(Clone)]
struct BoundDaemonEndpoint {
    identity: CompilerIdentity,
    socket: PathBuf,
    epoch: [u8; 32],
    program: OsString,
}

enum ScopedCompiler {
    Unbound,
    DaemonBound(BoundDaemonEndpoint),
    Active {
        transaction: CompilerTransaction,
        program: OsString,
    },
    /// An uncertain BEGIN cannot be retried by a later borrowed endpoint.
    AdmissionFailed(BoundDaemonEndpoint),
}

impl ScopedCompiler {
    fn identity_and_program(&self) -> Option<(&CompilerIdentity, &OsStr)> {
        match self {
            Self::Unbound => None,
            Self::DaemonBound(bound) | Self::AdmissionFailed(bound) => {
                Some((&bound.identity, &bound.program))
            }
            Self::Active {
                transaction,
                program,
            } => Some((&transaction.identity, program)),
        }
    }
}

struct TransactionScope {
    workload: CompileWorkload,
    compiler: ScopedCompiler,
    cancellation: Option<CompilerTransactionCancellation>,
    admission_close: Vec<CompilerTransactionCloseEvidence>,
    input_files: CompilerInputFiles,
}

thread_local! {
    static TRANSACTION_SCOPE: RefCell<Option<TransactionScope>> = const { RefCell::new(None) };
}

/// Retain an input descriptor through the enclosing compiler transaction's END
/// acknowledgement. An unconfirmed close carries it with the close evidence.
/// This grants no artifact authority and never duplicates the descriptor.
///
/// Returns false outside a scoped transaction, without acquiring custody. An
/// explicit transaction's caller must retain inputs through `finish`; an
/// indeterminate one-shot failure is not proof that the worker has stopped.
#[must_use]
pub fn retain_compiler_input_file(file: Arc<File>) -> bool {
    TRANSACTION_SCOPE.with(|scope| {
        let mut scope = scope.borrow_mut();
        let Some(scope) = scope.as_mut() else {
            return false;
        };
        scope.input_files.insert(file);
        true
    })
}

/// Check cooperative host work against its enclosing compiler cancellation owner.
/// Unscoped work is allowed. An interrupted checkpoint leaves accepted requests
/// and transaction cleanup custody with the existing scope.
pub fn compiler_host_checkpoint() -> io::Result<()> {
    let cancelled = TRANSACTION_SCOPE.with(|scope| {
        scope.borrow().as_ref().is_some_and(|state| {
            state
                .cancellation
                .as_ref()
                .is_some_and(CompilerTransactionCancellation::is_cancelled)
        })
    });
    if cancelled {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "compiler host work was cancelled",
        ))
    } else {
        Ok(())
    }
}

fn bind_scoped_endpoint(
    cmd: &ExtractCmd,
    bind: impl FnOnce(Option<&CompilerTransactionCancellation>) -> Result<CompilerEndpoint, SpawnError>,
) -> Result<CompilerIdentity, SpawnError> {
    let existing = TRANSACTION_SCOPE.with(|scope| {
        let scope = scope.borrow();
        let state = scope.as_ref()?;
        let (identity, program) = state.compiler.identity_and_program()?;
        Some((
            identity.clone(),
            program.to_owned(),
            matches!(&state.compiler, ScopedCompiler::AdmissionFailed(_)),
        ))
    });
    if let Some((identity, program, failed)) = existing {
        if failed {
            return Err(SpawnError::indeterminate(
                &cmd.program,
                io::Error::other("compiler transaction cannot continue after uncertain admission"),
            ));
        }
        if program != cmd.program {
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
    let (workload, cancellation) = TRANSACTION_SCOPE
        .with(|scope| {
            scope
                .borrow()
                .as_ref()
                .map(|state| (state.workload, state.cancellation.clone()))
        })
        .ok_or_else(|| {
            SpawnError::not_submitted(
                &cmd.program,
                io::Error::other("compiler transaction scope ended before binding"),
            )
        })?;
    if cancellation
        .as_ref()
        .is_some_and(CompilerTransactionCancellation::is_cancelled)
    {
        return Err(SpawnError::not_submitted(
            &cmd.program,
            io::Error::new(
                io::ErrorKind::Interrupted,
                "compiler transaction was cancelled before admission",
            ),
        ));
    }
    let endpoint = bind(cancellation.as_ref())?;
    let identity = endpoint.identity.clone();
    let compiler = match endpoint {
        CompilerEndpoint {
            identity,
            transport: Transport::Daemon { socket, epoch },
        } => ScopedCompiler::DaemonBound(BoundDaemonEndpoint {
            identity,
            socket,
            epoch,
            program: cmd.program.clone(),
        }),
        endpoint => ScopedCompiler::Active {
            transaction: endpoint.transaction_with_cancellation(workload, cancellation)?,
            program: cmd.program.clone(),
        },
    };
    TRANSACTION_SCOPE.with(|scope| -> Result<(), SpawnError> {
        let mut scope = scope.borrow_mut();
        let state = scope.as_mut().ok_or_else(|| {
            SpawnError::indeterminate(
                "compiler transaction",
                io::Error::other("compiler transaction scope ended while binding"),
            )
        })?;
        state.compiler = compiler;
        Ok(())
    })?;
    Ok(identity)
}

fn ensure_scoped_transaction(
    cmd: &ExtractCmd,
    expected: &CompilerIdentity,
) -> Result<(), SpawnError> {
    let (bound, workload, cancellation) = TRANSACTION_SCOPE.with(|scope| {
        let scope = scope.borrow();
        let state = scope.as_ref().ok_or_else(|| {
            SpawnError::indeterminate(
                "compiler transaction",
                io::Error::other("compiler transaction scope ended before execution"),
            )
        })?;
        let Some((identity, program)) = state.compiler.identity_and_program() else {
            return Err(SpawnError::not_submitted(
                &cmd.program,
                io::Error::other("borrowed endpoint has no bound compiler owner"),
            ));
        };
        if identity != expected || program != cmd.program.as_os_str() {
            return Err(SpawnError::not_submitted(
                &cmd.program,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "borrowed endpoint differs from its bound compiler owner",
                ),
            ));
        }
        match &state.compiler {
            ScopedCompiler::DaemonBound(bound) => Ok((
                Some(bound.clone()),
                state.workload,
                state.cancellation.clone(),
            )),
            ScopedCompiler::Active { .. } => Ok((None, state.workload, state.cancellation.clone())),
            ScopedCompiler::AdmissionFailed(_) => Err(SpawnError::indeterminate(
                &cmd.program,
                io::Error::other("compiler transaction cannot continue after uncertain admission"),
            )),
            ScopedCompiler::Unbound => unreachable!("bound identity checked"),
        }
    })?;
    let Some(bound) = bound else {
        return Ok(());
    };
    if cancellation
        .as_ref()
        .is_some_and(CompilerTransactionCancellation::is_cancelled)
    {
        return Err(SpawnError::not_submitted(
            &cmd.program,
            io::Error::new(
                io::ErrorKind::Interrupted,
                "compiler transaction was cancelled before admission",
            ),
        ));
    }
    // Use the authenticated socket/epoch captured before host preparation. A
    // rejected epoch returns to the caller; it never selects a new producer.
    let endpoint = CompilerEndpoint {
        identity: bound.identity.clone(),
        transport: Transport::Daemon {
            socket: bound.socket.clone(),
            epoch: bound.epoch,
        },
    };
    match endpoint.transaction_with_cancellation(workload, cancellation) {
        Ok(transaction) => TRANSACTION_SCOPE.with(|scope| {
            scope.borrow_mut().as_mut().expect("owning scope").compiler = ScopedCompiler::Active {
                transaction,
                program: bound.program,
            };
            Ok(())
        }),
        Err(error) => {
            if !error.definitely_unsubmitted() {
                TRANSACTION_SCOPE.with(|scope| {
                    scope.borrow_mut().as_mut().expect("owning scope").compiler =
                        ScopedCompiler::AdmissionFailed(bound);
                });
            }
            Err(error)
        }
    }
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

struct TransactionScopeGuard<C: FnOnce(CompilerTransactionClose)> {
    close_sink: Option<C>,
}

#[tracing::instrument(
    target = "exomonad_harness::timing",
    name = "compiler_transaction.finish_scope",
    level = "debug",
    skip_all,
    fields(abandoned, inclusive = true)
)]
fn finish_scope(abandoned: bool) -> CompilerTransactionClose {
    let Some(scope) = TRANSACTION_SCOPE.with(|scope| scope.borrow_mut().take()) else {
        return CompilerTransactionClose::NotStarted;
    };
    let close = match scope.compiler {
        ScopedCompiler::Active {
            mut transaction, ..
        } if abandoned => transaction.abandon(),
        ScopedCompiler::Active { transaction, .. } => transaction.finish(),
        ScopedCompiler::Unbound
        | ScopedCompiler::DaemonBound(_)
        | ScopedCompiler::AdmissionFailed(_) => {
            if let Some(cancellation) = scope.cancellation {
                cancellation.disarm();
            }
            CompilerTransactionClose::NotStarted
        }
    };
    let mut close = retain_earlier_close(close, scope.admission_close);
    if let CompilerTransactionClose::Unconfirmed(evidence) = &mut close {
        evidence.input_files.extend(scope.input_files);
    }
    close
}

impl<C: FnOnce(CompilerTransactionClose)> TransactionScopeGuard<C> {
    fn finish(mut self) -> CompilerTransactionClose {
        let close = finish_scope(false);
        if let Some(sink) = self.close_sink.take() {
            sink(close.clone());
        }
        close
    }
}

impl<C: FnOnce(CompilerTransactionClose)> Drop for TransactionScopeGuard<C> {
    fn drop(&mut self) {
        if let Some(sink) = self.close_sink.take() {
            // The owning sink retains exact retirement custody even during unwind.
            sink(finish_scope(true));
        }
    }
}

/// Run synchronous compiler preparation calls against one pinned worker.
/// The affine close sink is armed before the first action and invoked once on
/// normal finish or abandonment. It must retain close evidence without panicking
/// or performing asynchronous work. Cleanup never replaces the action result.
pub fn with_compiler_transaction<T>(
    close_sink: impl FnOnce(CompilerTransactionClose),
    action: impl FnOnce() -> T,
) -> CompilerTransactionOutcome<T> {
    with_compiler_transaction_inner(CompileWorkload::Foreground, None, close_sink, action)
}

/// As [`with_compiler_transaction`], with an external cancellation edge that
/// may be triggered when the async owner of the blocking preparation is dropped.
pub fn with_compiler_transaction_cancellable<T>(
    cancellation: CompilerTransactionCancellation,
    close_sink: impl FnOnce(CompilerTransactionClose),
    action: impl FnOnce() -> T,
) -> CompilerTransactionOutcome<T> {
    with_compiler_transaction_inner(
        CompileWorkload::Foreground,
        Some(cancellation),
        close_sink,
        action,
    )
}

/// Declare the workload before execution admits the pinned daemon transaction.
/// Binding observes its authenticated identity without reserving capacity.
/// Invoke inside the blocking compiler task; the typed class is not propagated
/// implicitly across async or OS-thread boundaries.
pub fn with_compiler_transaction_for_workload<T>(
    workload: CompileWorkload,
    close_sink: impl FnOnce(CompilerTransactionClose),
    action: impl FnOnce() -> T,
) -> CompilerTransactionOutcome<T> {
    with_compiler_transaction_inner(workload, None, close_sink, action)
}

pub fn with_compiler_transaction_cancellable_for_workload<T>(
    workload: CompileWorkload,
    cancellation: CompilerTransactionCancellation,
    close_sink: impl FnOnce(CompilerTransactionClose),
    action: impl FnOnce() -> T,
) -> CompilerTransactionOutcome<T> {
    with_compiler_transaction_inner(workload, Some(cancellation), close_sink, action)
}

#[tracing::instrument(
    target = "exomonad_harness::timing",
    name = "compiler_transaction.scope",
    level = "debug",
    skip_all,
    fields(inclusive = true)
)]
fn with_compiler_transaction_inner<T>(
    workload: CompileWorkload,
    cancellation: Option<CompilerTransactionCancellation>,
    close_sink: impl FnOnce(CompilerTransactionClose),
    action: impl FnOnce() -> T,
) -> CompilerTransactionOutcome<T> {
    TRANSACTION_SCOPE.with(|scope| {
        assert!(
            scope.borrow().is_none(),
            "compiler transaction scopes cannot nest"
        );
        *scope.borrow_mut() = Some(TransactionScope {
            workload,
            compiler: ScopedCompiler::Unbound,
            cancellation,
            admission_close: Vec::new(),
            input_files: CompilerInputFiles::default(),
        });
    });
    let guard = TransactionScopeGuard {
        close_sink: Some(close_sink),
    };
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

    pub fn finish(mut self) -> CompilerTransactionClose {
        self.close(false)
    }

    fn abandon(&mut self) -> CompilerTransactionClose {
        self.close(true)
    }

    #[tracing::instrument(
        target = "exomonad_harness::timing",
        name = "compiler_transaction.close",
        level = "debug",
        skip_all,
        fields(abandoned, inclusive = true)
    )]
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
                        input_files: CompilerInputFiles::default(),
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
                    let report = endpoint
                        .stdout
                        .as_mut()
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "direct compiler stdout is closed",
                            )
                        })
                        .and_then(|stdout| {
                            read_failure_end(stdout, Instant::now() + DIRECT_CLOSE_TIMEOUT, || {
                                self.cancellation
                                    .as_ref()
                                    .is_some_and(CompilerTransactionCancellation::is_cancelled)
                            })
                        });
                    drop(endpoint.stdout.take());
                    let mut retirement = wait_for_owned_child(&endpoint.child);
                    if let Ok(Some(report)) = &report {
                        retirement.worker_report = Some(report.clone());
                    }
                    endpoint.retired = true;
                    let clean_exit = matches!(&retirement.exit, Ok(status) if status.success())
                        && matches!(retirement.termination, CompilerTermination::NotRequested);
                    if end.is_ok() && matches!(report, Ok(None)) && clean_exit {
                        CompilerTransactionClose::Clean
                    } else {
                        let reason = match (end, report) {
                            (Err(source), _) => CompilerTransactionCloseReason::EndFailed(
                                CompilerTransactionCloseFailure::new(
                                    CompilerTransactionClosePhase::EndWrite,
                                    source,
                                ),
                            ),
                            (Ok(()), Err(source)) => CompilerTransactionCloseReason::EndFailed(
                                CompilerTransactionCloseFailure::new(CompilerTransactionClosePhase::EndRead, source),
                            ),
                            (Ok(()), Ok(Some(_))) if matches!(&retirement.exit, Ok(status) if status.success()) => {
                                CompilerTransactionCloseReason::EndFailed(CompilerTransactionCloseFailure::new(
                                    CompilerTransactionClosePhase::EndRead,
                                    invalid_close("failure END report conflicts with successful frontend exit"),
                                ))
                            }
                            (Ok(()), Ok(Some(_))) => CompilerTransactionCloseReason::FrontendReportedFailure,
                            (Ok(()), Ok(None)) if retirement.exit.is_err() => {
                                CompilerTransactionCloseReason::FrontendRetirementUnconfirmed
                            }
                            (Ok(()), Ok(None)) => CompilerTransactionCloseReason::FrontendExitUnsuccessful,
                        };
                        CompilerTransactionClose::Unconfirmed(CompilerTransactionCloseEvidence {
                            reason,
                            retirement: CompilerTransactionRetirement::Direct(retirement),
                            earlier: Vec::new(),
                            input_files: CompilerInputFiles::default(),
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
                        input_files: CompilerInputFiles::default(),
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
                                input_files: CompilerInputFiles::default(),
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

    fn property_config(cases: u32, test_name: &'static str) -> proptest::test_runner::Config {
        use proptest::test_runner::{contextualize_config, Config, FileFailurePersistence};
        let mut config = Config {
            cases,
            ..Config::default()
        };
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
        }
        let mut config = contextualize_config(config);
        config.source_file = Some("tidepool/extract-cmd/src/endpoint.rs");
        config.test_name = Some(test_name);
        config
    }

    #[test]
    fn manual_property_campaign_controls_preserve_cases_seed_shrinking_and_replay() {
        use proptest::prelude::*;
        use proptest::test_runner::{FileFailurePersistence, RngSeed, TestCaseError, TestRunner};
        const MODE: &str = "TIDEPOOL_PROPERTY_CONFIG_PROBE";
        const NAME: &str = concat!(
            module_path!(),
            "::manual_property_campaign_controls_preserve_cases_seed_shrinking_and_replay"
        );
        let Ok(mode) = std::env::var(MODE) else {
            for (mode, cases, shrink) in [
                ("defaults", None, "4"),
                ("campaign", Some("1"), "4"),
                ("no-shrink", Some("1"), "0"),
                ("disabled", Some("1"), "4"),
                ("replay", Some("1"), "0"),
            ] {
                let mut child = Command::new(std::env::current_exe().unwrap());
                child.args(["--exact", NAME.split_once("::").unwrap().1, "--nocapture"]);
                for (name, _) in std::env::vars_os() {
                    if name
                        .to_str()
                        .is_some_and(|name| name.starts_with("PROPTEST_"))
                    {
                        child.env_remove(name);
                    }
                }
                child
                    .env(MODE, mode)
                    .env("PROPTEST_RNG_SEED", "123")
                    .env("PROPTEST_MAX_SHRINK_ITERS", shrink);
                if let Some(cases) = cases {
                    child.env("PROPTEST_CASES", cases);
                }
                if mode == "disabled" {
                    child.env("PROPTEST_DISABLE_FAILURE_PERSISTENCE", "1");
                }
                let output = child.output().unwrap();
                assert!(
                    output.status.success(),
                    "{mode}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    stdout.contains(&format!("campaign mode={mode} fresh=")),
                    "child must execute the selected control probe: {stdout}"
                );
                print!("{stdout}");
            }
            return;
        };
        let config = property_config(128, NAME);
        let expected_cases = if mode == "defaults" { 128 } else { 1 };
        let expected_shrink_iters = if matches!(mode.as_str(), "no-shrink" | "replay") {
            0
        } else {
            4
        };
        assert_eq!(config.max_shrink_iters, expected_shrink_iters);
        assert_eq!(config.rng_seed, RngSeed::Fixed(123));
        assert_eq!(
            config.source_file,
            Some("tidepool/extract-cmd/src/endpoint.rs")
        );
        assert_eq!(config.test_name, Some(NAME));
        if mode == "disabled" {
            assert!(config.failure_persistence.is_none());
        } else {
            assert!(config.failure_persistence.is_some());
        }
        let mut fresh_config = config.clone();
        fresh_config.failure_persistence = None;
        let sample = || {
            let values = RefCell::new(Vec::new());
            TestRunner::new(fresh_config.clone())
                .run(&any::<u64>(), |value| {
                    values.borrow_mut().push(value);
                    Ok(())
                })
                .unwrap();
            values.into_inner()
        };
        let fresh = sample();
        println!(
            "campaign mode={mode} fresh={} configured_cases={} seed={}",
            fresh.len(),
            config.cases,
            config.rng_seed
        );
        assert_eq!(config.cases, expected_cases);
        assert_eq!(fresh.len(), expected_cases as usize);
        assert_eq!(
            fresh,
            sample(),
            "environment seed must reproduce the fresh sequence"
        );
        let shrinking = RefCell::new(Vec::new());
        let failure = TestRunner::new(fresh_config).run(&(1u64..1024), |value| {
            shrinking.borrow_mut().push(value);
            Err(TestCaseError::fail("deliberate control probe"))
        });
        assert!(failure.is_err());
        let shrinking = shrinking.into_inner();
        if expected_shrink_iters == 0 {
            assert_eq!(shrinking.len(), 1);
        } else {
            assert_eq!(config.max_shrink_iters, 4);
            assert!(shrinking.len() > 1 && shrinking.len() <= 5);
        }
        println!(
            "shrinking mode={mode} failure_callbacks={} shrink_limit={}",
            shrinking.len(),
            config.max_shrink_iters
        );
        if mode == "replay" {
            let directory = tempfile::tempdir().unwrap();
            let seed_path = directory.path().join("regressions.txt");
            let seed_path = Box::leak(seed_path.to_str().unwrap().to_owned().into_boxed_str());
            let mut persisted = config;
            persisted.failure_persistence =
                Some(Box::new(FileFailurePersistence::Direct(seed_path)));
            let generated = RefCell::new(Vec::new());
            assert!(TestRunner::new(persisted.clone())
                .run(&any::<u64>(), |value| {
                    generated.borrow_mut().push(value);
                    Err(TestCaseError::fail("persisted control probe"))
                })
                .is_err());
            assert_eq!(generated.borrow().len(), 1);
            assert_eq!(
                persisted
                    .failure_persistence
                    .as_ref()
                    .unwrap()
                    .load_persisted_failures2(persisted.source_file)
                    .len(),
                1
            );
            persisted.cases = 0;
            let replayed = RefCell::new(Vec::new());
            TestRunner::new(persisted.clone())
                .run(&any::<u64>(), |value| {
                    replayed.borrow_mut().push(value);
                    Ok(())
                })
                .unwrap();
            assert_eq!(*replayed.borrow(), *generated.borrow());
            persisted.cases = 1;
            let callbacks = RefCell::new(0);
            TestRunner::new(persisted)
                .run(&any::<u64>(), |_| {
                    *callbacks.borrow_mut() += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                *callbacks.borrow(),
                2,
                "persisted replay does not consume a fresh case"
            );
            println!("persistence generated_failures=1 replay_only_callbacks=1 fresh=1 replay=1 total_callbacks=2");
        }
    }

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
                    endpoint.transaction_with_cancellation(
                        CompileWorkload::Foreground,
                        Some(reader_cancellation),
                    )
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
                        CompileWorkload::Foreground,
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
            .transaction_with_cancellation(CompileWorkload::Foreground, Some(cancellation.clone()))
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

    fn observed_scope<T>(action: impl FnOnce() -> T) -> CompilerTransactionOutcome<T> {
        let retained = std::cell::RefCell::new(None);
        let outcome =
            with_compiler_transaction(|close| *retained.borrow_mut() = Some(close), action);
        assert_eq!(retained.into_inner(), Some(outcome.close.clone()));
        outcome
    }

    fn scoped_transport_fixture(
        phase: u8,
        primary_failure: bool,
    ) -> CompilerTransactionOutcome<Result<Vec<u8>, &'static str>> {
        let directory = tempfile::tempdir().unwrap();
        scoped_transport_fixture_in(directory.path(), phase, primary_failure)
    }

    fn scoped_transport_fixture_in(
        directory: &Path,
        phase: u8,
        primary_failure: bool,
    ) -> CompilerTransactionOutcome<Result<Vec<u8>, &'static str>> {
        let spec = direct_handshake_fixture(directory, phase);
        scoped_transport_fixture_after_response(spec, primary_failure, || {})
    }

    fn scoped_transport_fixture_after_response(
        spec: LaunchSpec,
        primary_failure: bool,
        action: impl FnOnce(),
    ) -> CompilerTransactionOutcome<Result<Vec<u8>, &'static str>> {
        observed_scope(|| {
            let endpoint = CompilerEndpoint::bind_launch(spec).unwrap();
            let identity = endpoint.identity.clone();
            let transaction = endpoint.transaction().unwrap();
            // Install the genuinely admitted transport in the same private scope
            // populated by lazy bind; this fixture never issues compiler authority.
            TRANSACTION_SCOPE.with(|scope| {
                scope.borrow_mut().as_mut().unwrap().compiler = ScopedCompiler::Active {
                    transaction,
                    program: "fixture".into(),
                }
            });
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
            action();
            if primary_failure {
                Err("primary action refusal")
            } else {
                Ok(response.output.stdout)
            }
        })
    }

    fn compiler_input_file(bytes: &[u8]) -> Arc<File> {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        Arc::new(file)
    }

    fn compiler_input_path(file: &File) -> String {
        format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd())
    }

    #[test]
    fn compiler_input_files_survive_action_return_until_real_end() {
        for primary_failure in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let spec = direct_handshake_fixture(directory.path(), 5);
            let mut weak = std::sync::Weak::new();
            let outcome = scoped_transport_fixture_after_response(spec, primary_failure, || {
                let file = compiler_input_file(b"retained through actual END");
                std::fs::write(
                    directory.path().join("input-at-end"),
                    compiler_input_path(&file),
                )
                .unwrap();
                weak = Arc::downgrade(&file);
                assert!(retain_compiler_input_file(Arc::clone(&file)));
                assert!(retain_compiler_input_file(Arc::clone(&file)));
                assert_eq!(
                    Arc::strong_count(&file),
                    2,
                    "one lease per original descriptor"
                );
                // Returning drops the final action-local owner before the real
                // transport fixture reads the input while processing END.
            });
            assert_eq!(outcome.close, CompilerTransactionClose::Clean);
            assert_eq!(outcome.action.is_err(), primary_failure);
            assert_eq!(
                std::fs::read(directory.path().join("observed-input")).unwrap(),
                b"retained through actual END"
            );
            assert!(
                weak.upgrade().is_none(),
                "clean END releases physical custody"
            );
        }
    }

    #[test]
    fn compiler_input_files_follow_last_uncertain_close_observer() {
        let directory = tempfile::tempdir().unwrap();
        let spec = direct_handshake_fixture(directory.path(), 6);
        let mut weak = std::sync::Weak::new();
        let outcome = scoped_transport_fixture_after_response(spec, false, || {
            let file = compiler_input_file(b"uncertain settlement");
            std::fs::write(
                directory.path().join("input-at-end"),
                compiler_input_path(&file),
            )
            .unwrap();
            weak = Arc::downgrade(&file);
            assert!(retain_compiler_input_file(file));
        });
        assert!(matches!(
            outcome.close,
            CompilerTransactionClose::Unconfirmed(_)
        ));
        assert_eq!(outcome.action, Ok(b"1".to_vec()));
        assert_eq!(
            std::fs::read(directory.path().join("observed-input")).unwrap(),
            b"uncertain settlement"
        );
        let observer = outcome.close.clone();
        drop(outcome);
        assert!(
            weak.upgrade().is_some(),
            "observed uncertainty still owns its inputs"
        );
        drop(observer);
        assert!(
            weak.upgrade().is_none(),
            "the final observation releases the file"
        );
    }

    #[test]
    fn compiler_input_files_have_no_unscoped_or_unstarted_owner() {
        let file = compiler_input_file(b"not submitted");
        let weak = Arc::downgrade(&file);
        assert!(!retain_compiler_input_file(Arc::clone(&file)));
        assert_eq!(Arc::strong_count(&file), 1);
        let outcome = observed_scope(|| {
            assert!(retain_compiler_input_file(file));
            assert!(weak.upgrade().is_some());
        });
        assert_eq!(outcome.close, CompilerTransactionClose::NotStarted);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn compiler_input_file_histories_preserve_physical_custody() {
        use proptest::prelude::*;
        use proptest::test_runner::TestRunner;
        let config = property_config(
            128,
            concat!(
                module_path!(),
                "::compiler_input_file_histories_preserve_physical_custody"
            ),
        );
        let directory = tempfile::tempdir().unwrap();
        let spec = direct_handshake_fixture(directory.path(), 6);
        let program = spec.program;
        TestRunner::new(config)
            .run(
                &(
                    proptest::collection::vec((0u8..3, 0usize..4), 0..64),
                    0usize..8,
                ),
                |(history, observations)| {
                    let mut owners: Vec<_> =
                        (0..4).map(|i| Some(compiler_input_file(&[i]))).collect();
                    let weak: Vec<_> = owners
                        .iter()
                        .map(|file| Arc::downgrade(file.as_ref().unwrap()))
                        .collect();
                    let mut retained = [false; 4];
                    let outcome = scoped_transport_fixture_after_response(
                        LaunchSpec::direct(program.clone()),
                        false,
                        || {
                            // Always reach repeated retention and dropping a local
                            // owner, then diversify surrounding observations/history.
                            for (operation, index) in
                                [(0, 0), (0, 0), (1, 0)].into_iter().chain(history)
                            {
                                match operation {
                                    0 => {
                                        if let Some(file) = &owners[index] {
                                            assert!(retain_compiler_input_file(Arc::clone(file)));
                                            retained[index] = true;
                                        }
                                    }
                                    1 => owners[index] = None,
                                    _ => {}
                                }
                                for index in 0..4 {
                                    assert_eq!(
                                        weak[index].strong_count(),
                                        usize::from(owners[index].is_some())
                                            + usize::from(retained[index])
                                    );
                                }
                            }
                        },
                    );
                    prop_assert!(matches!(
                        outcome.close,
                        CompilerTransactionClose::Unconfirmed(_)
                    ));
                    let mut observers: Vec<_> =
                        (0..observations).map(|_| outcome.close.clone()).collect();
                    drop(owners);
                    drop(outcome);
                    while !observers.is_empty() {
                        for index in 0..4 {
                            prop_assert_eq!(weak[index].upgrade().is_some(), retained[index]);
                        }
                        observers.pop();
                    }
                    prop_assert!(weak.iter().all(|file| file.upgrade().is_none()));
                    Ok(())
                },
            )
            .unwrap();
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
            .transaction_with_cancellation(CompileWorkload::Foreground, Some(cancellation.clone()))
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
        let outcome = observed_scope(|| {
            CompilerEndpoint::bind_launch(spec).and_then(|endpoint| {
                endpoint.transaction_with_cancellation_timeout(
                    CompileWorkload::Foreground,
                    None,
                    Duration::from_millis(100),
                )
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
        let outcome = observed_scope(|| {
            let refusal = CompilerEndpoint::bind_launch(refused_spec).unwrap_err();
            assert!(
                refusal.permits_rebind(),
                "identity refusal submitted no compiler request"
            );
            let endpoint = CompilerEndpoint::bind_launch(healthy_spec).unwrap();
            let identity = endpoint.identity.clone();
            let transaction = endpoint.transaction().unwrap();
            TRANSACTION_SCOPE.with(|scope| {
                scope.borrow_mut().as_mut().unwrap().compiler = ScopedCompiler::Active {
                    transaction,
                    program: "fixture".into(),
                }
            });
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

    fn scratch_close_report() -> CompilerFrontendCloseReport {
        CompilerFrontendCloseReport {
            worker: CompilerWorkerRetirement::Reaped(std::process::ExitStatus::from_raw(0)),
            scratch: CompilerScratchRetirement::Unconfirmed(vec![CompilerScratchFailure {
                path: PathBuf::from(OsString::from_vec(b"/owned/products/invalid-\xff".to_vec())),
                phase: crate::frontend::ScratchCleanupPhase::Products,
                cause: CompilerIoCause::from(&io::Error::from_raw_os_error(20)),
            }]),
        }
    }

    fn report_transport_fixture(
        bytes: &[u8],
        phase: u8,
        primary: bool,
    ) -> CompilerTransactionOutcome<Result<Vec<u8>, &'static str>> {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("failure-end"), bytes).unwrap();
        scoped_transport_fixture_in(directory.path(), phase, primary)
    }

    #[test]
    fn completed_action_retains_reported_worker_reap_and_exact_scratch_failure() {
        let report = scratch_close_report();
        let mut bytes = Vec::new();
        write_failure_end(&mut bytes, &report).unwrap();
        for primary in [false, true] {
            let outcome = report_transport_fixture(&bytes, 7, primary);
            if primary {
                assert_eq!(outcome.action, Err("primary action refusal"));
            } else {
                assert_eq!(outcome.action, Ok(b"1".to_vec()));
            }
            let CompilerTransactionClose::Unconfirmed(evidence) = outcome.close else {
                panic!("negative END cannot confirm close");
            };
            assert_eq!(
                evidence.reason,
                CompilerTransactionCloseReason::FrontendReportedFailure
            );
            let CompilerTransactionRetirement::Direct(retirement) = evidence.retirement else {
                panic!("direct facts required");
            };
            assert_eq!(retirement.exit.unwrap().code(), Some(17));
            assert_eq!(retirement.termination, CompilerTermination::NotRequested);
            assert_eq!(retirement.worker_report, Some(report.clone()));
        }
    }

    #[test]
    fn malformed_truncated_oversized_and_trailing_end_reports_never_confirm_close() {
        let mut valid = Vec::new();
        write_failure_end(&mut valid, &scratch_close_report()).unwrap();
        let mut oversized = FAILURE_END_MAGIC.to_vec();
        oversized.extend_from_slice(&((MAX_FAILURE_END_BYTES + 1) as u32).to_le_bytes());
        let mut trailing = valid.clone();
        trailing.push(0);
        let cases = [
            b"INVALID!".to_vec(),
            valid[..7].to_vec(),
            oversized,
            trailing,
        ];
        for bytes in cases {
            let outcome = report_transport_fixture(&bytes, 7, false);
            assert_eq!(outcome.action, Ok(b"1".to_vec()));
            let CompilerTransactionClose::Unconfirmed(evidence) = outcome.close else {
                panic!("bad END cannot confirm close");
            };
            let CompilerTransactionCloseReason::EndFailed(error) = evidence.reason else {
                panic!("typed protocol failure required");
            };
            assert_eq!(error.phase, CompilerTransactionClosePhase::EndRead);
            let CompilerTransactionRetirement::Direct(retirement) = evidence.retirement else {
                panic!("frontend retirement required");
            };
            assert!(retirement.exit.is_ok());
            assert!(
                retirement.worker_report.is_none(),
                "malformed frame proves no worker facts"
            );
        }
    }

    #[test]
    fn failure_end_report_conflicting_with_exit_zero_is_unconfirmed() {
        let report = scratch_close_report();
        let mut bytes = Vec::new();
        write_failure_end(&mut bytes, &report).unwrap();
        let outcome = report_transport_fixture(&bytes, 11, false);
        let CompilerTransactionClose::Unconfirmed(evidence) = outcome.close else {
            panic!("contradiction cannot confirm close");
        };
        let CompilerTransactionCloseReason::EndFailed(error) = evidence.reason else {
            panic!("typed contradiction required");
        };
        assert_eq!(error.phase, CompilerTransactionClosePhase::EndRead);
        assert_eq!(error.source.kind(), io::ErrorKind::InvalidData);
        let CompilerTransactionRetirement::Direct(retirement) = evidence.retirement else {
            panic!("direct facts required");
        };
        assert!(retirement.exit.unwrap().success());
        assert_eq!(retirement.worker_report, Some(report));
    }

    #[test]
    fn end_report_requires_eof_under_the_same_absolute_deadline() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        write_failure_end(&mut writer, &scratch_close_report()).unwrap();
        // Keep the writer alive: valid frame bytes alone cannot prove close.
        let error = read_failure_end(
            &mut reader,
            Instant::now() + Duration::from_millis(50),
            || false,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn empty_close_eof_after_deadline_cannot_confirm_success() {
        struct DelayedEof(UnixStream);
        impl std::os::fd::AsRawFd for DelayedEof {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                self.0.as_raw_fd()
            }
        }
        impl Read for DelayedEof {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                #[allow(
                    clippy::disallowed_methods,
                    reason = "deterministic delayed EOF observation"
                )]
                std::thread::sleep(Duration::from_millis(75));
                self.0.read(bytes)
            }
        }
        let (reader, writer) = UnixStream::pair().unwrap();
        drop(writer);
        let error = read_failure_end(
            &mut DelayedEof(reader),
            Instant::now() + Duration::from_millis(50),
            || false,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn close_codec_refuses_contradictory_worker_scratch_facts_and_count_growth() {
        let clean = CompilerFrontendCloseReport {
            worker: CompilerWorkerRetirement::Reaped(std::process::ExitStatus::from_raw(0)),
            scratch: CompilerScratchRetirement::Confirmed,
        };
        assert!(
            encode_failure_end(&clean).is_err(),
            "successful close has no extra ACK"
        );
        let mut oversized = scratch_close_report();
        let CompilerScratchRetirement::Unconfirmed(failures) = &mut oversized.scratch else {
            unreachable!()
        };
        failures.resize(MAX_SCRATCH_FAILURES + 1, failures[0].clone());
        assert!(encode_failure_end(&oversized).is_err());
        let mut malformed = encode_failure_end(&scratch_close_report()).unwrap();
        malformed[5] = 0; // Reaped worker with unobserved scratch is not a closed fact.
        assert!(decode_failure_end(&malformed).is_err());
    }

    #[test]
    fn unwind_close_sink_retains_exact_child_after_retirement_observation_failure() {
        let directory = tempfile::tempdir().unwrap();
        let spec = direct_handshake_fixture(directory.path(), 5);
        let retained = RefCell::new(None);
        let mut input = std::sync::Weak::new();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_compiler_transaction(
                |close| *retained.borrow_mut() = Some(close),
                || {
                    let file = compiler_input_file(b"input surviving unwind");
                    input = Arc::downgrade(&file);
                    assert!(retain_compiler_input_file(file));
                    let endpoint = CompilerEndpoint::bind_launch(spec).unwrap();
                    let mut transaction = endpoint.transaction().unwrap();
                    let Some(TransactionTransport::Direct(endpoint)) =
                        transaction.transport.as_mut()
                    else {
                        panic!("direct fixture required");
                    };
                    endpoint.wait_observation_fault = true;
                    TRANSACTION_SCOPE.with(|scope| {
                        scope.borrow_mut().as_mut().unwrap().compiler = ScopedCompiler::Active {
                            transaction,
                            program: "fixture".into(),
                        }
                    });
                    panic!("original action unwind");
                },
            )
        }));
        assert_eq!(
            unwind.unwrap_err().downcast_ref::<&str>(),
            Some(&"original action unwind")
        );
        let Some(CompilerTransactionClose::Unconfirmed(evidence)) = retained.into_inner() else {
            panic!("owning sink must survive unwind");
        };
        assert!(
            input.upgrade().is_some(),
            "unwind sink retains input custody"
        );
        assert_eq!(evidence.reason, CompilerTransactionCloseReason::Abandoned);
        let CompilerTransactionRetirement::Direct(retirement) = evidence.retirement else {
            panic!("exact process custody required");
        };
        assert!(retirement.exit.is_err());
        let held = retirement
            ._retained_child
            .as_ref()
            .expect("failed observation must retain exact child");
        let actual = retire_owned_child(&held.0, false);
        assert!(
            actual.exit.is_ok(),
            "same owner can subsequently observe actual reap"
        );
        assert!(actual._retained_child.is_none());
        drop(evidence.input_files);
        assert!(input.upgrade().is_none());
    }

    #[test]
    fn stale_direct_frontend_success_cannot_confirm_strengthened_close_contract() {
        let directory = tempfile::tempdir().unwrap();
        let spec = direct_handshake_fixture(directory.path(), 13);
        let outcome = observed_scope(|| {
            CompilerEndpoint::bind_launch(spec).and_then(CompilerEndpoint::transaction)
        });
        let primary = outcome.action.unwrap_err();
        assert!(
            !primary.permits_rebind(),
            "stale accepted grammar cannot authorize replay"
        );
        assert_eq!(primary.source.kind(), io::ErrorKind::UnexpectedEof);
        let CompilerTransactionClose::Unconfirmed(evidence) = outcome.close else {
            panic!("legacy frontend cannot confirm direct v2 close");
        };
        assert_eq!(
            evidence.reason,
            CompilerTransactionCloseReason::AdmissionAborted(
                CompilerTransactionClosePhase::BeginHandshake
            )
        );
        let CompilerTransactionRetirement::Direct(retirement) = evidence.retirement else {
            panic!("actual stale frontend retirement required");
        };
        assert!(
            retirement.exit.unwrap().success(),
            "stale exit0 alone is not close evidence"
        );
        assert!(retirement.worker_report.is_none());
    }

    #[derive(Clone, Copy)]
    enum DaemonFixtureBehavior {
        Normal,
        CancelBegin,
        CancelRequest,
    }

    #[derive(Default, Debug)]
    struct DaemonFixtureCounts {
        preflights: usize,
        begins: usize,
        admissions: usize,
        requests: usize,
        ends: usize,
        active: usize,
    }

    /// The peer speaks the actual preflight/transaction protocol. Its census is
    /// independent of the client's private scope state and uses no ambient env.
    struct DeferredDaemonFixture {
        _directory: tempfile::TempDir,
        socket: PathBuf,
        epoch: Arc<Mutex<[u8; 32]>>,
        counts: Arc<Mutex<DaemonFixtureCounts>>,
        server: Option<std::thread::JoinHandle<()>>,
    }

    impl DeferredDaemonFixture {
        fn new(
            behavior: DaemonFixtureBehavior,
            cancellation: CompilerTransactionCancellation,
        ) -> Self {
            use std::os::unix::net::UnixListener;
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("compiler.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let epoch = Arc::new(Mutex::new([9; 32]));
            let counts = Arc::new(Mutex::new(DaemonFixtureCounts::default()));
            let server_epoch = Arc::clone(&epoch);
            let server_counts = Arc::clone(&counts);
            let server = std::thread::spawn(move || {
                let mut handlers = Vec::new();
                for connection in listener.incoming() {
                    let mut connection = connection.unwrap();
                    connection
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    connection
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut kind = [0; 8];
                    connection.read_exact(&mut kind).unwrap();
                    if &kind == b"TESTSTOP" {
                        break;
                    }
                    if &kind == b"TPDPF001" {
                        server_counts.lock().unwrap().preflights += 1;
                        connection.write_all(b"TPDPI003").unwrap();
                        connection.write_all(&[7; 32]).unwrap();
                        connection.write_all(&[8; 32]).unwrap();
                        connection
                            .write_all(&*server_epoch.lock().unwrap())
                            .unwrap();
                        continue;
                    }
                    assert_eq!(&kind, daemon::TRANSACTION);
                    let mut requested_epoch = [0; 32];
                    connection.read_exact(&mut requested_epoch).unwrap();
                    let mut workload = [0];
                    connection.read_exact(&mut workload).unwrap();
                    assert_eq!(workload, [0]);
                    server_counts.lock().unwrap().begins += 1;
                    if requested_epoch != *server_epoch.lock().unwrap() {
                        let message = b"replaced daemon epoch";
                        connection.write_all(&[0]).unwrap();
                        connection
                            .write_all(&(message.len() as u32).to_le_bytes())
                            .unwrap();
                        connection.write_all(message).unwrap();
                        continue;
                    }
                    let admission = {
                        let mut counts = server_counts.lock().unwrap();
                        counts.admissions += 1;
                        counts.active += 1;
                        counts.admissions as u64
                    };
                    let counts = Arc::clone(&server_counts);
                    let cancellation = cancellation.clone();
                    handlers.push(std::thread::spawn(move || {
                        struct Permit(Arc<Mutex<DaemonFixtureCounts>>);
                        impl Drop for Permit {
                            fn drop(&mut self) {
                                self.0.lock().unwrap().active -= 1;
                            }
                        }
                        let _permit = Permit(counts.clone());
                        if matches!(behavior, DaemonFixtureBehavior::CancelBegin) {
                            cancellation.cancel();
                            return;
                        }
                        connection.write_all(&[1]).unwrap();
                        connection.write_all(&admission.to_le_bytes()).unwrap();
                        loop {
                            let mut command = [0];
                            if connection.read_exact(&mut command).is_err() {
                                break;
                            }
                            match command[0] {
                                daemon::TRANSACTION_END => {
                                    counts.lock().unwrap().ends += 1;
                                    connection.write_all(&[1]).unwrap();
                                    break;
                                }
                                daemon::TRANSACTION_REQUEST => {
                                    let _ = daemon::read_request(&mut connection).unwrap();
                                    let request = {
                                        let mut counts = counts.lock().unwrap();
                                        counts.requests += 1;
                                        counts.requests
                                    };
                                    if matches!(behavior, DaemonFixtureBehavior::CancelRequest) {
                                        cancellation.cancel();
                                        break;
                                    }
                                    daemon::write_response(
                                        &mut connection,
                                        0,
                                        &[request as u8],
                                        b"",
                                    )
                                    .unwrap();
                                }
                                _ => panic!("invalid transaction command"),
                            }
                        }
                    }));
                }
                for handler in handlers {
                    handler.join().unwrap();
                }
            });
            Self {
                _directory: directory,
                socket,
                epoch,
                counts,
                server: Some(server),
            }
        }

        fn command(&self) -> ExtractCmd {
            ExtractCmd {
                program: "fixture".into(),
                bin_source: BinSource::Explicit,
                request: ExtractRequest::default(),
            }
        }

        fn bind(&self, command: &ExtractCmd) -> Result<CompilerEndpoint, SpawnError> {
            let identity = bind_scoped_endpoint(command, |_| {
                let binding = daemon::preflight(&self.socket).unwrap();
                Ok(CompilerEndpoint {
                    identity: CompilerIdentity::daemon(
                        binding.producer,
                        binding.consumed_worker,
                        binding.epoch,
                    ),
                    transport: Transport::Daemon {
                        socket: self.socket.clone(),
                        epoch: binding.epoch,
                    },
                })
            })?;
            Ok(CompilerEndpoint {
                identity,
                transport: Transport::Scoped,
            })
        }

        fn settle(&mut self) {
            let Some(server) = self.server.take() else {
                return;
            };
            UnixStream::connect(&self.socket)
                .unwrap()
                .write_all(b"TESTSTOP")
                .unwrap();
            server.join().unwrap();
        }
    }

    impl Drop for DeferredDaemonFixture {
        fn drop(&mut self) {
            self.settle();
        }
    }

    #[test]
    fn scoped_daemon_host_preparation_has_no_admission_and_reuses_exact_binding() {
        use proptest::prelude::*;
        use proptest::test_runner::TestRunner;
        let config = property_config(
            128,
            concat!(
                module_path!(),
                "::scoped_daemon_host_preparation_has_no_admission_and_reuses_exact_binding"
            ),
        );
        let mut runner = TestRunner::new(config);
        // Host reads/work can surround any physical request. Exact protocol
        // admissions independently distinguish availability from capacity use.
        runner
            .run(
                &proptest::collection::vec((0_u8..3, 0_usize..32), 0..32),
                |history| {
                    let cancellation = CompilerTransactionCancellation::new();
                    let mut fixture = DeferredDaemonFixture::new(
                        DaemonFixtureBehavior::Normal,
                        cancellation.clone(),
                    );
                    let command = fixture.command();
                    let mut requests = 0;
                    let outcome = with_compiler_transaction_cancellable(
                        cancellation,
                        |_| {},
                        || {
                            let expected = fixture.bind(&command).unwrap().identity.clone();
                            let history = [(0, 0), (1, 17)].into_iter().chain(history).chain([
                                (2, 0),
                                (1, 23),
                                (2, 0),
                            ]);
                            for (operation, work) in history {
                                let bound = fixture.bind(&command).unwrap();
                                assert_eq!(bound.identity, expected);
                                if operation == 2 {
                                    requests += 1;
                                    assert_eq!(
                                        bound.execute(&command).unwrap().output.stdout,
                                        [requests as u8]
                                    );
                                } else if operation == 1 {
                                    // Reversible host preparation does not execute GHC.
                                    let digest = blake3::hash(&vec![7; work]);
                                    assert_ne!(digest.as_bytes(), &[0; 32]);
                                }
                                let counts = fixture.counts.lock().unwrap();
                                assert_eq!(counts.preflights, 1);
                                assert_eq!(counts.admissions, usize::from(requests > 0));
                                assert_eq!(counts.active, usize::from(requests > 0));
                                assert_eq!(counts.requests, requests);
                                assert_eq!(counts.begins, usize::from(requests > 0),
                            "first physical use alone admits; dependent requests remain pinned");
                            }
                        },
                    );
                    prop_assert_eq!(outcome.close, CompilerTransactionClose::Clean);
                    fixture.settle();
                    prop_assert_eq!(fixture.counts.lock().unwrap().active, 0);
                    Ok(())
                },
            )
            .unwrap();
    }

    #[test]
    fn scoped_host_checkpoint_refuses_prebind_cancellation_and_fresh_scope_recovers() {
        let cancellation = CompilerTransactionCancellation::new();
        let mut fixture =
            DeferredDaemonFixture::new(DaemonFixtureBehavior::Normal, cancellation.clone());
        let command = fixture.command();
        compiler_host_checkpoint().unwrap();
        let refused = with_compiler_transaction_cancellable(
            cancellation.clone(),
            |_| {},
            || {
                compiler_host_checkpoint().unwrap();
                cancellation.cancel();
                assert_eq!(
                    compiler_host_checkpoint().unwrap_err().kind(),
                    io::ErrorKind::Interrupted
                );
                TRANSACTION_SCOPE.with(|scope| {
                    assert!(matches!(
                        scope.borrow().as_ref().unwrap().compiler,
                        ScopedCompiler::Unbound
                    ));
                });
                let counts = fixture.counts.lock().unwrap();
                assert_eq!(
                    (
                        counts.preflights,
                        counts.begins,
                        counts.admissions,
                        counts.requests
                    ),
                    (0, 0, 0, 0)
                );
            },
        );
        assert_eq!(refused.close, CompilerTransactionClose::NotStarted);
        compiler_host_checkpoint().unwrap();
        let recovered = with_compiler_transaction_cancellable(
            CompilerTransactionCancellation::new(),
            |_| {},
            || {
                compiler_host_checkpoint().unwrap();
                let body = fixture
                    .bind(&command)
                    .unwrap()
                    .execute(&command)
                    .unwrap()
                    .output
                    .stdout;
                compiler_host_checkpoint().unwrap();
                body
            },
        );
        assert_eq!(recovered.action, [1]);
        assert_eq!(recovered.close, CompilerTransactionClose::Clean);
        fixture.settle();
        let counts = fixture.counts.lock().unwrap();
        assert_eq!(
            counts.ends, 1,
            "clean close follows the peer's END acknowledgement"
        );
        assert_eq!(
            (
                counts.preflights,
                counts.begins,
                counts.admissions,
                counts.requests,
                counts.active
            ),
            (1, 1, 1, 1, 0)
        );
    }

    #[test]
    fn scoped_daemon_unexecuted_and_pre_admission_cancelled_bindings_have_no_permit() {
        for cancel in [false, true] {
            let cancellation = CompilerTransactionCancellation::new();
            let mut fixture =
                DeferredDaemonFixture::new(DaemonFixtureBehavior::Normal, cancellation.clone());
            let command = fixture.command();
            let outcome = with_compiler_transaction_cancellable(
                cancellation.clone(),
                |_| {},
                || {
                    let endpoint = fixture.bind(&command).unwrap();
                    if cancel {
                        cancellation.cancel();
                        let error = endpoint.execute(&command).unwrap_err();
                        assert!(error.definitely_unsubmitted());
                        assert_eq!(error.source.kind(), io::ErrorKind::Interrupted);
                    }
                },
            );
            assert_eq!(outcome.close, CompilerTransactionClose::NotStarted);
            fixture.settle();
            let counts = fixture.counts.lock().unwrap();
            assert_eq!(
                (
                    counts.preflights,
                    counts.begins,
                    counts.admissions,
                    counts.requests,
                    counts.active
                ),
                (1, 0, 0, 0, 0)
            );
        }
    }

    #[test]
    fn scoped_daemon_epoch_replacement_refuses_original_offer_without_rebinding() {
        for replace_after_admission in [false, true] {
            let cancellation = CompilerTransactionCancellation::new();
            let mut fixture =
                DeferredDaemonFixture::new(DaemonFixtureBehavior::Normal, cancellation.clone());
            let command = fixture.command();
            let outcome = with_compiler_transaction_cancellable(
                cancellation,
                |_| {},
                || {
                    let endpoint = fixture.bind(&command).unwrap();
                    let original = endpoint.identity.clone();
                    if replace_after_admission {
                        endpoint.execute(&command).unwrap();
                    }
                    *fixture.epoch.lock().unwrap() = [10; 32];
                    let still_original = fixture.bind(&command).unwrap();
                    assert_eq!(still_original.identity, original);
                    let result = still_original.execute(&command);
                    if replace_after_admission {
                        assert_eq!(result.unwrap().output.stdout, [2]);
                    } else {
                        let error = result.unwrap_err();
                        assert!(error.permits_rebind());
                        assert!(error.definitely_unsubmitted());
                    }
                },
            );
            assert_eq!(
                outcome.close,
                if replace_after_admission {
                    CompilerTransactionClose::Clean
                } else {
                    CompilerTransactionClose::NotStarted
                }
            );
            fixture.settle();
            let counts = fixture.counts.lock().unwrap();
            assert_eq!(counts.preflights, 1);
            assert_eq!(counts.begins, 1);
            assert_eq!(counts.admissions, usize::from(replace_after_admission));
            assert_eq!(counts.requests, if replace_after_admission { 2 } else { 0 });
            assert_eq!(counts.active, 0);
        }
    }

    #[test]
    fn host_after_response_and_projection_histories_preserve_owned_transaction_until_close() {
        use proptest::prelude::*;
        use proptest::test_runner::TestRunner;
        #[derive(Clone, Debug)]
        enum Step {
            HostRead,
            Request,
        }
        let config = property_config(256, concat!(module_path!(), "::host_after_response_and_projection_histories_preserve_owned_transaction_until_close"));
        let strategy = (
            proptest::collection::vec(
                prop_oneof![Just(Step::HostRead), Just(Step::Request)],
                0..24,
            ),
            any::<bool>(),
        );
        TestRunner::new(config)
            .run(&strategy, |(history, cancel)| {
                let cancellation = CompilerTransactionCancellation::new();
                let mut fixture =
                    DeferredDaemonFixture::new(DaemonFixtureBehavior::Normal, cancellation.clone());
                let command = fixture.command();
                let mut projection = fixture.command();
                projection.declaration_join(Path::new("projection-input.cbor"));
                projection.declaration_join_out(Path::new("projection-output.cbor"));
                assert_eq!(
                    projection.request.mode(),
                    crate::request::RequestMode::DeclarationInterface
                );
                let mut bodies = Vec::new();
                let retained_close = RefCell::new(None);
                let outcome = with_compiler_transaction_cancellable(
                    cancellation.clone(),
                    |close| *retained_close.borrow_mut() = Some(close),
                    || {
                        let endpoint = fixture.bind(&command).unwrap();
                        let identity = endpoint.identity().clone();
                        bodies.push(endpoint.execute(&command).unwrap().output.stdout);
                        // Host reads precede a required later declaration-interface
                        // request. The first response is not the sequence's end.
                        compiler_host_checkpoint().unwrap();
                        assert_eq!(fixture.counts.lock().unwrap().active, 1);
                        for step in history {
                            match step {
                                Step::HostRead => {
                                    compiler_host_checkpoint().unwrap();
                                    assert_eq!(fixture.counts.lock().unwrap().active, 1);
                                }
                                Step::Request => {
                                    let bound = fixture.bind(&command).unwrap();
                                    assert_eq!(bound.identity(), &identity);
                                    bodies.push(bound.execute(&command).unwrap().output.stdout);
                                }
                            }
                        }
                        let bound = fixture.bind(&projection).unwrap();
                        assert_eq!(bound.identity(), &identity);
                        bodies.push(bound.execute(&projection).unwrap().output.stdout);
                        compiler_host_checkpoint().unwrap();
                        assert_eq!(fixture.counts.lock().unwrap().active, 1);
                        if cancel {
                            cancellation.cancel();
                            assert_eq!(
                                compiler_host_checkpoint().unwrap_err().kind(),
                                io::ErrorKind::Interrupted
                            );
                        }
                    },
                );
                prop_assert_eq!(retained_close.into_inner(), Some(outcome.close.clone()));
                if cancel {
                    let CompilerTransactionClose::Unconfirmed(evidence) = outcome.close else {
                        return Err(TestCaseError::fail(
                            "cancelled host work cannot acknowledge END",
                        ));
                    };
                    prop_assert_eq!(evidence.reason, CompilerTransactionCloseReason::Cancelled);
                } else {
                    prop_assert_eq!(outcome.close, CompilerTransactionClose::Clean);
                }
                fixture.settle();
                let counts = fixture.counts.lock().unwrap();
                prop_assert_eq!(counts.preflights, 1);
                prop_assert_eq!(counts.begins, 1);
                prop_assert_eq!(counts.admissions, 1);
                prop_assert_eq!(counts.requests, bodies.len());
                prop_assert_eq!(counts.ends, usize::from(!cancel));
                prop_assert_eq!(counts.active, 0);
                // The independent response sequence detects replay, dropped later
                // requests, and loss of completed bodies after host cancellation.
                for (index, body) in bodies.iter().enumerate() {
                    prop_assert_eq!(body, &vec![(index + 1) as u8]);
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn scoped_daemon_cancelled_after_response_preserves_body_and_prevents_further_work() {
        let cancellation = CompilerTransactionCancellation::new();
        let mut fixture =
            DeferredDaemonFixture::new(DaemonFixtureBehavior::Normal, cancellation.clone());
        let command = fixture.command();
        let outcome = with_compiler_transaction_cancellable(
            cancellation.clone(),
            |_| {},
            || {
                let body = fixture
                    .bind(&command)
                    .unwrap()
                    .execute(&command)
                    .unwrap()
                    .output
                    .stdout;
                cancellation.cancel();
                assert_eq!(
                    compiler_host_checkpoint().unwrap_err().kind(),
                    io::ErrorKind::Interrupted
                );
                let error = fixture
                    .bind(&command)
                    .unwrap()
                    .execute(&command)
                    .unwrap_err();
                assert!(!error.definitely_unsubmitted());
                body
            },
        );
        assert_eq!(outcome.action, [1]);
        let CompilerTransactionClose::Unconfirmed(evidence) = outcome.close else {
            panic!("disconnect cannot acknowledge daemon cleanup");
        };
        assert_eq!(
            evidence.reason,
            CompilerTransactionCloseReason::FailedRequest
        );
        assert_eq!(
            evidence.retirement,
            CompilerTransactionRetirement::DaemonUnobserved {
                disconnect: Some(Ok(())),
            }
        );
        assert!(evidence.earlier.is_empty());
        fixture.settle();
        let counts = fixture.counts.lock().unwrap();
        assert_eq!(counts.ends, 0, "no END acknowledgement was requested");
        assert_eq!(
            (
                counts.preflights,
                counts.begins,
                counts.admissions,
                counts.requests,
                counts.active
            ),
            (1, 1, 1, 1, 0)
        );
    }

    #[test]
    fn scoped_daemon_cancel_and_unwind_after_response_preserve_body_without_cleanup_proof() {
        for unwind in [false, true] {
            let cancellation = CompilerTransactionCancellation::new();
            let mut fixture =
                DeferredDaemonFixture::new(DaemonFixtureBehavior::Normal, cancellation.clone());
            let command = fixture.command();
            let retained = RefCell::new(None);
            let body = RefCell::new(None);
            let action = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                with_compiler_transaction_cancellable(
                    cancellation.clone(),
                    |close| *retained.borrow_mut() = Some(close),
                    || {
                        *body.borrow_mut() = Some(
                            fixture
                                .bind(&command)
                                .unwrap()
                                .execute(&command)
                                .unwrap()
                                .output
                                .stdout,
                        );
                        if unwind {
                            panic!("original action unwind");
                        }
                        cancellation.cancel();
                    },
                )
            }));
            if unwind {
                assert_eq!(
                    action.unwrap_err().downcast_ref::<&str>(),
                    Some(&"original action unwind")
                );
            } else {
                assert_eq!(action.unwrap().close, retained.borrow().clone().unwrap());
            }
            assert_eq!(body.into_inner(), Some(vec![1]));
            let Some(CompilerTransactionClose::Unconfirmed(evidence)) = retained.into_inner()
            else {
                panic!("completed response and disconnected peer cannot prove daemon cleanup");
            };
            assert_eq!(
                evidence.reason,
                if unwind {
                    CompilerTransactionCloseReason::Abandoned
                } else {
                    CompilerTransactionCloseReason::Cancelled
                }
            );
            assert_eq!(
                evidence.retirement,
                CompilerTransactionRetirement::DaemonUnobserved {
                    disconnect: Some(Ok(())),
                }
            );
            assert!(evidence.earlier.is_empty());
            fixture.settle();
            let counts = fixture.counts.lock().unwrap();
            assert_eq!(counts.requests, 1, "completed body is never replayed");
            assert_eq!(counts.ends, 0, "no END acknowledgement was requested");
            assert_eq!(counts.active, 0, "peer releases its own independent permit");
        }
    }

    #[test]
    fn scoped_daemon_borrowed_identity_and_program_cannot_replace_bound_owner() {
        let cancellation = CompilerTransactionCancellation::new();
        let mut fixture =
            DeferredDaemonFixture::new(DaemonFixtureBehavior::Normal, cancellation.clone());
        let command = fixture.command();
        let outcome = with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || {
                let mut endpoint = fixture.bind(&command).unwrap();
                endpoint.identity = CompilerIdentity::daemon([7; 32], [8; 32], [10; 32]);
                assert!(endpoint
                    .execute(&command)
                    .unwrap_err()
                    .definitely_unsubmitted());
                let mut another_program = fixture.command();
                another_program.program = "another compiler".into();
                assert!(fixture
                    .bind(&another_program)
                    .unwrap_err()
                    .definitely_unsubmitted());
                let endpoint = fixture.bind(&command).unwrap();
                assert!(endpoint
                    .execute(&another_program)
                    .unwrap_err()
                    .definitely_unsubmitted());
            },
        );
        assert_eq!(outcome.close, CompilerTransactionClose::NotStarted);
        fixture.settle();
        let counts = fixture.counts.lock().unwrap();
        assert_eq!(
            (
                counts.preflights,
                counts.begins,
                counts.admissions,
                counts.requests
            ),
            (1, 0, 0, 0)
        );
    }

    #[test]
    fn scoped_daemon_cancellation_during_admission_or_request_retains_uncertainty() {
        for behavior in [
            DaemonFixtureBehavior::CancelBegin,
            DaemonFixtureBehavior::CancelRequest,
        ] {
            let cancellation = CompilerTransactionCancellation::new();
            let mut fixture = DeferredDaemonFixture::new(behavior, cancellation.clone());
            let command = fixture.command();
            let outcome = with_compiler_transaction_cancellable(
                cancellation,
                |_| {},
                || {
                    let endpoint = fixture.bind(&command).unwrap();
                    assert!(!endpoint
                        .execute(&command)
                        .unwrap_err()
                        .definitely_unsubmitted());
                    // An uncertain admission/request cannot submit another body.
                    if let Ok(endpoint) = fixture.bind(&command) {
                        assert!(!endpoint.execute(&command).unwrap_err().permits_rebind());
                    }
                },
            );
            let CompilerTransactionClose::Unconfirmed(evidence) = outcome.close else {
                panic!("cancelled admission/request cannot acknowledge daemon cleanup");
            };
            match behavior {
                DaemonFixtureBehavior::CancelBegin => {
                    assert!(matches!(
                        evidence.reason,
                        CompilerTransactionCloseReason::AdmissionFailed(
                            CompilerTransactionCloseFailure {
                                phase: CompilerTransactionClosePhase::BeginHandshake,
                                ..
                            }
                        )
                    ));
                    assert_eq!(
                        evidence.retirement,
                        CompilerTransactionRetirement::DaemonUnobserved { disconnect: None }
                    );
                }
                DaemonFixtureBehavior::CancelRequest => {
                    assert_eq!(
                        evidence.reason,
                        CompilerTransactionCloseReason::FailedRequest
                    );
                    assert_eq!(
                        evidence.retirement,
                        CompilerTransactionRetirement::DaemonUnobserved {
                            disconnect: Some(Ok(())),
                        }
                    );
                }
                DaemonFixtureBehavior::Normal => unreachable!(),
            }
            assert!(evidence.earlier.is_empty());
            fixture.settle();
            let counts = fixture.counts.lock().unwrap();
            assert_eq!(counts.ends, 0, "no END acknowledgement was requested");
            assert_eq!(
                (
                    counts.preflights,
                    counts.begins,
                    counts.admissions,
                    counts.active
                ),
                (1, 1, 1, 0)
            );
            assert_eq!(
                counts.requests,
                usize::from(matches!(behavior, DaemonFixtureBehavior::CancelRequest))
            );
        }
    }

    #[test]
    fn scoped_endpoints_borrow_preparation_owner_without_finishing_close() {
        assert!(
            std::env::var_os(crate::DAEMON_SOCKET_ENV).is_none()
                && std::env::var_os(crate::REQUIRED_DAEMON_ENDPOINT_ENV).is_none(),
            "transport fixture requires the runner's explicit direct compiler mode"
        );
        let directory = tempfile::tempdir().unwrap();
        let launch = direct_handshake_fixture(directory.path(), 5);
        let command = ExtractCmd {
            program: launch.program,
            bin_source: BinSource::Explicit,
            request: ExtractRequest::default(),
        };
        let retained = RefCell::new(None);
        let outcome = with_compiler_transaction_for_workload(
            CompileWorkload::Preparation,
            |close| *retained.borrow_mut() = Some(close),
            || {
                for expected in [b"1", b"2"] {
                    let borrowed = command.bind().unwrap();
                    assert!(matches!(borrowed.transport, Transport::Scoped));
                    let body = borrowed.execute(&command).unwrap();
                    assert_eq!(body.output.stdout.as_slice(), expected);
                    assert!(retained.borrow().is_none());
                    TRANSACTION_SCOPE.with(|scope| {
                        let scope = scope.borrow();
                        let scope = scope.as_ref().unwrap();
                        assert_eq!(scope.workload, CompileWorkload::Preparation);
                        assert_eq!(
                            match &scope.compiler {
                                ScopedCompiler::Active { transaction, .. } => transaction.workload,
                                _ => panic!("execution admits the owning transaction"),
                            },
                            CompileWorkload::Preparation
                        );
                    });
                }
            },
        );
        assert_eq!(outcome.close, CompilerTransactionClose::Clean);
        assert_eq!(retained.into_inner(), Some(outcome.close));
        assert!(TRANSACTION_SCOPE.with(|scope| scope.borrow().is_none()));
        let error = SpawnError::capacity_refusal("daemon.sock", "no background capacity".into());
        assert!(!error.permits_rebind());
        assert!(error.definitely_unsubmitted());
        assert_eq!(error.source.kind(), io::ErrorKind::WouldBlock);
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
