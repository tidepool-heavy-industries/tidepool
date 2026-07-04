//! `SessionEngine` — the one substrate both servers drive a session turn through.
//!
//! A turn is a single `M a` program compiled to a JIT machine and run to its
//! first outcome: it completes, it suspends on an `Ask` (a coroutine
//! checkpoint), it pauses at the timeout-yield boundary, it errors, it times
//! out with no yield point, or it crashes. The engine owns the machinery that
//! is orthogonal to *what* the program computes: the eval thread, the
//! timeout→park→detach orchestration, the parked-continuation registry, the
//! concurrency pool (a semaphore + orphan reaper), and the `Ask`-effect
//! suspend/resume state machine.
//!
//! # Structured outcome, not wire text
//!
//! [`SessionEngine::start_turn`] / [`resume`](SessionEngine::resume) /
//! [`abort`](SessionEngine::abort) return a [`TurnOutcome`] — the *classified*
//! result, never a rendered response. The caller (an MCP server) maps each
//! variant to its own wire shape. This is deliberate: `tidepool-runtime` sits
//! below `rmcp` and below the per-server response formatters
//! (`format_with_output`, `build_suspension_envelope`, `format_error_with_source`),
//! so the engine cannot — and must not — produce the final `CallToolResult`.
//! Keeping every wire string on the server side is also what makes the cutover
//! behavior-preserving: the formatters (and their tests) are untouched.
//!
//! # E2 — threadless ask suspension (stowed machine)
//!
//! An `Ask` suspension no longer parks a blocked OS thread on an answer
//! channel. Instead the eval thread drives the turn through
//! [`crate::compile_and_run_suspendable`] to the ask boundary, where the JIT
//! effect machine — already a reified coroutine (its continuation is a heap
//! value; the native stack fully unwinds between yields) — is handed back as
//! DATA. The eval thread packages the stowed `JitEffectMachine` + its table +
//! the handler stack into a boxed resume closure ([`StowedResume`]), sends it in
//! [`EngineMessage::SuspendedAsk`], and EXITS. The [`Continuation`]'s
//! `AwaitingAnswer` state holds that closure (plus the turn's semaphore permit,
//! transferred off the exiting thread so the suspended session keeps its pool
//! slot). On [`resume`](SessionEngine::resume) a FRESH eval thread re-enters the
//! stowed machine via [`crate::resume_suspended_turn`] — the machine re-installs
//! its per-thread reach and re-points GC state at its RETAINED session heap
//! (never a nursery reset), then drives to the next boundary.
//!
//! The timeout-`Paused` suspension is unchanged: it parks the eval thread on the
//! gate mid-computation (NOT at an ask boundary — the native stack is live and
//! cannot be stowed), so that continuation still carries a live parked thread.
//! Only the ask boundary — a clean, reified yield point — goes threadless.
//!
//! # Oneshot vs. the end-state registry
//!
//! E1 drives the **oneshot** shape: `render = Json`, `retention = DropAfterDone`,
//! an empty `ModuleEnv` (no declaration accumulation), one pool slot per turn.
//! Only the stateless MCP eval server drives the engine today; the resident
//! REPL server stays a direct consumer of the lower session substrate
//! ([`super::SessionLib`] / [`super::compile_session_turn`]) with a parked
//! worker thread — unifying its resident-machine model onto this engine is a
//! separate step. The end-state registry entry the API is aimed at is
//! `{machine, ModuleEnv, render policy, retention, pool slot}`;
//! [`RenderPolicy`] and [`Retention`] are carried on [`EngineConfig`] as that
//! forward seam even though a oneshot turn pins them. Render policy is applied
//! at source-wrapping time (the server picks `toJSON` vs `Show`-default before
//! handing the engine a wrapped `source`), so the engine does not branch on it
//! today; retention is inherent — a completed oneshot thread exits and drops its
//! machine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use parking_lot::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::OwnedSemaphorePermit;
use tokio::time::{timeout, Duration};

use tidepool_bridge::{FromCore, ToCore};
use tidepool_effect::pause::PauseGate;

use crate::{
    classify, value_to_json, CancelHandle, DispatchEffect, FailureClass, Phase, ResumeInput,
    ResumedRun, RuntimeError, SuspendableRun, EVAL_STACK_SIZE,
};

// ---------------------------------------------------------------------------
// Output sink
// ---------------------------------------------------------------------------

/// The console-output buffer an effect handler writes into and the engine
/// drains when a turn yields. Abstracted so the engine stays below the server
/// crate that owns the concrete buffer (`tidepool_mcp::CapturedOutput`). The
/// buffer is `Clone` (Arc-backed) so the eval thread and the driver share one.
pub trait OutputSink: Clone + Send + 'static {
    /// Take all buffered lines, clearing the buffer.
    fn drain(&self) -> Vec<String>;
    /// Copy the buffered lines without clearing (a suspension keeps computing).
    fn snapshot(&self) -> Vec<String>;
}

// ---------------------------------------------------------------------------
// Registry-shape seams (RenderPolicy / Retention)
// ---------------------------------------------------------------------------

/// How a turn's result is rendered. The server applies this at source-wrapping
/// time (`toJSON` vs `Show`-default), so the engine carries it as the registry
/// seam rather than branching on it. Oneshot MCP eval pins [`RenderPolicy::Json`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderPolicy {
    /// Wrap the result in `toJSON` — the stateless eval contract.
    Json,
    /// Render via `Show`/`toWire` — the resident REPL surface.
    Show,
}

/// Whether the machine is dropped when its turn ends or kept resident. Oneshot
/// pins [`Retention::DropAfterDone`] (the eval thread exits, freeing its heap);
/// [`Retention::Persistent`] is the resident-REPL end-state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retention {
    /// Free the machine when the turn completes.
    DropAfterDone,
    /// Keep the machine resident across turns.
    Persistent,
}

// ---------------------------------------------------------------------------
// Eval-thread context + thread → driver messages
// ---------------------------------------------------------------------------

/// Everything a spawned eval thread needs to run one turn segment and report
/// back. Built by [`SessionEngine::run_turn`] and handed to the turn `body`.
/// The `permit` rides along so a body that suspends at an ask can transfer it
/// into the stowed continuation (keeping the suspended session's pool slot)
/// rather than releasing it on thread exit.
struct EvalThreadCtx<O: OutputSink> {
    session_tx: UnboundedSender<EngineMessage<O>>,
    gate: Arc<PauseGate>,
    cancel_slot: Arc<Mutex<Option<CancelHandle>>>,
    captured: O,
    permit: OwnedSemaphorePermit,
}

