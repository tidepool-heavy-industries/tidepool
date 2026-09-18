//! `PersistentSession` — the resident-JIT session core shared by
//! `tidepool-repl` and `tidepool-harness`.
//!
//! Both crates drive a long-lived [`JitEffectMachine`] turn-by-turn, accumulate
//! declarations (the [`SessionLib`] decl plane) and value bindings (the
//! [`BindingTable`] value plane), and union each turn's constructor metadata into
//! one growing [`DataConTable`]. That substrate — machine lifecycle, the two
//! planes, the accumulated table, and the fragment-run primitives — is identical
//! between them and lives here.
//!
//! Suspension is **threadless** everywhere: an `Ask` stows the machine (or, on
//! the parked lane below, one continuation of many) as DATA — the eval thread
//! exits, and a fresh thread re-enters to resume. No OS thread is parked per
//! suspended session — neither in the harness (a TREE of many
//! simultaneously-suspended nodes cannot pin N+1 threads) nor in the repl.
//!
//! Suspension uses the machine's continuation registry everywhere. This core
//! exposes a capacity-one façade for the REPL: it remembers one active
//! continuation id while the machine-level registry supplies rooting,
//! cancellation, retry, and resume semantics.
//!
//! Every run entry here therefore reports either a completion or a suspension,
//! and every one has a `resume_*` sibling that re-enters the stowed continuation
//! with an answer or an abort. The four result-materialization policies the JIT
//! supports — plain `Value`, single `Bind`, projected multi-bind, and
//! bind+render — each appear as such a pair, each carrying what it actually
//! produces ([`SuspendableOutcome`] for the two `Value`-completing policies,
//! [`Suspendable`] over the roots for the other two). See the module docstrings
//! of [`super::resident`] and `tidepool-repl`'s `session.rs` for the orchestration
//! around this core.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use tidepool_bridge::Value;
use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BindingTipId};
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{CancelHandle, FuncId, JitEffectMachine, MachineDisposition};
use tidepool_codegen::old_space::RootSlot;
use tidepool_codegen::prepared_program::ResidencyCounts;
use tidepool_codegen::scope::{ScopeId, ScopeTree};
use tidepool_codegen::suspension::{
    ContinuationId, ParkKind, ParkedOutcome, RealmId, ResumeInput, Suspendable, SuspendableOutcome,
    SuspensionRun,
};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{
    CoreExpr, DataCon, DataConTable, Generation, SessionModule, SessionVarId, VarId,
};

use tidepool_codegen::binding_table::BoundValue;
use tidepool_repr::execution_schema::{PreparedProgram, SymbolIdentity};

use super::binding_table::{BindRecord, BindingIndex};
use super::OutputSink;
use super::prepared::{PreparedEngine, PreparedRuntimeError};
use super::{
    ExactExportError, ExactExportSurface, SessionCompileView, SessionError, SessionLib,
    SourceImports,
};
use crate::JitError;

/// Which execution engine a resident session runs its turns on. Chosen once
/// at construction and never switched: a session on the prepared route never
/// falls back to Core after a turn starts or fails.
///
/// This selector is the cutover's temporary explicit migration route; see
/// `plans/core-removal-order.md` for the current removal-order survey. It is
/// deleted along with the Core engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    /// The Core JIT machine every session ran on before the cutover.
    Core,
    /// The prepared-STG machine: turns compile through `--prepared-turn` and
    /// run as settled scaffolds.
    Prepared,
}

impl EngineKind {
    /// The environment variable the composition roots read once per session.
    pub const ENV: &'static str = "TIDEPOOL_ENGINE";

    /// The route named by `TIDEPOOL_ENGINE` (`core` opts out to the Core
    /// machine; `prepared`, unset, or anything else selects the prepared
    /// machine, now the default). Read once at session construction by the
    /// composition roots, never inside a turn.
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var(Self::ENV) {
            Ok(value) if value.eq_ignore_ascii_case("core") => Self::Core,
            _ => Self::Prepared,
        }
    }
}

/// The session's live execution engine, once its first turn has bootstrapped
/// it. Exactly one variant ever exists for a session ([`EngineKind`]).
// One value per session, moved once per turn onto the eval thread; the size
// difference between the two machines is not a cost worth an indirection.
#[expect(
    clippy::large_enum_variant,
    reason = "one value per session, moved once per turn"
)]
pub enum ResidentEngine {
    Core(JitEffectMachine),
    Prepared(PreparedEngine),
}

impl ResidentEngine {
    #[must_use]
    pub fn core(&self) -> Option<&JitEffectMachine> {
        match self {
            Self::Core(machine) => Some(machine),
            Self::Prepared(_) => None,
        }
    }

    pub fn core_mut(&mut self) -> Option<&mut JitEffectMachine> {
        match self {
            Self::Core(machine) => Some(machine),
            Self::Prepared(_) => None,
        }
    }

    pub fn prepared_mut(&mut self) -> Option<&mut PreparedEngine> {
        match self {
            Self::Core(_) => None,
            Self::Prepared(engine) => Some(engine),
        }
    }

    /// The Core machine, or the typed refusal a Core-only turn path reports
    /// on the prepared route. No path ever falls back across engines.
    pub fn require_core(&mut self) -> Result<&mut JitEffectMachine, JitError> {
        self.core_mut().ok_or(JitError::InvalidSuspensionState(
            "this turn path runs only on the Core engine; the session runs prepared STG",
        ))
    }

    /// The prepared engine, or the typed refusal a prepared-only turn path
    /// reports on the Core route. No path ever falls back across engines.
    pub fn require_prepared(&mut self) -> Result<&mut PreparedEngine, JitError> {
        self.prepared_mut().ok_or(JitError::InvalidSuspensionState(
            "this session runs prepared STG but holds no prepared engine",
        ))
    }

    /// The continuation ids parked on this session's machine, whichever
    /// engine it runs.
    #[must_use]
    pub fn parked_ids(&self) -> Vec<ContinuationId> {
        match self {
            Self::Core(machine) => machine.parked_ids(),
            Self::Prepared(engine) => engine.parked_ids(),
        }
    }

    /// Cancellation handle for this capacity-one registry realm.
    #[must_use]
    pub fn cancel_handle(&mut self) -> CancelHandle {
        match self {
            Self::Core(machine) => machine.realm_cancel_handle(RealmId::ROOT),
            Self::Prepared(engine) => engine.cancel_handle(RealmId::ROOT),
        }
    }

    #[must_use]
    pub fn disposition(&self) -> MachineDisposition {
        match self {
            Self::Core(machine) => machine.disposition(),
            Self::Prepared(engine) => engine.disposition(),
        }
    }

    fn ensure_reusable(&self) -> Result<(), JitError> {
        match self {
            Self::Core(machine) => machine.ensure_reusable(),
            Self::Prepared(engine) => match engine.disposition() {
                MachineDisposition::Reusable => Ok(()),
                MachineDisposition::Unavailable => Err(JitError::InvalidSuspensionState(
                    "prepared machine is unavailable after an integrity failure",
                )),
            },
        }
    }

    fn persistent_roots_count(&self) -> usize {
        match self {
            Self::Core(machine) => machine.persistent_roots_count(),
            Self::Prepared(engine) => engine.persistent_roots_count(),
        }
    }

    fn value_handle_count(&self) -> usize {
        match self {
            Self::Core(machine) => machine.value_handle_count(),
            Self::Prepared(engine) => engine.handle_count(),
        }
    }

    fn stowed_roots_count(&self) -> usize {
        match self {
            Self::Core(machine) => machine.stowed_roots_count(),
            Self::Prepared(engine) => engine.stowed_roots_count(),
        }
    }

    fn parked_count(&self) -> usize {
        match self {
            Self::Core(machine) => machine.parked_count(),
            Self::Prepared(engine) => engine.parked_count(),
        }
    }

    /// The runtime resource scope owning the frame parked under `id`.
    #[must_use]
    pub fn parked_realm(&self, id: ContinuationId) -> Option<RealmId> {
        match self {
            Self::Core(machine) => machine.parked_realm(id),
            Self::Prepared(engine) => engine.parked_realm(id),
        }
    }

    /// Close a runtime resource scope: `(frames, handles_released)`.
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        match self {
            Self::Core(machine) => machine.close_realm(realm),
            Self::Prepared(engine) => engine.close_realm(realm),
        }
    }

    /// Prepared-machine residency counters; `None` on the Core route (Core
    /// has no bounded-residency accounting to report).
    #[must_use]
    pub fn residency(&self) -> Option<ResidencyCounts> {
        match self {
            Self::Core(_) => None,
            Self::Prepared(engine) => Some(engine.residency()),
        }
    }

    /// Lifetime `(functions, code_bytes)` of Cranelift work this engine's
    /// installs caused; `None` on the Core route.
    #[must_use]
    pub fn codegen_totals(&self) -> Option<(u64, u64)> {
        match self {
            Self::Core(_) => None,
            Self::Prepared(engine) => Some(engine.codegen_totals()),
        }
    }

    /// Prepared old-space bytes as of the last successful between-turn
    /// collection; `None` on the Core route.
    #[must_use]
    pub fn old_bytes(&self) -> Option<usize> {
        match self {
            Self::Core(_) => None,
            Self::Prepared(engine) => Some(engine.old_bytes()),
        }
    }

    /// Read-only heap/GC snapshot, whichever engine this session runs -- see
    /// [`PreparedEngine::heap_stats`] for the prepared route's field mapping.
    #[must_use]
    pub fn heap_stats(&self) -> tidepool_codegen::jit_machine::HeapStats {
        match self {
            Self::Core(machine) => machine.heap_stats(),
            Self::Prepared(engine) => engine.heap_stats(),
        }
    }
}

/// Cross-thread custody for one completed bind root. The root never moves
/// independently: it remains inside the session while that session is stowed,
/// and is taken only after the session returns to its owning thread.
struct LinearRootStash(Option<RootSlot>);

// SAFETY: identical to JitEffectMachine's stow-XOR-run guarantee. The raw
// slot is never dereferenced while the containing session is in transit and
// has a single owner at every point.
unsafe impl Send for LinearRootStash {}

// ---------------------------------------------------------------------------
// The shared session core
// ---------------------------------------------------------------------------

