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
//! # Oneshot vs. the end-state registry
//!
//! E1 drives the **oneshot** shape: `render = Json`, `retention = DropAfterDone`,
//! an empty `ModuleEnv` (no declaration accumulation), one pool slot per turn.
//! Only the stateless MCP eval server drives the engine today; the resident
//! REPL server stays a direct consumer of the lower session substrate
//! ([`super::SessionLib`] / [`super::compile_session_turn`]) with a parked
//! worker thread, because unifying its resident-machine model onto this
//! spawn-per-turn engine is not a behavior-preserving change — that convergence
//! is E2 (see the seam below).
//! The end-state registry entry the API is aimed at is
//! `{machine, ModuleEnv, render policy, retention, pool slot}`;
//! [`RenderPolicy`] and [`Retention`] are carried on [`EngineConfig`] as that
//! forward seam even though a oneshot turn pins them. Render policy is applied
//! at source-wrapping time (the server picks `toJSON` vs `Show`-default before
//! handing the engine a wrapped `source`), so the engine does not branch on it
//! today; retention is inherent — a completed oneshot thread exits and drops its
//! machine.
//!
//! # E2 seam — thread parking → machine stowing
//!
//! Today a suspended turn (`Ask` or timeout-pause) is a **real OS thread parked**
//! on its `response_rx` / pause gate, held live in a [`Continuation`]. E2
//! replaces that with a *stowed machine*: the machine-state TL's T3 work makes a
//! `JitEffectMachine` suspendable, so a [`Continuation`] would carry a stowed
//! machine instead of a blocked thread, and [`SessionEngine::drive`]'s park path
//! would stow-and-return rather than leave a thread on the gate. The
//! [`Continuation`] struct and that park path are the exact edit sites for E2.
//!
//! # Machine-scoped diagnostics — pending cutover
//!
//! The eval-thread error/panic arms call the ambient
//! [`crate::drain_diagnostics`], and [`AskDispatcher`]'s gate-abort checkpoint
//! calls the ambient `tidepool_codegen::host_fns::set_first_cause`. The
//! machine-state TL is moving those thread-locals onto per-machine state; once
//! that lands, these engine-internal call sites switch to
//! `JitEffectMachine::drain_diagnostics(&self)` / a machine-scoped first-cause
//! setter. Note the cross-thread reality: `set_first_cause` fires from the
//! dispatcher's stack frame, so the machine-scoped setter must reach the right
//! machine from there. Until then the ambient fns stay valid.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use parking_lot::Mutex;
use tokio::time::{timeout, Duration};

use tidepool_bridge::{FromCore, ToCore};
use tidepool_effect::pause::PauseGate;

use crate::{
    classify, value_to_json, CancelHandle, DispatchEffect, FailureClass, Phase, EVAL_STACK_SIZE,
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
    /// Render via `Show`/`toWire` — the resident REPL surface (E2).
    Show,
}

/// Whether the machine is dropped when its turn ends or kept resident. Oneshot
/// pins [`Retention::DropAfterDone`] (the eval thread exits, freeing its heap);
/// [`Retention::Persistent`] is the resident-REPL end-state (E2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retention {
    /// Free the machine when the turn completes.
    DropAfterDone,
    /// Keep the machine resident across turns.
    Persistent,
}

// ---------------------------------------------------------------------------
// Thread ↔ driver messages
// ---------------------------------------------------------------------------

/// Messages from the eval thread to the async driver.
pub(crate) enum EngineMessage {
    /// The program hit `Ask` and is parked waiting for a response. `meta`
    /// carries `AskWith` metadata (e.g. a `"schema"` key) as JSON.
    Suspended {
        prompt: String,
        meta: Option<serde_json::Value>,
    },
    /// The program completed successfully.
    Completed { result: String },
    /// The program failed, pre-classified on the eval thread (which still holds
    /// the structured [`crate::RuntimeError`]) so a wire-format skew stays
    /// `version-skew`/`compile` instead of being re-guessed from text.
    Error {
        error: String,
        class: FailureClass,
        phase: Phase,
    },
}

/// Messages from the driver back to the parked eval thread. `Answer` carries the
/// CANONICAL validated JSON (the validator's parse, single source of truth);
/// `Abort` terminates the ask as a handler error.
pub(crate) enum ResumeMsg {
    Answer(serde_json::Value),
    Abort(String),
}