/// A stowed ask-suspension: re-enter the captured machine with an answer or an
/// abort, on a fresh eval thread. Boxed so the registry need not be generic
/// over the handler stack `H` — the closure captures the concrete
/// `JitEffectMachine` + `DataConTable` + handlers and erases them here.
type StowedResume<O> = Box<dyn FnOnce(EvalThreadCtx<O>, EngineResumeInput) + Send>;

/// Engine-level resume input: the validated answer (still JSON — converted to a
/// core `Value` inside the stowed closure, which owns the table) or an abort.
enum EngineResumeInput {
    Answer(serde_json::Value),
    Abort(String),
}

/// Messages from an eval thread to the async driver.
enum EngineMessage<O: OutputSink> {
    /// The program hit `Ask` and stowed itself. `resume` re-enters the captured
    /// machine; `permit` is the turn's pool slot, transferred off the (now
    /// exiting) eval thread so the suspended session keeps its slot.
    SuspendedAsk {
        prompt: String,
        meta: Option<serde_json::Value>,
        resume: StowedResume<O>,
        permit: OwnedSemaphorePermit,
    },
    /// The program completed successfully.
    Completed { result: String },
    /// The program failed, pre-classified on the eval thread (which still holds
    /// the structured [`RuntimeError`]) so a wire-format skew stays
    /// `version-skew`/`compile` instead of being re-guessed from text.
    Error {
        error: String,
        class: FailureClass,
        phase: Phase,
    },
}

/// What a parked continuation is waiting for — decides resume semantics.
enum ContinuationState<O: OutputSink> {
    /// Paused at the timeout-yield boundary: a real eval thread is parked on the
    /// gate mid-computation (NOT an ask boundary, so the native stack is live
    /// and cannot be stowed). Resume wakes the gate and drives the same thread;
    /// abort wakes it with an error.
    Paused {
        session_rx: UnboundedReceiver<EngineMessage<O>>,
        thread: Option<JoinHandle<()>>,
        gate: Arc<PauseGate>,
    },
    /// Suspended at an `Ask`: STOWED as data (E2). No thread. `resume` re-enters
    /// on a fresh thread; `permit` is the session's held pool slot, handed to
    /// that thread. Resume validates the reply against `expected_schema` first.
    AwaitingAnswer {
        expected_schema: Option<serde_json::Value>,
        resume: StowedResume<O>,
        permit: OwnedSemaphorePermit,
    },
}

/// A suspended turn, waiting for a resume/abort call.
struct Continuation<O: OutputSink> {
    source: Arc<str>,
    created_at: std::time::Instant,
    captured: O,
    state: ContinuationState<O>,
}

// ---------------------------------------------------------------------------
// Turn outcome (what the server maps to wire)
// ---------------------------------------------------------------------------

/// The classified result of driving a turn to its first yield. Every variant is
/// pure data — the server renders it.
pub enum TurnOutcome {
    /// The program returned a value. `result` is the rendered value body.
    Completed { output: Vec<String>, result: String },
    /// The program suspended on `Ask`. The continuation is already registered
    /// under `cont_id` (with `expected_schema = meta["schema"]`); the server
    /// builds the suspension envelope from `prompt`/`meta`.
    SuspendedAsk {
        cont_id: String,
        prompt: String,
        meta: Option<serde_json::Value>,
        output: Vec<String>,
    },
    /// The turn paused at the timeout-yield boundary. The continuation is
    /// registered under `cont_id`; `timeout_secs` is the window that elapsed.
    Paused {
        cont_id: String,
        output: Vec<String>,
        timeout_secs: u64,
    },
    /// The program failed. `class`/`phase` were stamped from the structured
    /// error on the eval thread; `source` is the wrapped module for echoing.
    Error {
        class: FailureClass,
        phase: Phase,
        detail: String,
        output: Vec<String>,
        source: Arc<str>,
    },
    /// The window expired with no yield point; the thread was detached.
    /// `compiling` distinguishes a slow-GHC compile (`Infra`/`Compile`) from a
    /// pure runaway (`Runtime`/`Run`) — `class`/`phase` carry that decision.
    TimedOut {
        class: FailureClass,
        phase: Phase,
        compiling: bool,
        timeout_secs: u64,
        output: Vec<String>,
        source: Arc<str>,
    },
    /// The eval thread died (a caught signal that still took the frame down).
    /// `thread_panic` is the formatted panic payload if the handle was joinable.
    Crashed {
        output: Vec<String>,
        thread_panic: Option<String>,
        source: Arc<str>,
    },
}

/// Outcome of [`SessionEngine::resume`]. `V` is the server's validation-failure
/// payload (returned verbatim so the server renders its own retry body).
pub enum ResumeOutcome<V> {
    /// No continuation under that id (unknown or already consumed/expired).
    NotFound,
    /// The reply failed schema validation; the continuation is NOT consumed.
    Invalid(V),
    /// The eval thread is gone (its receiver dropped before the answer landed).
    ThreadGone,
    /// The turn was driven to its next outcome.
    Driven(TurnOutcome),
}

/// Outcome of [`SessionEngine::abort`].
pub enum AbortOutcome {
    /// No continuation under that id.
    NotFound,
    /// The eval thread is gone (its answer receiver dropped).
    ThreadGone,
    /// The abort was delivered and the turn driven to its terminal outcome.
    Driven(TurnOutcome),
}

/// Why [`SessionEngine::start_turn`] declined to run a turn (admission control).
pub enum StartError {
    /// Too many timed-out evals are still being reaped — shed load.
    Overloaded,
    /// Every pool slot is busy and none could be evicted.
    Busy,
}

// ---------------------------------------------------------------------------
// Start-turn request
// ---------------------------------------------------------------------------

/// Everything the engine needs to spawn and drive one oneshot turn. The server
/// does all source PREP (preamble, imports, render-policy wrapping, input
/// injection, lib fault-isolation) and hands the wrapped module here.
pub struct StartTurn<H, O> {
    /// The fully-wrapped module source (target binder `result`).
    pub source: Arc<str>,
    /// GHC include search paths.
    pub include: Vec<PathBuf>,
    /// The effect handler stack for this turn (moved onto the eval thread).
    pub handlers: H,
    /// The `Ask` effect's union tag, intercepted by the suspend driver.
    pub ask_tag: u64,
    /// Effect names by tag, for annotating an `UnhandledEffect` error.
    pub effect_names: Vec<String>,
    /// The console-output buffer this turn writes into.
    pub captured: O,
    /// JIT nursery size.
    pub nursery_size: usize,
    /// The turn window (already clamped by the caller).
    pub timeout_secs: u64,
}

