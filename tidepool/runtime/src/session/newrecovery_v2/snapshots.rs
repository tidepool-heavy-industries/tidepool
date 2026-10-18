//! Resident receipts couple immutable roots to the actual persistent maps.
//! Historical declaration and context blocks are acquired only on demand.
use super::*;
use crate::session::recovery_store::{
    DomainSnapshotRoot, PreparedPublicOwnerChange, PreparedRecoveryWrite, PublicSurfaceRoot,
    RecoveryPublicOwnerState,
};
use cid::Cid;
use fvm_ipld_blockstore::Blockstore;
use imbl::OrdMap;
use parking_lot::Mutex;
use tidepool_toolchain::declaration_join::ExactDeclarationContext;
use tidepool_toolchain::exact_context::{
    hydrate_context, hydrate_map, lookup_map, read_value, MaterializedContext, MaterializedMap,
    Materializer, SnapshotMapRoot, StagedBlocks,
};

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ContextMaterializationWork {
    pub(crate) attempted: u64,
    pub(crate) completed: u64,
    // Packet metrics cover completed attempts. When attempted != completed,
    // any additional byte work from failed attempts remains unknown.
    pub(crate) changed_entries: u64,
    pub(crate) offered_bytes: u64,
    pub(crate) serialized_bytes: u64,
    pub(crate) cid_validation_bytes: u64,
}

#[cfg(test)]
thread_local! {
    static CONTEXT_MATERIALIZATION_WORK: std::cell::Cell<Option<ContextMaterializationWork>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
fn record_context_materialization(update: impl FnOnce(&mut ContextMaterializationWork)) {
    CONTEXT_MATERIALIZATION_WORK.with(|held| {
        if let Some(mut work) = held.get() {
            update(&mut work);
            held.set(Some(work));
        }
    });
}

/// Observe the synchronous issuing operation on this test thread. Outside this
/// scope the observation is absent, never an inferred zero-work measurement.
#[cfg(test)]
pub(crate) fn observe_context_materialization<T>(
    operation: impl FnOnce() -> T,
) -> (T, ContextMaterializationWork) {
    struct Restore(Option<ContextMaterializationWork>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CONTEXT_MATERIALIZATION_WORK.with(|held| held.set(self.0));
        }
    }
    let previous = CONTEXT_MATERIALIZATION_WORK
        .with(|held| held.replace(Some(ContextMaterializationWork::default())));
    let restore = Restore(previous);
    assert!(
        previous.is_none(),
        "context work observations cannot overlap"
    );
    let value = operation();
    let work = CONTEXT_MATERIALIZATION_WORK
        .with(|held| held.get())
        .expect("active context work observation");
    drop(restore);
    (value, work)
}

const VERSION: u32 = 1;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DomainSnapshot {
    version: u32,
    source_session: u64,
    nodes: SnapshotMapRoot,
    artifacts: SnapshotMapRoot,
}
/// A retained domain descriptor permits exact immutable reads without
/// hydrating unrelated metadata maps or historical contexts.
pub(crate) struct RecoveryDomainReader {
    pub(crate) store: Arc<RunRecoveryStore>,
    snapshot: DomainSnapshot,
}
impl std::fmt::Debug for RecoveryDomainReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryDomainReader")
            .field("snapshot", &self.snapshot)
            .finish()
    }
}
impl RecoveryDomainReader {
    pub(crate) fn open(
        store: Arc<RunRecoveryStore>,
        state: &RecoveryDomainState,
    ) -> Result<Self, RecoveryError> {
        store.ensure_ready()?;
        let snapshot: DomainSnapshot = read_value(store.as_ref(), state.snapshot_root.cid())?;
        if snapshot.version != VERSION {
            return Err(error("unsupported domain snapshot version"));
        }
        Ok(Self { store, snapshot })
    }
    pub(crate) fn source_session(&self) -> u64 {
        self.snapshot.source_session
    }
    pub(crate) fn node_ids(&self) -> Result<Vec<Generation>, RecoveryError> {
        let nodes = hydrate_map::<u64, Cid, _>(self.store.as_ref(), &self.snapshot.nodes)?;
        Ok(nodes.facts().keys().copied().map(Generation).collect())
    }
    pub(crate) fn node(&self, id: Generation) -> Result<Option<RecoveryNode>, RecoveryError> {
        let Some(cid) =
            lookup_map::<u64, Cid, _>(self.store.as_ref(), &self.snapshot.nodes, &id.0)?
        else {
            return Ok(None);
        };
        let node = read_value(self.store.as_ref(), &cid)?;
        validate_node(&node, id)?;
        Ok(Some(node))
    }
    pub(crate) fn artifact(
        &self,
        id: ArtifactId,
    ) -> Result<Option<RecoveryArtifactClosure>, RecoveryError> {
        let Some(cid) =
            lookup_map::<ArtifactId, Cid, _>(self.store.as_ref(), &self.snapshot.artifacts, &id)?
        else {
            return Ok(None);
        };
        let artifact = read_value(self.store.as_ref(), &cid)?;
        validate_artifact(&artifact, id)?;
        Ok(Some(artifact))
    }
}

