//! Persistent prepared-STG session state shared by resident consumers.
//!
//! A session owns one prepared machine, accumulated constructor metadata,
//! persistent declarations and bindings, scoped bindings, and parked continuations.
//! Suspension is threadless: a continuation is rooted as data and a later
//! entry may resume it from a fresh evaluation thread.

use std::path::{Path, PathBuf};

use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BindingTipId};
use tidepool_codegen::machine::{CancelHandle, MachineDisposition};
use tidepool_codegen::prepared_program::ResidencyCounts;
use tidepool_codegen::scope::{ScopeId, ScopeTree};
use tidepool_codegen::suspension::{ContinuationId, RealmId};
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{DataCon, DataConTable, Generation, SessionModule, SessionVarId, VarId};

use tidepool_codegen::binding_table::BoundValue;
use tidepool_repr::execution_schema::{PreparedProgram, SymbolIdentity};

use super::binding_table::{BindRecord, BindingIndex};
use super::prepared::{PreparedEngine, PreparedRuntimeError};
use super::{
    ExactExportError, ExactExportSurface, SessionCompileView, SessionError, SessionLib,
    SourceImports,
};

/// Render the unqualified imports for the exact names visible from each live
/// value interface. A generated `Val.G` module can export helpers that have
/// not entered the binding table yet, so importing the module wholesale would
/// publish those helpers through later declaration modules.
fn value_import_specs(entries: impl IntoIterator<Item = (String, SessionModule)>) -> Vec<String> {
    let mut grouped = Vec::<(SessionModule, Vec<String>)>::new();
    for (name, module) in entries {
        if let Some((_, names)) = grouped.iter_mut().find(|(key, _)| *key == module) {
            names.push(name);
        } else {
            grouped.push((module, vec![name]));
        }
    }
    grouped.sort_by_key(|(module, _)| module.module_name());
    grouped
        .into_iter()
        .map(|(module, mut names)| {
            names.sort();
            names.dedup();
            format!("{} ({})", module.module_name(), names.join(", "))
        })
        .collect()
}

/// Cross-thread owned handle for one completed bind root. The root never moves
/// independently: it remains inside the session while that session is stowed,
/// and is taken only after the session returns to its owning thread.
// ---------------------------------------------------------------------------
// The shared session core
// ---------------------------------------------------------------------------

/// The resident-session substrate: one live [`PreparedEngine`]
/// (`None` until the first turn bootstraps it), the accumulated constructor
/// [`DataConTable`], the [`SessionLib`] persistent declaration environment, the [`BindingTable`] persistent
/// binding store, and the value-binding generation.
///
/// The consumers keep their own higher-level turn orchestration (source
/// wrapping, decl/pure-bind routing, output draining, continuation-id minting)
/// and delegate the machine and persistent-store operations here.
pub struct PersistentSession {
    /// The resident machine — `None` before the first turn bootstraps it,
    /// `Some` when idle/suspended, and moved out onto the eval thread for a
    /// turn's duration (stowed-XOR-running).
    machine: Option<PreparedEngine>,
    /// The constructor metadata unioned across turns (`insert_checked`, monotone:
    /// later turns are a subset), so an ADT value bound earlier renders with real
    /// con names later.
    session_table: DataConTable,
    /// The persistent declaration environment: user `data`/`class`/`f x = …` accumulated as source
    /// across turns, imported by later turns through the gen-versioned module.
    /// `None` for a session with no persistent declaration environment; `Some` for the repl and the
    /// accumulating harness.
    lib: Option<SessionLib>,
    /// The persistent binding store: `name → (SessionVarId, RootSlot, Val.G<g>)` for each
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
    /// stores. The binding store hangs [`BindingTable`] frames off these ids and
    /// the persistent declaration environment keys its per-scope tips off the SAME ids, which is why
    /// neither owns a forest of its own: two forests would be two answers to "is
    /// this scope live", and a scoped decl and a scoped binding would drift.
    /// [`ScopeId::ROOT`] is the flat session every pre-C2 caller lives in.
    scopes: ScopeTree,
    /// How requests interact with the handlers installed for this checkout.
    effect_policy: EffectRunPolicy,
    /// Live-value crossing policy paired with the current effect stack.
    live_payload: LivePayloadPolicy,
    /// JIT nursery size for the resident machine.
    nursery_size: usize,
}

