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
//! identity  ::= "TPDPI003" producer[32] consumed_worker[32] boot_epoch[32]
//! request   ::= "TPDRQ002" expected_epoch[32]
//!               frame(cwd) u32-LE(argc) frame(argv[0]) .. frame(argv[n-1])
//! transaction ::= "TPDTR003" expected_epoch[32]
//!                 (request-tag request)* end-tag
//! stop      ::= "TPDST001"
//! stop_ack  ::= 1u8
//! decision  ::= accepted:u8 admission_id:u64-LE | rejected:u8 frame(reason) | busy:u8
//! close_ack ::= accepted:u8
//! response  ::= i32-LE(exit_code) frame(stdout) frame(stderr)
//! ```
//!
//! `stop` is unauthenticated and carries no epoch: any local caller with
//! socket access may ask the daemon to retire. It acks once, then retires
//! its socket and exits its accept loop exactly as it does on a watched-stamp
//! change. The control ack is sent while accepted worker requests are still
//! running; those requests finish before the endpoint drains its backlog.
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

use crate::frontend::{DaemonConfig, FrontendError, PreparedWorker};
use crate::request::ExecutionGrant;
use crate::{CompileWorkload, ExtractRequest};

/// Bound on the daemon round-trip's I/O (connect itself is local and
/// near-instant over a UNIX domain socket, so this bounds the READ side — a
/// wedged or overloaded daemon must not hang the caller forever). Generous:
/// a COLD resident-session compile can legitimately take several seconds
/// so this is sized well above a cold compile, not
/// tuned to the warm case.
// Accepted requests may wait for a free worker slot. This bounds a genuinely
// wedged daemon without mistaking ordinary queueing for failure.
const IO_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_REQUEST_FRAME_BYTES: u32 = 16 * 1024 * 1024;
// Compiler artifacts are file-backed; only complete diagnostics cross this boundary.
const MAX_RESPONSE_PAYLOAD_BYTES: u32 = 16 * 1024 * 1024;
const MAX_REQUEST_ARGS: u32 = 4096;
const PREFLIGHT: &[u8; 8] = b"TPDPF001";
const PREFLIGHT_RESPONSE: &[u8; 8] = b"TPDPI003";
const REQUEST: &[u8; 8] = b"TPDRQ002";
/// A graceful-stop request: no epoch, no body. The daemon acks with a single
/// byte while accepted requests continue, then retires its socket exactly as
/// it does on a watched-stamp change. Unaccepted backlog clients receive an
/// explicit `REJECTED` and may safely rebind.
const STOP: &[u8; 8] = b"TPDST001";
const STOP_ACK: u8 = 1;
pub(crate) const TRANSACTION: &[u8; 8] = b"TPDTR003";
// Direct v2 exit0 confirms checked worker/scratch close, independently of
// the daemon's admission/BEGIN version and cleanup acknowledgement protocol.
pub(crate) const DIRECT_TRANSACTION: &[u8; 8] = b"TPDTR002";
pub(crate) const TRANSACTION_END: u8 = 0;
pub(crate) const TRANSACTION_REQUEST: u8 = 1;
/// Requests one worker serves before the daemon replaces it. Keep ordinary
/// request-count rotation out of the common session length; the RSS ceiling
/// remains the tighter bound when a worker grows quickly.
const DEFAULT_ROTATE_AFTER: u64 = 1024;
/// A worker replaced by RSS rotation after serving fewer than this many
/// requests never got the chance to pay off its cold-start cost: it logs as
/// memo loss, not ordinary rotation. Deliberately small — a worker that
/// serves a handful of requests before growing past the ceiling is the
/// three-3.2-GiB-slot failure this module's sizing exists to prevent, not
/// routine turnover.
const EARLY_REPLACEMENT_SERVED_THRESHOLD: u64 = 8;
/// Number of concurrent GHC worker slots a `--persistent` daemon runs by
/// default (`--workers`). Each slot is a full `Worker`: its own transaction
/// pinning, request deadline, peer-disconnect kill, and served/RSS rotation.
/// A single accept thread hands each accepted connection to a free slot
/// (`serve_workers`'s bounded channel — never an unbounded per-connection
/// thread); with N slots, up to N compiler-backed test processes stop queuing
/// behind one compiler worker. Native runners declare process concurrency.
///
/// Ordinary (non-`--persistent`) daemon mode ignores `--workers` and always
/// runs one worker: it retires the whole endpoint (not just a slot) the
/// first time any request's rotation bound is reached, which only makes
/// sense for the single short-lived worker that mode was designed around
/// (see `tidepool/extract-cmd/CLAUDE.md`).
const DEFAULT_WORKER_COUNT: usize = 3;
/// Measured RSS of one warm GHC worker: 6.1-6.5 GiB observed, rounded up to
/// one named constant so the worker-count derivation below and its doc
/// comments share the same figure. A per-worker RSS ceiling below this
/// rotates a worker on almost every request — a cold worker never gets the
/// chance to become warm — and discards the module memo the ceiling exists
/// to protect (the production incident this sizing rule fixes: a 21 GiB
/// budget divided by a fixed `DEFAULT_WORKER_COUNT` of 3, on a box where only
/// ~20 GiB was actually available, produced three ~3.2 GiB slots and a
/// worker replaced on almost every request).
const WARM_WORKER_MB: u64 = crate::SESSION_WORKER_RSS_CEILING_MB;
/// Total resident-worker RSS budget a `--persistent` daemon sizes its worker
/// pool from. The worker *count* is derived from this budget, not fixed:
/// `worker_count_from_budget` picks as many `WARM_WORKER_MB` slots as the
/// budget supports, up to `DEFAULT_WORKER_COUNT`, so the ceiling each slot
/// gets (`budget / worker_count`) never drops below the figure a worker
/// needs to actually stay warm. `--workers` and `--rss-ceiling-mb` keep their
/// old meanings and, when given explicitly, override the derived figures.
///
/// This is a CEILING on the default, not a fixed figure: `default_memory_budget_mb`
/// derives the actual default from memory available at daemon start, so two
/// compiler daemons that size themselves independently (this repo's own
/// persistent test daemon and an unrelated caller's, such as one Exomonad
/// run's daemon) do not both assume the whole machine budget is theirs to
/// claim. See that function's doc comment.
///
/// The figure itself comes from measurement on a 31 GiB box that runs
/// nothing else: at the default `DEFAULT_WORKER_COUNT` (3) this is
/// `WARM_WORKER_MB` (7 GiB) per worker, leaving roughly 10 GiB for the
/// concurrent native build issuing those `ghc-heavy` requests
/// alongside the pool. A shared box sizes its worker count down on its own
/// (see `worker_count_from_budget`); passing `--workers 2` remains available
/// for a caller that wants to pin it.
const DEFAULT_MEMORY_BUDGET_MB: u64 = 21 * 1024;
/// Memory reserved out of what's available at daemon start, never claimed by
/// the default worker budget — for the concurrent native build (or
/// whatever else the caller is doing) and for `/proc/meminfo`'s own
/// estimation slop. Matches the roughly 10 GiB the historical fixed budget
/// already left over on its reference 31 GiB box.
const DEFAULT_MEMORY_HEADROOM_MB: u64 = 10 * 1024;
/// Floor for the host sizing estimate. Actual observed capacity remains an
/// independent upper bound; startup rejects a pool that cannot retain even
/// one warm worker, rather than treating this estimate as available memory.
const MINIMUM_MEMORY_BUDGET_MB: u64 = 2 * 1024;
/// Default total RSS budget for this daemon's worker pool: the smaller of
/// the historical fixed ceiling (`DEFAULT_MEMORY_BUDGET_MB`) and what's
/// actually available right now, minus a headroom reserve
/// (`DEFAULT_MEMORY_BUDGET_MB`'s doc comment explains the collision this
/// avoids). A quiet box with ample free memory still gets the historical
/// figure; a box where another compiler daemon (this repo's persistent test
/// daemon, or a prior run's daemon that never exited) already holds RSS sees
/// less of it counted as available and sizes down instead of assuming the
/// whole machine is free.
///
/// Startup and admission additionally constrain this estimate using the
/// resource observer's enclosing cgroup limits and live headroom. Observation
/// adds no cross-process budget registry alongside command-resource admission.
fn default_memory_budget_mb() -> u64 {
    budget_from_available(available_memory_mb())
}

/// Pure sizing rule, split out from the `/proc` read so it can be tested
/// without a real machine's memory state.
fn budget_from_available(available_mb: Option<u64>) -> u64 {
    match available_mb {
        Some(available) => DEFAULT_MEMORY_BUDGET_MB
            .min(available.saturating_sub(DEFAULT_MEMORY_HEADROOM_MB))
            .max(MINIMUM_MEMORY_BUDGET_MB),
        None => DEFAULT_MEMORY_BUDGET_MB,
    }
}

/// Result of sizing a `--persistent` daemon's worker pool from its memory
/// budget: how many workers to run, the per-worker RSS ceiling each gets,
/// and whether that ceiling can actually hold a worker warm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkerSizing {
    workers: usize,
    rss_ceiling_mb: u64,
    /// `false` when even a single worker's ceiling falls below the measured
    /// warm footprint. Startup validates that footprint before worker spawn.
    can_stay_warm: bool,
}

/// Derives worker count and per-worker RSS ceiling from a total memory
/// budget instead of dividing the budget by a fixed worker count: as many
/// `WARM_WORKER_MB` slots as the budget supports, capped at
/// `DEFAULT_WORKER_COUNT`, and never fewer than one. This is what keeps a
/// smaller budget (a box already running other work) from producing several
/// slots too small to hold a warm worker, the failure this function exists
/// to prevent — see `DEFAULT_MEMORY_BUDGET_MB` and `WARM_WORKER_MB`'s doc
/// comments.
fn worker_sizing_from_budget(budget_mb: u64) -> WorkerSizing {
    let workers = (budget_mb / WARM_WORKER_MB).clamp(1, DEFAULT_WORKER_COUNT as u64) as usize;
    let rss_ceiling_mb = budget_mb / workers as u64;
    WorkerSizing {
        workers,
        rss_ceiling_mb,
        can_stay_warm: rss_ceiling_mb >= WARM_WORKER_MB,
    }
}

/// Memory actually available for new allocations right now, from
/// `/proc/meminfo`'s `MemAvailable` — the kernel's own headroom-aware
/// estimate (reclaimable caches counted back in), the same field
/// `exomonad-node`'s `command_resources` admission check reads. `None` when
/// `/proc/meminfo` is unreadable or malformed (non-Linux, a sandboxed
/// environment without `/proc`): callers fall back to the historical fixed
/// budget.
#[cfg(target_os = "linux")]
fn available_memory_mb() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    let kib = text.lines().find_map(|line| {
        line.strip_prefix("MemAvailable:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })?;
    Some(kib / 1024)
}

#[cfg(not(target_os = "linux"))]
fn available_memory_mb() -> Option<u64> {
    None
}
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
/// Capacity is transient and never permits rebinding to a direct worker.
const BUSY: u8 = 2;
const CAPACITY_REFUSAL: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CapacityRefusal {
    PreparationUnsupported,
    MemoryUnavailable,
}

impl CapacityRefusal {
    fn wire_tag(self) -> u8 {
        match self {
            Self::PreparationUnsupported => 0,
            Self::MemoryUnavailable => 1,
        }
    }
    fn read(reader: &mut impl Read) -> Result<Self, DaemonError> {
        match read_exact_or_crash(reader, 1)?[0] {
            0 => Ok(Self::PreparationUnsupported),
            1 => Ok(Self::MemoryUnavailable),
            other => Err(DaemonError::Protocol(format!(
                "unknown resource refusal {other}"
            ))),
        }
    }
}
const BUSY_RETRY_DELAY: Duration = Duration::from_millis(25);
const RAW_COMPILER_DETAIL_TARGET: &str = "tidepool_extract_cmd::daemon::compiler_detail";
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
    tracing_subscriber_with_trace_filter(
        detailed_writer,
        pane_writer,
        trace_writer,
        detailed_filter,
        compiler_trace_filter(std::env::var("RUST_LOG").ok().as_deref()),
    )
}

fn compiler_trace_filter(raw_filter: Option<&str>) -> tracing_subscriber::EnvFilter {
    let mut filter = tracing_subscriber::EnvFilter::new("info,tidepool_extract_cmd=debug");
    // The structured trace intentionally has its own stable defaults. Honor
    // only the standard RUST_LOG directive for this one high-volume target so
    // a matched A/B can disable raw compiler detail without losing request,
    // error, summary, or reuse events.
    for directive in raw_filter
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|directive| {
            directive
                .split_once('=')
                .is_some_and(|(target, _)| target.trim() == RAW_COMPILER_DETAIL_TARGET)
        })
    {
        if let Ok(directive) = directive.parse() {
            filter = filter.add_directive(directive);
        }
    }
    filter
}

fn tracing_subscriber_with_trace_filter<D, P, J>(
    detailed_writer: D,
    pane_writer: P,
    trace_writer: J,
    detailed_filter: tracing_subscriber::EnvFilter,
    trace_filter: tracing_subscriber::EnvFilter,
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
    // The structured sibling of the daemon's text log. Exact request joins
    // use daemon_epoch, admission_id and request_ordinal; compile_request
    // retains the content digest for comparing equivalent inputs.
    let trace = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_ansi(false)
        .with_writer(trace_writer)
        .with_filter(trace_filter);
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

/// The appender retries after backend IO failures. Retained evidence must be a
/// prefix: a failed work row must never be followed by a successful completion.
struct RetainedTraceWriter<W> {
    writer: W,
    failure: Option<io::ErrorKind>,
}

impl<W: Write> Write for RetainedTraceWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(kind) = self.failure {
            return Err(io::Error::new(
                kind,
                "compiler trace writer previously failed",
            ));
        }
        let result = match self.writer.write(bytes) {
            Ok(0) if !bytes.is_empty() => Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "compiler trace writer made no progress",
            )),
            result => result,
        };
        if let Err(error) = &result {
            self.failure = Some(error.kind());
        }
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(kind) = self.failure {
            return Err(io::Error::new(
                kind,
                "compiler trace writer previously failed",
            ));
        }
        let result = self.writer.flush();
        if let Err(error) = &result {
            self.failure = Some(error.kind());
        }
        result
    }
}

fn retained_trace_appender<W: Write + Send + 'static>(
    writer: W,
    buffered_lines_limit: usize,
) -> (
    tracing_appender::non_blocking::NonBlocking,
    tracing_appender::non_blocking::WorkerGuard,
) {
    // The writer thread owns only its file, without application locks or tracing
    // calls. Queue backpressure therefore has no application lock dependency.
    tracing_appender::non_blocking::NonBlockingBuilder::default()
        .buffered_lines_limit(buffered_lines_limit)
        .lossy(false)
        .finish(RetainedTraceWriter {
            writer,
            failure: None,
        })
}

/// The returned guard owns the trace appender's flush thread; the daemon's
/// entry point retains it through final logging. Its bounded shutdown wait is
/// not a durability guarantee: interrupted shutdown leaves incomplete evidence.
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
            let (writer, guard) = retained_trace_appender(
                file,
                tracing_appender::non_blocking::DEFAULT_BUFFERED_LINES_LIMIT,
            );
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DaemonEpoch([u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AdmissionId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RequestOrdinal(u64);

/// One accepted daemon admission. Ordered requests retain this identity even
/// when another slot rotates its worker. The daemon owns admission issuance.
#[derive(Debug)]
pub(crate) struct DaemonTransaction {
    pub(crate) stream: UnixStream,
    epoch: DaemonEpoch,
    admission_id: AdmissionId,
    next_request: RequestOrdinal,
}

#[cfg(test)]
impl DaemonTransaction {
    pub(crate) fn for_test(stream: UnixStream) -> Self {
        Self {
            stream,
            epoch: DaemonEpoch([0; 32]),
            admission_id: AdmissionId(1),
            next_request: RequestOrdinal(1),
        }
    }
}

fn read_admission_id(stream: &mut UnixStream) -> Result<AdmissionId, DaemonError> {
    let bytes: [u8; 8] = read_exact_or_crash(stream, 8)
        .map_err(|error| DaemonError::AfterAcceptance(Box::new(error)))?
        .try_into()
        .expect("fixed admission frame width");
    Ok(AdmissionId(u64::from_le_bytes(bytes)))
}

fn identify_request(
    epoch: DaemonEpoch,
    admission: AdmissionId,
    ordinal: RequestOrdinal,
    cwd: &Path,
    argv: &[OsString],
) {
    tracing::info!(
        target: "tidepool_extract_cmd::endpoint",
        daemon_epoch = %hex(&epoch.0),
        admission_id = admission.0,
        request_ordinal = ordinal.0,
        compile_request = %compile_request_correlation(cwd, argv),
        transport = "daemon",
        "compiler request identified"
    );
}

pub(crate) struct DaemonBinding {
    pub(crate) producer: [u8; 32],
    pub(crate) consumed_worker: [u8; 32],
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
    /// EOF before a complete frame/response arrived. This proves loss of the
    /// response, not the cause of the peer's termination.
    IncompleteResponse,
    /// The daemon rejected the bound epoch or deployment before acknowledging
    /// acceptance. It guarantees this request will not execute.
    NotAccepted(String),
    /// No work was admitted before the caller's admission deadline.
    Busy,
    CapacityRefusal(CapacityRefusal),
    /// The caller stopped waiting; acceptance may have raced cancellation.
    Cancelled,
    /// The daemon acknowledged acceptance before the enclosed response error.
    AfterAcceptance(Box<DaemonError>),
    Protocol(String),
    ResponseTooLarge {
        declared: u64,
        remaining: u64,
    },
}

impl DaemonError {
    #[cfg(test)]
    pub(crate) fn is_not_accepted(&self) -> bool {
        matches!(
            self,
            Self::Connect(_) | Self::NotAccepted(_) | Self::Busy | Self::CapacityRefusal(_)
        )
    }

    pub(crate) fn permits_rebind(&self) -> bool {
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
            DaemonError::IncompleteResponse => write!(f, "compiler response ended before a complete reply"),
            DaemonError::NotAccepted(message) => {
                write!(f, "daemon did not accept request: {message}")
            }
            DaemonError::Busy => {
                write!(f, "compiler daemon remained busy until admission deadline")
            }
            DaemonError::CapacityRefusal(CapacityRefusal::PreparationUnsupported) => write!(f, "compiler daemon has no separate preparation capacity; foreground work remains available"),
            DaemonError::CapacityRefusal(CapacityRefusal::MemoryUnavailable) => write!(f, "compiler daemon cannot admit the retained worker footprint within current memory headroom"),
            DaemonError::Cancelled => write!(f, "compiler daemon admission was cancelled"),
            DaemonError::AfterAcceptance(error) => {
                write!(f, "daemon response failed after acceptance: {error}")
            }
            DaemonError::Protocol(message) => write!(f, "daemon protocol error: {message}"),
            DaemonError::ResponseTooLarge { declared, remaining } => write!(
                f,
                "compiler response frame is {declared} bytes; remaining response payload budget is {remaining}"
            ),
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
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut busy_retries = 0u64;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                busy_retries,
                "compiler daemon admission timed out while busy"
            );
            return Err(DaemonError::Busy);
        }
        match execute_once(socket_path, epoch, cwd, argv, remaining) {
            Err(DaemonError::Busy) => {
                busy_retries = busy_retries.saturating_add(1);
                wait_for_busy(deadline, None)?;
            }
            result => return result,
        }
    }
}

fn execute_once(
    socket_path: &Path,
    epoch: &[u8; 32],
    cwd: &Path,
    argv: &[OsString],
    admission_timeout: Duration,
) -> Result<Output, DaemonError> {
    let admission_started = Instant::now();
    let mut stream = UnixStream::connect(socket_path).map_err(DaemonError::Connect)?;
    stream
        .set_read_timeout(Some(admission_timeout))
        .map_err(|error| DaemonError::NotAccepted(error.to_string()))?;
    stream
        .set_write_timeout(Some(admission_timeout))
        .map_err(|error| DaemonError::NotAccepted(error.to_string()))?;

    let mut req = Vec::new();
    req.extend_from_slice(REQUEST);
    req.extend_from_slice(epoch);
    req.extend_from_slice(&encode_request(cwd, argv));
    if let Err(error) = stream.write_all(&req) {
        // The daemon may reject and close before reading every byte. Only a
        // rejection it already sent proves nonacceptance; any other loss after
        // bytes may have reached it stays indeterminate.
        return Err(match explicit_refusal(&mut stream) {
            Some(refusal) => refusal,
            None => DaemonError::Io(error),
        });
    }
    // A missing marker (including orderly EOF) does not prove the peer did
    // not accept. Only an explicit rejection permits rebinding after submission.
    let state = read_exact_or_crash(&mut stream, 1)?[0];
    if state != BUSY {
        tracing::info!(
            phase = "compiler_request_admission",
            elapsed_ms = u64::try_from(admission_started.elapsed().as_millis()).unwrap_or(u64::MAX),
            accepted = state == ACCEPTED,
            "compiler phase finished"
        );
    }
    match state {
        ACCEPTED => {
            let admission_id = read_admission_id(&mut stream)?;
            identify_request(
                DaemonEpoch(*epoch),
                admission_id,
                RequestOrdinal(1),
                cwd,
                argv,
            );
            stream
                .set_read_timeout(Some(IO_TIMEOUT))
                .map_err(|error| DaemonError::AfterAcceptance(Box::new(DaemonError::Io(error))))?;
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
        BUSY => Err(DaemonError::Busy),
        CAPACITY_REFUSAL => Err(DaemonError::CapacityRefusal(CapacityRefusal::read(
            &mut stream,
        )?)),
        other => Err(DaemonError::Protocol(format!(
            "unknown acceptance marker {other}"
        ))),
    }
}

fn wait_for_busy(
    deadline: Instant,
    cancellation: Option<&crate::CompilerTransactionCancellation>,
) -> Result<(), DaemonError> {
    if cancellation.is_some_and(crate::CompilerTransactionCancellation::is_cancelled) {
        return Err(DaemonError::Cancelled);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(DaemonError::Busy);
    }
    // Synchronous compiler callers have no async runtime to yield through.
    #[allow(
        clippy::disallowed_methods,
        reason = "bounded synchronous daemon admission retry"
    )]
    std::thread::sleep(BUSY_RETRY_DELAY.min(remaining));
    if cancellation.is_some_and(crate::CompilerTransactionCancellation::is_cancelled) {
        return Err(DaemonError::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn begin_transaction(
    socket_path: &Path,
    epoch: &[u8; 32],
) -> Result<DaemonTransaction, DaemonError> {
    begin_transaction_with_cancellation(socket_path, epoch, None)
}

#[cfg(test)]
pub(crate) fn begin_transaction_with_cancellation(
    socket_path: &Path,
    epoch: &[u8; 32],
    cancellation: Option<&crate::CompilerTransactionCancellation>,
) -> Result<DaemonTransaction, DaemonError> {
    begin_transaction_for_workload(
        socket_path,
        epoch,
        CompileWorkload::Foreground,
        cancellation,
    )
}

pub(crate) fn begin_transaction_for_workload(
    socket_path: &Path,
    epoch: &[u8; 32],
    workload: CompileWorkload,
    cancellation: Option<&crate::CompilerTransactionCancellation>,
) -> Result<DaemonTransaction, DaemonError> {
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        if cancellation.is_some_and(crate::CompilerTransactionCancellation::is_cancelled) {
            return Err(DaemonError::Cancelled);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DaemonError::Busy);
        }
        let result = begin_transaction_once(socket_path, epoch, workload, remaining, cancellation);
        match result {
            Err(DaemonError::Busy) => {
                if let Some(cancellation) = cancellation {
                    cancellation.disarm();
                }
                wait_for_busy(deadline, cancellation)?;
            }
            Err(error) => {
                if let Some(cancellation) = cancellation {
                    cancellation.disarm();
                }
                return Err(error);
            }
            Ok(stream) => return Ok(stream),
        }
    }
}