/// The resident-session substrate both servers own: one live [`JitEffectMachine`]
/// (`None` until the first turn bootstraps it), the accumulated constructor
/// [`DataConTable`], the [`SessionLib`] decl plane, the [`BindingTable`] value
/// plane, and the value-binding generation.
///
/// The consumers keep their own higher-level turn orchestration (source
/// wrapping, decl/pure-bind routing, output draining, continuation-id minting)
/// and delegate the machine + plane operations here.
pub struct PersistentSession {
    /// The resident machine — `None` before the first turn bootstraps it,
    /// `Some` when idle/suspended, and moved out onto the eval thread for a
    /// turn's duration (stowed-XOR-running). Its variant is fixed by
    /// `engine_kind` for the session's life.
    machine: Option<ResidentEngine>,
    /// The route this session was constructed on; the only engine `machine`
    /// will ever hold.
    engine_kind: EngineKind,
    /// The constructor metadata unioned across turns (`insert_checked`, monotone:
    /// later turns are a subset), so an ADT value bound earlier renders with real
    /// con names later.
    session_table: DataConTable,
    /// The declaration plane: user `data`/`class`/`f x = …` accumulated as source
    /// across turns, imported by later turns through the gen-versioned module.
    /// `None` for a session with no decl plane; `Some` for the repl and the
    /// accumulating harness.
    lib: Option<SessionLib>,
    /// The value plane: `name → (SessionVarId, RootSlot, Val.G<g>)` for each
    /// materialized bind, seeded into a later fragment's [`ExternalEnv`].
    bindings: BindingTable,
    /// Incremental indexes over `bindings`' live set (prepared-import
    /// resolution, retained-import pairs, live module names, root-slot
    /// aliasing refcounts), kept in sync at every bind/evict site below so
    /// no per-turn caller scans the whole live set. See
    /// [`super::binding_table`].
    binding_index: BindingIndex,
    /// Monotonic value-binding generation. Each materialized bind mints a fresh
    /// `Val.G<g>` so its `stableVarId` is collision-free and a rebind shadows
    /// without clobbering the prior root.
    val_gen: Generation,
    /// The scope forest — one per session, shared by BOTH
    /// planes. The value plane hangs [`BindingTable`] frames off these ids and
    /// the decl plane keys its per-scope tips off the SAME ids, which is why
    /// neither owns a forest of its own: two forests would be two answers to "is
    /// this scope live", and a scoped decl and a scoped binding would drift.
    /// [`ScopeId::ROOT`] is the flat session every pre-C2 caller lives in.
    scopes: ScopeTree,
    /// Monotonic per-turn counter → unique fragment function names.
    turn_counter: u64,
    /// How requests interact with the handlers installed for this checkout.
    effect_policy: EffectRunPolicy,
    /// Live-value crossing policy paired with the current effect stack.
    live_payload: LivePayloadPolicy,
    /// Capacity-one façade state for the REPL/linear session surface.
    active_continuation: Option<ContinuationId>,
    /// A bind root completed through the registry and carried home inside the
    /// session because `RootSlot` is not `Send` on its own.
    last_bound_root: LinearRootStash,
    /// JIT nursery size for the resident machine.
    nursery_size: usize,
    /// Set once, by [`Self::mark_ready`], when a caller supplied a real
    /// first-turn seed on the prepared route even though `machine` stays
    /// `None` until that turn's prepared program installs
    /// ([`Self::install_prepared`]). Never reset. [`Self::is_bootstrapped`]
    /// folds this in so a prepared session built with real intent
    /// (`ResidentSession::bootstrap`) reports reusable, not uninitialized,
    /// before its machine exists — matching Core, where `machine.is_some()`
    /// alone already carries that fact.
    ready: bool,
}

/// The committed fact from moving one name to the materialized value plane.
/// Callers use this rather than inferring success from a partly-mutated view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValuePlaneCommit {
    pub name: String,
    pub module: SessionModule,
}

/// The committed facts from materializing one complete binding set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializationSetCommit {
    pub bindings: Vec<ValuePlaneCommit>,
}

/// The committed fact from adding declarations and evicting their same-scope
/// value-plane names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationPlaneCommit {
    pub generation: Generation,
    pub module: SessionModule,
    /// The value/type/class exports GHC reported for the committed source.
    pub items: Vec<super::ExportItem>,
    /// Same-scope materialized values actually evicted by those exports.
    pub evicted_values: Vec<String>,
}

impl PersistentSession {
    /// Build an idle session core. `lib` is the decl plane (`Some` for the repl
    /// and the accumulating harness; `None` for a value-plane-only session). The
    /// machine is not bootstrapped until the first turn.
    pub fn new(lib: Option<SessionLib>, nursery_size: usize, engine_kind: EngineKind) -> Self {
        PersistentSession {
            machine: None,
            engine_kind,
            session_table: DataConTable::new(),
            lib,
            bindings: BindingTable::new(),
            binding_index: BindingIndex::new(),
            val_gen: Generation(0),
            scopes: ScopeTree::new(),
            turn_counter: 0,
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            active_continuation: None,
            last_bound_root: LinearRootStash(None),
            nursery_size,
            ready: false,
        }
    }

    // -- accessors ---------------------------------------------------------

    /// The decl-plane library (read). Panics if the session has no decl plane —
    /// a repl invariant; the harness only calls this once a decl plane has been
    /// installed.
    pub fn lib(&self) -> &SessionLib {
        #[allow(clippy::expect_used, reason = "decl plane present")]
        self.lib.as_ref().expect("decl plane present")
    }
    /// The decl-plane library (mutate — e.g. `define_batch_with_vals`). Panics if
    /// the session has no decl plane (see [`Self::lib`]).
    pub fn lib_mut(&mut self) -> &mut SessionLib {
        #[allow(clippy::expect_used, reason = "decl plane present")]
        self.lib.as_mut().expect("decl plane present")
    }
    /// Whether this session has a decl plane.
    pub fn has_lib(&self) -> bool {
        self.lib.is_some()
    }
    /// The value-plane binding table (read).
    pub fn bindings(&self) -> &BindingTable {
        &self.bindings
    }
    /// The value-plane binding table (mutate).
    pub fn bindings_mut(&mut self) -> &mut BindingTable {
        &mut self.bindings
    }

    /// Keep eight automatic observations per scope. Explicit persistent code
    /// and fork tips retain their dependencies under the normal binding rules.
    pub fn save_observation(&mut self, id: SessionVarId, dependencies: &[VarId]) {
        let expired = self.bindings.save_observation(id, dependencies, 8);
        self.release_binding_roots(expired);
    }

    pub(super) fn release_binding_roots(&mut self, entries: Vec<BindingEntry>) -> usize {
        let mut released = 0usize;
        for entry in entries {
            let slot = entry.value.root();
            // `on_evict` is the single point of truth for whether any OTHER
            // live entry still shares this root slot (an alias published by
            // `bind_alias_in`, or a same-batch sibling evicted alongside
            // this entry) -- replacing the old whole-table scan. It must run
            // exactly once per entry that leaves `live`, which this is.
            let safe_to_release = self.binding_index.on_evict(&entry);
            if !safe_to_release {
                continue;
            }
            match (&entry.value, self.machine.as_mut()) {
                // A prepared binding's root IS its adopted handle: releasing
                // the handle deregisters the root.
                (BoundValue::Prepared { handle, .. }, Some(ResidentEngine::Prepared(engine))) => {
                    if engine.release(*handle) {
                        released += 1;
                    }
                }
                (_, Some(ResidentEngine::Core(machine))) => {
                    let held = machine.handle_holds_root(slot);
                    debug_assert!(!held, "retiring binding root still owned by a handle");
                    if !held {
                        machine.retire_scope_root(slot);
                        released += 1;
                    }
                }
                _ => {}
            }
        }
        released
    }
    /// The accumulated constructor table.
    pub fn session_table(&self) -> &DataConTable {
        &self.session_table
    }
    /// The current value-binding generation.
    pub fn val_gen(&self) -> Generation {
        self.val_gen
    }
    /// Advance the value-module generation high-water mark.
    pub fn set_val_gen(&mut self, g: Generation) {
        // MONOTONIC MAX, not assignment: with any-order resume, two in-flight
        // bind turns can materialize out of mint order —
        // gen 7 completing before gen 6. A plain assignment would REWIND the
        // counter on the late gen-6 materialization, and the next mint would
        // re-issue 7, colliding with the live Val.G7. Generations are only
        // ever bumped, never reused (`Generation::next`'s contract) — this
        // enforces it at the one write site.
        if g.0 > self.val_gen.0 {
            self.val_gen = g;
        }
    }
    pub fn effect_policy(&self) -> EffectRunPolicy {
        self.effect_policy
    }

    #[must_use]
    pub fn live_payload_policy(&self) -> LivePayloadPolicy {
        self.live_payload
    }

    /// Select request routing and live-value crossing for the next checkout.
    pub fn set_effect_execution(
        &mut self,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
    ) {
        self.effect_policy = effect_policy;
        self.live_payload = live_payload;
    }
    /// Whether the session is reusable without a fresh first-turn bootstrap:
    /// `machine.is_some()` on Core, where that alone is the fact; on the
    /// prepared route, also true once [`Self::mark_ready`] recorded a real
    /// first-turn seed, even before the first prepared install actually
    /// creates `machine`.
    pub fn is_bootstrapped(&self) -> bool {
        self.machine.is_some() || self.ready
    }

    /// Record that a caller supplied a real first-turn seed
    /// (`ResidentSession::bootstrap`) on the prepared route, where `machine`
    /// stays `None` until the first prepared program installs
    /// ([`Self::install_prepared`]). [`Self::is_bootstrapped`] folds this in
    /// so such a session reports reusable rather than uninitialized in the
    /// gap before that install. Never reset.
    pub(crate) fn mark_ready(&mut self) {
        self.ready = true;
    }
    /// The route this session runs on.
    #[must_use]
    pub fn engine_kind(&self) -> EngineKind {
        self.engine_kind
    }
    /// The resident Core machine, if bootstrapped (read — e.g. `heap_stats`).
    /// `None` on the prepared route, whose paths reach the engine through
    /// [`Self::prepared_mut`] instead; a Core-only path finding `None` here
    /// reports its typed refusal rather than falling back.
    pub fn machine(&self) -> Option<&JitEffectMachine> {
        self.machine.as_ref().and_then(ResidentEngine::core)
    }
    /// The resident Core machine, if bootstrapped (mutate).
    pub fn machine_mut(&mut self) -> Option<&mut JitEffectMachine> {
        self.machine.as_mut().and_then(ResidentEngine::core_mut)
    }
    /// The prepared engine, once the first prepared turn has installed it.
    pub fn prepared_mut(&mut self) -> Option<&mut PreparedEngine> {
        self.machine.as_mut().and_then(ResidentEngine::prepared_mut)
    }

    /// The prepared engine, or the typed refusal a prepared-only turn path
    /// reports on the Core route.
    pub fn require_prepared(&mut self) -> Result<&mut PreparedEngine, PreparedRuntimeError> {
        self.prepared_mut().ok_or(PreparedRuntimeError::WrongEngine)
    }

    /// The continuation ids parked on this session's machine, whichever
    /// engine it runs: the ground truth a hole is reconciled against after a
    /// failed resume. Empty before the machine exists.
    #[must_use]
    pub fn parked_ids(&self) -> Vec<ContinuationId> {
        self.machine
            .as_ref()
            .map(ResidentEngine::parked_ids)
            .unwrap_or_default()
    }

    /// Whether the resident machine can safely accept another entry.
    ///
    /// `None` means this session has not bootstrapped a machine yet. Once a
    /// machine exists, language failures and cancellation leave it
    /// [`MachineDisposition::Reusable`], while failures that make heap or code
    /// integrity uncertain monotonically make it
    /// [`MachineDisposition::Unavailable`]. Source recovery is a separate
    /// declaration-plane report and never changes this decision.
    #[must_use]
    pub fn machine_disposition(&self) -> Option<MachineDisposition> {
        self.machine.as_ref().map(ResidentEngine::disposition)
    }

