//! Scoped original-artifact ownership. Graph indices are private implementation
//! details; durable and compiler boundaries use content-bound artifact IDs.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

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

mod compiler_projection;
pub use compiler_projection::{CompilerInputProjection, CompilerInputRole};

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

/// Content-bound original group selection. These serialized facts grant no
/// authority until the inventory checks the certified group and its closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeGroupKey {
    pub artifact: ArtifactId,
    pub original_ordinal: u32,
}

/// Whole-module demand and a checked compiler entry cross the same admission
/// owner. A raw ordinal or recovered selection cannot mint a checked entry.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeArtifactDemand<'a> {
    AllGroups,
    /// Checked imports retain original byte custody and interface authority.
    /// They have no executable target that could demand a native group.
    ScopeInterfaces,
    /// A checked authored entry and its same compiled executable wrapper have
    /// distinct native dependencies. Neither implies whole-carrier demand.
    VerifiedTarget {
        entry: &'a crate::checked_cell::CheckedTypedEntry,
        imports: &'a [crate::certified_products::PendingImportOwner],
    },
    /// Existing target certification issued these imports against authenticated
    /// original membership. Only new products receive whole-module demand.
    CertifiedTargetImports(&'a [crate::certified_products::PendingImportOwner]),
}

/// Target imports were issued by the existing target certifier; check exact
/// original membership again before turning source references into graph roots.
fn certified_target_source_groups(
    entries: &[Arc<ArtifactEntry>],
    imports: &[crate::certified_products::PendingImportOwner],
) -> Result<BTreeSet<NativeGroupKey>, CompileError> {
    let mut groups = BTreeSet::new();
    for import in imports {
        let crate::certified_products::PendingImportOwner::Source {
            owner,
            original_ordinal,
            binder,
        } = import
        else {
            continue;
        };
        let mut originals = entries.iter().filter(|entry| {
            matches!(&entry.payload, ArtifactPayload::Original(product)
            if product.owner() == owner
                && crate::certified_products::authenticates_original_native_entry(
                    product, *original_ordinal, binder,
                ))
        });
        let original = originals
            .next()
            .ok_or_else(|| failure("certified target native membership"))?;
        if originals.next().is_some() {
            return Err(failure("certified target native membership"));
        }
        groups.insert(NativeGroupKey {
            artifact: original.descriptor.id,
            original_ordinal: *original_ordinal,
        });
    }
    Ok(groups)
}

/// Copying a retained closure preserves its issuing view's root intent. Full
/// carrier custody does not promote hidden dependencies to explicit roots.
enum ArtifactRootIntent<'a> {
    SuppliedArtifacts,
    RetainedView(&'a ArtifactView),
}

/// Graph vertices retain one full artifact allocation separately from its
/// admitted native groups. A carrier never implies demand for every group.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
enum InventoryNodeKey {
    Artifact(ArtifactId),
    Group(NativeGroupKey),
}
impl InventoryNodeKey {
    fn artifact(self) -> ArtifactId {
        match self {
            Self::Artifact(id) => id,
            Self::Group(key) => key.artifact,
        }
    }
}

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
    #[error("compiler owner {owner:?} selects original {existing:?} and {incoming:?}")]
    CompilerOriginalOfferConflict {
        owner: ExactModuleIdentity,
        existing: ArtifactId,
        incoming: ArtifactId,
    },
    #[error("authored generation {generation} requires one certified native root; found {found}")]
    AuthoredNativeRoot { generation: u64, found: usize },
    #[error("native root {artifact:?} is outside its retained view")]
    NativeRootOutsideView { artifact: ArtifactId },
    #[error("native root {artifact:?} has no original native proof")]
    NativeRootNotOriginal { artifact: ArtifactId },
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

/// An exact live value required by original native code. Execution selection
/// and conservative authored-module lifetime custody query this separately.
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
    /// Complete certified edges grouped by their dependent original ordinal.
    /// Zero-edge groups remain in the independent certified ordinal census.
    pub native_requirements: BTreeMap<u32, Vec<(ExactModuleIdentity, ArtifactDependency)>>,
    native_owners: BTreeMap<ExactModuleIdentity, NativeOwnerKey>,
    native_group_ordinals: BTreeSet<u32>,
    pub retained_packages: Vec<RetainedPackageDependency>,
}

fn index_native_requirements(
    requirements: Vec<(ExactModuleIdentity, ArtifactDependency)>,
) -> Result<BTreeMap<u32, Vec<(ExactModuleIdentity, ArtifactDependency)>>, CompileError> {
    let mut indexed = BTreeMap::<u32, Vec<_>>::new();
    for (owner, dependency) in requirements {
        let ordinal = match &dependency {
            ArtifactDependency::NativeGroup {
                dependent_ordinal, ..
            }
            | ArtifactDependency::NativeBinding {
                dependent_ordinal, ..
            } => *dependent_ordinal,
            ArtifactDependency::Interface => {
                return Err(failure("interface edge in native group witness"));
            }
        };
        indexed
            .entry(ordinal)
            .or_default()
            .push((owner, dependency));
    }
    for requirements in indexed.values_mut() {
        requirements.sort();
        requirements.dedup();
    }
    Ok(indexed)
}

