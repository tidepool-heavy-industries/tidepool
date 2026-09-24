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
/// A worker replaced by RSS rotation after serving fewer than this many
/// requests never got the chance to pay off its cold-start cost: it logs as
/// memo loss, not ordinary rotation. Deliberately small — a worker that
/// serves a handful of requests before growing past the ceiling is the
/// three-3.2-GiB-slot failure this module's sizing exists to prevent, not
/// routine turnover.
const EARLY_REPLACEMENT_SERVED_THRESHOLD: u64 = 8;
/// How often an idle pooled worker slot (one that has not yet served any
/// request) checks whether it should run its one-time pre-warm compile,
/// instead of blocking indefinitely for a real job. Small relative to any
/// real compile request, so it adds negligible latency to ordinary job
/// dispatch — see `serve_pooled`'s per-slot loop.
const WARM_UP_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Number of concurrent GHC worker slots a `--persistent` daemon runs by
/// default (`--workers`). Each slot is a full `Worker`: its own transaction
/// pinning, request deadline, peer-disconnect kill, and served/RSS rotation.
/// A single accept thread hands each accepted connection to a free slot
/// (`serve_pooled`'s rendezvous channel — never an unbounded per-connection
/// thread); with N slots, up to N ghc-heavy nextest processes stop queuing
/// behind one compiler worker. `.config/nextest.toml`'s
/// `[test-groups.ghc-heavy] max-threads` is sized at `DEFAULT_WORKER_COUNT + 1`
/// to match.
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
const WARM_WORKER_MB: u64 = 7 * 1024;
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
/// concurrent cargo/nextest build issuing those `ghc-heavy` requests
/// alongside the pool. A shared box sizes its worker count down on its own
/// (see `worker_count_from_budget`); passing `--workers 2` remains available
/// for a caller that wants to pin it.
const DEFAULT_MEMORY_BUDGET_MB: u64 = 21 * 1024;
/// Memory reserved out of what's available at daemon start, never claimed by
/// the default worker budget — for the concurrent cargo/nextest build (or
/// whatever else the caller is doing) and for `/proc/meminfo`'s own
/// estimation slop. Matches the roughly 10 GiB the historical fixed budget
/// already left over on its reference 31 GiB box.
const DEFAULT_MEMORY_HEADROOM_MB: u64 = 10 * 1024;
/// Floor under the derived default so a box that's nearly out of memory
/// still gets a daemon that can serve requests, just with a worker that
/// rotates often, rather than one sized to effectively nothing. Below this,
/// a warm module memo cannot survive between requests anyway, so there is no
/// further floor worth defending — the request still completes, just cold
/// every time.
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
/// This is the one place daemon sizing reads machine memory. It deliberately
/// does not add a second cross-process budget registry alongside
/// `exomonad-node`'s `command_resources` admission service: that service
/// already gates actor starts on memory actually available
/// (`/proc/meminfo`'s `MemAvailable`), so a daemon that also sizes itself
/// from current availability composes with that check for free — the
/// second daemon to start simply sees less room, without either side having
/// to register or negotiate anything with the other.
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
    /// `false` when even a single worker's ceiling (the whole budget) falls
    /// short of `WARM_WORKER_MB` — the daemon still runs one worker at that
    /// ceiling (the existing floor behaviour), but a worker will rotate on
    /// almost every request rather than staying warm.
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

/// Consecutive confirmed-empty job polls (each `WARM_UP_POLL_INTERVAL`) an
/// idle slot must observe before it starts its pre-warm compile. This is a
/// debounce, not a cost: it exists only so a slot that is about to receive
/// a real job concurrently dispatched to it (two requests landing on a
/// freshly started pool at nearly the same instant) does not instead spend
/// that time on a self-initiated warm-up and make the real request wait
/// behind it — a genuinely idle slot easily clears this many empty polls
/// before any real request would reasonably still be inbound.
const WARM_UP_IDLE_DEBOUNCE: u32 = 4;

