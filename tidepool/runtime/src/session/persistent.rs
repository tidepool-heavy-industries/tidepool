//! Persistent prepared-STG session state shared by resident consumers.
//!
//! A session owns one prepared machine, accumulated constructor metadata,
//! persistent declarations and bindings, scoped bindings, and parked continuations.
//! Suspension is threadless: a continuation is rooted as data and a later
//! entry may resume it from a fresh evaluation thread.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tidepool_codegen::binding_table::{
    BindingEntry, BindingScopeWitness, BindingTable, BindingTipId, SourceLeaseKey,
};
use tidepool_codegen::machine::{CancelHandle, MachineDisposition};
use tidepool_codegen::prepared_program::{
    DemandedImage, ImageRegistry, InheritedSourceDemand, PendingGroupInventory, ProgramId,
    ResidencyCounts, SourceBinder, SourceGroupOutline, SourceInstanceLease,
};
use tidepool_codegen::scope::{ScopeId, ScopeTree};
use tidepool_codegen::suspension::{ContinuationId, RealmId};
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{DataCon, DataConTable, Generation, SessionModule, SessionVarId, VarId};

use tidepool_codegen::binding_table::BoundValue;
use tidepool_repr::execution_schema::{
    CachedHomeOwner, CertifiedGroup, GlobalDecl, ImportOwner, PreparedProgram, SymbolIdentity,
};
use tidepool_toolchain::certified_products::PendingImportOwner;

use super::binding_table::{BindRecord, BindingIndex};
use super::prepared::{
    CertifiedTargetImage, InstallSnapshot, PreparedEngine, PreparedRuntimeError,
};
use super::turn::TurnCertification;
use super::{
    DeclarationCandidateRender, ExactExportError, ExactExportSurface, PublicManifestBase,
    PublicManifestCommit, PublicationDecision, RecoveryPublicOwner, SessionCompileView,
    SessionError, SessionLib, SourceImports, StagedPublicManifest,
};

/// Render the unqualified imports for the exact names visible from each live
/// value interface. A generated `Val.G` module can export helpers that have
/// not entered the binding table yet, so importing the module wholesale would
/// publish those helpers through later declaration modules.
/// [`PersistentSession::declaration_staging_context_in`]'s result: the
/// persistent imports a candidate compiles against, the value-interface
/// import specs its render pulls in, and the live-value environment (var id,
/// module name) a later adopt must still match.
type DeclarationStagingContext = (SourceImports, Vec<String>, Vec<(SessionVarId, String)>);

/// Exact compiler owners after retained names have been resolved through one
/// live lexical scope or the same machine's immutable export ledger. Native
/// installation rechecks the handles under the final checkout; this record
/// carries no permission to publish a binding.
pub(crate) struct ResolvedCertifiedTurn {
    pub groups: Vec<tidepool_codegen::prepared_program::ScopedCertifiedGroup>,
    pub target_owners: Vec<ImportOwner>,
    pub package_interfaces:
        tidepool_toolchain::certified_products::CertifiedTargetPackageInterfaces,
    pub source_evidence: BTreeMap<SourceBinder, (CachedHomeOwner, u32)>,
    pub inherited_needed: Vec<InheritedSourceDemand>,
    pub source_plan: ResolvedSourceDomainPlan,
}

#[derive(Clone)]
pub(crate) struct ResolvedSourceDomainPlan {
    pub(super) target:
        BTreeMap<SourceBinder, tidepool_codegen::prepared_program::ScopedSourceBinder>,
    selection: tidepool_codegen::prepared_program::SourceDomainSelection,
    snapshot: tidepool_codegen::binding_table::BindingScopeSnapshot,
}

impl ResolvedSourceDomainPlan {
    #[cfg(test)]
    pub(super) fn fixture(
        target: BTreeMap<SourceBinder, tidepool_codegen::prepared_program::ScopedSourceBinder>,
        selection: tidepool_codegen::prepared_program::SourceDomainSelection,
        snapshot: tidepool_codegen::binding_table::BindingScopeSnapshot,
    ) -> Self {
        Self {
            target,
            selection,
            snapshot,
        }
    }
}

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

// Cross-thread owned handle for one completed bind root. The root never moves
// independently: it remains inside the session while that session is stowed,
// and is taken only after the session returns to its owning thread.
// ---------------------------------------------------------------------------
// The shared session core
// ---------------------------------------------------------------------------

/// The original published surface authorizes one actor's native bootstrap.
/// This affine seal is consumed before interactive readiness; it cannot be
/// reconstructed from a scope identifier after bootstrap mutations.
pub struct DurablePublicBootstrap {
    admission_owner: Arc<super::admission::RuntimeAdmissionOwner>,
    admission_epoch: u64,
    session: super::SessionId,
    owner: RecoveryPublicOwner,
    initial: super::PublicVisibilitySnapshot,
    surface: super::recovery::RecoveryPublicSurface,
}

/// The exact checked interface prepared before a host value enters the heap.
/// Dropping an uncommitted token reaps only the file this staging operation created.
pub(super) struct StagedCheckedValueInterface {
    interface: Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>,
    owner: Arc<super::admission::RuntimeAdmissionOwner>,
    owner_epoch: u64,
    library: Option<(uuid::Uuid, super::SessionId, PathBuf)>,
    module: SessionModule,
    created: Option<StagedInterfaceFile>,
}

static_assertions::assert_not_impl_any!(StagedCheckedValueInterface: Clone, Copy);

impl StagedCheckedValueInterface {
    pub(super) fn module(&self) -> SessionModule {
        self.module
    }
}

struct StagedInterfaceFile {
    path: PathBuf,
    file: std::fs::File,
    committed: bool,
}

impl Drop for StagedInterfaceFile {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // An unrelated replacement must survive cleanup of the original creation.
        if let (Ok(owned), Ok(current)) =
            (self.file.metadata(), std::fs::symlink_metadata(&self.path))
        {
            if owned.dev() == current.dev() && owned.ino() == current.ino() {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

fn stage_interface_file(
    path: PathBuf,
    bytes: &[u8],
) -> std::io::Result<Option<StagedInterfaceFile>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => {
            let mut creation = StagedInterfaceFile {
                path,
                file,
                committed: false,
            };
            creation.file.write_all(bytes)?;
            Ok(Some(creation))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(&path)?.as_slice() != bytes {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "original checked interface conflicts with {}",
                        path.display()
                    ),
                ));
            }
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// The resident-session substrate: one live [`PreparedEngine`]
/// (`None` until the first turn bootstraps it), the accumulated constructor
/// [`DataConTable`], the [`SessionLib`] persistent declaration environment, the [`BindingTable`] persistent
/// binding store, and the value-binding generation.
///
/// The consumers keep their own higher-level turn orchestration (source
/// wrapping, decl/pure-bind routing, output draining, continuation-id minting)
/// and delegate the machine and persistent-store operations here.
pub struct PersistentSession {
    catalog_selection: tidepool_toolchain::toolchain::CatalogSelection,
    admission_owner: Arc<super::admission::RuntimeAdmissionOwner>,
    /// The resident machine — `None` before the first turn bootstraps it,
    /// `Some` when idle/suspended, and moved out onto the eval thread for a
    /// turn's duration (stowed-XOR-running).
    machine: Option<PreparedEngine>,
    /// Cancellation selected by the current invocation, including bootstrap.
    invocation_cancel: Option<Arc<AtomicBool>>,
    /// Names native instance IDs within the exact machine lifetime, including
    /// while the engine is temporarily moved into a machine lease.
    machine_incarnation: Option<tidepool_repr::SessionId>,
    /// The exact bootstrap dependencies sealed by the host's initialization
    /// entry before actor admission. Transfer never imports old heap roots.
    recovery_initialization: Option<RecoveryInitialization>,
    /// The image registry the machine installs through, held here so a
    /// registry given before the first turn reaches the bootstrap install.
    image_registry: Option<Arc<tidepool_codegen::prepared_program::ImageRegistry>>,
    /// The constructor metadata unioned across turns (`insert_checked`, monotone:
    /// later turns are a subset), so an ADT value bound earlier renders with real
    /// con names later.
    session_table: DataConTable,
    /// The persistent declaration environment: user `data`/`class`/`f x = …` accumulated as source
    /// across turns, imported by later turns through the gen-versioned module.
    /// `None` for a session with no persistent declaration environment; `Some` for the repl and the
    /// accumulating harness.
    lib: Option<SessionLib>,
    /// The persistent binding store: `name → (SessionVarId, PreparedHandle, Val.G<g>)` for each
    /// materialized bind, seeded into a later fragment's [`ExternalEnv`].
    bindings: BindingTable,
    /// Immutable scoped semantics, keyed by witnesses from the mutation owners.
    /// Request counters and the ambient injection inventory are refreshed separately.
    compile_views: parking_lot::Mutex<HashMap<ScopeId, Arc<CachedCompileView>>>,
    stub_revision: u64,
    #[cfg(test)]
    compile_view_bytes_hashed: std::sync::atomic::AtomicUsize,
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
    public_visibility_epochs: HashMap<ScopeId, u64>,
    /// How requests interact with the handlers installed for this checkout.
    effect_policy: EffectRunPolicy,
    /// Live-value crossing policy paired with the current effect stack.
    live_payload: LivePayloadPolicy,
    /// JIT nursery size for the resident machine.
    nursery_size: usize,
    /// Value-binding generations minted by [`super::resident::ResidentSession::mount_carrier_in`]
    /// rather than a real bind turn: their `Val.G<g>` source is a hand-written
    /// stub on the session include path, not an extract-compiled `.hi`. A
    /// stub generation must never be named `--inject-val` (no `.hi` exists
    /// for it) — [`Self::live_val_modules`] and [`Self::compile_view_in`]'s
    /// injected set both exclude it, while it stays an ordinary member of
    /// every other live-module computation (unqualified imports, shadowing,
    /// eviction) exactly like a real bind's generation.
    stub_generations: std::collections::BTreeSet<u64>,
    /// Stub generations [`Self::release_binding_roots`] just found fully
    /// unreferenced (no live binding resolves to their `Val.G<g>` module
    /// any more), drained by [`Self::take_retired_stub_sources`]. A caller
    /// with the session root reaps each one's on-disk source
    /// (`super::resident::ResidentSession::reap_evicted_stub_sources_in`) so
    /// a later turn that still names the generation (a declaration module
    /// that imported it before eviction) fails to find the module instead of
    /// silently compiling the stub's own self-referential body — see
    /// `mount_carrier_in`'s doc comment for why that body must never run.
    retired_stub_sources: Vec<Generation>,
}

#[derive(Clone, PartialEq, Eq)]
struct CompileViewKey {
    library: uuid::Uuid,
    tip: Generation,
    bindings: BindingScopeWitness,
    stubs: u64,
    public_epoch: u64,
}

struct CachedCompileView {
    key: Option<CompileViewKey>,
    view: SessionCompileView,
    digest: [u8; 32],
}

struct RecoveryInitialization {
    library: uuid::Uuid,
    manifest_owner: Arc<super::recovery_hydration::OwnedRecoveryManifest>,
    scope: ScopeId,
    owner_epoch: u64,
    machine_incarnation: Option<tidepool_repr::SessionId>,
    sources: Vec<SourceInstanceLease>,
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
        // Recovery retains original type interfaces even when their heap
        // values are lost. Their module identities cannot be issued again.
        let val_gen = lib
            .as_ref()
            .map(|lib| lib.log.retained_value_generation_high_water())
            .unwrap_or(Generation(0));
        PersistentSession {
            admission_owner: Arc::new(super::admission::RuntimeAdmissionOwner::new()),
            catalog_selection: Default::default(),
            machine: None,
            invocation_cancel: None,
            machine_incarnation: None,
            recovery_initialization: None,
            image_registry: None,
            session_table: DataConTable::new(),
            lib,
            bindings: BindingTable::new(),
            compile_views: parking_lot::Mutex::new(HashMap::new()),
            stub_revision: 1,
            #[cfg(test)]
            compile_view_bytes_hashed: std::sync::atomic::AtomicUsize::new(0),
            binding_index: BindingIndex::new(),
            val_gen,
            scopes: ScopeTree::new(),
            public_visibility_epochs: HashMap::new(),
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            nursery_size,
            stub_generations: std::collections::BTreeSet::new(),
            retired_stub_sources: Vec::new(),
        }
    }

    pub fn set_catalog_selection(
        &mut self,
        catalog: tidepool_toolchain::toolchain::CatalogSelection,
    ) {
        self.catalog_selection = catalog;
    }

    pub fn catalog_selection(&self) -> &tidepool_toolchain::toolchain::CatalogSelection {
        &self.catalog_selection
    }

    pub(super) fn admission_owner(&self) -> &Arc<super::admission::RuntimeAdmissionOwner> {
        &self.admission_owner
    }

    /// Record that `generation`'s `Val.G<g>` module is a hand-written source
    /// stub (a [`super::resident::HostCarrier`] mount), never an extract
    /// `.hi`. Idempotent.
    pub(super) fn mark_stub_generation(&mut self, generation: Generation) {
        if self.stub_generations.insert(generation.0) {
            self.advance_stub_revision();
        }
    }

    fn advance_stub_revision(&mut self) {
        self.stub_revision = if self.stub_revision == 0 {
            0
        } else {
            self.stub_revision.checked_add(1).unwrap_or(0)
        };
    }

    fn is_stub_module(&self, module: SessionModule) -> bool {
        self.stub_generations.contains(&module.gen().0)
    }