impl ArtifactEntry {
    fn native_requirements_for(
        &self,
        ordinal: u32,
    ) -> &[(ExactModuleIdentity, ArtifactDependency)] {
        self.native_requirements
            .get(&ordinal)
            .map_or(&[], Vec::as_slice)
    }

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
        let descriptor = Self::original_descriptor(producer, &product);
        Ok(Self {
            descriptor,
            payload: ArtifactPayload::Original(product),
            requirements,
            interface_seals,
            native_requirements: index_native_requirements(native_requirements.artifact_edges)?,
            native_owners,
            native_group_ordinals: native_requirements.group_ordinals,
            retained_packages: native_requirements.retained_packages,
        })
    }
    fn original_descriptor(
        producer: [u8; 32],
        product: &CertifiedRecoveryProduct,
    ) -> ArtifactDescriptor {
        let owner = product.owner();
        descriptor(
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
        )
    }
    pub(crate) fn original_artifact_id(
        producer: [u8; 32],
        product: &CertifiedRecoveryProduct,
    ) -> ArtifactId {
        Self::original_descriptor(producer, product).id
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
            native_requirements: BTreeMap::new(),
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
            native_requirements: BTreeMap::new(),
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
    graph: StableDiGraph<InventoryNodeKey, ArtifactDependency>,
    indices: BTreeMap<InventoryNodeKey, NodeIndex>,
    payloads: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    roots: BTreeMap<InventoryNodeKey, usize>,
    graph_visits: AtomicU64,
    view_queries: AtomicU64,
    entry_handle_copies: AtomicU64,
    admission_owner_lookups: AtomicU64,
    #[cfg(test)]
    native_requirement_rows_examined: AtomicU64,
    #[cfg(test)]
    dependency_graph_visits: AtomicU64,
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
        groups: &BTreeSet<NativeGroupKey>,
    ) -> Result<
        (
            BTreeMap<InventoryNodeKey, BTreeSet<(InventoryNodeKey, ArtifactDependency)>>,
            BTreeSet<NativeGroupKey>,
        ),
        CompileError,
    > {
        let mut planned = BTreeMap::new();
        for (id, entry) in entries {
            planned.insert(
                InventoryNodeKey::Artifact(*id),
                self.dependencies(state, entry, entries)?
                    .into_iter()
                    .map(|(id, edge)| (InventoryNodeKey::Artifact(id), edge))
                    .collect(),
            );
        }
        let mut pending = groups.iter().copied().collect::<Vec<_>>();
        while let Some(key) = pending.pop() {
            let node = InventoryNodeKey::Group(key);
            if planned.contains_key(&node) {
                continue;
            }
            let entry = entries.get(&key.artifact).ok_or_else(|| {
                admission_failure(ArtifactInventoryFailure::NativeRootOutsideView {
                    artifact: key.artifact,
                })
            })?;
            if !entry.native_group_ordinals.contains(&key.original_ordinal) {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeGroupUnavailable {
                        artifact: key.artifact,
                        original_ordinal: key.original_ordinal,
                    },
                ));
            }
            let mut edges = BTreeSet::from([(
                InventoryNodeKey::Artifact(key.artifact),
                ArtifactDependency::Interface,
            )]);
            for (owner, dependency) in entry.native_requirements_for(key.original_ordinal) {
                #[cfg(test)]
                state
                    .native_requirement_rows_examined
                    .fetch_add(1, Ordering::Relaxed);
                state
                    .admission_owner_lookups
                    .fetch_add(1, Ordering::Relaxed);
                let id = match dependency {
                    ArtifactDependency::NativeGroup { .. } => entry
                        .native_owners
                        .get(owner)
                        .and_then(|owner| self.native.get(owner)),
                    _ => self.interfaces.get(owner),
                }
                .copied()
                .ok_or_else(|| {
                    admission_failure(ArtifactInventoryFailure::MissingDependency {
                        artifact: key.artifact,
                        dependent: entry.descriptor.owner.clone(),
                        required: owner.clone(),
                        dependency: dependency.clone(),
                    })
                })?;
                let target = match dependency {
                    ArtifactDependency::NativeGroup {
                        required_ordinal, ..
                    } => {
                        let required = NativeGroupKey {
                            artifact: id,
                            original_ordinal: *required_ordinal,
                        };
                        pending.push(required);
                        InventoryNodeKey::Group(required)
                    }
                    _ => InventoryNodeKey::Artifact(id),
                };
                edges.insert((target, dependency.clone()));
            }
            planned.insert(node, edges);
        }
        let closed_groups = planned
            .keys()
            .filter_map(|key| match key {
                InventoryNodeKey::Group(key) => Some(*key),
                _ => None,
            })
            .collect();
        let mut additions = BTreeMap::new();
        for (key, expected) in planned {
            if let Some(index) = state.indices.get(&key) {
                let actual = state
                    .graph
                    .edges(*index)
                    .map(|edge| (state.graph[edge.target()], edge.weight().clone()))
                    .collect::<BTreeSet<_>>();
                if actual != expected {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::MetadataConflict {
                            artifact: key.artifact(),
                        },
                    ));
                }
            } else {
                additions.insert(key, expected);
            }
        }
        Ok((additions, closed_groups))
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
        roots: Vec<InventoryNodeKey>,
        parents: Vec<Arc<ViewLease>>,
        materialization_parents: Vec<
            Arc<crate::declaration_context::RetainedArtifactMaterialization>,
        >,
    ) -> ArtifactView {
        let mut state = self.0.lock().expect("inventory lock");
        for id in &roots {
            *state.roots.entry(*id).or_default() += 1;
        }
        drop(state);
        ArtifactView::new(ViewLease {
            inventory: self.clone(),
            roots,
            parents,
            materialization_parents,
            materialization: Mutex::new(BTreeMap::new()),
        })
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
                for requirements in entry.native_requirements.values_mut() {
                    requirements.sort();
                    requirements.dedup();
                }
                entry.retained_packages.sort();
                entry.retained_packages.dedup();
                Arc::new(entry)
            })
            .collect();
        self.admit_shared(parent, entries)
    }

    /// Ordinary whole-product consumers demand the complete certified census.
    pub(crate) fn admit_shared(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
    ) -> Result<ArtifactView, CompileError> {
        self.admit_shared_with_demand(parent, entries, NativeArtifactDemand::AllGroups)
    }
    pub(crate) fn admit_shared_with_demand(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        demand: NativeArtifactDemand<'_>,
    ) -> Result<ArtifactView, CompileError> {
        let groups = match demand {
            NativeArtifactDemand::ScopeInterfaces => BTreeSet::new(),
            NativeArtifactDemand::AllGroups => entries
                .iter()
                .flat_map(|entry| {
                    entry
                        .native_group_ordinals
                        .iter()
                        .map(move |ordinal| NativeGroupKey {
                            artifact: entry.descriptor.id,
                            original_ordinal: *ordinal,
                        })
                })
                .collect(),
            NativeArtifactDemand::VerifiedTarget { entry, imports } => {
                let mut groups = certified_target_source_groups(&entries, imports)?;
                groups.insert(entry.native_group_key());
                groups
            }
            NativeArtifactDemand::CertifiedTargetImports(imports) => {
                let inherited = parent.artifact_ids().into_iter().collect::<BTreeSet<_>>();
                let mut groups = entries
                    .iter()
                    .filter(|entry| !inherited.contains(&entry.descriptor.id))
                    .flat_map(|entry| {
                        entry
                            .native_group_ordinals
                            .iter()
                            .map(move |ordinal| NativeGroupKey {
                                artifact: entry.descriptor.id,
                                original_ordinal: *ordinal,
                            })
                    })
                    .collect::<BTreeSet<_>>();
                groups.extend(certified_target_source_groups(&entries, imports)?);
                groups
            }
        };
        self.admit_selected(
            parent,
            entries,
            groups,
            false,
            ArtifactRootIntent::SuppliedArtifacts,
        )
    }
    /// Persisted keys are checked against full certified bytes and exact closure.
    pub(crate) fn admit_recovery_selection(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        selected_native_groups: &BTreeSet<NativeGroupKey>,
    ) -> Result<ArtifactView, CompileError> {
        self.admit_selected(
            parent,
            entries,
            selected_native_groups.clone(),
            true,
            ArtifactRootIntent::SuppliedArtifacts,
        )
    }
    fn admit_selected(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        groups: BTreeSet<NativeGroupKey>,
        exact: bool,
        root_intent: ArtifactRootIntent<'_>,
    ) -> Result<ArtifactView, CompileError> {
        if !Arc::ptr_eq(&self.0, &parent.lease.inventory.0) {
            return Err(failure("view belongs to another inventory"));
        }
        if entries.is_empty() && groups.is_empty() {
            return Ok(parent.clone());
        }
        let mut materialization_parents = Vec::new();
        let retained_roots = match root_intent {
            ArtifactRootIntent::SuppliedArtifacts => None,
            ArtifactRootIntent::RetainedView(source) => {
                source.collect_materializations(&mut materialization_parents, &mut BTreeSet::new());
                Some(source.roots().to_vec())
            }
        };
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
        let parent_nodes = &parent.read_projection(&state).nodes;
        let parent_ids = artifact_ids(parent_nodes);
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
                    Arc::clone(previous)
                } else {
                    entry
                };
            supplied.insert(id, retained);
        }
        let roots = retained_roots.unwrap_or_else(|| {
            supplied
                .keys()
                .copied()
                .map(InventoryNodeKey::Artifact)
                .chain(groups.iter().copied().map(InventoryNodeKey::Group))
                .collect()
        });
        let mut selected_ids = parent_ids.clone();
        selected_ids.extend(artifact_ids(&admitted_closure(
            &state,
            supplied.keys().copied().map(InventoryNodeKey::Artifact),
        )));
        let mut selected = selected_ids
            .iter()
            .map(|id| (*id, Arc::clone(&state.payloads[id])))
            .collect::<BTreeMap<_, _>>();
        state
            .entry_handle_copies
            .fetch_add(selected.len() as u64, Ordering::Relaxed);
        selected.extend(supplied);
        let owners = SelectedOwners::new(&state, &selected, &parent_ids)?;
        let mut selected_groups = native_groups(parent_nodes);
        selected_groups.extend(groups.iter().copied());
        if roots.iter().any(|root| match root {
            InventoryNodeKey::Artifact(id) => !selected.contains_key(id),
            InventoryNodeKey::Group(group) => !selected_groups.contains(group),
        }) {
            return Err(failure("retained merge root outside copied selection"));
        }
        let (edges, closed_groups) = owners.planned_edges(&state, &selected, &selected_groups)?;
        if exact && closed_groups != selected_groups {
            return Err(failure(
                "recovered native group closure differs from certified demand",
            ));
        }
        for key in edges.keys() {
            let index = state.graph.add_node(*key);
            state.indices.insert(*key, index);
        }
        for (key, dependencies) in edges {
            let source = state.indices[&key];
            for (target, dependency) in dependencies {
                let target = state.indices[&target];
                state.graph.add_edge(source, target, dependency);
            }
            if let InventoryNodeKey::Artifact(id) = key {
                state.payloads.insert(id, Arc::clone(&selected[&id]));
                state.entry_handle_copies.fetch_add(1, Ordering::Relaxed);
            }
        }
        for key in &roots {
            *state.roots.entry(*key).or_default() += 1;
        }
        drop(state);
        Ok(ArtifactView::new(ViewLease {
            inventory: self.clone(),
            roots,
            parents: vec![Arc::clone(&parent.lease)],
            materialization_parents,
            materialization: Mutex::new(BTreeMap::new()),
        }))
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
    roots: Vec<InventoryNodeKey>,
    parents: Vec<Arc<ViewLease>>,
    // Projected selection must retain issued files without importing its
    // source view's selection roots or executable authority.
    materialization_parents: Vec<Arc<crate::declaration_context::RetainedArtifactMaterialization>>,
    materialization:
        Mutex<BTreeMap<[u8; 32], Arc<crate::declaration_context::RetainedArtifactMaterialization>>>,
}