    fn ensure_machine_reusable(&self) -> Result<(), JitError> {
        self.machine
            .as_ref()
            .map_or(Ok(()), ResidentEngine::ensure_reusable)
    }

    /// Cancellation handle for this capacity-one registry realm.
    pub fn cancel_handle(&mut self) -> Option<CancelHandle> {
        self.machine.as_mut().map(ResidentEngine::cancel_handle)
    }

    /// The runtime resource scope owning the frame parked under `id`,
    /// whichever engine this session runs. `None` before the machine exists
    /// or if `id` names no live frame.
    #[must_use]
    pub fn parked_realm(&self, id: ContinuationId) -> Option<RealmId> {
        self.machine.as_ref()?.parked_realm(id)
    }

    /// Close a runtime resource scope on the resident machine, whichever
    /// engine it runs: `(frames, handles_released)`. `(0, 0)` when the
    /// machine is not yet booted or the realm owns nothing (idempotent).
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        self.machine
            .as_mut()
            .map_or((0, 0), |engine| engine.close_realm(realm))
    }

    /// Prepared-machine residency counters; `None` on the Core route or
    /// before the machine has bootstrapped.
    #[must_use]
    pub fn residency(&self) -> Option<ResidencyCounts> {
        self.machine.as_ref()?.residency()
    }

    /// Lifetime `(functions, code_bytes)` of Cranelift work this session's
    /// installs caused; `None` on the Core route or before bootstrap.
    #[must_use]
    pub fn codegen_totals(&self) -> Option<(u64, u64)> {
        self.machine.as_ref()?.codegen_totals()
    }

    /// Prepared old-space bytes as of the last successful between-turn
    /// collection; `None` on the Core route or before the machine has
    /// bootstrapped.
    #[must_use]
    pub fn old_bytes(&self) -> Option<usize> {
        self.machine.as_ref()?.old_bytes()
    }

    /// Read-only heap/GC snapshot of this session's live machine, whichever
    /// engine it runs; `None` before the machine has bootstrapped.
    #[must_use]
    pub fn heap_stats(&self) -> Option<tidepool_codegen::jit_machine::HeapStats> {
        self.machine.as_ref().map(ResidentEngine::heap_stats)
    }

    // -- table accumulation ------------------------------------------------

    /// Seed the accumulated session table wholesale (the bootstrap turn's table
    /// becomes the base; later turns [`Self::merge_table`] onto it).
    pub fn seed_session_table(&mut self, table: DataConTable) {
        self.session_table = table;
    }

    /// Union `table`'s constructors into the accumulated session table
    /// (`extend_checked`; loud on a genuine `stableVarId` collision — gen-versioned
    /// names make that a real bug, not churn).
    ///
    /// A turn's table is normally a SUBSET of what earlier turns already
    /// accumulated, so entries already present with identical metadata are
    /// filtered out before touching the table at all — no clone, no index
    /// work, no sort for the steady-state no-new-constructors turn. What
    /// remains is batched through [`DataConTable::extend_checked`], which
    /// sorts each affected `by_type_name` bucket once instead of once per
    /// insert.
    pub fn merge_table(&mut self, table: &DataConTable) -> Result<(), String> {
        let turn_cons = table.iter().count();
        let incoming: Vec<DataCon> = table
            .iter()
            .filter(|&dc| self.session_table.get(dc.id) != Some(dc))
            .cloned()
            .collect();
        let advances_constructor_vocabulary = !incoming.is_empty();
        log::debug!(
            target: "tidepool::session",
            "merge_table turn_cons={turn_cons} skipped={} applied={} session_cons_before={}",
            turn_cons - incoming.len(),
            incoming.len(),
            self.session_table.len(),
        );
        self.session_table
            .extend_checked(incoming)
            .map_err(|e| format!("session DataConTable collision: {e}"))?;
        if advances_constructor_vocabulary {
            if let Some(ResidentEngine::Core(machine)) = self.machine.as_mut() {
                machine.refresh_parked_continuation_tables(&self.session_table);
            }
        }
        Ok(())
    }

    // -- machine lifecycle -------------------------------------------------
    //
    // Threading note: [`BindingTable`] holds `RootSlot(*mut *mut u8)` and
    // [`ExternalEnv`] holds raw slot addresses. Both the table and the
    // [`JitEffectMachine`] carry an `unsafe impl Send` justified by the
    // stowed-XOR-running discipline, so a whole `PersistentSession` can be moved
    // to another thread as long as exactly one thread owns it at a time — which
    // is what the repl does (the session is moved into a `spawn_blocking` turn
    // and returned out of it). The harness instead runs the deep-recursion turn
    // on a fresh big-stack thread while keeping the rest of the session on the
    // caller's frame: it [`Self::lease_machine`]s the machine over with the
    // accumulated table and lets the [`MachineLease`] restore it on `Drop`.
    // `add_function` (which needs the raw-pointer env) therefore always happens
    // on the thread that owns the session.

    /// Bootstrap the resident machine from `expr`/`table` if it is not already
    /// live (a session machine, so its heap is retained across turns). No-op when
    /// already bootstrapped. The bootstrap `expr` seeds the machine's ConTags; it
    /// is NOT run (the harness bootstrap seed, and the repl bind path, both
    /// bootstrap-then-fragment without running the seed).
    pub fn bootstrap_if_needed(
        &mut self,
        expr: &CoreExpr,
        table: &DataConTable,
    ) -> Result<(), JitError> {
        self.ensure_machine_reusable()?;
        if self.engine_kind != EngineKind::Core {
            return Err(JitError::InvalidSuspensionState(
                "a prepared-route session bootstraps from its first turn's prepared program, not Core",
            ));
        }
        if self.machine.is_none() {
            self.machine = Some(ResidentEngine::Core(JitEffectMachine::compile_session(
                expr,
                table,
                self.nursery_size,
            )?));
        }
        Ok(())
    }

    /// Install a prepared turn's program on the prepared route, bootstrapping
    /// the machine from it when this is the session's first turn. Every
    /// global the program declares resolves to a live prepared binding by
    /// the identity recorded when that binding was made.
    pub fn install_prepared(
        &mut self,
        prepared: PreparedProgram,
    ) -> Result<tidepool_codegen::prepared_program::ProgramId, PreparedRuntimeError> {
        match self.machine.as_mut() {
            None if self.engine_kind == EngineKind::Prepared => {
                let (engine, program) = PreparedEngine::bootstrap(prepared)?;
                self.machine = Some(ResidentEngine::Prepared(engine));
                Ok(program)
            }
            Some(ResidentEngine::Prepared(engine)) => {
                engine.install(prepared, &self.bindings, &self.binding_index)
            }
            _ => Err(PreparedRuntimeError::WrongEngine),
        }
    }

    /// The live prepared bindings a later turn compiles against: each one's
    /// import identity and the generation it was bound at, declared to the
    /// extractor as retained generations so the projection links against the
    /// binding instead of recompiling a body it does not have.
    #[must_use]
    pub fn prepared_retained(&self) -> Vec<(SymbolIdentity, u64)> {
        let mut retained = self.binding_index.prepared_retained();
        if let Some(ResidentEngine::Prepared(engine)) = self.machine.as_ref() {
            // Package tops the machine already carries compiled code for.
            // A value binding wins any collision: the value plane's own
            // generation is what a turn that reads `x` must link against.
            let bound: std::collections::BTreeSet<&SymbolIdentity> =
                retained.iter().map(|(identity, _)| identity).collect();
            let exported: Vec<(SymbolIdentity, u64)> = engine
                .code_export_retentions()
                .filter(|(identity, _)| !bound.contains(identity))
                .collect();
            retained.extend(exported);
        }
        retained
    }

    /// How many package tops this session's machine can hand a later turn
    /// instead of recompiling; `None` on the Core route or before bootstrap.
    #[must_use]
    pub fn code_export_count(&self) -> Option<usize> {
        match self.machine.as_ref()? {
            ResidentEngine::Core(_) => None,
            ResidentEngine::Prepared(engine) => Some(engine.code_export_count()),
        }
    }

    /// Move the resident machine out onto a [`MachineLease`] (to run a turn on
    /// a fresh big-stack eval thread — the machine is `Send`, the rest of the
    /// session is not). The lease mutably borrows this session for its whole
    /// lifetime and restores the SAME machine into it on `Drop` — there is no
    /// way to reach the emptied-slot state through a public method, and no way
    /// to hand the lease's machine to a different session's restore (the lease
    /// borrows the session it took from and nothing else). Panics if the
    /// machine is not bootstrapped or is already leased.
    pub fn lease_machine(&mut self) -> MachineLease<'_> {
        #[allow(
            clippy::expect_used,
            reason = "machine present (idle or suspended) before a turn"
        )]
        let machine = self
            .machine
            .take()
            .expect("machine present (idle or suspended) before a turn");
        MachineLease {
            session: self,
            machine: Some(machine),
        }
    }

    // There is deliberately no `drop_machine`: tearing a session down means
    // dropping the whole `PersistentSession` (which frees the heap through the
    // machine's own `Drop`). A method that emptied the machine slot in place
    // would leave a live session whose value-plane `RootSlot`s all dangle — a
    // state with no legitimate use and no way to detect from the outside.

    // -- fragment preparation (CALLER thread; touches the `!Send` env) ------

    /// Add `expr` as a fragment against the ACCUMULATED session table, seeded by
    /// `env`, minting the fragment name `"{name_hint}_{turn}"`. Returns the
    /// `FuncId` a later run drives. Merge the turn's table via [`Self::merge_table`]
    /// first. Requires the machine bootstrapped.
    pub fn add_fragment_session(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        env: &ExternalEnv,
    ) -> Result<FuncId, JitError> {
        self.ensure_machine_reusable()?;
        self.turn_counter += 1;
        let frag_name = format!("{name_hint}_{}", self.turn_counter);
        let PersistentSession {
            machine,
            session_table,
            ..
        } = self;
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before add_fragment_session"
        )]
        let machine = machine
            .as_mut()
            .and_then(ResidentEngine::core_mut)
            .expect("machine bootstrapped before add_fragment_session");
        machine.add_function(&frag_name, expr, session_table, env)
    }

    /// Nested-child sibling of [`Self::add_fragment_session`], minting the name
    /// `"{name_hint}_child_{turn}"`.
    pub fn add_child_fragment_session(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        env: &ExternalEnv,
    ) -> Result<FuncId, JitError> {
        self.ensure_machine_reusable()?;
        self.turn_counter += 1;
        let frag_name = format!("{name_hint}_child_{}", self.turn_counter);
        let PersistentSession {
            machine,
            session_table,
            ..
        } = self;
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before add_child_fragment_session"
        )]
        let machine = machine
            .as_mut()
            .and_then(ResidentEngine::core_mut)
            .expect("machine bootstrapped before add_child_fragment_session");
        machine.add_function(&frag_name, expr, session_table, env)
    }

    /// Add `expr` as a fragment against an EXTERNAL per-turn table (not the
    /// accumulated one) — the repl's plain-expression path, where a standalone
    /// turn carries its own table. Requires the machine bootstrapped.
    pub fn add_fragment_with_table(
        &mut self,
        name_hint: &str,
        expr: &CoreExpr,
        run_table: &DataConTable,
        env: &ExternalEnv,
    ) -> Result<FuncId, JitError> {
        self.ensure_machine_reusable()?;
        self.turn_counter += 1;
        let frag_name = format!("{name_hint}_{}", self.turn_counter);
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before add_fragment_with_table"
        )]
        let machine = self
            .machine
            .as_mut()
            .and_then(ResidentEngine::core_mut)
            .expect("machine bootstrapped before add_fragment_with_table");
        machine.add_function(&frag_name, expr, run_table, env)
    }

    // -- capacity-one registry façade --------------------------------------

    fn track_value_outcome(&mut self, outcome: ParkedOutcome) -> SuspendableOutcome {
        match outcome {
            ParkedOutcome::CompletedValue(value) => {
                self.active_continuation = None;
                SuspendableOutcome::Completed(value)
            }
            ParkedOutcome::Suspended {
                id,
                request,
                has_live_payload,
            } => {
                self.active_continuation = Some(id);
                SuspendableOutcome::Suspended {
                    request,
                    has_live_payload,
                }
            }
            other => panic!("plain registry run returned the wrong completion policy: {other:?}"),
        }
    }

    fn track_binding_outcome(&mut self, outcome: ParkedOutcome) -> SuspendableOutcome {
        match outcome {
            ParkedOutcome::CompletedBinding { value, root } => {
                self.active_continuation = None;
                self.last_bound_root.0 = Some(root);
                SuspendableOutcome::Completed(value)
            }
            ParkedOutcome::Suspended {
                id,
                request,
                has_live_payload,
            } => {
                self.active_continuation = Some(id);
                SuspendableOutcome::Suspended {
                    request,
                    has_live_payload,
                }
            }
            other => panic!("binding registry run returned the wrong policy: {other:?}"),
        }
    }

    fn track_project_outcome(&mut self, outcome: ParkedOutcome) -> Suspendable<Vec<RootSlot>> {
        match outcome {
            ParkedOutcome::CompletedProject { roots } => {
                self.active_continuation = None;
                Suspendable::Completed(roots)
            }
            ParkedOutcome::Suspended {
                id,
                request,
                has_live_payload,
            } => {
                self.active_continuation = Some(id);
                Suspendable::Suspended {
                    request,
                    has_live_payload,
                }
            }
            other => panic!("project registry run returned the wrong policy: {other:?}"),
        }
    }

    fn track_render_outcome(&mut self, outcome: ParkedOutcome) -> Suspendable<(RootSlot, Value)> {
        match outcome {
            ParkedOutcome::CompletedRender { root, rendered } => {
                self.active_continuation = None;
                Suspendable::Completed((root, rendered))
            }
            ParkedOutcome::Suspended {
                id,
                request,
                has_live_payload,
            } => {
                self.active_continuation = Some(id);
                Suspendable::Suspended {
                    request,
                    has_live_payload,
                }
            }
            other => panic!("render registry run returned the wrong policy: {other:?}"),
        }
    }

    fn resume_active<O, H>(
        &mut self,
        handlers: &mut H,
        captured: &O,
        input: ResumeInput,
    ) -> Result<ParkedOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        self.ensure_machine_reusable()?;
        let id = self
            .active_continuation
            .ok_or(JitError::InvalidSuspensionState(
                "resume requires an active continuation",
            ))?;
        let machine = self
            .machine
            .as_mut()
            .and_then(ResidentEngine::core_mut)
            .ok_or(JitError::InvalidSuspensionState(
                "resident machine is absent during resume",
            ))?;
        let resumed = machine.resume_continuation(id, handlers, captured, input);
        // Some errors reject the input before consuming the frame (for example
        // a non-NF bridged answer); those remain retryable. Abort and failures
        // after re-entry consume the frame, so keep the capacity-one façade in
        // sync with the registry even though there is no successful outcome to
        // pass through `track_*_outcome`.
        if resumed.is_err() && machine.parked_realm(id).is_none() {
            self.active_continuation = None;
        }
        resumed
    }

    /// Run the resident machine's ORIGINAL entry (the seed compiled by
    /// [`Self::bootstrap_if_needed`]) to its first boundary. The repl's first
    /// bare-expression turn, where the seed IS the program. `run_table` is the
    /// seed's table; [`Self::resume_with_table`] re-enters with the same one.
    /// Requires the machine bootstrapped.
    pub fn run_entry<O, H>(
        &mut self,
        run_table: &DataConTable,
        handlers: &mut H,
        captured: &O,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        self.ensure_machine_reusable()?;
        assert!(
            self.active_continuation.is_none(),
            "resume the active turn first"
        );
        let effect_policy = self.effect_policy;
        let live_payload = self.live_payload;
        #[allow(clippy::expect_used, reason = "machine bootstrapped before run_entry")]
        let machine = self
            .machine
            .as_mut()
            .and_then(ResidentEngine::core_mut)
            .expect("machine bootstrapped before run_entry");
        let run = SuspensionRun::main(run_table, effect_policy, RealmId::ROOT)
            .with_live_payload(live_payload);
        let outcome = machine.run_until_suspension(run, handlers, captured)?;
        Ok(self.track_value_outcome(outcome))
    }

    /// Drive a fragment (already added) to its first boundary against an
    /// EXTERNAL table. The repl plain-expression path; resumed by
    /// [`Self::resume_with_table`].
    pub fn run_funcid_with_table<O, H>(
        &mut self,
        func_id: FuncId,
        run_table: &DataConTable,
        handlers: &mut H,
        captured: &O,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        self.ensure_machine_reusable()?;
        assert!(
            self.active_continuation.is_none(),
            "resume the active turn first"
        );
        let effect_policy = self.effect_policy;
        let live_payload = self.live_payload;
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before run_funcid_with_table"
        )]
        let machine = self
            .machine
            .as_mut()
            .and_then(ResidentEngine::core_mut)
            .expect("machine bootstrapped before run_funcid_with_table");
        let run = SuspensionRun::fragment(
            func_id,
            run_table,
            effect_policy,
            RealmId::ROOT,
            ParkKind::Plain,
        )
        .with_live_payload(live_payload);
        let outcome = machine.run_until_suspension(run, handlers, captured)?;
        Ok(self.track_value_outcome(outcome))
    }

    /// Re-enter a turn suspended by [`Self::run_entry`] or
    /// [`Self::run_funcid_with_table`], against the SAME external `run_table`
    /// the run used (the suspend request and the completed value are both
    /// bridged against it).
    pub fn resume_with_table<O, H>(
        &mut self,
        _run_table: &DataConTable,
        handlers: &mut H,
        captured: &O,
        input: ResumeInput,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let outcome = self.resume_active(handlers, captured, input)?;
        Ok(self.track_value_outcome(outcome))
    }

    /// Drive a fragment (already added) to its first boundary against the
    /// ACCUMULATED session table. The repl's effectful session-reference path;
    /// resumed by [`Self::resume_session`].
    pub fn run_funcid_session<O, H>(
        &mut self,
        func_id: FuncId,
        handlers: &mut H,
        captured: &O,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        self.ensure_machine_reusable()?;
        assert!(
            self.active_continuation.is_none(),
            "resume the active turn first"
        );
        let effect_policy = self.effect_policy;
        let live_payload = self.live_payload;
        let PersistentSession {
            machine,
            session_table,
            ..
        } = self;
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before run_funcid_session"
        )]
        let machine = machine
            .as_mut()
            .and_then(ResidentEngine::core_mut)
            .expect("machine bootstrapped before run_funcid_session");
        let run = SuspensionRun::fragment(
            func_id,
            session_table,
            effect_policy,
            RealmId::ROOT,
            ParkKind::Plain,
        )
        .with_live_payload(live_payload);
        let outcome = machine.run_until_suspension(run, handlers, captured)?;
        Ok(self.track_value_outcome(outcome))
    }

    /// Re-enter a turn suspended by [`Self::run_funcid_session`], against the
    /// accumulated session table.
    pub fn resume_session<O, H>(
        &mut self,
        handlers: &mut H,
        captured: &O,
        input: ResumeInput,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let outcome = self.resume_active(handlers, captured, input)?;
        Ok(self.track_value_outcome(outcome))
    }

    /// Run a PURE fragment (no effect tree) to a value against the accumulated
    /// table. The repl's pure session-reference path (`run_fragment_pure`).
    /// Pure means no effects, hence no `Ask`, hence no suspension — this is the
    /// one run entry with no `resume_*` sibling.
    pub fn run_funcid_pure(&mut self, func_id: FuncId) -> Result<Value, JitError> {
        self.ensure_machine_reusable()?;
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before run_funcid_pure"
        )]
        let machine = self
            .machine
            .as_mut()
            .and_then(ResidentEngine::core_mut)
            .expect("machine bootstrapped before run_funcid_pure");
        machine.run_fragment_pure(func_id)
    }

    /// Drive a fragment (already added) as an effectful VALUE BIND against the
    /// accumulated table: run the effect tree and, on completion, deep-force
    /// (`forced` → Tier-0 data) or tenure-as-is (Tier-1 closure) and register the
    /// persistent root. Read the tenured root with [`Self::take_bound_root`]; a
    /// suspension tenures NOTHING yet, and lands its value on
    /// [`Self::resume_bind`] with the SAME `forced` flag.
    pub fn bind_funcid<O, H>(
        &mut self,
        func_id: FuncId,
        handlers: &mut H,
        captured: &O,
        forced: bool,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        self.ensure_machine_reusable()?;
        assert!(
            self.active_continuation.is_none(),
            "resume the active turn first"
        );
        let effect_policy = self.effect_policy;
        let live_payload = self.live_payload;
        let PersistentSession {
            machine,
            session_table,
            ..
        } = self;
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before bind_funcid"
        )]
        let machine = machine
            .as_mut()
            .and_then(ResidentEngine::core_mut)
            .expect("machine bootstrapped before bind_funcid");
        let run = SuspensionRun::fragment(
            func_id,
            session_table,
            effect_policy,
            RealmId::ROOT,
            ParkKind::Binding { forced },
        )
        .with_live_payload(live_payload);
        let outcome = machine.run_until_suspension(run, handlers, captured)?;
        Ok(self.track_binding_outcome(outcome))
    }

    /// Re-enter a turn suspended by [`Self::bind_funcid`]. `forced` is the SAME
    /// flag the run carried — the caller threads it across the suspension on the
    /// binder metadata it is already holding.
    pub fn resume_bind<O, H>(
        &mut self,
        handlers: &mut H,
        captured: &O,
        input: ResumeInput,
        forced: bool,
    ) -> Result<SuspendableOutcome, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let _ = forced;
        let outcome = self.resume_active(handlers, captured, input)?;
        Ok(self.track_binding_outcome(outcome))
    }

    /// Multi-binder sibling of [`Self::bind_funcid`]: on completion, project
    /// `n_fields` tuple components and tenure each as a separate root.
    ///
    /// Completion IS those roots. A projection has no result value of its own,
    /// and the outcome type says so — there is no bridged tuple to render and
    /// none to accidentally render.
    pub fn bind_funcid_projected<O, H>(
        &mut self,
        func_id: FuncId,
        handlers: &mut H,
        captured: &O,
        n_fields: usize,
    ) -> Result<Suspendable<Vec<RootSlot>>, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        self.ensure_machine_reusable()?;
        assert!(
            self.active_continuation.is_none(),
            "resume the active turn first"
        );
        let effect_policy = self.effect_policy;
        let live_payload = self.live_payload;
        let n_fields = NonZeroUsize::new(n_fields).ok_or(JitError::EmptyProjection)?;
        let PersistentSession {
            machine,
            session_table,
            ..
        } = self;
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before bind_funcid_projected"
        )]
        let machine = machine
            .as_mut()
            .and_then(ResidentEngine::core_mut)
            .expect("machine bootstrapped before bind_funcid_projected");
        let run = SuspensionRun::fragment(
            func_id,
            session_table,
            effect_policy,
            RealmId::ROOT,
            ParkKind::Project { n_fields },
        )
        .with_live_payload(live_payload);
        let outcome = machine.run_until_suspension(run, handlers, captured)?;
        Ok(self.track_project_outcome(outcome))
    }

    /// Re-enter a turn suspended by [`Self::bind_funcid_projected`]. `n_fields`
    /// is the SAME arity the run carried (the caller is holding the binder
    /// vector across the suspension).
    pub fn resume_bind_projected<O, H>(
        &mut self,
        handlers: &mut H,
        captured: &O,
        input: ResumeInput,
        n_fields: usize,
    ) -> Result<Suspendable<Vec<RootSlot>>, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let _ = n_fields;
        let outcome = self.resume_active(handlers, captured, input)?;
        Ok(self.track_project_outcome(outcome))
    }

    /// Bind-and-render sibling of [`Self::bind_funcid`]: run the fragment ONCE,
    /// binding field 0 (`field0_forced` → Tier-0 data) and rendering field 1 in
    /// the same run. Completion carries BOTH — field 0's tenured root and field
    /// 1's rendered value. The repl's bare-expression `it` path.
    pub fn bind_funcid_render<O, H>(
        &mut self,
        func_id: FuncId,
        handlers: &mut H,
        captured: &O,
        field0_forced: bool,
    ) -> Result<Suspendable<(RootSlot, Value)>, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        self.ensure_machine_reusable()?;
        assert!(
            self.active_continuation.is_none(),
            "resume the active turn first"
        );
        let effect_policy = self.effect_policy;
        let live_payload = self.live_payload;
        let PersistentSession {
            machine,
            session_table,
            ..
        } = self;
        #[allow(
            clippy::expect_used,
            reason = "machine bootstrapped before bind_funcid_render"
        )]
        let machine = machine
            .as_mut()
            .and_then(ResidentEngine::core_mut)
            .expect("machine bootstrapped before bind_funcid_render");
        let run = SuspensionRun::fragment(
            func_id,
            session_table,
            effect_policy,
            RealmId::ROOT,
            ParkKind::Render { field0_forced },
        )
        .with_live_payload(live_payload);
        let outcome = machine.run_until_suspension(run, handlers, captured)?;
        Ok(self.track_render_outcome(outcome))
    }

    /// Re-enter a turn suspended by [`Self::bind_funcid_render`]. The
    /// field1-before-field0-tenure ordering that keeps an aliased
    /// `(it, toWire it)` intact lives in the machine's one `materialize`, so it
    /// holds identically here and on the first run.
    pub fn resume_bind_render<O, H>(
        &mut self,
        handlers: &mut H,
        captured: &O,
        input: ResumeInput,
        field0_forced: bool,
    ) -> Result<Suspendable<(RootSlot, Value)>, JitError>
    where
        O: OutputSink,
        H: DispatchEffect<O>,
    {
        let _ = field0_forced;
        let outcome = self.resume_active(handlers, captured, input)?;
        Ok(self.track_render_outcome(outcome))
    }

    /// Take the tenured root a completed [`Self::bind_funcid`] (or
    /// [`Self::resume_bind`]) stashed on the machine. `None` when no bind
    /// completed — the caller treats that as the infra error it is.
    ///
    /// Only the single-bind policy needs this, and NOT because of its outcome
    /// type's shape. `RootSlot` is `!Send`; bind is the one policy
    /// [`super::ResidentSession`] drives, and it does so through
    /// `on_eval_thread`, which returns the completion across a scoped-thread
    /// join. A slot riding out inline does not compile there — stashing it
    /// inside the (`Send`-blessed) machine is what carries it across. The
    /// projected and render policies return their roots inline only because
    /// nothing drives them across a thread; their sole caller is this
    /// single-threaded repl path.
    ///
    /// Removing this was attempted and reverted: it requires
    /// `unsafe impl Send for RootSlot`, a standalone soundness claim on a raw
    /// pointer rather than a refactor.
    pub fn take_bound_root(&mut self) -> Option<RootSlot> {
        self.last_bound_root.0.take()
    }

    /// Whether the machine currently holds a stowed continuation (a turn
    /// suspended at an `Ask` and has not been resumed or aborted).
    pub fn is_suspended(&self) -> bool {
        self.active_continuation.is_some()
    }

    // -- value-plane bookkeeping (delegating over the two planes) ----------

    /// Build the [`ExternalEnv`] a later fragment consults at a `Var`-miss:
    /// the `SessionVarId → RootSlot` of every live binding `referenced`
    /// names — typically `tidepool_repr::free_vars(&fragment)`. Not every
    /// live binding: a binding absent from `referenced` still stays a GC
    /// root (registered at bind time, independent of this call) but is not
    /// seeded into this particular fragment's env.
    pub fn seed_external_env(&self, referenced: &[VarId]) -> ExternalEnv {
        self.bindings.seed_external_env(referenced)
    }

    /// Record a materialized value binding on the value plane.
    pub fn bind(&mut self, entry: BindingEntry) {
        self.binding_index.on_bind(&entry);
        self.bindings.bind(entry);
    }

    /// Module names of every live value binding — injected (`--inject-val`) AND
    /// so already-compiled fragments / closure captures keep resolving. Includes
    /// shadowed older gens.
    pub fn live_val_modules(&self) -> Vec<String> {
        self.binding_index.live_modules()
    }

    /// The CURRENT (newest) `Val.G<g>` module per still-live name — what a turn
    /// IMPORTS unqualified (excludes shadowed older gens, which would make a
    /// rebound name an ambiguous occurrence).
    pub fn current_val_modules(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .bindings
            .iter_current()
            .map(|(_, entry)| entry.module.module_name())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The current decl-plane module (`Lib.G<g>`), if any (also `None` when the
    /// session has no decl plane at all).
    pub fn current_lib_module(&self) -> Option<SessionModule> {
        self.lib.as_ref().and_then(|l| l.current_module())
    }

    /// Scoped [`Self::current_lib_module`]: the `Lib.G<g>` module at `scope`'s
    /// OWN tip — the module a turn compiled in that scope imports, and the head
    /// of a re-export chain that already runs up through its ancestors.
    /// `current_lib_module() == current_lib_module_in(ScopeId::ROOT)`.
    pub fn current_lib_module_in(&self, scope: ScopeId) -> Option<SessionModule> {
        self.lib.as_ref().and_then(|l| l.current_module_in(scope))
    }

    #[must_use]
    pub fn next_lib_module(&self) -> Option<SessionModule> {
        self.lib.as_ref().map(SessionLib::next_module)
    }

    /// Snapshot the exact source-side environment visible from `scope` so a
    /// caller can release its machine borrow before invoking GHC. Returns
    /// `None` for a dead scope or a session without a declaration/include
    /// plane.
    pub fn compile_view_in(&self, scope: ScopeId) -> Option<SessionCompileView> {
        if !self.scopes.is_live(scope) {
            return None;
        }
        let lib = self.lib.as_ref()?;
        let visible_values = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .map(|(_, entry)| entry.module)
            .collect();
        let injected_values = self.bindings.live_modules().collect();
        let mut shadowing = lib
            .current_declarations_in(scope)
            .into_iter()
            .map(|(item, _)| item)
            .collect::<Vec<_>>();
        shadowing.extend(
            self.bindings
                .iter_current_in(&self.scopes, scope)
                .into_iter()
                .map(|(name, _)| super::ExportItem::Value {
                    name: name.0.clone(),
                }),
        );
        Some(
            SessionCompileView {
                session: lib.session_id(),
                lexical_scope: scope,
                root: PathBuf::from(lib.include_dir()),
                persistent_imports: self.workbench_imports_in(scope),
                library: lib.current_module_in(scope),
                visible_values,
                injected_values,
                next_value_generation: self.val_gen.next(),
                shadowing,
                staged_hiding: Vec::new(),
            }
            .canonicalize(),
        )
    }

    /// Capture selected declaration heads from `scope` as an exact export
    /// surface. This is source/interface identity only; it acquires no live
    /// roots and creates no deployment registry entry.
    pub fn exact_exports_in(
        &self,
        scope: ScopeId,
        heads: &[&str],
    ) -> Result<ExactExportSurface, ExactExportError> {
        if !self.scopes.is_live(scope) {
            return Err(ExactExportError::DeadScope(scope));
        }
        let lib = self
            .lib
            .as_ref()
            .ok_or(ExactExportError::NoDeclarationPlane)?;
        lib.exact_exports_in(scope, heads)
    }

    /// The decl-plane include directory (where `Lib.G<g>.hs` modules live), for
    /// a later turn's compile search path. `None` when the session has no decl
    /// plane.
    /// Move the decl plane OUT (machine rotation, one-session living
    /// structure): the plane is SOURCE-side state (gen modules on disk +
    /// the in-memory decl log), independent of any machine's heap, so it
    /// transfers wholesale into a freshly-built session while the old
    /// machine (and its value plane, whose roots die with its heap) drops.
    /// KNOWN EDGE: a gen module that imports `Val.G<g>` (a decl rendered
    /// while value binds were live) will fail its next recompile after the
    /// transfer with an ordinary module-not-found — legible, not silent.
    pub fn take_lib(&mut self) -> Option<SessionLib> {
        self.lib.take()
    }

    pub fn lib_include_dir(&self) -> Option<&Path> {
        self.lib.as_ref().map(|l| l.include_dir())
    }

    /// Define decl text(s) scoped against live session values: the current
    /// `Val.G<g>` per still-live name are imported unqualified, every live
    /// `Val.G<g>` is injected for validation. The decl-plane analogue of GHCi
    /// seeing earlier bindings from a new top-level definition.
    pub fn define_scoped(&mut self, decl_texts: &[&str]) -> Result<Generation, SessionError> {
        self.define_scoped_in(ScopeId::ROOT, decl_texts)
    }

    /// Scoped [`Self::define_scoped`]: append to `scope`'s own decl tip,
    /// validated against the value bindings VISIBLE at `scope` (its frame plus
    /// every ancestor's). `define_scoped(d) == define_scoped_in(ScopeId::ROOT,
    /// d)`.
    ///
    /// Injection stays the FULL live set — `--inject-val` only has to make the
    /// referenced `Val.G<g>` modules findable, and restricting it by scope
    /// would buy nothing while risking a missing module for a shadowed gen.
    /// Visibility is decided by the IMPORT list, which is scoped.
    pub fn define_scoped_in(
        &mut self,
        scope: ScopeId,
        decl_texts: &[&str],
    ) -> Result<Generation, SessionError> {
        self.define_scoped_with_imports_in(scope, decl_texts, &SourceImports::new())
    }

    /// Scoped declaration commit with frontend-owned persistent imports.
    /// Trusted imports participate in this declaration but are not recorded as
    /// user-authored state; callers provide them again for later turns.
    pub fn define_scoped_with_imports_in(
        &mut self,
        scope: ScopeId,
        decl_texts: &[&str],
        external: &SourceImports,
    ) -> Result<Generation, SessionError> {
        self.commit_declarations_in(scope, decl_texts, external)
            .map(|receipt| receipt.generation)
    }

    /// Render and validate the exact next declaration module without changing
    /// the live log, scope tip, recovery manifest, or value plane.
    pub fn stage_declarations_in(
        &self,
        scope: ScopeId,
        receipt: &super::DeclarationReceipt,
        external: &SourceImports,
    ) -> Result<super::StagedDeclaration, SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let mut persistent_imports = external.clone();
        persistent_imports.extend(&self.workbench_imports_in(scope));
        #[allow(clippy::expect_used, reason = "decl plane present")]
        let lib = self.lib.as_ref().expect("decl plane present");
        let replaced_names = receipt
            .items
            .iter()
            .flat_map(super::ExportItem::all_names)
            .collect::<Vec<_>>();
        let visible_values = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .filter(|(name, _)| {
                !replaced_names
                    .iter()
                    .any(|replaced| replaced == &name.0.as_str())
            })
            .map(|(_, entry)| (entry.id, entry.module.module_name()))
            .collect::<Vec<_>>();
        let mut import_modules = visible_values
            .iter()
            .map(|(_, module)| module.clone())
            .collect::<Vec<_>>();
        import_modules.sort();
        import_modules.dedup();
        lib.stage_batch_with_receipt_and_vals_in(
            scope,
            &persistent_imports,
            receipt,
            &import_modules,
            &self.live_val_modules(),
        )
        .map(|staged| staged.with_visible_values(visible_values))
    }

    /// Adopt a declaration candidate which this session already rendered and
    /// validated. The opaque candidate carries its normalized source, imports,
    /// and declaration/value environment; this entry point only accepts it
    /// while that exact live environment still exists.
    pub fn adopt_staged_declaration_in(
        &mut self,
        staged: super::StagedDeclaration,
    ) -> Result<DeclarationPlaneCommit, SessionError> {
        let scope = staged.scope();
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let mut replaced_names: Vec<String> = staged
            .items()
            .iter()
            .flat_map(super::ExportItem::all_names)
            .map(str::to_owned)
            .collect();
        replaced_names.sort();
        replaced_names.dedup();
        let mut evicted_values: Vec<String> = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .filter(|(name, _)| replaced_names.iter().any(|replaced| replaced == &name.0))
            .map(|(name, _)| name.0.clone())
            .collect();
        evicted_values.sort();
        let visible_values = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .filter(|(name, _)| !replaced_names.iter().any(|replaced| replaced == &name.0))
            .map(|(_, entry)| (entry.id, entry.module.module_name()))
            .collect::<Vec<_>>();
        let captured_values = visible_values
            .iter()
            .map(|(id, _)| id.var())
            .collect::<Vec<_>>();
        let items = staged.items().to_vec();
        let generation = self
            .lib
            .as_mut()
            .ok_or(SessionError::StaleStagedDeclaration)?
            .adopt_staged_batch_with_receipt_and_vals_in(staged, &visible_values)?;
        self.bindings.preserve_observations(&captured_values);
        for name in &replaced_names {
            self.bindings.remove_current_in(scope, name);
        }
        Ok(DeclarationPlaneCommit {
            generation,
            module: SessionModule::lib(generation),
            items,
            evicted_values,
        })
    }

    pub fn discard_staged_declaration(&self, staged: &super::StagedDeclaration) {
        if let Some(lib) = &self.lib {
            lib.discard_staged(staged);
        }
    }

    /// Retract `name` from the decl plane (its binding migrated to the value
    /// plane). No-op when `name` is not a current decl head.
    pub fn retract(&mut self, name: &str) -> Result<(), SessionError> {
        self.retract_in(ScopeId::ROOT, name)
    }

    /// Scoped [`Self::retract`]: retract `name` from `scope`'s decl tip only.
    /// `retract(n) == retract_in(ScopeId::ROOT, n)`.
    ///
    /// A name lives in at most one plane per scope, so a CHILD binding
    /// `helper` on the value plane must not retract the PARENT's decl-plane
    /// `helper` — the parent's name is still the parent's, and nothing ever
    /// walks downward.
    pub fn retract_in(&mut self, scope: ScopeId, name: &str) -> Result<(), SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        match self.lib.as_mut() {
            Some(lib) => lib.retract_in(scope, name),
            None => Ok(()),
        }
    }

    /// Retract a set of declaration heads through one durable declaration
    /// generation. Used by set materialization so a later name cannot fail
    /// after an earlier name has already entered the value plane.
    fn retract_many_in(&mut self, scope: ScopeId, names: &[String]) -> Result<(), SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        match self.lib.as_mut() {
            Some(lib) => lib.retract_many_in(scope, names),
            None => Ok(()),
        }
    }

    // -- scopes --------------------------------------------------------------

    /// Mint a fresh child scope of `parent`. `None` if `parent` is not live
    /// (never minted, or already retired) — a scope is never born under a dead
    /// ancestor.
    ///
    /// This is also where both planes freeze the new scope's inherited
    /// environment. The declaration plane captures its parent's generation;
    /// the binding plane captures an immutable name-to-value tip with root
    /// leases. Capturing both here prevents parent or sibling progress between
    /// mint and first use from leaking into the child.
    pub fn mint_scope(&mut self, parent: ScopeId) -> Option<ScopeId> {
        let child = self.scopes.mint_child(parent)?;
        if let Some(lib) = self.lib.as_mut() {
            let inherited = lib.scope_tip(parent);
            lib.seed_scope(child, inherited);
        }
        self.bindings.seed_scope(&self.scopes, parent, child);
        Some(child)
    }

    /// The immutable inherited value-binding tip captured for `scope`.
    #[must_use]
    pub fn binding_tip_id(&self, scope: ScopeId) -> Option<BindingTipId> {
        self.bindings.tip_id(scope)
    }

    /// Mint a fresh lexical root with an empty declaration and value view.
    ///
    /// This is the fresh-actor boundary: unlike [`Self::mint_scope`], it does
    /// not seed a declaration tip from another scope and its binding lookup
    /// chain never reaches [`ScopeId::ROOT`]. Exact program-image facades are
    /// added later as explicit source imports rather than ambient ancestry.
    pub fn mint_isolated_scope(&mut self) -> ScopeId {
        self.scopes.mint_isolated()
    }

    /// The session's one scope forest — read by both planes for their lookup
    /// walks. There is no `_mut` sibling on purpose: minting and retiring are
    /// the only writes, and both go through this type so the value plane's
    /// frames and roots are released in the same step as the tree edge.
    pub fn scope_tree(&self) -> &ScopeTree {
        &self.scopes
    }

    /// Record a materialized value binding in `scope`'s frame.
    /// `bind(e) == bind_in(ScopeId::ROOT, e)`.
    ///
    /// Rejects a dead `scope` (never minted, or already retired) BEFORE
    /// touching the binding table: a binding written under a dead scope
    /// would sit in a frame no lookup chain ever walks and
    /// [`Self::retire_scope`] can never drain — for a mounted persistent
    /// root, a permanent GC root by construction. Every caller must check
    /// liveness before consuming whatever custody transfer led here (a
    /// [`super::resident::RootCustody`] or an adopted
    /// [`ValueHandle`](tidepool_codegen::suspension::ValueHandle)) — this
    /// check is the backstop, not the first line, since `bind_in` failing here
    /// is too late to return an adopted root to the machine's registry.
    pub fn bind_in(&mut self, scope: ScopeId, entry: BindingEntry) -> Result<(), SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        self.binding_index.on_bind(&entry);
        self.bindings.bind_in(scope, entry);
        Ok(())
    }

    /// Atomically move `entry.name` from this scope's declaration plane to its
    /// materialized value plane.  Durable retraction is the commit point: if
    /// it fails, the binding table is untouched and the caller must report the
    /// failure rather than a successful bind.
    pub fn bind_replacing_decl_in(
        &mut self,
        scope: ScopeId,
        entry: BindingEntry,
    ) -> Result<ValuePlaneCommit, SessionError> {
        let receipt = self.bind_replacing_decls_in(scope, vec![entry])?;
        #[allow(clippy::expect_used, reason = "one entry yields one receipt")]
        Ok(receipt
            .bindings
            .into_iter()
            .next()
            .expect("one materialization receipt"))
    }

    /// Publish a compiler-typed alias of an already registered binding root.
    /// The slot belongs to the source binding, so a failed declaration retract
    /// must never pass it through the new-root cleanup path used by a bind.
    pub(crate) fn publish_alias_in(
        &mut self,
        scope: ScopeId,
        entry: BindingEntry,
        source: SessionVarId,
    ) -> Result<ValuePlaneCommit, SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let name = entry.name.0.clone();
        self.retract_many_in(scope, std::slice::from_ref(&name))?;
        let receipt = ValuePlaneCommit {
            name,
            module: entry.module,
        };
        // Built from `entry` BEFORE the fallible `bind_alias_in` call
        // consumes it, but only indexed after that call actually succeeds
        // (below) -- an alias whose bind never happened must never appear
        // in the index either.
        let record = BindRecord::of(&entry);
        #[allow(
            clippy::expect_used,
            reason = "source liveness and identity are checked by the sole caller, publish_captured_alias_in, and retract_many_in above touches only the decl plane, never source's value-plane entry"
        )]
        let (_, expired) = self
            .bindings
            .bind_alias_in(scope, entry, source)
            .expect("source and alias identity validated before declaration retraction");
        self.binding_index.on_bind_record(&record);
        self.release_binding_roots(expired);
        Ok(receipt)
    }

    /// Root-scope [`Self::bind_replacing_decl_in`].
    pub fn bind_replacing_decl(
        &mut self,
        entry: BindingEntry,
    ) -> Result<ValuePlaneCommit, SessionError> {
        self.bind_replacing_decl_in(ScopeId::ROOT, entry)
    }

    /// Atomically materialize a whole binding set.  The declaration-plane
    /// retraction is one durable generation for every affected name; only after
    /// it succeeds are entries installed in the value table.  On any failure,
    /// every produced root is retired before the error returns, so none becomes
    /// an unowned persistent GC root.
    pub fn bind_replacing_decls_in(
        &mut self,
        scope: ScopeId,
        entries: Vec<BindingEntry>,
    ) -> Result<MaterializationSetCommit, SessionError> {
        if !self.scopes.is_live(scope) {
            self.discard_unbound_entries(entries);
            return Err(SessionError::DeadScope(scope));
        }
        let names: Vec<String> = entries.iter().map(|entry| entry.name.0.clone()).collect();
        if let Err(error) = self.retract_many_in(scope, &names) {
            self.discard_unbound_entries(entries);
            return Err(error);
        }
        let bindings = entries
            .into_iter()
            .map(|entry| {
                let receipt = ValuePlaneCommit {
                    name: entry.name.0.clone(),
                    module: entry.module,
                };
                self.binding_index.on_bind(&entry);
                self.bindings.bind_in(scope, entry);
                receipt
            })
            .collect();
        Ok(MaterializationSetCommit { bindings })
    }

    /// Root-scope [`Self::bind_replacing_decls_in`].
    pub fn bind_replacing_decls(
        &mut self,
        entries: Vec<BindingEntry>,
    ) -> Result<MaterializationSetCommit, SessionError> {
        self.bind_replacing_decls_in(ScopeId::ROOT, entries)
    }

    /// Dispose roots that were produced by a completed materialization but
    /// could not enter the value plane. They are registered persistent roots,
    /// not ordinary Rust-owned allocations, so dropping `RootSlot` alone would
    /// leak them until session teardown.
    fn discard_unbound_entries(&mut self, entries: Vec<BindingEntry>) {
        if let Some(ResidentEngine::Prepared(engine)) = self.machine.as_mut() {
            // A prepared entry's root is its adopted handle.
            for entry in entries {
                if let BoundValue::Prepared { handle, .. } = entry.value {
                    engine.release(handle);
                }
            }
            return;
        }
        let Some(machine) = self.machine.as_mut().and_then(ResidentEngine::core_mut) else {
            // Unit-level bookkeeping fixtures can carry synthetic slots before
            // a machine exists; there is no root ledger to release there.
            return;
        };
        let count = entries.len();
        let roots_before = machine.persistent_roots_count();
        for entry in entries {
            machine.abandon_uncommitted_root(entry.value.root());
        }
        let roots_after = machine.persistent_roots_count();
        debug_assert_eq!(
            roots_before.checked_sub(roots_after),
            Some(count),
            "every failed materialization entry must release exactly one persistent root"
        );
    }

    /// Commit declarations, then remove any same-scope materialized names they
    /// replace.  Definition is fallible and happens first, so a failed module
    /// write/validation leaves the old value view intact.  Once it succeeds,
    /// frame removal is in-memory and infallible; the receipt is the single
    /// committed source of truth for frontend metadata updates.
    pub fn define_replacing_values_in(
        &mut self,
        scope: ScopeId,
        decl_texts: &[&str],
    ) -> Result<DeclarationPlaneCommit, SessionError> {
        self.commit_declarations_in(scope, decl_texts, &SourceImports::new())
    }

    /// Own declaration validation, capture retention, and value-name replacement
    /// as one commit. Neither frontend entry point can omit a lifetime step.
    fn commit_declarations_in(
        &mut self,
        scope: ScopeId,
        decl_texts: &[&str],
        external: &SourceImports,
    ) -> Result<DeclarationPlaneCommit, SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        #[allow(clippy::expect_used, reason = "decl plane present")]
        let lib = self.lib.as_ref().expect("decl plane present");
        let Some(receipt) = lib.declaration_receipt(decl_texts)? else {
            let generation = lib.scope_tip(scope);
            return Ok(DeclarationPlaneCommit {
                generation,
                module: SessionModule::lib(generation),
                items: Vec::new(),
                evicted_values: Vec::new(),
            });
        };
        self.commit_declaration_receipt_in(scope, &receipt, external)
    }

    /// Consume compiler-owned source facts through the same capture and
    /// value-replacement boundary as ordinary definitions.
    pub fn commit_declaration_receipt_in(
        &mut self,
        scope: ScopeId,
        receipt: &super::DeclarationReceipt,
        external: &SourceImports,
    ) -> Result<DeclarationPlaneCommit, SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let mut persistent_imports = external.clone();
        persistent_imports.extend(&self.workbench_imports_in(scope));
        let mut replaced_names: Vec<String> = receipt
            .items
            .iter()
            .flat_map(super::ExportItem::all_names)
            .map(str::to_owned)
            .collect();
        replaced_names.sort();
        replaced_names.dedup();
        // Built once and consulted by `.contains` instead of re-scanning
        // `replaced_names` per current binding below: this scope's current
        // frame can hold many live names, and it is scanned twice.
        let replaced_set: std::collections::HashSet<&str> =
            replaced_names.iter().map(String::as_str).collect();
        let mut evicted_values: Vec<String> = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .filter(|(name, _)| replaced_set.contains(name.0.as_str()))
            .map(|(name, _)| name.0.clone())
            .collect();
        evicted_values.sort();
        // The candidate declaration owns these names, so it must not import
        // their old Val modules unqualified while GHC validates it.  Keep them
        // injected: already-compiled fragments may still need their ifaces,
        // but they are not visible providers in this new source turn.
        let (captured_values, mut import_modules): (Vec<_>, Vec<_>) = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .filter(|(name, _)| !replaced_set.contains(name.0.as_str()))
            .map(|(_, entry)| (entry.id.var(), entry.module.module_name()))
            .unzip();
        import_modules.sort();
        import_modules.dedup();
        let inject_modules = self.live_val_modules();
        #[allow(clippy::expect_used, reason = "decl plane present")]
        let generation = self
            .lib
            .as_mut()
            .expect("decl plane present")
            .define_batch_with_receipt_and_vals_in(
                scope,
                &persistent_imports,
                receipt,
                &import_modules,
                &inject_modules,
            )?;
        // GHC may compile these declaration bodies later. Their exact imported
        // value environment must outlive that future use, including any saved
        // observations and the compiled slots those observations depend on.
        self.bindings.preserve_observations(&captured_values);
        for name in &replaced_names {
            self.bindings.remove_current_in(scope, name);
        }
        Ok(DeclarationPlaneCommit {
            generation,
            module: SessionModule::lib(generation),
            items: receipt.items.clone(),
            evicted_values,
        })
    }

    /// Root-scope [`Self::define_replacing_values_in`].
    pub fn define_replacing_values(
        &mut self,
        decl_texts: &[&str],
    ) -> Result<DeclarationPlaneCommit, SessionError> {
        self.define_replacing_values_in(ScopeId::ROOT, decl_texts)
    }

    /// Resolve `name` as seen FROM `scope`: local frame first, then each
    /// ancestor up to its lexical root.
    pub fn resolve_in(&self, scope: ScopeId, name: &str) -> Option<&BindingEntry> {
        self.bindings.resolve_in(&self.scopes, scope, name)
    }

    /// Scoped [`Self::current_val_modules`]: the `Val.G<g>` module per name
    /// VISIBLE at `scope` (child frames shadowing parent ones) — what a turn
    /// compiled in that scope imports unqualified.
    pub fn current_val_modules_in(&self, scope: ScopeId) -> Vec<String> {
        let mut v: Vec<String> = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .map(|(_, entry)| entry.module.module_name())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// How many names `scope`'s own frame currently binds (accounting class 3,
    /// per scope). `scope_binding_count(ScopeId::ROOT)` is the flat session's
    /// `current_val_modules`/`binding_names` population.
    pub fn scope_binding_count(&self, scope: ScopeId) -> usize {
        self.bindings.scope_binding_count(scope)
    }

    /// Number of persistent GC roots registered on the resident machine
    /// (accounting class 4 — the GC ROOT LEDGER, the witness that a retirement
    /// actually released what it claims). 0 before the machine bootstraps.
    pub fn persistent_roots_count(&self) -> usize {
        self.machine
            .as_ref()
            .map_or(0, ResidentEngine::persistent_roots_count)
    }

    /// Accounting class 2 — live value handles on the resident machine,
    /// whichever engine it runs. 0 before the machine bootstraps.
    #[must_use]
    pub fn value_handle_count(&self) -> usize {
        self.machine
            .as_ref()
            .map_or(0, ResidentEngine::value_handle_count)
    }

    /// Accounting class 1, root half — the stowed roots of parked frames,
    /// whichever engine. 0 before the machine bootstraps.
    #[must_use]
    pub fn stowed_roots_count(&self) -> usize {
        self.machine
            .as_ref()
            .map_or(0, ResidentEngine::stowed_roots_count)
    }

    /// Accounting class 1, frame half — the parked continuations, whichever
    /// engine. Always equal to [`Self::stowed_roots_count`] at quiescence.
    #[must_use]
    pub fn parked_count(&self) -> usize {
        self.machine
            .as_ref()
            .map_or(0, ResidentEngine::parked_count)
    }

    /// Retire `scope` and its whole subtree: drop each scope's value-plane
    /// frame and RELEASE the GC roots those bindings solely owned.
    ///
    /// Walks [`ScopeTree::retire`]'s deepest-first order so a child's frames
    /// are gone before its parent's, drains each frame
    /// ([`BindingTable::drain_scope`]), and for every drained entry applies the
    /// **sole-ownership rule**: its root is deregistered
    /// ([`JitEffectMachine::retire_scope_root`]) only when no OTHER live
    /// `BindingEntry` — in any scope, including the not-yet-drained ancestors
    /// of this same retirement — holds the same slot address, and no live
    /// `ValueHandle` still does.
    ///
    /// That rule is what makes the escaped-closure case safe: a value produced
    /// in a child and mounted into a PARENT-scope binding is still owned by
    /// that surviving entry when the child retires, so its root stays
    /// registered and its captured heap subgraph stays traced transitively.
    ///
    /// Retiring ROOT, or a scope that is already retired, is a no-op returning
    /// an all-zero receipt.
    ///
    /// # What this reclaims
    /// Releasing each solely-owned root invokes the machine's quiescent
    /// retirement collector. That pass compacts unreachable old space and
    /// sweeps unreachable external payloads while retaining storage reachable
    /// from every remaining root. The returned receipt accounts names and root
    /// registrations; it is not a byte-reclamation receipt.
    pub fn retire_scope(&mut self, scope: ScopeId) -> ScopeRetirement {
        let roots_before = self.persistent_roots_count();
        let doomed = self.scopes.retire(scope);
        let mut receipt = ScopeRetirement {
            scopes_retired: doomed.len(),
            bindings_retired: 0,
            roots_released: 0,
        };
        let mut retired = Vec::new();
        for dead in &doomed {
            retired.extend(self.bindings.drain_scope(*dead));
        }
        retired.extend(self.bindings.collect_observations());
        receipt.bindings_retired = retired.len();
        receipt.roots_released = self.release_binding_roots(retired);
        debug_assert_eq!(
            roots_before - self.persistent_roots_count(),
            receipt.roots_released,
            "retire_scope receipt must be witnessed by the GC root ledger",
        );
        receipt
    }

    fn workbench_imports_in(&self, scope: ScopeId) -> SourceImports {
        self.lib
            .as_ref()
            .map_or_else(SourceImports::new, |lib| lib.workbench_imports_in(scope))
    }
}

