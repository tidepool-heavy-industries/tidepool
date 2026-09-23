//! Versioned transport for bound resident compiler endpoints.
//!
//! External wire: little-endian, length-prefixed frames over a Unix domain
//! socket. An ordinary connection carries one request; a transaction
//! connection carries ordered requests while retaining one worker. This
//! module owns both ends; the Haskell worker sees a private stdin/stdout loop.
//!
//! ```text
//! frame     ::= u32-LE length, then that many raw bytes (UTF-8 text)
//! preflight ::= "TPDPF001"
//! identity  ::= "TPDPI001" producer[32] boot_epoch[32]
//! request   ::= "TPDRQ001" expected_epoch[32]
//!               frame(cwd) u32-LE(argc) frame(argv[0]) .. frame(argv[n-1])
//! transaction ::= "TPDTR001" expected_epoch[32]
//!                 (request-tag request)* end-tag
//! stop      ::= "TPDST001"
//! stop_ack  ::= 1u8
//! decision  ::= accepted:u8 | rejected:u8 frame(reason)
//! response  ::= i32-LE(exit_code) frame(stdout) frame(stderr)
//! ```
//!
//! `stop` is unauthenticated and carries no epoch: any local caller with
//! socket access may ask the daemon to retire. It acks once, then retires
//! its socket and exits its accept loop exactly as it does on a watched-stamp
//! change — any request already accepted finishes first (the accept loop is
//! single-threaded), and any client still queued behind it gets an explicit
//! `rejected` in the same drain.
//!
//! Connect failure or an explicit rejection proves the request was not
//! accepted and permits rebinding. Once the accepted marker is observed, EOF
//! or any other response failure is indeterminate and must never be replayed.

use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, ExitStatus, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

use crate::frontend::{DaemonConfig, FrontendError, PreparedWorker};
use crate::ExtractRequest;

/// Bound on the daemon round-trip's I/O (connect itself is local and
/// near-instant over a UNIX domain socket, so this bounds the READ side — a
/// wedged or overloaded daemon must not hang the caller forever). Generous:
/// a COLD resident-session compile can legitimately take several seconds
/// so this is sized well above a cold compile, not
/// tuned to the warm case.
// The resident GHC worker is deliberately single-threaded. Four heavy clients
// may therefore wait for three complete cold compiles before their own reply;
// this bounds a genuinely wedged daemon without mistaking ordinary queueing
// for failure and creating a herd of duplicate direct workers.
const IO_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_REQUEST_FRAME_BYTES: u32 = 16 * 1024 * 1024;
const MAX_REQUEST_ARGS: u32 = 4096;
const PREFLIGHT: &[u8; 8] = b"TPDPF001";
const PREFLIGHT_RESPONSE: &[u8; 8] = b"TPDPI001";
const REQUEST: &[u8; 8] = b"TPDRQ001";
/// A graceful-stop request: no epoch, no body. The daemon acks with a single
/// byte, retires its socket exactly as it does on a watched-stamp change
/// (queued clients get an explicit `REJECTED` — known-unsubmitted, safe to
/// rebind direct), and exits its accept loop. The single-threaded accept
/// loop only reads this connection once the request it is currently
/// servicing has finished, so a `STOP` sent mid-compile never interrupts it.
const STOP: &[u8; 8] = b"TPDST001";
const STOP_ACK: u8 = 1;
pub(crate) const TRANSACTION: &[u8; 8] = b"TPDTR001";
pub(crate) const TRANSACTION_END: u8 = 0;
pub(crate) const TRANSACTION_REQUEST: u8 = 1;
/// Requests one worker serves before the daemon replaces it. Keep ordinary
/// request-count rotation out of the common session length; the RSS ceiling
/// remains the tighter bound when a worker grows quickly.
const DEFAULT_ROTATE_AFTER: u64 = 1024;
/// Worker RSS above which the daemon replaces it after a request. A warm
/// prepared-route worker holds its module memo at roughly 2.5 GiB; a lower
/// bound replaces it after nearly every request and discards that memo.
/// Logs showed warm workers crossing 6 GiB and rotating long before 1024
/// requests, discarding the memo; 10 GiB fits beside a 10-job cargo build on
/// a 31 GiB box.
const DEFAULT_RSS_CEILING_MB: u64 = 10 * 1024;
/// Absolute wall-clock bound on a single compiler request served by the
/// pinned GHC worker (begin_transaction/request/end_transaction are cheap;
/// this bounds the request itself). The daemon's accept loop is
/// single-threaded: a worker that stops replying — wedged GHC, an infinite
/// loop in the compiled program's desugaring, a stuck external tool — would
/// otherwise block every other client forever. `kill_process` reclaims the
/// worker at expiry and the request fails with a named deadline error; the
/// existing crash-recovery path (`persistent` mode) replaces the worker for
/// the next request the same way it recovers from any other worker failure.
/// Generous: a cold compile of the full stdlib can legitimately take
/// minutes, so this is sized well above any ordinary compile, not tuned to
/// the warm case.
const DEFAULT_REQUEST_DEADLINE: Duration = Duration::from_secs(15 * 60);
const ACCEPTED: u8 = 1;
const REJECTED: u8 = 0;
type WorkerResponse = (i32, Vec<u8>, Vec<u8>);

fn pane_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::new("warn,tidepool_extract_cmd::daemon=info")
}

fn tracing_subscriber<D, P, J>(
    detailed_writer: D,
    pane_writer: P,
    trace_writer: J,
    detailed_filter: tracing_subscriber::EnvFilter,
) -> impl tracing::Subscriber + Send + Sync
where
    D: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    P: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    J: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    let detailed = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(detailed_writer)
        .with_filter(detailed_filter);
    let pane = tracing_subscriber::fmt::layer()
        .compact()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_writer(pane_writer)
        .with_filter(pane_filter());
    // The structured sibling of the daemon's text log. Its `run_id` and
    // `compile_request` fields are the two keys an Exomonad run's host trace
    // joins on.
    let trace = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_ansi(false)
        .with_writer(trace_writer)
        .with_filter(tracing_subscriber::EnvFilter::new(
            "info,tidepool_extract_cmd=debug",
        ));
    tracing_subscriber::registry()
        .with(detailed)
        .with(pane)
        .with(trace)
}

/// The structured trace lands beside the daemon's text log, sharing its stem:
/// `<run_id>-compiler.log` gets `<run_id>-compiler.jsonl`.
pub(crate) fn trace_path(log_path: &Path) -> std::path::PathBuf {
    log_path.with_extension("jsonl")
}

/// The returned guard owns the trace appender's flush thread; the daemon's
/// entry point binds it for the process's life.
pub(crate) fn init_tracing(
    config: &DaemonConfig,
) -> Result<Option<tracing_appender::non_blocking::WorkerGuard>, FrontendError> {
    let detailed: Box<dyn Write + Send> = match &config.log_path {
        Some(path) => {
            let parent = path.parent().ok_or_else(|| {
                FrontendError::Daemon("compiler log path has no parent directory".to_owned())
            })?;
            fs::create_dir_all(parent).map_err(FrontendError::Io)?;
            Box::new(
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(FrontendError::Io)?,
            )
        }
        None => Box::new(io::sink()),
    };
    let (trace, guard): (Box<dyn Write + Send>, _) = match &config.log_path {
        Some(path) => {
            let file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(trace_path(path))
                .map_err(FrontendError::Io)?;
            let (writer, guard) = tracing_appender::non_blocking(file);
            (Box::new(writer), Some(guard))
        }
        None => (Box::new(io::sink()), None),
    };
    let detailed_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug"));
    tracing_subscriber(
        Mutex::new(detailed),
        io::stderr,
        Mutex::new(trace),
        detailed_filter,
    )
    .try_init()
    .map_err(|error| {
        FrontendError::Daemon(format!("could not initialize compiler tracing: {error}"))
    })?;
    Ok(guard)
}

pub(crate) struct DaemonBinding {
    pub(crate) producer: [u8; 32],
    pub(crate) epoch: [u8; 32],
}

/// Failure while attempting one daemon request. The point of failure carries
/// settlement information. Only connect/setup failure or explicit rejection
/// proves nonacceptance. A failed write or missing marker is indeterminate.
#[derive(Debug)]
pub(crate) enum DaemonError {
    /// The socket does not exist, or nothing is listening — the ordinary,
    /// expected shape of "no daemon running."
    Connect(io::Error),
    /// A read or write failed for a reason other than a clean EOF (a timeout,
    /// a reset connection, ...).
    Io(io::Error),
    /// EOF before a complete frame/response arrived — the daemon crashed (or
    /// was killed) mid-request. The wire's own framing makes this
    /// unambiguous: a clean response is always a complete, self-describing
    /// byte sequence, so any short read here can only mean the peer is gone.
    Crashed,
    /// The daemon rejected the bound epoch or deployment before acknowledging
    /// acceptance. It guarantees this request will not execute.
    NotAccepted(String),
    /// The daemon acknowledged acceptance before the enclosed response error.
    AfterAcceptance(Box<DaemonError>),
    Protocol(String),
}

impl DaemonError {
    pub(crate) fn is_not_accepted(&self) -> bool {
        matches!(self, Self::Connect(_) | Self::NotAccepted(_))
    }

    pub(crate) fn was_accepted(&self) -> bool {
        matches!(self, Self::AfterAcceptance(_))
    }
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::Connect(e) => write!(f, "daemon connect failed: {e}"),
            DaemonError::Io(e) => write!(f, "daemon I/O error: {e}"),
            DaemonError::Crashed => write!(f, "daemon crashed mid-request"),
            DaemonError::NotAccepted(message) => {
                write!(f, "daemon did not accept request: {message}")
            }
            DaemonError::AfterAcceptance(error) => {
                write!(f, "daemon response failed after acceptance: {error}")
            }
            DaemonError::Protocol(message) => write!(f, "daemon protocol error: {message}"),
        }
    }
}

impl std::error::Error for DaemonError {}