/// Parent custody retains roots and materializations without retaining derived
/// read projections. Only explicitly held views keep these caches alive.
#[derive(Default)]
struct ViewReadCache {
    canonical_roots: OnceLock<Vec<InventoryNodeKey>>,
    read_projection: OnceLock<ViewReadProjection>,
    #[cfg(test)]
    root_parent_visits: AtomicU64,
}

/// Live vertices never change payload or outgoing edges: admission checks reused
/// vertices against the complete plan, and reclamation cannot remove a view's
/// rooted closure. This read projection therefore needs no invalidation. Stable
/// keys survive unrelated graph-slot reuse; entry handles copy no artifact bytes.
/// Neither projection nor detached metadata retains a view or materialization.
struct ViewReadProjection {
    nodes: BTreeSet<InventoryNodeKey>,
    entries: Vec<Arc<ArtifactEntry>>,
    dependencies: OnceLock<Vec<(ArtifactId, ArtifactId, ArtifactDependency)>>,
}
impl ViewLease {
    fn collect_materializations(
        self: &Arc<Self>,
        materializations: &mut Vec<
            Arc<crate::declaration_context::RetainedArtifactMaterialization>,
        >,
        visited: &mut BTreeSet<usize>,
    ) {
        let mut pending = vec![self];
        while let Some(view) = pending.pop() {
            if !visited.insert(Arc::as_ptr(view) as usize) {
                continue;
            }
            let retained = view.materialization.lock().expect("materialization lock");
            if !retained.is_empty() {
                for materialization in retained.values() {
                    if !materializations
                        .iter()
                        .any(|existing| Arc::ptr_eq(existing, materialization))
                    {
                        materializations.push(Arc::clone(materialization));
                    }
                }
            } else {
                for materialization in &view.materialization_parents {
                    if !materializations
                        .iter()
                        .any(|existing| Arc::ptr_eq(existing, materialization))
                    {
                        materializations.push(Arc::clone(materialization));
                    }
                }
                pending.extend(view.parents.iter().rev());
            }
        }
    }
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
        let candidates = admitted_closure(&state, lost_roots.into_iter());
        state.reclamation_candidate_nodes += candidates.len() as u64;
        let survivors = candidates
            .iter()
            .copied()
            .filter(|id| {
                state.roots.contains_key(id)
                    || state
                        .graph
                        .edges_directed(state.indices[id], Direction::Incoming)
                        .any(|edge| !candidates.contains(&state.graph[edge.source()]))
            })
            .collect::<Vec<_>>();
        let retained = admitted_closure(&state, survivors.into_iter());
        for id in candidates.difference(&retained) {
            let index = state.indices.remove(id).expect("indexed artifact");
            state.graph.remove_node(index);
            state.reclaimed_nodes += 1;
            if let InventoryNodeKey::Artifact(id) = id {
                state.payloads.remove(id).expect("owned payload");
            }
        }
        state.reclamation_elapsed_ns += started.elapsed().as_nanos() as u64;
        // Parent drops after the lock guard, preserving recursive release.
    }
}
fn admitted_closure(
    state: &InventoryState,
    roots: impl Iterator<Item = InventoryNodeKey>,
) -> BTreeSet<InventoryNodeKey> {
    let mut pending = roots.collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    while let Some(id) = pending.pop() {
        // Planned seeds are validated by the admission solver, not represented
        // as retained vertices until its complete atomic plan is committed.
        let Some(index) = state.indices.get(&id) else {
            continue;
        };
        if seen.insert(id) {
            state.graph_visits.fetch_add(1, Ordering::Relaxed);
            pending.extend(
                state
                    .graph
                    .edges(*index)
                    .map(|edge| state.graph[edge.target()]),
            );
        }
    }
    seen
}
fn artifact_ids(nodes: &BTreeSet<InventoryNodeKey>) -> BTreeSet<ArtifactId> {
    nodes
        .iter()
        .filter_map(|key| match key {
            InventoryNodeKey::Artifact(id) => Some(*id),
            _ => None,
        })
        .collect()
}
fn native_groups(nodes: &BTreeSet<InventoryNodeKey>) -> BTreeSet<NativeGroupKey> {
    nodes
        .iter()
        .filter_map(|key| match key {
            InventoryNodeKey::Group(key) => Some(*key),
            _ => None,
        })
        .collect()
}
fn projected_dependencies(
    state: &InventoryState,
    nodes: &BTreeSet<InventoryNodeKey>,
) -> Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
    let mut dependencies = BTreeSet::new();
    for key in nodes {
        #[cfg(test)]
        state
            .dependency_graph_visits
            .fetch_add(1, Ordering::Relaxed);
        for edge in state.graph.edges(state.indices[key]) {
            let target = state.graph[edge.target()];
            // The group-to-carrier custody edge has no public dependency row.
            if matches!(key, InventoryNodeKey::Group(group)
                if target == InventoryNodeKey::Artifact(group.artifact))
                && matches!(edge.weight(), ArtifactDependency::Interface)
            {
                continue;
            }
            dependencies.insert((key.artifact(), target.artifact(), edge.weight().clone()));
        }
    }
    dependencies.into_iter().collect()
}
/// Clones share custody and the immutable read cache. Descendants retain only
/// parent custody, so replacing a view releases its unobserved ancestor caches.
#[derive(Clone)]
pub struct ArtifactView {
    lease: Arc<ViewLease>,
    reads: Arc<ViewReadCache>,
}