// ---------------------------------------------------------------------------
// MachineLease — the only way to move a session's machine onto another thread
// ---------------------------------------------------------------------------

/// An exclusive, scoped loan of a [`PersistentSession`]'s machine, minted by
/// [`PersistentSession::lease_machine`]. The lease mutably borrows the session
/// it came from for its entire lifetime, so the session's machine slot cannot
/// be observed or touched by anything else while the lease is outstanding, and
/// on `Drop` it restores EXACTLY the machine it took — never an arbitrary one,
/// and never into a different session. There is no public constructor and no
/// public field: the empty-slot state this replaces (the audited
/// `take_machine`/`restore_machine` pair) is not reachable through any safe
/// call, by construction rather than by convention.
pub struct MachineLease<'a> {
    session: &'a mut PersistentSession,
    machine: Option<ResidentEngine>,
}

// The exclusive-borrow guarantee this type exists for ("the session's machine
// slot cannot be observed or touched by anything else while the lease is
// outstanding") is a `&mut` the borrow checker already enforces — a
// `#[derive(Clone)]` could never actually compile against the `&'a mut
// PersistentSession` field as written, but a future refactor that swapped
// that field for something Clone-able (e.g. an `Rc`/raw pointer) would make
// the derive compile silently, losing the guarantee this pin exists to catch.
static_assertions::assert_not_impl_any!(MachineLease<'static>: Clone, Copy);

impl MachineLease<'_> {
    /// The leased machine and the session's accumulated constructor table, on
    /// loan together for a turn run on another thread. Panics if called after
    /// the lease's machine has somehow already been consumed — unreachable
    /// through this type's own API, kept as a `debug_assert`-strength backstop
    /// rather than an `unwrap` a reviewer has to re-verify by hand.
    pub fn parts(&mut self) -> (&mut ResidentEngine, &DataConTable) {
        #[allow(
            clippy::expect_used,
            reason = "lease holds its machine for its whole lifetime"
        )]
        let machine = self
            .machine
            .as_mut()
            .expect("lease holds its machine for its whole lifetime");
        let table = self.session.session_table();
        (machine, table)
    }
}