    /// Drain the stub generations [`Self::release_binding_roots`] has found
    /// fully unreferenced since the last drain. Each one's `.hs` source
    /// should be deleted by a caller holding the session root; the
    /// generation itself has already left [`Self::stub_generations`], so it
    /// no longer appears in [`Self::live_val_modules`] or
    /// [`Self::prepared_retained`] regardless of when (or whether) the file
    /// is actually reaped.
    pub(super) fn take_retired_stub_sources(&mut self) -> Vec<Generation> {
        std::mem::take(&mut self.retired_stub_sources)
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
    pub(super) fn stage_checked_value_interface(
        &self,
        interface: Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>,
    ) -> Result<StagedCheckedValueInterface, SessionError> {
        let module = interface.owner();
        if module != SessionModule::val(module.gen())
            || !self.binding_index.accepts_value_interface(&interface)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let library = self
            .lib
            .as_ref()
            .map(|lib| (lib.compile_view_identity, lib.id, lib.root.clone()));
        let created = match &library {
            Some((_, _, root)) => stage_interface_file(
                root.join(module.relative_hi_path()),
                interface.bytes_owned(),
            )?,
            None => None,
        };
        Ok(StagedCheckedValueInterface {
            interface,
            owner: self.admission_owner().clone(),
            owner_epoch: self.admission_owner().epoch(),
            library,
            module,
            created,
        })
    }

    /// Recheck an off-checkout stage without publishing it into compiler views.
    pub(super) fn validate_staged_value_interface(
        &self,
        staged: &StagedCheckedValueInterface,
    ) -> Result<(), SessionError> {
        let library = self
            .lib
            .as_ref()
            .map(|lib| (lib.compile_view_identity, lib.id, lib.root.clone()));
        if !Arc::ptr_eq(&staged.owner, self.admission_owner())
            || staged.owner_epoch != self.admission_owner().epoch()
            || staged.library != library
            || staged.module != staged.interface.owner()
            || staged.module != SessionModule::val(staged.module.gen())
            || !self
                .binding_index
                .accepts_value_interface(&staged.interface)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        Ok(())
    }

    /// The binding owner validated this token immediately before its native
    /// write under exclusive checkout. Commit has no IO or recoverable failure.
    pub(super) fn commit_staged_value_interface(
        &mut self,
        mut staged: StagedCheckedValueInterface,
    ) {
        self.binding_index.commit_value_interface(staged.interface);
        if let Some(created) = &mut staged.created {
            created.committed = true;
        }
    }

    pub(super) fn retain_checked_value_interface(
        &mut self,
        interface: Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>,
    ) -> Result<(), SessionError> {
        let module = interface.owner();
        if module != SessionModule::val(module.gen())
            || !self.binding_index.accepts_value_interface(&interface)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        // Ordinary completion may settle type evidence before any root is bound.
        // That evidence belongs to its checked snapshot, not the live include tree.
        if !self.binding_index.is_module_live(&module.module_name()) {
            return Ok(());
        }
        let staged = self.stage_checked_value_interface(interface)?;
        self.validate_staged_value_interface(&staged)?;
        self.commit_staged_value_interface(staged);
        Ok(())
    }

    pub(super) fn retained_value_interface(&self, module: SessionModule) -> Option<&Arc<[u8]>> {
        self.binding_index.value_interface(module)
    }

    pub(super) fn retained_checked_value_artifact(
        &self,
        module: SessionModule,
    ) -> Option<&Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>> {
        self.binding_index.checked_value_artifact(module)
    }

    pub(super) fn mark_legacy_value_interface(&mut self, module: SessionModule) {
        self.binding_index.mark_legacy_interface(module);
    }

    #[cfg(test)]
    pub(super) fn retain_fixture_value_interface(
        &mut self,
        module: SessionModule,
        bytes: Arc<[u8]>,
    ) {
        self.binding_index.retain_fixture_interface(module, bytes);
    }

    /// The persistent binding table (read).
    pub fn bindings(&self) -> &BindingTable {
        &self.bindings
    }
    pub(super) fn acquire_binding_leases(
        &mut self,
        ids: impl IntoIterator<Item = SessionVarId>,
    ) -> std::collections::HashSet<SessionVarId> {
        self.bindings.acquire_leases(ids)
    }

    pub(super) fn release_binding_leases(
        &mut self,
        ids: impl IntoIterator<Item = SessionVarId>,
    ) -> Vec<BindingEntry> {
        self.bindings.release_leases(ids)
    }

    pub(super) fn collect_binding_observations(&mut self) -> Vec<BindingEntry> {
        self.bindings.collect_observations()
    }

    /// One resolution pass over the exact scoped dependency closure. Sorted
    /// IDs preserve the binding owner's choice among same-owner aliases.
    fn scoped_prepared_bindings_in(
        &self,
        scope: ScopeId,
    ) -> HashMap<(&SymbolIdentity, SessionModule), &BindingEntry> {
        let mut entries = HashMap::new();
        for id in self
            .bindings
            .scope_reachable_binding_ids(&self.scopes, scope)
        {
            if let Some(entry) = self.bindings.get(id) {
                entries
                    .entry((&entry.value.identity, entry.module))
                    .or_insert(entry);
            }
        }
        entries
    }

    fn resolve_native_binding_custody(
        &self,
        scope: ScopeId,
        requirements: &[tidepool_toolchain::artifact_inventory::NativeBindingRequirement],
    ) -> Result<Vec<SessionVarId>, SessionError> {
        if requirements.is_empty() {
            return Ok(Vec::new());
        }
        let scoped = self.scoped_prepared_bindings_in(scope);
        let mut ids = Vec::with_capacity(requirements.len());
        for requirement in requirements {
            let module = SessionModule::val(Generation(requirement.generation));
            let entry =
                scoped
                    .get(&(&requirement.identity, module))
                    .ok_or(SessionError::InvalidPublicBindingPromotion(
                    tidepool_codegen::binding_table::BindingPromotionError::MissingOrForeignBinding,
                ))?;
            let handle = entry.value.handle;
            if self
                .prepared()
                .and_then(|engine| engine.prepared_handle_of(handle.raw()))
                != Some(handle)
            {
                return Err(SessionError::InvalidPublicBindingPromotion(
                    tidepool_codegen::binding_table::BindingPromotionError::MissingOrForeignBinding,
                ));
            }
            ids.push(entry.id);
        }
        Ok(ids)
    }

    /// Resolve the worker's retained imports against the exact inherited
    /// lexical view or owning engine's immutable export ledger, before native
    /// compilation. The worker never supplies a `SessionVarId` or native root
    /// id; spelling or the globally newest binding cannot choose a mutable
    /// value owned by another scope.
    pub(crate) fn resolve_certification_in(
        &self,
        scope: ScopeId,
        prepared: &PreparedProgram,
        certification: &TurnCertification,
    ) -> Result<ResolvedCertifiedTurn, PreparedRuntimeError> {
        if !self.scopes.is_live(scope) {
            return Err(PreparedRuntimeError::SourceScopeAdmission);
        }
        let retained = std::cell::OnceCell::new();
        let mut source_evidence = BTreeMap::new();
        let mut resolve = |globals: &[GlobalDecl], pending: &[PendingImportOwner]| {
            if globals.len() != pending.len() {
                return Err(PreparedRuntimeError::CertifiedTargetOwners);
            }
            globals
                .iter()
                .zip(pending)
                .map(|(global, owner)| {
                    if let PendingImportOwner::Source {
                        owner,
                        original_ordinal,
                        binder,
                    } = owner
                    {
                        if binder != &global.identity {
                            return Err(PreparedRuntimeError::CertifiedTargetOwners);
                        }
                        let source = SourceBinder {
                            version: owner.module_version.clone(),
                            binder: binder.clone(),
                        };
                        let exact = (owner.clone(), *original_ordinal);
                        if source_evidence
                            .insert(source.clone(), exact.clone())
                            .is_some_and(|prior| prior != exact)
                        {
                            return Err(PreparedRuntimeError::InvalidCertifiedSourceOwner(source));
                        }
                        return Ok(ImportOwner::Source {
                            version: owner.module_version.clone(),
                            binder: binder.clone(),
                        });
                    }
                    if let PendingImportOwner::Retained {
                        identity,
                        generation,
                    } = owner
                    {
                        if identity != &global.identity
                            || global.required_generation != Some(*generation)
                        {
                            return Err(PreparedRuntimeError::CertifiedTargetOwners);
                        }
                        if let Some(entry) = retained
                            .get_or_init(|| self.scoped_prepared_bindings_in(scope))
                            .get(&(identity, SessionModule::val(Generation(*generation))))
                        {
                            return Ok(ImportOwner::Retained {
                                id: entry.id,
                                generation: *generation,
                            });
                        }
                        return Err(PreparedRuntimeError::MissingRetainedCertifiedOwner {
                            identity: identity.clone(),
                            generation: *generation,
                        });
                    }
                    if let PendingImportOwner::RetainedPackage {
                        unit,
                        module,
                        binder,
                        generation,
                        interface_digest,
                    } = owner
                    {
                        if binder != &global.identity
                            || &binder.unit != unit
                            || &binder.module != module
                            || global.required_generation != Some(*generation)
                        {
                            return Err(PreparedRuntimeError::CertifiedTargetOwners);
                        }
                        return self
                            .machine
                            .as_ref()
                            .and_then(|engine| {
                                engine.retained_package_code_export_owner(
                                    binder,
                                    *generation,
                                    interface_digest,
                                )
                            })
                            .ok_or_else(|| PreparedRuntimeError::MissingRetainedCertifiedOwner {
                                identity: binder.clone(),
                                generation: *generation,
                            });
                    }
                    let PendingImportOwner::Package {
                        unit,
                        module,
                        binder,
                        interface_digest,
                    } = owner
                    else {
                        unreachable!("all pending owner variants handled")
                    };
                    if binder != &global.identity {
                        return Err(PreparedRuntimeError::CertifiedTargetOwners);
                    }
                    Ok(ImportOwner::Package {
                        unit: unit.clone(),
                        module: module.clone(),
                        binder: binder.clone(),
                        interface_digest: *interface_digest,
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        };
        let target_owners = resolve(prepared.globals(), &certification.target_owners)?;
        let outlines = certification
            .groups
            .iter()
            .map(|pending| {
                if pending.group().globals().len() != pending.imports().len() {
                    return Err(PreparedRuntimeError::CertifiedTargetOwners);
                }
                let source_imports = pending
                    .group()
                    .globals()
                    .iter()
                    .zip(pending.imports())
                    .filter_map(|(global, owner)| match owner {
                        PendingImportOwner::Source { owner, binder, .. } => {
                            Some(if binder == &global.identity {
                                Ok(SourceBinder {
                                    version: owner.module_version.clone(),
                                    binder: binder.clone(),
                                })
                            } else {
                                Err(PreparedRuntimeError::CertifiedTargetOwners)
                            })
                        }
                        PendingImportOwner::Retained { .. }
                        | PendingImportOwner::RetainedPackage { .. }
                        | PendingImportOwner::Package { .. } => None,
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(SourceGroupOutline::from_projected(
                    pending.owner().clone(),
                    pending.group(),
                    source_imports,
                )?)
            })
            .collect::<Result<Vec<_>, PreparedRuntimeError>>()?;
        let selection = self
            .bindings
            .source_domain_selection_in(&self.scopes, scope)
            .map_err(|_| PreparedRuntimeError::SourceScopeAdmission)?;
        let snapshot = self
            .bindings
            .scope_snapshot(&self.scopes, scope)
            .map_err(|_| PreparedRuntimeError::SourceScopeAdmission)?;
        let roots = target_owners.iter().filter_map(|owner| match owner {
            ImportOwner::Source { version, binder } => Some(SourceBinder {
                version: version.clone(),
                binder: binder.clone(),
            }),
            ImportOwner::Retained { .. }
            | ImportOwner::CodeExport { .. }
            | ImportOwner::Package { .. } => None,
        });
        let selected = PendingGroupInventory::new(outlines)?.seal_in_domains(roots, &selection)?;
        let (plans, inherited_needed, target) = selected.into_parts();
        let inherited_needed = inherited_needed
            .into_iter()
            .map(|scoped| scoped.into_demand())
            .collect();
        let mut groups = Vec::with_capacity(plans.len());
        for plan in plans {
            let pending = &certification.groups[plan.index()];
            let imports = resolve(pending.group().globals(), pending.imports())?;
            groups.push(
                tidepool_codegen::prepared_program::ScopedCertifiedGroup::admit(
                    CertifiedGroup::admit(
                        pending.owner().clone(),
                        pending.group().clone(),
                        imports,
                    )?,
                    plan,
                )?,
            );
        }
        Ok(ResolvedCertifiedTurn {
            groups,
            target_owners,
            package_interfaces: certification.package_interfaces.clone(),
            source_evidence,
            inherited_needed,
            source_plan: ResolvedSourceDomainPlan {
                target,
                selection,
                snapshot,
            },
        })
    }

    /// Install a certified target against this exact lexical view and place
    /// every newly materialized source root under its scope before returning
    /// the executable target. The machine batch is unpublished until the
    /// registrar accepts all roots; rejection releases its handles and pins.
    pub(crate) fn install_certified_turn_in(
        &mut self,
        scope: ScopeId,
        target: CertifiedTargetImage,
        target_owners: &[ImportOwner],
        source_evidence: &BTreeMap<SourceBinder, (CachedHomeOwner, u32)>,
        demanded: Vec<DemandedImage>,
        inherited_needed: &[InheritedSourceDemand],
    ) -> Result<
        (
            ProgramId,
            tidepool_codegen::binding_table::SourceScopeAdmission,
        ),
        PreparedRuntimeError,
    > {
        if !self.scopes.is_live(scope) {
            return Err(PreparedRuntimeError::SourceScopeAdmission);
        }
        let qualified = target.source_plan().is_some();
        let inherited = if let Some(plan) = target.source_plan() {
            let current = self
                .bindings
                .scope_snapshot(&self.scopes, scope)
                .map_err(|_| PreparedRuntimeError::SourceScopeAdmission)?;
            if current != plan.snapshot {
                return Err(PreparedRuntimeError::SourceScopeAdmission);
            }
            // The exact scoped snapshot binds the privately issued selection.
            plan.selection.inherited().clone()
        } else {
            #[cfg(not(test))]
            {
                return Err(PreparedRuntimeError::SourceScopeAdmission);
            }
            #[cfg(test)]
            {
                let mut inherited = BTreeMap::new();
                let mut anchors = HashMap::new();
                for lease in self
                    .bindings
                    .selected_source_instances_in(&self.scopes, scope)
                {
                    let group = (lease.owner().clone(), lease.original_ordinal());
                    if anchors
                        .insert(group.clone(), lease.instance())
                        .is_some_and(|old| old != lease.instance())
                    {
                        return Err(PreparedRuntimeError::AmbiguousSourceGroup {
                            owner: group.0,
                            ordinal: group.1,
                        });
                    }
                    let key = tidepool_codegen::prepared_program::ScopedSourceBinder {
                        domain: tidepool_codegen::prepared_program::SourceInstanceDomain::single(),
                        source: lease.binder().clone(),
                    };
                    if inherited.insert(key.clone(), lease.clone()).is_some_and(
                        |old: SourceInstanceLease| {
                            old.instance() != lease.instance() || old.handle() != lease.handle()
                        },
                    ) {
                        return Err(PreparedRuntimeError::AmbiguousSourceInstance(key.source));
                    }
                }
                inherited
            }
        };
        let mut exact_external = HashMap::new();
        let retained = std::cell::OnceCell::new();
        for (globals, owners) in std::iter::once((target.globals(), target_owners)).chain(
            demanded.iter().map(|selected| {
                (
                    selected.group().definitions().globals(),
                    selected.group().imports(),
                )
            }),
        ) {
            if globals.len() != owners.len() {
                return Err(PreparedRuntimeError::CertifiedTargetOwners);
            }
            for (global, owner) in globals.iter().zip(owners) {
                let ImportOwner::Retained { id, generation } = owner else {
                    continue;
                };
                let entry = retained
                    .get_or_init(|| self.scoped_prepared_bindings_in(scope))
                    .get(&(
                        &global.identity,
                        SessionModule::val(Generation(*generation)),
                    ))
                    .filter(|entry| entry.id == *id)
                    .ok_or_else(|| PreparedRuntimeError::MissingCertifiedOwner(owner.clone()))?;
                exact_external.insert(owner.clone(), entry.value.handle);
            }
        }
        drop(retained);
        let mut bootstrap = if self.machine.is_none() {
            Some(PreparedEngine::empty_certified(
                self.nursery_size,
                self.image_registry.clone(),
            )?)
        } else {
            None
        };
        let engine = bootstrap
            .as_mut()
            .or(self.machine.as_mut())
            .expect("certified machine exists or was constructed");
        engine.set_invocation_cancel(self.invocation_cancel.clone());
        let mut staged = engine.install_certified_turn(
            target,
            target_owners,
            source_evidence,
            demanded,
            inherited_needed,
            &inherited,
            &exact_external,
            &self.bindings,
        )?;
        let tokens = std::mem::take(&mut staged.leases);
        let admitted = if qualified {
            engine
                .admit_source_instances(
                    &mut self.bindings,
                    &self.scopes,
                    scope,
                    std::mem::take(&mut staged.domain_leases),
                )
                .map_err(|_| tokens.clone())
        } else {
            self.bindings
                .register_source_install_in(&self.scopes, scope, tokens.clone())
        };
        match admitted {
            Ok(keys) => {
                let engine = bootstrap
                    .as_mut()
                    .or(self.machine.as_mut())
                    .expect("certified machine remains installed");
                let target = engine.commit_certified_turn(staged);
                if let Some(engine) = bootstrap {
                    self.machine = Some(engine);
                    self.ensure_machine_incarnation();
                }
                Ok((target, keys))
            }
            Err(tokens) => {
                let engine = bootstrap
                    .as_mut()
                    .or(self.machine.as_mut())
                    .expect("certified machine remains installed");
                engine.abort_certified_turn(staged, tokens)?;
                Err(PreparedRuntimeError::SourceScopeAdmission)
            }
        }
    }

    /// Release only the source roots first introduced by a failed turn.
    /// Captured tips retain independent shares until their final owner drains.
    pub(crate) fn retire_failed_turn_source_instances(
        &mut self,
        scope: ScopeId,
        keys: &tidepool_codegen::binding_table::SourceScopeAdmission,
    ) -> bool {
        let Some(released) = self.bindings.rollback_source_admission(scope, keys) else {
            return false;
        };
        self.release_source_instance_roots(released);
        true
    }

    #[cfg(test)]
    pub(super) fn retire_fixture_source_subset(
        &mut self,
        scope: ScopeId,
        keys: &[SourceLeaseKey],
    ) -> bool {
        let Some(released) = self.bindings.retire_source_instances_in(scope, keys) else {
            return false;
        };
        self.release_source_instance_roots(released);
        true
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
            let module = entry.module;
            // `on_evict` is the single point of truth for whether any OTHER
            // live entry still shares this handle (an alias published by
            // `bind_alias_in`, or a same-batch sibling evicted alongside
            // this entry) -- replacing the old whole-table scan. It must run
            // exactly once per entry that leaves `live`, which this is.
            let safe_to_release = self.binding_index.on_evict(&entry);
            // This is THE single point where any binding -- from ANY
            // eviction path (a request-carrier retire, a scope close, a
            // declaration replacing same-scope names, an expired
            // observation) -- leaves `live`. A stub generation whose module
            // no longer resolves from any live entry is retired here,
            // synchronously with the bookkeeping: it stops being excluded
            // from `--inject-val`-style exclusion via `stub_generations` at
            // the exact moment it stops being reachable, matching a real
            // binding's eviction instead of lingering as a phantom stub.
            if self.is_stub_module(module)
                && !self.binding_index.is_module_live(&module.module_name())
            {
                self.stub_generations.remove(&module.gen().0);
                self.advance_stub_revision();
                self.retired_stub_sources.push(module.gen());
            }
            if !safe_to_release {
                continue;
            }
            // A prepared binding's root IS its adopted handle: releasing
            // the handle deregisters the root.
            if let (BoundValue { handle, .. }, Some(engine)) = (&entry.value, self.machine.as_mut())
            {
                if engine.release(*handle) {
                    released += 1;
                }
            }
        }
        released
    }

    fn release_source_instance_roots(
        &mut self,
        leases: Vec<tidepool_codegen::prepared_program::SourceInstanceLease>,
    ) -> usize {
        let mut released = 0;
        for lease in leases {
            if self
                .machine
                .as_mut()
                .is_some_and(|engine| engine.release(lease.handle()))
            {
                released += 1;
            } else {
                panic!("retired source instance has no registered machine root");
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

    /// Share `registry` with this session's machine: applied to the engine
    /// now if it exists, and to the bootstrap install otherwise.
    pub fn set_image_registry(
        &mut self,
        registry: Arc<tidepool_codegen::prepared_program::ImageRegistry>,
    ) {
        if let Some(engine) = self.machine.as_mut() {
            engine.set_image_registry(Arc::clone(&registry));
        }
        self.image_registry = Some(registry);
    }

    /// One shared immutable-code cache for this session's certified turns,
    /// including the first turn before its machine exists.
    pub(crate) fn certified_image_registry(&mut self) -> Arc<ImageRegistry> {
        if let Some(registry) = &self.image_registry {
            return Arc::clone(registry);
        }
        let registry = Arc::new(ImageRegistry::new());
        self.set_image_registry(Arc::clone(&registry));
        registry
    }

    pub(crate) fn replace_invocation_cancel(
        &mut self,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Option<Arc<AtomicBool>> {
        if let Some(engine) = self.machine.as_mut() {
            engine.set_invocation_cancel(cancel.clone());
        }
        std::mem::replace(&mut self.invocation_cancel, cancel)
    }

    pub(super) fn mount_cancelled(&mut self, realm: RealmId) -> bool {
        self.invocation_cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Acquire))
            || self
                .machine
                .as_mut()
                .is_some_and(|engine| engine.cancel_handle(realm).is_cancelled())
    }

    /// The prepared engine, once the first prepared turn has installed it.
    pub(crate) fn prepared(&self) -> Option<&PreparedEngine> {
        self.machine.as_ref()
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
    // The complete PreparedEngine moves under the stowed-XOR-running
    // discipline. Binding metadata carries identities and stays auto-Send;
    // only the machine resolves them under exclusive invocation ownership.
    // MachineLease restores the engine after a turn on another thread.

    /// Install a prepared turn's program. Every declared global resolves
    /// to a live binding by its recorded import identity.
    pub fn install_prepared(
        &mut self,
        prepared: PreparedProgram,
    ) -> Result<tidepool_codegen::prepared_program::ProgramId, PreparedRuntimeError> {
        match self.machine.as_mut() {
            None => {
                let (mut engine, program) = PreparedEngine::bootstrap_shared(
                    prepared,
                    self.nursery_size,
                    self.image_registry.clone(),
                )?;
                engine.set_invocation_cancel(self.invocation_cancel.clone());
                self.machine = Some(engine);
                self.ensure_machine_incarnation();
                Ok(program)
            }
            Some(engine) => engine.install(prepared, &self.bindings, &self.binding_index),
        }
    }

    pub(crate) fn ensure_machine_incarnation(&mut self) {
        self.machine_incarnation
            .get_or_insert_with(super::registry::fresh_session_id);
    }

    /// Step (a) of the off-checkout split install (see
    /// `PreparedEngine::snapshot_install`): `None` when this is the
    /// session's first turn (the machine does not exist yet to snapshot,
    /// so bootstrapping -- and its one inline compile -- is the caller's
    /// only option; `install_prepared` remains correct for that case).
    pub(crate) fn snapshot_install_prepared(
        &mut self,
        prepared: PreparedProgram,
    ) -> Result<Option<InstallSnapshot>, PreparedRuntimeError> {
        match self.machine.as_mut() {
            None => Ok(None),
            Some(engine) => engine
                .snapshot_install(prepared, &self.bindings, &self.binding_index)
                .map(Some),
        }
    }

    /// Step (c) of the off-checkout split install: revalidate `snapshot`'s
    /// imports and, if still current, install `compiled` (see
    /// `PreparedEngine::revalidate_and_install`). `None` both when the
    /// machine went away since the snapshot was taken (caller falls back to
    /// [`Self::install_prepared`]) and when revalidation finds a stale
    /// import (caller recompiles from a fresh snapshot, or falls back).
    pub(crate) fn revalidate_and_install_prepared(
        &mut self,
        snapshot: InstallSnapshot,
        compiled: std::sync::Arc<tidepool_codegen::prepared_program::CompiledProgram>,
    ) -> Result<Option<tidepool_codegen::prepared_program::ProgramId>, PreparedRuntimeError> {
        match self.machine.as_mut() {
            None => Ok(None),
            Some(engine) => engine.revalidate_and_install(
                snapshot,
                compiled,
                &self.bindings,
                &self.binding_index,
            ),
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
                .protected_code_export_retentions()
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
    // would invalidate every retained binding handle while leaving its lexical
    // metadata live. Session teardown settles both owners together.

    // -- persistent binding bookkeeping ----------------------------------

    /// Record a materialized value binding in the persistent binding store.
    pub fn bind(&mut self, entry: BindingEntry) -> Result<(), SessionError> {
        self.bind_in(ScopeId::ROOT, entry)
    }

    /// Module names of every live value binding — injected (`--inject-val`) AND
    /// so already-compiled fragments / closure captures keep resolving. Includes
    /// shadowed older gens.
    pub fn live_val_modules(&self) -> Vec<String> {
        if self.stub_generations.is_empty() {
            return self.binding_index.live_modules();
        }
        let stub_names: std::collections::BTreeSet<String> = self
            .stub_generations
            .iter()
            .map(|gen| SessionModule::val(Generation(*gen)).module_name())
            .collect();
        self.binding_index
            .live_modules()
            .into_iter()
            .filter(|module| !stub_names.contains(module))
            .collect()
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

    #[must_use]
    pub fn next_lib_module(&self) -> Option<SessionModule> {
        self.lib.as_ref().map(SessionLib::next_module)
    }

    /// Snapshot the exact source-side environment visible from `scope` so a
    /// caller can release its machine borrow before invoking GHC. Returns
    /// `None` for a dead scope or a session without a declaration/include
    /// persistent binding store.
    pub fn compile_view_in(&self, scope: ScopeId) -> Option<SessionCompileView> {
        let cached = self.scoped_compile_view_in(scope)?;
        let injected_values = self
            .bindings
            .live_modules()
            .filter(|module| !self.is_stub_module(*module))
            .collect();
        Some(
            cached
                .view
                .with_request_inventory(injected_values, self.val_gen.next()),
        )
    }

    pub(super) fn compile_view_digest_in(&self, scope: ScopeId) -> Option<[u8; 32]> {
        self.scoped_compile_view_in(scope).map(|view| view.digest)
    }

    fn scoped_compile_view_in(&self, scope: ScopeId) -> Option<Arc<CachedCompileView>> {
        if !self.scopes.is_live(scope) {
            return None;
        }
        let lib = self.lib.as_ref()?;
        let key = self
            .bindings
            .scope_witness(&self.scopes, scope)
            .filter(|_| self.stub_revision != 0)
            .map(|bindings| CompileViewKey {
                library: lib.compile_view_identity,
                tip: lib.scope_tip(scope),
                bindings,
                stubs: self.stub_revision,
                public_epoch: self
                    .public_visibility_epochs
                    .get(&scope)
                    .copied()
                    .unwrap_or(0),
            });
        if let Some(key) = &key {
            if let Some(cached) = self
                .compile_views
                .lock()
                .get(&scope)
                .filter(|cached| cached.key.as_ref() == Some(key))
            {
                return Some(cached.clone());
            }
        }
        let view = self.build_scoped_compile_view(scope, lib);
        let (digest, bytes_hashed) = view.admission_commitment();
        #[cfg(test)]
        self.compile_view_bytes_hashed
            .fetch_add(bytes_hashed, std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        let _ = bytes_hashed;
        let cached = Arc::new(CachedCompileView { digest, view, key });
        if cached.key.is_some() {
            self.compile_views.lock().insert(scope, cached.clone());
        }
        Some(cached)
    }

    fn build_scoped_compile_view(&self, scope: ScopeId, lib: &SessionLib) -> SessionCompileView {
        let visible_entries = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .collect::<Vec<_>>();
        let visible_values = visible_entries
            .iter()
            .map(|(_, entry)| entry.module)
            .collect();
        // A generated interface can also carry unpublished helper binders.
        let mut grouped = HashMap::<SessionModule, Vec<String>>::new();
        for (name, entry) in &visible_entries {
            grouped
                .entry(entry.module)
                .or_default()
                .push(name.0.clone());
        }
        let visible_value_names = grouped.into_iter().collect();
        let reachable_values = self
            .bindings
            .scope_reachable_modules(&self.scopes, scope)
            .filter(|module| !self.is_stub_module(*module))
            .collect();
        let mut shadowing = lib
            .current_declarations_in(scope)
            .into_iter()
            .map(|(item, _)| item)
            .collect::<Vec<_>>();
        shadowing.extend(
            visible_entries
                .iter()
                .map(|(name, _)| super::ExportItem::Value {
                    name: name.0.clone(),
                }),
        );
        SessionCompileView {
            session: lib.session_id(),
            lexical_scope: scope,
            injected_values: Vec::new(),
            next_value_generation: self.val_gen.next(),
            request_context: None,
            projection: Arc::new(super::view::CompileViewProjection {
                root: PathBuf::from(lib.include_dir()),
                persistent_imports: self.workbench_imports_in(scope),
                library: lib.current_module_in(scope).map(|original| {
                    match lib.current_declaration_projection_in(scope) {
                        Some(projection) => super::view::CompileLibrary::Certified {
                            original,
                            projection,
                        },
                        None => match lib.current_recovered_declaration_in(scope) {
                            Some(evidence) => {
                                super::view::CompileLibrary::Recovered { original, evidence }
                            }
                            None => super::view::CompileLibrary::Source(original),
                        },
                    }
                }),
                visible_values,
                visible_value_names,
                reachable_values,
                shadowing,
                staged_hiding: Vec::new(),
                exact_context: lib.current_exact_context_in(scope),
            }),
        }
        .canonicalize()
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

    pub fn exact_exports_in_namespace(
        &self,
        scope: ScopeId,
        namespace: tidepool_toolchain::declaration_join::ExportNamespace,
        heads: &[&str],
    ) -> Result<ExactExportSurface, ExactExportError> {
        if !self.scopes.is_live(scope) {
            return Err(ExactExportError::DeadScope(scope));
        }
        let lib = self
            .lib
            .as_ref()
            .ok_or(ExactExportError::NoDeclarationPlane)?;
        lib.exact_exports_in_namespace(scope, namespace, heads)
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
    pub fn define_scoped(
        &mut self,
        decl_texts: &[&str],
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<Generation, SessionError> {
        self.define_scoped_in(ScopeId::ROOT, decl_texts, settlement)
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
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<Generation, SessionError> {
        self.define_scoped_with_imports_in(scope, decl_texts, &SourceImports::new(), settlement)
    }

    /// Scoped declaration commit with frontend-owned persistent imports.
    /// Trusted imports participate in this declaration but are not recorded as
    /// user-authored state; callers provide them again for later turns.
    pub fn define_scoped_with_imports_in(
        &mut self,
        scope: ScopeId,
        decl_texts: &[&str],
        external: &SourceImports,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<Generation, SessionError> {
        self.commit_declarations_in(scope, decl_texts, external, settlement)
            .map(|receipt| receipt.generation)
    }

    /// The shared prefix of [`Self::stage_declarations_in`] and
    /// [`Self::render_declaration_candidate_in`]: the persistent imports a
    /// candidate compiles against and the exact live-value environment
    /// visible from `scope` once `receipt`'s names have taken over — the
    /// value plane's half of what makes a later `adopt` stale, alongside the
    /// declaration plane's own generation/tip check.
    pub(super) fn declaration_staging_context_in(
        &self,
        scope: ScopeId,
        receipt: &super::DeclarationReceipt,
        external: &SourceImports,
    ) -> Result<DeclarationStagingContext, SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let mut persistent_imports = external.clone();
        persistent_imports.extend(&self.workbench_imports_in(scope));
        let replaced_names = receipt
            .items
            .iter()
            .flat_map(super::ExportItem::value_names)
            .collect::<Vec<_>>();
        let visible_entries = self.bindings.iter_current_in(&self.scopes, scope);
        let visible_values = visible_entries
            .iter()
            .map(|(_, entry)| (entry.id, entry.module.module_name()))
            .collect::<Vec<_>>();
        let import_modules = value_import_specs(
            visible_entries
                .iter()
                .filter(|(name, _)| {
                    !replaced_names
                        .iter()
                        .any(|replaced| replaced == &name.0.as_str())
                })
                .map(|(name, entry)| (name.0.clone(), entry.module)),
        );
        Ok((persistent_imports, import_modules, visible_values))
    }

    /// Render and validate the exact next declaration module against the
    /// actor's source layer without changing the live scope tip or binding
    /// store. Attached-v2 sessions first burn the module identity durably.
    pub fn stage_declarations_in(
        &mut self,
        scope: ScopeId,
        receipt: &super::DeclarationReceipt,
        external: &SourceImports,
        source_layer: &[PathBuf],
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<super::StagedDeclaration, SessionError> {
        let (persistent_imports, import_modules, visible_values) =
            self.declaration_staging_context_in(scope, receipt, external)?;
        #[allow(clippy::expect_used, reason = "decl plane present")]
        let live_modules = self.live_val_modules();
        let lib = self.lib.as_mut().expect("decl plane present");
        let candidate = lib
            .render_admitted_candidate_in(
                scope,
                &persistent_imports,
                receipt,
                &import_modules,
                &live_modules,
            )?
            .with_source_layer(source_layer);
        super::validate_declaration_candidate(candidate, &lib.root, settlement)
            .map(|staged| staged.with_visible_values(visible_values))
    }

    /// The pure half of a split cell preparation's declaration staging:
    /// render the next candidate module and capture the exact live-value
    /// environment it must still match at adopt time. Attached-v2 sessions
    /// reserve its identity durably before the candidate leaves the checkout;
    /// the render itself invokes no compiler. Pair with
    /// [`super::validate_declaration_candidate`] off-checkout (attach the
    /// returned `visible_values` to its `StagedDeclaration` via
    /// [`super::StagedDeclaration::with_visible_values`]) and
    /// [`Self::adopt_staged_declaration_in`] on a later checkout.
    pub fn render_declaration_candidate_in(
        &mut self,
        scope: ScopeId,
        receipt: &super::DeclarationReceipt,
        external: &SourceImports,
    ) -> Result<(DeclarationCandidateRender, Vec<(SessionVarId, String)>), SessionError> {
        let (persistent_imports, import_modules, visible_values) =
            self.declaration_staging_context_in(scope, receipt, external)?;
        #[allow(clippy::expect_used, reason = "decl plane present")]
        let live_modules = self.live_val_modules();
        let lib = self.lib.as_mut().expect("decl plane present");
        let candidate = lib.render_admitted_candidate_in(
            scope,
            &persistent_imports,
            receipt,
            &import_modules,
            &live_modules,
        )?;
        Ok((candidate, visible_values))
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
        let next_epoch = self.prepare_public_visibility_advance(scope)?;
        let replaced_names = staged.replaced_value_names();
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
            .map(|(_, entry)| (entry.id, entry.module.module_name()))
            .collect::<Vec<_>>();
        let captured_values = visible_values
            .iter()
            .map(|(id, _)| id.var())
            .collect::<Vec<_>>();
        let items = staged.items().to_vec();
        let source_origin = staged
            .certified_authored
            .as_ref()
            .map(|certificate| {
                self.bindings.prepare_source_owner_origin_in(
                    &self.scopes,
                    scope,
                    certificate.evidence.product().owner().clone(),
                )
            })
            .transpose()
            .map_err(SessionError::InvalidPublicBindingPromotion)?;
        let admitted = self
            .lib
            .as_mut()
            .ok_or(SessionError::StaleStagedDeclaration)?
            .admit_staged_declaration_in(staged, &visible_values)?;
        admitted
            .map_commit(|generation| {
                if let Some(source_origin) = source_origin {
                    self.bindings.commit_source_owner_origin(source_origin);
                }
                self.bindings.preserve_observations(&captured_values);
                for name in &replaced_names {
                    self.bindings.remove_current_in(scope, name);
                }
                self.public_visibility_epochs.insert(scope, next_epoch);
                DeclarationPlaneCommit {
                    generation,
                    module: SessionModule::lib(generation),
                    items,
                    evicted_values,
                }
            })
            .into_result()
    }

    pub fn discard_staged_declaration(&self, staged: &super::StagedDeclaration) {
        if let Some(lib) = &self.lib {
            lib.discard_staged(staged);
        }
    }

    /// Retract `name` from the persistent declaration environment (its binding migrated to the
    /// binding store). No-op when `name` is not a current declaration head.
    pub fn retract(
        &mut self,
        name: &str,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<(), SessionError> {
        self.retract_in(ScopeId::ROOT, name, settlement)
    }

    /// Scoped [`Self::retract`]: retract `name` from `scope`'s decl tip only.
    /// `retract(n) == retract_in(ScopeId::ROOT, n)`.
    ///
    /// A name lives in at most one store per scope, so a child binding
    /// `helper` must not retract the parent's persistent declaration environment
    /// `helper` — the parent's name is still the parent's, and nothing ever
    /// walks downward.
    pub fn retract_in(
        &mut self,
        scope: ScopeId,
        name: &str,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<(), SessionError> {
        self.retract_many_in(scope, &[name.to_string()], settlement)
    }

    /// Retract current declaration heads in one generation while retaining
    /// their authored source origins in this scope's source-instance environment.
    /// Names without a current declaration head are ignored.
    pub fn retract_many_in(
        &mut self,
        scope: ScopeId,
        names: &[String],
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<(), SessionError> {
        self.retract_heads_in(scope, names, None, settlement)
    }

    /// Retract a set of declaration heads through one durable declaration
    /// generation. Used by set materialization so a later name cannot fail
    /// after an earlier name has already entered the persistent binding store.
    fn retract_value_heads_in(
        &mut self,
        scope: ScopeId,
        names: &[String],
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<(), SessionError> {
        self.retract_heads_in(
            scope,
            names,
            Some(tidepool_toolchain::declaration_join::ExportNamespace::Value),
            settlement,
        )
    }

    fn retract_heads_in(
        &mut self,
        scope: ScopeId,
        names: &[String],
        namespace: Option<tidepool_toolchain::declaration_join::ExportNamespace>,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<(), SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let Some(lib) = self.lib.as_mut() else {
            return Ok(());
        };
        if let Some(staged) = lib.prepare_retraction_in(scope, names, namespace, settlement)? {
            let visible_values = self
                .bindings
                .iter_current_in(&self.scopes, scope)
                .into_iter()
                .map(|(_, entry)| (entry.id, entry.module.module_name()))
                .collect();
            self.adopt_staged_declaration_in(staged.with_visible_values(visible_values))?;
        }
        Ok(())
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
            lib.inherit_source_context(parent, child);
        }
        self.bindings.seed_scope(&self.scopes, parent, child);
        Some(child)
    }

    /// Capture the exact declaration and value environment in an independent
    /// lexical root. It may outlive the actor whose scope supplied the view.
    /// Value leases include shadowed ancestor generations used by inherited
    /// declaration modules; no heap objects are copied or forced.
    pub fn mint_detached_scope(&mut self, parent: ScopeId) -> Option<ScopeId> {
        if !self.scopes.is_live(parent) {
            return None;
        }
        let root = self.scopes.mint_isolated();
        if let Some(lib) = self.lib.as_mut() {
            let inherited = lib.scope_tip(parent);
            lib.seed_scope(root, inherited);
            lib.inherit_source_context(parent, root);
        }
        self.bindings
            .seed_detached_scope(&self.scopes, parent, root);
        Some(root)
    }

    /// Keep closure dependencies from an explicitly imported authored entry
    /// while its branch runs in another captured environment.
    pub fn retain_scope_dependencies(&mut self, source: ScopeId, target: ScopeId) -> bool {
        self.bindings
            .retain_scope_dependencies(&self.scopes, source, target)
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

    pub(super) fn advance_public_visibility(&mut self, scope: ScopeId) {
        let next = self
            .prepare_public_visibility_advance(scope)
            .expect("public visibility epoch exhausted");
        self.public_visibility_epochs.insert(scope, next);
    }

    /// Reserve map capacity and check exhaustion before an authoritative
    /// manifest rename. Finalization only updates this existing entry.
    pub(super) fn prepare_public_visibility_advance(
        &mut self,
        scope: ScopeId,
    ) -> Result<u64, SessionError> {
        let path = self
            .lib
            .as_ref()
            .map_or_else(PathBuf::new, |lib| lib.root.clone());
        self.public_visibility_epochs
            .entry(scope)
            .or_default()
            .checked_add(1)
            .ok_or_else(|| SessionError::RecoveryManifest {
                path,
                detail: "public visibility epoch exhausted".into(),
            })
    }

    pub(super) fn commit_public_visibility_advance(&mut self, scope: ScopeId, epoch: u64) {
        self.public_visibility_epochs.insert(scope, epoch);
    }

    /// Full internal identity of the paired lexical view. Observation may
    /// filter host names, but those filters never authorize publication.
    pub fn public_visibility_snapshot_in(
        &self,
        scope: ScopeId,
    ) -> Option<super::PublicVisibilitySnapshot> {
        if !self.scopes.is_live(scope) {
            return None;
        }
        let lib = self.lib.as_ref()?;
        let source_selection = self.bindings.scope_snapshot(&self.scopes, scope).ok()?;
        let mut bindings: Vec<_> = self
            .bindings
            .iter_current_in(&self.scopes, scope)
            .into_iter()
            .map(|(name, entry)| (name.0.clone(), entry.id))
            .collect();
        bindings.sort_by(|a, b| a.0.cmp(&b.0));
        let mut source_instances: Vec<_> = self
            .bindings
            .source_instance_keys_in(&self.scopes, scope)
            .into_iter()
            .collect();
        source_instances.sort();
        Some(super::PublicVisibilitySnapshot {
            scope,
            epoch: self
                .public_visibility_epochs
                .get(&scope)
                .copied()
                .unwrap_or(0),
            declaration_tip: lib.scope_tip(scope),
            machine_incarnation: self.machine_incarnation,
            bindings,
            source_instances,
            source_selection,
        })
    }

    fn recovery_source_instances(
        &self,
        keys: impl IntoIterator<Item = SourceLeaseKey>,
    ) -> Result<Vec<super::recovery::RecoveryPublicSourceInstance>, SessionError> {
        keys.into_iter()
            .map(|key| {
                let incarnation =
                    self.machine_incarnation
                        .ok_or_else(|| SessionError::RecoveryManifest {
                            path: self.lib().root.clone(),
                            detail: "native source lease has no owning machine incarnation".into(),
                        })?;
                let identity = key.binder.binder.clone();
                Ok(super::recovery::RecoveryPublicSourceInstance {
                    machine_incarnation: incarnation.0,
                    instance: key.instance.raw(),
                    module_version: key.binder.version.0,
                    binder: super::recovery::RecoverySourceIdentity {
                        unit: identity.unit,
                        module: identity.module,
                        namespace: identity.namespace,
                        occurrence: identity.occurrence,
                        record_parent: identity.record_parent,
                    },
                })
            })
            .collect()
    }

    /// Admit one exact actor incarnation as the durable public owner of a
    /// minted lexical scope. The scope forest is the liveness authority;
    /// SessionLib only records the checked owner-to-scope association.
    pub fn bind_durable_public_scope(
        &mut self,
        owner: RecoveryPublicOwner,
        scope: ScopeId,
    ) -> Result<(), SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        self.lib
            .as_mut()
            .ok_or(SessionError::MissingDeclarationLibrary)?
            .bind_durable_public_scope(owner, scope)
    }

    /// Remint one persisted exact actor incarnation's public declaration scope.
    /// Heap bindings and mutable source instances are deliberately absent; the
    /// durable winning names remain loss metadata and never reveal old values.
    pub fn recover_public_scope(
        &mut self,
        owner: &RecoveryPublicOwner,
    ) -> Result<ScopeId, SessionError> {
        let lib = self
            .lib
            .as_ref()
            .ok_or(SessionError::MissingDeclarationLibrary)?;
        if !lib.validate_recovered_public_owner(owner)? {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        let state = lib
            .durable_graph
            .as_ref()
            .ok_or_else(|| SessionError::RecoveryManifest {
                path: lib.root.clone(),
                detail: "exact recovery graph is not attached".into(),
            })?;
        if lib.durable_public_scopes.contains_key(owner) {
            return Err(SessionError::RecoveryManifest {
                path: state.path.clone(),
                detail: "persisted public owner already has a live scope".into(),
            });
        }
        let surface = state
            .graph
            .public_surfaces()
            .find(|surface| &surface.owner == owner)
            .ok_or_else(|| SessionError::RecoveryManifest {
                path: state.path.clone(),
                detail: "persisted public owner path or incarnation does not match".into(),
            })?;
        let generation = surface.declaration_root.unwrap_or(Generation(0));
        let epoch = surface.epoch;
        let recovered_context = lib.recovered_public_compiler_context(owner)?;
        let published_sources = recovered_context
            .as_ref()
            .map(|context| context.published_source_original_selections())
            .transpose()?
            .unwrap_or_default();
        if generation != Generation(0)
            && (recovered_context.is_none() || lib.log.recovered_at(generation).is_none())
        {
            return Err(SessionError::RecoveryManifest {
                path: state.path.clone(),
                detail: "persisted declaration root lacks its exact public compiler selection"
                    .into(),
            });
        }
        let scope = self.mint_isolated_scope();
        let lib = self.lib.as_mut().expect("declaration library checked");
        lib.tips.insert(
            scope,
            super::ScopeDeclarationState {
                tip: generation,
                exact_context: recovered_context,
                published_sources,
            },
        );
        if let Err(error) = lib.bind_durable_public_scope(owner.clone(), scope) {
            self.retire_scope(scope);
            return Err(error);
        }
        self.public_visibility_epochs.insert(scope, epoch);
        Ok(scope)
    }

    pub fn validate_recovered_public_owner(
        &self,
        owner: &RecoveryPublicOwner,
    ) -> Result<bool, SessionError> {
        self.lib
            .as_ref()
            .ok_or(SessionError::MissingDeclarationLibrary)?
            .validate_recovered_public_owner(owner)
    }

    /// Freeze the actual native dependencies of the host's fresh root driver
    /// before admitting its actor. This does not authorize an actor or a
    /// transfer; the retained run owner and actor-journal proof remain required.
    pub fn seal_recovery_initialization_scope(
        &mut self,
        scope: ScopeId,
    ) -> Result<(), SessionError> {
        use super::RecoveryInitializationFailure as Failure;
        let snapshot = self
            .public_visibility_snapshot_in(scope)
            .ok_or(SessionError::DeadScope(scope))?;
        self.validate_fresh_recovery_scope(&snapshot)?;
        let state = self
            .lib()
            .durable_graph
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        let manifest_owner = state
            .owner
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        manifest_owner.validate_owner()?;
        if state.unconfirmed.is_some() {
            return Err(SessionError::InvalidRecoveryInitialization {
                scope,
                reason: Failure::UnconfirmedPublication,
            });
        }
        if !self.lib().durable_public_scopes.is_empty() {
            return Err(SessionError::InvalidRecoveryInitialization {
                scope,
                reason: Failure::ExistingPublicOwner,
            });
        }
        let sources = self.bindings.source_instances_in(&self.scopes, scope);
        self.validate_recovery_native_dependencies(scope, &sources)?;
        if self.recovery_initialization.is_some() {
            return self.validate_recovery_initialization(&snapshot);
        }
        self.recovery_initialization = Some(RecoveryInitialization {
            library: self.lib().compile_view_identity,
            manifest_owner: Arc::clone(manifest_owner),
            scope,
            owner_epoch: self.admission_owner().epoch(),
            machine_incarnation: self.machine_incarnation,
            sources,
        });
        Ok(())
    }

    fn validate_fresh_recovery_scope(
        &self,
        snapshot: &super::PublicVisibilitySnapshot,
    ) -> Result<(), SessionError> {
        use super::RecoveryInitializationFailure as Failure;
        let retained_values = self
            .bindings
            .scope_reachable_binding_ids(&self.scopes, snapshot.scope)
            .len();
        let reason = if snapshot.scope == ScopeId::ROOT {
            Some(Failure::RootScope)
        } else if snapshot.declaration_tip != Generation(0) {
            Some(Failure::DeclarationTip(snapshot.declaration_tip))
        } else if !snapshot.bindings.is_empty() {
            Some(Failure::ValueBindings(snapshot.bindings.len()))
        } else if retained_values != 0 {
            Some(Failure::RetainedValueBindings(retained_values))
        } else {
            None
        };
        match reason {
            Some(reason) => Err(SessionError::InvalidRecoveryInitialization {
                scope: snapshot.scope,
                reason,
            }),
            None => Ok(()),
        }
    }

    fn validate_recovery_native_dependencies(
        &self,
        scope: ScopeId,
        sources: &[SourceInstanceLease],
    ) -> Result<(), SessionError> {
        if !sources.is_empty()
            && (self.machine_incarnation.is_none()
                || self.machine.as_ref().is_none_or(|engine| {
                    sources.iter().any(|source| {
                        engine.prepared_handle_of(source.handle().raw()) != Some(source.handle())
                    })
                }))
        {
            return Err(SessionError::InvalidRecoveryInitialization {
                scope,
                reason: super::RecoveryInitializationFailure::UnavailableNativeDependency,
            });
        }
        Ok(())
    }

    fn validate_recovery_initialization(
        &self,
        snapshot: &super::PublicVisibilitySnapshot,
    ) -> Result<(), SessionError> {
        use super::RecoveryInitializationFailure as Failure;
        let fail = |reason| SessionError::InvalidRecoveryInitialization {
            scope: snapshot.scope,
            reason,
        };
        let Some(initialization) = &self.recovery_initialization else {
            return if snapshot.source_instances.is_empty() {
                Ok(())
            } else {
                Err(fail(Failure::MissingSeal))
            };
        };
        if initialization.library != self.lib().compile_view_identity
            || self
                .lib()
                .durable_graph
                .as_ref()
                .and_then(|graph| graph.owner.as_ref())
                .is_none_or(|owner| !Arc::ptr_eq(owner, &initialization.manifest_owner))
            || initialization.scope != snapshot.scope
            || initialization.owner_epoch != self.admission_owner().epoch()
            || initialization.machine_incarnation != self.machine_incarnation
        {
            return Err(fail(Failure::ForeignSeal));
        }
        let sources = self
            .bindings
            .source_instances_in(&self.scopes, snapshot.scope);
        if sources.len() != initialization.sources.len()
            || sources
                .iter()
                .zip(&initialization.sources)
                .any(|(current, original)| {
                    current.instance() != original.instance()
                        || current.binder() != original.binder()
                        || current.handle() != original.handle()
                        || current.owner() != original.owner()
                        || current.original_ordinal() != original.original_ordinal()
                        || current.value() != original.value()
                        || current.entry_signature() != original.entry_signature()
                })
        {
            return Err(fail(Failure::ChangedNativeDependencies));
        }
        self.validate_recovery_native_dependencies(snapshot.scope, &initialization.sources)
    }

    /// Durably initialize one actor's exact captured public surface before
    /// application readiness. Existing owners and declaration products remain
    /// unchanged; heap values are recorded as recovery loss metadata.
    pub fn initialize_durable_public_scope(
        &mut self,
        owner: RecoveryPublicOwner,
        target: ScopeId,
    ) -> Result<PublicManifestCommit, SessionError> {
        let snapshot = self
            .public_visibility_snapshot_in(target)
            .ok_or(SessionError::DeadScope(target))?;
        if target == ScopeId::ROOT {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        let lib = self
            .lib
            .as_ref()
            .ok_or(SessionError::MissingDeclarationLibrary)?;
        let state = lib
            .durable_graph
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        let invalid = |detail: String| SessionError::RecoveryManifest {
            path: state.path.clone(),
            detail,
        };
        let retained = state.owner.as_ref().ok_or_else(|| {
            invalid("initial durable public scope requires its configured run owner".into())
        })?;
        retained.validate_owner()?;
        if state.unconfirmed.is_some()
            || lib
                .durable_public_scopes
                .get(&owner)
                .is_some_and(|scope| *scope != target)
            || lib
                .durable_public_scopes
                .iter()
                .any(|(other, scope)| other != &owner && *scope == target)
            || snapshot.declaration_tip != Generation(0)
                && (!state
                    .graph
                    .nodes()
                    .any(|node| node.id == snapshot.declaration_tip)
                    || lib
                        .log
                        .joined_context_at(snapshot.declaration_tip)
                        .is_none())
        {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        let root = state
            .path
            .parent()
            .expect("attached canonical manifest parent");
        let original = match std::fs::read(&state.path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(invalid(error.to_string())),
        };
        if let Some(bytes) = &original {
            let read = super::recovery::read_v2_bytes(
                &state.path,
                root,
                bytes,
                super::recovery::RecoveryReadPurpose::Metadata,
            )
            .map_err(|error| invalid(error.to_string()))?
            .ok_or(SessionError::WrongPublicManifestTicket)?;
            if !read.artifact_losses.is_empty() || read.graph.checksum() != state.graph.checksum() {
                return Err(SessionError::WrongPublicManifestTicket);
            }
        } else if state.graph.high_water() != Generation(0)
            || !state.graph.nodes().next().is_none()
            || !state.graph.artifacts().next().is_none()
            || !state.graph.public_surfaces().next().is_none()
        {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        let bindings = snapshot
            .bindings
            .iter()
            .map(|(name, id)| super::recovery::RecoveryPublicBinding {
                name: name.clone(),
                owner: super::recovery::RecoveryBindingId {
                    session: lib.id.0,
                    variable: id.raw(),
                },
            })
            .collect::<Vec<_>>();
        let sources = self.recovery_source_instances(snapshot.source_instances.iter().cloned())?;
        let exact_context = lib.current_exact_context_in(target);
        let compiler_context = exact_context
            .as_ref()
            .map(|context| super::recovery::RecoveryCompilerContext::capture(context))
            .unwrap_or_default();
        if let Some(surface) = state
            .graph
            .public_surfaces()
            .find(|surface| surface.owner == owner)
        {
            if lib.durable_public_scopes.get(&owner) != Some(&target)
                || surface.declaration_root
                    != (snapshot.declaration_tip != Generation(0))
                        .then_some(snapshot.declaration_tip)
                || surface.epoch != snapshot.epoch
                || surface.bindings != bindings
                || surface.source_instances != sources
                || surface.compiler_context != compiler_context
            {
                return Err(SessionError::WrongPublicManifestTicket);
            }
            return Ok(PublicManifestCommit::Durable);
        }
        // Allocate all owner/tip entries before the visible rename. Finalization
        // swaps these prepared maps and updates an already reserved epoch entry.
        let mut public_scopes = lib.durable_public_scopes.clone();
        public_scopes.insert(owner.clone(), target);
        let mut tips = lib.tips.clone();
        tips.insert(
            target,
            super::ScopeDeclarationState {
                tip: snapshot.declaration_tip,
                exact_context: exact_context.clone(),
                published_sources: lib.published_source_selections_in(target).to_vec(),
            },
        );
        let (snapshot_graph, compiler_context) =
            super::recovery_hydration::materialize_public_compiler_context(
                &state.graph,
                root,
                exact_context.as_ref(),
            )?;
        let staged = super::recovery::stage_public_visibility_v2(
            &state.path,
            root,
            &snapshot_graph,
            owner.clone(),
            0,
            bindings,
            sources,
            (snapshot.declaration_tip != Generation(0)).then_some(snapshot.declaration_tip),
            compiler_context,
        )
        .map_err(|error| invalid(error.to_string()))?;
        retained.validate_owner()?;
        let current = match std::fs::read(&state.path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(invalid(error.to_string())),
        };
        if current != original {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        self.public_visibility_epochs
            .try_reserve(1)
            .map_err(|error| invalid(error.to_string()))?;
        let lib = self
            .lib
            .as_mut()
            .expect("initial owner preflight checked library");
        let outcome = lib.publish_recovery_manifest(staged);
        let state = lib
            .durable_graph
            .as_mut()
            .expect("initial owner preflight checked manifest");
        let commit = match outcome {
            super::recovery::RecoveryPublishOutcome::BeforeRename { detail, .. } => {
                return Ok(PublicManifestCommit::BeforeRename { detail })
            }
            super::recovery::RecoveryPublishOutcome::Durable { graph, .. } => {
                state.graph = graph;
                PublicManifestCommit::Durable
            }
            super::recovery::RecoveryPublishOutcome::PublishedDurabilityUnconfirmed {
                graph,
                publication,
                detail,
            } => {
                state.graph = graph;
                state.unconfirmed.set(publication);
                PublicManifestCommit::PublishedDurabilityUnconfirmed { detail }
            }
        };
        lib.durable_public_scopes = public_scopes;
        lib.tips = tips;
        self.public_visibility_epochs.insert(target, 1);
        self.recovery_initialization = None;
        Ok(commit)
    }

    /// Transfer the retained root to the exact durably admitted successor.
    /// The run owner and actor journal are checked before staging and again
    /// before rename. No heap values or mutable source instances are reminted.
    pub fn transfer_recovered_public_owner(
        &mut self,
        predecessor: &RecoveryPublicOwner,
        successor: RecoveryPublicOwner,
        target: ScopeId,
        authority: Arc<dyn super::RecoverySuccessorAuthority>,
    ) -> Result<PublicManifestCommit, SessionError> {
        if !self.scopes.is_live(target) {
            return Err(SessionError::DeadScope(target));
        }
        let snapshot = self
            .public_visibility_snapshot_in(target)
            .ok_or(SessionError::MissingDeclarationLibrary)?;
        self.validate_fresh_recovery_scope(&snapshot)?;
        self.validate_recovery_initialization(&snapshot)?;
        let lib = self
            .lib
            .as_ref()
            .ok_or(SessionError::MissingDeclarationLibrary)?;
        let state = lib
            .durable_graph
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        let invalid = |detail: String| SessionError::RecoveryManifest {
            path: state.path.clone(),
            detail,
        };
        let owner = state.owner.as_ref().ok_or_else(|| {
            invalid("root successor transfer requires the configured manifest owner".into())
        })?;
        owner.validate_owner()?;
        use super::RecoveryInitializationFailure as Failure;
        let reason = if state.unconfirmed.is_some() {
            Some(Failure::UnconfirmedPublication)
        } else if predecessor == &successor {
            Some(Failure::RepeatedIncarnation)
        } else if !lib.durable_public_scopes.is_empty() {
            Some(Failure::ExistingPublicOwner)
        } else if state
            .graph
            .public_surfaces()
            .any(|surface| surface.owner == successor)
        {
            Some(Failure::ExistingSuccessor)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(SessionError::InvalidRecoveryInitialization {
                scope: target,
                reason,
            });
        }
        let surface = state
            .graph
            .surface(predecessor)
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        let generation = surface.declaration_root.unwrap_or(Generation(0));
        let recovered_context = lib.recovered_public_compiler_context(predecessor)?;
        let published_sources = recovered_context
            .as_ref()
            .map(|context| context.published_source_original_selections())
            .transpose()?
            .unwrap_or_default();
        if generation != Generation(0)
            && (recovered_context.is_none() || lib.log.recovered_at(generation).is_none())
        {
            return Err(invalid(
                "successor declaration root lacks its exact public compiler selection".into(),
            ));
        }
        let root = state
            .path
            .parent()
            .ok_or_else(|| invalid("manifest has no canonical run parent".into()))?;
        if !authority
            .validate_successor(root, predecessor, &successor, lib.id, target)
            .map_err(|error| invalid(error.to_string()))?
        {
            return Err(invalid(
                "actual actor journal or placement refuses successor transfer".into(),
            ));
        }
        let current_bytes =
            std::fs::read(&state.path).map_err(|error| invalid(error.to_string()))?;
        let current = super::recovery::read_v2_bytes(
            &state.path,
            root,
            &current_bytes,
            super::recovery::RecoveryReadPurpose::Metadata,
        )
        .map_err(|error| invalid(error.to_string()))?
        .ok_or_else(|| invalid("current manifest is not the exact retained graph".into()))?;
        if !current.artifact_losses.is_empty()
            || current.graph.checksum() != state.graph.checksum()
            || current.graph.high_water() != state.graph.high_water()
        {
            return Err(invalid("manifest changed before successor transfer".into()));
        }
        let epoch = surface
            .epoch
            .checked_add(1)
            .ok_or_else(|| invalid("public visibility epoch exhausted".into()))?;
        let mut graph = state.graph.candidate();
        let mut replacement = surface.clone();
        replacement.owner = successor.clone();
        replacement.epoch = epoch;
        graph.remove_surface(predecessor);
        graph.replace_surface(replacement);
        let graph = graph.seal().map_err(|error| invalid(error.to_string()))?;
        let staged = super::recovery::stage_v2(&state.path, root, graph)
            .map_err(|error| invalid(error.to_string()))?;
        owner.validate_owner()?;
        if std::fs::read(&state.path).map_err(|error| invalid(error.to_string()))? != current_bytes
            || !authority
                .validate_successor(root, predecessor, &successor, lib.id, target)
                .map_err(|error| invalid(error.to_string()))?
        {
            return Err(invalid(
                "durable owner evidence changed before successor rename".into(),
            ));
        }
        self.public_visibility_epochs
            .try_reserve(1)
            .map_err(|error| invalid(error.to_string()))?;
        // Admission epoch advancement is preflighted before the durable write.
        let admission_epoch = self.prepare_execution_admission_epoch_advance()?;
        let lib = self
            .lib
            .as_mut()
            .expect("transfer preflight checked declaration library");
        let outcome = lib.publish_recovery_manifest(staged);
        let state = lib
            .durable_graph
            .as_mut()
            .expect("transfer preflight checked manifest");
        let committed = match outcome {
            super::recovery::RecoveryPublishOutcome::BeforeRename { detail, .. } => {
                return Ok(PublicManifestCommit::BeforeRename { detail })
            }
            super::recovery::RecoveryPublishOutcome::Durable { graph, .. } => {
                state.graph = graph;
                PublicManifestCommit::Durable
            }
            super::recovery::RecoveryPublishOutcome::PublishedDurabilityUnconfirmed {
                graph,
                publication,
                detail,
            } => {
                state.graph = graph;
                state.unconfirmed.set(publication);
                PublicManifestCommit::PublishedDurabilityUnconfirmed { detail }
            }
        };
        // Preflight proved this fresh scope has exactly G0. It may already
        // have an explicit G0 entry from ordinary scope minting, so transfer
        // must set the recovered tip rather than use inheritance-only seeding.
        lib.tips.insert(
            target,
            super::ScopeDeclarationState {
                tip: generation,
                exact_context: recovered_context,
                published_sources,
            },
        );
        lib.durable_public_scopes.insert(successor, target);
        self.public_visibility_epochs.insert(target, epoch);
        self.invalidate_execution_admissions_after_owner_transfer(admission_epoch);
        self.recovery_initialization = None;
        Ok(committed)
    }

    /// Admit bootstrap only against the exact already published actor surface.
    pub fn begin_durable_public_bootstrap(
        &self,
        owner: RecoveryPublicOwner,
        scope: ScopeId,
    ) -> Result<DurablePublicBootstrap, SessionError> {
        let initial = self.validate_durable_public_admission(&owner, scope)?;
        let lib = self.lib();
        let surface = lib
            .durable_graph
            .as_ref()
            .expect("validated graph")
            .graph
            .public_surfaces()
            .find(|surface| surface.owner == owner)
            .expect("validated surface")
            .clone();
        Ok(DurablePublicBootstrap {
            admission_owner: self.admission_owner().clone(),
            admission_epoch: self.admission_owner().epoch(),
            session: lib.id,
            owner,
            initial,
            surface,
        })
    }

    /// Publish native bootstrap's actual binding/source view under its original
    /// public owner. Declaration publication remains the declaration owner.
    pub fn publish_durable_public_bootstrap(
        &mut self,
        bootstrap: DurablePublicBootstrap,
    ) -> Result<PublicManifestCommit, SessionError> {
        use super::DurablePublicAdmissionFailure as Failure;
        let scope = bootstrap.initial.scope;
        let fail = |reason| SessionError::InvalidDurablePublicAdmission {
            owner: bootstrap.owner.clone(),
            scope,
            reason,
        };
        let current = self
            .public_visibility_snapshot_in(scope)
            .ok_or(SessionError::DeadScope(scope))?;
        let lib = self.lib();
        if !Arc::ptr_eq(&bootstrap.admission_owner, self.admission_owner())
            || bootstrap.admission_epoch != self.admission_owner().epoch()
            || bootstrap.session != lib.id
            || current.machine_incarnation != bootstrap.initial.machine_incarnation
        {
            return Err(fail(Failure::BootstrapIdentity));
        }
        let state = lib
            .durable_graph
            .as_ref()
            .ok_or_else(|| fail(Failure::MissingGraph))?;
        let retained = state
            .owner
            .as_ref()
            .ok_or_else(|| fail(Failure::MissingRunOwner))?;
        retained.validate_owner()?;
        if state.unconfirmed.is_some() {
            return Err(fail(Failure::Unconfirmed));
        }
        let mapped = lib.durable_public_scopes.get(&bootstrap.owner).copied();
        if mapped != Some(scope) {
            return Err(fail(Failure::Scope { mapped }));
        }
        let surface = state
            .graph
            .public_surfaces()
            .find(|surface| surface.owner == bootstrap.owner)
            .ok_or_else(|| fail(Failure::MissingSurface))?;
        if surface != &bootstrap.surface {
            return Err(fail(Failure::BootstrapSurface));
        }
        let tip = (current.declaration_tip != Generation(0)).then_some(current.declaration_tip);
        if tip != surface.declaration_root {
            return Err(fail(Failure::DeclarationTip {
                published: surface.declaration_root,
                current: tip,
            }));
        }
        let original = std::fs::read(&state.path)?;
        let root = state.path.parent().expect("canonical manifest parent");
        let read = super::recovery::read_v2_bytes(
            &state.path,
            root,
            &original,
            super::recovery::RecoveryReadPurpose::Metadata,
        )
        .map_err(|error| SessionError::RecoveryManifest {
            path: state.path.clone(),
            detail: error.to_string(),
        })?
        .ok_or_else(|| fail(Failure::UnrecognizedManifest))?;
        if !read.artifact_losses.is_empty() {
            return Err(fail(Failure::ArtifactLoss {
                count: read.artifact_losses.len(),
            }));
        }
        if read.graph.checksum() != state.graph.checksum() {
            return Err(fail(Failure::ManifestChecksum {
                published: read.graph.checksum().to_owned(),
                current: state.graph.checksum().to_owned(),
            }));
        }
        let exact_context = lib.current_exact_context_in(scope);
        let compiler_context = exact_context
            .as_ref()
            .map(|context| super::recovery::RecoveryCompilerContext::capture(context))
            .unwrap_or_default();
        if current == bootstrap.initial && compiler_context == surface.compiler_context {
            return Ok(PublicManifestCommit::Durable);
        }
        let next_epoch =
            current
                .epoch
                .checked_add(1)
                .ok_or_else(|| SessionError::RecoveryManifest {
                    path: state.path.clone(),
                    detail: "public visibility epoch exhausted".into(),
                })?;
        let bindings = current
            .bindings
            .iter()
            .map(|(name, id)| super::recovery::RecoveryPublicBinding {
                name: name.clone(),
                owner: super::recovery::RecoveryBindingId {
                    session: lib.id.0,
                    variable: id.raw(),
                },
            })
            .collect();
        let sources = self.recovery_source_instances(current.source_instances.iter().cloned())?;
        let (snapshot_graph, compiler_context) =
            super::recovery_hydration::materialize_public_compiler_context(
                &state.graph,
                root,
                exact_context.as_ref(),
            )?;
        let staged = super::recovery::stage_public_visibility_at_epoch_v2(
            &state.path,
            root,
            &snapshot_graph,
            bootstrap.owner.clone(),
            surface.epoch,
            next_epoch,
            bindings,
            sources,
            tip,
            compiler_context,
        )
        .map_err(|error| SessionError::RecoveryManifest {
            path: state.path.clone(),
            detail: error.to_string(),
        })?;
        retained.validate_owner()?;
        if std::fs::read(&state.path)? != original {
            return Err(fail(Failure::BootstrapSurface));
        }
        tracing::info!(target: "tidepool::session", owner=?bootstrap.owner, scope=?scope,
            declaration_tip=?current.declaration_tip, published_epoch=surface.epoch,
            bootstrap_epoch=current.epoch, next_epoch,
            binding_count=current.bindings.len(), source_count=current.source_instances.len(),
            "publishing completed native bootstrap public surface");
        let lib = self.lib_mut();
        let outcome = lib.publish_recovery_manifest(staged);
        let state = lib
            .durable_graph
            .as_mut()
            .expect("bootstrap preflight graph");
        let commit = match outcome {
            super::recovery::RecoveryPublishOutcome::BeforeRename { detail, .. } => {
                return Ok(PublicManifestCommit::BeforeRename { detail });
            }
            super::recovery::RecoveryPublishOutcome::Durable { graph, .. } => {
                state.graph = graph;
                PublicManifestCommit::Durable
            }
            super::recovery::RecoveryPublishOutcome::PublishedDurabilityUnconfirmed {
                graph,
                publication,
                detail,
            } => {
                state.graph = graph;
                state.unconfirmed.set(publication);
                PublicManifestCommit::PublishedDurabilityUnconfirmed { detail }
            }
        };
        self.public_visibility_epochs.insert(scope, next_epoch);
        Ok(commit)
    }

    /// Confirm the already visible initialization or transfer for this exact
    /// owner and local scope. This never stages or publishes another surface.
    pub fn confirm_durable_public_scope(
        &mut self,
        owner: &RecoveryPublicOwner,
        target: ScopeId,
    ) -> Result<(), SessionError> {
        let snapshot = self
            .public_visibility_snapshot_in(target)
            .ok_or(SessionError::DeadScope(target))?;
        let lib = self.lib();
        let state = lib
            .durable_graph
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        state
            .owner
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?
            .validate_owner()?;
        let surface = state
            .graph
            .public_surfaces()
            .find(|surface| &surface.owner == owner)
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        if lib.durable_public_scopes.get(owner) != Some(&target)
            || surface.declaration_root
                != (snapshot.declaration_tip != Generation(0)).then_some(snapshot.declaration_tip)
            || surface.epoch != snapshot.epoch
            || surface.compiler_context
                != lib
                    .current_exact_context_in(target)
                    .as_ref()
                    .map(|context| super::recovery::RecoveryCompilerContext::capture(context))
                    .unwrap_or_default()
        {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        self.lib_mut().confirm_recovery_durability()
    }

    /// Capture a binding-only publication against one exact private and public
    /// lexical view. Caller stages its bytes after releasing this checkout.
    pub fn snapshot_binding_publication(
        &self,
        owner: RecoveryPublicOwner,
        public_scope: ScopeId,
        private_scope: ScopeId,
        write_ids: Vec<SessionVarId>,
    ) -> Result<PublicManifestBase, SessionError> {
        self.snapshot_publication(owner, public_scope, private_scope, write_ids, Vec::new())
    }

    /// Shared data-plane snapshot for paired declaration publication. Every
    /// selected key must be an actual native lease owned by the private view.
    pub(super) fn snapshot_publication(
        &self,
        owner: RecoveryPublicOwner,
        public_scope: ScopeId,
        private_scope: ScopeId,
        write_ids: Vec<SessionVarId>,
        source_keys: Vec<SourceLeaseKey>,
    ) -> Result<PublicManifestBase, SessionError> {
        self.snapshot_publication_target(
            Some(owner),
            public_scope,
            private_scope,
            write_ids,
            source_keys,
        )
    }

    pub(super) fn snapshot_publication_target(
        &self,
        owner: Option<RecoveryPublicOwner>,
        public_scope: ScopeId,
        private_scope: ScopeId,
        write_ids: Vec<SessionVarId>,
        source_keys: Vec<SourceLeaseKey>,
    ) -> Result<PublicManifestBase, SessionError> {
        if !self.scopes.is_live(public_scope) {
            return Err(SessionError::DeadScope(public_scope));
        }
        if !self.scopes.is_live(private_scope) {
            return Err(SessionError::DeadScope(private_scope));
        }
        self.bindings
            .prepare_exact_publication_in(
                &self.scopes,
                private_scope,
                public_scope,
                &write_ids,
                &source_keys,
            )
            .map_err(SessionError::InvalidPublicBindingPromotion)?;
        let expected_public = self
            .public_visibility_snapshot_in(public_scope)
            .ok_or(SessionError::MissingDeclarationLibrary)?;
        let expected_private = self
            .public_visibility_snapshot_in(private_scope)
            .ok_or(SessionError::MissingDeclarationLibrary)?;
        let final_source_keys: std::collections::BTreeSet<_> = expected_public
            .source_instances
            .iter()
            .chain(source_keys.iter())
            .cloned()
            .collect();
        let final_source_instances = if owner.is_some() {
            self.recovery_source_instances(final_source_keys)?
        } else {
            Vec::new()
        };
        let mut final_by_name: BTreeMap<String, SessionVarId> =
            expected_public.bindings.iter().cloned().collect();
        for id in &write_ids {
            let entry =
                self.bindings
                    .get(*id)
                    .ok_or(SessionError::InvalidPublicBindingPromotion(
                    tidepool_codegen::binding_table::BindingPromotionError::MissingOrForeignBinding,
                ))?;
            final_by_name.insert(entry.name.0.clone(), *id);
        }
        let lib = self
            .lib
            .as_ref()
            .ok_or(SessionError::MissingDeclarationLibrary)?;
        let mut base = if let Some(owner) = owner {
            lib.snapshot_public_manifest(
                owner,
                public_scope,
                private_scope,
                write_ids,
                source_keys,
                expected_public,
                expected_private,
                final_by_name.into_iter().collect(),
                final_source_instances,
            )?
        } else {
            if lib
                .durable_public_scopes
                .values()
                .any(|scope| *scope == public_scope)
            {
                return Err(SessionError::WrongPublicManifestTicket);
            }
            PublicManifestBase {
                admission_owner: None,
                admission_owner_epoch: None,
                manifest_owner: None,
                session: lib.id,
                log_revision: lib
                    .log
                    .publication_revision()
                    .ok_or(SessionError::StaleStagedDeclaration)?,
                path: lib.root.clone(),
                target: super::PublicPublicationBaseline::Ephemeral,
                public_scope,
                private_scope,
                write_ids,
                source_keys,
                expected_public,
                expected_private,
                final_bindings: final_by_name.into_iter().collect(),
                final_source_instances,
            }
        };
        base.admission_owner = Some(self.admission_owner().clone());
        base.admission_owner_epoch = Some(self.admission_owner().epoch());
        Ok(base)
    }

    /// Compare and rename under the same exclusive session checkout. A stale
    /// candidate never claims the execution's publication decision.
    pub fn publish_staged_public_manifest(
        &mut self,
        ticket: StagedPublicManifest,
        decision: &Arc<PublicationDecision>,
    ) -> Result<PublicManifestCommit, SessionError> {
        self.publish_staged_public_manifest_admitted(ticket, || decision.claim_commit())
    }

    /// Admit the frontend's short control claim only after exact native preflight.
    /// The claim callback must not await or perform native publication itself.
    pub fn publish_staged_public_manifest_admitted(
        &mut self,
        ticket: StagedPublicManifest,
        claim: impl FnOnce() -> Option<super::PublicationClaim>,
    ) -> Result<PublicManifestCommit, SessionError> {
        if ticket
            .admission_owner
            .as_ref()
            .is_none_or(|owner| !Arc::ptr_eq(owner, self.admission_owner()))
            || ticket.admission_owner_epoch != Some(self.admission_owner().epoch())
        {
            return Ok(PublicManifestCommit::Stale);
        }
        let lib = self
            .lib
            .as_ref()
            .ok_or(SessionError::MissingDeclarationLibrary)?;
        let graph_is_current = lib.public_manifest_ticket_is_current(&ticket)?;
        if !graph_is_current
            || !lib.declaration_publication_is_ready(&ticket)
            || !self
                .publication_views_are_current(&ticket.expected_public, &ticket.expected_private)
        {
            return Ok(PublicManifestCommit::Stale);
        }
        let next_epoch = self.prepare_public_visibility_advance(ticket.public_scope)?;
        let prepared = if let Some(declaration) = &ticket.declaration {
            let binding_custody = self.resolve_native_binding_custody(
                ticket.private_scope,
                &declaration.binding_custody,
            )?;
            self.bindings.prepare_authored_source_publication_in(
                &self.scopes,
                ticket.private_scope,
                ticket.public_scope,
                &ticket.write_ids,
                &ticket.source_keys,
                &declaration.source_domain_owners,
                &binding_custody,
            )
        } else {
            self.bindings.prepare_exact_publication_in(
                &self.scopes,
                ticket.private_scope,
                ticket.public_scope,
                &ticket.write_ids,
                &ticket.source_keys,
            )
        }
        .map_err(SessionError::InvalidPublicBindingPromotion)?;
        let declared_names = ticket
            .declaration
            .as_ref()
            .map(|declaration| declaration.declared_names.clone())
            .unwrap_or_default();
        let Some(claim) = claim() else {
            return Ok(PublicManifestCommit::Cancelled);
        };
        let public_scope = ticket.public_scope;
        let outcome = self
            .lib
            .as_mut()
            .expect("manifest preflight found library")
            .publish_staged_public_manifest_unchecked(ticket);
        match &outcome {
            PublicManifestCommit::BeforeRename { .. } => {
                claim.before_rename_failure();
            }
            PublicManifestCommit::Durable
            | PublicManifestCommit::Ephemeral
            | PublicManifestCommit::PublishedDurabilityUnconfirmed { .. } => {
                self.bindings.commit_exact_binding_promotion(prepared);
                for name in declared_names {
                    self.bindings.remove_current_in(public_scope, &name);
                }
                self.public_visibility_epochs
                    .insert(public_scope, next_epoch);
                claim.published();
            }
            PublicManifestCommit::Stale | PublicManifestCommit::Cancelled => {
                unreachable!("preflight and claim resolved these outcomes")
            }
        }
        Ok(outcome)
    }

    pub(super) fn publication_views_are_current(
        &self,
        public: &super::PublicVisibilitySnapshot,
        private: &super::PublicVisibilitySnapshot,
    ) -> bool {
        self.public_visibility_snapshot_in(public.scope).as_ref() == Some(public)
            && self.public_visibility_snapshot_in(private.scope).as_ref() == Some(private)
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
        self.bindings.validate_bind_in(scope, &entry)?;
        let is_new = self.bindings.get(entry.id).is_none();
        let record = BindRecord::of(&entry);
        self.bindings.bind_in(scope, entry)?;
        if is_new {
            self.binding_index.on_bind_record(&record);
        }
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
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<ValuePlaneCommit, SessionError> {
        let receipt = self.bind_replacing_decls_in(scope, vec![entry], settlement)?;
        #[allow(clippy::expect_used, reason = "one entry yields one receipt")]
        Ok(receipt
            .bindings
            .into_iter()
            .next()
            .expect("one materialization receipt"))
    }

    /// Publish a compiler-typed alias of an already registered binding root.
    /// The handle belongs to the source binding, so a failed declaration retract
    /// must never pass it through the new-root cleanup path used by a bind.
    pub(crate) fn publish_alias_in(
        &mut self,
        scope: ScopeId,
        entry: BindingEntry,
        source: SessionVarId,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<ValuePlaneCommit, SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let name = entry.name.0.clone();
        self.bindings.validate_bind_in(scope, &entry)?;
        self.retract_value_heads_in(scope, std::slice::from_ref(&name), settlement)?;
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
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<ValuePlaneCommit, SessionError> {
        self.bind_replacing_decl_in(ScopeId::ROOT, entry, settlement)
    }

    /// Atomically materialize a whole binding set. The declaration-environment
    /// retraction is one durable generation for every affected name; only after
    /// it succeeds are entries installed in the value table.  On any failure,
    /// every produced root is retired before the error returns, so none becomes
    /// an unowned persistent GC root.
    #[allow(
        clippy::expect_used,
        reason = "the complete set is preflighted under the exclusive session checkout"
    )]
    pub fn bind_replacing_decls_in(
        &mut self,
        scope: ScopeId,
        entries: Vec<BindingEntry>,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<MaterializationSetCommit, SessionError> {
        if let Err(error) = self.validate_binding_set_in(scope, &entries) {
            self.discard_unbound_entries(entries);
            return Err(error);
        }
        let names: Vec<String> = entries
            .iter()
            .filter(|entry| self.bindings.get(entry.id).is_none())
            .map(|entry| entry.name.0.clone())
            .collect();
        if let Err(error) = self.retract_value_heads_in(scope, &names, settlement) {
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
                self.bind_in(scope, entry)
                    .expect("complete binding set preflighted before declaration retraction");
                receipt
            })
            .collect();
        Ok(MaterializationSetCommit { bindings })
    }

    /// A sealed checked native item overlays Value heads in its exact private
    /// scope. Its original declaration module remains available by qualification.
    #[allow(
        clippy::expect_used,
        reason = "the complete sealed set is preflighted under the exclusive session checkout"
    )]
    pub(crate) fn bind_checked_private_values_in(
        &mut self,
        completion: &super::admission::CheckedTurnCompletion,
        scope: ScopeId,
        entries: Vec<BindingEntry>,
        binders: &[&super::BoundBinder],
    ) -> Result<MaterializationSetCommit, SessionError> {
        match completion.validates_private_overlay(self, scope, binders) {
            Ok(true) => {}
            Ok(false) => {
                self.discard_unbound_entries(entries);
                return Err(SessionError::StaleStagedDeclaration);
            }
            Err(error) => {
                self.discard_unbound_entries(entries);
                return Err(error);
            }
        }
        if entries.len() != binders.len()
            || entries.iter().zip(binders).any(|(entry, binder)| {
                entry.name.0 != binder.name
                    || entry.id != SessionVarId::from_extract(binder.var_id)
                    || entry.value.identity.module != binder.module
                    || entry.value.identity.occurrence != binder.name
                    || entry.value.identity.namespace != "value"
                    || entry.value.identity.record_parent.is_some()
            })
        {
            self.discard_unbound_entries(entries);
            return Err(SessionError::StaleStagedDeclaration);
        }
        if let Err(error) = self.validate_binding_set_in(scope, &entries) {
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
                self.bind_in(scope, entry)
                    .expect("complete checked binding set preflighted before mutation");
                receipt
            })
            .collect();
        Ok(MaterializationSetCommit { bindings })
    }

    fn validate_binding_set_in(
        &self,
        scope: ScopeId,
        entries: &[BindingEntry],
    ) -> Result<(), SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let mut ids = std::collections::HashSet::new();
        for entry in entries {
            if !ids.insert(entry.id) {
                return Err(
                    tidepool_codegen::binding_table::BindingIdentityError { id: entry.id }.into(),
                );
            }
            self.bindings.validate_bind_in(scope, entry)?;
        }
        Ok(())
    }

    pub(crate) fn validate_new_binding_ids(
        &self,
        ids: impl IntoIterator<Item = SessionVarId>,
    ) -> Result<(), SessionError> {
        let mut unique = std::collections::HashSet::new();
        for id in ids {
            if !unique.insert(id) || self.bindings.get(id).is_some() {
                return Err(tidepool_codegen::binding_table::BindingIdentityError { id }.into());
            }
        }
        Ok(())
    }

    /// Root-scope [`Self::bind_replacing_decls_in`].
    pub fn bind_replacing_decls(
        &mut self,
        entries: Vec<BindingEntry>,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<MaterializationSetCommit, SessionError> {
        self.bind_replacing_decls_in(ScopeId::ROOT, entries, settlement)
    }

    /// Dispose roots that were produced by a completed materialization but
    /// could not enter the persistent binding store. Dropping their copied
    /// handle identities does not release the machine's retained values.
    fn discard_unbound_entries(&mut self, entries: Vec<BindingEntry>) {
        let mut owned = self
            .bindings
            .iter_live()
            .map(|entry| entry.value.handle.raw().0)
            .collect::<std::collections::HashSet<_>>();
        if let Some(engine) = self.machine.as_ref() {
            for (identity, generation) in engine.code_export_retentions() {
                if let Some(ImportOwner::CodeExport { root_id, .. }) =
                    engine.retained_code_export_owner(&identity, generation)
                {
                    owned.insert(root_id);
                }
            }
        }
        let mut released = std::collections::HashSet::new();
        if let Some(engine) = self.machine.as_mut() {
            // A prepared entry's root is its adopted handle.
            for entry in entries {
                let BoundValue { handle, .. } = entry.value;
                if !owned.contains(&handle.raw().0) && released.insert(handle.raw().0) {
                    engine.release(handle);
                }
            }
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
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<DeclarationPlaneCommit, SessionError> {
        self.commit_declarations_in(scope, decl_texts, &SourceImports::new(), settlement)
    }

    /// Own declaration validation, capture retention, and value-name replacement
    /// as one commit. Neither frontend entry point can omit a lifetime step.
    fn commit_declarations_in(
        &mut self,
        scope: ScopeId,
        decl_texts: &[&str],
        external: &SourceImports,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<DeclarationPlaneCommit, SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        #[allow(clippy::expect_used, reason = "decl plane present")]
        let lib = self.lib.as_ref().expect("decl plane present");
        let Some(receipt) = lib.declaration_receipt(decl_texts, settlement)? else {
            let generation = lib.scope_tip(scope);
            return Ok(DeclarationPlaneCommit {
                generation,
                module: SessionModule::lib(generation),
                items: Vec::new(),
                evicted_values: Vec::new(),
            });
        };
        self.commit_declaration_receipt_in(scope, &receipt, external, settlement)
    }

    /// Consume compiler-owned source facts through the same capture and
    /// value-replacement boundary as ordinary definitions.
    pub fn commit_declaration_receipt_in(
        &mut self,
        scope: ScopeId,
        receipt: &super::DeclarationReceipt,
        external: &SourceImports,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<DeclarationPlaneCommit, SessionError> {
        if !self.scopes.is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        if self.lib().durable_graph.is_some() {
            let staged = self.stage_declarations_in(scope, receipt, external, &[], settlement)?;
            return self.adopt_staged_declaration_in(staged);
        }
        let next_epoch = self.prepare_public_visibility_advance(scope)?;
        let mut persistent_imports = external.clone();
        persistent_imports.extend(&self.workbench_imports_in(scope));
        let mut replaced_names: Vec<String> = receipt
            .items
            .iter()
            .flat_map(super::ExportItem::value_names)
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
                settlement,
            )?;
        // GHC may compile these declaration bodies later. Their exact imported
        // value environment must outlive that future use, including any saved
        // observations and the compiled slots those observations depend on.
        self.bindings.preserve_observations(&captured_values);
        for name in &replaced_names {
            self.bindings.remove_current_in(scope, name);
        }
        self.public_visibility_epochs.insert(scope, next_epoch);
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
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<DeclarationPlaneCommit, SessionError> {
        self.define_replacing_values_in(ScopeId::ROOT, decl_texts, settlement)
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
        let mut source_instances = Vec::new();
        for dead in &doomed {
            if let Some(lib) = self.lib.as_mut() {
                lib.tips.remove(dead);
            }
            self.compile_views.lock().remove(dead);
            let drained = self.bindings.drain_scope_with_sources(*dead);
            retired.extend(drained.bindings);
            source_instances.extend(drained.source_instances);
        }
        retired.extend(self.bindings.collect_observations());
        receipt.bindings_retired = retired.len();
        receipt.roots_released = self.release_binding_roots(retired)
            + self.release_source_instance_roots(source_instances);
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
/// `bindings_retired`: a binding whose handle is still owned by a survivor (the
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
mod staged_interface_tests {
    use super::*;

    #[test]
    fn uncommitted_interface_creation_is_removed_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let path = root
            .path()
            .join(SessionModule::val(Generation(411)).relative_hi_path());
        let staged = stage_interface_file(path.clone(), b"checked interface").unwrap();
        assert!(staged.is_some());
        assert_eq!(std::fs::read(&path).unwrap(), b"checked interface");
        drop(staged);
        assert!(!path.exists());
    }

    #[test]
    fn committed_interface_creation_survives_drop() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checked.hi");
        let mut staged = stage_interface_file(path.clone(), b"checked interface")
            .unwrap()
            .unwrap();
        staged.committed = true;
        drop(staged);
        assert_eq!(std::fs::read(path).unwrap(), b"checked interface");
    }

    #[test]
    fn identical_preexisting_interface_is_preserved_without_cleanup_lease() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checked.hi");
        std::fs::write(&path, b"checked interface").unwrap();
        let before = std::fs::metadata(&path).unwrap().ino();
        let staged = stage_interface_file(path.clone(), b"checked interface").unwrap();
        assert!(staged.is_none());
        drop(staged);
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), before);
        assert_eq!(std::fs::read(path).unwrap(), b"checked interface");
    }

    #[test]
    fn conflicting_preexisting_interface_is_rejected_and_preserved() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checked.hi");
        std::fs::write(&path, b"original immutable interface").unwrap();
        let error = stage_interface_file(path.clone(), b"different interface")
            .err()
            .unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read(path).unwrap(),
            b"original immutable interface"
        );
    }

    #[test]
    fn interface_staging_rejects_blocked_directory_before_creating_file() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("not-a-directory");
        std::fs::write(&parent, b"blocker").unwrap();
        let path = parent.join("checked.hi");
        assert!(stage_interface_file(path.clone(), b"checked interface").is_err());
        assert!(!path.exists());
        assert_eq!(std::fs::read(parent).unwrap(), b"blocker");
    }

    #[test]
    fn interface_cleanup_preserves_replacement_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checked.hi");
        let staged = stage_interface_file(path.clone(), b"checked interface").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        drop(staged);
        assert_eq!(std::fs::read(path).unwrap(), b"replacement");
    }

    #[test]
    fn mount_cancellation_observes_invocation_before_bootstrap_and_exact_realm() {
        let mut session = PersistentSession::new(None, 1024 * 1024);
        let realm = RealmId::fresh();
        let other = RealmId::fresh();
        let invocation = Arc::new(AtomicBool::new(false));
        session.replace_invocation_cancel(Some(invocation.clone()));
        assert!(!session.mount_cancelled(realm));
        invocation.store(true, Ordering::Release);
        assert!(session.mount_cancelled(realm));
        invocation.store(false, Ordering::Release);
        let entry =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "value", 412);
        session.bind(entry).unwrap();
        session
            .prepared_mut()
            .unwrap()
            .cancel_handle(realm)
            .cancel();
        assert!(session.mount_cancelled(realm));
        assert!(!session.mount_cancelled(other));
        invocation.store(true, Ordering::Release);
        assert!(session.mount_cancelled(other));
    }
}

#[cfg(test)]
mod checkpoint_scope_tests {
    use super::*;

    #[test]
    fn scoped_prepared_inventory_keeps_historical_roots_and_excludes_sibling_instances() {
        let mut session = PersistentSession::new(None, 1024 * 1024);
        session.set_image_registry(Arc::new(ImageRegistry::new()));
        let public = session.mint_isolated_scope();
        let original =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "value", 460);
        let identity = original.value.identity.clone();
        session.bind_in(public, original).unwrap();
        let private = session.mint_detached_scope(public).unwrap();
        let replacement =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "value", 461);
        session.bind_in(public, replacement).unwrap();
        let own =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "value", 462);
        session.bind_in(private, own).unwrap();
        let sibling = session.mint_isolated_scope();
        let mut foreign =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "value", 463);
        // An equal exact Name/module in another live scope must not choose
        // that scope's distinct native CAF installation.
        foreign.module = SessionModule::val(Generation(462));
        session.bind_in(sibling, foreign).unwrap();
        let inventory = session.scoped_prepared_bindings_in(private);
        assert_eq!(inventory.len(), 2);
        for (generation, expected) in [(460, Some(460)), (461, None), (462, Some(462))] {
            let entry = inventory.get(&(&identity, SessionModule::val(Generation(generation))));
            assert_eq!(entry.map(|entry| entry.id.raw()), expected);
            assert_eq!(
                entry.map(|entry| entry.id),
                session.bindings().resolve_exact_prepared_in(
                    session.scope_tree(), private, &identity, generation,
                ).map(|entry| entry.id),
            );
        }
        let own = inventory
            .get(&(&identity, SessionModule::val(Generation(462))))
            .unwrap();
        assert_eq!(
            session
                .prepared()
                .unwrap()
                .prepared_handle_of(own.value.handle.raw()),
            Some(own.value.handle),
        );
    }

    #[test]
    fn scoped_view_commitment_reuses_unchanged_binding_owner_witness() {
        use std::sync::atomic::Ordering;
        for count in [1, 10, 100] {
            let dir = tempfile::tempdir().unwrap();
            let mut session = publication_session(dir.path(), 410);
            let public = session.mint_isolated_scope();
            let scope = session.mint_detached_scope(public).unwrap();
            for index in 0..count {
                let entry = super::super::prepared::tests::rooted_publication_fixture(
                    &mut session,
                    &format!("value{index}"),
                    410 + index,
                );
                session.bind_in(scope, entry).unwrap();
            }
            let cached = session.scoped_compile_view_in(scope).unwrap();
            let hashed = session.compile_view_bytes_hashed.load(Ordering::Relaxed);
            for _ in 0..100 {
                assert_eq!(session.compile_view_digest_in(scope), Some(cached.digest));
                let reader = session.compile_view_in(scope).unwrap();
                assert!(Arc::ptr_eq(&reader.projection, &cached.view.projection));
            }
            assert_eq!(
                session.compile_view_bytes_hashed.load(Ordering::Relaxed),
                hashed
            );
            assert!(Arc::ptr_eq(
                &session.scoped_compile_view_in(scope).unwrap(),
                &cached
            ));
            eprintln!("scoped view bindings={count} initial_hashed_bytes={hashed} repeated_checks=100 additional_hashed_bytes=0");
            session.retire_scope(scope);
            assert!(!session.compile_views.lock().contains_key(&scope));
        }
    }

    #[test]
    fn compile_view_readers_share_metadata_and_isolate_later_shadowing() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = publication_session(dir.path(), 417);
        let scope = session.mint_isolated_scope();
        let original =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "answer", 417);
        session.bind_in(scope, original).unwrap();
        let reader = session.compile_view_in(scope).unwrap();
        let last_reader = reader.clone();
        let weak = Arc::downgrade(&reader.projection);
        assert!(Arc::ptr_eq(&reader.projection, &last_reader.projection));
        let original_imports = reader.turn_imports(&SourceImports::new());
        let staged = reader
            .clone()
            .with_staged_values(SessionModule::val(Generation(418)), ["answer".into()]);
        assert!(!Arc::ptr_eq(&reader.projection, &staged.projection));
        assert_eq!(reader.turn_imports(&SourceImports::new()), original_imports);
        assert!(!staged.is_current_for(&reader));
        let replacement =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "answer", 419);
        session.bind_in(scope, replacement).unwrap();
        let latest = session.compile_view_in(scope).unwrap();
        assert!(!Arc::ptr_eq(&reader.projection, &latest.projection));
        assert!(!latest.is_current_for(&reader));
        assert_eq!(
            reader.visible_values(),
            &[SessionModule::val(Generation(417))]
        );
        assert_eq!(
            latest.visible_values(),
            &[SessionModule::val(Generation(419))]
        );
        session.retire_scope(scope);
        drop(reader);
        assert!(weak.upgrade().is_some());
        drop(last_reader);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn scoped_view_refreshes_sibling_inventory_and_counter_without_staleness() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = publication_session(dir.path(), 411);
        let public = session.mint_isolated_scope();
        let private = session.mint_detached_scope(public).unwrap();
        let cached = session.scoped_compile_view_in(private).unwrap();
        let initial = session.compile_view_in(private).unwrap();
        let sibling = session.mint_isolated_scope();
        let entry =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "sibling", 411);
        let module = entry.module;
        session.bind_in(sibling, entry).unwrap();
        session.set_val_gen(Generation(500));
        let view = session.compile_view_in(private).unwrap();
        assert_eq!(view.next_value_generation(), Generation(501));
        assert_ne!(initial.next_value_generation(), Generation(501));
        assert!(!initial.injected_values().contains(&module));
        assert!(Arc::ptr_eq(&initial.projection, &view.projection));
        assert!(Arc::ptr_eq(&view.projection, &cached.view.projection));
        assert!(view.injected_values().contains(&module));
        assert!(view.visible_values().is_empty());
        assert!(!view.reachable_values().contains(&module));
        assert_eq!(view.admission_digest(), cached.digest);
        assert!(Arc::ptr_eq(
            &session.scoped_compile_view_in(private).unwrap(),
            &cached
        ));
    }

    #[test]
    fn scoped_view_rebuilds_for_owned_binding_declaration_and_library_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = publication_session(dir.path(), 412);
        let entry =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "value", 412);
        session.bind(entry).unwrap();
        let scope = ScopeId::ROOT;
        let before = session.scoped_compile_view_in(scope).unwrap();
        let replacement =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "value", 414);
        session.bind(replacement).unwrap();
        let rebound = session.scoped_compile_view_in(scope).unwrap();
        assert!(!Arc::ptr_eq(&before, &rebound));
        assert_ne!(before.digest, rebound.digest);
        let turn = super::super::render::DeclTurn {
            normalized: Default::default(),
            external_imports: SourceImports::new(),
            sources: Vec::new(),
            workbench_imports: SourceImports::from_specs(["qualified Data.Set as Set"]),
            items: Vec::new(),
            value_types: BTreeMap::new(),
            retracts: Vec::new(),
            parent: None,
        };
        let generation = session.lib_mut().log.push(turn);
        session.lib_mut().set_scope_tip_in(scope, generation);
        let declared = session.scoped_compile_view_in(scope).unwrap();
        assert_ne!(declared.digest, rebound.digest);
        // Same session/path/tip counters cannot reuse another library's cache.
        let mut replacement = SessionLib::open(
            session.lib().session_id(),
            session.lib().include_dir(),
            super::super::ModuleEnv::standalone_default(),
        )
        .unwrap();
        let generation = replacement.log.push(super::super::render::DeclTurn {
            normalized: Default::default(),
            external_imports: SourceImports::new(),
            sources: Vec::new(),
            workbench_imports: SourceImports::from_specs(["qualified Data.Map as Map"]),
            items: Vec::new(),
            value_types: BTreeMap::new(),
            retracts: Vec::new(),
            parent: None,
        });
        replacement.set_scope_tip_in(scope, generation);
        *session.lib_mut() = replacement;
        let replaced = session.scoped_compile_view_in(scope).unwrap();
        assert!(!Arc::ptr_eq(&declared, &replaced));
        assert_ne!(declared.digest, replaced.digest);
    }

    #[test]
    fn immutable_binding_reinsertion_preserves_index_and_native_owner() {
        let mut session = PersistentSession::new(None, 1024);
        let scope = session.mint_isolated_scope();
        let mut entry =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "value", 401);
        entry.scope = scope;
        let id = entry.id;
        let same = BindingEntry {
            name: entry.name.clone(),
            id: entry.id,
            module: entry.module,
            value: entry.value.clone(),
            type_display: entry.type_display.clone(),
            defining_expr: entry.defining_expr.clone(),
            scope,
        };
        session.bind_in(scope, same).unwrap();
        let revision = session.bindings.mutation_revision();
        let roots = session.persistent_roots_count();
        let live_modules = session.live_val_modules();
        session.bind_in(scope, entry).unwrap();
        assert_eq!(session.bindings.mutation_revision(), revision);
        assert_eq!(session.persistent_roots_count(), roots);
        assert_eq!(session.live_val_modules(), live_modules);
        let retirement = session.retire_scope(scope);
        assert_eq!(retirement.roots_released, 1);
        assert!(session.bindings.get(id).is_none());
        assert!(session.live_val_modules().is_empty());
        assert_eq!(session.persistent_roots_count(), roots - 1);
    }

    #[test]
    fn late_binding_identity_conflict_refuses_entire_materialization_set() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = publication_session(dir.path(), 402);
        let existing = super::super::prepared::tests::rooted_publication_fixture(
            &mut session,
            "existing",
            402,
        );
        let id = existing.id;
        let existing_handle = existing.value.handle;
        session.bind(existing).unwrap();
        let before = session
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        let generation = session.lib().generation();
        let revision = session.bindings.mutation_revision();
        let first =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "first", 403);
        let first_id = first.id;
        let first_handle = first.value.handle;
        let mut conflict = super::super::prepared::tests::rooted_publication_fixture(
            &mut session,
            "conflict",
            404,
        );
        let conflict_handle = conflict.value.handle;
        conflict.id = id;
        let roots = session.persistent_roots_count();
        assert!(matches!(
            session.bind_replacing_decls(vec![first, conflict]),
            Err(SessionError::InvalidBindingIdentity(error)) if error.id == id
        ));
        assert_eq!(
            session
                .public_visibility_snapshot_in(ScopeId::ROOT)
                .unwrap(),
            before
        );
        assert_eq!(session.lib().generation(), generation);
        assert_eq!(session.bindings.mutation_revision(), revision);
        assert!(session.bindings.get(first_id).is_none());
        assert_eq!(session.persistent_roots_count(), roots - 2);
        let machine = session.prepared().unwrap();
        assert_eq!(
            machine.prepared_handle_of(existing_handle.raw()),
            Some(existing_handle)
        );
        assert!(machine.prepared_handle_of(first_handle.raw()).is_none());
        assert!(machine.prepared_handle_of(conflict_handle.raw()).is_none());
        assert!(!dir.path().join("declarations.json").exists());
    }

    #[test]
    fn duplicate_binding_ids_release_new_roots_without_partial_visibility() {
        let mut session = PersistentSession::new(None, 1024);
        let first =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "first", 405);
        let id = first.id;
        let mut second =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "second", 406);
        second.id = id;
        let handles = [first.value.handle, second.value.handle];
        let roots = session.persistent_roots_count();
        let revision = session.bindings.mutation_revision();
        assert!(matches!(
            session.bind_replacing_decls(vec![first, second]),
            Err(SessionError::InvalidBindingIdentity(error)) if error.id == id
        ));
        assert_eq!(session.bindings.mutation_revision(), revision);
        assert!(session.bindings.get(id).is_none());
        assert!(session.live_val_modules().is_empty());
        assert_eq!(session.persistent_roots_count(), roots - 2);
        for handle in handles {
            assert!(session
                .prepared()
                .unwrap()
                .prepared_handle_of(handle.raw())
                .is_none());
        }
    }

    fn public_owner(path: &str) -> RecoveryPublicOwner {
        RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse(path).unwrap(), 1).unwrap()
    }

    fn publication_session(root: &Path, id: u64) -> PersistentSession {
        let mut lib = SessionLib::open(
            tidepool_repr::SessionId(id),
            root.join(format!("session-{id}")),
            super::super::ModuleEnv::standalone_default(),
        )
        .unwrap();
        lib.attach_recovery_graph_v2(root.join("declarations.json"))
            .unwrap();
        PersistentSession::new(Some(lib), 1024)
    }

    fn bootstrap_session(
        root: &Path,
        id: u64,
    ) -> (PersistentSession, ScopeId, RecoveryPublicOwner) {
        struct RunOwner(PathBuf);
        impl super::super::RecoveryRunAuthority for RunOwner {
            fn owns_run(&self, root: &Path) -> std::io::Result<bool> {
                Ok(root.canonicalize()? == self.0)
            }
        }
        let mut lib = SessionLib::open(
            tidepool_repr::SessionId(id),
            root.join("session"),
            super::super::ModuleEnv::standalone_default(),
        )
        .unwrap();
        lib.attach_owned_recovery_graph_v3(
            root.join("declarations.json"),
            Arc::new(RunOwner(root.canonicalize().unwrap())),
        )
        .unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_isolated_scope();
        let initial =
            super::super::prepared::tests::rooted_publication_fixture(&mut session, "original", id);
        session.bind_in(public, initial).unwrap();
        let owner = public_owner("root/bootstrap");
        assert_eq!(
            session
                .initialize_durable_public_scope(owner.clone(), public)
                .unwrap(),
            PublicManifestCommit::Durable
        );
        (session, public, owner)
    }

    #[test]
    fn durable_bootstrap_publishes_native_visibility_before_private_admission() {
        use super::super::prepared::tests::install_selected_source_fixture;
        let root = tempfile::tempdir().unwrap();
        let (mut session, public, owner) = bootstrap_session(root.path(), 930);
        let seal = session
            .begin_durable_public_bootstrap(owner.clone(), public)
            .unwrap();
        let (target, added) = install_selected_source_fixture(&mut session, public, "a");
        assert!(!added.is_empty());
        session.advance_public_visibility(public);
        assert!(matches!(
            session.begin_durable_private_execution(&owner, public),
            Err(SessionError::InvalidDurablePublicAdmission {
                reason: super::super::DurablePublicAdmissionFailure::Epoch {
                    published: 1,
                    current: 2
                },
                ..
            })
        ));
        let before = session.public_visibility_snapshot_in(public).unwrap();
        assert_eq!(
            session.publish_durable_public_bootstrap(seal).unwrap(),
            PublicManifestCommit::Durable
        );
        let after = session.public_visibility_snapshot_in(public).unwrap();
        assert_eq!(after.bindings, before.bindings);
        assert_eq!(after.source_instances, before.source_instances);
        assert_eq!(after.declaration_tip, before.declaration_tip);
        assert_eq!(after.epoch, 3);
        session
            .begin_durable_private_execution(&owner, public)
            .unwrap();
        assert!(session.prepared_mut().unwrap().unpin(target));
    }

    #[test]
    fn durable_bootstrap_uncertainty_refuses_private_admission_until_confirmation() {
        let root = tempfile::tempdir().unwrap();
        let (mut session, public, owner) = bootstrap_session(root.path(), 931);
        let seal = session
            .begin_durable_public_bootstrap(owner.clone(), public)
            .unwrap();
        let binding = super::super::prepared::tests::rooted_publication_fixture(
            &mut session,
            "bootstrapResult",
            932,
        );
        session.bind_in(public, binding).unwrap();
        session.advance_public_visibility(public);
        session.lib_mut().fail_recovery_durability_once = true;
        assert!(matches!(
            session.publish_durable_public_bootstrap(seal).unwrap(),
            PublicManifestCommit::PublishedDurabilityUnconfirmed { .. }
        ));
        assert!(matches!(
            session.begin_durable_private_execution(&owner, public),
            Err(SessionError::InvalidDurablePublicAdmission {
                reason: super::super::DurablePublicAdmissionFailure::Unconfirmed,
                ..
            })
        ));
        session
            .confirm_durable_public_scope(&owner, public)
            .unwrap();
        session
            .begin_durable_private_execution(&owner, public)
            .unwrap();
        assert!(session.resolve_in(public, "bootstrapResult").is_some());
    }

    #[test]
    fn durable_bootstrap_foreign_runtime_and_changed_declaration_preserve_manifest() {
        let root = tempfile::tempdir().unwrap();
        let foreign_root = tempfile::tempdir().unwrap();
        let (mut session, public, owner) = bootstrap_session(root.path(), 933);
        let (mut foreign, _, _) = bootstrap_session(foreign_root.path(), 934);
        let seal = session
            .begin_durable_public_bootstrap(owner.clone(), public)
            .unwrap();
        let original = std::fs::read(foreign_root.path().join("declarations.json")).unwrap();
        assert!(matches!(
            foreign.publish_durable_public_bootstrap(seal),
            Err(SessionError::InvalidDurablePublicAdmission {
                reason: super::super::DurablePublicAdmissionFailure::BootstrapIdentity,
                ..
            })
        ));
        assert_eq!(
            std::fs::read(foreign_root.path().join("declarations.json")).unwrap(),
            original
        );
        let seal = session
            .begin_durable_public_bootstrap(owner.clone(), public)
            .unwrap();
        let original = std::fs::read(root.path().join("declarations.json")).unwrap();
        session.lib_mut().set_scope_tip_in(public, Generation(99));
        assert!(matches!(
            session.publish_durable_public_bootstrap(seal),
            Err(SessionError::InvalidDurablePublicAdmission {
                reason: super::super::DurablePublicAdmissionFailure::DeclarationTip {
                    published: None,
                    current: Some(Generation(99))
                },
                ..
            })
        ));
        assert_eq!(
            std::fs::read(root.path().join("declarations.json")).unwrap(),
            original
        );
    }

    #[test]
    fn durable_bootstrap_write_failure_keeps_published_surface_and_blocks_admission() {
        use std::os::unix::fs::PermissionsExt;
        struct Restore(PathBuf, std::fs::Permissions);
        impl Drop for Restore {
            fn drop(&mut self) {
                std::fs::set_permissions(&self.0, self.1.clone()).unwrap();
            }
        }
        let root = tempfile::tempdir().unwrap();
        let (mut session, public, owner) = bootstrap_session(root.path(), 935);
        let seal = session
            .begin_durable_public_bootstrap(owner.clone(), public)
            .unwrap();
        let original = std::fs::read(root.path().join("declarations.json")).unwrap();
        session.advance_public_visibility(public);
        let restore = Restore(
            root.path().to_owned(),
            std::fs::metadata(root.path()).unwrap().permissions(),
        );
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(matches!(
            session.publish_durable_public_bootstrap(seal),
            Err(SessionError::RecoveryManifest { .. })
        ));
        drop(restore);
        assert_eq!(
            std::fs::read(root.path().join("declarations.json")).unwrap(),
            original
        );
        assert!(matches!(
            session.begin_durable_private_execution(&owner, public),
            Err(SessionError::InvalidDurablePublicAdmission {
                reason: super::super::DurablePublicAdmissionFailure::Epoch {
                    published: 1,
                    current: 2
                },
                ..
            })
        ));
    }

    fn selected_sources(session: &PersistentSession, scope: ScopeId) -> Vec<SourceInstanceLease> {
        session
            .bindings
            .selected_source_instances_in(&session.scopes, scope)
    }

    fn mint_fixture_origin(
        session: &mut PersistentSession,
        scope: ScopeId,
        owner: CachedHomeOwner,
    ) {
        let origin = session
            .bindings
            .prepare_source_owner_origin_in(&session.scopes, scope, owner)
            .unwrap();
        session.bindings.commit_source_owner_origin(origin);
    }

    fn assert_unrelated_certified_turn_admitted(session: &mut PersistentSession, scope: ScopeId) {
        use tidepool_repr::execution_schema::testing;
        let mut wire = testing::wire_program();
        let tidepool_repr::execution_schema::Group::NonRecursive(entry) = &mut wire.bindings[0]
        else {
            unreachable!()
        };
        entry.identity.unit = "main".into();
        let prepared = testing::prepare(wire).unwrap();
        let resolved = session
            .resolve_certification_in(scope, &prepared, &TurnCertification::default())
            .unwrap();
        assert!(resolved.groups.is_empty());
        let target = CertifiedTargetImage::compile(prepared, &ImageRegistry::new()).unwrap();
        let (program, keys) = session
            .install_certified_turn_in(scope, target, &[], &BTreeMap::new(), vec![], &[])
            .unwrap();
        assert!(keys.is_empty());
        assert!(session.prepared_mut().unwrap().unpin(program));
    }

    #[test]
    fn binding_publication_retains_independent_source_instances_without_selecting_them() {
        use super::super::prepared::tests::install_selected_source_fixture;
        let root = tempfile::tempdir().unwrap();
        let mut session = publication_session(root.path(), 821);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let a = session.mint_detached_scope(public).unwrap();
        let b = session.mint_detached_scope(public).unwrap();
        let owner = public_owner("root/source-selection");
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let (target_a, keys_a) = install_selected_source_fixture(&mut session, a, "a");
        let (target_b, keys_b) = install_selected_source_fixture(&mut session, b, "a");
        assert_eq!(keys_a.len(), 2);
        assert_eq!(keys_b.len(), 2);
        assert_ne!(keys_a[0].instance, keys_b[0].instance);
        let external = session.mint_detached_scope(ScopeId::ROOT).unwrap();
        assert!(session.retain_scope_dependencies(a, external));
        assert!(session.retain_scope_dependencies(b, external));
        assert_eq!(
            session
                .bindings
                .source_instances_in(&session.scopes, external)
                .len(),
            4
        );
        assert!(selected_sources(&session, external).is_empty());
        assert_unrelated_certified_turn_admitted(&mut session, external);
        assert_eq!(session.retire_scope(external).roots_released, 0);
        let capture_a = session.mint_detached_scope(a).unwrap();
        let capture_b = session.mint_detached_scope(b).unwrap();
        for (scope, keys) in [(a, &keys_a), (b, &keys_b)] {
            let staged = session
                .snapshot_publication(owner.clone(), public, scope, vec![], keys.to_vec())
                .unwrap()
                .stage()
                .unwrap();
            assert_eq!(
                session
                    .publish_staged_public_manifest(staged, &PublicationDecision::new())
                    .unwrap(),
                PublicManifestCommit::Durable
            );
        }
        assert_eq!(
            session
                .bindings
                .source_instances_in(&session.scopes, public)
                .len(),
            4
        );
        assert!(selected_sources(&session, public).is_empty());
        assert_unrelated_certified_turn_admitted(&mut session, public);
        assert!(session.prepared_mut().unwrap().unpin(target_a));
        assert!(session.prepared_mut().unwrap().unpin(target_b));
        assert_eq!(session.retire_scope(a).roots_released, 0);
        assert_eq!(session.retire_scope(b).roots_released, 0);
        for (capture, original) in [(capture_a, &keys_a), (capture_b, &keys_b)] {
            let (reused, added) = install_selected_source_fixture(&mut session, capture, "a");
            assert!(added.is_empty());
            assert!(selected_sources(&session, capture)
                .iter()
                .all(|lease| original
                    .iter()
                    .any(|key| key.instance == lease.instance()
                        && key.binder.as_ref() == lease.binder())));
            assert!(session.prepared_mut().unwrap().unpin(reused));
        }
        let fresh = session.mint_detached_scope(public).unwrap();
        let (fresh_target, fresh_keys) = install_selected_source_fixture(&mut session, fresh, "a");
        assert_eq!(fresh_keys.len(), 2);
        assert!(fresh_keys.iter().all(|key| !keys_a
            .iter()
            .chain(&keys_b)
            .any(|old| old.instance == key.instance)));
        assert!(session.prepared_mut().unwrap().unpin(fresh_target));
        assert_eq!(session.retire_scope(fresh).roots_released, fresh_keys.len());
        assert_eq!(session.retire_scope(capture_a).roots_released, 0);
        assert_eq!(session.retire_scope(capture_b).roots_released, 0);
        assert_eq!(session.retire_scope(public).roots_released, 4);
        session
            .prepared_mut()
            .unwrap()
            .quiesce_and_collect_now()
            .unwrap();
        assert_eq!(session.residency().unwrap().programs, 0);
        assert_eq!(session.persistent_roots_count(), 0);
    }

    #[test]
    fn authored_source_selection_preserves_late_group_support_after_binding_publication() {
        use super::super::prepared::tests::install_selected_source_fixture;
        let root = tempfile::tempdir().unwrap();
        let mut session = publication_session(root.path(), 822);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let authored = session.mint_detached_scope(public).unwrap();
        let unrelated = session.mint_detached_scope(public).unwrap();
        let (target_a, keys_a) = install_selected_source_fixture(&mut session, authored, "a");
        let (target_b, keys_b) = install_selected_source_fixture(&mut session, unrelated, "a");
        assert!(session.retain_scope_dependencies(unrelated, authored));
        let authored_custody = session
            .public_visibility_snapshot_in(authored)
            .unwrap()
            .source_instances;
        assert_eq!(authored_custody.len(), 4);
        assert_eq!(selected_sources(&session, authored).len(), 2);
        let owner = selected_sources(&session, authored)[0].owner().clone();
        mint_fixture_origin(&mut session, authored, owner.clone());
        let authored_promotion = session
            .bindings
            .prepare_authored_source_publication_in(
                &session.scopes,
                authored,
                public,
                &[],
                &authored_custody,
                &[owner],
                &[],
            )
            .unwrap();
        session
            .bindings
            .commit_exact_binding_promotion(authored_promotion);
        let custody = session
            .bindings
            .prepare_exact_publication_in(&session.scopes, unrelated, public, &[], &keys_b)
            .unwrap();
        session.bindings.commit_exact_binding_promotion(custody);
        assert_eq!(
            session
                .bindings
                .source_instances_in(&session.scopes, public)
                .len(),
            4
        );
        assert_eq!(selected_sources(&session, public).len(), 2);
        let later = session.mint_detached_scope(public).unwrap();
        let (late_target, late_keys) = install_selected_source_fixture(&mut session, later, "late");
        assert_eq!(late_keys.len(), 1);
        assert_eq!(late_keys[0].binder.binder.occurrence, "late");
        for original in &keys_a {
            assert!(selected_sources(&session, later)
                .iter()
                .any(|lease| lease.instance() == original.instance
                    && lease.binder() == original.binder.as_ref()));
        }
        for target in [target_a, target_b, late_target] {
            assert!(session.prepared_mut().unwrap().unpin(target));
        }
        for scope in [authored, unrelated, later, public] {
            session.retire_scope(scope);
        }
        session
            .prepared_mut()
            .unwrap()
            .quiesce_and_collect_now()
            .unwrap();
        assert_eq!(session.residency().unwrap().programs, 0);
    }

    #[test]
    fn conflicting_authored_source_selection_refuses_before_machine_changes() {
        use super::super::prepared::tests::install_selected_source_fixture;
        let root = tempfile::tempdir().unwrap();
        let mut session = publication_session(root.path(), 823);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let a = session.mint_detached_scope(public).unwrap();
        let b = session.mint_detached_scope(public).unwrap();
        let (target_a, keys_a) = install_selected_source_fixture(&mut session, a, "a");
        let (target_b, keys_b) = install_selected_source_fixture(&mut session, b, "a");
        let owner = selected_sources(&session, a)[0].owner().clone();
        for scope in [a, b] {
            mint_fixture_origin(&mut session, scope, owner.clone());
        }
        let accepted = session
            .bindings
            .prepare_authored_source_publication_in(
                &session.scopes,
                a,
                public,
                &[],
                &keys_a,
                &[owner.clone()],
                &[],
            )
            .unwrap();
        session.bindings.commit_exact_binding_promotion(accepted);
        let before = session.residency().unwrap();
        let revision = session.bindings.mutation_revision();
        assert!(matches!(
            session.bindings.prepare_authored_source_publication_in(
                &session.scopes,
                b,
                public,
                &[],
                &keys_b,
                &[owner],
                &[],
            ),
            Err(tidepool_codegen::binding_table::BindingPromotionError::ConflictingSourceOrigin)
        ));
        assert_eq!(session.residency().unwrap(), before);
        assert_eq!(session.bindings.mutation_revision(), revision);
        for target in [target_a, target_b] {
            assert!(session.prepared_mut().unwrap().unpin(target));
        }
        for scope in [a, b, public] {
            session.retire_scope(scope);
        }
        session
            .prepared_mut()
            .unwrap()
            .quiesce_and_collect_now()
            .unwrap();
        assert_eq!(session.residency().unwrap().programs, 0);
    }

    #[test]
    fn authored_publication_keeps_exact_hidden_binding_custody_without_visible_aliases() {
        use super::super::prepared::tests::{
            install_selected_source_fixture, rooted_publication_fixture,
        };
        use tidepool_toolchain::artifact_inventory::{ArtifactId, NativeBindingRequirement};
        let root = tempfile::tempdir().unwrap();
        let mut session = publication_session(root.path(), 825);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let private = session.mint_detached_scope(public).unwrap();
        let sibling = session.mint_detached_scope(public).unwrap();
        let (target, keys) = install_selected_source_fixture(&mut session, private, "a");
        let owner = selected_sources(&session, private)[0].owner().clone();
        mint_fixture_origin(&mut session, private, owner.clone());
        let old = rooted_publication_fixture(&mut session, "captured", 826);
        let (old_id, old_handle) = (old.id, old.value.handle);
        let requirement = NativeBindingRequirement {
            artifact_id: ArtifactId([1; 32]),
            identity: old.value.identity.clone(),
            generation: old.module.gen.0,
        };
        session.bind_in(private, old).unwrap();
        let shadow = rooted_publication_fixture(&mut session, "captured", 827);
        let shadow_id = shadow.id;
        session.bind_in(private, shadow).unwrap();
        let unrelated = rooted_publication_fixture(&mut session, "unused", 828);
        let unrelated_id = unrelated.id;
        session.bind_in(private, unrelated).unwrap();
        let foreign = rooted_publication_fixture(&mut session, "foreign", 829);
        let foreign_id = foreign.id;
        let foreign_requirement = NativeBindingRequirement {
            identity: foreign.value.identity.clone(),
            generation: foreign.module.gen.0,
            ..requirement.clone()
        };
        session.bind_in(sibling, foreign).unwrap();
        let before = session.bindings.mutation_revision();
        for invalid in [
            foreign_requirement,
            NativeBindingRequirement {
                generation: 9999,
                ..requirement.clone()
            },
            NativeBindingRequirement {
                identity: SymbolIdentity {
                    module: "wrong-owner".into(),
                    ..requirement.identity.clone()
                },
                ..requirement.clone()
            },
        ] {
            assert!(session
                .resolve_native_binding_custody(private, &[invalid])
                .is_err());
        }
        assert!(session
            .resolve_native_binding_custody(public, std::slice::from_ref(&requirement))
            .is_err());
        assert!(matches!(
            session.bindings.prepare_authored_source_publication_in(
                &session.scopes,
                private,
                public,
                &[shadow_id],
                &keys,
                std::slice::from_ref(&owner),
                &[foreign_id]
            ),
            Err(tidepool_codegen::binding_table::BindingPromotionError::MissingOrForeignBinding)
        ));
        assert_eq!(session.bindings.mutation_revision(), before);
        let custody = session
            .resolve_native_binding_custody(private, std::slice::from_ref(&requirement))
            .unwrap();
        assert_eq!(custody, vec![old_id]);
        let prepared = session
            .bindings
            .prepare_authored_source_publication_in(
                &session.scopes,
                private,
                public,
                &[shadow_id],
                &keys,
                &[owner],
                &custody,
            )
            .unwrap();
        assert_eq!(session.bindings.mutation_revision(), before);
        session.bindings.commit_exact_binding_promotion(prepared);
        assert_eq!(
            session
                .bindings
                .resolve_in(&session.scopes, public, "captured")
                .unwrap()
                .id,
            shadow_id
        );
        assert!(session
            .bindings
            .resolve_in(&session.scopes, public, "unused")
            .is_none());
        assert!(session.prepared_mut().unwrap().unpin(target));
        session.retire_scope(private);
        assert!(session.bindings.get(unrelated_id).is_none());
        assert_eq!(
            session.bindings.get(old_id).unwrap().value.handle,
            old_handle
        );
        assert_eq!(
            session
                .prepared()
                .unwrap()
                .prepared_handle_of(old_handle.raw()),
            Some(old_handle)
        );
        assert_eq!(
            session
                .resolve_native_binding_custody(public, std::slice::from_ref(&requirement))
                .unwrap(),
            vec![old_id]
        );
        session.retire_scope(sibling);
        session.retire_scope(public);
        assert!(session.bindings.get(old_id).is_none());
        assert!(session.bindings.get(shadow_id).is_none());
        assert_eq!(
            session
                .prepared()
                .unwrap()
                .prepared_handle_of(old_handle.raw()),
            None
        );
        assert!(session
            .bindings
            .source_instances_in(&session.scopes, public)
            .is_empty());
    }

    #[test]
    fn independent_authored_domains_keep_late_group_original_support() {
        use super::super::prepared::tests::{
            certified_source_group_modules, install_selected_groups_fixture,
        };
        use tidepool_repr::execution_schema::{testing, ModuleVersion};
        let root = tempfile::tempdir().unwrap();
        let mut session = publication_session(root.path(), 824);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let d = session.mint_detached_scope(public).unwrap();
        let e = session.mint_detached_scope(public).unwrap();
        let groups = [
            certified_source_group_modules("D", "d1", 1, "Support", "support"),
            certified_source_group_modules("D", "d2", 2, "Support", "support"),
            certified_source_group_modules("E", "e1", 1, "Support", "support"),
            certified_source_group_modules("Support", "support", 5, "Support", "support"),
        ];
        let source = |module, name| SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity(module, name),
        };
        let (target_d, keys_d) =
            install_selected_groups_fixture(&mut session, d, &groups, source("D", "d1"));
        let (target_e, keys_e) =
            install_selected_groups_fixture(&mut session, e, &groups, source("E", "e1"));
        let original_support = selected_sources(&session, d)
            .into_iter()
            .find(|lease| lease.binder().binder.module == "Support")
            .unwrap();
        for (scope, keys, owner) in [
            (d, &keys_d, groups[0].owner().clone()),
            (e, &keys_e, groups[2].owner().clone()),
        ] {
            mint_fixture_origin(&mut session, scope, owner.clone());
            let publication = session
                .bindings
                .prepare_authored_source_publication_in(
                    &session.scopes,
                    scope,
                    public,
                    &[],
                    keys,
                    &[owner],
                    &[],
                )
                .unwrap();
            session.bindings.commit_exact_binding_promotion(publication);
        }
        // d2 must inherit D's original support instance despite E's independently
        // retained support. A single ambient map cannot express both domains.
        let later = session.mint_detached_scope(public).unwrap();
        let (target_late, _) =
            install_selected_groups_fixture(&mut session, later, &groups, source("D", "d2"));
        assert!(selected_sources(&session, later)
            .iter()
            .any(|lease| lease.instance() == original_support.instance()
                && lease.handle() == original_support.handle()));
        for target in [target_d, target_e, target_late] {
            session.prepared_mut().unwrap().unpin(target);
        }
        for scope in [d, e, later, public] {
            session.retire_scope(scope);
        }
    }

    #[test]
    fn nested_authored_domains_keep_unmaterialized_capture_after_origin_retirement() {
        use super::super::prepared::tests::{
            certified_source_group_modules, install_selected_groups_fixture,
            prepare_selected_groups_fixture,
        };
        use tidepool_repr::execution_schema::{testing, ModuleVersion};
        for reversed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut session = publication_session(root.path(), 825);
            let public = session.mint_scope(ScopeId::ROOT).unwrap();
            let d = session.mint_detached_scope(public).unwrap();
            let groups = [
                certified_source_group_modules("D", "d1", 1, "Support", "support"),
                certified_source_group_modules("D", "d2", 2, "Support", "support"),
                certified_source_group_modules("E", "e1", 1, "D", "d1"),
                certified_source_group_modules("E", "e2", 2, "D", "d2"),
                certified_source_group_modules("Support", "support", 5, "Support", "support"),
            ];
            let source = |module, name| SourceBinder {
                version: ModuleVersion([1; 32]),
                binder: testing::identity(module, name),
            };
            mint_fixture_origin(&mut session, d, groups[0].owner().clone());
            let e = session.mint_detached_scope(d).unwrap();
            assert!(
                selected_sources(&session, e).is_empty(),
                "D is unmaterialized at capture"
            );
            mint_fixture_origin(&mut session, e, groups[2].owner().clone());
            let (td, kd) =
                install_selected_groups_fixture(&mut session, d, &groups, source("D", "d1"));
            let (te, ke) =
                install_selected_groups_fixture(&mut session, e, &groups, source("E", "e1"));
            let support_a = selected_sources(&session, d)
                .into_iter()
                .find(|lease| lease.owner().module == "Support")
                .unwrap();
            let support_b = selected_sources(&session, e)
                .into_iter()
                .find(|lease| lease.owner().module == "Support")
                .unwrap();
            assert_ne!(support_a.instance(), support_b.instance());
            let publications = if reversed {
                vec![(e, &ke, groups[2].owner()), (d, &kd, groups[0].owner())]
            } else {
                vec![(d, &kd, groups[0].owner()), (e, &ke, groups[2].owner())]
            };
            for (scope, keys, owner) in publications {
                let prepared = session
                    .bindings
                    .prepare_authored_source_publication_in(
                        &session.scopes,
                        scope,
                        public,
                        &[],
                        keys,
                        &[owner.clone()],
                        &[],
                    )
                    .unwrap();
                session.bindings.commit_exact_binding_promotion(prepared);
            }
            for target in [td, te] {
                assert!(session.prepared_mut().unwrap().unpin(target));
            }
            session.retire_scope(d);
            session.retire_scope(e);
            let later = session.mint_detached_scope(public).unwrap();
            let (target, owners, evidence, demanded, inherited) = prepare_selected_groups_fixture(
                &session,
                later,
                &groups,
                &[source("D", "d2"), source("E", "e2")],
            );
            let root_domain = target.source_plan().unwrap().target[&source("D", "d2")].domain;
            let e_domain = target.source_plan().unwrap().target[&source("E", "e2")].domain;
            assert_ne!(root_domain, e_domain);
            let d2: Vec<_> = demanded
                .iter()
                .filter(|image| image.group().owner().module == "D")
                .collect();
            assert_eq!(d2.len(), 2);
            assert!(
                Arc::ptr_eq(d2[0].image(), d2[1].image()),
                "immutable D2 image is shared"
            );
            let selection = session
                .bindings
                .source_domain_selection_in(&session.scopes, later)
                .unwrap();
            for image in d2 {
                let key = image
                    .qualified_source(&source("Support", "support"))
                    .unwrap();
                let selected = &selection.inherited()[&key];
                assert_eq!(
                    selected.handle(),
                    if image.domain() == root_domain {
                        support_a.handle()
                    } else {
                        support_b.handle()
                    }
                );
            }
            let (mixed, _) = session
                .install_certified_turn_in(later, target, &owners, &evidence, demanded, &inherited)
                .unwrap();
            let d2_leases: Vec<_> = selected_sources(&session, later)
                .into_iter()
                .filter(|lease| lease.owner().module == "D" && lease.original_ordinal() == 2)
                .collect();
            assert_eq!(d2_leases.len(), 2);
            assert_ne!(d2_leases[0].instance(), d2_leases[1].instance());
            assert!(session.prepared_mut().unwrap().unpin(mixed));
            session.retire_scope(later);
            session.retire_scope(public);
            session
                .prepared_mut()
                .unwrap()
                .quiesce_and_collect_now()
                .unwrap();
            assert_eq!(session.persistent_roots_count(), 0);
            assert_eq!(session.residency().unwrap().programs, 0);
        }
    }