/// Connect to `socket_path`, send `(cwd, worker argv)` as one request, and
/// return the synthesized [`Output`] the daemon's response describes. The
/// worker argv includes the versioned typed request payload used by a direct
/// spawn, so both transports reach the same Haskell dispatch path.
pub(crate) fn execute(
    socket_path: &Path,
    epoch: &[u8; 32],
    cwd: &Path,
    argv: &[OsString],
) -> Result<Output, DaemonError> {
    let admission_started = Instant::now();
    let mut stream = UnixStream::connect(socket_path).map_err(DaemonError::Connect)?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| DaemonError::NotAccepted(error.to_string()))?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|error| DaemonError::NotAccepted(error.to_string()))?;

    let mut req = Vec::new();
    req.extend_from_slice(REQUEST);
    req.extend_from_slice(epoch);
    req.extend_from_slice(&encode_request(cwd, argv));
    if let Err(error) = stream.write_all(&req) {
        // The daemon may reject and close before reading every byte. Only a
        // rejection it already sent proves nonacceptance; any other loss after
        // bytes may have reached it stays indeterminate.
        return Err(match explicit_rejection(&mut stream) {
            Some(message) => DaemonError::NotAccepted(message),
            None => DaemonError::Io(error),
        });
    }
    // A missing marker (including orderly EOF) does not prove the peer did
    // not accept. Only an explicit rejection permits rebinding after submission.
    let state = read_exact_or_crash(&mut stream, 1)?[0];
    tracing::info!(
        phase = "compiler_request_admission",
        elapsed_ms = u64::try_from(admission_started.elapsed().as_millis()).unwrap_or(u64::MAX),
        accepted = state == ACCEPTED,
        "compiler phase finished"
    );
    match state {
        ACCEPTED => {
            let response_started = Instant::now();
            let response = decode_output(&mut stream)
                .map_err(|error| DaemonError::AfterAcceptance(Box::new(error)));
            tracing::info!(
                phase = "compiler_response",
                elapsed_ms =
                    u64::try_from(response_started.elapsed().as_millis()).unwrap_or(u64::MAX),
                success = response.is_ok(),
                "compiler phase finished"
            );
            response
        }
        REJECTED => {
            let message = String::from_utf8_lossy(&read_frame(&mut stream)?).into_owned();
            Err(DaemonError::NotAccepted(message))
        }
        other => Err(DaemonError::Protocol(format!(
            "unknown acceptance marker {other}"
        ))),
    }
}

pub(crate) fn begin_transaction(
    socket_path: &Path,
    epoch: &[u8; 32],
) -> Result<UnixStream, DaemonError> {
    let mut stream = UnixStream::connect(socket_path).map_err(DaemonError::Connect)?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| DaemonError::NotAccepted(error.to_string()))?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|error| DaemonError::NotAccepted(error.to_string()))?;
    stream.write_all(TRANSACTION).map_err(DaemonError::Io)?;
    stream.write_all(epoch).map_err(DaemonError::Io)?;
    stream.flush().map_err(DaemonError::Io)?;
    let state = read_exact_or_crash(&mut stream, 1)?[0];
    match state {
        ACCEPTED => Ok(stream),
        REJECTED => {
            let message = String::from_utf8_lossy(&read_frame(&mut stream)?).into_owned();
            Err(DaemonError::NotAccepted(message))
        }
        other => Err(DaemonError::Protocol(format!(
            "unknown transaction acceptance marker {other}"
        ))),
    }
}

pub(crate) fn execute_transaction_request(
    stream: &mut UnixStream,
    cwd: &Path,
    argv: &[OsString],
) -> Result<Output, DaemonError> {
    let started = Instant::now();
    stream
        .write_all(&[TRANSACTION_REQUEST])
        .map_err(|error| DaemonError::AfterAcceptance(Box::new(DaemonError::Io(error))))?;
    stream
        .write_all(&encode_request(cwd, argv))
        .map_err(|error| DaemonError::AfterAcceptance(Box::new(DaemonError::Io(error))))?;
    stream
        .flush()
        .map_err(|error| DaemonError::AfterAcceptance(Box::new(DaemonError::Io(error))))?;
    let response =
        decode_output(stream).map_err(|error| DaemonError::AfterAcceptance(Box::new(error)));
    tracing::info!(
        phase = "compiler_transaction_response",
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        success = response.is_ok(),
        "compiler phase finished"
    );
    response
}

pub(crate) fn end_transaction(stream: &mut UnixStream) -> Result<(), DaemonError> {
    stream
        .write_all(&[TRANSACTION_END])
        .map_err(|error| DaemonError::AfterAcceptance(Box::new(DaemonError::Io(error))))?;
    stream
        .flush()
        .map_err(|error| DaemonError::AfterAcceptance(Box::new(DaemonError::Io(error))))?;
    let acknowledgement = read_exact_or_crash(stream, 1)
        .map_err(|error| DaemonError::AfterAcceptance(Box::new(error)))?;
    if acknowledgement == [ACCEPTED] {
        Ok(())
    } else {
        Err(DaemonError::AfterAcceptance(Box::new(
            DaemonError::Protocol("compiler transaction close was not acknowledged".to_owned()),
        )))
    }
}

fn explicit_rejection(stream: &mut UnixStream) -> Option<String> {
    let marker = read_exact_or_crash(stream, 1).ok()?;
    if marker[0] != REJECTED {
        return None;
    }
    read_frame(stream)
        .ok()
        .map(|frame| String::from_utf8_lossy(&frame).into_owned())
}

pub(crate) fn preflight(socket_path: &Path) -> Result<DaemonBinding, DaemonError> {
    let started = Instant::now();
    let mut stream = UnixStream::connect(socket_path).map_err(DaemonError::Connect)?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(DaemonError::Io)?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(DaemonError::Io)?;
    stream.write_all(PREFLIGHT).map_err(DaemonError::Io)?;
    let magic = read_exact_or_crash(&mut stream, PREFLIGHT_RESPONSE.len())?;
    if magic != PREFLIGHT_RESPONSE {
        return Err(DaemonError::Protocol(
            "invalid preflight response".to_owned(),
        ));
    }
    let producer: [u8; 32] = read_exact_or_crash(&mut stream, 32)?
        .try_into()
        .map_err(|_| DaemonError::Crashed)?;
    let epoch: [u8; 32] = read_exact_or_crash(&mut stream, 32)?
        .try_into()
        .map_err(|_| DaemonError::Crashed)?;
    tracing::info!(
        phase = "compiler_preflight",
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "compiler phase finished"
    );
    Ok(DaemonBinding { producer, epoch })
}

/// Bound on the STOP round trip. The daemon writes its ack immediately,
/// before draining or retiring, so this only needs to cover local
/// round-trip time — never the in-flight compile the daemon may still be
/// finishing when `STOP` is sent.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Ask a running daemon at `socket_path` to stop gracefully: it finishes any
/// request already accepted, retires its socket so queued clients rebind
/// direct, and exits its accept loop. Idempotent: no daemon listening (the
/// connect itself fails) is success, not failure, since the desired end
/// state — no daemon — already holds.
pub(crate) fn request_stop(socket_path: &Path) -> Result<(), DaemonError> {
    let mut stream = match UnixStream::connect(socket_path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(DaemonError::Connect(error)),
    };
    stream
        .set_read_timeout(Some(STOP_TIMEOUT))
        .map_err(DaemonError::Io)?;
    stream
        .set_write_timeout(Some(STOP_TIMEOUT))
        .map_err(DaemonError::Io)?;
    stream.write_all(STOP).map_err(DaemonError::Io)?;
    let mut ack = [0u8; 1];
    match stream.read_exact(&mut ack) {
        // A clean ack or an EOF (the daemon may exit before this client
        // drains the reply) both prove the daemon saw the request and is
        // stopping or gone; only a genuine I/O failure (e.g. a timeout) is
        // reported as one.
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
        Err(error) => Err(DaemonError::Io(error)),
    }
}

