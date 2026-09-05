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
//! [`JitEffectMachine::resume_continuation`] re-installs the machine's
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
//! # Fragment suspension
//!
//! Each turn is compiled into the live machine as a fragment
//! ([`JitEffectMachine::add_function`]) and driven through
//! [`JitEffectMachine::run_until_suspension`]. A suspension parks a frame as a
//! registered GC root, and the machine stays fully usable while it waits
//! (further turns, further parks, resumes of other frames). The session
//! tracks its parked holes as an insertion-ordered `(hole, ContinuationId)`
//! list; [`ResidentSession::resume`] resumes ANY member hole by identity
//! (the machine imposes no order). Each entry runs in an explicit
//! [`SessionRunContext`] that pairs its heap-resource and lexical scopes; the
//! request routing is selected independently of the Haskell effect row.
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
//! bind's tenured root riding out as a [`ValueHandle`].

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use tidepool_codegen::binding_table::{BindingEntry, BoundValue};
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{FuncId, JitEffectMachine};
use tidepool_codegen::suspension::{
    ContinuationId, ParkKind, ParkedOutcome, RealmId, ResumeInput, SuspensionRun, ValueHandle,
};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_eval::value::Value;
use tidepool_repr::{
    BindingName, CoreExpr, DataConTable, Generation, MonotonicIdIssuer, SessionModule, SessionVarId,
};

use crate::render::EvalResult;
use crate::timing;
use crate::{JitError, RuntimeError, YieldSite, YieldSiteCollision, EVAL_STACK_SIZE};

/// Immutable compiler provenance that travels with live Haskell programs.
/// Sites are globally stable, while the map makes accidental hash collisions
/// loud before a continuation can be resumed against the wrong type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramProvenance {
    sites: BTreeMap<u64, YieldSite>,
}

pub type ProgramProvenanceError = YieldSiteCollision;

impl ProgramProvenance {
    pub fn from_sites(sites: &[YieldSite]) -> Result<Self, ProgramProvenanceError> {
        let mut provenance = Self::default();
        provenance.extend(sites)?;
        Ok(provenance)
    }

    fn extend(&mut self, sites: &[YieldSite]) -> Result<(), ProgramProvenanceError> {
        for site in sites {
            if let Some(previous) = self.sites.get(&site.site) {
                if previous != site {
                    return Err(YieldSiteCollision {
                        site: site.site,
                        first: Box::new(previous.clone()),
                        second: Box::new(site.clone()),
                    });
                }
            } else {
                self.sites.insert(site.site, site.clone());
            }
        }
        Ok(())
    }

    fn merge(&mut self, other: &Self) -> Result<(), ProgramProvenanceError> {
        for site in other.sites.values() {
            self.extend(std::slice::from_ref(site))?;
        }
        Ok(())
    }

    #[must_use]
    pub fn sites(&self) -> Vec<YieldSite> {
        self.sites.values().cloned().collect()
    }
}

use tidepool_codegen::scope::ScopeId;
use tidepool_repr::PrincipalId;

use super::engine::OutputSink;
use super::persistent::{PersistentSession, ScopeRetirement};
use super::turn::{BoundBinder, ValueTier};
use super::{SessionError, SessionLib, SourceImports};

/// Runtime context applied to every entry into a resident session.
///
/// These fields describe one logical execution window: `resource_scope`
/// owns parked frames and live handles, while `lexical_scope` selects the
/// declarations and bindings visible to compilation, and `principal` names
/// the exact runtime authority used by effect handlers. Keeping them in one
/// value prevents a shared session from combining one caller's heap ownership,
/// lexical environment, and privileges with another caller's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionRunContext {
    pub resource_scope: RealmId,
    pub lexical_scope: ScopeId,
    pub principal: PrincipalId,
}

impl SessionRunContext {
    pub const ROOT: Self = Self {
        resource_scope: RealmId::ROOT,
        lexical_scope: ScopeId::ROOT,
        principal: PrincipalId::SYSTEM,
    };

    #[must_use]
    pub const fn new(
        resource_scope: RealmId,
        lexical_scope: ScopeId,
        principal: PrincipalId,
    ) -> Self {
        Self {
            resource_scope,
            lexical_scope,
            principal,
        }
    }
}

impl Default for SessionRunContext {
    fn default() -> Self {
        Self::ROOT
    }
}

/// Exclusive custody of one machine-rooted value.
///
/// The session creates custody when a finalized value leaves a parked frame.
/// Consuming operations may deliver it once, adopt it into a binding, move it
/// to another resource scope, discard it, or turn it into a repeatable
/// [`RootedValueRef`]. Raw [`ValueHandle`] access stays inside this module, so
/// external callers cannot duplicate an ownership token through a numeric ID.
/// Dropping custody queues its root for release at the next mutable entry into
/// its originating session; resource-scope or machine teardown remains the
/// final cleanup backstop if the session is never entered again.
///
#[must_use = "custody must be delivered, mounted, retained, or deliberately discarded"]
#[derive(Debug)]
pub struct RootCustody {
    handle: Option<ValueHandle>,
    cleanup: Arc<CustodyCleanup>,
    provenance: Arc<ProgramProvenance>,
}

// Custody must remain exclusive.
static_assertions::assert_not_impl_any!(RootCustody: Clone, Copy);

/// Cloneable reference to a live value whose root remains owned by a runtime
/// resource scope.
///
/// This may be delivered repeatedly, but cannot be adopted, discarded, or
/// passed to raw machine APIs. Those ownership transitions require
/// [`RootCustody`] and a [`ResidentSession`].
#[derive(Debug, Clone)]
pub struct RootedValueRef {
    handle: ValueHandle,
    provenance: Arc<ProgramProvenance>,
}

impl RootCustody {
    /// Wrap a handle minted by the resident session.
    fn new(
        handle: ValueHandle,
        cleanup: Arc<CustodyCleanup>,
        provenance: Arc<ProgramProvenance>,
    ) -> Self {
        RootCustody {
            handle: Some(handle),
            cleanup,
            provenance,
        }
    }

    #[must_use]
    pub fn provenance(&self) -> &ProgramProvenance {
        &self.provenance
    }

    fn into_transfer(mut self) -> CustodyTransfer {
        let Some(handle) = self.handle.take() else {
            unreachable!("live custody always contains its handle");
        };
        CustodyTransfer {
            handle,
            cleanup: Arc::clone(&self.cleanup),
            provenance: Arc::clone(&self.provenance),
            committed: false,
        }
    }

    /// Convert exclusive custody into a repeatable reference while leaving
    /// cleanup responsibility with the handle's current resource scope.
    #[must_use]
    pub fn into_rooted_ref(self) -> RootedValueRef {
        let transfer = self.into_transfer();
        let rooted = RootedValueRef {
            handle: transfer.handle,
            provenance: Arc::clone(&transfer.provenance),
        };
        transfer.commit();
        rooted
    }
}

impl Drop for RootCustody {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.cleanup.enqueue(handle);
        }
    }
}

#[derive(Debug, Default)]
struct CustodyCleanup {
    abandoned: Mutex<Vec<ValueHandle>>,
}

impl CustodyCleanup {
    fn enqueue(&self, handle: ValueHandle) {
        self.abandoned.lock().push(handle);
    }

    fn take_all(&self) -> Vec<ValueHandle> {
        std::mem::take(&mut *self.abandoned.lock())
    }
}

struct CustodyTransfer {
    handle: ValueHandle,
    cleanup: Arc<CustodyCleanup>,
    provenance: Arc<ProgramProvenance>,
    committed: bool,
}

impl CustodyTransfer {
    fn commit(mut self) {
        self.committed = true;
    }

    fn into_custody(mut self) -> RootCustody {
        self.committed = true;
        RootCustody::new(
            self.handle,
            Arc::clone(&self.cleanup),
            Arc::clone(&self.provenance),
        )
    }
}

impl Drop for CustodyTransfer {
    fn drop(&mut self) {
        if !self.committed {
            self.cleanup.enqueue(self.handle);
        }
    }
}

/// A parked turn's own completion obligation, carried on the token
/// [`ResidentSession::run`]/[`ResidentSession::run_bind`]/[`ResidentSession::run_rooted_entry`]
/// hand back on suspension: a [`ParkKind::Plain`] turn's hole needs nothing
/// extra to resume; a [`ParkKind::Binding`] turn's hole must materialize its
/// binder into the value plane on completion; and a [`ParkKind::Project`]
/// hole must atomically materialize every GHC-reported pattern binder. Binding
/// obligations retain the SAME binder metadata and generation carried by the
/// initiating operation.
///
/// None of the hole payloads have public constructors or fields. The session
/// creates them at suspension time, keeping each completion obligation
/// inseparable from the token consumed by [`ResidentSession::resume`].
#[derive(Clone, Debug)]
pub struct PlainHole {
    id: String,
}

/// See [`ResidentHole`]'s doc — the `Binding` variant's payload.
#[derive(Clone, Debug)]
pub struct BindingHole {
    id: String,
    binder: BoundBinder,
    generation: Generation,
}

/// See [`ResidentHole`]'s doc — a projected pattern bind retains every GHC
/// binder and its one shared value generation across suspension.
#[derive(Clone, Debug)]
pub struct ProjectedBindingHole {
    id: String,
    binders: Vec<BoundBinder>,
    generation: Generation,
}

/// The public continuation token: a sum over a parked turn's completion
/// obligation. See the hole payload docs for why no variant is externally
/// constructible.
#[derive(Clone, Debug)]
pub enum ResidentHole {
    Plain(PlainHole),
    Binding(BindingHole),
    ProjectedBinding(ProjectedBindingHole),
}

impl ResidentHole {
    /// The minted continuation id this hole was parked under — the same
    /// identity [`ResidentSession::pending_continuation`]/[`ResidentSession::parked_holes`]
    /// read, for display/logging/tree-bookkeeping purposes that don't need
    /// (and shouldn't carry) the resume obligation itself.
    pub fn cont_id(&self) -> &str {
        match self {
            ResidentHole::Plain(h) => &h.id,
            ResidentHole::Binding(h) => &h.id,
            ResidentHole::ProjectedBinding(h) => &h.id,
        }
    }