impl Drop for MachineLease<'_> {
    fn drop(&mut self) {
        if let Some(machine) = self.machine.take() {
            self.session.machine = Some(machine);
        }
    }
}

/// What a [`PersistentSession::retire_scope`] actually released — counts, not
/// booleans, so a caller can assert the accounting rather than trust it.
///
/// `roots_released` is the number of persistent GC roots deregistered, and it
/// is exactly the drop a caller must observe in
/// [`PersistentSession::persistent_roots_count`] across the call. It is `<=`
/// `bindings_retired`: a binding whose slot is still owned by a survivor (the
/// sole-ownership rule) retires its NAME without releasing its ROOT.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeRetirement {
    /// Scopes removed from the tree — the retired scope plus every live
    /// descendant.
    pub scopes_retired: usize,
    /// `live` entries evicted across all of those scopes' frames, shadowed
    /// older gens included.
    pub bindings_retired: usize,
    /// Persistent GC roots deregistered — the sole-owner subset of the above.
    pub roots_released: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_effect::dispatch::EffectContext;
    use tidepool_effect::error::EffectError;
    use tidepool_effect::Response;
    use tidepool_repr::{CoreFrame, Literal, PrimOpKind, TreeBuilder};

    const VAL_ID: tidepool_repr::DataConId = tidepool_repr::DataConId(10);
    const E_ID: tidepool_repr::DataConId = tidepool_repr::DataConId(11);
    const UNION_ID: tidepool_repr::DataConId = tidepool_repr::DataConId(12);
    const LEAF_ID: tidepool_repr::DataConId = tidepool_repr::DataConId(13);
    const NODE_ID: tidepool_repr::DataConId = tidepool_repr::DataConId(14);

    #[derive(Clone)]
    struct TestSink;

    impl OutputSink for TestSink {
        fn drain(&self) -> Vec<String> {
            Vec::new()
        }

        fn snapshot(&self) -> Vec<String> {
            Vec::new()
        }
    }

    struct NoDispatch;

    impl DispatchEffect<TestSink> for NoDispatch {
        fn dispatch(
            &mut self,
            _request: &Value,
            _cx: &EffectContext<'_, TestSink>,
        ) -> Result<Option<Response>, EffectError> {
            Ok(None)
        }
    }

    struct RespondAfterSuspend {
        calls: usize,
    }

    impl DispatchEffect<TestSink> for RespondAfterSuspend {
        fn dispatch(
            &mut self,
            _request: &Value,
            _cx: &EffectContext<'_, TestSink>,
        ) -> Result<Option<Response>, EffectError> {
            self.calls += 1;
            Ok((self.calls > 1).then(|| Response::Complete(Value::Lit(Literal::LitInt(10)))))
        }
    }

    fn effect_table() -> DataConTable {
        let mut table = DataConTable::new();
        for (id, name, qualified_name, arity) in [
            (VAL_ID, "Val", "Control.Monad.Freer.Val", 1),
            (E_ID, "E", "Control.Monad.Freer.E", 2),
            (UNION_ID, "Union", "Data.OpenUnion.Union", 2),
            (LEAF_ID, "Leaf", "Data.FTCQueue.Leaf", 1),
            (NODE_ID, "Node", "Data.FTCQueue.Node", 2),
        ] {
            table.insert(DataCon {
                id,
                name: name.to_owned(),
                tag: 0,
                rep_arity: arity,
                field_bangs: Vec::new(),
                qualified_name: Some(qualified_name.to_owned()),
                type_name: String::new(),
            });
        }
        table
    }

    fn suspending_expr() -> CoreExpr {
        let mut builder = TreeBuilder::new();
        let final_answer = builder.push(CoreFrame::Var(VarId(2)));
        let final_val = builder.push(CoreFrame::Con {
            tag: VAL_ID,
            fields: vec![final_answer],
        });
        let final_continuation = builder.push(CoreFrame::Lam {
            binder: VarId(2),
            body: final_val,
        });
        let final_leaf = builder.push(CoreFrame::Con {
            tag: LEAF_ID,
            fields: vec![final_continuation],
        });
        let second_tag = builder.push(CoreFrame::Lit(Literal::LitWord(0)));
        let second_request = builder.push(CoreFrame::Lit(Literal::LitInt(8)));
        let second_union = builder.push(CoreFrame::Con {
            tag: UNION_ID,
            fields: vec![second_tag, second_request],
        });
        let second_effect = builder.push(CoreFrame::Con {
            tag: E_ID,
            fields: vec![second_union, final_leaf],
        });
        let continuation = builder.push(CoreFrame::Lam {
            binder: VarId(1),
            body: second_effect,
        });
        let leaf = builder.push(CoreFrame::Con {
            tag: LEAF_ID,
            fields: vec![continuation],
        });
        let effect_tag = builder.push(CoreFrame::Lit(Literal::LitWord(0)));
        let request = builder.push(CoreFrame::Lit(Literal::LitInt(7)));
        let union = builder.push(CoreFrame::Con {
            tag: UNION_ID,
            fields: vec![effect_tag, request],
        });
        builder.push(CoreFrame::Con {
            tag: E_ID,
            fields: vec![union, leaf],
        });
        builder.build()
    }

    #[test]
    fn persistent_session_allows_reuse_after_language_error_on_core() {
        let mut builder = TreeBuilder::new();
        let numerator = builder.push(CoreFrame::Lit(Literal::LitInt(5)));
        let zero = builder.push(CoreFrame::Lit(Literal::LitInt(0)));
        builder.push(CoreFrame::PrimOp {
            op: PrimOpKind::IntQuot,
            args: vec![numerator, zero],
        });
        let expr = builder.build();
        let table = effect_table();
        let mut session = PersistentSession::new(None, 4096, EngineKind::Core);
        session.bootstrap_if_needed(&expr, &table).unwrap();

        let failure = match session.run_entry(&table, &mut NoDispatch, &TestSink) {
            Err(error) => error,
            Ok(_) => panic!("division by zero unexpectedly completed"),
        };
        assert!(
            matches!(
                failure,
                JitError::Yield(tidepool_codegen::yield_type::YieldError::Runtime(
                    tidepool_codegen::host_fns::RuntimeError::DivisionByZero
                ))
            ),
            "unexpected language failure: {failure:?}"
        );
        assert_eq!(
            session.machine_disposition(),
            Some(MachineDisposition::Reusable)
        );
        session.bootstrap_if_needed(&expr, &table).unwrap();
    }

    #[test]
    fn persistent_session_refuses_unavailable_machine_reuse_on_core() {
        let mut builder = TreeBuilder::new();
        builder.push(CoreFrame::Var(VarId(0xfeed)));
        let expr = builder.build();
        let table = effect_table();
        let mut session = PersistentSession::new(None, 4096, EngineKind::Core);
        session.bootstrap_if_needed(&expr, &table).unwrap();

        let first = match session.run_entry(&table, &mut NoDispatch, &TestSink) {
            Err(error) => error,
            Ok(_) => panic!("unresolved variable unexpectedly completed"),
        };
        assert!(
            matches!(
                first,
                JitError::Yield(tidepool_codegen::yield_type::YieldError::Runtime(
                    tidepool_codegen::host_fns::RuntimeError::UnresolvedVar(..)
                ))
            ),
            "unexpected integrity failure: {first:?}"
        );
        assert!(matches!(
            session.bootstrap_if_needed(&expr, &table),
            Err(JitError::MachineUnavailable { .. })
        ));
        assert_eq!(
            session.machine_disposition(),
            Some(MachineDisposition::Unavailable)
        );
    }

    #[test]
    fn persistent_session_cancels_suspended_work_without_poisoning_reuse_on_core() {
        let expr = suspending_expr();
        let table = effect_table();
        let mut session = PersistentSession::new(None, 4096, EngineKind::Core);
        session.bootstrap_if_needed(&expr, &table).unwrap();
        let mut dispatch = RespondAfterSuspend { calls: 0 };

        assert!(matches!(
            session.run_entry(&table, &mut dispatch, &TestSink).unwrap(),
            SuspendableOutcome::Suspended { .. }
        ));
        let cancel = session.cancel_handle().expect("bootstrapped machine");
        cancel.cancel();
        let failure = match session.resume_with_table(
            &table,
            &mut dispatch,
            &TestSink,
            ResumeInput::Answer(Value::Lit(Literal::LitInt(9))),
        ) {
            Err(error) => error,
            Ok(_) => panic!("cancelled continuation unexpectedly completed"),
        };
        assert!(matches!(
            failure,
            JitError::Yield(tidepool_codegen::yield_type::YieldError::Runtime(
                tidepool_codegen::host_fns::RuntimeError::Cancelled
            ))
        ));
        assert_eq!(
            session.machine_disposition(),
            Some(MachineDisposition::Reusable)
        );
        assert!(
            cancel.is_cancelled(),
            "observing cancellation must not clear the consumer-owned flag"
        );
        let repeated = match session.run_entry(&table, &mut dispatch, &TestSink) {
            Err(error) => error,
            Ok(_) => panic!("a new run ignored the outstanding cancellation request"),
        };
        assert!(matches!(
            repeated,
            JitError::Yield(tidepool_codegen::yield_type::YieldError::Runtime(
                tidepool_codegen::host_fns::RuntimeError::Cancelled
            ))
        ));
        assert!(cancel.is_cancelled());
        cancel.reset();
        assert!(!cancel.is_cancelled());
        session.bootstrap_if_needed(&expr, &table).unwrap();
        let mut fresh_dispatch = RespondAfterSuspend { calls: 0 };
        assert!(matches!(
            session
                .run_entry(&table, &mut fresh_dispatch, &TestSink)
                .unwrap(),
            SuspendableOutcome::Suspended { .. }
        ));
    }
}