    #[test]
    fn certified_source_plan_ignores_unrelated_sibling_observation_changes() {
        use super::super::prepared::tests::{
            certified_source_group_modules, prepare_selected_groups_fixture,
        };
        use tidepool_repr::execution_schema::{testing, ModuleVersion};
        let root = tempfile::tempdir().unwrap();
        let mut session = publication_session(root.path(), 828);
        let scope = session.mint_detached_scope(ScopeId::ROOT).unwrap();
        let observation = super::super::prepared::tests::evaluated_publication_fixture(
            &mut session,
            "observation",
            922,
        );
        let id = observation.id;
        session.bind_in(scope, observation).unwrap();
        session.save_observation(id, &[]);
        let groups = [certified_source_group_modules("D", "d1", 1, "D", "d1")];
        let source = SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity("D", "d1"),
        };
        let (target, owners, evidence, demanded, inherited) =
            prepare_selected_groups_fixture(&session, scope, &groups, &[source]);
        let admitted = session.public_visibility_snapshot_in(scope).unwrap();
        let cache = session
            .bindings()
            .scope_witness(session.scope_tree(), scope)
            .unwrap();
        let sibling = session.mint_isolated_scope();
        let sibling_observation = super::super::prepared::tests::evaluated_publication_fixture(
            &mut session,
            "sibling",
            923,
        );
        let sibling_id = sibling_observation.id;
        session.bind_in(sibling, sibling_observation).unwrap();
        session.save_observation(sibling_id, &[]);
        assert_ne!(
            session
                .bindings()
                .scope_witness(session.scope_tree(), scope)
                .unwrap(),
            cache
        );
        assert_eq!(
            session.public_visibility_snapshot_in(scope).unwrap(),
            admitted
        );
        let (program, _) = session
            .install_certified_turn_in(scope, target, &owners, &evidence, demanded, &inherited)
            .unwrap();
        assert!(session.prepared_mut().unwrap().unpin(program));
    }

    #[test]
    fn source_domain_metadata_change_refuses_prepared_install_atomically() {
        use super::super::prepared::tests::{
            certified_source_group_modules, prepare_selected_groups_fixture,
        };
        use tidepool_repr::execution_schema::{testing, ModuleVersion};
        let root = tempfile::tempdir().unwrap();
        let mut session = publication_session(root.path(), 826);
        let scope = session.mint_detached_scope(ScopeId::ROOT).unwrap();
        let groups = [certified_source_group_modules("D", "d1", 1, "D", "d1")];
        let source = SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity("D", "d1"),
        };
        let (target, owners, evidence, demanded, inherited) =
            prepare_selected_groups_fixture(&session, scope, &groups, &[source]);
        let before = session.public_visibility_snapshot_in(scope).unwrap();
        mint_fixture_origin(&mut session, scope, groups[0].owner().clone());
        let after = session.public_visibility_snapshot_in(scope).unwrap();
        assert_eq!(before.source_instances, after.source_instances);
        assert_eq!(before.bindings, after.bindings);
        assert_ne!(before.source_selection, after.source_selection);
        assert!(matches!(
            session
                .install_certified_turn_in(scope, target, &owners, &evidence, demanded, &inherited),
            Err(PreparedRuntimeError::SourceScopeAdmission)
        ));
        assert!(session.residency().is_none());
        assert_eq!(session.public_visibility_snapshot_in(scope).unwrap(), after);
    }

    #[test]
    fn failed_late_sibling_attachment_restores_capture_custody_and_selection() {
        use super::super::prepared::tests::{
            certified_sibling_fixture, install_selected_groups_fixture,
        };
        use tidepool_repr::execution_schema::{testing, ModuleVersion};
        let root = tempfile::tempdir().unwrap();
        let mut session = publication_session(root.path(), 827);
        let a = session.mint_detached_scope(ScopeId::ROOT).unwrap();
        let groups = [certified_sibling_fixture()];
        let source = |name| SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity("Fixture", name),
        };
        let (ta, _) = install_selected_groups_fixture(&mut session, a, &groups, source("a"));
        let b = session.mint_detached_scope(a).unwrap();
        let (tb, _) = install_selected_groups_fixture(&mut session, a, &groups, source("b"));
        let original_b = selected_sources(&session, a)
            .into_iter()
            .find(|lease| lease.binder().binder.occurrence == "b")
            .unwrap();
        let before_custody = session.bindings.source_instance_keys_in(&session.scopes, b);
        let before_selection = selected_sources(&session, b);
        let (failed, delta) =
            install_selected_groups_fixture(&mut session, b, &groups, source("b"));
        assert_eq!(delta.len(), 1);
        assert!(selected_sources(&session, b)
            .iter()
            .any(|lease| lease.handle() == original_b.handle()));
        assert!(session.prepared_mut().unwrap().unpin(failed));
        assert!(session.retire_failed_turn_source_instances(b, &delta));
        assert!(
            !session.retire_failed_turn_source_instances(b, &delta),
            "admission delta rolls back once"
        );
        assert_eq!(
            session.bindings.source_instance_keys_in(&session.scopes, b),
            before_custody
        );
        assert_eq!(selected_sources(&session, b).len(), before_selection.len());
        assert!(selected_sources(&session, a)
            .iter()
            .any(|lease| lease.handle() == original_b.handle()));
        for target in [ta, tb] {
            session.prepared_mut().unwrap().unpin(target);
        }
        session.retire_scope(a);
        assert!(
            !session.prepared_mut().unwrap().release(original_b.handle()),
            "failed B admission does not retain A's b root"
        );
        assert_eq!(selected_sources(&session, b).len(), 1);
        session.retire_scope(b);
        session
            .prepared_mut()
            .unwrap()
            .quiesce_and_collect_now()
            .unwrap();
        assert_eq!(session.persistent_roots_count(), 0);
    }

    #[test]
    fn stale_sibling_descriptor_cannot_restore_released_root_with_live_anchor() {
        use super::super::prepared::tests::{
            certified_sibling_fixture, install_selected_groups_fixture,
            prepare_selected_groups_fixture,
        };
        use tidepool_repr::execution_schema::{testing, ModuleVersion};
        let root = tempfile::tempdir().unwrap();
        let mut session = publication_session(root.path(), 828);
        let a = session.mint_detached_scope(ScopeId::ROOT).unwrap();
        let groups = [certified_sibling_fixture()];
        let source = |name| SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity("Fixture", name),
        };
        let (ta, _) = install_selected_groups_fixture(&mut session, a, &groups, source("a"));
        let b = session.mint_detached_scope(a).unwrap();
        let (old_target, old_delta) =
            install_selected_groups_fixture(&mut session, a, &groups, source("b"));
        let stale = selected_sources(&session, a)
            .into_iter()
            .find(|lease| lease.binder().binder.occurrence == "b")
            .unwrap();
        assert!(session.prepared_mut().unwrap().unpin(old_target));
        assert!(session.retire_failed_turn_source_instances(a, &old_delta));
        assert!(!session.prepared_mut().unwrap().release(stale.handle()));
        let (target, owners, evidence, demanded, inherited) =
            prepare_selected_groups_fixture(&session, b, &groups, &[source("b")]);
        assert_eq!(inherited.len(), 1);
        assert!(
            session
                .bindings
                .retained_source_sibling_attachment(&inherited[0])
                .is_none(),
            "live anchor cannot issue a stale sibling descriptor"
        );
        let (fresh, _) = session
            .install_certified_turn_in(b, target, &owners, &evidence, demanded, &inherited)
            .unwrap();
        let live = selected_sources(&session, b)
            .into_iter()
            .find(|lease| lease.binder().binder.occurrence == "b")
            .unwrap();
        assert_eq!(
            live.instance(),
            stale.instance(),
            "exact group CAF is shared"
        );
        assert_ne!(
            live.handle(),
            stale.handle(),
            "only a new machine-issued root is admitted"
        );
        for target in [ta, fresh] {
            session.prepared_mut().unwrap().unpin(target);
        }
        session.retire_scope(a);
        session.retire_scope(b);
        session
            .prepared_mut()
            .unwrap()
            .quiesce_and_collect_now()
            .unwrap();
        assert_eq!(session.persistent_roots_count(), 0);
    }

    #[test]
    fn retained_package_certification_resolves_exact_export_and_refuses_plain_retained() {
        use tidepool_repr::execution_schema::{
            testing, Atom, ExprFrame, GlobalId, Group, RuntimeRep, SignatureId, ValueRef,
        };
        let binder = testing::identity("Fixture", "entry");
        let (legacy, _) =
            PreparedEngine::bootstrap(testing::prepare(testing::wire_program()).unwrap()).unwrap();
        let mut legacy_session = PersistentSession::new(None, 64 * 1024);
        legacy_session.machine = Some(legacy);
        assert!(legacy_session.prepared_retained().is_empty());
        let (engine, _) = super::super::prepared::tests::certified_package_export_fixture([9; 32]);
        let expected = engine
            .retained_package_code_export_owner(&binder, 0, &[9; 32])
            .unwrap();
        let mut session = PersistentSession::new(None, 64 * 1024);
        session.machine = Some(engine);
        assert!(session.prepared_retained().contains(&(binder.clone(), 0)));
        let mut wire = testing::wire_program();
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity.unit = "main".into();
        wire.expressions.nodes[0] = ExprFrame::Call {
            callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
            signature: SignatureId(0),
            arguments: vec![],
        };
        wire.globals.push(GlobalDecl {
            identity: binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(0)),
            required_evaluated: true,
            required_generation: Some(0),
        });
        let prepared = testing::prepare(wire).unwrap();
        let mut certification = TurnCertification::default();
        certification.target_owners = vec![PendingImportOwner::Retained {
            identity: binder.clone(),
            generation: 0,
        }];
        assert!(
            matches!(session.resolve_certification_in(ScopeId::ROOT, &prepared, &certification),
            Err(PreparedRuntimeError::MissingRetainedCertifiedOwner { identity, generation: 0 }) if identity == binder)
        );
        for digest in [[0; 32], [8; 32]] {
            certification.target_owners[0] = PendingImportOwner::RetainedPackage {
                unit: binder.unit.clone(),
                module: binder.module.clone(),
                binder: binder.clone(),
                generation: 0,
                interface_digest: digest,
            };
            assert!(
                matches!(session.resolve_certification_in(ScopeId::ROOT, &prepared, &certification),
                Err(PreparedRuntimeError::MissingRetainedCertifiedOwner { identity, generation: 0 }) if identity == binder)
            );
        }
        certification.target_owners[0] = PendingImportOwner::RetainedPackage {
            unit: binder.unit.clone(),
            module: binder.module.clone(),
            binder: binder.clone(),
            generation: 0,
            interface_digest: [9; 32],
        };
        let resolved = session
            .resolve_certification_in(ScopeId::ROOT, &prepared, &certification)
            .unwrap();
        assert_eq!(resolved.target_owners, vec![expected]);
        let registry = ImageRegistry::new();
        let target = CertifiedTargetImage::compile(prepared.clone(), &registry).unwrap();
        let (program, keys) = session
            .install_certified_turn_in(
                ScopeId::ROOT,
                target,
                &resolved.target_owners,
                &resolved.source_evidence,
                vec![],
                &resolved.inherited_needed,
            )
            .unwrap();
        assert!(keys.is_empty());
        assert!(session.machine.as_mut().unwrap().unpin(program));
        session.machine = None;
        assert!(
            matches!(session.resolve_certification_in(ScopeId::ROOT, &prepared, &certification),
            Err(PreparedRuntimeError::MissingRetainedCertifiedOwner { identity, generation: 0 }) if identity == binder)
        );
    }

    #[test]
    fn staged_sibling_publications_restage_without_erasing_the_other_actor() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = publication_session(dir.path(), 72);
        let public_a = session.mint_scope(ScopeId::ROOT).unwrap();
        let public_b = session.mint_scope(ScopeId::ROOT).unwrap();
        let private_a = session.mint_detached_scope(public_a).unwrap();
        let private_b = session.mint_detached_scope(public_b).unwrap();
        let actor_a = public_owner("root/a");
        let actor_b = public_owner("root/b");
        session
            .bind_durable_public_scope(actor_a.clone(), public_a)
            .unwrap();
        session
            .bind_durable_public_scope(actor_b.clone(), public_b)
            .unwrap();
        let staged_a = session
            .snapshot_binding_publication(actor_a.clone(), public_a, private_a, vec![])
            .unwrap()
            .stage()
            .unwrap();
        let staged_b = session
            .snapshot_binding_publication(actor_b.clone(), public_b, private_b, vec![])
            .unwrap()
            .stage()
            .unwrap();
        assert_eq!(
            session
                .publish_staged_public_manifest(staged_b, &PublicationDecision::new())
                .unwrap(),
            PublicManifestCommit::Durable
        );
        assert_eq!(
            session
                .public_visibility_snapshot_in(public_b)
                .unwrap()
                .epoch,
            1
        );
        assert_eq!(
            session
                .public_visibility_snapshot_in(public_a)
                .unwrap()
                .epoch,
            0
        );
        let a_decision = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(staged_a, &a_decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(a_decision.phase(), super::super::PublicationPhase::Running);
        let restaged_a = session
            .snapshot_binding_publication(actor_a.clone(), public_a, private_a, vec![])
            .unwrap()
            .stage()
            .unwrap();
        assert_eq!(
            session
                .publish_staged_public_manifest(restaged_a, &a_decision)
                .unwrap(),
            PublicManifestCommit::Durable
        );
        assert_eq!(
            session
                .public_visibility_snapshot_in(public_a)
                .unwrap()
                .epoch,
            1
        );
        let restored =
            super::super::recovery::read_v2(&dir.path().join("declarations.json"), dir.path())
                .unwrap()
                .unwrap()
                .graph;
        assert_eq!(restored.public_surfaces().count(), 2);
        assert_eq!(
            restored
                .public_surfaces()
                .filter(|surface| surface.owner == actor_a)
                .count(),
            1
        );
        assert_eq!(
            restored
                .public_surfaces()
                .filter(|surface| surface.owner == actor_b)
                .count(),
            1
        );
    }

    #[test]
    fn private_revision_invalidates_stage_before_cancellation_claim() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = publication_session(dir.path(), 72);
        let public = ScopeId::ROOT;
        let private = session.mint_detached_scope(public).unwrap();
        let owner = public_owner("root");
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let public_base = session.public_visibility_snapshot_in(public).unwrap();
        let stage = session
            .snapshot_binding_publication(owner.clone(), public, private, vec![])
            .unwrap()
            .stage()
            .unwrap();
        // Native instance-only progress may leave the declaration tip and
        // materialized names unchanged. Its owning revision still fences it.
        session.advance_public_visibility(private);
        assert_eq!(
            session.public_visibility_snapshot_in(public).unwrap(),
            public_base
        );
        let decision = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(stage, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), super::super::PublicationPhase::Running);
        assert!(!dir.path().join("declarations.json").exists());
        let retry = session
            .snapshot_binding_publication(owner, public, private, vec![])
            .unwrap()
            .stage()
            .unwrap();
        decision.request_cancellation();
        assert_eq!(
            session
                .publish_staged_public_manifest(retry, &decision)
                .unwrap(),
            PublicManifestCommit::Cancelled
        );
        assert_eq!(
            session.public_visibility_snapshot_in(public).unwrap(),
            public_base
        );
        assert!(!dir.path().join("declarations.json").exists());
    }

    #[test]
    fn public_revision_invalidates_stage_and_retry_advances_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = publication_session(dir.path(), 72);
        let public = ScopeId::ROOT;
        let private = session.mint_detached_scope(public).unwrap();
        let owner = public_owner("root");
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let stage = session
            .snapshot_binding_publication(owner.clone(), public, private, vec![])
            .unwrap()
            .stage()
            .unwrap();
        session.advance_public_visibility(public);
        let decision = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(stage, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), super::super::PublicationPhase::Running);
        let retry = session
            .snapshot_binding_publication(owner, public, private, vec![])
            .unwrap()
            .stage()
            .unwrap();
        assert_eq!(
            session
                .publish_staged_public_manifest(retry, &decision)
                .unwrap(),
            PublicManifestCommit::Durable
        );
        assert_eq!(decision.phase(), super::super::PublicationPhase::Published);
        assert_eq!(
            session.public_visibility_snapshot_in(public).unwrap().epoch,
            2
        );
    }

    #[test]
    fn exhausted_public_revision_refuses_before_manifest_and_claim() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = publication_session(dir.path(), 72);
        let public = ScopeId::ROOT;
        let private = session.mint_detached_scope(public).unwrap();
        let owner = public_owner("root");
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        session.public_visibility_epochs.insert(public, u64::MAX);
        let stage = session
            .snapshot_binding_publication(owner, public, private, vec![])
            .unwrap()
            .stage()
            .unwrap();
        let decision = PublicationDecision::new();
        assert!(matches!(
            session.publish_staged_public_manifest(stage, &decision),
            Err(SessionError::RecoveryManifest { .. })
        ));
        assert_eq!(decision.phase(), super::super::PublicationPhase::Running);
        assert!(!dir.path().join("declarations.json").exists());
    }

    #[test]
    fn staged_publication_refuses_retired_scope_and_foreign_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = publication_session(dir.path(), 72);
        let mut second = publication_session(dir.path(), 73);
        let public = first.mint_scope(ScopeId::ROOT).unwrap();
        let private = first.mint_detached_scope(public).unwrap();
        let owner = public_owner("root/a");
        first
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        second
            .bind_durable_public_scope(owner.clone(), ScopeId::ROOT)
            .unwrap();
        let foreign = first
            .snapshot_binding_publication(owner.clone(), public, private, vec![])
            .unwrap()
            .stage()
            .unwrap();
        assert!(matches!(
            second.publish_staged_public_manifest(foreign, &PublicationDecision::new()),
            Err(SessionError::WrongPublicManifestTicket)
        ));
        let retired = first
            .snapshot_binding_publication(owner, public, private, vec![])
            .unwrap()
            .stage()
            .unwrap();
        first.retire_scope(public);
        let decision = PublicationDecision::new();
        assert_eq!(
            first
                .publish_staged_public_manifest(retired, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), super::super::PublicationPhase::Running);
        assert!(!dir.path().join("declarations.json").exists());
    }

    #[test]
    fn staged_publication_refuses_foreign_owner_and_changed_graph_generation() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = publication_session(dir.path(), 72);
        let public_a = session.mint_scope(ScopeId::ROOT).unwrap();
        let public_b = session.mint_scope(ScopeId::ROOT).unwrap();
        let private_a = session.mint_detached_scope(public_a).unwrap();
        let actor_a = public_owner("root/a");
        let actor_b = public_owner("root/b");
        session
            .bind_durable_public_scope(actor_a.clone(), public_a)
            .unwrap();
        session
            .bind_durable_public_scope(actor_b.clone(), public_b)
            .unwrap();
        let mut foreign = session
            .snapshot_binding_publication(actor_a.clone(), public_a, private_a, vec![])
            .unwrap()
            .stage()
            .unwrap();
        let super::super::StagedPublicationTarget::Durable { owner, .. } = &mut foreign.target
        else {
            unreachable!()
        };
        *owner = actor_b;
        assert!(matches!(
            session.publish_staged_public_manifest(foreign, &PublicationDecision::new()),
            Err(SessionError::WrongPublicManifestTicket)
        ));

        let stage = session
            .snapshot_binding_publication(actor_a, public_a, private_a, vec![])
            .unwrap()
            .stage()
            .unwrap();
        session.lib_mut().reserve_join_generation_durable().unwrap();
        let decision = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(stage, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), super::super::PublicationPhase::Running);
    }

    #[test]
    fn cancellation_and_pre_rename_failure_do_not_publish() {
        let cancelled_dir = tempfile::tempdir().unwrap();
        let mut cancelled = publication_session(cancelled_dir.path(), 72);
        let private = cancelled.mint_detached_scope(ScopeId::ROOT).unwrap();
        let owner = public_owner("root");
        cancelled
            .bind_durable_public_scope(owner.clone(), ScopeId::ROOT)
            .unwrap();
        let stage = cancelled
            .snapshot_binding_publication(owner.clone(), ScopeId::ROOT, private, vec![])
            .unwrap()
            .stage()
            .unwrap();
        let decision = PublicationDecision::new();
        decision.request_cancellation();
        assert_eq!(
            cancelled
                .publish_staged_public_manifest(stage, &decision)
                .unwrap(),
            PublicManifestCommit::Cancelled
        );
        assert!(!cancelled_dir.path().join("declarations.json").exists());

        let failed_dir = tempfile::tempdir().unwrap();
        let mut failed = publication_session(failed_dir.path(), 73);
        let private = failed.mint_detached_scope(ScopeId::ROOT).unwrap();
        failed
            .bind_durable_public_scope(owner.clone(), ScopeId::ROOT)
            .unwrap();
        let stage = failed
            .snapshot_binding_publication(owner, ScopeId::ROOT, private, vec![])
            .unwrap()
            .stage()
            .unwrap();
        std::fs::create_dir(failed_dir.path().join("declarations.json")).unwrap();
        let decision = PublicationDecision::new();
        assert!(matches!(
            failed
                .publish_staged_public_manifest(stage, &decision)
                .unwrap(),
            PublicManifestCommit::BeforeRename { .. }
        ));
        assert_eq!(decision.phase(), super::super::PublicationPhase::Terminated);
        assert!(failed
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .graph
            .public_surfaces()
            .next()
            .is_none());
    }

    #[test]
    fn durable_public_owners_require_live_minted_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("declarations.json");
        let mut lib = SessionLib::open(
            tidepool_repr::SessionId(72),
            dir.path().join("session"),
            super::super::ModuleEnv::standalone_default(),
        )
        .unwrap();
        lib.attach_recovery_graph_v2(&manifest).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024);
        let first = session.mint_scope(ScopeId::ROOT).unwrap();
        let second = session.mint_scope(ScopeId::ROOT).unwrap();
        let actor = |path: &str| {
            RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse(path).unwrap(), 1).unwrap()
        };
        session
            .bind_durable_public_scope(actor("root/first"), first)
            .unwrap();
        session
            .bind_durable_public_scope(actor("root/second"), second)
            .unwrap();
        let unknown = ScopeId(u64::MAX);
        assert!(matches!(
            session.bind_durable_public_scope(actor("root/unknown"), unknown),
            Err(SessionError::DeadScope(scope)) if scope == unknown
        ));
        session.retire_scope(first);
        assert!(matches!(
            session.bind_durable_public_scope(actor("root/replacement"), first),
            Err(SessionError::DeadScope(scope)) if scope == first
        ));
        assert!(session
            .bind_durable_public_scope(actor("root/second"), first)
            .is_err());
        assert!(session
            .bind_durable_public_scope(actor("root/first"), second)
            .is_err());
    }

    #[test]
    fn detached_capture_survives_issuer_retirement_and_can_seed_later_child() {
        let mut session = PersistentSession::new(None, 1024);
        let issuer = session.mint_isolated_scope();
        let capture = session.mint_detached_scope(issuer).unwrap();
        assert_eq!(session.scope_tree().parent_of(capture), None);
        session.retire_scope(issuer);
        assert!(session.scope_tree().is_live(capture));
        let deferred = session.mint_detached_scope(capture).unwrap();
        assert_eq!(session.scope_tree().parent_of(deferred), None);
        session.retire_scope(capture);
        assert!(session.scope_tree().is_live(deferred));
        session.retire_scope(deferred);
    }
}