fn begin_transaction_once(
    socket_path: &Path,
    epoch: &[u8; 32],
    workload: CompileWorkload,
    admission_timeout: Duration,
    cancellation: Option<&crate::CompilerTransactionCancellation>,
) -> Result<DaemonTransaction, DaemonError> {
    let mut stream = UnixStream::connect(socket_path).map_err(DaemonError::Connect)?;
    if let Some(cancellation) = cancellation {
        cancellation.arm_daemon(&stream).map_err(DaemonError::Io)?;
    }
    stream
        .set_read_timeout(Some(admission_timeout))
        .map_err(|error| DaemonError::NotAccepted(error.to_string()))?;
    stream
        .set_write_timeout(Some(admission_timeout))
        .map_err(|error| DaemonError::NotAccepted(error.to_string()))?;
    if let Err(error) = stream
        .write_all(TRANSACTION)
        .and_then(|()| stream.write_all(epoch))
        .and_then(|()| stream.write_all(&[workload.wire_tag()]))
        .and_then(|()| stream.flush())
    {
        return Err(explicit_refusal(&mut stream).unwrap_or_else(|| {
            if cancellation.is_some_and(crate::CompilerTransactionCancellation::is_cancelled) {
                DaemonError::Cancelled
            } else {
                DaemonError::Io(error)
            }
        }));
    }
    let state = read_exact_or_crash(&mut stream, 1).map_err(|error| {
        if cancellation.is_some_and(crate::CompilerTransactionCancellation::is_cancelled) {
            DaemonError::Cancelled
        } else {
            error
        }
    })?[0];
    match state {
        ACCEPTED => {
            let admission_id = read_admission_id(&mut stream)?;
            stream
                .set_read_timeout(Some(IO_TIMEOUT))
                .map_err(|error| DaemonError::AfterAcceptance(Box::new(DaemonError::Io(error))))?;
            Ok(DaemonTransaction {
                stream,
                epoch: DaemonEpoch(*epoch),
                admission_id,
                next_request: RequestOrdinal(1),
            })
        }
        REJECTED => {
            let message = String::from_utf8_lossy(&read_frame(&mut stream)?).into_owned();
            Err(DaemonError::NotAccepted(message))
        }
        BUSY => Err(DaemonError::Busy),
        CAPACITY_REFUSAL => Err(DaemonError::CapacityRefusal(CapacityRefusal::read(
            &mut stream,
        )?)),
        other => Err(DaemonError::Protocol(format!(
            "unknown transaction acceptance marker {other}"
        ))),
    }
}

pub(crate) fn execute_transaction_request(
    transaction: &mut DaemonTransaction,
    cwd: &Path,
    argv: &[OsString],
) -> Result<Output, DaemonError> {
    let ordinal = transaction.next_request;
    transaction.next_request = RequestOrdinal(ordinal.0.checked_add(1).ok_or_else(|| {
        DaemonError::AfterAcceptance(Box::new(DaemonError::Protocol(
            "compiler request ordinal exhausted".into(),
        )))
    })?);
    identify_request(
        transaction.epoch,
        transaction.admission_id,
        ordinal,
        cwd,
        argv,
    );
    let stream = &mut transaction.stream;
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

pub(crate) fn end_transaction(transaction: &mut DaemonTransaction) -> Result<(), DaemonError> {
    let stream = &mut transaction.stream;
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

fn explicit_refusal(stream: &mut UnixStream) -> Option<DaemonError> {
    let marker = read_exact_or_crash(stream, 1).ok()?;
    match marker[0] {
        BUSY => Some(DaemonError::Busy),
        CAPACITY_REFUSAL => CapacityRefusal::read(stream)
            .ok()
            .map(DaemonError::CapacityRefusal),
        REJECTED => read_frame(stream)
            .ok()
            .map(|frame| DaemonError::NotAccepted(String::from_utf8_lossy(&frame).into_owned())),
        _ => None,
    }
}

pub(crate) fn preflight(socket_path: &Path) -> Result<DaemonBinding, DaemonError> {
    preflight_until(socket_path, Instant::now() + IO_TIMEOUT)
}

pub(crate) fn preflight_until(
    socket_path: &Path,
    deadline: Instant,
) -> Result<DaemonBinding, DaemonError> {
    let started = Instant::now();
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| {
                DaemonError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "compiler preflight deadline expired",
                ))
            })
    };
    let mut stream = UnixStream::connect(socket_path).map_err(DaemonError::Connect)?;
    stream
        .set_write_timeout(Some(remaining()?))
        .map_err(DaemonError::Io)?;
    stream.write_all(PREFLIGHT).map_err(DaemonError::Io)?;
    let mut reader = DeadlineReader {
        stream: &mut stream,
        deadline,
    };
    let magic = read_exact_or_crash(&mut reader, PREFLIGHT_RESPONSE.len())?;
    if magic != PREFLIGHT_RESPONSE {
        return Err(DaemonError::Protocol(
            "invalid preflight response".to_owned(),
        ));
    }
    let producer: [u8; 32] = read_exact_or_crash(&mut reader, 32)?
        .try_into()
        .map_err(|_| DaemonError::IncompleteResponse)?;
    let consumed_worker: [u8; 32] = read_exact_or_crash(&mut reader, 32)?
        .try_into()
        .map_err(|_| DaemonError::IncompleteResponse)?;
    let epoch: [u8; 32] = read_exact_or_crash(&mut reader, 32)?
        .try_into()
        .map_err(|_| DaemonError::IncompleteResponse)?;
    tracing::info!(
        phase = "compiler_preflight",
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "compiler phase finished"
    );
    Ok(DaemonBinding {
        producer,
        consumed_worker,
        epoch,
    })
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
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(DaemonError::IncompleteResponse),
        Err(e) => Err(DaemonError::Io(e)),
    }
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, DaemonError> {
    let b = read_exact_or_crash(r, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_frame<R: Read>(r: &mut R) -> Result<Vec<u8>, DaemonError> {
    let mut remaining = MAX_RESPONSE_PAYLOAD_BYTES;
    read_response_frame(r, &mut remaining)
}

fn read_response_frame<R: Read>(r: &mut R, remaining: &mut u32) -> Result<Vec<u8>, DaemonError> {
    let length = read_u32(r)?;
    if length > *remaining {
        return Err(DaemonError::ResponseTooLarge {
            declared: u64::from(length),
            remaining: u64::from(*remaining),
        });
    }
    *remaining -= length;
    read_exact_or_crash(r, length as usize)
}

/// Decode the wire's `response` shape from any [`Read`] — a real
/// [`UnixStream`] in production, a plain byte slice in the unit tests below
/// (pinning the codec's round-trip and truncated-input behavior without a
/// real socket).
pub(crate) fn decode_response<R: Read>(r: &mut R) -> Result<(i32, Vec<u8>, Vec<u8>), DaemonError> {
    let code_bytes = read_exact_or_crash(r, 4)?;
    let code = i32::from_le_bytes([code_bytes[0], code_bytes[1], code_bytes[2], code_bytes[3]]);
    let mut remaining = MAX_RESPONSE_PAYLOAD_BYTES;
    let stdout = read_response_frame(r, &mut remaining)?;
    let stderr = read_response_frame(r, &mut remaining)?;
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
    worker_slot: usize,
    prepared: &PreparedWorker,
    config: &DaemonConfig,
    run_id: &str,
    epoch: &[u8; 32],
    queue_wait: Duration,
    admission_id: AdmissionId,
    workload: CompileWorkload,
    grant: ExecutionGrant,
    request_deadline: Duration,
    rotate_after: u64,
    rss_ceiling_mb: u64,
    transaction: bool,
    served: &mut u64,
    followed_rotation: &mut bool,
    early_replacements: &mut u64,
    mut next_request: impl FnMut(&mut UnixStream) -> RequestStep,
) -> Result<ConnectionOutcome, FrontendError> {
    let compiler_workload = match workload {
        CompileWorkload::Foreground => "foreground",
        CompileWorkload::Preparation => "preparation",
    };
    tracing::info!(
        run_id,
        daemon_pid = std::process::id(),
        daemon_epoch = %hex(epoch),
        worker_pid = worker.child.id(),
        worker_slot,
        admission_id = admission_id.0,
        compiler_workload,
        compiler_jobs = grant.jobs,
        compiler_capabilities = grant.capabilities,
        transaction,
        queue_ms = u64::try_from(queue_wait.as_millis()).unwrap_or(u64::MAX),
        phase = "compiler_queue",
        "compiler job dequeued"
    );
    let mut transaction_failed = worker
        .begin_transaction_while_connected(&connection, request_deadline)
        .err();
    let mut orderly_end = false;
    let mut one_shot_response = None;
    let mut request_ordinal = RequestOrdinal(0);
    while transaction_failed.is_none() {
        match next_request(&mut connection) {
            RequestStep::End => {
                orderly_end = true;
                break;
            }
            RequestStep::Malformed => break,
            RequestStep::Request(cwd, argv) => {
                request_ordinal =
                    RequestOrdinal(request_ordinal.0.checked_add(1).ok_or_else(|| {
                        FrontendError::Daemon("compiler request ordinal exhausted".into())
                    })?);
                let compile_request = compile_request_correlation(&cwd, &argv);
                let request_mode = ExtractRequest::decode_worker_argv(&argv)
                    .ok()
                    .map(|request| request.mode());
                let request_span = tracing::info_span!(
                    "compile_request",
                    run_id,
                    %compile_request,
                    execution_layer = "physical",
                    physical_execution = %format!("{}:{}:{}", hex(epoch), admission_id.0, request_ordinal.0),
                    request_mode = request_mode.map(|mode| mode.to_string()).as_deref(),
                    admission_id = admission_id.0,
                    compiler_workload,
                    compiler_jobs = grant.jobs,
                    compiler_capabilities = grant.capabilities,
                    request_ordinal = request_ordinal.0,
                    followed_rotation = *followed_rotation,
                    served = *served,
                    transaction,
                    worker = worker_slot,
                    worker_pid = worker.child.id(),
                    daemon_pid = std::process::id(),
                    daemon_epoch = %hex(epoch),
                    transport = "daemon",
                );
                let _entered = request_span.enter();
                *followed_rotation = false;
                let started = Instant::now();
                tracing::info!(run_id, %compile_request, "compiler request started");
                tracing::debug!(run_id, %compile_request, source_root = %cwd.display(), "compiler request source");
                match worker.request_while_connected(&connection, &cwd, &argv, request_deadline) {
                    Ok((code, stdout, stderr)) => {
                        *served += 1;
                        // Keep worker service separate from host-side parsing
                        // and trace formatting: the first duration stops when
                        // the complete worker response arrives.
                        let service_elapsed_ns = started.elapsed().as_nanos();
                        let worker_rss_mb = worker_rss_mb_logged(run_id, worker.child.id());
                        tracing::info!(
                            run_id,
                            %compile_request,
                            elapsed_ns = u64::try_from(service_elapsed_ns).unwrap_or(u64::MAX),
                            elapsed_ms = u64::try_from(service_elapsed_ns / 1_000_000)
                                .unwrap_or(u64::MAX),
                            phase = "compiler_service",
                            exit_code = code,
                            worker_rss_mb,
                            stdout_bytes = stdout.len() as u64,
                            stderr_bytes = stderr.len() as u64,
                            transaction,
                            "compiler request finished"
                        );
                        let diagnostic_started = Instant::now();
                        let stderr_bytes = stderr.len() as u64;
                        log_compile_timing(run_id, &compile_request, &stderr);
                        let stderr = diagnostic_stderr(&stderr);
                        let diagnostic_elapsed_ns = diagnostic_started.elapsed().as_nanos();
                        tracing::info!(
                            run_id,
                            %compile_request,
                            elapsed_ns = u64::try_from(diagnostic_elapsed_ns).unwrap_or(u64::MAX),
                            elapsed_ms = u64::try_from(diagnostic_elapsed_ns / 1_000_000)
                                .unwrap_or(u64::MAX),
                            stderr_bytes,
                            retained_stderr_bytes = stderr.len() as u64,
                            phase = "compiler_diagnostic_processing",
                            "compiler diagnostics processed"
                        );
                        if transaction {
                            if write_response_with_timing(
                                &mut connection,
                                code,
                                &stdout,
                                &stderr,
                                run_id,
                                &compile_request,
                            )
                            .is_err()
                            {
                                break;
                            }
                        } else {
                            // The client may close and release its request inputs as
                            // soon as it receives this response. Keep it waiting
                            // until the worker finishes transaction cleanup.
                            one_shot_response = Some((
                                code,
                                stdout,
                                stderr,
                                compile_request.clone(),
                                request_span.clone(),
                            ));
                        }
                    }
                    Err(error) => {
                        let elapsed_ms =
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                        if matches!(error, FrontendError::WorkerClientDisconnected) {
                            tracing::info!(
                                run_id,
                                %compile_request,
                                elapsed_ms,
                                phase = "compiler_service",
                                %error,
                                transaction,
                                "compiler request abandoned by client"
                            );
                        } else {
                            tracing::error!(
                                run_id,
                                %compile_request,
                                elapsed_ms,
                                phase = "compiler_service",
                                %error,
                                transaction,
                                "compiler request failed"
                            );
                        }
                        transaction_failed = Some(error);
                    }
                }
            }
        }
    }
    if transaction_failed.is_none() {
        transaction_failed = worker
            .end_transaction_while_connected(&connection, request_deadline)
            .err();
    }
    if let Some(error) = transaction_failed {
        if matches!(error, FrontendError::WorkerClientDisconnected) {
            tracing::info!(run_id, %error, transaction, "compiler transaction abandoned by client");
        } else {
            tracing::error!(run_id, %error, transaction, "compiler transaction failed");
        }
        // The accepted request(s) stay indeterminate; do not replay them.
        // Drop the connection before replacing the failed worker.
        drop(connection);
        worker.abort();
        if !config.persistent {
            return Err(error);
        }
        worker.respawn(prepared)?;
        *served = 0;
        *followed_rotation = true;
        return Ok(ConnectionOutcome::Continue);
    }
    if transaction && orderly_end {
        log_send_failure(
            run_id,
            "transaction accepted",
            connection.write_all(&[ACCEPTED]),
        );
        log_send_failure(run_id, "transaction accepted flush", connection.flush());
    }
    if let Some((code, stdout, stderr, compile_request, request_span)) = one_shot_response {
        let _entered = request_span.enter();
        if let Err(error) = write_response_with_timing(
            &mut connection,
            code,
            &stdout,
            &stderr,
            run_id,
            &compile_request,
        ) {
            tracing::debug!(run_id, %error, "compiler response delivery failed after cleanup");
        }
    }
    drop(connection);
    let worker_rss = worker_rss_mb_logged(run_id, worker.child.id());
    let rss_replacement = worker_rss > rss_ceiling_mb;
    if *served >= rotate_after || rss_replacement {
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
        if rss_replacement && *served < EARLY_REPLACEMENT_SERVED_THRESHOLD {
            *early_replacements += 1;
            tracing::warn!(
                run_id,
                served = *served,
                worker_rss_mb = worker_rss,
                rss_ceiling_mb,
                worker_slot,
                early_replacements = *early_replacements,
                "memo loss: worker replaced for RSS after serving only a few requests"
            );
        }
        if config.persistent {
            // Long-lived composition roots keep the protocol endpoint stable
            // while bounding GHC state. The worker executable is boot-pinned
            // by `PreparedWorker`, so replacing only this child does not
            // change the endpoint's producer.
            worker.shutdown();
            worker.respawn(prepared)?;
            *served = 0;
            *followed_rotation = true;
            return Ok(ConnectionOutcome::Continue);
        }
        return Ok(ConnectionOutcome::Retire);
    }
    Ok(ConnectionOutcome::Continue)
}

fn validate_worker_footprint(workers: usize, budget_mb: u64) -> Result<(), FrontendError> {
    let footprint = (workers as u64).saturating_mul(WARM_WORKER_MB);
    if footprint > budget_mb {
        return Err(FrontendError::Daemon(format!(
            "configured compiler pool needs {footprint} MiB of retained worker capacity; actual admitted memory budget is {budget_mb} MiB")));
    }
    Ok(())
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
    let available_mb = available_memory_mb();
    let capacity = crate::resources::capacity();
    let budget_mb = default_memory_budget_mb().min(capacity.memory_mb);
    let sizing = worker_sizing_from_budget(budget_mb);
    // Ordinary (non-`--persistent`) daemon mode always runs a single worker
    // and ignores `--workers` — see `DEFAULT_WORKER_COUNT`'s doc comment for
    // why a worker pool only makes sense for a long-lived persistent
    // endpoint.
    let worker_count = if config.persistent {
        config.workers.unwrap_or(sizing.workers).max(1)
    } else {
        1
    };
    validate_worker_footprint(worker_count, budget_mb)?;
    // `--rss-ceiling-mb` keeps its historical per-worker meaning; only its
    // *default* changes, from a fixed figure divided by worker count to the
    // ceiling `worker_sizing_from_budget` derives alongside that count (see
    // `WARM_WORKER_MB`'s doc comment for why the count, not just the
    // ceiling, is derived from the budget).
    let rss_ceiling_mb = config.rss_ceiling_mb.unwrap_or_else(|| {
        if worker_count == sizing.workers {
            sizing.rss_ceiling_mb
        } else {
            // An explicit `--workers` (or non-persistent's forced count of
            // 1) no longer matches the derived count; recompute the ceiling
            // for the count actually in use.
            budget_mb / worker_count as u64
        }
    });
    let can_stay_warm = rss_ceiling_mb >= WARM_WORKER_MB;
    let request_deadline = config
        .request_deadline_secs
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_REQUEST_DEADLINE);
    let executable = std::env::current_exe().map_err(FrontendError::Io)?;
    let socket = OwnedSocket::bind(&config.socket)?;
    let listener = &socket.listener;
    let run_id = config.run_id.as_deref().unwrap_or("standalone");
    tracing::info!(
        run_id,
        version = env!("CARGO_PKG_VERSION"),
        executable = %executable.display(),
        worker = %prepared.selection().display(),
        producer = %hex(&producer),
        daemon_pid = std::process::id(),
        daemon_epoch = %hex(&epoch),
        socket = %config.socket.display(),
        available_mb = available_mb.unwrap_or(0),
        headroom_mb = DEFAULT_MEMORY_HEADROOM_MB,
        budget_mb,
        workers = worker_count,
        rss_ceiling_mb,
        warm_worker_mb = WARM_WORKER_MB,
        detailed_log = %config
            .log_path
            .as_deref()
            .unwrap_or_else(|| Path::new("disabled"))
            .display(),
        "compiler daemon ready"
    );
    if config.persistent && !can_stay_warm {
        tracing::warn!(
            run_id,
            budget_mb,
            workers = worker_count,
            rss_ceiling_mb,
            warm_worker_mb = WARM_WORKER_MB,
            explicit_workers = config.workers.is_some(),
            "worker sizing cannot keep a worker warm: rss_ceiling_mb is below warm_worker_mb, \
             so a worker rotates before it stays warm"
        );
    }

    let result = serve_workers(
        config,
        &prepared,
        listener,
        run_id,
        &boot_stamp,
        &epoch,
        &producer,
        worker_count,
        rotate_after,
        rss_ceiling_mb,
        request_deadline,
    );

    // Once accepted jobs settle, drain unaccepted clients with an explicit
    // rejection before the endpoint closes.
    let retired = socket.retire();
    drop(socket);
    match (result, retired) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(code), Ok(())) => Ok(code),
    }
}

/// One connection handed from the accept thread to a free worker slot. Only
/// the parts a slot's thread needs to keep servicing: the fence checks
/// (epoch, watched-stamp) and the initial typed-argv decode already happened
/// on the accept thread, exactly as they did before pooling, so a rejection
/// never crosses into a worker thread.
enum Job {
    Transaction(UnixStream),
    Request(UnixStream, std::path::PathBuf, Vec<OsString>),
}

struct PendingJob {
    admission_id: AdmissionId,
    queued_at: Instant,
    job: Job,
    accepted: std::sync::mpsc::Receiver<()>,
    permit: Option<AdmissionPermit>,
    resource_permit: ResourcePermit,
}

struct AdmissionPermit(std::sync::Arc<AtomicBool>);

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Allocation is held from before ACCEPTED through transaction cleanup. The
/// existing worker pool owns its release, including failed acceptance and panic.
#[derive(Default)]
struct ResourceUsage {
    jobs: usize,
    preparation: usize,
    foreground: usize,
    cpus: usize,
    preparation_cpus: usize,
    slots: Vec<usize>,
}

struct ResourceAdmission {
    usage: Mutex<ResourceUsage>,
    worker_rss: Vec<AtomicU64>,
    alive: Vec<AtomicBool>,
    memory_budget_mb: u64,
    foreground_jobs: usize,
    preparation_jobs: usize,
}

struct ResourcePermit {
    owner: std::sync::Arc<ResourceAdmission>,
    workload: CompileWorkload,
    grant: ExecutionGrant,
    slot: usize,
}

impl Drop for ResourcePermit {
    fn drop(&mut self) {
        let mut usage = self.owner.usage.lock().unwrap_or_else(|p| p.into_inner());
        usage.jobs -= 1;
        usage.slots[self.slot] -= 1;
        usage.cpus -= self.grant.capabilities as usize;
        match self.workload {
            CompileWorkload::Foreground => usage.foreground -= 1,
            CompileWorkload::Preparation => {
                usage.preparation -= 1;
                usage.preparation_cpus -= self.grant.capabilities as usize;
            }
        }
    }
}

impl ResourceAdmission {
    #[cfg(test)]
    fn new(workers: usize, memory_budget_mb: u64) -> std::sync::Arc<Self> {
        Self::with_limits(workers, memory_budget_mb, 2, 4)
    }