/// One retained closure observed under the inventory lock. Entries are ordered
/// by exact owner; dependency tuples keep their canonical wire order.
pub(crate) struct ArtifactMetadataSnapshot {
    pub entries: BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>,
    pub artifacts: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    pub ambiguous_native_owners: BTreeSet<ExactModuleIdentity>,
    dependencies: Vec<(ArtifactId, ArtifactId, ArtifactDependency)>,
    pub(crate) selected_native_groups: BTreeSet<NativeGroupKey>,
}

impl ArtifactMetadataSnapshot {
    pub(crate) fn materialization_key(&self) -> [u8; 32] {
        digest(
            &serde_json::to_vec(&(
                self.entries
                    .values()
                    .map(|entry| entry.descriptor.id)
                    .collect::<Vec<_>>(),
                &self.selected_native_groups,
            ))
            .expect("compiler projection materialization key"),
        )
    }
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
            .field("roots", &self.lease.roots)
            .finish()
    }
}
impl ArtifactView {
    fn new(lease: ViewLease) -> Self {
        Self {
            lease: Arc::new(lease),
            reads: Arc::new(ViewReadCache::default()),
        }
    }

    // Closure and dependency initializers receive an already locked inventory. Nothing
    // initialized under either OnceLock acquires that mutex, so concurrent first
    // reads and admission always take inventory -> projection in that order.
    fn read_projection(&self, state: &InventoryState) -> &ViewReadProjection {
        self.reads.read_projection.get_or_init(|| {
            let nodes = admitted_closure(state, self.roots().iter().copied());
            let mut entries = artifact_ids(&nodes)
                .iter()
                .map(|id| Arc::clone(&state.payloads[id]))
                .collect::<Vec<_>>();
            state
                .entry_handle_copies
                .fetch_add(entries.len() as u64, Ordering::Relaxed);
            entries.sort_by(|left, right| {
                (&left.descriptor.owner, left.descriptor.id)
                    .cmp(&(&right.descriptor.owner, right.descriptor.id))
            });
            ViewReadProjection {
                nodes,
                entries,
                dependencies: OnceLock::new(),
            }
        })
    }

