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

use tidepool_codegen::scope::ScopeId;

use super::engine::OutputSink;
use super::persistent::{PersistentSession, ScopeRetirement};
use super::turn::{BoundBinder, ValueTier};
use super::{SessionError, SessionLib};

/// A LINEAR custody token over a [`ValueHandle`] between the moment it enters
/// Rust-side custody — minted by [`ResidentSession::finalized_handle`] — and
/// the moment it is consumed: delivered into a sibling continuation
/// ([`ResidentSession::resume_handle`]) or mounted into a named binding
/// ([`ResidentSession::mount_handle`]/[`ResidentSession::mount_handle_in`]).
///
/// Deliberately NOT `Clone`/`Copy`, unlike [`ValueHandle`] itself (which stays
/// freely copyable at the machine layer — `tidepool_codegen::jit_machine`
/// tests read a handle non-linearly on purpose: `observe_handle`,
/// `handle_realm`, repeated `ResumeInput::Handle`, all borrows). At THIS
/// layer, the three-owner chain documented at [`ResidentSession::mount_handle_in`]'s
/// doc — handle registry, scope frame, GC root ledger, never two at once — used
/// to be enforced only by caller discipline plus the machine's debug-mode
/// `handle_holds_root` assert at scope retirement: a caller that mistakenly
/// handed the SAME live [`ValueHandle`] to both `resume_handle` and
/// `mount_handle_in` would not be caught until that assert fired, if it ever
/// ran (a resume delivery does not consume its handle from the machine's own
/// registry — see [`ResidentSession::resume_handle`]'s doc — so nothing at the
/// machine layer stops a second use of the same numeric id). Wrapping the
/// crossing here moves the check to COMPILE TIME: the only way to recover the
/// raw handle is [`Self::into_handle`], which consumes `self` by value, so a
/// second consumer has nothing left to consume — a use-after-move `rustc`
/// error, not a runtime race. See the compile-fail example below.
///
/// Dropping an unconsumed token means custody was LOST — a finalized value was
/// taken out of the machine and never delivered or mounted, so nothing will
/// ever explicitly release it (it still dies at the owning realm's
/// `close_realm`, exactly as before this token existed — this is a lint on
/// Rust-side bookkeeping, not a memory-safety backstop). Loud in debug builds
/// so the mistake surfaces at the call site that dropped it; silent in
/// release, matching every other debug-only assert in this custody chain.
///
/// # On an error path, dispose of custody BEFORE `?`
///
/// **A `Drop` panic is a leak DETECTOR, not an error channel.** An arm holding
/// live custody that hits `?` converts a perfectly recoverable `Err` into this
/// type's `Drop` panic: the token correctly notices the leak, but it REPLACES
/// the diagnosis — the caller sees "custody was lost" and never sees the error
/// that caused the early return. So on any fallible path, consume or
/// deliberately abandon custody before propagating.
///
/// The structural fix is usually better than the careful one: order the work
/// so nothing fallible sits between the mint and the consume. Two windows in
/// the green-thread driver taught this (`SelfHarnessDriver`'s `AsyncSpawnWith`
/// and `AsyncDoneWith` arms) — one had two `?`s between minting a thread body
/// and forking it, the other minted a result and then consumed it only inside
/// a state guard. Both were fixed by moving the fallible work out of the
/// window rather than by adding cleanup to each exit.
///
/// ```compile_fail
/// use tidepool_codegen::jit_machine::ValueHandle;
/// use tidepool_runtime::session::RootCustody;
///
/// let custody = RootCustody::new(ValueHandle(0));
/// let delivered = custody.into_handle();   // first (and only legal) consumer
/// let mounted = custody.into_handle();     // ERROR: `custody` was already moved
/// ```
///
/// (`new` is `pub(crate)` — see its own doc — so from outside this crate the
/// block above now also fails on privacy, before it ever reaches the
/// use-after-move it demonstrates. `compile_fail` only asserts "does not
/// compile", so that is still a true, still-enforced statement; the
/// use-after-move contract this docstring is about is exercised in-crate by
/// [`Self::mount_handle_in`]'s and [`Self::resume_handle`]'s own callers.)
#[derive(Debug)]
pub struct RootCustody(Option<ValueHandle>);

impl RootCustody {
    /// Mint a custody token over `handle`. `pub(crate)`, not `pub`: the two
    /// real mint sites ([`ResidentSession::finalized_handle`],
    /// [`ResidentSession::finalized_handle_owned_by`]) and every consumer
    /// live in this crate, so scoping construction to the crate is enough to
    /// turn fabrication into a reviewable, visible-intent act rather than a
    /// call any external dependent is one line away from making (codex
    /// review 2026-08-19, HIGH: "`RootCustody::new` is pub and one call from
    /// forging custody"). Production code never reaches for this directly —
    /// `finalized_handle`/`finalized_handle_owned_by` never hand out a bare
    /// [`ValueHandle`] at the finalize seam in the first place.
    pub(crate) fn new(handle: ValueHandle) -> Self {
        RootCustody(Some(handle))
    }

