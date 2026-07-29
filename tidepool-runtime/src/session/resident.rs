//! `ResidentSession` — the `Retention::Persistent` end-state (segment 20).
//!
//! The E2 stow engine's oneshot path (`SessionEngine`) drives one turn and
//! DROPS its machine when the turn completes (the `FnOnce` body owns the
//! machine and consumes it). A RESIDENT session keeps the machine across turns:
//! a completed turn returns the `JitEffectMachine` to its session slot so the
//! next turn re-enters the SAME heap and sees prior effect-plane state.
//!
//! This is the "end-state registry" entry the engine docstring names —
//! `{machine, table, handlers, retention}` — realized as a per-session owned
//! struct driven by DIRECT `run_*` calls (not the oneshot re-arming closures).
//! The oneshot `FnOnce` path in `engine.rs` is untouched; the stateless eval
//! server keeps driving it.
//!
//! # Why turns can run on a fresh thread each time (E2's gift)
//!
//! `tidepool-repl` pins its machine to one parked worker thread because its
//! ask-suspend mechanism parks a blocked thread. E2 removed that need at the
//! ask boundary: [`JitEffectMachine::resume_suspended`] re-installs the
//! machine's per-thread reach (`CURRENT_MACHINE`, stack-map/lambda registry,
//! cancel flag) and re-points GC state at the RETAINED session heap on ANY
//! thread. So a resident session drives each turn on a fresh eval thread and
//! moves the machine back afterward — no parked worker, no pinning. The
//! stowed-XOR-running discipline (`unsafe impl Send for JitEffectMachine`)
//! holds because the machine is in exactly one place at a time: owned by the
//! session slot when idle/suspended, moved onto the eval thread for the
//! duration of a turn.
//!
//! # Fragment × suspend
//!
//! Each turn is compiled into the live machine as a fragment
//! ([`JitEffectMachine::add_function`]) and driven through
//! [`JitEffectMachine::run_fragment_suspendable`] — the composition of the
//! fragment plane (C2 session re-entry) with E2 threadless suspension. An `Ask`
//! mid-fragment stows the continuation on the machine and yields
//! `Suspended`; [`ResidentSession::resume`] re-enters and drives the fragment
//! to completion.
//!
//! # Nested child runs (segment 40)
//!
//! A suspended session REJECTS a new TOP-LEVEL turn (see
//! [`ResidentError::Suspended`]) but ACCEPTS a nested CHILD run
//! ([`ResidentSession::run_child`]): a fragment driven against the suspended
//! parent's SAME heap — reading the parent's bindings zero-copy — while the
//! parent's stowed continuation is registered as a GC root
//! ([`JitEffectMachine::run_child_fragment`]). The child does not consume the
//! parent's continuation; the session stays suspended on its hole across the
//! child run. The L7 `suspended_continuation.is_none()` asserts in
//! `jit_machine.rs` stay intact for the plain entries; the child entry moves the
//! continuation into a registered stowed root for its duration (so those asserts
//! still pass) — see the jit_machine module docstring for the full invariant.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{JitEffectMachine, ResumeInput, SuspendableOutcome};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_repr::{CoreExpr, DataConTable};

use crate::render::EvalResult;
use crate::{JitError, RuntimeError, EVAL_STACK_SIZE};

use super::engine::OutputSink;
use super::persistent::{PersistentSession, SuspensionMechanism, Threadless};
use super::{SessionError, SessionLib};

/// The classified result of driving a resident turn to its first yield.
///
/// The suspend-and-completion shape mirrors [`super::TurnOutcome`], but a
/// resident turn is driven by direct `run_*` calls (not the oneshot engine), so
/// this is a distinct, smaller enum: no `Paused`/`TimedOut` (timeout-yield is
/// permanently excluded from the stowable resident path — a locked decision),
/// and completion carries the bridged result value.
// `Completed`'s `EvalResult` is the large variant; like the engine's
// `SuspendableRun`, this is a transient boundary carrier destructured
// immediately by the caller, so the size asymmetry is inherent, not a leak.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ResidentOutcome {
    /// The turn ran to completion. `result` is the bridged result value; the
    /// machine is back in its slot, ready for the next turn.
    Completed {
        output: Vec<String>,
        result: EvalResult,
    },
    /// The turn suspended at an `Ask`. The machine holds the continuation
    /// internally (stowed as data); call [`ResidentSession::resume`] with the
    /// answer. `request` is the bridged `Ask` request; `hole` is the minted
    /// continuation id.
    Suspended {
        output: Vec<String>,
        hole: String,
        request: Value,
    },
}