    fn with_limits(
        workers: usize,
        memory_budget_mb: u64,
        foreground_jobs: usize,
        preparation_jobs: usize,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            usage: Mutex::new(ResourceUsage {
                slots: vec![0; workers],
                ..ResourceUsage::default()
            }),
            worker_rss: (0..workers).map(|_| AtomicU64::new(0)).collect(),
            alive: (0..workers).map(|_| AtomicBool::new(true)).collect(),
            memory_budget_mb,
            foreground_jobs,
            preparation_jobs,
        })
    }

    fn refusal(
        &self,
        workload: CompileWorkload,
        capacity: crate::resources::ResourceCapacity,
    ) -> Option<CapacityRefusal> {
        let live_workers = self
            .alive
            .iter()
            .filter(|alive| alive.load(Ordering::Acquire))
            .count();
        if workload == CompileWorkload::Preparation
            && (live_workers < 2 || capacity.cpus <= capacity.cpus.min(self.foreground_jobs))
        {
            return Some(CapacityRefusal::PreparationUnsupported);
        }
        let resident: u64 = self
            .worker_rss
            .iter()
            .map(|rss| rss.load(Ordering::Acquire))
            .sum();
        let projected: u64 = self
            .worker_rss
            .iter()
            .zip(&self.alive)
            .filter(|(_, alive)| alive.load(Ordering::Acquire))
            .map(|(rss, _)| rss.load(Ordering::Acquire).max(WARM_WORKER_MB))
            .sum();
        if projected > self.memory_budget_mb
            || projected > capacity.memory_mb.saturating_add(resident)
        {
            return Some(CapacityRefusal::MemoryUnavailable);
        }
        None
    }

    fn acquire_with_capacity(
        self: &std::sync::Arc<Self>,
        workload: CompileWorkload,
        capacity: crate::resources::ResourceCapacity,
    ) -> Option<ResourcePermit> {
        let mut usage = self.usage.lock().unwrap_or_else(|p| p.into_inner());
        let workers = self.worker_rss.len();
        let live = |slot: usize| self.alive[slot].load(Ordering::Acquire);
        let reserved_slot = (0..workers).find(|&slot| live(slot))?;
        let live_workers = (0..workers).filter(|&slot| live(slot)).count();
        // Reserve one real worker and its warm footprint even while preparation
        // has spare CPU. A queued foreground job may use the existing pending slot.
        if workload == CompileWorkload::Preparation
            && usage.preparation >= live_workers.saturating_sub(1)
        {
            return None;
        }
        if usage.jobs >= live_workers + 1 {
            return None;
        }
        let slot = match workload {
            CompileWorkload::Foreground => {
                if usage.slots[reserved_slot] == 0 {
                    reserved_slot
                } else if let Some(slot) = (0..workers)
                    .find(|&slot| live(slot) && slot != reserved_slot && usage.slots[slot] == 0)
                {
                    slot
                } else if usage.slots[reserved_slot] == 1 {
                    reserved_slot
                } else {
                    return None;
                }
            }
            CompileWorkload::Preparation => (0..workers)
                .find(|&slot| live(slot) && slot != reserved_slot && usage.slots[slot] == 0)?,
        };
        let resident: u64 = self
            .worker_rss
            .iter()
            .map(|rss| rss.load(Ordering::Acquire))
            .sum();
        let projected: u64 = self
            .worker_rss
            .iter()
            .enumerate()
            .filter(|(slot, _)| live(*slot))
            .map(|(_, rss)| rss.load(Ordering::Acquire).max(WARM_WORKER_MB))
            .sum();
        // Count idle residents as well as active work. Live cgroup remaining
        // includes other workloads/build commitments; RSS rotation is separate.
        if projected > self.memory_budget_mb
            || projected > capacity.memory_mb.saturating_add(resident)
        {
            return None;
        }
        let foreground_cpus = capacity.cpus.min(self.foreground_jobs);
        let mut available = capacity.cpus.saturating_sub(usage.cpus);
        if workload == CompileWorkload::Preparation {
            // Accepted grants cannot change when another transaction finishes.
            // Keep preparation within its own budget even while a foreground
            // worker holds less than the full reserved foreground allowance.
            available = available.min(
                capacity
                    .cpus
                    .saturating_sub(foreground_cpus)
                    .saturating_sub(usage.preparation_cpus),
            );
        }
        let cpus = available.min(match workload {
            CompileWorkload::Foreground => self.foreground_jobs,
            CompileWorkload::Preparation => self.preparation_jobs,
        });
        if cpus == 0 {
            return None;
        }
        let grant = ExecutionGrant {
            jobs: cpus as u32,
            capabilities: cpus as u32,
        };
        usage.jobs += 1;
        usage.slots[slot] += 1;
        usage.cpus += cpus;
        match workload {
            CompileWorkload::Foreground => usage.foreground += 1,
            CompileWorkload::Preparation => {
                usage.preparation += 1;
                usage.preparation_cpus += cpus;
            }
        }
        Some(ResourcePermit {
            owner: std::sync::Arc::clone(self),
            workload,
            grant,
            slot,
        })
    }
}

fn grant_worker_argv(
    argv: &[OsString],
    workload: CompileWorkload,
    grant: ExecutionGrant,
) -> Result<Vec<OsString>, crate::ProtocolError> {
    let mut request = ExtractRequest::decode_worker_argv(argv)?;
    if request.workload() != workload {
        return Err(crate::ProtocolError::InvalidExecutionGrant);
    }
    request.set_execution_grant(grant);
    Ok(request.worker_argv())
}

struct ResidentSlot {
    resources: std::sync::Arc<ResourceAdmission>,
    slot: usize,
}

impl Drop for ResidentSlot {
    fn drop(&mut self) {
        self.resources.alive[self.slot].store(false, Ordering::Release);
        self.resources.worker_rss[self.slot].store(0, Ordering::Release);
    }
}

enum Admission {
    Continue,
    NoWorkers,
}

fn admit_job(
    senders: &[std::sync::mpsc::SyncSender<PendingJob>],
    ordinary_busy: Option<&std::sync::Arc<AtomicBool>>,
    resources: &std::sync::Arc<ResourceAdmission>,
    workload: CompileWorkload,
    job: Job,
    connection: &mut UnixStream,
    next_admission_id: &mut AdmissionId,
) -> Admission {
    admit_job_observed(
        senders,
        ordinary_busy,
        resources,
        workload,
        job,
        connection,
        next_admission_id,
        crate::resources::capacity(),
    )
}

#[allow(clippy::too_many_arguments)]
fn admit_job_observed(
    senders: &[std::sync::mpsc::SyncSender<PendingJob>],
    ordinary_busy: Option<&std::sync::Arc<AtomicBool>>,
    resources: &std::sync::Arc<ResourceAdmission>,
    workload: CompileWorkload,
    job: Job,
    connection: &mut UnixStream,
    next_admission_id: &mut AdmissionId,
    capacity: crate::resources::ResourceCapacity,
) -> Admission {
    if let Some(reason) = resources.refusal(workload, capacity) {
        connection
            .write_all(&[CAPACITY_REFUSAL, reason.wire_tag()])
            .ok();
        return Admission::Continue;
    }
    let permit = if let Some(busy) = ordinary_busy {
        if busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            write_busy(connection).ok();
            return Admission::Continue;
        }
        Some(AdmissionPermit(std::sync::Arc::clone(busy)))
    } else {
        None
    };
    let Some(resource_permit) = resources.acquire_with_capacity(workload, capacity) else {
        if let Job::Request(_, cwd, argv) = &job {
            tracing::debug!(
                compile_request = %compile_request_correlation(cwd, argv),
                compiler_workload = match workload {
                    CompileWorkload::Foreground => "foreground",
                    CompileWorkload::Preparation => "preparation",
                },
                "compiler request waiting for capacity"
            );
        }
        write_busy(connection).ok();
        return Admission::Continue;
    };
    let Some(next) = next_admission_id.0.checked_add(1) else {
        write_rejected(connection, "compiler admission identity exhausted").ok();
        return Admission::NoWorkers;
    };
    let admission_id = AdmissionId(next);
    *next_admission_id = admission_id;
    let (accepted_tx, accepted_rx) = std::sync::mpsc::sync_channel(1);
    tracing::info!(workload = ?workload, worker_slot = resource_permit.slot,
        compiler_jobs = resource_permit.grant.jobs, compiler_capabilities = resource_permit.grant.capabilities,
        "compiler resources reserved");
    match senders[resource_permit.slot].try_send(PendingJob {
        admission_id,
        queued_at: Instant::now(),
        job,
        accepted: accepted_rx,
        permit,
        resource_permit,
    }) {
        Ok(()) => {}
        Err(std::sync::mpsc::TrySendError::Full(_)) => {
            write_busy(connection).ok();
            return Admission::Continue;
        }
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
            write_rejected(connection, "compiler daemon stopping").ok();
            return Admission::NoWorkers;
        }
    }
    if connection
        .write_all(&[ACCEPTED])
        .and_then(|()| connection.write_all(&admission_id.0.to_le_bytes()))
        .and_then(|()| connection.flush())
        .is_ok()
    {
        accepted_tx.send(()).ok();
    }
    Admission::Continue
}

/// One accept thread handles fences and PREFLIGHT/STOP for both ordinary and
/// persistent modes. It reserves capacity with a nonblocking send, then
/// acknowledges acceptance before the worker may start. Ordinary mode has no
/// pending queue; persistent mode allows one pending job. Excess callers get
/// a known-unsubmitted rejection, leaving the control path responsive.
///
/// A persistent slot replaces a rotated worker in place. An ordinary slot
/// signals retirement to the accept thread when its worker reaches a bound.
///
/// STOP and a watched-stamp change are handled the same way: the accept
/// thread stops accepting and drops the sender. Accepted jobs finish or are
/// skipped if their clients disconnected; then the endpoint drains its
/// unaccepted backlog.
#[allow(clippy::too_many_arguments)]
fn serve_workers(
    config: &DaemonConfig,
    prepared: &PreparedWorker,
    listener: &UnixListener,
    run_id: &str,
    boot_stamp: &Option<Option<Vec<u8>>>,
    epoch: &[u8; 32],
    producer: &[u8; 32],
    worker_count: usize,
    rotate_after: u64,
    rss_ceiling_mb: u64,
    request_deadline: Duration,
) -> Result<u8, FrontendError> {
    let consumed_worker = prepared.consumed_worker_identity();
    listener.set_nonblocking(true).map_err(FrontendError::Io)?;
    // The accept owner routes to the existing pool's bounded slot queues.
    // Slot zero retains foreground context and never receives preparation.
    // Resource permits allow at most one pending foreground job globally.
    let (job_txs, job_rxs): (Vec<_>, Vec<_>) = (0..worker_count)
        .map(|_| std::sync::mpsc::sync_channel::<PendingJob>(1))
        .unzip();
    let ordinary_busy = (!config.persistent).then(|| std::sync::Arc::new(AtomicBool::new(false)));
    let retire = AtomicBool::new(false);
    let resources = ResourceAdmission::with_limits(
        worker_count,
        default_memory_budget_mb().min(crate::resources::capacity().memory_mb),
        config.foreground_jobs.unwrap_or(2),
        config.preparation_jobs.unwrap_or(4),
    );
    std::thread::scope(|scope| -> Result<u8, FrontendError> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let mut slots = Vec::with_capacity(worker_count);
        for (slot, job_rx) in job_rxs.into_iter().enumerate() {
            let retire = &retire;
            let ready_tx = ready_tx.clone();
            let resources = std::sync::Arc::clone(&resources);
            slots.push(scope.spawn(move || -> Result<(), FrontendError> {
                let _resident = ResidentSlot { resources: std::sync::Arc::clone(&resources), slot };
                let mut worker = Worker::spawn_in_slot(prepared, epoch, slot)?;
                tracing::info!(
                    run_id,
                    daemon_pid = std::process::id(),
                    daemon_epoch = %hex(epoch),
                    worker_pid = worker.child.id(),
                    worker_slot = slot,
                    "compiler worker ready"
                );
                resources.worker_rss[slot].store(worker_rss_mb(worker.child.id()).unwrap_or(0), Ordering::Release);
                ready_tx.send(()).ok();
                let mut served = 0u64;
                // The first request this slot ever serves is cold, exactly
                // like a freshly rotated single-worker daemon.
                let mut followed_rotation = true;
                // Counts this slot's RSS-driven replacements that lost a
                // fresh worker's memo before it warmed up.
                let mut early_replacements = 0u64;
                loop {
                    let pending = job_rx.recv();
                    let Ok(pending) = pending else {
                        break;
                    };
                    let _permit = pending.permit;
                    let resource_permit = pending.resource_permit;
                    let workload = resource_permit.workload;
                    let grant = resource_permit.grant;
                    // The accept thread reserves the slot before writing the
                    // acceptance marker. A failed write drops this sender and
                    // the worker must not execute that unacknowledged job.
                    if pending.accepted.recv().is_err() {
                        continue;
                    }
                    let job = pending.job;
                    let connection = match &job {
                        Job::Transaction(connection) | Job::Request(connection, ..) => connection,
                    };
                    if peer_disconnected(connection) {
                        continue;
                    }
                    let (connection, transaction, mut first_request) = match job {
                        Job::Transaction(connection) => (connection, true, None),
                        Job::Request(connection, cwd, argv) => {
                            (connection, false, Some((cwd, argv)))
                        }
                    };
                    let outcome = service_transaction(
                        connection,
                        &mut worker,
                        slot,
                        prepared,
                        config,
                        run_id,
                        epoch,
                        pending.queued_at.elapsed(),
                        pending.admission_id,
                        workload,
                        grant,
                        request_deadline,
                        rotate_after,
                        rss_ceiling_mb,
                        transaction,
                        &mut served,
                        &mut followed_rotation,
                        &mut early_replacements,
                        |connection| {
                            if transaction {
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
                                            Ok(argv) => match grant_worker_argv(&argv, workload, grant) {
                                                Ok(argv) => RequestStep::Request(cwd, argv),
                                                Err(_) => RequestStep::Malformed,
                                            },
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
                            } else {
                                match first_request.take() {
                                    Some((cwd, argv)) => match grant_worker_argv(&argv, workload, grant) {
                                        Ok(argv) => RequestStep::Request(cwd, argv),
                                        Err(_) => RequestStep::Malformed,
                                    },
                                    None => RequestStep::End,
                                }
                            }
                        },
                    );
                    resources.worker_rss[slot].store(worker_rss_mb(worker.child.id()).unwrap_or(0), Ordering::Release);
                    drop(resource_permit);
                    match outcome {
                        Ok(ConnectionOutcome::Continue) => {}
                        Ok(ConnectionOutcome::Retire) => {
                            retire.store(true, Ordering::Release);
                            break;
                        }
                        Err(error) => {
                            // The worker itself could not be replaced (e.g. the
                            // replacement process failed to spawn). This slot
                            // cannot keep serving; the remaining slots keep the
                            // daemon alive at reduced capacity rather than
                            // taking the whole endpoint down over one slot.
                            tracing::error!(run_id, slot, %error, "compiler worker slot failed and is retiring");
                            worker.abort();
                            return Err(error);
                        }
                    }
                }
                worker.shutdown();
                Ok(())
            }));
        }
        drop(ready_tx);

        // Do not advertise the endpoint before at least one worker has started.
        let started = ready_rx.recv().is_ok();
        let mut next_admission_id = AdmissionId(0);
        let outcome: Result<(), FrontendError> = 'accept: loop {
            if !started {
                break 'accept Err(FrontendError::Daemon(
                    "all compiler worker slots failed to start".to_owned(),
                ));
            }
            if retire.load(Ordering::Acquire) {
                break 'accept Ok(());
            }
            let (mut connection, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    #[allow(
                        clippy::disallowed_methods,
                        reason = "dedicated synchronous accept thread polls worker retirement"
                    )]
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => break 'accept Err(FrontendError::Io(error)),
            };
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
                match stamp_changed(config, boot_stamp) {
                    Ok(false) => {}
                    Ok(true) => break 'accept Ok(()),
                    Err(error) => break 'accept Err(error),
                }
                let mut response = Vec::with_capacity(104);
                response.extend_from_slice(PREFLIGHT_RESPONSE);
                response.extend_from_slice(producer);
                response.extend_from_slice(&consumed_worker);
                response.extend_from_slice(epoch);
                log_send_failure(
                    run_id,
                    "preflight response",
                    connection.write_all(&response),
                );
                continue;
            }
            if &kind == STOP {
                tracing::info!(run_id, "compiler daemon stopping on request");
                log_send_failure(run_id, "stop ack", connection.write_all(&[STOP_ACK]));
                log_send_failure(run_id, "stop ack flush", connection.flush());
                break 'accept Ok(());
            }
            if &kind == TRANSACTION {
                let expected_epoch = match read_exact_or_crash(&mut connection, 32) {
                    Ok(bytes) => bytes,
                    Err(_) => continue,
                };
                if expected_epoch != *epoch {
                    log_reject_failure(
                        run_id,
                        "stale epoch (transaction)",
                        write_rejected(&mut connection, "daemon boot epoch changed"),
                    );
                    continue;
                }
                match stamp_changed(config, boot_stamp) {
                    Ok(false) => {}
                    Ok(true) => {
                        log_reject_failure(
                            run_id,
                            "deployment changed (transaction)",
                            write_rejected(&mut connection, "watched deployment changed"),
                        );
                        break 'accept Ok(());
                    }
                    Err(error) => {
                        log_reject_failure(
                            run_id,
                            "daemon stopping (transaction)",
                            write_rejected(&mut connection, "daemon stopping"),
                        );
                        break 'accept Err(error);
                    }
                }
                let workload = match read_exact_or_crash(&mut connection, 1)
                    .ok()
                    .and_then(|bytes| CompileWorkload::from_wire(bytes[0]).ok())
                {
                    Some(workload) => workload,
                    None => {
                        write_rejected(&mut connection, "invalid compiler workload").ok();
                        continue;
                    }
                };
                log_send_failure(
                    run_id,
                    "transaction read timeout",
                    connection.set_read_timeout(Some(IO_TIMEOUT)),
                );
                let worker_connection = connection.try_clone().map_err(FrontendError::Io)?;
                if matches!(
                    admit_job(
                        &job_txs,
                        ordinary_busy.as_ref(),
                        &resources,
                        workload,
                        Job::Transaction(worker_connection),
                        &mut connection,
                        &mut next_admission_id,
                    ),
                    Admission::NoWorkers
                ) {
                    break 'accept Ok(());
                }
                continue;
            }
            if &kind != REQUEST {
                continue;
            }
            let expected_epoch = match read_exact_or_crash(&mut connection, 32) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            if expected_epoch != *epoch {
                tracing::warn!(run_id, "rejected compiler request for stale daemon epoch");
                log_reject_failure(
                    run_id,
                    "stale epoch (request)",
                    write_rejected(&mut connection, "daemon boot epoch changed"),
                );
                continue;
            }
            let (cwd, argv) = match read_request(&mut connection) {
                Ok(request) => request,
                Err(_) => {
                    tracing::warn!(run_id, "rejected malformed compiler request");
                    log_reject_failure(
                        run_id,
                        "malformed request",
                        write_rejected(&mut connection, "invalid compiler request"),
                    );
                    continue;
                }
            };
            let worker_argv = match normalize_worker_argv(argv) {
                Ok(argv) => argv,
                Err(_) => {
                    tracing::warn!(run_id, "rejected invalid typed compiler request");
                    log_reject_failure(
                        run_id,
                        "invalid typed request",
                        write_rejected(&mut connection, "invalid typed worker request"),
                    );
                    continue;
                }
            };
            // The second stamp check is the acceptance fence. If it passes,
            // the acknowledgement is flushed before work begins; every later
            // transport failure is therefore indeterminate and never replayed.
            match stamp_changed(config, boot_stamp) {
                Ok(false) => {}
                Ok(true) => {
                    log_reject_failure(
                        run_id,
                        "deployment changed (request)",
                        write_rejected(&mut connection, "watched deployment changed"),
                    );
                    break 'accept Ok(());
                }
                Err(error) => {
                    log_reject_failure(
                        run_id,
                        "daemon stopping (request)",
                        write_rejected(&mut connection, "daemon stopping"),
                    );
                    break 'accept Err(error);
                }
            }
            log_send_failure(
                run_id,
                "request read timeout",
                connection.set_read_timeout(Some(IO_TIMEOUT)),
            );
            let worker_connection = connection.try_clone().map_err(FrontendError::Io)?;
            if matches!(
                admit_job(
                    &job_txs,
                    ordinary_busy.as_ref(),
                    &resources,
                    ExtractRequest::decode_worker_argv(&worker_argv)
                        .expect("normalized request")
                        .workload(),
                    Job::Request(worker_connection, cwd, worker_argv),
                    &mut connection,
                    &mut next_admission_id,
                ),
                Admission::NoWorkers
            ) {
                break 'accept Ok(());
            }
        };

        // Accepted jobs finish before the listener backlog is drained.
        drop(job_txs);
        let mut first_error = outcome.err();
        for slot in slots {
            match slot.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
                Err(_) => {
                    if first_error.is_none() {
                        first_error = Some(FrontendError::Daemon(
                            "compiler worker slot thread panicked".to_owned(),
                        ));
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(0),
        }
    })
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

fn write_response_with_timing(
    stream: impl Write,
    code: i32,
    stdout: &[u8],
    stderr: &[u8],
    run_id: &str,
    compile_request: &str,
) -> Result<(), FrontendError> {
    let started = Instant::now();
    let result = write_response(stream, code, stdout, stderr);
    let elapsed_ns = started.elapsed().as_nanos();
    tracing::info!(
        run_id,
        %compile_request,
        elapsed_ns = u64::try_from(elapsed_ns).unwrap_or(u64::MAX),
        elapsed_ms = u64::try_from(elapsed_ns / 1_000_000).unwrap_or(u64::MAX),
        stdout_bytes = stdout.len() as u64,
        stderr_bytes = stderr.len() as u64,
        delivered = result.is_ok(),
        phase = "compiler_response_handoff",
        "compiler response handoff finished"
    );
    result
}

fn is_raw_compiler_detail(line: &str) -> bool {
    matches!(
        line.split_ascii_whitespace().next(),
        Some("tidepool-timing-detail" | "tidepool-timing-module-detail" | "tidepool-count")
    )
}

fn log_compile_timing(run_id: &str, compile_request: &str, stderr: &[u8]) {
    for line in String::from_utf8_lossy(stderr).lines() {
        let line = line.trim();
        if crate::diagnostics::is_machine_stderr_line(line) {
            if is_raw_compiler_detail(line) {
                tracing::debug!(
                    target: RAW_COMPILER_DETAIL_TARGET,
                    run_id,
                    %compile_request,
                    line,
                    "compiler timing"
                );
            } else {
                tracing::debug!(
                    target: "tidepool_extract_cmd::daemon",
                    run_id,
                    %compile_request,
                    line,
                    "compiler timing"
                );
            }
        }
    }
}

