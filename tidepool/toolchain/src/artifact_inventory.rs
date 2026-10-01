//! Scoped original-artifact ownership. Graph indices are private implementation
//! details; durable and compiler boundaries use content-bound artifact IDs.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::declaration_join::{ExactInterfaceOwner, ExactModuleIdentity};
use crate::recovery_artifacts::{CertifiedJoinedInterface, CertifiedRecoveryProduct};
use crate::CompileError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ArtifactId(pub [u8; 32]);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    OriginalModule,
    ValueInterface,
    LexicalJoin,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactDependency {
    Interface,
    NativeGroup {
        dependent_ordinal: u32,
        required_ordinal: u32,
    },
    NativeBinding {
        dependent_ordinal: u32,
        generation: u64,
        namespace: String,
        occurrence: String,
        record_parent: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactInventoryFailure {
    #[error("artifact {artifact:?} has differing metadata")]
    MetadataConflict { artifact: ArtifactId },
    #[error("original owner {owner:?} has differing artifacts")]
    OwnerConflict { owner: ExactModuleIdentity },
    #[error("{dependent:?} requires unavailable {required:?} through {dependency:?}")]
    MissingDependency {
        artifact: ArtifactId,
        dependent: ExactModuleIdentity,
        required: ExactModuleIdentity,
        dependency: ArtifactDependency,
    },
}

#[derive(Debug)]
pub struct ArtifactInventoryError {
    pub failure: ArtifactInventoryFailure,
    pub diagnostic_artifacts: Option<std::path::PathBuf>,
}

impl std::fmt::Display for ArtifactInventoryError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.failure.fmt(output)?;
        if let Some(path) = &self.diagnostic_artifacts {
            write!(
                output,
                "; compiler artifacts retained at {}",
                path.display()
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ArtifactInventoryError {}

fn admission_failure(failure: ArtifactInventoryFailure) -> CompileError {
    ArtifactInventoryError {
        failure,
        diagnostic_artifacts: None,
    }
    .into()
}

/// An exact live value required by the selected original native group closure.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct NativeBindingRequirement {
    pub artifact_id: ArtifactId,
    pub identity: tidepool_repr::execution_schema::SymbolIdentity,
    pub generation: u64,
}

/// A package export required by one original native group. Its interface
/// witness certifies the owner; activation still requires its exact live lease.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct RetainedPackageDependency {
    pub dependent_ordinal: u32,
    pub identity: tidepool_repr::execution_schema::SymbolIdentity,
    pub generation: u64,
    pub interface_digest: [u8; 32],
}

/// A selected external package obligation, rooted at its demanding original.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct NativePackageRequirement {
    pub artifact_id: ArtifactId,
    pub identity: tidepool_repr::execution_schema::SymbolIdentity,
    pub generation: u64,
    pub interface_digest: [u8; 32],
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeRequirements {
    pub bindings: Vec<NativeBindingRequirement>,
    pub packages: Vec<NativePackageRequirement>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactDescriptor {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub owner: ExactModuleIdentity,
    pub producer_sha256: [u8; 32],
    pub interface_sha256: [u8; 32],
    pub product_sha256: Option<[u8; 32]>,
    pub package_imports_sha256: [u8; 32],
    pub certification_sha256: Option<[u8; 32]>,
}

impl ArtifactDescriptor {
    /// Describes persisted evidence; constructing a descriptor does not admit it.
    pub fn from_recovery_product(
        reference: &crate::recovery_artifacts::RecoveryArtifactRef,
    ) -> Self {
        descriptor(
            ArtifactKind::OriginalModule,
            ExactModuleIdentity {
                unit: reference.unit.clone(),
                module: reference.module.clone(),
            },
            reference.toolchain_identity_sha256,
            reference.skinny_iface_sha256,
            Some(reference.product_sha256),
            reference.package_imports_sha256,
            Some(reference.certification_sha256),
        )
    }
    pub fn from_recovery_join(reference: &crate::recovery_artifacts::RecoveryJoinRef) -> Self {
        Self::from_recovery_interface(reference, ArtifactKind::LexicalJoin)
    }
    pub fn from_recovery_value_interface(
        reference: &crate::recovery_artifacts::RecoveryValueInterfaceRef,
    ) -> Self {
        Self::from_recovery_interface(&reference.interface, ArtifactKind::ValueInterface)
    }
    fn from_recovery_interface(
        reference: &crate::recovery_artifacts::RecoveryJoinRef,
        kind: ArtifactKind,
    ) -> Self {
        descriptor(
            kind,
            ExactModuleIdentity {
                unit: reference.unit.clone(),
                module: reference.module.clone(),
            },
            reference.toolchain_identity_sha256,
            reference.skinny_iface_sha256,
            None,
            reference.package_imports_sha256,
            None,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArtifactPayload {
    Original(CertifiedRecoveryProduct),
    Interface(CertifiedJoinedInterface, ArtifactKind),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ArtifactEntry {
    pub descriptor: ArtifactDescriptor,
    pub payload: ArtifactPayload,
    pub requirements: Vec<ExactModuleIdentity>,
    pub native_requirements: Vec<(ExactModuleIdentity, ArtifactDependency)>,
    pub retained_packages: Vec<RetainedPackageDependency>,
}

impl ArtifactEntry {
    pub(crate) fn original(
        producer: [u8; 32],
        product: CertifiedRecoveryProduct,
        requirements: Vec<ExactModuleIdentity>,
    ) -> Result<Self, CompileError> {
        let owner = product.owner();
        let native_requirements = crate::certified_products::certified_native_requirements(
            product.certification_bytes(),
            owner,
        )
        .map_err(|error| {
            CompileError::ExtractFailed(format!("artifact inventory native requirements: {error}"))
        })?;
        let descriptor = descriptor(
            ArtifactKind::OriginalModule,
            ExactModuleIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
            },
            producer,
            owner.skinny_iface_sha256,
            Some(owner.product_sha256),
            digest(product.package_imports_bytes()),
            Some(digest(product.certification_bytes())),
        );
        Ok(Self {
            descriptor,
            payload: ArtifactPayload::Original(product),
            requirements,
            native_requirements: native_requirements.artifact_edges,
            retained_packages: native_requirements.retained_packages,
        })
    }
    pub(crate) fn interface(
        interface: CertifiedJoinedInterface,
        kind: ArtifactKind,
        requirements: Vec<ExactModuleIdentity>,
    ) -> Self {
        let descriptor = descriptor(
            kind,
            ExactModuleIdentity {
                unit: interface.unit().into(),
                module: interface.module().into(),
            },
            interface.toolchain_identity_sha256(),
            digest(interface.interface_bytes()),
            None,
            digest(interface.package_imports_bytes()),
            None,
        );
        Self {
            descriptor,
            payload: ArtifactPayload::Interface(interface, kind),
            requirements,
            native_requirements: Vec::new(),
            retained_packages: Vec::new(),
        }
    }
}
pub(crate) fn restore_recovery_interface_dependencies(
    entries: &mut [ArtifactEntry],
    descriptors: &[ArtifactDescriptor],
    dependencies: &[(ArtifactId, ArtifactId, ArtifactDependency)],
) -> Result<(), CompileError> {
    use crate::artifact_inventory::ArtifactDependency;
    let expected = entries
        .iter()
        .map(|entry| (entry.descriptor.id, entry.descriptor.clone()))
        .collect::<BTreeMap<_, _>>();
    let supplied = descriptors
        .iter()
        .map(|descriptor| (descriptor.id, descriptor.clone()))
        .collect::<BTreeMap<_, _>>();
    if expected.len() != entries.len()
        || supplied.len() != descriptors.len()
        || supplied != expected
    {
        return Err(failure(
            "recovered artifact descriptor closure differs from certified bytes",
        ));
    }
    let edges = dependencies.iter().cloned().collect::<BTreeSet<_>>();
    if edges.len() != dependencies.len()
        || edges
            .iter()
            .any(|(from, to, _)| !expected.contains_key(from) || !expected.contains_key(to))
    {
        return Err(failure("invalid recovered dependency endpoints"));
    }
    if edges
        .iter()
        .any(|(_, _, dependency)| !matches!(dependency, ArtifactDependency::Interface))
    {
        return Err(failure(
            "native recovery dependencies must derive from original certification",
        ));
    }
    for entry in entries.iter_mut() {
        let requirements = edges
            .iter()
            .filter(|(from, _, dependency)| {
                *from == entry.descriptor.id && matches!(dependency, ArtifactDependency::Interface)
            })
            .map(|(_, to, _)| expected[to].owner.clone())
            .collect::<BTreeSet<_>>();
        if matches!(entry.descriptor.kind, ArtifactKind::ValueInterface)
            && requirements != entry.requirements.iter().cloned().collect()
        {
            return Err(failure(
                "value interface dependencies differ from retained evidence",
            ));
        }
        entry.requirements = requirements.into_iter().collect();
    }
    Ok(())
}

fn descriptor(
    kind: ArtifactKind,
    owner: ExactModuleIdentity,
    producer_sha256: [u8; 32],
    interface_sha256: [u8; 32],
    product_sha256: Option<[u8; 32]>,
    package_imports_sha256: [u8; 32],
    certification_sha256: Option<[u8; 32]>,
) -> ArtifactDescriptor {
    let mut descriptor = ArtifactDescriptor {
        id: ArtifactId([0; 32]),
        kind,
        owner,
        producer_sha256,
        interface_sha256,
        product_sha256,
        package_imports_sha256,
        certification_sha256,
    };
    descriptor.id = ArtifactId(digest(
        &serde_json::to_vec(&descriptor).expect("artifact descriptor encoding"),
    ));
    descriptor
}
fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn failure(message: &str) -> CompileError {
    CompileError::ExtractFailed(format!("artifact inventory: {message}"))
}

#[derive(Default)]
struct InventoryState {
    graph: StableDiGraph<ArtifactDescriptor, ArtifactDependency>,
    indices: BTreeMap<ArtifactId, NodeIndex>,
    payloads: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    owners: BTreeMap<ExactModuleIdentity, ArtifactId>,
    modules: BTreeMap<String, ArtifactId>,
    roots: BTreeMap<ArtifactId, usize>,
    graph_visits: AtomicU64,
    view_queries: AtomicU64,
    entry_handle_copies: AtomicU64,
    admission_owner_lookups: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactInventoryMetrics {
    pub nodes: usize,
    pub graph_visits: u64,
    pub view_queries: u64,
    pub entry_handle_copies: u64,
    pub whole_graph_copies: u64,
    pub admission_owner_lookups: u64,
}

/// A run/context family owns one inventory, without a global registry.
#[derive(Clone, Default)]
pub struct ArtifactInventory(Arc<Mutex<InventoryState>>);
impl std::fmt::Debug for ArtifactInventory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactInventory")
            .field(
                "nodes",
                &self.0.lock().expect("inventory lock").graph.node_count(),
            )
            .finish()
    }
}
impl ArtifactInventory {
    pub fn empty_view(&self) -> ArtifactView {
        self.retain(Vec::new(), Vec::new())
    }
    fn retain(&self, roots: Vec<ArtifactId>, parents: Vec<ArtifactView>) -> ArtifactView {
        let mut state = self.0.lock().expect("inventory lock");
        for id in &roots {
            *state.roots.entry(*id).or_default() += 1;
        }
        drop(state);
        ArtifactView(Arc::new(ViewLease {
            inventory: self.clone(),
            roots,
            parents,
        }))
    }
    pub(crate) fn admit(
        &self,
        parent: &ArtifactView,
        entries: Vec<ArtifactEntry>,
    ) -> Result<ArtifactView, CompileError> {
        let entries = entries
            .into_iter()
            .map(|mut entry| {
                entry.requirements.sort();
                entry.requirements.dedup();
                entry.native_requirements.sort();
                entry.native_requirements.dedup();
                entry.retained_packages.sort();
                entry.retained_packages.dedup();
                Arc::new(entry)
            })
            .collect();
        self.admit_shared(parent, entries)
    }

    /// The recovery owner supplies already normalized immutable entries.
    pub(crate) fn admit_shared(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
    ) -> Result<ArtifactView, CompileError> {
        if !Arc::ptr_eq(&self.0, &parent.0.inventory.0) {
            return Err(failure("view belongs to another inventory"));
        }
        if entries.is_empty() {
            return Ok(parent.clone());
        }
        let mut state = self.0.lock().expect("inventory lock");
        let mut additions = BTreeMap::new();
        let mut roots = BTreeSet::new();
        for entry in entries {
            let id = entry.descriptor.id;
            roots.insert(id);
            if let Some(previous) = state.payloads.get(&id).or_else(|| additions.get(&id)) {
                if previous != &entry {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::MetadataConflict { artifact: id },
                    ));
                }
            } else {
                additions.insert(id, entry);
            }
        }
        let owners = additions
            .values()
            .map(|entry| (entry.descriptor.owner.clone(), entry.descriptor.id))
            .collect::<BTreeMap<_, _>>();
        let modules = additions
            .values()
            .map(|entry| (entry.descriptor.owner.module.clone(), entry.descriptor.id))
            .collect::<BTreeMap<_, _>>();
        if owners.len() != additions.len() || modules.len() != additions.len() {
            return Err(failure(
                "one original owner or module has differing artifacts",
            ));
        }
        for entry in additions.values() {
            state
                .admission_owner_lookups
                .fetch_add(2, Ordering::Relaxed);
            if state.owners.contains_key(&entry.descriptor.owner) {
                return Err(admission_failure(ArtifactInventoryFailure::OwnerConflict {
                    owner: entry.descriptor.owner.clone(),
                }));
            }
            if state.modules.contains_key(&entry.descriptor.owner.module) {
                return Err(failure("same module occurs under multiple units"));
            }
            for (owner, dependency) in entry
                .requirements
                .iter()
                .map(|owner| (owner, ArtifactDependency::Interface))
                .chain(
                    entry
                        .native_requirements
                        .iter()
                        .map(|(owner, dependency)| (owner, dependency.clone())),
                )
            {
                state
                    .admission_owner_lookups
                    .fetch_add(1, Ordering::Relaxed);
                if !state.owners.contains_key(owner) && !owners.contains_key(owner) {
                    tracing::error!(
                        dependent = ?entry.descriptor.owner,
                        required = ?owner,
                        artifact = ?entry.descriptor.id,
                        "incomplete interface requirements"
                    );
                    return Err(admission_failure(
                        ArtifactInventoryFailure::MissingDependency {
                            artifact: entry.descriptor.id,
                            dependent: entry.descriptor.owner.clone(),
                            required: owner.clone(),
                            dependency,
                        },
                    ));
                }
            }
        }
        let roots = roots.into_iter().collect::<Vec<_>>();
        for (id, entry) in &additions {
            let index = state.graph.add_node(entry.descriptor.clone());
            state.indices.insert(*id, index);
        }
        for (id, entry) in additions {
            let source = state.indices[&id];
            for owner in &entry.requirements {
                let target = state.indices[&state
                    .owners
                    .get(owner)
                    .copied()
                    .unwrap_or_else(|| owners[owner])];
                state
                    .graph
                    .add_edge(source, target, ArtifactDependency::Interface);
            }
            for (owner, dependency) in &entry.native_requirements {
                let target = state.indices[&state
                    .owners
                    .get(owner)
                    .copied()
                    .unwrap_or_else(|| owners[owner])];
                state.graph.add_edge(source, target, dependency.clone());
            }
            state.owners.insert(entry.descriptor.owner.clone(), id);
            state
                .modules
                .insert(entry.descriptor.owner.module.clone(), id);
            state.payloads.insert(id, entry);
        }
        // Root registration occurs under the same lock as admission, so another
        // view's release cannot reclaim the newly admitted nodes.
        for id in &roots {
            *state.roots.entry(*id).or_default() += 1;
        }
        drop(state);
        Ok(ArtifactView(Arc::new(ViewLease {
            inventory: self.clone(),
            roots,
            parents: vec![parent.clone()],
        })))
    }
    pub fn metrics(&self) -> ArtifactInventoryMetrics {
        let state = self.0.lock().expect("inventory lock");
        ArtifactInventoryMetrics {
            nodes: state.graph.node_count(),
            graph_visits: state.graph_visits.load(Ordering::Relaxed),
            view_queries: state.view_queries.load(Ordering::Relaxed),
            entry_handle_copies: state.entry_handle_copies.load(Ordering::Relaxed),
            whole_graph_copies: 0,
            admission_owner_lookups: state.admission_owner_lookups.load(Ordering::Relaxed),
        }
    }
    pub fn node_count(&self) -> usize {
        self.0.lock().expect("inventory lock").graph.node_count()
    }
}

struct ViewLease {
    inventory: ArtifactInventory,
    roots: Vec<ArtifactId>,
    parents: Vec<ArtifactView>,
}
impl Drop for ViewLease {
    fn drop(&mut self) {
        let mut state = self.inventory.0.lock().expect("inventory lock");
        let mut lost_roots = Vec::new();
        for id in &self.roots {
            let count = state.roots.get_mut(id).expect("retained root");
            *count -= 1;
            if *count == 0 {
                state.roots.remove(id);
                lost_roots.push(*id);
            }
        }
        if lost_roots.is_empty() {
            return;
        }
        // Only the lost roots' reachable closure can become unowned. Nodes
        // outside it remain retained, so their incoming edges seed survivors.
        // This also handles cycles without scanning unrelated graph history.
        let candidates = closure(&state, lost_roots.into_iter());
        let survivors = candidates
            .iter()
            .copied()
            .filter(|id| {
                state.roots.contains_key(id)
                    || state
                        .graph
                        .edges_directed(state.indices[id], Direction::Incoming)
                        .any(|edge| !candidates.contains(&state.graph[edge.source()].id))
            })
            .collect::<Vec<_>>();
        let retained = closure(&state, survivors.into_iter());
        for id in candidates.difference(&retained) {
            let index = state.indices.remove(id).expect("indexed artifact");
            state.graph.remove_node(index);
            let entry = state.payloads.remove(id).expect("owned payload");
            state.owners.remove(&entry.descriptor.owner);
            state.modules.remove(&entry.descriptor.owner.module);
        }
        // Parent drops after the lock guard, preserving recursive release.
    }
}
fn closure(
    state: &InventoryState,
    roots: impl Iterator<Item = ArtifactId>,
) -> BTreeSet<ArtifactId> {
    let mut pending = roots.collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if seen.insert(id) {
            state.graph_visits.fetch_add(1, Ordering::Relaxed);
            if let Some(index) = state.indices.get(&id) {
                pending.extend(
                    state
                        .graph
                        .edges(*index)
                        .map(|edge| state.graph[edge.target()].id),
                );
            }
        }
    }
    seen
}
/// Cloning a view retains its roots without copying the graph or artifact bytes.
#[derive(Clone)]
pub struct ArtifactView(Arc<ViewLease>);

/// One retained closure observed under the inventory lock. Entries are ordered
/// by exact owner; dependency tuples keep their canonical wire order.
pub(crate) struct ArtifactMetadataSnapshot {
    pub entries: BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>,
}

impl ArtifactMetadataSnapshot {
    pub fn dependencies(&self) -> Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
        let mut dependencies = Vec::new();
        for entry in self.entries.values() {
            for (owner, dependency) in entry
                .requirements
                .iter()
                .map(|owner| (owner, ArtifactDependency::Interface))
                .chain(
                    entry
                        .native_requirements
                        .iter()
                        .map(|(owner, dependency)| (owner, dependency.clone())),
                )
            {
                dependencies.push((
                    entry.descriptor.id,
                    self.entries[owner].descriptor.id,
                    dependency,
                ));
            }
        }
        dependencies.sort();
        dependencies
    }
}
impl std::fmt::Debug for ArtifactView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactView")
            .field("roots", &self.0.roots)
            .finish()
    }
}
impl ArtifactView {
    pub(crate) fn metadata_snapshot(&self) -> ArtifactMetadataSnapshot {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        let ids = closure(&state, self.roots().into_iter());
        let mut entries = BTreeMap::new();
        for id in ids {
            let entry = &state.payloads[&id];
            entries.insert(entry.descriptor.owner.clone(), Arc::clone(entry));
        }
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        ArtifactMetadataSnapshot { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.roots().is_empty()
    }
    pub fn inventory(&self) -> &ArtifactInventory {
        &self.0.inventory
    }
    pub fn artifact_ids(&self) -> Vec<ArtifactId> {
        self.entries().iter().map(|e| e.descriptor.id).collect()
    }
    pub fn descriptors(&self) -> Vec<ArtifactDescriptor> {
        self.entries()
            .iter()
            .map(|e| e.descriptor.clone())
            .collect()
    }
    pub fn dependencies(&self) -> Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        let ids = closure(&state, self.roots().into_iter());
        let mut edges = Vec::new();
        for id in &ids {
            for edge in state.graph.edges(state.indices[id]) {
                edges.push((*id, state.graph[edge.target()].id, edge.weight().clone()));
            }
        }
        edges.sort();
        edges
    }