fn push_frame(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

/// Encode the wire's `request` shape. `pub(crate)` (not private) so the unit
/// tests below can pin its exact byte layout.
pub(crate) fn encode_request(cwd: &Path, argv: &[OsString]) -> Vec<u8> {
    let mut buf = Vec::new();
    push_frame(&mut buf, cwd.as_os_str().as_bytes());
    buf.extend_from_slice(&(argv.len() as u32).to_le_bytes());
    for a in argv {
        push_frame(&mut buf, a.as_bytes());
    }
    buf
}

fn read_exact_or_crash<R: Read>(r: &mut R, n: usize) -> Result<Vec<u8>, DaemonError> {
    let mut buf = vec![0u8; n];
    match r.read_exact(&mut buf) {
        Ok(()) => Ok(buf),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(DaemonError::Crashed),
        Err(e) => Err(DaemonError::Io(e)),
    }
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, DaemonError> {
    let b = read_exact_or_crash(r, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_frame<R: Read>(r: &mut R) -> Result<Vec<u8>, DaemonError> {
    let n = read_u32(r)? as usize;
    read_exact_or_crash(r, n)
}

/// Decode the wire's `response` shape from any [`Read`] — a real
/// [`UnixStream`] in production, a plain byte slice in the unit tests below
/// (pinning the codec's round-trip and truncated-input behavior without a
/// real socket).
pub(crate) fn decode_response<R: Read>(r: &mut R) -> Result<(i32, Vec<u8>, Vec<u8>), DaemonError> {
    let code_bytes = read_exact_or_crash(r, 4)?;
    let code = i32::from_le_bytes([code_bytes[0], code_bytes[1], code_bytes[2], code_bytes[3]]);
    let stdout = read_frame(r)?;
    let stderr = read_frame(r)?;
    Ok((code, stdout, stderr))
}

/// Encode a plain exit code as the wait(2)-style status
/// [`ExitStatusExt::from_raw`] expects — NOT the bare code. On Linux, a
/// normal exit encodes as `code << 8` (the low byte clear signals
/// `WIFEXITED`, the next byte is `WEXITSTATUS`). Masking through `u8` first
/// mirrors how a real process's exit code is truncated to one byte by the
/// OS itself (e.g. `exit(-1)` becomes exit code 255 to a waiting shell) —
/// this is not a bug workaround, it is what `wait(2)` actually encodes.
fn encode_wait_status(code: i32) -> i32 {
    ((code as u8) as i32) << 8
}

fn synthesize_output(code: i32, stdout: Vec<u8>, stderr: Vec<u8>) -> Output {
    Output {
        status: ExitStatus::from_raw(encode_wait_status(code)),
        stdout,
        stderr,
    }
}

pub(crate) fn decode_output<R: Read>(r: &mut R) -> Result<Output, DaemonError> {
    let (code, stdout, stderr) = decode_response(r)?;
    Ok(synthesize_output(code, stdout, stderr))
}

/// One step of a connection's request stream, as `service_transaction`'s
/// caller supplies it: a real transaction reads a command byte off the wire
/// each time; a plain request is a one-request transaction whose supplier
/// yields its single already-parsed request and then an orderly end.
enum RequestStep {
    Request(std::path::PathBuf, Vec<OsString>),
    End,
    Malformed,
}

/// What `serve`'s accept loop does after a connection has been serviced.
enum ConnectionOutcome {
    Continue,
    Retire,
}

/// Owns the pinned-worker portion of one connection's lifecycle: begin the
/// worker transaction, serve one or more requests supplied by `next_request`,
/// end the transaction, and apply failure replacement and RSS/served
/// rotation. A plain (non-transaction) connection is served as a
/// one-request transaction — `transaction` is false and `next_request`
/// yields exactly one request — so both shapes share one lifecycle, one set
/// of start/finish log records (`transaction` distinguishes them), and one
/// rotation policy. `connection` is owned so a failure can close it
/// immediately, before the worker is replaced: an accepted request stays
/// indeterminate and must never be replayed.
#[allow(clippy::too_many_arguments)]
fn service_transaction(
    mut connection: UnixStream,
    worker: &mut Worker,
    prepared: &PreparedWorker,
    config: &DaemonConfig,
    run_id: &str,
    request_deadline: Duration,
    rotate_after: u64,
    rss_ceiling_mb: u64,
    transaction: bool,
    served: &mut u64,
    followed_rotation: &mut bool,
    mut next_request: impl FnMut(&mut UnixStream) -> RequestStep,
) -> Result<ConnectionOutcome, FrontendError> {
    let mut transaction_failed = worker.begin_transaction().err();
    let mut orderly_end = false;
    while transaction_failed.is_none() {
        match next_request(&mut connection) {
            RequestStep::End => {
                orderly_end = true;
                break;
            }
            RequestStep::Malformed => break,
            RequestStep::Request(cwd, argv) => {
                let compile_request = compile_request_correlation(&cwd, &argv);
                let request_span = tracing::info_span!(
                    "compile_request",
                    run_id,
                    %compile_request,
                    followed_rotation = *followed_rotation,
                    served = *served,
                    transaction,
                );
                let _entered = request_span.enter();
                *followed_rotation = false;
                let started = Instant::now();
                tracing::info!(run_id, %compile_request, "compiler request started");
                tracing::debug!(run_id, %compile_request, source_root = %cwd.display(), "compiler request source");
                match worker.request_while_connected(&connection, &cwd, &argv, request_deadline) {
                    Ok((code, stdout, stderr)) => {
                        *served += 1;
                        let elapsed_ms =
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                        log_compile_timing(run_id, &compile_request, &stderr);
                        let stderr = diagnostic_stderr(&stderr);
                        tracing::info!(
                            run_id,
                            %compile_request,
                            elapsed_ms,
                            phase = "compiler_service",
                            exit_code = code,
                            transaction,
                            "compiler request finished"
                        );
                        if write_response(&mut connection, code, &stdout, &stderr).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let elapsed_ms =
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                        tracing::error!(
                            run_id,
                            %compile_request,
                            elapsed_ms,
                            phase = "compiler_service",
                            %error,
                            transaction,
                            "compiler request failed"
                        );
                        transaction_failed = Some(error);
                    }
                }
            }
        }
    }
    if transaction_failed.is_none() {
        transaction_failed = worker.end_transaction().err();
    }
    if let Some(error) = transaction_failed {
        tracing::error!(run_id, %error, transaction, "compiler transaction failed");
        // The accepted request(s) stay indeterminate; do not replay them.
        // Drop the connection before replacing the failed worker.
        drop(connection);
        worker.abort();
        if !config.persistent {
            return Err(error);
        }
        *worker = Worker::spawn(prepared)?;
        *served = 0;
        *followed_rotation = true;
        return Ok(ConnectionOutcome::Continue);
    }
    if transaction && orderly_end {
        log_send_failure(run_id, "transaction accepted", connection.write_all(&[ACCEPTED]));
        log_send_failure(run_id, "transaction accepted flush", connection.flush());
    }
    drop(connection);
    let worker_rss = worker_rss_mb_logged(run_id, worker.child.id());
    if *served >= rotate_after || worker_rss > rss_ceiling_mb {
        // Replacing the worker discards its module memo; the next request
        // recompiles every library module.
        tracing::info!(
            run_id,
            served = *served,
            rotate_after,
            worker_rss_mb = worker_rss,
            rss_ceiling_mb,
            transaction,
            "replacing compiler worker"
        );
        if config.persistent {
            // Long-lived composition roots keep the protocol endpoint stable
            // while bounding GHC state. The worker executable is boot-pinned
            // by `PreparedWorker`, so replacing only this child does not
            // change the endpoint's producer.
            worker.shutdown();
            *worker = Worker::spawn(prepared)?;
            *served = 0;
            *followed_rotation = true;
            return Ok(ConnectionOutcome::Continue);
        }
        return Ok(ConnectionOutcome::Retire);
    }
    Ok(ConnectionOutcome::Continue)
}

pub(crate) fn serve(config: &DaemonConfig, prepared: PreparedWorker) -> Result<u8, FrontendError> {
    let producer = prepared.producer_identity()?;
    let epoch = boot_epoch()?;
    if let Some(parent) = config.socket.parent() {
        fs::create_dir_all(parent).map_err(FrontendError::Io)?;
    }
    let boot_stamp = config
        .watch_stamp
        .as_deref()
        .map(read_optional)
        .transpose()
        .map_err(FrontendError::Io)?;
    let rotate_after = config.rotate_after.unwrap_or(DEFAULT_ROTATE_AFTER);
    let rss_ceiling_mb = config.rss_ceiling_mb.unwrap_or(DEFAULT_RSS_CEILING_MB);
    let request_deadline = config
        .request_deadline_secs
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_REQUEST_DEADLINE);
    let executable = std::env::current_exe().map_err(FrontendError::Io)?;
    let socket = OwnedSocket::bind(&config.socket)?;
    let listener = &socket.listener;
    let mut worker = Worker::spawn(&prepared)?;
    let run_id = config.run_id.as_deref().unwrap_or("standalone");
    tracing::info!(
        run_id,
        version = env!("CARGO_PKG_VERSION"),
        executable = %executable.display(),
        worker = %prepared.selection().display(),
        producer = %hex(&producer),
        socket = %config.socket.display(),
        detailed_log = %config
            .log_path
            .as_deref()
            .unwrap_or_else(|| Path::new("disabled"))
            .display(),
        "compiler daemon ready"
    );

    let result = (|| {
        let mut served = 0;
        // Set whenever the worker is replaced, and read by the next request's
        // span: that request recompiles every library module from cold.
        let mut followed_rotation = false;
        loop {
            let (mut connection, _) = listener.accept().map_err(FrontendError::Io)?;
            if connection
                .set_read_timeout(Some(Duration::from_secs(30)))
                .is_err()
            {
                continue;
            }
            let mut kind = [0u8; 8];
            if connection.read_exact(&mut kind).is_err() {
                continue;
            }
            if &kind == PREFLIGHT {
                if stamp_changed(config, &boot_stamp)? {
                    socket.retire()?;
                    break;
                }
                let mut response = Vec::with_capacity(72);
                response.extend_from_slice(PREFLIGHT_RESPONSE);
                response.extend_from_slice(&producer);
                response.extend_from_slice(&epoch);
                log_send_failure(run_id, "preflight response", connection.write_all(&response));
                continue;
            }
            if &kind == STOP {
                tracing::info!(run_id, "compiler daemon stopping on request");
                log_send_failure(run_id, "stop ack", connection.write_all(&[STOP_ACK]));
                log_send_failure(run_id, "stop ack flush", connection.flush());
                socket.retire()?;
                break;
            }
            if &kind == TRANSACTION {
                let expected_epoch = match read_exact_or_crash(&mut connection, 32) {
                    Ok(bytes) => bytes,
                    Err(_) => continue,
                };
                if expected_epoch != epoch {
                    log_reject_failure(run_id, "stale epoch (transaction)", write_rejected(&mut connection, "daemon boot epoch changed"));
                    continue;
                }
                match stamp_changed(config, &boot_stamp) {
                    Ok(false) => {}
                    Ok(true) => {
                        log_reject_failure(run_id, "deployment changed (transaction)", write_rejected(&mut connection, "watched deployment changed"));
                        socket.retire()?;
                        break;
                    }
                    Err(error) => {
                        log_reject_failure(run_id, "daemon stopping (transaction)", write_rejected(&mut connection, "daemon stopping"));
                        return Err(error);
                    }
                }
                if connection.write_all(&[ACCEPTED]).is_err() || connection.flush().is_err() {
                    continue;
                }
                log_send_failure(run_id, "transaction read timeout", connection.set_read_timeout(Some(IO_TIMEOUT)));
                let outcome = service_transaction(
                    connection,
                    &mut worker,
                    &prepared,
                    config,
                    run_id,
                    request_deadline,
                    rotate_after,
                    rss_ceiling_mb,
                    true,
                    &mut served,
                    &mut followed_rotation,
                    |connection| {
                        let mut command = [0u8; 1];
                        if connection.read_exact(&mut command).is_err() {
                            return RequestStep::Malformed;
                        }
                        match command[0] {
                            TRANSACTION_END => RequestStep::End,
                            TRANSACTION_REQUEST => {
                                let (cwd, argv) = match read_request(connection) {
                                    Ok(request) => request,
                                    Err(error) => {
                                        tracing::warn!(run_id, %error, "compiler transaction request was malformed");
                                        return RequestStep::Malformed;
                                    }
                                };
                                match normalize_worker_argv(argv) {
                                    Ok(argv) => RequestStep::Request(cwd, argv),
                                    Err(error) => {
                                        tracing::warn!(run_id, %error, "compiler transaction request was invalid");
                                        RequestStep::Malformed
                                    }
                                }
                            }
                            other => {
                                tracing::warn!(
                                    run_id,
                                    command = other,
                                    "unknown compiler transaction command"
                                );
                                RequestStep::Malformed
                            }
                        }
                    },
                )?;
                match outcome {
                    ConnectionOutcome::Continue => continue,
                    ConnectionOutcome::Retire => {
                        socket.retire()?;
                        break;
                    }
                }
            }
            if &kind != REQUEST {
                continue;
            }
            let expected_epoch = match read_exact_or_crash(&mut connection, 32) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            if expected_epoch != epoch {
                tracing::warn!(run_id, "rejected compiler request for stale daemon epoch");
                log_reject_failure(run_id, "stale epoch (request)", write_rejected(&mut connection, "daemon boot epoch changed"));
                continue;
            }
            let (cwd, argv) = match read_request(&mut connection) {
                Ok(request) => request,
                Err(_) => {
                    tracing::warn!(run_id, "rejected malformed compiler request");
                    log_reject_failure(run_id, "malformed request", write_rejected(&mut connection, "invalid compiler request"));
                    continue;
                }
            };
            let worker_argv = match normalize_worker_argv(argv) {
                Ok(argv) => argv,
                Err(_) => {
                    tracing::warn!(run_id, "rejected invalid typed compiler request");
                    log_reject_failure(run_id, "invalid typed request", write_rejected(&mut connection, "invalid typed worker request"));
                    continue;
                }
            };
            // The second stamp check is the acceptance fence. If it passes,
            // the acknowledgement is flushed before work begins; every later
            // transport failure is therefore indeterminate and never replayed.
            match stamp_changed(config, &boot_stamp) {
                Ok(false) => {}
                Ok(true) => {
                    log_reject_failure(run_id, "deployment changed (request)", write_rejected(&mut connection, "watched deployment changed"));
                    socket.retire()?;
                    break;
                }
                Err(error) => {
                    log_reject_failure(run_id, "daemon stopping (request)", write_rejected(&mut connection, "daemon stopping"));
                    return Err(error);
                }
            }
            if connection.write_all(&[ACCEPTED]).is_err() || connection.flush().is_err() {
                continue;
            }
            log_send_failure(run_id, "request read timeout", connection.set_read_timeout(Some(IO_TIMEOUT)));
            let mut first_request = Some((cwd, worker_argv));
            let outcome = service_transaction(
                connection,
                &mut worker,
                &prepared,
                config,
                run_id,
                request_deadline,
                rotate_after,
                rss_ceiling_mb,
                false,
                &mut served,
                &mut followed_rotation,
                |_connection| match first_request.take() {
                    Some((cwd, argv)) => RequestStep::Request(cwd, argv),
                    None => RequestStep::End,
                },
            )?;
            match outcome {
                ConnectionOutcome::Continue => continue,
                ConnectionOutcome::Retire => {
                    socket.retire()?;
                    break;
                }
            }
        }
        Ok(0)
    })();

    // Every exit, orderly or not, drains connected clients with an explicit
    // rejection before the endpoint closes. Retirement is idempotent.
    let retired = socket.retire();
    drop(socket);
    worker.shutdown();
    match (result, retired) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(code), Ok(())) => Ok(code),
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    encoded
}

/// Record the worker's compile-cost lines in the detailed daemon log (debug
/// level: the file, never the tmux pane), so an Exomonad run's compiler log and a
/// test battery's daemon log show where each request's time went. The worker always writes one `tidepool-compile-summary` line and,
/// under `TIDEPOOL_TIMING=1`, one `tidepool-timing` line per phase and one
/// `tidepool-memo-miss` line per memoized module it recompiled. Structural
/// `tidepool-checked` and `tidepool-target` lines distinguish metadata-only
/// checks from executable target desugaring. The prefixes are the single
/// list in [`crate::diagnostics`], shared with `tidepool_toolchain::diag`.
fn diagnostic_stderr(stderr: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| !crate::diagnostics::is_machine_stderr_line(line))
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

fn log_compile_timing(run_id: &str, compile_request: &str, stderr: &[u8]) {
    for line in String::from_utf8_lossy(stderr).lines() {
        let line = line.trim();
        if crate::diagnostics::is_machine_stderr_line(line) {
            tracing::debug!(run_id, %compile_request, line, "compiler timing");
        }
    }
}

/// The join key between a client's compile span and the daemon's own
/// `compile_request` span. Both sides hash the same bytes: the client sends
/// `ExtractRequest::worker_argv`, which is already the two-element typed form
/// `normalize_worker_argv` returns unchanged, so no id has to travel on the
/// wire.
pub(crate) fn compile_request_correlation(cwd: &Path, worker_argv: &[OsString]) -> String {
    let digest = blake3::hash(&encode_request(cwd, worker_argv));
    hex(&digest.as_bytes()[..8])
}

fn stamp_changed(
    config: &DaemonConfig,
    boot_stamp: &Option<Option<Vec<u8>>>,
) -> Result<bool, FrontendError> {
    match (&config.watch_stamp, boot_stamp) {
        (Some(path), Some(at_boot)) => {
            Ok(read_optional(path).map_err(FrontendError::Io)? != *at_boot)
        }
        _ => Ok(false),
    }
}

fn boot_epoch() -> Result<[u8; 32], FrontendError> {
    let mut epoch = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut epoch))
        .map_err(FrontendError::Io)?;
    Ok(epoch)
}