    fn mint(id: String, seed: HoleSeed) -> Self {
        match seed {
            HoleSeed::Plain => ResidentHole::Plain(PlainHole { id }),
            HoleSeed::Binding { binder, generation } => ResidentHole::Binding(BindingHole {
                id,
                binder,
                generation,
            }),
            HoleSeed::ProjectedBinding {
                binders,
                generation,
            } => ResidentHole::ProjectedBinding(ProjectedBindingHole {
                id,
                binders,
                generation,
            }),
        }
    }

    /// This hole's own seed — what [`ResidentSession::resume`] re-mints a
    /// fresh hole as, should this resume re-suspend: a Binding hole's chain
    /// of re-suspensions all carry the SAME binder/generation through to
    /// whichever one finally completes.
    fn seed(&self) -> HoleSeed {
        match self {
            ResidentHole::Plain(_) => HoleSeed::Plain,
            ResidentHole::Binding(h) => HoleSeed::Binding {
                binder: h.binder.clone(),
                generation: h.generation,
            },
            ResidentHole::ProjectedBinding(h) => HoleSeed::ProjectedBinding {
                binders: h.binders.clone(),
                generation: h.generation,
            },
        }
    }

    /// Construct a `Plain` hole from a bare continuation id, for a caller
    /// whose own bookkeeping stores just the id string (the self-iterating
    /// harness driver's `render`/`loop`/green-thread threads — every one of
    /// those goes through [`ResidentSession::run`]/[`ResidentSession::run_rooted_entry`],
    /// never [`ResidentSession::run_bind`]) rather than the [`ResidentHole`]
    /// this API otherwise hands back. NOT a backdoor around the
    /// completion-obligation guarantee: the one failure mode this type
    /// exists to prevent — a `Binding` hole silently resumed as `Plain`,
    /// dropping its value-plane materialization — is still impossible
    /// through this constructor, because it can only ever produce `Plain`.
    /// There is no way to fabricate a `Binding` hole from a bare string; a
    /// real suspension through `run_bind` is the only source of one.
    pub fn plain(cont_id: impl Into<String>) -> Self {
        ResidentHole::Plain(PlainHole { id: cont_id.into() })
    }
}

/// What kind of hole [`ResidentSession::classify_parked`] mints on a fresh
/// suspension — [`ResidentHole`] minus the id, which is minted alongside it.
#[derive(Clone)]
enum HoleSeed {
    Plain,
    Binding {
        binder: BoundBinder,
        generation: Generation,
    },
    ProjectedBinding {
        binders: Vec<BoundBinder>,
        generation: Generation,
    },
}

/// The classified result of driving a resident turn to its first yield.
///
/// The suspend-and-completion shape mirrors [`super::TurnOutcome`], but a
/// resident turn is driven by direct `run_*` calls (not the oneshot engine), so
/// this is a distinct, smaller enum: no `Paused`/`TimedOut` (timeout-yield is
/// permanently excluded from the stowable resident path, by design),
/// and completion distinguishes value-producing turns from projected binds
/// whose products were installed directly into the lexical scope.
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
    /// A projected pattern bind completed and its named values were installed
    /// in the resident lexical scope. Unlike an expression completion, this
    /// operation has no result value of its own.
    BindingsCommitted { output: Vec<String> },
    /// The turn suspended at an `Ask`. The machine holds the continuation
    /// internally (stowed as data); call [`ResidentSession::resume`] with the
    /// answer. `request` is the bridged `Ask` request; `hole` is the minted
    /// continuation id.
    Suspended {
        output: Vec<String>,
        hole: ResidentHole,
        request: Value,
    },
}