// ---------------------------------------------------------------------------
// Engine config + type
// ---------------------------------------------------------------------------

/// Engine tunables. `cont_prefix` distinguishes ids across servers (`cont_`
/// here, `scont_` for the resident session server).
pub struct EngineConfig {
    pub max_concurrent: usize,
    pub max_orphaned: usize,
    pub cont_prefix: String,
    pub default_timeout_secs: u64,
    /// Registry-shape seam (see [`RenderPolicy`]).
    pub render: RenderPolicy,
    /// Registry-shape seam (see [`Retention`]).
    pub retention: Retention,
}

/// The session-turn driver. Generic over the output sink `O` so it stays below
/// the server crate that owns the concrete buffer.
pub struct SessionEngine<O: OutputSink> {
    continuations: Arc<Mutex<HashMap<String, Continuation<O>>>>,
    next_cont_id: Arc<AtomicU64>,
    orphaned_threads: Arc<AtomicUsize>,
    semaphore: Arc<tokio::sync::Semaphore>,
    config: EngineConfig,
}

impl<O: OutputSink> SessionEngine<O> {
    /// Build an engine with the given pool sizing + id prefix.
    pub fn new(config: EngineConfig) -> Self {
        SessionEngine {
            continuations: Arc::new(Mutex::new(HashMap::new())),
            next_cont_id: Arc::new(AtomicU64::new(1)),
            orphaned_threads: Arc::new(AtomicUsize::new(0)),
            semaphore: Arc::new(tokio::sync::Semaphore::new(config.max_concurrent)),
            config,
        }
    }

    /// How many timed-out threads are still being reaped (the server's admission
    /// gate reads this to shed load before even building source).
    pub fn orphaned_count(&self) -> usize {
        self.orphaned_threads.load(Ordering::Relaxed)
    }

    /// The default turn window.
    pub fn default_timeout_secs(&self) -> u64 {
        self.config.default_timeout_secs
    }

    fn next_continuation_id(&self) -> String {
        let id = self.next_cont_id.fetch_add(1, Ordering::Relaxed);
        format!("{}_{}", self.config.cont_prefix, id)
    }

    /// Evict the oldest continuation, freeing its pool slot. Paused: the thread
    /// is parked on the gate — wake it with an abort and reap. AwaitingAnswer
    /// (E2): no thread — dropping the entry drops the stowed machine + releases
    /// the held `permit`, freeing the slot directly.
    fn evict_oldest_continuation(&self) {
        let mut conts = self.continuations.lock();
        if let Some(oldest_key) = conts
            .iter()
            .min_by_key(|(_, s)| s.created_at)
            .map(|(k, _)| k.clone())
        {
            log::info!("evicting oldest continuation {oldest_key} under pressure");
            if let Some(session) = conts.remove(&oldest_key) {
                match session.state {
                    ContinuationState::Paused { thread, gate, .. } => {
                        gate.request_abort("evicted under pressure while paused".into());
                        self.reap_detached(thread);
                    }
                    ContinuationState::AwaitingAnswer { .. } => {
                        // Dropping `session` drops the resume closure (freeing
                        // the stowed machine) and the `permit` (freeing the slot).
                    }
                }
            }
        }
    }