#[cfg(test)]
mod maintained_binding_lifetime_properties {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

    const SOURCE_GENERATION: u64 = 12_100;
    const ALIAS_GENERATION: u64 = 12_101;
    const SHADOW_GENERATION: u64 = 12_102;
    const ROOTED_IDS: [u64; 3] = [12_100, 12_103, 12_104];
    const ALIAS_ID: u64 = 12_105;

    #[derive(Clone, Debug)]
    struct Plan {
        chain_capture: bool,
        guided_capture_first: Option<bool>,
        release_ranks: Vec<u8>,
        reads: Vec<(u8, u8)>,
    }

    fn plan() -> impl Strategy<Value = Plan> {
        (
            any::<bool>(),
            proptest::collection::vec(any::<u8>(), 6),
            proptest::collection::vec((0_u8..4, 0_u8..4), 1..20),
        )
            .prop_map(|(chain_capture, release_ranks, reads)| Plan {
                chain_capture,
                guided_capture_first: None,
                release_ranks,
                reads,
            })
    }

    #[derive(Clone, Debug)]
    struct ModelBinding {
        id: SessionVarId,
        name: String,
        module: SessionModule,
        identity: tidepool_repr::execution_schema::SymbolIdentity,
        handle_group: u8,
        owner: ScopeId,
    }