fn validate_node(node: &RecoveryNode, id: Generation) -> Result<(), RecoveryError> {
    if node.id != id
        || node.parent.is_some_and(|parent| parent >= id)
        || node
            .implementation_refs
            .iter()
            .any(|dependency| *dependency >= id)
    {
        return Err(error(
            "declaration block identity or causal ancestry is invalid",
        ));
    }
    Ok(())
}
fn validate_artifact(
    artifact: &RecoveryArtifactClosure,
    id: ArtifactId,
) -> Result<(), RecoveryError> {
    if artifact.artifact_id() != id
        || artifact.declared_artifact_id() != id
        || artifact
            .component_paths()
            .iter()
            .any(|(_, path)| !relative(path))
    {
        return Err(error("artifact block identity or custody path is invalid"));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceSnapshot {
    version: u32,
    #[serde(with = "option_generation_serde")]
    declaration_root: Option<Generation>,
    context_root: Option<ContextRoot>,
    bindings: SnapshotMapRoot,
    source_instances: SnapshotMapRoot,
}
#[derive(Clone, Debug)]
pub(crate) struct RecoveryPublicSurface {
    pub owner: RecoveryPublicOwner,
    pub declaration_root: Option<Generation>,
    pub epoch: u64,
    pub context_root: Option<ContextRoot>,
    pub bindings: OrdMap<String, RecoveryBindingId>,
    pub source_instances: OrdMap<RecoveryPublicSourceInstance, ()>,
    binding_receipt: Option<MaterializedMap<String, RecoveryBindingId>>,
    source_receipt: Option<MaterializedMap<RecoveryPublicSourceInstance, ()>>,
}
impl RecoveryPublicSurface {
    pub(crate) fn empty(owner: RecoveryPublicOwner) -> Self {
        Self {
            owner,
            declaration_root: None,
            epoch: 0,
            context_root: None,
            bindings: OrdMap::new(),
            source_instances: OrdMap::new(),
            binding_receipt: None,
            source_receipt: None,
        }
    }
    /// Apply the owning visibility snapshot to the actual predecessor trees.
    /// Retained values stay shared and the materializer sees only map changes.
    pub(crate) fn set_visibility(
        &mut self,
        bindings: impl IntoIterator<Item = RecoveryPublicBinding>,
        sources: impl IntoIterator<Item = RecoveryPublicSourceInstance>,
    ) {
        let mut next = std::collections::BTreeMap::new();
        for binding in bindings {
            next.insert(binding.name, binding.owner);
        }
        let removed = self
            .bindings
            .keys()
            .filter(|name| !next.contains_key(*name))
            .cloned()
            .collect::<Vec<_>>();
        for name in removed {
            self.bindings.remove(&name);
        }
        for (name, owner) in next {
            if self.bindings.get(&name) != Some(&owner) {
                self.bindings.insert(name, owner);
            }
        }
        let next: std::collections::BTreeSet<_> = sources.into_iter().collect();
        let removed = self
            .source_instances
            .keys()
            .filter(|source| !next.contains(*source))
            .cloned()
            .collect::<Vec<_>>();
        for source in removed {
            self.source_instances.remove(&source);
        }
        for source in next {
            if !self.source_instances.contains_key(&source) {
                self.source_instances.insert(source, ());
            }
        }
    }
}

#[derive(Default, Debug)]
struct Acquired {
    nodes: BTreeMap<(Generation, Cid), Arc<RecoveryNode>>,
    artifacts: BTreeMap<(ArtifactId, Cid), Arc<RecoveryArtifactClosure>>,
    surfaces: BTreeMap<(RecoveryPublicOwner, u64, Cid), Arc<RecoveryPublicSurface>>,
    contexts: BTreeMap<ContextRoot, Arc<MaterializedContext>>,
}

/// Compiler authority and its committed representation travel together when
/// a scope is captured. Editing compiler facts creates `New` again.
#[derive(Clone)]
pub(crate) enum ScopedCompilerContext {
    New(Arc<ExactDeclarationContext>),
    Stored(Arc<StoredCompilerContext>),
}

pub(crate) struct StoredCompilerContext {
    exact: Arc<ExactDeclarationContext>,
    materialized: Arc<MaterializedContext>,
    origin: ContextStoreOrigin,
}

#[derive(Clone)]
struct ContextStoreOrigin {
    store: Arc<RunRecoveryStore>,
    domain: RecoveryDomainId,
    lineage: u64,
}

impl ScopedCompilerContext {
    pub(crate) fn new(exact: Arc<ExactDeclarationContext>) -> Self {
        Self::New(exact)
    }

    pub(crate) fn exact(&self) -> &Arc<ExactDeclarationContext> {
        match self {
            Self::New(exact) => exact,
            Self::Stored(stored) => &stored.exact,
        }
    }
}

/// This association has candidate-owned block custody. It cannot become a
/// cloneable stored context before its own publication is installed.
pub(crate) struct PreparedCompilerContext {
    exact: Arc<ExactDeclarationContext>,
    materialized: Arc<MaterializedContext>,
    origin: ContextStoreOrigin,
}

impl PreparedCompilerContext {
    pub(crate) fn root(&self) -> &ContextRoot {
        self.materialized.root()
    }

    pub(crate) fn exact(&self) -> &Arc<ExactDeclarationContext> {
        &self.exact
    }
}

pub(crate) struct StagedCompilerContextPublication {
    staged: StagedRecoveryManifest,
    context: Option<PreparedCompilerContext>,
}

/// Native installation still owns the store's existing exclusion and recovery
/// obligation. Only its successful acknowledgement releases `Stored`.
pub(crate) struct CompilerContextInstallation {
    installation: crate::session::recovery_store::NativeInstallationReceipt,
    context: Option<PreparedCompilerContext>,
}

static_assertions::assert_not_impl_any!(PreparedCompilerContext: Clone, Copy);
static_assertions::assert_not_impl_any!(StagedCompilerContextPublication: Clone, Copy);
static_assertions::assert_not_impl_any!(CompilerContextInstallation: Clone, Copy);

impl CompilerContextInstallation {
    pub(crate) fn state(&self) -> &RecoveryDomainState {
        &self.installation.state
    }

    pub(crate) fn installed_blocking(self) -> Result<Option<ScopedCompilerContext>, StoreError> {
        self.installation.installed_blocking()?;
        Ok(self.context.map(|context| {
            ScopedCompilerContext::Stored(Arc::new(StoredCompilerContext {
                exact: context.exact,
                materialized: context.materialized,
                origin: context.origin,
            }))
        }))
    }
}

impl StagedCompilerContextPublication {
    pub(crate) fn graph(&self) -> &RecoveryGraph {
        &self.staged.graph
    }

    pub(crate) fn expected_revision(&self) -> StoreRevision {
        self.staged.prepared.expected_revision
    }

    pub(crate) fn work(&self) -> crate::session::RecoveryPublicationWork {
        self.staged.work
    }

    pub(crate) fn commit(self) -> StoreDisposition<(RecoveryGraph, CompilerContextInstallation)> {
        match self.staged.commit() {
            StoreDisposition::Committed((graph, installation)) => StoreDisposition::Committed((
                graph,
                CompilerContextInstallation {
                    installation,
                    context: self.context,
                },
            )),
            StoreDisposition::Refused(error) => StoreDisposition::Refused(error),
            StoreDisposition::RecoveryRequired(error) => StoreDisposition::RecoveryRequired(error),
        }
    }
}

#[derive(Clone)]
pub(crate) struct RecoveryGraph {
    store: Arc<RunRecoveryStore>,
    domain: RecoveryDomainId,
    source_session: u64,
    lineage: u64,
    revision: StoreRevision,
    content_revision: StoreRevision,
    root: Option<DomainSnapshotRoot>,
    ceiling: Generation,
    nodes: OrdMap<u64, Cid>,
    artifacts: OrdMap<ArtifactId, Cid>,
    node_receipt: Option<MaterializedMap<u64, Cid>>,
    artifact_receipt: Option<MaterializedMap<ArtifactId, Cid>>,
    owner_roots: OrdMap<RecoveryPublicOwner, RecoveryPublicOwnerState>,
    /// Only newly prepared immutable bytes, retained until transaction settlement.
    pending: OrdMap<Cid, Arc<[u8]>>,
    public_changes: Vec<PreparedPublicOwnerChange>,
    work: crate::session::RecoveryPublicationWork,
    /// Coupled receipts created by this graph's materializer. Cancelled
    /// candidates cannot contribute receipts without their owned block packet.
    materialized_contexts: OrdMap<ContextRoot, Arc<MaterializedContext>>,
    acquired: Arc<Mutex<Acquired>>,
}
impl std::fmt::Debug for RecoveryGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryGraph")
            .field("domain", &self.domain)
            .field("revision", &self.revision)
            .field("nodes", &self.nodes.len())
            .field("artifacts", &self.artifacts.len())
            .finish()
    }
}
impl Blockstore for RecoveryGraph {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        self.store.ensure_ready()?;
        if let Some(bytes) = self.pending.get(cid) {
            return Ok(Some(bytes.to_vec()));
        }
        self.store.get(cid)
    }
    fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
        anyhow::bail!("recovery snapshots require an operation-local materializer")
    }
}
impl RecoveryGraph {
    fn context_origin(&self) -> ContextStoreOrigin {
        ContextStoreOrigin {
            store: self.store.clone(),
            domain: self.domain,
            lineage: self.lineage,
        }
    }

    fn owns_context_receipt(
        &self,
        origin: &ContextStoreOrigin,
        context: &Arc<MaterializedContext>,
    ) -> bool {
        if !self.store.shares_owner(&origin.store)
            || self.domain != origin.domain
            || self.lineage != origin.lineage
        {
            return false;
        }
        let root = context.root();
        self.materialized_contexts
            .get(root)
            .is_some_and(|owned| Arc::ptr_eq(owned, context))
            || self
                .acquired
                .lock()
                .contexts
                .get(root)
                .is_some_and(|owned| Arc::ptr_eq(owned, context))
    }

    pub(crate) fn stage_compiler_context(
        &self,
        artifact_root: &Path,
        context: &ScopedCompilerContext,
        previous: Option<&ContextRoot>,
    ) -> Result<(Self, PreparedCompilerContext), crate::session::SessionError> {
        self.store
            .ensure_ready()
            .map_err(|error| crate::session::SessionError::RecoveryStore(Box::new(error)))?;
        if let ScopedCompilerContext::Stored(stored) = context {
            if self.owns_context_receipt(&stored.origin, &stored.materialized) {
                return Ok((
                    self.clone(),
                    PreparedCompilerContext {
                        exact: stored.exact.clone(),
                        materialized: stored.materialized.clone(),
                        origin: stored.origin.clone(),
                    },
                ));
            }
        }
        let artifacts = super::materialize_missing_context_artifacts(
            self,
            artifact_root,
            context.exact(),
            previous,
        )?;
        let mut candidate = self.candidate();
        for artifact in artifacts {
            candidate
                .insert_artifact(artifact)
                .map_err(|error| crate::session::recovery::graph_error(artifact_root, error))?;
        }
        let materialized = candidate
            .materialize_compiler_context(context.exact(), previous)
            .map_err(|error| crate::session::recovery::graph_error(artifact_root, error))?;
        Ok((
            candidate
                .seal()
                .map_err(|error| crate::session::recovery::graph_error(artifact_root, error))?,
            PreparedCompilerContext {
                exact: context.exact().clone(),
                materialized,
                origin: self.context_origin(),
            },
        ))
    }