    /// Consume the token, releasing the raw handle to the caller — the ONLY
    /// way out. Every legitimate custody transfer (a mount, a resume
    /// delivery) goes through this exactly once. Takes `self` by value, so a
    /// second attempt to consume the SAME token is a compile-time
    /// use-after-move error (see the compile-fail example above) rather than
    /// a runtime double-custody bug.
    pub fn into_handle(mut self) -> ValueHandle {
        self.0
            .take()
            .expect("RootCustody always holds a handle until into_handle consumes it")
    }
}

impl Drop for RootCustody {
    fn drop(&mut self) {
        let Some(handle) = self.0 else { return };
        let detail = format!(
            "RootCustody dropped without being consumed — {handle:?}'s custody was lost \
             (never delivered via resume_handle, never mounted). The machine-side root is \
             unaffected (it releases at the owning realm's close_realm regardless), but the \
             value silently never reached wherever it was headed."
        );
        // NEVER panic while an unwind is already in flight. Two reasons, and
        // the second is why this guard exists at all:
        //
        // 1. A panic during unwinding is an immediate ABORT — no backtrace for
        //    the original fault, no test-harness failure report, nothing.
        // 2. The original panic is the INTERESTING one. This type is a leak
        //    detector; a leak observed while something else is already failing
        //    is almost always a CONSEQUENCE of that failure (the servicing path
        //    unwound past a delivery), not an independent bug. Eating the real
        //    diagnosis to report the consequence is exactly backwards, and it
        //    has already happened once here: a case trap surfaced as a custody
        //    panic, sending the reader after the wrong defect.
        //
        // Note this does NOT cover the `?`-on-a-fallible-path case in this
        // type's doc — an `Err` return is not an unwind, so `panicking()` is
        // false and the assert below still fires. That case is a real leak and
        // must stay loud; it is fixed by ordering, not by suppression.
        if std::thread::panicking() {
            tracing::error!("{detail} (reported during an active unwind, so not raised)");
            return;
        }
        debug_assert!(false, "{}", detail);
    }
}

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
    /// The resident eval thread could not be spawned (a transient OS
    /// resource failure — thread-limit exhaustion, out of memory). The
    /// session's machine is restored before this is returned; a retry is
    /// safe.
    #[error("failed to spawn resident eval thread: {0}")]
    EvalThread(std::io::Error),
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
    /// The scope tree node every turn this session runs is compiled and bound
    /// in ([`ScopeId::ROOT`] until [`ResidentSession::set_scope`]). The realm
    /// field above is the HEAP-side lifetime (parked frames, handles); this is
    /// the NAME-side one (decl tips, value-plane frames). A window carries
    /// both, and retiring it exits both — see `Harness::terminate_node`.
    scope: ScopeId,
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
            scope: ScopeId::ROOT,
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
            scope: ScopeId::ROOT,
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

    /// Scoped [`Self::define_scoped`]: append to `scope`'s own decl tip, which
    /// already re-exports its ancestors' — so the definition is visible to
    /// `scope` and its descendants and to nobody else. `define_scoped(d) ==
    /// define_scoped_in(ScopeId::ROOT, d)`.
    pub fn define_scoped_in(
        &mut self,
        scope: ScopeId,
        decls: &[&str],
    ) -> Result<tidepool_repr::Generation, SessionError> {
        self.core.define_scoped_in(scope, decls)
    }

    /// The current decl-plane module name (`Tidepool.Session.Lib.G<g>`) a later
    /// turn imports to see accumulated declarations, or `None` before any decl.
    pub fn session_import_module(&self) -> Option<String> {
        self.core.current_lib_module().map(|m| m.module_name())
    }

    /// Scoped [`Self::session_import_module`]: the `Lib.G<g>` module at
    /// `scope`'s tip. A turn compiled in a child scope imports THIS, not
    /// ROOT's — which is the whole of "parent declarations callable in every
    /// child" on the real compile path, since the child's tip module re-exports
    /// its parent's chain.
    pub fn session_import_module_in(&self, scope: ScopeId) -> Option<String> {
        self.core
            .current_lib_module_in(scope)
            .map(|m| m.module_name())
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

    /// Scoped [`Self::current_val_modules`]: the `Val.G<g>` module per name
    /// VISIBLE at `scope` — its own frame first, then each ancestor's, nearest
    /// frame winning. A sibling scope's bindings are never in this list, so a
    /// turn compiled here cannot even name them.
    pub fn current_val_modules_in(&self, scope: ScopeId) -> Vec<String> {
        self.core.current_val_modules_in(scope)
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

    /// Compile and bind every subsequent turn in `scope` (PRD 21 lane C2): the
    /// turn imports `scope`'s decl tip and the `Val.G<g>` modules VISIBLE from
    /// it, and a value-plane bind lands in `scope`'s own frame. The harness
    /// applies a node's scope here at the same site it applies its realm, so a
    /// node without one keeps compiling and binding at [`ScopeId::ROOT`] —
    /// exactly its pre-C2 behavior.
    ///
    /// Rejects a dead `scope` (never minted, or already retired) with a typed
    /// [`ResidentError`] and leaves [`Self::current_scope`] UNCHANGED — a
    /// failed assignment must not silently rebind subsequent turns to a
    /// dead frame. [`ScopeId::ROOT`] is always live, so this is a no-op
    /// widening for every pre-C2 caller.
    pub fn set_scope(&mut self, scope: ScopeId) -> Result<(), ResidentError> {
        if !self.core.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope).into());
        }
        self.scope = scope;
        Ok(())
    }

    /// The scope this session's turns currently compile and bind in.
    pub fn current_scope(&self) -> ScopeId {
        self.scope
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
    pub fn finalized_handle(&mut self, hole: &str) -> Option<RootCustody> {
        let &(_, id) = self.parked.iter().find(|(h, _)| h == hole)?;
        self.core
            .machine_mut()?
            .handle_from_finalized(id)
            .map(RootCustody::new)
    }

    /// [`Self::finalized_handle`]'s sibling for a result that must outlive
    /// the frame's OWN realm: mint the handle owned by `realm` instead (a
    /// green thread's `AsyncDoneWith` payload, owned by the SESSION's realm
    /// so a waiter's handle survives the thread's own realm later closing —
    /// PRD 20 S1-L4, `ResidentSession::run_forked`'s doc). Same
    /// frame-stays-parked semantics; `None` under the same conditions.
    /// Returns a [`RootCustody`] token, exactly as [`Self::finalized_handle`]
    /// does: minting under a different realm changes WHO owns the root, never
    /// whether the handle needs consuming exactly once.
    pub fn finalized_handle_owned_by(&mut self, hole: &str, realm: RealmId) -> Option<RootCustody> {
        let &(_, id) = self.parked.iter().find(|(h, _)| h == hole)?;
        let machine = self.core.machine_mut()?;
        let slot = machine.take_parked_finalized_root(id)?;
        Some(RootCustody::new(machine.mint_handle_from_root(slot, realm)))
    }

    /// Resume the turn parked on `cont_id` by DELIVERING a machine-side
    /// rooted value — the handle's payload feeds the continuation verbatim,
    /// no materialization, closures included (pillar B's delivery half; the
    /// one-session loop receives its `State -> State` this way). Same
    /// validate-before-consume and ground-truth reconciliation as
    /// [`Self::resume`].
    ///
    /// Takes the [`RootCustody`] token by value — this IS the consuming half
    /// of the custody crossing (see that type's doc): the delivery itself
    /// does not release the handle from the machine's own registry (a resume
    /// is a scope-owned BORROW at the machine layer, same as `observe_handle`),
    /// so without the token nothing at this layer stops a caller from also
    /// mounting the same raw handle. The token is unwrapped once, here, at
    /// the moment its custody is spent.
    pub fn resume_handle(
        &mut self,
        cont_id: &str,
        custody: RootCustody,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.reenter(cont_id, ResumeInput::Handle(custody.into_handle()), None)
    }

    /// The current value-plane binding for `name` — `(SessionVarId, module,
    /// tier, type display)` — if one is live. The mount seam (PRD 21 lane
    /// C1) reads this off a THROWAWAY same-type placeholder bind (any
    /// ordinary `x <- e` turn of the target type) to recover the already-
    /// minted `Val.G<g>` iface identity that [`Self::mount_handle`] then
    /// redirects to a value that arrived by a different path (a cross-node
    /// finalize handle) — no second iface is ever minted for the same name.
    pub fn current_binding(
        &self,
        name: &str,
    ) -> Option<(SessionVarId, SessionModule, ValueTier, Option<String>)> {
        self.current_binding_in(ScopeId::ROOT, name)
    }

    /// Scoped [`Self::current_binding`]: the binding `name` resolves to as seen
    /// FROM `scope` — its own frame first, then each ancestor up to ROOT, so a
    /// child reads a parent's mounts and a local mount shadows an inherited
    /// one. `current_binding(n) == current_binding_in(ScopeId::ROOT, n)`.
    pub fn current_binding_in(
        &self,
        scope: ScopeId,
        name: &str,
    ) -> Option<(SessionVarId, SessionModule, ValueTier, Option<String>)> {
        let entry = self.core.resolve_in(scope, name)?;
        let tier = match entry.value {
            BoundValue::Tier0Forced(_) => ValueTier::Tier0Data,
            BoundValue::Tier1Closure(_) => ValueTier::Tier1Closure,
        };
        Some((entry.id, entry.module, tier, entry.type_display.clone()))
    }

    /// Redirect an ALREADY-MINTED value-plane binding (`id`/`module`, read
    /// via [`Self::current_binding`]) to resolve through `handle`'s tenured
    /// payload instead of whatever it was bound to before — the mount seam
    /// (PRD 21 lane C1): "a handle installed under a name in a window's
    /// declaration scope", the closure-tenure-then-handle delivery path
    /// (pillar B) pointed the OTHER direction. `handle` is consumed exactly
    /// like an ordinary bind completion ([`Self::materialize_binder`]): its
    /// slot is read, the handle released from the machine's handle registry
    /// (ownership transfers to the value plane — a live [`BindingTable`]
    /// entry, ended only by the session machine dropping, never by a realm
    /// scope exit), and re-registered under the SAME `SessionVarId`/module a
    /// turn compiled against `name` already resolves through. No new
    /// `Val.G<g>` iface is minted here and the GHC-side type binding is
    /// unchanged — only WHICH heap object it points at moves. Errors if
    /// `handle` is not live (already released, or never minted).
    pub fn mount_handle(
        &mut self,
        name: &str,
        id: SessionVarId,
        module: SessionModule,
        tier: ValueTier,
        type_display: Option<String>,
        custody: RootCustody,
    ) -> Result<(), ResidentError> {
        self.mount_handle_in(ScopeId::ROOT, name, id, module, tier, type_display, custody)
    }

    /// Scoped [`Self::mount_handle`]: install the mount in `scope`'s frame
    /// instead of the flat session's. `mount_handle(..) ==
    /// mount_handle_in(ScopeId::ROOT, ..)`, so the flat mount path is
    /// bit-for-bit what it was.
    ///
    /// A scoped mount is what [`Self::retire_scope`] later releases: the
    /// handle registry hands ownership of the tenured root to this frame
    /// (`value_handle_count` drops as the frame's count rises), and the frame
    /// hands it to the GC root ledger's `retire_scope_root` at retirement —
    /// three named owners in sequence, never two at once. `custody` is the
    /// [`RootCustody`] token minted by [`Self::finalized_handle`]; consumed
    /// exactly once, here, at the moment ownership hands off to the frame.
    ///
    /// **Liveness is checked FIRST, before `custody` is touched.** A dead
    /// `scope` (never minted, or already retired) is rejected with a typed
    /// [`ResidentError`] and `custody` is disposed deliberately (its handle
    /// is taken and dropped, never mounted) — RootCustody's own doc requires
    /// consuming or deliberately abandoning custody before propagating an
    /// error, so its leak-detecting `Drop` never fires. The raw machine-side
    /// handle is left exactly as it was: still registered, released only at
    /// the owning realm's eventual `close_realm`, same as any other
    /// never-mounted finalize. Checking BEFORE `release_handle` also matters
    /// operationally: once the handle registry releases a root, only a
    /// successful bind gives it a new owner — a dead scope caught only by
    /// [`PersistentSession::bind_in`]'s own backstop check would otherwise
    /// leave that root untracked by every accounting class at once.
    #[allow(clippy::too_many_arguments)]
    pub fn mount_handle_in(
        &mut self,
        scope: ScopeId,
        name: &str,
        id: SessionVarId,
        module: SessionModule,
        tier: ValueTier,
        type_display: Option<String>,
        custody: RootCustody,
    ) -> Result<(), ResidentError> {
        if !self.core.scope_tree().is_live(scope) {
            let _ = custody.into_handle();
            return Err(SessionError::DeadScope(scope).into());
        }
        let handle = custody.into_handle();
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
                    "mount: handle is unknown to the machine (already released or never minted)"
                        .into(),
                ))))
            })?;
        let value = match tier {
            ValueTier::Tier0Data => BoundValue::Tier0Forced(slot),
            ValueTier::Tier1Closure => BoundValue::Tier1Closure(slot),
        };
        self.core.bind_in(
            scope,
            BindingEntry {
                name: BindingName(name.to_string()),
                id,
                module,
                value,
                type_display,
                defining_expr: None,
                // Overwritten by `bind_in` with `scope`; see `BindingEntry`.
                scope,
            },
        )?;
        Ok(())
    }

    /// Deliver an already-session-owned root into a parked continuation
    /// WITHOUT consuming custody — the REPEAT-delivery case.
    ///
    /// [`Self::resume_handle`]'s custody token guards a change of OWNER, not a
    /// delivery: at the machine layer a resume is a scope-owned BORROW (same
    /// as `observe_handle`), and the root is released by its owning realm's
    /// scope exit, never by a delivery. A green thread's result is exactly
    /// that shape — the root is minted under the SESSION's realm at settle
    /// time (see [`Self::finalized_handle_owned_by`]) and may then be read
    /// more than once: `poll` then `wait`, or two waiters joined on one
    /// thread. Each of those is another borrow of one root, not a second
    /// transfer of one custody.
    ///
    /// Use [`Self::resume_handle`] wherever the delivery IS the transfer (the
    /// finalize seam). Reach for this only when an owner already exists and
    /// outlives every delivery.
    pub fn resume_handle_borrowed(
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

    /// Number of live [`ValueHandle`]s outstanding on this session's machine
    /// (0 before the machine is bootstrapped) — the mount seam's ownership-
    /// accounting read: a handle minted over a finalize payload
    /// ([`Self::finalized_handle`]) counts here until [`Self::mount_handle`]
    /// (or an ordinary bind completion / realm close) releases it.
    pub fn value_handle_count(&self) -> usize {
        self.core.machine().map_or(0, |m| m.value_handle_count())
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

    // -- scopes (PRD 21 lane C2) -------------------------------------------

    /// Mint a fresh child scope of `parent` ([`ScopeId::ROOT`] for a top-level
    /// invocation scope). `None` if `parent` is not live.
    pub fn mint_scope(&mut self, parent: ScopeId) -> Option<ScopeId> {
        self.core.mint_scope(parent)
    }

    /// The value-plane names VISIBLE at `scope` — its own frame plus every
    /// ancestor's, nearest frame winning. `binding_names_in(ScopeId::ROOT)` is
    /// [`Self::binding_names`]'s set (sorted).
    pub fn binding_names_in(&self, scope: ScopeId) -> Vec<String> {
        self.core
            .bindings()
            .iter_current_in(self.core.scope_tree(), scope)
            .into_iter()
            .map(|(name, _)| name.0.clone())
            .collect()
    }

    /// How many names `scope`'s OWN frame binds (accounting class 3, per
    /// scope — inherited names are not counted, only locally-bound ones).
    /// Returns to 0 when the scope retires.
    pub fn scope_binding_count(&self, scope: ScopeId) -> usize {
        self.core.scope_binding_count(scope)
    }

    /// Number of persistent GC roots registered on this session's machine
    /// (accounting class 4 — the GC ROOT LEDGER; 0 before the machine
    /// bootstraps). This is the WITNESS for [`Self::retire_scope`]: it drops
    /// by exactly the receipt's `roots_released` and by nothing else.
    ///
    /// Deliberately separate from classes 1 (`stowed_roots_count() ==
    /// parked_count()`) and 2 ([`Self::value_handle_count`]), which a scope
    /// retirement leaves untouched — folding them together is what makes a
    /// leak invisible.
    pub fn persistent_roots_count(&self) -> usize {
        self.core.persistent_roots_count()
    }

    /// Accounting class 1 — the PARKED-CONTINUATION roots, as the pair that
    /// must always agree (`stowed_roots_count() == parked_count()`, the
    /// machine's own quiescence invariant). 0 before the machine bootstraps.
    /// A scope retirement must leave both UNCHANGED: a parked frame's root is
    /// a realm's, not a scope's, and folding the two classes together is how a
    /// leak becomes invisible.
    pub fn stowed_roots_count(&self) -> usize {
        self.core
            .machine()
            .map_or(0, JitEffectMachine::stowed_roots_count)
    }

    /// The parked-frame half of accounting class 1 — see
    /// [`Self::stowed_roots_count`].
    pub fn parked_count(&self) -> usize {
        self.core
            .machine()
            .map_or(0, JitEffectMachine::parked_count)
    }

    /// Retire `scope` and its subtree: drop their value-plane frames and
    /// release the GC roots those bindings solely owned. See
    /// [`PersistentSession::retire_scope`] for the sole-ownership rule and the
    /// deregistered-is-not-reclaimed bound; retiring ROOT or an already-retired
    /// scope is a no-op returning an all-zero receipt.
    pub fn retire_scope(&mut self, scope: ScopeId) -> ScopeRetirement {
        self.core.retire_scope(scope)
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
        body: RootCustody,
        realm: RealmId,
        run_table: Option<&DataConTable>,
    ) -> Result<ResidentOutcome, ResidentError> {
        // Forking CONSUMES the body's custody: the thread that runs it is the
        // handle's new owner, and there is no second consumer. Taking the
        // token by value is what makes that a compile-time fact rather than a
        // convention.
        let body = body.into_handle();
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
    ///
    /// Checks `self.scope`'s liveness FIRST, before `handle` is resolved and
    /// released from the machine's handle registry — same ordering reason as
    /// [`Self::mount_handle_in`]: once `release_handle` runs, only a
    /// successful bind gives the root a new owner, so a dead scope caught
    /// only by [`PersistentSession::bind_in`]'s backstop would leave it
    /// untracked by every accounting class at once. In ordinary operation
    /// `self.scope` cannot go dead mid-turn ([`Self::set_scope`] already
    /// refuses a dead scope), so this guards a defensive precondition rather
    /// than a reachable steady-state path.
    fn materialize_binder(
        &mut self,
        binder: &BoundBinder,
        gen: Generation,
        bound: Option<ValueHandle>,
    ) -> Result<(), ResidentError> {
        if !self.core.scope_tree().is_live(self.scope) {
            return Err(SessionError::DeadScope(self.scope).into());
        }
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
        // Evict any pure decl of the same name before binding (cross-plane
        // shadow: a name lives in at most one plane). SCOPED to the binding's
        // OWN scope — a child binding `helper` retracts the child's decl head,
        // never the parent's, because nothing in this tree ever walks downward.
        // At ROOT this is byte-for-byte the pre-C2 retraction.
        self.core.retract_in(self.scope, &binder.name)?;
        self.core.bind_in(
            self.scope,
            BindingEntry {
                name: BindingName(binder.name.clone()),
                id: SessionVarId::from_extract(binder.var_id),
                module: SessionModule::val(gen),
                value,
                type_display: Some(binder.type_display.clone()),
                defining_expr: None,
                scope: self.scope,
            },
        )?;
        self.core.set_val_gen(gen);
        Ok(())
    }

    /// Move the machine onto a stack-sized eval thread, run `body`, and move the
    /// machine back. E2 lets `body` run on this fresh thread — the threadless
    /// mechanism's `run_fragment`/`resume` re-install the machine's per-thread
    /// reach and re-point GC state at the retained heap. Only the machine (and
    /// the accumulated table) crosses to the thread; the rest of the session
    /// core is `!Send` (raw-pointer roots) and stays here.
    ///
    /// The machine is taken via [`MachineGuard`], whose `Drop` restores it into
    /// `self.core` on EVERY exit from this function — success, a `JitError`, a
    /// caught panic, or a failed thread spawn (a transient OS resource
    /// failure, not a bug) — so no path can leave the session permanently
    /// machineless.
    fn on_eval_thread<F, T>(&mut self, body: F) -> Result<T, ResidentError>
    where
        T: Send,
        F: FnOnce(&mut JitEffectMachine, &DataConTable, &mut H, &O) -> Result<T, JitError> + Send,
    {
        self.on_eval_thread_with_stack(EVAL_STACK_SIZE, body)
    }

    /// [`Self::on_eval_thread`], with the eval thread's stack size as a
    /// parameter rather than the hardcoded [`EVAL_STACK_SIZE`] — split out
    /// so a test can force `spawn_scoped` to fail deterministically (an
    /// absurd stack size) without changing production eval-thread semantics,
    /// which always go through [`Self::on_eval_thread`]'s fixed constant.
    fn on_eval_thread_with_stack<F, T>(
        &mut self,
        stack_size: usize,
        body: F,
    ) -> Result<T, ResidentError>
    where
        T: Send,
        F: FnOnce(&mut JitEffectMachine, &DataConTable, &mut H, &O) -> Result<T, JitError> + Send,
    {
        let mut guard = MachineGuard::take(&mut self.core);
        let table = guard.core.session_table();
        let machine_ref = guard
            .machine
            .as_mut()
            .expect("machine present while guard is alive");
        let handlers = &mut self.handlers;
        // The sink is Arc-backed (`OutputSink: Clone + Send`) and shares its
        // buffer; move a clone onto the thread rather than requiring `O: Sync`
        // for a borrow — matches the oneshot engine's `captured.clone()`.
        let captured = self.captured.clone();

        // A scoped thread borrows `machine_ref`/`handlers`/`table`/`captured`
        // from this frame. `EVAL_STACK_SIZE` matches the oneshot eval thread
        // (deep JIT recursion needs it), so `Builder::spawn_scoped` (the
        // stack-sized form of `scope.spawn`) is used. Unlike the oneshot form,
        // a failed spawn here is reported through `outcome`, not `.expect()` —
        // `guard` is still alive and restores the machine either way.
        let outcome = std::thread::scope(|scope| {
            match std::thread::Builder::new()
                .name("tidepool-resident-eval".into())
                .stack_size(stack_size)
                .spawn_scoped(scope, || {
                    tidepool_codegen::signal_safety::install();
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        body(machine_ref, table, handlers, &captured)
                    }))
                }) {
                Ok(handle) => match handle.join() {
                    Ok(Ok(body_result)) => EvalThreadOutcome::Ran(body_result),
                    Ok(Err(panic)) => EvalThreadOutcome::Panicked(panic),
                    Err(join_panic) => EvalThreadOutcome::Panicked(join_panic),
                },
                Err(spawn_err) => EvalThreadOutcome::SpawnFailed(spawn_err),
            }
        });

        // `guard` drops here (function-end, on every path above), restoring
        // the machine into `self.core` regardless of how `outcome` resolved.
        match outcome {
            EvalThreadOutcome::Ran(Ok(t)) => Ok(t),
            EvalThreadOutcome::Ran(Err(e)) => Err(ResidentError::Run(RuntimeError::Jit(e))),
            EvalThreadOutcome::Panicked(payload) => Err(panic_to_run_error(payload)),
            EvalThreadOutcome::SpawnFailed(e) => Err(ResidentError::EvalThread(e)),
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

/// RAII restore guard for [`ResidentSession::on_eval_thread`]: holds the
/// session's machine, taken via [`PersistentSession::take_machine`], and
/// restores it into `core` on `Drop` — on EVERY exit from the borrowing
/// function, including an early return between the take and the point the
/// turn's outcome is known (a failed thread spawn, a caught panic). This is
/// what closes the gap the external review flagged: `spawn_scoped(...).expect(...)`
/// used to panic AFTER the machine was taken and BEFORE it was restored,
/// permanently leaving the session machineless past that unwind.
struct MachineGuard<'a> {
    core: &'a mut PersistentSession,
    machine: Option<JitEffectMachine>,
}

impl<'a> MachineGuard<'a> {
    fn take(core: &'a mut PersistentSession) -> Self {
        let machine = core.take_machine();
        MachineGuard {
            core,
            machine: Some(machine),
        }
    }
}

impl Drop for MachineGuard<'_> {
    fn drop(&mut self) {
        if let Some(machine) = self.machine.take() {
            self.core.restore_machine(machine);
        }
    }
}