    /// Detach an eval thread to a background reaper: a grace period, then join,
    /// with orphan accounting (admission refuses new turns when too many
    /// detached threads still run). `std::thread`, not `spawn_blocking` — a
    /// tight infinite loop must not starve the runtime's blocking pool.
    fn reap_detached(&self, handle: Option<JoinHandle<()>>) {
        if let Some(h) = handle {
            let orphan_count = Arc::clone(&self.orphaned_threads);
            orphan_count.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(2));
                let _ = h.join();
                orphan_count.fetch_sub(1, Ordering::Relaxed);
            });
        }
    }

    /// Acquire a pool slot; under pressure, evict the oldest suspended turn and
    /// retry once. Mirrors the E1 admission dance.
    async fn acquire_permit(&self) -> Result<OwnedSemaphorePermit, StartError> {
        match self.semaphore.clone().try_acquire_owned() {
            Ok(p) => Ok(p),
            Err(_) => {
                self.evict_oldest_continuation();
                tokio::task::yield_now().await;
                match self.semaphore.clone().try_acquire_owned() {
                    Ok(p) => Ok(p),
                    Err(_) => Err(StartError::Busy),
                }
            }
        }
    }

    /// Spawn an eval thread running `body` and drive it to its first outcome.
    /// Shared by [`start_turn`](Self::start_turn) (fresh compile+run) and
    /// [`resume`](Self::resume)/[`abort`](Self::abort) (re-entry of a stowed
    /// machine). The `permit` is moved into the thread's [`EvalThreadCtx`] so a
    /// body that suspends can transfer it into the stowed continuation.
    async fn run_turn(
        &self,
        body: impl FnOnce(EvalThreadCtx<O>) + Send + 'static,
        captured: O,
        source: Arc<str>,
        permit: OwnedSemaphorePermit,
        timeout_secs: u64,
    ) -> TurnOutcome {
        let (session_tx, session_rx) = tokio::sync::mpsc::unbounded_channel::<EngineMessage<O>>();
        let gate = PauseGate::new();
        let cancel_slot: Arc<Mutex<Option<CancelHandle>>> = Arc::new(Mutex::new(None));

        let gate_thread = Arc::clone(&gate);
        let cancel_thread = Arc::clone(&cancel_slot);
        let captured_thread = captured.clone();

        let handle = std::thread::Builder::new()
            .name("tidepool-eval".into())
            .stack_size(EVAL_STACK_SIZE)
            .spawn(move || {
                // Catch SIGILL/SIGSEGV from JIT code instead of killing the host.
                tidepool_codegen::signal_safety::install();
                body(EvalThreadCtx {
                    session_tx,
                    gate: gate_thread,
                    cancel_slot: cancel_thread,
                    captured: captured_thread,
                    permit,
                });
            })
            .expect("failed to spawn eval thread");

        self.drive(
            session_rx,
            source,
            captured,
            Some(handle),
            gate,
            timeout_secs,
            cancel_slot,
        )
        .await
    }

    /// Spawn and drive one oneshot turn to its first outcome.
    pub async fn start_turn<H>(&self, turn: StartTurn<H, O>) -> Result<TurnOutcome, StartError>
    where
        H: DispatchEffect<O> + Send + 'static,
    {
        if self.orphaned_threads.load(Ordering::Relaxed) >= self.config.max_orphaned {
            return Err(StartError::Overloaded);
        }

        let StartTurn {
            source,
            include,
            handlers,
            ask_tag,
            effect_names,
            captured,
            nursery_size,
            timeout_secs,
        } = turn;

        let permit = self.acquire_permit().await?;

        let source_for_thread = Arc::clone(&source);
        let body = move |ctx: EvalThreadCtx<O>| {
            let EvalThreadCtx {
                session_tx,
                gate,
                cancel_slot,
                captured,
                permit,
            } = ctx;

            let include_paths: Vec<&Path> =
                include.iter().map(std::path::PathBuf::as_path).collect();

            // Compile starts now; the cancel-handle installer fires at machine
            // creation — the compile→run boundary — so a timeout before it is a
            // slow compile, not a runaway.
            gate.set_compiling(true);
            let gate_run = Arc::clone(&gate);
            let cancel_cb = Arc::clone(&cancel_slot);
            let mut wrapped = GateDispatcher {
                inner: handlers,
                gate: Arc::clone(&gate),
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::compile_and_run_suspendable(
                    &source_for_thread,
                    "result",
                    &include_paths,
                    &mut wrapped,
                    &captured,
                    nursery_size,
                    ask_tag,
                    |h| {
                        gate_run.set_compiling(false);
                        *cancel_cb.lock() = Some(h);
                    },
                )
            }));
            gate.set_compiling(false);

            let msg = match result {
                Ok(Ok(SuspendableRun::Completed(eval_result))) => {
                    drop(permit);
                    EngineMessage::Completed {
                        result: eval_result.to_string_pretty(),
                    }
                }
                Ok(Ok(SuspendableRun::Suspended {
                    machine,
                    table,
                    request,
                })) => build_suspended_message::<O, H>(
                    machine,
                    table,
                    wrapped,
                    request,
                    permit,
                    effect_names,
                    ask_tag,
                ),
                Ok(Err(e)) => {
                    drop(permit);
                    let (error, class, phase) = describe_run_error(&e, &effect_names);
                    EngineMessage::Error {
                        error,
                        class,
                        phase,
                    }
                }
                Err(panic_payload) => {
                    drop(permit);
                    let (error, class, phase) = describe_panic(panic_payload);
                    EngineMessage::Error {
                        error,
                        class,
                        phase,
                    }
                }
            };
            let _ = session_tx.send(msg);
        };

        Ok(self
            .run_turn(body, captured, source, permit, timeout_secs)
            .await)
    }

    /// Resume a parked continuation. `validate` is given the continuation's
    /// `expected_schema` and returns the canonical answer or a failure payload;
    /// it runs UNDER the registry lock (validate-then-consume is atomic — a
    /// failing reply must not consume the one-shot continuation, and two
    /// concurrent resumes must not both pass).
    pub async fn resume<F, V>(&self, cont_id: &str, validate: F) -> ResumeOutcome<V>
    where
        F: FnOnce(Option<&serde_json::Value>) -> Result<serde_json::Value, V>,
    {
        enum Taken<O: OutputSink> {
            Paused {
                session_rx: UnboundedReceiver<EngineMessage<O>>,
                thread: Option<JoinHandle<()>>,
                gate: Arc<PauseGate>,
                captured: O,
                source: Arc<str>,
            },
            Ask {
                resume: StowedResume<O>,
                permit: OwnedSemaphorePermit,
                captured: O,
                source: Arc<str>,
                canonical: serde_json::Value,
            },
        }

        let taken = {
            let mut conts = self.continuations.lock();
            match conts.get(cont_id) {
                None => return ResumeOutcome::NotFound,
                // Paused: nothing to validate. Consume, wake the gate, re-drive.
                Some(Continuation {
                    state: ContinuationState::Paused { .. },
                    ..
                }) => {
                    let session = conts
                        .remove(cont_id)
                        .expect("present: checked under the same lock");
                    let ContinuationState::Paused {
                        session_rx,
                        thread,
                        gate,
                    } = session.state
                    else {
                        unreachable!("matched Paused above")
                    };
                    gate.resume_run();
                    Taken::Paused {
                        session_rx,
                        thread,
                        gate,
                        captured: session.captured,
                        source: session.source,
                    }
                }
                Some(Continuation {
                    state:
                        ContinuationState::AwaitingAnswer {
                            expected_schema, ..
                        },
                    ..
                }) => {
                    let expected_schema = expected_schema.clone();
                    match validate(expected_schema.as_ref()) {
                        Err(v) => {
                            // Anti-starvation: a retrying continuation must not be
                            // the oldest-first eviction victim while its caller
                            // fixes the reply.
                            if let Some(session) = conts.get_mut(cont_id) {
                                session.created_at = std::time::Instant::now();
                            }
                            return ResumeOutcome::Invalid(v);
                        }
                        Ok(canonical) => {
                            let session = conts
                                .remove(cont_id)
                                .expect("present: checked under the same lock");
                            let ContinuationState::AwaitingAnswer { resume, permit, .. } =
                                session.state
                            else {
                                unreachable!("matched AwaitingAnswer above")
                            };
                            Taken::Ask {
                                resume,
                                permit,
                                captured: session.captured,
                                source: session.source,
                                canonical,
                            }
                        }
                    }
                }
            }
        };

        let timeout_secs = self.config.default_timeout_secs;
        match taken {
            Taken::Paused {
                session_rx,
                thread,
                gate,
                captured,
                source,
            } => ResumeOutcome::Driven(
                self.drive(
                    session_rx,
                    source,
                    captured,
                    thread,
                    gate,
                    timeout_secs,
                    Arc::new(Mutex::new(None)),
                )
                .await,
            ),
            Taken::Ask {
                resume,
                permit,
                captured,
                source,
                canonical,
            } => {
                let body =
                    move |ctx: EvalThreadCtx<O>| resume(ctx, EngineResumeInput::Answer(canonical));
                ResumeOutcome::Driven(
                    self.run_turn(body, captured, source, permit, timeout_secs)
                        .await,
                )
            }
        }
    }

    /// Abort a parked continuation: Paused wakes its parked thread with a gate
    /// abort and re-drives it to its terminal error; AwaitingAnswer (E2)
    /// re-enters the stowed machine with an abort input on a fresh thread,
    /// producing the same terminal error a pre-E2 answer-channel abort did.
    pub async fn abort(&self, cont_id: &str, reason: String) -> AbortOutcome {
        let session = {
            let mut conts = self.continuations.lock();
            match conts.remove(cont_id) {
                None => return AbortOutcome::NotFound,
                Some(s) => s,
            }
        };

        let timeout_secs = self.config.default_timeout_secs;
        match session.state {
            ContinuationState::Paused {
                session_rx,
                thread,
                gate,
            } => {
                gate.request_abort(format!("aborted by caller (while paused): {reason}"));
                AbortOutcome::Driven(
                    self.drive(
                        session_rx,
                        session.source,
                        session.captured,
                        thread,
                        gate,
                        timeout_secs,
                        Arc::new(Mutex::new(None)),
                    )
                    .await,
                )
            }
            ContinuationState::AwaitingAnswer { resume, permit, .. } => {
                let body =
                    move |ctx: EvalThreadCtx<O>| resume(ctx, EngineResumeInput::Abort(reason));
                AbortOutcome::Driven(
                    self.run_turn(body, session.captured, session.source, permit, timeout_secs)
                        .await,
                )
            }
        }
    }

    /// Drive a turn to its first outcome, with the window set by `timeout_secs`.
    /// At the window an eval reaching an effect boundary parks as a `Paused`
    /// continuation; a pure runaway is detached. An ask suspension arrives
    /// pre-stowed as [`EngineMessage::SuspendedAsk`].
    #[allow(clippy::too_many_arguments)]
    async fn drive(
        &self,
        mut session_rx: UnboundedReceiver<EngineMessage<O>>,
        source: Arc<str>,
        captured: O,
        mut handle: Option<JoinHandle<()>>,
        gate: Arc<PauseGate>,
        timeout_secs: u64,
        cancel_slot: Arc<Mutex<Option<CancelHandle>>>,
    ) -> TurnOutcome {
        let received = match timeout(Duration::from_secs(timeout_secs), session_rx.recv()).await {
            Ok(received) => received,
            Err(_elapsed) => {
                // The window expired. A message may have raced the deadline
                // (e.g. an ask suspend just as we timed out) — drain it rather
                // than pausing a thread that already sent and exited.
                match session_rx.try_recv() {
                    Ok(message) => Some(message),
                    Err(_) => {
                        // A timeout is a YIELD POINT, not a failure: ask the eval
                        // thread to pause at its next effect dispatch.
                        gate.request_pause();
                        let gate_for_wait = Arc::clone(&gate);
                        let parked = tokio::task::spawn_blocking(move || {
                            gate_for_wait.parked_or_in_effect(Duration::from_secs(2))
                        })
                        .await
                        .unwrap_or(false);

                        let output = captured.snapshot();
                        if parked {
                            let cont_id = self.next_continuation_id();
                            self.continuations.lock().insert(
                                cont_id.clone(),
                                Continuation {
                                    source,
                                    created_at: std::time::Instant::now(),
                                    captured,
                                    state: ContinuationState::Paused {
                                        session_rx,
                                        thread: handle.take(),
                                        gate,
                                    },
                                },
                            );
                            return TurnOutcome::Paused {
                                cont_id,
                                output,
                                timeout_secs,
                            };
                        }

                        // Pure-compute runaway: no effect dispatch within the
                        // grace period — nothing to park at. Detach to the reaper;
                        // flip the JIT cancel flag (polled at GC/tail-call
                        // safepoints, unlike the effect-only gate) so a pure
                        // runaway aborts at its next safepoint and EXITS, freeing
                        // its permit instead of pinning it forever.
                        gate.request_abort(
                            "detached after timeout (no yield point reached)".into(),
                        );
                        if let Some(cancel) = cancel_slot.lock().as_ref() {
                            cancel.cancel();
                        }
                        self.reap_detached(handle.take());
                        // Compile-phase timeout = slow-GHC infra, not the user's
                        // code; run-phase = a runtime pure loop past the window.
                        let compiling = gate.is_compiling();
                        let (class, phase) = if compiling {
                            (FailureClass::Infra, Phase::Compile)
                        } else {
                            (FailureClass::Runtime, Phase::Run)
                        };
                        return TurnOutcome::TimedOut {
                            class,
                            phase,
                            compiling,
                            timeout_secs,
                            output,
                            source,
                        };
                    }
                }
            }
        };

        match received {
            Some(message) => {
                let output = match &message {
                    EngineMessage::Completed { .. } | EngineMessage::Error { .. } => {
                        captured.drain()
                    }
                    EngineMessage::SuspendedAsk { .. } => captured.snapshot(),
                };
                match message {
                    EngineMessage::Completed { result } => {
                        TurnOutcome::Completed { output, result }
                    }
                    EngineMessage::SuspendedAsk {
                        prompt,
                        meta,
                        resume,
                        permit,
                    } => {
                        // The eval thread stowed itself and is exiting; detach
                        // its handle (nothing to join across the suspension).
                        let _ = handle.take();
                        let cont_id = self.next_continuation_id();
                        // `expected_schema` mirrors the server's envelope hoist:
                        // the `"schema"` key of an object `meta`, else `None`.
                        let expected_schema = meta
                            .as_ref()
                            .and_then(serde_json::Value::as_object)
                            .and_then(|o| o.get("schema").cloned());
                        self.continuations.lock().insert(
                            cont_id.clone(),
                            Continuation {
                                source,
                                created_at: std::time::Instant::now(),
                                captured,
                                state: ContinuationState::AwaitingAnswer {
                                    expected_schema,
                                    resume,
                                    permit,
                                },
                            },
                        );
                        TurnOutcome::SuspendedAsk {
                            cont_id,
                            prompt,
                            meta,
                            output,
                        }
                    }
                    EngineMessage::Error {
                        error,
                        class,
                        phase,
                    } => TurnOutcome::Error {
                        class,
                        phase,
                        detail: error,
                        output,
                        source,
                    },
                }
            }
            None => {
                // The channel closed with no message: the eval thread died.
                let output = captured.snapshot();
                let thread_panic = handle
                    .take()
                    .and_then(|h| h.join().err())
                    .map(format_panic_payload);
                TurnOutcome::Crashed {
                    output,
                    thread_panic,
                    source,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Eval-thread outcome → message helpers (shared by start + resume bodies)
// ---------------------------------------------------------------------------

/// Build an [`EngineMessage::SuspendedAsk`] from a stowed machine: extract the
/// prompt/meta from the bridged `request`, and box a [`StowedResume`] closure
/// capturing the machine + table + (unwrapped) handlers for the next re-entry.
/// `H` here is the WRAPPED [`GateDispatcher`] stack; `into_inner` recovers the
/// base handlers so each turn re-wraps with its own fresh gate.
fn build_suspended_message<O, H>(
    machine: tidepool_codegen::jit_machine::JitEffectMachine,
    table: tidepool_repr::DataConTable,
    wrapped: GateDispatcher<H>,
    request: tidepool_eval::value::Value,
    permit: OwnedSemaphorePermit,
    effect_names: Vec<String>,
    ask_tag: u64,
) -> EngineMessage<O>
where
    O: OutputSink,
    H: DispatchEffect<O> + Send + 'static,
{
    let (prompt, meta) = match extract_ask_request(&request, &table) {
        Ok(pm) => pm,
        Err(e) => {
            drop(permit);
            return EngineMessage::Error {
                error: format!("ask request malformed: {e}"),
                class: FailureClass::Runtime,
                phase: Phase::Run,
            };
        }
    };
    let resume = make_resume_closure::<O, H>(machine, table, wrapped.inner, effect_names, ask_tag);
    EngineMessage::SuspendedAsk {
        prompt,
        meta,
        resume,
        permit,
    }
}

/// Box a re-entry closure for a stowed machine. When invoked with a fresh
/// [`EvalThreadCtx`] and an answer/abort, it re-installs the machine's reach,
/// drives to the next boundary, and sends the resulting [`EngineMessage`] —
/// re-stowing (recursively) if it suspends at a further ask.
fn make_resume_closure<O, H>(
    mut machine: tidepool_codegen::jit_machine::JitEffectMachine,
    table: tidepool_repr::DataConTable,
    base: H,
    effect_names: Vec<String>,
    ask_tag: u64,
) -> StowedResume<O>
where
    O: OutputSink,
    H: DispatchEffect<O> + Send + 'static,
{
    Box::new(move |ctx: EvalThreadCtx<O>, input: EngineResumeInput| {
        let EvalThreadCtx {
            session_tx,
            gate,
            cancel_slot,
            captured,
            permit,
        } = ctx;

        // Convert the engine-level resume input to codegen's, using the stowed
        // table to bridge a JSON answer to a core Value.
        let codegen_input = match input {
            EngineResumeInput::Answer(json) => match json.to_value(&table) {
                Ok(v) => ResumeInput::Answer(v),
                Err(e) => {
                    drop(permit);
                    let _ = session_tx.send(EngineMessage::Error {
                        error: format!("ask answer could not be bridged to a value: {e}"),
                        class: FailureClass::Runtime,
                        phase: Phase::Run,
                    });
                    return;
                }
            },
            EngineResumeInput::Abort(reason) => ResumeInput::Abort(reason),
        };

        let mut wrapped = GateDispatcher {
            inner: base,
            gate: Arc::clone(&gate),
        };
        let cancel_cb = Arc::clone(&cancel_slot);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::resume_suspended_turn(
                &mut machine,
                &table,
                &mut wrapped,
                &captured,
                ask_tag,
                codegen_input,
                |h| {
                    *cancel_cb.lock() = Some(h);
                },
            )
        }));

        let msg = match result {
            Ok(Ok(ResumedRun::Completed(eval_result))) => {
                drop(permit);
                EngineMessage::Completed {
                    result: eval_result.to_string_pretty(),
                }
            }
            Ok(Ok(ResumedRun::Suspended { request })) => build_suspended_message::<O, H>(
                machine,
                table,
                wrapped,
                request,
                permit,
                effect_names,
                ask_tag,
            ),
            Ok(Err(e)) => {
                drop(permit);
                let (error, class, phase) = describe_run_error(&e, &effect_names);
                EngineMessage::Error {
                    error,
                    class,
                    phase,
                }
            }
            Err(panic_payload) => {
                drop(permit);
                let (error, class, phase) = describe_panic(panic_payload);
                EngineMessage::Error {
                    error,
                    class,
                    phase,
                }
            }
        };
        let _ = session_tx.send(msg);
    })
}

/// Classify a run error into `(detail, class, phase)` for an
/// [`EngineMessage::Error`], annotating an `UnhandledEffect` with the effect
/// name + roster and appending any JIT diagnostics. Byte-identical to the E1
/// eval-thread error arm.
fn describe_run_error(e: &RuntimeError, effect_names: &[String]) -> (String, FailureClass, Phase) {
    let env = classify(e);
    let diagnostics = crate::drain_diagnostics();
    let mut detail = env.message;
    // Annotate UnhandledEffect with the effect name + roster.
    if let Some(tag_str) = detail.strip_prefix("Unhandled effect at tag ") {
        if let Ok(tag) = tag_str.trim().parse::<usize>() {
            if tag < effect_names.len() {
                detail = format!("{} (effect: {})", detail, effect_names[tag]);
            }
        }
        let roster: String = effect_names
            .iter()
            .enumerate()
            .map(|(i, name)| format!("  {} = {}", i, name))
            .collect::<Vec<_>>()
            .join("\n");
        detail.push_str(&format!("\n\nRegistered effects:\n{roster}"));
    }
    if !diagnostics.is_empty() {
        detail.push_str("\n\n## JIT Diagnostics\n");
        for d in &diagnostics {
            detail.push_str(d);
            detail.push('\n');
        }
    }
    (detail, env.class, env.phase)
}

/// Classify a caught panic (a signal that still took the eval frame down) as a
/// run-phase runtime crash, appending any JIT diagnostics. Byte-identical to
/// the E1 eval-thread panic arm.
fn describe_panic(payload: Box<dyn std::any::Any + Send>) -> (String, FailureClass, Phase) {
    let diagnostics = crate::drain_diagnostics();
    let mut detail = format_panic_payload(payload);
    if !diagnostics.is_empty() {
        detail.push_str("\n\n## JIT Diagnostics\n");
        for d in &diagnostics {
            detail.push_str(d);
            detail.push('\n');
        }
    }
    (detail, FailureClass::Runtime, Phase::Run)
}

// ---------------------------------------------------------------------------
// Gate dispatcher (timeout-yield checkpoint; ask is intercepted upstream)
// ---------------------------------------------------------------------------

/// Wraps a handler stack with the shared [`PauseGate`] timeout-yield checkpoint.
/// Unlike E1's `AskDispatcher`, it does NOT intercept the ask tag — that is now
/// handled by the codegen suspend driver (`ask_tag` → threadless suspension), so
/// the ask never reaches this dispatcher. Every non-ask dispatch entry is a
/// timeout-yield checkpoint: park while paused, error out on abort.
struct GateDispatcher<H> {
    inner: H,
    gate: Arc<PauseGate>,
}

impl<H: DispatchEffect<O>, O> DispatchEffect<O> for GateDispatcher<H> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &tidepool_eval::value::Value,
        cx: &tidepool_effect::dispatch::EffectContext<'_, O>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        // Yield point: park here while paused; error out on abort. A gate abort
        // is a cancellation, not a handler fault — record it in the JIT's
        // first-cause cell so the run boundary surfaces `Cancelled` regardless
        // of which cancellation channel fired first.
        self.gate.checkpoint().map_err(|reason| {
            tidepool_codegen::host_fns::set_first_cause(
                tidepool_codegen::host_fns::RuntimeError::Cancelled,
            );
            tidepool_effect::error::EffectError::Handler(reason)
        })?;
        let result = self.inner.dispatch(tag, request, cx);
        self.gate.exit_effect();
        result
    }
}