    pub(crate) fn empty(
        store: Arc<RunRecoveryStore>,
        domain: RecoveryDomainId,
        source_session: u64,
    ) -> Self {
        Self {
            store,
            domain,
            source_session,
            lineage: source_session,
            revision: StoreRevision(0),
            content_revision: StoreRevision(0),
            root: None,
            ceiling: Generation(0),
            nodes: OrdMap::new(),
            artifacts: OrdMap::new(),
            node_receipt: None,
            artifact_receipt: None,
            owner_roots: OrdMap::new(),
            pending: OrdMap::new(),
            public_changes: Vec::new(),
            work: Default::default(),
            materialized_contexts: OrdMap::new(),
            acquired: Arc::new(Mutex::new(Acquired::default())),
        }
    }
    pub(crate) fn hydrate_domain(
        store: Arc<RunRecoveryStore>,
        domain: RecoveryDomainId,
        state: &RecoveryDomainState,
    ) -> Result<Self, RecoveryError> {
        store.ensure_ready()?;
        let snapshot: DomainSnapshot = read_value(store.as_ref(), state.snapshot_root.cid())?;
        if snapshot.version != VERSION {
            return Err(error("unsupported domain snapshot version"));
        }
        let nodes = hydrate_map::<u64, Cid, _>(store.as_ref(), &snapshot.nodes)?;
        let artifacts = hydrate_map::<ArtifactId, Cid, _>(store.as_ref(), &snapshot.artifacts)?;
        let owners = store.public_owner_states(domain)?;
        let graph = Self {
            source_session: snapshot.source_session,
            lineage: state.lineage,
            revision: state.revision,
            content_revision: state.content_revision,
            root: Some(state.snapshot_root.clone()),
            ceiling: state.allocation_ceiling,
            nodes: nodes.facts().clone(),
            artifacts: artifacts.facts().clone(),
            node_receipt: Some(nodes),
            artifact_receipt: Some(artifacts),
            owner_roots: owners
                .into_iter()
                .map(|owner| (owner.owner.clone(), owner))
                .collect(),
            store,
            domain,
            pending: OrdMap::new(),
            public_changes: Vec::new(),
            work: Default::default(),
            materialized_contexts: OrdMap::new(),
            acquired: Arc::new(Mutex::new(Acquired::default())),
        };
        graph.load_public_surfaces()?;
        Ok(graph)
    }
    pub(crate) fn store(&self) -> &Arc<RunRecoveryStore> {
        &self.store
    }
    pub(crate) fn domain_id(&self) -> RecoveryDomainId {
        self.domain
    }
    pub(crate) fn store_revision(&self) -> StoreRevision {
        self.revision
    }
    pub(crate) fn high_water(&self) -> Generation {
        self.ceiling
    }
    pub(crate) fn source_session(&self) -> u64 {
        self.source_session
    }
    pub(crate) fn lineage(&self) -> u64 {
        self.lineage
    }
    pub(crate) fn node_ids(&self) -> impl Iterator<Item = Generation> + '_ {
        self.nodes.keys().copied().map(Generation)
    }
    pub(crate) fn artifact_ids(&self) -> impl Iterator<Item = ArtifactId> + '_ {
        self.artifacts.keys().copied()
    }
    pub(crate) fn contains_artifact(&self, id: ArtifactId) -> bool {
        self.artifacts.contains_key(&id)
    }
    pub(crate) fn public_owners(&self) -> impl Iterator<Item = &RecoveryPublicOwner> {
        self.owner_roots.keys()
    }
    pub(crate) fn public_owner_state(
        &self,
        owner: &RecoveryPublicOwner,
    ) -> Option<&RecoveryPublicOwnerState> {
        self.owner_roots.get(owner)
    }
    pub(crate) fn load_public_surfaces(&self) -> Result<(), RecoveryError> {
        for owner in self.public_owners() {
            self.surface(owner)?;
        }
        Ok(())
    }
    pub(crate) fn public_surfaces(&self) -> impl Iterator<Item = Arc<RecoveryPublicSurface>> + '_ {
        // Attachment acquires active surfaces before exposing this resident view.
        let acquired = self.acquired.lock();
        self.owner_roots
            .keys()
            .filter_map(|owner| {
                acquired
                    .surfaces
                    .get(&(
                        owner.clone(),
                        self.owner_roots[owner].epoch,
                        *self.owner_roots[owner].surface_root.cid(),
                    ))
                    .cloned()
            })
            .collect::<Vec<_>>()
            .into_iter()
    }
    pub(crate) fn node(&self, id: Generation) -> Result<Option<Arc<RecoveryNode>>, RecoveryError> {
        self.store.ensure_ready()?;
        let Some(cid) = self.nodes.get(&id.0) else {
            return Ok(None);
        };
        if let Some(node) = self.acquired.lock().nodes.get(&(id, *cid)).cloned() {
            return Ok(Some(node));
        }
        let node: RecoveryNode = read_value(self, cid)?;
        validate_node(&node, id)?;
        let node = Arc::new(node);
        self.acquired.lock().nodes.insert((id, *cid), node.clone());
        Ok(Some(node))
    }
    pub(crate) fn artifact(
        &self,
        id: ArtifactId,
    ) -> Result<Option<Arc<RecoveryArtifactClosure>>, RecoveryError> {
        self.store.ensure_ready()?;
        let Some(cid) = self.artifacts.get(&id) else {
            return Ok(None);
        };
        if let Some(artifact) = self.acquired.lock().artifacts.get(&(id, *cid)).cloned() {
            return Ok(Some(artifact));
        }
        let artifact: RecoveryArtifactClosure = read_value(self, cid)?;
        validate_artifact(&artifact, id)?;
        let artifact = Arc::new(artifact);
        self.acquired
            .lock()
            .artifacts
            .insert((id, *cid), artifact.clone());
        Ok(Some(artifact))
    }
    pub(crate) fn context(
        &self,
        root: &ContextRoot,
    ) -> Result<Arc<MaterializedContext>, RecoveryError> {
        self.store.ensure_ready()?;
        if let Some(context) = self.materialized_contexts.get(root) {
            return Ok(context.clone());
        }
        self.acquire_context(root)
    }

    /// Acquired receipts have actual store block custody. Candidate packets
    /// cannot enter this owner, even when their root matches a requested CID.
    pub(crate) fn acquire_context(
        &self,
        root: &ContextRoot,
    ) -> Result<Arc<MaterializedContext>, RecoveryError> {
        self.store.ensure_ready()?;
        if let Some(context) = self.acquired.lock().contexts.get(root).cloned() {
            return Ok(context);
        }
        let context = Arc::new(hydrate_context(self.store.as_ref(), root)?);
        self.acquired
            .lock()
            .contexts
            .insert(root.clone(), context.clone());
        Ok(context)
    }

    pub(crate) fn recover_compiler_context(
        &self,
        inventory: &tidepool_toolchain::declaration_join::RecoveredArtifactInventory,
        root: &ContextRoot,
    ) -> Result<ScopedCompilerContext, crate::session::SessionError> {
        let receipt = self
            .acquire_context(root)
            .map_err(|error| crate::session::recovery::graph_error(self.store.path(), error))?;
        let snapshot = receipt.snapshot();
        let mut exact = inventory.context_with_snapshot(snapshot)?;
        if exact.toolchain_identity_sha256() != snapshot.producer_sha256() {
            return Err(crate::session::SessionError::WrongPublicManifestTicket);
        }
        exact.bind_snapshot_baseline(&receipt)?;
        Ok(ScopedCompilerContext::Stored(Arc::new(
            StoredCompilerContext {
                exact: Arc::new(exact),
                materialized: receipt,
                origin: self.context_origin(),
            },
        )))
    }
    pub(crate) fn surface(
        &self,
        owner: &RecoveryPublicOwner,
    ) -> Result<Option<Arc<RecoveryPublicSurface>>, RecoveryError> {
        self.store.ensure_ready()?;
        let Some(header) = self.owner_roots.get(owner) else {
            return Ok(None);
        };
        if let Some(surface) = self
            .acquired
            .lock()
            .surfaces
            .get(&(owner.clone(), header.epoch, *header.surface_root.cid()))
            .cloned()
        {
            return Ok(Some(surface));
        }
        let snapshot: SurfaceSnapshot = read_value(self, header.surface_root.cid())?;
        if snapshot.version != VERSION
            || snapshot
                .declaration_root
                .is_some_and(|id| !self.nodes.contains_key(&id.0))
        {
            return Err(error(
                "public surface has invalid version or declaration root",
            ));
        }
        let bindings = hydrate_map::<String, RecoveryBindingId, _>(self, &snapshot.bindings)?;
        let sources =
            hydrate_map::<RecoveryPublicSourceInstance, (), _>(self, &snapshot.source_instances)?;
        let surface = Arc::new(RecoveryPublicSurface {
            owner: owner.clone(),
            epoch: header.epoch,
            declaration_root: snapshot.declaration_root,
            context_root: snapshot.context_root,
            bindings: bindings.facts().clone(),
            source_instances: sources.facts().clone(),
            binding_receipt: Some(bindings),
            source_receipt: Some(sources),
        });
        self.acquired.lock().surfaces.insert(
            (owner.clone(), header.epoch, *header.surface_root.cid()),
            surface.clone(),
        );
        Ok(Some(surface))
    }
    pub(crate) fn candidate(&self) -> RecoveryGraphCandidate {
        RecoveryGraphCandidate {
            graph: self.clone(),
        }
    }
    pub(crate) fn domain_root(&self) -> Option<&DomainSnapshotRoot> {
        self.root.as_ref()
    }
    pub(crate) fn public_surface_root(
        &self,
        owner: &RecoveryPublicOwner,
    ) -> Option<&PublicSurfaceRoot> {
        self.owner_roots
            .get(owner)
            .map(|header| &header.surface_root)
    }
    pub(crate) fn install_ceiling_revision(
        &mut self,
        revision: StoreRevision,
        content_revision: StoreRevision,
        ceiling: Generation,
    ) {
        self.revision = revision;
        self.content_revision = content_revision;
        self.ceiling = ceiling;
    }
    pub(crate) fn content_revision(&self) -> StoreRevision {
        self.content_revision
    }
    pub(crate) fn prepare_owner_transfer(
        &self,
        previous: &RecoveryPublicOwner,
        successor: RecoveryPublicOwner,
        epoch: u64,
    ) -> Result<Self, RecoveryError> {
        let mut graph = self.clone();
        let header = self
            .owner_roots
            .get(previous)
            .ok_or_else(|| error("transfer predecessor is absent"))?;
        let old = self
            .surface(previous)?
            .ok_or_else(|| error("transfer predecessor surface is absent"))?;
        let mut surface = (*old).clone();
        surface.owner = successor.clone();
        surface.epoch = epoch;
        graph.owner_roots.remove(previous);
        graph.owner_roots.insert(
            successor.clone(),
            RecoveryPublicOwnerState {
                owner: successor.clone(),
                epoch,
                surface_root: header.surface_root.clone(),
            },
        );
        graph.acquired.lock().surfaces.insert(
            (successor, epoch, *header.surface_root.cid()),
            Arc::new(surface),
        );
        Ok(graph)
    }
    pub(crate) fn materialize_domain(
        mut self,
    ) -> Result<(Self, DomainSnapshotRoot, Vec<(Cid, Arc<[u8]>)>), RecoveryError> {
        let mut materializer = Materializer::new(&self);
        let nodes = materializer.materialize_map(self.node_receipt.as_ref(), &self.nodes)?;
        let artifacts =
            materializer.materialize_map(self.artifact_receipt.as_ref(), &self.artifacts)?;
        let root = DomainSnapshotRoot::new(materializer.materialize_value(&DomainSnapshot {
            version: VERSION,
            source_session: self.source_session,
            nodes: nodes.root().clone(),
            artifacts: artifacts.root().clone(),
        })?);
        let packet = materializer.finish()?;
        self.node_receipt = Some(nodes);
        self.artifact_receipt = Some(artifacts);
        self.stage_packet(packet);
        let blocks = self
            .pending
            .iter()
            .map(|(cid, bytes)| (*cid, bytes.clone()))
            .collect();
        Ok((self, root, blocks))
    }
    pub(crate) fn install_store_state(&mut self, state: &RecoveryDomainState) {
        self.revision = state.revision;
        self.content_revision = state.content_revision;
        self.ceiling = state.allocation_ceiling;
        self.lineage = state.lineage;
        self.root = Some(state.snapshot_root.clone());
        self.pending.clear();
        self.public_changes.clear();
        self.work = Default::default();
    }

    fn stage_packet(&mut self, packet: StagedBlocks) {
        let metrics = packet.metrics();
        self.work.changed_entries += metrics.changed_entries;
        self.work.offered_bytes += metrics.offered_bytes;
        self.work.serialized_bytes += metrics.serialized_bytes;
        self.work.cid_validation_bytes += metrics.cid_validation_bytes;
        for (cid, bytes) in packet.into_blocks() {
            self.pending.insert(cid, bytes);
        }
    }
}