/// The three ways a resident eval thread's lifecycle can resolve — spawn
/// failure, a caught panic, or a completed run of `body` (itself carrying its
/// own `Result`). Distinct from `SpawnError`/join-panic being conflated into
/// one `.expect()`, which is exactly what let a spawn failure escape as an
/// unguarded panic before this fix.
enum EvalThreadOutcome<T> {
    Ran(Result<T, JitError>),
    Panicked(Box<dyn std::any::Any + Send>),
    SpawnFailed(std::io::Error),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{CoreFrame, Literal, TreeBuilder};

    /// A no-op sink — these tests never suspend or produce output.
    #[derive(Clone, Default)]
    struct NullSink;

    impl OutputSink for NullSink {
        fn drain(&self) -> Vec<String> {
            Vec::new()
        }
        fn snapshot(&self) -> Vec<String> {
            Vec::new()
        }
    }

    /// A trivial, hand-built `Lit` expression over an empty table — no GHC
    /// extract needed. `ConTags` resolution (`Val`/`E`/`Union`/`Leaf`/`Node`)
    /// is LAZY on a compiled [`JitEffectMachine`] (its `tags` field is a
    /// `Result`, not resolved eagerly), so an empty table compiles fine as
    /// long as nothing ever dispatches an effect — true in every test below,
    /// since the eval thread never actually runs `body`.
    fn trivial_expr_and_table() -> (CoreExpr, DataConTable) {
        let mut b = TreeBuilder::new();
        b.push(CoreFrame::Lit(Literal::LitInt(42)));
        (b.build(), DataConTable::new())
    }