/// The logical request digest shared by the client and daemon compile spans.
/// Equivalent inputs have the same digest; exact executions are identified
/// separately by daemon epoch, admission ID and request ordinal.
/// The daemon replaces the typed execution grant before worker submission.
/// Normalize only that grant for diagnostic correlation; actual worker bytes
/// retain the admitted allowance, reported separately in the physical span.
/// Untyped diagnostic inputs retain their raw digest. This hash grants no
/// authority and is not an artifact or endpoint identity.
pub(crate) fn compile_request_correlation(cwd: &Path, worker_argv: &[OsString]) -> String {
    let logical_argv = ExtractRequest::decode_worker_argv(worker_argv)
        .ok()
        .map(|mut request| {
            request.set_execution_grant(ExecutionGrant::default());
            request.worker_argv()
        });
    let digest = blake3::hash(&encode_request(
        cwd,
        logical_argv.as_deref().unwrap_or(worker_argv),
    ));
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

#[derive(Clone, Copy)]
enum BuildProductsTransport {
    Direct,
    Daemon,
}

/// Mutable output ownership follows the process owner, across reaped rotations.
/// Only the generated slot directories are reclaimed; logical roots and other
/// slots remain intact. An ungraceful frontend/daemon death can leave orphans.
pub(crate) struct BuildProductsNamespace {
    path: std::path::PathBuf,
    transport: BuildProductsTransport,
    directories: std::collections::BTreeSet<std::path::PathBuf>,
    empty_namespaces: std::collections::BTreeSet<std::path::PathBuf>,
}

impl BuildProductsNamespace {
    fn new(path: std::path::PathBuf, transport: BuildProductsTransport) -> Self {
        Self {
            path,
            transport,
            directories: Default::default(),
            empty_namespaces: Default::default(),
        }
    }

    pub(crate) fn direct() -> Result<Self, FrontendError> {
        Ok(Self::new(
            std::path::PathBuf::from(format!("direct-{}", hex(&boot_epoch()?))).join("0"),
            BuildProductsTransport::Direct,
        ))
    }

    pub(crate) fn place(
        &mut self,
        cwd: &Path,
        argv: &[OsString],
    ) -> Result<(Vec<OsString>, Vec<u8>), FrontendError> {
        let mut request = ExtractRequest::decode_worker_argv(argv)?;
        let mut diagnostics = Vec::new();
        for (logical, physical) in request.place_build_products(&self.path) {
            self.directories.insert(if physical.is_absolute() {
                physical.clone()
            } else {
                cwd.join(&physical)
            });
            if matches!(self.transport, BuildProductsTransport::Direct) {
                let line = format!(
                    "tidepool-build-products logical_root={logical:?} physical_dir={physical:?}\n"
                );
                diagnostics.extend_from_slice(line.as_bytes());
            }
        }
        Ok((request.worker_argv(), diagnostics))
    }

    /// Called only after the last owning child was reaped. Failed paths stay
    /// with this owner; retries never remove another slot's live products.
    pub(crate) fn cleanup_checked(
        &mut self,
    ) -> Result<(), Vec<crate::frontend::ScratchCleanupFailure>> {
        use crate::frontend::{ScratchCleanupFailure, ScratchCleanupPhase};
        let mut failures = Vec::new();
        for directory in self.directories.clone() {
            match fs::remove_dir_all(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    failures.push(ScratchCleanupFailure {
                        path: directory,
                        phase: ScratchCleanupPhase::Products,
                        source,
                    });
                    continue;
                }
            }
            self.directories.remove(&directory);
            if let Some(parent) = directory.parent() {
                self.empty_namespaces.insert(parent.to_owned());
            }
        }
        for namespace in self.empty_namespaces.clone() {
            // A sibling may still own this shared epoch directory. Only remove
            // it if empty, including on retries after a prior IO failure.
            match fs::remove_dir(&namespace) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(source) => {
                    failures.push(ScratchCleanupFailure {
                        path: namespace,
                        phase: ScratchCleanupPhase::EmptyNamespace,
                        source,
                    });
                    continue;
                }
            }
            self.empty_namespaces.remove(&namespace);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }

    pub(crate) fn cleanup(&mut self) {
        if let Err(failures) = self.cleanup_checked() {
            for failure in failures {
                tracing::warn!(path = %failure.path.display(), phase = ?failure.phase, error = %failure.source, "compiler scratch cleanup remains unconfirmed");
            }
        }
    }
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
                "compiler transport deadline",
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
    check_response_size(stdout.len() as u64 + stderr.len() as u64)?;
    let mut response = code.to_le_bytes().to_vec();
    push_frame(&mut response, stdout);
    push_frame(&mut response, stderr);
    stream.write_all(&response).map_err(FrontendError::Io)
}

fn check_response_size(total: u64) -> Result<(), FrontendError> {
    if total > u64::from(MAX_RESPONSE_PAYLOAD_BYTES) {
        return Err(daemon_frontend_error(DaemonError::ResponseTooLarge {
            declared: total,
            remaining: u64::from(MAX_RESPONSE_PAYLOAD_BYTES),
        }));
    }
    Ok(())
}

fn write_rejected(stream: &mut impl Write, message: &str) -> Result<(), FrontendError> {
    let mut response = vec![REJECTED];
    push_frame(&mut response, message.as_bytes());
    stream.write_all(&response).map_err(FrontendError::Io)?;
    stream.flush().map_err(FrontendError::Io)
}

fn write_busy(stream: &mut impl Write) -> Result<(), FrontendError> {
    stream.write_all(&[BUSY]).map_err(FrontendError::Io)?;
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
    const POLLERR: std::os::raw::c_short = 0x8;
    const POLLHUP: std::os::raw::c_short = 0x10;
    const POLLRDHUP: std::os::raw::c_short = 0x2000;
    #[repr(C)]
    struct PollFd {
        fd: std::os::raw::c_int,
        events: std::os::raw::c_short,
        revents: std::os::raw::c_short,
    }
    unsafe extern "C" {
        fn poll(
            fds: *mut PollFd,
            count: std::os::raw::c_ulong,
            timeout: std::os::raw::c_int,
        ) -> std::os::raw::c_int;
    }
    let mut descriptor = PollFd {
        fd: stream.as_raw_fd(),
        events: POLLRDHUP,
        revents: 0,
    };
    // SAFETY: the descriptor points to one initialized pollfd and the stream
    // retains ownership of its live descriptor throughout this nonblocking call.
    // Read-half closure must remain observable behind queued request bytes.
    let ready = unsafe { poll(&mut descriptor, 1, 0) };
    if ready < 0 {
        io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
    } else {
        descriptor.revents & (POLLRDHUP | POLLHUP | POLLERR) != 0
    }
}

#[cfg(not(target_os = "linux"))]
fn peer_disconnected(_stream: &UnixStream) -> bool {
    false
}

#[derive(Clone, Copy, Debug)]
enum WorkerOperation {
    BeginTransaction,
    Request,
    EndTransaction,
}

pub(crate) struct Worker {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    build_products_namespace: BuildProductsNamespace,
}

impl Worker {
    pub(crate) fn spawn(prepared: &PreparedWorker) -> Result<Self, FrontendError> {
        Self::spawn_in_namespace(prepared, BuildProductsNamespace::direct()?)
    }

    fn spawn_in_slot(
        prepared: &PreparedWorker,
        epoch: &[u8; 32],
        slot: usize,
    ) -> Result<Self, FrontendError> {
        let namespace =
            std::path::PathBuf::from(format!("daemon-{}", hex(epoch))).join(slot.to_string());
        Self::spawn_in_namespace(
            prepared,
            BuildProductsNamespace::new(namespace, BuildProductsTransport::Daemon),
        )
    }

