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

#[cfg(test)]
mod properties;

/// Exact artifacts persist SHA-256 of the compiler's stable producer bytes.
/// Endpoint identities and their raw producer bytes are not this identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CanonicalProducerIdentity([u8; 32]);

impl CanonicalProducerIdentity {
    pub(crate) fn from_producer_bytes(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub(crate) fn from_compiler(identity: &tidepool_extract_cmd::CompilerIdentity) -> Self {
        Self::from_producer_bytes(identity.producer_bytes())
    }

    pub(crate) fn sha256(self) -> [u8; 32] {
        self.0
    }

    pub(crate) fn hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[cfg(test)]
    pub(crate) fn from_test_sha256(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ArtifactId(pub [u8; 32]);

/// Native demand starts at a complete module or one compiler-issued group.
/// Selecting demand does not change the retained artifact custody.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum NativeRequirementRoot {
    AllGroups(ArtifactId),
    /// The ordinal is issued by the original module certificate, independently
    /// of executable entry indices or the position of a cell item.
    Group {
        artifact: ArtifactId,
        original_ordinal: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    OriginalModule,
    CanonicalModuleInterface,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactInventoryFailure {
    #[error("artifact {artifact:?} has differing metadata")]
    MetadataConflict { artifact: ArtifactId },
    #[error("original owner {owner:?} has differing artifacts")]
    OwnerConflict { owner: ExactModuleIdentity },
    #[error("owner {owner:?} selects multiple native implementations")]
    NativeOwnerAmbiguity { owner: ExactModuleIdentity },
    #[error("authored generation {generation} requires one certified native root; found {found}")]
    AuthoredNativeRoot { generation: u64, found: usize },
    #[error("native root {artifact:?} is outside its retained view")]
    NativeRootOutsideView { artifact: ArtifactId },
    #[error("artifact {artifact:?} has no certified native group {original_ordinal}")]
    NativeGroupUnavailable {
        artifact: ArtifactId,
        original_ordinal: u32,
    },
    #[error("{dependent:?} requires another exact interface seal for {required:?}")]
    InterfaceSealMismatch {
        dependent: ExactModuleIdentity,
        required: ExactModuleIdentity,
    },
    #[error("{dependent:?} requires unavailable {required:?} through {dependency:?}")]
    MissingDependency {
        artifact: ArtifactId,
        dependent: ExactModuleIdentity,
        required: ExactModuleIdentity,
        dependency: ArtifactDependency,
    },
}

pub struct ArtifactInventoryError {
    pub failure: ArtifactInventoryFailure,
    pub diagnostic_artifacts: Option<std::path::PathBuf>,
    pub(crate) owner_conflict: Option<ArtifactOwnerConflictEvidence>,
}

pub(crate) struct ArtifactOwnerConflictEvidence {
    existing: Arc<ArtifactEntry>,
    incoming: Arc<ArtifactEntry>,
    existing_in_parent: bool,
}

impl std::fmt::Debug for ArtifactInventoryError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output
            .debug_struct("ArtifactInventoryError")
            .field("failure", &self.failure)
            .field("diagnostic_artifacts", &self.diagnostic_artifacts)
            .finish()
    }
}

impl ArtifactInventoryError {
    /// Only immutable evidence from the refused admission is retained. Reading
    /// these diagnostics cannot admit either artifact or change its scope.
    pub(crate) fn retain_owner_conflict(&self, root: &std::path::Path) -> std::io::Result<()> {
        let Some(evidence) = &self.owner_conflict else {
            return Ok(());
        };
        let root = root.join("owner-conflict");
        std::fs::create_dir(&root)?;
        let metadata = serde_json::json!({
            "existing": evidence.existing.descriptor,
            "incoming": evidence.incoming.descriptor,
            "existing_in_parent": evidence.existing_in_parent,
        });
        std::fs::write(
            root.join("index.json"),
            serde_json::to_vec_pretty(&metadata).map_err(std::io::Error::other)?,
        )?;
        for (label, entry) in [
            ("existing", &evidence.existing),
            ("incoming", &evidence.incoming),
        ] {
            if let ArtifactPayload::Canonical(interface) = &entry.payload {
                for (extension, bytes) in [
                    ("finalized.cbor", interface.certificate_bytes()),
                    ("hi", interface.interface_bytes()),
                    ("packages", interface.package_imports_bytes()),
                ] {
                    std::fs::write(root.join(format!("{label}.{extension}")), bytes)?;
                }
                if let Some(core) = interface.core_bytes() {
                    std::fs::write(root.join(format!("{label}.core")), core)?;
                }
            }
        }
        Ok(())
    }
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

pub(crate) fn admission_failure(failure: ArtifactInventoryFailure) -> CompileError {
    ArtifactInventoryError {
        failure,
        diagnostic_artifacts: None,
        owner_conflict: None,
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
    pub fn from_recovery_module_interface(
        reference: &crate::recovery_artifacts::RecoveryModuleInterfaceRef,
    ) -> Self {
        let interface = &reference.interface;
        descriptor(
            ArtifactKind::CanonicalModuleInterface,
            ExactModuleIdentity {
                unit: interface.unit.clone(),
                module: interface.module.clone(),
            },
            interface.toolchain_identity_sha256,
            interface.skinny_iface_sha256,
            None,
            interface.package_imports_sha256,
            Some(reference.certificate_sha256),
        )
    }
    pub fn from_recovery_join(reference: &crate::recovery_artifacts::RecoveryJoinRef) -> Self {
        Self::from_recovery_interface(reference, JoinedInterfaceRole::LexicalJoin)
    }
    pub fn from_recovery_value_interface(
        reference: &crate::recovery_artifacts::RecoveryValueInterfaceRef,
    ) -> Self {
        Self::from_recovery_interface(&reference.interface, JoinedInterfaceRole::ValueInterface)
    }
    fn from_recovery_interface(
        reference: &crate::recovery_artifacts::RecoveryJoinRef,
        role: JoinedInterfaceRole,
    ) -> Self {
        descriptor(
            role.artifact_kind(),
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

/// The two roles carried by a certified joined interface. Native products and
/// canonical module interfaces have their own payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JoinedInterfaceRole {
    LexicalJoin,
    ValueInterface,
}

impl JoinedInterfaceRole {
    fn artifact_kind(self) -> ArtifactKind {
        match self {
            Self::LexicalJoin => ArtifactKind::LexicalJoin,
            Self::ValueInterface => ArtifactKind::ValueInterface,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ArtifactPayload {
    Original(CertifiedRecoveryProduct),
    Canonical(crate::certified_products::CertifiedModuleInterface),
    Interface(CertifiedJoinedInterface, JoinedInterfaceRole),
}

impl PartialEq for ArtifactPayload {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Original(left), Self::Original(right)) => left.same_durable_artifact(right),
            (Self::Canonical(left), Self::Canonical(right)) => left == right,
            (Self::Interface(left, left_role), Self::Interface(right, right_role)) => {
                left_role == right_role && left == right
            }
            _ => false,
        }
    }
}
impl Eq for ArtifactPayload {}

impl ArtifactPayload {
    pub(crate) fn artifact_kind(&self) -> ArtifactKind {
        match self {
            Self::Original(_) => ArtifactKind::OriginalModule,
            Self::Canonical(_) => ArtifactKind::CanonicalModuleInterface,
            Self::Interface(_, role) => role.artifact_kind(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ArtifactEntry {
    pub descriptor: ArtifactDescriptor,
    pub payload: ArtifactPayload,
    pub requirements: Vec<ExactModuleIdentity>,
    interface_seals: BTreeMap<ExactModuleIdentity, [u8; 32]>,
    pub native_requirements: Vec<(ExactModuleIdentity, ArtifactDependency)>,
    native_owners: BTreeMap<ExactModuleIdentity, NativeOwnerKey>,
    native_group_ordinals: BTreeSet<u32>,
    pub retained_packages: Vec<RetainedPackageDependency>,
}

impl ArtifactEntry {
    #[cfg(test)]
    pub(crate) fn original(
        producer: [u8; 32],
        product: CertifiedRecoveryProduct,
    ) -> Result<Self, CompileError> {
        Self::original_with_validation(
            producer,
            product,
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
    }

    pub(crate) fn original_with_validation(
        producer: [u8; 32],
        product: CertifiedRecoveryProduct,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Self, CompileError> {
        let owner = product.owner();
        let canonical = product
            .module_interface()
            .ok_or_else(|| failure("native product lacks its canonical finalized module"))?;
        if canonical.producer_sha256() != producer {
            return Err(failure("native product has another canonical producer"));
        }
        let native_owners = crate::certified_products::original_home_requirements_with_validation(
            &product, validation,
        )
        .map_err(|error| failure(&format!("native source owners: {error}")))?
        .into_iter()
        .map(|owner| {
            (
                ExactModuleIdentity {
                    unit: owner.unit.clone(),
                    module: owner.module.clone(),
                },
                NativeOwnerKey::from_owner(&owner),
            )
        })
        .collect();
        let interface_seals =
            crate::certified_products::original_interface_requirements_with_operation(
                &product,
                &validation.inventory,
            )
            .map_err(|error| failure(&format!("original interface requirements: {error}")))?
            .into_iter()
            .map(|((unit, module), seal)| (ExactModuleIdentity { unit, module }, seal))
            .collect::<BTreeMap<_, _>>();
        // The canonical compiler interface owns type closure. Source lexical
        // adjacency cannot add or remove its sealed interface requirements.
        let requirements = interface_seals.keys().cloned().collect();
        let native_requirements =
            crate::certified_products::original_native_requirements_with_operation(
                &product,
                &validation.inventory,
            )
            .map_err(|error| {
                CompileError::ExtractFailed(format!(
                    "artifact inventory native requirements: {error}"
                ))
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
            interface_seals,
            native_requirements: native_requirements.artifact_edges,
            native_owners,
            native_group_ordinals: native_requirements.group_ordinals,
            retained_packages: native_requirements.retained_packages,
        })
    }
    pub(crate) fn canonical(
        interface: crate::certified_products::CertifiedModuleInterface,
    ) -> Self {
        let interface_seals = interface
            .requirements()
            .iter()
            .map(|((unit, module), seal)| {
                (
                    ExactModuleIdentity {
                        unit: unit.clone(),
                        module: module.clone(),
                    },
                    *seal,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let descriptor = descriptor(
            ArtifactKind::CanonicalModuleInterface,
            ExactModuleIdentity {
                unit: interface.unit().into(),
                module: interface.module().into(),
            },
            interface.producer_sha256(),
            interface.interface_sha256(),
            None,
            interface.package_imports_sha256(),
            Some(digest(interface.certificate_bytes())),
        );
        Self {
            descriptor,
            payload: ArtifactPayload::Canonical(interface),
            requirements: interface_seals.keys().cloned().collect(),
            interface_seals,
            native_requirements: Vec::new(),
            native_owners: BTreeMap::new(),
            native_group_ordinals: BTreeSet::new(),
            retained_packages: Vec::new(),
        }
    }

    fn is_native(&self) -> bool {
        matches!(self.payload, ArtifactPayload::Original(_))
    }

    pub(crate) fn interface(
        interface: CertifiedJoinedInterface,
        role: JoinedInterfaceRole,
        mut requirements: Vec<ExactModuleIdentity>,
    ) -> Self {
        requirements.sort();
        requirements.dedup();
        let descriptor = descriptor(
            role.artifact_kind(),
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
            payload: ArtifactPayload::Interface(interface, role),
            requirements,
            interface_seals: BTreeMap::new(),
            native_requirements: Vec::new(),
            native_owners: BTreeMap::new(),
            native_group_ordinals: BTreeSet::new(),
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
        if let ArtifactPayload::Original(product) = &entry.payload {
            let canonical = ArtifactEntry::canonical(
                product
                    .module_interface()
                    .ok_or_else(|| failure("recovered native lacks canonical carrier"))?
                    .clone(),
            )
            .descriptor
            .id;
            let direct = edges
                .iter()
                .filter(|(from, _, _)| *from == entry.descriptor.id)
                .cloned()
                .collect::<Vec<_>>();
            if direct
                != vec![(
                    entry.descriptor.id,
                    canonical,
                    ArtifactDependency::Interface,
                )]
            {
                return Err(failure(
                    "native interface edge differs from canonical carrier",
                ));
            }
            continue;
        }
        let requirements = edges
            .iter()
            .filter(|(from, to, dependency)| {
                *from == entry.descriptor.id
                    && matches!(dependency, ArtifactDependency::Interface)
                    && expected[to].owner != entry.descriptor.owner
            })
            .map(|(_, to, _)| expected[to].owner.clone())
            .collect::<BTreeSet<_>>();
        if matches!(
            entry.descriptor.kind,
            ArtifactKind::ValueInterface | ArtifactKind::CanonicalModuleInterface
        ) && requirements != entry.requirements.iter().cloned().collect()
        {
            return Err(failure(
                "value interface dependencies differ from retained evidence",
            ));
        }
        if !entry
            .interface_seals
            .keys()
            .all(|owner| requirements.contains(owner))
        {
            return Err(failure(
                "recovered original omits certified interface requirements",
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

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct NativeOwnerKey {
    owner: ExactModuleIdentity,
    version: [u8; 32],
    interface: [u8; 32],
    product: [u8; 32],
}
impl NativeOwnerKey {
    fn from_owner(owner: &tidepool_repr::execution_schema::CachedHomeOwner) -> Self {
        Self {
            owner: ExactModuleIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
            },
            version: owner.module_version.0,
            interface: owner.skinny_iface_sha256,
            product: owner.product_sha256,
        }
    }
}

#[derive(Default)]
struct InventoryState {
    graph: StableDiGraph<ArtifactDescriptor, ArtifactDependency>,
    indices: BTreeMap<ArtifactId, NodeIndex>,
    payloads: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    roots: BTreeMap<ArtifactId, usize>,
    graph_visits: AtomicU64,
    view_queries: AtomicU64,
    entry_handle_copies: AtomicU64,
    admission_owner_lookups: AtomicU64,
    reclamation_runs: u64,
    reclamation_candidate_nodes: u64,
    reclaimed_nodes: u64,
    reclamation_elapsed_ns: u64,
}

/// Owner selection belongs to one sealed reachable closure, not the inventory.
#[derive(Default)]
struct SelectedOwners {
    interfaces: BTreeMap<ExactModuleIdentity, ArtifactId>,
    native: BTreeMap<NativeOwnerKey, ArtifactId>,
}
impl SelectedOwners {
    fn new(
        state: &InventoryState,
        entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
        parent_ids: &BTreeSet<ArtifactId>,
    ) -> Result<Self, CompileError> {
        let mut selected = Self::default();
        for (id, entry) in entries {
            state
                .admission_owner_lookups
                .fetch_add(1, Ordering::Relaxed);
            let previous = if let ArtifactPayload::Original(product) = &entry.payload {
                selected
                    .native
                    .insert(NativeOwnerKey::from_owner(product.owner()), *id)
            } else {
                selected
                    .interfaces
                    .insert(entry.descriptor.owner.clone(), *id)
            };
            if let Some(previous) = previous.filter(|previous| previous != id) {
                let (existing, incoming) =
                    if parent_ids.contains(id) && !parent_ids.contains(&previous) {
                        (entry, &entries[&previous])
                    } else {
                        (&entries[&previous], entry)
                    };
                return Err(ArtifactInventoryError {
                    failure: ArtifactInventoryFailure::OwnerConflict {
                        owner: entry.descriptor.owner.clone(),
                    },
                    diagnostic_artifacts: None,
                    owner_conflict: Some(ArtifactOwnerConflictEvidence {
                        existing: Arc::clone(existing),
                        incoming: Arc::clone(incoming),
                        existing_in_parent: parent_ids.contains(&existing.descriptor.id),
                    }),
                }
                .into());
            }
        }
        Ok(selected)
    }
    fn planned_edges(
        &self,
        state: &InventoryState,
        entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    ) -> Result<BTreeMap<ArtifactId, BTreeSet<(ArtifactId, ArtifactDependency)>>, CompileError>
    {
        let mut additions = BTreeMap::new();
        for (id, entry) in entries {
            let expected = self.dependencies(state, entry, entries)?;
            if let Some(index) = state.indices.get(id) {
                let actual = state
                    .graph
                    .edges(*index)
                    .map(|edge| (state.graph[edge.target()].id, edge.weight().clone()))
                    .collect::<BTreeSet<_>>();
                if actual != expected {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::MetadataConflict { artifact: *id },
                    ));
                }
            } else {
                additions.insert(*id, expected);
            }
        }
        Ok(additions)
    }
    fn dependencies(
        &self,
        state: &InventoryState,
        entry: &ArtifactEntry,
        entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    ) -> Result<BTreeSet<(ArtifactId, ArtifactDependency)>, CompileError> {
        let canonical = match &entry.payload {
            ArtifactPayload::Canonical(interface) => Some(interface),
            ArtifactPayload::Original(product) => Some(
                product
                    .module_interface()
                    .ok_or_else(|| failure("native canonical carrier missing"))?,
            ),
            ArtifactPayload::Interface(_, _) => None,
        };
        if let Some(canonical) = canonical {
            let sealed = canonical
                .requirements()
                .iter()
                .map(|((unit, module), seal)| {
                    (
                        ExactModuleIdentity {
                            unit: unit.clone(),
                            module: module.clone(),
                        },
                        *seal,
                    )
                })
                .collect::<BTreeMap<_, _>>();
            if entry.interface_seals != sealed
                || entry.requirements.iter().collect::<BTreeSet<_>>() != sealed.keys().collect()
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::MetadataConflict {
                        artifact: entry.descriptor.id,
                    },
                ));
            }
        }
        let resolve = |owner: &ExactModuleIdentity, dependency: &ArtifactDependency| {
            state
                .admission_owner_lookups
                .fetch_add(1, Ordering::Relaxed);
            let id = if matches!(dependency, ArtifactDependency::NativeGroup { .. }) {
                entry
                    .native_owners
                    .get(owner)
                    .and_then(|key| self.native.get(key))
            } else {
                self.interfaces.get(owner)
            };
            id.copied().ok_or_else(|| {
                admission_failure(ArtifactInventoryFailure::MissingDependency {
                    artifact: entry.descriptor.id,
                    dependent: entry.descriptor.owner.clone(),
                    required: owner.clone(),
                    dependency: dependency.clone(),
                })
            })
        };
        for (owner, seal) in &entry.interface_seals {
            let id = resolve(owner, &ArtifactDependency::Interface)?;
            let required = &entries[&id].descriptor;
            if required.interface_sha256 != *seal
                || required.producer_sha256 != entry.descriptor.producer_sha256
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::InterfaceSealMismatch {
                        dependent: entry.descriptor.owner.clone(),
                        required: owner.clone(),
                    },
                ));
            }
        }
        let mut edges = BTreeSet::new();
        for owner in &entry.requirements {
            let id = resolve(owner, &ArtifactDependency::Interface)?;
            if !entry.is_native() {
                edges.insert((id, ArtifactDependency::Interface));
            }
        }
        for (owner, dependency) in &entry.native_requirements {
            edges.insert((resolve(owner, dependency)?, dependency.clone()));
        }
        if let ArtifactPayload::Original(product) = &entry.payload {
            let carrier = ArtifactEntry::canonical(
                product
                    .module_interface()
                    .ok_or_else(|| failure("native canonical carrier missing"))?
                    .clone(),
            );
            let id = resolve(&entry.descriptor.owner, &ArtifactDependency::Interface)?;
            if id != carrier.descriptor.id {
                return Err(admission_failure(ArtifactInventoryFailure::OwnerConflict {
                    owner: entry.descriptor.owner.clone(),
                }));
            }
            edges.insert((id, ArtifactDependency::Interface));
        }
        Ok(edges)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactInventoryMetrics {
    pub nodes: usize,
    pub graph_visits: u64,
    pub view_queries: u64,
    pub entry_handle_copies: u64,
    /// Structural invariant of this shared owner, not an activity measurement.
    #[serde(rename = "structural_whole_graph_copies")]
    pub whole_graph_copies: u64,
    pub reclamation_runs: u64,
    pub reclamation_candidate_nodes: u64,
    pub reclaimed_nodes: u64,
    /// Existing reclamation traversal/removal work; excludes parent destruction.
    pub reclamation_elapsed_ns: u64,
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
        self.retain(Vec::new(), Vec::new(), Vec::new())
    }
    fn retain(
        &self,
        roots: Vec<ArtifactId>,
        parents: Vec<ArtifactView>,
        materialization_parents: Vec<
            Arc<crate::declaration_context::RetainedArtifactMaterialization>,
        >,
    ) -> ArtifactView {
        let mut state = self.0.lock().expect("inventory lock");
        for id in &roots {
            *state.roots.entry(*id).or_default() += 1;
        }
        drop(state);
        ArtifactView(Arc::new(ViewLease {
            inventory: self.clone(),
            roots,
            parents,
            materialization_parents,
            materialization: Mutex::new(None),
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
        // Every native implementation retains its own authenticated canonical
        // carrier, including when that implementation already exists by ID.
        let mut expanded = entries;
        let mut implicit = Vec::new();
        for entry in &expanded {
            if let ArtifactPayload::Original(product) = &entry.payload {
                implicit.push(Arc::new(ArtifactEntry::canonical(
                    product
                        .module_interface()
                        .ok_or_else(|| failure("native canonical carrier missing"))?
                        .clone(),
                )));
            }
        }
        expanded.extend(implicit);
        let mut state = self.0.lock().expect("inventory lock");
        let parent_ids = closure(&state, parent.roots().into_iter());
        let mut supplied = BTreeMap::new();
        for entry in expanded {
            let id = entry.descriptor.id;
            let retained =
                if let Some(previous) = state.payloads.get(&id).or_else(|| supplied.get(&id)) {
                    if previous != &entry {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::MetadataConflict { artifact: id },
                        ));
                    }
                    // Implicit carriers must reuse an already admitted or supplied
                    // entry rather than replace its immutable shared allocation.
                    Arc::clone(previous)
                } else {
                    entry
                };
            supplied.insert(id, retained);
        }
        let roots = supplied.keys().copied().collect::<Vec<_>>();
        // Reused immutable nodes bring their actual graph targets, never the
        // inventory's unrelated owner variants, into the selected closure.
        let mut selected_ids = parent_ids.clone();
        selected_ids.extend(closure(
            &state,
            supplied
                .keys()
                .filter(|id| state.payloads.contains_key(*id))
                .copied(),
        ));
        let mut selected = selected_ids
            .iter()
            .map(|id| (*id, Arc::clone(&state.payloads[id])))
            .collect::<BTreeMap<_, _>>();
        state
            .entry_handle_copies
            .fetch_add(selected.len() as u64, Ordering::Relaxed);
        selected.extend(supplied);
        let owners = SelectedOwners::new(&state, &selected, &parent_ids)?;
        let edges = owners.planned_edges(&state, &selected)?;
        // All owner, seal and graph checks finish before any inventory mutation.
        for id in edges.keys() {
            let index = state.graph.add_node(selected[id].descriptor.clone());
            state.indices.insert(*id, index);
        }
        for (id, dependencies) in edges {
            let source = state.indices[&id];
            for (target, dependency) in dependencies {
                let target = state.indices[&target];
                state.graph.add_edge(source, target, dependency);
            }
            state.payloads.insert(id, Arc::clone(&selected[&id]));
            state.entry_handle_copies.fetch_add(1, Ordering::Relaxed);
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
            materialization_parents: Vec::new(),
            materialization: Mutex::new(None),
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
            reclamation_runs: state.reclamation_runs,
            reclamation_candidate_nodes: state.reclamation_candidate_nodes,
            reclaimed_nodes: state.reclaimed_nodes,
            reclamation_elapsed_ns: state.reclamation_elapsed_ns,
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
    // Projected selection must retain issued files without importing its
    // source view's selection roots or executable authority.
    materialization_parents: Vec<Arc<crate::declaration_context::RetainedArtifactMaterialization>>,
    materialization:
        Mutex<Option<Arc<crate::declaration_context::RetainedArtifactMaterialization>>>,
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
        let started = std::time::Instant::now();
        state.reclamation_runs += 1;
        let candidates = closure(&state, lost_roots.into_iter());
        state.reclamation_candidate_nodes += candidates.len() as u64;
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
            state.reclaimed_nodes += 1;
            state.payloads.remove(id).expect("owned payload");
        }
        state.reclamation_elapsed_ns += started.elapsed().as_nanos() as u64;
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
    pub artifacts: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    pub ambiguous_native_owners: BTreeSet<ExactModuleIdentity>,
    dependencies: Vec<(ArtifactId, ArtifactId, ArtifactDependency)>,
}

impl ArtifactMetadataSnapshot {
    pub(crate) fn validate_native_selection(&self) -> Result<(), CompileError> {
        if let Some(owner) = self.ambiguous_native_owners.first() {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeOwnerAmbiguity {
                    owner: owner.clone(),
                },
            ));
        }
        Ok(())
    }
    /// The semantic wire order is exact owner followed by immutable artifact ID.
    /// The payload registry itself remains keyed by content ID.
    pub(crate) fn descriptors(&self) -> Vec<&ArtifactDescriptor> {
        let mut descriptors = self
            .artifacts
            .values()
            .map(|entry| &entry.descriptor)
            .collect::<Vec<_>>();
        descriptors.sort_by_key(|descriptor| (&descriptor.owner, descriptor.id));
        descriptors
    }
    pub fn dependencies(&self) -> Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
        self.dependencies.clone()
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
    /// Private materialization belongs to this immutable graph view. Failed
    /// preparation leaves no retained entry; descendants borrow completed owners.
    pub(crate) fn retain_materialization(
        &self,
        prepare: impl FnOnce(
            Vec<Arc<crate::declaration_context::RetainedArtifactMaterialization>>,
        ) -> Result<
            crate::declaration_context::RetainedArtifactMaterialization,
            CompileError,
        >,
    ) -> Result<Arc<crate::declaration_context::RetainedArtifactMaterialization>, CompileError>
    {
        let mut retained = self.0.materialization.lock().expect("materialization lock");
        if let Some(materialization) = retained.as_ref() {
            return Ok(Arc::clone(materialization));
        }
        let mut parents = self.0.materialization_parents.clone();
        let mut visited = BTreeSet::new();
        for parent in &self.0.parents {
            parent.collect_materializations(&mut parents, &mut visited);
        }
        let materialization = Arc::new(prepare(parents)?);
        *retained = Some(Arc::clone(&materialization));
        Ok(materialization)
    }

    fn collect_materializations(
        &self,
        materializations: &mut Vec<
            Arc<crate::declaration_context::RetainedArtifactMaterialization>,
        >,
        visited: &mut BTreeSet<usize>,
    ) {
        let mut pending = vec![self];
        while let Some(view) = pending.pop() {
            if !visited.insert(Arc::as_ptr(&view.0) as usize) {
                continue;
            }
            if let Some(materialization) = view.retained_materialization() {
                if !materializations
                    .iter()
                    .any(|existing| Arc::ptr_eq(existing, &materialization))
                {
                    materializations.push(materialization);
                }
            } else {
                for materialization in &view.0.materialization_parents {
                    if !materializations
                        .iter()
                        .any(|existing| Arc::ptr_eq(existing, materialization))
                    {
                        materializations.push(Arc::clone(materialization));
                    }
                }
                pending.extend(view.0.parents.iter().rev());
            }
        }
    }

    pub(crate) fn retained_materialization(
        &self,
    ) -> Option<Arc<crate::declaration_context::RetainedArtifactMaterialization>> {
        self.0
            .materialization
            .lock()
            .expect("materialization lock")
            .clone()
    }

    pub(crate) fn metadata_snapshot(&self) -> ArtifactMetadataSnapshot {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        let ids = closure(&state, self.roots().into_iter());
        let mut entries = BTreeMap::new();
        let mut artifacts = BTreeMap::new();
        let mut dependencies = Vec::new();
        let mut natives = BTreeMap::<ExactModuleIdentity, Vec<ArtifactId>>::new();
        for id in ids {
            let entry = &state.payloads[&id];
            let owner = entry.descriptor.owner.clone();
            if entry.is_native() {
                natives.entry(owner).or_default().push(id);
            } else {
                entries.insert(owner, id);
            }
            artifacts.insert(id, Arc::clone(entry));
            dependencies.extend(
                state
                    .graph
                    .edges(state.indices[&id])
                    .map(|edge| (id, state.graph[edge.target()].id, edge.weight().clone())),
            );
        }
        let mut ambiguous_native_owners = BTreeSet::new();
        for (owner, ids) in natives {
            if ids.len() == 1 {
                entries.insert(owner, ids[0]);
            } else {
                ambiguous_native_owners.insert(owner);
            }
        }
        dependencies.sort();
        let entries = entries
            .into_iter()
            .map(|(owner, id)| (owner, Arc::clone(&artifacts[&id])))
            .collect::<BTreeMap<_, _>>();
        state
            .entry_handle_copies
            .fetch_add((entries.len() + artifacts.len()) as u64, Ordering::Relaxed);
        ArtifactMetadataSnapshot {
            entries,
            artifacts,
            dependencies,
            ambiguous_native_owners,
        }
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

    /// Canonical interfaces authorize types alongside their implementations;
    /// they cannot replace or grant a selected source implementation role.
    pub(crate) fn source_implementation_roles(
        &self,
    ) -> BTreeMap<ExactModuleIdentity, ArtifactKind> {
        self.descriptors()
            .into_iter()
            .filter(|descriptor| descriptor.kind != ArtifactKind::CanonicalModuleInterface)
            .map(|descriptor| (descriptor.owner, descriptor.kind))
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
            edges.extend(
                state
                    .graph
                    .edges(state.indices[&id])
                    .filter(|edge| matches!(edge.weight(), ArtifactDependency::Interface))
                    .map(|edge| {
                        (
                            id,
                            state.graph[edge.target()].id,
                            ArtifactDependency::Interface,
                        )
                    }),
            );
        }
        edges.sort();
        edges
    }
    /// Select native implementation dependencies independently of interface
    /// visibility. Roots explicitly select all groups or one issued ordinal;
    /// native group edges then select only their exact required ordinals.
    pub fn native_binding_requirements_from_roots(
        &self,
        roots: &[NativeRequirementRoot],
    ) -> Result<Vec<NativeBindingRequirement>, CompileError> {
        Ok(self.native_requirements_from_roots(roots)?.bindings)
    }

    pub fn native_requirements_from_roots(
        &self,
        roots: &[NativeRequirementRoot],
    ) -> Result<NativeRequirements, CompileError> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        let owned = closure(&state, self.roots().into_iter());
        let mut pending = Vec::with_capacity(roots.len());
        for root in roots {
            let (id, ordinal) = match *root {
                NativeRequirementRoot::AllGroups(id) => (id, None),
                NativeRequirementRoot::Group {
                    artifact,
                    original_ordinal,
                } => (artifact, Some(original_ordinal)),
            };
            if !owned.contains(&id) {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeRootOutsideView { artifact: id },
                ));
            }
            if let Some(original_ordinal) = ordinal {
                if !state.payloads[&id]
                    .native_group_ordinals
                    .contains(&original_ordinal)
                {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::NativeGroupUnavailable {
                            artifact: id,
                            original_ordinal,
                        },
                    ));
                }
            }
            pending.push((id, ordinal));
        }
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
        let mut materializations = Vec::new();
        if !roots.is_empty() {
            self.collect_materializations(&mut materializations, &mut BTreeSet::new());
        }
        Ok(self.0.inventory.retain(roots, Vec::new(), materializations))
    }
    pub(crate) fn merge(&self, other: &Self) -> Result<Self, CompileError> {
        if Arc::ptr_eq(&self.0, &other.0) || other.is_empty() {
            return Ok(self.clone());
        }
        if self.is_empty() {
            return Ok(other.clone());
        }
        if Arc::ptr_eq(&self.0.inventory.0, &other.0.inventory.0) {
            let state = self.0.inventory.0.lock().expect("inventory lock");
            let parent_ids = closure(&state, self.roots().into_iter());
            let mut ids = parent_ids.clone();
            ids.extend(closure(&state, other.roots().into_iter()));
            let entries = ids
                .iter()
                .map(|id| (*id, Arc::clone(&state.payloads[id])))
                .collect();
            state
                .entry_handle_copies
                .fetch_add(ids.len() as u64, Ordering::Relaxed);
            SelectedOwners::new(&state, &entries, &parent_ids)?.planned_edges(&state, &entries)?;
            drop(state);
            Ok(ArtifactView(Arc::new(ViewLease {
                inventory: self.0.inventory.clone(),
                roots: Vec::new(),
                parents: vec![self.clone(), other.clone()],
                materialization_parents: Vec::new(),
                materialization: Mutex::new(None),
            })))
        } else {
            let mut roots = self.roots();
            roots.extend(other.roots());
            let merged = self.0.inventory.admit(
                self,
                other
                    .entries()
                    .iter()
                    .map(|entry| entry.as_ref().clone())
                    .collect(),
            )?;
            merged.select_roots(roots)
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
        entries.sort_by(|left, right| {
            (&left.descriptor.owner, left.descriptor.id)
                .cmp(&(&right.descriptor.owner, right.descriptor.id))
        });
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
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        let owned = closure(&state, self.roots().into_iter());
        let mut interfaces = BTreeMap::new();
        let mut native = BTreeMap::<ExactModuleIdentity, Vec<ArtifactId>>::new();
        for id in owned {
            let entry = &state.payloads[&id];
            if entry.is_native() {
                native
                    .entry(entry.descriptor.owner.clone())
                    .or_default()
                    .push(id);
            } else {
                interfaces.insert(entry.descriptor.owner.clone(), id);
            }
        }
        let mut entries = BTreeMap::new();
        for owner in owners {
            let id = match native.get(&owner).map(Vec::as_slice) {
                Some([id]) => Some(*id),
                Some(_) => {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::NativeOwnerAmbiguity { owner },
                    ))
                }
                None => interfaces.get(&owner).copied(),
            };
            if let Some(id) = id {
                entries.insert(owner, Arc::clone(&state.payloads[&id]));
            }
        }
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        Ok(entries)
    }

    pub fn interface_projection(
        &self,
        owners: &[ExactModuleIdentity],
    ) -> Result<Self, CompileError> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        let owned = closure(&state, self.roots().into_iter());
        let selected = owned
            .iter()
            .filter_map(|id| {
                let entry = &state.payloads[id];
                (!entry.is_native()).then_some((entry.descriptor.owner.clone(), *id))
            })
            .collect::<BTreeMap<_, _>>();
        let roots = owners
            .iter()
            .map(|owner| {
                selected
                    .get(owner)
                    .copied()
                    .ok_or_else(|| failure("type owner outside retained view"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        drop(state);
        self.select_roots(roots)
    }

    pub(crate) fn interface_owners(&self) -> Vec<ExactInterfaceOwner> {
        self.metadata_snapshot()
            .entries
            .values()
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

    #[test]
    fn canonical_producer_identity_preserves_exact_artifact_digest() {
        let raw = [3; 32];
        let identity = CanonicalProducerIdentity::from_producer_bytes(&raw);
        let expected: [u8; 32] = Sha256::digest(raw).into();
        assert_eq!(identity.sha256(), expected);
        assert_ne!(identity.sha256(), raw);
        assert_eq!(identity.hex(), format!("{:x}", Sha256::digest(raw)));
    }
    use tidepool_repr::execution_schema::{CachedHomeOwner, ModuleVersion};
    fn module(name: &str) -> ExactModuleIdentity {
        ExactModuleIdentity {
            unit: "unit".into(),
            module: name.into(),
        }
    }
    fn entry(name: &str, requirements: &[&str]) -> ArtifactEntry {
        entry_in_unit(
            "unit",
            name,
            requirements.iter().map(|name| module(name)).collect(),
        )
    }
    fn entry_in_unit(
        unit: &str,
        name: &str,
        requirements: Vec<ExactModuleIdentity>,
    ) -> ArtifactEntry {
        let requirements = requirements
            .into_iter()
            .map(|owner| {
                (
                    (owner.unit, owner.module.clone()),
                    digest(owner.module.as_bytes()),
                )
            })
            .collect();
        ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [2; 32],
            unit,
            name,
            requirements,
        ))
    }

    fn native_entry(name: &str, requirements: &[&str]) -> ArtifactEntry {
        let interface = crate::certified_products::fixture_module_interface(
            [2; 32],
            "unit",
            name,
            requirements
                .iter()
                .map(|required| {
                    (
                        ("unit".into(), (*required).into()),
                        digest(required.as_bytes()),
                    )
                })
                .collect(),
        );
        let owner = CachedHomeOwner {
            unit: "unit".into(),
            module: name.into(),
            module_version: ModuleVersion([1; 32]),
            skinny_iface_sha256: interface.interface_sha256(),
            product_sha256: digest(name.as_bytes()),
        };
        let certification = crate::certified_products::encode_home_certification_with_module(
            &owner,
            &[],
            &BTreeMap::new(),
            interface.requirements(),
            digest(interface.certificate_bytes()),
        )
        .unwrap();
        let product = CertifiedRecoveryProduct::from_certification(
            owner,
            name.as_bytes().to_vec(),
            name.as_bytes().to_vec(),
            interface.package_imports_bytes().to_vec(),
            certification,
        )
        .with_module_interface(interface)
        .unwrap();
        let mut entry = ArtifactEntry::original([2; 32], product).unwrap();
        for required in ["Original", "Helper"] {
            entry.native_owners.insert(
                module(required),
                NativeOwnerKey {
                    owner: module(required),
                    version: [1; 32],
                    interface: digest(required.as_bytes()),
                    product: digest(required.as_bytes()),
                },
            );
        }
        entry
    }

    #[test]
    fn native_inventory_reuse_preserves_each_owners_source_admission_witness() {
        let cold = native_entry("Original", &[]);
        let mut fresh = cold.clone();
        let ArtifactPayload::Original(product) = &cold.payload else {
            unreachable!()
        };
        fresh.payload = ArtifactPayload::Original(product.clone().with_source_sha256([1; 32]));
        let ArtifactPayload::Original(fresh_product) = &fresh.payload else {
            unreachable!()
        };
        assert_ne!(product, fresh_product);
        assert_eq!(cold, fresh);

        let cold_inventory = ArtifactInventory::default();
        let cold_view = cold_inventory
            .admit(&cold_inventory.empty_view(), vec![cold.clone()])
            .unwrap();
        let fresh_inventory = ArtifactInventory::default();
        let fresh_view = fresh_inventory
            .admit(&fresh_inventory.empty_view(), vec![fresh.clone()])
            .unwrap();
        for (retained, incoming, expected_source) in [
            (&cold_view, &fresh_view, None),
            (&fresh_view, &cold_view, Some([1; 32])),
        ] {
            let merged = retained.merge(incoming).unwrap();
            let selected = merged
                .entries()
                .into_iter()
                .find(|entry| entry.descriptor.id == cold.descriptor.id)
                .unwrap();
            let ArtifactPayload::Original(product) = &selected.payload else {
                unreachable!()
            };
            assert_eq!(product.source_sha256(), expected_source);
        }

        let altered = CertifiedRecoveryProduct::from_certification(
            product.owner().clone(),
            product.interface_bytes().to_vec(),
            b"changed original payload".to_vec(),
            product.package_imports_bytes().to_vec(),
            product.certification_bytes().to_vec(),
        )
        .with_module_interface(product.module_interface().unwrap().clone())
        .unwrap();
        let mut conflicting_bytes = cold.clone();
        conflicting_bytes.payload = ArtifactPayload::Original(altered);
        let mut conflicting_edges = cold.clone();
        conflicting_edges.native_requirements.push((
            module("Original"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 0,
                required_ordinal: 0,
            },
        ));
        for incoming in [conflicting_bytes, conflicting_edges] {
            assert!(matches!(
                cold_inventory.admit(&cold_view, vec![incoming]),
                Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure, ArtifactInventoryFailure::MetadataConflict { artifact }
                        if artifact == cold.descriptor.id)
            ));
        }
        assert_eq!(cold_view.artifact_ids(), fresh_view.artifact_ids());
    }

    #[test]
    fn joined_interface_requirements_are_sets_without_discarding_changed_edges() {
        let canonical = crate::certified_products::fixture_module_interface(
            [2; 32],
            "unit",
            "Joined",
            BTreeMap::new(),
        );
        let interface = CertifiedJoinedInterface::from_certification(
            [2; 32],
            "unit".into(),
            "Joined".into(),
            canonical.interface_bytes().to_vec(),
            canonical.package_imports_bytes().to_vec(),
        )
        .unwrap();
        let joined = |requirements| {
            ArtifactEntry::interface(
                interface.clone(),
                JoinedInterfaceRole::LexicalJoin,
                requirements,
            )
        };
        let first = joined(vec![module("B"), module("A"), module("B")]);
        let second = joined(vec![module("A"), module("B")]);
        assert_eq!(first, second);
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit(
                &inventory.empty_view(),
                vec![entry("A", &[]), entry("B", &[]), first.clone()],
            )
            .unwrap();
        assert_eq!(
            inventory.admit(&view, vec![second]).unwrap().artifact_ids(),
            view.artifact_ids()
        );
        assert!(matches!(
            inventory.admit(&view, vec![joined(vec![module("A")])]),
            Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::MetadataConflict { artifact }
                    if artifact == first.descriptor.id)
        ));
    }

    fn native_variant(entry: &ArtifactEntry, version: u8) -> ArtifactEntry {
        let ArtifactPayload::Original(product) = &entry.payload else {
            panic!("native fixture")
        };
        let interface = product.module_interface().unwrap().clone();
        let mut owner = product.owner().clone();
        owner.module_version = ModuleVersion([version; 32]);
        let certification = crate::certified_products::encode_home_certification_with_module(
            &owner,
            &[],
            &BTreeMap::new(),
            interface.requirements(),
            digest(interface.certificate_bytes()),
        )
        .unwrap();
        let variant = CertifiedRecoveryProduct::from_certification(
            owner,
            product.interface_bytes().to_vec(),
            product.product_bytes().to_vec(),
            product.package_imports_bytes().to_vec(),
            certification,
        )
        .with_module_interface(interface)
        .unwrap();
        ArtifactEntry::original(entry.descriptor.producer_sha256, variant).unwrap()
    }

    #[test]
    fn native_variants_keep_exact_carriers_and_require_explicit_materialization_selection() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let first = native_entry("Original", &[]);
        let second = native_variant(&first, 2);
        let mut consumer = native_entry("Consumer", &["Original"]);
        let ArtifactPayload::Original(product) = &second.payload else {
            unreachable!()
        };
        consumer.native_owners.insert(
            module("Original"),
            NativeOwnerKey::from_owner(product.owner()),
        );
        consumer.native_requirements.push((
            module("Original"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 0,
                required_ordinal: 0,
            },
        ));
        let view = inventory
            .admit(
                &empty,
                vec![first.clone(), second.clone(), consumer.clone()],
            )
            .unwrap();
        let metadata = view.metadata_snapshot();
        assert_eq!(
            metadata.ambiguous_native_owners,
            BTreeSet::from([module("Original")])
        );
        assert!(view
            .entries_for_owners(std::iter::once(module("Original")))
            .is_err());
        let carrier = match &first.payload {
            ArtifactPayload::Original(product) => {
                ArtifactEntry::canonical(product.module_interface().unwrap().clone())
                    .descriptor
                    .id
            }
            _ => unreachable!(),
        };
        let dependencies = view.dependencies();
        for native in [&first, &second] {
            assert!(dependencies.contains(&(
                native.descriptor.id,
                carrier,
                ArtifactDependency::Interface
            )));
        }
        assert!(dependencies.contains(&(
            consumer.descriptor.id,
            second.descriptor.id,
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 0,
                required_ordinal: 0
            }
        )));
        assert!(!dependencies.contains(&(
            consumer.descriptor.id,
            first.descriptor.id,
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 0,
                required_ordinal: 0
            }
        )));
        let selected = view.select_roots(vec![first.descriptor.id]).unwrap();
        assert!(selected
            .metadata_snapshot()
            .ambiguous_native_owners
            .is_empty());
        assert_eq!(
            selected
                .entries_for_owners(std::iter::once(module("Original")))
                .unwrap()[&module("Original")]
                .descriptor
                .id,
            first.descriptor.id
        );
        let types = view.interface_projection(&[module("Original")]).unwrap();
        assert_eq!(types.artifact_ids(), vec![carrier]);
        drop(view);
        assert_eq!(inventory.node_count(), 2);
        drop(selected);
        assert_eq!(inventory.node_count(), 1);
        drop(types);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn native_requirement_cannot_select_an_ambient_exact_native_key() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let original = native_entry("Original", &[]);
        let ambient = inventory.admit(&empty, vec![original.clone()]).unwrap();
        let mut consumer = native_entry("Consumer", &["Original"]);
        consumer.native_requirements.push((
            module("Original"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 0,
                required_ordinal: 0,
            },
        ));
        let types = ambient.interface_projection(&[module("Original")]).unwrap();
        assert!(inventory.admit(&types, vec![consumer.clone()]).is_err());
        let selected = inventory.admit(&ambient, vec![consumer]).unwrap();
        assert!(selected
            .dependencies()
            .iter()
            .any(|(_, target, dependency)| *target == original.descriptor.id
                && matches!(dependency, ArtifactDependency::NativeGroup { .. })));
    }

    #[test]
    fn later_native_admission_cannot_broaden_an_interface_capability() {
        let inventory = ArtifactInventory::default();
        let canonical = entry("Owner", &[]);
        let canonical_id = canonical.descriptor.id;
        let type_cap = inventory
            .admit(&inventory.empty_view(), vec![canonical])
            .unwrap();
        let native = native_entry("Owner", &[]);
        let native_id = native.descriptor.id;
        let execution_cap = inventory.admit(&type_cap, vec![native]).unwrap();
        assert_eq!(type_cap.artifact_ids(), vec![canonical_id]);
        assert!(type_cap.entries().iter().all(|entry| !entry.is_native()));
        assert!(type_cap.source_implementation_roles().is_empty());
        assert_eq!(
            execution_cap.source_implementation_roles(),
            BTreeMap::from([(module("Owner"), ArtifactKind::OriginalModule)])
        );
        assert!(execution_cap.artifact_ids().contains(&native_id));
        assert!(execution_cap.dependencies().contains(&(
            native_id,
            canonical_id,
            ArtifactDependency::Interface
        )));
        let projected = execution_cap
            .interface_projection(&[module("Owner")])
            .unwrap();
        assert_eq!(projected.artifact_ids(), vec![canonical_id]);
        assert!(projected.source_implementation_roles().is_empty());
        drop(execution_cap);
        assert_eq!(inventory.node_count(), 1);
        assert_eq!(type_cap.artifact_ids(), vec![canonical_id]);
    }

    #[test]
    fn same_module_in_distinct_units_admits_together_with_exact_dependencies() {
        let inventory = ArtifactInventory::default();
        let first = entry_in_unit("cohort-a", "EpochModule", vec![]);
        let first_owner = first.descriptor.owner.clone();
        let first_id = first.descriptor.id;
        let second = entry_in_unit("cohort-b", "EpochModule", vec![first_owner.clone()]);
        let second_owner = second.descriptor.owner.clone();
        let second_id = second.descriptor.id;
        let view = inventory
            .admit(&inventory.empty_view(), vec![first, second])
            .unwrap();
        assert_eq!(view.descriptors().len(), 2);
        let entries = view
            .entries_for_owners([first_owner.clone(), second_owner.clone()].into_iter())
            .unwrap();
        assert_eq!(entries[&first_owner].descriptor.id, first_id);
        assert_eq!(entries[&second_owner].descriptor.id, second_id);
        assert_eq!(
            view.dependencies(),
            vec![(second_id, first_id, ArtifactDependency::Interface)]
        );
        let selected = view.select_roots(vec![second_id]).unwrap();
        assert_eq!(selected.root_entries().len(), 1);
        assert_eq!(selected.root_entries()[0].descriptor.owner, second_owner);
        drop(view);
        assert_eq!(selected.descriptors().len(), 2);
        assert_eq!(inventory.node_count(), 2);
        drop(selected);
        assert_eq!(inventory.node_count(), 0);
    }

    fn interface_only_original(
        name: &str,
        required: &ArtifactDescriptor,
        seal: [u8; 32],
    ) -> ArtifactEntry {
        ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [2; 32],
            "unit",
            name,
            BTreeMap::from([(
                (required.owner.unit.clone(), required.owner.module.clone()),
                seal,
            )]),
        ))
    }

    #[test]
    fn certified_interface_only_owner_survives_its_originating_reader_and_reclaims_with_original() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let private = entry("PrivateNominalOwner", &[]);
        let dependent = interface_only_original(
            "PublishedOriginal",
            &private.descriptor,
            private.descriptor.interface_sha256,
        );
        assert_eq!(
            dependent.requirements,
            vec![private.descriptor.owner.clone()]
        );
        assert!(dependent.native_requirements.is_empty());
        let published = dependent.descriptor.id;
        let originating_reader = inventory.admit(&empty, vec![private]).unwrap();
        let issued = inventory
            .admit(&originating_reader, vec![dependent])
            .unwrap();
        let retained = issued.select_roots(vec![published]).unwrap();
        drop(issued);
        drop(originating_reader);
        assert_eq!(inventory.node_count(), 2);
        assert_eq!(retained.descriptors().len(), 2);
        assert_eq!(retained.interface_dependencies().len(), 1);
        assert!(retained
            .native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(published)])
            .unwrap()
            .bindings
            .is_empty());
        drop(retained);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn recovery_cannot_drop_original_certified_interface_only_edges() {
        let private = entry("PrivateNominalOwner", &[]);
        let dependent = interface_only_original(
            "PublishedOriginal",
            &private.descriptor,
            private.descriptor.interface_sha256,
        );
        let descriptors = vec![private.descriptor.clone(), dependent.descriptor.clone()];
        let mut entries = vec![private, dependent];
        assert!(restore_recovery_interface_dependencies(&mut entries, &descriptors, &[]).is_err());
        let edges = vec![(
            descriptors[1].id,
            descriptors[0].id,
            ArtifactDependency::Interface,
        )];
        restore_recovery_interface_dependencies(&mut entries, &descriptors, &edges).unwrap();
        let inventory = ArtifactInventory::default();
        let recovered = inventory.admit(&inventory.empty_view(), entries).unwrap();
        assert_eq!(recovered.interface_dependencies(), edges);
    }