/// Why a resident-session operation was refused or failed.
#[derive(thiserror::Error, Debug)]
pub enum ResidentError {
    /// A new top-level `run` was attempted while the session is suspended on an
    /// `Ask`. A suspended session accepts `resume`/`abort` (parent) or
    /// `run_child` (nested child) — never a fresh top-level turn.
    #[error("session is suspended on continuation {0}; resume, abort, or run a child before a new top-level run")]
    Suspended(String),
    /// A `run_child` was attempted on an idle (not-suspended) session — a
    /// nested child requires a suspended parent by construction (segment 40).
    #[error("session is not suspended; a nested child run requires a suspended parent")]
    NotSuspended,
    /// A nested child fragment itself suspended at an `Ask`. R0 is single-level
    /// sequential-isolated nesting — the machine holds exactly one stowed
    /// continuation, so a child cannot suspend while the parent already is.
    #[error("nested child suspended at an ask; R0 supports single-level nesting only")]
    ChildSuspended,
    /// A `resume`/`abort` referenced a continuation id that is not the one this
    /// session is currently suspended on (or the session is not suspended).
    /// Atomic validate-before-consume: the pending continuation is NOT touched.
    #[error("no continuation {attempted} pending{}", match .pending {
        Some(p) => format!(" (session is suspended on {p})"),
        None => " (session is not suspended)".to_string(),
    })]
    WrongContinuation {
        attempted: String,
        pending: Option<String>,
    },
    /// The turn's fragment failed to add to the live machine.
    #[error("fragment compile failed: {0}")]
    AddFunction(JitError),
    /// The turn errored during the run (a runtime fault, a caught panic, or an
    /// ask-protocol error).
    #[error("turn run failed: {0}")]
    Run(RuntimeError),
    /// Merging this turn's constructor metadata into the session table hit a
    /// collision (a Haskell-side DataCon-scheme regression, mirroring the repl's
    /// `merge_table`).
    #[error("session DataConTable collision: {0}")]
    TableCollision(String),
}

/// A resident JIT session: one long-lived [`JitEffectMachine`] whose heap and
/// effect-plane state persist across turns.
///
/// Generic over the effect handler stack `H` and the output sink `O` so it
/// stays below the server crate that owns the concrete buffer, exactly like
/// [`super::SessionEngine`]. The registry (`tidepool-harness`) instantiates
/// `Slot<ResidentSession<H, O>>`.
pub struct ResidentSession<H, O> {
    /// The shared persistent-session core (machine + accumulated table + the two
    /// planes), driven through the threadless suspend mechanism. The harness does
    /// not (yet) accumulate on the decl/value planes — they sit empty here until
    /// W1b turns them on — but the machine lifecycle + table merge + fragment-run
    /// primitives all live in the core, shared with the repl's parked-thread
    /// session.
    core: PersistentSession<Threadless>,
    /// The effect handler stack, borrowed by each turn's eval thread.
    handlers: H,
    /// Effect names by tag (registry-entry metadata; exposed via
    /// [`ResidentSession::effect_names`] for the harness's effect-roster
    /// rendering — the resident surface does not re-classify run errors here).
    effect_names: Vec<String>,
    /// The console-output buffer turns write into.
    captured: O,
    /// GHC include search paths for fragment compiles (unused today — fragments
    /// are pre-compiled Core — but carried as the registry-entry seam).
    #[allow(dead_code)]
    include: Vec<PathBuf>,
    /// Monotonic continuation-id counter.
    next_id: AtomicU64,
    /// The continuation id this session is suspended on, or `None` when idle.
    /// The machine's `suspended_continuation` is the ground truth; this is the
    /// string identity the caller resumes/aborts against (atomic
    /// validate-before-consume, mirroring `engine.rs`:684–698).
    pending: Option<String>,
    /// Continuation-id prefix (`scont` for the resident surface).
    cont_prefix: String,
}