/// Pure gate for whether an idle pooled worker slot should attempt its
/// one-time pre-warm compile right now: only a slot that has not yet served
/// a real request, has not already attempted (successfully or not) its own
/// warm-up, has been confirmed idle (no job received) for
/// `WARM_UP_IDLE_DEBOUNCE` consecutive polls, and only once some request's
/// include set (workspace/source root and argv) is actually known
/// daemon-wide.
fn should_attempt_warm_up(
    served: u64,
    warm_up_attempted: bool,
    idle_polls: u32,
    include_set_known: bool,
) -> bool {
    served == 0 && !warm_up_attempted && idle_polls >= WARM_UP_IDLE_DEBOUNCE && include_set_known
}

/// A scratch directory made solely to catch one warm-up compile's redirected
/// output writes; never a caller-visible artifact, so cleanup is best-effort
/// and unconditional (every exit path from `warm_up_slot`'s inner closure —
/// early `?` return included — drops this guard).
struct WarmUpScratch(std::path::PathBuf);

impl Drop for WarmUpScratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

/// Best-effort warm-up compile for a pooled worker slot's freshly spawned
/// worker, using the include set (`cwd`, `argv`) revealed by the first real
/// request any slot has served. This is what lets the daemon's *other* idle
/// slots find a warm module memo on their own first real request, instead
/// of every slot independently paying the cold `ghc_load` + `lowering` cost
/// the production incident this module's sizing fix exists for showed
/// (12s + 20-26s per cold worker, versus ~150ms warm). Never fails the
/// slot: a failed warm-up logs a WARN and respawns the worker so the slot
/// still serves normally, just cold on its first real request as before
/// this change.
///
/// The real request's argv is never replayed verbatim: it names output
/// paths (`ExtractRequest::redirect_outputs_for_warm_up`'s doc comment lists
/// exactly which) that the real request's own consumer may still be reading
/// or that name shared session state, so this decodes the typed request,
/// rewrites it into a side-effect-free copy — same includes, session root,
/// target, and files, so the same module graph loads and lowers and the
/// memo warms, but every output redirected into a scratch directory removed
/// immediately after — and only replays *that*.
fn warm_up_slot(
    worker: &mut Worker,
    prepared: &PreparedWorker,
    cwd: &Path,
    argv: &[OsString],
    run_id: &str,
    slot: usize,
) {
    let started = Instant::now();
    let result: Result<WorkerResponse, FrontendError> = (|| {
        let mut request = ExtractRequest::decode_worker_argv(argv).map_err(|error| {
            FrontendError::Daemon(format!(
                "compiler worker pre-warm request could not be decoded: {error}"
            ))
        })?;
        let scratch_dir = std::env::temp_dir().join(format!(
            "tidepool-warm-up-{}-slot{slot}",
            std::process::id()
        ));
        fs::create_dir_all(&scratch_dir).map_err(FrontendError::Io)?;
        let _scratch = WarmUpScratch(scratch_dir.clone());
        request.redirect_outputs_for_warm_up(&scratch_dir);
        let warm_up_argv = request.worker_argv();
        worker.begin_transaction()?;
        let outcome = worker.request(cwd, &warm_up_argv)?;
        worker.end_transaction()?;
        Ok(outcome)
    })();
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match result {
        Ok(_) => {
            tracing::info!(
                run_id,
                worker = slot,
                elapsed_ms,
                "compiler worker slot pre-warmed"
            );
        }
        Err(error) => {
            tracing::warn!(
                run_id,
                worker = slot,
                elapsed_ms,
                %error,
                "compiler worker slot pre-warm failed; replacing worker and continuing"
            );
            match Worker::spawn(prepared) {
                Ok(fresh) => *worker = fresh,
                Err(spawn_error) => {
                    tracing::warn!(
                        run_id,
                        worker = slot,
                        %spawn_error,
                        "failed to respawn compiler worker after a failed pre-warm"
                    );
                }
            }
        }
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
    worker_slot: usize,
    prepared: &PreparedWorker,
    config: &DaemonConfig,
    run_id: &str,
    request_deadline: Duration,
    rotate_after: u64,
    rss_ceiling_mb: u64,
    transaction: bool,
    served: &mut u64,
    followed_rotation: &mut bool,
    early_replacements: &mut u64,
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
                    worker = worker_slot,
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
        log_send_failure(
            run_id,
            "transaction accepted",
            connection.write_all(&[ACCEPTED]),
        );
        log_send_failure(run_id, "transaction accepted flush", connection.flush());
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
    let available_mb = available_memory_mb();
    let budget_mb = default_memory_budget_mb();
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

    let result = if worker_count <= 1 {
        serve_single(
            config,
            &prepared,
            listener,
            &socket,
            run_id,
            &boot_stamp,
            &epoch,
            &producer,
            rotate_after,
            rss_ceiling_mb,
            request_deadline,
        )
    } else {
        serve_pooled(
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
        )
    };

    // Every exit, orderly or not, drains connected clients with an explicit
    // rejection before the endpoint closes. Retirement is idempotent: both
    // branches above already retire the socket on their own orderly exits;
    // this also covers early returns via `?`.
    let retired = socket.retire();
    drop(socket);
    match (result, retired) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(code), Ok(())) => Ok(code),
    }
}