    #[test]
    fn certified_interface_only_owner_rejects_wrong_unit_changed_interface_and_producer_atomically()
    {
        for wrong_unit in [false, true] {
            let inventory = ArtifactInventory::default();
            let empty = inventory.empty_view();
            let private = entry("PrivateNominalOwner", &[]);
            let mut evidence = private.descriptor.clone();
            if wrong_unit {
                evidence.owner.unit = "another-cohort".into();
            }
            let seal = if wrong_unit {
                evidence.interface_sha256
            } else {
                [9; 32]
            };
            let dependent = interface_only_original("PublishedOriginal", &evidence, seal);
            let failure = inventory
                .admit(&empty, vec![private, dependent])
                .unwrap_err();
            let CompileError::ArtifactInventory(failure) = failure else {
                panic!("expected inventory refusal")
            };
            let failure = &failure.failure;
            assert!(if wrong_unit {
                matches!(failure, ArtifactInventoryFailure::MissingDependency { .. })
            } else {
                matches!(
                    failure,
                    ArtifactInventoryFailure::InterfaceSealMismatch { .. }
                )
            });
            assert_eq!(inventory.node_count(), 0);
        }
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let mut private = entry("PrivateNominalOwner", &[]);
        let dependent = interface_only_original(
            "PublishedOriginal",
            &private.descriptor,
            private.descriptor.interface_sha256,
        );
        private = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [8; 32],
            "unit",
            "PrivateNominalOwner",
            BTreeMap::new(),
        ));
        assert!(inventory.admit(&empty, vec![private, dependent]).is_err());
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn owner_conflict_retains_exact_pair_after_unrelated_view_is_reclaimed() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let existing =
            ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                [1; 32],
                "unit",
                "PrivateOwner",
                BTreeMap::new(),
            ));
        let incoming =
            ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                [2; 32],
                "unit",
                "PrivateOwner",
                BTreeMap::new(),
            ));
        let retained = inventory.admit(&empty, vec![existing.clone()]).unwrap();
        let CompileError::ArtifactInventory(error) = inventory
            .admit(&retained, vec![incoming.clone()])
            .unwrap_err()
        else {
            panic!("expected exact owner conflict")
        };
        drop(retained);
        assert_eq!(inventory.node_count(), 0);
        let destination = tempfile::tempdir().unwrap();
        error.retain_owner_conflict(destination.path()).unwrap();
        let root = destination.path().join("owner-conflict");
        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("index.json")).unwrap()).unwrap();
        assert_eq!(index["existing_in_parent"], true);
        for (label, entry) in [("existing", existing), ("incoming", incoming)] {
            let ArtifactPayload::Canonical(interface) = entry.payload else {
                unreachable!()
            };
            assert_eq!(
                std::fs::read(root.join(format!("{label}.finalized.cbor"))).unwrap(),
                interface.certificate_bytes()
            );
            assert_eq!(
                index[label],
                serde_json::to_value(entry.descriptor).unwrap()
            );
        }
    }

    #[test]
    fn same_module_in_distinct_units_admits_successively_and_reclaims_independently() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let first_entry = entry_in_unit("cohort-a", "EpochModule", vec![]);
        let first_owner = first_entry.descriptor.owner.clone();
        let second_entry = entry_in_unit("cohort-b", "EpochModule", vec![]);
        let second_owner = second_entry.descriptor.owner.clone();
        let first = inventory.admit(&empty, vec![first_entry.clone()]).unwrap();
        let captured = first.clone();
        let second = inventory.admit(&first, vec![second_entry]).unwrap();
        assert_eq!(second.descriptors().len(), 2);
        assert_eq!(
            first
                .entries_for_owners([first_owner.clone(), second_owner].into_iter())
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec![&first_owner],
            "retaining a same-named foreign unit does not expose it through an older view"
        );
        drop(second);
        assert_eq!(inventory.node_count(), 1);
        drop(first);
        assert_eq!(inventory.node_count(), 1);
        assert_eq!(captured.descriptors()[0].owner, first_owner);
        drop(captured);
        assert_eq!(inventory.node_count(), 0);
        let readmitted = inventory.admit(&empty, vec![first_entry]).unwrap();
        assert_eq!(readmitted.descriptors()[0].owner, first_owner);
        drop(readmitted);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn disjoint_exact_owner_variants_survive_and_reclaim_independently() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let old = entry("Shared", &[]);
        let new = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [3; 32],
            "unit",
            "Shared",
            BTreeMap::new(),
        ));
        let old_view = inventory.admit(&empty, vec![old.clone()]).unwrap();
        let new_view = inventory.admit(&empty, vec![new.clone()]).unwrap();
        assert_eq!(old_view.descriptors(), vec![old.descriptor.clone()]);
        assert_eq!(new_view.descriptors(), vec![new.descriptor.clone()]);
        assert_eq!(inventory.node_count(), 2);
        let before = inventory.node_count();
        assert!(old_view.merge(&new_view).is_err());
        assert_eq!(inventory.node_count(), before);
        let duplicate = inventory.admit(&empty, vec![old]).unwrap();
        drop(old_view);
        assert_eq!(inventory.node_count(), 2);
        drop(duplicate);
        assert_eq!(inventory.node_count(), 1);
        assert_eq!(new_view.descriptors(), vec![new.descriptor]);
        drop(new_view);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn cross_inventory_conflicting_merge_is_atomic() {
        let first = ArtifactInventory::default();
        let second = ArtifactInventory::default();
        let old = first
            .admit(&first.empty_view(), vec![entry("Shared", &[])])
            .unwrap();
        let new = second
            .admit(
                &second.empty_view(),
                vec![
                    ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                        [3; 32],
                        "unit",
                        "Shared",
                        BTreeMap::new(),
                    )),
                    entry("Fresh", &[]),
                ],
            )
            .unwrap();
        let old_ids = old.artifact_ids();
        let new_ids = new.artifact_ids();
        assert!(old.merge(&new).is_err());
        assert_eq!(first.node_count(), 1);
        assert_eq!(second.node_count(), 2);
        assert_eq!(old.artifact_ids(), old_ids);
        assert_eq!(new.artifact_ids(), new_ids);
    }

    #[test]
    fn ambient_owner_cannot_satisfy_a_dependency_or_missing_seal() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let ambient = inventory
            .admit(&empty, vec![entry("Support", &[])])
            .unwrap();
        let dependent = entry("Consumer", &["Support"]);
        assert!(matches!(inventory.admit(&empty, vec![dependent.clone()]),
            Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::MissingDependency { .. })));
        assert_eq!(inventory.node_count(), 1);
        for drop_requirements in [false, true] {
            let mut unsealed = dependent.clone();
            unsealed.interface_seals.clear();
            if drop_requirements {
                unsealed.requirements.clear();
            }
            assert!(inventory.admit(&ambient, vec![unsealed]).is_err());
            assert_eq!(inventory.node_count(), 1);
        }
        let selected = inventory.admit(&ambient, vec![dependent]).unwrap();
        assert_eq!(selected.descriptors().len(), 2);
    }

    #[test]
    fn reused_artifact_cannot_hide_a_conflicting_dependency_variant() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let consumer = entry("Consumer", &["Support"]);
        let retained = inventory
            .admit(&empty, vec![consumer.clone(), entry("Support", &[])])
            .unwrap();
        let replacement =
            ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                [3; 32],
                "unit",
                "Support",
                BTreeMap::new(),
            ));
        assert!(inventory
            .admit(&empty, vec![consumer, replacement, entry("Fresh", &[])])
            .is_err());
        assert_eq!(inventory.node_count(), 2);
        assert_eq!(retained.descriptors().len(), 2);
    }

    #[test]
    fn reused_graph_targets_must_match_the_selected_sealed_dependencies() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let consumer = entry("Consumer", &["Support"]);
        let retained = inventory
            .admit(&empty, vec![consumer.clone(), entry("Support", &[])])
            .unwrap();
        let unrelated = inventory
            .admit(&empty, vec![entry("Unrelated", &[])])
            .unwrap();
        // Exercise the owning admission guard against a corrupted reused graph.
        let mut state = inventory.0.lock().unwrap();
        let source = state.indices[&consumer.descriptor.id];
        let target = state.indices[&unrelated.0.roots[0]];
        state
            .graph
            .add_edge(source, target, ArtifactDependency::Interface);
        drop(state);
        assert!(inventory.admit(&empty, vec![consumer]).is_err());
        assert_eq!(inventory.node_count(), 3);
        drop(retained);
        drop(unrelated);
    }

    #[test]
    fn exact_owner_conflicts_are_atomic_in_one_batch_and_later_admission() {
        for simultaneous in [true, false] {
            let inventory = ArtifactInventory::default();
            let empty = inventory.empty_view();
            let original = entry_in_unit("cohort-a", "EpochModule", vec![]);
            let owner = original.descriptor.owner.clone();
            let conflicting =
                ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                    [3; 32],
                    "cohort-a",
                    "EpochModule",
                    BTreeMap::new(),
                ));
            let retained = if simultaneous {
                empty.clone()
            } else {
                inventory.admit(&empty, vec![original.clone()]).unwrap()
            };
            let mut incoming = vec![conflicting, entry("Unrelated", &[])];
            if simultaneous {
                incoming.push(original.clone());
            }
            let failure = inventory.admit(&retained, incoming).unwrap_err();
            assert!(matches!(failure, CompileError::ArtifactInventory(error)
                if error.failure == ArtifactInventoryFailure::OwnerConflict { owner: owner.clone() }));
            assert_eq!(inventory.node_count(), usize::from(!simultaneous));
            if !simultaneous {
                assert_eq!(retained.entries()[0].as_ref(), &original);
                let duplicate = inventory.admit(&retained, vec![original]).unwrap();
                assert_eq!(duplicate.descriptors().len(), 1);
                drop(duplicate);
            }
            drop(retained);
            assert_eq!(inventory.node_count(), 0);
        }
    }

    #[test]
    fn same_module_in_another_unit_does_not_satisfy_missing_exact_dependency() {
        let inventory = ArtifactInventory::default();
        let retained = inventory
            .admit(
                &inventory.empty_view(),
                vec![entry_in_unit("cohort-b", "EpochModule", vec![])],
            )
            .unwrap();
        let missing = ExactModuleIdentity {
            unit: "cohort-a".into(),
            module: "EpochModule".into(),
        };
        let failure = inventory
            .admit(
                &retained,
                vec![entry_in_unit("main", "Consumer", vec![missing.clone()])],
            )
            .unwrap_err();
        assert!(matches!(failure, CompileError::ArtifactInventory(error)
            if matches!(&error.failure, ArtifactInventoryFailure::MissingDependency { required, .. }
                if required == &missing)));
        assert_eq!(inventory.node_count(), 1);
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
    fn cross_inventory_merge_keeps_hidden_dependencies_out_of_selected_roots() {
        let destination = ArtifactInventory::default();
        let existing = destination
            .admit(&destination.empty_view(), vec![entry("Existing", &[])])
            .unwrap();
        let source = ArtifactInventory::default();
        let support = entry("Support", &["Hidden"]);
        let support_id = support.descriptor.id;
        let history = source
            .admit(
                &source.empty_view(),
                vec![support, entry("Hidden", &[]), entry("Unrelated", &[])],
            )
            .unwrap();
        let selected = history.select_roots(vec![support_id]).unwrap();
        let merged = existing.merge(&selected).unwrap();
        let mut roots = merged
            .root_entries()
            .iter()
            .map(|entry| entry.descriptor.owner.module.clone())
            .collect::<Vec<_>>();
        roots.sort();
        assert_eq!(roots, ["Existing", "Support"]);
        assert_eq!(merged.descriptors().len(), 3);
        assert_eq!(merged.dependencies().len(), 1);
        drop(existing);
        drop(selected);
        drop(history);
        assert_eq!(source.node_count(), 0);
        assert_eq!(destination.node_count(), 3);
        drop(merged);
        assert_eq!(destination.node_count(), 0);
    }
    #[test]
    fn reclamation_metrics_follow_last_reader_and_selected_closure() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let original = inventory
            .admit(&empty, vec![entry("Original", &[])])
            .unwrap();
        let retained = original.clone();
        let dependent = inventory
            .admit(&original, vec![entry("Consumer", &["Original"])])
            .unwrap();
        let before = inventory.metrics();
        drop(dependent);
        let after = inventory.metrics();
        assert_eq!(after.reclamation_runs - before.reclamation_runs, 1);
        assert_eq!(
            after.reclamation_candidate_nodes - before.reclamation_candidate_nodes,
            2
        );
        assert_eq!(after.reclaimed_nodes - before.reclaimed_nodes, 1);
        assert_eq!(after.nodes, 1);
        drop(original);
        assert_eq!(
            inventory.metrics(),
            after,
            "an Arc reader drop performs no graph reclamation"
        );
        drop(retained);
        let final_metrics = inventory.metrics();
        assert_eq!(final_metrics.reclamation_runs - before.reclamation_runs, 2);
        assert_eq!(
            final_metrics.reclamation_candidate_nodes - before.reclamation_candidate_nodes,
            3
        );
        assert_eq!(final_metrics.reclaimed_nodes - before.reclaimed_nodes, 2);
        assert_eq!(final_metrics.nodes, 0);
        let encoded = serde_json::to_value(final_metrics).unwrap();
        assert!(encoded.get("whole_graph_copies").is_none());
        assert_eq!(encoded["structural_whole_graph_copies"], 0);
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
            4
        );
        drop(second);
        drop(view);
        assert_eq!(inventory.node_count(), 0);
        // Reclamation removes the immutable ID registry entries with the graph nodes.
        let replacement = inventory
            .admit(&empty, vec![entry("Original", &[])])
            .unwrap();
        assert_eq!(replacement.descriptors().len(), 1);
    }

    #[test]
    fn metadata_snapshot_preserves_canonical_typed_edges_in_one_closure() {
        let inventory = ArtifactInventory::default();
        let mut consumer = native_entry("Consumer", &["Original"]);
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
                vec![consumer, native_entry("Original", &[])],
            )
            .unwrap();
        let expected_descriptors = view.descriptors();
        let expected_dependencies = view.dependencies();
        let before = inventory.metrics();
        let metadata = view.metadata_snapshot();
        assert_eq!(
            metadata
                .descriptors()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
            expected_descriptors,
        );
        assert_eq!(metadata.dependencies(), expected_dependencies);
        let after = inventory.metrics();
        assert_eq!(after.graph_visits - before.graph_visits, 4);
        assert_eq!(after.view_queries - before.view_queries, 1);
        assert_eq!(after.entry_handle_copies - before.entry_handle_copies, 6);
    }

    #[test]
    fn reclamation_preserves_incoming_cycles_and_does_not_visit_unrelated_history() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let cycle = inventory
            .admit(&empty, vec![entry("A", &["B"]), entry("B", &["A"])])
            .unwrap();
        let incoming = inventory
            .admit(&cycle, vec![entry("Outside", &["A"])])
            .unwrap();
        let unrelated = inventory
            .admit(&empty, vec![entry("Unrelated", &[])])
            .unwrap();
        let before = inventory.metrics().graph_visits;
        drop(cycle);
        assert_eq!(inventory.node_count(), 4);
        assert_eq!(inventory.metrics().graph_visits - before, 0);
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
        let mut helper = native_entry("Helper", &["Val"]);
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
        let mut consumer = native_entry("Consumer", &["TypeOnly", "Helper"]);
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
            .native_binding_requirements_from_roots(&[NativeRequirementRoot::AllGroups(type_id)])
            .unwrap()
            .is_empty());
        let selected = view
            .native_binding_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
                consumer_id,
            )])
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].identity.module, "Val");
        assert_eq!(selected[0].identity.occurrence, "x");
        assert_eq!(selected[0].generation, 7);
        assert_eq!(
            view.native_binding_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
                helper_id
            )])
            .unwrap()
            .len(),
            2
        );
        assert!(view
            .native_binding_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
                ArtifactId([99; 32])
            )])
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
        let mut consumer = native_entry("Consumer", &["Original"]);
        consumer.native_requirements.push((
            module("Original"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 2,
                required_ordinal: 7,
            },
        ));
        let view = inventory
            .admit(&empty, vec![native_entry("Original", &[]), consumer])
            .unwrap();
        let dependencies = view.dependencies();
        assert_eq!(dependencies.len(), 4);
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
        let original = native_entry("Original", &[]);
        let mut consumer = native_entry("Consumer", &["Original"]);
        consumer.native_requirements.push((
            module("Original"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 2,
                required_ordinal: 7,
            },
        ));
        let canonical = |entry: &ArtifactEntry| {
            let ArtifactPayload::Original(product) = &entry.payload else {
                panic!("native fixture")
            };
            ArtifactEntry::canonical(product.module_interface().unwrap().clone())
        };
        let original_interface = canonical(&original);
        let consumer_interface = canonical(&consumer);
        let edges = vec![
            (
                original.descriptor.id,
                original_interface.descriptor.id,
                ArtifactDependency::Interface,
            ),
            (
                consumer.descriptor.id,
                consumer_interface.descriptor.id,
                ArtifactDependency::Interface,
            ),
            (
                consumer_interface.descriptor.id,
                original_interface.descriptor.id,
                ArtifactDependency::Interface,
            ),
        ];
        let mut entries = vec![
            original.clone(),
            consumer.clone(),
            original_interface,
            consumer_interface,
        ];
        let descriptors = entries
            .iter()
            .map(|entry| entry.descriptor.clone())
            .collect::<Vec<_>>();
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
    fn selected_initial_group_excludes_later_captures_and_accepts_issued_empty_group() {
        use crate::certified_products::{PackageInterfaceWitness, PendingImportOwner};
        use tidepool_repr::execution_schema::testing;

        let package_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(package_file.path(), b"selected package interface").unwrap();
        let package_digest = digest(b"selected package interface");
        let package_identity = |occurrence: &str| {
            let mut identity = testing::identity("Package", occurrence);
            identity.unit = "package-unit".into();
            identity
        };
        let packages = BTreeMap::from([(
            ("package-unit".into(), "Package".into()),
            PackageInterfaceWitness {
                selected_path: package_file.path().to_path_buf(),
                sha256: package_digest,
            },
        )]);
        let capture = |occurrence: &str, generation| PendingImportOwner::Retained {
            identity: testing::identity("Values", occurrence),
            generation,
        };
        let package = |occurrence: &str, generation| PendingImportOwner::RetainedPackage {
            unit: "package-unit".into(),
            module: "Package".into(),
            binder: package_identity(occurrence),
            generation,
            interface_digest: package_digest,
        };
        let product = crate::certified_products::tests::original_groups_fixture(
            "Segment",
            vec![
                (3, vec![capture("first", 4), package("first", 0)]),
                (11, vec![capture("later", 9), package("later", 7)]),
                (29, Vec::new()),
            ],
            1,
            &packages,
        );
        let segment = ArtifactEntry::original(
            [2; 32],
            crate::certified_products::fixture_finalized_product(product, [2; 32]),
        )
        .unwrap();
        let values = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [2; 32],
            "fixture",
            "Values",
            BTreeMap::new(),
        ));
        let outside = entry("Outside", &[]);
        let (segment_id, values_id, outside_id) = (
            segment.descriptor.id,
            values.descriptor.id,
            outside.descriptor.id,
        );
        let inventory = ArtifactInventory::default();
        let admitted = inventory
            .admit(&inventory.empty_view(), vec![segment, values, outside])
            .unwrap();
        let view = admitted.select_roots(vec![segment_id]).unwrap();
        let group = |original_ordinal| NativeRequirementRoot::Group {
            artifact: segment_id,
            original_ordinal,
        };
        let first = view.native_requirements_from_roots(&[group(3)]).unwrap();
        assert_eq!(
            first.bindings,
            vec![NativeBindingRequirement {
                artifact_id: values_id,
                identity: testing::identity("Values", "first"),
                generation: 4,
            }]
        );
        assert_eq!(
            first.packages,
            vec![NativePackageRequirement {
                artifact_id: segment_id,
                identity: package_identity("first"),
                generation: 0,
                interface_digest: package_digest,
            }]
        );
        assert_eq!(
            view.native_requirements_from_roots(&[group(29)]).unwrap(),
            NativeRequirements::default()
        );
        assert_eq!(
            view.native_requirements_from_roots(&[group(3), group(3)])
                .unwrap(),
            first
        );
        let all = view
            .native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(segment_id)])
            .unwrap();
        assert_eq!(all.bindings.len(), 2);
        assert_eq!(all.packages.len(), 2);
        assert_eq!(
            all,
            view.native_requirements_from_roots(&[group(3), group(11), group(29)])
                .unwrap()
        );
        for (artifact, original_ordinal) in [(segment_id, 0), (segment_id, 30), (values_id, 3)] {
            assert!(
                matches!(view.native_requirements_from_roots(&[NativeRequirementRoot::Group { artifact, original_ordinal }]),
                Err(CompileError::ArtifactInventory(error)) if error.failure == ArtifactInventoryFailure::NativeGroupUnavailable { artifact, original_ordinal })
            );
        }
        for artifact in [outside_id, ArtifactId([255; 32])] {
            assert!(
                matches!(view.native_requirements_from_roots(&[NativeRequirementRoot::Group { artifact, original_ordinal: 3 }]),
                Err(CompileError::ArtifactInventory(error)) if error.failure == ArtifactInventoryFailure::NativeRootOutsideView { artifact })
            );
        }
        // Demand selection leaves the original module and its later groups in
        // custody; it only narrows which native obligations this call requests.
        assert!(view.artifact_ids().contains(&segment_id));
        drop(admitted);
        drop(view);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn native_package_obligations_follow_selected_groups_without_package_artifact_nodes() {
        let inventory = ArtifactInventory::default();
        let mut root = native_entry("Root", &[]);
        root.native_requirements.push((
            module("Helper"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 2,
                required_ordinal: 7,
            },
        ));
        let mut helper = native_entry("Helper", &[]);
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
        let requirements = view
            .native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(root_id)])
            .unwrap();
        assert!(requirements.bindings.is_empty());
        assert_eq!(requirements.packages.len(), 1);
        assert_eq!(requirements.packages[0].artifact_id, helper_id);
        assert_eq!(requirements.packages[0].identity.occurrence, "map");
        assert_eq!(
            view.native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(helper_id)])
                .unwrap()
                .packages
                .len(),
            2
        );
        assert_eq!(inventory.metrics().nodes, 4);
    }
}