pub(crate) struct RecoveryGraphCandidate {
    graph: RecoveryGraph,
}
impl RecoveryGraphCandidate {
    fn encode<T: Serialize>(&mut self, value: &T) -> Result<Cid, RecoveryError> {
        let mut materializer = Materializer::new(&self.graph);
        let cid = materializer.materialize_value(value)?;
        let packet = materializer.finish()?;
        self.graph.stage_packet(packet);
        Ok(cid)
    }
    pub(crate) fn insert_node(&mut self, node: RecoveryNode) -> Result<(), RecoveryError> {
        if node.id.0 == 0
            || self.graph.nodes.contains_key(&node.id.0)
            || node
                .parent
                .is_some_and(|id| id >= node.id || !self.graph.nodes.contains_key(&id.0))
            || node
                .implementation_refs
                .iter()
                .any(|id| *id >= node.id || !self.graph.nodes.contains_key(&id.0))
        {
            return Err(error(
                "declaration insertion violates immutable causal identity",
            ));
        }
        self.graph.context(&node.context_root)?;
        if node
            .artifact_refs
            .iter()
            .any(|id| !self.graph.artifacts.contains_key(id))
        {
            return Err(error(
                "declaration producer artifact is absent from the candidate",
            ));
        }
        let cid = self.encode(&node)?;
        self.graph.nodes.insert(node.id.0, cid);
        self.graph
            .acquired
            .lock()
            .nodes
            .insert((node.id, cid), Arc::new(node));
        Ok(())
    }
    pub(crate) fn insert_artifact(
        &mut self,
        artifact: RecoveryArtifactClosure,
    ) -> Result<(), RecoveryError> {
        let id = artifact.artifact_id();
        if artifact.declared_artifact_id() != id
            || artifact
                .component_paths()
                .iter()
                .any(|(_, path)| !relative(path))
        {
            return Err(error(
                "artifact insertion has invalid identity or custody path",
            ));
        }
        if let Some(existing) = self.graph.artifact(id)? {
            if *existing != artifact {
                return Err(error(
                    "artifact identity cannot replace immutable custody facts",
                ));
            }
            return Ok(());
        }
        let cid = self.encode(&artifact)?;
        self.graph.artifacts.insert(id, cid);
        self.graph
            .acquired
            .lock()
            .artifacts
            .insert((id, cid), Arc::new(artifact));
        Ok(())
    }
    fn materialize_compiler_context(
        &mut self,
        context: &ExactDeclarationContext,
        previous: Option<&ContextRoot>,
    ) -> Result<Arc<MaterializedContext>, RecoveryError> {
        let base = previous.map(|root| self.graph.context(root)).transpose()?;
        let mut materializer = Materializer::new(&self.graph);
        #[cfg(test)]
        record_context_materialization(|work| work.attempted += 1);
        let context =
            Arc::new(materializer.materialize_context(base.as_deref(), &context.snapshot())?);
        let packet = materializer.finish()?;
        #[cfg(test)]
        record_context_materialization(|work| {
            let metrics = packet.metrics();
            work.completed += 1;
            work.changed_entries += metrics.changed_entries;
            work.offered_bytes += metrics.offered_bytes;
            work.serialized_bytes += metrics.serialized_bytes;
            work.cid_validation_bytes += metrics.cid_validation_bytes;
        });
        self.graph.stage_packet(packet);
        let root = context.root().clone();
        // A successful materialization may reproduce an already owned block
        // root. Keep its existing representation receipt so earlier exact
        // owners do not lose their graph-custody association.
        if let Some(owned) = self.graph.materialized_contexts.get(&root) {
            return Ok(owned.clone());
        }
        self.graph
            .materialized_contexts
            .insert(root, context.clone());
        Ok(context)
    }
    pub(crate) fn replace_surface(
        &mut self,
        mut surface: RecoveryPublicSurface,
    ) -> Result<(), RecoveryError> {
        if surface
            .declaration_root
            .is_some_and(|id| !self.graph.nodes.contains_key(&id.0))
        {
            return Err(error(
                "public declaration root is absent from the candidate",
            ));
        }
        if let Some(root) = &surface.context_root {
            self.graph.context(root)?;
        }
        // The receiving graph owns the predecessor receipts and their block
        // custody. A copied surface supplies facts, not another candidate's
        // uncommitted materialization baseline.
        let predecessor = self.graph.surface(&surface.owner)?;
        let previous = self.graph.owner_roots.get(&surface.owner).cloned();
        let mut materializer = Materializer::new(&self.graph);
        let bindings = materializer.materialize_map(
            predecessor
                .as_ref()
                .and_then(|surface| surface.binding_receipt.as_ref()),
            &surface.bindings,
        )?;
        let sources = materializer.materialize_map(
            predecessor
                .as_ref()
                .and_then(|surface| surface.source_receipt.as_ref()),
            &surface.source_instances,
        )?;
        let root = PublicSurfaceRoot::new(materializer.materialize_value(&SurfaceSnapshot {
            version: VERSION,
            declaration_root: surface.declaration_root,
            context_root: surface.context_root.clone(),
            bindings: bindings.root().clone(),
            source_instances: sources.root().clone(),
        })?);
        let packet = materializer.finish()?;
        surface.binding_receipt = Some(bindings);
        surface.source_receipt = Some(sources);
        self.graph.stage_packet(packet);
        let next = RecoveryPublicOwnerState {
            owner: surface.owner.clone(),
            epoch: surface.epoch,
            surface_root: root,
        };
        self.graph.public_changes.push(match previous {
            Some(previous) => PreparedPublicOwnerChange::Replace {
                expected_epoch: previous.epoch,
                next: next.clone(),
            },
            None => PreparedPublicOwnerChange::Insert(next.clone()),
        });
        self.graph
            .owner_roots
            .insert(next.owner.clone(), next.clone());
        self.graph.acquired.lock().surfaces.insert(
            (
                surface.owner.clone(),
                surface.epoch,
                *next.surface_root.cid(),
            ),
            Arc::new(surface),
        );
        Ok(())
    }
    pub(crate) fn seal(self) -> Result<RecoveryGraph, RecoveryError> {
        Ok(self.graph)
    }
    pub(crate) fn prepare_publication(
        self,
        base: &crate::session::DurableWriteBase,
    ) -> Result<StagedRecoveryManifest, RecoveryError> {
        if self.graph.domain != base.domain {
            return Err(error(
                "publication candidate belongs to a different durable domain",
            ));
        }
        self.prepare(base.store_revision, base.allocation_ceiling)
    }
    pub(crate) fn prepare(
        self,
        expected: StoreRevision,
        ceiling: Generation,
    ) -> Result<StagedRecoveryManifest, RecoveryError> {
        let (graph, root, blocks) = self.graph.materialize_domain()?;
        let work = crate::session::RecoveryPublicationWork {
            staged_blocks: blocks.len() as u64,
            staged_bytes: blocks.iter().map(|(_, bytes)| bytes.len() as u64).sum(),
            ..graph.work
        };
        let prepared = PreparedRecoveryWrite {
            domain_id: graph.domain,
            expected_revision: expected,
            snapshot_root: root,
            allocation_ceiling: ceiling,
            blocks,
            public_changes: graph.public_changes.clone(),
            generation_change: None,
        };
        Ok(StagedRecoveryManifest {
            graph,
            prepared,
            work,
        })
    }
}

