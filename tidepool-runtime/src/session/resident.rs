//! `ResidentSession` — the `Retention::Persistent` end-state.
//!
//! The stow engine's oneshot path (`SessionEngine`) drives one turn and DROPS
//! its machine when the turn completes (the `FnOnce` body owns the machine
//! and consumes it). A RESIDENT session keeps the machine across turns: a
//! completed turn returns the `JitEffectMachine` to its session slot so the
//! next turn re-enters the SAME heap and sees prior effect-plane state.
//!
//! This is the "end-state registry" entry the engine docstring names —
//! `{machine, table, handlers, retention}` — realized as a per-session owned
//! struct driven by DIRECT `run_*` calls (not the oneshot re-arming closures).
//! The oneshot `FnOnce` path in `engine.rs` is untouched; the stateless eval
//! server keeps driving it.
//!
//! # Why turns can run on a fresh thread each time
//!
//! Nothing about a suspended session is pinned to the thread that suspended it:
//! [`JitEffectMachine::resume_suspended`] re-installs the machine's
//! per-thread reach (`CURRENT_MACHINE`, stack-map/lambda registry, cancel
//! flag) and re-points GC state at the RETAINED session heap on ANY thread.
//! So a resident session drives each turn on a fresh eval thread and moves
//! the machine back afterward — no parked worker, no pinning. `tidepool-repl`
//! leans on the same property one level up: it moves the WHOLE session into a
//! `spawn_blocking` turn and back out. The
//! stowed-XOR-running discipline (`unsafe impl Send for JitEffectMachine`)
//! holds because the machine is in exactly one place at a time: owned by the
//! session slot when idle/suspended, moved onto the eval thread for the
//! duration of a turn.
//!
//! # Fragment × suspend — the PARKED path (one-session plan, Phase 1)
//!
//! Each turn is compiled into the live machine as a fragment
//! ([`JitEffectMachine::add_function`]) and driven through
//! [`JitEffectMachine::run_fragment_suspendable_parked`] — the continuation
//! REGISTRY, not the legacy single slot: a suspension parks a frame as a
//! registered GC root, and the machine stays fully usable while it waits
//! (further turns, further parks, resumes of other frames). The session
//! tracks its parked holes as an insertion-ordered `(hole, ContinuationId)`
//! list; [`ResidentSession::resume`] resumes ANY member hole by identity
//! (the machine imposes no order). The realm every park is owned by is
//! [`ResidentSession::set_realm`]-scoped (per-node realms arrive with the
//! collapse); the handled prefix is DERIVED from the session's own
//! `effect_names[..ask_tag]` — one source of truth, per the parking
//! contract's "derive, don't declare" guidance.
//!
//! # Child runs
//!
//! With the registry, a "child" is just an ordinary fragment run while
//! frames are parked — the machine is never slot-suspended, so nothing is
//! special about it. [`ResidentSession::run_child`] keeps its value-shaped
//! signature (a fork answer IS a value): a child that suspends is aborted
//! wholesale (its throwaway realm closed) rather than parked, because this
//! API cannot carry a hole; suspension-capable turns go through
//! [`ResidentSession::run`]. `!Send` `RootSlot`s never cross the eval-thread
//! boundary: parked completions are projected in-thread to `Send` data, a
//! bind's tenured root riding out as a [`ValueHandle`] (pillar B's
//! laundering).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tidepool_codegen::binding_table::{BindingEntry, BoundValue};
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{
    ContinuationId, FuncId, JitEffectMachine, ParkKind, ParkedOutcome, RealmId, ResumeInput,
    ValueHandle,
};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_repr::{BindingName, CoreExpr, DataConTable, Generation, SessionModule, SessionVarId};

use crate::render::EvalResult;
use crate::timing;
use crate::{JitError, RuntimeError, EVAL_STACK_SIZE};