/// Owns only the socket inode created by this bind, under an advisory lock
/// beside the path that one daemon holds from bind until retirement. A second
/// daemon, starting or running, fails on the lock and never touches the path.
/// Holding the lock, a path that refuses connections is a dead daemon's, and
/// only the inode observed refusing is removed. Retirement cannot unlink a
/// replacement endpoint. The lock file itself is never removed: unlinking a
/// lock file lets two holders lock different inodes.
struct OwnedSocket {
    listener: UnixListener,
    path: std::path::PathBuf,
    identity: (u64, u64),
    endpoint_lock: Cell<Option<fs::File>>,
    retired: Cell<bool>,
}

fn endpoint_lock_path(path: &Path) -> std::path::PathBuf {
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    lock.into()
}

impl OwnedSocket {
    fn bind(path: &Path) -> Result<Self, FrontendError> {
        let endpoint_lock = Self::acquire_endpoint_lock(path)?;
        let listener = match UnixListener::bind(path) {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                Self::remove_dead_endpoint(path)?;
                UnixListener::bind(path).map_err(FrontendError::Io)?
            }
            Err(error) => return Err(FrontendError::Io(error)),
        };
        let metadata = fs::symlink_metadata(path).map_err(FrontendError::Io)?;
        Ok(Self {
            listener,
            path: path.to_owned(),
            identity: (metadata.dev(), metadata.ino()),
            endpoint_lock: Cell::new(Some(endpoint_lock)),
            retired: Cell::new(false),
        })
    }

    fn acquire_endpoint_lock(path: &Path) -> Result<fs::File, FrontendError> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(endpoint_lock_path(path))
            .map_err(FrontendError::Io)?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(fs::TryLockError::WouldBlock) => Err(FrontendError::Daemon(format!(
                "compiler socket {} is owned by another daemon",
                path.display()
            ))),
            Err(fs::TryLockError::Error(error)) => Err(FrontendError::Io(error)),
        }
    }

    fn remove_dead_endpoint(path: &Path) -> Result<(), FrontendError> {
        let observed = fs::symlink_metadata(path).map_err(FrontendError::Io)?;
        if !observed.file_type().is_socket() {
            return Err(FrontendError::Daemon(format!(
                "compiler socket path {} exists and is not a socket",
                path.display()
            )));
        }
        match UnixStream::connect(path) {
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
            _ => {
                return Err(FrontendError::Daemon(format!(
                    "compiler socket {} is live without the endpoint lock; stop its owner before starting",
                    path.display()
                )))
            }
        }
        let current = fs::symlink_metadata(path).map_err(FrontendError::Io)?;
        if (current.dev(), current.ino()) != (observed.dev(), observed.ino()) {
            return Err(FrontendError::Daemon(format!(
                "compiler socket {} was replaced while checking it",
                path.display()
            )));
        }
        fs::remove_file(path).map_err(FrontendError::Io)
    }

    fn unlink(&self) -> io::Result<()> {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if (metadata.dev(), metadata.ino()) == self.identity => {
                fs::remove_file(&self.path)
            }
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn retire(&self) -> Result<(), FrontendError> {
        if self.retired.replace(true) {
            return Ok(());
        }
        self.unlink().map_err(FrontendError::Io)?;
        // The path is free: a replacement daemon may take the endpoint while
        // this one drains its own, now unlinked, listener.
        drop(self.endpoint_lock.take());
        self.listener
            .set_nonblocking(true)
            .map_err(FrontendError::Io)?;
        // Bound shutdown even when a queued client sends a partial request.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let (mut connection, _) = match self.listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(FrontendError::Io(error)),
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            connection
                .set_write_timeout(Some(remaining.min(Duration::from_millis(100))))
                .map_err(FrontendError::Io)?;
            let preflight = {
                let mut reader = DeadlineReader {
                    stream: &mut connection,
                    deadline: deadline.min(Instant::now() + Duration::from_millis(100)),
                };
                let mut kind = [0; 8];
                let kind_read = reader.read_exact(&mut kind).is_ok();
                if kind_read && &kind == REQUEST {
                    let mut epoch = [0; 32];
                    if reader.read_exact(&mut epoch).is_ok() {
                        // best-effort: draining the request during rotation only
                        // to advance past it; this connection is being rejected
                        // either way and the parsed request is discarded.
                        read_request(&mut reader).ok();
                    }
                }
                kind_read && &kind == PREFLIGHT
            };
            // A request, complete or partial, never infers its settlement
            // from a closed connection. A preflight has nothing to settle.
            if !preflight {
                // best-effort: the peer may already be gone during rotation drain.
                write_rejected(&mut connection, "daemon rotating").ok();
            }
        }
        Ok(())
    }
}

struct DeadlineReader<'a> {
    stream: &'a mut UnixStream,
    deadline: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "rotation drain deadline",
            ));
        }
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(bytes)
    }
}

impl Drop for OwnedSocket {
    fn drop(&mut self) {
        // best-effort: Drop cannot propagate errors; a socket path already
        // gone (e.g. `retire` already unlinked it) is not a failure to report.
        self.unlink().ok();
    }
}

pub(crate) fn normalize_worker_argv(argv: Vec<OsString>) -> Result<Vec<OsString>, FrontendError> {
    if matches!(argv.as_slice(), [flag, _] if flag == crate::request::WORKER_REQUEST_FLAG) {
        ExtractRequest::decode_worker_argv(&argv)?;
        return Ok(argv);
    }
    if argv
        .iter()
        .any(|arg| arg == crate::request::WORKER_REQUEST_FLAG)
    {
        return Err(FrontendError::Usage(
            "typed worker request must contain exactly a marker and payload".to_owned(),
        ));
    }
    Ok(ExtractRequest::from_cli(&argv)?.worker_argv())
}

pub(crate) fn read_request(
    stream: &mut impl Read,
) -> Result<(std::path::PathBuf, Vec<OsString>), FrontendError> {
    let mut remaining = MAX_REQUEST_FRAME_BYTES;
    let cwd = OsString::from_vec(read_request_frame(stream, &mut remaining)?).into();
    let count = read_u32(stream).map_err(daemon_frontend_error)?;
    if count > MAX_REQUEST_ARGS {
        return Err(FrontendError::Daemon(format!(
            "daemon request has {count} arguments"
        )));
    }
    let mut argv = Vec::with_capacity(count as usize);
    for _ in 0..count {
        argv.push(OsString::from_vec(read_request_frame(
            stream,
            &mut remaining,
        )?));
    }
    Ok((cwd, argv))
}

fn read_request_frame(
    stream: &mut impl Read,
    remaining: &mut u32,
) -> Result<Vec<u8>, FrontendError> {
    let length = read_u32(stream).map_err(daemon_frontend_error)?;
    if length > *remaining {
        return Err(FrontendError::Daemon(format!(
            "daemon request frame is {length} bytes; remaining request budget is {remaining}"
        )));
    }
    *remaining -= length;
    read_exact_or_crash(stream, length as usize).map_err(daemon_frontend_error)
}