pub(crate) struct StagedRecoveryManifest {
    pub(crate) graph: RecoveryGraph,
    pub(crate) prepared: PreparedRecoveryWrite,
    pub(crate) work: crate::session::RecoveryPublicationWork,
}
impl StagedRecoveryManifest {
    pub(crate) fn with_compiler_context(
        self,
        context: Option<PreparedCompilerContext>,
    ) -> Result<StagedCompilerContextPublication, RecoveryError> {
        if context.as_ref().is_some_and(|context| {
            !self
                .graph
                .owns_context_receipt(&context.origin, &context.materialized)
        }) {
            return Err(error(
                "compiler context does not belong to this staged publication",
            ));
        }
        Ok(StagedCompilerContextPublication {
            staged: self,
            context,
        })
    }

    pub(crate) fn commit(
        self,
    ) -> StoreDisposition<(
        RecoveryGraph,
        crate::session::recovery_store::NativeInstallationReceipt,
    )> {
        let Self {
            mut graph,
            prepared,
            ..
        } = self;
        match graph.store.commit_publication(prepared) {
            StoreDisposition::Committed(receipt) => {
                graph.install_store_state(&receipt.state);
                StoreDisposition::Committed((graph, receipt))
            }
            StoreDisposition::Refused(error) => StoreDisposition::Refused(error),
            StoreDisposition::RecoveryRequired(error) => StoreDisposition::RecoveryRequired(error),
        }
    }

    #[cfg(test)]
    fn publish_and_install<T>(
        self,
        install: impl FnOnce(RecoveryGraph, RecoveryDomainState) -> T,
    ) -> StoreDisposition<T> {
        match self.commit() {
            StoreDisposition::Committed((graph, receipt)) => {
                let value = install(graph, receipt.state.clone());
                match receipt.installed_blocking() {
                    Ok(()) => StoreDisposition::Committed(value),
                    Err(error) => StoreDisposition::RecoveryRequired(error),
                }
            }
            StoreDisposition::Refused(error) => StoreDisposition::Refused(error),
            StoreDisposition::RecoveryRequired(error) => StoreDisposition::RecoveryRequired(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{recovery_store::StoreFault, SessionId};
    use proptest::prelude::*;

    fn owner(incarnation: u64) -> RecoveryPublicOwner {
        RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root").unwrap(),
            incarnation,
        )
        .unwrap()
    }
    fn fixture() -> (tempfile::TempDir, RecoveryGraph) {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(RunRecoveryStore::create(&directory.path().join("recovery.sqlite")).unwrap());
        let (graph, _, _) =
            load_or_create_domain_graph(store, RecoveryDomainId::Root, 7, Generation(u64::MAX))
                .unwrap();
        (directory, graph)
    }
    fn context(candidate: &mut RecoveryGraphCandidate) -> ContextRoot {
        let context =
            tidepool_toolchain::declaration_join::ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap();
        candidate
            .materialize_compiler_context(&context, None)
            .unwrap()
            .root()
            .clone()
    }
    fn new_scoped_context() -> ScopedCompilerContext {
        ScopedCompilerContext::new(Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![]).unwrap(),
        ))
    }

    fn stage_scoped_context(
        base: &RecoveryGraph,
        context: &ScopedCompilerContext,
        incarnation: u64,
    ) -> (
        ScopedCompilerContext,
        ContextRoot,
        StagedCompilerContextPublication,
    ) {
        let (graph, context) = base
            .stage_compiler_context(base.store.path().parent().unwrap(), context, None)
            .unwrap();
        let root = context.root().clone();
        let installing = ScopedCompilerContext::new(context.exact().clone());
        let mut surface = RecoveryPublicSurface::empty(owner(incarnation));
        surface.context_root = Some(root.clone());
        let mut candidate = graph.candidate();
        candidate.replace_surface(surface).unwrap();
        let staged = candidate
            .prepare(base.store_revision(), base.high_water())
            .unwrap()
            .with_compiler_context(Some(context))
            .unwrap();
        (installing, root, staged)
    }

    fn publish_scoped_context(
        base: &RecoveryGraph,
        context: &ScopedCompilerContext,
        incarnation: u64,
    ) -> (RecoveryGraph, ScopedCompilerContext, ContextRoot) {
        let (installing, root, staged) = stage_scoped_context(base, context, incarnation);
        let (graph, installation) = match staged.commit() {
            StoreDisposition::Committed(value) => value,
            _ => panic!("context publication must commit"),
        };
        // This component fixture models installation before acknowledgement;
        // scope inheritance is exercised separately through PersistentSession.
        assert!(matches!(installing, ScopedCompilerContext::New(_)));
        let installed = installation.installed_blocking().unwrap().unwrap();
        assert!(Arc::ptr_eq(installing.exact(), installed.exact()));
        (graph, installed, root)
    }

    #[test]
    fn stored_context_custody_reuses_issuer_but_equal_facts_reenter_admission() {
        let (_directory, base) = fixture();
        let new_a = new_scoped_context();
        let ((graph, a, a_root), first) =
            observe_context_materialization(|| publish_scoped_context(&base, &new_a, 1));
        assert_eq!((first.attempted, first.completed), (1, 1));
        assert!(first.serialized_bytes > 0 && first.offered_bytes > 0);
        let ((graph, captured_a, root), reuse) =
            observe_context_materialization(|| publish_scoped_context(&graph, &a.clone(), 2));
        assert_eq!(reuse, ContextMaterializationWork::default());
        assert_eq!(root, a_root);
        assert!(Arc::ptr_eq(a.exact(), captured_a.exact()));

        let new_b = new_scoped_context();
        assert!(new_a.exact() == new_b.exact());
        assert!(!Arc::ptr_eq(new_a.exact(), new_b.exact()));
        let ((graph, b, b_root), independent) =
            observe_context_materialization(|| publish_scoped_context(&graph, &new_b, 3));
        assert_eq!((independent.attempted, independent.completed), (1, 1));
        assert_eq!(
            b_root, a_root,
            "equal output CIDs are permitted after admission"
        );
        assert!(!Arc::ptr_eq(a.exact(), b.exact()));
        let ((graph, again_a, root), again) =
            observe_context_materialization(|| publish_scoped_context(&graph, &a, 4));
        assert_eq!(again, ContextMaterializationWork::default());
        assert_eq!(root, a_root);
        assert!(Arc::ptr_eq(again_a.exact(), captured_a.exact()));
        assert_eq!(graph.public_owners().count(), 4);

        let (_other_directory, other) = fixture();
        let ((other, copied, other_root), foreign) =
            observe_context_materialization(|| publish_scoped_context(&other, &a, 1));
        assert_eq!((foreign.attempted, foreign.completed), (1, 1));
        assert_eq!(other_root, a_root);
        assert!(Arc::ptr_eq(copied.exact(), a.exact()));
        assert!(!other.store.shares_owner(&graph.store));

        let independently_opened = Arc::new(RunRecoveryStore::open(graph.store.path()).unwrap());
        let reopened = load_domain_graph(independently_opened, RecoveryDomainId::Root).unwrap();
        let ((_, _, reopened_root), reopening) =
            observe_context_materialization(|| publish_scoped_context(&reopened, &a, 5));
        assert_eq!((reopening.attempted, reopening.completed), (1, 1));
        assert_eq!(reopened_root, a_root);
    }

    #[test]
    fn context_custody_refuses_foreign_pair_and_never_promotes_abandoned_publication() {
        let (_directory, base) = fixture();
        let context = new_scoped_context();
        let (candidate, pending) = base
            .stage_compiler_context(base.store.path().parent().unwrap(), &context, None)
            .unwrap();
        let root = pending.root().clone();
        assert!(
            candidate.context(&root).is_ok(),
            "candidate checks own their pending representation"
        );
        assert!(
            candidate.acquire_context(&root).is_err(),
            "cold acquisition cannot consume pending bytes"
        );
        assert!(base.acquire_context(&root).is_err());

        let (_other_directory, other) = fixture();
        let (other_candidate, other_pending) = other
            .stage_compiler_context(other.store.path().parent().unwrap(), &context, None)
            .unwrap();
        assert_eq!(
            other_pending.root(),
            &root,
            "same CID in another store is the discriminator"
        );
        assert!(other_candidate
            .candidate()
            .prepare(other.store_revision(), other.high_water())
            .unwrap()
            .with_compiler_context(Some(pending))
            .is_err());
        drop(other_pending);
        drop(candidate);
        assert!(base.acquire_context(&root).is_err());
        assert!(base.store.ensure_ready().is_ok());

        let (_, refused_root, refused) = stage_scoped_context(&base, &context, 1);
        base.store.inject_fault(StoreFault::BeforeCommit);
        assert!(matches!(refused.commit(), StoreDisposition::Refused(_)));
        assert!(base.acquire_context(&refused_root).is_err());
        assert!(base.store.ensure_ready().is_ok());
        assert!(base.public_owners().next().is_none());

        let (_, _, stale) = stage_scoped_context(&base, &context, 2);
        let (graph, stored, _) = publish_scoped_context(&base, &context, 1);
        assert!(matches!(
            stale.commit(),
            StoreDisposition::Refused(crate::session::recovery_store::StoreRefusal::StaleRevision)
        ));
        let ((_, _, _), work) =
            observe_context_materialization(|| publish_scoped_context(&graph, &stored, 3));
        assert_eq!(
            work,
            ContextMaterializationWork::default(),
            "refusal must not invalidate confirmed custody"
        );

        let (_uncertain_directory, uncertain_base) = fixture();
        let (installing, _, staged) = stage_scoped_context(&uncertain_base, &context, 1);
        let (_graph, installation) = match staged.commit() {
            StoreDisposition::Committed(value) => value,
            _ => panic!("uncertainty control requires an actual durable commit"),
        };
        assert!(matches!(installing, ScopedCompilerContext::New(_)));
        drop(installation);
        assert!(
            uncertain_base.store.ensure_ready().is_err(),
            "lost installation acknowledgement retains recovery obligation"
        );
        assert!(uncertain_base
            .stage_compiler_context(
                uncertain_base.store.path().parent().unwrap(),
                &installing,
                None
            )
            .is_err());
    }

