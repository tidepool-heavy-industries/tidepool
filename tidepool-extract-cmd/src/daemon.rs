//! Versioned transport for bound resident compiler endpoints.
//!
//! External wire: little-endian, length-prefixed frames over a Unix domain
//! socket, one request/response per connection. This module owns both ends of
//! that transport; the Haskell worker sees only a private stdin/stdout loop.
//!
//! ```text
//! frame     ::= u32-LE length, then that many raw bytes (UTF-8 text)
//! preflight ::= "TPDPF001"
//! identity  ::= "TPDPI001" producer[32] boot_epoch[32]
//! request   ::= "TPDRQ001" expected_epoch[32]
//!               frame(cwd) u32-LE(argc) frame(argv[0]) .. frame(argv[n-1])
//! decision  ::= accepted:u8 | rejected:u8 frame(reason)
//! response  ::= i32-LE(exit_code) frame(stdout) frame(stderr)
//! ```
//!
//! Connect failure or an explicit rejection proves the request was not
//! accepted and permits rebinding. Once the accepted marker is observed, EOF
//! or any other response failure is indeterminate and must never be replayed.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::UnixStream;
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
const ACCEPTED: u8 = 1;
const REJECTED: u8 = 0;

fn pane_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::new("warn,tidepool_extract_cmd::daemon=info")
}

fn tracing_subscriber<D, P>(
    detailed_writer: D,
    pane_writer: P,
    detailed_filter: tracing_subscriber::EnvFilter,
) -> impl tracing::Subscriber + Send + Sync
where
    D: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    P: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
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
    tracing_subscriber::registry().with(detailed).with(pane)
}

pub(crate) fn init_tracing(config: &DaemonConfig) -> Result<(), FrontendError> {
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
    let detailed_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug"));
    tracing_subscriber(Mutex::new(detailed), io::stderr, detailed_filter)
        .try_init()
        .map_err(|error| {
            FrontendError::Daemon(format!("could not initialize compiler tracing: {error}"))
        })
}

pub(crate) struct DaemonBinding {
    pub(crate) producer: [u8; 32],
    pub(crate) epoch: [u8; 32],
}

/// Failure while attempting one daemon request. The point of failure carries
/// settlement information. Connect, setup, write, and explicit rejection are
/// known-unsubmitted; failures after the acceptance marker are never retried.
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
    stream
        .write_all(&req)
        .map_err(|error| DaemonError::NotAccepted(error.to_string()))?;
    // The canonical server flushes the marker before beginning work. An
    // orderly EOF without a preceding marker therefore proves no acceptance;
    // timeouts/resets stay conservative because they can race delivery.
    let state = match read_exact_or_crash(&mut stream, 1) {
        Ok(state) => state[0],
        Err(DaemonError::Crashed) => {
            return Err(DaemonError::NotAccepted(
                "daemon closed before acceptance".to_owned(),
            ))
        }
        Err(error) => return Err(error),
    };
    match state {
        ACCEPTED => decode_output(&mut stream)
            .map_err(|error| DaemonError::AfterAcceptance(Box::new(error))),
        REJECTED => {
            let message = String::from_utf8_lossy(&read_frame(&mut stream)?).into_owned();
            Err(DaemonError::NotAccepted(message))
        }
        other => Err(DaemonError::Protocol(format!(
            "unknown acceptance marker {other}"
        ))),
    }
}