    fn spawn_in_namespace(
        prepared: &PreparedWorker,
        build_products_namespace: BuildProductsNamespace,
    ) -> Result<Self, FrontendError> {
        let mut command = prepared.command()?;
        command.arg("--worker-loop-v2");
        if matches!(
            build_products_namespace.transport,
            BuildProductsTransport::Daemon
        ) {
            // Idle major GC can occupy the retained heap when the next request
            // arrives. Allocation-driven GC and the daemon's memory limits
            // still govern this worker; finite direct compilers keep defaults.
            command.args(["+RTS", "-I0", "-RTS"]);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
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
            build_products_namespace,
        })
    }

    /// A slot may retain its warm disk products only after its former child
    /// has been reaped. Never start two writers in the same namespace.
    fn respawn(&mut self, prepared: &PreparedWorker) -> Result<(), FrontendError> {
        if self.child.try_wait().map_err(FrontendError::Io)?.is_none() {
            return Err(FrontendError::Daemon(
                "cannot reuse compiler build products before worker is reaped".to_owned(),
            ));
        }
        let mut replacement = Self::spawn_in_namespace(
            prepared,
            BuildProductsNamespace::new(
                self.build_products_namespace.path.clone(),
                self.build_products_namespace.transport,
            ),
        )?;
        replacement.build_products_namespace.directories =
            std::mem::take(&mut self.build_products_namespace.directories);
        replacement.build_products_namespace.empty_namespaces =
            std::mem::take(&mut self.build_products_namespace.empty_namespaces);
        *self = replacement;
        Ok(())
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
        let (worker_argv, placement_diagnostics) =
            self.build_products_namespace.place(cwd, argv)?;
        let bytes = encode_request(cwd, &worker_argv);
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| FrontendError::Daemon("worker stdin is closed".to_owned()))?;
        stdin
            .write_all(&[TRANSACTION_REQUEST])
            .map_err(FrontendError::Io)?;
        stdin.write_all(&bytes).map_err(FrontendError::Io)?;
        stdin.flush().map_err(FrontendError::Io)?;
        let (code, stdout, stderr) =
            decode_response(&mut self.stdout).map_err(daemon_frontend_error)?;
        if placement_diagnostics.is_empty() {
            return Ok((code, stdout, stderr));
        }
        check_response_size(
            stdout.len() as u64 + stderr.len() as u64 + placement_diagnostics.len() as u64,
        )?;
        let mut combined_stderr = placement_diagnostics;
        combined_stderr.extend_from_slice(&stderr);
        Ok((code, stdout, combined_stderr))
    }

    fn begin_transaction_while_connected(
        &mut self,
        connection: &UnixStream,
        deadline: Duration,
    ) -> Result<(), FrontendError> {
        self.operation_while_connected(
            connection,
            deadline,
            WorkerOperation::BeginTransaction,
            Self::begin_transaction,
        )
    }

    fn request_while_connected(
        &mut self,
        connection: &UnixStream,
        cwd: &Path,
        argv: &[OsString],
        deadline: Duration,
    ) -> Result<WorkerResponse, FrontendError> {
        let request_span = tracing::Span::current();
        self.operation_while_connected(connection, deadline, WorkerOperation::Request, |worker| {
            let _entered = request_span.enter();
            worker.request(cwd, argv)
        })
    }

    fn end_transaction_while_connected(
        &mut self,
        connection: &UnixStream,
        deadline: Duration,
    ) -> Result<(), FrontendError> {
        self.operation_while_connected(
            connection,
            deadline,
            WorkerOperation::EndTransaction,
            Self::end_transaction,
        )
    }

    /// Each worker operation has its own deadline and peer-disconnect monitor.
    /// Deadline expiry or disconnect kills the worker and joins its blocked IO
    /// before the transaction owner replaces it. Other failures also settle
    /// through that owner's quarantine path. Waiting between client
    /// requests remains governed by the connection's existing idle policy.
    fn operation_while_connected<T: Send>(
        &mut self,
        connection: &UnixStream,
        deadline: Duration,
        operation: WorkerOperation,
        action: impl FnOnce(&mut Self) -> Result<T, FrontendError> + Send,
    ) -> Result<T, FrontendError> {
        let pid = self.child.id();
        let started = Instant::now();
        let result = std::thread::scope(|scope| {
            let (completed_tx, completed_rx) = std::sync::mpsc::sync_channel(1);
            scope.spawn(move || {
                // best-effort: the receiver may already have returned via the
                // deadline or disconnect branch below and dropped its end.
                completed_tx.send(action(self)).ok();
            });
            loop {
                match completed_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(result) => {
                        // A cancelled client can release request-local inputs
                        // before the worker's reply reaches this monitor.
                        // That reply does not make the abandoned request safe
                        // to reuse or classify as a normal completion.
                        if peer_disconnected(connection) {
                            if let Err(error) = crate::process::kill_process(pid) {
                                tracing::warn!(pid, %error, "failed to kill compiler worker after client disconnect");
                            }
                            return Err(FrontendError::WorkerClientDisconnected);
                        }
                        return result;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(FrontendError::Daemon(
                            "compiler worker operation monitor disconnected".to_owned(),
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
                        ?operation,
                        worker_pid = pid,
                        elapsed_secs = elapsed.as_secs(),
                        deadline_secs = deadline.as_secs(),
                        "compiler worker exceeded its operation deadline; killing it"
                    );
                    // The kill unblocks whatever the worker was doing (a read
                    // or a write) so the monitor thread settles quickly; its
                    // own result is discarded in favor of a deadline error
                    // that names the bound, matching the disconnect branch's
                    // own definite report below.
                    completed_rx.recv().ok();
                    return Err(FrontendError::Daemon(format!(
                        "compiler worker exceeded its {}s operation deadline ({operation:?}) and was killed",
                        deadline.as_secs()
                    )));
                }
                if peer_disconnected(connection) {
                    if let Err(error) = crate::process::kill_process(pid) {
                        tracing::warn!(pid, %error, "failed to kill compiler worker after client disconnect");
                    }
                    // EOF caused by this retirement must not be classified as
                    // an independent worker failure. Drain the monitor before
                    // the slot replaces the pinned worker.
                    completed_rx.recv().ok();
                    return Err(FrontendError::WorkerClientDisconnected);
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

    pub(crate) fn abort(&mut self) {
        drop(self.stdin.take());
        if let Err(error) = self.child.kill() {
            tracing::warn!(%error, "failed to kill aborted compiler worker");
        }
        if let Err(error) = self.child.wait() {
            tracing::warn!(%error, "failed to reap aborted compiler worker");
        }
    }

    /// Direct frontend success must prove both bracket acknowledgement and
    /// retirement of its exact worker, rather than merely requesting shutdown.
    pub(crate) fn shutdown_confirmed(&mut self) -> Result<(), FrontendError> {
        drop(self.stdin.take());
        match self.child.wait() {
            Ok(status) => {
                let scratch = self.build_products_namespace.cleanup_checked();
                if status.success() {
                    scratch.map_err(|failures| FrontendError::ScratchCleanup { status, failures })
                } else {
                    Err(FrontendError::WorkerClose {
                        status,
                        scratch: scratch.err().unwrap_or_default(),
                    })
                }
            }
            Err(source) => {
                // Products may still be in use. Preserve the primary wait
                // failure and keep their existing owner attached to the child.
                self.abort();
                Err(FrontendError::WorkerWait(source))
            }
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

impl Drop for Worker {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            self.abort();
        }
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            self.build_products_namespace.cleanup();
        } else {
            tracing::warn!(
                worker_pid = self.child.id(),
                "retaining compiler scratch products because worker was not reaped"
            );
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
#[path = "response_properties.rs"]
mod response_properties;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_trace_backpressure_preserves_every_row_and_flushes_on_shutdown() {
        use std::sync::{atomic::AtomicUsize, mpsc, Arc};

        struct GatedWriter {
            bytes: Arc<Mutex<Vec<u8>>>,
            entered: Option<mpsc::Sender<()>>,
            release: mpsc::Receiver<()>,
            flushes: Arc<AtomicUsize>,
        }

        impl Write for GatedWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if let Some(entered) = self.entered.take() {
                    entered.send(()).unwrap();
                    self.release
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error))?;
                }
                self.bytes.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                self.flushes.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let bytes = Arc::new(Mutex::new(Vec::new()));
        let flushes = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (mut writer, guard) = retained_trace_appender(
            GatedWriter {
                bytes: Arc::clone(&bytes),
                entered: Some(entered_tx),
                release: release_rx,
                flushes: Arc::clone(&flushes),
            },
            1,
        );
        let errors = writer.error_counter();
        writer.write_all(b"work:first\n").unwrap();
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        // The worker is blocked in the first write and this fills its one slot.
        writer.write_all(b"work:queued\n").unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            writer.write_all(b"complete\n").unwrap();
            // A burst also exercises repeated saturation after the gate opens.
            for _ in 0..256 {
                writer.write_all(b"retained\n").unwrap();
            }
            finished_tx.send(()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            finished_rx.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "the full queue must apply backpressure rather than discard rows"
        );
        release_tx.send(()).unwrap();
        finished_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        producer.join().unwrap();
        // Guard shutdown drains queued rows; NonBlocking::flush itself is a noop.
        drop(guard);
        let mut expected = b"work:first\nwork:queued\ncomplete\n".to_vec();
        for _ in 0..256 {
            expected.extend_from_slice(b"retained\n");
        }
        assert_eq!(*bytes.lock().unwrap(), expected);
        assert!(flushes.load(Ordering::SeqCst) > 0);
        assert_eq!(errors.dropped_lines(), 0);
    }

    #[test]
    fn retained_trace_partial_failure_cannot_resume_with_a_completion() {
        struct PartialFailure {
            bytes: Vec<u8>,
            calls: usize,
        }

        impl Write for PartialFailure {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                match self.calls {
                    1 => {
                        self.bytes.extend_from_slice(&bytes[..2]);
                        Ok(2)
                    }
                    2 => Err(io::Error::other("injected write failure")),
                    _ => {
                        self.bytes.extend_from_slice(bytes);
                        Ok(bytes.len())
                    }
                }
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = RetainedTraceWriter {
            writer: PartialFailure {
                bytes: Vec::new(),
                calls: 0,
            },
            failure: None,
        };
        assert!(writer.write_all(b"work\n").is_err());
        assert!(writer.write_all(b"complete\n").is_err());
        assert!(writer.flush().is_err());
        assert_eq!(writer.writer.bytes, b"wo");
        assert_eq!(writer.writer.calls, 2);
    }

    #[test]
    fn retained_trace_zero_write_cannot_resume_with_a_completion() {
        struct ZeroOnce(bool);

        impl Write for ZeroOnce {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if std::mem::replace(&mut self.0, false) {
                    Ok(0)
                } else {
                    Ok(bytes.len())
                }
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = RetainedTraceWriter {
            writer: ZeroOnce(true),
            failure: None,
        };
        assert_eq!(
            writer.write_all(b"work\n").unwrap_err().kind(),
            io::ErrorKind::WriteZero
        );
        assert!(writer.write_all(b"complete\n").is_err());
    }

    #[test]
    fn retained_trace_flush_failure_cannot_resume_with_a_completion() {
        struct FlushFailure(Vec<u8>);

        impl Write for FlushFailure {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("injected flush failure"))
            }
        }

        let mut writer = RetainedTraceWriter {
            writer: FlushFailure(Vec::new()),
            failure: None,
        };
        writer.write_all(b"work\n").unwrap();
        assert!(writer.flush().is_err());
        assert!(writer.write_all(b"complete\n").is_err());
        assert_eq!(writer.writer.0, b"work\n");
    }

    fn admission_capacity(cpus: usize, memory_mb: u64) -> crate::resources::ResourceCapacity {
        crate::resources::ResourceCapacity { cpus, memory_mb }
    }

    #[test]
    fn serial_configuration_progresses_foreground_and_refuses_preparation_immediately() {
        let resources = ResourceAdmission::new(1, WARM_WORKER_MB);
        let capacity = admission_capacity(1, WARM_WORKER_MB);
        assert_eq!(
            resources.refusal(CompileWorkload::Preparation, capacity),
            Some(CapacityRefusal::PreparationUnsupported)
        );
        assert_eq!(
            resources.refusal(CompileWorkload::Foreground, capacity),
            None
        );
        let foreground = resources
            .acquire_with_capacity(CompileWorkload::Foreground, capacity)
            .unwrap();
        assert_eq!(
            foreground.grant,
            ExecutionGrant {
                jobs: 1,
                capabilities: 1
            }
        );
        drop(foreground);
        assert!(resources
            .acquire_with_capacity(CompileWorkload::Foreground, capacity)
            .is_some());
        let two_workers = ResourceAdmission::new(2, 2 * WARM_WORKER_MB);
        assert_eq!(
            two_workers.refusal(
                CompileWorkload::Preparation,
                admission_capacity(1, 2 * WARM_WORKER_MB)
            ),
            Some(CapacityRefusal::PreparationUnsupported)
        );
        assert!(validate_worker_footprint(1, WARM_WORKER_MB).is_ok());
        assert!(validate_worker_footprint(1, WARM_WORKER_MB - 1).is_err());
        assert!(validate_worker_footprint(3, 2 * WARM_WORKER_MB).is_err());
    }

    #[test]
    fn typed_resource_refusal_is_known_unsubmitted_without_retry_or_rebind() {
        let dir = tempfile::tempdir().unwrap();
        for transaction in [false, true] {
            let socket = dir.path().join(if transaction {
                "transaction.sock"
            } else {
                "request.sock"
            });
            let listener = UnixListener::bind(&socket).unwrap();
            let server = std::thread::spawn(move || {
                let (mut connection, _) = listener.accept().unwrap();
                let mut header = [0; 40];
                connection.read_exact(&mut header).unwrap();
                if transaction {
                    assert_eq!(&header[..8], TRANSACTION);
                    assert_eq!(read_exact_or_crash(&mut connection, 1).unwrap(), [1]);
                } else {
                    assert_eq!(&header[..8], REQUEST);
                    read_request(&mut connection).unwrap();
                }
                connection
                    .write_all(&[
                        CAPACITY_REFUSAL,
                        CapacityRefusal::PreparationUnsupported.wire_tag(),
                    ])
                    .unwrap();
            });
            let error = if transaction {
                begin_transaction_for_workload(
                    &socket,
                    &[7; 32],
                    CompileWorkload::Preparation,
                    None,
                )
                .unwrap_err()
            } else {
                execute(
                    &socket,
                    &[7; 32],
                    Path::new("/tmp"),
                    &ExtractRequest::default().worker_argv(),
                )
                .unwrap_err()
            };
            server.join().unwrap();
            assert!(matches!(
                error,
                DaemonError::CapacityRefusal(CapacityRefusal::PreparationUnsupported)
            ));
            assert!(error.is_not_accepted());
            assert!(!error.permits_rebind());
            assert!(!error.was_accepted());
        }
    }

    #[test]
    fn preparation_reserves_stable_warm_foreground_slot_and_aggregate_cpus() {
        let resources = ResourceAdmission::with_limits(3, 3 * WARM_WORKER_MB, 2, 16);
        let capacity = admission_capacity(32, 3 * WARM_WORKER_MB);
        let first = resources
            .acquire_with_capacity(CompileWorkload::Preparation, capacity)
            .unwrap();
        let second = resources
            .acquire_with_capacity(CompileWorkload::Preparation, capacity)
            .unwrap();
        assert_eq!((first.slot, second.slot), (1, 2));
        assert_eq!((first.grant.jobs, second.grant.jobs), (16, 14));
        assert!(resources
            .acquire_with_capacity(CompileWorkload::Preparation, capacity)
            .is_none());
        let foreground = resources
            .acquire_with_capacity(CompileWorkload::Foreground, capacity)
            .unwrap();
        assert_eq!(foreground.slot, 0);
        assert_eq!(foreground.grant.capabilities, 2);
        assert_eq!(resources.usage.lock().unwrap().cpus, 32);
        drop(foreground);
        assert!(resources
            .acquire_with_capacity(CompileWorkload::Preparation, capacity)
            .is_none());
        let foreground = resources
            .acquire_with_capacity(CompileWorkload::Foreground, capacity)
            .unwrap();
        assert_eq!(foreground.slot, 0);
        drop((first, second, foreground));
        assert_eq!(resources.usage.lock().unwrap().cpus, 0);
    }

    #[test]
    fn queued_foreground_grants_cannot_loan_preparation_the_reserved_cpu_budget() {
        let resources = ResourceAdmission::new(2, 2 * WARM_WORKER_MB);
        let capacity = admission_capacity(5, 2 * WARM_WORKER_MB);
        let first = resources
            .acquire_with_capacity(CompileWorkload::Foreground, capacity)
            .unwrap();
        let spill = resources
            .acquire_with_capacity(CompileWorkload::Foreground, capacity)
            .unwrap();
        let queued = resources
            .acquire_with_capacity(CompileWorkload::Foreground, capacity)
            .unwrap();
        assert_eq!((first.slot, spill.slot, queued.slot), (0, 1, 0));
        assert_eq!(
            (
                first.grant.capabilities,
                spill.grant.capabilities,
                queued.grant.capabilities
            ),
            (2, 2, 1)
        );
        drop((first, spill));

        let preparation = resources
            .acquire_with_capacity(CompileWorkload::Preparation, capacity)
            .unwrap();
        assert_eq!(preparation.slot, 1);
        assert_eq!(
            preparation.grant.capabilities, 3,
            "preparation cannot borrow the CPU missing from an immutable foreground grant"
        );
        drop(queued);
        let foreground = resources
            .acquire_with_capacity(CompileWorkload::Foreground, capacity)
            .unwrap();
        assert_eq!(foreground.slot, 0);
        assert_eq!(
            foreground.grant.capabilities, 2,
            "the full foreground allowance remains available after predecessor cleanup"
        );
        assert_eq!(resources.usage.lock().unwrap().cpus, 5);
        drop((preparation, foreground));
        let usage = resources.usage.lock().unwrap();
        assert_eq!(usage.cpus, 0);
        assert_eq!(usage.preparation_cpus, 0);
        assert_eq!(usage.jobs, 0);
    }

    #[test]
    fn live_memory_accounts_for_idle_residents_and_preserves_real_limits() {
        let resources = ResourceAdmission::new(3, 3 * WARM_WORKER_MB);
        assert!(resources
            .acquire_with_capacity(
                CompileWorkload::Foreground,
                admission_capacity(8, 3 * WARM_WORKER_MB - 1)
            )
            .is_none());
        for rss in &resources.worker_rss {
            rss.store(WARM_WORKER_MB, Ordering::Release);
        }
        let foreground = resources
            .acquire_with_capacity(CompileWorkload::Foreground, admission_capacity(8, 0))
            .unwrap();
        drop(foreground);
        // A retained oversized idle context consumes its own footprint and does
        // not become free memory because no request is currently using it.
        resources.worker_rss[2].store(2 * WARM_WORKER_MB, Ordering::Release);
        assert!(resources
            .acquire_with_capacity(
                CompileWorkload::Preparation,
                admission_capacity(8, 10 * WARM_WORKER_MB)
            )
            .is_none());
        assert!(resources
            .acquire_with_capacity(
                CompileWorkload::Foreground,
                admission_capacity(0, 10 * WARM_WORKER_MB)
            )
            .is_none());
    }

    fn admission_property_config() -> proptest::test_runner::Config {
        let mut config = proptest::test_runner::Config::default();
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(
                proptest::test_runner::FileFailurePersistence::Direct(path),
            ));
        }
        config
    }

    proptest::proptest! {
        #![proptest_config(admission_property_config())]
        #[test]
        fn admission_histories_never_loan_foreground_or_exceed_cpu_capacity(
            cpus in 1usize..=32,
            workers in 1usize..=5,
            foreground_jobs in 1usize..=4,
            preparation_jobs in 1usize..=8,
            history in proptest::collection::vec((0u8..3, 0usize..12), 1..100)
        ) {
            // The oracle recomputes occupancy/CPU from live permits. Shrinking
            // preserves explicit acquisition/release operations and logical handles.
            let resources = ResourceAdmission::with_limits(workers, workers as u64 * WARM_WORKER_MB, foreground_jobs, preparation_jobs);
            let capacity = admission_capacity(cpus, workers as u64 * WARM_WORKER_MB);
            let mut held: Vec<ResourcePermit> = Vec::new();
            for (operation, index) in history {
                match operation {
                    0 if !held.is_empty() => { held.remove(index % held.len()); },
                    1 | 2 => {
                        let workload = if operation == 1 { CompileWorkload::Foreground } else { CompileWorkload::Preparation };
                        if let Some(permit) = resources.acquire_with_capacity(workload, capacity) { held.push(permit); }
                    }
                    _ => {},
                }
                let used: usize = held.iter().map(|permit| permit.grant.capabilities as usize).sum();
                let preparation_cpus: usize = held.iter().filter(|permit| permit.workload == CompileWorkload::Preparation).map(|permit| permit.grant.capabilities as usize).sum();
                proptest::prop_assert!(used <= cpus);
                proptest::prop_assert!(held.iter().filter(|permit| permit.workload == CompileWorkload::Preparation).all(|permit| permit.slot != 0));
                proptest::prop_assert!(preparation_cpus <= cpus.saturating_sub(cpus.min(foreground_jobs)));
                proptest::prop_assert!(held.iter().all(|permit| {
                    let maximum = match permit.workload {
                        CompileWorkload::Foreground => foreground_jobs,
                        CompileWorkload::Preparation => preparation_jobs,
                    };
                    permit.grant.jobs > 0 && permit.grant.jobs as usize <= maximum
                        && permit.grant.jobs == permit.grant.capabilities
                }), "each grant must fit its workload allowance");
                if held.iter().all(|permit| permit.workload == CompileWorkload::Preparation) {
                    let foreground = resources.acquire_with_capacity(CompileWorkload::Foreground, capacity);
                    proptest::prop_assert!(foreground.is_some(), "preparation must preserve foreground progress");
                    let foreground = foreground.unwrap();
                    proptest::prop_assert_eq!(foreground.slot, 0);
                    proptest::prop_assert_eq!(foreground.grant.capabilities as usize, cpus.min(foreground_jobs));
                    drop(foreground);
                }
                let mut slots = vec![0; workers];
                for permit in &held { slots[permit.slot] += 1; }
                let actual = resources.usage.lock().unwrap();
                proptest::prop_assert_eq!(actual.cpus, used);
                proptest::prop_assert_eq!(actual.preparation_cpus, preparation_cpus);
                proptest::prop_assert_eq!(actual.jobs, held.len());
                proptest::prop_assert_eq!(actual.foreground, held.iter().filter(|permit| permit.workload == CompileWorkload::Foreground).count());
                proptest::prop_assert_eq!(actual.preparation, held.iter().filter(|permit| permit.workload == CompileWorkload::Preparation).count());
                proptest::prop_assert_eq!(&actual.slots, &slots);
            }
            drop(held);
            let actual = resources.usage.lock().unwrap();
            proptest::prop_assert_eq!((actual.cpus, actual.preparation_cpus, actual.jobs), (0, 0, 0));
            proptest::prop_assert!(actual.slots.iter().all(|used| *used == 0));
        }
    }

    #[test]
    fn failed_acceptance_releases_slot_and_grant_without_worker_execution() {
        for (workload, workers, slot) in [
            (CompileWorkload::Foreground, 1, 0),
            (CompileWorkload::Preparation, 2, 1),
        ] {
            let memory = workers as u64 * WARM_WORKER_MB;
            let resources = ResourceAdmission::new(workers, memory);
            let capacity = admission_capacity(4, memory);
            let (senders, receivers): (Vec<_>, Vec<_>) = (0..workers)
                .map(|_| std::sync::mpsc::sync_channel(1))
                .unzip();
            let (mut connection, peer) = UnixStream::pair().unwrap();
            let worker_connection = connection.try_clone().unwrap();
            drop(peer);
            let mut identity = AdmissionId(0);
            assert!(matches!(
                admit_job_observed(
                    &senders,
                    None,
                    &resources,
                    workload,
                    Job::Transaction(worker_connection),
                    &mut connection,
                    &mut identity,
                    capacity
                ),
                Admission::Continue
            ));
            let pending = receivers[slot].recv().unwrap();
            assert_eq!(pending.resource_permit.slot, slot);
            assert!(pending.accepted.recv().is_err());
            {
                let usage = resources.usage.lock().unwrap();
                assert_eq!(usage.jobs, 1);
                assert_eq!(
                    usage.cpus,
                    pending.resource_permit.grant.capabilities as usize
                );
                assert_eq!(
                    usage.preparation_cpus,
                    if workload == CompileWorkload::Preparation {
                        usage.cpus
                    } else {
                        0
                    }
                );
            }
            drop(pending);
            {
                let usage = resources.usage.lock().unwrap();
                assert_eq!((usage.jobs, usage.cpus, usage.preparation_cpus), (0, 0, 0));
                assert!(usage.slots.iter().all(|used| *used == 0));
            }
            let foreground = resources
                .acquire_with_capacity(CompileWorkload::Foreground, capacity)
                .unwrap();
            assert_eq!(foreground.grant.capabilities, 2);
        }
    }

    #[test]
    fn worker_receives_owner_grant_and_refuses_transaction_class_switch() {
        let mut request = ExtractRequest::default();
        request.set_workload(CompileWorkload::Preparation);
        request.set_execution_grant(ExecutionGrant {
            jobs: 99,
            capabilities: 99,
        });
        request.input("Source.hs");
        let argv = grant_worker_argv(
            &request.worker_argv(),
            CompileWorkload::Preparation,
            ExecutionGrant {
                jobs: 4,
                capabilities: 4,
            },
        )
        .unwrap();
        let granted = ExtractRequest::decode_worker_argv(&argv).unwrap();
        assert_eq!(
            granted.execution_grant(),
            ExecutionGrant {
                jobs: 4,
                capabilities: 4
            }
        );
        assert_eq!(granted.cli_argv(), request.cli_argv());
        assert!(grant_worker_argv(
            &argv,
            CompileWorkload::Foreground,
            ExecutionGrant {
                jobs: 4,
                capabilities: 4
            }
        )
        .is_err());
    }
    use std::io::Cursor;
    use std::os::unix::ffi::OsStringExt;

    fn build_products_fixture() -> &'static Path {
        static FIXTURE: std::sync::OnceLock<(tempfile::TempDir, std::path::PathBuf)> =
            std::sync::OnceLock::new();
        &FIXTURE
            .get_or_init(|| {
                let dir = tempfile::tempdir().unwrap();
                let source = dir.path().join("worker.rs");
                fs::write(&source, include_str!("fixtures/build_products_worker.rs")).unwrap();
                let binary = dir.path().join("worker");
                #[allow(
                    clippy::disallowed_methods,
                    reason = "compile one immutable test worker fixture"
                )]
                let status = std::process::Command::new("rustc")
                    .arg(&source)
                    .arg("--edition=2021")
                    .arg("-o")
                    .arg(&binary)
                    .status()
                    .unwrap();
                assert!(status.success());
                (dir, binary)
            })
            .1
    }

    fn products_request(root: &Path, input: &Path) -> Vec<OsString> {
        let mut request = ExtractRequest::default();
        request.input(input);
        request.build_products_dir(root);
        request.worker_argv()
    }

    fn products_response(worker: &mut Worker, cwd: &Path, argv: &[OsString]) -> Vec<String> {
        worker.begin_transaction().unwrap();
        let (code, output, stderr) = worker.request(cwd, argv).unwrap();
        worker.end_transaction().unwrap();
        assert_eq!(code, 0);
        assert!(String::from_utf8(stderr)
            .unwrap()
            .lines()
            .all(crate::diagnostics::is_machine_stderr_line));
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn scratch_failure_fixture() -> (tempfile::TempDir, Worker, std::path::PathBuf, Vec<String>) {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("Expr.hs");
        fs::write(&input, "completed body").unwrap();
        let prepared = PreparedWorker::for_test(build_products_fixture().to_owned()).unwrap();
        let mut worker = Worker::spawn(&prepared).unwrap();
        let response = products_response(
            &mut worker,
            directory.path(),
            &products_request(&directory.path().join("products"), &input),
        );
        let owned = worker
            .build_products_namespace
            .directories
            .iter()
            .next()
            .unwrap()
            .clone();
        // A filesystem obstruction at the real placed path makes cleanup fail
        // deterministically, without permissions depending on the test account.
        fs::remove_dir_all(&owned).unwrap();
        fs::write(&owned, b"owned scratch obstruction").unwrap();
        (directory, worker, owned, response)
    }

    #[test]
    fn reaped_worker_scratch_failure_preserves_completed_body_and_owned_retry_path() {
        let (_directory, mut worker, owned, response) = scratch_failure_fixture();
        let result = worker.shutdown_confirmed();
        assert!(
            worker.child.try_wait().unwrap().unwrap().success(),
            "scratch failure does not imply a live worker"
        );
        assert!(!response.is_empty(), "request completed before close");
        let Err(FrontendError::ScratchCleanup { status, failures }) = result else {
            panic!("scratch health must be separate from successful worker retirement");
        };
        assert!(status.success());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].path, owned);
        assert_eq!(
            failures[0].phase,
            crate::frontend::ScratchCleanupPhase::Products
        );
        assert!(worker.build_products_namespace.directories.contains(&owned));
        fs::remove_file(&owned).unwrap();
        worker.build_products_namespace.cleanup_checked().unwrap();
        assert!(worker.build_products_namespace.directories.is_empty());
        assert!(!owned.exists());
    }

    #[test]
    fn unsuccessful_worker_retirement_keeps_secondary_scratch_failure() {
        let (_directory, mut worker, owned, response) = scratch_failure_fixture();
        worker.child.kill().unwrap();
        let result = worker.shutdown_confirmed();
        assert!(
            !response.is_empty(),
            "completed action is independent of worker retirement"
        );
        let Err(FrontendError::WorkerClose { status, scratch }) = result else {
            panic!("worker and scratch failures must both be retained");
        };
        assert!(!status.success());
        assert_eq!(scratch.len(), 1);
        assert_eq!(scratch[0].path, owned);
        assert!(worker.build_products_namespace.directories.contains(&owned));
        fs::remove_file(&owned).unwrap();
        worker.build_products_namespace.cleanup_checked().unwrap();
    }

    #[test]
    fn build_products_isolate_concurrent_slots_and_daemon_epochs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("products");
        let a = dir.path().join("a/Expr.hs");
        let b = dir.path().join("b/Expr.hs");
        fs::create_dir_all(a.parent().unwrap()).unwrap();
        fs::create_dir_all(b.parent().unwrap()).unwrap();
        fs::write(&a, "alpha").unwrap();
        fs::write(&b, "beta").unwrap();
        let prepared = PreparedWorker::for_test(build_products_fixture().to_owned()).unwrap();
        let mut first = Worker::spawn_in_slot(&prepared, &[0; 32], 0).unwrap();
        let mut second = Worker::spawn_in_slot(&prepared, &[0; 32], 1).unwrap();
        let argv_a = products_request(&root, &a);
        let argv_b = products_request(&root, &b);
        let original = argv_a.clone();
        let (out_a, out_b) = std::thread::scope(|scope| {
            let a = scope.spawn(|| products_response(&mut first, dir.path(), &argv_a));
            let b = scope.spawn(|| products_response(&mut second, dir.path(), &argv_b));
            (a.join().unwrap(), b.join().unwrap())
        });
        assert_eq!(out_a[2], "alpha");
        assert_eq!(out_b[2], "beta");
        assert_ne!(out_a[0], out_b[0]);
        assert_eq!(argv_a, original, "logical request must remain unchanged");
        let mut independent = Worker::spawn_in_slot(&prepared, &[1; 32], 0).unwrap();
        let out = products_response(&mut independent, dir.path(), &argv_b);
        assert_ne!(out[0], out_a[0]);
        assert_ne!(out[0], out_b[0]);
        assert_eq!(out[1], "", "independent daemon must start cold");
        first.shutdown();
        second.shutdown();
        independent.shutdown();
    }

    #[test]
    fn build_products_stay_warm_only_after_reaped_slot_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("Expr.hs");
        fs::write(&input, "alpha").unwrap();
        let prepared = PreparedWorker::for_test(build_products_fixture().to_owned()).unwrap();
        let mut worker = Worker::spawn_in_slot(&prepared, &[0; 32], 0).unwrap();
        let argv = products_request(&dir.path().join("products"), &input);
        let first = products_response(&mut worker, dir.path(), &argv);
        let second = products_response(&mut worker, dir.path(), &argv);
        assert_eq!(first[0], second[0]);
        assert_eq!(second[1], "alpha");
        assert!(
            worker.respawn(&prepared).is_err(),
            "live worker must prohibit directory reuse"
        );
        worker.shutdown();
        assert!(worker.child.try_wait().unwrap().is_some());
        worker.respawn(&prepared).unwrap();
        let rotated = products_response(&mut worker, dir.path(), &argv);
        assert_eq!(first[0], rotated[0]);
        assert_eq!(rotated[1], "alpha");
        assert_ne!(first[3], rotated[3]);
        worker.abort();
        worker.respawn(&prepared).unwrap();
        assert_eq!(
            products_response(&mut worker, dir.path(), &argv)[1],
            "alpha"
        );
        worker.shutdown();
    }

    #[test]
    fn build_products_direct_invocations_have_private_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("Expr.hs");
        fs::write(&input, "alpha").unwrap();
        let prepared = PreparedWorker::for_test(build_products_fixture().to_owned()).unwrap();
        let argv = products_request(&dir.path().join("products"), &input);
        let mut first = Worker::spawn(&prepared).unwrap();
        let mut second = Worker::spawn(&prepared).unwrap();
        let out_a = products_response(&mut first, dir.path(), &argv);
        let out_b = products_response(&mut second, dir.path(), &argv);
        assert_ne!(out_a[0], out_b[0]);
        assert_eq!(out_b[1], "");
        let mut cli_namespace = BuildProductsNamespace::direct().unwrap();
        let (cli, diagnostic) = cli_namespace.place(dir.path(), &argv).unwrap();
        assert_ne!(cli, argv);
        assert!(String::from_utf8(diagnostic)
            .unwrap()
            .starts_with("tidepool-build-products "));
        assert_ne!(cli_namespace.path, first.build_products_namespace.path);
        assert_ne!(cli_namespace.path, second.build_products_namespace.path);
        first.shutdown();
        second.shutdown();
    }

    #[test]
    fn build_products_cleanup_reaps_owners_and_preserves_other_slots_and_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("products");
        fs::create_dir(&root).unwrap();
        let unrelated = root.join("caller-owned");
        fs::write(&unrelated, "retain").unwrap();
        let input = dir.path().join("Expr.hs");
        fs::write(&input, "alpha").unwrap();
        let prepared = PreparedWorker::for_test(build_products_fixture().to_owned()).unwrap();
        let argv = products_request(&root, &input);
        let mut first = Worker::spawn_in_slot(&prepared, &[0; 32], 0).unwrap();
        let mut second = Worker::spawn_in_slot(&prepared, &[0; 32], 1).unwrap();
        let out_a = products_response(&mut first, dir.path(), &argv);
        let out_b = products_response(&mut second, dir.path(), &argv);
        let pid_a = first.child.id();
        drop(first); // Drop must kill/reap before removing owned scratch.
        assert!(!Path::new(&out_a[0]).exists());
        assert!(!Path::new(&format!("/proc/{pid_a}")).exists());
        assert!(Path::new(&out_b[0]).join("Expr.hi").exists());
        second.abort();
        drop(second);
        assert!(!Path::new(&out_b[0]).exists());
        assert!(!Path::new(&out_a[0]).parent().unwrap().exists());
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "retain");
        assert!(root.exists());

        let mut direct = Worker::spawn(&prepared).unwrap();
        // The same owner can receive relative roots and several logical roots.
        let relative = products_request(Path::new("products"), &input);
        let direct_out = products_response(&mut direct, dir.path(), &relative);
        let other_root = dir.path().join("other-products");
        let other_out = products_response(
            &mut direct,
            dir.path(),
            &products_request(&other_root, &input),
        );
        direct.shutdown();
        drop(direct);
        assert!(!Path::new(&other_out[0]).exists());
        assert!(!dir.path().join(&direct_out[0]).exists());
        assert!(other_root.exists());
    }

    #[test]
    fn default_request_rotation_is_1024_with_existing_rss_ceiling() {
        assert_eq!(DEFAULT_ROTATE_AFTER, 1024);
        assert_eq!(DEFAULT_REQUEST_DEADLINE, Duration::from_secs(15 * 60));
        assert_eq!(DEFAULT_WORKER_COUNT, 3);
        assert_eq!(DEFAULT_MEMORY_BUDGET_MB, 21 * 1024);
        assert_eq!(
            DEFAULT_MEMORY_BUDGET_MB / DEFAULT_WORKER_COUNT as u64,
            7 * 1024,
            "the default per-worker RSS ceiling is the total budget split across the default worker count"
        );
    }

    /// A quiet box with ample free memory still gets the historical fixed
    /// budget — this must not regress the common (no competing daemon) case.
    #[test]
    fn budget_from_available_uses_the_fixed_ceiling_when_memory_is_plentiful() {
        assert_eq!(
            budget_from_available(Some(
                DEFAULT_MEMORY_BUDGET_MB + DEFAULT_MEMORY_HEADROOM_MB + 4 * 1024
            )),
            DEFAULT_MEMORY_BUDGET_MB
        );
    }

    /// The scenario this change exists for: a competing compiler daemon
    /// (this repo's persistent test daemon, or a stray prior run) already
    /// holds enough RSS that only a fraction of the historical budget is
    /// actually available. The default must size down to what's left minus
    /// headroom, not claim the fixed figure regardless.
    #[test]
    fn budget_from_available_sizes_down_next_to_a_competing_daemon() {
        // 31 GiB box, ~16.5 GiB already held by a warm test daemon: about
        // 14.5 GiB available.
        let available = 14 * 1024 + 512;
        assert_eq!(
            budget_from_available(Some(available)),
            available - DEFAULT_MEMORY_HEADROOM_MB
        );
        assert!(budget_from_available(Some(available)) < DEFAULT_MEMORY_BUDGET_MB);
    }

    /// Never sizes to (near) zero even when memory is almost gone — the
    /// daemon still starts and serves requests, just with a worker that
    /// rotates often.
    #[test]
    fn budget_from_available_floors_at_the_minimum_when_memory_is_scarce() {
        assert_eq!(budget_from_available(Some(0)), MINIMUM_MEMORY_BUDGET_MB);
        assert_eq!(
            budget_from_available(Some(DEFAULT_MEMORY_HEADROOM_MB)),
            MINIMUM_MEMORY_BUDGET_MB
        );
    }

    /// `/proc/meminfo` unreadable (non-Linux, a sandbox) falls back to the
    /// historical fixed figure rather than failing the daemon.
    #[test]
    fn budget_from_available_falls_back_to_the_fixed_ceiling_when_unknown() {
        assert_eq!(budget_from_available(None), DEFAULT_MEMORY_BUDGET_MB);
    }

    /// Plentiful memory: the full 3-worker pool at the warm-worker ceiling —
    /// the common case this module exists to keep working.
    #[test]
    fn worker_sizing_uses_three_workers_at_the_warm_ceiling_when_memory_is_plentiful() {
        let sizing = worker_sizing_from_budget(DEFAULT_MEMORY_BUDGET_MB);
        assert_eq!(sizing.workers, 3);
        assert_eq!(sizing.rss_ceiling_mb, WARM_WORKER_MB);
        assert!(sizing.can_stay_warm);
    }

    /// The exact incident this change fixes: a budget that supports fewer
    /// than `DEFAULT_WORKER_COUNT` warm workers must size the *count* down,
    /// not shrink every slot below `WARM_WORKER_MB`. A 9.8 GiB budget fits
    /// only one 7 GiB worker (two would leave under 5 GiB each).
    #[test]
    fn worker_sizing_drops_to_one_worker_when_the_budget_cannot_fit_two() {
        let budget_mb = 9 * 1024 + 800;
        let sizing = worker_sizing_from_budget(budget_mb);
        assert_eq!(sizing.workers, 1);
        assert_eq!(sizing.rss_ceiling_mb, budget_mb);
        assert!(sizing.can_stay_warm);
    }

    /// A tiny budget: even one worker cannot reach `WARM_WORKER_MB`, so
    /// sizing still floors at one worker (the existing floor behaviour) but
    /// flags that a worker cannot stay warm at this ceiling.
    #[test]
    fn worker_sizing_flags_cannot_stay_warm_on_a_tiny_budget() {
        let sizing = worker_sizing_from_budget(MINIMUM_MEMORY_BUDGET_MB);
        assert_eq!(sizing.workers, 1);
        assert_eq!(sizing.rss_ceiling_mb, MINIMUM_MEMORY_BUDGET_MB);
        assert!(!sizing.can_stay_warm);
    }

    /// Unknown available memory (`/proc/meminfo` unreadable) falls back to
    /// the historical fixed budget, which still derives the same 3-worker,
    /// 7 GiB-ceiling sizing as before this change.
    #[test]
    fn worker_sizing_from_unknown_available_memory_matches_the_fixed_default() {
        let sizing = worker_sizing_from_budget(budget_from_available(None));
        assert_eq!(sizing.workers, DEFAULT_WORKER_COUNT);
        assert_eq!(sizing.rss_ceiling_mb, WARM_WORKER_MB);
        assert!(sizing.can_stay_warm);
    }

    /// An explicit `--workers` that outruns what the budget can hold warm
    /// must not be silently overridden — `serve` still honors it — but the
    /// resulting ceiling is what `can_stay_warm` (and `serve`'s startup WARN)
    /// checks against.
    #[test]
    fn explicit_workers_on_a_small_budget_yields_a_ceiling_below_warm() {
        let budget_mb = 9 * 1024;
        let explicit_workers = 3u64;
        let rss_ceiling_mb = budget_mb / explicit_workers;
        assert!(
            rss_ceiling_mb < WARM_WORKER_MB,
            "an explicit --workers 3 on a 9 GiB budget must fall below the warm-worker ceiling, \
             which is exactly the case `serve` warns on"
        );
    }

    /// `/proc/meminfo` parsing itself, against a real MemAvailable line —
    /// exercised only where `/proc` exists.
    #[cfg(target_os = "linux")]
    #[test]
    fn available_memory_mb_reads_a_positive_value_from_proc_meminfo() {
        let available = available_memory_mb().expect("this test runs on Linux, /proc exists");
        assert!(available > 0);
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
        assert!(matches!(error, DaemonError::IncompleteResponse));
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
        let cwd = Path::new("/tmp/work");
        for workload in [CompileWorkload::Foreground, CompileWorkload::Preparation] {
            let mut request =
                ExtractRequest::from_cli(&["Expr.hs".into(), "--turn".into()]).unwrap();
            request.set_workload(workload);
            let client_argv = request.worker_argv();
            let logical_digest = compile_request_correlation(cwd, &client_argv);
            for jobs in [2, 4, 8, 16] {
                let grant = ExecutionGrant {
                    jobs,
                    capabilities: jobs + 1,
                };
                let daemon_argv = grant_worker_argv(
                    &normalize_worker_argv(client_argv.clone()).unwrap(),
                    workload,
                    grant,
                )
                .unwrap();
                assert_ne!(client_argv, daemon_argv);
                assert_eq!(
                    ExtractRequest::decode_worker_argv(&daemon_argv)
                        .unwrap()
                        .execution_grant(),
                    grant
                );
                assert_eq!(
                    compile_request_correlation(cwd, &daemon_argv),
                    logical_digest
                );
            }
        }
        let request = ExtractRequest::from_cli(&["Expr.hs".into(), "--turn".into()]).unwrap();
        let client_argv = request.worker_argv();
        let mut preparation = request.clone();
        preparation.set_workload(CompileWorkload::Preparation);
        assert_ne!(
            compile_request_correlation(cwd, &client_argv),
            compile_request_correlation(cwd, &preparation.worker_argv())
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
        let reuse = r#"tidepool-reuse {"schema":1,"cycle":621890506509752,"purpose":"general","stage":"source_frontend","decision":"hit","reason":"matched","items":1,"bytes":null,"observed_ns":621890512307275,"unit":"main","module":"CompilerWidthLeaf00","version_kind":"source_fingerprint","version":"cb94c89252fb7ccb94e11ddcdd6331e6"}"#;
        let trace = CapturedWriter::default();
        let subscriber = tracing_subscriber_with_trace_filter(
            CapturedWriter::default(),
            CapturedWriter::default(),
            trace.clone(),
            tracing_subscriber::EnvFilter::new("debug"),
            compiler_trace_filter(None),
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
tidepool-meta-execution request=7 unit=\"main\" module=\"Original\"\n\
tidepool-canonical-frontend module=Original\n\
tidepool-canonical-finalization module=Original\n\
tidepool-checked module=Inspect target=False\n\
tidepool-target phase=desugar module=Execute\n",
            );
            log_compile_timing(
                "run-7",
                "abcdef0123456789",
                format!(
                    "  {reuse}\ntidepool-reuse-error: witness failed\nOriginal.hs:1: error: tidepool-reuse is not in scope\n"
                )
                .as_bytes(),
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
                "tidepool-meta-execution request=7 unit=\"main\" module=\"Original\"",
                "tidepool-canonical-frontend module=Original",
                "tidepool-canonical-finalization module=Original",
                "tidepool-checked module=Inspect target=False",
                "tidepool-target phase=desugar module=Execute",
                reuse,
            ]
        );
    }

    #[test]
    fn raw_compiler_detail_filter_preserves_request_errors_and_reuse_rows() {
        let detailed = CapturedWriter::default();
        let trace = CapturedWriter::default();
        let raw_filter = "debug,tidepool_extract_cmd::daemon::compiler_detail=off";
        let worker_stderr = b"tidepool-timing phase=cycle_modules_wall ms=7\n\
tidepool-timing-detail parent=prepared_recover phase=lookup ms=1\n\
tidepool-timing-module-detail module=Execute phase=interface ms=1\n\
tidepool-count name=prepared_recover_rounds count=2\n\
tidepool-reuse {\"decision\":\"hit\"}\n\
tidepool-reuse-error: witness failed\n";
        let subscriber = tracing_subscriber_with_trace_filter(
            detailed.clone(),
            CapturedWriter::default(),
            trace.clone(),
            tracing_subscriber::EnvFilter::new(raw_filter),
            compiler_trace_filter(Some(raw_filter)),
        );

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "tidepool_extract_cmd::daemon",
                run_id = "run-filter",
                compile_request = "request-filter",
                "compiler request started"
            );
            log_compile_timing(
                "run-filter",
                "request-filter",
                worker_stderr,
            );
            tracing::error!(
                target: "tidepool_extract_cmd::daemon",
                run_id = "run-filter",
                compile_request = "request-filter",
                "compiler request failed"
            );
        });

        let events = trace
            .text()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let retained_lines = events
            .iter()
            .filter(|event| event["fields"]["message"] == "compiler timing")
            .map(|event| event["fields"]["line"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(retained_lines.contains(&"tidepool-timing phase=cycle_modules_wall ms=7"));
        assert!(retained_lines.contains(&"tidepool-reuse {\"decision\":\"hit\"}"));
        assert!(retained_lines
            .iter()
            .all(|line| !is_raw_compiler_detail(line)));
        assert!(!retained_lines.contains(&"tidepool-reuse-error: witness failed"));
        assert!(String::from_utf8(diagnostic_stderr(worker_stderr).to_vec())
            .unwrap()
            .contains("tidepool-reuse-error: witness failed"));
        assert!(events.iter().any(|event| {
            event["fields"]["message"] == "compiler request started"
                && event["fields"]["run_id"] == "run-filter"
                && event["fields"]["compile_request"] == "request-filter"
        }));
        assert!(events.iter().any(|event| {
            event["fields"]["message"] == "compiler request failed"
                && event["fields"]["run_id"] == "run-filter"
                && event["fields"]["compile_request"] == "request-filter"
        }));

        let detailed_text = detailed.text();
        assert!(detailed_text.contains("compiler request started"));
        assert!(detailed_text.contains("compiler request failed"));
        assert!(!detailed_text.contains("tidepool-reuse-error: witness failed"));
        assert!(!detailed_text.contains("tidepool-timing-detail"));
        assert!(!detailed_text.contains("tidepool-timing-module-detail"));
        assert!(!detailed_text.contains("tidepool-count"));
    }

    #[test]
    fn machine_stderr_is_kept_in_daemon_log_but_removed_from_diagnostics() {
        let stderr = b"ghc: panic!\ntidepool-timing phase=load ms=12\n  tidepool-meta-execution request=7 unit=\"main\" module=\"Original\"\n  tidepool-checked module=Foo target=True\ntidepool-dependency-witness nodes=3\n  tidepool-reuse {\"schema\":1}\ntidepool-reuse-error: witness failed\nOriginal.hs:1: error: tidepool-reuse is not in scope\nuseful detail\n";
        let diagnostic = String::from_utf8(diagnostic_stderr(stderr)).unwrap();
        assert_eq!(diagnostic, "ghc: panic!\ntidepool-reuse-error: witness failed\nOriginal.hs:1: error: tidepool-reuse is not in scope\nuseful detail");
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
            OsString::from("--worker-request-v14"),
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
    fn response_payload_limit_refuses_forged_header_before_body_read() {
        let mut bytes = 0i32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        let mut input = Cursor::new(bytes);
        assert!(matches!(decode_response(&mut input),
            Err(DaemonError::ResponseTooLarge { declared, remaining })
            if declared == u64::from(u32::MAX) && remaining == u64::from(MAX_RESPONSE_PAYLOAD_BYTES)));
        assert_eq!(input.position(), 8);
        let mut rejection = Cursor::new(u32::MAX.to_le_bytes());
        assert!(matches!(
            read_frame(&mut rejection),
            Err(DaemonError::ResponseTooLarge { .. })
        ));
        assert_eq!(rejection.position(), 4);
    }

    #[test]
    fn response_payload_limit_accepts_exact_aggregate_and_refuses_overflow() {
        let half = (MAX_RESPONSE_PAYLOAD_BYTES / 2) as usize;
        let out = vec![b'o'; half];
        let err = vec![b'e'; half];
        let mut wire = Vec::new();
        write_response(&mut wire, 0, &out, &err).unwrap();
        let decoded = decode_response(&mut Cursor::new(wire)).unwrap();
        assert_eq!(decoded, (0, out, err));

        let mut overflow = 0i32.to_le_bytes().to_vec();
        push_frame(&mut overflow, b"x");
        overflow.extend_from_slice(&MAX_RESPONSE_PAYLOAD_BYTES.to_le_bytes());
        let mut input = Cursor::new(overflow);
        assert!(matches!(decode_response(&mut input),
            Err(DaemonError::ResponseTooLarge { declared, remaining })
            if declared == u64::from(MAX_RESPONSE_PAYLOAD_BYTES)
                && remaining == u64::from(MAX_RESPONSE_PAYLOAD_BYTES - 1)));
        assert_eq!(input.position(), 13);
        let mut output = Vec::new();
        assert!(write_response(
            &mut output,
            0,
            &vec![0; MAX_RESPONSE_PAYLOAD_BYTES as usize],
            b"x"
        )
        .is_err());
        assert!(
            output.is_empty(),
            "oversized response must fail before output or encoding"
        );
    }

    #[test]
    fn response_payload_limit_matches_worker_capture_budget() {
        let source = include_str!("../../../bridge/haskell/src/Tidepool/WorkerServer.hs");
        let definition = format!("maxResponsePayloadBytes = {}", MAX_RESPONSE_PAYLOAD_BYTES);
        assert!(source.lines().any(|line| line == definition));
    }

    #[test]
    fn response_payload_limit_after_acceptance_is_indeterminate() {
        let socket = test_socket("accepted-oversized");
        fs::remove_file(&socket).ok();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut header = [0u8; 40];
            connection.read_exact(&mut header).unwrap();
            read_request(&mut connection).unwrap();
            connection.write_all(&[ACCEPTED]).unwrap();
            connection.write_all(&1u64.to_le_bytes()).unwrap();
            connection.write_all(&0i32.to_le_bytes()).unwrap();
            connection.write_all(&u32::MAX.to_le_bytes()).unwrap();
        });
        let error = execute(&socket, &[1; 32], Path::new("/tmp"), &["request".into()]).unwrap_err();
        server.join().unwrap();
        assert!(error.was_accepted());
        assert!(!error.permits_rebind());
        assert!(matches!(error,
            DaemonError::AfterAcceptance(inner) if matches!(*inner, DaemonError::ResponseTooLarge { .. })));
        fs::remove_file(socket).ok();
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
            Err(DaemonError::IncompleteResponse) => {}
            other => panic!("expected IncompleteResponse, got {other:?}"),
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
            Err(DaemonError::IncompleteResponse) => {}
            other => panic!("expected IncompleteResponse, got {other:?}"),
        }
    }

    #[test]
    fn decode_response_empty_stream_is_crashed() {
        let mut cur = Cursor::new(Vec::<u8>::new());
        match decode_response(&mut cur) {
            Err(DaemonError::IncompleteResponse) => {}
            other => panic!("expected IncompleteResponse, got {other:?}"),
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
    fn preflight_stalled_peer_obeys_readiness_deadline() {
        let socket = test_socket("preflight-deadline");
        let listener = UnixListener::bind(&socket).unwrap();
        let (release, held) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut request = [0u8; 8];
            connection.read_exact(&mut request).unwrap();
            connection.write_all(PREFLIGHT_RESPONSE).unwrap();
            held.recv().unwrap();
        });
        let started = Instant::now();
        let result = preflight_until(&socket, started + Duration::from_millis(50));
        release.send(()).unwrap();
        server.join().unwrap();
        assert!(
            matches!(result, Err(DaemonError::Io(error)) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        std::fs::remove_file(socket).unwrap();
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
            connection.write_all(&[8; 32]).unwrap();
            connection.write_all(&[9; 32]).unwrap();
        });
        let binding = preflight(&socket).unwrap();
        server.join().unwrap();
        assert_eq!(binding.producer, [7; 32]);
        assert_eq!(binding.consumed_worker, [8; 32]);
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
            let mut workload = [0];
            connection.read_exact(&mut workload).unwrap();
            assert_eq!(workload, [0]);
            assert_eq!(&header[8..], &[3; 32]);
            connection.write_all(&[ACCEPTED]).unwrap();
            connection.write_all(&1u64.to_le_bytes()).unwrap();
            for expected in ["one", "one"] {
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
        let trace = CapturedWriter::default();
        let subscriber = tracing_subscriber(
            CapturedWriter::default(),
            CapturedWriter::default(),
            trace.clone(),
            tracing_subscriber::EnvFilter::new("debug"),
        );
        tracing::subscriber::with_default(subscriber, || {
            for expected in ["one", "one"] {
                let output = execute_transaction_request(
                    &mut transaction,
                    Path::new("/tmp"),
                    &[OsString::from(expected)],
                )
                .unwrap();
                assert_eq!(output.stdout, expected.as_bytes());
            }
        });
        let events: Vec<serde_json::Value> = trace
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let identified: Vec<_> = events
            .iter()
            .filter(|row| row["fields"]["message"] == "compiler request identified")
            .collect();
        assert_eq!(identified.len(), 2);
        for (row, ordinal) in identified.iter().zip([1, 2]) {
            assert_eq!(row["fields"]["admission_id"], 1);
            assert_eq!(row["fields"]["request_ordinal"], ordinal);
            assert_eq!(row["fields"]["daemon_epoch"], hex(&[3; 32]));
        }
        assert_eq!(
            identified[0]["fields"]["compile_request"],
            identified[1]["fields"]["compile_request"]
        );
        end_transaction(&mut transaction).unwrap();
        server.join().unwrap();
        std::fs::remove_file(socket).ok();
    }

    #[test]
    fn transaction_queue_and_repeated_requests_have_exact_identities() {
        let dir = tempfile::tempdir().unwrap();
        let argv = vec![OsString::from("Expr.hs")];
        let client_argv = normalize_worker_argv(argv.clone()).unwrap();
        let client_digest = compile_request_correlation(dir.path(), &client_argv);
        let grant = ExecutionGrant {
            jobs: 2,
            capabilities: 2,
        };
        let granted_argv =
            grant_worker_argv(&client_argv, CompileWorkload::Foreground, grant).unwrap();
        assert_eq!(
            ExtractRequest::decode_worker_argv(&granted_argv)
                .unwrap()
                .execution_grant(),
            grant
        );
        let worker_bin = compile_sleepy_fake_worker(dir.path(), &argv, 0);
        let prepared = PreparedWorker::for_test(worker_bin).unwrap();
        let mut worker = Worker::spawn(&prepared).unwrap();
        let config = DaemonConfig {
            socket: dir.path().join("unused.sock"),
            rotate_after: None,
            rss_ceiling_mb: None,
            request_deadline_secs: None,
            watch_stamp: None,
            persistent: true,
            run_id: None,
            log_path: None,
            workers: Some(1),
            foreground_jobs: None,
            preparation_jobs: None,
        };
        let trace = CapturedWriter::default();
        let subscriber = tracing_subscriber(
            CapturedWriter::default(),
            CapturedWriter::default(),
            trace.clone(),
            tracing_subscriber::EnvFilter::new("debug"),
        );
        let mut served = 0;
        let mut followed_rotation = false;
        let mut early_replacements = 0;
        tracing::subscriber::with_default(subscriber, || {
            for (admission, request_count) in [(41, 2), (42, 0)] {
                let (connection, mut client) = UnixStream::pair().unwrap();
                let mut remaining = request_count;
                service_transaction(
                    connection,
                    &mut worker,
                    0,
                    &prepared,
                    &config,
                    "run-test",
                    &[9; 32],
                    Duration::from_millis(17),
                    AdmissionId(admission),
                    CompileWorkload::Foreground,
                    grant,
                    Duration::from_secs(10),
                    100,
                    u64::MAX,
                    true,
                    &mut served,
                    &mut followed_rotation,
                    &mut early_replacements,
                    |_| {
                        if remaining == 0 {
                            RequestStep::End
                        } else {
                            remaining -= 1;
                            identify_request(
                                DaemonEpoch([9; 32]),
                                AdmissionId(admission),
                                RequestOrdinal(request_count - remaining),
                                dir.path(),
                                &client_argv,
                            );
                            RequestStep::Request(dir.path().to_path_buf(), granted_argv.clone())
                        }
                    },
                )
                .unwrap();
                for _ in 0..request_count {
                    assert_eq!(decode_output(&mut client).unwrap().status.code(), Some(0));
                }
                assert_eq!(read_exact_or_crash(&mut client, 1).unwrap(), [ACCEPTED]);
            }
        });
        worker.shutdown();
        let events: Vec<serde_json::Value> = trace
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let queued: Vec<_> = events
            .iter()
            .filter(|row| row["fields"]["message"] == "compiler job dequeued")
            .collect();
        assert_eq!(queued.len(), 2);
        for (row, admission) in queued.iter().zip([41, 42]) {
            assert_eq!(row["fields"]["admission_id"], admission);
            assert_eq!(row["fields"]["queue_ms"], 17);
            assert_eq!(row["fields"]["compiler_workload"], "foreground");
            assert_eq!(row["fields"]["compiler_jobs"], 2);
            assert_eq!(row["fields"]["compiler_capabilities"], 2);
            assert!(row["fields"].get("request_ordinal").is_none());
        }
        let finished: Vec<_> = events
            .iter()
            .filter(|row| row["fields"]["message"] == "compiler request finished")
            .collect();
        assert_eq!(finished.len(), 2);
        let identified: Vec<_> = events
            .iter()
            .filter(|row| row["fields"]["message"] == "compiler request identified")
            .collect();
        assert_eq!(identified.len(), finished.len());
        for (client, physical) in identified.iter().zip(&finished) {
            for field in [
                "compile_request",
                "daemon_epoch",
                "admission_id",
                "request_ordinal",
            ] {
                assert_eq!(client["fields"][field], physical["span"][field]);
            }
        }
        for (row, ordinal) in finished.iter().zip([1, 2]) {
            assert_eq!(row["span"]["compile_request"], client_digest);
            assert_eq!(row["fields"]["compile_request"], client_digest);
            assert_eq!(row["span"]["admission_id"], 41);
            assert_eq!(row["span"]["request_ordinal"], ordinal);
            assert_eq!(row["span"]["daemon_epoch"], hex(&[9; 32]));
            assert_eq!(row["span"]["compiler_workload"], "foreground");
            assert_eq!(row["span"]["compiler_jobs"], 2);
            assert_eq!(row["span"]["compiler_capabilities"], 2);
        }
        assert_eq!(
            finished[0]["fields"]["compile_request"],
            finished[1]["fields"]["compile_request"]
        );
        assert_eq!(served, 2);
    }

    #[test]
    fn accepted_partial_identity_is_indeterminate_for_plain_and_transaction_requests() {
        for transaction in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("daemon.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut header = [0; 40];
                stream.read_exact(&mut header).unwrap();
                if !transaction {
                    read_request(&mut stream).unwrap();
                }
                stream.write_all(&[ACCEPTED, 1, 2, 3]).unwrap();
            });
            let error = if transaction {
                begin_transaction(&socket, &[1; 32]).unwrap_err()
            } else {
                execute(&socket, &[1; 32], Path::new("/tmp"), &["request".into()]).unwrap_err()
            };
            server.join().unwrap();
            assert!(error.was_accepted(), "{error}");
            assert!(!error.permits_rebind());
        }
    }

    #[test]
    fn busy_retry_identifies_only_the_accepted_admission() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            for busy in [true, false] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut header = [0; 40];
                stream.read_exact(&mut header).unwrap();
                let (_, argv) = read_request(&mut stream).unwrap();
                assert_eq!(argv, [OsString::from("same-request")]);
                if busy {
                    write_busy(&mut stream).unwrap();
                } else {
                    stream.write_all(&[ACCEPTED]).unwrap();
                    stream.write_all(&77u64.to_le_bytes()).unwrap();
                    write_response(&mut stream, 0, b"", b"").unwrap();
                }
            }
        });
        let trace = CapturedWriter::default();
        let subscriber = tracing_subscriber(
            CapturedWriter::default(),
            CapturedWriter::default(),
            trace.clone(),
            tracing_subscriber::EnvFilter::new("debug"),
        );
        tracing::subscriber::with_default(subscriber, || {
            assert_eq!(
                execute(
                    &socket,
                    &[9; 32],
                    Path::new("/tmp"),
                    &["same-request".into()]
                )
                .unwrap()
                .status
                .code(),
                Some(0)
            );
        });
        server.join().unwrap();
        let events: Vec<serde_json::Value> = trace
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let identified: Vec<_> = events
            .iter()
            .filter(|row| row["fields"]["message"] == "compiler request identified")
            .collect();
        assert_eq!(identified.len(), 1);
        assert_eq!(identified[0]["fields"]["admission_id"], 77);
        assert_eq!(identified[0]["fields"]["request_ordinal"], 1);
        assert_eq!(identified[0]["fields"]["daemon_epoch"], hex(&[9; 32]));
    }

    #[test]
    fn cancelling_an_identified_transaction_keeps_its_identity_without_replay() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (written_tx, written_rx) = std::sync::mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut header = [0; 40];
            stream.read_exact(&mut header).unwrap();
            assert_eq!(&header[..8], TRANSACTION);
            let mut workload = [0];
            stream.read_exact(&mut workload).unwrap();
            assert_eq!(workload, [0]);
            stream.write_all(&[ACCEPTED]).unwrap();
            stream.write_all(&81u64.to_le_bytes()).unwrap();
            let mut command = [0];
            stream.read_exact(&mut command).unwrap();
            assert_eq!(command, [TRANSACTION_REQUEST]);
            assert_eq!(
                read_request(&mut stream).unwrap().1,
                [OsString::from("same-request")]
            );
            written_tx.send(()).unwrap();
            assert_eq!(stream.read(&mut command).unwrap(), 0);
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        });
        let cancellation = crate::CompilerTransactionCancellation::new();
        let cancel = std::thread::spawn({
            let cancellation = cancellation.clone();
            move || {
                written_rx.recv().unwrap();
                cancellation.cancel();
            }
        });
        let trace = CapturedWriter::default();
        let subscriber = tracing_subscriber(
            CapturedWriter::default(),
            CapturedWriter::default(),
            trace.clone(),
            tracing_subscriber::EnvFilter::new("debug"),
        );
        tracing::subscriber::with_default(subscriber, || {
            let mut transaction =
                begin_transaction_with_cancellation(&socket, &[9; 32], Some(&cancellation))
                    .unwrap();
            let error = execute_transaction_request(
                &mut transaction,
                Path::new("/work"),
                &["same-request".into()],
            )
            .unwrap_err();
            assert!(error.was_accepted(), "{error}");
            assert!(!error.permits_rebind());
        });
        cancel.join().unwrap();
        server.join().unwrap();
        let events: Vec<serde_json::Value> = trace
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let identified: Vec<_> = events
            .iter()
            .filter(|row| row["fields"]["message"] == "compiler request identified")
            .collect();
        assert_eq!(identified.len(), 1);
        assert_eq!(identified[0]["fields"]["admission_id"], 81);
        assert_eq!(identified[0]["fields"]["request_ordinal"], 1);
        assert_eq!(identified[0]["fields"]["daemon_epoch"], hex(&[9; 32]));
    }

    #[test]
    fn transaction_disconnect_interrupts_an_inflight_worker_request() {
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: fakes a stuck compiler worker, not a production launch site"
        )]
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
            build_products_namespace: BuildProductsNamespace::direct().unwrap(),
        };
        let (connection, client) = UnixStream::pair().unwrap();
        drop(client);
        let started = Instant::now();
        let result = worker.request_while_connected(
            &connection,
            Path::new("/tmp"),
            &normalize_worker_argv(vec![OsString::from("request")]).unwrap(),
            DEFAULT_REQUEST_DEADLINE,
        );
        assert!(matches!(
            result,
            Err(FrontendError::WorkerClientDisconnected)
        ));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "disconnect did not interrupt the worker promptly"
        );
        worker.abort();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn completed_worker_result_after_peer_shutdown_is_abandoned() {
        for disconnected in [false, true] {
            for code in [0, 1] {
                #[allow(
                    clippy::disallowed_methods,
                    reason = "test fixture: owns an idle worker and an unrelated live process"
                )]
                let mut children = (0..2)
                    .map(|_| {
                        std::process::Command::new("sleep")
                            .arg("30")
                            .stdin(Stdio::piped())
                            .stdout(Stdio::piped())
                            .spawn()
                            .unwrap()
                    })
                    .collect::<Vec<_>>();
                let mut other = children.pop().unwrap();
                let mut child = children.pop().unwrap();
                let mut worker = Worker {
                    stdin: child.stdin.take(),
                    stdout: child.stdout.take().unwrap(),
                    child,
                    build_products_namespace: BuildProductsNamespace::direct().unwrap(),
                };
                let (connection, client) = UnixStream::pair().unwrap();
                let action_client = client.try_clone().unwrap();
                let result = worker.operation_while_connected(
                    &connection,
                    DEFAULT_REQUEST_DEADLINE,
                    WorkerOperation::Request,
                    move |_| {
                        // Shutdown precedes completion publication; either
                        // monitor branch must classify it as abandonment.
                        if disconnected {
                            action_client.shutdown(std::net::Shutdown::Both).unwrap();
                        }
                        Ok((code, vec![1], vec![2]))
                    },
                );
                let owned_status = worker.child.try_wait().unwrap();
                let other_alive = other.try_wait().unwrap().is_none();
                drop(client);
                worker.abort();
                let retired_status = worker.child.wait().unwrap();
                other.kill().unwrap();
                other.wait().unwrap();
                // Clean both owned fixtures before assertions, including a
                // failing regression against the original monitor body.
                if disconnected {
                    assert!(matches!(
                        result,
                        Err(FrontendError::WorkerClientDisconnected)
                    ));
                    assert!(!retired_status.success());
                } else {
                    assert_eq!(result.unwrap(), (code, vec![1], vec![2]));
                    assert!(owned_status.is_none());
                }
                assert!(other_alive);
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn peer_disconnect_is_visible_behind_unread_request_bytes() {
        let (mut connection, mut client) = UnixStream::pair().unwrap();
        client.write_all(&[TRANSACTION_REQUEST]).unwrap();
        assert!(!peer_disconnected(&connection));
        client.shutdown(std::net::Shutdown::Both).unwrap();
        assert!(peer_disconnected(&connection));
        let mut queued = [0];
        connection.read_exact(&mut queued).unwrap();
        assert_eq!(queued, [TRANSACTION_REQUEST]);
    }

    #[test]
    fn independently_exited_worker_is_not_a_client_disconnect() {
        let cwd = Path::new("/tmp");
        let argv = normalize_worker_argv(vec![OsString::from("request")]).unwrap();
        let request_bytes = encode_request(cwd, &argv).len() + 1;
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: consumes one worker request and exits without replying"
        )]
        let mut child = std::process::Command::new("dd")
            .args(["bs=1", &format!("count={request_bytes}"), "of=/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut worker = Worker {
            stdin: child.stdin.take(),
            stdout: child.stdout.take().unwrap(),
            child,
            build_products_namespace: BuildProductsNamespace::direct().unwrap(),
        };
        let (connection, client) = UnixStream::pair().unwrap();
        let result =
            worker.request_while_connected(&connection, cwd, &argv, DEFAULT_REQUEST_DEADLINE);
        assert!(
            matches!(result, Err(FrontendError::Daemon(_))),
            "{result:?}"
        );
        assert!(!peer_disconnected(&connection));
        drop(client);
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
            connection.write_all(&1u64.to_le_bytes()).unwrap();
        });
        let error = execute(&socket, &[1; 32], Path::new("/tmp"), &["request".into()]).unwrap_err();
        server.join().unwrap();
        assert!(!error.is_not_accepted());
        assert!(error.was_accepted());
        assert!(matches!(
            error,
            DaemonError::AfterAcceptance(inner) if matches!(*inner, DaemonError::IncompleteResponse)
        ));
        std::fs::remove_file(socket).ok();
    }

    #[derive(Clone, Copy)]
    enum ControlRecovery {
        Deadline,
        Cancel,
        Stop,
    }

    fn compile_control_ack_worker(directory: &Path) -> std::path::PathBuf {
        let source = directory.join("worker.rs");
        std::fs::write(&source, include_str!("test_fixtures/control_ack_worker.rs")).unwrap();
        let fixture = directory.join("worker");
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compile one immutable fake worker"
        )]
        let built = std::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&fixture)
            .status()
            .unwrap();
        assert!(built.success());
        fixture
    }

    #[test]
    fn one_shot_response_commits_after_cleanup_and_only_early_disconnect_replaces_worker() {
        let scratch = tempfile::tempdir().unwrap();
        let fixture = compile_control_ack_worker(scratch.path());
        for cancel in [false, true] {
            let dir = scratch
                .path()
                .join(if cancel { "cancel" } else { "complete" });
            std::fs::create_dir(&dir).unwrap();
            let worker_bin = dir.join("worker");
            std::fs::copy(&fixture, &worker_bin).unwrap();
            std::fs::write(dir.join("phase"), [2]).unwrap();
            let socket = dir.join("daemon.sock");
            let prepared = PreparedWorker::for_test(worker_bin).unwrap();
            let config = DaemonConfig {
                socket: socket.clone(),
                rotate_after: Some(100),
                rss_ceiling_mb: Some(u64::MAX),
                request_deadline_secs: Some(10),
                watch_stamp: None,
                persistent: true,
                run_id: None,
                log_path: None,
                workers: Some(1),
                foreground_jobs: None,
                preparation_jobs: None,
            };
            let (settled_tx, settled_rx) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let result = serve(&config, prepared);
                settled_tx.send(()).unwrap();
                result
            });
            let ready_deadline = Instant::now() + Duration::from_secs(10);
            let binding = loop {
                if let Ok(binding) = preflight(&socket) {
                    break binding;
                }
                assert!(Instant::now() < ready_deadline);
                #[allow(
                    clippy::disallowed_methods,
                    reason = "test fixture: await owned daemon startup"
                )]
                std::thread::sleep(Duration::from_millis(10));
            };
            let argv = vec![OsString::from("Expr.hs")];
            let mut client = UnixStream::connect(&socket).unwrap();
            client.write_all(REQUEST).unwrap();
            client.write_all(&binding.epoch).unwrap();
            client.write_all(&encode_request(&dir, &argv)).unwrap();
            assert_eq!(read_exact_or_crash(&mut client, 1).unwrap(), [ACCEPTED]);
            read_exact_or_crash(&mut client, 8).unwrap();
            while !dir.join("stalled").exists() {
                assert!(
                    Instant::now() < ready_deadline,
                    "worker never reached END_ACK"
                );
                #[allow(
                    clippy::disallowed_methods,
                    reason = "test fixture: await owned worker cleanup"
                )]
                std::thread::sleep(Duration::from_millis(10));
            }
            client
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            let observed = client.read(&mut [0; 1]);
            let response_pending = matches!(
                observed,
                Err(ref error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
            );
            if !cancel {
                std::fs::write(dir.join("release"), b"acknowledge cleanup").unwrap();
                if response_pending {
                    client
                        .set_read_timeout(Some(Duration::from_secs(4)))
                        .unwrap();
                    let output = decode_output(&mut client).unwrap();
                    assert_eq!(output.status.code(), Some(0));
                    assert_eq!(output.stdout, b"1");
                }
            }
            let started = Instant::now();
            drop(client);
            let output = execute(&socket, &binding.epoch, &dir, &argv).unwrap();
            let retained_count = output.stdout;
            request_stop(&socket).unwrap();
            settled_rx
                .recv_timeout(Duration::from_secs(4))
                .expect("daemon did not stop");
            assert_eq!(server.join().unwrap().unwrap(), 0);
            assert!(
                response_pending,
                "one-shot response was visible before END_ACK: {observed:?}"
            );
            assert_eq!(retained_count, if cancel { b"1" } else { b"2" });
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "early disconnect waited for the 10s deadline"
            );
        }
    }

    fn control_ack_recovery_cases(recovery: ControlRecovery) {
        let scratch = tempfile::tempdir().unwrap();
        let fixture = compile_control_ack_worker(scratch.path());

        for operation in [
            WorkerOperation::BeginTransaction,
            WorkerOperation::EndTransaction,
        ] {
            let dir = scratch.path().join(format!("{operation:?}"));
            std::fs::create_dir(&dir).unwrap();
            let worker_bin = dir.join("worker");
            std::fs::copy(&fixture, &worker_bin).unwrap();
            let phase = match operation {
                WorkerOperation::BeginTransaction => TRANSACTION_REQUEST,
                WorkerOperation::EndTransaction => TRANSACTION_END,
                WorkerOperation::Request => unreachable!(),
            };
            std::fs::write(dir.join("phase"), [phase]).unwrap();
            let socket = dir.join("daemon.sock");
            let prepared = PreparedWorker::for_test(worker_bin).unwrap();
            let config = DaemonConfig {
                socket: socket.clone(),
                rotate_after: None,
                rss_ceiling_mb: None,
                request_deadline_secs: Some(match recovery {
                    ControlRecovery::Cancel => 20,
                    _ => 1,
                }),
                watch_stamp: None,
                persistent: true,
                run_id: None,
                log_path: None,
                workers: Some(1),
                foreground_jobs: None,
                preparation_jobs: None,
            };
            let (settled_tx, settled_rx) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let result = crate::daemon::serve(&config, prepared);
                settled_tx.send(()).unwrap();
                result
            });
            let ready_deadline = Instant::now() + Duration::from_secs(10);
            let binding = loop {
                if let Ok(binding) = preflight(&socket) {
                    break binding;
                }
                assert!(Instant::now() < ready_deadline);
                #[allow(
                    clippy::disallowed_methods,
                    reason = "test fixture: await owned daemon startup"
                )]
                std::thread::sleep(Duration::from_millis(10));
            };
            let cancellation = crate::CompilerTransactionCancellation::new();
            let mut transaction =
                begin_transaction_with_cancellation(&socket, &binding.epoch, Some(&cancellation))
                    .unwrap();
            let argv = vec![OsString::from("Expr.hs")];
            if matches!(operation, WorkerOperation::EndTransaction) {
                let output = execute_transaction_request(&mut transaction, &dir, &argv).unwrap();
                assert_eq!(output.status.code(), Some(0));
            }
            let client_dir = dir.clone();
            let client_argv = argv.clone();
            let (client_settled_tx, client_settled_rx) = std::sync::mpsc::channel();
            let client = std::thread::spawn(move || {
                let result = match operation {
                    WorkerOperation::BeginTransaction => {
                        execute_transaction_request(&mut transaction, &client_dir, &client_argv)
                            .map(|_| ())
                    }
                    WorkerOperation::EndTransaction => end_transaction(&mut transaction),
                    WorkerOperation::Request => unreachable!(),
                };
                client_settled_tx.send(()).unwrap();
                result
            });
            while !dir.join("stalled").exists() {
                assert!(
                    Instant::now() < ready_deadline,
                    "worker never stalled at {operation:?}"
                );
                #[allow(
                    clippy::disallowed_methods,
                    reason = "test fixture: await fake worker control phase"
                )]
                std::thread::sleep(Duration::from_millis(10));
            }
            let started = Instant::now();
            match recovery {
                ControlRecovery::Deadline => {}
                ControlRecovery::Cancel => cancellation.cancel(),
                ControlRecovery::Stop => {
                    request_stop(&socket).unwrap();
                    assert!(
                        started.elapsed() < Duration::from_secs(1),
                        "STOP waited on control acknowledgement"
                    );
                }
            }
            client_settled_rx
                .recv_timeout(Duration::from_secs(4))
                .expect("control operation did not settle");
            let error = client.join().unwrap().unwrap_err();
            assert!(error.was_accepted(), "{error}");
            assert!(!error.permits_rebind(), "{error}");
            if !matches!(recovery, ControlRecovery::Stop) {
                let next = preflight(&socket).unwrap();
                assert_eq!(next.epoch, binding.epoch);
                let output = execute(&socket, &binding.epoch, &dir, &argv).unwrap();
                assert_eq!(output.status.code(), Some(0));
                if matches!(recovery, ControlRecovery::Cancel) {
                    assert!(
                        started.elapsed() < Duration::from_secs(2),
                        "worker replacement waited for the 20s deadline"
                    );
                }
                request_stop(&socket).unwrap();
            }
            settled_rx
                .recv_timeout(Duration::from_secs(4))
                .expect("STOP did not drain the worker");
            assert_eq!(server.join().unwrap().unwrap(), 0);
        }
    }

    #[test]
    fn worker_control_ack_deadlines_replace_hung_workers() {
        control_ack_recovery_cases(ControlRecovery::Deadline);
    }

    #[test]
    fn worker_control_ack_cancellation_replaces_hung_workers() {
        control_ack_recovery_cases(ControlRecovery::Cancel);
    }

    #[test]
    fn worker_control_ack_stop_drains_at_deadline() {
        control_ack_recovery_cases(ControlRecovery::Stop);
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
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compiles a throwaway fake worker binary, not a production launch site"
        )]
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
            // Pin one worker slot: this test relies on the single fake
            // worker's sentinel-file trick to deterministically hang on its
            // first invocation and answer on its second.
            workers: Some(1),
            foreground_jobs: None,
            preparation_jobs: None,
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
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync polling loop waiting for the daemon/fake worker, not async code"
            )]
            std::thread::sleep(Duration::from_millis(10));
        };

        let trace = CapturedWriter::default();
        let dispatch = tracing::Dispatch::new(tracing_subscriber(
            CapturedWriter::default(),
            CapturedWriter::default(),
            trace.clone(),
            tracing_subscriber::EnvFilter::new("debug"),
        ));
        let started = Instant::now();
        let error = tracing::dispatcher::with_default(&dispatch, || {
            execute(&socket, &binding.epoch, &dir, &argv)
        })
        .unwrap_err();
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
        let output = tracing::dispatcher::with_default(&dispatch, || {
            execute(&socket, &binding.epoch, &dir, &argv)
        })
        .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let events: Vec<serde_json::Value> = trace
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let identified: Vec<_> = events
            .iter()
            .filter(|row| row["fields"]["message"] == "compiler request identified")
            .collect();
        assert_eq!(identified.len(), 2);
        assert_ne!(
            identified[0]["fields"]["admission_id"],
            identified[1]["fields"]["admission_id"]
        );
        assert_eq!(
            identified[0]["fields"]["compile_request"],
            identified[1]["fields"]["compile_request"]
        );
        for row in identified {
            assert_eq!(row["fields"]["request_ordinal"], 1);
            assert_eq!(row["fields"]["daemon_epoch"], hex(&binding.epoch));
        }

        std::fs::write(&stamp, b"changed").unwrap();
        assert!(preflight(&socket).is_err());
        assert_eq!(server.join().unwrap().unwrap(), 0);
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn one_worker_keeps_control_responsive_and_bounds_pending_admission() {
        let dir = std::env::temp_dir().join(format!("tp-control-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let started = dir.join("started");
        let release = dir.join("release");
        let second_started = dir.join("second-started");
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
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut one = [0u8; 1];
    let mut requests = 0;
    while stdin.read_exact(&mut one).is_ok() {{
        stdout.write_all(&[1]).unwrap();
        stdout.flush().unwrap();
        stdin.read_exact(&mut one).unwrap();
        let mut payload = vec![0; {payload_len}];
        stdin.read_exact(&mut payload).unwrap();
        requests += 1;
        if requests == 1 {{
            std::fs::write(r"{started}", b"").unwrap();
            while !std::path::Path::new(r"{release}").exists() {{
                std::thread::sleep(std::time::Duration::from_millis(5));
            }}
        }} else {{
            std::fs::write(r"{second_started}", b"").unwrap();
        }}
        stdout.write_all(&[0u8; 12]).unwrap();
        stdout.flush().unwrap();
        stdin.read_exact(&mut one).unwrap();
        stdout.write_all(&[1]).unwrap();
        stdout.flush().unwrap();
    }}
}}
"#,
                payload_len = payload_len,
                started = started.display(),
                release = release.display(),
                second_started = second_started.display(),
            ),
        )
        .unwrap();
        let worker_bin = dir.join("fake-worker");
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compiles a throwaway fake worker binary"
        )]
        let rustc = std::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&worker_bin)
            .status()
            .unwrap();
        assert!(rustc.success());
        let prepared = PreparedWorker::for_test(worker_bin).unwrap();
        let config = DaemonConfig {
            socket: socket.clone(),
            rotate_after: None,
            rss_ceiling_mb: None,
            request_deadline_secs: Some(10),
            watch_stamp: None,
            persistent: true,
            run_id: None,
            log_path: None,
            workers: Some(1),
            foreground_jobs: None,
            preparation_jobs: None,
        };
        let server = std::thread::spawn(move || serve(&config, prepared));
        let ready_deadline = Instant::now() + Duration::from_secs(10);
        let binding = loop {
            if let Ok(binding) = preflight(&socket) {
                break binding;
            }
            assert!(
                Instant::now() < ready_deadline,
                "daemon did not become ready"
            );
            #[allow(
                clippy::disallowed_methods,
                reason = "test polls a fake daemon startup sentinel"
            )]
            std::thread::sleep(Duration::from_millis(10));
        };
        let first_socket = socket.clone();
        let first_dir = dir.clone();
        let first_epoch = binding.epoch;
        let first_argv = argv.clone();
        let first = std::thread::spawn(move || {
            execute(&first_socket, &first_epoch, &first_dir, &first_argv)
        });
        while !started.exists() {
            assert!(Instant::now() < ready_deadline, "worker did not start");
            #[allow(
                clippy::disallowed_methods,
                reason = "test polls a fake worker barrier sentinel"
            )]
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut queued = UnixStream::connect(&socket).unwrap();
        queued.write_all(REQUEST).unwrap();
        queued.write_all(&binding.epoch).unwrap();
        queued.write_all(&encode_request(&dir, &argv)).unwrap();
        let mut decision = [0];
        queued.read_exact(&mut decision).unwrap();
        assert_eq!(decision, [ACCEPTED]);

        let mut excess = UnixStream::connect(&socket).unwrap();
        excess.write_all(REQUEST).unwrap();
        excess.write_all(&binding.epoch).unwrap();
        excess.write_all(&encode_request(&dir, &argv)).unwrap();
        excess.read_exact(&mut decision).unwrap();
        assert_eq!(decision, [BUSY]);
        drop(queued);

        let retrying = std::thread::spawn({
            let socket = socket.clone();
            let dir = dir.clone();
            let argv = argv.clone();
            let epoch = binding.epoch;
            move || execute(&socket, &epoch, &dir, &argv)
        });

        let control_started = Instant::now();
        assert_eq!(preflight(&socket).unwrap().epoch, binding.epoch);
        request_stop(&socket).unwrap();
        assert!(
            control_started.elapsed() < Duration::from_secs(1),
            "control messages waited on the held compile"
        );
        assert!(
            !release.exists(),
            "the compile was still held during control"
        );
        std::fs::write(&release, b"").unwrap();
        assert_eq!(first.join().unwrap().unwrap().status.code(), Some(0));
        assert_eq!(server.join().unwrap().unwrap(), 0);
        let refusal = retrying.join().unwrap().unwrap_err();
        assert!(refusal.is_not_accepted(), "{refusal}");
        assert!(!second_started.exists(), "disconnected queued job ran");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stop_drains_the_in_flight_request_then_exits_and_rejects_a_queued_client() {
        // A fake worker that acks `begin_transaction`, then on a request
        // touches a "started" sentinel before sleeping briefly and replying.
        // `STOP` sent during that sleep must let the accepted request finish.
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
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compiles a throwaway fake worker binary, not a production launch site"
        )]
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
            // Pin one worker slot: this test dispatches the in-flight and
            // queued requests in a specific order relative to `STOP` and
            // relies on there being exactly one worker to serve them.
            workers: Some(1),
            foreground_jobs: None,
            preparation_jobs: None,
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
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync polling loop waiting for the daemon/fake worker, not async code"
            )]
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
        // request and the worker is blocked in `request_while_connected`.
        let dispatched_deadline = Instant::now() + Duration::from_secs(10);
        while !started_sentinel.exists() {
            assert!(
                Instant::now() < dispatched_deadline,
                "the in-flight request was never dispatched to the worker"
            );
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync polling loop waiting for the daemon/fake worker, not async code"
            )]
            std::thread::sleep(Duration::from_millis(5));
        }

        // Send `STOP` and then a second request. The stop path closes
        // admission while preserving the request already running.
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

        // `STOP` acks and the daemon exits normally.
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

    /// Compile a fake worker (rustc, matching the style of the fixtures
    /// above) that acks `begin_transaction`, sleeps `sleep_ms` per request,
    /// and answers successfully. A `--workers 2`
    /// daemon spawns one of these per slot; two connections dispatched to
    /// two distinct slots therefore run this sleep concurrently in two
    /// separate OS processes.
    fn compile_sleepy_fake_worker(
        dir: &Path,
        argv: &[OsString],
        sleep_ms: u64,
    ) -> std::path::PathBuf {
        let worker_argv = normalize_worker_argv(argv.to_vec()).unwrap();
        let payload_len = encode_request(dir, &worker_argv).len();
        let started = dir.join("started");
        let source = dir.join("fake_worker.rs");
        std::fs::write(
            &source,
            format!(
                r#"
use std::io::{{Read, Write}};

fn main() {{
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut one = [0u8; 1];

    while stdin.read_exact(&mut one).is_ok() {{ // begin_transaction
        stdout.write_all(&[1]).unwrap();
        stdout.flush().unwrap();

        loop {{
        stdin.read_exact(&mut one).unwrap(); // request or end prefix
        if one[0] == 0 {{ break; }}
        let mut payload = vec![0u8; {payload_len}];
        stdin.read_exact(&mut payload).unwrap();
        std::fs::write(r"{started}", b"").unwrap();

        std::thread::sleep(std::time::Duration::from_millis({sleep_ms}));

        stdout.write_all(&[0u8; 12]).unwrap(); // code=0, empty stdout/stderr frames
        stdout.flush().unwrap();

        }}
        stdout.write_all(&[1]).unwrap();
        stdout.flush().unwrap();
    }}
}}
"#,
                payload_len = payload_len,
                sleep_ms = sleep_ms,
                started = started.display(),
            ),
        )
        .unwrap();
        let worker_bin = dir.join("fake-worker");
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compiles a throwaway fake worker binary, not a production launch site"
        )]
        let rustc = std::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&worker_bin)
            .status()
            .unwrap();
        assert!(rustc.success(), "fake worker failed to compile");
        worker_bin
    }

    #[test]
    fn ordinary_one_worker_retires_after_rotation() {
        let dir = std::env::temp_dir().join(format!("tp-ordinary-rotate-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let argv = vec![OsString::from("Expr.hs")];
        let worker_bin = compile_sleepy_fake_worker(&dir, &argv, 500);
        let prepared = PreparedWorker::for_test(worker_bin).unwrap();
        let config = DaemonConfig {
            socket: socket.clone(),
            rotate_after: Some(1),
            rss_ceiling_mb: None,
            request_deadline_secs: Some(10),
            watch_stamp: None,
            persistent: false,
            run_id: None,
            log_path: None,
            workers: Some(2),
            foreground_jobs: None,
            preparation_jobs: None,
        };
        let server = std::thread::spawn(move || serve(&config, prepared));
        let deadline = Instant::now() + Duration::from_secs(10);
        let binding = loop {
            if let Ok(binding) = preflight(&socket) {
                break binding;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            #[allow(
                clippy::disallowed_methods,
                reason = "test polls a fake daemon startup sentinel"
            )]
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut first = UnixStream::connect(&socket).unwrap();
        first.write_all(REQUEST).unwrap();
        first.write_all(&binding.epoch).unwrap();
        first.write_all(&encode_request(&dir, &argv)).unwrap();
        let mut decision = [0];
        first.read_exact(&mut decision).unwrap();
        assert_eq!(decision, [ACCEPTED]);
        let mut excess = UnixStream::connect(&socket).unwrap();
        excess.write_all(REQUEST).unwrap();
        excess.write_all(&binding.epoch).unwrap();
        excess.write_all(&encode_request(&dir, &argv)).unwrap();
        excess.read_exact(&mut decision).unwrap();
        assert_eq!(decision, [BUSY]);
        assert_eq!(decode_output(&mut first).unwrap().status.code(), Some(0));
        assert_eq!(server.join().unwrap().unwrap(), 0);
        assert!(!socket.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saturated_daemon_serves_callers_beyond_its_pending_slot() {
        let dir = std::env::temp_dir().join(format!("tp-busy-retry-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let argv = vec![OsString::from("Expr.hs")];
        let worker_bin = compile_sleepy_fake_worker(&dir, &argv, 250);
        let prepared = PreparedWorker::for_test(worker_bin).unwrap();
        let config = DaemonConfig {
            socket: socket.clone(),
            rotate_after: None,
            rss_ceiling_mb: None,
            request_deadline_secs: Some(10),
            watch_stamp: None,
            persistent: true,
            run_id: None,
            log_path: None,
            workers: Some(1),
            foreground_jobs: None,
            preparation_jobs: None,
        };
        let server = std::thread::spawn(move || serve(&config, prepared));
        let deadline = Instant::now() + Duration::from_secs(10);
        let binding = loop {
            if let Ok(binding) = preflight(&socket) {
                break binding;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            #[allow(clippy::disallowed_methods, reason = "test polls fake daemon startup")]
            std::thread::sleep(Duration::from_millis(10));
        };
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(5));
        let clients: Vec<_> = (0..4)
            .map(|index| {
                let socket = socket.clone();
                let dir = dir.clone();
                let argv = argv.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                let epoch = binding.epoch;
                std::thread::spawn(move || {
                    barrier.wait();
                    if index == 0 {
                        let mut stream = begin_transaction(&socket, &epoch)?;
                        let output = execute_transaction_request(&mut stream, &dir, &argv)?;
                        end_transaction(&mut stream)?;
                        Ok(output)
                    } else {
                        execute(&socket, &epoch, &dir, &argv)
                    }
                })
            })
            .collect();
        barrier.wait();
        let control_started = Instant::now();
        assert_eq!(preflight(&socket).unwrap().epoch, binding.epoch);
        assert!(control_started.elapsed() < Duration::from_secs(1));
        for client in clients {
            assert_eq!(client.join().unwrap().unwrap().status.code(), Some(0));
        }
        request_stop(&socket).unwrap();
        assert_eq!(server.join().unwrap().unwrap(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancelling_a_busy_transaction_stops_waiting_without_admission() {
        let dir = std::env::temp_dir().join(format!("tp-busy-cancel-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let argv = vec![OsString::from("Expr.hs")];
        let worker_bin = compile_sleepy_fake_worker(&dir, &argv, 800);
        let prepared = PreparedWorker::for_test(worker_bin).unwrap();
        let config = DaemonConfig {
            socket: socket.clone(),
            rotate_after: None,
            rss_ceiling_mb: None,
            request_deadline_secs: Some(10),
            watch_stamp: None,
            persistent: true,
            run_id: None,
            log_path: None,
            workers: Some(1),
            foreground_jobs: None,
            preparation_jobs: None,
        };
        let server = std::thread::spawn(move || serve(&config, prepared));
        let deadline = Instant::now() + Duration::from_secs(10);
        let binding = loop {
            if let Ok(binding) = preflight(&socket) {
                break binding;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            #[allow(clippy::disallowed_methods, reason = "test polls fake daemon startup")]
            std::thread::sleep(Duration::from_millis(10));
        };
        let first = std::thread::spawn({
            let socket = socket.clone();
            let dir = dir.clone();
            let argv = argv.clone();
            let epoch = binding.epoch;
            move || execute(&socket, &epoch, &dir, &argv)
        });
        while !dir.join("started").exists() {
            assert!(Instant::now() < deadline, "worker did not start");
            #[allow(clippy::disallowed_methods, reason = "test polls fake worker sentinel")]
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut queued = UnixStream::connect(&socket).unwrap();
        queued.write_all(REQUEST).unwrap();
        queued.write_all(&binding.epoch).unwrap();
        queued.write_all(&encode_request(&dir, &argv)).unwrap();
        let mut decision = [0];
        queued.read_exact(&mut decision).unwrap();
        assert_eq!(decision, [ACCEPTED]);

        let cancellation = crate::CompilerTransactionCancellation::new();
        let waiting = std::thread::spawn({
            let socket = socket.clone();
            let cancellation = cancellation.clone();
            let epoch = binding.epoch;
            move || begin_transaction_with_cancellation(&socket, &epoch, Some(&cancellation))
        });
        #[allow(
            clippy::disallowed_methods,
            reason = "test lets the waiting client receive Busy"
        )]
        std::thread::sleep(Duration::from_millis(50));
        cancellation.cancel();
        let result = waiting.join().unwrap();
        assert!(matches!(result, Err(DaemonError::Cancelled)), "{result:?}");
        drop(queued);
        assert_eq!(first.join().unwrap().unwrap().status.code(), Some(0));
        assert_eq!(preflight(&socket).unwrap().epoch, binding.epoch);
        request_stop(&socket).unwrap();
        assert_eq!(server.join().unwrap().unwrap(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two clients, two worker slots: each request is served by its own OS
    /// process, so the combined wall time for both is close to one sleep,
    /// not two — the daemon-wide serialization the redesign removes.
    #[test]
    fn two_requests_with_two_workers_are_served_in_parallel() {
        let dir = std::env::temp_dir().join(format!("tp-parallel-{}", std::process::id()));
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let stamp = dir.join("stamp");
        std::fs::write(&stamp, b"boot").unwrap();
        let argv = vec![OsString::from("Expr.hs")];
        const SLEEP_MS: u64 = 600;
        let worker_bin = compile_sleepy_fake_worker(&dir, &argv, SLEEP_MS);

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
            workers: Some(2),
            foreground_jobs: None,
            preparation_jobs: None,
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
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync polling loop waiting for the daemon/fake worker, not async code"
            )]
            std::thread::sleep(Duration::from_millis(10));
        };

        let started = Instant::now();
        let clients: Vec<_> = (0..2)
            .map(|_| {
                let socket = socket.clone();
                let epoch = binding.epoch;
                let dir = dir.clone();
                let argv = argv.clone();
                std::thread::spawn(move || execute(&socket, &epoch, &dir, &argv))
            })
            .collect();
        for client in clients {
            let output = client.join().unwrap().unwrap();
            assert_eq!(output.status.code(), Some(0));
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(SLEEP_MS * 3 / 2),
            "two concurrent requests on two worker slots took {elapsed:?}, \
             close to twice the {SLEEP_MS}ms sleep — they were not served in parallel"
        );

        assert!(request_stop(&socket).is_ok());
        assert_eq!(server.join().unwrap().unwrap(), 0);
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Idle slots must not replay a real request's source or compile-time IO.
    #[test]
    fn idle_pooled_slots_do_not_repeat_requests_or_mutate_outputs() {
        let dir = std::env::temp_dir().join(format!("tp-idle-pool-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let source = dir.join("fake_worker.rs");
        std::fs::write(
            &source,
            r#"
use std::io::{Read, Write};
fn read_u32(input: &mut impl Read) -> u32 {
    let mut bytes = [0u8; 4];
    input.read_exact(&mut bytes).unwrap();
    u32::from_le_bytes(bytes)
}
fn read_frame(input: &mut impl Read) -> Vec<u8> {
    let length = read_u32(input) as usize;
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes).unwrap();
    bytes
}
fn main() {
    let mut input = std::io::stdin();
    let mut output = std::io::stdout();
    loop {
        let mut command = [0u8; 1];
        if input.read_exact(&mut command).is_err() { break; }
        output.write_all(&[1]).unwrap();
        output.flush().unwrap();
        input.read_exact(&mut command).unwrap();
        let cwd = std::path::PathBuf::from(String::from_utf8(read_frame(&mut input)).unwrap());
        let argc = read_u32(&mut input);
        for _ in 0..argc { let _ = read_frame(&mut input); }
        let log = cwd.join("dispatch.log");
        let mut dispatches = std::fs::read_to_string(&log).unwrap_or_default();
        dispatches.push_str(&format!("{}\n", std::process::id()));
        std::fs::write(&log, &dispatches).unwrap();
        let implicit_output = cwd.join("Expr_cbor");
        std::fs::create_dir_all(&implicit_output).unwrap();
        std::fs::write(implicit_output.join("result"), dispatches.lines().count().to_string()).unwrap();
        output.write_all(&[0u8; 12]).unwrap();
        output.flush().unwrap();
        input.read_exact(&mut command).unwrap();
        output.write_all(&[1]).unwrap();
        output.flush().unwrap();
    }
}
"#,
        )
        .unwrap();
        let worker_bin = dir.join("fake-worker");
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compile its own worker"
        )]
        let built = std::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&worker_bin)
            .status()
            .unwrap();
        assert!(built.success());
        let prepared = PreparedWorker::for_test(worker_bin).unwrap();
        let config = DaemonConfig {
            socket: socket.clone(),
            rotate_after: None,
            rss_ceiling_mb: None,
            request_deadline_secs: Some(2),
            watch_stamp: None,
            persistent: true,
            run_id: None,
            log_path: None,
            workers: Some(2),
            foreground_jobs: None,
            preparation_jobs: None,
        };
        let server = std::thread::spawn(move || crate::daemon::serve(&config, prepared));
        let ready_deadline = Instant::now() + Duration::from_secs(10);
        let binding = loop {
            if let Ok(binding) = preflight(&socket) {
                break binding;
            }
            assert!(Instant::now() < ready_deadline);
            #[allow(
                clippy::disallowed_methods,
                reason = "test fixture: await own daemon startup"
            )]
            std::thread::sleep(Duration::from_millis(10));
        };
        // No explicit output directory: compiler defaults also belong to the caller.
        let argv = vec![
            OsString::from("Expr.hs"),
            OsString::from("--target"),
            OsString::from("result"),
        ];
        let output = execute(&socket, &binding.epoch, &dir, &argv).unwrap();
        assert_eq!(output.status.code(), Some(0));
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: expose speculative idle dispatch"
        )]
        std::thread::sleep(Duration::from_secs(1));
        assert!(request_stop(&socket).is_ok());
        assert_eq!(server.join().unwrap().unwrap(), 0);
        let dispatches = std::fs::read_to_string(dir.join("dispatch.log")).unwrap();
        assert_eq!(
            dispatches.lines().count(),
            1,
            "idle worker repeated a request: {dispatches}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("Expr_cbor/result")).unwrap(),
            "1"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// STOP drains accepted requests across both worker slots.
    #[test]
    fn stop_with_two_workers_lets_both_in_flight_requests_finish() {
        let dir = std::env::temp_dir().join(format!("tp-stop-pool-{}", std::process::id()));
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let stamp = dir.join("stamp");
        std::fs::write(&stamp, b"boot").unwrap();
        let argv = vec![OsString::from("Expr.hs")];
        const SLEEP_MS: u64 = 500;
        let worker_bin = compile_sleepy_fake_worker(&dir, &argv, SLEEP_MS);

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
            workers: Some(2),
            foreground_jobs: None,
            preparation_jobs: None,
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
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync polling loop waiting for the daemon/fake worker, not async code"
            )]
            std::thread::sleep(Duration::from_millis(10));
        };

        // Both in-flight requests, on their own threads: each blocks on its
        // slot's sleep and is only expected to return once its worker
        // replies.
        let in_flight: Vec<_> = (0..2)
            .map(|_| {
                let socket = socket.clone();
                let epoch = binding.epoch;
                let dir = dir.clone();
                let argv = argv.clone();
                std::thread::spawn(move || execute(&socket, &epoch, &dir, &argv))
            })
            .collect();

        // Give both requests time to reach their worker slots before STOP is
        // sent — generous relative to the daemon's own local dispatch cost,
        // well short of the fake workers' own sleep.
        #[allow(
            clippy::disallowed_methods,
            reason = "test: bounded settle time before sending STOP, not a retry loop"
        )]
        std::thread::sleep(Duration::from_millis(100));

        let mut stop_connection = UnixStream::connect(&socket).unwrap();
        stop_connection.write_all(STOP).unwrap();

        // Both in-flight requests complete successfully, unaffected by the
        // `STOP` that arrived while they were still running.
        for client in in_flight {
            let output = client.join().unwrap().unwrap();
            assert_eq!(output.status.code(), Some(0));
        }

        let mut ack = [0u8; 1];
        stop_connection.read_exact(&mut ack).unwrap();
        assert_eq!(ack, [STOP_ACK]);
        assert_eq!(server.join().unwrap().unwrap(), 0);

        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// One worker slot's hung request is killed at its own deadline without
    /// blocking the other slot: with two concurrent requests and exactly one
    /// hung worker process, one request finishes fast (its own slot was
    /// never touched by the other's hang) and the other is killed at the
    /// deadline — never serialized behind it.
    #[test]
    fn a_hung_worker_deadline_kill_does_not_block_the_other_slot() {
        let dir = std::env::temp_dir().join(format!("tp-deadline-pool-{}", std::process::id()));
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let stamp = dir.join("stamp");
        std::fs::write(&stamp, b"boot").unwrap();
        let sentinel = dir.join("hung-once");
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
    let sentinel = std::path::Path::new(r"{sentinel}");
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut one = [0u8; 1];

    // Exactly one of the two worker processes spawned at daemon boot wins
    // this race and hangs; the other behaves normally. Which client request
    // lands on which slot is not controlled by this fixture — the test only
    // asserts the *pattern* (one fast success, one deadline-killed failure).
    // Atomic: `create_new` fails if the file already exists, so exactly one
    // of the two racing processes observes `hang == true`, even if both
    // reach this line at nearly the same instant.
    let hang = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(sentinel)
        .is_ok();

    stdin.read_exact(&mut one).unwrap(); // begin_transaction
    stdout.write_all(&[1]).unwrap();
    stdout.flush().unwrap();

    stdin.read_exact(&mut one).unwrap(); // request prefix
    let mut payload = vec![0u8; {payload_len}];
    stdin.read_exact(&mut payload).unwrap();

    if hang {{
        std::thread::sleep(std::time::Duration::from_secs(3600));
        return;
    }}

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
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compiles a throwaway fake worker binary, not a production launch site"
        )]
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
            workers: Some(2),
            foreground_jobs: None,
            preparation_jobs: None,
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
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync polling loop waiting for the daemon/fake worker, not async code"
            )]
            std::thread::sleep(Duration::from_millis(10));
        };

        let results: Vec<_> = (0..2)
            .map(|_| {
                let socket = socket.clone();
                let epoch = binding.epoch;
                let dir = dir.clone();
                let argv = argv.clone();
                std::thread::spawn(move || {
                    let started = Instant::now();
                    let result = execute(&socket, &epoch, &dir, &argv);
                    (started.elapsed(), result)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|client| client.join().unwrap())
            .collect();

        let mut fast = None;
        let mut slow = None;
        for (elapsed, result) in results {
            if result.is_ok() {
                fast = Some((elapsed, result));
            } else {
                slow = Some((elapsed, result));
            }
        }
        let (fast_elapsed, fast_result) = fast.expect("the healthy slot's request must succeed");
        let (slow_elapsed, slow_result) =
            slow.expect("the hung slot's request must be killed at its deadline");

        assert_eq!(fast_result.unwrap().status.code(), Some(0));
        assert!(
            fast_elapsed < Duration::from_millis(800),
            "the healthy slot's request should not be delayed by the other \
             slot's hang: {fast_elapsed:?}"
        );

        let slow_error = slow_result.unwrap_err();
        assert!(slow_error.was_accepted(), "{slow_error}");
        assert!(
            slow_elapsed < Duration::from_secs(8),
            "the deadline did not bound the hung request: {slow_elapsed:?}"
        );

        assert!(request_stop(&socket).is_ok());
        assert_eq!(server.join().unwrap().unwrap(), 0);
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn daemon_boot_epoch_contributes_to_endpoint_identity() {
        let a = crate::CompilerIdentity::daemon([3; 32], [2; 32], [4; 32]);
        let b = crate::CompilerIdentity::daemon([3; 32], [2; 32], [5; 32]);
        assert_eq!(a.producer_bytes(), b.producer_bytes());
        assert_ne!(a.as_bytes(), b.as_bytes());
    }
}