    fn bootstrap_trivial_session() -> ResidentSession<frunk::HNil, NullSink> {
        let (expr, table) = trivial_expr_and_table();
        ResidentSession::bootstrap(
            &expr,
            table,
            frunk::HNil,
            0,
            Vec::new(),
            NullSink,
            Vec::new(),
            crate::DEFAULT_NURSERY_SIZE,
            None,
        )
        .expect("a trivial Lit expression over an empty table compiles")
    }

    /// The exact regression the external review flagged
    /// (`tidepool-runtime/src/session/resident.rs:1113-1141` in the review):
    /// `spawn_scoped(...).expect(...)` used to run AFTER `take_machine()` and
    /// panic BEFORE `restore_machine()`, permanently leaving the session
    /// machineless past that unwind. This forces the spawn to fail
    /// deterministically — a stack size that vastly exceeds any real address
    /// space, so `pthread_create` rejects it outright, no actual thread-limit
    /// exhaustion needed — and asserts: no panic, a typed
    /// `ResidentError::EvalThread`, and the machine is back in the session's
    /// slot afterward, still genuinely usable.
    #[test]
    fn a_forced_eval_thread_spawn_failure_restores_the_machine_and_returns_a_typed_error() {
        let mut session = bootstrap_trivial_session();

        let result = session.on_eval_thread_with_stack(
            1_usize << 56,
            |_, _, _, _| -> Result<(), JitError> {
                unreachable!("the spawn itself must fail before body ever runs")
            },
        );

        assert!(
            matches!(result, Err(ResidentError::EvalThread(_))),
            "expected ResidentError::EvalThread, got {result:?}"
        );
        assert!(
            session.heap_stats().is_some(),
            "the machine must be restored into the session's slot after a failed spawn, \
             not left permanently machineless"
        );

        // The restored machine is genuinely usable, not just present: an
        // ordinary call through the normal (production) stack size succeeds
        // right after.
        let ok = session.on_eval_thread(|_, _, _, _| -> Result<i32, JitError> { Ok(7) });
        assert_eq!(ok.unwrap(), 7);
    }