pub(crate) fn preflight(socket_path: &Path) -> Result<DaemonBinding, DaemonError> {
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
    Ok(DaemonBinding { producer, epoch })
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

pub(crate) fn serve(config: &DaemonConfig, prepared: PreparedWorker) -> Result<u8, FrontendError> {
    let producer = prepared.producer_identity()?;
    let mut worker = Worker::spawn(&prepared)?;
    let epoch = boot_epoch()?;
    if let Some(parent) = config.socket.parent() {
        fs::create_dir_all(parent).map_err(FrontendError::Io)?;
    }
    remove_socket(&config.socket)?;
    let boot_stamp = config
        .watch_stamp
        .as_deref()
        .map(read_optional)
        .transpose()
        .map_err(FrontendError::Io)?;
    let rotate_after = config.rotate_after.unwrap_or(256);
    let rss_ceiling_mb = config.rss_ceiling_mb.unwrap_or(2048);
    let executable = std::env::current_exe().map_err(FrontendError::Io)?;
    let listener =
        std::os::unix::net::UnixListener::bind(&config.socket).map_err(FrontendError::Io)?;
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
            if stamp_changed(config, &boot_stamp)? {
                if &kind == REQUEST {
                    let _ = write_rejected(&mut connection, "watched deployment changed");
                }
                remove_socket(&config.socket)?;
                break;
            }
            if &kind == PREFLIGHT {
                let mut response = Vec::with_capacity(72);
                response.extend_from_slice(PREFLIGHT_RESPONSE);
                response.extend_from_slice(&producer);
                response.extend_from_slice(&epoch);
                let _ = connection.write_all(&response);
                continue;
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
                let _ = write_rejected(&mut connection, "daemon boot epoch changed");
                continue;
            }
            let (cwd, argv) = match read_request(&mut connection) {
                Ok(request) => request,
                Err(_) => {
                    tracing::warn!(run_id, "rejected malformed compiler request");
                    let _ = write_rejected(&mut connection, "invalid compiler request");
                    continue;
                }
            };
            let worker_argv = match normalize_worker_argv(argv) {
                Ok(argv) => argv,
                Err(_) => {
                    tracing::warn!(run_id, "rejected invalid typed compiler request");
                    let _ = write_rejected(&mut connection, "invalid typed worker request");
                    continue;
                }
            };
            // The second stamp check is the acceptance fence. If it passes,
            // the acknowledgement is flushed before work begins; every later
            // transport failure is therefore indeterminate and never replayed.
            if stamp_changed(config, &boot_stamp)? {
                let _ = write_rejected(&mut connection, "watched deployment changed");
                remove_socket(&config.socket)?;
                break;
            }
            if connection.write_all(&[ACCEPTED]).is_err() || connection.flush().is_err() {
                continue;
            }
            let compile_request = compile_request_correlation(&cwd, &worker_argv);
            let started = Instant::now();
            tracing::info!(run_id, %compile_request, "compiler request started");
            tracing::debug!(run_id, %compile_request, source_root = %cwd.display(), "compiler request source");
            let response = worker.request(&cwd, &worker_argv);
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let (code, stdout, stderr) = match response {
                Ok(response) => {
                    tracing::info!(
                        run_id,
                        %compile_request,
                        elapsed_ms,
                        exit_code = response.0,
                        "compiler request finished"
                    );
                    response
                }
                Err(error) => {
                    tracing::error!(
                        run_id,
                        %compile_request,
                        elapsed_ms,
                        %error,
                        "compiler request failed"
                    );
                    return Err(error);
                }
            };
            served += 1;
            let _ = write_response(&mut connection, code, &stdout, &stderr);

            if served >= rotate_after
                || worker_rss_mb(worker.child.id()).unwrap_or(0) > rss_ceiling_mb
            {
                if config.persistent {
                    // Long-lived composition roots keep the protocol endpoint
                    // stable while bounding GHC state. The worker executable
                    // is boot-pinned by `PreparedWorker`, so replacing only
                    // this child does not change the endpoint's producer.
                    worker.shutdown();
                    worker = Worker::spawn(&prepared)?;
                    served = 0;
                } else {
                    remove_socket(&config.socket)?;
                    break;
                }
            }
        }
        Ok(0)
    })();

    drop(listener);
    let _ = fs::remove_file(&config.socket);
    worker.shutdown();
    result
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

fn compile_request_correlation(cwd: &Path, worker_argv: &[OsString]) -> String {
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

fn remove_socket(path: &Path) -> Result<(), FrontendError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(FrontendError::Io(error)),
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
    let cwd = OsString::from_vec(read_request_frame(stream)?).into();
    let count = read_u32(stream).map_err(daemon_frontend_error)?;
    if count > MAX_REQUEST_ARGS {
        return Err(FrontendError::Daemon(format!(
            "daemon request has {count} arguments"
        )));
    }
    let mut argv = Vec::with_capacity(count as usize);
    for _ in 0..count {
        argv.push(OsString::from_vec(read_request_frame(stream)?));
    }
    Ok((cwd, argv))
}

fn read_request_frame(stream: &mut impl Read) -> Result<Vec<u8>, FrontendError> {
    let length = read_u32(stream).map_err(daemon_frontend_error)?;
    if length > MAX_REQUEST_FRAME_BYTES {
        return Err(FrontendError::Daemon(format!(
            "daemon request frame is {length} bytes; maximum is {MAX_REQUEST_FRAME_BYTES}"
        )));
    }
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

pub(crate) struct Worker {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
}

impl Worker {
    pub(crate) fn spawn(prepared: &PreparedWorker) -> Result<Self, FrontendError> {
        let mut command = prepared.command();
        command
            .arg("--worker-loop-v1")
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
        stdin.write_all(&bytes).map_err(FrontendError::Io)?;
        stdin.flush().map_err(FrontendError::Io)?;
        decode_response(&mut self.stdout).map_err(daemon_frontend_error)
    }

    pub(crate) fn shutdown(&mut self) {
        drop(self.stdin.take());
        if self.child.wait().is_err() {
            let _ = self.child.kill();
            let _ = self.child.wait();
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
    fn daemon_tracing_fans_out_safe_info_but_keeps_source_debug_in_the_file() {
        let detailed = CapturedWriter::default();
        let pane = CapturedWriter::default();
        let subscriber = tracing_subscriber(
            detailed.clone(),
            pane.clone(),
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
            OsString::from("--worker-request-v5"),
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
            "54505245513030350100000009".into(),
        ];
        assert!(matches!(
            normalize_worker_argv(malformed),
            Err(FrontendError::WorkerProtocol(
                crate::request::ProtocolError::RetiredFieldTag(9)
            ))
        ));
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
        let _ = std::fs::remove_file(&socket);
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
    fn epoch_rejection_is_known_not_accepted() {
        use std::os::unix::net::UnixListener;

        let socket = test_socket("reject");
        let _ = std::fs::remove_file(&socket);
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
        let _ = std::fs::remove_file(&socket);
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
    fn daemon_boot_epoch_contributes_to_endpoint_identity() {
        let a = crate::CompilerIdentity::daemon([3; 32], [4; 32]);
        let b = crate::CompilerIdentity::daemon([3; 32], [5; 32]);
        assert_eq!(a.producer_bytes(), b.producer_bytes());
        assert_ne!(a.as_bytes(), b.as_bytes());
    }
}