impl<H, O> ResidentSession<H, O>
where
    H: DispatchEffect<O> + Send,
    // `Sync` so the per-turn eval thread can borrow the shared sink (every
    // real sink is `Arc`-backed and already `Sync`; the `OutputSink` trait
    // itself only requires `Clone + Send`).
    O: OutputSink + Sync,
{
    /// Bootstrap a resident session from an initial `expr`/`table` (a session
    /// machine, so its heap is retained across turns). The bootstrap expr is
    /// compiled but NOT run — it seeds the machine's ConTags (an `Eff` module
    /// carrying the effect tag list the dispatch needs); turns are then added as
    /// fragments. Mirrors the repl's bootstrap (`session.rs`: compile_session on
    /// the first turn's table).
    // The arg list mirrors the engine's `StartTurn` field carrier (source,
    // handlers, ask_tag, effect_names, captured, include, nursery) — bundling
    // them into a struct would just move the arity, not remove it.
    ///
    /// `lib` is the decl plane: pass `Some` to accumulate declarations across
    /// turns (the harness, once W1b turns it on), or `None` for a value-only
    /// session. The boot table seeds the accumulated session table.
    #[allow(clippy::too_many_arguments)]
    pub fn bootstrap(
        expr: &CoreExpr,
        table: DataConTable,
        handlers: H,
        ask_tag: u64,
        effect_names: Vec<String>,
        captured: O,
        include: Vec<PathBuf>,
        nursery_size: usize,
        lib: Option<SessionLib>,
    ) -> Result<Self, JitError> {
        let mut core = PersistentSession::<Threadless>::new(lib, ask_tag, nursery_size);
        core.bootstrap_if_needed(expr, &table)?;
        core.seed_session_table(table);
        Ok(ResidentSession {
            core,
            handlers,
            effect_names,
            captured,
            include,
            next_id: AtomicU64::new(1),
            pending: None,
            cont_prefix: "scont".to_string(),
        })
    }

    /// Accumulate `decls` on the decl plane (mirrors the repl's
    /// `Session::define_scoped`): a declaration turn appends to the gen-versioned
    /// `Lib.G<g>` module a later turn imports. Requires a decl plane (`Some(lib)`
    /// at bootstrap). Each node's plane is independent, so a parent's accumulated
    /// declarations survive across a child run on a different node.
    pub fn define_scoped(
        &mut self,
        decls: &[&str],
    ) -> Result<tidepool_repr::Generation, SessionError> {
        self.core.define_scoped(decls)
    }

    /// The current decl-plane module name (`Tidepool.Session.Lib.G<g>`) a later
    /// turn imports to see accumulated declarations, or `None` before any decl.
    pub fn session_import_module(&self) -> Option<String> {
        self.core.current_lib_module().map(|m| m.module_name())
    }

    /// The decl-plane include directory to add to a later turn's compile search
    /// path (so `import Lib.G<g>` resolves), or `None` with no decl plane.
    pub fn lib_include_dir(&self) -> Option<PathBuf> {
        self.core.lib_include_dir().map(Path::to_path_buf)
    }

    /// The continuation id this session is suspended on, if any.
    pub fn pending_continuation(&self) -> Option<&str> {
        self.pending.as_deref()
    }

    /// Whether the session is idle (ready for a new turn).
    pub fn is_idle(&self) -> bool {
        self.pending.is_none()
    }

    /// Effect names by union tag (the roster the harness renders alongside an
    /// unhandled-effect error).
    pub fn effect_names(&self) -> &[String] {
        &self.effect_names
    }

    /// Read-only heap/GC snapshot of this session's live machine (observatory
    /// heap pane) — `None` only during the transient window a turn is running
    /// on its own eval thread (the machine moved out; see [`Self::on_eval_thread`]).
    pub fn heap_stats(&self) -> Option<tidepool_codegen::jit_machine::HeapStats> {
        self.core.machine().map(|m| m.heap_stats())
    }

    fn next_cont_id(&self) -> String {
        format!(
            "{}_{}",
            self.cont_prefix,
            self.next_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Run one turn: add `expr` as a fragment referencing prior session bindings
    /// via `external_env`, then drive it through the suspend-capable fragment
    /// path. A suspended session REJECTS this (segment 40 owns nested runs).
    ///
    /// `table` is this turn's constructor metadata; it is merged into the
    /// session table (later turns are a subset, so the merge is monotone).
    pub fn run(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        table: &DataConTable,
        external_env: &ExternalEnv,
    ) -> Result<ResidentOutcome, ResidentError> {
        if let Some(p) = &self.pending {
            return Err(ResidentError::Suspended(p.clone()));
        }
        // Merge this turn's table into the accumulated session table (later turns
        // are a subset; the merge is monotone). `add_fragment_session` mints the
        // fragment against that table on THIS (calling) thread — the env is
        // `!Send` and cannot cross to the eval thread; only the machine (Send)
        // does. The run itself goes through the threadless mechanism.
        self.core
            .merge_table(table)
            .map_err(ResidentError::TableCollision)?;
        let func_id = self
            .core
            .add_fragment_session(name_hint, expr, external_env)
            .map_err(ResidentError::AddFunction)?;

        let ask_tag = self.core.ask_tag();
        let outcome = self.on_eval_thread(move |machine, table, handlers, captured| {
            Threadless::run_fragment(machine, func_id, table, handlers, captured, ask_tag)
        })?;
        Ok(self.classify(outcome))
    }

    /// Run a NESTED CHILD turn against this SUSPENDED session (segment 40): add
    /// `expr` as a fragment referencing the suspended parent's session bindings
    /// (via `external_env`, zero-copy against the same retained heap), then drive
    /// it through [`JitEffectMachine::run_child_fragment`] while the parent's
    /// stowed continuation is GC-rooted. The session STAYS suspended on the same
    /// hole afterward — the child does not consume the parent's continuation.
    ///
    /// Requires the session to be suspended (a child needs a suspended parent);
    /// an idle session is rejected with [`ResidentError::NotSuspended`]. A child
    /// that itself suspends is rejected ([`ResidentError::ChildSuspended`]) —
    /// the machine holds exactly one stowed continuation, so a child cannot
    /// suspend while the parent is already suspended (R0 is sequential-isolated,
    /// single-level nesting).
    pub fn run_child(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        table: &DataConTable,
        external_env: &ExternalEnv,
    ) -> Result<EvalResult, ResidentError> {
        // A child requires a suspended parent.
        if self.pending.is_none() {
            return Err(ResidentError::NotSuspended);
        }
        // Merge this child's table into the session table (monotone). Add the
        // child fragment on THIS thread (env is `!Send`); module accretion is
        // inert for the parent — a fresh FuncId, the stowed continuation
        // untouched.
        self.core
            .merge_table(table)
            .map_err(ResidentError::TableCollision)?;
        let func_id = self
            .core
            .add_child_fragment_session(name_hint, expr, external_env)
            .map_err(ResidentError::AddFunction)?;

        // `pending` is untouched throughout — the parent stays suspended on the
        // same hole across the child run.
        let outcome = self.on_eval_thread(move |machine, table, handlers, captured| {
            machine
                .run_child_fragment(func_id, table, handlers, captured)
                // A child run returns a plain `Value` (not a SuspendableOutcome);
                // wrap it as Completed so `on_eval_thread`'s shared plumbing
                // applies. A child fragment does not go through the suspend
                // driver, so it either completes or errors — it never suspends.
                .map(SuspendableOutcome::Completed)
        })?;

        match outcome {
            SuspendableOutcome::Completed(value) => {
                // Drain the child's debug output so it does not leak into a
                // later parent-turn snapshot/drain. The child's RESULT is the
                // deliverable; its console output is debug-only here.
                let _ = self.captured.drain();
                Ok(EvalResult::new(
                    value,
                    self.core.session_table().clone(),
                    Vec::new(),
                ))
            }
            // Unreachable by construction (run_child_fragment can't suspend),
            // but keep it a typed error rather than a panic.
            SuspendableOutcome::Suspended { .. } => Err(ResidentError::ChildSuspended),
        }
    }

    /// Resume the suspended turn with `answer`, driving the fragment to its next
    /// suspension or completion. Atomic validate-before-consume: `cont_id` must
    /// match the pending continuation or the pending one is untouched
    /// ([`ResidentError::WrongContinuation`], mirroring `engine.rs`:684–698 and
    /// the repl server's three-way resume errors).
    pub fn resume(
        &mut self,
        cont_id: &str,
        answer: Value,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.reenter(cont_id, ResumeInput::Answer(answer))
    }

    /// Abort the suspended turn WITHOUT running the continuation — the ask
    /// itself fails (byte-identically to the engine's stowed-abort path). Same
    /// validate-before-consume as [`Self::resume`].
    pub fn abort(
        &mut self,
        cont_id: &str,
        reason: String,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.reenter(cont_id, ResumeInput::Abort(reason))
    }

    fn reenter(
        &mut self,
        cont_id: &str,
        input: ResumeInput,
    ) -> Result<ResidentOutcome, ResidentError> {
        // Validate BEFORE consuming the pending continuation. A mismatch leaves
        // `self.pending` intact — the caller can retry with the right id.
        match &self.pending {
            Some(p) if p == cont_id => {}
            other => {
                return Err(ResidentError::WrongContinuation {
                    attempted: cont_id.to_string(),
                    pending: other.clone(),
                })
            }
        }
        let ask_tag = self.core.ask_tag();
        // `resume_suspended` consumes the machine's stowed continuation as soon
        // as it is entered (`.take()`), so the OLD hole is spent regardless of
        // the re-entry's outcome — clear `pending` up front. `classify` re-arms
        // it with a FRESH hole if the re-entry suspends again; an error leaves
        // the session idle (the spent continuation cannot be resumed twice).
        self.pending = None;
        let outcome = self.on_eval_thread(move |machine, table, handlers, captured| {
            Threadless::resume(machine, table, handlers, captured, ask_tag, input)
        })?;
        Ok(self.classify(outcome))
    }

    /// Move the machine onto a stack-sized eval thread, run `body`, and move the
    /// machine back. E2 lets `body` run on this fresh thread — the threadless
    /// mechanism's `run_fragment`/`resume` re-install the machine's per-thread
    /// reach and re-point GC state at the retained heap. Only the machine (and
    /// the accumulated table) crosses to the thread; the rest of the session
    /// core is `!Send` (raw-pointer roots) and stays here.
    fn on_eval_thread<F>(&mut self, body: F) -> Result<SuspendableOutcome, ResidentError>
    where
        F: FnOnce(
                &mut JitEffectMachine,
                &DataConTable,
                &mut H,
                &O,
            ) -> Result<SuspendableOutcome, JitError>
            + Send,
    {
        let mut machine = self.core.take_machine();
        let table = self.core.session_table();
        let handlers = &mut self.handlers;
        // The sink is Arc-backed (`OutputSink: Clone + Send`) and shares its
        // buffer; move a clone onto the thread rather than requiring `O: Sync`
        // for a borrow — matches the oneshot engine's `captured.clone()`.
        let captured = self.captured.clone();

        // A scoped thread borrows `machine`/`handlers`/`table`/`captured` from
        // this frame — the machine is moved back into the core after the scope
        // joins, so it stays resident. `EVAL_STACK_SIZE` matches the oneshot
        // eval thread (deep JIT recursion needs it), so `Builder::spawn_scoped`
        // (the stack-sized form of `scope.spawn`) is used.
        let result = std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .name("tidepool-resident-eval".into())
                .stack_size(EVAL_STACK_SIZE)
                .spawn_scoped(scope, || {
                    tidepool_codegen::signal_safety::install();
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        body(&mut machine, table, handlers, &captured)
                    }))
                })
                .expect("failed to spawn resident eval thread");
            handle.join()
        });

        // The machine is resident again regardless of the turn's fate.
        self.core.restore_machine(machine);

        match result {
            Ok(Ok(outcome)) => outcome.map_err(|e| ResidentError::Run(RuntimeError::Jit(e))),
            Ok(Err(panic)) => Err(panic_to_run_error(panic)),
            Err(join_panic) => Err(panic_to_run_error(join_panic)),
        }
    }

    /// Classify a raw [`SuspendableOutcome`] into a [`ResidentOutcome`], minting
    /// and arming a continuation id on suspension and draining/snapshotting
    /// output the same way the engine does (drain on completion, snapshot on
    /// suspend).
    fn classify(&mut self, outcome: SuspendableOutcome) -> ResidentOutcome {
        match outcome {
            SuspendableOutcome::Completed(value) => {
                self.pending = None;
                let output = self.captured.drain();
                ResidentOutcome::Completed {
                    output,
                    result: EvalResult::new(value, self.core.session_table().clone(), Vec::new()),
                }
            }
            SuspendableOutcome::Suspended { request } => {
                let hole = self.next_cont_id();
                self.pending = Some(hole.clone());
                let output = self.captured.snapshot();
                ResidentOutcome::Suspended {
                    output,
                    hole,
                    request,
                }
            }
        }
    }
}

/// Map a caught panic payload (a Rust-level fault that unwound past the JIT's
/// own `with_signal_protection` — a genuine bug, not a language-level error) to
/// a run error carrying the payload string.
fn panic_to_run_error(payload: Box<dyn std::any::Any + Send>) -> ResidentError {
    let detail = if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    };
    ResidentError::Run(RuntimeError::Jit(JitError::Effect(EffectError::Handler(
        format!("resident turn panicked: {detail}"),
    ))))
}