    #[derive(Clone, Debug, Default)]
    struct ModelScope {
        live: bool,
        visible: BTreeMap<String, SessionVarId>,
        custody: HashSet<SessionVarId>,
    }

    #[derive(Debug, Default)]
    struct Model {
        bindings: Vec<ModelBinding>,
        scopes: BTreeMap<ScopeId, ModelScope>,
        explicit: Vec<Option<HashSet<SessionVarId>>>,
        owner_retired: bool,
    }

    #[derive(Debug, Default, serde::Serialize)]
    struct Support {
        same_generation_originals: usize,
        same_generation_identity_pair: usize,
        shared_handle_bindings: usize,
        aliases: usize,
        shadows: usize,
        captures_before_shadow: usize,
        captures_after_shadow: usize,
        chained_captures: usize,
        explicit_alias_leases: usize,
        capture_releases: usize,
        explicit_releases: usize,
        pending_owner_bindings: usize,
        released_handles: usize,
        alias_hidden_from_capture: usize,
        held_root_receipts: usize,
        owner_retired_with_holders: usize,
        owner_retired_without_holders: usize,
        shared_pair_survived_member_eviction: usize,
        interface_survived_single_member: usize,
        capture_before_explicit_release: usize,
        explicit_before_capture_release: usize,
        chained_capture_outlived_issuer: usize,
        reads: usize,
        dead_scope_reads: usize,
    }