/// Extract the prompt (+ optional `AskWith` metadata) from an `Ask` request.
/// The request is `Con(AskWith, [prompt_val, meta_val])`, dispatched by
/// constructor name.
fn extract_ask_request(
    request: &tidepool_eval::value::Value,
    table: &tidepool_repr::DataConTable,
) -> Result<(String, Option<serde_json::Value>), String> {
    use tidepool_eval::value::Value;

    let Value::Con(con_id, fields) = request else {
        return Err(format!(
            "ask received unexpected request shape (expected Con(Ask|AskWith, ..)): {request:?}"
        ));
    };
    let con_name = table.name_of(*con_id).unwrap_or("<unknown>");
    // `ask` always suspends via AskWith (carrying the schema); the bare `Ask`
    // constructor was reaped with the structured-Ask collapse.
    match con_name {
        "AskWith" => {}
        other => {
            return Err(format!(
                "ask received unexpected constructor {other:?} (expected AskWith)"
            ))
        }
    }
    let Some(prompt_val) = fields.first() else {
        return Err(format!(
            "ask received unexpected request shape (expected Con({con_name}, ..)): {request:?}"
        ));
    };
    let prompt = String::from_value(prompt_val, table).map_err(|e| {
        format!(
            "ask prompt could not be evaluated to Text: {e}. The expression passed to `ask` \
             likely crashed during evaluation (check for unresolved externals or runtime errors \
             in the prompt string)."
        )
    })?;
    // Requests arrive fully forced from the JIT bridge, so the aeson Value
    // sub-tree is already materialized — render it directly.
    let meta = fields.get(1).map(|m| value_to_json(m, table, 0));
    Ok((prompt, meta))
}