/// What a parked continuation is waiting for — decides resume semantics.
enum ContinuationKind {
    /// Parked on an `Ask`: resume validates the reply against `expected_schema`
    /// (if any) and sends it down the channel.
    AwaitingAnswer {
        expected_schema: Option<serde_json::Value>,
    },
    /// Paused at an effect boundary (timeout-as-yield): resume wakes the gate
    /// and waits another window (its payload is ignored — sending on the channel
    /// would poison the next ask); abort wakes the gate with an error.
    Paused,
}

/// A suspended turn, waiting for a resume/abort call. The E2 seam: this holds a
/// parked OS thread today; it will hold a stowed machine instead.
struct Continuation<O> {
    response_tx: Sender<ResumeMsg>,
    session_rx: tokio::sync::mpsc::UnboundedReceiver<EngineMessage>,
    source: Arc<str>,
    created_at: std::time::Instant,
    captured: O,
    kind: ContinuationKind,
    /// The eval thread's join handle, carried across park/resume cycles so abort
    /// (and crash forensics) can reap it.
    thread: Option<JoinHandle<()>>,
    /// The pause gate shared with the eval thread's dispatcher.
    gate: Arc<PauseGate>,
}

// ---------------------------------------------------------------------------
// Turn outcome (what the server maps to wire)
// ---------------------------------------------------------------------------

/// The classified result of driving a turn to its first yield. Every variant is
/// pure data — the server renders it.
pub enum TurnOutcome {
    /// The program returned a value. `result` is the rendered value body.
    Completed {
        output: Vec<String>,
        result: String,
    },
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
    /// The `Ask` effect's union tag, intercepted by the dispatcher.
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
pub struct SessionEngine<O> {
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

    /// Evict the oldest continuation, freeing its pool slot. AwaitingAnswer:
    /// dropping the entry drops `response_tx` → the blocked thread's
    /// `recv()` returns `Err` → it exits → permit freed. Paused: the thread is
    /// parked on the gate's condvar (dropping alone would leak it parked
    /// forever) — wake it with an abort and reap.
    fn evict_oldest_continuation(&self) {
        let mut conts = self.continuations.lock();
        if let Some(oldest_key) = conts
            .iter()
            .min_by_key(|(_, s)| s.created_at)
            .map(|(k, _)| k.clone())
        {
            log::info!("evicting oldest continuation {oldest_key} under pressure");
            if let Some(session) = conts.remove(&oldest_key) {
                if matches!(session.kind, ContinuationKind::Paused) {
                    session
                        .gate
                        .request_abort("evicted under pressure while paused".into());
                    self.reap_detached(session.thread);
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

        // Channels + pause gate for this turn.
        let (session_tx, session_rx) = tokio::sync::mpsc::unbounded_channel::<EngineMessage>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        let gate = PauseGate::new();
        let gate_for_thread = Arc::clone(&gate);

        // Filled with the machine's JIT cancel handle once built (see `on_ready`);
        // the timeout path reads it to abort a pure runaway at a safepoint.
        let cancel_slot: Arc<Mutex<Option<CancelHandle>>> = Arc::new(Mutex::new(None));
        let cancel_slot_thread = Arc::clone(&cancel_slot);

        // Acquire a pool slot; under pressure, evict the oldest suspended turn.
        let permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                self.evict_oldest_continuation();
                tokio::task::yield_now().await;
                match self.semaphore.clone().try_acquire_owned() {
                    Ok(p) => p,
                    Err(_) => return Err(StartError::Busy),
                }
            }
        };

        let source_for_thread = Arc::clone(&source);
        let captured_for_thread = captured.clone();
        let thread_session_tx = session_tx;

        let handle = std::thread::Builder::new()
            .name("tidepool-eval".into())
            .stack_size(EVAL_STACK_SIZE)
            .spawn(move || {
                let _permit = permit;
                // Catch SIGILL/SIGSEGV from JIT code instead of killing the host.
                tidepool_codegen::signal_safety::install();

                let include_paths: Vec<&Path> =
                    include.iter().map(std::path::PathBuf::as_path).collect();
                let gate_phase = Arc::clone(&gate_for_thread);
                let mut dispatcher = AskDispatcher {
                    inner: handlers,
                    ask_tag,
                    session_tx: thread_session_tx.clone(),
                    response_rx,
                    gate: gate_for_thread,
                };

                // Compile starts now; the cancel-handle installer fires at
                // machine creation — the compile→run boundary — so a timeout
                // before it is a slow compile, not a runaway.
                gate_phase.set_compiling(true);
                let gate_run = Arc::clone(&gate_phase);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::compile_and_run_cancellable(
                        &source_for_thread,
                        "result",
                        &include_paths,
                        &mut dispatcher,
                        &captured_for_thread,
                        nursery_size,
                        |h| {
                            gate_run.set_compiling(false);
                            *cancel_slot_thread.lock() = Some(h);
                        },
                    )
                }));
                gate_phase.set_compiling(false);

                match result {
                    Ok(Ok(eval_result)) => {
                        let _ = thread_session_tx.send(EngineMessage::Completed {
                            result: eval_result.to_string_pretty(),
                        });
                    }
                    Ok(Err(e)) => {
                        // Classify from the STRUCTURED error while we still hold
                        // it — a wire-format skew becomes version-skew/compile
                        // with a self-diagnosing message, not re-guessed text.
                        let env = classify(&e);
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
                        let _ = thread_session_tx.send(EngineMessage::Error {
                            error: detail,
                            class: env.class,
                            phase: env.phase,
                        });
                    }
                    Err(panic_payload) => {
                        // A panic unwinding out of the JIT is a run-phase engine
                        // crash (a caught signal that still took the frame down).
                        let diagnostics = crate::drain_diagnostics();
                        let mut detail = format_panic_payload(panic_payload);
                        if !diagnostics.is_empty() {
                            detail.push_str("\n\n## JIT Diagnostics\n");
                            for d in &diagnostics {
                                detail.push_str(d);
                                detail.push('\n');
                            }
                        }
                        let _ = thread_session_tx.send(EngineMessage::Error {
                            error: detail,
                            class: FailureClass::Runtime,
                            phase: Phase::Run,
                        });
                    }
                }
            })
            .map_err(|_| StartError::Busy)?;