    fn read_dependencies(
        &self,
        state: &InventoryState,
    ) -> &Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
        let projection = self.read_projection(state);
        projection
            .dependencies
            .get_or_init(|| projected_dependencies(state, &projection.nodes))
    }

    /// Private materialization belongs to this immutable graph view. Failed
    /// preparation leaves no retained entry; descendants borrow completed owners.
    pub(crate) fn retain_materialization(
        &self,
        metadata: &ArtifactMetadataSnapshot,
        prepare: impl FnOnce(
            Vec<Arc<crate::declaration_context::RetainedArtifactMaterialization>>,
        ) -> Result<
            crate::declaration_context::RetainedArtifactMaterialization,
            CompileError,
        >,
    ) -> Result<Arc<crate::declaration_context::RetainedArtifactMaterialization>, CompileError>
    {
        let mut retained = self
            .lease
            .materialization
            .lock()
            .expect("materialization lock");
        let key = metadata.materialization_key();
        if let Some(materialization) = retained.get(&key) {
            return Ok(Arc::clone(materialization));
        }
        let mut parents = self.lease.materialization_parents.clone();
        let mut visited = BTreeSet::new();
        for parent in &self.lease.parents {
            parent.collect_materializations(&mut parents, &mut visited);
        }
        let materialization = Arc::new(prepare(parents)?);
        retained.insert(key, Arc::clone(&materialization));
        Ok(materialization)
    }

    fn collect_materializations(
        &self,
        materializations: &mut Vec<
            Arc<crate::declaration_context::RetainedArtifactMaterialization>,
        >,
        visited: &mut BTreeSet<usize>,
    ) {
        self.lease
            .collect_materializations(materializations, visited);
    }

    pub(crate) fn retained_materialization(
        &self,
        metadata: &ArtifactMetadataSnapshot,
    ) -> Option<Arc<crate::declaration_context::RetainedArtifactMaterialization>> {
        self.lease
            .materialization
            .lock()
            .expect("materialization lock")
            .get(&metadata.materialization_key())
            .cloned()
    }
    pub(crate) fn metadata_snapshot(&self) -> ArtifactMetadataSnapshot {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        let projection = self.read_projection(&state);
        let selected_native_groups = native_groups(&projection.nodes);
        let mut entries = BTreeMap::new();
        let mut artifacts = BTreeMap::new();
        let dependencies = self.read_dependencies(&state).clone();
        let mut natives = BTreeMap::<ExactModuleIdentity, Vec<ArtifactId>>::new();
        for entry in &projection.entries {
            let id = entry.descriptor.id;
            let owner = entry.descriptor.owner.clone();
            if entry.is_native() {
                natives.entry(owner).or_default().push(id);
            } else {
                entries.insert(owner, id);
            }
            artifacts.insert(id, Arc::clone(entry));
        }
        let mut ambiguous_native_owners = BTreeSet::new();
        for (owner, ids) in natives {
            if ids.len() == 1 {
                entries.insert(owner, ids[0]);
            } else {
                ambiguous_native_owners.insert(owner);
            }
        }
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
            selected_native_groups,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.roots().is_empty()
    }
    pub fn inventory(&self) -> &ArtifactInventory {
        &self.lease.inventory
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
    /// Classify retained implementations for lexical traversal. Exact variants
    /// share their kind; this census never selects a compiler original offer.
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
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        self.read_dependencies(&state).clone()
    }
    pub fn selected_native_groups(&self) -> BTreeSet<NativeGroupKey> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        native_groups(&self.read_projection(&state).nodes)
    }
    /// Interface dependencies remain independent of executable group demand.
    pub fn interface_dependencies(&self) -> Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
        self.dependencies()
            .into_iter()
            .filter(|(_, _, dependency)| matches!(dependency, ArtifactDependency::Interface))
            .collect()
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

    /// Read-only lifetime requirements for accepted authored owners. Retaining a
    /// full module also retains private helpers that may be called by a later
    /// exported entry; this query grants no executable group selection.
    pub(crate) fn authored_native_binding_custody_requirements(
        &self,
        accepted_owners: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<Vec<NativeBindingRequirement>, CompileError> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let owned = &self.read_projection(&state).nodes;
        let ids = artifact_ids(owned);
        let entries = ids
            .iter()
            .map(|id| (*id, Arc::clone(&state.payloads[id])))
            .collect::<BTreeMap<_, _>>();
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        let owners = SelectedOwners::new(&state, &entries, &ids)?;
        let roots = entries
            .values()
            .filter(|entry| {
                accepted_owners.contains(&entry.descriptor.owner)
                    && matches!(&entry.payload, ArtifactPayload::Original(product)
                        if product.module_interface().is_some_and(|interface|
                            matches!(interface.origin(), crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. })))
            })
            .flat_map(|entry| entry.native_group_ordinals.iter().map(|ordinal| NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: *ordinal,
            }))
            .collect();
        // The same owner/version/ordinal resolver used for executable admission
        // validates full-body dependencies, without installing its planned edges.
        let (_, groups) = owners.planned_edges(&state, &entries, &roots)?;
        let mut requirements = BTreeSet::new();
        for group in groups {
            state.graph_visits.fetch_add(1, Ordering::Relaxed);
            for (owner, dependency) in
                entries[&group.artifact].native_requirements_for(group.original_ordinal)
            {
                #[cfg(test)]
                state
                    .native_requirement_rows_examined
                    .fetch_add(1, Ordering::Relaxed);
                if let ArtifactDependency::NativeBinding {
                    generation,
                    namespace,
                    occurrence,
                    record_parent,
                    ..
                } = dependency
                {
                    let id = owners.interfaces.get(owner).copied().ok_or_else(|| {
                        failure("validated native binding lacks its interface owner")
                    })?;
                    requirements.insert(NativeBindingRequirement {
                        artifact_id: id,
                        identity: tidepool_repr::execution_schema::SymbolIdentity {
                            unit: owner.unit.clone(),
                            module: owner.module.clone(),
                            namespace: namespace.clone(),
                            occurrence: occurrence.clone(),
                            record_parent: record_parent.clone(),
                        },
                        generation: *generation,
                    });
                }
            }
        }
        Ok(requirements.into_iter().collect())
    }

    pub fn native_requirements_from_roots(
        &self,
        roots: &[NativeRequirementRoot],
    ) -> Result<NativeRequirements, CompileError> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let owned = &self.read_projection(&state).nodes;
        let mut pending = Vec::new();
        for root in roots {
            let (id, ordinals) = match *root {
                NativeRequirementRoot::AllGroups(id) => {
                    if !owned.contains(&InventoryNodeKey::Artifact(id)) {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::NativeRootOutsideView { artifact: id },
                        ));
                    }
                    (id, state.payloads[&id].native_group_ordinals.clone())
                }
                NativeRequirementRoot::Group {
                    artifact,
                    original_ordinal,
                } => (artifact, BTreeSet::from([original_ordinal])),
            };
            if !owned.contains(&InventoryNodeKey::Artifact(id)) {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeRootOutsideView { artifact: id },
                ));
            }
            for original_ordinal in ordinals {
                let key = NativeGroupKey {
                    artifact: id,
                    original_ordinal,
                };
                if !owned.contains(&InventoryNodeKey::Group(key)) {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::NativeGroupUnavailable {
                            artifact: id,
                            original_ordinal,
                        },
                    ));
                }
                pending.push(key);
            }
        }
        let mut seen = BTreeSet::new();
        let mut requirements = BTreeSet::new();
        let mut packages = BTreeSet::new();
        while let Some(key) = pending.pop() {
            if !seen.insert(key) {
                continue;
            }
            state.graph_visits.fetch_add(1, Ordering::Relaxed);
            for package in &state.payloads[&key.artifact].retained_packages {
                if key.original_ordinal == package.dependent_ordinal {
                    packages.insert(NativePackageRequirement {
                        artifact_id: key.artifact,
                        identity: package.identity.clone(),
                        generation: package.generation,
                        interface_digest: package.interface_digest,
                    });
                }
            }
            for edge in state
                .graph
                .edges(state.indices[&InventoryNodeKey::Group(key)])
            {
                let target = state.graph[edge.target()];
                match edge.weight() {
                    ArtifactDependency::NativeGroup { .. } => {
                        let InventoryNodeKey::Group(required) = target else {
                            return Err(failure("native group edge lacks group target"));
                        };
                        if !owned.contains(&target) {
                            return Err(failure("native dependency outside selected view"));
                        }
                        pending.push(required);
                    }
                    ArtifactDependency::NativeBinding {
                        generation,
                        namespace,
                        occurrence,
                        record_parent,
                        ..
                    } => {
                        let descriptor = &state.payloads[&target.artifact()].descriptor;
                        requirements.insert(NativeBindingRequirement {
                            artifact_id: descriptor.id,
                            identity: tidepool_repr::execution_schema::SymbolIdentity {
                                unit: descriptor.owner.unit.clone(),
                                module: descriptor.owner.module.clone(),
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

    fn roots(&self) -> &[InventoryNodeKey] {
        self.reads.canonical_roots.get_or_init(|| {
            let mut pending = vec![&self.lease];
            let mut roots = BTreeSet::new();
            let mut seen = BTreeSet::new();
            while let Some(lease) = pending.pop() {
                if seen.insert(Arc::as_ptr(lease)) {
                    #[cfg(test)]
                    self.reads
                        .root_parent_visits
                        .fetch_add(1, Ordering::Relaxed);
                    roots.extend(lease.roots.iter().copied());
                    pending.extend(lease.parents.iter());
                }
            }
            roots.into_iter().collect()
        })
    }
    /// Retain exactly these reachable artifact roots, independently of the
    /// source view's lifetime. Hidden dependencies remain graph-owned.
    pub fn select_roots(&self, roots: Vec<ArtifactId>) -> Result<Self, CompileError> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let owned = &self.read_projection(&state).nodes;
        if roots
            .iter()
            .any(|id| !owned.contains(&InventoryNodeKey::Artifact(*id)))
        {
            return Err(failure("selected artifact is outside retained view"));
        }
        let ids = roots.iter().copied().collect::<BTreeSet<_>>();
        let mut selected = roots
            .into_iter()
            .map(InventoryNodeKey::Artifact)
            .collect::<Vec<_>>();
        selected.extend(owned.iter().filter(|key| matches!(key, InventoryNodeKey::Group(group) if ids.contains(&group.artifact))).copied());
        drop(state);
        let mut materializations = Vec::new();
        if !selected.is_empty() {
            self.collect_materializations(&mut materializations, &mut BTreeSet::new());
        }
        Ok(self
            .lease
            .inventory
            .retain(selected, Vec::new(), materializations))
    }
    pub(crate) fn merge(&self, other: &Self) -> Result<Self, CompileError> {
        if Arc::ptr_eq(&self.lease, &other.lease) || other.is_empty() {
            return Ok(self.clone());
        }
        if self.is_empty() {
            return Ok(other.clone());
        }
        if Arc::ptr_eq(&self.lease.inventory.0, &other.lease.inventory.0) {
            let state = self.lease.inventory.0.lock().expect("inventory lock");
            let parent_nodes = &self.read_projection(&state).nodes;
            let parent_ids = artifact_ids(parent_nodes);
            let mut nodes = parent_nodes.clone();
            nodes.extend(other.read_projection(&state).nodes.iter().copied());
            let ids = artifact_ids(&nodes);
            let entries = ids
                .iter()
                .map(|id| (*id, Arc::clone(&state.payloads[id])))
                .collect();
            state
                .entry_handle_copies
                .fetch_add(ids.len() as u64, Ordering::Relaxed);
            SelectedOwners::new(&state, &entries, &parent_ids)?.planned_edges(
                &state,
                &entries,
                &native_groups(&nodes),
            )?;
            drop(state);
            Ok(ArtifactView::new(ViewLease {
                inventory: self.lease.inventory.clone(),
                roots: Vec::new(),
                parents: vec![Arc::clone(&self.lease), Arc::clone(&other.lease)],
                materialization_parents: Vec::new(),
                materialization: Mutex::new(BTreeMap::new()),
            }))
        } else {
            self.lease.inventory.admit_selected(
                self,
                other.entries(),
                other.selected_native_groups(),
                true,
                ArtifactRootIntent::RetainedView(other),
            )
        }
    }
    /// Reuse only this view's exact completed native carrier. Demand and graph
    /// closure remain the admission owner's responsibility for each consumer.
    pub(crate) fn retained_original_entry_with_validation(
        &self,
        producer: [u8; 32],
        product: &CertifiedRecoveryProduct,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Option<Arc<ArtifactEntry>>, CompileError> {
        let Some(native) = product.original_native() else {
            return Ok(None);
        };
        if !native.matches_original(product) {
            return Ok(None);
        }
        let descriptor = ArtifactEntry::original_descriptor(producer, product);
        let existing = {
            let state = self.lease.inventory.0.lock().expect("inventory lock");
            state.view_queries.fetch_add(1, Ordering::Relaxed);
            let retained = self
                .read_projection(&state)
                .entries
                .iter()
                .find(|entry| entry.descriptor.id == descriptor.id)
                .cloned();
            if retained.is_some() {
                state.entry_handle_copies.fetch_add(1, Ordering::Relaxed);
            }
            retained
        };
        let Some(existing) = existing else {
            return Ok(None);
        };
        let ArtifactPayload::Original(previous) = &existing.payload else {
            return Err(admission_failure(
                ArtifactInventoryFailure::MetadataConflict {
                    artifact: descriptor.id,
                },
            ));
        };
        if existing.descriptor != descriptor || !previous.same_durable_artifact(product) {
            return Err(admission_failure(
                ArtifactInventoryFailure::MetadataConflict {
                    artifact: descriptor.id,
                },
            ));
        }
        // Inventory equality deliberately excludes the fresh source witness.
        // A different source admission follows ordinary construction instead.
        if previous.source_sha256() != product.source_sha256() {
            return Ok(None);
        }
        let Some(previous_native) = previous.original_native() else {
            return Ok(None);
        };
        if !Arc::ptr_eq(native, previous_native) || !previous_native.matches_original(previous) {
            return Ok(None);
        }
        if product
            .module_interface()
            .is_none_or(|interface| interface.producer_sha256() != producer)
        {
            return Err(failure("native product has another canonical producer"));
        }
        crate::certified_products::original_execution_source_digest_with_validation(
            product, validation,
        )
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        Ok(Some(existing))
    }

    pub(crate) fn entries(&self) -> Vec<Arc<ArtifactEntry>> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        let entries = self.read_projection(&state).entries.clone();
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        entries
    }
    /// Explicitly retained roots, excluding their hidden dependency closure.
    pub(crate) fn root_entries(&self) -> Vec<Arc<ArtifactEntry>> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        let entries = self
            .roots()
            .iter()
            .copied()
            .map(InventoryNodeKey::artifact)
            .collect::<BTreeSet<_>>()
            .iter()
            .filter_map(|id| state.payloads.get(id).cloned())
            .collect::<Vec<_>>();
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        entries
    }
    #[cfg(test)]
    pub(crate) fn entries_for_owners(
        &self,
        owners: impl Iterator<Item = ExactModuleIdentity>,
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let owned = &self.read_projection(&state).nodes;
        let mut interfaces = BTreeMap::new();
        let mut native = BTreeMap::<ExactModuleIdentity, Vec<ArtifactId>>::new();
        for id in artifact_ids(owned) {
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
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let projection = self.read_projection(&state);
        let selected = projection
            .entries
            .iter()
            .filter_map(|entry| {
                (!entry.is_native())
                    .then_some((entry.descriptor.owner.clone(), entry.descriptor.id))
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
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        self.read_projection(&state)
            .entries
            .iter()
            .filter(|entry| !entry.is_native())
            .map(|entry| ExactInterfaceOwner {
                owner: entry.descriptor.owner.clone(),
                requirements: entry.requirements.clone(),
            })
            .collect()
    }
}
impl PartialEq for ArtifactView {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lease, &other.lease)
            || (self.entries() == other.entries()
                && self.selected_native_groups() == other.selected_native_groups())
    }
}
impl Eq for ArtifactView {}

#[cfg(test)]
mod native_history_properties;

#[cfg(test)]
mod compiler_projection_properties;

#[cfg(test)]
mod view_read_properties;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_requirement_index_refuses_interface_edges() {
        assert!(index_native_requirements(vec![(
            module("Support"),
            ArtifactDependency::Interface
        )])
        .is_err());
    }

    #[test]
    fn native_requirement_index_preserves_empty_sparse_and_overlapping_edges() {
        assert!(index_native_requirements(Vec::new()).unwrap().is_empty());
        let edge = (
            module("Helper"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: u32::MAX,
                required_ordinal: 7,
            },
        );
        let overlapping = (module("OtherHelper"), edge.1.clone());
        let indexed =
            index_native_requirements(vec![edge.clone(), overlapping.clone(), edge.clone()])
                .unwrap();
        assert_eq!(indexed.len(), 1);
        assert_eq!(
            indexed[&u32::MAX].iter().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([edge, overlapping])
        );
        assert_eq!(indexed[&u32::MAX].len(), 2);
    }

    #[test]
    fn native_requirement_planning_visits_only_selected_group_edges() {
        for count in [8u32, 64, 512] {
            let inventory = ArtifactInventory::default();
            let helper =
                issued_native_groups("Helper", vec![(7, Vec::new())], &[], &BTreeMap::new());
            let consumer = issued_native_groups(
                "Consumer",
                (0..count)
                    .map(|i| (3 + 8 * i, vec![issued_source(&helper, 7)]))
                    .collect(),
                &[&helper],
                &BTreeMap::new(),
            );
            let consumer_id = consumer.descriptor.id;
            let helper_key = NativeGroupKey {
                artifact: helper.descriptor.id,
                original_ordinal: 7,
            };
            let entries = vec![Arc::new(helper), Arc::new(consumer)];
            let selected = BTreeSet::from([
                helper_key,
                NativeGroupKey {
                    artifact: consumer_id,
                    original_ordinal: 3,
                },
            ]);
            let view = inventory
                .admit_recovery_selection(&inventory.empty_view(), entries, &selected)
                .unwrap();
            assert_eq!(view.selected_native_groups(), selected);
            let examined = || {
                inventory
                    .0
                    .lock()
                    .unwrap()
                    .native_requirement_rows_examined
                    .load(Ordering::Relaxed)
            };
            assert_eq!(
                examined(),
                1,
                "unused groups must not add edge scans at count={count}"
            );
            let all = (0..count)
                .map(|i| NativeGroupKey {
                    artifact: consumer_id,
                    original_ordinal: 3 + 8 * i,
                })
                .chain([helper_key])
                .collect::<BTreeSet<_>>();
            let before = examined();
            let expanded = inventory
                .admit_recovery_selection(&view, Vec::new(), &all)
                .unwrap();
            assert_eq!(expanded.selected_native_groups(), all);
            assert_eq!(
                examined() - before,
                u64::from(count),
                "whole selection visits each edge once"
            );
            let before = examined();
            let repeated = inventory
                .admit_recovery_selection(&expanded, Vec::new(), &all)
                .unwrap();
            assert_eq!(repeated, expanded);
            assert_eq!(
                examined() - before,
                u64::from(count),
                "known nodes still validate every selected edge"
            );
        }
    }

    #[test]
    fn cold_original_admission_distinguishes_supplied_rows_from_retained_vertices() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let original =
            issued_native_groups("ColdOriginal", vec![(3, Vec::new())], &[], &BTreeMap::new());
        let id = original.descriptor.id;
        let groups = original
            .native_group_ordinals
            .iter()
            .map(|ordinal| NativeGroupKey {
                artifact: id,
                original_ordinal: *ordinal,
            })
            .collect::<BTreeSet<_>>();
        assert!(!groups.is_empty());
        let planned = std::iter::once(InventoryNodeKey::Artifact(id))
            .chain(groups.iter().copied().map(InventoryNodeKey::Group));
        assert!(admitted_closure(&inventory.0.lock().unwrap(), planned).is_empty());
        let first = inventory.admit(&empty, vec![original.clone()]).unwrap();
        assert_eq!(first.selected_native_groups(), groups);
        assert!(first.artifact_ids().contains(&id));
        first
            .native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(id)])
            .unwrap();
        let node_count = inventory.node_count();
        let reused = inventory.admit(&first, vec![original]).unwrap();
        assert_eq!(reused, first);
        assert_eq!(inventory.node_count(), node_count);
        let missing = inventory
            .admit(&reused, vec![entry("NeedsMissing", &["Missing"])])
            .unwrap_err();
        assert!(
            matches!(missing, CompileError::ArtifactInventory(ref error) if matches!(error.failure, ArtifactInventoryFailure::MissingDependency { .. }))
        );
        assert_eq!(inventory.node_count(), node_count);
        assert_eq!(first.selected_native_groups(), groups);
    }

    #[test]
    fn admitted_closure_never_adds_unknown_artifact_or_group_seeds() {
        let inventory = ArtifactInventory::default();
        let original =
            issued_native_groups("Existing", vec![(11, Vec::new())], &[], &BTreeMap::new());
        let id = original.descriptor.id;
        let retained = inventory
            .admit(&inventory.empty_view(), vec![original])
            .unwrap();
        let unknown_id = ArtifactId([0xfe; 32]);
        let unknown_group = NativeGroupKey {
            artifact: id,
            original_ordinal: u32::MAX,
        };
        let state = inventory.0.lock().unwrap();
        let known = admitted_closure(&state, retained.roots().iter().copied());
        assert!(known.contains(&InventoryNodeKey::Group(NativeGroupKey {
            artifact: id,
            original_ordinal: 11
        })));
        let mixed = admitted_closure(
            &state,
            retained.roots().iter().copied().chain([
                InventoryNodeKey::Artifact(unknown_id),
                InventoryNodeKey::Group(unknown_group),
            ]),
        );
        assert_eq!(mixed, known);
        drop(state);
        assert!(retained.select_roots(vec![unknown_id]).is_err());
        assert!(retained
            .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: id,
                original_ordinal: u32::MAX,
            }])
            .is_err());
    }

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
        ArtifactEntry::original([2; 32], product).unwrap()
    }

    fn issued_native_groups(
        name: &str,
        groups: Vec<(u32, Vec<crate::certified_products::PendingImportOwner>)>,
        requirements: &[&ArtifactEntry],
        packages: &BTreeMap<(String, String), crate::certified_products::PackageInterfaceWitness>,
    ) -> ArtifactEntry {
        let product =
            crate::certified_products::tests::original_groups_fixture(name, groups, 1, packages);
        let requirements = requirements
            .iter()
            .map(|entry| {
                (
                    (
                        entry.descriptor.owner.unit.clone(),
                        entry.descriptor.owner.module.clone(),
                    ),
                    entry.descriptor.interface_sha256,
                )
            })
            .collect();
        ArtifactEntry::original(
            [2; 32],
            crate::certified_products::fixture_finalized_product_with_requirements(
                product,
                [2; 32],
                Some(requirements),
            ),
        )
        .unwrap()
    }

    fn issued_source(
        entry: &ArtifactEntry,
        ordinal: u32,
    ) -> crate::certified_products::PendingImportOwner {
        let ArtifactPayload::Original(product) = &entry.payload else {
            unreachable!()
        };
        let occurrence = if entry.native_group_ordinals.len() == 1 && ordinal == 7 {
            "entry".into()
        } else {
            format!("entry_{ordinal}")
        };
        crate::certified_products::PendingImportOwner::Source {
            owner: product.owner().clone(),
            original_ordinal: ordinal,
            binder: tidepool_repr::execution_schema::SymbolIdentity {
                unit: product.owner().unit.clone(),
                module: product.owner().module.clone(),
                namespace: "value".into(),
                occurrence,
                record_parent: None,
            },
        }
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
        conflicting_edges
            .native_requirements
            .entry(0)
            .or_default()
            .push((
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

    #[test]
    fn native_variants_keep_exact_carriers_and_require_explicit_materialization_selection() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let first = issued_native_groups("Original", vec![(0, Vec::new())], &[], &BTreeMap::new());
        let second = ArtifactEntry::original(
            [2; 32],
            crate::certified_products::fixture_finalized_product(
                crate::certified_products::tests::original_groups_fixture(
                    "Original",
                    vec![(0, Vec::new())],
                    2,
                    &BTreeMap::new(),
                ),
                [2; 32],
            ),
        )
        .unwrap();
        let original_owner = first.descriptor.owner.clone();
        let consumer = issued_native_groups(
            "Consumer",
            vec![(0, vec![issued_source(&second, 0)])],
            &[&second],
            &BTreeMap::new(),
        );
        let view = inventory
            .admit(
                &empty,
                vec![first.clone(), second.clone(), consumer.clone()],
            )
            .unwrap();
        let metadata = view.metadata_snapshot();
        assert_eq!(
            metadata.ambiguous_native_owners,
            BTreeSet::from([original_owner.clone()])
        );
        assert!(view
            .entries_for_owners(std::iter::once(original_owner.clone()))
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
                .entries_for_owners(std::iter::once(original_owner.clone()))
                .unwrap()[&original_owner]
                .descriptor
                .id,
            first.descriptor.id
        );
        let types = view.interface_projection(&[original_owner]).unwrap();
        assert_eq!(types.artifact_ids(), vec![carrier]);
        drop(view);
        assert_eq!(inventory.node_count(), 3);
        drop(selected);
        assert_eq!(inventory.node_count(), 1);
        drop(types);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn native_requirement_cannot_select_an_ambient_exact_native_key() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let original =
            issued_native_groups("Original", vec![(0, Vec::new())], &[], &BTreeMap::new());
        let ambient = inventory.admit(&empty, vec![original.clone()]).unwrap();
        let consumer = issued_native_groups(
            "Consumer",
            vec![(0, vec![issued_source(&original, 0)])],
            &[&original],
            &BTreeMap::new(),
        );
        let types = ambient
            .interface_projection(&[original.descriptor.owner.clone()])
            .unwrap();
        let before = inventory.node_count();
        assert!(inventory.admit(&types, vec![consumer.clone()]).is_err());
        assert_eq!(inventory.node_count(), before);
        assert!(types
            .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: original.descriptor.id,
                original_ordinal: 0,
            }])
            .is_err());
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
        // A warm read projection cannot replace reused-edge validation during
        // admission. The corrupted edge below violates the live-view invariant.
        retained.metadata_snapshot();
        // Exercise the owning admission guard against a corrupted reused graph.
        let mut state = inventory.0.lock().unwrap();
        let source = state.indices[&InventoryNodeKey::Artifact(consumer.descriptor.id)];
        let target = state.indices[&unrelated.lease.roots[0]];
        state
            .graph
            .add_edge(source, target, ArtifactDependency::Interface);
        drop(state);
        assert!(matches!(
            inventory.admit(&retained, vec![consumer.clone()]),
            Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::MetadataConflict { artifact }
                    if artifact == consumer.descriptor.id)
        ));
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
        assert!(Arc::ptr_eq(&view.lease, &unchanged.lease));
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
        let original =
            issued_native_groups("Original", vec![(7, Vec::new())], &[], &BTreeMap::new());
        let consumer = issued_native_groups(
            "Consumer",
            vec![(
                4,
                vec![
                    issued_source(&original, 7),
                    crate::certified_products::PendingImportOwner::Retained {
                        identity: tidepool_repr::execution_schema::SymbolIdentity {
                            unit: original.descriptor.owner.unit.clone(),
                            module: "Original".into(),
                            namespace: "value".into(),
                            occurrence: "kept".into(),
                            record_parent: None,
                        },
                        generation: 9,
                    },
                ],
            )],
            &[&original],
            &BTreeMap::new(),
        );
        let view = inventory
            .admit(&inventory.empty_view(), vec![consumer, original])
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
        assert_eq!(after.graph_visits - before.graph_visits, 0);
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
        let capture = |occurrence: &str, generation| {
            crate::certified_products::PendingImportOwner::Retained {
                identity: tidepool_repr::execution_schema::SymbolIdentity {
                    unit: value.descriptor.owner.unit.clone(),
                    module: "Val".into(),
                    namespace: "value".into(),
                    occurrence: occurrence.into(),
                    record_parent: None,
                },
                generation,
            }
        };
        let helper = issued_native_groups(
            "Helper",
            vec![(1, vec![capture("x", 7)]), (2, vec![capture("y", 8)])],
            &[&value],
            &BTreeMap::new(),
        );
        let consumer = issued_native_groups(
            "Consumer",
            vec![(0, vec![issued_source(&helper, 1)])],
            &[&type_user, &helper],
            &BTreeMap::new(),
        );
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
        let original =
            issued_native_groups("Original", vec![(7, Vec::new())], &[], &BTreeMap::new());
        let consumer = issued_native_groups(
            "Consumer",
            vec![(2, vec![issued_source(&original, 7)])],
            &[&original],
            &BTreeMap::new(),
        );
        let view = inventory.admit(&empty, vec![original, consumer]).unwrap();
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
        let original =
            issued_native_groups("Original", vec![(7, Vec::new())], &[], &BTreeMap::new());
        let consumer = issued_native_groups(
            "Consumer",
            vec![(2, vec![issued_source(&original, 7)])],
            &[&original],
            &BTreeMap::new(),
        );
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
        assert_eq!(
            entries[1].requirements,
            vec![original.descriptor.owner.clone()]
        );
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
            consumer.native_requirements[&2][0].1.clone(),
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
        let package_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(package_file.path(), b"native package interface").unwrap();
        let package_digest = digest(b"native package interface");
        let packages = BTreeMap::from([(
            ("ghc-internal".into(), "GHC.Internal.Base".into()),
            crate::certified_products::PackageInterfaceWitness {
                selected_path: package_file.path().to_path_buf(),
                sha256: package_digest,
            },
        )]);
        let package =
            |occurrence: &str| crate::certified_products::PendingImportOwner::RetainedPackage {
                unit: "ghc-internal".into(),
                module: "GHC.Internal.Base".into(),
                binder: tidepool_repr::execution_schema::SymbolIdentity {
                    unit: "ghc-internal".into(),
                    module: "GHC.Internal.Base".into(),
                    namespace: "value".into(),
                    occurrence: occurrence.into(),
                    record_parent: None,
                },
                generation: 0,
                interface_digest: package_digest,
            };
        let helper = issued_native_groups(
            "Helper",
            vec![(7, vec![package("map")]), (8, vec![package("foldr")])],
            &[],
            &packages,
        );
        let root = issued_native_groups(
            "Root",
            vec![(2, vec![issued_source(&helper, 7)])],
            &[],
            &BTreeMap::new(),
        );
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
        assert_eq!(inventory.metrics().nodes, 7);
    }
}