/// Format a caught panic payload as a human string (downcast to `&str`/`String`).
fn format_panic_payload(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct TestSink {
        lines: Arc<Mutex<Vec<String>>>,
    }
    impl TestSink {
        fn with(lines: &[&str]) -> Self {
            TestSink {
                lines: Arc::new(Mutex::new(lines.iter().map(|s| s.to_string()).collect())),
            }
        }
    }
    impl OutputSink for TestSink {
        fn drain(&self) -> Vec<String> {
            std::mem::take(&mut *self.lines.lock())
        }
        fn snapshot(&self) -> Vec<String> {
            self.lines.lock().clone()
        }
    }

    fn test_engine() -> SessionEngine<TestSink> {
        SessionEngine::new(EngineConfig {
            max_concurrent: 4,
            max_orphaned: 10,
            cont_prefix: "cont".into(),
            // Small window: the harness feeds a message before driving, so this
            // is only a backstop against a silent channel hanging a test.
            default_timeout_secs: 5,
            render: RenderPolicy::Json,
            retention: Retention::DropAfterDone,
        })
    }

    /// A driver harness: feed one `EngineMessage`, then drive with no live
    /// thread (handle `None`). Asserts the STRUCTURED outcome the server maps to
    /// wire. Mirrors the retired mcp `handle_session_result` unit tests.
    async fn drive_one(
        engine: &SessionEngine<TestSink>,
        captured: TestSink,
        message: EngineMessage<TestSink>,
    ) -> TurnOutcome {
        let (session_tx, session_rx) =
            tokio::sync::mpsc::unbounded_channel::<EngineMessage<TestSink>>();
        session_tx.send(message).unwrap();
        // Hold the sender open so the channel does not read as a crash.
        engine
            .drive(
                session_rx,
                "src".into(),
                captured,
                None,
                PauseGate::new(),
                600,
                Arc::new(Mutex::new(None)),
            )
            .await
    }

    /// A dummy stowed-resume closure that immediately reports `Completed` — lets
    /// the AwaitingAnswer registry/validation lifecycle be tested without a real
    /// JIT machine.
    fn dummy_resume(result: &'static str) -> StowedResume<TestSink> {
        Box::new(
            move |ctx: EvalThreadCtx<TestSink>, _input: EngineResumeInput| {
                let EvalThreadCtx {
                    session_tx, permit, ..
                } = ctx;
                drop(permit);
                let _ = session_tx.send(EngineMessage::Completed {
                    result: result.to_string(),
                });
            },
        )
    }

    fn one_permit(engine: &SessionEngine<TestSink>) -> OwnedSemaphorePermit {
        engine
            .semaphore
            .clone()
            .try_acquire_owned()
            .expect("a permit is available")
    }

    #[tokio::test]
    async fn completed_splits_output_and_result() {
        let engine = test_engine();
        let outcome = drive_one(
            &engine,
            TestSink::with(&["log1"]),
            EngineMessage::Completed {
                result: "42".into(),
            },
        )
        .await;
        match outcome {
            TurnOutcome::Completed { output, result } => {
                assert_eq!(output, vec!["log1".to_string()]);
                assert_eq!(result, "42");
            }
            _ => panic!("expected Completed"),
        }
    }

    #[tokio::test]
    async fn suspended_registers_continuation_and_hoists_schema() {
        let engine = test_engine();
        let meta = serde_json::json!({"schema": {"type": "string"}, "moves": ["a"]});
        let permit = one_permit(&engine);
        let outcome = drive_one(
            &engine,
            TestSink::with(&["pre"]),
            EngineMessage::SuspendedAsk {
                prompt: "pick".into(),
                meta: Some(meta.clone()),
                resume: dummy_resume("ok"),
                permit,
            },
        )
        .await;
        let cont_id = match outcome {
            TurnOutcome::SuspendedAsk {
                cont_id,
                prompt,
                meta: out_meta,
                output,
            } => {
                assert_eq!(prompt, "pick");
                assert_eq!(out_meta, Some(meta));
                // Suspended keeps output un-drained (snapshot), not cleared.
                assert_eq!(output, vec!["pre".to_string()]);
                cont_id
            }
            _ => panic!("expected SuspendedAsk"),
        };
        // The continuation is registered, awaiting an answer, schema hoisted.
        let conts = engine.continuations.lock();
        let entry = conts.get(&cont_id).expect("continuation registered");
        match &entry.state {
            ContinuationState::AwaitingAnswer {
                expected_schema, ..
            } => {
                assert_eq!(
                    expected_schema,
                    &Some(serde_json::json!({"type": "string"}))
                );
            }
            _ => panic!("expected AwaitingAnswer"),
        }
    }

    #[tokio::test]
    async fn error_carries_class_phase_and_output() {
        let engine = test_engine();
        let outcome = drive_one(
            &engine,
            TestSink::with(&["oops"]),
            EngineMessage::Error {
                error: "boom".into(),
                class: FailureClass::Runtime,
                phase: Phase::Run,
            },
        )
        .await;
        match outcome {
            TurnOutcome::Error {
                class,
                phase,
                detail,
                output,
                ..
            } => {
                assert_eq!(class, FailureClass::Runtime);
                assert_eq!(phase, Phase::Run);
                assert_eq!(detail, "boom");
                assert_eq!(output, vec!["oops".to_string()]);
            }
            _ => panic!("expected Error"),
        }
    }

    /// A failing validator leaves the continuation in place (retryable); a valid
    /// one consumes it and drives the stowed closure to its outcome; a third
    /// resume finds nothing. Mirrors the retired mcp
    /// `test_resume_validation_fail_then_retry` wire lifecycle.
    #[tokio::test]
    async fn resume_validation_fail_then_retry_then_gone() {
        let engine = test_engine();
        let permit = one_permit(&engine);
        engine.continuations.lock().insert(
            "cont_1".into(),
            Continuation {
                source: "src".into(),
                created_at: std::time::Instant::now(),
                captured: TestSink::default(),
                state: ContinuationState::AwaitingAnswer {
                    expected_schema: Some(serde_json::json!({"type": "string"})),
                    resume: dummy_resume("resumed-value"),
                    permit,
                },
            },
        );

        // 1. Invalid reply: continuation NOT consumed.
        let r = engine
            .resume::<_, String>("cont_1", |_schema| Err("nope".to_string()))
            .await;
        assert!(matches!(r, ResumeOutcome::Invalid(v) if v == "nope"));
        assert!(engine.continuations.lock().contains_key("cont_1"));

        // 2. Valid reply: continuation consumed; the dummy closure runs on a
        //    fresh eval thread and reports Completed.
        let r = engine
            .resume::<_, String>("cont_1", |_schema| Ok(serde_json::json!("ok")))
            .await;
        match r {
            ResumeOutcome::Driven(TurnOutcome::Completed { result, .. }) => {
                assert_eq!(result, "resumed-value");
            }
            _ => panic!("expected Driven(Completed)"),
        }
        assert!(!engine.continuations.lock().contains_key("cont_1"));

        // 3. Third resume: nothing there.
        let r = engine
            .resume::<_, String>("cont_1", |_schema| Ok(serde_json::json!("ok")))
            .await;
        assert!(matches!(r, ResumeOutcome::NotFound));
    }

    /// Aborting a stowed ask continuation drives the dummy closure with an abort
    /// input to a terminal outcome (here the dummy still reports Completed; a
    /// real machine would surface the "ask aborted by caller" error).
    #[tokio::test]
    async fn abort_awaiting_answer_drives_to_terminal() {
        let engine = test_engine();
        let permit = one_permit(&engine);
        engine.continuations.lock().insert(
            "cont_9".into(),
            Continuation {
                source: "src".into(),
                created_at: std::time::Instant::now(),
                captured: TestSink::default(),
                state: ContinuationState::AwaitingAnswer {
                    expected_schema: None,
                    resume: dummy_resume("aborted"),
                    permit,
                },
            },
        );
        let r = engine.abort("cont_9", "stop".into()).await;
        assert!(matches!(r, AbortOutcome::Driven(_)));
        assert!(!engine.continuations.lock().contains_key("cont_9"));
    }

    #[tokio::test]
    async fn abort_unknown_is_not_found() {
        let engine = test_engine();
        assert!(matches!(
            engine.abort("nope", "x".into()).await,
            AbortOutcome::NotFound
        ));
    }

    /// The eval thread's channel closing with no message (the thread died) is a
    /// crash: partial output is surfaced, no panic payload without a joinable
    /// handle.
    #[tokio::test]
    async fn closed_channel_is_crashed() {
        let engine = test_engine();
        let (session_tx, session_rx) =
            tokio::sync::mpsc::unbounded_channel::<EngineMessage<TestSink>>();
        drop(session_tx); // thread gone before any message
        let outcome = engine
            .drive(
                session_rx,
                "src".into(),
                TestSink::with(&["last words"]),
                None,
                PauseGate::new(),
                5,
                Arc::new(Mutex::new(None)),
            )
            .await;
        match outcome {
            TurnOutcome::Crashed {
                output,
                thread_panic,
                ..
            } => {
                assert_eq!(output, vec!["last words".to_string()]);
                assert!(thread_panic.is_none());
            }
            _ => panic!("expected Crashed"),
        }
    }

    /// The window expiring with nothing sent and no thread parked at the gate is
    /// a pure runaway: the turn detaches, classified run-phase runtime, carrying
    /// partial output. (The sender is held open so the channel does not read as
    /// a crash.)
    #[tokio::test]
    async fn timeout_with_no_yield_point_detaches() {
        let engine = test_engine();
        let (session_tx, session_rx) =
            tokio::sync::mpsc::unbounded_channel::<EngineMessage<TestSink>>();
        let outcome = engine
            .drive(
                session_rx,
                "src".into(),
                TestSink::with(&["partial"]),
                None,
                PauseGate::new(),
                1,
                Arc::new(Mutex::new(None)),
            )
            .await;
        drop(session_tx);
        match outcome {
            TurnOutcome::TimedOut {
                class,
                phase,
                compiling,
                output,
                ..
            } => {
                assert_eq!(class, FailureClass::Runtime);
                assert_eq!(phase, Phase::Run);
                assert!(!compiling);
                assert_eq!(output, vec!["partial".to_string()]);
            }
            _ => panic!("expected TimedOut"),
        }
    }
}