/// The committed fact from moving one name into the persistent binding store.
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
/// persistent binding names.
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
    /// Build an idle session core. `lib` is the persistent declaration environment (`Some` for the repl
    /// and the accumulating harness; `None` for a session with no persistent declarations). The
    /// machine is not bootstrapped until the first turn.
    pub fn new(lib: Option<SessionLib>, nursery_size: usize) -> Self {
        PersistentSession {
            machine: None,
            session_table: DataConTable::new(),
            lib,
            bindings: BindingTable::new(),
            binding_index: BindingIndex::new(),
            val_gen: Generation(0),
            scopes: ScopeTree::new(),
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            nursery_size,
        }
    }

    // -- accessors ---------------------------------------------------------

    /// The persistent declaration environment library (read). Panics if the session has no persistent declaration environment —
    /// a repl invariant; the harness only calls this once a persistent declaration environment has been
    /// installed.
    pub fn lib(&self) -> &SessionLib {
        #[allow(clippy::expect_used, reason = "decl plane present")]
        self.lib.as_ref().expect("decl plane present")
    }
    /// The persistent declaration environment library (mutate — e.g. `define_batch_with_vals`). Panics if
    /// the session has no persistent declaration environment (see [`Self::lib`]).
    pub fn lib_mut(&mut self) -> &mut SessionLib {
        #[allow(clippy::expect_used, reason = "decl plane present")]
        self.lib.as_mut().expect("decl plane present")
    }
    /// Whether this session has a persistent declaration environment.
    pub fn has_lib(&self) -> bool {
        self.lib.is_some()
    }
    /// The persistent binding table (read).
    pub fn bindings(&self) -> &BindingTable {
        &self.bindings
    }
    /// The persistent binding table (mutate).
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
                (BoundValue { handle, .. }, Some(engine)) => {
                    if engine.release(*handle) {
                        released += 1;
                    }
                }
                _ => {}
            }
        }
        released
    }

    /// Drop one newly published value when no compiled turn can yet have
    /// captured it. This is deliberately narrower than name retraction:
    /// callers must supply the exact id, so an older captured generation is
    /// never disturbed.
    pub fn discard_unleased_binding(&mut self, id: SessionVarId) -> bool {
        let Some(entry) = self.bindings.remove_live(id) else {
            return false;
        };
        self.release_binding_roots(vec![entry]);
        true
    }

    /// Retire one owner while preserving it until any existing dependency
    /// lease settles. This is the request-carrier lifecycle primitive.
    pub fn retire_binding_owner(&mut self, id: SessionVarId) {
        if let Some(entry) = self.bindings.retire_owner(id) {
            self.release_binding_roots(vec![entry]);
        }
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
    /// Whether the first prepared program has installed the session machine.
    pub fn is_bootstrapped(&self) -> bool {
        self.machine.is_some()
    }

    /// The prepared engine, once the first prepared turn has installed it.
    pub fn prepared_mut(&mut self) -> Option<&mut PreparedEngine> {
        self.machine.as_mut()
    }

    /// The prepared engine, or a typed refusal before the machine is installed.
    pub fn require_prepared(&mut self) -> Result<&mut PreparedEngine, PreparedRuntimeError> {
        self.prepared_mut()
            .ok_or(PreparedRuntimeError::MachineNotInstalled)
    }

    /// The continuation ids parked on this session's machine: the ground
    /// truth a hole is reconciled against after a
    /// failed resume. Empty before the machine exists.
    #[must_use]
    pub fn parked_ids(&self) -> Vec<ContinuationId> {
        self.machine
            .as_ref()
            .map(PreparedEngine::parked_ids)
            .unwrap_or_default()
    }

    /// Whether the resident machine can safely accept another entry.
    ///
    /// `None` means this session has not bootstrapped a machine yet. Once a
    /// machine exists, language failures and cancellation leave it
    /// [`MachineDisposition::Reusable`], while failures that make heap or code
    /// integrity uncertain monotonically make it
    /// [`MachineDisposition::Unavailable`]. Source recovery is a separate
    /// declaration-environment report and never changes this decision.
    #[must_use]
    pub fn machine_disposition(&self) -> Option<MachineDisposition> {
        self.machine.as_ref().map(PreparedEngine::disposition)
    }

    /// Cancellation handle for this capacity-one registry resource scope.
    pub fn cancel_handle(&mut self) -> Option<CancelHandle> {
        self.machine
            .as_mut()
            .map(|engine| engine.cancel_handle(RealmId::ROOT))
    }

    /// The runtime resource scope owning the frame parked under `id`,
    /// `None` before the machine exists or if `id` names no live frame.
    #[must_use]
    pub fn parked_realm(&self, id: ContinuationId) -> Option<RealmId> {
        self.machine.as_ref()?.parked_realm(id)
    }

    /// Close a runtime resource scope on the resident machine:
    /// `(frames, handles_released)`. `(0, 0)` when the
    /// machine is not yet booted or the resource scope owns nothing (idempotent).
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        self.machine
            .as_mut()
            .map_or((0, 0), |engine| engine.close_realm(realm))
    }

    /// Prepared-machine residency counters; `None` before bootstrap.
    #[must_use]
    pub fn residency(&self) -> Option<ResidencyCounts> {
        self.machine.as_ref().map(PreparedEngine::residency)
    }

    /// Lifetime `(functions, code_bytes)` of Cranelift work this session's
    /// installs caused; `None` before the prepared machine is installed.
    #[must_use]
    pub fn codegen_totals(&self) -> Option<(u64, u64)> {
        self.machine.as_ref().map(PreparedEngine::codegen_totals)
    }

    /// Prepared old-space bytes as of the last successful between-turn
    /// collection; `None` before the machine has
    /// bootstrapped.
    #[must_use]
    pub fn old_bytes(&self) -> Option<usize> {
        self.machine.as_ref().map(PreparedEngine::old_bytes)
    }

    /// Read-only heap/GC snapshot; `None` before bootstrap.
    #[must_use]
    pub fn heap_stats(&self) -> Option<tidepool_codegen::machine::HeapStats> {
        self.machine.as_ref().map(PreparedEngine::heap_stats)
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
        let _ = advances_constructor_vocabulary;
        Ok(())
    }

    // -- machine lifecycle -------------------------------------------------
    //
    // Threading note: [`BindingTable`] holds `RootSlot(*mut *mut u8)` and
    // [`ExternalEnv`] holds raw slot addresses. Both the table and the
    // [`PreparedEngine`] carry an `unsafe impl Send` justified by the
    // stowed-XOR-running discipline, so a whole `PersistentSession` can be moved
    // to another thread as long as exactly one thread owns it at a time — which
    // is what the repl does (the session is moved into a `spawn_blocking` turn
    // and returned out of it). The harness instead runs the deep-recursion turn
    // on a fresh big-stack thread while keeping the rest of the session on the
    // caller's frame: it [`Self::lease_machine`]s the machine over with the
    // accumulated table and lets the [`MachineLease`] restore it on `Drop`.
    // `add_function` (which needs the raw-pointer env) therefore always happens
    // on the thread that owns the session.

    /// Install a prepared turn's program, bootstrapping
    /// the machine from it when this is the session's first turn. Every
    /// global the program declares resolves to a live prepared binding by
    /// the identity recorded when that binding was made.
    pub fn install_prepared(
        &mut self,
        prepared: PreparedProgram,
    ) -> Result<tidepool_codegen::prepared_program::ProgramId, PreparedRuntimeError> {
        match self.machine.as_mut() {
            None => {
                let (engine, program) =
                    PreparedEngine::bootstrap_with_nursery_bytes(prepared, self.nursery_size)?;
                self.machine = Some(engine);
                Ok(program)
            }
            Some(engine) => engine.install(prepared, &self.bindings, &self.binding_index),
        }
    }

    /// The live prepared bindings a later turn compiles against: each one's
    /// import identity and the generation it was bound at, declared to the
    /// extractor as retained generations so the projection links against the
    /// binding instead of recompiling a body it does not have.
    #[must_use]
    pub fn prepared_retained(&self) -> Vec<(SymbolIdentity, u64)> {
        let mut retained = self.binding_index.prepared_retained();
        if let Some(engine) = self.machine.as_ref() {
            // Package tops the machine already carries compiled code for.
            // A value binding wins any collision: the binding store's own
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
    /// instead of recompiling; `None` before the prepared machine is installed.
    #[must_use]
    pub fn code_export_count(&self) -> Option<usize> {
        self.machine.as_ref().map(PreparedEngine::code_export_count)
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
    // would leave a live session whose binding-store `RootSlot`s all dangle — a
    // state with no legitimate use and no way to detect from the outside.

    // -- persistent binding bookkeeping ----------------------------------

    /// Record a materialized value binding in the persistent binding store.
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

    /// The current persistent declaration environment module (`Lib.G<g>`), if any (also `None` when the
    /// session has no persistent declaration environment at all).
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
    /// persistent binding store.
    pub fn compile_view_in(&self, scope: ScopeId) -> Option<SessionCompileView> {
        if !self.scopes.is_live(scope) {
            return None;
        }
        let lib = self.lib.as_ref()?;
        let visible_entries = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .collect::<Vec<_>>();
        let visible_values = visible_entries
            .iter()
            .map(|(_, entry)| entry.module)
            .collect();
        // A value interface may carry generated helpers beside its published
        // binder. Keep the exact visible names so source compilation imports
        // only bindings that reached the persistent binding store. This deliberately uses
        // a tiny vector: `SessionModule` has identity equality, not an
        // ordering contract, and a scope normally has few live modules.
        let mut visible_value_names = Vec::<(SessionModule, Vec<String>)>::new();
        for (name, entry) in &visible_entries {
            if let Some((_, names)) = visible_value_names
                .iter_mut()
                .find(|(module, _)| *module == entry.module)
            {
                names.push(name.0.clone());
            } else {
                visible_value_names.push((entry.module, vec![name.0.clone()]));
            }
        }
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
                visible_value_names,
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

    /// The persistent declaration environment include directory (where `Lib.G<g>.hs` modules live), for
    /// a later turn's compile search path. `None` when the session has no decl
    /// persistent declaration environment.
    /// Move the persistent declaration environment OUT (machine rotation, one-session living
    /// structure): the declaration environment is source-side state (gen modules on disk +
    /// the in-memory decl log), independent of any machine's heap, so it
    /// transfers wholesale into a freshly-built session while the old
    /// machine (and its binding store, whose roots die with its heap) drops.
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
    /// `Val.G<g>` is injected for validation. The persistent declaration environment analogue of GHCi
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
    /// the live log, scope tip, recovery manifest, or binding store.
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
        let visible_entries = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .filter(|(name, _)| {
                !replaced_names
                    .iter()
                    .any(|replaced| replaced == &name.0.as_str())
            })
            .collect::<Vec<_>>();
        let visible_values = visible_entries
            .iter()
            .map(|(_, entry)| (entry.id, entry.module.module_name()))
            .collect::<Vec<_>>();
        let import_modules = value_import_specs(
            visible_entries
                .iter()
                .map(|(name, entry)| (name.0.clone(), entry.module)),
        );
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

    /// Retract `name` from the persistent declaration environment (its binding migrated to the
    /// binding store). No-op when `name` is not a current declaration head.
    pub fn retract(&mut self, name: &str) -> Result<(), SessionError> {
        self.retract_in(ScopeId::ROOT, name)
    }

    /// Scoped [`Self::retract`]: retract `name` from `scope`'s decl tip only.
    /// `retract(n) == retract_in(ScopeId::ROOT, n)`.
    ///
    /// A name lives in at most one store per scope, so a child binding
    /// `helper` must not retract the parent's persistent declaration environment
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
    /// after an earlier name has already entered the persistent binding store.
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
    /// This is also where both stores freeze the new scope's inherited
    /// environment. The persistent declaration environment captures its parent's generation;
    /// the binding store captures an immutable name-to-value tip with root
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

    /// The session's one scope forest — read by both stores for their lookup
    /// walks. There is no `_mut` sibling on purpose: minting and retiring are
    /// the only writes, and both go through this type so the binding store's
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
    /// liveness before consuming whatever ownership transfer led here (a
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

    /// Atomically move `entry.name` from this scope's persistent declaration environment to its
    /// materialized value store. Durable retraction is the commit point: if
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

    /// Atomically materialize a whole binding set. The declaration-environment
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
    /// could not enter the persistent binding store. They are registered persistent roots,
    /// not ordinary Rust-owned allocations, so dropping `RootSlot` alone would
    /// leak them until session teardown.
    fn discard_unbound_entries(&mut self, entries: Vec<BindingEntry>) {
        if let Some(engine) = self.machine.as_mut() {
            // A prepared entry's root is its adopted handle.
            for entry in entries {
                let BoundValue { handle, .. } = entry.value;
                engine.release(handle);
            }
            return;
        }
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
        let visible_entries = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .filter(|(name, _)| !replaced_set.contains(name.0.as_str()))
            .collect::<Vec<_>>();
        let captured_values = visible_entries
            .iter()
            .map(|(_, entry)| entry.id.var())
            .collect::<Vec<_>>();
        let import_modules = value_import_specs(
            visible_entries
                .iter()
                .map(|(name, entry)| (name.0.clone(), entry.module)),
        );
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
            .map_or(0, PreparedEngine::persistent_roots_count)
    }

    /// Accounting class 2 — live value handles on the resident machine,
    /// 0 before the machine bootstraps.
    #[must_use]
    pub fn value_handle_count(&self) -> usize {
        self.machine
            .as_ref()
            .map_or(0, PreparedEngine::handle_count)
    }

    /// Accounting class 1, root half — the stowed roots of parked frames,
    /// 0 before the machine bootstraps.
    #[must_use]
    pub fn stowed_roots_count(&self) -> usize {
        self.machine
            .as_ref()
            .map_or(0, PreparedEngine::stowed_roots_count)
    }

    /// Accounting class 1, frame half — the parked continuations, whichever
    /// engine. Always equal to [`Self::stowed_roots_count`] at quiescence.
    #[must_use]
    pub fn parked_count(&self) -> usize {
        self.machine
            .as_ref()
            .map_or(0, PreparedEngine::parked_count)
    }

    /// Retire `scope` and its whole subtree: drop each scope's binding-store
    /// frame and RELEASE the GC roots those bindings solely owned.
    ///
    /// Walks [`ScopeTree::retire`]'s deepest-first order so a child's frames
    /// are gone before its parent's, drains each frame
    /// ([`BindingTable::drain_scope`]), and for every drained entry applies the
    /// **sole-ownership rule**: its root is deregistered
    /// ([`PreparedEngine::retire_scope_root`]) only when no OTHER live
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
    machine: Option<PreparedEngine>,
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
    pub fn parts(&mut self) -> (&mut PreparedEngine, &DataConTable) {
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