    fn node(id: u64, parent: Option<u64>, context_root: ContextRoot, label: &str) -> RecoveryNode {
        RecoveryNode {
            id: Generation(id),
            parent: parent.map(Generation),
            kind: RecoveryNodeKind::Authored,
            implementation_refs: vec![],
            context_root,
            lexical_roots: vec![],
            artifact_refs: vec![],
            exports: vec![],
            retracts: vec![],
            workbench_imports: vec![label.into()],
            instances: RecoveryInstanceInventory::default(),
            live_dependencies: vec![],
            state: RecoveryNodeState::ExactArtifactClosure,
        }
    }
    fn publish(candidate: RecoveryGraphCandidate, base: &RecoveryGraph) -> RecoveryGraph {
        match candidate
            .prepare(base.store_revision(), base.high_water())
            .unwrap()
            .publish_and_install(|graph, _| graph)
        {
            StoreDisposition::Committed(graph) => graph,
            other => panic!("publication refused: {other:?}"),
        }
    }
    fn reopened(graph: &RecoveryGraph) -> RecoveryGraph {
        let store = Arc::new(RunRecoveryStore::open(graph.store().path()).unwrap());
        let state = store.read_domain(graph.domain_id()).unwrap().unwrap();
        RecoveryGraph::hydrate_domain(store, graph.domain_id(), &state).unwrap()
    }

    #[test]
    fn selected_domain_observation_retains_roots_and_refuses_selected_damage() {
        let (_directory, base) = fixture();
        let mut first = base.candidate();
        let root = context(&mut first);
        first
            .insert_node(node(1, None, root.clone(), "first"))
            .unwrap();
        let first = publish(first, &base);
        let observe = || {
            crate::session::observe_declaration_domain(first.store().path(), RecoveryDomainId::Root)
                .unwrap()
                .unwrap()
        };
        let retained = observe();
        assert_eq!(retained.source_session, 7);
        assert_eq!(retained.allocation_ceiling, Generation(u64::MAX));
        assert_eq!(
            retained.declaration_generations().unwrap(),
            vec![Generation(1)]
        );
        assert!(retained.declaration(Generation(1)).unwrap().is_some());
        assert!(retained.declaration(Generation(2)).unwrap().is_none());
        assert!(retained
            .declaration_context(Generation(1))
            .unwrap()
            .snapshot()
            .binding_dependencies()
            .unwrap()
            .is_empty());
        let mut next = first.candidate();
        next.insert_node(node(2, Some(1), root, "second")).unwrap();
        let next = publish(next, &first);
        assert!(
            retained.declaration(Generation(2)).unwrap().is_none(),
            "later publication cannot widen a retained exact snapshot"
        );
        let selected = observe();
        assert_eq!(
            retained.declaration_generations().unwrap(),
            vec![Generation(1)]
        );
        assert_eq!(
            selected.declaration_generations().unwrap(),
            vec![Generation(1), Generation(2)]
        );
        assert_eq!(
            selected.declaration(Generation(2)).unwrap().unwrap().parent,
            Some(Generation(1))
        );
        let damaged = next.nodes.get(&2).unwrap();
        // Inject damage at raw storage ingress, outside the production writer.
        let raw = rusqlite::Connection::open(next.store().path()).unwrap();
        raw.execute(
            "UPDATE blocks SET bytes=?1 WHERE cid=?2",
            rusqlite::params![
                b"damaged selected declaration".as_slice(),
                damaged.to_bytes()
            ],
        )
        .unwrap();
        let header = observe();
        assert_eq!(
            header.declaration_generations().unwrap(),
            vec![Generation(1), Generation(2)],
            "index observation does not hydrate unrelated damaged bodies"
        );
        assert!(
            header.declaration(Generation(1)).unwrap().is_some(),
            "unselected damaged leaves cannot broaden a narrow observation"
        );
        assert!(matches!(header.declaration(Generation(2)),
            Err(crate::session::SessionError::RecoverySnapshot(error))
                if matches!(*error, SnapshotError::Integrity(cid) if cid == *damaged)));
        raw.execute("DELETE FROM blocks WHERE cid=?1", [damaged.to_bytes()])
            .unwrap();
        assert!(matches!(header.declaration(Generation(2)),
            Err(crate::session::SessionError::RecoverySnapshot(error))
                if matches!(*error, SnapshotError::MissingBlock(cid) if cid == *damaged)));
    }

    #[test]
    fn conflicting_preparations_keep_decoded_caches_bound_to_exact_cids() {
        let (_directory, base) = fixture();
        let mut first = base.candidate();
        let root = context(&mut first);
        first
            .insert_node(node(1, None, root.clone(), "first"))
            .unwrap();
        let first = first
            .prepare(base.store_revision(), base.high_water())
            .unwrap();
        let mut conflicting = base.candidate();
        context(&mut conflicting);
        conflicting
            .insert_node(node(1, None, root, "cancelled-other-facts"))
            .unwrap();
        let conflicting = conflicting
            .prepare(base.store_revision(), base.high_water())
            .unwrap();
        let committed = match first.publish_and_install(|graph, _| graph) {
            StoreDisposition::Committed(graph) => graph,
            other => panic!("first: {other:?}"),
        };
        assert_eq!(
            committed
                .node(Generation(1))
                .unwrap()
                .unwrap()
                .workbench_imports,
            ["first"]
        );
        let installed = std::cell::Cell::new(false);
        assert!(matches!(
            conflicting.publish_and_install(|_, _| installed.set(true)),
            StoreDisposition::Refused(crate::session::recovery_store::StoreRefusal::StaleRevision)
        ));
        assert!(!installed.get());
        assert_eq!(
            reopened(&committed)
                .node(Generation(1))
                .unwrap()
                .unwrap()
                .workbench_imports,
            ["first"]
        );
    }

    #[test]
    fn cancelled_context_cannot_supply_a_required_reference_to_another_candidate() {
        let (_directory, base) = fixture();
        let before = base.store().work().unwrap();
        let mut cancelled = base.candidate();
        let root = context(&mut cancelled);
        drop(cancelled);
        let mut candidate = base.candidate();
        let error = candidate
            .insert_node(node(1, None, root.clone(), "missing"))
            .unwrap_err();
        assert!(
            matches!(error.kind, RecoveryErrorKind::Snapshot(SnapshotError::MissingBlock(cid)) if cid == *root.cid())
        );
        let mut surface = RecoveryPublicSurface::empty(owner(1));
        surface.context_root = Some(root);
        assert!(matches!(
            candidate.replace_surface(surface).unwrap_err().kind,
            RecoveryErrorKind::Snapshot(SnapshotError::MissingBlock(_))
        ));
        assert_eq!(base.store().work().unwrap(), before);
        assert!(base.node(Generation(1)).unwrap().is_none());
        assert!(base.public_owners().next().is_none());
        // A fresh issuing materializer stages its complete packet and retries.
        let root = context(&mut candidate);
        let mut missing_artifact = node(1, None, root.clone(), "missing-artifact");
        missing_artifact.artifact_refs.push(ArtifactId([17; 32]));
        assert!(candidate.insert_node(missing_artifact).is_err());
        assert_eq!(base.store().work().unwrap(), before);
        candidate
            .insert_node(node(1, None, root, "complete"))
            .unwrap();
        let committed = publish(candidate, &base);
        assert_eq!(
            reopened(&committed)
                .node(Generation(1))
                .unwrap()
                .unwrap()
                .workbench_imports,
            ["complete"]
        );
    }

    #[test]
    fn cancelled_surface_receipts_are_rebuilt_from_receiving_candidate() {
        let (_directory, base) = fixture();
        let owning = owner(1);
        let binding = RecoveryBindingId {
            session: u64::MAX,
            variable: u64::MAX,
        };
        let mut authored = RecoveryPublicSurface::empty(owning.clone());
        authored.epoch = 1;
        authored.bindings.insert("winner".into(), binding);
        let mut cancelled = base.candidate();
        cancelled.replace_surface(authored).unwrap();
        let cancelled = cancelled.seal().unwrap();
        let transferred = (*cancelled.surface(&owning).unwrap().unwrap()).clone();
        let binding_root = *transferred.binding_receipt.as_ref().unwrap().root().cid();
        assert!(cancelled.pending.contains_key(&binding_root));
        assert!(base.store().get(&binding_root).unwrap().is_none());
        drop(cancelled);
        assert!(base.surface(&owning).unwrap().is_none());

        let mut receiving = base.candidate();
        receiving.replace_surface(transferred).unwrap();
        let committed = publish(receiving, &base);
        let cold = reopened(&committed);
        let actual = cold.surface(&owning).unwrap().unwrap();
        assert_eq!(actual.epoch, 1);
        assert_eq!(actual.bindings.len(), 1);
        assert_eq!(actual.bindings["winner"], binding);
        assert!(actual.source_instances.is_empty());
    }

