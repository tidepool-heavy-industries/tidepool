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
use tidepool_toolchain::exact_context::{
    hydrate_context, hydrate_map, read_value, MaterializedContext, MaterializedMap, Materializer,
    SnapshotMapRoot, StagedBlocks,
};

const VERSION: u32 = 1;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DomainSnapshot {
    version: u32,
    source_session: u64,
    nodes: SnapshotMapRoot,
    artifacts: SnapshotMapRoot,
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
        if artifact.artifact_id() != id
            || artifact.declared_artifact_id() != id
            || artifact
                .component_paths()
                .iter()
                .any(|(_, path)| !relative(path))
        {
            return Err(error("artifact block identity or custody path is invalid"));
        }
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
        if let Some(context) = self.acquired.lock().contexts.get(root).cloned() {
            return Ok(context);
        }
        let context = Arc::new(hydrate_context(self, root)?);
        self.acquired
            .lock()
            .contexts
            .insert(root.clone(), context.clone());
        Ok(context)
    }
    pub(crate) fn remember_context(
        &self,
        context: Arc<MaterializedContext>,
    ) -> Result<(), RecoveryError> {
        self.store.ensure_ready()?;
        self.acquired
            .lock()
            .contexts
            .insert(context.root().clone(), context);
        Ok(())
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
    pub(crate) fn insert_context(
        &mut self,
        context: &tidepool_toolchain::declaration_join::ExactDeclarationContext,
        previous: Option<&ContextRoot>,
    ) -> Result<ContextRoot, RecoveryError> {
        let base = previous.map(|root| self.graph.context(root)).transpose()?;
        let mut materializer = Materializer::new(&self.graph);
        let context =
            Arc::new(materializer.materialize_context(base.as_deref(), &context.snapshot())?);
        let packet = materializer.finish()?;
        self.graph.stage_packet(packet);
        let root = context.root().clone();
        self.graph
            .materialized_contexts
            .insert(root.clone(), context);
        Ok(root)
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
        let previous = self.graph.owner_roots.get(&surface.owner).cloned();
        let mut materializer = Materializer::new(&self.graph);
        let bindings =
            materializer.materialize_map(surface.binding_receipt.as_ref(), &surface.bindings)?;
        let sources = materializer
            .materialize_map(surface.source_receipt.as_ref(), &surface.source_instances)?;
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
    pub(crate) fn publish_and_install<T>(
        self,
        install: impl FnOnce(RecoveryGraph, RecoveryDomainState) -> T,
    ) -> StoreDisposition<T> {
        let Self {
            mut graph,
            prepared,
            ..
        } = self;
        let store = graph.store.clone();
        store.publish_and_install(prepared, move |state| {
            graph.install_store_state(&state);
            install(graph, state)
        })
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
        candidate.insert_context(&context, None).unwrap()
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
        let before = base.store().work();
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
        assert_eq!(base.store().work(), before);
        assert!(base.node(Generation(1)).unwrap().is_none());
        assert!(base.public_owners().next().is_none());
        // A fresh issuing materializer stages its complete packet and retries.
        let root = context(&mut candidate);
        let mut missing_artifact = node(1, None, root.clone(), "missing-artifact");
        missing_artifact.artifact_refs.push(ArtifactId([17; 32]));
        assert!(candidate.insert_node(missing_artifact).is_err());
        assert_eq!(base.store().work(), before);
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
        let before = graph.store().work();
        graph = match staged.publish_and_install(|graph, _| graph) {
            StoreDisposition::Committed(graph) => graph,
            other => panic!("delta: {other:?}"),
        };
        let after = graph.store().work();
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
    proptest! {
        #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]
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