/// Why a resident-session operation was refused or failed.
#[derive(thiserror::Error, Debug)]
pub enum ResidentError {
    /// A rooted value minted by another resident session was presented to
    /// this machine. Handle ids are session-local and must never be resolved
    /// by numeric coincidence.
    #[error("root custody belongs to a different resident session")]
    ForeignCustody,
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
    #[error(transparent)]
    ProgramProvenance(#[from] ProgramProvenanceError),
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
    /// The console-output buffer turns write into.
    captured: O,
    /// GHC include search paths for fragment compiles (unused today — fragments
    /// are pre-compiled Core — but carried as the registry-entry seam).
    #[allow(dead_code)]
    include: Vec<PathBuf>,
    /// Monotonic continuation-id counter (prefix `scont` for the resident
    /// surface).
    cont_id_issuer: MonotonicIdIssuer,
    /// The parked holes, insertion-ordered: `(hole string, machine
    /// ContinuationId)` per live parked frame. The machine's continuation
    /// registry is the ground truth; these are the string identities callers
    /// resume/abort against (atomic validate-before-consume). Top = last.
    parked: Vec<(String, ContinuationId)>,
    parked_provenance: HashMap<ContinuationId, Arc<ProgramProvenance>>,
    binding_provenance: HashMap<u64, Arc<ProgramProvenance>>,
    /// The resource and lexical scopes for the next session entry. Callers
    /// sharing a machine replace this atomically at checkout boundaries.
    run_context: SessionRunContext,
    /// Deferred releases produced when affine custody is dropped away from a
    /// machine checkout. The next mutable session entry settles them.
    custody_cleanup: Arc<CustodyCleanup>,
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
    // handlers, captured, include, nursery) — bundling
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
        captured: O,
        include: Vec<PathBuf>,
        nursery_size: usize,
        lib: Option<SessionLib>,
    ) -> Result<Self, JitError> {
        let mut core = PersistentSession::new(lib, nursery_size);
        core.bootstrap_if_needed(expr, &table)?;
        core.seed_session_table(table);
        Ok(ResidentSession {
            core,
            handlers,
            captured,
            include,
            cont_id_issuer: MonotonicIdIssuer::new("scont"),
            parked: Vec::new(),
            parked_provenance: HashMap::new(),
            binding_provenance: HashMap::new(),
            run_context: SessionRunContext::ROOT,
            custody_cleanup: Arc::new(CustodyCleanup::default()),
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
        captured: O,
        include: Vec<PathBuf>,
        nursery_size: usize,
        lib: Option<SessionLib>,
    ) -> Self {
        let core = PersistentSession::new(lib, nursery_size);
        ResidentSession {
            core,
            handlers,
            captured,
            include,
            cont_id_issuer: MonotonicIdIssuer::new("scont"),
            parked: Vec::new(),
            parked_provenance: HashMap::new(),
            binding_provenance: HashMap::new(),
            run_context: SessionRunContext::ROOT,
            custody_cleanup: Arc::new(CustodyCleanup::default()),
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

    /// Commit declarations against frontend-owned imports without recording
    /// those trusted imports as user-authored workbench state.
    pub fn define_scoped_with_imports_in(
        &mut self,
        scope: ScopeId,
        decls: &[&str],
        imports: &SourceImports,
    ) -> Result<tidepool_repr::Generation, SessionError> {
        self.core
            .define_scoped_with_imports_in(scope, decls, imports)
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
    /// generation into `Val.G<g>`, and [`Self::run_bind`]/[`Self::resume`]
    /// (via a [`ResidentHole::Binding`]) materialize at the same `g`.
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

    /// Immutable compile environment for `scope`, suitable for carrying out of
    /// a registry peek before a blocking GHC invocation.
    pub fn compile_view_in(&self, scope: ScopeId) -> Option<super::SessionCompileView> {
        self.core.compile_view_in(scope)
    }

    /// Capture an exact, selective declaration surface from `scope` for a
    /// fresh actor's model-visible environment.
    pub fn exact_exports_in(
        &self,
        scope: ScopeId,
        heads: &[&str],
    ) -> Result<super::ExactExportSurface, super::ExactExportError> {
        self.core.exact_exports_in(scope, heads)
    }

    /// Exact declaration-head incarnations visible from `scope`.
    ///
    /// Actor sealing pairs this with compiler-produced nominal heads so a
    /// same-spelled declaration introduced after a live program was compiled
    /// cannot replace the program's original type.
    #[must_use]
    pub fn current_decl_heads_in(&self, scope: ScopeId) -> Vec<(String, u64)> {
        self.core.lib().current_decl_heads_in(scope)
    }

    /// The most recently parked hole (top of the stack), if any.
    ///
    /// Use [`Self::parked_holes`] when the caller needs the complete registry.
    pub fn pending_continuation(&self) -> Option<&str> {
        self.parked.last().map(|(h, _)| h.as_str())
    }

    /// Every parked hole, insertion-ordered (oldest first).
    pub fn parked_holes(&self) -> Vec<&str> {
        self.parked.iter().map(|(h, _)| h.as_str()).collect()
    }

    /// Runtime resource scope owning one parked continuation. Lifecycle
    /// interpreters use this to distinguish installed-program suspensions from
    /// disposable workbench fragments without trusting request payload data.
    #[must_use]
    pub fn parked_realm(&self, hole: &ResidentHole) -> Option<RealmId> {
        let &(_, id) = self
            .parked
            .iter()
            .find(|(name, _)| name == hole.cont_id())?;
        self.core.machine()?.parked_realm(id)
    }

    #[must_use]
    pub fn parked_program_provenance(&self, hole: &ResidentHole) -> Option<Arc<ProgramProvenance>> {
        let (_, id) = self
            .parked
            .iter()
            .find(|(name, _)| name == hole.cont_id())?;
        self.parked_provenance.get(id).cloned()
    }

    /// Whether the session has no parked frames (ready and quiescent).
    pub fn is_idle(&self) -> bool {
        self.parked.is_empty()
    }

    /// Select the resource ownership and lexical environment for subsequent
    /// work on this checkout.
    ///
    /// Validation happens before assignment, so a dead lexical scope leaves
    /// both halves of the previous context unchanged.
    pub fn set_run_context(&mut self, context: SessionRunContext) -> Result<(), ResidentError> {
        if !self.core.scope_tree().is_live(context.lexical_scope) {
            return Err(SessionError::DeadScope(context.lexical_scope).into());
        }
        self.run_context = context;
        Ok(())
    }

    /// Select request routing and live-value crossing without changing the
    /// current execution principal or resource scopes.
    pub fn set_effect_execution(
        &mut self,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
    ) {
        self.core.set_effect_execution(effect_policy, live_payload);
    }

    /// Atomically select one actor's authority/scopes and request policy.
    /// Validation precedes both assignments, so a dead lexical scope
    /// cannot leave half of another actor's execution contract installed.
    pub fn set_actor_execution(
        &mut self,
        context: SessionRunContext,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
    ) -> Result<(), ResidentError> {
        if !self.core.scope_tree().is_live(context.lexical_scope) {
            return Err(SessionError::DeadScope(context.lexical_scope).into());
        }
        self.run_context = context;
        self.set_effect_execution(effect_policy, live_payload);
        Ok(())
    }

    /// The context currently selected for resident-session entries.
    #[must_use]
    pub fn run_context(&self) -> SessionRunContext {
        self.run_context
    }

    /// Request policy currently selected for resident entries.
    #[must_use]
    pub fn effect_policy(&self) -> EffectRunPolicy {
        self.core.effect_policy()
    }

    /// Live-value crossing policy currently installed with the effect stack.
    #[must_use]
    pub fn live_payload_policy(&self) -> LivePayloadPolicy {
        self.core.live_payload_policy()
    }

    /// Scope exit for `realm`: close the realm
    /// on the machine (frames dropped, roots deregistered, handles released)
    /// and RECONCILE this session's parked-hole list against the machine's
    /// surviving frame ids — the machine is the ground truth, so holes whose
    /// frames the close dropped disappear here too, and sibling realms'
    /// holes are untouched. Returns `(frames_dropped, handles_released)`;
    /// `(0, 0)` when the machine is not yet booted or the realm owns
    /// nothing (idempotent).
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        self.settle_dropped_custody();
        let Some(machine) = self.core.machine_mut() else {
            return (0, 0);
        };
        let counts = machine.close_realm(realm);
        let survivors = machine.parked_ids();
        self.parked.retain(|(_, id)| survivors.contains(id));
        self.parked_provenance
            .retain(|id, _| survivors.contains(id));
        counts
    }

    /// Mint a [`ValueHandle`] over the declared live payload of the frame
    /// parked on `hole` (the payload never bridges to a
    /// data `Value`; the `Send` handle is how it is passed around and
    /// eventually DELIVERED into a sibling hole via [`Self::resume_handle`]).
    /// The frame stays parked; the handle is owned by the frame's realm.
    /// `None` when `hole` is not parked or its frame holds no untaken live
    /// payload.
    pub fn live_payload_handle(&mut self, hole: &str) -> Option<RootCustody> {
        self.settle_dropped_custody();
        let &(_, id) = self.parked.iter().find(|(h, _)| h == hole)?;
        let provenance = self.parked_provenance.get(&id).cloned().unwrap_or_default();
        self.core
            .machine_mut()?
            .handle_from_live_payload(id)
            .map(|handle| RootCustody::new(handle, Arc::clone(&self.custody_cleanup), provenance))
    }

    /// [`Self::live_payload_handle`]'s sibling for a result that must outlive
    /// the frame's OWN realm: mint the handle owned by `realm` instead (a
    /// green thread's `AsyncDoneWith` payload, owned by the SESSION's realm
    /// so a waiter's handle survives the thread's own realm later closing —
    /// see [`ResidentSession::run_rooted_entry`]). Same
    /// frame-stays-parked semantics; `None` under the same conditions.
    /// Returns a [`RootCustody`] token, exactly as [`Self::live_payload_handle`]
    /// does: minting under a different realm changes WHO owns the root, never
    /// whether the handle needs consuming exactly once.
    pub fn live_payload_handle_owned_by(
        &mut self,
        hole: &str,
        realm: RealmId,
    ) -> Option<RootCustody> {
        self.settle_dropped_custody();
        let &(_, id) = self.parked.iter().find(|(h, _)| h == hole)?;
        let provenance = self.parked_provenance.get(&id).cloned().unwrap_or_default();
        let machine = self.core.machine_mut()?;
        let slot = machine.take_parked_live_payload_root(id)?;
        let handle = machine.mint_handle_from_root(slot, realm);
        tracing::debug!(
            hole,
            frame = ?id,
            owner = ?realm,
            ?handle,
            "claimed parked live payload"
        );
        Some(RootCustody::new(
            handle,
            Arc::clone(&self.custody_cleanup),
            provenance,
        ))
    }

    /// Transfer a rooted value to another runtime resource scope.
    ///
    /// This is the ownership operation used when a live value outlives the
    /// scope that produced it, such as a queued message or detached child.
    /// The value and handle identity are unchanged; only the scope responsible
    /// for eventual cleanup changes.
    pub fn rehome_custody(
        &mut self,
        custody: RootCustody,
        owner: RealmId,
    ) -> Result<RootCustody, ResidentError> {
        self.settle_dropped_custody();
        let transfer = custody.into_transfer();
        let handle = transfer.handle;
        let moved = self
            .core
            .machine_mut()
            .is_some_and(|machine| machine.rehome_handle(handle, owner));
        if !moved {
            return Err(ResidentError::Run(RuntimeError::Jit(JitError::Effect(
                EffectError::Handler(format!(
                    "cannot transfer {handle:?}: handle is not live on this machine"
                )),
            ))));
        }
        Ok(transfer.into_custody())
    }

    /// Abandon a rooted value deliberately, releasing its root immediately.
    pub fn discard_custody(&mut self, custody: RootCustody) -> bool {
        self.settle_dropped_custody();
        let transfer = custody.into_transfer();
        let discarded = self
            .core
            .machine_mut()
            .is_some_and(|machine| machine.discard_handle(transfer.handle));
        if discarded {
            transfer.commit();
        }
        discarded
    }

    /// Resume the turn represented by `hole` by DELIVERING a machine-side
    /// rooted value — the handle's payload feeds the continuation verbatim,
    /// no materialization, closures included. The authored loop receives
    /// closure-valued state transitions this way. Same
    /// validate-before-consume and ground-truth reconciliation as
    /// [`Self::resume`]. The typed hole carries the same binding-completion
    /// obligation as the ordinary value path; handle delivery cannot silently
    /// turn a suspended bind into a plain fragment.
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
        hole: ResidentHole,
        custody: RootCustody,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.settle_dropped_custody();
        let seed = hole.seed();
        let cont_id = match hole {
            ResidentHole::Plain(hole) => hole.id,
            ResidentHole::Binding(hole) => hole.id,
            ResidentHole::ProjectedBinding(hole) => hole.id,
        };
        let transfer = custody.into_transfer();
        tracing::debug!(
            continuation = %cont_id,
            handle = ?transfer.handle,
            obligation = match &seed {
                HoleSeed::Plain => "plain",
                HoleSeed::Binding { .. } => "binding",
                HoleSeed::ProjectedBinding { .. } => "projected-binding",
            },
            actor_scope = ?self.run_context.lexical_scope,
            actor_realm = ?self.run_context.resource_scope,
            "resuming resident continuation with rooted value"
        );
        let provenance = Arc::clone(&transfer.provenance);
        let result = self.reenter(
            &cont_id,
            ResumeInput::Handle(transfer.handle),
            seed,
            Some(&provenance),
        );
        if result.is_ok() {
            transfer.commit();
        }
        result
    }

    /// Scoped read-only query: the binding `name` resolves to as seen FROM
    /// `scope` — its own frame first, then each ancestor up to its lexical
    /// root, so a child reads a parent's mounts and a local mount shadows an
    /// inherited one. Independent of the mount seam ([`Self::mount_handle_in`]
    /// resolves its own target internally — see that method's doc); this is
    /// a plain existence/identity probe for callers that need to know what
    /// `name` is bound to without mounting anything.
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

    /// Redirect an ALREADY-MINTED value-plane binding for `name` to resolve
    /// through `custody`'s tenured payload instead of whatever it was bound
    /// to before — the mount seam: "a handle installed under
    /// a name in a window's declaration scope", the closure-tenure-then-handle
    /// delivery path points the other direction.
    ///
    /// **Atomic:** `name`'s current identity (`SessionVarId`/module/tier/type
    /// display) is resolved INTERNALLY, at mount time, from the live
    /// `(scope, name)` binding — never carried in by the caller. The idiom
    /// producing that identity is unchanged: mint a real `Val.G<g>`
    /// iface/`SessionVarId` cheaply by running an ordinary throwaway bind of
    /// the mounted type under `name` in `scope` (its own tenured value is
    /// thrown away), THEN call this to swap in the real value's root. GHC
    /// never needs to see the real value — only its type, which the
    /// throwaway bind already established correctly.
    ///
    /// The handle's root is adopted into the value plane under the existing
    /// `SessionVarId` and module identity. No new interface is generated; only
    /// the heap object behind the binding changes.
    pub fn mount_handle(&mut self, name: &str, custody: RootCustody) -> Result<(), ResidentError> {
        self.mount_handle_in(ScopeId::ROOT, name, custody)
    }

    /// Scoped [`Self::mount_handle`]. The target scope and binding are
    /// validated before the handle root is adopted. On either validation
    /// failure, custody is discarded immediately; no root waits for eventual
    /// resource-scope cleanup and no unowned root can escape the registry.
    pub fn mount_handle_in(
        &mut self,
        scope: ScopeId,
        name: &str,
        custody: RootCustody,
    ) -> Result<(), ResidentError> {
        self.settle_dropped_custody();
        if !self.core.scope_tree().is_live(scope) {
            self.discard_custody(custody);
            return Err(SessionError::DeadScope(scope).into());
        }
        let resolved = self.core.resolve_in(scope, name).map(|entry| {
            let tier = match entry.value {
                BoundValue::Tier0Forced(_) => ValueTier::Tier0Data,
                BoundValue::Tier1Closure(_) => ValueTier::Tier1Closure,
            };
            (entry.id, entry.module, tier, entry.type_display.clone())
        });
        let (id, module, tier, type_display) = match resolved {
            Some(v) => v,
            None => {
                self.discard_custody(custody);
                return Err(SessionError::UnknownBinding {
                    scope,
                    name: name.to_string(),
                }
                .into());
            }
        };
        let transfer = custody.into_transfer();
        let provenance = Arc::clone(&transfer.provenance);
        let handle = transfer.handle;
        let slot = self
            .core
            .machine_mut()
            .and_then(|machine| machine.take_handle_root(handle))
            .ok_or_else(|| {
                ResidentError::Run(RuntimeError::Jit(JitError::Effect(EffectError::Handler(
                    "mount: handle is unknown to the machine (already released or never minted)"
                        .into(),
                ))))
            })?;
        transfer.commit();
        let value = match tier {
            ValueTier::Tier0Data => BoundValue::Tier0Forced(slot),
            ValueTier::Tier1Closure => BoundValue::Tier1Closure(slot),
        };
        self.core.bind_replacing_decl_in(
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
        self.binding_provenance.insert(id.raw(), provenance);
        Ok(())
    }

    /// Install a rooted live value under a binder GHC has already compiled,
    /// without evaluating a throwaway placeholder of that type.
    ///
    /// `run_turn` writes the binder's thin `Val.G<gen>` interface and returns
    /// its exact identity. This operation joins that type-plane identity to a
    /// same-typed in-heap value supplied under affine custody. It is the mount
    /// path for actor inputs and messages: the authoritative value already
    /// exists, so running `undefined`, a guessed inhabitant, or a second copy
    /// merely to create the binding would be both wasteful and semantically
    /// wrong.
    ///
    /// Validation and table merge happen before custody is consumed. On
    /// success ownership transfers from the handle registry to the scoped
    /// value plane exactly once.
    pub fn mount_compiled_binding_in(
        &mut self,
        scope: ScopeId,
        binder: &BoundBinder,
        gen: Generation,
        table: &DataConTable,
        custody: RootCustody,
    ) -> Result<(), ResidentError> {
        self.settle_dropped_custody();
        if !self.core.scope_tree().is_live(scope) {
            self.discard_custody(custody);
            return Err(SessionError::DeadScope(scope).into());
        }
        let expected_module = SessionModule::val(gen).module_name();
        if binder.module != expected_module {
            self.discard_custody(custody);
            return Err(ResidentError::Run(RuntimeError::Jit(JitError::Effect(
                EffectError::Handler(format!(
                    "compiled binder `{}` belongs to {}, expected {expected_module}",
                    binder.name, binder.module
                )),
            ))));
        }
        self.core
            .merge_table(table)
            .map_err(ResidentError::TableCollision)?;
        let transfer = custody.into_transfer();
        let provenance = Arc::clone(&transfer.provenance);
        let handle = transfer.handle;
        let handle_is_live = self
            .core
            .machine()
            .is_some_and(|machine| machine.handle_slot(handle).is_some());
        if !handle_is_live {
            return Err(ResidentError::Run(RuntimeError::Jit(JitError::Effect(
                EffectError::Handler(
                    "compiled binding mount received an unknown or already-consumed handle".into(),
                ),
            ))));
        }
        self.core.retract_in(scope, &binder.name)?;
        let Some(slot) = self
            .core
            .machine_mut()
            .and_then(|machine| machine.take_handle_root(handle))
        else {
            return Err(ResidentError::Run(RuntimeError::Jit(JitError::Effect(
                EffectError::Handler(
                    "compiled binding mount lost a handle during exclusive session access".into(),
                ),
            ))));
        };
        transfer.commit();
        let value = match binder.tier {
            ValueTier::Tier0Data => BoundValue::Tier0Forced(slot),
            ValueTier::Tier1Closure => BoundValue::Tier1Closure(slot),
        };
        self.core.bind_in(
            scope,
            BindingEntry {
                name: BindingName(binder.name.clone()),
                id: SessionVarId::from_extract(binder.var_id),
                module: SessionModule::val(gen),
                value,
                type_display: Some(binder.type_display.clone()),
                defining_expr: None,
                scope,
            },
        )?;
        self.core.set_val_gen(gen);
        self.binding_provenance.insert(binder.var_id, provenance);
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
    /// time (see [`Self::live_payload_handle_owned_by`]) and may then be read
    /// more than once: `poll` then `wait`, or two waiters joined on one
    /// thread. Each of those is another borrow of one root, not a second
    /// transfer of one custody.
    ///
    /// Use [`Self::resume_handle`] wherever the delivery IS the transfer (the
    /// finalize seam). Reach for this only when an owner already exists and
    /// outlives every delivery.
    pub fn resume_handle_borrowed(
        &mut self,
        hole: ResidentHole,
        handle: RootedValueRef,
    ) -> Result<ResidentOutcome, ResidentError> {
        let seed = hole.seed();
        let cont_id = match hole {
            ResidentHole::Plain(hole) => hole.id,
            ResidentHole::Binding(hole) => hole.id,
            ResidentHole::ProjectedBinding(hole) => hole.id,
        };
        self.reenter(
            &cont_id,
            ResumeInput::Handle(handle.handle),
            seed,
            Some(&handle.provenance),
        )
    }

    /// Borrow a retained value as the final field of a typed constructor.
    /// The caller keeps custody alive through resumption; the resulting heap
    /// value has ordinary Haskell reachability independent of that root.
    pub fn resume_framed_custody(
        &mut self,
        hole: ResidentHole,
        custody: &RootCustody,
        constructor: tidepool_repr::DataConId,
        prefix: Vec<Value>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let Some(handle) = custody.handle else {
            unreachable!("live custody always contains its handle");
        };
        let seed = hole.seed();
        let cont_id = match hole {
            ResidentHole::Plain(hole) => hole.id,
            ResidentHole::Binding(hole) => hole.id,
            ResidentHole::ProjectedBinding(hole) => hole.id,
        };
        self.reenter(
            &cont_id,
            ResumeInput::FramedHandle {
                handle,
                constructor,
                prefix,
            },
            seed,
            Some(&custody.provenance),
        )
    }

    /// Whether the resident machine has been bootstrapped yet. `false` from
    /// [`Self::unbootstrapped`] until the session's first real turn brings the
    /// machine up (`run`/`run_bind`/`run_child`/`run_child_pure`); always
    /// `true` from [`Self::bootstrap`].
    pub fn is_bootstrapped(&self) -> bool {
        self.core.is_bootstrapped()
    }

    #[must_use]
    pub fn data_con_table(&self) -> &DataConTable {
        self.core.session_table()
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
    /// ([`Self::live_payload_handle`]) counts here until [`Self::mount_handle`]
    /// (or an ordinary bind completion / realm close) releases it.
    pub fn value_handle_count(&mut self) -> usize {
        self.settle_dropped_custody();
        self.core.machine().map_or(0, |m| m.value_handle_count())
    }

    pub fn heap_stats(&self) -> Option<tidepool_codegen::jit_machine::HeapStats> {
        self.core.machine().map(|m| m.heap_stats())
    }

    /// Test/debug-only passthrough to
    /// [`JitEffectMachine::force_gc_for_test`] — forces a real minor
    /// collection against the session's retained heap without running any
    /// compiled code, for diagnosing whether a collection landing between a
    /// suspend-time tenure and a later resume corrupts a parked frame's own
    /// reference into what tenuring evacuated. No-op (does nothing) before
    /// the machine bootstraps.
    #[doc(hidden)]
    pub fn force_gc_for_test(&mut self) {
        if let Some(m) = self.core.machine_mut() {
            m.force_gc_for_test();
        }
    }

    /// The CURRENT value-plane binding names (newest gen per name) — what a
    /// machine rotation would lose (enumerated, legible loss, never silent).
    pub fn binding_names(&self) -> Vec<String> {
        self.core
            .bindings()
            .iter_current()
            .map(|(name, _)| name.0.clone())
            .collect()
    }

    // -- scopes --------------------------------------------------------------

    /// Mint a fresh child scope of `parent` ([`ScopeId::ROOT`] for a top-level
    /// invocation scope). `None` if `parent` is not live.
    pub fn mint_scope(&mut self, parent: ScopeId) -> Option<ScopeId> {
        self.core.mint_scope(parent)
    }

    /// Immutable value-binding snapshot captured when `scope` was minted.
    #[must_use]
    pub fn binding_tip_id(
        &self,
        scope: ScopeId,
    ) -> Option<tidepool_codegen::binding_table::BindingTipId> {
        self.core.binding_tip_id(scope)
    }

    /// Mint a fresh actor lexical root with no ambient declaration or value
    /// ancestry. Program visibility must be supplied through exact imports.
    pub fn mint_isolated_scope(&mut self) -> ScopeId {
        self.core.mint_isolated_scope()
    }

    /// The value-plane names visible at `scope`: its own mutable frame over
    /// the immutable inherited tip captured when the scope was minted.
    /// `binding_names_in(ScopeId::ROOT)` is [`Self::binding_names`]'s set.
    pub fn binding_names_in(&self, scope: ScopeId) -> Vec<String> {
        self.core
            .bindings()
            .iter_current_in(self.core.scope_tree(), scope)
            .into_iter()
            .map(|(name, _)| name.0.clone())
            .collect()
    }

    /// Term-level names visible to a GHCi-style `:bindings` query.
    ///
    /// The declaration environment and materialized binding store are one
    /// lexical view. Materialized names win on collision, matching ordinary
    /// turn compilation, and the returned order is deterministic. This query
    /// never forces a live value.
    pub fn workbench_bindings_in(&self, scope: ScopeId) -> Vec<super::WorkbenchBinding> {
        let mut bindings = std::collections::BTreeMap::new();
        for (item, _) in self.core.lib().current_declarations_in(scope) {
            if let super::ExportItem::Value { name } = &item {
                bindings.insert(
                    name.clone(),
                    super::WorkbenchBinding::declaration(name.clone(), item.render_entry()),
                );
            }
        }
        for name in self.binding_names_in(scope) {
            let type_display = self
                .current_binding_in(scope, &name)
                .and_then(|(_, _, _, type_display)| type_display);
            bindings.insert(
                name.clone(),
                super::WorkbenchBinding::materialized(name, type_display),
            );
        }
        bindings.into_values().collect()
    }

    /// Source-only declaration recovery facts for this machine incarnation.
    /// Live values and handles are intentionally absent because they cannot
    /// survive machine replacement.
    #[must_use]
    pub fn declaration_recovery_report(&self) -> Option<&super::DeclarationRecoveryReport> {
        self.core.lib().declaration_recovery_report()
    }

    /// A manifest publication failure that happened after a successful
    /// declaration commit, if durability has not recovered since.
    #[must_use]
    pub fn recovery_manifest_warning(&self) -> Option<&str> {
        self.core.lib().recovery_manifest_warning()
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
    pub fn persistent_roots_count(&mut self) -> usize {
        self.settle_dropped_custody();
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

    fn provenance_for(
        &self,
        expr: &CoreExpr,
        sites: &[YieldSite],
    ) -> Result<Arc<ProgramProvenance>, ResidentError> {
        let mut provenance = ProgramProvenance::from_sites(sites)?;
        for var in tidepool_repr::free_vars::free_vars(expr) {
            if let Some(parent) = self.binding_provenance.get(&var.0) {
                provenance.merge(parent)?;
            }
        }
        Ok(Arc::new(provenance))
    }

    fn next_cont_id(&self) -> String {
        self.cont_id_issuer.next_id()
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
        self.run_with_sites(name_hint, expr, table, &[])
    }

    pub fn run_with_sites(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        table: &DataConTable,
        sites: &[YieldSite],
    ) -> Result<ResidentOutcome, ResidentError> {
        let provenance = self.provenance_for(expr, sites)?;
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

        let effect_policy = self.core.effect_policy();
        let live_payload = self.core.live_payload_policy();
        let realm = self.run_context.resource_scope;
        let principal = self.run_context.principal;
        let run_exec_started = std::time::Instant::now();
        let outcome = self.on_eval_thread(move |machine, table, handlers, captured| {
            let run =
                SuspensionRun::fragment(func_id, table, effect_policy, realm, ParkKind::Plain)
                    .with_live_payload(live_payload)
                    .with_principal(principal);
            machine
                .run_until_suspension(run, handlers, captured)
                .map(|o| project_parked(machine, o, realm))
        })?;
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_RUN_EXEC,
            run_exec_started.elapsed(),
            0,
        );
        Ok(self.classify_parked(outcome, None, HoleSeed::Plain, provenance))
    }

    /// Run a value-plane BIND turn (`x <- e`): seed the env from prior bindings,
    /// add the fragment, and drive it through the suspendable BIND path
    /// (tenure-on-completion). On completion, materialize `binder` into the value
    /// plane at `gen` (the SAME generation the extract stamped into
    /// `binder.module` — mint it once at compile, thread it here). A fork bind
    /// SUSPENDS here (no value yet); the returned [`ResidentHole::Binding`]
    /// carries `binder`/`gen` forward, so the eventual [`Self::resume`] on
    /// that hole materializes it without the caller re-supplying either.
    pub fn run_bind(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        table: &DataConTable,
        binder: &BoundBinder,
        gen: Generation,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.run_bind_with_sites(name_hint, expr, table, binder, gen, &[])
    }

    pub fn run_bind_with_sites(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        table: &DataConTable,
        binder: &BoundBinder,
        gen: Generation,
        sites: &[YieldSite],
    ) -> Result<ResidentOutcome, ResidentError> {
        // Claim the compiled value-module identity before this bind can park.
        // Another actor may compile against the same resident session while
        // this one awaits an effect; completion-time advancement would let it
        // overwrite this bind's `Val.G<g>` interface.
        self.core.set_val_gen(gen);
        let provenance = self.provenance_for(expr, sites)?;
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

        let effect_policy = self.core.effect_policy();
        let live_payload = self.core.live_payload_policy();
        // Tier0 data is deep-forced to NF before tenuring; a Tier1 closure is
        // tenured as-is.
        let forced = matches!(binder.tier, ValueTier::Tier0Data);
        let realm = self.run_context.resource_scope;
        let principal = self.run_context.principal;
        let run_exec_started = std::time::Instant::now();
        let outcome = self.on_eval_thread(move |machine, table, handlers, captured| {
            let run = SuspensionRun::fragment(
                func_id,
                table,
                effect_policy,
                realm,
                ParkKind::Binding { forced },
            )
            .with_live_payload(live_payload)
            .with_principal(principal);
            machine
                .run_until_suspension(run, handlers, captured)
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
        // root rode out as a handle). A suspension defers to the eventual
        // `resume` on the `ResidentHole::Binding` this mints below, which
        // carries `binder`/`gen` forward itself.
        let bound = match &outcome {
            ParkedRun::CompletedValue { bound, .. } => *bound,
            ParkedRun::CompletedProject { .. } => None,
            ParkedRun::Suspended { .. } => None,
        };
        let completed = !matches!(outcome, ParkedRun::Suspended { .. });
        let seed = HoleSeed::Binding {
            binder: binder.clone(),
            generation: gen,
        };
        let resident_outcome = self.classify_parked(outcome, None, seed, Arc::clone(&provenance));
        if completed {
            self.materialize_binder(binder, gen, bound)?;
            self.binding_provenance.insert(binder.var_id, provenance);
        }
        Ok(resident_outcome)
    }

    /// Run one GHC-classified pattern bind and materialize every projected
    /// component atomically into the current lexical scope. The JIT owns tuple
    /// projection; Rust receives only GHC's binder metadata and never parses
    /// the authored pattern.
    pub fn run_projected_bind_with_sites(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        table: &DataConTable,
        binders: &[BoundBinder],
        gen: Generation,
        sites: &[YieldSite],
    ) -> Result<ResidentOutcome, ResidentError> {
        let n_fields = NonZeroUsize::new(binders.len()).ok_or_else(|| {
            ResidentError::Run(RuntimeError::Jit(JitError::Effect(EffectError::Handler(
                "a projected resident bind requires at least one GHC binder".into(),
            ))))
        })?;
        self.core.set_val_gen(gen);
        let provenance = self.provenance_for(expr, sites)?;
        self.core
            .merge_table(table)
            .map_err(ResidentError::TableCollision)?;
        self.core
            .bootstrap_if_needed(expr, table)
            .map_err(ResidentError::Bootstrap)?;
        let env = self.seed_external_env_for(expr);
        let function = self
            .core
            .add_fragment_session(name_hint, expr, &env)
            .map_err(ResidentError::AddFunction)?;
        let effect_policy = self.core.effect_policy();
        let live_payload = self.core.live_payload_policy();
        let realm = self.run_context.resource_scope;
        let principal = self.run_context.principal;
        let outcome = self.on_eval_thread(move |machine, table, handlers, captured| {
            let run = SuspensionRun::fragment(
                function,
                table,
                effect_policy,
                realm,
                ParkKind::Project { n_fields },
            )
            .with_live_payload(live_payload)
            .with_principal(principal);
            machine
                .run_until_suspension(run, handlers, captured)
                .map(|outcome| project_parked(machine, outcome, realm))
        })?;
        let projected = match &outcome {
            ParkedRun::CompletedProject { projected } => projected.clone(),
            ParkedRun::CompletedValue { .. } => Vec::new(),
            ParkedRun::Suspended { .. } => Vec::new(),
        };
        let completed = !matches!(outcome, ParkedRun::Suspended { .. });
        let seed = HoleSeed::ProjectedBinding {
            binders: binders.to_vec(),
            generation: gen,
        };
        let resident_outcome = self.classify_parked(outcome, None, seed, Arc::clone(&provenance));
        if completed {
            self.materialize_binders(binders, gen, projected, provenance)?;
        }
        Ok(resident_outcome)
    }

    /// Run a nested child turn against this suspended session: add
    /// `expr` as a fragment referencing the suspended parent's session bindings
    /// (via `external_env`, zero-copy against the same retained heap), then drive
    /// it through an ordinary fragment run while the parent's
    /// stowed continuation is GC-rooted. The session STAYS suspended on the same
    /// hole afterward — the child does not consume the parent's continuation.
    ///
    /// Requires the session to be suspended (a child needs a suspended parent);
    /// an idle session is rejected with [`ResidentError::NotSuspended`]. A child
    /// that itself suspends is rejected ([`ResidentError::ChildSuspended`])
    /// because this value-returning API has no continuation handle to return.
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
        // job. The shared issuer keeps it distinct from every caller scope.
        let child_realm = RealmId::fresh();
        let principal = self.run_context.principal;
        let effect_policy = self.core.effect_policy();
        let live_payload = self.core.live_payload_policy();
        let outcome = self.on_eval_thread(move |machine, table, handlers, captured| {
            let run = SuspensionRun::fragment(
                func_id,
                table,
                effect_policy,
                child_realm,
                ParkKind::Plain,
            )
            .with_live_payload(live_payload)
            .with_principal(principal);
            machine
                .run_until_suspension(run, handlers, captured)
                .map(|o| project_parked(machine, o, child_realm))
        })?;
        match outcome {
            ParkedRun::CompletedValue { value, .. } => {
                let _ = self.captured.drain();
                Ok(EvalResult::new(
                    value,
                    self.core.session_table().clone(),
                    Vec::new(),
                ))
            }
            ParkedRun::CompletedProject { .. } => {
                unreachable!("a value-returning child run cannot complete as a projection")
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

    /// Pure sibling of [`Self::run_child`]. Use it for fragments that produce a
    /// boxed value directly rather than an `Eff` `Val`/`E` tree.
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
    /// ([`JitEffectMachine::take_parked_live_payload_root`]); this seeds that slot into a
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
            .and_then(|m| m.take_parked_live_payload_root(frame_id))
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

    /// Apply a handle-rooted entry closure to an integer and run it as a new
    /// suspension-capable top-level computation under `realm`.
    ///
    /// Unlike [`Self::run_child`] and [`Self::run_child_pure`], this operation
    /// has a suspension-shaped result: a parked frame joins the ordinary
    /// continuation registry and can be resumed by identity in any order.
    ///
    /// `entry` is a `ValueHandle` over a tenured `Int -> M a` closure. It is
    /// applied through the same
    /// `App(Var, Lit)` synthesis [`Self::apply_finalized`] uses. The argument
    /// crosses as a bare unboxed `Lit`, so it does not depend on a caller-owned
    /// wrapper-constructor id. Execution goes through the canonical
    /// suspension entry and registry.
    ///
    /// **`realm` is the thread's, and it propagates.** `resume_continuation` replays
    /// a frame's OWN realm, so every later suspension of this thread parks under
    /// `realm` too — which is what makes `close_realm(realm)` a complete
    /// cancellation rather than a first-frame one.
    ///
    /// The result contract belongs to the entry program. The async adapter, for
    /// example, ends by suspending on `AsyncDoneWith`; actor startup can use the
    /// same rooted entry without acquiring a second execution primitive.
    pub fn run_rooted_entry(
        &mut self,
        name_hint: &str,
        entry: RootCustody,
        argument: i64,
        realm: RealmId,
        run_table: Option<&DataConTable>,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.settle_dropped_custody();
        if !Arc::ptr_eq(&entry.cleanup, &self.custody_cleanup) {
            return Err(ResidentError::ForeignCustody);
        }
        let provenance = Arc::clone(&entry.provenance);
        // Entry consumes custody: the computation that runs it is the handle's
        // new owner, with no second consumer.
        let transfer = entry.into_transfer();
        let entry = transfer.handle;
        let slot = self
            .core
            .machine_mut()
            .and_then(|m| m.handle_slot(entry))
            .ok_or_else(|| {
                ResidentError::Run(RuntimeError::Jit(JitError::Effect(EffectError::Handler(
                    format!(
                        "run_rooted_entry: handle {entry:?} is not live (never minted, or its \
                         realm was already closed)"
                    ),
                ))))
            })?;
        // `App(Var(ROOTED_ENTRY_VAR), argument)`. Same shape and reasoning as
        // `apply_finalized`: the Var-miss arm keys the external override on
        // ExternalEnv MEMBERSHIP, and the argument rides as a bare `Lit` whose
        // plain `TAG_LIT` object the closure's own Lit-tolerant `I#` alt
        // accepts. Distinct id from `apply_finalized`'s so the two can never be
        // confused in a trace.
        const ROOTED_ENTRY_VAR: tidepool_repr::VarId = tidepool_repr::VarId(0xF4_0000_0002);
        let mut b = tidepool_repr::TreeBuilder::new();
        let f = b.push(tidepool_repr::CoreFrame::Var(ROOTED_ENTRY_VAR));
        let arg = b.push(tidepool_repr::CoreFrame::Lit(
            tidepool_repr::Literal::LitInt(argument),
        ));
        let _app = b.push(tidepool_repr::CoreFrame::App { fun: f, arg });
        let expr = b.build();

        let mut env = ExternalEnv::new();
        env.insert(ROOTED_ENTRY_VAR, slot.addr());

        let outcome = self.run_rooted_fragment(name_hint, &expr, &env, realm, run_table)?;
        transfer.commit();
        Ok(self.classify_parked(outcome, None, HoleSeed::Plain, provenance))
    }

    /// Apply one rooted Haskell function to one rooted Haskell argument and
    /// run the resulting `Eff` computation as a suspension-capable top-level
    /// turn. Both values remain opaque: no bridge, serialization, constructor
    /// inspection, or type-directed Rust code sits on this path.
    ///
    /// This is the value-to-code counterpart of [`Self::run_rooted_entry`]. It
    /// exists for boundaries such as actor mailboxes where both the handler
    /// and its protocol-indexed request are live Haskell values. Custody is
    /// transferred into the running computation only after both handles have
    /// been validated against this exact resident session. A rejected call
    /// still drops the by-value custody arguments normally.
    pub fn run_rooted_application(
        &mut self,
        name_hint: &str,
        function: RootCustody,
        argument: RootCustody,
        realm: RealmId,
        run_table: Option<&DataConTable>,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.settle_dropped_custody();
        if !Arc::ptr_eq(&function.cleanup, &self.custody_cleanup)
            || !Arc::ptr_eq(&argument.cleanup, &self.custody_cleanup)
        {
            return Err(ResidentError::ForeignCustody);
        }

        let Some(function_handle) = function.handle else {
            unreachable!("live custody always contains its handle");
        };
        let Some(argument_handle) = argument.handle else {
            unreachable!("live custody always contains its handle");
        };
        let (function_addr, argument_addr) = {
            let machine = self.core.machine_mut().ok_or_else(|| {
                ResidentError::Run(RuntimeError::Jit(JitError::Effect(EffectError::Handler(
                    "run_rooted_application: resident machine is not live".into(),
                ))))
            })?;
            let function_addr = machine
                .handle_slot(function_handle)
                .ok_or_else(|| {
                    ResidentError::Run(RuntimeError::Jit(JitError::Effect(EffectError::Handler(
                        format!(
                        "run_rooted_application: function handle {function_handle:?} is not live"
                    ),
                    ))))
                })?
                .addr();
            let argument_addr = machine
                .handle_slot(argument_handle)
                .ok_or_else(|| {
                    ResidentError::Run(RuntimeError::Jit(JitError::Effect(EffectError::Handler(
                        format!(
                        "run_rooted_application: argument handle {argument_handle:?} is not live"
                    ),
                    ))))
                })?
                .addr();
            (function_addr, argument_addr)
        };

        let mut provenance = (*function.provenance).clone();
        provenance.merge(&argument.provenance)?;
        let function = function.into_transfer();
        let argument = argument.into_transfer();

        const ROOTED_FUNCTION_VAR: tidepool_repr::VarId = tidepool_repr::VarId(0xF4_0000_0003);
        const ROOTED_ARGUMENT_VAR: tidepool_repr::VarId = tidepool_repr::VarId(0xF4_0000_0004);
        let mut builder = tidepool_repr::TreeBuilder::new();
        let function_node = builder.push(tidepool_repr::CoreFrame::Var(ROOTED_FUNCTION_VAR));
        let argument_node = builder.push(tidepool_repr::CoreFrame::Var(ROOTED_ARGUMENT_VAR));
        let _application = builder.push(tidepool_repr::CoreFrame::App {
            fun: function_node,
            arg: argument_node,
        });
        let expression = builder.build();

        let mut environment = ExternalEnv::new();
        environment.insert(ROOTED_FUNCTION_VAR, function_addr);
        environment.insert(ROOTED_ARGUMENT_VAR, argument_addr);

        let outcome =
            self.run_rooted_fragment(name_hint, &expression, &environment, realm, run_table)?;
        function.commit();
        argument.commit();
        Ok(self.classify_parked(outcome, None, HoleSeed::Plain, Arc::new(provenance)))
    }

    fn run_rooted_fragment(
        &mut self,
        name_hint: &str,
        expression: &CoreExpr,
        environment: &ExternalEnv,
        realm: RealmId,
        run_table: Option<&DataConTable>,
    ) -> Result<ParkedRun, ResidentError> {
        let table = run_table
            .cloned()
            .unwrap_or_else(|| self.core.session_table().clone());
        self.core
            .merge_table(&table)
            .map_err(ResidentError::TableCollision)?;
        self.core
            .bootstrap_if_needed(expression, &table)
            .map_err(ResidentError::Bootstrap)?;
        // Rooted runs are peers, not value-shaped children of an arbitrary
        // parked continuation.
        let function = self
            .core
            .add_fragment_session(name_hint, expression, environment)
            .map_err(ResidentError::AddFunction)?;
        let effect_policy = self.core.effect_policy();
        let live_payload = self.core.live_payload_policy();
        // Completed live results belong to the actor/session realm, not the
        // shorter-lived turn realm that happened to produce them.
        let owning_realm = self.run_context.resource_scope;
        let principal = self.run_context.principal;
        self.on_eval_thread(move |machine, table, handlers, captured| {
            let run =
                SuspensionRun::fragment(function, table, effect_policy, realm, ParkKind::Plain)
                    .with_live_payload(live_payload)
                    .with_principal(principal);
            machine
                .run_until_suspension(run, handlers, captured)
                .map(|outcome| project_parked(machine, outcome, owning_realm))
        })
    }

    /// Resume the suspended turn `hole` answered with `answer`, driving the
    /// fragment to its next suspension or completion. Atomic
    /// validate-before-consume: `hole`'s id must match the pending
    /// continuation or the pending one is untouched
    /// ([`ResidentError::WrongContinuation`], mirroring `engine.rs`:684–698 and
    /// the repl server's three-way resume errors).
    ///
    /// The ONE consuming entry point — replaces the old `resume`/`resume_bind`
    /// split. `hole` carries its own completion obligation ([`ResidentHole`]'s
    /// doc): a [`ResidentHole::Binding`] materializes its binder into the
    /// value plane on completion, using the SAME binder/generation its
    /// initiating [`Self::run_bind`] carried; a [`ResidentHole::Plain`] does
    /// nothing extra. There is no external "is this pending a bind" flag left
    /// for a caller to get out of sync with which method it calls — there is
    /// only this one method, and the hole itself says what it owes.
    pub fn resume(
        &mut self,
        hole: ResidentHole,
        answer: Value,
    ) -> Result<ResidentOutcome, ResidentError> {
        let seed = hole.seed();
        let id = match hole {
            ResidentHole::Plain(h) => h.id,
            ResidentHole::Binding(h) => h.id,
            ResidentHole::ProjectedBinding(h) => h.id,
        };
        self.reenter(&id, ResumeInput::Answer(answer), seed, None)
    }

    /// Abort the suspended turn WITHOUT running the continuation — the ask
    /// itself fails (byte-identically to the engine's stowed-abort path). Same
    /// validate-before-consume as [`Self::resume`]. Keyed by the raw
    /// continuation id (not a [`ResidentHole`]) — an abort never materializes
    /// a bind regardless of the hole's own kind, so it carries no obligation
    /// to preserve.
    pub fn abort(
        &mut self,
        cont_id: &str,
        reason: String,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.reenter(cont_id, ResumeInput::Abort(reason), HoleSeed::Plain, None)
    }

    fn reenter(
        &mut self,
        cont_id: &str,
        input: ResumeInput,
        seed: HoleSeed,
        additional_provenance: Option<&ProgramProvenance>,
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
        let mut provenance = self
            .parked_provenance
            .get(&frame_id)
            .map(|value| (**value).clone())
            .unwrap_or_default();
        if let Some(additional) = additional_provenance {
            provenance.merge(additional)?;
        }
        let provenance = Arc::new(provenance);
        // The machine is authoritative on whether the frame was actually
        // consumed: `resume_continuation` NF-forces a data-kinded answer BEFORE
        // removing the frame (A5), and on a retryable rejection leaves it
        // parked and rooted — this hole must NOT be cleared here, or a
        // retryable failure wedges the session. `classify_parked` (on `Ok`)
        // is the sole owner of the parked set on a real outcome. The frame
        // replays its own kind/table/tag, so bind-vs-plain needs no
        // re-declaration here (`bind` is only used for materialization
        // below).
        let realm = self.run_context.resource_scope;
        let outcome = self.on_eval_thread(move |machine, _table, handlers, captured| {
            machine
                .resume_continuation(frame_id, handlers, captured, input)
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
                    self.parked_provenance.remove(&frame_id);
                    self.parked.retain(|(h, _)| h != cont_id);
                }
                return Err(e);
            }
        };
        // A completed bind materializes AFTER `classify_parked` has already
        // retired this hole, so a materialize failure cannot leave the hole
        // stuck on a frame the machine no longer holds.
        let (bound, projected) = match &outcome {
            ParkedRun::CompletedValue { bound, .. } => (*bound, Vec::new()),
            ParkedRun::CompletedProject { projected } => (None, projected.clone()),
            ParkedRun::Suspended { .. } => (None, Vec::new()),
        };
        let completed = !matches!(outcome, ParkedRun::Suspended { .. });
        let resident_outcome = self.classify_parked(
            outcome,
            Some(cont_id),
            seed.clone(),
            Arc::clone(&provenance),
        );
        if completed {
            match seed {
                HoleSeed::Plain => {}
                HoleSeed::Binding { binder, generation } => {
                    self.materialize_binder(&binder, generation, bound)?;
                    self.binding_provenance.insert(binder.var_id, provenance);
                }
                HoleSeed::ProjectedBinding {
                    binders,
                    generation,
                } => {
                    self.materialize_binders(&binders, generation, projected, provenance)?;
                }
            }
        }
        Ok(resident_outcome)
    }

    /// Materialize a completed bind's tenured root into the value plane at `gen`
    /// (the generation the extract stamped into `binder.module`). Mirrors the
    /// repl's `bind_materialized`: the session layer owns the `BindingEntry`
    /// construction, the core owns the plane. Evicts any same-name decl (the
    /// one-plane invariant — a value bind wins over an earlier decl head).
    ///
    /// The lexical scope is validated before the handle root is adopted. A
    /// failed adoption therefore cannot leave the root outside both the
    /// handle registry and the binding table.
    fn materialize_binder(
        &mut self,
        binder: &BoundBinder,
        gen: Generation,
        bound: Option<ValueHandle>,
    ) -> Result<(), ResidentError> {
        let scope = self.run_context.lexical_scope;
        tracing::debug!(
            binder = %binder.name,
            generation = gen.0,
            ?scope,
            ?bound,
            tier = ?binder.tier,
            "materializing completed resident binding"
        );
        if !self.core.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope).into());
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
            .and_then(|machine| machine.take_handle_root(handle))
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
        // Root-scope bindings use this same scoped path.
        self.core.bind_replacing_decl_in(
            scope,
            BindingEntry {
                name: BindingName(binder.name.clone()),
                id: SessionVarId::from_extract(binder.var_id),
                module: SessionModule::val(gen),
                value,
                type_display: Some(binder.type_display.clone()),
                defining_expr: None,
                scope,
            },
        )?;
        self.core.set_val_gen(gen);
        tracing::debug!(
            binder = %binder.name,
            generation = gen.0,
            ?scope,
            visible = self.current_binding_in(scope, &binder.name).is_some(),
            "materialized completed resident binding"
        );
        Ok(())
    }

    fn materialize_binders(
        &mut self,
        binders: &[BoundBinder],
        gen: Generation,
        handles: Vec<ValueHandle>,
        provenance: Arc<ProgramProvenance>,
    ) -> Result<(), ResidentError> {
        if binders.len() != handles.len() {
            let produced = handles.len();
            for handle in handles {
                if let Some(machine) = self.core.machine_mut() {
                    machine.discard_handle(handle);
                }
            }
            return Err(ResidentError::Run(RuntimeError::Jit(JitError::Effect(
                EffectError::Handler(format!(
                    "projected bind produced {} roots for {} GHC binders",
                    produced,
                    binders.len()
                )),
            ))));
        }
        let scope = self.run_context.lexical_scope;
        if !self.core.scope_tree().is_live(scope) {
            for handle in handles {
                if let Some(machine) = self.core.machine_mut() {
                    machine.discard_handle(handle);
                }
            }
            return Err(SessionError::DeadScope(scope).into());
        }
        // Validate the whole projection before consuming any handle. This is
        // the atomicity membrane: an internal mismatch cannot leave half a
        // Haskell pattern installed or half its roots detached from realm
        // custody.
        let Some(machine) = self.core.machine_mut() else {
            return Err(ResidentError::Run(RuntimeError::Jit(JitError::Effect(
                EffectError::Handler("projected bind completed without a resident machine".into()),
            ))));
        };
        if handles
            .iter()
            .any(|handle| machine.handle_slot(*handle).is_none())
        {
            for handle in handles {
                machine.discard_handle(handle);
            }
            return Err(ResidentError::Run(RuntimeError::Jit(JitError::Effect(
                EffectError::Handler(
                    "projected bind root was unknown to the resident machine".into(),
                ),
            ))));
        }
        let mut slots = Vec::with_capacity(handles.len());
        let mut remaining_handles = handles.into_iter();
        while let Some(handle) = remaining_handles.next() {
            let Some(slot) = machine.take_handle_root(handle) else {
                // Defensive even though the immutable preflight above and
                // this loop share one exclusive machine borrow.
                for slot in slots {
                    machine.abandon_uncommitted_root(slot);
                }
                for remaining in remaining_handles {
                    machine.discard_handle(remaining);
                }
                return Err(ResidentError::Run(RuntimeError::Jit(JitError::Effect(
                    EffectError::Handler(
                        "projected bind root disappeared during atomic materialization".into(),
                    ),
                ))));
            };
            slots.push(slot);
        }

        let mut entries: Vec<BindingEntry> = Vec::with_capacity(binders.len());
        for (binder, slot) in binders.iter().zip(slots) {
            let value = match binder.tier {
                ValueTier::Tier0Data => BoundValue::Tier0Forced(slot),
                ValueTier::Tier1Closure => BoundValue::Tier1Closure(slot),
            };
            entries.push(BindingEntry {
                name: BindingName(binder.name.clone()),
                id: SessionVarId::from_extract(binder.var_id),
                module: SessionModule::val(gen),
                value,
                type_display: Some(binder.type_display.clone()),
                defining_expr: None,
                scope,
            });
        }
        self.core.bind_replacing_decls_in(scope, entries)?;
        self.core.set_val_gen(gen);
        for binder in binders {
            self.binding_provenance
                .insert(binder.var_id, Arc::clone(&provenance));
        }
        Ok(())
    }

    /// Move the machine onto a stack-sized eval thread, run `body`, and move the
    /// machine back. The threadless mechanism's `run_fragment`/`resume`
    /// re-install the machine's per-thread
    /// reach and re-point GC state at the retained heap. Only the machine (and
    /// the accumulated table) crosses to the thread; the rest of the session
    /// core is `!Send` (raw-pointer roots) and stays here.
    ///
    /// The machine is taken via [`PersistentSession::lease_machine`], whose
    /// [`super::MachineLease`] restores it into `self.core` on EVERY exit from
    /// this function — success, a `JitError`, a caught panic, or a failed
    /// thread spawn (a transient OS resource failure, not a bug) — so no path
    /// can leave the session permanently machineless.
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
        self.settle_dropped_custody();
        let mut lease = self.core.lease_machine();
        let (machine_ref, table) = lease.parts();
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

        // `lease` drops here (function-end, on every path above), restoring
        // the machine into `self.core` regardless of how `outcome` resolved.
        match outcome {
            EvalThreadOutcome::Ran(Ok(t)) => Ok(t),
            EvalThreadOutcome::Ran(Err(e)) => Err(ResidentError::Run(RuntimeError::Jit(e))),
            EvalThreadOutcome::Panicked(payload) => Err(panic_to_run_error(payload)),
            EvalThreadOutcome::SpawnFailed(e) => Err(ResidentError::EvalThread(e)),
        }
    }

    /// Release affine roots whose custody was dropped while the machine was
    /// checked into a registry or otherwise unavailable to the token itself.
    fn settle_dropped_custody(&mut self) -> usize {
        let handles = self.custody_cleanup.take_all();
        let count = handles.len();
        if let Some(machine) = self.core.machine_mut() {
            for handle in handles {
                machine.discard_handle(handle);
            }
        }
        count
    }

    /// Classify a projected parked outcome into a [`ResidentOutcome`]:
    /// completion retires `resumed` (the hole this outcome answered — `None`
    /// for a fresh run, which retires nothing), suspension mints a hole of
    /// `seed`'s obligation and pushes `(id string, id)` onto the parked set.
    /// Output is drained on completion and snapshotted on suspension, same as
    /// the engine.
    fn classify_parked(
        &mut self,
        outcome: ParkedRun,
        resumed: Option<&str>,
        seed: HoleSeed,
        provenance: Arc<ProgramProvenance>,
    ) -> ResidentOutcome {
        match outcome {
            ParkedRun::CompletedValue { value, .. } => {
                self.retire_resumed(resumed);
                let output = self.captured.drain();
                ResidentOutcome::Completed {
                    output,
                    result: EvalResult::new(value, self.core.session_table().clone(), Vec::new()),
                }
            }
            ParkedRun::CompletedProject { .. } => {
                self.retire_resumed(resumed);
                ResidentOutcome::BindingsCommitted {
                    output: self.captured.drain(),
                }
            }
            ParkedRun::Suspended { id, request } => {
                // A resume that re-suspended: the OLD hole is spent (the
                // frame was consumed; a fresh frame parked under a FRESH id —
                // ids are never reused) and the new one replaces it.
                self.retire_resumed(resumed);
                let cont_id = self.next_cont_id();
                self.parked.push((cont_id.clone(), id));
                self.parked_provenance.insert(id, provenance);
                let output = self.captured.snapshot();
                ResidentOutcome::Suspended {
                    output,
                    hole: ResidentHole::mint(cont_id, seed),
                    request,
                }
            }
        }
    }

    fn retire_resumed(&mut self, resumed: Option<&str>) {
        let Some(hole) = resumed else {
            return;
        };
        if let Some((_, id)) = self.parked.iter().find(|(name, _)| name == hole) {
            self.parked_provenance.remove(id);
        }
        self.parked.retain(|(name, _)| name != hole);
    }
}

/// The kernel's generalized suspension seam
/// ([`super::kernel::SuspendableSession`]), implemented directly against this
/// session's own [`Self::resume`]/[`Self::abort`]. This type already has the
/// shape the kernel requires: one obligation-carrying [`ResidentHole`] token
/// and one resume/abort entry point per token. `Context = ()`: this session owns
/// its captured-output buffer and handler stack as fields, so a call needs
/// nothing extra beyond the hole and the answer.
impl<H, O> super::kernel::SuspendableSession for ResidentSession<H, O>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    type Hole = ResidentHole;
    type Answer = Value;
    type Context = ();
    type Outcome = ResidentOutcome;
    type Error = ResidentError;

    fn resume(
        &mut self,
        hole: Self::Hole,
        answer: Self::Answer,
        (): Self::Context,
    ) -> Result<Self::Outcome, Self::Error> {
        Self::resume(self, hole, answer)
    }

    fn abort(
        &mut self,
        hole: Self::Hole,
        reason: String,
        (): Self::Context,
    ) -> Result<Self::Outcome, Self::Error> {
        Self::abort(self, hole.cont_id(), reason)
    }
}

/// The `Send` projection of a [`ParkedOutcome`] that crosses the eval-thread
/// boundary: a bind's tenured `!Send` `RootSlot` is minted into a
/// [`ValueHandle`] IN-THREAD (`realm`-owned) and the id crosses instead —
/// resolved back to its slot by `materialize_binder` on the session thread.
/// `CompletedProject` is the multi-binder lane; `CompletedRender` remains
/// unreachable because resident turns do not use render parking. Live-payload
/// presence is dropped because the payload itself is acquired explicitly from
/// its frame.
enum ParkedRun {
    CompletedValue {
        value: Value,
        bound: Option<ValueHandle>,
    },
    CompletedProject {
        projected: Vec<ValueHandle>,
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
    realm: RealmId,
) -> ParkedRun {
    match outcome {
        ParkedOutcome::CompletedValue(value) => ParkedRun::CompletedValue { value, bound: None },
        ParkedOutcome::CompletedBinding { value, root } => ParkedRun::CompletedValue {
            value,
            bound: Some(machine.mint_handle_from_root(root, realm)),
        },
        ParkedOutcome::CompletedProject { roots } => ParkedRun::CompletedProject {
            projected: roots
                .into_iter()
                .map(|root| machine.mint_handle_from_root(root, realm))
                .collect(),
        },
        ParkedOutcome::CompletedRender { .. } => {
            unreachable!("the resident lane does not park render turns")
        }
        ParkedOutcome::Suspended { id, request, .. } => ParkedRun::Suspended { id, request },
    }
}

/// The three ways a resident eval thread's lifecycle can resolve — spawn
/// failure, a caught panic, or a completed run of `body` (itself carrying its
/// own `Result`). Distinct from `SpawnError`/join-panic being conflated into
/// one `.expect()`, which is exactly what let a spawn failure escape as an
/// unguarded panic.
enum EvalThreadOutcome<T> {
    Ran(Result<T, JitError>),
    Panicked(Box<dyn std::any::Any + Send>),
    SpawnFailed(std::io::Error),
}

/// Map a caught panic payload (a Rust-level fault that unwound past the JIT's
/// own `with_signal_protection` — a genuine bug, not a language-level error) to
/// a run error carrying the payload string.
fn panic_to_run_error(payload: Box<dyn std::any::Any + Send>) -> ResidentError {
    let detail = crate::panic_payload_message(payload);
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
            NullSink,
            Vec::new(),
            crate::DEFAULT_NURSERY_SIZE,
            None,
        )
        .expect("a trivial Lit expression over an empty table compiles")
    }

    fn typed_site(site: u64, ty: &str) -> YieldSite {
        YieldSite {
            site,
            origin: "M.program".into(),
            ordinal: 0,
            ty: ty.into(),
            modules: Vec::new(),
            heads: Vec::new(),
            inputs: Vec::new(),
        }
    }

    #[test]
    fn program_provenance_unions_identical_sites_and_rejects_collisions() {
        let mut provenance =
            ProgramProvenance::from_sites(&[typed_site(11, "Int")]).expect("first site");
        let same = ProgramProvenance::from_sites(&[typed_site(11, "Int")]).expect("same site");
        provenance.merge(&same).expect("identical metadata merges");
        assert_eq!(provenance.sites(), vec![typed_site(11, "Int")]);

        let conflicting =
            ProgramProvenance::from_sites(&[typed_site(11, "Bool")]).expect("other site set");
        let error = provenance
            .merge(&conflicting)
            .expect_err("same id with different metadata must fail");
        assert_eq!(error.site, 11);
        assert_eq!(error.first.ty, "Int");
        assert_eq!(error.second.ty, "Bool");
    }

    /// A deterministic thread-spawn failure must return a typed error and
    /// restore the leased machine to the session.
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

    #[test]
    fn run_context_rejects_a_dead_scope_atomically() {
        let mut session = bootstrap_trivial_session();
        let live = session.mint_scope(ScopeId::ROOT).expect("ROOT is live");
        let live_context = SessionRunContext::new(RealmId(41), live, PrincipalId::new(7, 1));
        session
            .set_run_context(live_context)
            .expect("freshly-minted scope is live");
        assert_eq!(session.run_context(), live_context);

        session.retire_scope(live);
        let now_dead = live;

        let result = session.set_run_context(SessionRunContext::new(
            RealmId(42),
            now_dead,
            PrincipalId::new(8, 1),
        ));
        assert!(
            matches!(
                result,
                Err(ResidentError::Session(SessionError::DeadScope(s))) if s == now_dead
            ),
            "expected a typed DeadScope error, got {result:?}"
        );
        assert_eq!(
            session.run_context(),
            live_context,
            "a rejected assignment must change neither resource nor lexical scope"
        );

        let never_minted = ScopeId(999_999);
        assert!(matches!(
            session.set_run_context(SessionRunContext::new(
                RealmId(43),
                never_minted,
                PrincipalId::new(9, 1),
            )),
            Err(ResidentError::Session(SessionError::DeadScope(s))) if s == never_minted
        ));

        session
            .set_run_context(SessionRunContext::ROOT)
            .expect("ROOT is always live");
        assert_eq!(session.run_context(), SessionRunContext::ROOT);
    }

    /// A dead target scope rejects the mount and consumes custody without
    /// creating a binding.
    #[test]
    fn mount_handle_in_rejects_a_dead_scope_without_leaking_custody() {
        let mut session = bootstrap_trivial_session();
        let scope = session.mint_scope(ScopeId::ROOT).expect("ROOT is live");
        session.retire_scope(scope);

        // An arbitrary handle id: the liveness check must short-circuit
        // before this is ever resolved against the machine's handle
        // registry, so it need not be a real, live-minted handle.
        let custody = RootCustody::new(
            ValueHandle(0),
            Arc::clone(&session.custody_cleanup),
            Arc::new(ProgramProvenance::default()),
        );
        let result = session.mount_handle_in(scope, "escapee", custody);

        assert!(
            matches!(
                result,
                Err(ResidentError::Session(SessionError::DeadScope(s))) if s == scope
            ),
            "expected a typed DeadScope error, got {result:?}"
        );
        assert_eq!(
            session.binding_names_in(scope),
            Vec::<String>::new(),
            "a rejected mount must not have written a binding"
        );
    }

    /// A missing target binding rejects the mount and consumes custody.
    #[test]
    fn mount_handle_in_rejects_a_missing_binding_without_leaking_custody() {
        let mut session = bootstrap_trivial_session();
        let scope = session.mint_scope(ScopeId::ROOT).expect("ROOT is live");

        // Arbitrary, need not be live-minted — resolution fails before the
        // handle registry is ever consulted.
        let custody = RootCustody::new(
            ValueHandle(0),
            Arc::clone(&session.custody_cleanup),
            Arc::new(ProgramProvenance::default()),
        );
        let result = session.mount_handle_in(scope, "nope", custody);

        assert!(
            matches!(
                &result,
                Err(ResidentError::Session(SessionError::UnknownBinding { scope: s, name }))
                    if *s == scope && name == "nope"
            ),
            "expected a typed UnknownBinding error, got {result:?}"
        );
        assert_eq!(
            session.binding_names_in(scope),
            Vec::<String>::new(),
            "a rejected mount must not have written a binding"
        );
    }

    #[test]
    fn dropped_custody_is_queued_and_settled_without_panicking() {
        let mut session = bootstrap_trivial_session();
        let custody = RootCustody::new(
            ValueHandle(u64::MAX),
            Arc::clone(&session.custody_cleanup),
            Arc::new(ProgramProvenance::default()),
        );

        drop(custody);

        assert_eq!(session.custody_cleanup.abandoned.lock().len(), 1);
        assert_eq!(session.settle_dropped_custody(), 1);
        assert!(session.custody_cleanup.abandoned.lock().is_empty());
    }
}
