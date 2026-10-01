//! Scoped original-artifact ownership. Graph indices are private implementation
//! details; durable and compiler boundaries use content-bound artifact IDs.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use petgraph::visit::EdgeRef;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactDependency {
    Interface,
    NativeGroup { original_ordinal: u32 },
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
}

impl ArtifactEntry {
    pub(crate) fn original(
        producer: [u8; 32],
        product: CertifiedRecoveryProduct,
        requirements: Vec<ExactModuleIdentity>,
    ) -> Self {
        let owner = product.owner();
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
        Self {
            descriptor,
            payload: ArtifactPayload::Original(product),
            requirements,
        }
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
        }
    }
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
    roots: BTreeMap<ArtifactId, usize>,
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
        self.retain(Vec::new(), None)
    }
    fn retain(&self, roots: Vec<ArtifactId>, parent: Option<ArtifactView>) -> ArtifactView {
        let mut state = self.0.lock().expect("inventory lock");
        for id in &roots {
            *state.roots.entry(*id).or_default() += 1;
        }
        drop(state);
        ArtifactView(Arc::new(ViewLease {
            inventory: self.clone(),
            roots,
            parent,
        }))
    }
    pub(crate) fn admit(
        &self,
        parent: &ArtifactView,
        entries: Vec<ArtifactEntry>,
    ) -> Result<ArtifactView, CompileError> {
        if !Arc::ptr_eq(&self.0, &parent.0.inventory.0) {
            return Err(failure("view belongs to another inventory"));
        }
        let mut state = self.0.lock().expect("inventory lock");
        let mut owners: BTreeMap<_, _> = state
            .payloads
            .values()
            .map(|e| (e.descriptor.owner.clone(), e.descriptor.id))
            .collect();
        let mut additions = BTreeMap::new();
        let mut roots = BTreeSet::new();
        for mut entry in entries {
            entry.requirements.sort();
            entry.requirements.dedup();
            let id = entry.descriptor.id;
            roots.insert(id);
            if let Some(previous) = state
                .payloads
                .get(&id)
                .map(Arc::as_ref)
                .or_else(|| additions.get(&id))
            {
                if previous != &entry {
                    return Err(failure("one artifact has differing metadata"));
                }
            } else {
                additions.insert(id, entry);
            }
        }
        for entry in additions.values() {
            if owners
                .insert(entry.descriptor.owner.clone(), entry.descriptor.id)
                .is_some_and(|id| id != entry.descriptor.id)
            {
                return Err(failure("one original owner has differing artifacts"));
            }
        }
        if owners
            .keys()
            .map(|owner| &owner.module)
            .collect::<BTreeSet<_>>()
            .len()
            != owners.len()
        {
            return Err(failure("same module occurs under multiple units"));
        }
        for entry in additions.values() {
            if entry
                .requirements
                .iter()
                .any(|owner| !owners.contains_key(owner))
            {
                return Err(failure("incomplete interface requirements"));
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
                let target = state.indices[&owners[owner]];
                state
                    .graph
                    .add_edge(source, target, ArtifactDependency::Interface);
            }
            state.payloads.insert(id, Arc::new(entry));
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
            parent: Some(parent.clone()),
        })))
    }
    pub fn node_count(&self) -> usize {
        self.0.lock().expect("inventory lock").graph.node_count()
    }
}

struct ViewLease {
    inventory: ArtifactInventory,
    roots: Vec<ArtifactId>,
    parent: Option<ArtifactView>,
}
impl Drop for ViewLease {
    fn drop(&mut self) {
        let mut state = self.inventory.0.lock().expect("inventory lock");
        for id in &self.roots {
            let count = state.roots.get_mut(id).expect("retained root");
            *count -= 1;
            if *count == 0 {
                state.roots.remove(id);
            }
        }
        let retained = closure(&state, state.roots.keys().copied());
        let removed = state
            .indices
            .keys()
            .filter(|id| !retained.contains(id))
            .copied()
            .collect::<Vec<_>>();
        for id in removed {
            let index = state.indices.remove(&id).expect("indexed artifact");
            state.graph.remove_node(index);
            state.payloads.remove(&id);
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
impl std::fmt::Debug for ArtifactView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactView")
            .field("roots", &self.0.roots)
            .finish()
    }
}
impl ArtifactView {
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
                edges.push((*id, state.graph[edge.target()].id, *edge.weight()));
            }
        }
        edges.sort();
        edges
    }
    fn roots(&self) -> Vec<ArtifactId> {
        let mut roots = self.0.roots.clone();
        let mut parent = self.0.parent.as_ref();
        while let Some(view) = parent {
            roots.extend_from_slice(&view.0.roots);
            parent = view.0.parent.as_ref();
        }
        roots
    }
    pub(crate) fn entries(&self) -> Vec<Arc<ArtifactEntry>> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        let mut entries = closure(&state, self.roots().into_iter())
            .iter()
            .filter_map(|id| state.payloads.get(id).cloned())
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.descriptor.owner.cmp(&b.descriptor.owner));
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
        let product = CertifiedRecoveryProduct::from_certification(
            CachedHomeOwner {
                unit: "unit".into(),
                module: name.into(),
                module_version: ModuleVersion([1; 32]),
                skinny_iface_sha256: digest(name.as_bytes()),
                product_sha256: digest(name.as_bytes()),
            },
            name.as_bytes().to_vec(),
            name.as_bytes().to_vec(),
            vec![],
            vec![],
        );
        ArtifactEntry::original(
            [2; 32],
            product,
            requirements.iter().map(|name| module(name)).collect(),
        )
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
}