    fn model_live_ids(model: &Model) -> HashSet<SessionVarId> {
        let mut retained = HashSet::new();
        for scope in model.scopes.values().filter(|scope| scope.live) {
            retained.extend(scope.custody.iter().copied());
        }
        for lease in model.explicit.iter().flatten() {
            retained.extend(lease.iter().copied());
        }
        model
            .bindings
            .iter()
            .filter(|binding| !model.owner_retired || retained.contains(&binding.id))
            .map(|binding| binding.id)
            .collect()
    }

    fn model_handles(model: &Model, live: &HashSet<SessionVarId>) -> BTreeSet<u8> {
        model
            .bindings
            .iter()
            .filter(|binding| live.contains(&binding.id))
            .map(|binding| binding.handle_group)
            .collect()
    }

    fn alias_dependency_closure(
        alias: SessionVarId,
        source: SessionVarId,
    ) -> HashSet<SessionVarId> {
        HashSet::from([alias, source])
    }

    fn check_state(
        state: &PersistentSession,
        model: &Model,
        handles: &BTreeMap<u8, tidepool_codegen::prepared_program::PreparedHandle>,
        interfaces: &HashMap<SessionModule, Arc<[u8]>>,
    ) -> HashSet<SessionVarId> {
        let expected_live = model_live_ids(model);
        let actual_live: HashSet<_> = state.bindings().iter_live().map(|entry| entry.id).collect();
        assert_eq!(actual_live, expected_live, "live binding IDs");
        for binding in &model.bindings {
            match state.bindings().get(binding.id) {
                Some(actual) => {
                    assert!(expected_live.contains(&binding.id));
                    assert_eq!(
                        actual.name.0, binding.name,
                        "binding name for {:?}",
                        binding.id
                    );
                    assert_eq!(
                        actual.module, binding.module,
                        "binding module for {:?}",
                        binding.id
                    );
                    assert_eq!(
                        actual.value.identity, binding.identity,
                        "binding identity for {:?}",
                        binding.id
                    );
                    assert_eq!(
                        actual.scope, binding.owner,
                        "binding owner for {:?}",
                        binding.id
                    );
                    assert_eq!(
                        actual.value.handle, handles[&binding.handle_group],
                        "actual adopted handle for {:?}",
                        binding.id
                    );
                }
                None => assert!(!expected_live.contains(&binding.id)),
            }
        }

        assert_eq!(
            state
                .live_val_modules()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            model
                .bindings
                .iter()
                .filter(|binding| expected_live.contains(&binding.id))
                .map(|binding| binding.module.module_name())
                .collect(),
            "live interface modules"
        );

        let expected_pairs: BTreeSet<_> = model
            .bindings
            .iter()
            .filter(|binding| expected_live.contains(&binding.id))
            .map(|binding| (binding.identity.clone(), binding.module.gen().0))
            .collect();
        assert_eq!(
            state
                .binding_index
                .prepared_retained()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            expected_pairs,
            "prepared identity/generation pairs"
        );
        for binding in &model.bindings {
            let eligible: Vec<_> = model
                .bindings
                .iter()
                .filter(|candidate| {
                    candidate.identity == binding.identity && expected_live.contains(&candidate.id)
                })
                .collect();
            let newest = eligible
                .iter()
                .map(|candidate| candidate.module.gen().0)
                .max();
            match (
                state
                    .binding_index
                    .resolve_prepared(&binding.identity, None),
                newest,
            ) {
                (Some(id), Some(generation)) => assert!(eligible.iter().any(|candidate| {
                    candidate.id == id && candidate.module.gen().0 == generation
                })),
                (None, None) => {}
                (actual, expected) => panic!(
                    "prepared resolution mismatch for {:?}: {actual:?}, newest {expected:?}",
                    binding.identity
                ),
            }
        }

        for (scope_id, scope) in &model.scopes {
            for name in ["source", "pair", "other", "alias"] {
                let expected = scope
                    .live
                    .then(|| scope.visible.get(name).copied())
                    .flatten()
                    .filter(|id| expected_live.contains(id));
                assert_eq!(
                    state.resolve_in(*scope_id, name).map(|entry| entry.id),
                    expected,
                    "resolve_in({scope_id:?}, {name})"
                );
            }
        }

        for (module, bytes) in interfaces {
            let is_live = model
                .bindings
                .iter()
                .any(|binding| binding.module == *module && expected_live.contains(&binding.id));
            assert_eq!(
                state.retained_value_interface(*module),
                is_live.then_some(bytes),
                "retained test interface for {module:?}"
            );
        }

        let expected_handles = model_handles(model, &expected_live);
        assert_eq!(state.value_handle_count(), expected_handles.len());
        for (group, handle) in handles {
            let actual = state
                .prepared()
                .and_then(|engine| engine.prepared_handle_of(handle.raw()));
            assert_eq!(
                actual.is_some(),
                expected_handles.contains(group),
                "prepared handle group {group}"
            );
            if let Some(actual) = actual {
                assert_eq!(actual, *handle);
            }
        }
        expected_live
    }