pub(crate) fn write_response(
    mut stream: impl Write,
    code: i32,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<(), FrontendError> {
    let mut response = code.to_le_bytes().to_vec();
    push_frame(&mut response, stdout);
    push_frame(&mut response, stderr);
    stream.write_all(&response).map_err(FrontendError::Io)
}

fn write_rejected(stream: &mut impl Write, message: &str) -> Result<(), FrontendError> {
    let mut response = vec![REJECTED];
    push_frame(&mut response, message.as_bytes());
    stream.write_all(&response).map_err(FrontendError::Io)?;
    stream.flush().map_err(FrontendError::Io)
}

/// Best-effort connection write: the caller already decided to drop this
/// connection (retire, continue, or break) regardless of the outcome, so a
/// failure cannot change control flow. It usually means the peer hung up
/// first; log it for diagnosis rather than discarding it silently.
fn log_send_failure(run_id: &str, context: &str, result: io::Result<()>) {
    if let Err(error) = result {
        tracing::debug!(run_id, %error, context, "daemon connection write failed");
    }
}

/// As [`log_send_failure`], for the typed rejection-write path.
fn log_reject_failure(run_id: &str, context: &str, result: Result<(), FrontendError>) {
    if let Err(error) = result {
        tracing::debug!(run_id, %error, context, "daemon rejection write failed");
    }
}

fn daemon_frontend_error(error: DaemonError) -> FrontendError {
    FrontendError::Daemon(error.to_string())
}

fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Worker RSS for rotation, with an operator-visible warning when `/proc` is
/// unreadable. Reading `0` here must be distinguishable in the log from a
/// worker that is simply small: an unlogged `unwrap_or(0)` would silently
/// disable the RSS ceiling instead of reporting "unknown".
fn worker_rss_mb_logged(run_id: &str, pid: u32) -> u64 {
    match worker_rss_mb(pid) {
        Ok(rss) => rss,
        Err(error) => {
            tracing::warn!(
                run_id,
                pid,
                %error,
                "could not read compiler worker RSS from /proc; rotation ceiling check treated it as 0 for this request"
            );
            0
        }
    }
}

fn worker_rss_mb(pid: u32) -> io::Result<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    Ok(status
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
        .unwrap_or(0)
        / 1024)
}

#[cfg(target_os = "linux")]
fn peer_disconnected(stream: &UnixStream) -> bool {
    const MSG_PEEK: std::os::raw::c_int = 0x2;
    const MSG_DONTWAIT: std::os::raw::c_int = 0x40;
    unsafe extern "C" {
        fn recv(
            socket: std::os::raw::c_int,
            buffer: *mut std::ffi::c_void,
            length: usize,
            flags: std::os::raw::c_int,
        ) -> isize;
    }
    let mut byte = 0u8;
    // SAFETY: `byte` is writable for the one-byte length supplied, and the
    // stream owns a live socket descriptor for the duration of this call.
    let received = unsafe {
        recv(
            stream.as_raw_fd(),
            (&mut byte as *mut u8).cast(),
            1,
            MSG_PEEK | MSG_DONTWAIT,
        )
    };
    if received == 0 {
        true
    } else if received < 0 {
        !matches!(
            io::Error::last_os_error().kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        )
    } else {
        false
    }
}

#[cfg(not(target_os = "linux"))]
fn peer_disconnected(_stream: &UnixStream) -> bool {
    false
}

pub(crate) struct Worker {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
}

impl Worker {
    pub(crate) fn spawn(prepared: &PreparedWorker) -> Result<Self, FrontendError> {
        let mut command = prepared.command();
        command
            .arg("--worker-loop-v2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        crate::process::child_dies_with_parent(&mut command);
        let mut child = command.spawn().map_err(FrontendError::Io)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| FrontendError::Daemon("worker stdin was not piped".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| FrontendError::Daemon("worker stdout was not piped".to_owned()))?;
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout,
        })
    }

    pub(crate) fn begin_transaction(&mut self) -> Result<(), FrontendError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| FrontendError::Daemon("worker stdin is closed".to_owned()))?;
        stdin.write_all(&[1]).map_err(FrontendError::Io)?;
        stdin.flush().map_err(FrontendError::Io)?;
        let acknowledgement =
            read_exact_or_crash(&mut self.stdout, 1).map_err(daemon_frontend_error)?;
        if acknowledgement != [1] {
            return Err(FrontendError::Daemon(
                "worker rejected transaction protocol".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn request(
        &mut self,
        cwd: &Path,
        argv: &[OsString],
    ) -> Result<(i32, Vec<u8>, Vec<u8>), FrontendError> {
        let bytes = encode_request(cwd, argv);
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| FrontendError::Daemon("worker stdin is closed".to_owned()))?;
        stdin
            .write_all(&[TRANSACTION_REQUEST])
            .map_err(FrontendError::Io)?;
        stdin.write_all(&bytes).map_err(FrontendError::Io)?;
        stdin.flush().map_err(FrontendError::Io)?;
        decode_response(&mut self.stdout).map_err(daemon_frontend_error)
    }

    /// Serve one request against the pinned worker, bounded on two axes: the
    /// caller's own connection (dropping the client interrupts an in-flight
    /// compile promptly) and an absolute `deadline` from when this request
    /// started (a wedged worker — hung GHC, a stuck external tool — must not
    /// block every other client on this single-threaded accept loop forever,
    /// even while the caller stays connected).
    fn request_while_connected(
        &mut self,
        connection: &UnixStream,
        cwd: &Path,
        argv: &[OsString],
        deadline: Duration,
    ) -> Result<WorkerResponse, FrontendError> {
        let pid = self.child.id();
        let started = Instant::now();
        let result = std::thread::scope(|scope| {
            let (completed_tx, completed_rx) = std::sync::mpsc::sync_channel(1);
            scope.spawn(move || {
                // best-effort: the receiver may already have returned via the
                // deadline or disconnect branch below and dropped its end.
                completed_tx.send(self.request(cwd, argv)).ok();
            });
            loop {
                match completed_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(result) => return result,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(FrontendError::Daemon(
                            "compiler worker request monitor disconnected".to_owned(),
                        ));
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
                let elapsed = started.elapsed();
                if elapsed >= deadline {
                    if let Err(error) = crate::process::kill_process(pid) {
                        tracing::warn!(pid, %error, "failed to kill deadline-exceeded compiler worker");
                    }
                    tracing::warn!(
                        cwd = %cwd.display(),
                        elapsed_secs = elapsed.as_secs(),
                        deadline_secs = deadline.as_secs(),
                        "compiler worker exceeded its request deadline; killing it"
                    );
                    // The kill unblocks whatever the worker was doing (a read
                    // or a write) so the monitor thread settles quickly; its
                    // own result is discarded in favor of a deadline error
                    // that names the bound, matching the disconnect branch's
                    // own definite report below.
                    completed_rx.recv().ok();
                    return Err(FrontendError::Daemon(format!(
                        "compiler worker exceeded its {}s request deadline and was killed",
                        deadline.as_secs()
                    )));
                }
                if peer_disconnected(connection) {
                    if let Err(error) = crate::process::kill_process(pid) {
                        tracing::warn!(pid, %error, "failed to kill compiler worker after client disconnect");
                    }
                    return completed_rx.recv().unwrap_or_else(|_| {
                        Err(FrontendError::Daemon(
                            "compiler worker stopped after client disconnect".to_owned(),
                        ))
                    });
                }
            }
        });
        result
    }

    pub(crate) fn end_transaction(&mut self) -> Result<(), FrontendError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| FrontendError::Daemon("worker stdin is closed".to_owned()))?;
        stdin
            .write_all(&[TRANSACTION_END])
            .map_err(FrontendError::Io)?;
        stdin.flush().map_err(FrontendError::Io)?;
        let acknowledgement =
            read_exact_or_crash(&mut self.stdout, 1).map_err(daemon_frontend_error)?;
        if acknowledgement != [1] {
            return Err(FrontendError::Daemon(
                "worker did not close transaction".to_owned(),
            ));
        }
        Ok(())
    }

    fn abort(&mut self) {
        drop(self.stdin.take());
        if let Err(error) = self.child.kill() {
            tracing::warn!(%error, "failed to kill aborted compiler worker");
        }
        if let Err(error) = self.child.wait() {
            tracing::warn!(%error, "failed to reap aborted compiler worker");
        }
    }

    pub(crate) fn shutdown(&mut self) {
        drop(self.stdin.take());
        if self.child.wait().is_err() {
            if let Err(error) = self.child.kill() {
                tracing::warn!(%error, "failed to kill compiler worker during shutdown");
            }
            if let Err(error) = self.child.wait() {
                tracing::warn!(%error, "failed to reap compiler worker during shutdown");
            }
        }
    }
}