    #[test]
    fn binding_tombstones_and_source_removal_preserve_sibling_roots_and_u64() {
        let (_directory, mut graph) = fixture();
        let sibling_domain = RecoveryDomainId::Session(SessionId(u64::MAX));
        let (_, sibling, _) = load_or_create_domain_graph(
            graph.store().clone(),
            sibling_domain,
            u64::MAX,
            Generation(u64::MAX),
        )
        .unwrap();
        let mut candidate = graph.candidate();
        let a = owner(1);
        let b = owner(2);
        let original = RecoveryBindingId {
            session: u64::MAX,
            variable: u64::MAX,
        };
        let source = RecoveryPublicSourceInstance {
            machine_incarnation: u64::MAX,
            instance: u32::MAX,
            module_version: [255; 32],
            binder: RecoverySourceIdentity {
                unit: "main".into(),
                module: "Own".into(),
                namespace: "value".into(),
                occurrence: "entry".into(),
                record_parent: None,
            },
        };
        for owning in [&a, &b] {
            let mut surface = RecoveryPublicSurface::empty(owning.clone());
            surface.epoch = 1;
            surface.bindings.insert("winner".into(), original);
            surface.source_instances.insert(source.clone(), ());
            candidate.replace_surface(surface).unwrap();
        }
        graph = publish(candidate, &graph);
        let root = graph.domain_root().unwrap().clone();
        let sibling_root = graph.public_surface_root(&b).unwrap().clone();
        let mut candidate = graph.candidate();
        let mut surface = (*graph.surface(&a).unwrap().unwrap()).clone();
        surface.epoch += 1;
        surface.bindings.remove("winner");
        surface.source_instances.remove(&source);
        candidate.replace_surface(surface).unwrap();
        let prepared = candidate
            .prepare(graph.store_revision(), graph.high_water())
            .unwrap();
        assert_eq!(prepared.work.changed_entries, 2);
        graph = match prepared.publish_and_install(|graph, _| graph) {
            StoreDisposition::Committed(graph) => graph,
            other => panic!("remove: {other:?}"),
        };
        assert_eq!(graph.domain_root(), Some(&root));
        assert_eq!(graph.public_surface_root(&b), Some(&sibling_root));
        assert_eq!(
            graph.store().read_domain(sibling_domain).unwrap(),
            Some(sibling)
        );
        let cold = reopened(&graph);
        assert!(cold.surface(&a).unwrap().unwrap().bindings.is_empty());
        assert!(cold
            .surface(&a)
            .unwrap()
            .unwrap()
            .source_instances
            .is_empty());
        assert_eq!(
            cold.surface(&b).unwrap().unwrap().bindings["winner"],
            original
        );
        assert!(cold
            .surface(&b)
            .unwrap()
            .unwrap()
            .source_instances
            .contains_key(&source));
        assert_eq!(cold.high_water(), Generation(u64::MAX));
    }

    #[test]
    fn one_node_edit_has_bounded_metadata_work_after_retained_history() {
        let (_directory, mut graph) = fixture();
        let mut candidate = graph.candidate();
        let root = context(&mut candidate);
        for id in 1..=512 {
            candidate
                .insert_node(node(
                    id,
                    (id > 1).then_some(id - 1),
                    root.clone(),
                    "history",
                ))
                .unwrap();
        }
        graph = publish(candidate, &graph);
        let mut candidate = graph.candidate();
        candidate
            .insert_node(node(513, Some(512), root, "new"))
            .unwrap();
        let staged = candidate
            .prepare(graph.store_revision(), graph.high_water())
            .unwrap();
        let work = staged.work;
        assert_eq!(work.changed_entries, 1);
        assert!(work.staged_blocks <= 8, "{work:?}");
        assert!(work.staged_bytes < 16_384, "{work:?}");
        assert!(work.offered_bytes < 16_384, "{work:?}");
        assert!(work.serialized_bytes < 2_048, "{work:?}");
        assert!(work.cid_validation_bytes < 65_536, "{work:?}");
        let before = graph.store().work().unwrap();
        graph = match staged.publish_and_install(|graph, _| graph) {
            StoreDisposition::Committed(graph) => graph,
            other => panic!("delta: {other:?}"),
        };
        let after = graph.store().work().unwrap();
        assert_eq!(after.transactions - before.transactions, 1);
        assert!(after.block_writes - before.block_writes <= work.staged_blocks);
        assert_eq!(graph.node_ids().count(), 513);
        eprintln!(
            "incremental-recovery-work {}",
            serde_json::to_string(&work).unwrap()
        );
    }

    #[test]
    fn cold_domain_does_not_decode_unselected_historical_node_blocks() {
        let (_directory, graph) = fixture();
        let mut candidate = graph.candidate();
        let root = context(&mut candidate);
        candidate
            .insert_node(node(1, None, root, "historical"))
            .unwrap();
        let graph = publish(candidate, &graph);
        let cid = graph.nodes[&1];
        let connection = rusqlite::Connection::open(graph.store().path()).unwrap();
        assert_eq!(
            connection
                .execute("DELETE FROM blocks WHERE cid=?1", [cid.to_bytes()])
                .unwrap(),
            1
        );
        drop(connection);
        let cold = reopened(&graph);
        assert!(cold.acquired.lock().nodes.is_empty());
        assert!(
            matches!(cold.node(Generation(1)).unwrap_err().kind, RecoveryErrorKind::Snapshot(SnapshotError::MissingBlock(missing)) if missing==cid)
        );
    }

    #[derive(Clone, Default)]
    struct Model {
        nodes: BTreeMap<u64, Option<u64>>,
        owners: BTreeMap<u64, (u64, BTreeMap<String, RecoveryBindingId>)>,
        revision: u64,
    }