    /// [`ResidentSession::set_scope`] on a dead scope (never minted, or
    /// already retired) must return a typed error and leave
    /// [`ResidentSession::current_scope`] exactly where it was — the exact
    /// gap the 2026-08-19 review flagged (a dead-scope assignment used to
    /// silently succeed, so a later turn compiled and bound against a scope
    /// no lookup chain could ever see again).
    #[test]
    fn set_scope_rejects_a_dead_scope_and_leaves_current_scope_unchanged() {
        let mut session = bootstrap_trivial_session();
        let live = session.mint_scope(ScopeId::ROOT).expect("ROOT is live");
        session
            .set_scope(live)
            .expect("freshly-minted scope is live");
        assert_eq!(session.current_scope(), live);

        session.retire_scope(live);
        let now_dead = live;

        let result = session.set_scope(now_dead);
        assert!(
            matches!(
                result,
                Err(ResidentError::Session(SessionError::DeadScope(s))) if s == now_dead
            ),
            "expected a typed DeadScope error, got {result:?}"
        );
        assert_eq!(
            session.current_scope(),
            live,
            "a rejected assignment must not move the session off its last live scope"
        );

        // A never-minted, always-invalid id is rejected the same way.
        let never_minted = ScopeId(999_999);
        assert!(matches!(
            session.set_scope(never_minted),
            Err(ResidentError::Session(SessionError::DeadScope(s))) if s == never_minted
        ));

        // ROOT stays byte-identical: always live, never refused.
        session
            .set_scope(ScopeId::ROOT)
            .expect("ROOT is always live");
        assert_eq!(session.current_scope(), ScopeId::ROOT);
    }