    fn capture_model(
        model: &mut Model,
        capture: ScopeId,
        parent: ScopeId,
        visible: BTreeMap<String, SessionVarId>,
    ) {
        let parent_scope = model.scopes.get(&parent).expect("live parent modeled");
        model.scopes.insert(
            capture,
            ModelScope {
                live: true,
                visible,
                custody: parent_scope.custody.clone(),
            },
        );
    }

    fn captured_visible(
        visible: &BTreeMap<String, SessionVarId>,
    ) -> BTreeMap<String, SessionVarId> {
        visible
            .iter()
            .filter(|(name, _)| name.as_str() != "alias")
            .map(|(name, id)| (name.clone(), *id))
            .collect()
    }

    fn owner_custody(model: &Model, owner: ScopeId) -> HashSet<SessionVarId> {
        let mut custody: HashSet<_> = model
            .bindings
            .iter()
            .filter(|binding| binding.owner == owner)
            .map(|binding| binding.id)
            .collect();
        if let Some(alias) = model
            .bindings
            .iter()
            .find(|binding| binding.name == "alias")
        {
            // This fixture publishes one alias whose sole dependency is source.
            custody.insert(SessionVarId::from_extract(ROOTED_IDS[0]));
            custody.insert(alias.id);
        }
        custody
    }