    #[derive(Clone, Debug)]
    enum SurfacePacketDisposition {
        Cancelled,
        Refused,
        Stale,
    }
    #[derive(Clone, Debug)]
    enum SurfaceEdit {
        PutBinding(u8, u64),
        RemoveBinding(u8),
        PutSource(u64),
        RemoveSource(u64),
    }
    #[derive(Clone, Default)]
    struct SurfaceModel {
        epoch: u64,
        bindings: BTreeMap<String, RecoveryBindingId>,
        sources: std::collections::BTreeSet<RecoveryPublicSourceInstance>,
    }
    fn source(identity: u64) -> RecoveryPublicSourceInstance {
        RecoveryPublicSourceInstance {
            machine_incarnation: identity,
            instance: u32::MAX,
            module_version: [identity as u8; 32],
            binder: RecoverySourceIdentity {
                unit: "main".into(),
                module: "Own".into(),
                namespace: "value".into(),
                occurrence: format!("source{identity}"),
                record_parent: None,
            },
        }
    }
    fn surface_operation() -> impl Strategy<Value = (SurfacePacketDisposition, u64, SurfaceEdit)> {
        let disposition = prop_oneof![
            Just(SurfacePacketDisposition::Cancelled),
            Just(SurfacePacketDisposition::Refused),
            Just(SurfacePacketDisposition::Stale),
        ];
        let identity = prop_oneof![0u64..4, Just(u64::MAX)];
        let edit = prop_oneof![
            (0u8..4, any::<u64>()).prop_map(|(key, value)| SurfaceEdit::PutBinding(key, value)),
            (0u8..4).prop_map(SurfaceEdit::RemoveBinding),
            identity.clone().prop_map(SurfaceEdit::PutSource),
            identity.prop_map(SurfaceEdit::RemoveSource),
        ];
        (disposition, 1u64..4, edit)
    }
    proptest! {
        #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]
        #[test]
        fn observed_declarations_follow_publication_not_reservation_ceiling(
            generated in prop::collection::vec((any::<bool>(), 0u64..32), 0..12)
        ) {
            let (_directory, mut graph) = fixture();
            let mut expected = Vec::new();
            let mut next_id = 0;
            let operations = [(true, 0), (false, 30), (true, 1)]
                .into_iter().chain(generated);
            for (commit, gap) in operations {
                let before = crate::session::observe_declaration_domain(
                    graph.store().path(), RecoveryDomainId::Root,
                ).unwrap().unwrap();
                let previous = expected.clone();
                next_id += gap + 1;
                let mut candidate = graph.candidate();
                let root = context(&mut candidate);
                candidate.insert_node(node(
                    next_id, expected.last().map(|id: &Generation| id.0), root, "authored",
                )).unwrap();
                if commit {
                    graph = publish(candidate, &graph);
                    expected.push(Generation(next_id));
                } else {
                    drop(candidate);
                }
                let after = crate::session::observe_declaration_domain(
                    graph.store().path(), RecoveryDomainId::Root,
                ).unwrap().unwrap();
                prop_assert_eq!(after.allocation_ceiling, Generation(u64::MAX));
                prop_assert_eq!(before.declaration_generations().unwrap(), previous);
                prop_assert_eq!(after.declaration_generations().unwrap(), expected.clone());
            }
        }

        #[test]
        fn transferred_surface_histories_match_independent_facts(
            generated in prop::collection::vec(surface_operation(), 1..32)
        ) {
            let (_directory, mut graph) = fixture();
            let mut model = BTreeMap::<u64, SurfaceModel>::new();
            // Every case reaches lost-packet transfer after cancellation,
            // definitive rollback and stale rejection, plus actual tombstones.
            let mut operations = vec![
                (SurfacePacketDisposition::Cancelled, 1, SurfaceEdit::PutBinding(0, u64::MAX)),
                (SurfacePacketDisposition::Refused, 1, SurfaceEdit::PutSource(u64::MAX)),
                (SurfacePacketDisposition::Stale, 1, SurfaceEdit::RemoveBinding(0)),
                (SurfacePacketDisposition::Cancelled, 1, SurfaceEdit::RemoveSource(u64::MAX)),
            ];
            operations.extend(generated);
            let original_domain_root = graph.domain_root().cloned();
            for (disposition, incarnation, edit) in operations {
                let owning = owner(incarnation);
                let mut expected = model.get(&incarnation).cloned().unwrap_or_default();
                expected.epoch += 1;
                match edit {
                    SurfaceEdit::PutBinding(key, variable) => {
                        expected.bindings.insert(format!("name{key}"), RecoveryBindingId {session: u64::MAX, variable});
                    },
                    SurfaceEdit::RemoveBinding(key) => { expected.bindings.remove(&format!("name{key}")); },
                    SurfaceEdit::PutSource(identity) => { expected.sources.insert(source(identity)); },
                    SurfaceEdit::RemoveSource(identity) => { expected.sources.remove(&source(identity)); },
                }
                let mut authored = graph.surface(&owning).unwrap()
                    .map(|surface| (*surface).clone())
                    .unwrap_or_else(|| RecoveryPublicSurface::empty(owning.clone()));
                authored.epoch = expected.epoch;
                authored.bindings = expected.bindings.iter().map(|(name, binding)| (name.clone(), *binding)).collect();
                authored.source_instances = expected.sources.iter().cloned().map(|source| (source, ())).collect();
                let before = graph.store().read_domain(graph.domain_id()).unwrap().unwrap();
                let mut abandoned = graph.candidate();
                abandoned.replace_surface(authored).unwrap();
                let abandoned = abandoned.seal().unwrap();
                let transferred = (*abandoned.surface(&owning).unwrap().unwrap()).clone();
                match disposition {
                    SurfacePacketDisposition::Cancelled => drop(abandoned),
                    SurfacePacketDisposition::Refused | SurfacePacketDisposition::Stale => {
                        let staged = abandoned.candidate().prepare(graph.store_revision(), graph.high_water()).unwrap();
                        drop(abandoned);
                        if matches!(disposition, SurfacePacketDisposition::Refused) {
                            graph.store().inject_fault(StoreFault::BeforeCommit);
                        } else {
                            graph = publish(graph.candidate(), &graph);
                        }
                        let installed = std::cell::Cell::new(false);
                        let outcome = staged.publish_and_install(|_, _| installed.set(true));
                        prop_assert!(matches!(outcome, StoreDisposition::Refused(_)), "{outcome:?}");
                        if matches!(disposition, SurfacePacketDisposition::Stale) {
                            prop_assert!(matches!(outcome, StoreDisposition::Refused(crate::session::recovery_store::StoreRefusal::StaleRevision)));
                        } else {
                            prop_assert_eq!(graph.store().read_domain(graph.domain_id()).unwrap().unwrap(), before);
                        }
                        prop_assert!(!installed.get());
                        prop_assert!(!graph.store().is_recovery_required());
                    },
                }
                let sibling_roots = graph.owner_roots.clone();
                let mut receiving = graph.candidate();
                receiving.replace_surface(transferred).unwrap();
                graph = publish(receiving, &graph);
                model.insert(incarnation, expected);
                // Independent cold acquisition cannot consult the warm sibling
                // cache that retained the transferred map receipts.
                graph = reopened(&graph);
                prop_assert_eq!(graph.domain_root(), original_domain_root.as_ref());
                prop_assert_eq!(graph.high_water(), Generation(u64::MAX));
                prop_assert_eq!(graph.public_owners().count(), model.len());
                for (sibling, root) in sibling_roots.iter().filter(|(sibling, _)| *sibling != &owning) {
                    prop_assert_eq!(graph.public_owner_state(sibling), Some(root));
                }
                for (&incarnation, expected) in &model {
                    let actual = graph.surface(&owner(incarnation)).unwrap().unwrap();
                    prop_assert_eq!(actual.epoch, expected.epoch);
                    prop_assert_eq!(actual.bindings.iter().map(|(name, binding)| (name.clone(), *binding)).collect::<BTreeMap<_, _>>(), expected.bindings.clone());
                    prop_assert_eq!(actual.source_instances.keys().cloned().collect::<std::collections::BTreeSet<_>>(), expected.sources.clone());
                    prop_assert!(actual.declaration_root.is_none());
                    prop_assert!(actual.context_root.is_none());
                }
            }
        }

        #[test]
        fn persistent_graph_histories_match_independent_maps(operations in prop::collection::vec((0u8..10, 0u8..4, any::<u64>()), 1..64)) {
            let (_directory, mut graph) = fixture();
            let (_, sibling, _) = load_or_create_domain_graph(graph.store().clone(), RecoveryDomainId::Session(SessionId(99)), 99, Generation(64)).unwrap();
            let mut model = Model::default();
            for (operation, selector, variable) in operations {
                if operation == 5 { graph = reopened(&graph); }
                else {
                    let mut next = model.clone();
                    let mut candidate = graph.candidate();
                    if operation == 0 {
                        let id = next.nodes.len() as u64 + 1;
                        let parent = (id>1).then_some(id-1);
                        let root = context(&mut candidate);
                        candidate.insert_node(node(id, parent, root, "issued")).unwrap();
                        next.nodes.insert(id, parent);
                    } else {
                        let incarnation = u64::from(selector)+1;
                        let owning = owner(incarnation);
                        let mut surface = graph.surface(&owning).unwrap().map(|surface| (*surface).clone()).unwrap_or_else(|| RecoveryPublicSurface::empty(owning));
                        let entry = next.owners.entry(incarnation).or_default();
                        entry.0 += 1;
                        surface.epoch = entry.0;
                        let name = format!("name{}", variable%4);
                        if operation == 2 {
                            entry.1.remove(&name); surface.bindings.remove(&name);
                        } else {
                            let binding = RecoveryBindingId {session:u64::MAX,variable};
                            entry.1.insert(name.clone(), binding); surface.bindings.insert(name,binding);
                        }
                        candidate.replace_surface(surface).unwrap();
                    }
                    let staged = candidate.prepare(graph.store_revision(), graph.high_water()).unwrap();
                    if operation != 6 {
                        let fault = match operation {7=>Some(StoreFault::BeforeCommit),8=>Some(StoreFault::CommitError),9=>Some(StoreFault::RollbackError),_=>None};
                        if let Some(fault)=fault { graph.store().inject_fault(fault); }
                        let installed = std::cell::Cell::new(false);
                        let outcome = staged.publish_and_install(|graph,_| {installed.set(true);graph});
                        if matches!(operation,7|9) {} else {next.revision+=1;model=next;}
                        match outcome {
                            StoreDisposition::Committed(committed)=>{prop_assert!(installed.get());graph=committed;},
                            StoreDisposition::Refused(_)=>{prop_assert_eq!(operation,7);prop_assert!(!installed.get());},
                            StoreDisposition::RecoveryRequired(_)=>{
                                prop_assert!(matches!(operation,8|9));prop_assert!(!installed.get());
                                let old=graph.store().clone();graph=reopened(&graph);prop_assert!(old.is_recovery_required());
                            }
                        }
                    }
                }
                prop_assert_eq!(graph.store_revision(),StoreRevision(model.revision));
                prop_assert_eq!(graph.content_revision(),StoreRevision(model.revision));
                prop_assert_eq!(graph.high_water(),Generation(u64::MAX));
                prop_assert_eq!(graph.node_ids().map(|id|id.0).collect::<Vec<_>>(),model.nodes.keys().copied().collect::<Vec<_>>());
                for (&id,&parent) in &model.nodes {prop_assert_eq!(graph.node(Generation(id)).unwrap().unwrap().parent.map(|id|id.0),parent);}
                prop_assert_eq!(graph.public_owners().count(),model.owners.len());
                for (&incarnation,(epoch,bindings)) in &model.owners {
                    let surface=graph.surface(&owner(incarnation)).unwrap().unwrap();
                    prop_assert_eq!(surface.epoch,*epoch);
                    prop_assert_eq!(surface.bindings.iter().map(|(name,binding)|(name.clone(),*binding)).collect::<BTreeMap<_,_>>(),bindings.clone());
                }
                prop_assert_eq!(graph.store().read_domain(RecoveryDomainId::Session(SessionId(99))).unwrap(),Some(sibling.clone()));
            }
        }
    }
}