    /// [`ResidentSession::mount_handle_in`] on a dead scope must reject with
    /// a typed error WITHOUT leaking the [`RootCustody`] token — the HIGH
    /// finding from the 2026-08-19 review: a stale/forged scope id used to
    /// unconditionally transfer a persistent root into a frame no
    /// `retire_scope` could ever drain (a permanent GC root by construction).
    ///
    /// The liveness check runs before `custody` is touched, so a dead-scope
    /// rejection must consume it deliberately (`RootCustody::into_handle`)
    /// rather than dropping it unconsumed — an unconsumed drop is this
    /// type's own loud leak detector (`debug_assert!` in `Drop`), so a test
    /// process that panics here would prove the opposite of what this test
    /// asserts. No panic is therefore itself the "custody not leaked" proof.
    #[test]
    fn mount_handle_in_rejects_a_dead_scope_without_leaking_custody() {
        let mut session = bootstrap_trivial_session();
        let scope = session.mint_scope(ScopeId::ROOT).expect("ROOT is live");
        session.retire_scope(scope);

        // An arbitrary handle id: the liveness check must short-circuit
        // before this is ever resolved against the machine's handle
        // registry, so it need not be a real, live-minted handle.
        let custody = RootCustody::new(ValueHandle(0));
        let result = session.mount_handle_in(
            scope,
            "escapee",
            SessionVarId::from_extract(0),
            SessionModule::val(Generation(1)),
            ValueTier::Tier0Data,
            None,
            custody,
        );

        assert!(
            matches!(
                result,
                Err(ResidentError::Session(SessionError::DeadScope(s))) if s == scope
            ),
            "expected a typed DeadScope error, got {result:?}"
        );
        // `custody`'s Drop already ran as part of returning from
        // `mount_handle_in` (it was consumed by value); reaching this line
        // at all — rather than a debug_assert panic mid-call — is the leak
        // proof. Nothing was bound under the dead scope either.
        assert_eq!(
            session.binding_names_in(scope),
            Vec::<String>::new(),
            "a rejected mount must not have written a binding"
        );
    }
}