use super::engine::OutputSink;
use super::persistent::PersistentSession;
use super::turn::{BoundBinder, ValueTier};
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
    /// LEGACY (parked path): retained for callers that still match on it; the
    /// parked path never raises it — a new top-level `run` while frames are
    /// parked is the POINT of the registry.
    #[error("session is suspended on continuation {0}; resume, abort, or run a child before a new top-level run")]
    Suspended(String),
    /// A `run_child`/`apply_finalized` was attempted with no parked frame — a
    /// child run reads a suspended parent's world by construction.
    #[error("session has no parked continuation; a child run requires a suspended parent")]
    NotSuspended,
    /// A VALUE-SHAPED child run ([`ResidentSession::run_child`]) suspended:
    /// its signature cannot carry a hole, so the child was ABORTED (its
    /// throwaway realm closed) rather than parked. Not a machine limitation
    /// anymore (the registry parks children fine) — a policy of this one
    /// API; drive suspension-capable turns through [`ResidentSession::run`].
    #[error(
        "child run suspended; the value-shaped run_child aborts a suspending child — \
         drive suspension-capable turns through run()"
    )]
    ChildSuspended,
    /// A `resume`/`abort` referenced a continuation id that is not among this
    /// session's parked holes. Atomic validate-before-consume: no parked
    /// frame is touched.
    #[error("no continuation {attempted} parked{}", if .pending.is_empty() {
        " (session has no parked continuations)".to_string()
    } else {
        format!(" (parked: {})", .pending.join(", "))
    })]
    WrongContinuation {
        attempted: String,
        pending: Vec<String>,
    },
    /// The turn's fragment failed to add to the live machine.
    #[error("fragment compile failed: {0}")]
    AddFunction(JitError),
    /// The session machine failed to bootstrap from the first real turn's
    /// expr/table (lazy boot — see [`ResidentSession::unbootstrapped`]).
    #[error("session bootstrap failed: {0}")]
    Bootstrap(JitError),
    /// The turn errored during the run (a runtime fault, a caught panic, or an
    /// ask-protocol error).
    #[error("turn run failed: {0}")]
    Run(RuntimeError),
    /// Merging this turn's constructor metadata into the session table hit a
    /// collision (a Haskell-side DataCon-scheme regression, mirroring the repl's
    /// `merge_table`).
    #[error("session DataConTable collision: {0}")]
    TableCollision(String),
    /// A decl-plane operation failed while materializing a value bind — the
    /// cross-plane shadow retract (a value bind evicting a same-name decl head).
    #[error(transparent)]
    Session(#[from] SessionError),
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
    /// planes). The harness does not (yet) accumulate on the decl/value planes —
    /// they sit empty here until enabled — but the machine lifecycle + table
    /// merge + fragment-run primitives all live in the core, shared with the
    /// repl's resident session.
    core: PersistentSession,
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
    /// The parked holes, insertion-ordered: `(hole string, machine
    /// ContinuationId)` per live parked frame. The machine's continuation
    /// registry is the ground truth; these are the string identities callers
    /// resume/abort against (atomic validate-before-consume). Top = last.
    parked: Vec<(String, ContinuationId)>,
    /// The realm every park this session initiates is owned by. `RealmId(0)`
    /// until [`ResidentSession::set_realm`] — per-node realms arrive with the
    /// collapse (one-session plan, Phase 3).
    realm: RealmId,
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
    ///
    /// No production caller: `tidepool-harness`'s `Harness::force`/
    /// `SelfHarnessDriver::bootstrap` both use [`Self::unbootstrapped`], which
    /// pays no compile until the first REAL turn. Kept as a public constructor
    /// because this crate's own GHC-heavy test suite
    /// (`tidepool-runtime/tests/resident_session.rs`, `realm_varid_pinning.rs`)
    /// still calls it directly for one-shot setup convenience — a caller that
    /// already has an `expr`/`table` in hand and wants a live machine
    /// immediately, compiling a real program and then driving real turns
    /// against the SAME machine [`Self::unbootstrapped`] would also have
    /// booted from their first `run`.
    // The arg list mirrors the engine's `StartTurn` field carrier (source,
    // handlers, ask_tag, effect_names, captured, include, nursery) — bundling
    // them into a struct would just move the arity, not remove it.
    ///
    /// `lib` is the decl plane: pass `Some` to accumulate declarations across
    /// turns (once the harness enables it), or `None` for a value-only
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
        let mut core = PersistentSession::new(lib, ask_tag, nursery_size);
        core.bootstrap_if_needed(expr, &table)?;
        core.seed_session_table(table);
        Ok(ResidentSession {
            core,
            handlers,
            effect_names,
            captured,
            include,
            next_id: AtomicU64::new(1),
            parked: Vec::new(),
            realm: RealmId(0),
            cont_prefix: "scont".to_string(),
        })
    }

    /// Build a resident session with NO live machine yet — the lazy
    /// counterpart to [`Self::bootstrap`]. Same arguments MINUS `expr`/`table`:
    /// there is no seed program to compile, so construction cannot fail and
    /// pays no GHC extract compile. The machine comes up on the first REAL
    /// turn ([`Self::run`]/[`Self::run_bind`]/[`Self::run_child`]/
    /// [`Self::run_child_pure`], via `PersistentSession::bootstrap_if_needed`
    /// immediately before that turn's fragment is added) — mirrors the repl's
    /// bootstrap-from-first-real-compile (`tidepool-repl/src/session.rs`).
    #[allow(clippy::too_many_arguments)]
    pub fn unbootstrapped(
        handlers: H,
        ask_tag: u64,
        effect_names: Vec<String>,
        captured: O,
        include: Vec<PathBuf>,
        nursery_size: usize,
        lib: Option<SessionLib>,
    ) -> Self {
        let core = PersistentSession::new(lib, ask_tag, nursery_size);
        ResidentSession {
            core,
            handlers,
            effect_names,
            captured,
            include,
            next_id: AtomicU64::new(1),
            parked: Vec::new(),
            realm: RealmId(0),
            cont_prefix: "scont".to_string(),
        }
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

    /// The current value-binding generation. The caller mints the NEXT one
    /// (`val_gen().next()`) BEFORE compiling a bind turn — the extract stamps that
    /// generation into `Val.G<g>`, and [`Self::run_bind`]/[`Self::resume_bind`]
    /// materialize at the same `g`.
    pub fn val_gen(&self) -> Generation {
        self.core.val_gen()
    }

    /// The live `Val.G<g>` module names to inject (`--inject-val`) so a turn can
    /// reference earlier value bindings — ALL live gens (incl. shadowed).
    pub fn inject_val_modules(&self) -> Vec<String> {
        self.core.live_val_modules()
    }

    /// The CURRENT `Val.G<g>` module per still-live name — what a turn IMPORTS
    /// (unqualified) so the reference typechecks. Excludes shadowed older gens
    /// (those are injected but not imported, to avoid an ambiguous occurrence).
    pub fn current_val_modules(&self) -> Vec<String> {
        self.core.current_val_modules()
    }

    /// The MOST RECENT parked hole (top of the stack), if any — the
    /// single-hole compatibility view; multi-hole callers use
    /// [`Self::parked_holes`].
    pub fn pending_continuation(&self) -> Option<&str> {
        self.parked.last().map(|(h, _)| h.as_str())
    }

    /// Every parked hole, insertion-ordered (oldest first).
    pub fn parked_holes(&self) -> Vec<&str> {
        self.parked.iter().map(|(h, _)| h.as_str()).collect()
    }

    /// Whether the session has no parked frames (ready and quiescent).
    pub fn is_idle(&self) -> bool {
        self.parked.is_empty()
    }

    /// Scope every subsequent park under `realm` (one-session plan: the
    /// driver assigns per-answerer-node realms; scope exit is the machine's
    /// `close_realm`).
    pub fn set_realm(&mut self, realm: RealmId) {
        self.realm = realm;
    }

    /// SCOPE EXIT for `realm` (one-session plan, pillar A): close the realm
    /// on the machine (frames dropped, roots deregistered, handles released)
    /// and RECONCILE this session's parked-hole list against the machine's
    /// surviving frame ids — the machine is the ground truth, so holes whose
    /// frames the close dropped disappear here too, and sibling realms'
    /// holes are untouched. Returns `(frames_dropped, handles_released)`;
    /// `(0, 0)` when the machine is not yet booted or the realm owns
    /// nothing (idempotent).
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        let Some(machine) = self.core.machine_mut() else {
            return (0, 0);
        };
        let counts = machine.close_realm(realm);
        let survivors = machine.parked_ids();
        self.parked.retain(|(_, id)| survivors.contains(id));
        counts
    }

    /// Mint a [`ValueHandle`] over the closure-valued `finalize` payload of
    /// the frame parked on `hole` (pillar B: the payload never bridges to a
    /// data `Value`; the `Send` handle is how it is passed around and
    /// eventually DELIVERED into a sibling hole via [`Self::resume_handle`]).
    /// The frame stays parked (consume/abort it separately, as the finalize
    /// flow always has); the handle is owned by the frame's realm. `None`
    /// when `hole` is not parked or its frame holds no (untaken) finalized
    /// payload.
    pub fn finalized_handle(&mut self, hole: &str) -> Option<ValueHandle> {
        let &(_, id) = self.parked.iter().find(|(h, _)| h == hole)?;
        self.core.machine_mut()?.handle_from_finalized(id)
    }

    /// Resume the turn parked on `cont_id` by DELIVERING a machine-side
    /// rooted value — the handle's payload feeds the continuation verbatim,
    /// no materialization, closures included (pillar B's delivery half; the
    /// one-session loop receives its `State -> State` this way). Same
    /// validate-before-consume and ground-truth reconciliation as
    /// [`Self::resume`].
    pub fn resume_handle(
        &mut self,
        cont_id: &str,
        handle: ValueHandle,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.reenter(cont_id, ResumeInput::Handle(handle), None)
    }

    /// This session's handled-effect prefix, DERIVED from its own
    /// `effect_names` and ask tag (the names below the suspend threshold, in
    /// position order) — the parking contract's "derive, don't declare".
    fn handled_prefix(&self) -> Vec<String> {
        let n = (self.core.ask_tag() as usize).min(self.effect_names.len());
        self.effect_names[..n].to_vec()
    }

    /// Whether the resident machine has been bootstrapped yet. `false` from
    /// [`Self::unbootstrapped`] until the session's first real turn brings the
    /// machine up (`run`/`run_bind`/`run_child`/`run_child_pure`); always
    /// `true` from [`Self::bootstrap`].
    pub fn is_bootstrapped(&self) -> bool {
        self.core.is_bootstrapped()
    }

    /// Effect names by union tag (the roster the harness renders alongside an
    /// unhandled-effect error).
    pub fn effect_names(&self) -> &[String] {
        &self.effect_names
    }

    /// Read-only heap/GC snapshot of this session's live machine (observatory
    /// heap pane) — `None` either before the machine is bootstrapped (see
    /// [`Self::unbootstrapped`]/[`Self::is_bootstrapped`]) or during the
    /// transient window a turn is running on its own eval thread (the machine
    /// moved out; see [`Self::on_eval_thread`]).
    /// Move the decl plane out for a machine rotation — see
    /// [`super::persistent::PersistentSession::take_lib`].
    pub fn take_lib(&mut self) -> Option<crate::session::SessionLib> {
        self.core.take_lib()
    }

    pub fn heap_stats(&self) -> Option<tidepool_codegen::jit_machine::HeapStats> {
        self.core.machine().map(|m| m.heap_stats())
    }

    /// The CURRENT value-plane binding names (newest gen per name) — what a
    /// machine rotation would lose (one-session plan, Phase 4: enumerated,
    /// legible loss, never silent).
    pub fn binding_names(&self) -> Vec<String> {
        self.core
            .bindings()
            .iter_current()
            .map(|(name, _)| name.0.clone())
            .collect()
    }

    /// The `ExternalEnv` a fragment compiling `expr` is seeded with: the
    /// session's live value bindings that `expr` actually references, so
    /// the fragment can resolve an earlier `x <- e` at a Var-miss. Empty until
    /// the first bind materializes AND this fragment references one, so a
    /// value-plane-free session behaves exactly as before.
    ///
    /// [`Self::run`] and [`Self::run_bind`] call this on their way to
    /// `add_fragment_session`, so it is the seeding path rather than a
    /// reconstruction of it — a test asserting on the returned env is
    /// asserting on the env a fragment really compiles against, and the
    /// VarId-keyed isolation property (only referenced `SessionVarId`s, never
    /// another scope's) cannot drift away from what this returns.
    pub fn seed_external_env_for(&self, expr: &CoreExpr) -> ExternalEnv {
        let referenced = tidepool_repr::free_vars::free_vars(expr);
        self.core.seed_external_env(&referenced)
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
    ) -> Result<ResidentOutcome, ResidentError> {
        // No reject-while-suspended: on the parked path, a new turn over
        // parked frames is ordinary (the machine is never slot-suspended).
        // Merge this turn's table into the accumulated session table (later turns
        // are a subset; the merge is monotone). `add_fragment_session` mints the
        // fragment against that table on THIS (calling) thread — the env is
        // `!Send` and cannot cross to the eval thread; only the machine (Send)
        // does. The run itself goes through the threadless mechanism.
        self.core
            .merge_table(table)
            .map_err(ResidentError::TableCollision)?;
        // Lazy boot: no-op once the machine is live. On the FIRST real run this
        // is what brings the machine up (mirrors the repl's merge-then-bootstrap
        // ordering, `tidepool-repl/src/session.rs`) — must run on the calling
        // thread, same as `add_fragment_session` below (both touch the `!Send`
        // env / pipeline).
        self.core
            .bootstrap_if_needed(expr, table)
            .map_err(ResidentError::Bootstrap)?;
        let env = self.seed_external_env_for(expr);
        let jit_codegen_started = std::time::Instant::now();
        let func_id = self
            .core
            .add_fragment_session(name_hint, expr, &env)
            .map_err(ResidentError::AddFunction)?;
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_JIT_CODEGEN,
            jit_codegen_started.elapsed(),
            0,
        );

        let ask_tag = self.core.ask_tag();
        let realm = self.realm;
        let prefix = self.handled_prefix();
        let run_exec_started = std::time::Instant::now();
        let outcome = self.on_eval_thread(move |machine, table, handlers, captured| {
            machine
                .run_fragment_suspendable_parked(
                    func_id,
                    table,
                    handlers,
                    captured,
                    ask_tag,
                    realm,
                    ParkKind::Plain,
                    &prefix,
                )
                .map(|o| project_parked(machine, o, realm))
        })?;
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_RUN_EXEC,
            run_exec_started.elapsed(),
            0,
        );
        Ok(self.classify_parked(outcome, None))
    }

    /// Run a value-plane BIND turn (`x <- e`): seed the env from prior bindings,
    /// add the fragment, and drive it through the suspendable BIND path
    /// (tenure-on-completion). On completion, materialize `binder` into the value
    /// plane at `gen` (the SAME generation the extract stamped into
    /// `binder.module` — mint it once at compile, thread it here). A fork bind
    /// SUSPENDS here (no value yet); it is bound on the eventual
    /// [`Self::resume_bind`] with the same `binder`/`gen`.
    pub fn run_bind(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        table: &DataConTable,
        binder: &BoundBinder,
        gen: Generation,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.core
            .merge_table(table)
            .map_err(ResidentError::TableCollision)?;
        // Lazy boot (see `run`'s comment): no-op once live, brings the machine
        // up on the FIRST real turn otherwise (a bind may itself be it).
        self.core
            .bootstrap_if_needed(expr, table)
            .map_err(ResidentError::Bootstrap)?;
        let env = self.seed_external_env_for(expr);
        let jit_codegen_started = std::time::Instant::now();
        let func_id = self
            .core
            .add_fragment_session(name_hint, expr, &env)
            .map_err(ResidentError::AddFunction)?;
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_JIT_CODEGEN,
            jit_codegen_started.elapsed(),
            0,
        );

        let ask_tag = self.core.ask_tag();
        // Tier0 data is deep-forced to NF before tenuring; a Tier1 closure is
        // tenured as-is.
        let forced = matches!(binder.tier, ValueTier::Tier0Data);
        let realm = self.realm;
        let prefix = self.handled_prefix();
        let run_exec_started = std::time::Instant::now();
        let outcome = self.on_eval_thread(move |machine, table, handlers, captured| {
            machine
                .run_fragment_suspendable_parked(
                    func_id,
                    table,
                    handlers,
                    captured,
                    ask_tag,
                    realm,
                    ParkKind::Binding { forced },
                    &prefix,
                )
                .map(|o| project_parked(machine, o, realm))
        })?;
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_RUN_EXEC,
            run_exec_started.elapsed(),
            0,
        );
        // A completion (no suspension) tenured the result — bind it now (the
        // root rode out as a handle). A suspension defers to `resume_bind`.
        let bound = match &outcome {
            ParkedRun::Completed { bound, .. } => *bound,
            ParkedRun::Suspended { .. } => None,
        };
        let completed = matches!(outcome, ParkedRun::Completed { .. });
        let resident_outcome = self.classify_parked(outcome, None);
        if completed {
            self.materialize_binder(binder, gen, bound)?;
        }
        Ok(resident_outcome)
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
        let func_id = self.prepare_child_fragment(name_hint, expr, table, external_env)?;
        // `parked` is untouched throughout — the parent's frames stay parked
        // and rooted across the child run (that is the registry's whole
        // point; nothing here is a special "child window" anymore).
        //
        // A THROWAWAY realm: this API's value-shaped signature cannot carry a
        // hole, so a child that suspends is ABORTED wholesale (its realm
        // closed) rather than parked. Suspension-capable turns are `run`'s
        // job. High-bit-tagged so it can never collide with a caller realm.
        let child_realm = RealmId((1 << 63) | self.next_id.fetch_add(1, Ordering::Relaxed));
        let ask_tag = self.core.ask_tag();
        let prefix = self.handled_prefix();
        let outcome = self.on_eval_thread(move |machine, table, handlers, captured| {
            machine
                .run_fragment_suspendable_parked(
                    func_id,
                    table,
                    handlers,
                    captured,
                    ask_tag,
                    child_realm,
                    ParkKind::Plain,
                    &prefix,
                )
                .map(|o| project_parked(machine, o, child_realm))
        })?;
        match outcome {
            ParkedRun::Completed { value, .. } => {
                let _ = self.captured.drain();
                Ok(EvalResult::new(
                    value,
                    self.core.session_table().clone(),
                    Vec::new(),
                ))
            }
            ParkedRun::Suspended { .. } => {
                // Scope exit for the throwaway realm — the child's park (and
                // any finalized payload it tenured) must not outlive this
                // call.
                if let Some(m) = self.core.machine_mut() {
                    let _ = m.close_realm(child_realm);
                }
                Err(ResidentError::ChildSuspended)
            }
        }
    }

    /// PURE sibling of [`Self::run_child`]: drives `expr` through
    /// [`JitEffectMachine::run_child_fragment_pure`] instead of
    /// [`JitEffectMachine::run_child_fragment`] — no freer-simple `Val`/`E`
    /// decode, no effect dispatch. `run_child`'s effect-driving path REQUIRES
    /// its fragment to be an `Eff` computation (its compiled result is
    /// classified as the `Val`/`E` union the JIT's calling convention expects
    /// for every effectful entry); a bare, non-monadic function application
    /// — like the `App(Var, arg)` fragment [`Self::apply_finalized`]
    /// synthesizes to apply a finalized closure — produces neither, so
    /// `run_child_fragment`'s classification misreads the plain boxed result's
    /// own constructor tag as an unrecognized `Val`/`E` tag and errors. This is
    /// the PURE run family already used by the machine's own entry
    /// (`run_pure`/`run_fragment_pure`) — [`ResidentSession`] simply did not
    /// expose the child-run sibling before.
    pub fn run_child_pure(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        table: &DataConTable,
        external_env: &ExternalEnv,
    ) -> Result<EvalResult, ResidentError> {
        let func_id = self.prepare_child_fragment(name_hint, expr, table, external_env)?;
        let value = self.on_eval_thread(move |machine, _table, _handlers, _captured| {
            // Plain pure entry: on the parked path the machine is never
            // slot-suspended, so the L7-guarded plain entries serve child
            // fragments directly (the realm suites run fragments over parked
            // frames the same way).
            machine.run_fragment_pure(func_id)
        })?;
        let _ = self.captured.drain();
        Ok(EvalResult::new(
            value,
            self.core.session_table().clone(),
            Vec::new(),
        ))
    }

    /// Shared child-run prelude: requires a suspended parent, merges `table`
    /// into the accumulated session table (monotone), and adds `expr` as a
    /// child fragment on THIS thread (env is `!Send`) — module accretion is
    /// inert for the parent, a fresh FuncId, the stowed continuation
    /// untouched. Shared by [`Self::run_child`]/[`Self::run_child_pure`].
    fn prepare_child_fragment(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        table: &DataConTable,
        external_env: &ExternalEnv,
    ) -> Result<FuncId, ResidentError> {
        if self.parked.is_empty() {
            return Err(ResidentError::NotSuspended);
        }
        self.core
            .merge_table(table)
            .map_err(ResidentError::TableCollision)?;
        // Lazy boot (see `run`'s comment). A child run requires a parked
        // parent, so in practice the machine is always already live by the
        // time this is reachable — kept for symmetry with `run`/`run_bind`
        // and because `bootstrap_if_needed` is a no-op once live.
        self.core
            .bootstrap_if_needed(expr, table)
            .map_err(ResidentError::Bootstrap)?;
        self.core
            .add_child_fragment_session(name_hint, expr, external_env)
            .map_err(ResidentError::AddFunction)
    }

    /// Apply a `finalize`d closure BY REFERENCE (self-iterating-harness W4) to a
    /// boxed `Int` argument, returning the (data) result. The finalized value —
    /// a closure of type `Int -> Int`, say — was tenured into old-space at
    /// suspend time and its persistent root slot stashed on the machine
    /// ([`JitEffectMachine::take_finalized_root`]); this seeds that slot into a
    /// per-call [`ExternalEnv`] and drives a synthesized `App(Var, arg)`
    /// fragment against the SAME suspended heap via [`Self::run_child_pure`] —
    /// the closure is never bridged to a data `Value`, never leaves the heap.
    /// Proves the "code as a value" round-trip: an answerer `finalize`s a
    /// function and the harness runs it in place.
    ///
    /// The argument crosses as a BARE unboxed `Lit`, not a hand-built
    /// `Con(I#, [lit])`: `App`'s argument-boxing (`ensure_heap_ptr`) allocates
    /// a plain `TAG_LIT` heap object, carrying no `DataConId` at all, so no
    /// wrapper-constructor id needs to match anything — the closure's own
    /// `case x of I# n#` was ALREADY compiled Lit-tolerant (`emit_data_dispatch`'s
    /// wrapper-alt path, `tidepool-codegen/src/emit/case.rs`) against its OWN
    /// defining compile's table, which is unrelated to `run_table` here. The
    /// gap this closed was one layer up: [`Self::run_child`] drives its
    /// fragment through the freer-simple `Val`/`E` decode every `Eff`
    /// computation's calling convention expects, and this `App` is a bare,
    /// non-monadic application — [`Self::run_child_pure`] skips that decode.
    ///
    /// Errors if the session is not suspended on a closure-valued finalize (no
    /// finalized root was stashed).
    pub fn apply_finalized(
        &mut self,
        arg: i64,
        run_table: Option<&DataConTable>,
    ) -> Result<EvalResult, ResidentError> {
        // A child (this apply is one) requires a parked parent — the
        // finalize suspension's own frame (top of the stack: apply follows
        // the suspension that stashed the payload).
        let Some(&(_, frame_id)) = self.parked.last() else {
            return Err(ResidentError::NotSuspended);
        };
        // Take the finalized closure's persistent root slot off the parked
        // frame. It stays a registered GC root (release is the owning realm's
        // scope exit), so referencing it by slot address below is GC-safe
        // across the child run.
        let slot = self
            .core
            .machine_mut()
            .and_then(|m| m.take_parked_finalized_root(frame_id))
            .ok_or_else(|| {
                ResidentError::Run(RuntimeError::Jit(JitError::Effect(EffectError::Handler(
                    "no finalized closure to apply (session is not suspended on a \
                     closure-valued finalize)"
                        .into(),
                ))))
            })?;

        // Synthesize `App(Var(FINALIZED_VAR), arg)`. FINALIZED_VAR is any
        // VarId not otherwise bound in this childless fragment — the JIT Var-miss
        // arm keys the external override on ExternalEnv MEMBERSHIP, not the id's
        // tag, so a plain id resolves through the seeded slot.
        const FINALIZED_VAR: tidepool_repr::VarId = tidepool_repr::VarId(0xF4_0000_0001);
        let mut b = tidepool_repr::TreeBuilder::new();
        let f = b.push(tidepool_repr::CoreFrame::Var(FINALIZED_VAR));
        // Pass the argument as a BARE unboxed `Lit` (see this fn's doc): App's
        // argument-boxing allocates a plain `TAG_LIT` object with no
        // `DataConId`, which the closure's own Lit-tolerant `I#` case alt
        // accepts directly.
        let arg_node = b.push(tidepool_repr::CoreFrame::Lit(
            tidepool_repr::Literal::LitInt(arg),
        ));
        let _app = b.push(tidepool_repr::CoreFrame::App {
            fun: f,
            arg: arg_node,
        });
        let expr = b.build();

        let mut env = ExternalEnv::new();
        env.insert(FINALIZED_VAR, slot.addr());

        // The compile table this fragment merges into the accumulated session
        // table (monotone) — the suspend turn's table when known, so the
        // closure's own defining constructors are visible session-wide. Falls
        // back to the accumulated session table.
        let table = run_table
            .cloned()
            .unwrap_or_else(|| self.core.session_table().clone());
        // PURE, not `run_child`: `App(Var, arg)` applies the closure directly —
        // it is not an `Eff` computation, so it must not go through the
        // freer-simple `Val`/`E` decode `run_child` drives (see
        // `run_child_pure`'s doc for why that decode misfires on a plain
        // result).
        self.run_child_pure("apply_finalized", &expr, &table, &env)
    }

    /// Run a handle-rooted BODY as a NEW suspension-capable top-level run under
    /// `realm` — the green-thread fork entry (PRD 20 S1-L4,
    /// `plans/self-iterating-harness/20-s1l4-green-threads.md`).
    ///
    /// This is a THIRD entry beside [`Self::run_child`]/[`Self::run_child_pure`],
    /// not a change to either: their refusal of a suspending child
    /// ([`ResidentError::ChildSuspended`]) is a policy that protects their
    /// value-shaped signatures and stays exactly as it is. What is new here is
    /// a run that MAY park, whose parked frame joins the registry beside every
    /// other, resumable by identity in any order — which is what makes two
    /// green threads blocked on two different effects genuinely concurrent.
    ///
    /// `body` is a `ValueHandle` over a tenured `Int -> M ()` closure — in
    /// practice the one an `AsyncSpawnWith` suspension left on its parked frame
    /// (field 1, tenured by the machine's sentinel-keyed scan and minted via
    /// [`Self::finalized_handle`]). It is applied through the same
    /// `App(Var, Lit)` synthesis [`Self::apply_finalized`] uses — see that
    /// method for why the argument crosses as a bare unboxed `Lit` and needs no
    /// wrapper-constructor id to match. The difference is the DRIVER: this goes
    /// through `run_fragment_suspendable_parked` (suspension-capable, registry-
    /// parking) rather than the pure entry, because a green thread's whole
    /// purpose is to park.
    ///
    /// **`realm` is the thread's, and it propagates.** `resume_parked` replays
    /// a frame's OWN realm, so every later suspension of this thread parks under
    /// `realm` too — which is what makes `close_realm(realm)` a complete
    /// cancellation rather than a first-frame one.
    ///
    /// The body must END by suspending on `AsyncDoneWith` carrying its result,
    /// so a thread's value comes back through the same field-1 crossing it went
    /// out by. Nothing here reads that result: the caller takes it off the
    /// resulting hole exactly as it takes a `finalize` payload.
    pub fn run_forked(
        &mut self,
        name_hint: &str,
        body: ValueHandle,
        realm: RealmId,
        run_table: Option<&DataConTable>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let slot = self
            .core
            .machine_mut()
            .and_then(|m| m.handle_slot(body))
            .ok_or_else(|| {
                ResidentError::Run(RuntimeError::Jit(JitError::Effect(EffectError::Handler(
                    format!(
                        "run_forked: handle {body:?} is not live (never minted, or its \
                         realm was already closed)"
                    ),
                ))))
            })?;

        // `App(Var(FORKED_BODY_VAR), 0)`. Same shape and same reasoning as
        // `apply_finalized`: the Var-miss arm keys the external override on
        // ExternalEnv MEMBERSHIP, and the argument rides as a bare `Lit` whose
        // plain `TAG_LIT` object the closure's own Lit-tolerant `I#` alt
        // accepts. Distinct id from `apply_finalized`'s so the two can never be
        // confused in a trace.
        const FORKED_BODY_VAR: tidepool_repr::VarId = tidepool_repr::VarId(0xF4_0000_0002);
        let mut b = tidepool_repr::TreeBuilder::new();
        let f = b.push(tidepool_repr::CoreFrame::Var(FORKED_BODY_VAR));
        let arg = b.push(tidepool_repr::CoreFrame::Lit(
            tidepool_repr::Literal::LitInt(0),
        ));
        let _app = b.push(tidepool_repr::CoreFrame::App { fun: f, arg });
        let expr = b.build();

        let mut env = ExternalEnv::new();
        env.insert(FORKED_BODY_VAR, slot.addr());

        let table = run_table
            .cloned()
            .unwrap_or_else(|| self.core.session_table().clone());
        self.core
            .merge_table(&table)
            .map_err(ResidentError::TableCollision)?;
        // A fork requires a live machine by construction (the handle came off a
        // parked frame on it), so this is a no-op — kept for symmetry with
        // `run`/`run_bind`.
        self.core
            .bootstrap_if_needed(&expr, &table)
            .map_err(ResidentError::Bootstrap)?;
        // TOP-LEVEL, not `add_child_fragment_session`: a green thread is not a
        // child run over a suspended parent's world, it is a peer.
        let func_id = self
            .core
            .add_fragment_session(name_hint, &expr, &env)
            .map_err(ResidentError::AddFunction)?;

        let ask_tag = self.core.ask_tag();
        let prefix = self.handled_prefix();
        // Handle ownership on completion is the SESSION's realm, deliberately:
        // a result must outlive the thread realm that produced it, since
        // cancelling or retiring a thread closes that realm while a waiter may
        // still be holding the value.
        let owning_realm = self.realm;
        let outcome = self.on_eval_thread(move |machine, table, handlers, captured| {
            machine
                .run_fragment_suspendable_parked(
                    func_id,
                    table,
                    handlers,
                    captured,
                    ask_tag,
                    realm,
                    ParkKind::Plain,
                    &prefix,
                )
                .map(|o| project_parked(machine, o, owning_realm))
        })?;
        Ok(self.classify_parked(outcome, None))
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
        self.reenter(cont_id, ResumeInput::Answer(answer), None)
    }

    /// Resume a suspended value-plane BIND turn: like [`Self::resume`], but on
    /// completion materialize `binder` at `gen` into the value plane — a bind that
    /// suspended at a fork lands its value here. `binder`/`gen` are the SAME ones
    /// the initiating [`Self::run_bind`] carried (threaded by the caller across the
    /// suspension).
    pub fn resume_bind(
        &mut self,
        cont_id: &str,
        answer: Value,
        binder: &BoundBinder,
        gen: Generation,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.reenter(cont_id, ResumeInput::Answer(answer), Some((binder, gen)))
    }

    /// Abort the suspended turn WITHOUT running the continuation — the ask
    /// itself fails (byte-identically to the engine's stowed-abort path). Same
    /// validate-before-consume as [`Self::resume`].
    pub fn abort(
        &mut self,
        cont_id: &str,
        reason: String,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.reenter(cont_id, ResumeInput::Abort(reason), None)
    }

    fn reenter(
        &mut self,
        cont_id: &str,
        input: ResumeInput,
        bind: Option<(&BoundBinder, Generation)>,
    ) -> Result<ResidentOutcome, ResidentError> {
        // Validate BEFORE consuming: `cont_id` must be a MEMBER of the parked
        // set (any-order resume — the machine imposes no order and neither do
        // we). A mismatch leaves every parked frame intact.
        let Some(&(_, frame_id)) = self.parked.iter().find(|(h, _)| h == cont_id) else {
            return Err(ResidentError::WrongContinuation {
                attempted: cont_id.to_string(),
                pending: self.parked.iter().map(|(h, _)| h.clone()).collect(),
            });
        };
        // The machine is authoritative on whether the frame was actually
        // consumed: `resume_parked` NF-forces a data-kinded answer BEFORE
        // removing the frame (A5), and on a retryable rejection leaves it
        // parked and rooted — this hole must NOT be cleared here, or a
        // retryable failure wedges the session. `classify_parked` (on `Ok`)
        // is the sole owner of the parked set on a real outcome. The frame
        // replays its own kind/table/tag, so bind-vs-plain needs no
        // re-declaration here (`bind` is only used for materialization
        // below).
        let realm = self.realm;
        let outcome = self.on_eval_thread(move |machine, _table, handlers, captured| {
            machine
                .resume_parked(frame_id, handlers, captured, input)
                .map(|o| project_parked(machine, o, realm))
        });
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => {
                // Reconcile against the machine's ground truth BY IDENTITY:
                // if the frame is gone from the registry, it WAS consumed
                // before this run failed (a genuine mid-run error, or an
                // abort) — the hole is spent. If it is still parked, this was
                // a retryable rejection (e.g. A5's NF-force) — the hole stays,
                // untouched. A boolean "is the machine suspended" cannot
                // answer this with N frames parked; membership can.
                let still_parked = self
                    .core
                    .machine_mut()
                    .map(|m| m.parked_ids().contains(&frame_id))
                    .unwrap_or(false);
                if !still_parked {
                    self.parked.retain(|(h, _)| h != cont_id);
                }
                return Err(e);
            }
        };
        // A completed bind materializes AFTER `classify_parked` has already
        // retired this hole, so a materialize failure cannot leave the hole
        // stuck on a frame the machine no longer holds.
        let bound = match &outcome {
            ParkedRun::Completed { bound, .. } => *bound,
            ParkedRun::Suspended { .. } => None,
        };
        let completed = matches!(outcome, ParkedRun::Completed { .. });
        let resident_outcome = self.classify_parked(outcome, Some(cont_id));
        if let (Some((binder, gen)), true) = (bind, completed) {
            self.materialize_binder(binder, gen, bound)?;
        }
        Ok(resident_outcome)
    }

    /// Materialize a completed bind's tenured root into the value plane at `gen`
    /// (the generation the extract stamped into `binder.module`). Mirrors the
    /// repl's `bind_materialized`: the session layer owns the `BindingEntry`
    /// construction, the core owns the plane. Evicts any same-name decl (the
    /// one-plane invariant — a value bind wins over an earlier decl head).
    fn materialize_binder(
        &mut self,
        binder: &BoundBinder,
        gen: Generation,
        bound: Option<ValueHandle>,
    ) -> Result<(), ResidentError> {
        // The tenured root rode out of the eval thread as a `Send` handle
        // (pillar-B laundering); resolve it back to its slot HERE, on the
        // session thread where the `BindingTable` lives, and release the
        // handle — ownership transfers to the value plane (the persistent
        // root registration is untouched; a realm scope-exit no longer sees
        // it).
        let handle = bound.ok_or_else(|| {
            ResidentError::Run(RuntimeError::Jit(JitError::Effect(EffectError::Handler(
                "value-plane bind completed but no tenured root was recorded".into(),
            ))))
        })?;
        let slot = self
            .core
            .machine_mut()
            .and_then(|m| {
                let slot = m.handle_slot(handle);
                m.release_handle(handle);
                slot
            })
            .ok_or_else(|| {
                ResidentError::Run(RuntimeError::Jit(JitError::Effect(EffectError::Handler(
                    "value-plane bind completed but its handle was unknown to the machine".into(),
                ))))
            })?;
        let value = match binder.tier {
            ValueTier::Tier0Data => BoundValue::Tier0Forced(slot),
            ValueTier::Tier1Closure => BoundValue::Tier1Closure(slot),
        };
        // Evict any pure decl of the same name before binding (cross-plane shadow).
        self.core.retract(&binder.name)?;
        self.core.bind(BindingEntry {
            name: BindingName(binder.name.clone()),
            id: SessionVarId::from_extract(binder.var_id),
            module: SessionModule::val(gen),
            value,
            type_display: Some(binder.type_display.clone()),
            defining_expr: None,
        });
        self.core.set_val_gen(gen);
        Ok(())
    }

    /// Move the machine onto a stack-sized eval thread, run `body`, and move the
    /// machine back. E2 lets `body` run on this fresh thread — the threadless
    /// mechanism's `run_fragment`/`resume` re-install the machine's per-thread
    /// reach and re-point GC state at the retained heap. Only the machine (and
    /// the accumulated table) crosses to the thread; the rest of the session
    /// core is `!Send` (raw-pointer roots) and stays here.
    fn on_eval_thread<F, T>(&mut self, body: F) -> Result<T, ResidentError>
    where
        T: Send,
        F: FnOnce(&mut JitEffectMachine, &DataConTable, &mut H, &O) -> Result<T, JitError> + Send,
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

    /// Classify a projected parked outcome into a [`ResidentOutcome`]:
    /// completion retires `resumed` (the hole this outcome answered — `None`
    /// for a fresh run, which retires nothing), suspension mints a hole and
    /// pushes `(hole, id)` onto the parked set. Output is drained on
    /// completion and snapshotted on suspension, same as the engine.
    fn classify_parked(&mut self, outcome: ParkedRun, resumed: Option<&str>) -> ResidentOutcome {
        match outcome {
            ParkedRun::Completed { value, .. } => {
                if let Some(hole) = resumed {
                    self.parked.retain(|(h, _)| h != hole);
                }
                let output = self.captured.drain();
                ResidentOutcome::Completed {
                    output,
                    result: EvalResult::new(value, self.core.session_table().clone(), Vec::new()),
                }
            }
            ParkedRun::Suspended { id, request } => {
                // A resume that re-suspended: the OLD hole is spent (the
                // frame was consumed; a fresh frame parked under a FRESH id —
                // ids are never reused) and the new one replaces it.
                if let Some(hole) = resumed {
                    self.parked.retain(|(h, _)| h != hole);
                }
                let hole = self.next_cont_id();
                self.parked.push((hole.clone(), id));
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

/// The `Send` projection of a [`ParkedOutcome`] that crosses the eval-thread
/// boundary: a bind's tenured `!Send` `RootSlot` is minted into a
/// [`ValueHandle`] IN-THREAD (`realm`-owned) and the id crosses instead —
/// resolved back to its slot by `materialize_binder` on the session thread.
/// `CompletedProject`/`CompletedRender` are unreachable on this lane (the
/// resident session parks only `Plain`/`Binding`); the finalized-closure flag
/// is dropped (the harness detects that case from the `CLOSURE_SENTINEL` in
/// the request, and the payload itself is read per-frame at apply time).
enum ParkedRun {
    Completed {
        value: Value,
        bound: Option<ValueHandle>,
    },
    Suspended {
        id: ContinuationId,
        request: Value,
    },
}

/// Project a [`ParkedOutcome`] to [`ParkedRun`] on the eval thread (see
/// [`ParkedRun`]'s doc).
fn project_parked(
    machine: &mut JitEffectMachine,
    outcome: ParkedOutcome,
    realm: tidepool_codegen::jit_machine::RealmId,
) -> ParkedRun {
    match outcome {
        ParkedOutcome::Completed { value, bound_root } => ParkedRun::Completed {
            value,
            bound: bound_root.map(|slot| machine.mint_handle_from_root(slot, realm)),
        },
        ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
            unreachable!("the resident lane parks only Plain/Binding turns")
        }
        ParkedOutcome::Suspended { id, request, .. } => ParkedRun::Suspended { id, request },
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