        Ok(self
            .drive(
                session_rx,
                source,
                response_tx,
                captured,
                Some(handle),
                gate,
                timeout_secs,
                cancel_slot,
            )
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
        enum Consumed<O, V> {
            Session(Continuation<O>),
            Invalid(V),
        }
        let consumed = {
            let mut conts = self.continuations.lock();
            match conts.get(cont_id) {
                None => return ResumeOutcome::NotFound,
                // Paused: nothing listens on the channel (sending would poison
                // the next ask). Consume, wake the gate, wait another window.
                Some(Continuation {
                    kind: ContinuationKind::Paused,
                    ..
                }) => {
                    let session = conts
                        .remove(cont_id)
                        .expect("present: checked under the same lock");
                    session.gate.resume_run();
                    Consumed::Session(session)
                }
                Some(Continuation {
                    kind: ContinuationKind::AwaitingAnswer { expected_schema },
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
                            Consumed::Invalid(v)
                        }
                        Ok(canonical) => {
                            let session = conts
                                .remove(cont_id)
                                .expect("present: checked under the same lock");
                            match session.response_tx.send(ResumeMsg::Answer(canonical)) {
                                Ok(()) => Consumed::Session(session),
                                Err(_) => return ResumeOutcome::ThreadGone,
                            }
                        }
                    }
                }
            }
        };