    fn run_history(plan: Plan) -> Support {
        let mut state = PersistentSession::new(None, 64 * 1024);
        let mut support = Support::default();
        let owner = state.mint_isolated_scope();
        let mut source_identity = super::super::prepared::tests::rooted_publication_fixture(
            &mut state,
            "source",
            SOURCE_GENERATION,
        );
        let source_handle = source_identity.value.handle;
        source_identity.value.identity.occurrence = "source".into();
        let source_identity = source_identity.value.identity.clone();
        source_identity_entry(&mut state, owner, source_identity.clone(), source_handle);
        support.same_generation_originals += 1;
        support.same_generation_identity_pair += 1;
        support.shared_handle_bindings += 1;

        let source_id = SessionVarId::from_extract(ROOTED_IDS[0]);
        let source_value = tidepool_codegen::binding_table::BoundValue {
            handle: source_handle,
            identity: source_identity.clone(),
        };
        let alias_module = SessionModule::val(tidepool_repr::Generation(ALIAS_GENERATION));
        let mut alias_identity = source_identity.clone();
        alias_identity.module = alias_module.module_name();
        alias_identity.occurrence = "alias".into();
        let mut alias_value = source_value.clone();
        alias_value.identity = alias_identity.clone();
        let alias_shares_source_handle = alias_value.handle == source_handle;
        let alias_id = SessionVarId::from_extract(ALIAS_ID);
        state
            .publish_alias_in(
                owner,
                BindingEntry {
                    name: tidepool_repr::BindingName("alias".into()),
                    id: alias_id,
                    module: alias_module,
                    value: alias_value,
                    type_display: None,
                    defining_expr: None,
                    scope: owner,
                },
                source_id,
            )
            .unwrap();
        support.aliases += 1;
        support.shared_handle_bindings += usize::from(alias_shares_source_handle);

        let source_module = SessionModule::val(tidepool_repr::Generation(SOURCE_GENERATION));
        let mut model = Model::default();
        let source_model = ModelBinding {
            id: source_id,
            name: "source".into(),
            module: source_module,
            identity: source_identity.clone(),
            handle_group: 0,
            owner,
        };
        let alias_model = ModelBinding {
            id: alias_id,
            name: "alias".into(),
            module: alias_module,
            identity: alias_identity,
            handle_group: 0,
            owner,
        };
        model.bindings.extend([source_model, alias_model]);
        let mut visible = BTreeMap::from([
            ("source".to_owned(), source_id),
            ("alias".to_owned(), alias_id),
        ]);
        model.scopes.insert(
            owner,
            ModelScope {
                live: true,
                visible: visible.clone(),
                custody: owner_custody(&model, owner),
            },
        );

        let before = state.mint_detached_scope(owner).unwrap();
        capture_model(&mut model, before, owner, captured_visible(&visible));
        assert!(state.resolve_in(before, "alias").is_none());
        support.captures_before_shadow += 1;
        support.alias_hidden_from_capture +=
            usize::from(state.resolve_in(before, "alias").is_none());

        let mut other_identity = source_identity.clone();
        other_identity.occurrence = "other".into();
        for (name, id, identity) in [
            ("pair", ROOTED_IDS[1], source_identity.clone()),
            ("other", ROOTED_IDS[2], other_identity.clone()),
        ] {
            let mut value = source_value.clone();
            value.identity = identity.clone();
            state
                .bind_in(
                    owner,
                    BindingEntry {
                        name: tidepool_repr::BindingName(name.into()),
                        id: SessionVarId::from_extract(id),
                        module: source_module,
                        value,
                        type_display: None,
                        defining_expr: None,
                        scope: owner,
                    },
                )
                .unwrap();
            support.same_generation_originals += 1;
            let bound_handle = state
                .bindings()
                .get(SessionVarId::from_extract(id))
                .unwrap()
                .value
                .handle;
            support.shared_handle_bindings += usize::from(bound_handle == source_handle);
            if identity == source_identity {
                support.same_generation_identity_pair += 1;
            }
        }
        model.bindings.extend([
            ModelBinding {
                id: SessionVarId::from_extract(ROOTED_IDS[1]),
                name: "pair".into(),
                module: source_module,
                identity: source_identity.clone(),
                handle_group: 0,
                owner,
            },
            ModelBinding {
                id: SessionVarId::from_extract(ROOTED_IDS[2]),
                name: "other".into(),
                module: source_module,
                identity: other_identity,
                handle_group: 0,
                owner,
            },
        ]);
        visible.insert("pair".into(), SessionVarId::from_extract(ROOTED_IDS[1]));
        visible.insert("other".into(), SessionVarId::from_extract(ROOTED_IDS[2]));
        model.scopes.get_mut(&owner).unwrap().visible = visible.clone();
        model.scopes.get_mut(&owner).unwrap().custody = owner_custody(&model, owner);

        let mut shadow = super::super::prepared::tests::rooted_publication_fixture(
            &mut state,
            "source",
            SHADOW_GENERATION,
        );
        let shadow_handle = shadow.value.handle;
        shadow.value.identity = source_identity.clone();
        shadow.scope = owner;
        state.bind_in(owner, shadow).unwrap();
        support.shadows += 1;
        let shadow_id = SessionVarId::from_extract(SHADOW_GENERATION);
        let shadow_binding = ModelBinding {
            id: shadow_id,
            name: "source".into(),
            module: SessionModule::val(tidepool_repr::Generation(SHADOW_GENERATION)),
            identity: source_identity.clone(),
            handle_group: 1,
            owner,
        };
        model.bindings.push(shadow_binding.clone());
        visible.insert("source".into(), shadow_id);
        model.scopes.get_mut(&owner).unwrap().visible = visible.clone();
        model.scopes.get_mut(&owner).unwrap().custody = owner_custody(&model, owner);

        let after = state.mint_detached_scope(owner).unwrap();
        capture_model(&mut model, after, owner, captured_visible(&visible));
        support.captures_after_shadow += 1;
        let chained = if plan.chain_capture {
            let capture = state.mint_detached_scope(before).unwrap();
            let chain_visible = model.scopes[&before].visible.clone();
            capture_model(&mut model, capture, before, chain_visible);
            support.chained_captures += 1;
            Some(capture)
        } else {
            None
        };

        // These fixture bytes exercise generation/interface lifetime only; they
        // are not Haskell compiler authority or checked-interface evidence.
        let scope_ids = [owner, before, after]
            .into_iter()
            .chain(chained)
            .collect::<BTreeSet<_>>();
        assert_eq!(scope_ids.len(), 3 + usize::from(chained.is_some()));
        for scope in &scope_ids {
            assert!(state.scope_tree().is_live(*scope));
            assert_eq!(state.scope_tree().parent_of(*scope), None);
        }

        let modules = [source_module, alias_module, shadow_binding.module];
        let interfaces: HashMap<_, _> = modules
            .into_iter()
            .enumerate()
            .map(|(index, module)| {
                let bytes: Arc<[u8]> = Arc::from([index as u8 + 17]);
                state.retain_fixture_value_interface(module, bytes.clone());
                (module, bytes)
            })
            .collect();

        let alias_lease_baseline = state.bindings().lease_count(alias_id);
        let source_lease_baseline = state.bindings().lease_count(source_id);
        let expected_lease = alias_dependency_closure(alias_id, source_id);
        let first_lease = state.acquire_binding_leases([alias_id]);
        assert_eq!(first_lease, expected_lease);
        assert_eq!(
            state.bindings().lease_count(alias_id),
            alias_lease_baseline + 1
        );
        assert_eq!(
            state.bindings().lease_count(source_id),
            source_lease_baseline + 1
        );
        let second_lease = state.acquire_binding_leases([alias_id]);
        assert_eq!(second_lease, expected_lease);
        assert_eq!(
            state.bindings().lease_count(alias_id),
            alias_lease_baseline + 2
        );
        assert_eq!(
            state.bindings().lease_count(source_id),
            source_lease_baseline + 2
        );
        model.explicit = vec![Some(expected_lease.clone()), Some(expected_lease)];
        support.explicit_alias_leases = 2;

        let handles = BTreeMap::from([(0, source_handle), (1, shadow_handle)]);
        let live = check_state(&state, &model, &handles, &interfaces);
        assert_eq!(live.len(), 5);

        let captures = [Some(before), Some(after), chained];
        let mut explicit_sets = vec![Some(first_lease), Some(second_lease)];
        let mut actions = (0_u8..6).collect::<Vec<_>>();
        // Every generated action executes. An absent optional capture has no
        // retirement action, and broad histories allow the owner to go last.
        if chained.is_none() {
            actions.retain(|action| *action != 3);
        }
        actions.sort_by_key(|action| {
            let Some(capture_first) = plan.guided_capture_first else {
                return (0, 0, 0, plan.release_ranks[usize::from(*action)], *action);
            };
            if *action == 0 {
                (0_u8, 0_u8, 0_u8, 0_u8, *action)
            } else {
                let capture = (1..=3).contains(action);
                let partition = if capture_first {
                    if capture {
                        1
                    } else {
                        2
                    }
                } else if capture {
                    2
                } else {
                    1
                };
                let capture_tier = if capture { u8::from(*action == 1) } else { 0 };
                (
                    1,
                    partition,
                    capture_tier,
                    plan.release_ranks[usize::from(*action)],
                    *action,
                )
            }
        });
        let capture_position = actions
            .iter()
            .position(|action| (1..=3).contains(action))
            .unwrap();
        let explicit_position = actions.iter().position(|action| *action >= 4).unwrap();
        if capture_position < explicit_position {
            support.capture_before_explicit_release += 1;
        } else {
            support.explicit_before_capture_release += 1;
        }
        for action in actions {
            let before_live = model_live_ids(&model);
            let before_handles = model_handles(&model, &before_live);
            let before_roots = state.persistent_roots_count();
            let released_roots = match action {
                0 => {
                    let receipt = state.retire_scope(owner);
                    model.owner_retired = true;
                    model.scopes.get_mut(&owner).unwrap().live = false;
                    let retained = model_live_ids(&model);
                    support.pending_owner_bindings += retained.len();
                    support.owner_retired_with_holders += usize::from(!retained.is_empty());
                    support.owner_retired_without_holders += usize::from(retained.is_empty());
                    receipt.roots_released
                }
                1..=3 => {
                    let index = usize::from(action - 1);
                    if let Some(scope) = captures[index] {
                        let receipt = state.retire_scope(scope);
                        model.scopes.get_mut(&scope).unwrap().live = false;
                        support.capture_releases += 1;
                        receipt.roots_released
                    } else {
                        0
                    }
                }
                4..=5 => {
                    let index = usize::from(action - 4);
                    if let Some(lease) = explicit_sets[index].take() {
                        let prior_counts: HashMap<_, _> = lease
                            .iter()
                            .map(|id| (*id, state.bindings().lease_count(*id)))
                            .collect();
                        let mut ids = lease.iter().copied().collect::<Vec<_>>();
                        ids.sort_by_key(|id| id.raw());
                        if plan.release_ranks[usize::from(action)] % 2 != 0 {
                            ids.reverse();
                        }
                        let released = state.release_binding_leases(ids);
                        let released_roots = state.release_binding_roots(released);
                        model.explicit[index] = None;
                        for id in lease {
                            assert_eq!(
                                state.bindings().lease_count(id),
                                prior_counts[&id].saturating_sub(1),
                                "explicit lease decrement for {id:?}"
                            );
                        }
                        support.explicit_releases += 1;
                        released_roots
                    } else {
                        0
                    }
                }
                _ => unreachable!(),
            };
            let after_live = model_live_ids(&model);
            let after_handles = model_handles(&model, &after_live);
            let expected_released = before_handles.difference(&after_handles).count();
            assert_eq!(released_roots, expected_released, "release action {action}");
            assert_eq!(
                before_roots - state.persistent_roots_count(),
                released_roots,
                "GC-root ledger delta for action {action}"
            );
            support.held_root_receipts +=
                usize::from(expected_released == 0 && !before_handles.is_empty());
            support.released_handles += expected_released;
            check_state(&state, &model, &handles, &interfaces);
            if let Some(scope) = chained {
                support.chained_capture_outlived_issuer +=
                    usize::from(model.scopes[&scope].live && !model.scopes[&before].live);
            }
            let after_source = after_live.contains(&source_id);
            let after_pair = after_live.contains(&SessionVarId::from_extract(ROOTED_IDS[1]));
            if after_source && !after_pair {
                support.shared_pair_survived_member_eviction += 1;
                if state.retained_value_interface(source_module).is_some() {
                    support.interface_survived_single_member += 1;
                }
            }

            for &(scope_selector, name_selector) in &plan.reads {
                let query_scopes = [owner, before, after, chained.unwrap_or(before)];
                let query_names = ["source", "pair", "other", "alias"];
                let scope = query_scopes[usize::from(scope_selector) % query_scopes.len()];
                let name = query_names[usize::from(name_selector) % query_names.len()];
                let scope_model = &model.scopes[&scope];
                let expected = scope_model
                    .live
                    .then(|| scope_model.visible.get(name).copied())
                    .flatten()
                    .filter(|id| after_live.contains(id));
                assert_eq!(
                    state.resolve_in(scope, name).map(|entry| entry.id),
                    expected,
                    "generated read {scope:?}/{name} after action {action}"
                );
                support.reads += 1;
                support.dead_scope_reads += usize::from(!scope_model.live);
            }
        }

        assert_eq!(support.same_generation_originals, 3);
        assert_eq!(support.same_generation_identity_pair, 2);
        assert_eq!(support.shared_handle_bindings, 4);
        assert_eq!(support.aliases, 1);
        assert_eq!(support.shadows, 1);
        assert_eq!(support.captures_before_shadow, 1);
        assert_eq!(support.captures_after_shadow, 1);
        assert_eq!(support.explicit_alias_leases, 2);
        assert_eq!(support.capture_releases, 2 + support.chained_captures);
        assert_eq!(support.explicit_releases, 2);
        assert_eq!(support.released_handles, 2);
        assert_eq!(support.alias_hidden_from_capture, 1);
        assert_eq!(
            model_live_ids(&model),
            HashSet::new(),
            "support: {support:?}"
        );
        assert_eq!(state.value_handle_count(), 0, "support: {support:?}");
        assert!(state.live_val_modules().is_empty());

        let old_raw = source_handle.raw();
        let fresh =
            super::super::prepared::tests::rooted_publication_fixture(&mut state, "fresh", 12_200);
        let fresh_handle = fresh.value.handle;
        assert_ne!(
            old_raw,
            fresh_handle.raw(),
            "released handle identity cannot revive"
        );
        assert!(state
            .prepared()
            .unwrap()
            .prepared_handle_of(old_raw)
            .is_none());
        let fresh_scope = state.mint_isolated_scope();
        assert!(
            !scope_ids.contains(&fresh_scope),
            "scope identity cannot revive"
        );
        state.bind_in(fresh_scope, fresh).unwrap();
        assert_eq!(state.retire_scope(fresh_scope).roots_released, 1);
        assert_eq!(state.value_handle_count(), 0);
        support
    }

    fn source_identity_entry(
        state: &mut PersistentSession,
        owner: ScopeId,
        identity: tidepool_repr::execution_schema::SymbolIdentity,
        handle: tidepool_codegen::prepared_program::PreparedHandle,
    ) {
        state
            .bind_in(
                owner,
                BindingEntry {
                    name: tidepool_repr::BindingName("source".into()),
                    id: SessionVarId::from_extract(ROOTED_IDS[0]),
                    module: SessionModule::val(tidepool_repr::Generation(SOURCE_GENERATION)),
                    value: tidepool_codegen::binding_table::BoundValue { handle, identity },
                    type_display: None,
                    defining_expr: None,
                    scope: owner,
                },
            )
            .unwrap();
    }

    fn config() -> Config {
        let mut config = Config::default();
        if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
            config.max_shrink_iters = 4096;
        }
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
        }
        config
    }

    #[derive(Default, serde::Serialize)]
    struct Observed {
        callbacks: usize,
        completed_histories: usize,
        chained_histories: usize,
        owner_with_holders: usize,
        owner_without_holders: usize,
        partial_generation_survival: usize,
        chained_capture_outlived_issuer: usize,
        reads: usize,
        dead_scope_reads: usize,
    }

    #[test]
    fn actual_binding_lifetime_histories_match_rooted_model() {
        // Apply contextual campaign controls after selecting native persistence.
        let mut config = proptest::test_runner::contextualize_config(config());
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::actual_binding_lifetime_histories_match_rooted_model"
        ));
        let configuration = format!("{config:?}");
        let cases = config.cases;
        let observed = RefCell::new(Observed::default());
        let result = TestRunner::new(config).run(&plan(), |history| {
            observed.borrow_mut().callbacks += 1;
            let support = run_history(history);
            let mut observed = observed.borrow_mut();
            observed.completed_histories += 1;
            observed.chained_histories += usize::from(support.chained_captures != 0);
            observed.owner_with_holders += support.owner_retired_with_holders;
            observed.owner_without_holders += support.owner_retired_without_holders;
            observed.partial_generation_survival += support.interface_survived_single_member;
            observed.chained_capture_outlived_issuer += support.chained_capture_outlived_issuer;
            observed.reads += support.reads;
            observed.dead_scope_reads += support.dead_scope_reads;
            Ok(())
        });
        // Callback counts include persisted replay and shrinking. Explicit
        // cases=0 remains a valid replay setting; qualification checks counts.
        eprintln!(
            "binding_lifetime_campaign={}",
            serde_json::json!({
                "configured_cases": cases,
                "configuration": configuration,
                "observation_scope": "runner callbacks in this process, including replay and shrinking",
                "observed": &*observed.borrow(),
            })
        );
        if let Err(error) = result {
            panic!("actual binding lifetime property failed: {error}");
        }
    }

    #[test]
    fn guided_actual_release_partitions_retain_pair_and_alias_custody() {
        for chain_capture in [false, true] {
            for capture_first in [true, false] {
                let support = run_history(Plan {
                    chain_capture,
                    guided_capture_first: Some(capture_first),
                    release_ranks: if capture_first {
                        vec![0, 4, 0, 2, 3, 4]
                    } else {
                        vec![0, 3, 2, 4, 0, 1]
                    },
                    reads: vec![(0, 0), (1, 3), (2, 1), (3, 2)],
                });
                assert_eq!(support.owner_retired_with_holders, 1, "{support:?}");
                assert!(support.held_root_receipts > 0, "{support:?}");
                assert!(
                    support.shared_pair_survived_member_eviction > 0,
                    "{support:?}"
                );
                assert!(support.interface_survived_single_member > 0, "{support:?}");
                if capture_first {
                    assert_eq!(support.capture_before_explicit_release, 1, "{support:?}");
                    assert_eq!(support.explicit_before_capture_release, 0, "{support:?}");
                } else {
                    assert_eq!(support.explicit_before_capture_release, 1, "{support:?}");
                    assert_eq!(support.capture_before_explicit_release, 0, "{support:?}");
                }
                eprintln!(
                    "actual binding lifetime partition chain={chain_capture} capture_first={capture_first}: {support:?}"
                );
            }
        }
        for release_ranks in [vec![0, 1, 4, 5, 2, 3], vec![5, 0, 1, 2, 3, 4]] {
            let support = run_history(Plan {
                chain_capture: true,
                guided_capture_first: None,
                release_ranks,
                reads: vec![(0, 3), (1, 0), (3, 0)],
            });
            assert!(support.chained_capture_outlived_issuer > 0, "{support:?}");
            assert!(
                support.owner_retired_with_holders > 0 || support.owner_retired_without_holders > 0,
                "{support:?}"
            );
            eprintln!("actual binding lifetime broad release partition: {support:?}");
        }
    }
}