/// A `PathBuf` from raw wire bytes — used only by the fake-daemon test
/// harness (never on the hot path; every real caller builds `Path`/`PathBuf`
/// from Rust-side values, never from decoded wire bytes).
#[cfg(test)]
fn path_from_bytes(bytes: Vec<u8>) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStringExt;
    std::path::PathBuf::from(OsString::from_vec(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn default_request_rotation_is_1024_with_existing_rss_ceiling() {
        assert_eq!(DEFAULT_ROTATE_AFTER, 1024);
        assert_eq!(DEFAULT_RSS_CEILING_MB, 10 * 1024);
        assert_eq!(DEFAULT_REQUEST_DEADLINE, Duration::from_secs(15 * 60));
    }

    #[derive(Clone, Default)]
    struct CapturedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    struct CapturedGuard(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for CapturedGuard {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedWriter {
        type Writer = CapturedGuard;

        fn make_writer(&'writer self) -> Self::Writer {
            CapturedGuard(std::sync::Arc::clone(&self.0))
        }
    }

    impl CapturedWriter {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    #[test]
    fn socket_ownership_preserves_existing_and_replacement_endpoints() {
        let path = test_socket("ownership");
        let owner = OwnedSocket::bind(&path).unwrap();
        assert!(OwnedSocket::bind(&path).is_err());
        assert!(UnixStream::connect(&path).is_ok());
        owner.retire().unwrap();
        let replacement = OwnedSocket::bind(&path).unwrap();
        drop(owner);
        assert!(UnixStream::connect(&path).is_ok());
        drop(replacement);
        assert!(!path.exists());
        // best-effort: test cleanup of a temp path.
        fs::remove_file(endpoint_lock_path(&path)).ok();
    }

    #[test]
    fn a_dead_daemons_socket_is_replaced_under_the_endpoint_lock() {
        let path = test_socket("dead-endpoint");
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let owner = OwnedSocket::bind(&path).unwrap();
        assert!(UnixStream::connect(&path).is_ok());
        drop(owner);
        assert!(!path.exists());
        // best-effort: test cleanup of a temp path.
        fs::remove_file(endpoint_lock_path(&path)).ok();
    }

    #[test]
    fn a_locked_endpoint_is_never_unlinked() {
        let path = test_socket("locked-endpoint");
        drop(UnixListener::bind(&path).unwrap());
        let starting_peer = OwnedSocket::acquire_endpoint_lock(&path).unwrap();
        assert!(OwnedSocket::bind(&path).is_err());
        assert!(
            path.exists(),
            "a starting peer's endpoint path must survive"
        );
        drop(starting_peer);
        fs::remove_file(&path).unwrap();
        // best-effort: test cleanup of a temp path.
        fs::remove_file(endpoint_lock_path(&path)).ok();
    }

    #[test]
    fn a_live_endpoint_without_the_lock_is_not_unlinked() {
        let path = test_socket("live-unlocked");
        let live = UnixListener::bind(&path).unwrap();
        assert!(OwnedSocket::bind(&path).is_err());
        assert!(UnixStream::connect(&path).is_ok());
        drop(live);
        fs::remove_file(&path).unwrap();
        // best-effort: test cleanup of a temp path.
        fs::remove_file(endpoint_lock_path(&path)).ok();
    }

    #[test]
    fn explicit_rejection_survives_a_failed_submission_write() {
        let path = test_socket("reject-before-read");
        let socket = OwnedSocket::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = socket.listener.accept().unwrap();
            let mut kind = [0; 8];
            connection.read_exact(&mut kind).unwrap();
            write_rejected(&mut connection, "daemon rotating").unwrap();
            drop(connection);
            drop(socket);
        });
        let large = OsString::from_vec(vec![b'x'; 8 << 20]);
        let error = execute(&path, &[1; 32], Path::new("/tmp"), &[large]).unwrap_err();
        server.join().unwrap();
        assert!(
            matches!(&error, DaemonError::NotAccepted(message) if message == "daemon rotating"),
            "{error}"
        );
        // best-effort: test cleanup of a temp path.
        fs::remove_file(endpoint_lock_path(&path)).ok();
    }

    #[test]
    fn transport_loss_during_submission_is_indeterminate() {
        let path = test_socket("lost-during-write");
        let socket = OwnedSocket::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = socket.listener.accept().unwrap();
            let mut kind = [0; 8];
            connection.read_exact(&mut kind).unwrap();
            drop(connection);
            drop(socket);
        });
        let large = OsString::from_vec(vec![b'x'; 8 << 20]);
        let error = execute(&path, &[1; 32], Path::new("/tmp"), &[large]).unwrap_err();
        server.join().unwrap();
        assert!(!error.is_not_accepted(), "{error}");
        assert!(!error.was_accepted(), "{error}");
        // best-effort: test cleanup of a temp path.
        fs::remove_file(endpoint_lock_path(&path)).ok();
    }

    #[test]
    fn rotation_explicitly_rejects_queued_requests() {
        let path = test_socket("queued-rotation");
        let socket = OwnedSocket::bind(&path).unwrap();
        let mut client = UnixStream::connect(&path).unwrap();
        client.write_all(REQUEST).unwrap();
        client.write_all(&[3; 32]).unwrap();
        client
            .write_all(&encode_request(Path::new("/tmp"), &["Expr.hs".into()]))
            .unwrap();
        socket.retire().unwrap();
        assert!(!path.exists());
        assert_eq!(read_exact_or_crash(&mut client, 1).unwrap(), [REJECTED]);
        assert_eq!(read_frame(&mut client).unwrap(), b"daemon rotating");
    }

    #[test]
    fn rotation_does_not_wait_forever_for_partial_clients() {
        let path = test_socket("partial-rotation");
        let socket = OwnedSocket::bind(&path).unwrap();
        let mut client = UnixStream::connect(&path).unwrap();
        client.write_all(REQUEST).unwrap();
        let started = Instant::now();
        socket.retire().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(read_exact_or_crash(&mut client, 1).unwrap(), [REJECTED]);
        // best-effort: test cleanup of a temp path.
        fs::remove_file(endpoint_lock_path(&path)).ok();
    }

    #[test]
    fn lost_marker_before_acceptance_is_indeterminate() {
        let path = test_socket("missing-marker");
        let socket = OwnedSocket::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = socket.listener.accept().unwrap();
            let mut header = [0; 40];
            connection.read_exact(&mut header).unwrap();
            read_request(&mut connection).unwrap();
        });
        let error = execute(&path, &[1; 32], Path::new("/tmp"), &["request".into()]).unwrap_err();
        server.join().unwrap();
        assert!(!error.is_not_accepted());
        assert!(!error.was_accepted());
        assert!(matches!(error, DaemonError::Crashed));
    }

    #[test]
    fn request_frames_share_one_allocation_budget() {
        let mut remaining = 3;
        let mut wire = Vec::new();
        push_frame(&mut wire, b"abc");
        push_frame(&mut wire, b"d");
        let mut cursor = Cursor::new(wire);
        assert_eq!(
            read_request_frame(&mut cursor, &mut remaining).unwrap(),
            b"abc"
        );
        assert!(read_request_frame(&mut cursor, &mut remaining).is_err());
        assert_eq!(remaining, 0);
    }

    #[test]
    fn daemon_tracing_fans_out_safe_info_but_keeps_source_debug_in_the_file() {
        let detailed = CapturedWriter::default();
        let pane = CapturedWriter::default();
        let subscriber = tracing_subscriber(
            detailed.clone(),
            pane.clone(),
            CapturedWriter::default(),
            tracing_subscriber::EnvFilter::new("debug"),
        );

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "tidepool_extract_cmd::daemon",
                compile_request = "request-correlation",
                "compiler request visible"
            );
            tracing::debug!(
                target: "tidepool_extract_cmd::daemon",
                source_root = "/sensitive/source",
                "compiler request source"
            );
        });

        let detailed = detailed.text();
        let pane = pane.text();
        assert!(detailed.contains("compiler request visible"));
        assert!(pane.contains("compiler request visible"));
        assert!(detailed.contains("/sensitive/source"));
        assert!(!pane.contains("/sensitive/source"));
        assert!(!detailed.contains('\u{1b}'));
        assert!(!pane.contains('\u{1b}'));
    }

    #[test]
    fn the_client_and_the_daemon_name_one_compile_request_identically() {
        // The client hashes what it is about to send; the daemon hashes what
        // it normalized after reading. For the typed worker form those are
        // the same bytes, which is what lets the two traces join without an
        // id on the wire.
        let cwd = Path::new("/tmp/work");
        let request = ExtractRequest::from_cli(&["Expr.hs".into(), "--turn".into()]).unwrap();
        let client_argv = request.worker_argv();
        let daemon_argv = normalize_worker_argv(client_argv.clone()).unwrap();
        assert_eq!(client_argv, daemon_argv);
        assert_eq!(
            compile_request_correlation(cwd, &client_argv),
            compile_request_correlation(cwd, &daemon_argv)
        );
        let other = ExtractRequest::from_cli(&["Other.hs".into()]).unwrap();
        assert_ne!(
            compile_request_correlation(cwd, &client_argv),
            compile_request_correlation(cwd, &other.worker_argv())
        );
    }

    #[test]
    fn the_daemon_trace_carries_the_compile_request_span_as_json() {
        let trace = CapturedWriter::default();
        let subscriber = tracing_subscriber(
            CapturedWriter::default(),
            CapturedWriter::default(),
            trace.clone(),
            tracing_subscriber::EnvFilter::new("debug"),
        );

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                target: "tidepool_extract_cmd::daemon",
                "compile_request",
                run_id = "run-7",
                compile_request = "abcdef0123456789",
                followed_rotation = true,
                served = 256_u64,
            );
            let _entered = span.enter();
            tracing::info!(target: "tidepool_extract_cmd::daemon", "compiler request started");
        });

        let lines: Vec<serde_json::Value> = trace
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let started = lines
            .iter()
            .find(|line| line["fields"]["message"] == "compiler request started")
            .expect("the daemon trace holds the request event");
        assert_eq!(started["spans"][0]["name"], "compile_request");
        assert_eq!(started["spans"][0]["run_id"], "run-7");
        assert_eq!(started["spans"][0]["compile_request"], "abcdef0123456789");
        assert_eq!(started["spans"][0]["followed_rotation"], true);
        let closed = lines
            .iter()
            .find(|line| line["fields"]["message"] == "close")
            .expect("the request span closes with its duration");
        assert_eq!(closed["span"]["name"], "compile_request");
        assert!(closed["fields"]["time.busy"].is_string());
    }

    #[test]
    fn compiler_timings_and_structure_are_forwarded_to_the_daemon_trace() {
        let trace = CapturedWriter::default();
        let subscriber = tracing_subscriber(
            CapturedWriter::default(),
            CapturedWriter::default(),
            trace.clone(),
            tracing_subscriber::EnvFilter::new("debug"),
        );

        tracing::subscriber::with_default(subscriber, || {
            log_compile_timing(
                "run-7",
                "abcdef0123456789",
                b"tidepool-timing phase=cycle_modules_wall ms=14700\n\
tidepool-timing-detail parent=prepared_recover phase=lookup ms=5\n\
tidepool-timing-module module=Execute ms=17 interface_ms=4\n\
tidepool-timing-module-detail module=Execute parent=module_interface phase=make_iface ms=4\n\
tidepool-count name=prepared_recover_rounds count=2\n\
tidepool-checked module=Inspect target=False\n\
tidepool-target phase=desugar module=Execute\n",
            );
        });

        let output = trace.text();
        let mut events = output
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap());
        let event = events.next().unwrap();
        assert_eq!(event["fields"]["message"], "compiler timing");
        assert_eq!(event["fields"]["run_id"], "run-7");
        assert_eq!(event["fields"]["compile_request"], "abcdef0123456789");
        assert_eq!(
            event["fields"]["line"],
            "tidepool-timing phase=cycle_modules_wall ms=14700"
        );
        let structural: Vec<_> = events
            .map(|event| event["fields"]["line"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            structural,
            [
                "tidepool-timing-detail parent=prepared_recover phase=lookup ms=5",
                "tidepool-timing-module module=Execute ms=17 interface_ms=4",
                "tidepool-timing-module-detail module=Execute parent=module_interface phase=make_iface ms=4",
                "tidepool-count name=prepared_recover_rounds count=2",
                "tidepool-checked module=Inspect target=False",
                "tidepool-target phase=desugar module=Execute",
            ]
        );
    }

    #[test]
    fn machine_stderr_is_kept_in_daemon_log_but_removed_from_diagnostics() {
        let stderr = b"ghc: panic!\ntidepool-timing phase=load ms=12\n  tidepool-checked module=Foo target=True\ntidepool-dependency-witness nodes=3\nuseful detail\n";
        let diagnostic = String::from_utf8(diagnostic_stderr(stderr)).unwrap();
        assert_eq!(diagnostic, "ghc: panic!\nuseful detail");
        let filtered = String::from_utf8_lossy(stderr);
        assert!(filtered.contains("tidepool-timing"));
        assert!(filtered.contains("tidepool-checked"));
    }

    #[test]
    fn the_daemon_trace_file_sits_beside_the_compiler_log() {
        assert_eq!(
            trace_path(Path::new("/tmp/project/.exomonad/logs/run-1-compiler.log")),
            Path::new("/tmp/project/.exomonad/logs/run-1-compiler.jsonl")
        );
    }

    fn test_socket(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tp-daemon-wire-{name}-{}.sock", std::process::id()))
    }

    #[test]
    fn encode_request_matches_the_documented_wire_shape() {
        let cwd = Path::new("/tmp/work");
        let argv = vec![OsString::from("Expr.hs"), OsString::from("--target")];
        let bytes = encode_request(cwd, &argv);

        // frame(cwd)
        let mut expected = Vec::new();
        expected.extend_from_slice(&9u32.to_le_bytes());
        expected.extend_from_slice(b"/tmp/work");
        // argc
        expected.extend_from_slice(&2u32.to_le_bytes());
        // frame(argv[0])
        expected.extend_from_slice(&7u32.to_le_bytes());
        expected.extend_from_slice(b"Expr.hs");
        // frame(argv[1])
        expected.extend_from_slice(&8u32.to_le_bytes());
        expected.extend_from_slice(b"--target");

        assert_eq!(bytes, expected);
    }

    #[test]
    fn encode_request_round_trips_through_a_hand_rolled_decoder() {
        // Decode with an independent inline parser to pin the documented
        // external wire shape.
        let cwd = Path::new("/a/b c/d");
        let argv = vec![
            OsString::from(""),
            OsString::from("x y"),
            OsString::from("z"),
        ];
        let bytes = encode_request(cwd, &argv);

        let mut cur = Cursor::new(bytes);
        let cwd_len = {
            let mut b = [0u8; 4];
            cur.read_exact(&mut b).unwrap();
            u32::from_le_bytes(b) as usize
        };
        let mut cwd_bytes = vec![0u8; cwd_len];
        cur.read_exact(&mut cwd_bytes).unwrap();
        assert_eq!(path_from_bytes(cwd_bytes), cwd);

        let mut argc_b = [0u8; 4];
        cur.read_exact(&mut argc_b).unwrap();
        let argc = u32::from_le_bytes(argc_b);
        assert_eq!(argc as usize, argv.len());

        let mut decoded_argv = Vec::new();
        for _ in 0..argc {
            let mut len_b = [0u8; 4];
            cur.read_exact(&mut len_b).unwrap();
            let len = u32::from_le_bytes(len_b) as usize;
            let mut s = vec![0u8; len];
            cur.read_exact(&mut s).unwrap();
            decoded_argv.push(OsString::from_vec(s));
        }
        assert_eq!(decoded_argv, argv);
        // The whole buffer was consumed — no trailing bytes.
        assert_eq!(cur.position() as usize, cur.get_ref().len());
    }

    #[test]
    fn compiler_request_correlation_is_stable_and_content_addressed() {
        let argv = [
            OsString::from("--worker-request-v12"),
            OsString::from("payload"),
        ];
        assert_eq!(
            compile_request_correlation(Path::new("/work"), &argv),
            compile_request_correlation(Path::new("/work"), &argv)
        );
        assert_ne!(
            compile_request_correlation(Path::new("/work"), &argv),
            compile_request_correlation(Path::new("/other-work"), &argv)
        );
    }

    #[test]
    fn typed_worker_request_must_be_the_complete_argv() {
        let valid = ExtractRequest::from_cli(&["Expr.hs".into()])
            .unwrap()
            .worker_argv();
        assert_eq!(normalize_worker_argv(valid.clone()).unwrap(), valid);

        let mixed = vec![
            "Expr.hs".into(),
            crate::request::WORKER_REQUEST_FLAG.into(),
            "payload".into(),
        ];
        assert!(normalize_worker_argv(mixed).is_err());
    }

    #[test]
    fn malformed_typed_worker_request_is_rejected() {
        let malformed = vec![
            crate::request::WORKER_REQUEST_FLAG.into(),
            "54505245513031320100000009".into(),
        ];
        assert!(normalize_worker_argv(malformed).is_err());
    }

    fn encode_response_bytes(code: i32, stdout: &[u8], stderr: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&code.to_le_bytes());
        push_frame(&mut buf, stdout);
        push_frame(&mut buf, stderr);
        buf
    }

    #[test]
    fn decode_response_round_trips() {
        let bytes = encode_response_bytes(2, b"out text", b"err text");
        let mut cur = Cursor::new(bytes);
        let (code, out, err) = decode_response(&mut cur).unwrap();
        assert_eq!(code, 2);
        assert_eq!(out, b"out text");
        assert_eq!(err, b"err text");
    }

    #[test]
    fn decode_response_negative_exit_code_round_trips() {
        let bytes = encode_response_bytes(-1, b"", b"");
        let mut cur = Cursor::new(bytes);
        let (code, _, _) = decode_response(&mut cur).unwrap();
        assert_eq!(code, -1);
    }

    #[test]
    fn decode_response_truncated_length_prefix_is_crashed() {
        // Only 2 of the 4 exit-code bytes.
        let bytes = vec![0u8, 1u8];
        let mut cur = Cursor::new(bytes);
        match decode_response(&mut cur) {
            Err(DaemonError::Crashed) => {}
            other => panic!("expected Crashed, got {other:?}"),
        }
    }

    #[test]
    fn decode_response_truncated_frame_body_is_crashed() {
        // Exit code is complete; stdout claims 100 bytes but only 3 follow.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.extend_from_slice(&100u32.to_le_bytes());
        bytes.extend_from_slice(b"abc");
        let mut cur = Cursor::new(bytes);
        match decode_response(&mut cur) {
            Err(DaemonError::Crashed) => {}
            other => panic!("expected Crashed, got {other:?}"),
        }
    }

    #[test]
    fn decode_response_empty_stream_is_crashed() {
        let mut cur = Cursor::new(Vec::<u8>::new());
        match decode_response(&mut cur) {
            Err(DaemonError::Crashed) => {}
            other => panic!("expected Crashed, got {other:?}"),
        }
    }

    #[test]
    fn exit_status_round_trips_0_1_2() {
        for code in [0i32, 1, 2] {
            let status = ExitStatus::from_raw(encode_wait_status(code));
            assert_eq!(status.code(), Some(code), "code {code} did not round-trip");
            assert_eq!(status.success(), code == 0);
        }
    }

    #[test]
    fn exit_status_negative_code_truncates_like_a_real_process() {
        // A real process's exit(-1) is observed as exit code 255 by a
        // waiting shell/parent — encode_wait_status must match that, not
        // invent a different truncation.
        let status = ExitStatus::from_raw(encode_wait_status(-1));
        assert_eq!(status.code(), Some(255));
    }

    #[test]
    fn preflight_returns_immutable_identity_material() {
        use std::os::unix::net::UnixListener;

        let socket = test_socket("preflight");
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&socket).ok();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut request = [0u8; 8];
            connection.read_exact(&mut request).unwrap();
            assert_eq!(&request, PREFLIGHT);
            connection.write_all(PREFLIGHT_RESPONSE).unwrap();
            connection.write_all(&[7; 32]).unwrap();
            connection.write_all(&[9; 32]).unwrap();
        });
        let binding = preflight(&socket).unwrap();
        server.join().unwrap();
        assert_eq!(binding.producer, [7; 32]);
        assert_eq!(binding.epoch, [9; 32]);
        std::fs::remove_file(socket).ok();
    }

    #[test]
    fn transaction_carries_ordered_requests_and_waits_for_close_acknowledgement() {
        let socket = test_socket("transaction");
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&socket).ok();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut header = [0u8; 40];
            connection.read_exact(&mut header).unwrap();
            assert_eq!(&header[..8], TRANSACTION);
            assert_eq!(&header[8..], &[3; 32]);
            connection.write_all(&[ACCEPTED]).unwrap();
            for expected in ["one", "two"] {
                let mut command = [0u8; 1];
                connection.read_exact(&mut command).unwrap();
                assert_eq!(command, [TRANSACTION_REQUEST]);
                let (_, argv) = read_request(&mut connection).unwrap();
                assert_eq!(argv, [OsString::from(expected)]);
                write_response(&mut connection, 0, expected.as_bytes(), b"").unwrap();
            }
            let mut command = [0u8; 1];
            connection.read_exact(&mut command).unwrap();
            assert_eq!(command, [TRANSACTION_END]);
            connection.write_all(&[ACCEPTED]).unwrap();
        });

        let mut transaction = begin_transaction(&socket, &[3; 32]).unwrap();
        for expected in ["one", "two"] {
            let output = execute_transaction_request(
                &mut transaction,
                Path::new("/tmp"),
                &[OsString::from(expected)],
            )
            .unwrap();
            assert_eq!(output.stdout, expected.as_bytes());
        }
        end_transaction(&mut transaction).unwrap();
        server.join().unwrap();
        std::fs::remove_file(socket).ok();
    }

    #[test]
    fn transaction_disconnect_interrupts_an_inflight_worker_request() {
        #[allow(clippy::disallowed_methods, reason = "test fixture: fakes a stuck compiler worker, not a production launch site")]
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut worker = Worker {
            child,
            stdin: Some(stdin),
            stdout,
        };
        let (connection, client) = UnixStream::pair().unwrap();
        drop(client);
        let started = Instant::now();
        let result = worker.request_while_connected(
            &connection,
            Path::new("/tmp"),
            &[OsString::from("request")],
            DEFAULT_REQUEST_DEADLINE,
        );
        assert!(result.is_err());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "disconnect did not interrupt the worker promptly"
        );
        worker.abort();
    }

    #[test]
    fn epoch_rejection_is_known_not_accepted() {
        use std::os::unix::net::UnixListener;

        let socket = test_socket("reject");
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&socket).ok();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut header = [0u8; 40];
            connection.read_exact(&mut header).unwrap();
            assert_eq!(&header[..8], REQUEST);
            write_rejected(&mut connection, "epoch changed").unwrap();
        });
        let error = execute(&socket, &[1; 32], Path::new("/tmp"), &["request".into()]).unwrap_err();
        server.join().unwrap();
        assert!(error.is_not_accepted());
        std::fs::remove_file(socket).ok();
    }

    #[test]
    fn lost_response_after_acceptance_is_indeterminate() {
        use std::os::unix::net::UnixListener;

        let socket = test_socket("accepted-close");
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&socket).ok();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut header = [0u8; 40];
            connection.read_exact(&mut header).unwrap();
            read_request(&mut connection).unwrap();
            connection.write_all(&[ACCEPTED]).unwrap();
        });
        let error = execute(&socket, &[1; 32], Path::new("/tmp"), &["request".into()]).unwrap_err();
        server.join().unwrap();
        assert!(!error.is_not_accepted());
        assert!(error.was_accepted());
        assert!(matches!(
            error,
            DaemonError::AfterAcceptance(inner) if matches!(*inner, DaemonError::Crashed)
        ));
        std::fs::remove_file(socket).ok();
    }

    #[test]
    fn hung_worker_request_is_killed_at_the_deadline_and_the_next_request_uses_a_fresh_worker() {
        // A fake worker, playing the resident GHC worker's stdin/stdout
        // protocol directly (no real GHC): it acks `begin_transaction`
        // (a single `\x01` byte in, a single `\x01` byte back) and then,
        // on its first invocation only, never answers the request that
        // follows — exactly the "GHC worker that never replies" case the
        // request deadline exists for. A sentinel file makes its second
        // invocation (the daemon's replacement worker, spawned after the
        // deadline kills the first) answer immediately instead, so the
        // test can observe the daemon serving a subsequent request from a
        // fresh worker rather than staying wedged. `PreparedWorker::command`
        // execs the selected binary via `/proc/self/fd/N`, which only
        // resolves for a real ELF (a shebang script's interpreter re-opens
        // the path in its own, unrelated fd table) — so the fake worker is
        // a tiny Rust program, compiled once here with `rustc`.
        let dir = std::env::temp_dir().join(format!("tp-request-deadline-{}", std::process::id()));
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let stamp = dir.join("stamp");
        std::fs::write(&stamp, b"boot").unwrap();
        let sentinel = dir.join("spawned-once");
        let argv = vec![OsString::from("Expr.hs")];
        // The daemon normalizes the client's raw argv into the worker's own
        // typed wire form before it ever reaches the worker's stdin; the
        // fake worker's second invocation must drain exactly that many
        // bytes (never reading the frames' contents) so it neither blocks
        // on a partial read nor races its own exit against the daemon's
        // write.
        let worker_argv = normalize_worker_argv(argv.clone()).unwrap();
        let payload_len = encode_request(&dir, &worker_argv).len();
        let source = dir.join("fake_worker.rs");
        std::fs::write(
            &source,
            format!(
                r#"
use std::io::{{Read, Write}};

fn main() {{
    let sentinel = std::path::Path::new(r"{sentinel}");
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut one = [0u8; 1];

    if !sentinel.exists() {{
        std::fs::write(sentinel, b"").unwrap();
        stdin.read_exact(&mut one).unwrap(); // begin_transaction
        stdout.write_all(&[1]).unwrap();
        stdout.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3600));
        return;
    }}

    stdin.read_exact(&mut one).unwrap(); // begin_transaction
    stdout.write_all(&[1]).unwrap();
    stdout.flush().unwrap();

    stdin.read_exact(&mut one).unwrap(); // request prefix
    let mut payload = vec![0u8; {payload_len}];
    stdin.read_exact(&mut payload).unwrap();
    stdout.write_all(&[0u8; 12]).unwrap(); // code=0, empty stdout/stderr frames
    stdout.flush().unwrap();

    stdin.read_exact(&mut one).unwrap(); // end_transaction
    stdout.write_all(&[1]).unwrap();
    stdout.flush().unwrap();
}}
"#,
                sentinel = sentinel.display(),
                payload_len = payload_len,
            ),
        )
        .unwrap();
        let worker_bin = dir.join("fake-worker");
        #[allow(clippy::disallowed_methods, reason = "test fixture: compiles a throwaway fake worker binary, not a production launch site")]
        let rustc = std::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&worker_bin)
            .status()
            .unwrap();
        assert!(rustc.success(), "fake worker failed to compile");

        let prepared = PreparedWorker::for_test(worker_bin).unwrap();
        let config = DaemonConfig {
            socket: socket.clone(),
            rotate_after: None,
            rss_ceiling_mb: None,
            request_deadline_secs: Some(1),
            watch_stamp: Some(stamp.clone()),
            persistent: true,
            run_id: None,
            log_path: None,
        };
        let server = std::thread::spawn(move || crate::daemon::serve(&config, prepared));
        let ready_deadline = Instant::now() + Duration::from_secs(10);
        let binding = loop {
            if let Ok(binding) = preflight(&socket) {
                break binding;
            }
            assert!(
                Instant::now() < ready_deadline,
                "daemon did not become ready"
            );
            #[allow(clippy::disallowed_methods, reason = "test: sync polling loop waiting for the daemon/fake worker, not async code")]
            std::thread::sleep(Duration::from_millis(10));
        };

        let started = Instant::now();
        let error = execute(&socket, &binding.epoch, &dir, &argv).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the deadline did not bound the hung request: {:?}",
            started.elapsed()
        );
        assert!(error.was_accepted(), "{error}");

        // The daemon is still alive under the same boot epoch: the deadline
        // replaced the dead worker in place instead of the daemon exiting.
        let next = preflight(&socket).unwrap();
        assert_eq!(next.epoch, binding.epoch);

        // The replacement worker (second script invocation) serves the next
        // request normally.
        let output = execute(&socket, &binding.epoch, &dir, &argv).unwrap();
        assert_eq!(output.status.code(), Some(0));

        std::fs::write(&stamp, b"changed").unwrap();
        assert!(preflight(&socket).is_err());
        assert_eq!(server.join().unwrap().unwrap(), 0);
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stop_drains_the_in_flight_request_then_exits_and_rejects_a_queued_client() {
        // A fake worker that acks `begin_transaction`, then on a request
        // touches a "started" sentinel (proving the daemon has dispatched
        // the request and its single-threaded accept loop is now blocked
        // waiting on the worker) before sleeping briefly and replying
        // successfully. `STOP` sent during that sleep must let the request
        // finish rather than interrupt it — the accept loop only reads a
        // queued connection once the current one is fully serviced.
        let dir = std::env::temp_dir().join(format!("tp-stop-drain-{}", std::process::id()));
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let stamp = dir.join("stamp");
        std::fs::write(&stamp, b"boot").unwrap();
        let started_sentinel = dir.join("started");
        let argv = vec![OsString::from("Expr.hs")];
        let worker_argv = normalize_worker_argv(argv.clone()).unwrap();
        let payload_len = encode_request(&dir, &worker_argv).len();
        let source = dir.join("fake_worker.rs");
        std::fs::write(
            &source,
            format!(
                r#"
use std::io::{{Read, Write}};

fn main() {{
    let started = std::path::Path::new(r"{started}");
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut one = [0u8; 1];

    stdin.read_exact(&mut one).unwrap(); // begin_transaction
    stdout.write_all(&[1]).unwrap();
    stdout.flush().unwrap();

    stdin.read_exact(&mut one).unwrap(); // request prefix
    let mut payload = vec![0u8; {payload_len}];
    stdin.read_exact(&mut payload).unwrap();

    std::fs::write(started, b"").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));

    stdout.write_all(&[0u8; 12]).unwrap(); // code=0, empty stdout/stderr frames
    stdout.flush().unwrap();

    stdin.read_exact(&mut one).unwrap(); // end_transaction
    stdout.write_all(&[1]).unwrap();
    stdout.flush().unwrap();
}}
"#,
                started = started_sentinel.display(),
                payload_len = payload_len,
            ),
        )
        .unwrap();
        let worker_bin = dir.join("fake-worker");
        #[allow(clippy::disallowed_methods, reason = "test fixture: compiles a throwaway fake worker binary, not a production launch site")]
        let rustc = std::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&worker_bin)
            .status()
            .unwrap();
        assert!(rustc.success(), "fake worker failed to compile");

        let prepared = PreparedWorker::for_test(worker_bin).unwrap();
        let config = DaemonConfig {
            socket: socket.clone(),
            rotate_after: None,
            rss_ceiling_mb: None,
            request_deadline_secs: None,
            watch_stamp: Some(stamp.clone()),
            persistent: true,
            run_id: None,
            log_path: None,
        };
        let server = std::thread::spawn(move || crate::daemon::serve(&config, prepared));
        let ready_deadline = Instant::now() + Duration::from_secs(10);
        let binding = loop {
            if let Ok(binding) = preflight(&socket) {
                break binding;
            }
            assert!(
                Instant::now() < ready_deadline,
                "daemon did not become ready"
            );
            #[allow(clippy::disallowed_methods, reason = "test: sync polling loop waiting for the daemon/fake worker, not async code")]
            std::thread::sleep(Duration::from_millis(10));
        };

        // Submit the in-flight request from its own thread; it blocks on the
        // worker's sleep and is only expected to return once the worker
        // replies.
        let client_socket = socket.clone();
        let client_epoch = binding.epoch;
        let client_dir = dir.clone();
        let client_argv = argv.clone();
        let in_flight = std::thread::spawn(move || {
            execute(&client_socket, &client_epoch, &client_dir, &client_argv)
        });

        // Wait for the fake worker to confirm the daemon has dispatched the
        // request and is now blocked in `request_while_connected` — only
        // then is the accept loop guaranteed to be busy servicing it, so a
        // `STOP` and a second client connected now are guaranteed to queue
        // behind it rather than race it for the next `accept()`.
        let dispatched_deadline = Instant::now() + Duration::from_secs(10);
        while !started_sentinel.exists() {
            assert!(
                Instant::now() < dispatched_deadline,
                "the in-flight request was never dispatched to the worker"
            );
            #[allow(clippy::disallowed_methods, reason = "test: sync polling loop waiting for the daemon/fake worker, not async code")]
            std::thread::sleep(Duration::from_millis(5));
        }

        // Queue `STOP` and then a second, ordinary request behind the
        // in-flight one. Both connects land in the listener's backlog while
        // the accept loop is still blocked servicing the first request; the
        // loop reads them, in order, only once that request completes.
        let mut stop_connection = UnixStream::connect(&socket).unwrap();
        stop_connection.write_all(STOP).unwrap();

        let queued = std::thread::spawn({
            let socket = socket.clone();
            let epoch = binding.epoch;
            let dir = dir.clone();
            move || execute(&socket, &epoch, &dir, &[OsString::from("Expr.hs")])
        });

        // The in-flight request completes successfully, unaffected by the
        // `STOP` queued behind it.
        let in_flight_result = in_flight.join().unwrap();
        assert!(
            matches!(&in_flight_result, Ok(output) if output.status.code() == Some(0)),
            "{in_flight_result:?}"
        );

        // `STOP` acks and the daemon exits its accept loop normally, exactly
        // as it does on a watched-stamp change.
        let mut ack = [0u8; 1];
        stop_connection.read_exact(&mut ack).unwrap();
        assert_eq!(ack, [STOP_ACK]);
        assert_eq!(server.join().unwrap().unwrap(), 0);

        // The queued client, still waiting behind `STOP`, is drained with an
        // explicit rejection — known-unsubmitted, so it is safe to rebind
        // direct — rather than left to time out or observe a crash.
        let queued_result = queued.join().unwrap();
        let error = queued_result.unwrap_err();
        assert!(error.is_not_accepted(), "{error}");

        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn daemon_boot_epoch_contributes_to_endpoint_identity() {
        let a = crate::CompilerIdentity::daemon([3; 32], [4; 32]);
        let b = crate::CompilerIdentity::daemon([3; 32], [5; 32]);
        assert_eq!(a.producer_bytes(), b.producer_bytes());
        assert_ne!(a.as_bytes(), b.as_bytes());
    }
}