        let session = match consumed {
            Consumed::Invalid(v) => return ResumeOutcome::Invalid(v),
            Consumed::Session(s) => s,
        };
        ResumeOutcome::Driven(self.drive_continuation(session).await)
    }

    /// Abort a parked continuation: wake it (answer-channel `Abort` for an ask,
    /// gate abort for a pause) and drive it to its terminal error outcome.
    pub async fn abort(&self, cont_id: &str, reason: String) -> AbortOutcome {
        let session = {
            let mut conts = self.continuations.lock();
            match conts.remove(cont_id) {
                None => return AbortOutcome::NotFound,
                Some(s) => s,
            }
        };

        match &session.kind {
            ContinuationKind::AwaitingAnswer { .. } => {
                if session.response_tx.send(ResumeMsg::Abort(reason)).is_err() {
                    return AbortOutcome::ThreadGone;
                }
            }
            ContinuationKind::Paused => {
                session
                    .gate
                    .request_abort(format!("aborted by caller (while paused): {reason}"));
            }
        }
        AbortOutcome::Driven(self.drive_continuation(session).await)
    }

    /// Drive a resumed/aborted continuation with the default window. There is no
    /// JIT cancel handle to forward (the original thread holds its own; a
    /// resumed runaway falls back to the detach path).
    async fn drive_continuation(&self, session: Continuation<O>) -> TurnOutcome {
        self.drive(
            session.session_rx,
            session.source,
            session.response_tx,
            session.captured,
            session.thread,
            session.gate,
            self.config.default_timeout_secs,
            Arc::new(Mutex::new(None)),
        )
        .await
    }

    /// Drive a turn to its first outcome, with the window set by `timeout_secs`.
    /// At the window an eval at (or reaching) an effect boundary parks as a
    /// continuation; a pure runaway is detached.
    #[allow(clippy::too_many_arguments)]
    async fn drive(
        &self,
        mut session_rx: tokio::sync::mpsc::UnboundedReceiver<EngineMessage>,
        source: Arc<str>,
        response_tx: Sender<ResumeMsg>,
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
                // (e.g. an Ask suspend just as we timed out) — drain it rather
                // than pausing a thread already parked on the answer channel.
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
                                    response_tx,
                                    session_rx,
                                    source,
                                    created_at: std::time::Instant::now(),
                                    captured,
                                    kind: ContinuationKind::Paused,
                                    thread: handle.take(),
                                    gate,
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
                        gate.request_abort("detached after timeout (no yield point reached)".into());
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
                    EngineMessage::Suspended { .. } => captured.snapshot(),
                };
                match message {
                    EngineMessage::Completed { result } => TurnOutcome::Completed { output, result },
                    EngineMessage::Suspended { prompt, meta } => {
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
                                response_tx,
                                session_rx,
                                source,
                                created_at: std::time::Instant::now(),
                                captured,
                                kind: ContinuationKind::AwaitingAnswer { expected_schema },
                                thread: handle.take(),
                                gate,
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
// Ask-effect dispatcher (parks the eval thread on suspend)
// ---------------------------------------------------------------------------

/// Wraps a handler stack and intercepts the `Ask` tag: on `Ask`, emits a
/// [`EngineMessage::Suspended`] and blocks the eval thread on `response_rx`
/// until the driver answers or aborts. Every dispatch entry is also a
/// timeout-yield checkpoint (the shared [`PauseGate`]).
struct AskDispatcher<H> {
    inner: H,
    ask_tag: u64,
    session_tx: tokio::sync::mpsc::UnboundedSender<EngineMessage>,
    response_rx: Receiver<ResumeMsg>,
    gate: Arc<PauseGate>,
}

impl<H: DispatchEffect<O>, O> DispatchEffect<O> for AskDispatcher<H> {
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
        let result = self.dispatch_inner(tag, request, cx);
        self.gate.exit_effect();
        result
    }
}

impl<H> AskDispatcher<H> {
    fn dispatch_inner<O>(
        &mut self,
        tag: u64,
        request: &tidepool_eval::value::Value,
        cx: &tidepool_effect::dispatch::EffectContext<'_, O>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError>
    where
        H: DispatchEffect<O>,
    {
        if tag == self.ask_tag {
            let (prompt, meta) = extract_ask_request(request, cx.table())
                .map_err(tidepool_effect::error::EffectError::Handler)?;
            let _ = self
                .session_tx
                .send(EngineMessage::Suspended { prompt, meta });
            // Block until the driver sends a canonical (already validated) reply.
            let msg = self.response_rx.recv().map_err(|_| {
                tidepool_effect::error::EffectError::Handler(
                    "Ask session closed (timeout or client disconnected)".into(),
                )
            })?;
            match msg {
                ResumeMsg::Answer(json_val) => {
                    let core_val = json_val
                        .to_value(cx.table())
                        .map_err(tidepool_effect::error::EffectError::Bridge)?;
                    Ok(core_val.into())
                }
                ResumeMsg::Abort(reason) => Err(tidepool_effect::error::EffectError::Handler(
                    format!("ask aborted by caller: {reason}"),
                )),
            }
        } else {
            self.inner.dispatch(tag, request, cx)
        }
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
    /// thread (handle `None`). Mirrors the retired mcp `handle_session_result`
    /// unit tests, now asserting the STRUCTURED outcome the server maps to wire.
    async fn drive_one(
        engine: &SessionEngine<TestSink>,
        captured: TestSink,
        message: EngineMessage,
    ) -> (TurnOutcome, Sender<ResumeMsg>) {
        let (session_tx, session_rx) = tokio::sync::mpsc::unbounded_channel::<EngineMessage>();
        let (response_tx, _response_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        session_tx.send(message).unwrap();
        let outcome = engine
            .drive(
                session_rx,
                "src".into(),
                response_tx.clone(),
                captured,
                None,
                PauseGate::new(),
                600,
                Arc::new(Mutex::new(None)),
            )
            .await;
        (outcome, response_tx)
    }

    #[tokio::test]
    async fn completed_splits_output_and_result() {
        let engine = test_engine();
        let (outcome, _tx) = drive_one(
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
        let (outcome, _tx) = drive_one(
            &engine,
            TestSink::with(&["pre"]),
            EngineMessage::Suspended {
                prompt: "pick".into(),
                meta: Some(meta.clone()),
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
        match &entry.kind {
            ContinuationKind::AwaitingAnswer { expected_schema } => {
                assert_eq!(expected_schema, &Some(serde_json::json!({"type": "string"})));
            }
            _ => panic!("expected AwaitingAnswer"),
        }
    }

    #[tokio::test]
    async fn error_carries_class_phase_and_output() {
        let engine = test_engine();
        let (outcome, _tx) = drive_one(
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
    /// one consumes it; a third resume finds nothing. Mirrors the retired
    /// mcp `test_resume_validation_fail_then_retry` wire lifecycle.
    #[tokio::test]
    async fn resume_validation_fail_then_retry_then_gone() {
        let engine = test_engine();
        // Register a suspended continuation directly (no live thread; the answer
        // channel receiver is kept so `send` on resume succeeds).
        let (response_tx, response_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        let (session_tx, session_rx) = tokio::sync::mpsc::unbounded_channel::<EngineMessage>();
        engine.continuations.lock().insert(
            "cont_1".into(),
            Continuation {
                response_tx,
                session_rx,
                source: "src".into(),
                created_at: std::time::Instant::now(),
                captured: TestSink::default(),
                kind: ContinuationKind::AwaitingAnswer {
                    expected_schema: Some(serde_json::json!({"type": "string"})),
                },
                thread: None,
                gate: PauseGate::new(),
            },
        );
        // No live eval thread: dropping the sender closes the channel, so the
        // post-resume `drive` observes the "thread gone" close immediately
        // instead of blocking the whole window.
        drop(session_tx);

        // 1. Invalid reply: continuation NOT consumed.
        let r = engine
            .resume::<_, String>("cont_1", |_schema| Err("nope".to_string()))
            .await;
        assert!(matches!(r, ResumeOutcome::Invalid(v) if v == "nope"));
        assert!(engine.continuations.lock().contains_key("cont_1"));

        // 2. Valid reply: canonical crosses the channel; continuation consumed;
        //    drive then observes the thread gone (channel closed) → Crashed.
        let r = engine
            .resume::<_, String>("cont_1", |_schema| Ok(serde_json::json!("ok")))
            .await;
        assert!(matches!(r, ResumeOutcome::Driven(_)));
        assert!(matches!(
            response_rx.recv(),
            Ok(ResumeMsg::Answer(v)) if v == serde_json::json!("ok")
        ));
        assert!(!engine.continuations.lock().contains_key("cont_1"));

        // 3. Third resume: nothing there.
        let r = engine
            .resume::<_, String>("cont_1", |_schema| Ok(serde_json::json!("ok")))
            .await;
        assert!(matches!(r, ResumeOutcome::NotFound));
    }

    /// The eval thread's channel closing with no message (the thread died) is a
    /// crash: partial output is surfaced, no panic payload without a joinable
    /// handle.
    #[tokio::test]
    async fn closed_channel_is_crashed() {
        let engine = test_engine();
        let (session_tx, session_rx) = tokio::sync::mpsc::unbounded_channel::<EngineMessage>();
        let (response_tx, _response_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        drop(session_tx); // thread gone before any message
        let outcome = engine
            .drive(
                session_rx,
                "src".into(),
                response_tx,
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
        let (session_tx, session_rx) = tokio::sync::mpsc::unbounded_channel::<EngineMessage>();
        let (response_tx, _response_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        let outcome = engine
            .drive(
                session_rx,
                "src".into(),
                response_tx,
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