    /// Durable recovery retains direct interface requirements. Native rows
    /// stay in the inventory and derive again from authenticated Home seals.
    pub fn interface_dependencies(&self) -> Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        let ids = closure(&state, self.roots().into_iter());
        let mut edges = Vec::new();
        for id in ids {
            for owner in &state.payloads[&id].requirements {
                edges.push((id, state.owners[owner], ArtifactDependency::Interface));
            }
        }
        edges.sort();
        edges
    }
    /// Select native implementation dependencies independently of interface
    /// visibility. Each original root initially selects all its native groups;
    /// native group edges then select only their exact required ordinals.
    pub fn native_binding_requirements_from_roots(
        &self,
        roots: &[ArtifactId],
    ) -> Result<Vec<NativeBindingRequirement>, CompileError> {
        Ok(self.native_requirements_from_roots(roots)?.bindings)
    }

    pub fn native_requirements_from_roots(
        &self,
        roots: &[ArtifactId],
    ) -> Result<NativeRequirements, CompileError> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        let owned = closure(&state, self.roots().into_iter());
        if roots.iter().any(|id| !owned.contains(id)) {
            return Err(failure("native root is outside retained view"));
        }
        let mut pending = roots.iter().map(|id| (*id, None)).collect::<Vec<_>>();
        let mut seen = BTreeSet::new();
        let mut requirements = BTreeSet::new();
        let mut packages = BTreeSet::new();
        while let Some((id, ordinal)) = pending.pop() {
            if !seen.insert((id, ordinal)) {
                continue;
            }
            state.graph_visits.fetch_add(1, Ordering::Relaxed);
            for package in &state.payloads[&id].retained_packages {
                if ordinal.is_none_or(|selected| selected == package.dependent_ordinal) {
                    packages.insert(NativePackageRequirement {
                        artifact_id: id,
                        identity: package.identity.clone(),
                        generation: package.generation,
                        interface_digest: package.interface_digest,
                    });
                }
            }
            for edge in state.graph.edges(state.indices[&id]) {
                let target = &state.graph[edge.target()];
                match edge.weight() {
                    ArtifactDependency::NativeGroup {
                        dependent_ordinal,
                        required_ordinal,
                    } if ordinal.is_none_or(|selected| selected == *dependent_ordinal) => {
                        pending.push((target.id, Some(*required_ordinal)));
                    }
                    ArtifactDependency::NativeBinding {
                        dependent_ordinal,
                        generation,
                        namespace,
                        occurrence,
                        record_parent,
                    } if ordinal.is_none_or(|selected| selected == *dependent_ordinal) => {
                        requirements.insert(NativeBindingRequirement {
                            artifact_id: target.id,
                            identity: tidepool_repr::execution_schema::SymbolIdentity {
                                unit: target.owner.unit.clone(),
                                module: target.owner.module.clone(),
                                namespace: namespace.clone(),
                                occurrence: occurrence.clone(),
                                record_parent: record_parent.clone(),
                            },
                            generation: *generation,
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(NativeRequirements {
            bindings: requirements.into_iter().collect(),
            packages: packages.into_iter().collect(),
        })
    }

    fn roots(&self) -> Vec<ArtifactId> {
        let mut pending = vec![self];
        let mut roots = BTreeSet::new();
        let mut seen = BTreeSet::new();
        while let Some(view) = pending.pop() {
            if seen.insert(Arc::as_ptr(&view.0)) {
                roots.extend(view.0.roots.iter().copied());
                pending.extend(view.0.parents.iter());
            }
        }
        roots.into_iter().collect()
    }
    /// Retain exactly these reachable artifact roots, independently of the
    /// source view's lifetime. Hidden dependencies remain graph-owned.
    pub fn select_roots(&self, roots: Vec<ArtifactId>) -> Result<Self, CompileError> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        let owned = closure(&state, self.roots().into_iter());
        if roots.iter().any(|id| !owned.contains(id)) {
            return Err(failure("selected artifact is outside retained view"));
        }
        drop(state);
        Ok(self.0.inventory.retain(roots, Vec::new()))
    }
    pub(crate) fn merge(&self, other: &Self) -> Result<Self, CompileError> {
        if self.is_empty() {
            return Ok(other.clone());
        }
        if Arc::ptr_eq(&self.0.inventory.0, &other.0.inventory.0) {
            Ok(ArtifactView(Arc::new(ViewLease {
                inventory: self.0.inventory.clone(),
                roots: Vec::new(),
                parents: vec![self.clone(), other.clone()],
            })))
        } else {
            self.0.inventory.admit(
                self,
                other
                    .entries()
                    .iter()
                    .map(|entry| entry.as_ref().clone())
                    .collect(),
            )
        }
    }
    pub(crate) fn entries(&self) -> Vec<Arc<ArtifactEntry>> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        let mut entries = closure(&state, self.roots().into_iter())
            .iter()
            .filter_map(|id| state.payloads.get(id).cloned())
            .collect::<Vec<_>>();
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        entries.sort_by(|a, b| a.descriptor.owner.cmp(&b.descriptor.owner));
        entries
    }
    /// Explicitly retained roots, excluding their hidden dependency closure.
    pub(crate) fn root_entries(&self) -> Vec<Arc<ArtifactEntry>> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        let entries = self
            .roots()
            .iter()
            .filter_map(|id| state.payloads.get(id).cloned())
            .collect::<Vec<_>>();
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        entries
    }
    pub(crate) fn entries_for_owners(
        &self,
        owners: impl Iterator<Item = ExactModuleIdentity>,
    ) -> BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        let owned = closure(&state, self.roots().into_iter());
        let entries = owners
            .filter_map(|owner| {
                let id = state.owners.get(&owner)?;
                owned
                    .contains(id)
                    .then(|| (owner, Arc::clone(&state.payloads[id])))
            })
            .collect::<BTreeMap<_, _>>();
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        entries
    }

    pub(crate) fn interface_owners(&self) -> Vec<ExactInterfaceOwner> {
        self.entries()
            .iter()
            .map(|entry| ExactInterfaceOwner {
                owner: entry.descriptor.owner.clone(),
                requirements: entry.requirements.clone(),
            })
            .collect()
    }
}
impl PartialEq for ArtifactView {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.entries() == other.entries()
    }
}
impl Eq for ArtifactView {}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::{CachedHomeOwner, ModuleVersion};
    fn module(name: &str) -> ExactModuleIdentity {
        ExactModuleIdentity {
            unit: "unit".into(),
            module: name.into(),
        }
    }
    fn entry(name: &str, requirements: &[&str]) -> ArtifactEntry {
        let owner = CachedHomeOwner {
            unit: "unit".into(),
            module: name.into(),
            module_version: ModuleVersion([1; 32]),
            skinny_iface_sha256: digest(name.as_bytes()),
            product_sha256: digest(name.as_bytes()),
        };
        let certification =
            crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
                .unwrap();
        let product = CertifiedRecoveryProduct::from_certification(
            owner,
            name.as_bytes().to_vec(),
            name.as_bytes().to_vec(),
            vec![],
            certification,
        );
        ArtifactEntry::original(
            [2; 32],
            product,
            requirements.iter().map(|name| module(name)).collect(),
        )
        .unwrap()
    }
    #[test]
    fn retained_views_reclaim_after_last_reader_without_copying_payloads() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let first = inventory
            .admit(&empty, vec![entry("Original", &[])])
            .unwrap();
        let capture = first.clone();
        let second = inventory
            .admit(&first, vec![entry("Consumer", &["Original"])])
            .unwrap();
        assert_eq!(second.dependencies().len(), 1);
        let original = first.entries()[0].clone();
        let inherited = second
            .entries()
            .into_iter()
            .find(|entry| entry.descriptor.owner == module("Original"))
            .unwrap();
        assert!(Arc::ptr_eq(&original, &inherited));
        drop(second);
        assert_eq!(inventory.node_count(), 1);
        drop(first);
        assert_eq!(inventory.node_count(), 1);
        drop(capture);
        assert_eq!(inventory.node_count(), 0);
        // The temporary inspection owner may retain bytes, never graph authority.
        assert_eq!(original.descriptor.owner, module("Original"));
    }
    #[test]
    fn empty_admission_and_parent_only_release_do_not_scan_history() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let view = inventory
            .admit(&empty, vec![entry("Original", &[])])
            .unwrap();
        let before = inventory.metrics();
        let unchanged = inventory.admit(&view, Vec::new()).unwrap();
        assert!(Arc::ptr_eq(&view.0, &unchanged.0));
        let merged = view.merge(&unchanged).unwrap();
        drop(merged);
        drop(unchanged);
        let after = inventory.metrics();
        assert_eq!(before.graph_visits, after.graph_visits);
        assert_eq!(
            before.admission_owner_lookups,
            after.admission_owner_lookups
        );
        let second = inventory
            .admit(&view, vec![entry("Consumer", &["Original"])])
            .unwrap();
        assert_eq!(
            inventory.metrics().admission_owner_lookups - after.admission_owner_lookups,
            3
        );
        drop(second);
        drop(view);
        assert_eq!(inventory.node_count(), 0);
        // Reclamation removes the metadata indexes with the graph entries.
        let replacement = inventory
            .admit(&empty, vec![entry("Original", &[])])
            .unwrap();
        assert_eq!(replacement.descriptors().len(), 1);
    }

    #[test]
    fn metadata_snapshot_preserves_canonical_typed_edges_in_one_closure() {
        let inventory = ArtifactInventory::default();
        let mut consumer = entry("Consumer", &["Original"]);
        consumer.native_requirements.push((
            module("Original"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 4,
                required_ordinal: 7,
            },
        ));
        consumer.native_requirements.push((
            module("Original"),
            ArtifactDependency::NativeBinding {
                dependent_ordinal: 4,
                generation: 9,
                namespace: "value".into(),
                occurrence: "kept".into(),
                record_parent: None,
            },
        ));
        let view = inventory
            .admit(
                &inventory.empty_view(),
                vec![consumer, entry("Original", &[])],
            )
            .unwrap();
        let expected_descriptors = view.descriptors();
        let expected_dependencies = view.dependencies();
        let before = inventory.metrics();
        let metadata = view.metadata_snapshot();
        assert_eq!(
            metadata
                .entries
                .values()
                .map(|entry| entry.descriptor.clone())
                .collect::<Vec<_>>(),
            expected_descriptors,
        );
        assert_eq!(metadata.dependencies(), expected_dependencies);
        let after = inventory.metrics();
        assert_eq!(after.graph_visits - before.graph_visits, 2);
        assert_eq!(after.view_queries - before.view_queries, 1);
        assert_eq!(after.entry_handle_copies - before.entry_handle_copies, 2);
    }

    #[test]
    fn reclamation_preserves_incoming_cycles_and_does_not_visit_unrelated_history() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let cycle = inventory
            .admit(&empty, vec![entry("A", &["B"]), entry("B", &["A"])])
            .unwrap();
        let incoming = inventory
            .admit(&empty, vec![entry("Outside", &["A"])])
            .unwrap();
        let unrelated = inventory
            .admit(&empty, vec![entry("Unrelated", &[])])
            .unwrap();
        let before = inventory.metrics().graph_visits;
        drop(cycle);
        assert_eq!(inventory.node_count(), 4);
        assert_eq!(inventory.metrics().graph_visits - before, 4);
        drop(incoming);
        assert_eq!(inventory.node_count(), 1);
        assert_eq!(unrelated.descriptors()[0].owner.module, "Unrelated");
        drop(unrelated);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn native_binding_selection_ignores_type_edges_and_unselected_group_ordinals() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let value = entry("Val", &[]);
        let type_user = entry("TypeOnly", &["Val"]);
        let mut helper = entry("Helper", &["Val"]);
        helper.native_requirements.push((
            module("Val"),
            ArtifactDependency::NativeBinding {
                dependent_ordinal: 1,
                generation: 7,
                namespace: "value".into(),
                occurrence: "x".into(),
                record_parent: None,
            },
        ));
        helper.native_requirements.push((
            module("Val"),
            ArtifactDependency::NativeBinding {
                dependent_ordinal: 2,
                generation: 8,
                namespace: "value".into(),
                occurrence: "y".into(),
                record_parent: None,
            },
        ));
        let mut consumer = entry("Consumer", &["TypeOnly", "Helper"]);
        consumer.native_requirements.push((
            module("Helper"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 0,
                required_ordinal: 1,
            },
        ));
        let type_id = type_user.descriptor.id;
        let helper_id = helper.descriptor.id;
        let consumer_id = consumer.descriptor.id;
        let view = inventory
            .admit(&empty, vec![value, type_user, helper, consumer])
            .unwrap();
        assert!(view
            .native_binding_requirements_from_roots(&[type_id])
            .unwrap()
            .is_empty());
        let selected = view
            .native_binding_requirements_from_roots(&[consumer_id])
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].identity.module, "Val");
        assert_eq!(selected[0].identity.occurrence, "x");
        assert_eq!(selected[0].generation, 7);
        assert_eq!(
            view.native_binding_requirements_from_roots(&[helper_id])
                .unwrap()
                .len(),
            2
        );
        assert!(view
            .native_binding_requirements_from_roots(&[ArtifactId([99; 32])])
            .is_err());
    }

    #[test]
    fn conflicting_owner_and_missing_dependency_admit_nothing() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let first = inventory
            .admit(&empty, vec![entry("Original", &[])])
            .unwrap();
        let mut changed = entry("Original", &[]);
        changed.descriptor.id = ArtifactId([9; 32]);
        assert!(inventory
            .admit(&first, vec![entry("Unrelated", &[]), changed])
            .is_err());
        assert_eq!(inventory.node_count(), 1);
        assert!(inventory
            .admit(&first, vec![entry("Missing", &["Absent"])])
            .is_err());
        assert_eq!(inventory.node_count(), 1);
    }
    #[test]
    fn independent_views_retain_existing_nodes_without_sibling_visibility() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let first = inventory.admit(&empty, vec![entry("First", &[])]).unwrap();
        let second = inventory.admit(&empty, vec![entry("Second", &[])]).unwrap();
        assert_eq!(
            second
                .descriptors()
                .iter()
                .map(|descriptor| descriptor.owner.module.as_str())
                .collect::<Vec<_>>(),
            vec!["Second"]
        );
        let imported = inventory.admit(&second, vec![entry("First", &[])]).unwrap();
        drop(first);
        assert_eq!(imported.descriptors().len(), 2);
        drop(imported);
        assert_eq!(inventory.node_count(), 1);
        drop(second);
        assert_eq!(inventory.node_count(), 0);
    }
    #[test]
    fn selected_root_keeps_hidden_requirements_and_reclaims_unselected_history() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let view = inventory
            .admit(
                &empty,
                vec![
                    entry("Hidden", &[]),
                    entry("Visible", &["Hidden"]),
                    entry("Obsolete", &[]),
                ],
            )
            .unwrap();
        let visible = view
            .descriptors()
            .into_iter()
            .find(|descriptor| descriptor.owner == module("Visible"))
            .unwrap()
            .id;
        let selected = view.select_roots(vec![visible]).unwrap();
        assert!(selected.select_roots(vec![ArtifactId([99; 32])]).is_err());
        drop(view);
        assert_eq!(inventory.node_count(), 2);
        assert_eq!(selected.descriptors().len(), 2);
        let metrics = inventory.metrics();
        assert_eq!(metrics.whole_graph_copies, 0);
        assert!(metrics.graph_visits > 0);
        assert!(metrics.entry_handle_copies > 0);
        drop(selected);
        assert_eq!(inventory.node_count(), 0);
    }
    #[test]
    fn native_requirements_remain_typed_and_do_not_grant_a_value_implementation() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let mut consumer = entry("Consumer", &["Original"]);
        consumer.native_requirements.push((
            module("Original"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 2,
                required_ordinal: 7,
            },
        ));
        let view = inventory
            .admit(&empty, vec![entry("Original", &[]), consumer])
            .unwrap();
        let dependencies = view.dependencies();
        assert_eq!(dependencies.len(), 2);
        assert!(dependencies
            .iter()
            .any(|(_, _, edge)| matches!(edge, ArtifactDependency::Interface)));
        assert!(dependencies.iter().any(|(_, _, edge)| matches!(
            edge,
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 2,
                required_ordinal: 7
            }
        )));
        assert_eq!(
            view.interface_dependencies(),
            dependencies
                .into_iter()
                .filter(|(_, _, dependency)| matches!(dependency, ArtifactDependency::Interface))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn recovery_restores_interface_edges_and_keeps_certified_native_facts() {
        let original = entry("Original", &[]);
        let mut consumer = entry("Consumer", &[]);
        consumer.native_requirements.push((
            module("Original"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 2,
                required_ordinal: 7,
            },
        ));
        let descriptors = vec![original.descriptor.clone(), consumer.descriptor.clone()];
        let edges = vec![(
            consumer.descriptor.id,
            original.descriptor.id,
            ArtifactDependency::Interface,
        )];
        let mut entries = vec![original.clone(), consumer.clone()];
        restore_recovery_interface_dependencies(&mut entries, &descriptors, &edges).unwrap();
        assert_eq!(entries[1].requirements, vec![module("Original")]);
        assert_eq!(entries[1].native_requirements, consumer.native_requirements);
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit(&inventory.empty_view(), entries.clone())
            .unwrap();
        assert!(view.dependencies().iter().any(|(_, _, edge)| matches!(
            edge,
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 2,
                required_ordinal: 7
            }
        )));
        let mut supplied_native = edges.clone();
        supplied_native.push((
            consumer.descriptor.id,
            original.descriptor.id,
            consumer.native_requirements[0].1.clone(),
        ));
        assert!(restore_recovery_interface_dependencies(
            &mut entries,
            &descriptors,
            &supplied_native
        )
        .is_err());
        let mut forged = descriptors.clone();
        forged[0].producer_sha256 = [99; 32];
        assert!(restore_recovery_interface_dependencies(&mut entries, &forged, &edges).is_err());
        let mut duplicated = edges.clone();
        duplicated.push(edges[0].clone());
        assert!(
            restore_recovery_interface_dependencies(&mut entries, &descriptors, &duplicated)
                .is_err()
        );
        let mut wrong_endpoint = edges;
        wrong_endpoint[0].1 = ArtifactId([99; 32]);
        assert!(restore_recovery_interface_dependencies(
            &mut entries,
            &descriptors,
            &wrong_endpoint
        )
        .is_err());
    }

    #[test]
    fn dependency_and_artifact_ids_are_stable_across_inventory_allocation() {
        let a = ArtifactInventory::default();
        let b = ArtifactInventory::default();
        let av = a
            .admit(
                &a.empty_view(),
                vec![entry("Original", &[]), entry("Consumer", &["Original"])],
            )
            .unwrap();
        let bv = b
            .admit(
                &b.empty_view(),
                vec![entry("Consumer", &["Original"]), entry("Original", &[])],
            )
            .unwrap();
        assert_eq!(av.artifact_ids(), bv.artifact_ids());
        assert_eq!(av.dependencies(), bv.dependencies());
        let json = serde_json::to_string(&av.descriptors()).unwrap();
        assert!(!json.contains("NodeIndex"));
    }
    #[test]
    fn native_package_obligations_follow_selected_groups_without_package_artifact_nodes() {
        let inventory = ArtifactInventory::default();
        let mut root = entry("Root", &[]);
        root.native_requirements.push((
            module("Helper"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 2,
                required_ordinal: 7,
            },
        ));
        let mut helper = entry("Helper", &[]);
        let package = |ordinal, occurrence: &str| RetainedPackageDependency {
            dependent_ordinal: ordinal,
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "ghc-internal".into(),
                module: "GHC.Internal.Base".into(),
                namespace: "value".into(),
                occurrence: occurrence.into(),
                record_parent: None,
            },
            generation: 0,
            interface_digest: [4; 32],
        };
        helper.retained_packages = vec![package(7, "map"), package(8, "foldr")];
        let root_id = root.descriptor.id;
        let helper_id = helper.descriptor.id;
        let view = inventory
            .admit(&inventory.empty_view(), vec![root, helper])
            .unwrap();
        let requirements = view.native_requirements_from_roots(&[root_id]).unwrap();
        assert!(requirements.bindings.is_empty());
        assert_eq!(requirements.packages.len(), 1);
        assert_eq!(requirements.packages[0].artifact_id, helper_id);
        assert_eq!(requirements.packages[0].identity.occurrence, "map");
        assert_eq!(
            view.native_requirements_from_roots(&[helper_id])
                .unwrap()
                .packages
                .len(),
            2
        );
        assert_eq!(inventory.metrics().nodes, 2);
    }
}