/// The single-worker daemon loop: one worker, one accept thread, exactly the
/// original (pre-pool) daemon behavior. Used whenever the daemon runs with
/// one worker slot — always for ordinary (non-`--persistent`) mode, and for
/// `--persistent --workers 1`.
#[allow(clippy::too_many_arguments)]
fn serve_single(
    config: &DaemonConfig,
    prepared: &PreparedWorker,
    listener: &UnixListener,
    socket: &OwnedSocket,
    run_id: &str,
    boot_stamp: &Option<Option<Vec<u8>>>,
    epoch: &[u8; 32],
    producer: &[u8; 32],
    rotate_after: u64,
    rss_ceiling_mb: u64,
    request_deadline: Duration,
) -> Result<u8, FrontendError> {
    let mut worker = Worker::spawn(prepared)?;
    let result = (|| {
        let mut served = 0;
        // Set whenever the worker is replaced, and read by the next request's
        // span: that request recompiles every library module from cold.
        let mut followed_rotation = false;
        // Counts RSS-driven replacements that lost a fresh worker's memo
        // before it had served `EARLY_REPLACEMENT_SERVED_THRESHOLD` requests.
        let mut early_replacements = 0u64;
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
                if stamp_changed(config, boot_stamp)? {
                    socket.retire()?;
                    break;
                }
                let mut response = Vec::with_capacity(72);
                response.extend_from_slice(PREFLIGHT_RESPONSE);
                response.extend_from_slice(producer);
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
                socket.retire()?;
                break;
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
                        socket.retire()?;
                        break;
                    }
                    Err(error) => {
                        log_reject_failure(
                            run_id,
                            "daemon stopping (transaction)",
                            write_rejected(&mut connection, "daemon stopping"),
                        );
                        return Err(error);
                    }
                }
                if connection.write_all(&[ACCEPTED]).is_err() || connection.flush().is_err() {
                    continue;
                }
                log_send_failure(
                    run_id,
                    "transaction read timeout",
                    connection.set_read_timeout(Some(IO_TIMEOUT)),
                );
                let outcome = service_transaction(
                    connection,
                    &mut worker,
                    0,
                    prepared,
                    config,
                    run_id,
                    request_deadline,
                    rotate_after,
                    rss_ceiling_mb,
                    true,
                    &mut served,
                    &mut followed_rotation,
                    &mut early_replacements,
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
                    socket.retire()?;
                    break;
                }
                Err(error) => {
                    log_reject_failure(
                        run_id,
                        "daemon stopping (request)",
                        write_rejected(&mut connection, "daemon stopping"),
                    );
                    return Err(error);
                }
            }
            if connection.write_all(&[ACCEPTED]).is_err() || connection.flush().is_err() {
                continue;
            }
            log_send_failure(
                run_id,
                "request read timeout",
                connection.set_read_timeout(Some(IO_TIMEOUT)),
            );
            let mut first_request = Some((cwd, worker_argv));
            let outcome = service_transaction(
                connection,
                &mut worker,
                0,
                prepared,
                config,
                run_id,
                request_deadline,
                rotate_after,
                rss_ceiling_mb,
                false,
                &mut served,
                &mut followed_rotation,
                &mut early_replacements,
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

    worker.shutdown();
    result
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

/// Result of one bounded attempt to fetch the next job for a pooled worker
/// slot: a real job, the accept thread having shut down (`Disconnected`), or
/// `Idle` — this slot held `job_rx`'s receiver for a full
/// `WARM_UP_POLL_INTERVAL` and found no job. Acquiring the receiver itself
/// still blocks (a plain, fair `Mutex::lock`, not a `try_lock` retry loop):
/// with only a couple of slots sharing one receiver, a thread that releases
/// and immediately re-acquires a `try_lock` in a sleep/retry cycle can starve
/// another slot indefinitely, since nothing about `try_lock` guarantees the
/// two threads' independent poll timers ever land in the brief gap between
/// release and re-acquire. Blocking on the OS mutex instead lets the kernel
/// arbitrate fairly between waiters.
enum PollOutcome {
    Job(Job),
    Disconnected,
    Idle,
}

/// Fetch the next job with a bounded wait, instead of blocking indefinitely
/// on `job_rx`, so an idle slot periodically comes back out to check whether
/// it should run its own pre-warm compile (`should_attempt_warm_up`) between
/// attempts.
fn poll_for_job(job_rx: &Mutex<std::sync::mpsc::Receiver<Job>>) -> PollOutcome {
    let receiver = job_rx.lock().unwrap_or_else(|poison| poison.into_inner());
    match receiver.recv_timeout(WARM_UP_POLL_INTERVAL) {
        Ok(job) => PollOutcome::Job(job),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => PollOutcome::Idle,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => PollOutcome::Disconnected,
    }
}

/// The N-worker daemon loop. One accept thread performs every fence check
/// (epoch, watched-stamp) and PREFLIGHT/STOP handling exactly as
/// `serve_single` does, then hands an accepted, already-fenced connection to
/// a free worker slot over a rendezvous channel (`sync_channel(0)`): the
/// accept thread's `send` blocks until some idle slot's thread calls `recv`,
/// which is the bounded queue of depth one the design calls for — an
/// over-subscribed daemon backs up in the kernel's own listen backlog, never
/// in an unbounded set of spawned threads.
///
/// Only reachable with `config.persistent` set (see `serve`): every rotation
/// and transaction-failure path in `service_transaction` replaces a
/// persistent worker in place and returns `ConnectionOutcome::Continue`,
/// never `Retire` — a persistent slot never asks to retire the whole
/// endpoint, so no cross-thread signal back to the accept loop is needed for
/// that case.
///
/// STOP and a watched-stamp change are handled the same way: the accept
/// thread stops accepting, drops the job sender (each slot's thread then
/// exits its loop once its current job, if any, finishes — bounded by that
/// job's own request deadline, so this never blocks past the existing
/// per-request bound), joins every slot thread, and only then calls
/// `socket.retire()` to drain and reject whatever is left queued — exactly
/// the "stop accepting, let in-flight finish, reject queued, then exit"
/// order the design calls for, built entirely from mechanisms this module
/// already had.
#[allow(clippy::too_many_arguments)]
fn serve_pooled(
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
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<Job>(0);
    let job_rx = Mutex::new(job_rx);
    // Populated with the first real request's (cwd, argv) any slot serves,
    // daemon-wide. The other idle slots (still on their first, `served ==
    // 0` worker) use it to run a pre-warm compile before their own first
    // real request arrives — see `warm_up_slot` and `should_attempt_warm_up`.
    let include_set: std::sync::Arc<std::sync::OnceLock<(std::path::PathBuf, Vec<OsString>)>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    std::thread::scope(|scope| -> Result<u8, FrontendError> {
        let mut slots = Vec::with_capacity(worker_count);
        for slot in 0..worker_count {
            let job_rx = &job_rx;
            let include_set = std::sync::Arc::clone(&include_set);
            slots.push(scope.spawn(move || -> Result<(), FrontendError> {
                let mut worker = Worker::spawn(prepared)?;
                let mut served = 0u64;
                // The first request this slot ever serves is cold, exactly
                // like a freshly rotated single-worker daemon.
                let mut followed_rotation = true;
                // Counts this slot's RSS-driven replacements that lost a
                // fresh worker's memo before it warmed up.
                let mut early_replacements = 0u64;
                // Set once this slot has attempted its own pre-warm compile
                // (successfully or not), so it is attempted at most once.
                let mut warm_up_attempted = false;
                // Consecutive confirmed-empty polls this slot itself has
                // observed while holding the shared receiver — see
                // `WARM_UP_IDLE_DEBOUNCE`. Once `served` leaves 0 the count
                // no longer matters (`should_attempt_warm_up` excludes it).
                let mut idle_polls = 0u32;
                loop {
                    // A job pending for this slot always wins over starting
                    // a pre-warm compile: check for one first, and only
                    // consider warm-up once a poll comes back confirmed
                    // idle.
                    let job = loop {
                        match poll_for_job(job_rx) {
                            PollOutcome::Job(job) => break Some(job),
                            PollOutcome::Disconnected => break None,
                            PollOutcome::Idle => {
                                idle_polls = idle_polls.saturating_add(1);
                            }
                        }
                        if should_attempt_warm_up(
                            served,
                            warm_up_attempted,
                            idle_polls,
                            include_set.get().is_some(),
                        ) {
                            if let Some((cwd, argv)) = include_set.get() {
                                warm_up_slot(&mut worker, prepared, cwd, argv, run_id, slot);
                            }
                            warm_up_attempted = true;
                        }
                    };
                    let Some(job) = job else {
                        // The accept thread dropped the sender: shutting down.
                        break;
                    };
                    let (connection, transaction, mut first_request) = match job {
                        Job::Transaction(connection) => (connection, true, None),
                        Job::Request(connection, cwd, argv) => {
                            // Other idle slots pre-warm from whichever
                            // request (plain or transaction-pinned) reveals
                            // the include set first.
                            include_set.get_or_init(|| (cwd.clone(), argv.clone()));
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
                                            Ok(argv) => {
                                                // Other idle slots pre-warm from
                                                // whichever request (plain or
                                                // transaction-pinned) reveals the
                                                // include set first.
                                                include_set
                                                    .get_or_init(|| (cwd.clone(), argv.clone()));
                                                RequestStep::Request(cwd, argv)
                                            }
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
                                    Some((cwd, argv)) => RequestStep::Request(cwd, argv),
                                    None => RequestStep::End,
                                }
                            }
                        },
                    );
                    match outcome {
                        Ok(ConnectionOutcome::Continue) => {}
                        Ok(ConnectionOutcome::Retire) => unreachable!(
                            "a persistent worker slot never retires the whole endpoint \
                             (rotation always replaces it in place under config.persistent)"
                        ),
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

        let outcome: Result<(), FrontendError> = 'accept: loop {
            let (mut connection, _) = match listener.accept() {
                Ok(connection) => connection,
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
                let mut response = Vec::with_capacity(72);
                response.extend_from_slice(PREFLIGHT_RESPONSE);
                response.extend_from_slice(producer);
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
                if connection.write_all(&[ACCEPTED]).is_err() || connection.flush().is_err() {
                    continue;
                }
                log_send_failure(
                    run_id,
                    "transaction read timeout",
                    connection.set_read_timeout(Some(IO_TIMEOUT)),
                );
                if job_tx.send(Job::Transaction(connection)).is_err() {
                    // Every slot has already exited (each hit a fatal,
                    // unrecoverable error); nothing left can serve requests.
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
            if connection.write_all(&[ACCEPTED]).is_err() || connection.flush().is_err() {
                continue;
            }
            log_send_failure(
                run_id,
                "request read timeout",
                connection.set_read_timeout(Some(IO_TIMEOUT)),
            );
            if job_tx
                .send(Job::Request(connection, cwd, worker_argv))
                .is_err()
            {
                break 'accept Ok(());
            }
        };

        // Stop accepting, then let every slot finish whatever job it is
        // currently on (bounded by that request's own deadline) before this
        // thread rejects anything still queued behind the accept loop.
        drop(job_tx);
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

    /// The pure gate a pooled slot's warm-up decision reduces to: only an
    /// unserved, not-yet-attempted, confirmed-idle slot with a known
    /// include set warms up.
    #[test]
    fn should_attempt_warm_up_requires_unserved_unattempted_debounced_and_known() {
        // Nothing known yet: never warm up, no matter how idle.
        assert!(!should_attempt_warm_up(0, false, 100, false));
        // Known, but this slot already served (or already has) a request:
        // it has its own real memo, warming up would be redundant.
        assert!(!should_attempt_warm_up(1, false, 100, true));
        // Known and idle long enough, but already attempted once.
        assert!(!should_attempt_warm_up(0, true, 100, true));
        // Known but not yet debounced: a job may still be inbound.
        assert!(!should_attempt_warm_up(
            0,
            false,
            WARM_UP_IDLE_DEBOUNCE - 1,
            true
        ));
        // Every condition satisfied.
        assert!(should_attempt_warm_up(
            0,
            false,
            WARM_UP_IDLE_DEBOUNCE,
            true
        ));
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
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync polling loop waiting for the daemon/fake worker, not async code"
            )]
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

    /// Compile a fake worker (rustc, matching the style of the fixtures
    /// above) that acks `begin_transaction`, reads exactly one request,
    /// sleeps `sleep_ms`, and answers successfully — once. A `--workers 2`
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

    stdin.read_exact(&mut one).unwrap(); // begin_transaction
    stdout.write_all(&[1]).unwrap();
    stdout.flush().unwrap();

    stdin.read_exact(&mut one).unwrap(); // request prefix
    let mut payload = vec![0u8; {payload_len}];
    stdin.read_exact(&mut payload).unwrap();

    std::thread::sleep(std::time::Duration::from_millis({sleep_ms}));

    stdout.write_all(&[0u8; 12]).unwrap(); // code=0, empty stdout/stderr frames
    stdout.flush().unwrap();

    stdin.read_exact(&mut one).unwrap(); // end_transaction
    stdout.write_all(&[1]).unwrap();
    stdout.flush().unwrap();
}}
"#,
                payload_len = payload_len,
                sleep_ms = sleep_ms,
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

    /// The pre-warm behaviour this section exists for: a `--workers 2` pool
    /// serves exactly one real client request, so only one slot's worker
    /// ever sees a real job — but after the idle debounce clears, the
    /// *other* slot's own worker independently completes one full
    /// begin/request/end cycle against the same include set on its own,
    /// with no client involved. A multi-shot fake worker (unlike the other
    /// fixtures' one-shot fakes) logs its own pid AND the hex request
    /// payload it received on every completed cycle (parsing the wire's
    /// length-prefixed frames itself, since the pre-warm's redirected
    /// request is not the same byte length as the real one), so this test
    /// asserts on the *decoded* requests: two distinct worker processes each
    /// served exactly one transaction, and the pre-warm's own request wrote
    /// its output to a different directory than the real request's —
    /// verifying `ExtractRequest::redirect_outputs_for_warm_up` actually ran
    /// rather than the real request's argv being replayed verbatim.
    #[test]
    fn an_idle_pooled_slot_pre_warms_from_the_first_requests_include_set() {
        let dir = std::env::temp_dir().join(format!("tp-prewarm-{}", std::process::id()));
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let stamp = dir.join("stamp");
        std::fs::write(&stamp, b"boot").unwrap();
        // An output-path field (here `--output-dir`) is exactly what a
        // verbatim replay would get wrong: it names a real, caller-owned
        // directory the pre-warm compile must never write into.
        let real_out_dir = dir.join("real-out");
        let argv = vec![
            OsString::from("Expr.hs"),
            OsString::from("--output-dir"),
            real_out_dir.clone().into_os_string(),
        ];
        let served_log = dir.join("served.log");
        let source = dir.join("fake_worker.rs");
        std::fs::write(
            &source,
            r#"
use std::io::{Read, Write};

fn read_u32(stdin: &mut impl Read) -> u32 {
    let mut buf = [0u8; 4];
    stdin.read_exact(&mut buf).unwrap();
    u32::from_le_bytes(buf)
}

fn read_frame(stdin: &mut impl Read) -> Vec<u8> {
    let len = read_u32(stdin) as usize;
    let mut buf = vec![0u8; len];
    stdin.read_exact(&mut buf).unwrap();
    buf
}

fn main() {
    let log_path = std::env::var("TP_TEST_PREWARM_SERVED_LOG").unwrap();
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    loop {
        let mut one = [0u8; 1];
        // Daemon shutdown closes stdin; a clean EOF here ends this worker.
        if stdin.read_exact(&mut one).is_err() {
            break;
        }
        stdout.write_all(&[1]).unwrap(); // begin_transaction ack
        stdout.flush().unwrap();

        stdin.read_exact(&mut one).unwrap(); // request prefix
        let _cwd = read_frame(&mut stdin); // frame(cwd)
        let argc = read_u32(&mut stdin);
        let mut args = Vec::with_capacity(argc as usize);
        for _ in 0..argc {
            args.push(String::from_utf8(read_frame(&mut stdin)).unwrap());
        }

        stdout.write_all(&[0u8; 12]).unwrap(); // code=0, empty stdout/stderr frames
        stdout.flush().unwrap();

        stdin.read_exact(&mut one).unwrap(); // end_transaction
        stdout.write_all(&[1]).unwrap();
        stdout.flush().unwrap();

        // args[0] is the worker-request flag, args[1] the hex payload.
        let payload = args.get(1).cloned().unwrap_or_default();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .unwrap();
        writeln!(file, "{} {}", std::process::id(), payload).unwrap();
    }
}
"#,
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

        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: points the fake worker at its own log file, not production config"
        )]
        std::env::set_var("TP_TEST_PREWARM_SERVED_LOG", &served_log);

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

        // Exactly one real client request: only one of the two slots ever
        // sees a real job.
        let output = execute(&socket, &binding.epoch, &dir, &argv).unwrap();
        assert_eq!(output.status.code(), Some(0));

        // Give the other, still-idle slot time to clear the debounce
        // (`WARM_UP_IDLE_DEBOUNCE` confirmed-empty polls) and run its
        // pre-warm compile, generously bounded.
        let settle_deadline = Instant::now() + Duration::from_secs(10);
        let served = loop {
            let served: Vec<String> = std::fs::read_to_string(&served_log)
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect();
            if served.len() >= 2 || Instant::now() >= settle_deadline {
                break served;
            }
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync polling loop waiting for the daemon/fake worker, not async code"
            )]
            std::thread::sleep(Duration::from_millis(20));
        };

        assert!(request_stop(&socket).is_ok());
        assert_eq!(server.join().unwrap().unwrap(), 0);

        assert_eq!(
            served.len(),
            2,
            "expected the real request plus one pre-warm compile, got: {served:?}"
        );
        let pids: Vec<&str> = served
            .iter()
            .map(|line| line.split_once(' ').expect("pid and payload").0)
            .collect();
        let distinct_pids: std::collections::HashSet<_> = pids.iter().collect();
        assert_eq!(
            distinct_pids.len(),
            2,
            "the pre-warm must run on the OTHER slot's own idle worker process, \
             not the one that already served the real request: {served:?}"
        );

        // Decode both requests' output directory the same way the daemon
        // itself does, rather than pattern-matching the hex payload.
        let output_dirs: Vec<Option<std::path::PathBuf>> = served
            .iter()
            .map(|line| {
                let (_pid, payload) = line.split_once(' ').expect("pid and payload");
                let argv = vec![
                    OsString::from(crate::request::WORKER_REQUEST_FLAG),
                    OsString::from(payload),
                ];
                ExtractRequest::decode_worker_argv(&argv)
                    .expect("both requests were encoded by this crate's own worker_argv")
                    .output_directory()
                    .map(std::path::PathBuf::from)
            })
            .collect();
        let matching_real_dir = output_dirs
            .iter()
            .filter(|dir| dir.as_deref() == Some(real_out_dir.as_path()))
            .count();
        assert_eq!(
            matching_real_dir,
            1,
            "exactly the real client request should write to {}: {output_dirs:?}",
            real_out_dir.display()
        );
        let redirected = output_dirs
            .iter()
            .find(|dir| dir.is_some() && dir.as_deref() != Some(real_out_dir.as_path()))
            .and_then(|dir| dir.as_deref())
            .expect("the pre-warm compile's own request should carry a redirected output dir");
        assert!(
            redirected.starts_with(std::env::temp_dir()),
            "the pre-warm's redirected output dir should live under a scratch \
             temp directory, not {}",
            redirected.display()
        );

        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: cleans up its own env var, not production config"
        )]
        std::env::remove_var("TP_TEST_PREWARM_SERVED_LOG");
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `STOP` with two requests in flight across two worker slots lets both
    /// finish before the daemon exits — the pooled daemon's drain applies to
    /// every slot, not just whichever one happened to accept the connection.
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
        let a = crate::CompilerIdentity::daemon([3; 32], [4; 32]);
        let b = crate::CompilerIdentity::daemon([3; 32], [5; 32]);
        assert_eq!(a.producer_bytes(), b.producer_bytes());
        assert_ne!(a.as_bytes(), b.as_bytes());
    }
}
