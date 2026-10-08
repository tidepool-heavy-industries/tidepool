//! Final Rust admission of module products checked by the resident compiler.
//! Worker receipts describe GHC's selected owners; stored product bytes and
//! dependency witnesses remain independent inputs to this check.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ciborium::value::Value;
use sha2::{Digest, Sha256};
#[cfg(test)]
use tidepool_repr::execution_schema::parse_module_products;
use tidepool_repr::execution_schema::{
    CachedHomeOwner, GlobalDecl, InventoryOperation, ModuleVersion, PreparedProgram,
    ProjectedGroup, RawModuleProduct, ResultContract, RuntimeRep, Signature, SymbolIdentity,
};

mod finalized_module;
mod retained_core;
pub(crate) use finalized_module::{
    CanonicalOrigin, CanonicalSourceImport, CertifiedModuleInterface,
};
pub use finalized_module::{
    CapturedArtifactDescriptor, CapturedValueInterfaceReceipt, FinalizationEnvelope,
    FinalizedModuleReceipt,
};

use crate::cache::{CompletedSourceEvidence, DependencyEvidence, ProductAvailability};
use crate::module_candidates::CandidateSet;
use crate::recovery_artifacts::PackageInterfaceValidation;

/// Durable compiler records have an independent bound from scope descriptors
/// and canonical per-module certificates. Their reads and decoding still share
/// the inventory operation's cumulative accounting.
pub(crate) const COMPILER_RECEIPT_BYTES_LIMIT: usize = 32 << 20;
const PACKAGE_LIMIT: usize = 4096;
const PACKAGE_INTERFACE_LIMIT: u64 = 32 << 20;
const SOURCE_LIMIT: u64 = 32 << 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductOrigin {
    Fresh,
    Cached,
    /// Newly prepared native child of an inherited canonical source original.
    RetainedCore,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PendingImportOwner {
    Source {
        owner: CachedHomeOwner,
        original_ordinal: u32,
        binder: SymbolIdentity,
    },
    Retained {
        identity: SymbolIdentity,
        generation: u64,
    },
    RetainedPackage {
        unit: String,
        module: String,
        binder: SymbolIdentity,
        generation: u64,
        interface_digest: [u8; 32],
    },
    Package {
        unit: String,
        module: String,
        binder: SymbolIdentity,
        interface_digest: [u8; 32],
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReceiptImportOwner {
    Source {
        unit: String,
        module: String,
        module_version: Option<ModuleVersion>,
        original_ordinal: u32,
        binder: SymbolIdentity,
    },
    Retained {
        identity: SymbolIdentity,
        generation: u64,
    },
    RetainedPackage {
        unit: String,
        module: String,
        binder: SymbolIdentity,
        generation: u64,
        interface_digest: [u8; 32],
    },
    Package {
        unit: String,
        module: String,
        binder: SymbolIdentity,
        interface_digest: [u8; 32],
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedGlobal {
    pub identity: SymbolIdentity,
    pub rep: RuntimeRep,
    pub entry_signature: Option<Signature>,
    pub required_evaluated: bool,
    pub owner: ReceiptImportOwner,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedGroup {
    pub original_ordinal: u32,
    pub globals: Vec<AcceptedGlobal>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedModuleReceipt {
    pub origin: ProductOrigin,
    pub unit: String,
    pub module: String,
    pub module_version: Option<ModuleVersion>,
    pub skinny_iface_sha256: [u8; 32],
    pub product_sha256: [u8; 32],
    pub source_sha256: [u8; 32],
    pub dependency_witness_sha256: [u8; 32],
    pub groups: Vec<AcceptedGroup>,
    pub interface_requirements: BTreeMap<(String, String), [u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedReceipt {
    pub modules: Vec<CertifiedModuleReceipt>,
    pub targets: BTreeMap<String, Vec<AcceptedGlobal>>,
    pub packages: BTreeMap<(String, String), PackageInterfaceWitness>,
    pub finalization: FinalizationEnvelope,
    pub source_recipe: WorkerExecutionSource,
}

/// Untrusted worker result until the original product admission authenticates
/// its complete source domain. A recipe never grants native package authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerExecutionSource {
    Ordinary,
    ExactUnavailable(SourceRecipeUnavailable),
    ExactAvailable { digest: [u8; 32], bytes: Arc<[u8]> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRecipeUnavailable {
    NoFreshOriginals,
    IncompleteSourceEvidence,
    UnsupportedSourceRecipe,
    UnavailableSourceRoot,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageInterfaceWitness {
    pub selected_path: PathBuf,
    pub sha256: [u8; 32],
}

/// The selected package closure of one sealed target. A digest from another
/// transaction cannot authorize a local package definition in this target.
#[derive(Clone, Debug, Default)]
pub struct CertifiedTargetPackageInterfaces {
    target: Option<std::sync::Arc<PreparedProgram>>,
    interfaces: BTreeMap<(String, String), [u8; 32]>,
}

impl CertifiedTargetPackageInterfaces {
    pub(crate) fn matches_bundle(&self, other: &Self, target: &PreparedProgram) -> bool {
        self.matches_target(target)
            && other.matches_target(target)
            && self.interfaces == other.interfaces
    }

    pub fn matches_target(&self, target: &PreparedProgram) -> bool {
        self.target
            .as_deref()
            .is_some_and(|bound| std::ptr::eq(bound, target) || bound == target)
    }

    pub fn interface_digest(&self, unit: &str, module: &str) -> Option<[u8; 32]> {
        self.interfaces
            .get(&(unit.to_owned(), module.to_owned()))
            .copied()
    }
}

#[cfg(test)]
pub(crate) fn certify_target_package_interfaces(
    target: &std::sync::Arc<PreparedProgram>,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
) -> CertResult<CertifiedTargetPackageInterfaces> {
    certify_target_package_interfaces_with_validation(
        target,
        packages,
        &mut PackageInterfaceValidation::default(),
    )
}

pub(crate) fn certify_target_package_interfaces_with_validation(
    target: &std::sync::Arc<PreparedProgram>,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<CertifiedTargetPackageInterfaces> {
    let mut interfaces = BTreeMap::new();
    for (owner, witness) in packages {
        if owner.0.is_empty() || owner.1.is_empty() || !witness.selected_path.is_absolute() {
            return Err(CertificationError::Mismatch(
                "target package interface owner",
            ));
        }
        verify_package_interface(validation, witness)?;
        interfaces.insert(owner.clone(), witness.sha256);
    }
    Ok(CertifiedTargetPackageInterfaces {
        target: Some(target.clone()),
        interfaces,
    })
}

pub(crate) fn inherited_package_witnesses_with_validation(
    products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
    validation: &mut PackageInterfaceValidation,
) -> CertResult<BTreeMap<(String, String), PackageInterfaceWitness>> {
    let mut selected = BTreeMap::new();
    for product in products {
        let decoded;
        let packages = match product.original_native() {
            Some(witness) if witness.matches_original(product) => &witness.packages,
            Some(_) => {
                return Err(CertificationError::Mismatch(
                    "original native witness bytes",
                ));
            }
            None => {
                decoded = decode_home_witness_with_operation(
                    product.certification_bytes(),
                    &validation.inventory,
                )?;
                if &decoded.owner != product.owner() {
                    return Err(CertificationError::Mismatch(
                        "inherited package product owner",
                    ));
                }
                &decoded.packages
            }
        };
        for (owner, interface) in packages {
            verify_package_interface(validation, interface)?;
            match selected.get(owner) {
                Some(old) if old != interface => {
                    return Err(CertificationError::Mismatch("inherited package selection"));
                }
                Some(_) => continue,
                None => {}
            }
            // The returned selection owns its containers; the issued witness
            // retains immutable native facts under the original byte anchors.
            validation
                .inventory
                .reserve::<((String, String), PackageInterfaceWitness, [usize; 4])>(1)?;
            validation.inventory.charge(owner.0.len())?;
            validation.inventory.charge(owner.1.len())?;
            validation
                .inventory
                .charge(interface.selected_path.as_os_str().as_encoded_bytes().len())?;
            selected.insert(owner.clone(), interface.clone());
        }
    }
    Ok(selected)
}

/// A group whose retained globals still need an exact live binding or native
/// export owner. Runtime resolves those through its binding table or owning
/// machine's export ledger before constructing `CertifiedGroup`. Immutable
/// decoded payloads are shared when request contexts append native groups.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingCertifiedGroup {
    origin: ProductOrigin,
    owner: CachedHomeOwner,
    group: Arc<ProjectedGroup>,
    imports: Arc<[PendingImportOwner]>,
}

pub(crate) struct CertifiedProducts {
    pub groups: Vec<PendingCertifiedGroup>,
    pub recovery_products: Vec<crate::recovery_artifacts::CertifiedRecoveryProduct>,
    pub module_interfaces: Vec<CertifiedModuleInterface>,
    pub value_interfaces: Vec<crate::recovery_artifacts::CertifiedValueInterface>,
    pub retained_core_products: CertifiedRetainedCoreProducts,
    pub(crate) source_selection: CertifiedSourceSelection,
}

/// Assembly authority issued only after complete native promotion certification.
/// Empty values carry no authority; the nonempty inventory remains private.
#[derive(Debug, Default)]
pub(crate) struct CertifiedRetainedCoreProducts {
    products: BTreeMap<(String, String), crate::recovery_artifacts::CertifiedRecoveryProduct>,
}

impl CertifiedRetainedCoreProducts {
    pub(crate) fn contains_original(
        &self,
        original: &crate::recovery_artifacts::CertifiedRecoveryProduct,
    ) -> bool {
        self.products
            .get(&(
                original.owner().unit.clone(),
                original.owner().module.clone(),
            ))
            .is_some_and(|admitted| {
                admitted
                    .original_native()
                    .is_some_and(|native| native.matches_original(original))
            })
    }

    pub(crate) fn matches_emitted(&self, emitted: &RawModuleProduct) -> Option<bool> {
        let product = self
            .products
            .get(&(emitted.unit.clone(), emitted.module.clone()))?;
        Some(product.original_native().is_some_and(|native| {
            native.matches_original(product)
                && emitted.interface == product.interface_bytes()
                && emitted.groups.len() == native.groups.len()
                && emitted
                    .groups
                    .iter()
                    .zip(native.groups.iter())
                    .all(|(emitted, original)| emitted == original.group())
        }))
    }
}

impl PendingCertifiedGroup {
    pub fn origin(&self) -> ProductOrigin {
        self.origin
    }
    pub fn owner(&self) -> &CachedHomeOwner {
        &self.owner
    }

    pub fn group(&self) -> &ProjectedGroup {
        &self.group
    }

    pub fn imports(&self) -> &[PendingImportOwner] {
        &self.imports
    }

    pub fn into_parts(self) -> (CachedHomeOwner, ProjectedGroup, Vec<PendingImportOwner>) {
        (
            self.owner,
            Arc::unwrap_or_clone(self.group),
            self.imports.to_vec(),
        )
    }
}

/// Complete local product facts authenticated from original bytes and headers.
/// External group dependencies remain declarative until contextual admission.
#[derive(Clone, Debug, Eq, PartialEq)]
struct AuthenticatedOriginalGroup {
    owner: CachedHomeOwner,
    group: Arc<ProjectedGroup>,
    imports: Arc<[PendingImportOwner]>,
}

impl AuthenticatedOriginalGroup {
    fn group(&self) -> &ProjectedGroup {
        &self.group
    }

    fn imports(&self) -> &[PendingImportOwner] {
        &self.imports
    }

    /// The caller has checked this group's selected dependency closure.
    fn admitted(&self) -> PendingCertifiedGroup {
        PendingCertifiedGroup {
            origin: ProductOrigin::Cached,
            owner: self.owner.clone(),
            group: Arc::clone(&self.group),
            imports: Arc::clone(&self.imports),
        }
    }
}

/// Native facts authenticated from these exact immutable original bytes. Construction
/// stays in the issuer and original recovery authentication; selected contexts
/// check exact group closure and current package evidence before reuse.
#[derive(Debug)]
pub(crate) struct OriginalNativeWitness {
    owner: CachedHomeOwner,
    execution_source_sha256: Option<[u8; 32]>,
    anchors: [Arc<[u8]>; 4],
    groups: Arc<[AuthenticatedOriginalGroup]>,
    group_positions: BTreeMap<u32, usize>,
    sources: Vec<CachedHomeOwner>,
    interface_requirements: BTreeMap<(String, String), [u8; 32]>,
    packages: BTreeMap<(String, String), PackageInterfaceWitness>,
    native_requirements: CertifiedNativeRequirements,
}

impl OriginalNativeWitness {
    fn group(&self, original_ordinal: u32) -> Option<&AuthenticatedOriginalGroup> {
        self.group_positions
            .get(&original_ordinal)
            .map(|position| &self.groups[*position])
    }

    fn validate_promoted_groups(
        &self,
        current: &[(&ProjectedGroup, Vec<PendingImportOwner>)],
    ) -> CertResult<()> {
        if current.len() != self.groups.len() {
            return Err(original_group_conflict(
                &self.owner,
                OriginalGroupFailure::PromotionCensus {
                    current: current.len(),
                    original: self.groups.len(),
                },
            ));
        }
        let mut ordinals = BTreeSet::new();
        for (group, imports) in current {
            if !ordinals.insert(group.original_ordinal()) {
                return Err(original_group_conflict(
                    &self.owner,
                    OriginalGroupFailure::PromotionDuplicateOrdinal {
                        ordinal: group.original_ordinal(),
                    },
                ));
            }
            let original = self.group(group.original_ordinal()).ok_or_else(|| {
                original_group_conflict(
                    &self.owner,
                    OriginalGroupFailure::PromotionMissingOrdinal {
                        ordinal: group.original_ordinal(),
                    },
                )
            })?;
            if original.group() != *group {
                return Err(original_group_conflict(
                    &self.owner,
                    OriginalGroupFailure::PromotionBody {
                        ordinal: group.original_ordinal(),
                    },
                ));
            }
            if original.imports() != imports {
                return Err(original_group_conflict(
                    &self.owner,
                    OriginalGroupFailure::PromotionImports {
                        ordinal: group.original_ordinal(),
                    },
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn matches_original(
        &self,
        product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
    ) -> bool {
        self.owner == *product.owner()
            && self
                .anchors
                .iter()
                .zip(product.original_byte_anchors())
                .all(|(expected, actual)| Arc::ptr_eq(expected, actual))
    }
}

/// Authenticate local entry membership without admitting executable dependencies.
/// Checked-entry issuers use this census before the inventory selects closure.
pub(crate) fn authenticates_original_native_entry(
    product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
    original_ordinal: u32,
    binder: &SymbolIdentity,
) -> bool {
    product.original_native().is_some_and(|witness| {
        witness.matches_original(product)
            && binder.unit == witness.owner.unit
            && binder.module == witness.owner.module
            && witness
                .group(original_ordinal)
                .is_some_and(|group| group.group.binders().contains(binder))
    })
}

fn retain_original_native(
    product: crate::recovery_artifacts::CertifiedRecoveryProduct,
    groups: Vec<PendingCertifiedGroup>,
    witness: HomeCertification,
) -> CertResult<crate::recovery_artifacts::CertifiedRecoveryProduct> {
    retain_authenticated_original_native(
        product,
        groups
            .into_iter()
            .map(|group| AuthenticatedOriginalGroup {
                owner: group.owner,
                group: group.group,
                imports: group.imports,
            })
            .collect(),
        witness,
    )
}

fn retain_authenticated_original_native(
    product: crate::recovery_artifacts::CertifiedRecoveryProduct,
    groups: Vec<AuthenticatedOriginalGroup>,
    witness: HomeCertification,
) -> CertResult<crate::recovery_artifacts::CertifiedRecoveryProduct> {
    if product.owner() != &witness.owner
        || groups.iter().any(|group| &group.owner != product.owner())
    {
        return Err(CertificationError::Mismatch(
            "original native witness owner",
        ));
    }
    let mut group_positions = BTreeMap::new();
    for (position, group) in groups.iter().enumerate() {
        if group_positions
            .insert(group.group.original_ordinal(), position)
            .is_some()
        {
            return Err(CertificationError::Mismatch(
                "duplicate original native ordinal",
            ));
        }
    }
    let native_requirements = native_requirements_from_witness(&witness);
    let facts = Arc::new(OriginalNativeWitness {
        owner: witness.owner,
        execution_source_sha256: witness.execution_source_sha256,
        anchors: product.original_byte_anchors().map(Arc::clone),
        groups: groups.into(),
        group_positions,
        sources: witness.sources.into_values().collect(),
        interface_requirements: witness.interface_requirements,
        packages: witness.packages,
        native_requirements,
    });
    product
        .with_original_native(facts)
        .map_err(|_| CertificationError::Mismatch("original native witness bytes"))
}

#[cfg(test)]
thread_local! {
    pub(crate) static ORIGINAL_PRODUCT_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static HOME_CERTIFICATION_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertificationFormat {
    ProductReceipt,
    HomeOwners,
    CanonicalModuleCertificate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceReadOperation {
    Metadata,
    Open,
    Read,
}

#[derive(Debug, thiserror::Error)]
pub enum EvidenceReadFailure {
    #[error("path is not absolute")]
    NonAbsolutePath,
    #[error("path is not a regular file")]
    NotFile,
    #[error("size {actual} exceeds {limit} bytes")]
    SizeLimit { actual: u64, limit: u64 },
    #[error("{operation:?} failed: {error}")]
    Io {
        operation: EvidenceReadOperation,
        #[source]
        error: std::io::Error,
    },
    #[error("length changed during read: expected {expected}, read {actual} bytes")]
    LengthChanged { expected: u64, actual: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceBinderPhase {
    NativePromotion,
    SelectedGroups,
    InheritedNativeWitness,
    InheritedInventory,
    CurrentReceipt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceBinderConflict {
    pub phase: SourceBinderPhase,
    pub owner: CachedHomeOwner,
    pub original_ordinal: u32,
    pub binder: SymbolIdentity,
    pub existing_origin: ProductOrigin,
    pub incoming_origin: ProductOrigin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OriginalGroupFailure {
    PromotionCensus {
        current: usize,
        original: usize,
    },
    PromotionDuplicateOrdinal {
        ordinal: u32,
    },
    PromotionMissingOrdinal {
        ordinal: u32,
    },
    PromotionBody {
        ordinal: u32,
    },
    PromotionImports {
        ordinal: u32,
    },
    SelectionOverlap {
        ordinal: u32,
        current_origin: ProductOrigin,
        inherited_origin: ProductOrigin,
        body_matches: bool,
        imports_match: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OriginalGroupConflict {
    pub owner: CachedHomeOwner,
    pub failure: OriginalGroupFailure,
}

fn original_group_conflict(
    owner: &CachedHomeOwner,
    failure: OriginalGroupFailure,
) -> CertificationError {
    CertificationError::OriginalGroupConflict(Box::new(OriginalGroupConflict {
        owner: owner.clone(),
        failure,
    }))
}

#[derive(Debug, thiserror::Error)]
pub enum CertificationError {
    #[error("unsupported {format:?} version {found}; expected {expected}")]
    UnsupportedVersion {
        format: CertificationFormat,
        found: u64,
        expected: u64,
    },
    #[error("{format:?} size {actual} exceeds {limit} bytes")]
    SizeLimit {
        format: CertificationFormat,
        actual: usize,
        limit: usize,
    },
    #[error("malformed bounded compiler product receipt: {0}")]
    Receipt(&'static str),
    #[error("compiler product certificate disagrees with {0}")]
    Mismatch(&'static str),
    #[error("compiler product certificate duplicate source binder: {0:?}")]
    DuplicateSourceBinder(Box<SourceBinderConflict>),
    #[error("compiler product original group conflict: {0:?}")]
    OriginalGroupConflict(Box<OriginalGroupConflict>),
    #[error("finalized module payload capture failed: {0}")]
    CapturedModulePayload(#[source] crate::recovery_artifacts::RecoveryArtifactError),
    #[error("finalized interface {unit}:{module} requires {required_unit}:{required_module} seal {expected_sha256}; selected seal {selected_sha256:?}")]
    FinalizedInterfaceRequirement {
        unit: String,
        module: String,
        required_unit: String,
        required_module: String,
        expected_sha256: String,
        selected_sha256: Option<String>,
    },
    #[error("compiler product certificate interface closure mismatch: {owner:?} requires {dependency:?} at {expected}, admitted {actual:?}")]
    OriginalInterfaceClosure {
        owner: (String, String),
        dependency: (String, String),
        expected: String,
        actual: Option<String>,
    },
    #[error("compiler product evidence is no longer valid")]
    StaleEvidence,
    #[error("compiler evidence read {}: {failure}", path.display())]
    EvidenceRead {
        path: PathBuf,
        #[source]
        failure: EvidenceReadFailure,
    },
    #[error("original candidate evidence for {unit}:{module} failed: {failure:?}")]
    CandidateEvidence {
        unit: String,
        module: String,
        failure: Box<crate::cache::DependencyEvidenceFailure>,
    },
    #[error("compiler execution source proof rejected: {0}")]
    ExecutionSource(#[source] Box<crate::CompileError>),
    #[error("invalid original module product: {0}")]
    Product(#[from] tidepool_repr::execution_schema::ParseError),
}

type CertResult<T> = Result<T, CertificationError>;

fn array(value: &Value) -> CertResult<&[Value]> {
    match value {
        Value::Array(items) => Ok(items),
        _ => Err(CertificationError::Receipt("expected array")),
    }
}

fn sized<'a>(value: &'a Value, size: usize) -> CertResult<&'a [Value]> {
    let items = array(value)?;
    if items.len() != size {
        return Err(CertificationError::Receipt("wrong array arity"));
    }
    Ok(items)
}

fn string(value: &Value) -> CertResult<&str> {
    match value {
        Value::Text(value) => Ok(value),
        _ => Err(CertificationError::Receipt("expected text")),
    }
}

fn number(value: &Value) -> CertResult<u64> {
    match value {
        Value::Integer(value) => u64::try_from(*value)
            .map_err(|_| CertificationError::Receipt("expected unsigned integer")),
        _ => Err(CertificationError::Receipt("expected unsigned integer")),
    }
}

fn boolean(value: &Value) -> CertResult<bool> {
    match value {
        Value::Bool(value) => Ok(*value),
        _ => Err(CertificationError::Receipt("expected bool")),
    }
}

fn digest(value: &Value) -> CertResult<[u8; 32]> {
    let encoded = string(value)?.as_bytes();
    if encoded.len() != 64 {
        return Err(CertificationError::Receipt("digest is not SHA-256 hex"));
    }
    let mut decoded = [0; 32];
    for (index, pair) in encoded.chunks_exact(2).enumerate() {
        let hi = (pair[0] as char)
            .to_digit(16)
            .ok_or(CertificationError::Receipt("invalid SHA-256 hex"))?;
        let lo = (pair[1] as char)
            .to_digit(16)
            .ok_or(CertificationError::Receipt("invalid SHA-256 hex"))?;
        decoded[index] = ((hi << 4) | lo) as u8;
    }
    Ok(decoded)
}

fn optional_version(value: &Value) -> CertResult<Option<ModuleVersion>> {
    match value {
        Value::Null => Ok(None),
        value => Ok(Some(ModuleVersion(digest(value)?))),
    }
}

fn identity(value: &Value) -> CertResult<SymbolIdentity> {
    let row = sized(value, 5)?;
    Ok(SymbolIdentity {
        unit: string(&row[0])?.to_owned(),
        module: string(&row[1])?.to_owned(),
        namespace: string(&row[2])?.to_owned(),
        occurrence: string(&row[3])?.to_owned(),
        record_parent: match &row[4] {
            Value::Null => None,
            value => Some(string(value)?.to_owned()),
        },
    })
}

fn rep(value: &Value) -> CertResult<RuntimeRep> {
    let row = sized(value, 2)?;
    let bits = u8::try_from(number(&row[1])?)
        .map_err(|_| CertificationError::Receipt("representation width"))?;
    let rep = match (string(&row[0])?, bits) {
        ("void", 0) => RuntimeRep::Void,
        ("lifted", 0) => RuntimeRep::LiftedRef,
        ("unlifted", 0) => RuntimeRep::UnliftedRef,
        ("address", 0) => RuntimeRep::Address,
        ("int", bits) => RuntimeRep::Int(bits),
        ("word", bits) => RuntimeRep::Word(bits),
        ("float", bits) => RuntimeRep::Float(bits),
        _ => return Err(CertificationError::Receipt("representation tag")),
    };
    Ok(rep)
}

fn signature(value: &Value) -> CertResult<Option<Signature>> {
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    let row = sized(value, 2)?;
    let arguments = array(&row[0])?.iter().map(rep).collect::<CertResult<_>>()?;
    let result = sized(&row[1], 2)?;
    let returned = array(&result[1])?;
    let results = match string(&result[0])? {
        "returns" => ResultContract::Returns(returned.iter().map(rep).collect::<CertResult<_>>()?),
        "no_success" if returned.is_empty() => ResultContract::NoSuccess,
        "caller_result" if returned.is_empty() => ResultContract::CallerResult,
        _ => return Err(CertificationError::Receipt("result contract")),
    };
    Ok(Some(Signature { arguments, results }))
}

fn owner(value: &Value) -> CertResult<ReceiptImportOwner> {
    let row = array(value)?;
    let Some(tag) = row.first() else {
        return Err(CertificationError::Receipt("empty import owner"));
    };
    match string(tag)? {
        "source" => {
            if row.len() != 6 {
                return Err(CertificationError::Receipt("source owner arity"));
            }
            Ok(ReceiptImportOwner::Source {
                unit: string(&row[1])?.to_owned(),
                module: string(&row[2])?.to_owned(),
                module_version: optional_version(&row[3])?,
                original_ordinal: u32::try_from(number(&row[4])?)
                    .map_err(|_| CertificationError::Receipt("source group ordinal"))?,
                binder: identity(&row[5])?,
            })
        }
        "retained" => {
            if row.len() != 3 {
                return Err(CertificationError::Receipt("retained owner arity"));
            }
            Ok(ReceiptImportOwner::Retained {
                identity: identity(&row[1])?,
                generation: number(&row[2])?,
            })
        }
        "retained-package" => {
            if row.len() != 6 {
                return Err(CertificationError::Receipt("retained package owner arity"));
            }
            Ok(ReceiptImportOwner::RetainedPackage {
                unit: string(&row[1])?.to_owned(),
                module: string(&row[2])?.to_owned(),
                interface_digest: digest(&row[3])?,
                binder: identity(&row[4])?,
                generation: number(&row[5])?,
            })
        }
        "package" => {
            if row.len() != 5 {
                return Err(CertificationError::Receipt("package owner arity"));
            }
            Ok(ReceiptImportOwner::Package {
                unit: string(&row[1])?.to_owned(),
                module: string(&row[2])?.to_owned(),
                interface_digest: digest(&row[3])?,
                binder: identity(&row[4])?,
            })
        }
        _ => Err(CertificationError::Receipt("import owner tag")),
    }
}

fn accepted_global(value: &Value) -> CertResult<AcceptedGlobal> {
    let row = sized(value, 5)?;
    Ok(AcceptedGlobal {
        identity: identity(&row[0])?,
        rep: rep(&row[1])?,
        entry_signature: signature(&row[2])?,
        required_evaluated: boolean(&row[3])?,
        owner: owner(&row[4])?,
    })
}

fn validate_global_witness(
    declaration: &GlobalDecl,
    signatures: &[Signature],
    selected: &AcceptedGlobal,
) -> CertResult<ReceiptImportOwner> {
    let actual_signature = declaration
        .entry_signature
        .and_then(|id| signatures.get(id.0 as usize))
        .cloned();
    if declaration.identity != selected.identity
        || declaration.rep != selected.rep
        || declaration.required_evaluated != selected.required_evaluated
        || actual_signature != selected.entry_signature
    {
        return Err(CertificationError::Mismatch(
            "global representation/signature",
        ));
    }
    let aligned = match &selected.owner {
        ReceiptImportOwner::Retained {
            identity,
            generation,
        } => {
            identity == &declaration.identity
                && declaration.required_generation == Some(*generation)
        }
        ReceiptImportOwner::RetainedPackage {
            unit,
            module,
            binder,
            generation,
            ..
        } => {
            binder == &declaration.identity
                && &binder.unit == unit
                && &binder.module == module
                && declaration.required_generation == Some(*generation)
        }
        ReceiptImportOwner::Source { binder, .. } => {
            binder == &declaration.identity && declaration.required_generation.is_none()
        }
        ReceiptImportOwner::Package {
            unit,
            module,
            binder,
            ..
        } => {
            binder == &declaration.identity
                && &binder.unit == unit
                && &binder.module == module
                && declaration.required_generation.is_none()
        }
    };
    if !aligned {
        return Err(CertificationError::Mismatch("global owner"));
    }
    Ok(selected.owner.clone())
}

/// Decode the worker's bounded `TPCERT9` tuple with exact owner rows shared
/// through an immutable dictionary. Older ownership formats are refused.
/// Original groups and executable targets preserve their ordered witnesses.
pub fn decode_receipt(bytes: &[u8]) -> CertResult<CertifiedReceipt> {
    decode_receipt_in(bytes, None)
}

pub(crate) fn decode_receipt_in(
    bytes: &[u8],
    output_dir: Option<&Path>,
) -> CertResult<CertifiedReceipt> {
    decode_receipt_with_operation(
        bytes,
        output_dir,
        &InventoryOperation::new(Default::default()),
    )
}

pub(crate) fn decode_receipt_with_operation(
    bytes: &[u8],
    output_dir: Option<&Path>,
    operation: &InventoryOperation,
) -> CertResult<CertifiedReceipt> {
    decode_receipt_packet_with_operation(bytes, output_dir, operation)
        .map(|decoded| decoded.receipt)
}

struct DecodedReceiptPacket {
    receipt: CertifiedReceipt,
    #[cfg(test)]
    globals: Vec<AcceptedGlobal>,
}

#[cfg(test)]
pub(crate) fn fixture_decoded_receipt(
    bytes: &[u8],
    output_dir: Option<&Path>,
) -> CertResult<(
    CertifiedReceipt,
    Vec<AcceptedGlobal>,
    BTreeMap<String, Vec<usize>>,
    Vec<[u8; 32]>,
)> {
    let decoded = decode_receipt_packet_in(bytes, output_dir)?;
    let key = |global: &AcceptedGlobal| -> CertResult<Vec<u8>> {
        let mut bytes = vec![];
        ciborium::ser::into_writer(&value_global(global), &mut bytes)
            .map_err(|_| CertificationError::Receipt("global dictionary encoding"))?;
        Ok(bytes)
    };
    let keys = decoded
        .globals
        .iter()
        .map(key)
        .collect::<CertResult<Vec<_>>>()?;
    let global_sha256 = keys.iter().map(|bytes| sha(bytes)).collect();
    let indices = keys
        .into_iter()
        .enumerate()
        .map(|(index, key)| (key, index))
        .collect::<BTreeMap<_, _>>();
    let references = decoded
        .receipt
        .targets
        .iter()
        .map(|(name, globals)| {
            Ok((
                name.clone(),
                globals
                    .iter()
                    .map(|global| {
                        indices
                            .get(&key(global)?)
                            .copied()
                            .ok_or(CertificationError::Receipt("global dictionary index"))
                    })
                    .collect::<CertResult<Vec<_>>>()?,
            ))
        })
        .collect::<CertResult<_>>()?;
    Ok((decoded.receipt, decoded.globals, references, global_sha256))
}

#[cfg(test)]
fn decode_receipt_packet_in(
    bytes: &[u8],
    output_dir: Option<&Path>,
) -> CertResult<DecodedReceiptPacket> {
    decode_receipt_packet_with_operation(
        bytes,
        output_dir,
        &InventoryOperation::new(Default::default()),
    )
}

fn decode_receipt_packet_with_operation(
    bytes: &[u8],
    output_dir: Option<&Path>,
    operation: &InventoryOperation,
) -> CertResult<DecodedReceiptPacket> {
    let limit = operation.limits().max_bytes;
    if bytes.len() > limit {
        return Err(CertificationError::SizeLimit {
            format: CertificationFormat::ProductReceipt,
            actual: bytes.len(),
            limit,
        });
    }
    let value = operation.decode_value(bytes, limit)?;
    operation.charge_value_copies(&value, 3)?;
    decode_receipt_value_in(&value, output_dir, operation)
}

fn decode_receipt_value_in(
    value: &Value,
    output_dir: Option<&Path>,
    operation: &InventoryOperation,
) -> CertResult<DecodedReceiptPacket> {
    let header = array(value)?;
    if header.len() < 2 || string(&header[0])? != "TPCERT" {
        return Err(CertificationError::Receipt("receipt header"));
    }
    let version = number(&header[1])?;
    if version != 9 {
        return Err(CertificationError::UnsupportedVersion {
            format: CertificationFormat::ProductReceipt,
            found: version,
            expected: 9,
        });
    }
    if header.len() != 9 {
        return Err(CertificationError::Receipt("receipt header"));
    }
    let mut coordinates = OwnerCoordinates::decode(&header[8])?;
    let mut dictionary =
        GlobalDictionary::decode_with_operation(&header[5], &mut coordinates, operation)?;
    let mut read_globals = |value: &Value| {
        let globals = array(value)?;
        dictionary.resolve_with_operation(globals, operation)
    };
    let modules = array(&header[2])?;
    let modules = modules
        .iter()
        .map(|module| {
            let row = sized(module, 10)?;
            let origin = match string(&row[0])? {
                "fresh" => ProductOrigin::Fresh,
                "cached" => ProductOrigin::Cached,
                "retained-core" => ProductOrigin::RetainedCore,
                _ => return Err(CertificationError::Receipt("product origin")),
            };
            let groups = array(&row[8])?;
            let groups = groups
                .iter()
                .map(|group| {
                    let row = sized(group, 2)?;
                    Ok(AcceptedGroup {
                        original_ordinal: u32::try_from(number(&row[0])?)
                            .map_err(|_| CertificationError::Receipt("group ordinal"))?,
                        globals: read_globals(&row[1])?,
                    })
                })
                .collect::<CertResult<_>>()?;
            Ok(CertifiedModuleReceipt {
                origin,
                unit: string(&row[1])?.to_owned(),
                module: string(&row[2])?.to_owned(),
                module_version: optional_version(&row[3])?,
                skinny_iface_sha256: digest(&row[5])?,
                product_sha256: digest(&row[6])?,
                source_sha256: digest(&row[4])?,
                dependency_witness_sha256: digest(&row[7])?,
                groups,
                interface_requirements: decode_interface_requirements(&row[9])?,
            })
        })
        .collect::<CertResult<Vec<_>>>()?;
    let target_rows = array(&header[3])?;
    let mut targets = BTreeMap::new();
    for target in target_rows {
        let row = sized(target, 2)?;
        let name = string(&row[0])?.to_owned();
        if name.is_empty() {
            return Err(CertificationError::Receipt("empty target name"));
        }
        if targets.insert(name, read_globals(&row[1])?).is_some() {
            return Err(CertificationError::Receipt("duplicate target"));
        }
    }
    if dictionary.used.len() != dictionary.rows.len() {
        return Err(CertificationError::Receipt(
            "unreferenced global dictionary row",
        ));
    }
    if coordinates.used.len() != coordinates.rows.len() {
        return Err(CertificationError::Receipt("unreferenced owner coordinate"));
    }

    let package_rows = array(&header[4])?;
    if package_rows.len() > PACKAGE_LIMIT {
        return Err(CertificationError::Receipt("package count"));
    }
    let mut packages = BTreeMap::new();
    for package in package_rows {
        let row = sized(package, 4)?;
        let unit = string(&row[0])?.to_owned();
        let module = string(&row[1])?.to_owned();
        let selected_path = PathBuf::from(string(&row[2])?);
        if unit.is_empty() || module.is_empty() || !selected_path.is_absolute() {
            return Err(CertificationError::Receipt("package witness identity/path"));
        }
        if packages
            .insert(
                (unit, module),
                PackageInterfaceWitness {
                    selected_path,
                    sha256: digest(&row[3])?,
                },
            )
            .is_some()
        {
            return Err(CertificationError::Receipt("duplicate package witness"));
        }
    }
    let recipe = array(&header[7])?;
    let source_recipe = match recipe.first().map(string).transpose()? {
        Some("ordinary") if recipe.len() == 1 => WorkerExecutionSource::Ordinary,
        Some("exact-unavailable") if recipe.len() == 2 => {
            let reason = match string(&recipe[1])? {
                "no-fresh-originals" => SourceRecipeUnavailable::NoFreshOriginals,
                "incomplete-source-evidence" => SourceRecipeUnavailable::IncompleteSourceEvidence,
                "unsupported-source-recipe" => SourceRecipeUnavailable::UnsupportedSourceRecipe,
                "unavailable-source-root" => SourceRecipeUnavailable::UnavailableSourceRoot,
                _ => {
                    return Err(CertificationError::Receipt(
                        "source recipe unavailable reason",
                    ))
                }
            };
            WorkerExecutionSource::ExactUnavailable(reason)
        }
        Some("exact-available") if recipe.len() == 2 => {
            use std::io::Read;
            let directory =
                output_dir.ok_or(CertificationError::Receipt("source recipe output owner"))?;
            let expected = digest(&recipe[1])?;
            let file = std::fs::File::open(directory.join("execution-source.cbor"))
                .map_err(|_| CertificationError::Receipt("source recipe unavailable"))?;
            let mut bytes = Vec::new();
            file.take((crate::execution_source::GRAPH_BYTES_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| CertificationError::Receipt("source recipe read"))?;
            if bytes.len() > crate::execution_source::GRAPH_BYTES_LIMIT || sha(&bytes) != expected {
                return Err(CertificationError::Receipt("source recipe digest or bound"));
            }
            WorkerExecutionSource::ExactAvailable {
                digest: expected,
                bytes: bytes.into(),
            }
        }
        _ => return Err(CertificationError::Receipt("source recipe result")),
    };
    let finalization = finalized_module::decode_envelope_with_operation(&header[6], operation)?;
    finalization.validate_owners(&modules, &packages)?;
    Ok(DecodedReceiptPacket {
        receipt: CertifiedReceipt {
            modules,
            targets,
            packages,
            finalization,
            source_recipe,
        },
        #[cfg(test)]
        globals: dictionary
            .rows
            .into_iter()
            .map(|(global, _)| global)
            .collect(),
    })
}

// Coordinates name an owning source group or package interface; they never
// substitute for the exact global symbol. Only the binder is shared with it.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
enum OwnerCoordinate {
    Source {
        unit: String,
        module: String,
        module_version: Option<ModuleVersion>,
    },
    Package {
        unit: String,
        module: String,
        interface_digest: [u8; 32],
    },
}

struct OwnerCoordinates {
    rows: Vec<OwnerCoordinate>,
    used: BTreeSet<usize>,
}

impl OwnerCoordinates {
    fn decode(value: &Value) -> CertResult<Self> {
        let values = array(value)?;
        let mut unique = BTreeSet::new();
        let rows = values
            .iter()
            .map(|value| {
                let row = sized(value, 4)?;
                let coordinate = match string(&row[0])? {
                    "source" => OwnerCoordinate::Source {
                        unit: string(&row[1])?.to_owned(),
                        module: string(&row[2])?.to_owned(),
                        module_version: optional_version(&row[3])?,
                    },
                    "package" => OwnerCoordinate::Package {
                        unit: string(&row[1])?.to_owned(),
                        module: string(&row[2])?.to_owned(),
                        interface_digest: digest(&row[3])?,
                    },
                    _ => return Err(CertificationError::Receipt("owner coordinate kind")),
                };
                if !unique.insert(coordinate.clone()) {
                    return Err(CertificationError::Receipt("duplicate owner coordinate"));
                }
                Ok(coordinate)
            })
            .collect::<CertResult<_>>()?;
        Ok(Self {
            rows,
            used: BTreeSet::new(),
        })
    }

    fn get(&mut self, value: &Value) -> CertResult<&OwnerCoordinate> {
        let index = usize::try_from(number(value)?)
            .map_err(|_| CertificationError::Receipt("owner coordinate index"))?;
        let coordinate = self
            .rows
            .get(index)
            .ok_or(CertificationError::Receipt("owner coordinate index"))?;
        self.used.insert(index);
        Ok(coordinate)
    }

    fn owner(&mut self, value: &Value, binder: &SymbolIdentity) -> CertResult<ReceiptImportOwner> {
        let row = array(value)?;
        let tag = row
            .first()
            .ok_or(CertificationError::Receipt("empty import owner"))?;
        match string(tag)? {
            "source" if row.len() == 3 => {
                let original_ordinal = u32::try_from(number(&row[2])?)
                    .map_err(|_| CertificationError::Receipt("source group ordinal"))?;
                let OwnerCoordinate::Source {
                    unit,
                    module,
                    module_version,
                } = self.get(&row[1])?
                else {
                    return Err(CertificationError::Receipt("owner coordinate kind"));
                };
                Ok(ReceiptImportOwner::Source {
                    unit: unit.clone(),
                    module: module.clone(),
                    module_version: module_version.clone(),
                    original_ordinal,
                    binder: binder.clone(),
                })
            }
            "retained" if row.len() == 2 => Ok(ReceiptImportOwner::Retained {
                identity: binder.clone(),
                generation: number(&row[1])?,
            }),
            tag @ ("package" | "retained-package")
                if row.len() == (if tag == "package" { 2 } else { 3 }) =>
            {
                let generation = if tag == "package" {
                    None
                } else {
                    Some(number(&row[2])?)
                };
                let OwnerCoordinate::Package {
                    unit,
                    module,
                    interface_digest,
                } = self.get(&row[1])?
                else {
                    return Err(CertificationError::Receipt("owner coordinate kind"));
                };
                Ok(match generation {
                    None => ReceiptImportOwner::Package {
                        unit: unit.clone(),
                        module: module.clone(),
                        interface_digest: *interface_digest,
                        binder: binder.clone(),
                    },
                    Some(generation) => ReceiptImportOwner::RetainedPackage {
                        unit: unit.clone(),
                        module: module.clone(),
                        interface_digest: *interface_digest,
                        binder: binder.clone(),
                        generation,
                    },
                })
            }
            _ => Err(CertificationError::Receipt("import owner tag/arity")),
        }
    }
}

struct GlobalDictionary {
    rows: Vec<(AcceptedGlobal, usize)>,
    used: BTreeSet<usize>,
}

impl GlobalDictionary {
    #[cfg(test)]
    fn decode(value: &Value, coordinates: &mut OwnerCoordinates) -> CertResult<Self> {
        Self::decode_with_operation(
            value,
            coordinates,
            &InventoryOperation::new(Default::default()),
        )
    }
    fn decode_with_operation(
        value: &Value,
        coordinates: &mut OwnerCoordinates,
        operation: &InventoryOperation,
    ) -> CertResult<Self> {
        let rows = array(value)?;
        let mut unique = BTreeSet::new();
        let rows = rows
            .iter()
            .map(|value| {
                let row = sized(value, 5)?;
                let identity = identity(&row[0])?;
                let global = AcceptedGlobal {
                    owner: coordinates.owner(&row[4], &identity)?,
                    identity,
                    rep: rep(&row[1])?,
                    entry_signature: signature(&row[2])?,
                    required_evaluated: boolean(&row[3])?,
                };
                charge_global(operation, &global)?;
                let encoded = value_global(&global);
                operation.charge_value_copies(&encoded, 1)?;
                let length = encoded_size(&encoded)?;
                operation.charge(length)?;
                let mut canonical = Vec::with_capacity(length);
                ciborium::ser::into_writer(&encoded, &mut canonical)
                    .map_err(|_| CertificationError::Receipt("global dictionary encoding"))?;
                if !unique.insert(canonical) {
                    return Err(CertificationError::Receipt(
                        "duplicate global dictionary row",
                    ));
                }
                Ok((global, length))
            })
            .collect::<CertResult<_>>()?;
        Ok(Self {
            rows,
            used: BTreeSet::new(),
        })
    }

    #[cfg(test)]
    fn resolve(&mut self, indices: &[Value]) -> CertResult<Vec<AcceptedGlobal>> {
        self.resolve_with_operation(indices, &InventoryOperation::new(Default::default()))
    }

    fn resolve_with_operation(
        &mut self,
        indices: &[Value],
        operation: &InventoryOperation,
    ) -> CertResult<Vec<AcceptedGlobal>> {
        operation.reserve::<AcceptedGlobal>(indices.len())?;
        indices
            .iter()
            .map(|value| {
                let index = usize::try_from(number(value)?)
                    .map_err(|_| CertificationError::Receipt("global dictionary index"))?;
                let (global, length) = self
                    .rows
                    .get(index)
                    .ok_or(CertificationError::Receipt("global dictionary index"))?;
                // Canonical bytes account payload expansion; reserve structural
                // slots separately before cloning the witness and its owner.
                operation.charge(
                    length
                        .checked_mul(4)
                        .ok_or(CertificationError::Receipt("expanded global bytes"))?,
                )?;
                operation.reserve::<(usize, usize, usize)>(1)?;
                self.used.insert(index);
                Ok(global.clone())
            })
            .collect()
    }
}

struct EncodingSize(usize);
impl std::io::Write for EncodingSize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("encoding size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encoded_size(value: &Value) -> CertResult<usize> {
    let mut size = EncodingSize(0);
    ciborium::ser::into_writer(value, &mut size)
        .map_err(|_| CertificationError::Receipt("encoding size"))?;
    Ok(size.0)
}

fn encode_value_with_operation(
    value: &Value,
    format: CertificationFormat,
    limit: usize,
    operation: &InventoryOperation,
) -> CertResult<Vec<u8>> {
    let size = encoded_size(value)?;
    if size > limit {
        return Err(CertificationError::SizeLimit {
            format,
            actual: size,
            limit,
        });
    }
    operation.charge(
        size.checked_mul(2)
            .ok_or(CertificationError::Receipt("encoding work"))?,
    )?;
    let mut bytes = Vec::with_capacity(size);
    ciborium::ser::into_writer(value, &mut bytes)
        .map_err(|_| CertificationError::Receipt("encoding"))?;
    Ok(bytes)
}

fn sha(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn read_bounded_with_operation(
    path: &Path,
    limit: u64,
    operation: &InventoryOperation,
) -> CertResult<Vec<u8>> {
    use std::io::Read;
    let failure = |failure| CertificationError::EvidenceRead {
        path: path.to_path_buf(),
        failure,
    };
    if !path.is_absolute() {
        return Err(failure(EvidenceReadFailure::NonAbsolutePath));
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        failure(EvidenceReadFailure::Io {
            operation: EvidenceReadOperation::Metadata,
            error,
        })
    })?;
    if !metadata.is_file() {
        return Err(failure(EvidenceReadFailure::NotFile));
    }
    if metadata.len() > limit {
        return Err(failure(EvidenceReadFailure::SizeLimit {
            actual: metadata.len(),
            limit,
        }));
    }
    operation.charge(metadata.len() as usize + 1)?;
    let file = std::fs::File::open(path).map_err(|error| {
        failure(EvidenceReadFailure::Io {
            operation: EvidenceReadOperation::Open,
            error,
        })
    })?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize + 1);
    file.take(metadata.len() + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            failure(EvidenceReadFailure::Io {
                operation: EvidenceReadOperation::Read,
                error,
            })
        })?;
    if bytes.len() as u64 != metadata.len() {
        return Err(failure(EvidenceReadFailure::LengthChanged {
            expected: metadata.len(),
            actual: bytes.len() as u64,
        }));
    }
    Ok(bytes)
}

fn verify_package_interface(
    validation: &mut PackageInterfaceValidation,
    witness: &PackageInterfaceWitness,
) -> CertResult<()> {
    validation
        .verify(&witness.selected_path, &witness.sha256)
        .map_err(package_validation_error)
}

fn package_validation_error(
    error: crate::recovery_artifacts::RecoveryArtifactError,
) -> CertificationError {
    match error {
        error @ crate::recovery_artifacts::RecoveryArtifactError::InventoryAccounting(_) => {
            CertificationError::CapturedModulePayload(error)
        }
        _ => CertificationError::StaleEvidence,
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn ready_source_sha(
    evidence: &DependencyEvidence,
    unit: &str,
    module: &str,
) -> CertResult<[u8; 32]> {
    let matching: Vec<_> = evidence
        .modules
        .iter()
        .filter(|row| {
            row.unit == unit
                && row.module == module
                && !row.boot
                && row.product.has_native_product()
        })
        .collect();
    let [module] = matching.as_slice() else {
        return Err(CertificationError::Mismatch("ready source module"));
    };
    let source = evidence
        .sources
        .iter()
        .find(|source| source.path == module.source)
        .ok_or(CertificationError::Mismatch("source evidence"))?;
    let value = Value::Text(source.sha256.clone());
    digest(&value)
}

fn matching_product<'a>(
    products: &'a [RawModuleProduct],
    unit: &str,
    module: &str,
) -> CertResult<&'a RawModuleProduct> {
    let matching: Vec<_> = products
        .iter()
        .filter(|product| product.unit == unit && product.module == module)
        .collect();
    let [product] = matching.as_slice() else {
        return Err(CertificationError::Mismatch("unique module product"));
    };
    Ok(product)
}

fn fresh_module_version(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    source_sha: &[u8; 32],
    interface: &[u8],
    product_bytes: &[u8],
    package_imports: &[u8],
) -> CertResult<ModuleVersion> {
    let include = crate::module_candidates::context_paths(include)
        .ok_or(CertificationError::Mismatch("include context"))?;
    Ok(crate::module_candidates::module_version_for_product(
        endpoint_identity,
        &include,
        &hex(source_sha),
        interface,
        product_bytes,
        package_imports,
    ))
}

type SourceGroupKey = (CachedHomeOwner, u32, SymbolIdentity);

fn duplicate_source_binder(
    phase: SourceBinderPhase,
    key: &SourceGroupKey,
    existing_origin: ProductOrigin,
    incoming_origin: ProductOrigin,
) -> CertificationError {
    CertificationError::DuplicateSourceBinder(Box::new(SourceBinderConflict {
        phase,
        owner: key.0.clone(),
        original_ordinal: key.1,
        binder: key.2.clone(),
        existing_origin,
        incoming_origin,
    }))
}

/// Exact type and native roles issued for one compiler request, completed by
/// that request's validated output rows. Full retained custody is not an offer.
#[derive(Clone, Debug, Default)]
pub(crate) struct CertifiedSourceSelection {
    modules: BTreeMap<(String, String), ReceiptSourceSelection>,
}

#[derive(Clone, Debug)]
enum ReceiptSourceSelection {
    InterfaceOnly {
        interface: crate::artifact_inventory::ArtifactId,
    },
    Native {
        interface: crate::artifact_inventory::ArtifactId,
        native: ReceiptNativeSelection,
    },
}

#[derive(Clone, Debug)]
struct ReceiptNativeSelection {
    owner: CachedHomeOwner,
    version: ReceiptSourceVersion,
}

impl ReceiptSourceSelection {
    fn interface(&self) -> crate::artifact_inventory::ArtifactId {
        match self {
            Self::InterfaceOnly { interface } | Self::Native { interface, .. } => *interface,
        }
    }
    fn native(&self) -> Option<&ReceiptNativeSelection> {
        match self {
            Self::Native { native, .. } => Some(native),
            Self::InterfaceOnly { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ReceiptSourceVersion {
    Original,
    Fresh,
    RetainedCore,
}

impl CertifiedSourceSelection {
    /// Resolve already issued compiler roles against their authenticated custody.
    /// Merely retaining a native artifact does not grant an original offer.
    pub(crate) fn from_compiler_projection(
        projection: &crate::artifact_inventory::CompilerInputProjection,
        metadata: &crate::artifact_inventory::ArtifactMetadataSnapshot,
        operation: &InventoryOperation,
    ) -> CertResult<Self> {
        let entries = projection
            .entries_from_metadata(metadata)
            .map_err(|_| CertificationError::Mismatch("compiler original projection"))?;
        let products = entries
            .values()
            .filter_map(|entry| match &entry.payload {
                crate::artifact_inventory::ArtifactPayload::Original(product) => {
                    Some(product.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut selection = Self::from_projected_originals(&products, operation)?;
        for entry in entries.values().filter(|entry| {
            !matches!(
                &entry.payload,
                crate::artifact_inventory::ArtifactPayload::Original(_)
            )
        }) {
            selection.admit_interface(entry, operation)?;
        }
        Ok(selection)
    }

    fn native_selections(&self) -> impl Iterator<Item = &ReceiptNativeSelection> {
        self.modules
            .values()
            .filter_map(ReceiptSourceSelection::native)
    }

    /// Exact originals selected by this request's issuer, including validated
    /// newly finalized rows. The caller still authenticates their artifact IDs.
    pub(crate) fn selected_original_owners(&self) -> impl Iterator<Item = &CachedHomeOwner> {
        self.native_selections().map(|selected| &selected.owner)
    }

    /// Convert this transaction's issued complete owner identities to exact
    /// artifact roles. Native availability alone cannot issue this selection.
    pub(crate) fn compiler_projection(
        &self,
        view: &crate::artifact_inventory::ArtifactView,
    ) -> CertResult<crate::artifact_inventory::CompilerInputProjection> {
        use crate::artifact_inventory::{ArtifactPayload, CompilerInputProjection};
        let metadata = view.metadata_snapshot();
        let mut entries = Vec::new();
        for selected in self.modules.values() {
            let interface = metadata
                .artifacts
                .get(&selected.interface())
                .filter(|entry| {
                    !matches!(
                        &entry.payload,
                        crate::artifact_inventory::ArtifactPayload::Original(_)
                    )
                })
                .ok_or(CertificationError::Mismatch(
                    "issued compiler interface artifact",
                ))?;
            entries.push(Arc::clone(interface));
            if let Some(native) = selected.native() {
                let matching = metadata.artifacts.values().filter(|entry| {
                    matches!(&entry.payload, ArtifactPayload::Original(product) if product.owner() == &native.owner)
                }).collect::<Vec<_>>();
                let [entry] = matching.as_slice() else {
                    return Err(CertificationError::Mismatch(
                        "issued compiler original artifact",
                    ));
                };
                entries.push(Arc::clone(entry));
            }
        }
        let projection = CompilerInputProjection::from_issued_entries(&entries)
            .map_err(|_| CertificationError::Mismatch("issued compiler original projection"))?;
        Self::from_compiler_projection(
            &projection,
            &metadata,
            &InventoryOperation::new(Default::default()),
        )?;
        Ok(projection)
    }

    fn from_projected_originals(
        products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
        operation: &InventoryOperation,
    ) -> CertResult<Self> {
        let mut selected = Self::default();
        for product in products {
            let witness = product
                .original_native()
                .ok_or(CertificationError::Mismatch(
                    "compiler original native witness",
                ))?;
            if !witness.matches_original(product) {
                return Err(CertificationError::Mismatch(
                    "original native witness bytes",
                ));
            }
            operation.charge(
                2 * (witness.owner.unit.len() + witness.owner.module.len())
                    + std::mem::size_of::<ReceiptSourceSelection>(),
            )?;
            let key = (witness.owner.unit.clone(), witness.owner.module.clone());
            if selected
                .modules
                .insert(
                    key,
                    ReceiptSourceSelection::Native {
                        interface: crate::artifact_inventory::ArtifactEntry::canonical(
                            product
                                .module_interface()
                                .ok_or(CertificationError::Mismatch(
                                    "compiler original canonical interface",
                                ))?
                                .clone(),
                        )
                        .descriptor
                        .id,
                        native: ReceiptNativeSelection {
                            owner: witness.owner.clone(),
                            version: ReceiptSourceVersion::Original,
                        },
                    },
                )
                .is_some()
            {
                return Err(CertificationError::Mismatch(
                    "ambiguous compiler original owner",
                ));
            }
        }
        Ok(selected)
    }

    fn admit_interface(
        &mut self,
        entry: &crate::artifact_inventory::ArtifactEntry,
        operation: &InventoryOperation,
    ) -> CertResult<()> {
        if matches!(
            &entry.payload,
            crate::artifact_inventory::ArtifactPayload::Original(_)
        ) {
            return Err(CertificationError::Mismatch("compiler interface role"));
        }
        let key = (
            entry.descriptor.owner.unit.clone(),
            entry.descriptor.owner.module.clone(),
        );
        if let Some(previous) = self.modules.get(&key) {
            if previous.interface() != entry.descriptor.id {
                return Err(CertificationError::Mismatch(
                    "compiler selected interface replaced",
                ));
            }
            return Ok(());
        }
        operation.charge(
            2 * (key.0.len() + key.1.len()) + std::mem::size_of::<ReceiptSourceSelection>(),
        )?;
        self.modules.insert(
            key,
            ReceiptSourceSelection::InterfaceOnly {
                interface: entry.descriptor.id,
            },
        );
        Ok(())
    }

    fn admit_current(
        &mut self,
        owner: &CachedHomeOwner,
        origin: ProductOrigin,
        canonical: &CertifiedModuleInterface,
        operation: &InventoryOperation,
    ) -> CertResult<()> {
        let key = (owner.unit.clone(), owner.module.clone());
        if let Some(previous) = self
            .modules
            .get(&key)
            .and_then(ReceiptSourceSelection::native)
        {
            if previous.owner != *owner || origin != ProductOrigin::Cached {
                return Err(CertificationError::Mismatch(
                    "compiler original owner replaced",
                ));
            }
        }
        if canonical.unit() != owner.unit
            || canonical.module() != owner.module
            || canonical.interface_sha256() != owner.skinny_iface_sha256
        {
            return Err(CertificationError::Mismatch(
                "compiler current canonical interface",
            ));
        }
        let interface = crate::artifact_inventory::ArtifactEntry::canonical(canonical.clone())
            .descriptor
            .id;
        if let Some(previous) = self.modules.get(&key) {
            if previous.interface() != interface {
                return Err(CertificationError::Mismatch(
                    "compiler selected interface replaced",
                ));
            }
            if previous.native().is_some() {
                return Ok(());
            }
        } else {
            operation.charge(
                2 * (owner.unit.len() + owner.module.len())
                    + std::mem::size_of::<ReceiptSourceSelection>(),
            )?;
        }
        let version = match origin {
            ProductOrigin::Cached => ReceiptSourceVersion::Original,
            ProductOrigin::Fresh => ReceiptSourceVersion::Fresh,
            ProductOrigin::RetainedCore => ReceiptSourceVersion::RetainedCore,
        };
        self.modules.insert(
            key,
            ReceiptSourceSelection::Native {
                interface,
                native: ReceiptNativeSelection {
                    owner: owner.clone(),
                    version,
                },
            },
        );
        Ok(())
    }

    fn promote(&mut self, versions: &BTreeMap<(String, String), ModuleVersion>) -> CertResult<()> {
        for (key, selected) in &mut self.modules {
            let ReceiptSourceSelection::Native {
                native: selected, ..
            } = selected
            else {
                continue;
            };
            if matches!(selected.version, ReceiptSourceVersion::RetainedCore) {
                selected.owner.module_version = versions
                    .get(key)
                    .ok_or(CertificationError::Mismatch("retained derived identity"))?
                    .clone();
            }
        }
        Ok(())
    }
}

fn current_module_interface<'a>(
    owner: &CachedHomeOwner,
    origin: ProductOrigin,
    candidates: Option<&'a CandidateSet>,
    finalized: &'a [CertifiedModuleInterface],
    inherited: &'a [CertifiedModuleInterface],
) -> CertResult<&'a CertifiedModuleInterface> {
    let matches = |interface: &&CertifiedModuleInterface| {
        interface.unit() == owner.unit && interface.module() == owner.module
    };
    match origin {
        ProductOrigin::Fresh => finalized.iter().find(matches),
        ProductOrigin::Cached => candidates
            .and_then(|set| {
                set.by_owner
                    .get(&(owner.unit.clone(), owner.module.clone()))
            })
            .filter(|bundle| bundle.owner == *owner)
            .map(|bundle| &bundle.original_module_interface)
            .or_else(|| finalized.iter().chain(inherited.iter()).find(matches)),
        ProductOrigin::RetainedCore => inherited.iter().find(matches),
    }
    .ok_or(CertificationError::Mismatch(
        "native canonical module carrier",
    ))
}

/// Exact originals whose complete current promotion has been compared with custody.
#[derive(Default)]
struct ValidatedPromotionSharing(BTreeSet<CachedHomeOwner>);

#[derive(Default)]
struct SourceGroupMap {
    groups: BTreeMap<SourceGroupKey, (CachedHomeOwner, ProductOrigin)>,
    modules: SourceModuleIndex,
}

#[derive(Default)]
struct SourceModuleIndex(BTreeMap<String, BTreeSet<String>>);

impl SourceModuleIndex {
    fn insert(&mut self, unit: &str, module: &str) {
        if let Some(modules) = self.0.get_mut(unit) {
            if !modules.contains(module) {
                modules.insert(module.to_owned());
            }
        } else {
            self.0
                .insert(unit.to_owned(), BTreeSet::from([module.to_owned()]));
        }
    }

    fn contains(&self, unit: &str, module: &str) -> bool {
        self.0
            .get(unit)
            .is_some_and(|modules| modules.contains(module))
    }
}

impl SourceGroupMap {
    fn new() -> Self {
        Self::default()
    }

    fn insert(
        &mut self,
        key: SourceGroupKey,
        owner: (CachedHomeOwner, ProductOrigin),
    ) -> Option<(CachedHomeOwner, ProductOrigin)> {
        self.modules.insert(&key.0.unit, &key.0.module);
        self.groups.insert(key, owner)
    }

    fn get(&self, key: &SourceGroupKey) -> Option<&(CachedHomeOwner, ProductOrigin)> {
        self.groups.get(key)
    }

    fn insert_unique(
        &mut self,
        key: SourceGroupKey,
        owner: (CachedHomeOwner, ProductOrigin),
        phase: SourceBinderPhase,
    ) -> CertResult<()> {
        self.modules.insert(&key.0.unit, &key.0.module);
        match self.groups.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(owner);
                Ok(())
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let conflict = duplicate_source_binder(phase, entry.key(), entry.get().1, owner.1);
                entry.insert(owner);
                Err(conflict)
            }
        }
    }

    fn contains_module(&self, unit: &str, module: &str) -> bool {
        self.modules.contains(unit, module)
    }

    fn promote(
        &mut self,
        versions: &BTreeMap<CachedHomeOwner, ModuleVersion>,
        sharing: &ValidatedPromotionSharing,
    ) -> CertResult<()> {
        let staged = std::mem::take(&mut self.groups);
        for ((mut key_owner, ordinal, binder), (mut owner, origin)) in staged {
            if let Some(version) = versions.get(&key_owner) {
                owner.module_version = version.clone();
                key_owner.module_version = version.clone();
            }
            let key = (key_owner, ordinal, binder);
            if self.groups.contains_key(&key) && sharing.0.contains(&key.0) {
                // Two validated provenance paths name one exact original membership.
                continue;
            }
            self.insert_unique(key, (owner, origin), SourceBinderPhase::NativePromotion)?;
        }
        Ok(())
    }
}

#[cfg(test)]
fn resolve_receipt_owner(
    import: ReceiptImportOwner,
    sources: &SourceGroupMap,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
) -> CertResult<PendingImportOwner> {
    resolve_receipt_owner_with_validation(
        import,
        sources,
        None,
        packages,
        &mut PackageInterfaceValidation::default(),
    )
}

fn resolve_receipt_owner_with_validation(
    import: ReceiptImportOwner,
    sources: &SourceGroupMap,
    selection: Option<&CertifiedSourceSelection>,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<PendingImportOwner> {
    match import {
        ReceiptImportOwner::Source {
            unit,
            module,
            module_version,
            original_ordinal,
            binder,
        } => {
            let resolved = if let Some(selected) = selection.and_then(|selection| {
                selection
                    .modules
                    .get(&(unit.clone(), module.clone()))
                    .and_then(ReceiptSourceSelection::native)
            }) {
                match selected.version {
                    ReceiptSourceVersion::Original if module_version.is_none() => {
                        return Err(CertificationError::Mismatch(
                            "missing cached source version",
                        ));
                    }
                    ReceiptSourceVersion::RetainedCore if module_version.is_some() => {
                        return Err(CertificationError::Mismatch(
                            "retained source version must be derived",
                        ));
                    }
                    _ => {}
                }
                if module_version
                    .as_ref()
                    .is_some_and(|version| version != &selected.owner.module_version)
                {
                    return Err(CertificationError::Mismatch("source module version"));
                }
                sources
                    .get(&(selected.owner.clone(), original_ordinal, binder.clone()))
                    .map(|(owner, _)| owner)
                    .ok_or(CertificationError::Mismatch("source binder/group closure"))?
            } else {
                // Versioned legacy/original receipts can identify their native
                // generation; an unversioned receipt requires a request issuer.
                let version = module_version.as_ref().ok_or(CertificationError::Mismatch(
                    "unversioned source lacks compiler selection",
                ))?;
                if selection.is_some() {
                    return Err(CertificationError::Mismatch(
                        "source outside compiler selection",
                    ));
                }
                let mut matches =
                    sources
                        .groups
                        .iter()
                        .filter(|((owner, ordinal, candidate), _)| {
                            owner.unit == unit
                                && owner.module == module
                                && &owner.module_version == version
                                && *ordinal == original_ordinal
                                && candidate == &binder
                        });
                let (key, _) = matches
                    .next()
                    .ok_or(CertificationError::Mismatch("source binder/group closure"))?;
                if matches.next().is_some() {
                    return Err(CertificationError::Mismatch(
                        "ambiguous source original identity",
                    ));
                }
                &key.0
            };
            Ok(PendingImportOwner::Source {
                owner: resolved.clone(),
                original_ordinal,
                binder,
            })
        }
        ReceiptImportOwner::Retained {
            identity,
            generation,
        } => Ok(PendingImportOwner::Retained {
            identity,
            generation,
        }),
        ReceiptImportOwner::RetainedPackage {
            unit,
            module,
            binder,
            generation,
            interface_digest,
        } => {
            validate_package_owner(
                &unit,
                &module,
                &binder,
                &interface_digest,
                sources,
                packages,
                validation,
            )?;
            Ok(PendingImportOwner::RetainedPackage {
                unit,
                module,
                binder,
                generation,
                interface_digest,
            })
        }
        ReceiptImportOwner::Package {
            unit,
            module,
            binder,
            interface_digest,
        } => {
            validate_package_owner(
                &unit,
                &module,
                &binder,
                &interface_digest,
                sources,
                packages,
                validation,
            )?;
            Ok(PendingImportOwner::Package {
                unit,
                module,
                binder,
                interface_digest,
            })
        }
    }
}

/// A cached original carries its own exact source domain. The compiler's
/// current namespace may select another implementation of the same module.
fn validate_original_group_receipts(
    group: &ProjectedGroup,
    receipts: &[ReceiptImportOwner],
    original: &AuthenticatedOriginalGroup,
    sources: &SourceGroupMap,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<PendingCertifiedGroup> {
    if original.group() != group || original.imports.len() != receipts.len() {
        return Err(CertificationError::Mismatch("shared original home groups"));
    }
    for (receipt, expected) in receipts.iter().zip(original.imports.iter()) {
        match (receipt, expected) {
            (
                ReceiptImportOwner::Source {
                    unit,
                    module,
                    module_version,
                    original_ordinal,
                    binder,
                },
                PendingImportOwner::Source {
                    owner,
                    original_ordinal: expected_ordinal,
                    binder: expected_binder,
                },
            ) => {
                if unit != &owner.unit
                    || module != &owner.module
                    || module_version.as_ref() != Some(&owner.module_version)
                    || original_ordinal != expected_ordinal
                    || binder != expected_binder
                {
                    return Err(CertificationError::Mismatch("shared home import ownership"));
                }
                if sources
                    .get(&(owner.clone(), *original_ordinal, binder.clone()))
                    .is_none()
                {
                    return Err(CertificationError::Mismatch("source binder/group closure"));
                }
            }
            _ => {
                let actual = resolve_receipt_owner_with_validation(
                    receipt.clone(),
                    sources,
                    None,
                    packages,
                    validation,
                )?;
                if &actual != expected {
                    return Err(CertificationError::Mismatch("shared home import ownership"));
                }
            }
        }
    }
    Ok(original.admitted())
}

/// Merge equal current and prior selected groups using their original arenas.
/// Proved promotions retain the current event; cached reoffers retain the prior
/// selected event. Full-original equality is issued once at the owner boundary.
fn append_original_selection(
    groups: &mut Vec<PendingCertifiedGroup>,
    inherited: &[PendingCertifiedGroup],
    operation: &InventoryOperation,
    sharing: &ValidatedPromotionSharing,
) -> CertResult<()> {
    for group in groups.iter().chain(inherited) {
        operation.charge(
            2 * (group.owner.unit.len() + group.owner.module.len())
                + 2 * std::mem::size_of::<(CachedHomeOwner, u32)>(),
        )?;
    }
    let mut positions = BTreeMap::new();
    for (index, group) in groups.iter().enumerate() {
        if positions
            .insert((group.owner.clone(), group.group.original_ordinal()), index)
            .is_some()
        {
            return Err(CertificationError::Mismatch(
                "duplicate current original ordinal",
            ));
        }
    }
    let mut seen = BTreeSet::new();
    for group in inherited {
        let key = (group.owner.clone(), group.group.original_ordinal());
        if !seen.insert(key.clone()) {
            return Err(CertificationError::Mismatch(
                "duplicate selected original ordinal",
            ));
        }
        if let Some(index) = positions.get(&key) {
            let current = &groups[*index];
            let shared = sharing.0.contains(&current.owner);
            if (current.origin != ProductOrigin::Cached && !shared)
                || current.group != group.group
                || current.imports != group.imports
            {
                return Err(original_group_conflict(
                    &current.owner,
                    OriginalGroupFailure::SelectionOverlap {
                        ordinal: current.group.original_ordinal(),
                        current_origin: current.origin,
                        inherited_origin: group.origin,
                        body_matches: current.group == group.group,
                        imports_match: current.imports == group.imports,
                    },
                ));
            }
            groups[*index] = if shared {
                // Compiler input roles and selected execution groups are separate.
                // The same proved original may be re-emitted and already selected;
                // retain the current event while sharing immutable original arenas.
                PendingCertifiedGroup {
                    origin: current.origin,
                    ..group.clone()
                }
            } else {
                group.clone()
            };
        } else {
            groups.push(group.clone());
        }
    }
    Ok(())
}

/// Resolve a source receipt from the authenticated owner declaration. Cold
/// recovery can authenticate that declaration without loading every original
/// it names; exact binder and group membership are checked when a context
/// admits the selected originals together.
fn resolve_cold_recovery_source(
    import: ReceiptImportOwner,
    witness: &HomeCertification,
    product: &RawModuleProduct,
) -> CertResult<PendingImportOwner> {
    let ReceiptImportOwner::Source {
        unit,
        module,
        module_version,
        original_ordinal,
        binder,
    } = import
    else {
        return Err(CertificationError::Mismatch(
            "expected recovery source owner",
        ));
    };
    let key = (unit.clone(), module.clone());
    let owner = if key == (witness.owner.unit.clone(), witness.owner.module.clone()) {
        &witness.owner
    } else {
        witness
            .sources
            .get(&key)
            .ok_or(CertificationError::Mismatch("home source witness"))?
    };
    if owner == &witness.owner
        && !product.groups.iter().any(|group| {
            group.original_ordinal() == original_ordinal && group.binders().contains(&binder)
        })
    {
        return Err(CertificationError::Mismatch("source binder/group closure"));
    }
    if binder.unit != unit || binder.module != module {
        return Err(CertificationError::Mismatch("home source witness"));
    }
    if module_version.as_ref() != Some(&owner.module_version) {
        return Err(CertificationError::Mismatch("source module version"));
    }
    Ok(PendingImportOwner::Source {
        owner: owner.clone(),
        original_ordinal,
        binder,
    })
}

fn validate_package_owner(
    unit: &str,
    module: &str,
    binder: &SymbolIdentity,
    interface_digest: &[u8; 32],
    sources: &SourceGroupMap,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<()> {
    if sources.contains_module(unit, module) {
        return Err(CertificationError::Mismatch(
            "home owner downgraded to package",
        ));
    }
    if binder.unit != unit || binder.module != module || *interface_digest == [0; 32] {
        return Err(CertificationError::Mismatch("package owner identity"));
    }
    let witness = packages
        .get(&(unit.to_owned(), module.to_owned()))
        .ok_or(CertificationError::Mismatch("package interface witness"))?;
    if witness.sha256 != *interface_digest || !witness.selected_path.is_absolute() {
        return Err(CertificationError::Mismatch("package interface witness"));
    }
    verify_package_interface(validation, witness)
}

#[cfg(test)]
fn check_direct_package_agreement(
    sidecar: &[u8],
    owner: &CachedHomeOwner,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
) -> CertResult<()> {
    check_direct_package_agreement_with_validation(
        sidecar,
        owner,
        packages,
        &mut PackageInterfaceValidation::default(),
    )
}

fn check_direct_package_agreement_with_validation(
    sidecar: &[u8],
    owner: &CachedHomeOwner,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<()> {
    let roots = crate::recovery_artifacts::validate_package_imports_with_validation(
        sidecar,
        &owner.unit,
        &owner.module,
        &owner.skinny_iface_sha256,
        Path::new("module-package-imports.cbor"),
        validation,
    )
    .map_err(|_| CertificationError::Mismatch("direct package import witness"))?;
    for (key, (path, digest)) in roots {
        if let Some(witness) = packages.get(&key) {
            if witness.selected_path != path || hex(&witness.sha256) != digest {
                return Err(CertificationError::Mismatch(
                    "direct package/receipt selection",
                ));
            }
        }
    }
    Ok(())
}

fn certified_source_map(groups: &[PendingCertifiedGroup]) -> CertResult<SourceGroupMap> {
    let mut sources = SourceGroupMap::new();
    for group in groups {
        for binder in group.group.binders() {
            let key = (
                group.owner.clone(),
                group.group.original_ordinal(),
                binder.clone(),
            );
            sources.insert_unique(
                key,
                (group.owner.clone(), group.origin),
                SourceBinderPhase::SelectedGroups,
            )?;
        }
    }
    Ok(sources)
}

/// Read-only source membership from complete authenticated original witnesses.
/// This index does not admit executable groups or add inventory roots.
fn available_original_source_map(
    products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
    selected: &[PendingCertifiedGroup],
    operation: &InventoryOperation,
) -> CertResult<SourceGroupMap> {
    // Charge the complete lookup before allocating owned keys. This conservative
    // bound covers the owner/ordinal and binder indices plus source-map copies;
    // it uses the enclosing admission's budget, not a new decoding operation.
    for product in products {
        let witness = product
            .original_native()
            .ok_or(CertificationError::Mismatch(
                "available original native witness",
            ))?;
        operation.charge(
            4 * (witness.owner.unit.len() + witness.owner.module.len())
                + 4 * std::mem::size_of::<CachedHomeOwner>(),
        )?;
        for group in witness.groups.iter() {
            operation.charge(
                2 * (witness.owner.unit.len() + witness.owner.module.len())
                    + std::mem::size_of::<SourceGroupKey>(),
            )?;
            for binder in group.group.binders() {
                operation.charge(
                    3 * (witness.owner.unit.len() + witness.owner.module.len())
                        + 3 * std::mem::size_of::<SourceGroupKey>(),
                )?;
                for _ in 0..3 {
                    charge_symbol(operation, binder)?;
                }
            }
        }
    }
    for group in selected {
        for binder in group.group.binders() {
            operation.charge(
                2 * (group.owner.unit.len() + group.owner.module.len())
                    + 2 * std::mem::size_of::<SourceGroupKey>(),
            )?;
            charge_symbol(operation, binder)?;
        }
    }
    let mut sources = certified_source_map(selected)?;
    let mut owners = BTreeSet::new();
    let mut binders = BTreeSet::new();
    let mut originals = BTreeMap::new();
    for product in products {
        let witness = product
            .original_native()
            .ok_or(CertificationError::Mismatch(
                "available original native witness",
            ))?;
        if !witness.matches_original(product) {
            return Err(CertificationError::Mismatch(
                "original native witness bytes",
            ));
        }
        if !owners.insert(witness.owner.clone()) {
            return Err(CertificationError::Mismatch(
                "duplicate available original owner",
            ));
        }
        sources
            .modules
            .insert(&witness.owner.unit, &witness.owner.module);
        for group in witness.groups.iter() {
            let key = (witness.owner.clone(), group.group.original_ordinal());
            if originals.insert(key, group).is_some() {
                return Err(CertificationError::Mismatch(
                    "duplicate available original ordinal",
                ));
            }
            for binder in group.group.binders() {
                if binder.unit != witness.owner.unit
                    || binder.module != witness.owner.module
                    || !binders.insert((witness.owner.clone(), binder.clone()))
                {
                    return Err(CertificationError::Mismatch(
                        "duplicate available original binder",
                    ));
                }
            }
        }
    }
    for group in selected {
        let original = originals
            .get(&(group.owner.clone(), group.group.original_ordinal()))
            .ok_or(CertificationError::Mismatch(
                "selected original outside availability",
            ))?;
        if original.owner != group.owner
            || original.group != group.group
            || original.imports != group.imports
        {
            return Err(CertificationError::Mismatch("shared original home groups"));
        }
    }
    for original in originals.values() {
        for binder in original.group.binders() {
            let key = (
                original.owner.clone(),
                original.group.original_ordinal(),
                binder.clone(),
            );
            if sources.get(&key).is_none() {
                sources.insert(key, (original.owner.clone(), ProductOrigin::Cached));
            }
        }
    }
    Ok(sources)
}

/// Exact artifact authenticated by the run-owned recovery reference.
pub struct InheritedProductInput<'a> {
    pub artifact: &'a crate::recovery_artifacts::VerifiedRecoveryArtifact,
}

struct HomeCertification {
    owner: CachedHomeOwner,
    execution_source_sha256: Option<[u8; 32]>,
    finalized_module_sha256: Option<[u8; 32]>,
    groups: Vec<(u32, Vec<SymbolIdentity>, Vec<AcceptedGlobal>)>,
    sources: BTreeMap<(String, String), CachedHomeOwner>,
    interface_requirements: BTreeMap<(String, String), [u8; 32]>,
    packages: BTreeMap<(String, String), PackageInterfaceWitness>,
}

fn decode_interface_requirements(
    value: &Value,
) -> CertResult<BTreeMap<(String, String), [u8; 32]>> {
    let rows = array(value)?;
    let mut requirements = BTreeMap::new();
    let mut previous = None;
    for row in rows {
        let row = sized(row, 3)?;
        let key = (string(&row[0])?.to_owned(), string(&row[1])?.to_owned());
        let seal = digest(&row[2])?;
        if key.0.is_empty()
            || key.1.is_empty()
            || seal == [0; 32]
            || string(&row[2])? != hex(&seal)
            || previous.as_ref().is_some_and(|owner| owner >= &key)
        {
            return Err(CertificationError::Receipt(
                "noncanonical interface owner seal",
            ));
        }
        previous = Some(key.clone());
        requirements.insert(key, seal);
    }
    Ok(requirements)
}

fn encode_interface_requirements(requirements: &BTreeMap<(String, String), [u8; 32]>) -> Value {
    value_array(requirements.iter().map(|((unit, module), seal)| {
        value_array([value_text(unit), value_text(module), value_text(hex(seal))])
    }))
}

/// Interface-only closure is separate from executable global/group requirements.
/// The inventory verifies these exact seals against the admitted owner graph.
pub(crate) fn original_interface_requirements(
    product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
) -> CertResult<BTreeMap<(String, String), [u8; 32]>> {
    original_interface_requirements_with_operation(
        product,
        &InventoryOperation::new(Default::default()),
    )
}

pub(crate) fn original_interface_requirements_with_operation(
    product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
    operation: &InventoryOperation,
) -> CertResult<BTreeMap<(String, String), [u8; 32]>> {
    match product.original_native() {
        Some(witness) if witness.matches_original(product) => {
            // A retained proof avoids decoding, but admission still reserves
            // its copied map nodes and owned text before cloning.
            operation.reserve::<((String, String), [u8; 32])>(
                witness.interface_requirements.len().checked_mul(4).ok_or(
                    tidepool_repr::execution_schema::ParseError::LimitExceeded("work"),
                )?,
            )?;
            for (unit, module) in witness.interface_requirements.keys() {
                operation.charge(unit.len())?;
                operation.charge(module.len())?;
            }
            Ok(witness.interface_requirements.clone())
        }
        Some(_) => Err(CertificationError::Mismatch(
            "original native witness bytes",
        )),
        None => {
            let witness =
                decode_home_witness_with_operation(product.certification_bytes(), operation)?;
            if &witness.owner != product.owner() {
                return Err(CertificationError::Mismatch("interface requirements owner"));
            }
            Ok(witness.interface_requirements)
        }
    }
}

fn home_owner(value: &Value) -> CertResult<CachedHomeOwner> {
    let row = sized(value, 5)?;
    let unit = string(&row[0])?.to_owned();
    let module = string(&row[1])?.to_owned();
    if unit.is_empty() || module.is_empty() {
        return Err(CertificationError::Receipt("empty home owner"));
    }
    Ok(CachedHomeOwner {
        unit,
        module,
        module_version: ModuleVersion(digest(&row[2])?),
        skinny_iface_sha256: digest(&row[3])?,
        product_sha256: digest(&row[4])?,
    })
}

fn value_text(text: impl Into<String>) -> Value {
    Value::Text(text.into())
}
fn value_array(items: impl IntoIterator<Item = Value>) -> Value {
    Value::Array(items.into_iter().collect())
}
fn value_identity(symbol: &SymbolIdentity) -> Value {
    value_array([
        value_text(&symbol.unit),
        value_text(&symbol.module),
        value_text(&symbol.namespace),
        value_text(&symbol.occurrence),
        symbol
            .record_parent
            .as_ref()
            .map_or(Value::Null, value_text),
    ])
}
fn value_home(owner: &CachedHomeOwner) -> Value {
    value_array([
        value_text(&owner.unit),
        value_text(&owner.module),
        value_text(hex(&owner.module_version.0)),
        value_text(hex(&owner.skinny_iface_sha256)),
        value_text(hex(&owner.product_sha256)),
    ])
}
fn value_rep(rep: &RuntimeRep) -> Value {
    let (tag, bits) = match rep {
        RuntimeRep::Void => ("void", 0),
        RuntimeRep::LiftedRef => ("lifted", 0),
        RuntimeRep::UnliftedRef => ("unlifted", 0),
        RuntimeRep::Address => ("address", 0),
        RuntimeRep::Int(bits) => ("int", *bits),
        RuntimeRep::Word(bits) => ("word", *bits),
        RuntimeRep::Float(bits) => ("float", *bits),
    };
    value_array([value_text(tag), Value::Integer(bits.into())])
}
fn value_signature(signature: &Option<Signature>) -> Value {
    let Some(signature) = signature else {
        return Value::Null;
    };
    let (tag, results) = match &signature.results {
        ResultContract::Returns(results) => ("returns", results.as_slice()),
        ResultContract::NoSuccess => ("no_success", &[][..]),
        ResultContract::CallerResult => ("caller_result", &[][..]),
    };
    value_array([
        value_array(signature.arguments.iter().map(value_rep)),
        value_array([value_text(tag), value_array(results.iter().map(value_rep))]),
    ])
}
fn value_import(owner: &ReceiptImportOwner) -> Value {
    match owner {
        ReceiptImportOwner::Source {
            unit,
            module,
            module_version,
            original_ordinal,
            binder,
        } => value_array([
            value_text("source"),
            value_text(unit),
            value_text(module),
            module_version
                .as_ref()
                .map_or(Value::Null, |version| value_text(hex(&version.0))),
            Value::Integer((*original_ordinal).into()),
            value_identity(binder),
        ]),
        ReceiptImportOwner::Retained {
            identity,
            generation,
        } => value_array([
            value_text("retained"),
            value_identity(identity),
            Value::Integer((*generation).into()),
        ]),
        ReceiptImportOwner::RetainedPackage {
            unit,
            module,
            binder,
            generation,
            interface_digest,
        } => value_array([
            value_text("retained-package"),
            value_text(unit),
            value_text(module),
            value_text(hex(interface_digest)),
            value_identity(binder),
            Value::Integer((*generation).into()),
        ]),
        ReceiptImportOwner::Package {
            unit,
            module,
            binder,
            interface_digest,
        } => value_array([
            value_text("package"),
            value_text(unit),
            value_text(module),
            value_text(hex(interface_digest)),
            value_identity(binder),
        ]),
    }
}
fn value_global(global: &AcceptedGlobal) -> Value {
    value_array([
        value_identity(&global.identity),
        value_rep(&global.rep),
        value_signature(&global.entry_signature),
        Value::Bool(global.required_evaluated),
        value_import(&global.owner),
    ])
}
fn encode_home_witness(witness: &HomeCertification) -> CertResult<Vec<u8>> {
    encode_home_witness_with_operation(witness, &InventoryOperation::new(Default::default()))
}

fn charge_symbol(operation: &InventoryOperation, symbol: &SymbolIdentity) -> CertResult<()> {
    operation.reserve::<Value>(8)?;
    for text in [
        &symbol.unit,
        &symbol.module,
        &symbol.namespace,
        &symbol.occurrence,
    ]
    .into_iter()
    .chain(symbol.record_parent.iter())
    {
        operation.charge(text.len())?;
    }
    Ok(())
}
fn charge_global(operation: &InventoryOperation, global: &AcceptedGlobal) -> CertResult<()> {
    charge_symbol(operation, &global.identity)?;
    operation.reserve::<Value>(16)?;
    match &global.owner {
        ReceiptImportOwner::Source {
            unit,
            module,
            binder,
            ..
        }
        | ReceiptImportOwner::Package {
            unit,
            module,
            binder,
            ..
        }
        | ReceiptImportOwner::RetainedPackage {
            unit,
            module,
            binder,
            ..
        } => {
            operation.charge(
                unit.len()
                    .checked_add(module.len())
                    .ok_or(CertificationError::Receipt("owner size"))?,
            )?;
            charge_symbol(operation, binder)?;
        }
        ReceiptImportOwner::Retained { identity, .. } => charge_symbol(operation, identity)?,
    }
    if let Some(signature) = &global.entry_signature {
        operation.reserve::<Value>(
            signature
                .arguments
                .len()
                .checked_mul(8)
                .ok_or(CertificationError::Receipt("signature size"))?,
        )?;
        operation.reserve::<Value>(
            signature
                .results
                .returned_reps()
                .map_or(0, |reps| reps.len())
                .checked_mul(8)
                .ok_or(CertificationError::Receipt("result size"))?,
        )?;
    }
    Ok(())
}
fn encode_home_witness_with_operation(
    witness: &HomeCertification,
    operation: &InventoryOperation,
) -> CertResult<Vec<u8>> {
    operation.reserve::<Value>(32)?;
    for (_, binders, globals) in &witness.groups {
        operation.reserve::<Value>(8)?;
        for binder in binders {
            charge_symbol(operation, binder)?;
        }
        for global in globals {
            charge_global(operation, global)?;
        }
    }
    for owner in std::iter::once(&witness.owner).chain(witness.sources.values()) {
        operation.reserve::<Value>(16)?;
        operation.charge(
            owner
                .unit
                .len()
                .checked_add(owner.module.len())
                .and_then(|size| size.checked_add(256))
                .ok_or(CertificationError::Receipt("home size"))?,
        )?;
    }
    for ((unit, module), package) in &witness.packages {
        operation.reserve::<Value>(8)?;
        operation.charge(
            unit.len()
                .checked_add(module.len())
                .and_then(|size| size.checked_add(package.selected_path.as_os_str().len()))
                .and_then(|size| size.checked_add(64))
                .ok_or(CertificationError::Receipt("package size"))?,
        )?;
    }
    for (unit, module) in witness.interface_requirements.keys() {
        operation.reserve::<Value>(8)?;
        operation.charge(
            unit.len()
                .checked_add(module.len())
                .and_then(|size| size.checked_add(64))
                .ok_or(CertificationError::Receipt("interface size"))?,
        )?;
    }
    let mut fields = vec![
        value_text("TPHOMEOWNERS"),
        Value::Integer(5.into()),
        value_home(&witness.owner),
        value_array(witness.groups.iter().map(|(ordinal, binders, globals)| {
            value_array([
                Value::Integer((*ordinal).into()),
                value_array(binders.iter().map(value_identity)),
                value_array(globals.iter().map(value_global)),
            ])
        })),
        value_array(witness.sources.values().map(value_home)),
        value_array(
            witness
                .packages
                .iter()
                .map(|((unit, module), package)| {
                    Ok(value_array([
                        value_text(unit),
                        value_text(module),
                        value_text(
                            package
                                .selected_path
                                .to_str()
                                .ok_or(CertificationError::Mismatch("package witness path"))?
                                .to_owned(),
                        ),
                        value_text(hex(&package.sha256)),
                    ]))
                })
                .collect::<CertResult<Vec<_>>>()?,
        ),
    ];
    fields.push(
        witness
            .execution_source_sha256
            .map_or(Value::Null, |digest| value_text(hex(&digest))),
    );
    fields.push(encode_interface_requirements(
        &witness.interface_requirements,
    ));
    fields.push(
        witness
            .finalized_module_sha256
            .map_or(Value::Null, |digest| value_text(hex(&digest))),
    );
    let value = value_array(fields);
    encode_value_with_operation(
        &value,
        CertificationFormat::HomeOwners,
        COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_module_bytes),
        operation,
    )
}

fn decode_home_witness(bytes: &[u8]) -> CertResult<HomeCertification> {
    decode_home_witness_with_operation(bytes, &InventoryOperation::new(Default::default()))
}
fn decode_home_witness_with_operation(
    bytes: &[u8],
    operation: &InventoryOperation,
) -> CertResult<HomeCertification> {
    #[cfg(test)]
    HOME_CERTIFICATION_DECODES.with(|count| count.set(count.get() + 1));
    let limit = COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_module_bytes);
    if bytes.len() > limit {
        return Err(CertificationError::SizeLimit {
            format: CertificationFormat::HomeOwners,
            actual: bytes.len(),
            limit,
        });
    }
    let value = operation.decode_value(bytes, limit)?;
    operation.charge_value_copies(&value, 3)?;
    let row = array(&value)?;
    if row.len() < 2 {
        return Err(CertificationError::Receipt("home witness header"));
    }
    if string(&row[0])? != "TPHOMEOWNERS" {
        return Err(CertificationError::Receipt("home witness header"));
    }
    let version = number(&row[1])?;
    if version != 5 {
        return Err(CertificationError::UnsupportedVersion {
            format: CertificationFormat::HomeOwners,
            found: version,
            expected: 5,
        });
    }
    let row = sized(&value, 9)?;
    let finalized_module_sha256 = match &row[8] {
        Value::Null => None,
        value => {
            let seal = digest(value)?;
            if seal == [0; 32] || string(value)? != hex(&seal) {
                return Err(CertificationError::Receipt("finalized module digest"));
            }
            Some(seal)
        }
    };
    let execution_source_sha256 = if !matches!(&row[6], Value::Null) {
        let digest = digest(&row[6])?;
        if digest == [0; 32] {
            return Err(CertificationError::Receipt("empty execution source digest"));
        }
        Some(digest)
    } else {
        None
    };
    let owner = home_owner(&row[2])?;
    let interface_requirements = decode_interface_requirements(&row[7])?;
    if interface_requirements.contains_key(&(owner.unit.clone(), owner.module.clone())) {
        return Err(CertificationError::Receipt("self interface requirement"));
    }
    let groups = array(&row[3])?;
    let mut ordinals = BTreeSet::new();
    let mut binders_seen = BTreeSet::new();
    let groups = groups
        .iter()
        .map(|group| {
            let group = sized(group, 3)?;
            let ordinal = u32::try_from(number(&group[0])?)
                .map_err(|_| CertificationError::Receipt("source group ordinal"))?;
            if !ordinals.insert(ordinal) {
                return Err(CertificationError::Receipt("duplicate group ordinal"));
            }
            let binders = array(&group[1])?
                .iter()
                .map(identity)
                .collect::<CertResult<Vec<_>>>()?;
            if binders.is_empty()
                || binders.iter().any(|binder| {
                    binder.unit != owner.unit
                        || binder.module != owner.module
                        || !binders_seen.insert(binder.clone())
                })
            {
                return Err(CertificationError::Receipt("home witness binders"));
            }
            let globals = array(&group[2])?;
            Ok((
                ordinal,
                binders,
                globals
                    .iter()
                    .map(accepted_global)
                    .collect::<CertResult<Vec<_>>>()?,
            ))
        })
        .collect::<CertResult<Vec<_>>>()?;
    let mut sources = BTreeMap::new();
    for source in array(&row[4])? {
        let source = home_owner(source)?;
        if sources
            .insert((source.unit.clone(), source.module.clone()), source)
            .is_some()
        {
            return Err(CertificationError::Receipt("duplicate source owner"));
        }
    }
    let mut packages = BTreeMap::new();
    if array(&row[5])?.len() > PACKAGE_LIMIT {
        return Err(CertificationError::Receipt("package count"));
    }
    for package in array(&row[5])? {
        let fields = sized(package, 4)?;
        let key = (
            string(&fields[0])?.to_owned(),
            string(&fields[1])?.to_owned(),
        );
        let selected_path = PathBuf::from(string(&fields[2])?);
        if key.0.is_empty()
            || key.1.is_empty()
            || !selected_path.is_absolute()
            || packages
                .insert(
                    key,
                    PackageInterfaceWitness {
                        selected_path,
                        sha256: digest(&fields[3])?,
                    },
                )
                .is_some()
        {
            return Err(CertificationError::Receipt("package witness"));
        }
    }
    let mut used_sources = BTreeSet::new();
    let mut used_packages = BTreeSet::new();
    for (_, _, globals) in &groups {
        for global in globals {
            match &global.owner {
                ReceiptImportOwner::Source {
                    unit,
                    module,
                    module_version,
                    binder,
                    ..
                } => {
                    let key = (unit.clone(), module.clone());
                    let source = sources
                        .get(&key)
                        .ok_or(CertificationError::Mismatch("home source witness"))?;
                    if module_version.as_ref() != Some(&source.module_version)
                        || binder != &global.identity
                        || binder.unit != *unit
                        || binder.module != *module
                    {
                        return Err(CertificationError::Mismatch("home source witness"));
                    }
                    used_sources.insert(key);
                }
                ReceiptImportOwner::Package {
                    unit,
                    module,
                    interface_digest,
                    binder,
                }
                | ReceiptImportOwner::RetainedPackage {
                    unit,
                    module,
                    interface_digest,
                    binder,
                    ..
                } => {
                    let key = (unit.clone(), module.clone());
                    if packages.get(&key).map(|witness| witness.sha256) != Some(*interface_digest)
                        || binder != &global.identity
                        || binder.unit != *unit
                        || binder.module != *module
                    {
                        return Err(CertificationError::Mismatch("home package witness"));
                    }
                    used_packages.insert(key);
                }
                ReceiptImportOwner::Retained { identity, .. } if identity != &global.identity => {
                    return Err(CertificationError::Mismatch("home retained witness"));
                }
                _ => {}
            }
        }
    }
    if used_sources.len() != sources.len() || used_packages.len() != packages.len() {
        return Err(CertificationError::Receipt("unused owner witness"));
    }
    let witness = HomeCertification {
        owner,
        execution_source_sha256,
        finalized_module_sha256,
        groups,
        sources,
        interface_requirements,
        packages,
    };
    validate_home_witness_structure(&witness)?;
    if encode_home_witness_with_operation(&witness, operation)? != bytes {
        return Err(CertificationError::Receipt("noncanonical home witness"));
    }
    Ok(witness)
}

fn validate_home_witness_structure(witness: &HomeCertification) -> CertResult<()> {
    if witness.owner.unit.is_empty() || witness.owner.module.is_empty() {
        return Err(CertificationError::Receipt("empty home owner"));
    }
    if witness.execution_source_sha256 == Some([0; 32]) {
        return Err(CertificationError::Receipt("empty execution source digest"));
    }
    let mut ordinals = BTreeSet::new();
    let mut binders_seen = BTreeSet::new();
    for (ordinal, binders, _) in &witness.groups {
        if !ordinals.insert(*ordinal) {
            return Err(CertificationError::Receipt("duplicate group ordinal"));
        }
        if binders.is_empty()
            || binders.iter().any(|binder| {
                binder.unit != witness.owner.unit
                    || binder.module != witness.owner.module
                    || !binders_seen.insert(binder.clone())
            })
        {
            return Err(CertificationError::Receipt("home witness binders"));
        }
    }
    for ((unit, module), source) in &witness.sources {
        if unit.is_empty() || module.is_empty() || unit != &source.unit || module != &source.module
        {
            return Err(CertificationError::Receipt("empty home owner"));
        }
    }
    if witness.packages.len() > PACKAGE_LIMIT {
        return Err(CertificationError::Receipt("package count"));
    }
    for ((unit, module), package) in &witness.packages {
        if unit.is_empty() || module.is_empty() || !package.selected_path.is_absolute() {
            return Err(CertificationError::Receipt("package witness"));
        }
    }
    if witness.finalized_module_sha256 == Some([0; 32])
        || witness
            .interface_requirements
            .iter()
            .any(|((unit, module), seal)| {
                unit.is_empty()
                    || module.is_empty()
                    || *seal == [0; 32]
                    || (unit == &witness.owner.unit && module == &witness.owner.module)
            })
    {
        return Err(CertificationError::Receipt(
            "invalid canonical interface requirements",
        ));
    }
    let mut used_sources = BTreeSet::new();
    let mut used_packages = BTreeSet::new();
    for (_, _, globals) in &witness.groups {
        for global in globals {
            match &global.owner {
                ReceiptImportOwner::Source {
                    unit,
                    module,
                    module_version,
                    binder,
                    ..
                } => {
                    let key = (unit.clone(), module.clone());
                    let source = witness
                        .sources
                        .get(&key)
                        .ok_or(CertificationError::Mismatch("home source witness"))?;
                    if module_version.as_ref() != Some(&source.module_version)
                        || binder != &global.identity
                        || binder.unit != *unit
                        || binder.module != *module
                    {
                        return Err(CertificationError::Mismatch("home source witness"));
                    }
                    used_sources.insert(key);
                }
                ReceiptImportOwner::Package {
                    unit,
                    module,
                    interface_digest,
                    binder,
                }
                | ReceiptImportOwner::RetainedPackage {
                    unit,
                    module,
                    interface_digest,
                    binder,
                    ..
                } => {
                    let key = (unit.clone(), module.clone());
                    if witness.packages.get(&key).map(|witness| witness.sha256)
                        != Some(*interface_digest)
                        || binder != &global.identity
                        || binder.unit != *unit
                        || binder.module != *module
                    {
                        return Err(CertificationError::Mismatch("home package witness"));
                    }
                    used_packages.insert(key);
                }
                ReceiptImportOwner::Retained { identity, .. } if identity != &global.identity => {
                    return Err(CertificationError::Mismatch("home retained witness"));
                }
                _ => {}
            }
        }
    }
    if used_sources.len() != witness.sources.len() || used_packages.len() != witness.packages.len()
    {
        return Err(CertificationError::Receipt("unused owner witness"));
    }
    Ok(())
}

/// Called by the recovery owner before retaining or materializing a seal.
pub fn validate_home_certification(bytes: &[u8], owner: &CachedHomeOwner) -> CertResult<()> {
    validate_home_certification_with_validation(
        bytes,
        owner,
        &mut PackageInterfaceValidation::default(),
    )
}

pub(crate) fn validate_home_certification_with_validation(
    bytes: &[u8],
    owner: &CachedHomeOwner,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<()> {
    verify_home_witness_with_validation(bytes, owner, validation).map(|_| ())
}

#[cfg(test)]
pub(crate) fn certified_home_requirements(
    bytes: &[u8],
    owner: &CachedHomeOwner,
) -> CertResult<Vec<CachedHomeOwner>> {
    Ok(verify_home_witness(bytes, owner)?
        .sources
        .into_values()
        .collect())
}

pub(crate) fn original_home_requirements_with_validation(
    product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<CachedHomeOwner>> {
    match product.original_native() {
        Some(witness) if witness.matches_original(product) => {
            for package in witness.packages.values() {
                verify_package_interface(validation, package)?;
            }
            Ok(witness.sources.clone())
        }
        Some(_) => Err(CertificationError::Mismatch(
            "original native witness bytes",
        )),
        None => Ok(verify_home_witness_with_validation(
            product.certification_bytes(),
            product.owner(),
            validation,
        )?
        .sources
        .into_values()
        .collect()),
    }
}

/// Private construction proves a native issuer selected this same canonical
/// carrier; recovery uses the full wire decoder instead.
#[cfg(test)]
pub(crate) fn fixture_module_interface(
    producer: [u8; 32],
    unit: &str,
    module: &str,
    requirements: BTreeMap<(String, String), [u8; 32]>,
) -> CertifiedModuleInterface {
    fixture_source_module_interface(producer, unit, module, [1; 32], requirements, None)
}

#[cfg(test)]
pub(crate) fn fixture_source_module_interface(
    producer: [u8; 32],
    unit: &str,
    module: &str,
    source_sha256: [u8; 32],
    requirements: BTreeMap<(String, String), [u8; 32]>,
    package: Option<&Path>,
) -> CertifiedModuleInterface {
    let bytes = module.as_bytes().to_vec();
    let value = value_array([
        value_text("TPPKGROOTS"),
        value_text("2"),
        value_array([
            value_text(unit),
            value_text(module),
            value_text(hex(&sha(&bytes))),
        ]),
        value_array(package.into_iter().map(|path| {
            value_array([
                value_text("package-unit"),
                value_text("Package.Module"),
                value_text(path.to_str().unwrap()),
                value_text(hex(&sha(&std::fs::read(path).unwrap()))),
            ])
        })),
        value_array([]),
    ]);
    let mut packages = Vec::new();
    ciborium::ser::into_writer(&value, &mut packages).unwrap();
    finalized_module::fixture_interface(
        producer,
        unit,
        module,
        source_sha256,
        bytes,
        packages,
        requirements,
        Some(b"fixture-core".to_vec()),
    )
}

#[cfg(test)]
pub(crate) fn fixture_module_source_imports(
    interface: CertifiedModuleInterface,
    imports: Vec<CanonicalSourceImport>,
) -> CertifiedModuleInterface {
    finalized_module::fixture_source_imports(interface, imports)
}

#[cfg(test)]
pub(crate) fn fixture_interface_bytes(
    producer: [u8; 32],
    unit: &str,
    module: &str,
    interface: Vec<u8>,
    packages: Vec<u8>,
) -> CertifiedModuleInterface {
    finalized_module::fixture_interface(
        producer,
        unit,
        module,
        [1; 32],
        interface,
        packages,
        BTreeMap::new(),
        Some(b"fixture-core".to_vec()),
    )
}

#[cfg(test)]
pub(crate) fn fixture_finalized_product(
    product: crate::recovery_artifacts::CertifiedRecoveryProduct,
    producer: [u8; 32],
) -> crate::recovery_artifacts::CertifiedRecoveryProduct {
    fixture_finalized_product_with_requirements(product, producer, None)
}

#[cfg(test)]
pub(crate) fn fixture_finalized_product_with_requirements(
    product: crate::recovery_artifacts::CertifiedRecoveryProduct,
    producer: [u8; 32],
    requirements: Option<BTreeMap<(String, String), [u8; 32]>>,
) -> crate::recovery_artifacts::CertifiedRecoveryProduct {
    fixture_finalized_product_inner(product, producer, requirements, None)
}

#[cfg(test)]
pub(crate) fn fixture_source_finalized_product(
    product: crate::recovery_artifacts::CertifiedRecoveryProduct,
    producer: [u8; 32],
    imports: Vec<CanonicalSourceImport>,
) -> crate::recovery_artifacts::CertifiedRecoveryProduct {
    fixture_finalized_product_inner(product, producer, None, Some(imports))
}

#[cfg(test)]
fn fixture_finalized_product_inner(
    product: crate::recovery_artifacts::CertifiedRecoveryProduct,
    producer: [u8; 32],
    requirements: Option<BTreeMap<(String, String), [u8; 32]>>,
    imports: Option<Vec<CanonicalSourceImport>>,
) -> crate::recovery_artifacts::CertifiedRecoveryProduct {
    let mut witness = decode_home_witness(product.certification_bytes()).unwrap();
    if let Some(requirements) = requirements {
        witness.interface_requirements = requirements;
    }
    let package_bytes = if product.package_imports_bytes().is_empty() {
        let value = value_array([
            value_text("TPPKGROOTS"),
            value_text("2"),
            value_array([
                value_text(&product.owner().unit),
                value_text(&product.owner().module),
                value_text(hex(&product.owner().skinny_iface_sha256)),
            ]),
            value_array([]),
            value_array([]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    } else {
        product.package_imports_bytes().to_vec()
    };
    let interface = finalized_module::fixture_interface(
        producer,
        &product.owner().unit,
        &product.owner().module,
        product.source_sha256().unwrap_or([1; 32]),
        product.interface_bytes().to_vec(),
        package_bytes.clone(),
        witness.interface_requirements.clone(),
        Some(b"fixture-core".to_vec()),
    );
    let interface = match imports {
        Some(imports) => finalized_module::fixture_source_imports(interface, imports),
        None => interface,
    };
    witness.finalized_module_sha256 = Some(sha(interface.certificate_bytes()));
    let binding = validate_module_binding(
        &witness,
        &interface,
        product.interface_bytes(),
        &package_bytes,
        product.source_sha256(),
    )
    .unwrap();
    let mut finalized =
        crate::recovery_artifacts::CertifiedRecoveryProduct::from_finalized_certification(
            product.owner().clone(),
            product.product_bytes().to_vec(),
            encode_home_witness(&witness).unwrap(),
            binding,
        );
    if let Some(source) = product.source_sha256() {
        finalized = finalized.with_source_sha256(source);
    }
    finalized
}

pub(crate) struct ValidatedModuleBinding(CertifiedModuleInterface);
impl ValidatedModuleBinding {
    pub(crate) fn into_interface(self) -> CertifiedModuleInterface {
        self.0
    }
}

fn validate_module_binding(
    witness: &HomeCertification,
    interface: &CertifiedModuleInterface,
    actual_interface: &[u8],
    actual_packages: &[u8],
    expected_source_sha256: Option<[u8; 32]>,
) -> CertResult<ValidatedModuleBinding> {
    if expected_source_sha256.is_some_and(|source| source != interface.source_sha256())
        || !interface.home_units().contains(&witness.owner.unit)
        || witness
            .sources
            .values()
            .any(|owner| !interface.home_units().contains(&owner.unit))
        || witness
            .packages
            .keys()
            .any(|(unit, _)| interface.home_units().contains(unit))
        || witness.finalized_module_sha256 != Some(sha(interface.certificate_bytes()))
        || interface.unit() != witness.owner.unit
        || interface.module() != witness.owner.module
        || interface.interface_sha256() != witness.owner.skinny_iface_sha256
        || interface.interface_bytes() != actual_interface
        || interface.package_imports_bytes() != actual_packages
        || interface.requirements() != &witness.interface_requirements
        || interface.core_bytes().is_none()
    {
        return Err(CertificationError::Mismatch(
            "native canonical module binding",
        ));
    }
    Ok(ValidatedModuleBinding(interface.clone()))
}

pub(crate) fn original_execution_source_digest_with_validation(
    product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Option<[u8; 32]>> {
    match product.original_native() {
        Some(witness) if witness.matches_original(product) => {
            for package in witness.packages.values() {
                verify_package_interface(validation, package)?;
            }
            Ok(witness.execution_source_sha256)
        }
        Some(_) => Err(CertificationError::Mismatch(
            "original native witness bytes",
        )),
        None => home_execution_source_digest_with_validation(
            product.certification_bytes(),
            product.owner(),
            validation,
        ),
    }
}

pub(crate) fn recover_module_interface(
    producer: [u8; 32],
    certificate: Vec<u8>,
    interface: Vec<u8>,
    packages: Vec<u8>,
    core: Option<Vec<u8>>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<CertifiedModuleInterface> {
    finalized_module::recover_interface(
        producer,
        certificate,
        interface,
        packages,
        core,
        validation,
    )
}

pub(crate) fn validate_original_module_interface(
    product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
    interface: &CertifiedModuleInterface,
) -> CertResult<()> {
    validate_canonical_native_bytes(
        product.owner(),
        product.certification_bytes(),
        product.interface_bytes(),
        product.package_imports_bytes(),
        product.source_sha256(),
        interface,
    )
}

pub(crate) fn validate_canonical_native_bytes(
    owner: &CachedHomeOwner,
    certification: &[u8],
    actual_interface: &[u8],
    actual_packages: &[u8],
    source_sha256: Option<[u8; 32]>,
    interface: &CertifiedModuleInterface,
) -> CertResult<()> {
    validate_canonical_native_bytes_with_operation(
        owner,
        certification,
        actual_interface,
        actual_packages,
        source_sha256,
        interface,
        &InventoryOperation::new(Default::default()),
    )
}

pub(crate) fn validate_canonical_native_bytes_with_operation(
    owner: &CachedHomeOwner,
    certification: &[u8],
    actual_interface: &[u8],
    actual_packages: &[u8],
    source_sha256: Option<[u8; 32]>,
    interface: &CertifiedModuleInterface,
    operation: &InventoryOperation,
) -> CertResult<()> {
    let witness = decode_home_witness_with_operation(certification, operation)?;

    if witness.owner != *owner {
        return Err(CertificationError::Mismatch(
            "native canonical source owner",
        ));
    }
    validate_module_binding(
        &witness,
        interface,
        actual_interface,
        actual_packages,
        source_sha256,
    )
    .map(|_| ())
}

pub(crate) fn original_native_requirements(
    product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
) -> CertResult<CertifiedNativeRequirements> {
    original_native_requirements_with_operation(
        product,
        &InventoryOperation::new(Default::default()),
    )
}

pub(crate) fn original_native_requirements_with_operation(
    product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
    operation: &InventoryOperation,
) -> CertResult<CertifiedNativeRequirements> {
    match product.original_native() {
        Some(witness) if witness.matches_original(product) => {
            charge_native_requirements(operation, &witness.native_requirements)?;
            Ok(witness.native_requirements.clone())
        }
        Some(_) => Err(CertificationError::Mismatch(
            "original native witness bytes",
        )),
        None => certified_native_requirements_with_operation(
            product.certification_bytes(),
            product.owner(),
            operation,
        ),
    }
}

/// Direct native requirements are distinct from typechecking closure. They
/// retain the dependent and required original group ordinals, or exact live
/// binding generation; graph reachability grants no native lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CertifiedNativeRequirements {
    pub group_ordinals: BTreeSet<u32>,
    pub artifact_edges: Vec<(
        crate::declaration_join::ExactModuleIdentity,
        crate::artifact_inventory::ArtifactDependency,
    )>,
    pub retained_packages: Vec<crate::artifact_inventory::RetainedPackageDependency>,
}

fn charge_native_requirements(
    operation: &InventoryOperation,
    requirements: &CertifiedNativeRequirements,
) -> CertResult<()> {
    operation.reserve::<u32>(requirements.group_ordinals.len())?;
    use crate::artifact_inventory::ArtifactDependency;
    operation.reserve::<(
        crate::declaration_join::ExactModuleIdentity,
        ArtifactDependency,
    )>(requirements.artifact_edges.len())?;
    for (owner, dependency) in &requirements.artifact_edges {
        operation.charge(owner.unit.len())?;
        operation.charge(owner.module.len())?;
        if let ArtifactDependency::NativeBinding {
            namespace,
            occurrence,
            record_parent,
            ..
        } = dependency
        {
            operation.charge(namespace.len())?;
            operation.charge(occurrence.len())?;
            if let Some(parent) = record_parent {
                operation.charge(parent.len())?;
            }
        }
    }
    operation.reserve::<crate::artifact_inventory::RetainedPackageDependency>(
        requirements.retained_packages.len(),
    )?;
    for package in &requirements.retained_packages {
        charge_symbol(operation, &package.identity)?;
    }
    Ok(())
}

pub(crate) fn certified_native_requirements(
    bytes: &[u8],
    owner: &CachedHomeOwner,
) -> CertResult<CertifiedNativeRequirements> {
    certified_native_requirements_with_operation(
        bytes,
        owner,
        &InventoryOperation::new(Default::default()),
    )
}
fn certified_native_requirements_with_operation(
    bytes: &[u8],
    owner: &CachedHomeOwner,
    operation: &InventoryOperation,
) -> CertResult<CertifiedNativeRequirements> {
    let witness = decode_home_witness_with_operation(bytes, operation)?;

    if &witness.owner != owner {
        return Err(CertificationError::Mismatch("native requirements owner"));
    }
    Ok(native_requirements_from_witness(&witness))
}

fn native_requirements_from_witness(witness: &HomeCertification) -> CertifiedNativeRequirements {
    use crate::artifact_inventory::ArtifactDependency;
    let mut requirements = Vec::new();
    let mut retained_packages = Vec::new();
    for (dependent_ordinal, _, globals) in &witness.groups {
        for global in globals {
            let (owner, dependency) = match &global.owner {
                ReceiptImportOwner::Source {
                    unit,
                    module,
                    original_ordinal,
                    ..
                } => (
                    crate::declaration_join::ExactModuleIdentity {
                        unit: unit.clone(),
                        module: module.clone(),
                    },
                    ArtifactDependency::NativeGroup {
                        dependent_ordinal: *dependent_ordinal,
                        required_ordinal: *original_ordinal,
                    },
                ),
                ReceiptImportOwner::Retained {
                    identity,
                    generation,
                } => (
                    crate::declaration_join::ExactModuleIdentity {
                        unit: identity.unit.clone(),
                        module: identity.module.clone(),
                    },
                    ArtifactDependency::NativeBinding {
                        dependent_ordinal: *dependent_ordinal,
                        generation: *generation,
                        namespace: identity.namespace.clone(),
                        occurrence: identity.occurrence.clone(),
                        record_parent: identity.record_parent.clone(),
                    },
                ),
                ReceiptImportOwner::RetainedPackage {
                    binder,
                    generation,
                    interface_digest,
                    ..
                } => {
                    retained_packages.push(crate::artifact_inventory::RetainedPackageDependency {
                        dependent_ordinal: *dependent_ordinal,
                        identity: binder.clone(),
                        generation: *generation,
                        interface_digest: *interface_digest,
                    });
                    continue;
                }
                ReceiptImportOwner::Package { .. } => continue,
            };
            requirements.push((owner, dependency));
        }
    }
    requirements.sort();
    requirements.dedup();
    retained_packages.sort();
    retained_packages.dedup();
    CertifiedNativeRequirements {
        group_ordinals: witness
            .groups
            .iter()
            .map(|(ordinal, _, _)| *ordinal)
            .collect(),
        artifact_edges: requirements,
        retained_packages,
    }
}

#[cfg(test)]
fn verify_home_witness(bytes: &[u8], owner: &CachedHomeOwner) -> CertResult<HomeCertification> {
    verify_home_witness_with_validation(bytes, owner, &mut PackageInterfaceValidation::default())
}

fn verify_home_witness_with_validation(
    bytes: &[u8],
    owner: &CachedHomeOwner,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<HomeCertification> {
    #[cfg(test)]
    {
        validation.home_witness_validations += 1;
    }
    let witness = decode_home_witness_with_operation(bytes, &validation.inventory)?;
    if &witness.owner != owner {
        return Err(CertificationError::Mismatch("home certification owner"));
    }
    for package in witness.packages.values() {
        verify_package_interface(validation, package)?;
    }
    Ok(witness)
}

#[cfg(test)]
pub(crate) fn bind_home_execution_source(
    bytes: &[u8],
    owner: &CachedHomeOwner,
    source_digest: [u8; 32],
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<u8>> {
    let mut witness = verify_home_witness_with_validation(bytes, owner, validation)?;
    if source_digest == [0; 32]
        || witness
            .execution_source_sha256
            .is_some_and(|original| original != source_digest)
    {
        return Err(CertificationError::Mismatch("execution source seal"));
    }
    witness.execution_source_sha256 = Some(source_digest);
    encode_home_witness(&witness)
}

pub(crate) fn home_certification_digests_with_validation(
    bytes: &[u8],
    owner: &CachedHomeOwner,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<(Option<[u8; 32]>, Option<[u8; 32]>)> {
    let witness = verify_home_witness_with_validation(bytes, owner, validation)?;
    Ok((
        witness.execution_source_sha256,
        witness.finalized_module_sha256,
    ))
}

pub(crate) fn home_execution_source_digest_with_validation(
    bytes: &[u8],
    owner: &CachedHomeOwner,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Option<[u8; 32]>> {
    Ok(verify_home_witness_with_validation(bytes, owner, validation)?.execution_source_sha256)
}

pub(crate) fn candidate_execution_source_digest_with_validation(
    bytes: &[u8],
    owner: &CachedHomeOwner,
    package_imports: &[u8],
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Option<[u8; 32]>> {
    let witness = verify_home_witness_with_validation(bytes, owner, validation)?;
    check_direct_package_agreement_with_validation(
        package_imports,
        owner,
        &witness.packages,
        validation,
    )?;
    Ok(witness.execution_source_sha256)
}

/// Seal original Rust-admitted ownership, without selecting new owners from
/// spellings. Even an instance-only Home with zero groups has an explicit seal.
pub fn encode_home_certification(
    owner: &CachedHomeOwner,
    groups: &[PendingCertifiedGroup],
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
) -> CertResult<Vec<u8>> {
    encode_home_certification_with_validation(
        owner,
        groups,
        packages,
        &BTreeMap::new(),
        &mut PackageInterfaceValidation::default(),
    )
}

/// Encode native evidence bound to one independently validated canonical module.
/// Complete product admission still decodes that certificate and its payloads.
pub fn encode_home_certification_with_module(
    owner: &CachedHomeOwner,
    groups: &[PendingCertifiedGroup],
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    interface_requirements: &BTreeMap<(String, String), [u8; 32]>,
    module_certificate_sha256: [u8; 32],
) -> CertResult<Vec<u8>> {
    let witness = issue_home_certification_with_validation(
        owner,
        groups,
        packages,
        interface_requirements,
        None,
        Some(module_certificate_sha256),
        &mut PackageInterfaceValidation::default(),
    )?;
    encode_home_witness(&witness)
}

fn encode_home_certification_with_validation(
    owner: &CachedHomeOwner,
    groups: &[PendingCertifiedGroup],
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    interface_requirements: &BTreeMap<(String, String), [u8; 32]>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<u8>> {
    let witness = issue_home_certification_with_validation(
        owner,
        groups,
        packages,
        interface_requirements,
        None,
        None,
        validation,
    )?;
    encode_home_witness(&witness)
}

fn issue_home_certification_with_validation(
    owner: &CachedHomeOwner,
    groups: &[PendingCertifiedGroup],
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    interface_requirements: &BTreeMap<(String, String), [u8; 32]>,
    execution_source_sha256: Option<[u8; 32]>,
    finalized_module_sha256: Option<[u8; 32]>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<HomeCertification> {
    let mut witness = HomeCertification {
        owner: owner.clone(),
        execution_source_sha256,
        finalized_module_sha256,
        groups: Vec::new(),
        sources: BTreeMap::new(),
        interface_requirements: interface_requirements.clone(),
        packages: BTreeMap::new(),
    };
    for group in groups {
        if group.owner() != owner {
            return Err(CertificationError::Mismatch(
                "home certification group owner",
            ));
        }
        if group.group.globals().len() != group.imports.len() {
            return Err(CertificationError::Mismatch(
                "home certification global count",
            ));
        }
        let mut globals = Vec::new();
        for (declaration, selected) in group.group.globals().iter().zip(group.imports.iter()) {
            let selected = match selected {
                PendingImportOwner::Source {
                    owner,
                    original_ordinal,
                    binder,
                } => {
                    let key = (owner.unit.clone(), owner.module.clone());
                    if witness
                        .sources
                        .insert(key, owner.clone())
                        .is_some_and(|old| old != *owner)
                    {
                        return Err(CertificationError::Mismatch("ambiguous home source owner"));
                    }
                    ReceiptImportOwner::Source {
                        unit: owner.unit.clone(),
                        module: owner.module.clone(),
                        module_version: Some(owner.module_version.clone()),
                        original_ordinal: *original_ordinal,
                        binder: binder.clone(),
                    }
                }
                PendingImportOwner::Retained {
                    identity,
                    generation,
                } => ReceiptImportOwner::Retained {
                    identity: identity.clone(),
                    generation: *generation,
                },
                PendingImportOwner::RetainedPackage {
                    unit,
                    module,
                    binder,
                    generation,
                    interface_digest,
                } => {
                    let key = (unit.clone(), module.clone());
                    let package = packages
                        .get(&key)
                        .ok_or(CertificationError::Mismatch("package interface witness"))?;
                    witness.packages.insert(key, package.clone());
                    ReceiptImportOwner::RetainedPackage {
                        unit: unit.clone(),
                        module: module.clone(),
                        binder: binder.clone(),
                        generation: *generation,
                        interface_digest: *interface_digest,
                    }
                }
                PendingImportOwner::Package {
                    unit,
                    module,
                    binder,
                    interface_digest,
                } => {
                    let key = (unit.clone(), module.clone());
                    let package = packages
                        .get(&key)
                        .ok_or(CertificationError::Mismatch("package interface witness"))?;
                    witness.packages.insert(key, package.clone());
                    ReceiptImportOwner::Package {
                        unit: unit.clone(),
                        module: module.clone(),
                        binder: binder.clone(),
                        interface_digest: *interface_digest,
                    }
                }
            };
            let global = AcceptedGlobal {
                identity: declaration.identity.clone(),
                rep: declaration.rep.clone(),
                entry_signature: declaration
                    .entry_signature
                    .and_then(|id| group.group.definitions().signatures().get(id.0 as usize))
                    .cloned(),
                required_evaluated: declaration.required_evaluated,
                owner: selected,
            };
            validate_global_witness(declaration, group.group.definitions().signatures(), &global)?;
            globals.push(global);
        }
        witness.groups.push((
            group.group.original_ordinal(),
            group.group.binders().to_vec(),
            globals,
        ));
    }
    validate_home_witness_structure(&witness)?;
    for package in witness.packages.values() {
        verify_package_interface(validation, package)?;
    }
    Ok(witness)
}

/// Re-admit complete original products using durable ownership receipts. Source
/// cycles are resolved after the entire current/inherited inventory is built.
pub fn certify_inherited_products(
    inputs: &[InheritedProductInput<'_>],
    current_groups: &[PendingCertifiedGroup],
) -> CertResult<Vec<PendingCertifiedGroup>> {
    certify_inherited_products_with_validation(
        inputs,
        current_groups,
        &mut PackageInterfaceValidation::default(),
    )
}

pub(crate) fn certify_inherited_products_with_validation(
    inputs: &[InheritedProductInput<'_>],
    current_groups: &[PendingCertifiedGroup],
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<PendingCertifiedGroup>> {
    let mut modules = BTreeSet::new();
    let mut parsed = Vec::new();
    for input in inputs {
        let reference = &input.artifact.reference;
        if !modules.insert((reference.unit.clone(), reference.module.clone())) {
            return Err(CertificationError::Mismatch("duplicate inherited module"));
        }
        parsed.push(capture_inherited_product_with_validation(
            input, validation,
        )?);
    }
    certify_inherited_inventory_with_validation(parsed, current_groups, validation)
}

fn capture_inherited_product_with_validation(
    input: &InheritedProductInput<'_>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<(RawModuleProduct, HomeCertification)> {
    let requirements = crate::prepared_artifact::production_requirements()
        .map_err(|_| CertificationError::Mismatch("production requirements"))?;
    let artifact = input.artifact;
    let reference = &artifact.reference;
    let owner = CachedHomeOwner {
        unit: reference.unit.clone(),
        module: reference.module.clone(),
        module_version: ModuleVersion(reference.module_version),
        skinny_iface_sha256: reference.skinny_iface_sha256,
        product_sha256: reference.product_sha256,
    };
    if sha(&artifact.certification_bytes) != reference.certification_sha256 {
        return Err(CertificationError::Mismatch("inherited artifact bytes"));
    }
    capture_original_product_with_validation(
        &owner,
        &artifact.interface_bytes,
        &artifact.product_bytes,
        &artifact.package_imports_bytes,
        &artifact.certification_bytes,
        &requirements,
        validation,
    )
}

/// Scratch materialization writes already-owned certified bytes. Reuse those
/// bytes for native admission; cold recovery still authenticates disk captures,
/// and worker-output admission still checks consumed materializations.
#[cfg(test)]
pub(crate) fn certify_owned_products_with_validation(
    products: &[&crate::recovery_artifacts::CertifiedRecoveryProduct],
    current_groups: &[PendingCertifiedGroup],
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<PendingCertifiedGroup>> {
    certify_owned_products_in_context_with_validation(
        products,
        current_groups,
        products,
        validation,
    )
}

pub(crate) fn certify_owned_products_in_context_with_validation(
    products: &[&crate::recovery_artifacts::CertifiedRecoveryProduct],
    current_groups: &[PendingCertifiedGroup],
    available: &[&crate::recovery_artifacts::CertifiedRecoveryProduct],
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<PendingCertifiedGroup>> {
    if products.is_empty() {
        return Ok(Vec::new());
    }
    let mut current_homes = BTreeMap::new();
    let mut current_by_owner: BTreeMap<(String, String), BTreeMap<u32, &PendingCertifiedGroup>> =
        BTreeMap::new();
    for group in current_groups {
        let owner = group.owner();
        if current_homes
            .insert((owner.unit.clone(), owner.module.clone()), owner.clone())
            .is_some_and(|old| old != *owner)
        {
            return Err(CertificationError::Mismatch(
                "ambiguous current home module",
            ));
        }
        if current_by_owner
            .entry((owner.unit.clone(), owner.module.clone()))
            .or_default()
            .insert(group.group.original_ordinal(), group)
            .is_some()
        {
            return Err(CertificationError::Mismatch(
                "duplicate current original ordinal",
            ));
        }
    }
    let mut homes = current_homes.clone();
    for product in available {
        let owner = product.owner();
        if homes
            .insert((owner.unit.clone(), owner.module.clone()), owner.clone())
            .is_some_and(|old| old != *owner)
        {
            return Err(CertificationError::Mismatch(
                "ambiguous inherited/current home module",
            ));
        }
    }
    for group in current_groups {
        for import in group.imports() {
            if let PendingImportOwner::Package { unit, module, .. }
            | PendingImportOwner::RetainedPackage { unit, module, .. } = import
            {
                if homes.contains_key(&(unit.clone(), module.clone())) {
                    return Err(CertificationError::Mismatch(
                        "home owner downgraded to package",
                    ));
                }
            }
        }
    }
    if products
        .iter()
        .any(|product| product.original_native().is_none())
    {
        let result = certify_owned_uncaptured_products_with_validation(
            products,
            current_groups,
            validation,
        )?;
        // Legacy constructors still enter the full byte-based validator. The
        // selected context also includes zero-group originals when excluding
        // package downgrades.
        for group in &result {
            for import in group.imports() {
                if let PendingImportOwner::Package { unit, module, .. }
                | PendingImportOwner::RetainedPackage { unit, module, .. } = import
                {
                    if homes.contains_key(&(unit.clone(), module.clone())) {
                        return Err(CertificationError::Mismatch(
                            "home owner downgraded to package",
                        ));
                    }
                }
            }
        }
        return Ok(result);
    }
    let mut sources = certified_source_map(current_groups)?;
    let mut seen = BTreeSet::new();
    let mut shared = BTreeSet::new();
    for product in products {
        let witness = product
            .original_native()
            .expect("all original native witnesses selected");
        if !witness.matches_original(product) {
            return Err(CertificationError::Mismatch(
                "original native witness bytes",
            ));
        }
        let key = (witness.owner.unit.clone(), witness.owner.module.clone());
        if !seen.insert(key.clone()) {
            return Err(CertificationError::Mismatch("duplicate inherited module"));
        }
        if current_homes.contains_key(&key) {
            let current = &current_by_owner[&key];
            if current.len() != witness.groups.len()
                || witness.groups.iter().any(|original| {
                    current
                        .get(&original.group.original_ordinal())
                        .is_none_or(|group| {
                            !(Arc::ptr_eq(&group.group, &original.group)
                                || group.group == original.group)
                                || !(Arc::ptr_eq(&group.imports, &original.imports)
                                    || group.imports == original.imports)
                        })
                })
            {
                return Err(CertificationError::Mismatch("shared original home groups"));
            }
            shared.insert(key);
        } else {
            for group in witness.groups.iter() {
                for binder in group.group.binders() {
                    sources.insert_unique(
                        (
                            witness.owner.clone(),
                            group.group.original_ordinal(),
                            binder.clone(),
                        ),
                        (witness.owner.clone(), ProductOrigin::Cached),
                        SourceBinderPhase::InheritedNativeWitness,
                    )?;
                }
            }
        }
    }
    let mut result = Vec::new();
    for product in products {
        let witness = product
            .original_native()
            .expect("all original native witnesses selected");
        if homes.get(&(witness.owner.unit.clone(), witness.owner.module.clone()))
            != Some(&witness.owner)
            || witness
                .sources
                .iter()
                .any(|owner| homes.get(&(owner.unit.clone(), owner.module.clone())) != Some(owner))
        {
            return Err(CertificationError::Mismatch(
                "inherited source owner closure",
            ));
        }
        for group in witness.groups.iter() {
            for import in group.imports() {
                match import {
                    PendingImportOwner::Source {
                        owner,
                        original_ordinal,
                        binder,
                    } => {
                        if sources
                            .get(&(owner.clone(), *original_ordinal, binder.clone()))
                            .is_none_or(|(selected, _)| selected != owner)
                        {
                            return Err(CertificationError::Mismatch(
                                "source binder/group closure",
                            ));
                        }
                    }
                    PendingImportOwner::Package {
                        unit,
                        module,
                        binder,
                        interface_digest,
                    }
                    | PendingImportOwner::RetainedPackage {
                        unit,
                        module,
                        binder,
                        interface_digest,
                        ..
                    } => {
                        if homes.contains_key(&(unit.clone(), module.clone())) {
                            return Err(CertificationError::Mismatch(
                                "home owner downgraded to package",
                            ));
                        }
                        validate_package_owner(
                            unit,
                            module,
                            binder,
                            interface_digest,
                            &sources,
                            &witness.packages,
                            validation,
                        )?;
                    }
                    PendingImportOwner::Retained { .. } => {}
                }
            }
        }
        if !shared.contains(&(witness.owner.unit.clone(), witness.owner.module.clone())) {
            result.extend(
                witness
                    .groups
                    .iter()
                    .map(AuthenticatedOriginalGroup::admitted),
            );
        }
    }
    Ok(result)
}

/// Admit executable groups independently of immutable original byte custody.
/// The selection comes from the inventory's authenticated demand closure; full
/// original witnesses remain complete and can supply later group admissions.
pub(crate) fn certify_selected_owned_products_in_context_with_validation(
    available: &BTreeMap<
        crate::artifact_inventory::ArtifactId,
        &crate::recovery_artifacts::CertifiedRecoveryProduct,
    >,
    current_groups: &[PendingCertifiedGroup],
    selected: &BTreeSet<crate::artifact_inventory::NativeGroupKey>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<PendingCertifiedGroup>> {
    use crate::artifact_inventory::NativeGroupKey;
    let mut native_ids = std::collections::HashMap::new();
    let mut home_modules = BTreeSet::new();
    for (id, product) in available {
        let owner = product.owner();
        if native_ids
            .insert(owner.clone(), *id)
            .is_some_and(|old| old != *id)
        {
            return Err(CertificationError::Mismatch(
                "ambiguous exact original native identity",
            ));
        }
        home_modules.insert((owner.unit.clone(), owner.module.clone()));
    }
    let mut originals = BTreeMap::new();
    for key in selected {
        let product = available
            .get(&key.artifact)
            .ok_or(CertificationError::Mismatch("selected original artifact"))?;
        let witness = product
            .original_native()
            .ok_or(CertificationError::Mismatch(
                "selected original native witness",
            ))?;
        if !witness.matches_original(product) {
            return Err(CertificationError::Mismatch(
                "original native witness bytes",
            ));
        }
        let group = witness
            .group(key.original_ordinal)
            .ok_or(CertificationError::Mismatch("selected original group"))?;
        if originals.insert(*key, (group, witness)).is_some() {
            return Err(CertificationError::Mismatch(
                "duplicate selected original ordinal",
            ));
        }
    }
    let mut inherited = BTreeSet::new();
    for group in current_groups {
        let key = NativeGroupKey {
            artifact: *native_ids
                .get(group.owner())
                .ok_or(CertificationError::Mismatch(
                    "current group has no exact original identity",
                ))?,
            original_ordinal: group.group().original_ordinal(),
        };
        let (original, _) = originals.get(&key).ok_or(CertificationError::Mismatch(
            "current group outside selected closure",
        ))?;
        if original.owner != group.owner
            || !(Arc::ptr_eq(&original.group, &group.group) || original.group == group.group)
            || !(Arc::ptr_eq(&original.imports, &group.imports)
                || original.imports == group.imports)
        {
            return Err(CertificationError::Mismatch("shared original home groups"));
        }
        if !inherited.insert(key) {
            return Err(CertificationError::Mismatch(
                "duplicate current original ordinal",
            ));
        }
    }
    let mut additional = Vec::new();
    // Package refusal uses the complete admitted home census. Native source
    // membership uses exact artifact keys, never a module-only source map.
    let package_sources = SourceGroupMap::new();
    for (key, (group, witness)) in &originals {
        for import in group.imports() {
            match import {
                PendingImportOwner::Source {
                    owner,
                    original_ordinal,
                    binder,
                } => {
                    let artifact = native_ids
                        .get(owner)
                        .ok_or(CertificationError::Mismatch("source binder/group closure"))?;
                    let required = NativeGroupKey {
                        artifact: *artifact,
                        original_ordinal: *original_ordinal,
                    };
                    if originals.get(&required).is_none_or(|(original, _)| {
                        original.owner != *owner || !original.group.binders().contains(binder)
                    }) {
                        return Err(CertificationError::Mismatch("source binder/group closure"));
                    }
                }
                PendingImportOwner::Package {
                    unit,
                    module,
                    binder,
                    interface_digest,
                }
                | PendingImportOwner::RetainedPackage {
                    unit,
                    module,
                    binder,
                    interface_digest,
                    ..
                } => {
                    if home_modules.contains(&(unit.clone(), module.clone())) {
                        return Err(CertificationError::Mismatch(
                            "home owner downgraded to package",
                        ));
                    }
                    validate_package_owner(
                        unit,
                        module,
                        binder,
                        interface_digest,
                        &package_sources,
                        &witness.packages,
                        validation,
                    )?;
                }
                PendingImportOwner::Retained { .. } => {}
            }
        }
        if !inherited.contains(key) {
            additional.push(group.admitted());
        }
    }
    Ok(additional)
}

fn certify_owned_uncaptured_products_with_validation(
    products: &[&crate::recovery_artifacts::CertifiedRecoveryProduct],
    current_groups: &[PendingCertifiedGroup],
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<PendingCertifiedGroup>> {
    let requirements = crate::prepared_artifact::production_requirements()
        .map_err(|_| CertificationError::Mismatch("production requirements"))?;
    let mut modules = BTreeSet::new();
    let parsed = products
        .iter()
        .map(|product| {
            let owner = product.owner();
            if !modules.insert((owner.unit.clone(), owner.module.clone())) {
                return Err(CertificationError::Mismatch("duplicate inherited module"));
            }
            capture_original_product_with_validation(
                owner,
                product.interface_bytes(),
                product.product_bytes(),
                product.package_imports_bytes(),
                product.certification_bytes(),
                &requirements,
                validation,
            )
        })
        .collect::<CertResult<Vec<_>>>()?;
    certify_inherited_inventory_with_validation(parsed, current_groups, validation)
}

fn capture_original_product_with_validation(
    owner: &CachedHomeOwner,
    interface_bytes: &[u8],
    product_bytes: &[u8],
    package_imports_bytes: &[u8],
    certification_bytes: &[u8],
    requirements: &tidepool_repr::execution_schema::ProgramRequirements,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<(RawModuleProduct, HomeCertification)> {
    if sha(interface_bytes) != owner.skinny_iface_sha256
        || sha(product_bytes) != owner.product_sha256
    {
        return Err(CertificationError::Mismatch("inherited artifact bytes"));
    }
    #[cfg(test)]
    ORIGINAL_PRODUCT_DECODES.with(|count| count.set(count.get() + 1));
    let mut products = validation
        .inventory
        .parse_module_products(product_bytes, requirements)?;
    if products.len() != 1 {
        return Err(CertificationError::Mismatch("inherited per-module product"));
    }
    let product = products.pop().expect("one original product checked");
    if product.unit != owner.unit
        || product.module != owner.module
        || product.interface != interface_bytes
    {
        return Err(CertificationError::Mismatch(
            "inherited interface/product pair",
        ));
    }
    let witness = verify_home_witness_with_validation(certification_bytes, owner, validation)?;
    check_direct_package_agreement_with_validation(
        package_imports_bytes,
        owner,
        &witness.packages,
        validation,
    )?;
    Ok((product, witness))
}

/// Authenticate every retained original once. Different lexical views may
/// retain different versions of one module; source witnesses select a full
/// owner here, while recovery context admission later selects one per view.
pub(crate) struct RecoveryOriginalRequirements {
    pub owner: CachedHomeOwner,
    pub sources: Vec<CachedHomeOwner>,
    pub packages: Vec<crate::declaration_join::ExactModuleIdentity>,
}

pub(crate) struct CertifiedRecoveredOriginal {
    pub(crate) producer_sha256: [u8; 32],
    pub(crate) product: crate::recovery_artifacts::CertifiedRecoveryProduct,
    pub(crate) requirements: RecoveryOriginalRequirements,
}

pub(crate) fn certify_recovery_products_with_validation(
    artifacts: Vec<crate::recovery_artifacts::VerifiedRecoveryArtifact>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<CertifiedRecoveredOriginal>> {
    let parsed = artifacts
        .iter()
        .map(|artifact| {
            capture_inherited_product_with_validation(
                &InheritedProductInput { artifact },
                validation,
            )
        })
        .collect::<CertResult<Vec<_>>>()?;
    let mut original_owners = std::collections::HashSet::new();
    for (product, witness) in &parsed {
        if !original_owners.insert(witness.owner.clone()) {
            return Err(CertificationError::Mismatch("duplicate recovery original"));
        }
        validate_inherited_group_headers(product, witness)?;
    }
    let mut resolved = Vec::with_capacity(parsed.len());
    for (product, witness) in &parsed {
        let mut scoped_owners = BTreeMap::new();
        for owner in std::iter::once(&witness.owner).chain(witness.sources.values()) {
            let key = (owner.unit.clone(), owner.module.clone());
            if scoped_owners
                .insert(key, owner)
                .is_some_and(|old| old != owner)
            {
                return Err(CertificationError::Mismatch(
                    "ambiguous recovery source owner",
                ));
            }
        }
        let mut group_imports = Vec::with_capacity(product.groups.len());
        for (group, (_, _, globals)) in product.groups.iter().zip(&witness.groups) {
            let mut imports = Vec::with_capacity(globals.len());
            for (declaration, selected) in group.globals().iter().zip(globals) {
                if let ReceiptImportOwner::Package { unit, module, .. }
                | ReceiptImportOwner::RetainedPackage { unit, module, .. } = &selected.owner
                {
                    if scoped_owners.contains_key(&(unit.clone(), module.clone())) {
                        return Err(CertificationError::Mismatch(
                            "home owner downgraded to package",
                        ));
                    }
                }
                let import = validate_global_witness(
                    declaration,
                    group.definitions().signatures(),
                    selected,
                )?;
                let resolved_import = match import {
                    source @ ReceiptImportOwner::Source { .. } => {
                        resolve_cold_recovery_source(source, witness, product)?
                    }
                    other => resolve_receipt_owner_with_validation(
                        other,
                        &SourceGroupMap::new(),
                        None,
                        &witness.packages,
                        validation,
                    )?,
                };
                imports.push(resolved_import);
            }
            group_imports.push(imports);
        }
        resolved.push(group_imports);
    }
    artifacts
        .into_iter()
        .zip(parsed)
        .zip(resolved)
        .map(|((artifact, (raw, witness)), imports)| {
            let requirements = RecoveryOriginalRequirements {
                owner: witness.owner.clone(),
                sources: witness.sources.values().cloned().collect(),
                packages: witness
                    .packages
                    .keys()
                    .map(
                        |(unit, module)| crate::declaration_join::ExactModuleIdentity {
                            unit: unit.clone(),
                            module: module.clone(),
                        },
                    )
                    .collect(),
            };
            let groups = raw
                .groups
                .into_iter()
                .zip(imports)
                .map(|(group, imports)| AuthenticatedOriginalGroup {
                    owner: witness.owner.clone(),
                    group: Arc::new(group),
                    imports: imports.into(),
                })
                .collect();
            let producer_sha256 = artifact.reference.toolchain_identity_sha256;
            let mut product =
                crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
                    witness.owner.clone(),
                    artifact.interface_bytes,
                    artifact.product_bytes,
                    artifact.package_imports_bytes,
                    artifact.certification_bytes,
                );
            product = product
                .with_module_interface_with_validation(artifact.module_interface, validation)
                .map_err(|_| CertificationError::Mismatch("recovered canonical module"))?;
            if let Some(graph) = artifact.execution_source {
                product = product
                    .with_execution_source_with_validation(graph, validation)
                    .map_err(|_| CertificationError::Mismatch("execution source owner"))?;
            }
            let product = retain_authenticated_original_native(product, groups, witness)?;
            Ok(CertifiedRecoveredOriginal {
                producer_sha256,
                product,
                requirements,
            })
        })
        .collect()
}

fn validate_inherited_group_headers(
    product: &RawModuleProduct,
    witness: &HomeCertification,
) -> CertResult<()> {
    if product.groups.len() != witness.groups.len() {
        return Err(CertificationError::Mismatch(
            "inherited original group count",
        ));
    }
    for (group, (ordinal, binders, globals)) in product.groups.iter().zip(&witness.groups) {
        if group.original_ordinal() != *ordinal
            || group.binders() != binders
            || group.globals().len() != globals.len()
        {
            return Err(CertificationError::Mismatch(
                "inherited original group/globals",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
fn certify_inherited_inventory(
    parsed: Vec<(RawModuleProduct, HomeCertification)>,
    current_groups: &[PendingCertifiedGroup],
) -> CertResult<Vec<PendingCertifiedGroup>> {
    certify_inherited_inventory_with_validation(
        parsed,
        current_groups,
        &mut PackageInterfaceValidation::default(),
    )
}

fn certify_inherited_inventory_with_validation(
    parsed: Vec<(RawModuleProduct, HomeCertification)>,
    current_groups: &[PendingCertifiedGroup],
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<PendingCertifiedGroup>> {
    let mut sources = certified_source_map(current_groups)?;
    let mut homes = BTreeMap::new();
    let mut shared = BTreeSet::new();
    let mut inherited_seen = BTreeSet::new();
    for current in current_groups {
        let owner = current.owner();
        if homes
            .insert((owner.unit.clone(), owner.module.clone()), owner.clone())
            .is_some_and(|old| old != *owner)
        {
            return Err(CertificationError::Mismatch(
                "ambiguous current home module",
            ));
        }
    }
    for (product, witness) in &parsed {
        let owner = &witness.owner;
        let key = (owner.unit.clone(), owner.module.clone());
        if !inherited_seen.insert(key.clone()) {
            return Err(CertificationError::Mismatch(
                "duplicate inherited home module",
            ));
        }
        if let Some(current_owner) = homes.get(&key) {
            if current_owner != owner {
                return Err(CertificationError::Mismatch(
                    "ambiguous inherited/current home module",
                ));
            }
            let current: Vec<_> = current_groups
                .iter()
                .filter(|group| group.owner() == owner)
                .collect();
            if current.len() != product.groups.len()
                || product
                    .groups
                    .iter()
                    .any(|original| !current.iter().any(|group| group.group() == original))
            {
                return Err(CertificationError::Mismatch("shared original home groups"));
            }
            shared.insert(key);
        } else {
            homes.insert(key, owner.clone());
        }
        validate_inherited_group_headers(product, witness)?;
        for (group, (ordinal, _, _)) in product.groups.iter().zip(&witness.groups) {
            for binder in group.binders() {
                if shared.contains(&(owner.unit.clone(), owner.module.clone())) {
                    continue;
                }
                sources.insert_unique(
                    (owner.clone(), *ordinal, binder.clone()),
                    (owner.clone(), ProductOrigin::Cached),
                    SourceBinderPhase::InheritedInventory,
                )?;
            }
        }
    }
    let mut result = Vec::new();
    for (product, witness) in parsed {
        for source in witness.sources.values() {
            if homes.get(&(source.unit.clone(), source.module.clone())) != Some(source) {
                return Err(CertificationError::Mismatch(
                    "inherited source owner closure",
                ));
            }
        }
        for group in product.groups.iter().cloned() {
            let (_, _, globals) = witness
                .groups
                .iter()
                .find(|(ordinal, _, _)| *ordinal == group.original_ordinal())
                .ok_or(CertificationError::Mismatch("inherited group witness"))?;
            let imports = group
                .globals()
                .iter()
                .zip(globals)
                .map(|(declaration, selected)| {
                    if let ReceiptImportOwner::Package { unit, module, .. }
                    | ReceiptImportOwner::RetainedPackage { unit, module, .. } = &selected.owner
                    {
                        if homes.contains_key(&(unit.clone(), module.clone())) {
                            return Err(CertificationError::Mismatch(
                                "home owner downgraded to package",
                            ));
                        }
                    }
                    let import = validate_global_witness(
                        declaration,
                        group.definitions().signatures(),
                        selected,
                    )?;
                    match import {
                        source @ ReceiptImportOwner::Source { .. } => {
                            let resolved =
                                resolve_cold_recovery_source(source, &witness, &product)?;
                            let PendingImportOwner::Source {
                                owner,
                                original_ordinal,
                                binder,
                            } = &resolved
                            else {
                                return Err(CertificationError::Mismatch(
                                    "expected recovery source owner",
                                ));
                            };
                            if sources
                                .get(&(owner.clone(), *original_ordinal, binder.clone()))
                                .is_none()
                            {
                                return Err(CertificationError::Mismatch(
                                    "source binder/group closure",
                                ));
                            }
                            Ok(resolved)
                        }
                        other => resolve_receipt_owner_with_validation(
                            other,
                            &sources,
                            None,
                            &witness.packages,
                            validation,
                        ),
                    }
                })
                .collect::<CertResult<Vec<_>>>()?;
            if shared.contains(&(witness.owner.unit.clone(), witness.owner.module.clone())) {
                let current = current_groups
                    .iter()
                    .find(|current| current.owner() == &witness.owner && current.group() == &group)
                    .ok_or(CertificationError::Mismatch("shared original home groups"))?;
                if current.imports() != imports {
                    return Err(CertificationError::Mismatch("shared home import ownership"));
                }
                continue;
            }
            result.push(PendingCertifiedGroup {
                origin: ProductOrigin::Cached,
                owner: witness.owner.clone(),
                group: Arc::new(group),
                imports: imports.into(),
            });
        }
    }
    Ok(result)
}

/// Bounded decoded products tied to the exact immutable bytes that issued them.
/// Keeping construction private prevents pairing a different inventory with a
/// valid sidecar. Shared consumers compare their observed bytes before reuse.
pub(crate) struct ParsedModuleProducts {
    operation: Arc<InventoryOperation>,
    bytes: Arc<[u8]>,
    package_bundle: Arc<[u8]>,
    products: Vec<RawModuleProduct>,
    sidecars: Vec<Vec<u8>>,
    package_imports: Option<BTreeMap<(String, String), Vec<u8>>>,
}

impl ParsedModuleProducts {
    pub(crate) fn decode(bytes: &[u8], package_bundle: &[u8]) -> CertResult<Self> {
        Self::decode_with_operation(
            bytes,
            package_bundle,
            Arc::new(InventoryOperation::new(Default::default())),
        )
    }
    pub(crate) fn operation(&self) -> &Arc<InventoryOperation> {
        &self.operation
    }
    pub(crate) fn decode_with_operation(
        bytes: &[u8],
        package_bundle: &[u8],
        operation: Arc<InventoryOperation>,
    ) -> CertResult<Self> {
        let requirements = crate::prepared_artifact::production_requirements()
            .map_err(|_| CertificationError::Mismatch("production requirements"))?;
        let (products, sidecars) =
            operation.parse_module_products_with_framing(bytes, &requirements)?;
        let package_imports = crate::module_candidates::split_package_imports_with_operation(
            package_bundle,
            &products,
            &operation,
        )?;
        operation.charge(bytes.len())?;
        operation.charge(package_bundle.len())?;
        Ok(Self {
            operation,
            bytes: Arc::from(bytes),
            package_bundle: Arc::from(package_bundle),
            products,
            sidecars,
            package_imports,
        })
    }

    /// Reuse is representation-only: every consumer still supplies and checks
    /// its physical bytes under the same admission's accounting owner.
    pub(crate) fn validate_observation(
        &self,
        bytes: &[u8],
        package_bundle: &[u8],
        operation: &Arc<InventoryOperation>,
    ) -> CertResult<()> {
        if !Arc::ptr_eq(&self.operation, operation) {
            return Err(CertificationError::Mismatch(
                "shared inventory accounting owner",
            ));
        }
        operation.charge(bytes.len())?;
        operation.charge(package_bundle.len())?;
        if self.bytes.as_ref() != bytes || self.package_bundle.as_ref() != package_bundle {
            return Err(CertificationError::Mismatch(
                "physical segment original inventory changed",
            ));
        }
        Ok(())
    }

    /// Candidate publication needs independent mutable container ownership;
    /// recursive-group definitions remain their existing immutable shared data.
    pub(crate) fn copy_for_publication(&self) -> CertResult<Self> {
        self.operation
            .reserve::<RawModuleProduct>(self.products.len())?;
        for product in &self.products {
            self.operation.charge(product.unit.len())?;
            self.operation.charge(product.module.len())?;
            self.operation.charge(product.interface.len())?;
            self.operation
                .reserve::<tidepool_repr::execution_schema::ProjectedGroup>(product.groups.len())?;
        }
        self.operation.reserve::<Vec<u8>>(self.sidecars.len())?;
        for bytes in &self.sidecars {
            self.operation.charge(bytes.len())?;
        }
        if let Some(packages) = &self.package_imports {
            self.operation
                .reserve::<((String, String), Vec<u8>, [usize; 4])>(packages.len())?;
            for ((unit, module), bytes) in packages {
                self.operation.charge(unit.len())?;
                self.operation.charge(module.len())?;
                self.operation.charge(bytes.len())?;
            }
        }
        Ok(Self {
            operation: self.operation.clone(),
            bytes: self.bytes.clone(),
            package_bundle: self.package_bundle.clone(),
            products: self.products.clone(),
            sidecars: self.sidecars.clone(),
            package_imports: self.package_imports.clone(),
        })
    }

    pub(crate) fn aggregate_bytes_len(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) fn products(&self) -> &[RawModuleProduct] {
        &self.products
    }

    pub(crate) fn into_products(self) -> Vec<RawModuleProduct> {
        self.products
    }

    pub(crate) fn into_publication_parts(
        self,
    ) -> (
        Arc<InventoryOperation>,
        Vec<RawModuleProduct>,
        Vec<Vec<u8>>,
        Option<BTreeMap<(String, String), Vec<u8>>>,
    ) {
        (
            self.operation,
            self.products,
            self.sidecars,
            self.package_imports,
        )
    }

    fn sidecars(&self) -> CertResult<BTreeMap<(String, String), &[u8]>> {
        if self
            .sidecars
            .iter()
            .any(|bytes| bytes.len() > self.operation.limits().max_module_bytes)
        {
            return Err(CertificationError::Mismatch("fresh module product framing"));
        }
        Ok(self
            .products
            .iter()
            .zip(&self.sidecars)
            .map(|(product, bytes)| {
                (
                    (product.unit.clone(), product.module.clone()),
                    bytes.as_slice(),
                )
            })
            .collect())
    }
}

/// A shared offer outlives individual program slots. Recheck its accepted
/// cached subgraph against this slot's growing exact context before inherited
/// groups can participate in global-owner resolution.
fn matches_cached_original_reoffer(
    product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
    owner: &CachedHomeOwner,
    product_bytes: &[u8],
    package_bytes: &[u8],
    canonical: &CertifiedModuleInterface,
) -> bool {
    product.owner() == owner
        && product
            .original_native()
            .is_some_and(|native| native.matches_original(product))
        && product.product_bytes() == product_bytes
        && product.package_imports_bytes() == package_bytes
        && product.module_interface().is_some_and(|original| {
            original.certificate_bytes() == canonical.certificate_bytes()
                && original.interface_bytes() == canonical.interface_bytes()
        })
}

fn receipt_module_index(
    receipt: &CertifiedReceipt,
) -> CertResult<BTreeMap<(String, String), &CertifiedModuleReceipt>> {
    let mut accepted = BTreeMap::new();
    for module in &receipt.modules {
        if accepted
            .insert((module.unit.clone(), module.module.clone()), module)
            .is_some()
        {
            return Err(CertificationError::Mismatch("duplicate receipt module"));
        }
    }
    Ok(accepted)
}

#[cfg(test)]
fn validate_exact_cached_closure(
    candidates: Option<&CandidateSet>,
    receipt: &CertifiedReceipt,
    context: &crate::declaration_context::ExactDeclarationContext,
    evidence: &DependencyEvidence,
    exact_imports: &BTreeMap<
        crate::declaration_join::ExactModuleIdentity,
        Vec<crate::declaration_join::ExactModuleIdentity>,
    >,
) -> CertResult<()> {
    let accepted = receipt_module_index(receipt)?;
    if accepted
        .values()
        .all(|module| module.origin != ProductOrigin::Cached)
    {
        return Ok(());
    }
    let metadata = context
        .compiler_metadata_snapshot()
        .map_err(|_| CertificationError::Mismatch("compiler original projection"))?;
    validate_exact_cached_closure_with_validation(
        candidates,
        receipt,
        context,
        &metadata,
        evidence,
        exact_imports,
        &mut PackageInterfaceValidation::default(),
    )
}

fn validate_exact_cached_closure_with_validation(
    candidates: Option<&CandidateSet>,
    receipt: &CertifiedReceipt,
    context: &crate::declaration_context::ExactDeclarationContext,
    metadata: &crate::artifact_inventory::ArtifactMetadataSnapshot,
    evidence: &DependencyEvidence,
    exact_imports: &BTreeMap<
        crate::declaration_join::ExactModuleIdentity,
        Vec<crate::declaration_join::ExactModuleIdentity>,
    >,
    package_validation: &mut PackageInterfaceValidation,
) -> CertResult<()> {
    use crate::cache::ImportQualifier;
    use crate::module_candidates::dependencies::CandidateDependencyInventory;
    let accepted = receipt_module_index(receipt)?;
    if accepted
        .values()
        .all(|module| module.origin != ProductOrigin::Cached)
    {
        return Ok(());
    }
    let mut protected = BTreeSet::new();
    for entry in context.interface_owners() {
        protected.insert((entry.owner.unit, entry.owner.module));
        protected.extend(
            entry
                .requirements
                .into_iter()
                .map(|owner| (owner.unit, owner.module)),
        );
    }
    for node in context.lexical_graph() {
        protected.insert((node.owner.unit.clone(), node.owner.module.clone()));
        protected.extend(
            node.imports
                .iter()
                .map(|owner| (owner.unit.clone(), owner.module.clone())),
        );
    }
    // The current generated owner is not yet part of the admitted context.
    protected.extend(
        evidence
            .modules
            .iter()
            .filter(|row| row.source == Path::new("@generated-source"))
            .map(|row| (row.unit.clone(), row.module.clone())),
    );
    // An offered dependency rejected by the worker cannot satisfy this receipt.
    let originals = metadata
        .artifacts
        .values()
        .filter_map(|entry| match &entry.payload {
            crate::artifact_inventory::ArtifactPayload::Original(product) => Some(product.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let projected_originals = metadata
        .entries
        .values()
        .filter_map(|entry| match &entry.payload {
            crate::artifact_inventory::ArtifactPayload::Original(product) => Some(product.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut reoffers = BTreeMap::new();
    let mut accepted_candidates = BTreeMap::new();
    for (key, module) in &accepted {
        if module.origin != ProductOrigin::Cached {
            continue;
        }
        let bundle = candidates
            .and_then(|set| set.by_owner.get(key))
            .ok_or(CertificationError::Mismatch("selected candidate"))?;
        if bundle.owner.unit != module.unit
            || bundle.owner.module != module.module
            || module.module_version.as_ref() != Some(&bundle.owner.module_version)
            || module.skinny_iface_sha256 != bundle.owner.skinny_iface_sha256
            || module.product_sha256 != bundle.owner.product_sha256
        {
            return Err(CertificationError::Mismatch(
                "cached candidate dependency body/version",
            ));
        }
        let matching = originals
            .iter()
            .filter(|product| product.owner() == &bundle.owner)
            .collect::<Vec<_>>();
        let original = match matching.as_slice() {
            [] => None,
            [product] => {
                if !matches_cached_original_reoffer(
                    product,
                    &bundle.owner,
                    bundle.product.bytes(),
                    &bundle.package_imports_bytes,
                    &bundle.original_module_interface,
                ) {
                    return Err(CertificationError::Mismatch(
                        "cached original reoffer bytes",
                    ));
                }
                Some(*product)
            }
            _ => {
                return Err(CertificationError::Mismatch(
                    "duplicate available original owner",
                ))
            }
        };
        if protected.contains(key) && original.is_none() {
            return Err(CertificationError::Mismatch(
                "cached candidate overlaps exact owner",
            ));
        }
        if let Some(original) = original {
            reoffers.insert(key.clone(), original);
        }
        accepted_candidates.insert(key.clone(), bundle);
    }
    let inventory = CandidateDependencyInventory::from_candidates(
        accepted_candidates.values().copied(),
        &projected_originals,
    );
    for (key, bundle) in &accepted_candidates {
        let identity = crate::declaration_join::ExactModuleIdentity {
            unit: key.0.clone(),
            module: key.1.clone(),
        };
        let original_dependencies: BTreeSet<_> = match reoffers.get(key) {
            Some(product) => {
                let native = product
                    .original_native()
                    .ok_or(CertificationError::Mismatch(
                        "available original native witness",
                    ))?;
                native
                    .sources
                    .iter()
                    .map(|owner| (owner.unit.clone(), owner.module.clone()))
                    .chain(native.interface_requirements.keys().cloned())
                    .collect()
            }
            None => {
                let dependencies = match &bundle.original_execution {
                    Some(proof) => inventory
                        .direct_originals(&bundle.owner, &proof.graph)
                        .map_err(|_| {
                            CertificationError::Mismatch(
                                "cached candidate exact dependency closure",
                            )
                        })?,
                    None => BTreeMap::new(),
                };
                let exact_edges = exact_imports
                    .get(&identity)
                    .into_iter()
                    .flatten()
                    .map(|owner| (owner.unit.clone(), owner.module.clone()))
                    .collect::<BTreeSet<_>>();
                if exact_edges != dependencies.keys().cloned().collect() {
                    return Err(CertificationError::Mismatch(
                        "cached candidate exact dependency authority",
                    ));
                }
                dependencies.into_keys().collect()
            }
        };
        let current = evidence
            .modules
            .iter()
            .filter(|row| {
                row.unit == key.0
                    && row.module == key.1
                    && !row.boot
                    && row.product == ProductAvailability::Ready
            })
            .collect::<Vec<_>>();
        let [current] = current.as_slice() else {
            return Err(CertificationError::Mismatch(
                "cached candidate current source owner",
            ));
        };
        if std::fs::canonicalize(&current.source).ok().as_ref() != Some(&bundle.source) {
            return Err(CertificationError::Mismatch(
                "cached candidate current source path",
            ));
        }
        let original = bundle
            .evidence
            .modules
            .iter()
            .filter(|row| row.unit == key.0 && row.module == key.1 && !row.boot)
            .collect::<Vec<_>>();
        let [original] = original.as_slice() else {
            return Err(CertificationError::Mismatch(
                "cached candidate original source owner",
            ));
        };
        let package_roots =
            crate::recovery_artifacts::validate_package_import_evidence_with_validation(
                &bundle.package_imports_bytes,
                &key.0,
                &key.1,
                &bundle.owner.skinny_iface_sha256,
                &bundle.package_imports_path,
                package_validation,
            )
            .map_err(|_| CertificationError::Mismatch("cached candidate package witness"))?;
        let mut ordinary_imports = Vec::new();
        for edge in &original.imports {
            let original_matches = original_dependencies
                .iter()
                .filter(|(unit, module)| {
                    module == &edge.module
                        && !edge.boot
                        && match &edge.qualifier {
                            ImportQualifier::Unqualified => true,
                            ImportQualifier::ThisUnit(qualified) => qualified == unit,
                            ImportQualifier::OtherUnit(_) => false,
                        }
                })
                .collect::<Vec<_>>();
            if let [original_key] = original_matches.as_slice() {
                if let Some(path) = &edge.selected {
                    let selected = std::fs::canonicalize(path).map_err(|_| {
                        CertificationError::Mismatch("cached candidate selected home path")
                    })?;
                    let count = bundle
                        .evidence
                        .modules
                        .iter()
                        .filter(|row| {
                            row.unit == original_key.0
                                && row.module == original_key.1
                                && !row.boot
                                && std::fs::canonicalize(&row.source).ok().as_ref()
                                    == Some(&selected)
                        })
                        .count();
                    if count != 1 {
                        return Err(CertificationError::Mismatch(
                            "cached candidate selected home owner",
                        ));
                    }
                } else if crate::module_candidates::package_edge_matches(edge, &package_roots) {
                    return Err(CertificationError::Mismatch(
                        "cached candidate ambiguous original edge",
                    ));
                }
                // Exact source evidence removes these edges; its separately
                // checked receipt proves current lexical/source authority.
                continue;
            }
            ordinary_imports.push(edge);
            let Some(path) = &edge.selected else {
                if !crate::module_candidates::package_edge_matches(edge, &package_roots) {
                    return Err(CertificationError::Mismatch(
                        "cached candidate package edge",
                    ));
                }
                continue;
            };
            if edge.boot {
                return Err(CertificationError::Mismatch(
                    "cached candidate home body edge",
                ));
            }
            let selected = std::fs::canonicalize(path)
                .map_err(|_| CertificationError::Mismatch("cached candidate selected home path"))?;
            let source_rows = bundle
                .evidence
                .modules
                .iter()
                .filter(|row| {
                    row.module == edge.module
                        && !row.boot
                        && std::fs::canonicalize(&row.source).ok().as_ref() == Some(&selected)
                        && match &edge.qualifier {
                            ImportQualifier::Unqualified => true,
                            ImportQualifier::ThisUnit(unit) => &row.unit == unit,
                            ImportQualifier::OtherUnit(_) => false,
                        }
                })
                .collect::<Vec<_>>();
            let [selected_owner] = source_rows.as_slice() else {
                return Err(CertificationError::Mismatch(
                    "cached candidate selected home owner",
                ));
            };
            let dependency_key = (selected_owner.unit.clone(), selected_owner.module.clone());
            let dependency =
                accepted_candidates
                    .get(&dependency_key)
                    .ok_or(CertificationError::Mismatch(
                        "cached candidate dependency is not accepted",
                    ))?;
            if dependency.source != selected {
                return Err(CertificationError::Mismatch(
                    "cached candidate dependency body/version",
                ));
            }
            let selected_rows = evidence
                .modules
                .iter()
                .filter(|row| {
                    row.unit == dependency_key.0
                        && row.module == dependency_key.1
                        && !row.boot
                        && row.product == ProductAvailability::Ready
                        && std::fs::canonicalize(&row.source).ok().as_ref() == Some(&selected)
                })
                .count();
            if selected_rows != 1 {
                return Err(CertificationError::Mismatch(
                    "cached candidate dependency current path",
                ));
            }
        }
        if ordinary_imports.len() != current.imports.len()
            || ordinary_imports
                .iter()
                .zip(&current.imports)
                .any(|(old, new)| {
                    old.qualifier != new.qualifier
                        || old.module != new.module
                        || old.boot != new.boot
                        || old.selected != new.selected
                })
        {
            return Err(CertificationError::Mismatch("candidate direct imports"));
        }
    }
    Ok(())
}

/// Recheck original/fresh sidecars and dependency bytes against a worker
/// receipt. A candidate is never admitted merely because its name or hash
/// appears in the receipt: every original global and source edge is checked.
/// The compiler output owner supplies the captured payload root independently
/// of the source path used to authenticate dependency and source evidence.
#[cfg(test)]
pub(crate) fn certify_products(
    candidates: Option<&CandidateSet>,
    receipt: &CertifiedReceipt,
    fresh_products: &ParsedModuleProducts,
    fresh_evidence_bytes: &[u8],
    fresh_input_path: &Path,
    captured_payload_root: &Path,
    final_evidence: &CompletedSourceEvidence,
    final_target_source: &str,
    endpoint_identity: &[u8],
    include: &[PathBuf],
    exact: Option<&crate::declaration_context::ExactProductAdmission<'_>>,
    authored: Option<&crate::declaration_join::NativeAuthoredDeclarationAdmission>,
) -> CertResult<CertifiedProducts> {
    certify_products_with_validation(
        candidates,
        receipt,
        fresh_products,
        fresh_evidence_bytes,
        fresh_input_path,
        captured_payload_root,
        final_evidence,
        final_target_source,
        endpoint_identity,
        include,
        exact,
        authored,
        &[],
        None,
        &mut PackageInterfaceValidation::with_inventory(fresh_products.operation().clone()),
    )
}

pub(crate) fn certify_products_with_validation(
    candidates: Option<&CandidateSet>,
    receipt: &CertifiedReceipt,
    fresh_products: &ParsedModuleProducts,
    fresh_evidence_bytes: &[u8],
    fresh_input_path: &Path,
    captured_payload_root: &Path,
    final_evidence: &CompletedSourceEvidence,
    final_target_source: &str,
    endpoint_identity: &[u8],
    include: &[PathBuf],
    exact: Option<&crate::declaration_context::ExactProductAdmission<'_>>,
    authored: Option<&crate::declaration_join::NativeAuthoredDeclarationAdmission>,
    selected_session_values: &[tidepool_repr::SessionModule],
    produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<CertifiedProducts> {
    if !Arc::ptr_eq(&validation.inventory, fresh_products.operation()) {
        return Err(CertificationError::Mismatch("inventory accounting owner"));
    }
    receipt
        .finalization
        .validate_owners(&receipt.modules, &receipt.packages)?;
    let evidence_start = std::time::Instant::now();
    let normalized = match exact {
        None => CompletedSourceEvidence::from_worker(
            fresh_evidence_bytes,
            fresh_input_path,
            final_target_source,
        )
        .ok_or(CertificationError::StaleEvidence)?,
        Some(admission) => {
            admission
                .source
                .validate_ineligible_evidence(fresh_evidence_bytes)
                .map_err(|_| CertificationError::Mismatch("exact fresh evidence"))?;
            admission.source.evidence.clone()
        }
    };
    if serde_json::to_vec(&normalized)
        .map_err(|_| CertificationError::Mismatch("fresh evidence encoding"))?
        != serde_json::to_vec(final_evidence)
            .map_err(|_| CertificationError::Mismatch("fresh evidence encoding"))?
    {
        return Err(CertificationError::Mismatch("fresh evidence bytes"));
    }
    if final_evidence.revalidate(final_target_source).is_err() {
        return Err(CertificationError::StaleEvidence);
    }
    if let Some(admission) = authored {
        let owner = admission.owner();
        let rows = final_evidence
            .modules
            .iter()
            .filter(|row| row.unit == owner.unit && row.module == owner.module && !row.boot)
            .collect::<Vec<_>>();
        let [node] = rows.as_slice() else {
            return Err(CertificationError::Mismatch(
                "reserved authored source owner",
            ));
        };
        let products = receipt
            .modules
            .iter()
            .filter(|row| row.unit == owner.unit && row.module == owner.module)
            .collect::<Vec<_>>();
        let [accepted] = products.as_slice() else {
            return Err(CertificationError::Mismatch(
                "reserved authored native owner",
            ));
        };
        let source_matches = if node.is_generated_source() {
            admission.source_path() == fresh_input_path
        } else {
            std::fs::canonicalize(&node.source)
                .ok()
                .zip(std::fs::canonicalize(admission.source_path()).ok())
                .is_some_and(|(actual, expected)| actual == expected)
        };
        if !source_matches
            || ready_source_sha(final_evidence, &owner.unit, &owner.module)?
                != admission.source_sha256()
            || accepted.origin != ProductOrigin::Fresh
            || !receipt
                .finalization
                .modules
                .contains_key(&(owner.unit.clone(), owner.module.clone()))
        {
            return Err(CertificationError::Mismatch(
                "reserved authored source admission",
            ));
        }
    }
    crate::timing::record_stage(
        crate::timing::NO_NODE,
        crate::timing::NO_ROUND,
        "products.certify_evidence",
        evidence_start.elapsed(),
        fresh_evidence_bytes.len() as u64,
    );
    let producer_sha256 =
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
            endpoint_identity,
        )
        .sha256();
    let compiler_inputs = exact
        .map(|admission| admission.request.compiler_inputs())
        .transpose()
        .map_err(|_| CertificationError::Mismatch("compiler original projection"))?;
    let projected_metadata = compiler_inputs.as_ref().map(|inputs| &inputs.metadata);
    let inherited_module_interfaces =
        projected_metadata
            .as_ref()
            .map_or_else(Vec::new, |metadata| {
                metadata
                    .entries
                    .values()
                    .filter_map(|entry| match &entry.payload {
                        crate::artifact_inventory::ArtifactPayload::Canonical(interface) => {
                            Some(interface.clone())
                        }
                        crate::artifact_inventory::ArtifactPayload::Original(product) => {
                            product.module_interface().cloned()
                        }
                        crate::artifact_inventory::ArtifactPayload::Interface(_, _) => None,
                    })
                    .collect()
            });
    let projected_originals = projected_metadata
        .as_ref()
        .map_or_else(Vec::new, |metadata| {
            metadata
                .entries
                .values()
                .filter_map(|entry| match &entry.payload {
                    crate::artifact_inventory::ArtifactPayload::Original(product) => {
                        Some(product.clone())
                    }
                    _ => None,
                })
                .collect()
        });
    let inherited_products = projected_metadata.map_or_else(Vec::new, |metadata| {
        metadata
            .artifacts
            .values()
            .filter_map(|entry| match &entry.payload {
                crate::artifact_inventory::ArtifactPayload::Original(product) => {
                    Some(product.clone())
                }
                _ => None,
            })
            .collect()
    });
    let mut retained_seals = BTreeMap::new();
    for interface in inherited_module_interfaces.iter().chain(
        projected_originals
            .iter()
            .filter_map(|product| product.module_interface()),
    ) {
        let key = (interface.unit().to_owned(), interface.module().to_owned());
        if retained_seals
            .insert(key, interface.interface_sha256())
            .is_some_and(|previous| previous != interface.interface_sha256())
        {
            return Err(CertificationError::Mismatch(
                "conflicting inherited canonical owner",
            ));
        }
    }
    if projected_metadata
        .into_iter()
        .flat_map(|metadata| metadata.artifacts.values())
        .any(|entry| entry.descriptor.producer_sha256 != producer_sha256)
    {
        return Err(CertificationError::Mismatch(
            "inherited finalization producer",
        ));
    }
    let parsed_fresh = fresh_products.products();
    let fresh_product_bytes = fresh_products.bytes.as_ref();
    if let Some(admission) = exact {
        validate_exact_cached_closure_with_validation(
            candidates,
            receipt,
            &admission.request.context,
            projected_metadata
                .ok_or(CertificationError::Mismatch("compiler original projection"))?,
            final_evidence,
            &admission.source.exact_imports,
            validation,
        )?;
    }
    let ownership_start = std::time::Instant::now();
    let fresh_sidecars = fresh_products.sidecars()?;
    let fresh_package_imports = fresh_products
        .package_imports
        .as_ref()
        .ok_or(CertificationError::Mismatch("fresh package import framing"))?;
    let mut source_packages = BTreeMap::new();
    for product in parsed_fresh {
        let key = (product.unit.clone(), product.module.clone());
        let sidecar = fresh_package_imports
            .get(&key)
            .ok_or(CertificationError::Mismatch("fresh package imports"))?;
        let iface_sha: [u8; 32] = sha(&product.interface);
        let imported_packages =
            crate::recovery_artifacts::validate_package_imports_with_validation(
                sidecar,
                &product.unit,
                &product.module,
                &iface_sha,
                Path::new("module-package-imports.cbor"),
                validation,
            )
            .map_err(|_| CertificationError::Mismatch("fresh package import witness"))?;
        let promoted = receipt.modules.iter().any(|module| {
            module.unit == product.unit
                && module.module == product.module
                && module.origin == ProductOrigin::RetainedCore
        });
        for (owner, (selected_path, sha256)) in imported_packages {
            if promoted {
                continue;
            }
            let witness = PackageInterfaceWitness {
                selected_path,
                sha256: crate::execution_source::parse_digest(&sha256)
                    .map_err(|error| CertificationError::ExecutionSource(Box::new(error)))?,
            };
            if source_packages
                .insert(owner, witness.clone())
                .is_some_and(|old| old != witness)
            {
                return Err(CertificationError::Mismatch(
                    "source package import conflict",
                ));
            }
        }
    }
    let mut seen_modules = BTreeSet::new();
    let mut origin_counts = [[0_u64; 3]; 3];
    let mut fresh_modules = BTreeSet::new();
    let mut groups = Vec::new();
    validation.inventory.reserve::<(
        CachedHomeOwner,
        [u8; 32],
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        ProductOrigin,
    )>(receipt.modules.len())?;
    let mut module_bytes = Vec::with_capacity(receipt.modules.len());
    // Fresh and retained-Core rows refer to these immutable aggregate buffers.
    // Authenticate them once, while retaining each module's independent checks.
    let fresh_digests = receipt
        .modules
        .iter()
        .any(|module| {
            matches!(
                module.origin,
                ProductOrigin::Fresh | ProductOrigin::RetainedCore
            )
        })
        .then(|| {
            let start = std::time::Instant::now();
            let digests = (sha(fresh_product_bytes), sha(fresh_evidence_bytes));
            crate::timing::record_stage(
                crate::timing::NO_NODE,
                crate::timing::NO_ROUND,
                "products.certify_fresh_buffers",
                start.elapsed(),
                (fresh_product_bytes.len() + fresh_evidence_bytes.len()) as u64,
            );
            digests
        });
    let mut candidate_evidence_validation =
        crate::module_candidates::shared_evidence::ValidationStage::publication();
    for accepted in &receipt.modules {
        let key = (accepted.unit.clone(), accepted.module.clone());
        if !seen_modules.insert(key.clone()) {
            return Err(CertificationError::Mismatch("duplicate receipt module"));
        }
        let (
            product,
            receipt_bytes,
            product_bytes,
            package_bytes,
            evidence_digest,
            source_sha,
            version,
        ) = match accepted.origin {
            ProductOrigin::Fresh => {
                fresh_modules.insert(key.clone());
                if accepted.module_version.is_some() {
                    return Err(CertificationError::Mismatch(
                        "fresh version must be derived",
                    ));
                }
                let product = matching_product(parsed_fresh, &key.0, &key.1)?;
                let module_bytes = fresh_sidecars
                    .get(&key)
                    .ok_or(CertificationError::Mismatch("fresh module product"))?;
                let source_sha = ready_source_sha(final_evidence, &key.0, &key.1)?;
                let version = if let Some(admission) = exact {
                    crate::module_candidates::exact_module_version_for_product(
                        endpoint_identity,
                        &admission.request.semantic_sha256,
                        &key.0,
                        &key.1,
                        &source_sha,
                        &product.interface,
                        module_bytes,
                        fresh_package_imports
                            .get(&key)
                            .ok_or(CertificationError::Mismatch("fresh package imports"))?,
                    )
                } else {
                    fresh_module_version(
                        endpoint_identity,
                        include,
                        &source_sha,
                        &product.interface,
                        module_bytes,
                        fresh_package_imports
                            .get(&key)
                            .ok_or(CertificationError::Mismatch("fresh package imports"))?,
                    )?
                };
                (
                    product,
                    fresh_product_bytes,
                    *module_bytes,
                    fresh_package_imports
                        .get(&key)
                        .ok_or(CertificationError::Mismatch("fresh package imports"))?
                        .as_slice(),
                    fresh_digests
                        .expect("fresh receipt has aggregate digests")
                        .1,
                    source_sha,
                    version,
                )
            }
            ProductOrigin::Cached => {
                let bundle = candidates
                    .and_then(|set| set.by_owner.get(&key))
                    .ok_or(CertificationError::Mismatch("selected candidate"))?;
                if accepted.module_version.as_ref() != Some(&bundle.owner.module_version)
                    || bundle.owner.skinny_iface_sha256 != accepted.skinny_iface_sha256
                    || bundle.owner.product_sha256 != accepted.product_sha256
                    || bundle.source_sha256 != hex(&accepted.source_sha256)
                    || bundle.iface_sha256 != hex(&accepted.skinny_iface_sha256)
                {
                    return Err(CertificationError::Mismatch("candidate owner/evidence"));
                }
                candidate_evidence_validation
                    .validate(&bundle.evidence, &bundle.target_source)
                    .map_err(|failure| CertificationError::CandidateEvidence {
                        unit: key.0.clone(),
                        module: key.1.clone(),
                        failure: Box::new(failure),
                    })?;
                if sha(&read_bounded_with_operation(
                    &bundle.source,
                    SOURCE_LIMIT,
                    &validation.inventory,
                )?) != accepted.source_sha256
                    || sha(&read_bounded_with_operation(
                        &bundle.iface_path,
                        PACKAGE_INTERFACE_LIMIT,
                        &validation.inventory,
                    )?) != accepted.skinny_iface_sha256
                    || hex(&sha(&read_bounded_with_operation(
                        &bundle.package_imports_path,
                        32 << 20,
                        &validation.inventory,
                    )?)) != bundle.package_imports_sha256
                {
                    return Err(CertificationError::StaleEvidence);
                }
                let sidecar = read_bounded_with_operation(
                    &bundle.package_imports_path,
                    32 << 20,
                    &validation.inventory,
                )?;
                if sidecar != bundle.package_imports_bytes {
                    return Err(CertificationError::StaleEvidence);
                }
                crate::recovery_artifacts::validate_package_imports_with_validation(
                    &sidecar,
                    &key.0,
                    &key.1,
                    &accepted.skinny_iface_sha256,
                    &bundle.package_imports_path,
                    validation,
                )
                .map_err(package_validation_error)?;
                if ready_source_sha(final_evidence, &key.0, &key.1)? != accepted.source_sha256 {
                    return Err(CertificationError::Mismatch("candidate current source"));
                }
                if exact.is_none() {
                    let original_imports = bundle
                        .evidence
                        .modules
                        .iter()
                        .find(|row| row.unit == key.0 && row.module == key.1 && !row.boot)
                        .map(|row| &row.imports);
                    let current_imports = final_evidence
                        .modules
                        .iter()
                        .find(|row| row.unit == key.0 && row.module == key.1 && !row.boot)
                        .map(|row| &row.imports);
                    if original_imports.is_none()
                        || original_imports.map(|rows| rows.len())
                            != current_imports.map(|rows| rows.len())
                        || original_imports
                            .unwrap()
                            .iter()
                            .zip(current_imports.unwrap())
                            .any(|(old, new)| {
                                old.qualifier != new.qualifier
                                    || old.module != new.module
                                    || old.boot != new.boot
                                    || old.selected != new.selected
                            })
                    {
                        return Err(CertificationError::Mismatch("candidate direct imports"));
                    }
                }
                // The candidate carrier owns the original bounded decode and its bytes.
                (
                    bundle.product.decoded(),
                    bundle.product.bytes(),
                    bundle.product.bytes(),
                    bundle.package_imports_bytes.as_slice(),
                    bundle
                        .evidence
                        .json_sha256()
                        .ok_or(CertificationError::Mismatch("dependency witness encoding"))?,
                    ready_source_sha(&bundle.evidence, &key.0, &key.1)?,
                    bundle.owner.module_version.clone(),
                )
            }
            ProductOrigin::RetainedCore => {
                exact.ok_or(CertificationError::Mismatch(
                    "retained core requires exact request",
                ))?;
                if projected_originals
                    .iter()
                    .any(|product| product.owner().unit == key.0 && product.owner().module == key.1)
                {
                    return Err(CertificationError::Mismatch(
                        "retained core replaces native owner",
                    ));
                }
                let canonical = inherited_module_interfaces
                    .iter()
                    .find(|interface| interface.unit() == key.0 && interface.module() == key.1)
                    .ok_or(CertificationError::Mismatch(
                        "retained core lacks inherited canonical original",
                    ))?;
                let product = matching_product(parsed_fresh, &key.0, &key.1)?;
                let module_bytes = fresh_sidecars
                    .get(&key)
                    .ok_or(CertificationError::Mismatch("retained module product"))?;
                let package_bytes = fresh_package_imports
                    .get(&key)
                    .ok_or(CertificationError::Mismatch("retained package imports"))?;
                canonical.validate_native_promotion(
                    accepted,
                    producer_sha256,
                    &retained_seals,
                    &product.interface,
                    package_bytes,
                )?;
                let source_sha = canonical.source_sha256();
                // Local source edges are validated before assigning stable
                // promotion versions. This placeholder never leaves staging.
                let version = ModuleVersion([0; 32]);
                fresh_modules.insert(key.clone());
                (
                    product,
                    fresh_product_bytes,
                    *module_bytes,
                    package_bytes.as_slice(),
                    sha(canonical.certificate_bytes()),
                    source_sha,
                    version,
                )
            }
        };
        let owner = CachedHomeOwner {
            unit: key.0.clone(),
            module: key.1.clone(),
            module_version: version,
            skinny_iface_sha256: accepted.skinny_iface_sha256,
            product_sha256: sha(product_bytes),
        };
        let receipt_digest = match accepted.origin {
            ProductOrigin::Fresh | ProductOrigin::RetainedCore => {
                fresh_digests
                    .expect("fresh receipt has aggregate digests")
                    .0
            }
            ProductOrigin::Cached => sha(receipt_bytes),
        };
        if source_sha != accepted.source_sha256
            || receipt_digest != accepted.product_sha256
            || sha(&product.interface) != owner.skinny_iface_sha256
            || evidence_digest != accepted.dependency_witness_sha256
        {
            return Err(CertificationError::Mismatch(
                "product/iface/source/evidence digest",
            ));
        }
        check_direct_package_agreement_with_validation(
            package_bytes,
            &owner,
            &receipt.packages,
            validation,
        )?;
        if product.groups.len() != accepted.groups.len() {
            return Err(CertificationError::Mismatch("original group count"));
        }
        let origin_index = match accepted.origin {
            ProductOrigin::Fresh => 0,
            ProductOrigin::Cached => 1,
            ProductOrigin::RetainedCore => 2,
        };
        origin_counts[origin_index][0] += 1;
        origin_counts[origin_index][1] += product.groups.len() as u64;
        origin_counts[origin_index][2] += product_bytes.len() as u64;
        // Certification retains independent immutable payload copies.
        // Shared group arenas below clone handles rather than whole programs.
        validation.inventory.charge(product.interface.len())?;
        validation.inventory.charge(product_bytes.len())?;
        validation.inventory.charge(package_bytes.len())?;
        validation.inventory.charge(owner.unit.len())?;
        validation.inventory.charge(owner.module.len())?;
        module_bytes.push((
            owner.clone(),
            source_sha,
            product.interface.clone(),
            product_bytes.to_vec(),
            package_bytes.to_vec(),
            accepted.origin,
        ));
        let mut seen_ordinals = BTreeSet::new();
        for (group, witness) in product.groups.iter().zip(&accepted.groups) {
            if group.original_ordinal() != witness.original_ordinal
                || !seen_ordinals.insert(witness.original_ordinal)
                || group.globals().len() != witness.globals.len()
            {
                return Err(CertificationError::Mismatch("original group/globals"));
            }
            validation
                .inventory
                .reserve::<ReceiptImportOwner>(witness.globals.len())?;
            let mut imports = Vec::with_capacity(witness.globals.len());
            for (declaration, selected) in group.globals().iter().zip(&witness.globals) {
                charge_global(&validation.inventory, selected)?;
                imports.push(validate_global_witness(
                    declaration,
                    group.definitions().signatures(),
                    selected,
                )?);
            }
            validation.inventory.reserve::<(
                ProductOrigin,
                CachedHomeOwner,
                ProjectedGroup,
                Vec<ReceiptImportOwner>,
            )>(2)?;
            validation.inventory.charge(owner.unit.len())?;
            validation.inventory.charge(owner.module.len())?;
            groups.push((accepted.origin, owner.clone(), group.clone(), imports));
        }
    }
    for product in parsed_fresh {
        if !fresh_modules.contains(&(product.unit.clone(), product.module.clone())) {
            return Err(CertificationError::Mismatch("unwitnessed fresh module"));
        }
    }
    // Each cached row has passed the same source, native product, package,
    // digest and global checks as fresh rows. Its selected immutable canonical
    // carrier supplies type authority to this transaction's fresh interfaces.
    let selected_cached_interfaces = module_bytes
        .iter()
        .filter(|row| row.5 == ProductOrigin::Cached)
        .map(|(owner, source_sha, _, _, _, _)| {
            let interface = &candidates
                .and_then(|set| {
                    set.by_owner
                        .get(&(owner.unit.clone(), owner.module.clone()))
                })
                .filter(|bundle| bundle.owner == *owner)
                .ok_or(CertificationError::Mismatch("selected candidate"))?
                .original_module_interface;
            if interface.producer_sha256() != producer_sha256
                || interface.unit() != owner.unit
                || interface.module() != owner.module
                || interface.source_sha256() != *source_sha
                || interface.interface_sha256() != owner.skinny_iface_sha256
            {
                return Err(CertificationError::Mismatch(
                    "native canonical producer/source",
                ));
            }
            Ok(interface)
        })
        .collect::<CertResult<Vec<_>>>()?;
    let mut inherited_seals = projected_metadata
        .into_iter()
        .flat_map(|metadata| metadata.entries.values())
        .map(|entry| {
            let descriptor = &entry.descriptor;
            (
                (
                    descriptor.owner.unit.clone(),
                    descriptor.owner.module.clone(),
                ),
                descriptor.interface_sha256,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for interface in &selected_cached_interfaces {
        let key = (interface.unit().to_owned(), interface.module().to_owned());
        if inherited_seals
            .insert(key, interface.interface_sha256())
            .is_some_and(|previous| previous != interface.interface_sha256())
        {
            return Err(CertificationError::Mismatch(
                "conflicting admitted interface owner",
            ));
        }
    }
    let value_interfaces = finalized_module::issue_value_interfaces(
        &receipt.finalization,
        captured_payload_root,
        producer_sha256,
        selected_session_values,
        produced_types,
        &inherited_seals,
        validation,
    )?;
    for interface in &value_interfaces {
        let value = interface.interface();
        let key = (value.unit().to_owned(), value.module().to_owned());
        let seal = sha(value.interface_bytes());
        if inherited_seals
            .insert(key, seal)
            .is_some_and(|old| old != seal)
        {
            return Err(CertificationError::Mismatch(
                "captured value inherited owner conflict",
            ));
        }
    }
    let module_interfaces = finalized_module::issue_interfaces(
        &receipt.finalization,
        captured_payload_root,
        producer_sha256,
        final_evidence,
        &inherited_seals,
        authored,
        exact.map_or(&BTreeMap::new(), |admission| {
            &admission.source.exact_source_imports
        }),
        validation,
    )?;
    let mut admitted_interfaces = BTreeMap::new();
    for module in &receipt.modules {
        let key = (module.unit.clone(), module.module.clone());
        if admitted_interfaces
            .insert(key, module.skinny_iface_sha256)
            .is_some()
        {
            return Err(CertificationError::Mismatch("duplicate interface owner"));
        }
    }
    if let Some(admission) = exact {
        for artifact in &admission.request.artifacts {
            let key = (
                artifact.interface.unit.clone(),
                artifact.interface.module.clone(),
            );
            let seal = digest(&value_text(&artifact.interface.sha256))?;
            if admitted_interfaces
                .insert(key, seal)
                .is_some_and(|previous| previous != seal)
            {
                return Err(CertificationError::Mismatch(
                    "conflicting admitted interface owner",
                ));
            }
        }
    }
    admit_canonical_interface_owners(
        &mut admitted_interfaces,
        module_interfaces
            .iter()
            .chain(inherited_module_interfaces.iter())
            .chain(selected_cached_interfaces.iter().copied()),
    )?;
    for interface in &value_interfaces {
        let value = interface.interface();
        let key = (value.unit().to_owned(), value.module().to_owned());
        let seal = sha(value.interface_bytes());
        if admitted_interfaces
            .insert(key, seal)
            .is_some_and(|old| old != seal)
        {
            return Err(CertificationError::Mismatch(
                "captured value admitted owner conflict",
            ));
        }
    }
    validate_original_interface_owner_closure(&receipt.modules, &admitted_interfaces)?;
    let mut source_selection = match exact {
        Some(_) => CertifiedSourceSelection::from_compiler_projection(
            &compiler_inputs
                .as_ref()
                .ok_or(CertificationError::Mismatch("compiler original projection"))?
                .projection,
            projected_metadata
                .as_ref()
                .ok_or(CertificationError::Mismatch("compiler original projection"))?,
            &validation.inventory,
        )?,
        None => CertifiedSourceSelection::default(),
    };
    for interface in &module_interfaces {
        source_selection.admit_interface(
            &crate::artifact_inventory::ArtifactEntry::canonical(interface.clone()),
            &validation.inventory,
        )?;
    }
    for value in &value_interfaces {
        source_selection.admit_interface(
            &crate::artifact_inventory::ArtifactEntry::interface(
                value.interface().clone(),
                crate::artifact_inventory::JoinedInterfaceRole::ValueInterface,
                value.requirements().to_vec(),
            ),
            &validation.inventory,
        )?;
    }
    for (owner, _, _, _, _, origin) in &module_bytes {
        let canonical = current_module_interface(
            owner,
            *origin,
            candidates,
            &module_interfaces,
            &inherited_module_interfaces,
        )?;
        source_selection.admit_current(owner, *origin, canonical, &validation.inventory)?;
    }
    let available_originals = inherited_products;
    let mut source_groups = match exact {
        Some(admission) => available_original_source_map(
            &available_originals,
            &admission.request.groups,
            &validation.inventory,
        )?,
        None => SourceGroupMap::new(),
    };
    for (origin, owner, group, _) in &groups {
        for binder in group.binders() {
            let key = (owner.clone(), group.original_ordinal(), binder.clone());
            if let Some((_, existing_origin)) = source_groups.get(&key) {
                if *origin != ProductOrigin::Cached {
                    return Err(duplicate_source_binder(
                        SourceBinderPhase::CurrentReceipt,
                        &key,
                        *existing_origin,
                        *origin,
                    ));
                }
                // The complete body and imports are checked against the original
                // below, after every current source owner has entered the map.
            } else {
                source_groups.insert(key, (owner.clone(), *origin));
            }
        }
    }
    let mut promoted = BTreeMap::new();
    for (owner, _, _, _, package_bytes, origin) in &module_bytes {
        if *origin != ProductOrigin::RetainedCore {
            continue;
        }
        let canonical = inherited_module_interfaces
            .iter()
            .find(|interface| interface.unit() == owner.unit && interface.module() == owner.module)
            .ok_or(CertificationError::Mismatch("retained canonical identity"))?;
        promoted.insert(
            (owner.unit.clone(), owner.module.clone()),
            retained_core::PromotedModule {
                canonical_sha256: sha(canonical.certificate_bytes()),
                product_sha256: owner.product_sha256,
                package_sha256: sha(package_bytes),
                groups: BTreeMap::new(),
            },
        );
    }
    for (origin, owner, group, imports) in &groups {
        if *origin != ProductOrigin::RetainedCore {
            continue;
        }
        let imports = imports
            .iter()
            .cloned()
            .map(|import| {
                resolve_receipt_owner_with_validation(
                    import,
                    &source_groups,
                    Some(&source_selection),
                    &receipt.packages,
                    validation,
                )
            })
            .collect::<CertResult<Vec<_>>>()?;
        promoted
            .get_mut(&(owner.unit.clone(), owner.module.clone()))
            .ok_or(CertificationError::Mismatch("retained staged owner"))?
            .groups
            .insert(group.original_ordinal(), imports);
    }
    let promotion_versions = retained_core::module_versions(&promoted, &receipt.packages)?;
    let promotion_owners = module_bytes
        .iter()
        .filter(|(_, _, _, _, _, origin)| *origin == ProductOrigin::RetainedCore)
        .map(|(owner, _, _, _, _, _)| {
            let version = promotion_versions
                .get(&(owner.unit.clone(), owner.module.clone()))
                .ok_or(CertificationError::Mismatch("retained derived identity"))?;
            Ok((owner.clone(), version.clone()))
        })
        .collect::<CertResult<BTreeMap<_, _>>>()?;
    for (owner, _, _, _, _, origin) in &mut module_bytes {
        if *origin == ProductOrigin::RetainedCore {
            owner.module_version = promotion_versions
                .get(&(owner.unit.clone(), owner.module.clone()))
                .ok_or(CertificationError::Mismatch("retained derived identity"))?
                .clone();
        }
    }
    for (origin, owner, _, _) in &mut groups {
        if *origin == ProductOrigin::RetainedCore {
            owner.module_version = promotion_versions
                .get(&(owner.unit.clone(), owner.module.clone()))
                .ok_or(CertificationError::Mismatch("retained derived identity"))?
                .clone();
        }
    }
    // A stable derived owner can already be present in full custody even when
    // only its canonical/Core carrier was offered to this compiler request.
    // Prove complete equality before merging those two membership paths.
    let mut sharing = ValidatedPromotionSharing::default();
    for (owner, source_sha, interface, product_bytes, package_bytes, origin) in &module_bytes {
        if *origin != ProductOrigin::RetainedCore {
            continue;
        }
        let Some(original) = available_originals
            .iter()
            .find(|product| product.owner() == owner)
        else {
            continue;
        };
        let canonical = inherited_module_interfaces
            .iter()
            .find(|interface| interface.unit() == owner.unit && interface.module() == owner.module)
            .ok_or(CertificationError::Mismatch("retained canonical identity"))?;
        if original.interface_bytes() != interface
            || original.product_bytes() != product_bytes
            || original.package_imports_bytes() != package_bytes
            || original.source_sha256() != Some(*source_sha)
            || original
                .module_interface()
                .map(|interface| interface.certificate_bytes())
                != Some(canonical.certificate_bytes())
        {
            return Err(CertificationError::Mismatch("shared original native bytes"));
        }
        let native = original
            .original_native()
            .ok_or(CertificationError::Mismatch(
                "available original native witness",
            ))?;
        let resolved = &promoted[&(owner.unit.clone(), owner.module.clone())].groups;
        let current = groups
            .iter()
            .filter(|(_, current, _, _)| current == owner)
            .map(|(_, _, group, _)| {
                let mut imports = resolved[&group.original_ordinal()].clone();
                for import in &mut imports {
                    if let PendingImportOwner::Source { owner, .. } = import {
                        if let Some(version) = promotion_owners.get(owner) {
                            owner.module_version = version.clone();
                        }
                    }
                }
                (group, imports)
            })
            .collect::<Vec<_>>();
        native.validate_promoted_groups(&current)?;
        sharing.0.insert(owner.clone());
    }
    source_selection.promote(&promotion_versions)?;
    source_groups.promote(&promotion_owners, &sharing)?;
    let mut groups: Vec<PendingCertifiedGroup> = groups
        .into_iter()
        .map(|(origin, owner, group, imports)| {
            if origin == ProductOrigin::Cached {
                if let Some(product) = available_originals
                    .iter()
                    .find(|product| product.owner() == &owner)
                {
                    let native = product
                        .original_native()
                        .ok_or(CertificationError::Mismatch(
                            "available original native witness",
                        ))?;
                    let original = native
                        .groups
                        .iter()
                        .find(|original| {
                            original.group.original_ordinal() == group.original_ordinal()
                        })
                        .ok_or(CertificationError::Mismatch("shared original home groups"))?;
                    return validate_original_group_receipts(
                        &group,
                        &imports,
                        original,
                        &source_groups,
                        &receipt.packages,
                        validation,
                    );
                }
            }
            let imports = imports
                .into_iter()
                .map(|import| {
                    resolve_receipt_owner_with_validation(
                        import,
                        &source_groups,
                        Some(&source_selection),
                        &receipt.packages,
                        validation,
                    )
                })
                .collect::<CertResult<Vec<_>>>()?;
            Ok(PendingCertifiedGroup {
                origin,
                owner,
                group: Arc::new(group),
                imports: imports.into(),
            })
        })
        .collect::<CertResult<_>>()?;
    let fresh_execution_owners: BTreeSet<_> = receipt
        .modules
        .iter()
        .filter(|module| module.origin == ProductOrigin::Fresh)
        .map(|module| crate::declaration_join::ExactModuleIdentity {
            unit: module.unit.clone(),
            module: module.module.clone(),
        })
        .collect();
    let execution_source_start = std::time::Instant::now();
    enum SourceRecipeAdmission<'a> {
        Ordinary,
        Issued {
            digest: [u8; 32],
            bytes: &'a Arc<[u8]>,
        },
    }
    let source_recipe = match (&receipt.source_recipe, exact) {
        (WorkerExecutionSource::Ordinary, None) => Some(SourceRecipeAdmission::Ordinary),
        (WorkerExecutionSource::ExactUnavailable(_), Some(_)) => None,
        (WorkerExecutionSource::ExactAvailable { digest, bytes }, Some(_)) => {
            Some(SourceRecipeAdmission::Issued {
                digest: *digest,
                bytes,
            })
        }
        _ => {
            return Err(CertificationError::Mismatch(
                "source recipe compilation route",
            ))
        }
    };
    let execution_graph = if let Some(recipe) =
        source_recipe.filter(|_| final_evidence.cache_safe && final_evidence.selection_complete)
    {
        // A new recipe describes this compiler request's namespace. Older
        // original graphs stay attached to their exact retained carriers.
        let owners: Vec<_> = source_selection
            .native_selections()
            .filter(|selected| !matches!(selected.version, ReceiptSourceVersion::RetainedCore))
            .map(|selected| selected.owner.clone())
            .collect();
        let mut retained_sources: BTreeMap<_, _> = projected_originals
            .iter()
            .filter_map(|product| {
                product.execution_source().map(|graph| {
                    (
                        crate::declaration_join::ExactModuleIdentity {
                            unit: product.owner().unit.clone(),
                            module: product.owner().module.clone(),
                        },
                        graph.digest(),
                    )
                })
            })
            .collect();
        if let Some(candidates) = candidates {
            for (owner, _, _, _, _, origin) in &module_bytes {
                if *origin != ProductOrigin::Cached {
                    continue;
                }
                if let Some(proof) = candidates
                    .by_owner
                    .get(&(owner.unit.clone(), owner.module.clone()))
                    .filter(|bundle| bundle.owner == *owner)
                    .and_then(|bundle| bundle.original_execution.as_ref())
                {
                    let identity = crate::declaration_join::ExactModuleIdentity {
                        unit: owner.unit.clone(),
                        module: owner.module.clone(),
                    };
                    if retained_sources
                        .insert(identity, proof.graph.digest())
                        .is_some_and(|original| original != proof.graph.digest())
                    {
                        return Err(CertificationError::Mismatch(
                            "cached original execution graph",
                        ));
                    }
                }
            }
        }
        match <[u8; 32]>::try_from(endpoint_identity) {
            Ok(raw_producer) if raw_producer != [0; 32] => {
                let empty_imports = BTreeMap::new();
                let input = crate::execution_source::ExecutionSourceGraphInput {
                    producer:
                        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                            &raw_producer,
                        ),
                    semantic_sha256: exact.map(|admission| admission.request.semantic_sha256),
                    include,
                    source_path: fresh_input_path,
                    source: final_target_source,
                    evidence: final_evidence,
                    exact_imports: exact
                        .map_or(&empty_imports, |admission| &admission.source.exact_imports),
                    owners: &owners,
                    fresh_owners: &fresh_execution_owners,
                    retained_sources: &retained_sources,
                    packages: &source_packages,
                };
                let admitted = match recipe {
                    SourceRecipeAdmission::Ordinary => {
                        crate::execution_source::CertifiedExecutionSourceGraph::admit(input)
                    }
                    SourceRecipeAdmission::Issued { digest, bytes } => {
                        crate::execution_source::CertifiedExecutionSourceGraph::admit_issued(
                            Arc::clone(bytes),
                            digest,
                            input,
                        )
                        .map(crate::execution_source::ExecutionSourceAdmission::Available)
                    }
                };
                match admitted
                    .map_err(|error| CertificationError::ExecutionSource(Box::new(error)))?
                {
                    crate::execution_source::ExecutionSourceAdmission::Available(graph) => {
                        Some(graph)
                    }
                    crate::execution_source::ExecutionSourceAdmission::Unavailable(reason) => {
                        tracing::debug!(
                            ?reason,
                            "native products retain no compiler execution recipe"
                        );
                        None
                    }
                }
            }
            _ if matches!(
                receipt.source_recipe,
                WorkerExecutionSource::ExactAvailable { .. }
            ) =>
            {
                return Err(CertificationError::Mismatch(
                    "issued source recipe owner or producer",
                ))
            }
            _ => None,
        }
    } else if matches!(
        receipt.source_recipe,
        WorkerExecutionSource::ExactAvailable { .. }
    ) {
        return Err(CertificationError::Mismatch(
            "issued source recipe lacks complete evidence",
        ));
    } else {
        None
    };
    crate::timing::record_stage(
        crate::timing::NO_NODE,
        crate::timing::NO_ROUND,
        "products.execution_source_admission",
        execution_source_start.elapsed(),
        execution_graph
            .as_ref()
            .map_or(0, |graph| graph.bytes().len() as u64),
    );
    let mut recovery_products: Vec<_> = module_bytes
        .into_iter()
        .map(
            |(owner, source_sha, interface, product_bytes, package_bytes, origin)| {
                if origin == ProductOrigin::Cached || sharing.0.contains(&owner) {
                    if let Some(product) = available_originals.iter().find(|product| product.owner() == &owner) {
                        if product.interface_bytes() != interface || product.product_bytes() != product_bytes
                            || product.package_imports_bytes() != package_bytes || product.source_sha256() != Some(source_sha) {
                            return Err(CertificationError::Mismatch("shared original native bytes"));
                        }
                        return Ok(product.clone());
                    }
                }
                let original: Vec<_> = groups
                    .iter()
                    .filter(|group| group.owner() == &owner)
                    .cloned()
                    .collect();
                let accepted = receipt
                    .modules
                    .iter()
                    .find(|module| module.unit == owner.unit && module.module == owner.module)
                    .ok_or(CertificationError::Mismatch(
                        "original interface owner receipt",
                    ))?;
                let canonical_interface = current_module_interface(&owner, origin, candidates, &module_interfaces, &inherited_module_interfaces)?;
                if canonical_interface.producer_sha256() != producer_sha256
                    || canonical_interface.source_sha256() != source_sha {
                    return Err(CertificationError::Mismatch("native canonical producer/source"));
                }
                let execution_source = match origin {
                    ProductOrigin::Fresh => execution_graph
                        .as_ref()
                        .filter(|graph| graph.eligible_source_replay_root(&owner)),
                    ProductOrigin::Cached => candidates
                        .and_then(|set| {
                            set.by_owner
                                .get(&(owner.unit.clone(), owner.module.clone()))
                        })
                        .filter(|bundle| bundle.owner == owner)
                        .and_then(|bundle| bundle.original_execution.as_ref())
                        .map(|proof| &proof.graph),
                    ProductOrigin::RetainedCore => None,
                };
                let witness = issue_home_certification_with_validation(
                    &owner,
                    &original,
                    &receipt.packages,
                    &accepted.interface_requirements,
                    execution_source.map(|graph| graph.digest()),
                    Some(sha(canonical_interface.certificate_bytes())),
                    validation,
                )?;
                let certification = encode_home_witness_with_operation(&witness, &validation.inventory)?;
                let binding = validate_module_binding(&witness, canonical_interface, &interface, &package_bytes, Some(source_sha))?;
                let product = crate::recovery_artifacts::CertifiedRecoveryProduct::from_finalized_certification(
                    owner, product_bytes, certification, binding,
                ).with_source_sha256(source_sha);
                let product = retain_original_native(product, original, witness)?;
                match execution_source {
                    Some(graph) => product
                        .with_execution_source_with_validation(Arc::clone(graph), validation)
                        .map_err(|_| CertificationError::Mismatch("execution source owner")),
                    None => Ok(product),
                }
            },
        )
        .collect::<CertResult<Vec<_>>>()?;
    if std::env::var(crate::timing::TIMING_ENV).as_deref() == Ok("1") {
        for (accepted, product) in receipt.modules.iter().zip(&recovery_products) {
            tracing::debug!(target: "tidepool_toolchain::module_candidates",
                phase = "candidate_certification_owner", origin = ?accepted.origin,
                unit = accepted.unit.as_str(), module = accepted.module.as_str(),
                group_rows = accepted.groups.len(), original_bytes = product.product_bytes().len(),
                "certified original module attribution");
        }
    }
    let retained_core_products = CertifiedRetainedCoreProducts {
        products: receipt
            .modules
            .iter()
            .filter(|module| module.origin == ProductOrigin::RetainedCore)
            .map(|module| {
                let product = recovery_products
                    .iter()
                    .find(|product| {
                        product.owner().unit == module.unit
                            && product.owner().module == module.module
                    })
                    .ok_or(CertificationError::Mismatch(
                        "certified retained assembly owner",
                    ))?;
                Ok((
                    (module.unit.clone(), module.module.clone()),
                    product.clone(),
                ))
            })
            .collect::<CertResult<_>>()?,
    };
    if let Some(admission) = exact {
        append_original_selection(
            &mut groups,
            &admission.request.groups,
            &validation.inventory,
            &sharing,
        )?;
        for product in &available_originals {
            if !recovery_products
                .iter()
                .any(|current| current.owner() == product.owner())
            {
                recovery_products.push(product.clone());
            }
        }
    }
    crate::timing::record_stage(
        crate::timing::NO_NODE,
        crate::timing::NO_ROUND,
        "products.certify_ownership",
        ownership_start.elapsed(),
        fresh_product_bytes.len() as u64,
    );
    tracing::info!(target: "tidepool_toolchain::module_candidates",
        phase = "candidate_certification_success",
        fresh_modules = origin_counts[0][0], fresh_group_rows = origin_counts[0][1], fresh_original_bytes = origin_counts[0][2],
        cached_modules = origin_counts[1][0], cached_group_rows = origin_counts[1][1], cached_original_bytes = origin_counts[1][2],
        retained_core_modules = origin_counts[2][0], retained_core_group_rows = origin_counts[2][1], retained_core_original_bytes = origin_counts[2][2]);
    Ok(CertifiedProducts {
        groups,
        recovery_products,
        module_interfaces,
        value_interfaces,
        retained_core_products,
        source_selection,
    })
}

/// Add seals only from canonical interfaces already authenticated by fresh
/// finalization or the exact request's protected declaration context. These
/// include source-only owners that intentionally have no native product row.
/// Executable group and source ownership remain governed by their separate
/// receipt checks.
fn admit_canonical_interface_owners<'a>(
    admitted: &mut BTreeMap<(String, String), [u8; 32]>,
    interfaces: impl IntoIterator<Item = &'a crate::certified_products::CertifiedModuleInterface>,
) -> CertResult<()> {
    for interface in interfaces {
        let key = (interface.unit().to_owned(), interface.module().to_owned());
        let seal = interface.interface_sha256();
        if admitted
            .insert(key, seal)
            .is_some_and(|previous| previous != seal)
        {
            return Err(CertificationError::Mismatch(
                "conflicting admitted interface owner",
            ));
        }
    }
    Ok(())
}

fn validate_original_interface_owner_closure(
    modules: &[CertifiedModuleReceipt],
    admitted: &BTreeMap<(String, String), [u8; 32]>,
) -> CertResult<()> {
    for module in modules {
        for (key, seal) in &module.interface_requirements {
            if key == &(module.unit.clone(), module.module.clone())
                || admitted.get(key) != Some(seal)
            {
                return Err(CertificationError::OriginalInterfaceClosure {
                    owner: (module.unit.clone(), module.module.clone()),
                    dependency: key.clone(),
                    expected: hex(seal),
                    actual: admitted.get(key).map(|seal| hex(seal)),
                });
            }
        }
    }
    Ok(())
}

/// Bind a target's declared globals to the same compiler transaction's
/// source inventory. Retained identities remain unresolved until runtime
/// checks their lexical scope and assigns authoritative session IDs.
pub fn certify_target_owners(
    prepared: &PreparedProgram,
    accepted: &[AcceptedGlobal],
    groups: &[PendingCertifiedGroup],
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
) -> CertResult<Vec<PendingImportOwner>> {
    let sources = certified_source_map(groups)?;
    certify_target_owners_from_sources(
        prepared,
        accepted,
        &sources,
        None,
        packages,
        &mut PackageInterfaceValidation::default(),
    )
}

pub(crate) fn certify_target_owners_with_validation(
    prepared: &PreparedProgram,
    accepted: &[AcceptedGlobal],
    groups: &[PendingCertifiedGroup],
    selection: &CertifiedSourceSelection,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<PendingImportOwner>> {
    let sources = certified_source_map(groups)?;
    certify_target_owners_from_sources(
        prepared,
        accepted,
        &sources,
        Some(selection),
        packages,
        validation,
    )
}

/// Authenticate ordinary and checked-wrapper target imports from full immutable
/// source membership. Executable admission belongs to the inventory and selected
/// certifier; a checked target also supplies its independently issued entry root.
pub(crate) fn certify_target_available_owners_with_validation(
    prepared: &PreparedProgram,
    accepted: &[AcceptedGlobal],
    products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
    selected: &[PendingCertifiedGroup],
    selection: &CertifiedSourceSelection,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<PendingImportOwner>> {
    let sources = available_original_source_map(products, selected, &validation.inventory)?;
    certify_target_owners_from_sources(
        prepared,
        accepted,
        &sources,
        Some(selection),
        packages,
        validation,
    )
}

fn certify_target_owners_from_sources(
    prepared: &PreparedProgram,
    accepted: &[AcceptedGlobal],
    sources: &SourceGroupMap,
    selection: Option<&CertifiedSourceSelection>,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<PendingImportOwner>> {
    if prepared.globals().len() != accepted.len() {
        return Err(CertificationError::Mismatch("target global count"));
    }
    prepared
        .globals()
        .iter()
        .zip(accepted)
        .map(|(declaration, selected)| {
            let owner = validate_global_witness(declaration, prepared.signatures(), selected)?;
            resolve_receipt_owner_with_validation(owner, sources, selection, packages, validation)
        })
        .collect()
}

#[cfg(test)]
mod home_self_issuer_tests;

#[cfg(test)]
pub(crate) mod tests {
    mod issued_interface_selection_history;
    mod promotion_import_history;
    mod sparse_interface_selection_properties;
    use super::*;
    use crate::cache::{ModuleEvidence, SourceEvidence};
    use tidepool_repr::execution_schema::testing;
    use tidepool_repr::execution_schema::{DecodeLimits, InventoryDecodeLimits};

    #[test]
    fn bounded_evidence_read_preserves_path_reason_and_operation_budget() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipt.cbor");
        let operation = InventoryOperation::new(Default::default());
        assert!(matches!(
            read_bounded_with_operation(Path::new("relative"), 4, &operation),
            Err(CertificationError::EvidenceRead {
                failure: EvidenceReadFailure::NonAbsolutePath,
                ..
            })
        ));
        assert!(matches!(
            read_bounded_with_operation(&path, 4, &operation),
            Err(CertificationError::EvidenceRead {
                path: failed,
                failure: EvidenceReadFailure::Io {
                    operation: EvidenceReadOperation::Metadata,
                    error,
                },
            }) if failed == path && error.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(matches!(
            read_bounded_with_operation(directory.path(), 4, &operation),
            Err(CertificationError::EvidenceRead {
                failure: EvidenceReadFailure::NotFile,
                ..
            })
        ));
        std::fs::write(&path, b"proof").unwrap();
        let error = read_bounded_with_operation(&path, 4, &operation).unwrap_err();
        assert!(error.to_string().contains(path.to_str().unwrap()));
        assert!(matches!(
            error,
            CertificationError::EvidenceRead {
                failure: EvidenceReadFailure::SizeLimit {
                    actual: 5,
                    limit: 4
                },
                ..
            }
        ));
        assert_eq!(
            read_bounded_with_operation(&path, 5, &operation).unwrap(),
            b"proof"
        );
        let restricted = InventoryOperation::new(InventoryDecodeLimits {
            max_work: 5,
            ..Default::default()
        });
        assert!(matches!(
            read_bounded_with_operation(&path, 5, &restricted),
            Err(CertificationError::Product(
                tidepool_repr::execution_schema::ParseError::LimitExceeded("work")
            ))
        ));
    }

    fn recovery_native_fixture(
        root: &Path,
    ) -> (
        crate::recovery_artifacts::RecoveryArtifactRef,
        crate::recovery_artifacts::RecoveryValueInterfaceRef,
    ) {
        use crate::recovery_artifacts::{
            CertifiedJoinedInterface, CertifiedRecoveryProduct, CertifiedValueInterface,
        };
        use tidepool_repr::execution_schema::{parse_program, parse_projected_group, Group};
        let bytes =
            tidepool_test_data::prepared_encode::encode_wire_program(&testing::wire_program());
        let requirements = crate::prepared_artifact::production_requirements().unwrap();
        let prepared = parse_program(&bytes, &requirements, DecodeLimits::default()).unwrap();
        let mut binders = prepared
            .bindings()
            .iter()
            .flat_map(|group| match group {
                Group::NonRecursive(top) => std::slice::from_ref(top),
                Group::Recursive(tops) => tops,
            })
            .map(|top| top.identity.clone())
            .collect::<Vec<_>>();
        binders.truncate(1);
        let entry = binders[0].clone();
        let retained = SymbolIdentity {
            unit: entry.unit.clone(),
            module: "Val1".into(),
            namespace: "value".into(),
            occurrence: "x".into(),
            record_parent: None,
        };
        let symbol = |identity: &SymbolIdentity| {
            value_array([
                value_text(&identity.unit),
                value_text(&identity.module),
                value_text(&identity.namespace),
                value_text(&identity.occurrence),
                value_array([Value::Integer(0.into())]),
            ])
        };
        let Value::Array(mut fields) = ciborium::de::from_reader(bytes.as_slice()).unwrap() else {
            unreachable!()
        };
        fields[7] = value_array([
            value_array([
                symbol(&entry),
                value_array([Value::Integer(1.into())]),
                value_array([Value::Integer(0.into())]),
                Value::Bool(false),
                value_array([Value::Integer(0.into())]),
            ]),
            value_array([
                symbol(&retained),
                value_array([Value::Integer(1.into())]),
                value_array([Value::Integer(0.into())]),
                Value::Bool(false),
                value_array([Value::Integer(1.into()), Value::Integer(1.into())]),
            ]),
        ]);
        // Keep the pinned envelope and replace executable fixture payload with
        // a neutral original group whose two certified imports are explicit.
        let number = |value: u64| Value::Integer(value.into());
        fields[6] = value_array([value_array([
            value_array([]),
            value_array([
                number(0),
                value_array([value_array([number(4), number(64)])]),
            ]),
        ])]);
        fields[8] = value_array([]);
        fields[9] = value_array([]);
        fields[10] = value_array([value_array([
            number(0),
            value_array([value_array([
                number(1),
                value_array([
                    number(0),
                    number(64),
                    Value::Bytes(42_i64.to_be_bytes().to_vec()),
                ]),
            ])]),
        ])]);
        fields[11] = value_array([value_array([
            number(0),
            value_array([
                symbol(&entry),
                value_array([
                    number(0),
                    value_array([
                        number(0),
                        number(0),
                        value_array([]),
                        value_array([]),
                        number(0),
                    ]),
                ]),
            ]),
        ])]);
        fields[13] = value_array([]);
        fields[14] = value_array([]);
        fields[15] = value_array([]);
        fields[16] = value_array([number(0)]);
        fields.remove(12);
        let mut group_fields = vec![
            value_text("TPGRP"),
            Value::Integer(1.into()),
            Value::Integer(7.into()),
            value_array(binders.iter().map(symbol)),
        ];
        group_fields.extend(fields.into_iter().skip(1));
        let mut group_bytes = Vec::new();
        ciborium::ser::into_writer(&value_array(group_fields), &mut group_bytes).unwrap();
        let group =
            parse_projected_group(&group_bytes, &requirements, DecodeLimits::default()).unwrap();
        let interface = b"original interface".to_vec();
        let value = value_array([
            value_text("TPMOD"),
            Value::Integer(1.into()),
            value_array([value_array([
                value_text(&entry.unit),
                value_text(&entry.module),
                Value::Bytes(interface.clone()),
                value_array([Value::Bytes(group_bytes)]),
            ])]),
        ]);
        let mut product_bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut product_bytes).unwrap();
        let owner = CachedHomeOwner {
            unit: entry.unit.clone(),
            module: entry.module.clone(),
            module_version: ModuleVersion([1; 32]),
            skinny_iface_sha256: sha(&interface),
            product_sha256: sha(&product_bytes),
        };
        let certified = PendingCertifiedGroup {
            owner: owner.clone(),
            group: Arc::new(group),
            origin: ProductOrigin::Cached,
            imports: vec![
                PendingImportOwner::Source {
                    owner: owner.clone(),
                    original_ordinal: 7,
                    binder: entry,
                },
                PendingImportOwner::Retained {
                    identity: retained,
                    generation: 1,
                },
            ]
            .into(),
        };
        let packages = |unit: &str, module: &str, bytes: &[u8]| {
            let value = value_array([
                value_text("TPPKGROOTS"),
                value_text("2"),
                value_array([
                    value_text(unit),
                    value_text(module),
                    value_text(hex(&sha(bytes))),
                ]),
                value_array([]),
                value_array([]),
            ]);
            let mut output = Vec::new();
            ciborium::ser::into_writer(&value, &mut output).unwrap();
            output
        };
        let seal = encode_home_certification(&owner, &[certified], &BTreeMap::new()).unwrap();
        let package_bytes = packages(&owner.unit, &owner.module, &interface);
        let product = CertifiedRecoveryProduct::from_certification(
            owner.clone(),
            interface,
            product_bytes,
            package_bytes,
            seal,
        );
        let product = fixture_finalized_product(product, [2; 32]);
        let original =
            crate::recovery_artifacts::materialize_certified_products(root, [2; 32], &[product])
                .unwrap()
                .remove(0);
        let value_bytes = b"value interface".to_vec();
        let package_bytes = packages(&owner.unit, "Val1", &value_bytes);
        let interface = CertifiedJoinedInterface::from_certification(
            [2; 32],
            owner.unit,
            "Val1".into(),
            value_bytes,
            package_bytes,
        )
        .unwrap();
        let value = CertifiedValueInterface::from_admitted_interface(interface, vec![])
            .materialize(root)
            .unwrap();
        (original, value)
    }

    #[test]
    fn recovery_derives_native_relations_from_roundtripped_original_seals_once() {
        use crate::artifact_inventory::{ArtifactDependency, ArtifactDescriptor};
        use crate::declaration_join::RecoveredArtifactInventory;
        let root = tempfile::tempdir().unwrap();
        let (original, value) = recovery_native_fixture(root.path());
        let (original, value) = serde_json::from_slice::<(
            crate::recovery_artifacts::RecoveryArtifactRef,
            crate::recovery_artifacts::RecoveryValueInterfaceRef,
        )>(&serde_json::to_vec(&(original, value)).unwrap())
        .unwrap();
        let canonical = original.module_interface.clone().unwrap();
        let descriptors = [
            ArtifactDescriptor::from_recovery_product(&original),
            ArtifactDescriptor::from_recovery_value_interface(&value),
            ArtifactDescriptor::from_recovery_module_interface(&canonical),
        ];
        let interfaces = [(
            descriptors[0].id,
            descriptors[2].id,
            ArtifactDependency::Interface,
        )];
        let inventory = RecoveredArtifactInventory::capture(
            root.path(),
            &[original.clone()],
            &[canonical.clone()],
            &[],
            &[value.clone()],
            &descriptors,
            &interfaces,
        )
        .unwrap();
        let ids = descriptors
            .iter()
            .map(|descriptor| descriptor.id)
            .collect::<Vec<_>>();
        // The admission owns authenticated bytes; scoped views need no file reread.
        std::fs::remove_file(root.path().join(&original.product_path)).unwrap();
        let first = inventory.context_all_groups(&ids, vec![]).unwrap();
        let second = inventory.context_all_groups(&ids, vec![]).unwrap();
        let first_entries = first.artifact_view().entries();
        let second_entries = second.artifact_view().entries();
        assert!(first_entries
            .iter()
            .zip(&second_entries)
            .all(|(a, b)| Arc::ptr_eq(a, b)));
        let edges = first.artifact_view().dependencies();
        assert!(edges.iter().any(|(_, _, edge)| matches!(
            edge,
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 7,
                required_ordinal: 7
            }
        )));
        assert!(edges.iter().any(|(_, target, edge)| *target == value.artifact_id && matches!(edge, ArtifactDependency::NativeBinding { dependent_ordinal: 7, generation: 1, occurrence, .. } if occurrence == "x")));
        assert_eq!(first.artifact_view().interface_dependencies(), interfaces);
        let required = first
            .artifact_view()
            .native_binding_requirements_from_roots(&[
                crate::artifact_inventory::NativeRequirementRoot::AllGroups(descriptors[0].id),
            ])
            .unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].generation, 1);
        assert_eq!(required[0].artifact_id, value.artifact_id);
        assert!(inventory.context_all_groups(&ids[..1], vec![]).is_err());
        assert!(RecoveredArtifactInventory::capture(
            root.path(),
            &[original],
            &[canonical],
            &[],
            &[value],
            &descriptors,
            &interfaces
        )
        .is_err());
    }

    #[test]
    fn recovery_refuses_tampered_and_ambiguous_native_dependency_owners() {
        use crate::artifact_inventory::{ArtifactDependency, ArtifactDescriptor};
        use crate::declaration_join::RecoveredArtifactInventory;
        let root = tempfile::tempdir().unwrap();
        let (original, value) = recovery_native_fixture(root.path());
        let canonical = original.module_interface.clone().unwrap();
        let descriptors = [
            ArtifactDescriptor::from_recovery_product(&original),
            ArtifactDescriptor::from_recovery_value_interface(&value),
            ArtifactDescriptor::from_recovery_module_interface(&canonical),
        ];
        let interfaces = [(
            descriptors[0].id,
            descriptors[2].id,
            ArtifactDependency::Interface,
        )];
        let mut forged = descriptors.clone();
        forged[1].interface_sha256 = [99; 32];
        assert!(RecoveredArtifactInventory::capture(
            root.path(),
            &[original.clone()],
            &[canonical.clone()],
            &[],
            &[value.clone()],
            &forged,
            &interfaces
        )
        .is_err());
        let bytes = b"another value interface".to_vec();
        let package = value_array([
            value_text("TPPKGROOTS"),
            value_text("2"),
            value_array([
                value_text(&value.interface.unit),
                value_text("Val1"),
                value_text(hex(&sha(&bytes))),
            ]),
            value_array([]),
            value_array([]),
        ]);
        let mut packages = Vec::new();
        ciborium::ser::into_writer(&package, &mut packages).unwrap();
        let interface = crate::recovery_artifacts::CertifiedJoinedInterface::from_certification(
            [2; 32],
            value.interface.unit.clone(),
            "Val1".into(),
            bytes,
            packages,
        )
        .unwrap();
        let alternative =
            crate::recovery_artifacts::CertifiedValueInterface::from_admitted_interface(
                interface,
                vec![],
            )
            .materialize(root.path())
            .unwrap();
        let descriptors = [
            descriptors[0].clone(),
            descriptors[1].clone(),
            descriptors[2].clone(),
            ArtifactDescriptor::from_recovery_value_interface(&alternative),
        ];
        let inventory = RecoveredArtifactInventory::capture(
            root.path(),
            &[original.clone()],
            &[canonical.clone()],
            &[],
            &[value.clone(), alternative],
            &descriptors,
            &interfaces,
        )
        .unwrap();
        assert!(inventory
            .context_all_groups(
                &[descriptors[0].id, descriptors[1].id, descriptors[2].id],
                vec![]
            )
            .is_ok());
        assert!(inventory
            .context_all_groups(
                &[descriptors[0].id, descriptors[3].id, descriptors[2].id],
                vec![]
            )
            .is_ok());
        assert!(inventory
            .context_all_groups(
                &descriptors.iter().map(|entry| entry.id).collect::<Vec<_>>(),
                vec![]
            )
            .is_err());
        std::fs::write(root.path().join(&original.certification_path), b"tampered").unwrap();
        assert!(RecoveredArtifactInventory::capture(
            root.path(),
            &[original],
            &[canonical],
            &[],
            &[value],
            &descriptors[..3],
            &interfaces
        )
        .is_err());
    }

    #[test]
    #[ignore = "requires an exact retained original declaration packet"]
    fn retained_original_package_validation_cost() {
        use std::time::Instant;

        fn io_counters() -> (u64, u64) {
            let counters = std::fs::read_to_string("/proc/thread-self/io").unwrap();
            let value = |key: &str| {
                counters
                    .lines()
                    .find_map(|line| line.strip_prefix(key))
                    .unwrap()
                    .trim()
                    .parse::<u64>()
                    .unwrap()
            };
            (value("rchar:"), value("syscr:"))
        }

        let root = PathBuf::from(std::env::var_os("TIDEPOOL_PACKAGE_VALIDATION_PACKET").unwrap());
        let receipt_bytes = std::fs::read(root.join("certified-products.cbor")).unwrap();
        let product_bytes = std::fs::read(root.join("module-products.cbor")).unwrap();
        let receipt = decode_receipt(&receipt_bytes).unwrap();
        let requirements = crate::prepared_artifact::production_requirements().unwrap();
        let products = parse_module_products(
            &product_bytes,
            &requirements,
            crate::module_candidates::product_decode_limits(),
        )
        .unwrap();
        let home_modules: BTreeSet<_> = receipt
            .modules
            .iter()
            .map(|module| (&module.unit, &module.module))
            .collect();
        let mut imports = Vec::new();
        let mut all_globals = 0;
        for accepted in &receipt.modules {
            let product = matching_product(&products, &accepted.unit, &accepted.module).unwrap();
            assert_eq!(product.groups.len(), accepted.groups.len());
            for (group, accepted) in product.groups.iter().zip(&accepted.groups) {
                assert_eq!(group.original_ordinal(), accepted.original_ordinal);
                assert_eq!(group.globals().len(), accepted.globals.len());
                for (declaration, accepted) in group.globals().iter().zip(&accepted.globals) {
                    all_globals += 1;
                    let import = validate_global_witness(
                        declaration,
                        group.definitions().signatures(),
                        accepted,
                    )
                    .unwrap();
                    if let ReceiptImportOwner::Package { unit, module, .. } = &import {
                        // Source/import resolution is outside this package-I/O measurement.
                        // Preserve the same home-to-package refusal before selecting this row.
                        assert!(!home_modules.contains(&(unit, module)));
                        imports.push(import);
                    }
                }
            }
        }
        assert!(imports.len() >= 100);
        let unique_paths: BTreeSet<_> = receipt
            .packages
            .values()
            .map(|witness| &witness.selected_path)
            .collect();
        let unique_bytes: u64 = unique_paths
            .iter()
            .map(|path| std::fs::metadata(path).unwrap().len())
            .sum();
        for count in [1, 10, 100, imports.len()] {
            let before = io_counters();
            let started = Instant::now();
            let mut validation = PackageInterfaceValidation::default();
            let sources = SourceGroupMap::new();
            for import in &imports[..count] {
                resolve_receipt_owner_with_validation(
                    import.clone(),
                    &sources,
                    None,
                    &receipt.packages,
                    &mut validation,
                )
                .unwrap();
            }
            let elapsed = started.elapsed();
            let after = io_counters();
            println!(
                "PACKAGE_VALIDATION_COST {}",
                serde_json::json!({
                    "packet_receipt_sha256": hex(&sha(&receipt_bytes)),
                    "packet_product_sha256": hex(&sha(&product_bytes)),
                    "modules": receipt.modules.len(), "all_globals": all_globals,
                    "package_globals": imports.len(), "selected_globals": count,
                    "unique_package_paths": unique_paths.len(), "unique_package_bytes": unique_bytes,
                    "nanoseconds": elapsed.as_nanos(), "thread_rchar": after.0-before.0,
                    "thread_read_syscalls": after.1-before.1,
                    "qualification": "actual original package-owner validation only; decode and other owner checks excluded"
                })
            );
        }
    }

    #[test]
    #[ignore = "requires an exact retained original declaration packet"]
    fn retained_original_source_module_lookup_cost() {
        use std::hint::black_box;
        use std::time::Instant;

        let root = PathBuf::from(std::env::var_os("TIDEPOOL_PACKAGE_VALIDATION_PACKET").unwrap());
        let receipt_bytes = std::fs::read(root.join("certified-products.cbor")).unwrap();
        let product_bytes = std::fs::read(root.join("module-products.cbor")).unwrap();
        let receipt = decode_receipt(&receipt_bytes).unwrap();
        let products = parse_module_products(
            &product_bytes,
            &crate::prepared_artifact::production_requirements().unwrap(),
            crate::module_candidates::product_decode_limits(),
        )
        .unwrap();
        // Only exact source-key membership is measured; no source-owner capability
        // is constructed from this worker receipt or used to install a program.
        let mut keys = BTreeMap::<(String, String, u32, SymbolIdentity), ()>::new();
        for accepted in &receipt.modules {
            let product = matching_product(&products, &accepted.unit, &accepted.module).unwrap();
            for group in &product.groups {
                for binder in group.binders() {
                    assert!(keys
                        .insert(
                            (
                                accepted.unit.clone(),
                                accepted.module.clone(),
                                group.original_ordinal(),
                                binder.clone()
                            ),
                            ()
                        )
                        .is_none());
                }
            }
        }
        let setup_started = Instant::now();
        let mut index = SourceModuleIndex::default();
        for (unit, module, _, _) in keys.keys() {
            index.insert(unit, module);
        }
        let setup = setup_started.elapsed();
        let queries: Vec<_> = receipt
            .modules
            .iter()
            .flat_map(|module| &module.groups)
            .flat_map(|group| &group.globals)
            .filter_map(|global| match &global.owner {
                ReceiptImportOwner::Package { unit, module, .. }
                | ReceiptImportOwner::RetainedPackage { unit, module, .. } => Some((unit, module)),
                _ => None,
            })
            .collect();
        assert!(queries.len() >= 100);
        for count in [1, 10, 100, queries.len()] {
            for algorithm in ["flat_source_keys", "indexed_source_modules"] {
                let started = Instant::now();
                for (unit, module) in &queries[..count] {
                    let (unit, module) = black_box((*unit, *module));
                    let exists = if algorithm == "flat_source_keys" {
                        keys.keys().any(|(home_unit, home_module, _, _)| {
                            home_unit == unit && home_module == module
                        })
                    } else {
                        index.contains(unit, module)
                    };
                    assert!(!black_box(exists));
                }
                let elapsed = started.elapsed();
                println!(
                    "SOURCE_MODULE_LOOKUP_COST {}",
                    serde_json::json!({
                        "packet_receipt_sha256": hex(&sha(&receipt_bytes)),
                        "packet_product_sha256": hex(&sha(&product_bytes)),
                        "algorithm": algorithm, "source_keys": keys.len(), "queries": count,
                        "indexed_units": index.0.len(), "indexed_modules": index.0.values().map(BTreeSet::len).sum::<usize>(),
                        "index_setup_nanoseconds": setup.as_nanos(), "nanoseconds": elapsed.as_nanos(),
                        "qualification": "exact captured source-key predicate only; flat map omits unused owner values; not full validation or prefix latency"
                    })
                );
            }
        }
        for (unit, modules) in &index.0 {
            for module in modules {
                assert!(keys.keys().any(
                    |(home_unit, home_module, _, _)| home_unit == unit && home_module == module
                ));
                assert!(index.contains(unit, module));
            }
        }
    }

    #[test]
    fn original_product_admission_shares_package_reads_and_rechecks_next_admission() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Package.hi");
        let bytes = vec![0x42; 131_073];
        std::fs::write(&path, &bytes).unwrap();
        let producer = [2; 32];
        let package_binder = SymbolIdentity {
            unit: "package-unit".into(),
            module: "Package.Module".into(),
            namespace: "value".into(),
            occurrence: "packageValue".into(),
            record_parent: None,
        };
        let package_program = |module: &str| {
            use tidepool_repr::execution_schema::{
                Atom, ExprFrame, GlobalId, Group, SignatureId, ValueRef,
            };
            let mut wire = testing::wire_program();
            let Group::NonRecursive(binding) = &mut wire.bindings[0] else {
                unreachable!()
            };
            binding.identity.unit = "home-unit".into();
            binding.identity.module = module.into();
            wire.globals.push(GlobalDecl {
                identity: package_binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: Some(SignatureId(0)),
                required_evaluated: false,
                required_generation: None,
            });
            wire.expressions.nodes[0] = ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
                signature: SignatureId(0),
                arguments: Vec::new(),
            };
            wire
        };
        let make_products = |names: &[String]| {
            let packages = BTreeMap::from([(
                ("package-unit".into(), "Package.Module".into()),
                PackageInterfaceWitness {
                    selected_path: path.clone(),
                    sha256: sha(&std::fs::read(&path).unwrap()),
                },
            )]);
            let products = names
                .iter()
                .map(|name| {
                    let interface = fixture_source_module_interface(
                        producer,
                        "home-unit",
                        name,
                        [1; 32],
                        BTreeMap::new(),
                        Some(&path),
                    );
                    let group =
                        Arc::new(testing::projected_group(package_program(name), 7).unwrap());
                    let product_bytes =
                        tidepool_test_data::prepared_encode::encode_module_products(&[
                            RawModuleProduct {
                                unit: "home-unit".into(),
                                module: name.clone(),
                                interface: interface.interface_bytes().to_vec(),
                                groups: vec![group.as_ref().clone()],
                            },
                        ]);
                    let owner = CachedHomeOwner {
                        unit: "home-unit".into(),
                        module: name.clone(),
                        module_version: ModuleVersion([1; 32]),
                        skinny_iface_sha256: interface.interface_sha256(),
                        product_sha256: sha(&product_bytes),
                    };
                    let group = PendingCertifiedGroup {
                        owner: owner.clone(),
                        origin: ProductOrigin::Fresh,
                        group,
                        imports: vec![PendingImportOwner::Package {
                            unit: package_binder.unit.clone(),
                            module: package_binder.module.clone(),
                            binder: package_binder.clone(),
                            interface_digest: packages
                                [&(package_binder.unit.clone(), package_binder.module.clone())]
                                .sha256,
                        }]
                        .into(),
                    };
                    let certification = encode_home_certification_with_module(
                        &owner,
                        &[group],
                        &packages,
                        interface.requirements(),
                        sha(interface.certificate_bytes()),
                    )
                    .unwrap();
                    let witness = decode_home_witness(&certification).unwrap();
                    assert_eq!(witness.packages, packages);
                    assert_eq!(witness.groups.len(), 1);
                    assert_eq!(witness.groups[0].2.len(), 1);
                    crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
                        owner,
                        interface.interface_bytes().to_vec(),
                        product_bytes,
                        interface.package_imports_bytes().to_vec(),
                        certification,
                    )
                    .with_module_interface(interface)
                    .unwrap()
                })
                .collect::<Vec<_>>();
            (products, packages)
        };
        let names = (0..17).map(|i| format!("Home{i}")).collect::<Vec<_>>();
        let (products, packages) = make_products(&names);
        let target = Arc::new(testing::prepare(package_program("Target")).unwrap());
        let observed = crate::recovery_artifacts::package_interface_io;
        let before = observed();
        let mut admission = PackageInterfaceValidation::default();
        let target_interfaces =
            certify_target_package_interfaces_with_validation(&target, &packages, &mut admission)
                .unwrap();
        let view = crate::declaration_context::certified_product_artifact_view_with_validation(
            producer,
            &products,
            &[],
            &[],
            None,
            crate::artifact_inventory::NativeArtifactDemand::AllGroups,
            &mut admission,
        )
        .unwrap();
        use crate::artifact_inventory::ArtifactKind;
        let expected_roles = names
            .iter()
            .flat_map(|name| {
                [
                    ArtifactKind::OriginalModule,
                    ArtifactKind::CanonicalModuleInterface,
                ]
                .map(|kind| ("home-unit".to_owned(), name.clone(), kind))
            })
            .collect::<BTreeSet<_>>();
        let descriptors = view.descriptors();
        let actual_roles = descriptors
            .iter()
            .map(|entry| {
                (
                    entry.owner.unit.clone(),
                    entry.owner.module.clone(),
                    entry.kind,
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(actual_roles, expected_roles);
        assert_eq!(descriptors.len(), expected_roles.len());
        assert!(target_interfaces.matches_target(&target));
        let after = observed();
        assert_eq!(after.0 - before.0, 1);
        assert_eq!(after.1 - before.1, bytes.len() as u64);
        assert_eq!(admission.work().read_bytes, bytes.len() as u64);
        assert_eq!(admission.work().hash_bytes, bytes.len() as u64);
        assert!(
            crate::declaration_context::certified_product_artifact_view_with_validation(
                [3; 32],
                &products,
                &[],
                &[],
                None,
                crate::artifact_inventory::NativeArtifactDemand::AllGroups,
                &mut admission,
            )
            .is_err(),
            "a captured path does not confer another producer's authority"
        );

        // The former per-original validation shape rereads this same demanded
        // package seventeen times. Shared admission must remain sensitive to it.
        let before = observed();
        for product in &products {
            crate::artifact_inventory::ArtifactEntry::original(producer, product.clone()).unwrap();
        }
        let after = observed();
        assert_eq!(after.0 - before.0, products.len() as u64);
        assert_eq!(
            after.1 - before.1,
            products.len() as u64 * bytes.len() as u64
        );

        // Another admission must really open/read even unchanged package bytes.
        let before = observed();
        let mut next = PackageInterfaceValidation::default();
        crate::declaration_context::certified_product_artifact_view_with_validation(
            producer,
            &products,
            &[],
            &[],
            None,
            crate::artifact_inventory::NativeArtifactDemand::AllGroups,
            &mut next,
        )
        .unwrap();
        let after = observed();
        assert_eq!(after.0 - before.0, 1);
        assert_eq!(after.1 - before.1, bytes.len() as u64);
        assert_eq!(next.work().read_bytes, bytes.len() as u64);

        let replacement = root.path().join("replacement.hi");
        std::fs::write(&replacement, vec![0x43; bytes.len()]).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let before = observed();
        let mut changed = PackageInterfaceValidation::default();
        assert!(
            crate::declaration_context::certified_product_artifact_view_with_validation(
                producer,
                &products,
                &[],
                &[],
                None,
                crate::artifact_inventory::NativeArtifactDemand::AllGroups,
                &mut changed,
            )
            .is_err()
        );
        let after = observed();
        assert_eq!(after.0 - before.0, 1);
        assert_eq!(after.1 - before.1, bytes.len() as u64);
        assert_eq!(changed.work().read_bytes, bytes.len() as u64);

        let (replacement_products, _) = make_products(&["Replacement".into()]);
        let before = observed();
        assert!(
            crate::declaration_context::certified_product_artifact_view_with_validation(
                producer,
                &replacement_products,
                &[],
                &[],
                None,
                crate::artifact_inventory::NativeArtifactDemand::AllGroups,
                &mut admission,
            )
            .is_err(),
            "the same path with a different expected digest cannot reuse a capture"
        );
        assert_eq!(observed(), before);
    }

    #[test]
    fn target_package_interfaces_remain_bound_to_exact_target_and_selected_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Package.hi");
        std::fs::write(&path, b"selected interface").unwrap();
        let packages = BTreeMap::from([(
            ("fixture-unit".into(), "Package".into()),
            PackageInterfaceWitness {
                selected_path: path.clone(),
                sha256: sha(b"selected interface"),
            },
        )]);
        let target = std::sync::Arc::new(testing::prepare(testing::wire_program()).unwrap());
        let retained = certify_target_package_interfaces(&target, &packages).unwrap();
        assert!(std::sync::Arc::ptr_eq(
            retained.target.as_ref().unwrap(),
            &target
        ));
        assert!(retained.matches_target(&target));
        assert_eq!(
            retained.interface_digest("fixture-unit", "Package"),
            Some(sha(b"selected interface"))
        );
        assert_eq!(retained.interface_digest("other-unit", "Package"), None);
        let mut other = testing::wire_program();
        let tidepool_repr::execution_schema::Group::NonRecursive(binding) = &mut other.bindings[0]
        else {
            unreachable!()
        };
        binding.identity.occurrence.push_str("Changed");
        assert!(!retained.matches_target(&testing::prepare(other).unwrap()));
        assert!(!CertifiedTargetPackageInterfaces::default().matches_target(&target));
        std::fs::write(&path, b"changed interface").unwrap();
        assert!(matches!(
            certify_target_package_interfaces(&target, &packages),
            Err(CertificationError::StaleEvidence)
        ));
    }

    fn inherited_owner(module: &str) -> CachedHomeOwner {
        CachedHomeOwner {
            unit: "fixture".into(),
            module: module.into(),
            module_version: ModuleVersion([7; 32]),
            skinny_iface_sha256: sha(&[0x42]),
            product_sha256: [8; 32],
        }
    }

    #[test]
    fn source_group_module_index_preserves_exact_lookup_and_package_refusal() {
        let owner = inherited_owner("Home");
        let binder = SymbolIdentity {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            namespace: "value".into(),
            occurrence: "original".into(),
            record_parent: None,
        };
        let key = (owner.clone(), 7, binder.clone());
        let mut sources = SourceGroupMap::new();
        assert!(sources
            .insert(key.clone(), (owner.clone(), ProductOrigin::Cached))
            .is_none());
        assert!(sources.contains_module("fixture", "Home"));
        assert!(!sources.contains_module("another", "Home"));
        assert!(!sources.contains_module("fixture", "HomeOther"));
        assert_eq!(
            sources.get(&key),
            Some(&(owner.clone(), ProductOrigin::Cached))
        );
        let source = ReceiptImportOwner::Source {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            module_version: Some(owner.module_version.clone()),
            original_ordinal: 7,
            binder: binder.clone(),
        };
        assert!(matches!(
            resolve_receipt_owner(source, &sources, &BTreeMap::new()),
            Ok(PendingImportOwner::Source { .. })
        ));
        let downgrade = ReceiptImportOwner::Package {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            binder,
            interface_digest: [1; 32],
        };
        assert!(matches!(
            resolve_receipt_owner(downgrade, &sources, &BTreeMap::new()),
            Err(CertificationError::Mismatch(
                "home owner downgraded to package"
            ))
        ));
        assert!(sources
            .insert(key, (owner, ProductOrigin::Cached))
            .is_some());
    }

    fn inherited_group(
        owner: &CachedHomeOwner,
        import: PendingImportOwner,
    ) -> PendingCertifiedGroup {
        let mut wire = testing::wire_program();
        let tidepool_repr::execution_schema::Group::NonRecursive(top) = &mut wire.bindings[0]
        else {
            unreachable!()
        };
        top.identity = testing::identity(&owner.module, "entry");
        let (identity, required_generation) = match &import {
            PendingImportOwner::Source { binder, .. }
            | PendingImportOwner::Package { binder, .. } => (binder.clone(), None),
            PendingImportOwner::Retained {
                identity,
                generation,
            } => (identity.clone(), Some(*generation)),
            PendingImportOwner::RetainedPackage {
                binder, generation, ..
            } => (binder.clone(), Some(*generation)),
        };
        wire.globals.push(GlobalDecl {
            identity,
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation,
        });
        PendingCertifiedGroup {
            owner: owner.clone(),
            origin: ProductOrigin::Cached,
            group: Arc::new(testing::projected_group(wire, 7).unwrap()),
            imports: vec![import].into(),
        }
    }

    #[test]
    fn compile_input_proof_rejects_substituted_dependency_bundle() {
        let source = "module Fresh where";
        let target = Arc::new(testing::prepare(testing::wire_program()).unwrap());
        let packages = BTreeMap::new();
        let interfaces = certify_target_package_interfaces(&target, &packages).unwrap();
        let owner = inherited_owner("Driver");
        let import = PendingImportOwner::Source {
            owner: owner.clone(),
            original_ordinal: 1,
            binder: testing::identity("Driver", "entry"),
        };
        let groups: Arc<[_]> = vec![inherited_group(&owner, import.clone())].into();
        let table = tidepool_repr::DataConTable::default();
        let producer = crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
            b"admitted-producer",
        )
        .sha256();
        let artifacts = crate::declaration_context::certified_product_artifact_view(
            producer,
            &[],
            &[fixture_module_interface(
                producer,
                &owner.unit,
                &owner.module,
                BTreeMap::new(),
            )],
            None,
        )
        .unwrap();
        let proof = crate::compile_input::seal(
            b"admitted-producer",
            &[],
            &evidence(source),
            &crate::compile_input::ValidatedInputPackages::fixture(packages.clone()),
            source,
            "root",
            &target,
            &groups,
            &[import.clone()],
            &interfaces,
            table.clone(),
            vec![],
            &artifacts,
        )
        .unwrap()
        .unwrap();
        assert!(proof.matches_bundle(
            &target,
            &groups,
            &[import.clone()],
            &interfaces,
            &table,
            &[]
        ));
        let mut changed_owner = owner.clone();
        changed_owner.module_version = ModuleVersion([99; 32]);
        let changed_groups = vec![inherited_group(&changed_owner, import.clone())];
        assert!(!proof.matches_bundle(
            &target,
            &changed_groups,
            &[import.clone()],
            &interfaces,
            &table,
            &[]
        ));
        let changed_import = PendingImportOwner::Source {
            owner: owner.clone(),
            original_ordinal: 2,
            binder: testing::identity("Driver", "entry"),
        };
        assert!(!proof.matches_bundle(
            &target,
            &groups,
            &[changed_import],
            &interfaces,
            &table,
            &[]
        ));
        assert!(!proof.matches_bundle(
            &target,
            &groups,
            &[import],
            &CertifiedTargetPackageInterfaces::default(),
            &table,
            &[]
        ));
    }

    #[test]
    fn certified_group_clones_share_decoded_payload_and_import_inventory() {
        let group = inherited_group(
            &inherited_owner("Home"),
            PendingImportOwner::Retained {
                identity: testing::identity("Val", "x"),
                generation: 7,
            },
        );
        let copy = group.clone();
        assert!(Arc::ptr_eq(&group.group, &copy.group));
        assert!(Arc::ptr_eq(&group.imports, &copy.imports));
        let (owner, decoded, imports) = copy.into_parts();
        assert_eq!(owner, *group.owner());
        assert_eq!(&decoded, group.group());
        assert_eq!(imports, group.imports());
    }

    fn inherited_parsed(
        group: &PendingCertifiedGroup,
        packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    ) -> (RawModuleProduct, HomeCertification) {
        let bytes = encode_home_certification(group.owner(), std::slice::from_ref(group), packages)
            .unwrap();
        (
            RawModuleProduct {
                unit: group.owner.unit.clone(),
                module: group.owner.module.clone(),
                interface: vec![0x42],
                groups: vec![group.group.as_ref().clone()],
            },
            decode_home_witness(&bytes).unwrap(),
        )
    }

    #[test]
    fn artifact_native_edges_preserve_original_group_and_binding_generations() {
        use crate::artifact_inventory::ArtifactDependency;
        let owner = inherited_owner("Consumer");
        let source = inherited_owner("Original");
        let source_group = inherited_group(
            &owner,
            PendingImportOwner::Source {
                owner: source.clone(),
                original_ordinal: 11,
                binder: testing::identity("Original", "entry"),
            },
        );
        let bytes = encode_home_certification(&owner, &[source_group], &BTreeMap::new()).unwrap();
        let edges = certified_native_requirements(&bytes, &owner)
            .unwrap()
            .artifact_edges;
        assert_eq!(
            edges,
            vec![(
                crate::declaration_join::ExactModuleIdentity {
                    unit: source.unit,
                    module: source.module
                },
                ArtifactDependency::NativeGroup {
                    dependent_ordinal: 7,
                    required_ordinal: 11
                }
            )]
        );
        let binding = testing::identity("Value", "bound");
        let retained_group = inherited_group(
            &owner,
            PendingImportOwner::Retained {
                identity: binding.clone(),
                generation: 41,
            },
        );
        let bytes = encode_home_certification(&owner, &[retained_group], &BTreeMap::new()).unwrap();
        assert!(
            matches!(&certified_native_requirements(&bytes,&owner).unwrap().artifact_edges[0].1,ArtifactDependency::NativeBinding{dependent_ordinal:7,generation:41,occurrence,..} if occurrence=="bound")
        );
        assert!(certified_native_requirements(&bytes, &inherited_owner("Imposter")).is_err());
        let original = original_witness_fixture(
            "Consumer",
            Some(PendingImportOwner::Retained {
                identity: binding,
                generation: 41,
            }),
            9,
            &BTreeMap::new(),
        );
        let expected =
            certified_native_requirements(original.certification_bytes(), original.owner())
                .unwrap();
        let recovered = recovered_witness_fixtures(&[original]);
        let certificate_decodes = HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get);
        assert_eq!(
            original_native_requirements(&recovered[0].product).unwrap(),
            expected
        );
        assert_eq!(
            HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get),
            certificate_decodes
        );
    }

    pub(crate) fn original_witness_fixture(
        module: &str,
        import: Option<PendingImportOwner>,
        version: u8,
        packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    ) -> crate::recovery_artifacts::CertifiedRecoveryProduct {
        let groups = import.into_iter().map(|import| (7, vec![import])).collect();
        original_groups_fixture(module, groups, version, packages)
    }

    pub(crate) fn original_groups_fixture(
        module: &str,
        groups: Vec<(u32, Vec<PendingImportOwner>)>,
        version: u8,
        packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    ) -> crate::recovery_artifacts::CertifiedRecoveryProduct {
        original_groups_fixture_with_interface(module, groups, version, packages, vec![0x42])
    }

    pub(crate) fn original_groups_fixture_with_interface(
        module: &str,
        groups: Vec<(u32, Vec<PendingImportOwner>)>,
        version: u8,
        packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
        interface: Vec<u8>,
    ) -> crate::recovery_artifacts::CertifiedRecoveryProduct {
        let projected = groups
            .iter()
            .map(|(ordinal, imports)| {
                let mut wire = testing::wire_program();
                let tidepool_repr::execution_schema::Group::NonRecursive(top) =
                    &mut wire.bindings[0]
                else {
                    unreachable!()
                };
                top.identity = testing::identity(module, &format!("entry_{ordinal}"));
                // The one-group fixture retains its existing entry identity.
                if groups.len() == 1 && *ordinal == 7 {
                    top.identity = testing::identity(module, "entry");
                }
                top.binding.rhs = tidepool_repr::execution_schema::HeapRhs::Bytes(Vec::new());
                let top = top.clone();
                wire.bindings = vec![tidepool_repr::execution_schema::Group::Recursive(vec![top])];
                wire.expressions.nodes.clear();
                wire.globals = imports
                    .iter()
                    .map(|import| {
                        let (identity, required_generation) = match import {
                            PendingImportOwner::Source { binder, .. }
                            | PendingImportOwner::Package { binder, .. } => (binder, None),
                            PendingImportOwner::Retained {
                                identity,
                                generation,
                            } => (identity, Some(*generation)),
                            PendingImportOwner::RetainedPackage {
                                binder, generation, ..
                            } => (binder, Some(*generation)),
                        };
                        GlobalDecl {
                            identity: identity.clone(),
                            rep: RuntimeRep::LiftedRef,
                            entry_signature: None,
                            required_evaluated: false,
                            required_generation,
                        }
                    })
                    .collect();
                testing::projected_group(wire, *ordinal).unwrap()
            })
            .collect();
        let product_bytes =
            tidepool_test_data::prepared_encode::encode_module_products(&[RawModuleProduct {
                unit: "fixture".into(),
                module: module.into(),
                interface: interface.clone(),
                groups: projected,
            }]);
        let owner = CachedHomeOwner {
            unit: "fixture".into(),
            module: module.into(),
            module_version: ModuleVersion([version; 32]),
            skinny_iface_sha256: sha(&interface),
            product_sha256: sha(&product_bytes),
        };
        let raw = parse_module_products(
            &product_bytes,
            &crate::prepared_artifact::production_requirements().unwrap(),
            crate::module_candidates::product_decode_limits(),
        )
        .unwrap()
        .pop()
        .unwrap();
        let groups = raw
            .groups
            .into_iter()
            .zip(groups)
            .map(|(group, (_, imports))| PendingCertifiedGroup {
                owner: owner.clone(),
                origin: ProductOrigin::Fresh,
                group: Arc::new(group),
                imports: imports.into(),
            })
            .collect::<Vec<_>>();
        let seal = encode_home_certification(&owner, &groups, packages).unwrap();
        let package_bytes = crate::module_candidates::tests::package_imports_with_roots(
            &owner.unit,
            &owner.module,
            &interface,
            Vec::new(),
        );
        crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            owner,
            interface,
            product_bytes,
            package_bytes,
            seal,
        )
    }

    pub(crate) fn recovered_witness_fixtures(
        products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
    ) -> Vec<CertifiedRecoveredOriginal> {
        let root = tempfile::tempdir().unwrap();
        let producer = products
            .first()
            .and_then(|product| product.module_interface())
            .map_or([1; 32], CertifiedModuleInterface::producer_sha256);
        let references = crate::recovery_artifacts::materialize_certified_products(
            root.path(),
            producer,
            products,
        )
        .unwrap();
        let verified = references
            .iter()
            .map(|reference| {
                crate::recovery_artifacts::verify_materialized_ref(root.path(), reference).unwrap()
            })
            .collect();
        certify_recovery_products_with_validation(
            verified,
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap()
    }

    fn full_native_fixture(
        module: &str,
        groups: Vec<(u32, Vec<PendingImportOwner>)>,
        version: u8,
    ) -> crate::recovery_artifacts::CertifiedRecoveryProduct {
        recovered_witness_fixtures(&[fixture_finalized_product(
            original_groups_fixture(module, groups, version, &BTreeMap::new()),
            [1; 32],
        )])
        .remove(0)
        .product
    }

    fn check_original_native_index_against_linear_scan(raw: &[(u32, bool, bool)], query: u32) {
        use crate::artifact_inventory::{ArtifactId, NativeGroupKey};
        let original = full_native_fixture(
            "Indexed",
            raw.iter()
                .map(|(ordinal, has_import, _)| {
                    let imports = has_import
                        .then(|| PendingImportOwner::Retained {
                            identity: testing::identity("Captured", &format!("value_{ordinal}")),
                            generation: u64::from(*ordinal) + 1,
                        })
                        .into_iter()
                        .collect();
                    (*ordinal, imports)
                })
                .collect(),
            7,
        );
        let native = original.original_native().unwrap();
        assert_eq!(
            native
                .groups
                .iter()
                .map(|group| group.group.original_ordinal())
                .collect::<Vec<_>>(),
            raw.iter()
                .map(|(ordinal, _, _)| *ordinal)
                .collect::<Vec<_>>(),
            "issuance preserves authenticated group order",
        );
        for ordinal in raw.iter().map(|(ordinal, _, _)| *ordinal).chain([query]) {
            let expected = native
                .groups
                .iter()
                .find(|group| group.group.original_ordinal() == ordinal);
            let actual = native.group(ordinal);
            assert_eq!(actual.is_some(), expected.is_some());
            if let (Some(actual), Some(expected)) = (actual, expected) {
                assert!(std::ptr::eq(actual, expected));
                assert!(Arc::ptr_eq(&actual.group, &expected.group));
                assert!(Arc::ptr_eq(&actual.imports, &expected.imports));
            }
            let binder = testing::identity("Indexed", &format!("entry_{ordinal}"));
            // The existing one-group fixture has a special binder for ordinal 7.
            let binder = if raw.len() == 1 && ordinal == 7 {
                testing::identity("Indexed", "entry")
            } else {
                binder
            };
            assert_eq!(
                authenticates_original_native_entry(&original, ordinal, &binder),
                expected.is_some_and(|group| group.group.binders().contains(&binder)),
            );
            let mut wrong_owner = binder.clone();
            wrong_owner.module = "Other".into();
            assert!(!authenticates_original_native_entry(
                &original,
                ordinal,
                &wrong_owner
            ));
            wrong_owner = binder.clone();
            wrong_owner.unit = "other-unit".into();
            assert!(!authenticates_original_native_entry(
                &original,
                ordinal,
                &wrong_owner
            ));
            let mut missing_binder = binder;
            missing_binder.occurrence = "missing".into();
            assert!(!authenticates_original_native_entry(
                &original,
                ordinal,
                &missing_binder
            ));
        }
        let current = native
            .groups
            .iter()
            .rev()
            .map(|group| (group.group(), group.imports().to_vec()))
            .collect::<Vec<_>>();
        native.validate_promoted_groups(&current).unwrap();

        let artifact = ArtifactId([3; 32]);
        let available = BTreeMap::from([(artifact, &original)]);
        let selected = raw
            .iter()
            .filter(|(_, _, select)| *select)
            .map(|(ordinal, _, _)| NativeGroupKey {
                artifact,
                original_ordinal: *ordinal,
            })
            .collect::<BTreeSet<_>>();
        let admitted = certify_selected_owned_products_in_context_with_validation(
            &available,
            &[],
            &selected,
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        assert_eq!(admitted.len(), selected.len());
        for (actual, key) in admitted.iter().zip(&selected) {
            let expected = native
                .groups
                .iter()
                .find(|group| group.group.original_ordinal() == key.original_ordinal)
                .unwrap();
            assert_eq!(actual.owner(), &expected.owner);
            assert!(Arc::ptr_eq(&actual.group, &expected.group));
            assert!(Arc::ptr_eq(&actual.imports, &expected.imports));
        }
        assert!(certify_selected_owned_products_in_context_with_validation(
            &available,
            &admitted,
            &selected,
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap()
        .is_empty());
        if !raw.iter().any(|(ordinal, _, _)| *ordinal == query) {
            assert!(matches!(
                certify_selected_owned_products_in_context_with_validation(
                    &available,
                    &[],
                    &BTreeSet::from([NativeGroupKey {
                        artifact,
                        original_ordinal: query
                    }]),
                    &mut PackageInterfaceValidation::default(),
                ),
                Err(CertificationError::Mismatch("selected original group"))
            ));
        }
    }

    #[test]
    fn original_native_index_preserves_sparse_order_and_selected_identity() {
        check_original_native_index_against_linear_scan(
            &[(u32::MAX, true, true), (0, false, false), (29, true, true)],
            1,
        );
        check_original_native_index_against_linear_scan(&[], u32::MAX);
        check_original_native_index_against_linear_scan(&[(7, false, true)], 7);
    }

    #[test]
    fn original_native_index_issuer_rejects_duplicate_ordinals_and_wrong_owners() {
        let original = full_native_fixture("Indexed", vec![(29, vec![]), (3, vec![])], 7);
        let native = original.original_native().unwrap();
        let witness =
            || verify_home_witness(original.certification_bytes(), original.owner()).unwrap();
        let mut duplicate = native.groups.to_vec();
        duplicate.push(duplicate[0].clone());
        assert!(matches!(
            retain_authenticated_original_native(original.clone(), duplicate, witness()),
            Err(CertificationError::Mismatch(
                "duplicate original native ordinal"
            )),
        ));
        let mut wrong_owner = native.groups.to_vec();
        wrong_owner[0].owner.module_version = ModuleVersion([8; 32]);
        assert!(matches!(
            retain_authenticated_original_native(original.clone(), wrong_owner, witness()),
            Err(CertificationError::Mismatch(
                "original native witness owner"
            )),
        ));
        let mut wrong_witness = witness();
        wrong_witness.owner.module_version = ModuleVersion([8; 32]);
        assert!(matches!(
            retain_authenticated_original_native(
                original.clone(),
                native.groups.to_vec(),
                wrong_witness
            ),
            Err(CertificationError::Mismatch(
                "original native witness owner"
            )),
        ));
    }

    fn original_native_index_property_config() -> proptest::test_runner::Config {
        let mut config = proptest::test_runner::Config::default();
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(
                proptest::test_runner::FileFailurePersistence::Direct(path),
            ));
        }
        config
    }

    proptest::proptest! {
        #![proptest_config(original_native_index_property_config())]
        #[test]
        fn original_native_index_matches_linear_scan_for_generated_sparse_groups(
            groups in proptest::collection::vec((proptest::prelude::any::<u32>(),
                proptest::prelude::any::<bool>(), proptest::prelude::any::<bool>()), 0..48),
            query in proptest::prelude::any::<u32>(),
        ) {
            let mut seen = BTreeSet::new();
            let groups = groups.into_iter().filter(|(ordinal, _, _)| seen.insert(*ordinal)).collect::<Vec<_>>();
            check_original_native_index_against_linear_scan(&groups, query);
        }
    }

    #[test]
    fn issued_interface_upgrade_preserves_exact_choice_and_native_authority() {
        use crate::artifact_inventory::{
            ArtifactEntry, ArtifactInventory, CompilerInputProjection,
        };
        let original = full_native_fixture("Upgrade", vec![(3, vec![])], 7);
        let canonical = original.module_interface().unwrap();
        let interface = Arc::new(ArtifactEntry::canonical(canonical.clone()));
        let native = Arc::new(
            ArtifactEntry::original(canonical.producer_sha256(), original.clone()).unwrap(),
        );
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit_recovery_selection(
                &inventory.empty_view(),
                vec![native, interface.clone()],
                &BTreeSet::new(),
            )
            .unwrap();
        let issued = CompilerInputProjection::from_issued_entries(&[interface]).unwrap();
        let operation = InventoryOperation::new(Default::default());
        let mut selection = CertifiedSourceSelection::from_compiler_projection(
            &issued,
            &view.metadata_snapshot(),
            &operation,
        )
        .unwrap();
        assert_eq!(selection.selected_original_owners().count(), 0);
        assert_eq!(selection.compiler_projection(&view).unwrap(), issued);
        let sources =
            available_original_source_map(std::slice::from_ref(&original), &[], &operation)
                .unwrap();
        assert!(matches!(
            resolve_receipt_owner_with_validation(
                ReceiptImportOwner::Source {
                    unit: original.owner().unit.clone(),
                    module: original.owner().module.clone(),
                    module_version: Some(original.owner().module_version.clone()),
                    original_ordinal: 3,
                    binder: testing::identity("Upgrade", "entry_3")
                },
                &sources,
                Some(&selection),
                &BTreeMap::new(),
                &mut PackageInterfaceValidation::default(),
            ),
            Err(CertificationError::Mismatch(
                "source outside compiler selection"
            ))
        ));

        let other = recovered_witness_fixtures(&[fixture_finalized_product(
            original_groups_fixture_with_interface(
                "Upgrade",
                vec![(3, vec![])],
                9,
                &BTreeMap::new(),
                vec![0x43],
            ),
            [1; 32],
        )])
        .remove(0)
        .product;
        assert_ne!(
            ArtifactEntry::canonical(other.module_interface().unwrap().clone())
                .descriptor
                .id,
            issued.roles()[0].interface()
        );
        assert!(matches!(
            selection.admit_current(
                other.owner(),
                ProductOrigin::Fresh,
                other.module_interface().unwrap(),
                &operation
            ),
            Err(CertificationError::Mismatch(
                "compiler selected interface replaced"
            ))
        ));
        assert_eq!(selection.compiler_projection(&view).unwrap(), issued);
        selection
            .admit_current(
                original.owner(),
                ProductOrigin::Fresh,
                canonical,
                &operation,
            )
            .unwrap();
        assert_eq!(
            selection.selected_original_owners().collect::<Vec<_>>(),
            vec![original.owner()]
        );
        let upgraded = selection.compiler_projection(&view).unwrap();
        assert_eq!(
            upgraded.roles()[0].interface(),
            issued.roles()[0].interface()
        );
        assert!(upgraded.roles()[0].original().is_some());
        selection
            .admit_current(
                original.owner(),
                ProductOrigin::Cached,
                canonical,
                &operation,
            )
            .unwrap();
        assert_eq!(selection.compiler_projection(&view).unwrap(), upgraded);
        assert!(matches!(
            selection.admit_current(
                original.owner(),
                ProductOrigin::Fresh,
                canonical,
                &operation
            ),
            Err(CertificationError::Mismatch(
                "compiler original owner replaced"
            ))
        ));
        assert_eq!(selection.compiler_projection(&view).unwrap(), upgraded);
    }

    #[test]
    fn full_original_availability_is_read_only_and_checks_selected_authority() {
        let operation = InventoryOperation::new(Default::default());
        let original = full_native_fixture("Available", vec![(8, vec![]), (11, vec![])], 7);
        let sources =
            available_original_source_map(std::slice::from_ref(&original), &[], &operation)
                .unwrap();
        let owner = original.owner();
        let key = (owner.clone(), 8, testing::identity("Available", "entry_8"));
        assert_eq!(
            sources.get(&key),
            Some(&(owner.clone(), ProductOrigin::Cached))
        );
        assert_eq!(original.original_native().unwrap().groups.len(), 2);
        let selected = original.original_native().unwrap().groups[0].admitted();
        available_original_source_map(
            std::slice::from_ref(&original),
            std::slice::from_ref(&selected),
            &operation,
        )
        .unwrap();
        let mut wrong = selected.clone();
        wrong.owner.module_version = ModuleVersion([9; 32]);
        assert!(available_original_source_map(
            std::slice::from_ref(&original),
            &[wrong],
            &operation
        )
        .is_err());
        let mut wrong = selected.clone();
        wrong.imports = vec![PendingImportOwner::Retained {
            identity: testing::identity("Available", "captured"),
            generation: 9,
        }]
        .into();
        assert!(available_original_source_map(
            std::slice::from_ref(&original),
            &[wrong],
            &operation
        )
        .is_err());
        let duplicate = available_original_source_map(
            std::slice::from_ref(&original),
            &[selected.clone(), selected.clone()],
            &operation,
        );
        let Err(CertificationError::DuplicateSourceBinder(conflict)) = duplicate else {
            panic!("duplicate selected groups must retain their exact conflict");
        };
        assert_eq!(
            *conflict,
            SourceBinderConflict {
                phase: SourceBinderPhase::SelectedGroups,
                owner: selected.owner.clone(),
                original_ordinal: selected.group.original_ordinal(),
                binder: selected.group.binders()[0].clone(),
                existing_origin: selected.origin,
                incoming_origin: selected.origin,
            }
        );
        assert!(available_original_source_map(
            &[original.clone(), original.clone()],
            &[],
            &operation
        )
        .is_err());
        let other = full_native_fixture("Available", vec![(8, vec![]), (11, vec![])], 9);
        available_original_source_map(&[original.clone(), other], &[], &operation).unwrap();
        let empty = full_native_fixture("EmptyNative", vec![], 7);
        let empty_sources = available_original_source_map(&[empty], &[], &operation).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let selected_path = directory.path().join("EmptyNative.hi");
        std::fs::write(&selected_path, b"otherwise valid package interface").unwrap();
        let interface_digest = sha(b"otherwise valid package interface");
        let packages = BTreeMap::from([(
            ("fixture".into(), "EmptyNative".into()),
            PackageInterfaceWitness {
                selected_path,
                sha256: interface_digest,
            },
        )]);
        let package = ReceiptImportOwner::Package {
            unit: "fixture".into(),
            module: "EmptyNative".into(),
            binder: testing::identity("EmptyNative", "unavailable"),
            interface_digest,
        };
        resolve_receipt_owner(package.clone(), &SourceGroupMap::new(), &packages).unwrap();
        assert!(matches!(
            resolve_receipt_owner(package, &empty_sources, &packages),
            Err(CertificationError::Mismatch(
                "home owner downgraded to package"
            ))
        ));
        assert!(operation.work_usage().unwrap().0 > 0);
        let exhausted = InventoryOperation::new(InventoryDecodeLimits {
            max_work: 0,
            ..Default::default()
        });
        assert!(available_original_source_map(&[original], &[], &exhausted).is_err());
    }

    #[test]
    fn source_receipts_use_request_selection_without_collapsing_original_versions() {
        let captured = |generation| PendingImportOwner::Retained {
            identity: testing::identity("Val.G1", "captured"),
            generation,
        };
        let old = full_native_fixture("Shared", vec![(3, vec![captured(1)]), (11, vec![])], 7);
        let new = full_native_fixture("Shared", vec![(3, vec![captured(2)]), (11, vec![])], 9);
        let products = [old.clone(), new.clone()];
        let selected = [
            old.original_native().unwrap().groups[0].admitted(),
            new.original_native().unwrap().groups[0].admitted(),
        ];
        let mut validation = PackageInterfaceValidation::default();
        let sources =
            available_original_source_map(&products, &selected, &validation.inventory).unwrap();
        let binder = testing::identity("Shared", "entry_3");
        for product in &products {
            assert!(sources
                .get(&(product.owner().clone(), 3, binder.clone()))
                .is_some());
        }
        assert_eq!(certified_source_map(&selected).unwrap().groups.len(), 2);
        let receipt = |version, ordinal, binder| ReceiptImportOwner::Source {
            unit: "fixture".into(),
            module: "Shared".into(),
            module_version: version,
            original_ordinal: ordinal,
            binder,
        };
        let old_selection = CertifiedSourceSelection::from_projected_originals(
            std::slice::from_ref(&old),
            &validation.inventory,
        )
        .unwrap();
        assert_eq!(
            resolve_receipt_owner_with_validation(
                receipt(Some(old.owner().module_version.clone()), 3, binder.clone()),
                &sources,
                Some(&old_selection),
                &BTreeMap::new(),
                &mut validation
            )
            .unwrap(),
            PendingImportOwner::Source {
                owner: old.owner().clone(),
                original_ordinal: 3,
                binder: binder.clone()
            }
        );
        let mut current_selection = CertifiedSourceSelection::default();
        current_selection
            .admit_current(
                new.owner(),
                ProductOrigin::Fresh,
                new.module_interface().unwrap(),
                &validation.inventory,
            )
            .unwrap();
        for version in [None, Some(new.owner().module_version.clone())] {
            assert_eq!(
                resolve_receipt_owner_with_validation(
                    receipt(version, 3, binder.clone()),
                    &sources,
                    Some(&current_selection),
                    &BTreeMap::new(),
                    &mut validation
                )
                .unwrap(),
                PendingImportOwner::Source {
                    owner: new.owner().clone(),
                    original_ordinal: 3,
                    binder: binder.clone()
                }
            );
        }
        assert!(matches!(
            resolve_receipt_owner_with_validation(
                receipt(None, 3, binder.clone()),
                &sources,
                None,
                &BTreeMap::new(),
                &mut validation
            ),
            Err(CertificationError::Mismatch(
                "unversioned source lacks compiler selection"
            ))
        ));
        assert!(matches!(
            resolve_receipt_owner_with_validation(
                receipt(Some(old.owner().module_version.clone()), 3, binder.clone()),
                &sources,
                Some(&current_selection),
                &BTreeMap::new(),
                &mut validation
            ),
            Err(CertificationError::Mismatch("source module version"))
        ));
        for (ordinal, wrong_binder) in [
            (4, binder.clone()),
            (3, testing::identity("Shared", "other")),
        ] {
            assert!(matches!(
                resolve_receipt_owner_with_validation(
                    receipt(None, ordinal, wrong_binder),
                    &sources,
                    Some(&current_selection),
                    &BTreeMap::new(),
                    &mut validation
                ),
                Err(CertificationError::Mismatch("source binder/group closure"))
            ));
        }
        let only_new =
            available_original_source_map(std::slice::from_ref(&new), &[], &validation.inventory)
                .unwrap();
        assert!(matches!(
            resolve_receipt_owner_with_validation(
                receipt(Some(old.owner().module_version.clone()), 3, binder.clone()),
                &only_new,
                Some(&old_selection),
                &BTreeMap::new(),
                &mut validation
            ),
            Err(CertificationError::Mismatch("source binder/group closure"))
        ));
        assert!(matches!(
            CertifiedSourceSelection::from_projected_originals(&products, &validation.inventory),
            Err(CertificationError::Mismatch(
                "ambiguous compiler original owner"
            ))
        ));
        assert!(matches!(
            old_selection.clone().admit_current(
                new.owner(),
                ProductOrigin::Fresh,
                new.module_interface().unwrap(),
                &validation.inventory
            ),
            Err(CertificationError::Mismatch(
                "compiler original owner replaced"
            ))
        ));

        // A version alone cannot choose between contradictory original byte seals.
        let conflicting =
            full_native_fixture("Shared", vec![(3, vec![captured(2)]), (11, vec![])], 7);
        assert_ne!(old.owner(), conflicting.owner());
        let conflict_sources =
            available_original_source_map(&[old.clone(), conflicting], &[], &validation.inventory)
                .unwrap();
        assert!(matches!(
            resolve_receipt_owner_with_validation(
                receipt(Some(old.owner().module_version.clone()), 3, binder.clone()),
                &conflict_sources,
                None,
                &BTreeMap::new(),
                &mut validation
            ),
            Err(CertificationError::Mismatch(
                "ambiguous source original identity"
            ))
        ));
        assert!(resolve_receipt_owner_with_validation(
            receipt(Some(old.owner().module_version.clone()), 3, binder),
            &conflict_sources,
            Some(&old_selection),
            &BTreeMap::new(),
            &mut validation
        )
        .is_ok());
    }

    #[test]
    fn cached_reoffer_keeps_original_source_domain_and_byte_anchors() {
        let retained = |generation| PendingImportOwner::Retained {
            identity: testing::identity("Val.G1", "captured"),
            generation,
        };
        let old_helper = full_native_fixture("Helper", vec![(3, vec![retained(1)])], 7);
        let new_helper = full_native_fixture("Helper", vec![(3, vec![retained(2)])], 9);
        let old_edge = PendingImportOwner::Source {
            owner: old_helper.owner().clone(),
            original_ordinal: 3,
            binder: testing::identity("Helper", "entry_3"),
        };
        let original = full_native_fixture("Consumer", vec![(11, vec![old_edge.clone()])], 7);
        let products = [original.clone(), old_helper.clone(), new_helper.clone()];
        let mut validation = PackageInterfaceValidation::default();
        let sources = available_original_source_map(&products, &[], &validation.inventory).unwrap();
        let mut current = CertifiedSourceSelection::default();
        current
            .admit_current(
                new_helper.owner(),
                ProductOrigin::Fresh,
                new_helper.module_interface().unwrap(),
                &validation.inventory,
            )
            .unwrap();
        let old_receipt = ReceiptImportOwner::Source {
            unit: old_helper.owner().unit.clone(),
            module: old_helper.owner().module.clone(),
            module_version: Some(old_helper.owner().module_version.clone()),
            original_ordinal: 3,
            binder: testing::identity("Helper", "entry_3"),
        };
        let native_group = &original.original_native().unwrap().groups[0];
        let admitted = validate_original_group_receipts(
            native_group.group(),
            &[old_receipt.clone()],
            native_group,
            &sources,
            &BTreeMap::new(),
            &mut validation,
        )
        .unwrap();
        assert_eq!(admitted.imports(), &[old_edge]);
        let current_edge = resolve_receipt_owner_with_validation(
            ReceiptImportOwner::Source {
                unit: new_helper.owner().unit.clone(),
                module: new_helper.owner().module.clone(),
                module_version: None,
                original_ordinal: 3,
                binder: testing::identity("Helper", "entry_3"),
            },
            &sources,
            Some(&current),
            &BTreeMap::new(),
            &mut validation,
        )
        .unwrap();
        assert!(
            matches!(current_edge, PendingImportOwner::Source { owner, .. } if owner == *new_helper.owner())
        );
        let mut wrong = old_receipt.clone();
        if let ReceiptImportOwner::Source { module_version, .. } = &mut wrong {
            *module_version = Some(new_helper.owner().module_version.clone());
        }
        assert!(matches!(
            validate_original_group_receipts(
                native_group.group(),
                &[wrong],
                native_group,
                &sources,
                &BTreeMap::new(),
                &mut validation
            ),
            Err(CertificationError::Mismatch("shared home import ownership"))
        ));
        let missing = available_original_source_map(
            &[original.clone(), new_helper],
            &[],
            &validation.inventory,
        )
        .unwrap();
        assert!(matches!(
            validate_original_group_receipts(
                native_group.group(),
                &[old_receipt.clone()],
                native_group,
                &missing,
                &BTreeMap::new(),
                &mut validation
            ),
            Err(CertificationError::Mismatch("source binder/group closure"))
        ));
        let changed_body = full_native_fixture("Consumer", vec![(11, vec![])], 7);
        assert!(matches!(
            validate_original_group_receipts(
                changed_body.original_native().unwrap().groups[0].group(),
                &[old_receipt],
                native_group,
                &sources,
                &BTreeMap::new(),
                &mut validation
            ),
            Err(CertificationError::Mismatch("shared original home groups"))
        ));
        let canonical = original.module_interface().unwrap();
        assert!(matches_cached_original_reoffer(
            &original,
            original.owner(),
            original.product_bytes(),
            original.package_imports_bytes(),
            canonical
        ));
        let mut changed_bytes = original.product_bytes().to_vec();
        changed_bytes[0] ^= 1;
        assert!(!matches_cached_original_reoffer(
            &original,
            original.owner(),
            &changed_bytes,
            original.package_imports_bytes(),
            canonical
        ));
        assert!(!matches_cached_original_reoffer(
            &original,
            original.owner(),
            original.product_bytes(),
            b"different package witness",
            canonical
        ));
        let different_canonical = fixture_finalized_product(
            original_groups_fixture(
                "Consumer",
                vec![(11, vec![retained(3)])],
                7,
                &BTreeMap::new(),
            ),
            [2; 32],
        );
        assert!(!matches_cached_original_reoffer(
            &original,
            original.owner(),
            original.product_bytes(),
            original.package_imports_bytes(),
            different_canonical.module_interface().unwrap()
        ));
        let mut selected = vec![admitted.clone()];
        let inherited = native_group.admitted();
        append_original_selection(
            &mut selected,
            std::slice::from_ref(&inherited),
            &validation.inventory,
            &ValidatedPromotionSharing::default(),
        )
        .unwrap();
        assert_eq!(selected, vec![inherited.clone()]);
        assert!(Arc::ptr_eq(&selected[0].group, &inherited.group));
        assert!(Arc::ptr_eq(&selected[0].imports, &inherited.imports));
        let before = original.original_byte_anchors();
        let retained_original = original.clone();
        assert!(before
            .iter()
            .zip(retained_original.original_byte_anchors())
            .all(|(old, retained)| Arc::ptr_eq(old, retained)));
        assert!(matches!(
            append_original_selection(
                &mut vec![admitted.clone(), admitted.clone()],
                &[],
                &validation.inventory,
                &ValidatedPromotionSharing::default(),
            ),
            Err(CertificationError::Mismatch(
                "duplicate current original ordinal"
            ))
        ));
        let mut unproved_promotion = admitted.clone();
        unproved_promotion.origin = ProductOrigin::RetainedCore;
        assert!(matches!(append_original_selection(
            &mut vec![unproved_promotion], std::slice::from_ref(&inherited),
            &validation.inventory, &ValidatedPromotionSharing::default(),
        ), Err(CertificationError::OriginalGroupConflict(conflict))
            if conflict.owner == *original.owner()
                && conflict.failure == (OriginalGroupFailure::SelectionOverlap {
                    ordinal: 11,
                    current_origin: ProductOrigin::RetainedCore,
                    inherited_origin: ProductOrigin::Cached,
                    body_matches: true,
                    imports_match: true,
                })
        ));
        let mut contradictory = inherited.clone();
        contradictory.imports = vec![retained(99)].into();
        assert!(matches!(
            append_original_selection(&mut vec![admitted], &[contradictory], &validation.inventory,
                &ValidatedPromotionSharing::default()),
            Err(CertificationError::OriginalGroupConflict(conflict))
                if conflict.owner == *original.owner()
                    && conflict.failure == (OriginalGroupFailure::SelectionOverlap {
                        ordinal: 11,
                        current_origin: ProductOrigin::Cached,
                        inherited_origin: ProductOrigin::Cached,
                        body_matches: true,
                        imports_match: false,
                    })
        ));
    }

    #[test]
    fn retained_promotion_rekeys_only_its_exact_source_owner() {
        let original = full_native_fixture("Promoted", vec![(3, vec![])], 7);
        let old = original.owner().clone();
        let mut staged = old.clone();
        staged.module_version = ModuleVersion([0; 32]);
        staged.product_sha256 = [9; 32];
        let binder = testing::identity("Promoted", "entry");
        let mut sources = SourceGroupMap::new();
        sources.insert(
            (old.clone(), 3, binder.clone()),
            (old.clone(), ProductOrigin::RetainedCore),
        );
        sources.insert(
            (staged.clone(), 3, binder.clone()),
            (staged.clone(), ProductOrigin::RetainedCore),
        );
        let operation = InventoryOperation::new(Default::default());
        let mut selection = CertifiedSourceSelection::default();
        selection
            .admit_current(
                &staged,
                ProductOrigin::RetainedCore,
                original.module_interface().unwrap(),
                &operation,
            )
            .unwrap();
        let versions = BTreeMap::from([(
            (staged.unit.clone(), staged.module.clone()),
            ModuleVersion([3; 32]),
        )]);
        selection.promote(&versions).unwrap();
        sources
            .promote(
                &BTreeMap::from([(staged.clone(), ModuleVersion([3; 32]))]),
                &ValidatedPromotionSharing::default(),
            )
            .unwrap();
        let mut final_owner = staged.clone();
        final_owner.module_version = ModuleVersion([3; 32]);
        assert!(sources.get(&(staged, 3, binder.clone())).is_none());
        assert!(sources.get(&(old, 3, binder.clone())).is_some());
        assert_eq!(
            selection.selected_original_owners().collect::<Vec<_>>(),
            vec![&final_owner]
        );
        assert_eq!(
            resolve_receipt_owner_with_validation(
                ReceiptImportOwner::Source {
                    unit: final_owner.unit.clone(),
                    module: final_owner.module.clone(),
                    module_version: None,
                    original_ordinal: 3,
                    binder: binder.clone(),
                },
                &sources,
                Some(&selection),
                &BTreeMap::new(),
                &mut PackageInterfaceValidation::default()
            )
            .unwrap(),
            PendingImportOwner::Source {
                owner: final_owner,
                original_ordinal: 3,
                binder
            }
        );
    }

    #[test]
    fn retained_promotion_conflict_reports_exact_identity_and_provenance() {
        let original = inherited_owner("Promoted");
        let mut staged = original.clone();
        staged.module_version = ModuleVersion([0; 32]);
        let binder = testing::identity("Promoted", "entry");
        let mut sources = SourceGroupMap::new();
        sources.insert(
            (original.clone(), 3, binder.clone()),
            (original.clone(), ProductOrigin::Cached),
        );
        sources.insert(
            (staged.clone(), 3, binder.clone()),
            (staged.clone(), ProductOrigin::RetainedCore),
        );
        let failure = sources.promote(
            &BTreeMap::from([(staged, original.module_version.clone())]),
            &ValidatedPromotionSharing::default(),
        );
        let Err(CertificationError::DuplicateSourceBinder(conflict)) = failure else {
            panic!("convergent promotion must retain its exact conflict");
        };
        assert_eq!(conflict.phase, SourceBinderPhase::NativePromotion);
        assert_eq!(conflict.owner, original);
        assert_eq!(conflict.original_ordinal, 3);
        assert_eq!(conflict.binder, binder);
        assert!(matches!(
            (conflict.existing_origin, conflict.incoming_origin),
            (ProductOrigin::Cached, ProductOrigin::RetainedCore)
                | (ProductOrigin::RetainedCore, ProductOrigin::Cached)
        ));
    }

    #[test]
    fn ordinary_target_selects_old_full_carrier_transitively_without_new_native() {
        use crate::artifact_inventory::{
            ArtifactEntry, ArtifactInventory, NativeArtifactDemand, NativeGroupKey,
        };
        let dependency = full_native_fixture("Dependency", vec![(9, vec![]), (12, vec![])], 7);
        let dependency_import = PendingImportOwner::Source {
            owner: dependency.owner().clone(),
            original_ordinal: 9,
            binder: testing::identity("Dependency", "entry_9"),
        };
        for unrelated in [0, 1, 7, 17] {
            let mut outlines = vec![(8, vec![dependency_import.clone()])];
            outlines.extend((0..unrelated).map(|index| (20 + index, vec![])));
            let original = full_native_fixture("Original", outlines, 7);
            let products = vec![original.clone(), dependency.clone()];
            let mut validation = PackageInterfaceValidation::default();
            let entries = products
                .iter()
                .cloned()
                .map(|product| {
                    Arc::new(
                        ArtifactEntry::original_with_validation([1; 32], product, &mut validation)
                            .unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            let inventory = ArtifactInventory::default();
            let baseline = inventory
                .admit_recovery_selection(
                    &inventory.empty_view(),
                    entries.clone(),
                    &BTreeSet::new(),
                )
                .unwrap();
            assert!(baseline.selected_native_groups().is_empty());
            let mut wire = testing::wire_program();
            let binder = testing::identity("Original", "entry_8");
            wire.globals = vec![GlobalDecl {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            }];
            let target = testing::prepare(wire).unwrap();
            let accepted = vec![AcceptedGlobal {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                owner: ReceiptImportOwner::Source {
                    unit: original.owner().unit.clone(),
                    module: original.owner().module.clone(),
                    module_version: Some(original.owner().module_version.clone()),
                    original_ordinal: 8,
                    binder,
                },
            }];
            assert!(certify_target_owners(&target, &accepted, &[], &BTreeMap::new()).is_err());
            let imports = certify_target_available_owners_with_validation(
                &target,
                &accepted,
                &products,
                &[],
                &CertifiedSourceSelection::from_projected_originals(
                    &products,
                    &validation.inventory,
                )
                .unwrap(),
                &BTreeMap::new(),
                &mut validation,
            )
            .unwrap();
            let selected = inventory
                .admit_shared_with_demand(
                    &baseline,
                    entries.clone(),
                    NativeArtifactDemand::CertifiedTargetImports(&imports),
                )
                .unwrap();
            let selected_keys = BTreeSet::from([
                NativeGroupKey {
                    artifact: entries[0].descriptor.id,
                    original_ordinal: 8,
                },
                NativeGroupKey {
                    artifact: entries[1].descriptor.id,
                    original_ordinal: 9,
                },
            ]);
            assert_eq!(selected.selected_native_groups(), selected_keys);
            assert_eq!(selected.artifact_ids(), baseline.artifact_ids());
            let groups = crate::declaration_context::certify_artifact_view_groups_with_validation(
                &selected,
                &[],
                &[],
                &mut validation,
            )
            .unwrap();
            assert_eq!(groups.len(), 2);
            certify_target_owners(&target, &accepted, &groups, &BTreeMap::new()).unwrap();
            let repeated = inventory
                .admit_shared_with_demand(
                    &selected,
                    entries.clone(),
                    NativeArtifactDemand::CertifiedTargetImports(&imports),
                )
                .unwrap();
            assert_eq!(repeated.selected_native_groups(), selected_keys);
            crate::declaration_context::certify_artifact_view_groups_with_validation(
                &repeated,
                &[],
                &groups,
                &mut validation,
            )
            .unwrap();
            assert!(
                crate::declaration_context::certify_artifact_view_groups_with_validation(
                    &baseline,
                    &groups,
                    &groups,
                    &mut validation
                )
                .is_err()
            );
            let mut stale = accepted.clone();
            let ReceiptImportOwner::Source {
                original_ordinal, ..
            } = &mut stale[0].owner
            else {
                unreachable!()
            };
            *original_ordinal = 100;
            assert!(certify_target_available_owners_with_validation(
                &target,
                &stale,
                &products,
                &[],
                &CertifiedSourceSelection::from_projected_originals(
                    &products,
                    &validation.inventory
                )
                .unwrap(),
                &BTreeMap::new(),
                &mut validation
            )
            .is_err());
            let missing = vec![entries[0].clone()];
            let partial_inventory = ArtifactInventory::default();
            let partial = partial_inventory
                .admit_recovery_selection(
                    &partial_inventory.empty_view(),
                    missing.clone(),
                    &BTreeSet::new(),
                )
                .unwrap();
            assert!(partial_inventory
                .admit_shared_with_demand(
                    &partial,
                    missing,
                    NativeArtifactDemand::CertifiedTargetImports(&imports)
                )
                .is_err());
        }
    }

    #[test]
    fn selected_fresh_candidates_preserve_origin_and_full_native_bytes() {
        use crate::artifact_inventory::{ArtifactEntry, ArtifactInventory, NativeGroupKey};
        let original = full_native_fixture("FreshSubset", vec![(8, vec![]), (11, vec![])], 7);
        let mut candidates = original
            .original_native()
            .unwrap()
            .groups
            .iter()
            .map(AuthenticatedOriginalGroup::admitted)
            .collect::<Vec<_>>();
        for group in &mut candidates {
            group.origin = ProductOrigin::Fresh;
        }
        let mut validation = PackageInterfaceValidation::default();
        let entry = Arc::new(
            ArtifactEntry::original_with_validation([1; 32], original.clone(), &mut validation)
                .unwrap(),
        );
        let inventory = ArtifactInventory::default();
        let selected = inventory
            .admit_recovery_selection(
                &inventory.empty_view(),
                vec![entry.clone()],
                &BTreeSet::from([NativeGroupKey {
                    artifact: entry.descriptor.id,
                    original_ordinal: 8,
                }]),
            )
            .unwrap();
        let groups = crate::declaration_context::certify_artifact_view_groups_with_validation(
            &selected,
            &candidates,
            &[],
            &mut validation,
        )
        .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].origin(), ProductOrigin::Fresh);
        assert_eq!(groups[0].group().original_ordinal(), 8);
        assert_eq!(original.original_native().unwrap().groups.len(), 2);
        let selected_entries = selected.entries();
        let retained_entry = selected_entries
            .iter()
            .find(|candidate| candidate.descriptor.id == entry.descriptor.id)
            .unwrap();
        let crate::artifact_inventory::ArtifactPayload::Original(retained) =
            &retained_entry.payload
        else {
            unreachable!()
        };
        assert_eq!(retained.owner(), original.owner());
        assert_eq!(retained.product_bytes(), original.product_bytes());
        assert_eq!(
            retained.certification_bytes(),
            original.certification_bytes()
        );
    }

    #[test]
    fn retained_core_selection_requires_exact_certified_original() {
        let packages = BTreeMap::new();
        let fixture = fixture_finalized_product(
            original_witness_fixture("Original", None, 7, &packages),
            [1; 32],
        );
        let original = recovered_witness_fixtures(std::slice::from_ref(&fixture))
            .remove(0)
            .product;
        let packet = CertifiedRetainedCoreProducts {
            products: BTreeMap::from([(
                (
                    original.owner().unit.clone(),
                    original.owner().module.clone(),
                ),
                original.clone(),
            )]),
        };
        assert!(packet.contains_original(&original.clone()));
        assert!(!CertifiedRetainedCoreProducts::default().contains_original(&original));
        let recaptured = recovered_witness_fixtures(&[fixture]).remove(0).product;
        assert_eq!(recaptured.owner(), original.owner());
        assert_eq!(recaptured.product_bytes(), original.product_bytes());
        assert!(!packet.contains_original(&recaptured));
    }

    #[test]
    fn original_native_witness_reuses_nonempty_cycle_and_checks_selected_closure() {
        let source = |owner: CachedHomeOwner| PendingImportOwner::Source {
            binder: testing::identity(&owner.module, "entry"),
            owner,
            original_ordinal: 7,
        };
        let packages = BTreeMap::new();
        let a = original_witness_fixture("A", Some(source(inherited_owner("B"))), 7, &packages);
        let b = original_witness_fixture("B", Some(source(a.owner().clone())), 7, &packages);
        let a = original_witness_fixture("A", Some(source(b.owner().clone())), 7, &packages);
        let before = ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get);
        let recovered = recovered_witness_fixtures(&[a, b]);
        assert_eq!(
            ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get) - before,
            2
        );
        let products = recovered
            .iter()
            .map(|original| &original.product)
            .collect::<Vec<_>>();
        let expected = products
            .iter()
            .map(|product| {
                (
                    certified_home_requirements(product.certification_bytes(), product.owner())
                        .unwrap(),
                    certified_native_requirements(product.certification_bytes(), product.owner())
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let certificate_decodes = HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get);
        for (product, (sources, native)) in products.iter().zip(&expected) {
            assert_eq!(
                &original_home_requirements_with_validation(
                    product,
                    &mut PackageInterfaceValidation::default()
                )
                .unwrap(),
                sources
            );
            assert_eq!(&original_native_requirements(product).unwrap(), native);
            let entry =
                crate::artifact_inventory::ArtifactEntry::original([1; 32], (*product).clone())
                    .unwrap();
            assert_eq!(entry.native_requirements, native.artifact_edges);
            assert_eq!(entry.retained_packages, native.retained_packages);
        }
        let groups = certify_owned_products_with_validation(
            &products,
            &[],
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        assert_eq!(groups.len(), 2);
        assert!(groups
            .iter()
            .all(|group| group.origin() == ProductOrigin::Cached));
        assert!(Arc::ptr_eq(
            &groups[0].group,
            &products[0].original_native().unwrap().groups[0].group
        ));
        assert!(certify_owned_products_with_validation(
            &products,
            &groups,
            &mut PackageInterfaceValidation::default()
        )
        .unwrap()
        .is_empty());
        assert!(certify_owned_products_with_validation(
            &[products[0]],
            &[],
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
        assert!(certify_owned_products_with_validation(
            &[products[0], products[0]],
            &[],
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
        assert_eq!(
            ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get) - before,
            2,
            "reusing witnesses cannot decode native products again"
        );
        assert_eq!(
            HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get),
            certificate_decodes,
            "witness reuse and inventory admission cannot decode certificates again"
        );
        let duplicate = vec![groups[0].clone(), groups[0].clone()];
        assert!(matches!(
            certify_owned_products_with_validation(
                &[products[0]],
                &duplicate,
                &mut PackageInterfaceValidation::default()
            ),
            Err(CertificationError::Mismatch(
                "duplicate current original ordinal"
            ))
        ));
        let mut changed = groups.clone();
        changed[0].imports = vec![PendingImportOwner::Retained {
            identity: testing::identity("Value", "changed"),
            generation: 99,
        }]
        .into();
        assert!(matches!(
            certify_owned_products_with_validation(
                &products,
                &changed,
                &mut PackageInterfaceValidation::default()
            ),
            Err(CertificationError::Mismatch("shared original home groups"))
        ));
        println!("native witness original_decodes=2 reuse_decodes=0 nonempty_groups=2");
    }

    #[test]
    fn recovered_native_witness_preserves_distinct_original_versions() {
        let retained = |generation| PendingImportOwner::Retained {
            identity: testing::identity("Value", "live"),
            generation,
        };
        let old = original_witness_fixture("A", Some(retained(11)), 7, &BTreeMap::new());
        let new = original_witness_fixture("A", Some(retained(22)), 8, &BTreeMap::new());
        let recovered = recovered_witness_fixtures(&[old, new]);
        for (index, original) in recovered.iter().enumerate() {
            let groups = certify_owned_products_with_validation(
                &[&original.product],
                &[],
                &mut PackageInterfaceValidation::default(),
            )
            .unwrap();
            assert!(
                matches!(groups[0].imports()[0], PendingImportOwner::Retained { generation, .. } if generation == [11, 22][index])
            );
        }
        let products = recovered
            .iter()
            .map(|original| &original.product)
            .collect::<Vec<_>>();
        assert!(
            certify_owned_products_with_validation(
                &products,
                &[],
                &mut PackageInterfaceValidation::default()
            )
            .is_err(),
            "one selected context cannot combine different full versions of one owner"
        );
    }

    #[test]
    fn inherited_packages_reuse_issued_witness_and_charge_owned_selection() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("External.hi");
        std::fs::write(&path, [0x43]).unwrap();
        let packages = BTreeMap::from([(
            ("external-package".into(), "External".into()),
            PackageInterfaceWitness {
                selected_path: path,
                sha256: sha(&[0x43]),
            },
        )]);
        let fixture = fixture_finalized_product(
            original_witness_fixture(
                "Consumer",
                Some(PendingImportOwner::Package {
                    unit: "external-package".into(),
                    module: "External".into(),
                    binder: SymbolIdentity {
                        unit: "external-package".into(),
                        ..testing::identity("External", "entry")
                    },
                    interface_digest: sha(&[0x43]),
                }),
                7,
                &packages,
            ),
            [1; 32],
        );
        let issued = recovered_witness_fixtures(&[fixture]).remove(0).product;
        assert_eq!(
            issued.module_interface().unwrap().producer_sha256(),
            [1; 32]
        );
        assert!(issued.original_native().unwrap().matches_original(&issued));
        let legacy = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            issued.owner().clone(),
            issued.interface_bytes().to_vec(),
            issued.product_bytes().to_vec(),
            issued.package_imports_bytes().to_vec(),
            issued.certification_bytes().to_vec(),
        );
        assert!(legacy.original_native().is_none());
        assert!(
            legacy
                .clone()
                .with_original_native(issued.original_native().unwrap().clone())
                .is_err(),
            "identical recaptured bytes cannot borrow another original's witness"
        );
        let mut legacy_validation = PackageInterfaceValidation::default();
        let before = HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get);
        let expected = inherited_package_witnesses_with_validation(
            std::slice::from_ref(&legacy),
            &mut legacy_validation,
        )
        .unwrap();
        assert_eq!(
            HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get),
            before + 1
        );
        assert_eq!(expected, packages);
        let legacy_spent = legacy_validation.inventory.work_usage().unwrap().0;
        let mut validation = PackageInterfaceValidation::default();
        let inventory = validation.inventory.clone();
        let before = HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get);
        let selected = inherited_package_witnesses_with_validation(
            std::slice::from_ref(&issued),
            &mut validation,
        )
        .unwrap();
        assert_eq!(selected, expected);
        assert_eq!(
            HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get),
            before
        );
        let spent = inventory.work_usage().unwrap().0;
        assert!(spent > 0 && spent < legacy_spent);
        let repeated = inherited_package_witnesses_with_validation(
            &[issued.clone(), issued.clone()],
            &mut validation,
        )
        .unwrap();
        assert_eq!(repeated, expected);
        assert_eq!(
            HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get),
            before
        );
        assert!(
            inventory.work_usage().unwrap().0 > spent,
            "owned output copies stay charged"
        );
        assert_ne!(
            selected
                .values()
                .next()
                .unwrap()
                .selected_path
                .as_os_str()
                .as_encoded_bytes()
                .as_ptr(),
            repeated
                .values()
                .next()
                .unwrap()
                .selected_path
                .as_os_str()
                .as_encoded_bytes()
                .as_ptr()
        );
        let remaining = inventory.work_usage().unwrap().1;
        inventory.charge(remaining).unwrap();
        assert!(matches!(
            inherited_package_witnesses_with_validation(
                std::slice::from_ref(&issued),
                &mut validation,
            ),
            Err(CertificationError::Product(
                tidepool_repr::execution_schema::ParseError::LimitExceeded("work")
            ))
        ));
        assert_eq!(inventory.work_usage().unwrap().1, 0);
        assert_eq!(
            inherited_package_witnesses_with_validation(
                &[issued],
                &mut PackageInterfaceValidation::default(),
            )
            .unwrap(),
            expected
        );
        let mut wrong_owner = legacy.owner().clone();
        wrong_owner.module = "AnotherConsumer".into();
        let wrong_owner = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            wrong_owner,
            legacy.interface_bytes().to_vec(),
            legacy.product_bytes().to_vec(),
            legacy.package_imports_bytes().to_vec(),
            legacy.certification_bytes().to_vec(),
        );
        assert!(matches!(
            inherited_package_witnesses_with_validation(
                &[wrong_owner],
                &mut PackageInterfaceValidation::default(),
            ),
            Err(CertificationError::Mismatch(
                "inherited package product owner"
            ))
        ));
        let mut changed = legacy.certification_bytes().to_vec();
        changed[0] ^= 1;
        let changed = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            legacy.owner().clone(),
            legacy.interface_bytes().to_vec(),
            legacy.product_bytes().to_vec(),
            legacy.package_imports_bytes().to_vec(),
            changed,
        );
        assert!(inherited_package_witnesses_with_validation(
            &[changed],
            &mut PackageInterfaceValidation::default(),
        )
        .is_err());
    }

    #[test]
    fn inherited_package_witness_reuse_preserves_current_files_and_conflicts() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("External.hi");
        let second = root.path().join("AnotherExternal.hi");
        for path in [&first, &second] {
            std::fs::write(path, [0x43]).unwrap();
        }
        let package = |path| {
            BTreeMap::from([(
                ("external-package".into(), "External".into()),
                PackageInterfaceWitness {
                    selected_path: path,
                    sha256: sha(&[0x43]),
                },
            )])
        };
        let fixture = |module, packages: &BTreeMap<_, _>| {
            fixture_finalized_product(
                original_groups_fixture_with_interface(
                    module,
                    vec![(
                        7,
                        vec![PendingImportOwner::Package {
                            unit: "external-package".into(),
                            module: "External".into(),
                            binder: SymbolIdentity {
                                unit: "external-package".into(),
                                ..testing::identity("External", "entry")
                            },
                            interface_digest: sha(&[0x43]),
                        }],
                    )],
                    7,
                    packages,
                    format!("{module} interface").into_bytes(),
                ),
                [1; 32],
            )
        };
        let packages = package(first.clone());
        let originals = recovered_witness_fixtures(&[
            fixture("First", &packages),
            fixture("Second", &package(second)),
        ]);
        let first_product = originals[0].product.clone();
        let second_product = originals[1].product.clone();
        assert_ne!(
            first_product.interface_bytes(),
            second_product.interface_bytes()
        );
        let legacy = |product: &crate::recovery_artifacts::CertifiedRecoveryProduct| {
            crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
                product.owner().clone(),
                product.interface_bytes().to_vec(),
                product.product_bytes().to_vec(),
                product.package_imports_bytes().to_vec(),
                product.certification_bytes().to_vec(),
            )
        };
        for products in [
            vec![first_product.clone(), second_product],
            vec![legacy(&first_product), legacy(&originals[1].product)],
        ] {
            assert!(matches!(
                inherited_package_witnesses_with_validation(
                    &products,
                    &mut PackageInterfaceValidation::default(),
                ),
                Err(CertificationError::Mismatch("inherited package selection"))
            ));
        }
        for product in [first_product, legacy(&originals[0].product)] {
            assert_eq!(
                inherited_package_witnesses_with_validation(
                    std::slice::from_ref(&product),
                    &mut PackageInterfaceValidation::default(),
                )
                .unwrap(),
                packages
            );
            std::fs::write(&first, [0x44]).unwrap();
            assert!(matches!(
                inherited_package_witnesses_with_validation(
                    std::slice::from_ref(&product),
                    &mut PackageInterfaceValidation::default(),
                ),
                Err(CertificationError::StaleEvidence)
            ));
            std::fs::write(&first, [0x43]).unwrap();
            assert_eq!(
                inherited_package_witnesses_with_validation(
                    &[product],
                    &mut PackageInterfaceValidation::default(),
                )
                .unwrap(),
                packages
            );
        }
    }

    #[test]
    fn original_native_witness_checks_package_drift_and_zero_group_downgrade() {
        let root = tempfile::tempdir().unwrap();
        let package_path = root.path().join("External.hi");
        std::fs::write(&package_path, [0x43]).unwrap();
        let packages = BTreeMap::from([(
            ("fixture".into(), "External".into()),
            PackageInterfaceWitness {
                selected_path: package_path.clone(),
                sha256: sha(&[0x43]),
            },
        )]);
        let import = PendingImportOwner::Package {
            unit: "fixture".into(),
            module: "External".into(),
            binder: testing::identity("External", "entry"),
            interface_digest: sha(&[0x43]),
        };
        let consumer = original_witness_fixture("Consumer", Some(import), 7, &packages);
        let empty = original_witness_fixture("External", None, 7, &BTreeMap::new());
        let recovered = recovered_witness_fixtures(&[consumer, empty]);
        let consumer = &recovered[0].product;
        let empty = &recovered[1].product;
        assert_eq!(
            certify_owned_products_with_validation(
                &[consumer],
                &[],
                &mut PackageInterfaceValidation::default()
            )
            .unwrap()
            .len(),
            1
        );
        assert!(matches!(
            certify_owned_products_in_context_with_validation(
                &[consumer],
                &[],
                &[consumer, empty],
                &mut PackageInterfaceValidation::default()
            ),
            Err(CertificationError::Mismatch(
                "home owner downgraded to package"
            ))
        ));
        let current = certify_owned_products_with_validation(
            &[consumer],
            &[],
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        let legacy_empty = original_witness_fixture("External", None, 7, &BTreeMap::new());
        assert!(matches!(
            certify_owned_products_in_context_with_validation(
                &[empty],
                &current,
                &[consumer, empty],
                &mut PackageInterfaceValidation::default(),
            ),
            Err(CertificationError::Mismatch(
                "home owner downgraded to package"
            ))
        ));
        assert!(matches!(
            certify_owned_products_in_context_with_validation(
                &[&legacy_empty],
                &current,
                &[consumer, &legacy_empty],
                &mut PackageInterfaceValidation::default(),
            ),
            Err(CertificationError::Mismatch(
                "home owner downgraded to package"
            ))
        ));
        std::fs::write(&package_path, [0x44]).unwrap();
        assert!(matches!(
            original_home_requirements_with_validation(
                consumer,
                &mut PackageInterfaceValidation::default(),
            ),
            Err(CertificationError::StaleEvidence)
        ));
        assert!(matches!(
            certify_owned_products_with_validation(
                &[consumer],
                &[],
                &mut PackageInterfaceValidation::default()
            ),
            Err(CertificationError::StaleEvidence)
        ));
    }

    #[test]
    fn inherited_original_cycle_requires_full_exact_source_closure() {
        let a = inherited_owner("A");
        let b = inherited_owner("B");
        let ga = inherited_group(
            &a,
            PendingImportOwner::Source {
                owner: b.clone(),
                original_ordinal: 7,
                binder: testing::identity("B", "entry"),
            },
        );
        let gb = inherited_group(
            &b,
            PendingImportOwner::Source {
                owner: a,
                original_ordinal: 7,
                binder: testing::identity("A", "entry"),
            },
        );
        let packages = BTreeMap::new();
        assert_eq!(
            certify_inherited_inventory(
                vec![
                    inherited_parsed(&ga, &packages),
                    inherited_parsed(&gb, &packages)
                ],
                std::slice::from_ref(&ga)
            )
            .unwrap(),
            vec![gb.clone()]
        );
        let mut conflicting_current = ga.clone();
        if let PendingImportOwner::Source { owner, .. } =
            &mut Arc::make_mut(&mut conflicting_current.imports)[0]
        {
            owner.skinny_iface_sha256 = [91; 32];
        }
        assert!(matches!(
            certify_inherited_inventory(
                vec![
                    inherited_parsed(&ga, &packages),
                    inherited_parsed(&gb, &packages)
                ],
                &[conflicting_current]
            ),
            Err(CertificationError::Mismatch("shared home import ownership"))
        ));
        assert_eq!(
            certify_inherited_inventory(
                vec![
                    inherited_parsed(&ga, &packages),
                    inherited_parsed(&gb, &packages)
                ],
                &[]
            )
            .unwrap(),
            vec![ga.clone(), gb.clone()]
        );
        assert!(matches!(
            certify_inherited_inventory(vec![inherited_parsed(&ga, &packages)], &[]),
            Err(CertificationError::Mismatch(
                "inherited source owner closure"
            ))
        ));
        // A freshly certified current module can complete the inherited cycle.
        let mut current = gb.clone();
        current.origin = ProductOrigin::Fresh;
        assert_eq!(
            certify_inherited_inventory(vec![inherited_parsed(&ga, &packages)], &[current])
                .unwrap(),
            vec![ga.clone()]
        );
        let (product, mut witness) = inherited_parsed(&ga, &packages);
        witness
            .sources
            .get_mut(&(b.unit.clone(), b.module.clone()))
            .unwrap()
            .product_sha256 = [99; 32];
        assert!(matches!(
            certify_inherited_inventory(
                vec![(product, witness), inherited_parsed(&gb, &packages)],
                &[]
            ),
            Err(CertificationError::Mismatch(
                "inherited source owner closure"
            ))
        ));
        let (product, mut witness) = inherited_parsed(&ga, &packages);
        if let ReceiptImportOwner::Source {
            original_ordinal, ..
        } = &mut witness.groups[0].2[0].owner
        {
            *original_ordinal = 8;
        }
        assert!(matches!(
            certify_inherited_inventory(
                vec![(product, witness), inherited_parsed(&gb, &packages)],
                &[]
            ),
            Err(CertificationError::Mismatch("source binder/group closure"))
        ));
        let (product, mut witness) = inherited_parsed(&ga, &packages);
        witness.groups[0].1[0].occurrence = "rebound".into();
        assert!(matches!(
            certify_inherited_inventory(
                vec![(product, witness), inherited_parsed(&gb, &packages)],
                &[]
            ),
            Err(CertificationError::Mismatch(
                "inherited original group/globals"
            ))
        ));
    }

    #[test]
    fn inherited_owner_seal_preserves_retained_identity_and_refuses_package_downgrade() {
        let a = inherited_owner("A");
        let retained = inherited_group(
            &a,
            PendingImportOwner::Retained {
                identity: testing::identity("Val.G9", "old"),
                generation: 9,
            },
        );
        assert_eq!(
            certify_inherited_inventory(vec![inherited_parsed(&retained, &BTreeMap::new())], &[])
                .unwrap(),
            vec![retained]
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("B.hi");
        std::fs::write(&path, [0x42]).unwrap();
        let b = inherited_owner("B");
        let packages = BTreeMap::from([(
            ("fixture".into(), "B".into()),
            PackageInterfaceWitness {
                selected_path: path.clone(),
                sha256: sha(&[0x42]),
            },
        )]);
        let fake_package = inherited_group(
            &a,
            PendingImportOwner::Package {
                unit: "fixture".into(),
                module: "B".into(),
                binder: testing::identity("B", "entry"),
                interface_digest: sha(&[0x42]),
            },
        );
        let gb = inherited_group(
            &b,
            PendingImportOwner::Retained {
                identity: testing::identity("Val.G9", "old"),
                generation: 9,
            },
        );
        assert!(matches!(
            certify_inherited_inventory(
                vec![
                    inherited_parsed(&fake_package, &packages),
                    inherited_parsed(&gb, &BTreeMap::new())
                ],
                &[]
            ),
            Err(CertificationError::Mismatch(
                "home owner downgraded to package"
            ))
        ));
        let sealed = encode_home_certification(&a, &[fake_package.clone()], &packages).unwrap();
        let mut target = testing::wire_program();
        target.globals = fake_package.group.globals().to_vec();
        let target = testing::prepare(target).unwrap();
        let accepted = decode_home_witness(&sealed).unwrap().groups.remove(0).2;
        assert!(matches!(
            certify_target_owners(&target, &accepted, &[gb], &packages),
            Err(CertificationError::Mismatch(
                "home owner downgraded to package"
            ))
        ));
        std::fs::write(path, [0x43]).unwrap();
        assert!(matches!(
            validate_home_certification(&sealed, &a),
            Err(CertificationError::StaleEvidence)
        ));
    }

    #[test]
    fn inherited_empty_product_requires_canonical_exact_seal_and_bytes() {
        let bytes = sidecar();
        let owner = CachedHomeOwner {
            unit: "main".into(),
            module: "Fresh".into(),
            module_version: ModuleVersion([7; 32]),
            skinny_iface_sha256: sha(&[0x42]),
            product_sha256: sha(&bytes),
        };
        let seal = encode_home_certification(&owner, &[], &BTreeMap::new()).unwrap();
        let source = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let interface = source.path().join("Fresh.hi");
        let product = source.path().join("Fresh.cbor");
        std::fs::write(&interface, [0x42]).unwrap();
        std::fs::write(&product, bytes).unwrap();
        std::fs::write(interface.with_extension("hi.owners"), &seal).unwrap();
        let package_seal = value_array([
            value_text("TPPKGROOTS"),
            value_text("2"),
            value_array([
                value_text("main"),
                value_text("Fresh"),
                value_text(hex(&owner.skinny_iface_sha256)),
            ]),
            value_array([]),
            value_array([]),
        ]);
        let mut packages = Vec::new();
        ciborium::ser::into_writer(&package_seal, &mut packages).unwrap();
        std::fs::write(interface.with_extension("hi.packages"), packages).unwrap();
        let finalized = fixture_finalized_product(
            crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
                owner.clone(),
                vec![0x42],
                std::fs::read(&product).unwrap(),
                std::fs::read(interface.with_extension("hi.packages")).unwrap(),
                seal,
            ),
            [1; 32],
        );
        let seal = finalized.certification_bytes().to_vec();
        std::fs::write(
            interface.with_extension("hi.owners"),
            finalized.certification_bytes(),
        )
        .unwrap();
        let canonical = crate::recovery_artifacts::materialize_module_interface(
            source.path(),
            finalized.module_interface().unwrap(),
            &mut PackageInterfaceValidation::default(),
            crate::recovery_artifacts::MaterializationMode::Durable,
        )
        .unwrap();
        let refs = crate::recovery_artifacts::materialize_recovery_closure(
            run.path(),
            [1; 32],
            &[crate::recovery_artifacts::RecoveryArtifactInput {
                owner: &owner,
                interface_source: &interface,
                module_interface: (source.path(), &canonical),
                product_source: &product,
            }],
        )
        .unwrap();
        let mut artifact =
            crate::recovery_artifacts::verify_materialized_ref(run.path(), &refs[0]).unwrap();
        let owned = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            owner.clone(),
            artifact.interface_bytes.clone(),
            artifact.product_bytes.clone(),
            artifact.package_imports_bytes.clone(),
            artifact.certification_bytes.clone(),
        )
        .with_module_interface(artifact.module_interface.clone())
        .unwrap();
        assert_eq!(
            certify_owned_products_with_validation(
                &[&owned],
                &[],
                &mut PackageInterfaceValidation::default(),
            )
            .unwrap(),
            certify_inherited_products(
                &[InheritedProductInput {
                    artifact: &artifact
                }],
                &[]
            )
            .unwrap(),
        );
        assert!(certify_owned_products_with_validation(
            &[&owned, &owned],
            &[],
            &mut PackageInterfaceValidation::default(),
        )
        .is_err());
        let corrupt = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            owner.clone(),
            artifact.interface_bytes.clone(),
            vec![0],
            artifact.package_imports_bytes.clone(),
            artifact.certification_bytes.clone(),
        );
        assert!(certify_owned_products_with_validation(
            &[&corrupt],
            &[],
            &mut PackageInterfaceValidation::default(),
        )
        .is_err());
        assert!(certify_inherited_products(
            &[InheritedProductInput {
                artifact: &artifact
            }],
            &[]
        )
        .unwrap()
        .is_empty());
        assert!(certify_inherited_products(
            &[
                InheritedProductInput {
                    artifact: &artifact
                },
                InheritedProductInput {
                    artifact: &artifact
                },
            ],
            &[]
        )
        .is_err());
        let mut trailing = seal.clone();
        trailing.push(0);
        assert!(validate_home_certification(&trailing, &owner).is_err());
        let size_operation = InventoryOperation::new(InventoryDecodeLimits {
            max_module_bytes: seal.len() - 1,
            ..InventoryDecodeLimits::default()
        });
        assert!(matches!(
            decode_home_witness_with_operation(&seal, &size_operation),
            Err(CertificationError::SizeLimit {
                format: CertificationFormat::HomeOwners,
                ..
            })
        ));
        let mut wrong_owner = owner.clone();
        wrong_owner.module_version = ModuleVersion([9; 32]);
        let wrong_seal = encode_home_certification(&wrong_owner, &[], &BTreeMap::new()).unwrap();
        artifact.certification_bytes = wrong_seal;
        assert!(certify_inherited_products(
            &[InheritedProductInput {
                artifact: &artifact
            }],
            &[]
        )
        .is_err());
        artifact.certification_bytes = seal.clone();
        artifact.reference.certification_sha256 = [0; 32];
        assert!(certify_inherited_products(
            &[InheritedProductInput {
                artifact: &artifact
            }],
            &[]
        )
        .is_err());
        artifact.reference.certification_sha256 = sha(&seal);
        let mut witness = decode_home_witness(&seal).unwrap();
        witness.owner.skinny_iface_sha256 = [0; 32];
        artifact.certification_bytes = encode_home_witness(&witness).unwrap();
        artifact.reference.certification_sha256 = sha(&artifact.certification_bytes);
        assert!(certify_inherited_products(
            &[InheritedProductInput {
                artifact: &artifact
            }],
            &[]
        )
        .is_err());
    }

    fn sidecar() -> Vec<u8> {
        let value = Value::Array(vec![
            Value::Text("TPMOD".into()),
            Value::Integer(1.into()),
            Value::Array(vec![Value::Array(vec![
                Value::Text("main".into()),
                Value::Text("Fresh".into()),
                Value::Bytes(vec![0x42]),
                Value::Array(vec![]),
            ])]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn nonempty_sidecar(body_tag: u8) -> Vec<u8> {
        let groups = [7, 11]
            .into_iter()
            .map(|ordinal| {
                let mut wire = testing::wire_program();
                let tidepool_repr::execution_schema::Group::NonRecursive(top) =
                    &mut wire.bindings[0]
                else {
                    unreachable!()
                };
                top.identity = SymbolIdentity {
                    unit: "main".into(),
                    module: "Fresh".into(),
                    namespace: "value".into(),
                    occurrence: format!("entry_{ordinal}"),
                    record_parent: None,
                };
                top.binding.rhs =
                    tidepool_repr::execution_schema::HeapRhs::Bytes(vec![ordinal as u8, body_tag]);
                let top = top.clone();
                wire.bindings = vec![tidepool_repr::execution_schema::Group::Recursive(vec![top])];
                wire.expressions.nodes.clear();
                wire.globals.clear();
                testing::projected_group(wire, ordinal).unwrap()
            })
            .collect();
        tidepool_test_data::prepared_encode::encode_module_products(&[RawModuleProduct {
            unit: "main".into(),
            module: "Fresh".into(),
            interface: vec![0x42],
            groups,
        }])
    }

    fn empty_package_bundle() -> Vec<u8> {
        empty_package_bundle_for("Fresh")
    }

    fn empty_package_bundle_for(module: &str) -> Vec<u8> {
        let roots = Value::Array(vec![
            Value::Text("TPPKGROOTS".into()),
            Value::Text("2".into()),
            Value::Array(vec![
                Value::Text("main".into()),
                Value::Text(module.into()),
                Value::Text(hex(&sha(&[0x42]))),
            ]),
            Value::Array(vec![]),
            Value::Array(vec![]),
        ]);
        let mut sidecar = Vec::new();
        ciborium::ser::into_writer(&roots, &mut sidecar).unwrap();
        let bundle = Value::Array(vec![
            Value::Text("TPPKGBUNDLES".into()),
            Value::Integer(1.into()),
            Value::Array(vec![Value::Array(vec![
                Value::Text("main".into()),
                Value::Text(module.into()),
                Value::Bytes(sidecar),
            ])]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&bundle, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn direct_package_root_and_receipt_must_select_same_path() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("selected.hi");
        let substituted = root.path().join("substituted.hi");
        std::fs::write(&selected, b"same interface").unwrap();
        std::fs::write(&substituted, b"same interface").unwrap();
        let owner = CachedHomeOwner {
            unit: "main".into(),
            module: "Fresh".into(),
            module_version: ModuleVersion([7; 32]),
            skinny_iface_sha256: sha(&[0x42]),
            product_sha256: [8; 32],
        };
        let digest = sha(b"same interface");
        let sidecar = value_array([
            value_text("TPPKGROOTS"),
            value_text("2"),
            value_array([
                value_text("main"),
                value_text("Fresh"),
                value_text(hex(&owner.skinny_iface_sha256)),
            ]),
            value_array([value_array([
                value_text("base-unit"),
                value_text("Data.Base"),
                value_text(selected.display().to_string()),
                value_text(hex(&digest)),
            ])]),
            value_array([]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&sidecar, &mut bytes).unwrap();
        let packages = BTreeMap::from([(
            ("base-unit".into(), "Data.Base".into()),
            PackageInterfaceWitness {
                selected_path: substituted,
                sha256: digest,
            },
        )]);
        assert!(matches!(
            check_direct_package_agreement(&bytes, &owner, &packages),
            Err(CertificationError::Mismatch(
                "direct package/receipt selection"
            ))
        ));
    }

    fn evidence(source: &str) -> DependencyEvidence {
        DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![SourceEvidence {
                path: "@generated-source".into(),
                sha256: hex(&sha(source.as_bytes())),
            }],
            resolutions: vec![],
            packages: vec![],
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "Fresh".into(),
                boot: false,
                source: "@generated-source".into(),
                imports: vec![],
                product: ProductAvailability::Ready,
            }],
        }
    }

    fn receipt(
        bytes: &[u8],
        evidence: &DependencyEvidence,
        source: &str,
    ) -> CertifiedModuleReceipt {
        CertifiedModuleReceipt {
            origin: ProductOrigin::Fresh,
            unit: "main".into(),
            module: "Fresh".into(),
            module_version: None,
            skinny_iface_sha256: sha(&[0x42]),
            product_sha256: sha(bytes),
            source_sha256: sha(source.as_bytes()),
            dependency_witness_sha256: sha(&serde_json::to_vec(evidence).unwrap()),
            interface_requirements: BTreeMap::new(),
            groups: vec![],
        }
    }

    fn fixture_finalization(
        root: Option<&Path>,
        modules: &[CertifiedModuleReceipt],
    ) -> FinalizationEnvelope {
        const INTERFACE: &[u8] = &[0x42];
        let interfaces = modules
            .iter()
            .map(|module| ((module.unit.clone(), module.module.clone()), INTERFACE))
            .collect();
        fixture_finalization_from_interfaces(root, modules, interfaces, None)
    }

    pub(crate) fn fixture_finalization_from_products(
        root: Option<&Path>,
        modules: &[CertifiedModuleReceipt],
        products: &ParsedModuleProducts,
    ) -> FinalizationEnvelope {
        let interfaces = products
            .products()
            .iter()
            .map(|product| {
                (
                    (product.unit.clone(), product.module.clone()),
                    product.interface.as_slice(),
                )
            })
            .collect();
        fixture_finalization_from_interfaces(
            root,
            modules,
            interfaces,
            Some(
                products
                    .package_imports
                    .as_ref()
                    .expect("product finalization fixture requires actual package bytes"),
            ),
        )
    }

    fn fixture_finalization_from_interfaces(
        root: Option<&Path>,
        modules: &[CertifiedModuleReceipt],
        interfaces: BTreeMap<(String, String), &[u8]>,
        package_roots: Option<&BTreeMap<(String, String), Vec<u8>>>,
    ) -> FinalizationEnvelope {
        let mut finalized = BTreeMap::new();
        let mut home_units = BTreeSet::from(["main".to_owned()]);
        for module in modules {
            home_units.insert(module.unit.clone());
            home_units.extend(
                module
                    .interface_requirements
                    .keys()
                    .map(|(unit, _)| unit.clone()),
            );
            if module.origin != ProductOrigin::Fresh {
                continue;
            }
            let interface_bytes = *interfaces
                .get(&(module.unit.clone(), module.module.clone()))
                .expect("fresh finalization fixture requires actual interface bytes");
            let interface_sha256 = sha(interface_bytes);
            let mut empty_packages = Vec::new();
            let packages = match package_roots {
                Some(roots) => roots
                    .get(&(module.unit.clone(), module.module.clone()))
                    .expect("fresh finalization fixture requires actual package bytes")
                    .as_slice(),
                None => {
                    let package_value = value_array([
                        value_text("TPPKGROOTS"),
                        value_text("2"),
                        value_array([
                            value_text(&module.unit),
                            value_text(&module.module),
                            value_text(hex(&interface_sha256)),
                        ]),
                        value_array([]),
                        value_array([]),
                    ]);
                    ciborium::ser::into_writer(&package_value, &mut empty_packages).unwrap();
                    empty_packages.as_slice()
                }
            };
            let descriptor = |suffix: &str, bytes: &[u8], digest| CapturedArtifactDescriptor {
                relative_path: PathBuf::from(format!(
                    "finalized-fixture/{}.{}.{}",
                    module.unit, module.module, suffix
                )),
                sha256: digest,
                bytes: bytes.len() as u64,
            };
            let interface = descriptor("hi", interface_bytes, interface_sha256);
            let package_imports = descriptor("packages", packages, sha(packages));
            let core_bytes = b"fixture-finalized-core";
            let core = descriptor("core", core_bytes, sha(core_bytes));
            if let Some(root) = root {
                std::fs::create_dir_all(root.join("finalized-fixture")).unwrap();
                for (artifact, bytes) in [
                    (&interface, interface_bytes),
                    (&package_imports, packages),
                    (&core, core_bytes.as_slice()),
                ] {
                    std::fs::write(root.join(&artifact.relative_path), bytes).unwrap();
                }
            }
            finalized.insert(
                (module.unit.clone(), module.module.clone()),
                FinalizedModuleReceipt {
                    unit: module.unit.clone(),
                    module: module.module.clone(),
                    source_sha256: module.source_sha256,
                    interface,
                    package_imports,
                    core: Some(core),
                    interface_requirements: module.interface_requirements.clone(),
                },
            );
        }
        FinalizationEnvelope {
            profile: finalized_module::FINALIZATION_ENVELOPE_PROFILE.into(),
            value_interfaces: BTreeMap::new(),
            home_units,
            modules: finalized,
        }
    }

    fn cached_closure_fixture(
        root: &Path,
        module: &str,
    ) -> (
        crate::module_candidates::CandidateBundle,
        CertifiedModuleReceipt,
        crate::recovery_artifacts::CertifiedRecoveryProduct,
    ) {
        let source = root.join(format!("{module}.hs"));
        let source_text = format!("module {module} where\n");
        std::fs::write(&source, &source_text).unwrap();
        let mut value: Value = ciborium::de::from_reader(sidecar().as_slice()).unwrap();
        let Value::Array(header) = &mut value else {
            unreachable!()
        };
        let Value::Array(rows) = &mut header[2] else {
            unreachable!()
        };
        let Value::Array(row) = &mut rows[0] else {
            unreachable!()
        };
        row[1] = value_text(module);
        let mut product_bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut product_bytes).unwrap();
        let parsed = ParsedModuleProducts::decode(&product_bytes, b"").unwrap();
        let product = parsed.products()[0].clone();
        let mut roots: Value =
            ciborium::de::from_reader(empty_package_bundle().as_slice()).unwrap();
        let roots = roots.as_array_mut().unwrap()[2].as_array_mut().unwrap()[0]
            .as_array_mut()
            .unwrap()[2]
            .as_bytes()
            .unwrap()
            .to_vec();
        let mut roots: Value = ciborium::de::from_reader(roots.as_slice()).unwrap();
        roots.as_array_mut().unwrap()[2].as_array_mut().unwrap()[1] = value_text(module);
        let mut package_bytes = Vec::new();
        ciborium::ser::into_writer(&roots, &mut package_bytes).unwrap();
        let owner = CachedHomeOwner {
            unit: "main".into(),
            module: module.into(),
            module_version: ModuleVersion([7; 32]),
            skinny_iface_sha256: sha(&product.interface),
            product_sha256: sha(&product_bytes),
        };
        let mut evidence = evidence("target");
        evidence.modules[0].module = "Target".into();
        evidence.sources.push(SourceEvidence {
            path: source.clone(),
            sha256: hex(&sha(source_text.as_bytes())),
        });
        evidence.modules.push(ModuleEvidence {
            unit: "main".into(),
            module: module.into(),
            boot: false,
            source: source.clone(),
            imports: vec![],
            product: ProductAvailability::Ready,
        });
        let interface_path = root.join(format!("{module}.hi"));
        let package_path = root.join(format!("{module}.hi.packages"));
        std::fs::write(&interface_path, &product.interface).unwrap();
        std::fs::write(&package_path, &package_bytes).unwrap();
        let recovery = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            owner.clone(),
            product.interface.clone(),
            product_bytes.clone(),
            package_bytes.clone(),
            encode_home_certification(&owner, &[], &BTreeMap::new()).unwrap(),
        );
        let recovery = fixture_finalized_product(
            recovery.with_source_sha256(sha(source_text.as_bytes())),
            [3; 32],
        );
        let accepted = CertifiedModuleReceipt {
            origin: ProductOrigin::Cached,
            unit: owner.unit.clone(),
            module: module.into(),
            module_version: Some(owner.module_version.clone()),
            skinny_iface_sha256: owner.skinny_iface_sha256,
            product_sha256: owner.product_sha256,
            source_sha256: sha(source_text.as_bytes()),
            dependency_witness_sha256: sha(&serde_json::to_vec(&evidence).unwrap()),
            interface_requirements: BTreeMap::new(),
            groups: vec![],
        };
        (
            crate::module_candidates::CandidateBundle {
                owner,
                product: crate::module_candidates::CandidateProduct::decode(product_bytes).unwrap(),
                source,
                source_sha256: hex(&accepted.source_sha256),
                iface_path: interface_path,
                iface_sha256: hex(&accepted.skinny_iface_sha256),
                package_imports_path: package_path,
                package_imports_sha256: hex(&sha(&package_bytes)),
                package_imports_bytes: package_bytes,
                evidence: evidence.into(),
                target_source: "target".into(),
                origin: crate::module_candidates::CandidateOrigin::Ordinary,
                original_module_interface: recovery.module_interface().unwrap().clone(),
                original_execution: None,
                execution_admitted: false,
            },
            accepted,
            recovery,
        )
    }

    fn cached_closure_pair(
        root: &Path,
    ) -> (
        CandidateSet,
        CertifiedReceipt,
        DependencyEvidence,
        crate::recovery_artifacts::CertifiedRecoveryProduct,
        crate::recovery_artifacts::CertifiedRecoveryProduct,
    ) {
        use crate::cache::{ImportQualifier, ModuleImportEvidence, ResolutionEvidence};
        let (mut a, accepted_a, original_a) = cached_closure_fixture(root, "A");
        let (b, accepted_b, original_b) = cached_closure_fixture(root, "B");
        a.evidence.make_mut().modules[1]
            .imports
            .push(ModuleImportEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "B".into(),
                boot: false,
                selected: Some(b.source.clone()),
            });
        a.evidence.make_mut().resolutions.push(ResolutionEvidence {
            qualifier: ImportQualifier::Unqualified,
            module: "B".into(),
            boot: false,
            selected: Some(b.source.clone()),
            candidates: vec![b.source.clone()],
        });
        a.evidence
            .make_mut()
            .sources
            .push(b.evidence.sources[1].clone());
        a.evidence
            .make_mut()
            .modules
            .push(b.evidence.modules[1].clone());
        assert!(a.evidence.valid(&a.target_source));
        let current = (*a.evidence).clone();
        (
            CandidateSet {
                manifest_path: root.join("unused.cbor"),
                by_owner: BTreeMap::from([
                    (("main".into(), "A".into()), a),
                    (("main".into(), "B".into()), b),
                ]),
            },
            CertifiedReceipt {
                source_recipe: WorkerExecutionSource::Ordinary,
                finalization: fixture_finalization(
                    None,
                    &vec![accepted_a.clone(), accepted_b.clone()],
                ),
                modules: vec![accepted_a, accepted_b],
                targets: BTreeMap::new(),
                packages: BTreeMap::new(),
            },
            current,
            original_a,
            original_b,
        )
    }

    #[test]
    fn exact_cached_closure_without_cached_modules_does_not_walk_inventory() {
        let root = tempfile::tempdir().unwrap();
        let (_, mut receipt, current, original_a, original_b) = cached_closure_pair(root.path());
        let context = crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([3; 32], &[original_a, original_b])
            .unwrap();
        for module in &mut receipt.modules {
            module.origin = ProductOrigin::Fresh;
        }
        receipt.finalization = fixture_finalization(None, &receipt.modules);
        let before = context.artifact_view().inventory().metrics();
        validate_exact_cached_closure(None, &receipt, &context, &current, &BTreeMap::new())
            .unwrap();
        assert_eq!(context.artifact_view().inventory().metrics(), before);
        receipt.modules.push(receipt.modules[0].clone());
        assert!(matches!(
            validate_exact_cached_closure(None, &receipt, &context, &current, &BTreeMap::new()),
            Err(CertificationError::Mismatch("duplicate receipt module"))
        ));
        assert_eq!(context.artifact_view().inventory().metrics(), before);
    }

    #[test]
    fn exact_cached_closure_requires_accepted_original_dependency_and_current_path() {
        let root = tempfile::tempdir().unwrap();
        let (candidates, mut receipt, mut current, _, _) = cached_closure_pair(root.path());
        let context =
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
        let imports = BTreeMap::new();
        validate_exact_cached_closure(Some(&candidates), &receipt, &context, &current, &imports)
            .unwrap();
        let b = receipt.modules.pop().unwrap();
        assert!(matches!(
            validate_exact_cached_closure(
                Some(&candidates),
                &receipt,
                &context,
                &current,
                &imports
            ),
            Err(CertificationError::Mismatch(
                "cached candidate dependency is not accepted"
            ))
        ));
        receipt.modules.push(b);
        receipt.modules[1].origin = ProductOrigin::Fresh;
        assert!(matches!(
            validate_exact_cached_closure(
                Some(&candidates),
                &receipt,
                &context,
                &current,
                &imports
            ),
            Err(CertificationError::Mismatch(
                "cached candidate dependency is not accepted"
            ))
        ));
        receipt.modules[1].origin = ProductOrigin::Cached;
        receipt.modules[1].module_version = Some(ModuleVersion([9; 32]));
        assert!(validate_exact_cached_closure(
            Some(&candidates),
            &receipt,
            &context,
            &current,
            &imports
        )
        .is_err());
        receipt.modules[1].module_version = Some(ModuleVersion([7; 32]));
        let replacement = root.path().join("another-B.hs");
        std::fs::copy(&current.modules[2].source, &replacement).unwrap();
        current.modules[2].source = replacement;
        assert!(matches!(
            validate_exact_cached_closure(
                Some(&candidates),
                &receipt,
                &context,
                &current,
                &imports
            ),
            Err(CertificationError::Mismatch(
                "cached candidate dependency current path"
            ))
        ));
    }

    #[test]
    fn exact_cached_closure_refuses_zero_group_owner_overlap_and_inherited_substitute() {
        let root = tempfile::tempdir().unwrap();
        let (candidates, mut receipt, current, a, b) = cached_closure_pair(root.path());
        let empty =
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
        let overlapping = empty
            .clone()
            .extend_checked_original_products([3; 32], &[a])
            .unwrap();
        assert!(overlapping.recovery_products()[0].product_bytes().len() > 0);
        // A sealed zero-group payload without a recovered native witness is
        // unavailable as an exact original reoffer, even for an identical owner.
        assert!(overlapping.recovery_products()[0]
            .original_native()
            .is_none());
        let refused = validate_exact_cached_closure(
            Some(&candidates),
            &receipt,
            &overlapping,
            &current,
            &BTreeMap::new(),
        );
        assert!(
            matches!(
                &refused,
                Err(CertificationError::Mismatch(
                    "cached original reoffer bytes"
                ))
            ),
            "missing-native reoffer refusal: {refused:?}"
        );
        let inherited = empty
            .clone()
            .extend_checked_original_products([3; 32], &[b])
            .unwrap();
        receipt.modules.pop();
        assert!(matches!(
            validate_exact_cached_closure(
                Some(&candidates),
                &receipt,
                &inherited,
                &current,
                &BTreeMap::new()
            ),
            Err(CertificationError::Mismatch(
                "cached candidate dependency is not accepted"
            ))
        ));
        let identity = |name: &str| crate::declaration_join::ExactModuleIdentity {
            unit: "main".into(),
            module: name.into(),
        };
        let imports = BTreeMap::from([(identity("A"), vec![identity("B")])]);
        assert!(matches!(
            validate_exact_cached_closure(Some(&candidates), &receipt, &empty, &current, &imports),
            Err(CertificationError::Mismatch(
                "cached candidate exact dependency authority"
            ))
        ));
    }

    fn cached_dependency_graph(
        root: &Path,
        evidence: &DependencyEvidence,
        mut owners: Vec<CachedHomeOwner>,
    ) -> Arc<crate::execution_source::CertifiedExecutionSourceGraph> {
        use crate::execution_source::{ExecutionSourceAdmission, ExecutionSourceGraphInput};
        let source_path = root.join("Target.hs");
        std::fs::write(&source_path, "target").unwrap();
        owners.push(CachedHomeOwner {
            unit: "main".into(),
            module: "Target".into(),
            module_version: ModuleVersion([4; 32]),
            skinny_iface_sha256: [5; 32],
            product_sha256: [6; 32],
        });
        let fresh = owners
            .iter()
            .map(|owner| crate::declaration_join::ExactModuleIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
            })
            .collect();
        let graph = crate::execution_source::CertifiedExecutionSourceGraph::admit(
            ExecutionSourceGraphInput {
                producer: crate::artifact_inventory::CanonicalProducerIdentity::from_test_sha256(
                    [3; 32],
                ),
                semantic_sha256: None,
                include: &[],
                source_path: &source_path,
                source: "target",
                evidence,
                exact_imports: &BTreeMap::new(),
                owners: &owners,
                fresh_owners: &fresh,
                retained_sources: &BTreeMap::new(),
                packages: &BTreeMap::new(),
            },
        )
        .unwrap();
        let ExecutionSourceAdmission::Available(graph) = graph else {
            panic!("dependency fixture graph unavailable");
        };
        graph
    }

    #[test]
    fn exact_cached_closure_admits_checked_original_across_receipts() {
        let root = tempfile::tempdir().unwrap();
        let (mut candidates, mut receipt, mut current, _, b) = cached_closure_pair(root.path());
        let a_key = ("main".into(), "A".into());
        let b_key = ("main".into(), "B".into());
        let b_graph = cached_dependency_graph(
            root.path(),
            &candidates.by_owner[&b_key].evidence,
            vec![b.owner().clone()],
        );
        let seal = bind_home_execution_source(
            b.certification_bytes(),
            b.owner(),
            b_graph.digest(),
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        let b = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            b.owner().clone(),
            b.interface_bytes().to_vec(),
            b.product_bytes().to_vec(),
            b.package_imports_bytes().to_vec(),
            seal,
        )
        .with_module_interface(b.module_interface().unwrap().clone())
        .unwrap()
        .with_execution_source(b_graph.clone())
        .unwrap();
        let context = crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([3; 32], &[b.clone()])
            .unwrap();
        assert!(context.lexical_graph().is_empty());
        let accepted_b = receipt.modules.pop().unwrap();
        current.modules[1].imports.clear();
        current.modules.retain(|module| module.module != "B");
        current
            .sources
            .retain(|source| source.path != candidates.by_owner[&b_key].source);
        current.resolutions.clear();
        let identity = |name: &str| crate::declaration_join::ExactModuleIdentity {
            unit: "main".into(),
            module: name.into(),
        };
        let imports = BTreeMap::from([(identity("A"), vec![identity("B")])]);
        let a_owner = candidates.by_owner[&a_key].owner.clone();
        let a_evidence = candidates.by_owner[&a_key].evidence.clone();
        let graph = cached_dependency_graph(
            root.path(),
            &a_evidence,
            vec![a_owner.clone(), b.owner().clone()],
        );
        assert_ne!(graph.digest(), b_graph.digest());
        candidates
            .by_owner
            .get_mut(&a_key)
            .unwrap()
            .original_execution = Some(crate::module_candidates::OriginalCandidateExecution {
            graph: graph.clone(),
        });
        validate_exact_cached_closure(Some(&candidates), &receipt, &context, &current, &imports)
            .unwrap();
        receipt.modules.push(accepted_b);
        assert!(b.original_native().is_none());
        let refused = validate_exact_cached_closure(
            Some(&candidates),
            &receipt,
            &context,
            &current,
            &imports,
        );
        assert!(
            matches!(
                &refused,
                Err(CertificationError::Mismatch(
                    "cached original reoffer bytes"
                ))
            ),
            "missing-native dependency reoffer refusal: {refused:?}"
        );
        receipt.modules.pop();
        // A retained dependency additionally binds B's separately issued graph.
        let retained = crate::execution_source::test_graph_requiring_original(
            &graph,
            b.owner(),
            b_graph.digest(),
        );
        candidates
            .by_owner
            .get_mut(&a_key)
            .unwrap()
            .original_execution =
            Some(crate::module_candidates::OriginalCandidateExecution { graph: retained });
        validate_exact_cached_closure(Some(&candidates), &receipt, &context, &current, &imports)
            .unwrap();
        // Available native originals alone never grant their lexical imports.
        assert!(matches!(
            validate_exact_cached_closure(
                Some(&candidates),
                &receipt,
                &context,
                &current,
                &BTreeMap::new()
            ),
            Err(CertificationError::Mismatch(
                "cached candidate exact dependency authority"
            ))
        ));
        for component in 0..3 {
            let mut changed = b.owner().clone();
            match component {
                0 => changed.module_version = ModuleVersion([9; 32]),
                1 => changed.skinny_iface_sha256 = [9; 32],
                _ => changed.product_sha256 = [9; 32],
            }
            let graph =
                cached_dependency_graph(root.path(), &a_evidence, vec![a_owner.clone(), changed]);
            candidates
                .by_owner
                .get_mut(&a_key)
                .unwrap()
                .original_execution =
                Some(crate::module_candidates::OriginalCandidateExecution { graph });
            assert!(matches!(
                validate_exact_cached_closure(
                    Some(&candidates),
                    &receipt,
                    &context,
                    &current,
                    &imports
                ),
                Err(CertificationError::Mismatch(
                    "cached candidate exact dependency closure"
                ))
            ));
        }
        candidates
            .by_owner
            .get_mut(&a_key)
            .unwrap()
            .original_execution =
            Some(crate::module_candidates::OriginalCandidateExecution { graph });
        let empty =
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
        // B remains offered, but its rejected receipt cannot satisfy A's graph.
        assert!(matches!(
            validate_exact_cached_closure(Some(&candidates), &receipt, &empty, &current, &imports),
            Err(CertificationError::Mismatch(
                "cached candidate exact dependency closure"
            ))
        ));
        let mut wrong_graph = b_graph.digest();
        wrong_graph[0] ^= 1;
        let graph = candidates.by_owner[&a_key]
            .original_execution
            .as_ref()
            .unwrap()
            .graph
            .clone();
        let graph =
            crate::execution_source::test_graph_requiring_original(&graph, b.owner(), wrong_graph);
        candidates
            .by_owner
            .get_mut(&a_key)
            .unwrap()
            .original_execution =
            Some(crate::module_candidates::OriginalCandidateExecution { graph });
        assert!(validate_exact_cached_closure(
            Some(&candidates),
            &receipt,
            &context,
            &current,
            &imports
        )
        .is_err());
    }

    #[test]
    #[serial_test::serial]
    fn published_candidate_restores_original_execution_graph_after_cached_certification() {
        published_candidate_handoff(false, false);
    }

    #[test]
    #[serial_test::serial]
    fn exact_published_candidate_preserves_original_version_and_execution_graph() {
        published_candidate_handoff(true, false);
    }

    #[test]
    #[serial_test::serial]
    fn fresh_identical_candidate_uses_receipt_origin_without_original_graph_backfill() {
        published_candidate_handoff(false, true);
    }

    fn published_candidate_handoff(exact_mode: bool, recompile_fresh: bool) {
        use crate::module_candidates::{
            prepare_publication, publish_prepared, select, CandidateVersionOrigin,
        };
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("Target.hs");
        let support = root.path().join("Fresh.hs");
        let source = "module Target where";
        let support_source = "module Fresh where";
        std::fs::write(&input, source).unwrap();
        std::fs::write(&support, support_source).unwrap();
        let mut admitted = evidence(support_source);
        admitted.sources[0].path = support.clone();
        admitted.modules[0].source = support;
        admitted.sources.push(SourceEvidence {
            path: "@generated-source".into(),
            sha256: hex(&sha(source.as_bytes())),
        });
        let mut worker = admitted.clone();
        worker.sources[1].path = input.clone();
        let mut evidence_bytes = serde_json::to_vec(&worker).unwrap();
        let bytes = sidecar();
        let packages = empty_package_bundle();
        let producer = [3; 32];
        let include = [root.path().to_owned()];
        let previous = std::env::var_os("TIDEPOOL_COMPILE_CACHE_DIR");
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe {
                    match self.0.take() {
                        Some(value) => std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", value),
                        None => std::env::remove_var("TIDEPOOL_COMPILE_CACHE_DIR"),
                    }
                }
            }
        }
        let _restore = Restore(previous);
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", root.path());
        }
        let mut accepted = receipt(&bytes, &admitted, support_source);
        accepted.dependency_witness_sha256 = sha(&evidence_bytes);
        let mut fresh_receipt = CertifiedReceipt {
            source_recipe: WorkerExecutionSource::Ordinary,
            finalization: fixture_finalization(Some(root.path()), &vec![accepted.clone()]),
            modules: vec![accepted.clone()],
            targets: BTreeMap::new(),
            packages: BTreeMap::new(),
        };
        let context = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products(
                    crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                        &producer,
                    )
                    .sha256(),
                    &[],
                )
                .unwrap(),
        );
        let request = context
            .prepare_compilation(&root.path().join("exact-inputs"), &producer)
            .unwrap();
        if exact_mode {
            let receipt_root = root.path().join(".exact-compilations/original");
            std::fs::create_dir_all(&receipt_root).unwrap();
            let snapshot = receipt_root.join("source.hs");
            std::fs::write(&snapshot, source).unwrap();
            let exact_receipt = Value::Array(vec![
                Value::Text("TPEXACTCOMPILE".into()),
                Value::Text("3".into()),
                Value::Text(request.request_sha256.clone()),
                Value::Text(hex(&request.semantic_sha256)),
                Value::Text(input.to_string_lossy().into_owned()),
                Value::Text(hex(&sha(source.as_bytes()))),
                Value::Text(snapshot.to_string_lossy().into_owned()),
                Value::Text(String::from_utf8(evidence_bytes.clone()).unwrap()),
                Value::Array(vec![Value::Array(vec![
                    Value::Text("main".into()),
                    Value::Text("Fresh".into()),
                    Value::Bool(false),
                    Value::Array(vec![]),
                ])]),
                Value::Array(vec![Value::Array(vec![]), Value::Null]),
            ]);
            let mut encoded = Vec::new();
            ciborium::ser::into_writer(&exact_receipt, &mut encoded).unwrap();
            std::fs::write(receipt_root.join("receipt.cbor"), encoded).unwrap();
            worker.cache_safe = false;
            worker.selection_complete = false;
            evidence_bytes = serde_json::to_vec(&worker).unwrap();
            fresh_receipt.modules[0].dependency_witness_sha256 = sha(&evidence_bytes);
        }
        let exact_source = exact_mode.then(|| {
            request
                .admit_source(&input, source, &evidence_bytes)
                .unwrap()
        });
        let exact =
            exact_source
                .as_ref()
                .map(|source| crate::declaration_context::ExactProductAdmission {
                    request: &request,
                    source,
                });
        let version_origin = exact
            .as_ref()
            .map_or(CandidateVersionOrigin::Ordinary, |admission| {
                CandidateVersionOrigin::Exact {
                    semantic_sha256: admission.request.semantic_sha256,
                }
            });
        let parsed = ParsedModuleProducts::decode(&bytes, &packages).unwrap();
        if let Some(exact) = exact.as_ref() {
            assert!(matches!(
                certify_products(
                    None,
                    &fresh_receipt,
                    &parsed,
                    &evidence_bytes,
                    &input,
                    input.parent().unwrap(),
                    &CompletedSourceEvidence::from_normalized(admitted.clone(), source).unwrap(),
                    source,
                    &producer,
                    &include,
                    Some(exact),
                    None
                ),
                Err(CertificationError::Mismatch(
                    "source recipe compilation route"
                ))
            ));
            let product = &parsed.products()[0];
            let native = &parsed.sidecars[0];
            let packages =
                &parsed.package_imports.as_ref().unwrap()[&("main".into(), "Fresh".into())];
            let owner = CachedHomeOwner {
                unit: product.unit.clone(),
                module: product.module.clone(),
                module_version: crate::module_candidates::exact_module_version_for_product(
                    &producer,
                    &request.semantic_sha256,
                    &product.unit,
                    &product.module,
                    &accepted.source_sha256,
                    &product.interface,
                    native,
                    packages,
                ),
                skinny_iface_sha256: sha(&product.interface),
                product_sha256: sha(native),
            };
            let owners = [owner];
            let fresh = BTreeSet::from([crate::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: "Fresh".into(),
            }]);
            let crate::execution_source::ExecutionSourceAdmission::Available(graph) =
                crate::execution_source::CertifiedExecutionSourceGraph::admit(
                    crate::execution_source::ExecutionSourceGraphInput {
                        producer: crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&producer),
                        semantic_sha256: Some(request.semantic_sha256), include: &include,
                        source_path: &input, source, evidence: &admitted,
                        exact_imports: &exact.source.exact_imports, owners: &owners,
                        fresh_owners: &fresh, retained_sources: &BTreeMap::new(), packages: &BTreeMap::new(),
                    }).unwrap() else { panic!("complete fixture source recipe") };
            fresh_receipt.source_recipe = WorkerExecutionSource::ExactAvailable {
                digest: graph.digest(),
                bytes: graph.bytes().into(),
            };
        }
        let original = certify_products(
            None,
            &fresh_receipt,
            &parsed,
            &evidence_bytes,
            &input,
            input.parent().unwrap(),
            &CompletedSourceEvidence::from_normalized(admitted.clone(), source).unwrap(),
            source,
            &producer,
            &include,
            exact.as_ref(),
            None,
        )
        .unwrap();
        let original_graph = original.recovery_products[0]
            .execution_source()
            .unwrap()
            .clone();
        let (_, publication) = prepare_publication(
            &producer,
            &include,
            &admitted,
            parsed,
            source,
            version_origin,
            &original.recovery_products,
        );
        publish_prepared(publication);
        let scratch = root.path().join("offer");
        let selected = select(&producer, &include, &scratch).unwrap();
        let bundle = selected
            .by_owner
            .get(&(accepted.unit.clone(), accepted.module.clone()))
            .unwrap();
        assert_eq!(bundle.owner, *original.recovery_products[0].owner());
        assert_eq!(
            bundle.original_execution.as_ref().unwrap().graph.digest(),
            original_graph.digest()
        );
        let manifest = std::fs::read(&selected.manifest_path).unwrap();
        let ciborium::value::Value::Array(fields) =
            ciborium::de::from_reader(manifest.as_slice()).unwrap()
        else {
            panic!("manifest");
        };
        assert_eq!(fields[1].as_text(), Some("10"));
        let execution = fields[5].as_array().unwrap();
        assert_eq!(execution[0].as_array().unwrap().len(), 1);
        assert_eq!(execution[1].as_array().unwrap().len(), 1);
        if recompile_fresh {
            // A new transaction can produce the exact same native owner. Its
            // source recipe belongs to that transaction, not the offered record.
            let new_input = root.path().join("TargetAgain.hs");
            std::fs::write(&new_input, source).unwrap();
            let mut new_worker = admitted.clone();
            new_worker.sources[1].path = new_input.clone();
            let new_evidence_bytes = serde_json::to_vec(&new_worker).unwrap();
            let mut new_receipt = fresh_receipt.clone();
            new_receipt.modules[0].dependency_witness_sha256 = sha(&new_evidence_bytes);
            let parsed = ParsedModuleProducts::decode(&bytes, &packages).unwrap();
            let fresh = certify_products(
                Some(&selected),
                &new_receipt,
                &parsed,
                &new_evidence_bytes,
                &new_input,
                new_input.parent().unwrap(),
                &CompletedSourceEvidence::from_normalized(admitted.clone(), source).unwrap(),
                source,
                &producer,
                &include,
                None,
                None,
            )
            .unwrap();
            let product = &fresh.recovery_products[0];
            assert_eq!(product.owner(), &bundle.owner);
            let graph = product.execution_source().unwrap();
            assert_ne!(graph.digest(), original_graph.digest());
            assert!(graph.required_original_graphs(product.owner()).is_empty());

            let mut incomplete = admitted.clone();
            incomplete.selection_complete = false;
            new_worker.selection_complete = false;
            let incomplete_bytes = serde_json::to_vec(&new_worker).unwrap();
            new_receipt.modules[0].dependency_witness_sha256 = sha(&incomplete_bytes);
            let fresh = certify_products(
                Some(&selected),
                &new_receipt,
                &parsed,
                &incomplete_bytes,
                &new_input,
                new_input.parent().unwrap(),
                &CompletedSourceEvidence::from_normalized(incomplete.clone(), source).unwrap(),
                source,
                &producer,
                &include,
                None,
                None,
            )
            .unwrap();
            assert_eq!(fresh.recovery_products[0].owner(), &bundle.owner);
            assert!(fresh.recovery_products[0].execution_source().is_none());
            assert_eq!(
                home_execution_source_digest_with_validation(
                    fresh.recovery_products[0].certification_bytes(),
                    &bundle.owner,
                    &mut PackageInterfaceValidation::default(),
                )
                .unwrap(),
                None
            );
            new_receipt.modules[0].dependency_witness_sha256 = [0; 32];
            assert!(certify_products(
                Some(&selected),
                &new_receipt,
                &parsed,
                &incomplete_bytes,
                &new_input,
                new_input.parent().unwrap(),
                &CompletedSourceEvidence::from_normalized(incomplete.clone(), source).unwrap(),
                source,
                &producer,
                &include,
                None,
                None,
            )
            .is_err());
        }
        accepted.origin = ProductOrigin::Cached;
        accepted.module_version = Some(bundle.owner.module_version.clone());
        accepted.product_sha256 = bundle.owner.product_sha256;
        accepted.dependency_witness_sha256 = sha(&serde_json::to_vec(&bundle.evidence).unwrap());
        let cached_receipt = CertifiedReceipt {
            source_recipe: if exact_mode {
                WorkerExecutionSource::ExactUnavailable(SourceRecipeUnavailable::NoFreshOriginals)
            } else {
                WorkerExecutionSource::Ordinary
            },
            finalization: fixture_finalization(Some(root.path()), &vec![accepted.clone()]),
            modules: vec![accepted],
            targets: BTreeMap::new(),
            packages: BTreeMap::new(),
        };
        let mut empty_bytes = Vec::new();
        ciborium::ser::into_writer(&("TPMOD", 1u64, Vec::<Value>::new()), &mut empty_bytes)
            .unwrap();
        let mut empty_packages = Vec::new();
        ciborium::ser::into_writer(
            &("TPPKGBUNDLES", 1u64, Vec::<Value>::new()),
            &mut empty_packages,
        )
        .unwrap();
        let empty = ParsedModuleProducts::decode(&empty_bytes, &empty_packages).unwrap();
        let cached = certify_products(
            Some(&selected),
            &cached_receipt,
            &empty,
            &evidence_bytes,
            &input,
            input.parent().unwrap(),
            &CompletedSourceEvidence::from_normalized(admitted.clone(), source).unwrap(),
            source,
            &producer,
            &include,
            exact.as_ref(),
            None,
        )
        .unwrap();
        let mut stale_cached_receipt = cached_receipt.clone();
        stale_cached_receipt.modules[0].dependency_witness_sha256 = [0; 32];
        assert!(certify_products(
            Some(&selected),
            &stale_cached_receipt,
            &empty,
            &evidence_bytes,
            &input,
            input.parent().unwrap(),
            &CompletedSourceEvidence::from_normalized(admitted.clone(), source).unwrap(),
            source,
            &producer,
            &include,
            exact.as_ref(),
            None,
        )
        .is_err());
        let restored = &cached.recovery_products[0];
        assert_eq!(restored.owner(), original.recovery_products[0].owner());
        assert_eq!(
            restored.execution_source().unwrap().bytes(),
            original_graph.bytes()
        );
        assert_eq!(
            home_execution_source_digest_with_validation(
                restored.certification_bytes(),
                restored.owner(),
                &mut PackageInterfaceValidation::default()
            )
            .unwrap(),
            Some(original_graph.digest())
        );
        if !exact_mode {
            use crate::cache::{
                ImportQualifier, ModuleEvidence, ModuleImportEvidence, ProductAvailability,
                ResolutionEvidence,
            };
            let quoter_source = "module Quoter where\nimport Fresh\n";
            let quoter_path = root.path().join("Quoter.hs");
            std::fs::write(&quoter_path, quoter_source).unwrap();
            let mut current = admitted.clone();
            current.sources.push(SourceEvidence {
                path: quoter_path.clone(),
                sha256: hex(&sha(quoter_source.as_bytes())),
            });
            current.modules.push(ModuleEvidence {
                unit: "main".into(),
                module: "Quoter".into(),
                boot: false,
                source: quoter_path,
                imports: vec![ModuleImportEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: "Fresh".into(),
                    boot: false,
                    selected: Some(bundle.source.clone()),
                }],
                product: ProductAvailability::Ready,
            });
            current.resolutions.push(ResolutionEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "Fresh".into(),
                boot: false,
                selected: Some(bundle.source.clone()),
                candidates: vec![bundle.source.clone()],
            });
            let mut worker = current.clone();
            worker
                .sources
                .iter_mut()
                .find(|row| row.path == Path::new("@generated-source"))
                .unwrap()
                .path = input.clone();
            let current_bytes = serde_json::to_vec(&worker).unwrap();
            let mut value: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
            let Value::Array(header) = &mut value else {
                unreachable!()
            };
            let Value::Array(rows) = &mut header[2] else {
                unreachable!()
            };
            let Value::Array(row) = &mut rows[0] else {
                unreachable!()
            };
            row[1] = Value::Text("Quoter".into());
            let mut quoter_bytes = Vec::new();
            ciborium::ser::into_writer(&value, &mut quoter_bytes).unwrap();
            let roots = Value::Array(vec![
                Value::Text("TPPKGROOTS".into()),
                Value::Text("2".into()),
                Value::Array(vec![
                    Value::Text("main".into()),
                    Value::Text("Quoter".into()),
                    Value::Text(hex(&sha(&[0x42]))),
                ]),
                Value::Array(vec![]),
                Value::Array(vec![]),
            ]);
            let mut roots_bytes = Vec::new();
            ciborium::ser::into_writer(&roots, &mut roots_bytes).unwrap();
            let mut package_bytes = Vec::new();
            ciborium::ser::into_writer(
                &(
                    "TPPKGBUNDLES",
                    1u64,
                    vec![("main", "Quoter", Value::Bytes(roots_bytes))],
                ),
                &mut package_bytes,
            )
            .unwrap();
            let parsed = ParsedModuleProducts::decode(&quoter_bytes, &package_bytes).unwrap();
            let mut quoter_receipt = receipt(&quoter_bytes, &current, quoter_source);
            quoter_receipt.module = "Quoter".into();
            quoter_receipt.dependency_witness_sha256 = sha(&current_bytes);
            quoter_receipt.interface_requirements.insert(
                (bundle.owner.unit.clone(), bundle.owner.module.clone()),
                bundle.original_module_interface.interface_sha256(),
            );
            let mixed = CertifiedReceipt {
                source_recipe: WorkerExecutionSource::Ordinary,
                finalization: fixture_finalization(
                    Some(root.path()),
                    &vec![cached_receipt.modules[0].clone(), quoter_receipt.clone()],
                ),
                modules: vec![cached_receipt.modules[0].clone(), quoter_receipt],
                targets: BTreeMap::new(),
                packages: BTreeMap::new(),
            };
            let issued = certify_products(
                Some(&selected),
                &mixed,
                &parsed,
                &current_bytes,
                &input,
                input.parent().unwrap(),
                &CompletedSourceEvidence::from_normalized(current.clone(), source).unwrap(),
                source,
                &producer,
                &include,
                None,
                None,
            )
            .unwrap();
            let mut offered_only = mixed.clone();
            offered_only
                .modules
                .retain(|module| module.origin != ProductOrigin::Cached);
            assert!(matches!(
                certify_products(
                    Some(&selected),
                    &offered_only,
                    &parsed,
                    &current_bytes,
                    &input,
                    input.parent().unwrap(),
                    &CompletedSourceEvidence::from_normalized(current.clone(), source).unwrap(),
                    source,
                    &producer,
                    &include,
                    None,
                    None,
                ),
                Err(CertificationError::FinalizedInterfaceRequirement {
                    selected_sha256: None,
                    ..
                })
            ));
            let quoter = issued
                .recovery_products
                .iter()
                .find(|product| product.owner().module == "Quoter")
                .unwrap();
            let recipe = quoter.execution_source().unwrap();
            assert_eq!(
                recipe.required_original_graphs(quoter.owner()),
                vec![(bundle.owner.clone(), original_graph.digest())]
            );
            assert!(recipe.eligible_source_replay_root(quoter.owner()));
            if let Some(output) = std::env::var_os("TIDEPOOL_CANDIDATE_PROVENANCE_OUTPUT") {
                let output = PathBuf::from(output);
                std::fs::create_dir_all(&output).unwrap();
                let parsed_for_publication =
                    ParsedModuleProducts::decode(&quoter_bytes, &package_bytes).unwrap();
                let (_, publication) = prepare_publication(
                    &producer,
                    &include,
                    &current,
                    parsed_for_publication,
                    source,
                    CandidateVersionOrigin::Ordinary,
                    &issued.recovery_products,
                );
                publish_prepared(publication);
                let fixture =
                    select(&producer, &include, &root.path().join("two-owner-offer")).unwrap();
                assert_eq!(fixture.by_owner.len(), 2);
                let manifest = std::fs::read(&fixture.manifest_path).unwrap();
                std::fs::write(output.join("module-candidates.cbor"), &manifest).unwrap();
                std::fs::write(output.join("fixture.json"), serde_json::to_vec_pretty(&serde_json::json!({
                    "scope": "production Rust issuer/publication/record-selection/TPMCAN8 encoder; synthetic native/interface bytes; decoder-only cross-language fixture, not GHC semantic admission",
                    "raw_producer_hex": hex(&producer), "canonical_producer_sha256": hex(&crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&producer).sha256()),
                    "manifest_sha256": hex(&sha(&manifest)), "live_root": root.path(), "manifest_live_path": fixture.manifest_path,
                    "owners": fixture.by_owner.values().map(|bundle| serde_json::json!({
                        "module": bundle.owner.module, "module_version": hex(&bundle.owner.module_version.0),
                        "interface": bundle.iface_path, "packages": bundle.package_imports_path,
                        "source": bundle.source, "original_graph_sha256": bundle.original_execution.as_ref().map(|proof| hex(&proof.graph.digest()))
                    })).collect::<Vec<_>>()
                })).unwrap()).unwrap();
            }
            let mut without_proof = selected;
            without_proof
                .by_owner
                .get_mut(&("main".into(), "Fresh".into()))
                .unwrap()
                .original_execution = None;
            let native_only = certify_products(
                Some(&without_proof),
                &mixed,
                &parsed,
                &current_bytes,
                &input,
                input.parent().unwrap(),
                &CompletedSourceEvidence::from_normalized(current.clone(), source).unwrap(),
                source,
                &producer,
                &include,
                None,
                None,
            )
            .unwrap();
            assert!(native_only
                .recovery_products
                .iter()
                .all(|product| product.execution_source().is_none()));
        }
        // Advertised proof cannot silently downgrade when the shared blob disappears.
        let graph_path =
            crate::module_candidates::test_candidate_graph_path(&producer, original_graph.digest());
        let graph_bytes = std::fs::read(&graph_path).unwrap();
        std::fs::remove_file(&graph_path).unwrap();
        assert!(select(&producer, &include, &root.path().join("missing-offer")).is_none());
        std::fs::write(&graph_path, b"tampered").unwrap();
        assert!(select(&producer, &include, &root.path().join("tampered-offer")).is_none());
        std::fs::write(&graph_path, graph_bytes).unwrap();
        assert!(select(&producer, &include, &root.path().join("restored-offer")).is_some());
        if !exact_mode && std::env::var_os("TIDEPOOL_CANDIDATE_PROVENANCE_OUTPUT").is_some() {
            let _retained = root.keep();
        }
    }

    #[test]
    fn exact_source_recipe_selects_current_owner_with_two_retained_versions() {
        exact_current_original_history(false);
    }

    #[test]
    #[serial_test::serial]
    fn nonempty_exact_source_recipe_preserves_original_versions_and_selected_subsets() {
        exact_current_original_history(true);
    }

    #[test]
    fn repeated_retained_core_promotion_preserves_exact_original_membership() {
        use crate::artifact_inventory::CanonicalProducerIdentity;
        use crate::declaration_context::{ExactDeclarationContext, ExactProductAdmission};
        let root = tempfile::tempdir().unwrap();
        let source = "module Target where";
        let support_source = "module Fresh where";
        let support = root.path().join("Fresh.hs");
        std::fs::write(&support, support_source).unwrap();
        let mut admitted = evidence(support_source);
        admitted.sources[0].path = support.clone();
        admitted.modules[0].source = support;
        admitted.sources.push(SourceEvidence {
            path: "@generated-source".into(),
            sha256: hex(&sha(source.as_bytes())),
        });
        let producer = [3; 32];
        let canonical_producer = CanonicalProducerIdentity::from_producer_bytes(&producer).sha256();
        let include = [root.path().to_path_buf()];
        let bytes = nonempty_sidecar(0);
        let parsed = ParsedModuleProducts::decode(&bytes, &empty_package_bundle()).unwrap();
        let normalized =
            CompletedSourceEvidence::from_normalized(admitted.clone(), source).unwrap();
        let initial_input = root.path().join("Target.hs");
        std::fs::write(&initial_input, source).unwrap();
        let mut worker = admitted.clone();
        worker.sources[1].path = initial_input.clone();
        let initial_evidence = serde_json::to_vec(&worker).unwrap();
        let mut accepted = receipt(&bytes, &admitted, support_source);
        {
            accepted.groups = [7, 11]
                .into_iter()
                .map(|original_ordinal| AcceptedGroup {
                    original_ordinal,
                    globals: vec![],
                })
                .collect();
            assert_eq!(
                parsed.products()[0]
                    .groups
                    .iter()
                    .map(|group| group.binders().len())
                    .sum::<usize>(),
                2,
            );
        }
        accepted.dependency_witness_sha256 = sha(&initial_evidence);
        let mut packet = CertifiedReceipt {
            source_recipe: WorkerExecutionSource::Ordinary,
            finalization: fixture_finalization(Some(root.path()), &vec![accepted.clone()]),
            modules: vec![accepted],
            targets: BTreeMap::new(),
            packages: BTreeMap::new(),
        };
        let initial = certify_products(
            None,
            &packet,
            &parsed,
            &initial_evidence,
            &initial_input,
            root.path(),
            &normalized,
            source,
            &producer,
            &include,
            None,
            None,
        )
        .unwrap();
        let mut prior = vec![initial.recovery_products[0].clone()];
        let mut context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products(canonical_producer, &prior)
            .unwrap();

        let native = prior[0].original_native().unwrap();
        let valid = parsed.products()[0]
            .groups
            .iter()
            .map(|group| (group, vec![]))
            .collect::<Vec<_>>();
        native.validate_promoted_groups(&valid).unwrap();
        assert!(matches!(native.validate_promoted_groups(&valid[..1]),
            Err(CertificationError::OriginalGroupConflict(conflict))
                if conflict.owner == *prior[0].owner()
                    && conflict.failure == (OriginalGroupFailure::PromotionCensus { current: 1, original: 2 })
        ));
        let duplicate_ordinal = vec![valid[0].clone(), valid[0].clone()];
        assert!(
            matches!(native.validate_promoted_groups(&duplicate_ordinal),
                Err(CertificationError::OriginalGroupConflict(conflict))
                    if conflict.failure == (OriginalGroupFailure::PromotionDuplicateOrdinal { ordinal: 7 })
            )
        );
        // The first collided binder can agree while a later group's body or
        // import history differs. Sharing requires the entire native witness.
        let changed =
            ParsedModuleProducts::decode(&nonempty_sidecar(1), &empty_package_bundle()).unwrap();
        let mut wrong_body = valid.clone();
        wrong_body[1].0 = &changed.products()[0].groups[1];
        assert!(matches!(native.validate_promoted_groups(&wrong_body),
            Err(CertificationError::OriginalGroupConflict(conflict))
                if conflict.failure == (OriginalGroupFailure::PromotionBody { ordinal: 11 })
        ));
        let mut wrong_import = valid.clone();
        wrong_import[1].1.push(PendingImportOwner::Retained {
            identity: testing::identity("Val.G1", "captured"),
            generation: 1,
        });
        assert!(matches!(native.validate_promoted_groups(&wrong_import),
            Err(CertificationError::OriginalGroupConflict(conflict))
                if conflict.failure == (OriginalGroupFailure::PromotionImports { ordinal: 11 })
        ));
        let canonical = prior[0].module_interface().unwrap().clone();
        packet.modules[0].origin = ProductOrigin::RetainedCore;
        packet.modules[0].module_version = None;
        packet.modules[0].dependency_witness_sha256 = sha(canonical.certificate_bytes());
        packet.finalization = fixture_finalization(None, &packet.modules);
        packet.source_recipe =
            WorkerExecutionSource::ExactUnavailable(SourceRecipeUnavailable::NoFreshOriginals);
        let mut expected_owners = BTreeSet::from([prior[0].owner().clone()]);
        let mut first_promoted = None;
        // Current source compilation owns Target only. Fresh is emitted from
        // authenticated retained Core, without a fresh source observation.
        let mut current_evidence = evidence(source);
        current_evidence.modules[0].module = "Target".into();
        let normalized =
            CompletedSourceEvidence::from_normalized(current_evidence.clone(), source).unwrap();
        for (index, body_tag) in [0, 0, 1, 0].into_iter().enumerate() {
            let projection = context.compiler_input_projection().interface_only();
            context = context.with_compiler_input_projection(projection).unwrap();
            let directory = root.path().join(format!("promotion-{index}"));
            std::fs::create_dir(&directory).unwrap();
            let input = directory.join("Target.hs");
            std::fs::write(&input, source).unwrap();
            let request = Arc::new(context.clone())
                .prepare_compilation(&directory.join("inputs"), &producer)
                .unwrap();
            assert!(request.compiler_original_products().unwrap().is_empty());
            assert_eq!(
                request.context.recovery_products().len(),
                expected_owners.len()
            );
            let mut worker = current_evidence.clone();
            worker.sources[0].path = input.clone();
            let receipt_root = directory.join(".exact-compilations/current");
            std::fs::create_dir_all(&receipt_root).unwrap();
            let snapshot = receipt_root.join("source.hs");
            std::fs::write(&snapshot, source).unwrap();
            let exact_receipt = value_array([
                value_text("TPEXACTCOMPILE"),
                value_text("3"),
                value_text(&request.request_sha256),
                value_text(hex(&request.semantic_sha256)),
                value_text(input.to_string_lossy()),
                value_text(hex(&sha(source.as_bytes()))),
                value_text(snapshot.to_string_lossy()),
                value_text(String::from_utf8(serde_json::to_vec(&worker).unwrap()).unwrap()),
                value_array([value_array([
                    value_text("main"),
                    value_text("Target"),
                    Value::Bool(false),
                    value_array([]),
                ])]),
                value_array([value_array([]), Value::Null]),
            ]);
            std::fs::write(
                receipt_root.join("receipt.cbor"),
                receipt_bytes(&exact_receipt),
            )
            .unwrap();
            worker.cache_safe = false;
            worker.selection_complete = false;
            let evidence_bytes = serde_json::to_vec(&worker).unwrap();
            let admission = request
                .admit_source(&input, source, &evidence_bytes)
                .unwrap();
            let exact = ExactProductAdmission {
                request: &request,
                source: &admission,
            };
            let bytes = nonempty_sidecar(body_tag);
            let parsed = ParsedModuleProducts::decode(&bytes, &empty_package_bundle()).unwrap();
            packet.modules[0].product_sha256 = sha(&bytes);
            // Independent finite graph oracle: this fixture has two complete groups
            // and no imports. Neither membership nor the production graph builder
            // supplies the expected owner or group census.
            let package_bytes =
                &parsed.package_imports.as_ref().unwrap()[&("main".into(), "Fresh".into())];
            let graph =
                value_array([value_array([
                    value_text("main"),
                    value_text("Fresh"),
                    value_text(hex(&sha(canonical.certificate_bytes()))),
                    value_text(hex(&sha(&parsed.sidecars[0]))),
                    value_text(hex(&sha(package_bytes))),
                    value_array([7_u32, 11].into_iter().map(|ordinal| {
                        value_array([Value::Integer(ordinal.into()), value_array([])])
                    })),
                ])]);
            let graph_bytes = receipt_bytes(&graph);
            let mut hash = Sha256::new();
            for field in [
                b"retained-core-home-v1".as_slice(),
                b"main",
                b"Fresh",
                graph_bytes.as_slice(),
            ] {
                hash.update((field.len() as u64).to_be_bytes());
                hash.update(field);
            }
            let owner = CachedHomeOwner {
                unit: "main".into(),
                module: "Fresh".into(),
                module_version: ModuleVersion(hash.finalize().into()),
                skinny_iface_sha256: sha(&[0x42]),
                product_sha256: sha(&parsed.sidecars[0]),
            };
            if body_tag == 0 {
                if let Some(first) = &first_promoted {
                    assert_eq!(&owner, first);
                } else {
                    first_promoted = Some(owner.clone());
                }
            } else {
                assert_ne!(Some(&owner), first_promoted.as_ref());
            }
            let issued = certify_products(
                None,
                &packet,
                &parsed,
                &evidence_bytes,
                &input,
                root.path(),
                &normalized,
                source,
                &producer,
                &include,
                Some(&exact),
                None,
            )
            .unwrap_or_else(|error| {
                panic!("retained promotion iteration={index} body_tag={body_tag} operation=certify_products request_groups={}: {error:?}", request.groups.len())
            });
            assert_eq!(
                issued
                    .source_selection
                    .selected_original_owners()
                    .collect::<Vec<_>>(),
                vec![&owner]
            );
            let current = issued
                .recovery_products
                .iter()
                .find(|product| product.owner() == &owner)
                .unwrap();
            assert_eq!(current.original_native().unwrap().groups.len(), 2);
            assert!(issued.retained_core_products.contains_original(current));
            let selected = issued
                .groups
                .iter()
                .filter(|group| group.owner() == &owner)
                .collect::<Vec<_>>();
            assert_eq!(selected.len(), 2);
            for (group, ordinal) in selected.iter().zip([7, 11]) {
                assert_eq!(group.origin, ProductOrigin::RetainedCore);
                assert_eq!(group.group().original_ordinal(), ordinal);
                assert_eq!(
                    group.group(),
                    &parsed.products()[0]
                        .groups
                        .iter()
                        .find(|raw| raw.original_ordinal() == ordinal)
                        .unwrap()
                        .clone()
                );
                assert!(group.imports().is_empty());
            }
            for old in &prior {
                let retained = issued
                    .recovery_products
                    .iter()
                    .find(|product| product.owner() == old.owner())
                    .unwrap();
                assert!(retained
                    .original_byte_anchors()
                    .iter()
                    .zip(old.original_byte_anchors())
                    .all(|(actual, expected)| Arc::ptr_eq(actual, expected)));
            }
            expected_owners.insert(owner.clone());
            assert_eq!(
                issued
                    .recovery_products
                    .iter()
                    .map(|product| product.owner().clone())
                    .collect::<BTreeSet<_>>(),
                expected_owners
            );
            let expected_keys = expected_owners
                .iter()
                .flat_map(|owner| {
                    [7, 11].map(|ordinal| {
                        (
                            owner.clone(),
                            ordinal,
                            SymbolIdentity {
                                unit: "main".into(),
                                module: "Fresh".into(),
                                namespace: "value".into(),
                                occurrence: format!("entry_{ordinal}"),
                                record_parent: None,
                            },
                        )
                    })
                })
                .collect::<BTreeSet<_>>();
            for count in 0..=2 {
                let subset = selected
                    .iter()
                    .take(count)
                    .map(|group| (*group).clone())
                    .collect::<Vec<_>>();
                let available = available_original_source_map(
                    &issued.recovery_products,
                    &subset,
                    &InventoryOperation::new(Default::default()),
                )
                .unwrap();
                assert_eq!(
                    available.groups.keys().cloned().collect::<BTreeSet<_>>(),
                    expected_keys
                );
            }
            let mut duplicate = packet.clone();
            let duplicated = duplicate.modules[0].groups[0].clone();
            duplicate.modules[0].groups.push(duplicated);
            assert!(certify_products(
                None,
                &duplicate,
                &parsed,
                &evidence_bytes,
                &input,
                root.path(),
                &normalized,
                source,
                &producer,
                &include,
                Some(&exact),
                None
            )
            .is_err());
            let mut wrong_canonical = packet.clone();
            wrong_canonical.modules[0].dependency_witness_sha256[0] ^= 1;
            assert!(certify_products(
                None,
                &wrong_canonical,
                &parsed,
                &evidence_bytes,
                &input,
                root.path(),
                &normalized,
                source,
                &producer,
                &include,
                Some(&exact),
                None
            )
            .is_err());
            context = context
                .extend_checked_original_products(canonical_producer, std::slice::from_ref(current))
                .unwrap();
            prior = context.recovery_products();
        }
        assert_eq!(expected_owners.len(), 3);
    }

    fn exact_current_original_history(nonempty: bool) {
        use crate::artifact_inventory::{ArtifactInventory, CanonicalProducerIdentity};
        use crate::declaration_context::{
            ExactDeclarationContext, ExactProductAdmission, OriginalCompilerInputs,
        };
        use crate::execution_source::{
            CertifiedExecutionSourceGraph, ExecutionSourceAdmission, ExecutionSourceGraphInput,
        };
        let root = tempfile::tempdir().unwrap();
        struct RestoreCache(Option<std::ffi::OsString>);
        impl Drop for RestoreCache {
            fn drop(&mut self) {
                unsafe {
                    match self.0.take() {
                        Some(value) => std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", value),
                        None => std::env::remove_var("TIDEPOOL_COMPILE_CACHE_DIR"),
                    }
                }
            }
        }
        let _cache = nonempty.then(|| {
            let previous = std::env::var_os("TIDEPOOL_COMPILE_CACHE_DIR");
            unsafe {
                std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", root.path());
            }
            RestoreCache(previous)
        });
        let source = "module Target where";
        let support_source = "module Fresh where";
        let support = root.path().join("Fresh.hs");
        std::fs::write(&support, support_source).unwrap();
        let mut admitted = evidence(support_source);
        admitted.sources[0].path = support.clone();
        admitted.modules[0].source = support;
        admitted.sources.push(SourceEvidence {
            path: "@generated-source".into(),
            sha256: hex(&sha(source.as_bytes())),
        });
        let producer = [3; 32];
        let canonical_producer = CanonicalProducerIdentity::from_producer_bytes(&producer).sha256();
        let include = [root.path().to_path_buf()];
        let bytes = if nonempty {
            nonempty_sidecar(0)
        } else {
            sidecar()
        };
        let parsed = ParsedModuleProducts::decode(&bytes, &empty_package_bundle()).unwrap();
        let normalized =
            CompletedSourceEvidence::from_normalized(admitted.clone(), source).unwrap();
        let initial_input = root.path().join("Target.hs");
        std::fs::write(&initial_input, source).unwrap();
        let mut worker = admitted.clone();
        worker.sources[1].path = initial_input.clone();
        let initial_evidence = serde_json::to_vec(&worker).unwrap();
        let mut accepted = receipt(&bytes, &admitted, support_source);
        if nonempty {
            accepted.groups = [7, 11]
                .into_iter()
                .map(|original_ordinal| AcceptedGroup {
                    original_ordinal,
                    globals: vec![],
                })
                .collect();
            assert_eq!(
                parsed.products()[0]
                    .groups
                    .iter()
                    .map(|group| group.binders().len())
                    .sum::<usize>(),
                2,
            );
        }
        accepted.dependency_witness_sha256 = sha(&initial_evidence);
        let mut packet = CertifiedReceipt {
            source_recipe: WorkerExecutionSource::Ordinary,
            finalization: fixture_finalization(Some(root.path()), &vec![accepted.clone()]),
            modules: vec![accepted],
            targets: BTreeMap::new(),
            packages: BTreeMap::new(),
        };
        let initial = certify_products(
            None,
            &packet,
            &parsed,
            &initial_evidence,
            &initial_input,
            root.path(),
            &normalized,
            source,
            &producer,
            &include,
            None,
            None,
        )
        .unwrap();
        let mut prior = vec![initial.recovery_products[0].clone()];
        let mut context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products(canonical_producer, &prior)
            .unwrap();
        for index in 0..2 {
            let projection = context.compiler_input_projection().interface_only();
            context = context.with_compiler_input_projection(projection).unwrap();
            assert_eq!(context.recovery_products().len(), prior.len());
            let directory = root.path().join(format!("request-{index}"));
            std::fs::create_dir(&directory).unwrap();
            let input = directory.join("Target.hs");
            std::fs::write(&input, source).unwrap();
            // An unrelated, genuinely certified public owner changes declaration
            // identity without exposing Fresh in the public source scope.
            let marker = format!("Marker{index}");
            let marker_source = format!("module {marker} where");
            let marker_input = directory.join(format!("{marker}.hs"));
            std::fs::write(&marker_input, &marker_source).unwrap();
            let marker_bytes =
                tidepool_test_data::prepared_encode::encode_module_products(&[RawModuleProduct {
                    unit: "main".into(),
                    module: marker.clone(),
                    interface: vec![0x42],
                    groups: vec![],
                }]);
            let marker_parsed =
                ParsedModuleProducts::decode(&marker_bytes, &empty_package_bundle_for(&marker))
                    .unwrap();
            let mut marker_evidence = evidence(&marker_source);
            marker_evidence.modules[0].module = marker.clone();
            let mut marker_worker = marker_evidence.clone();
            marker_worker.sources[0].path = marker_input.clone();
            marker_worker.modules[0].source = marker_input.clone();
            let marker_worker = serde_json::to_vec(&marker_worker).unwrap();
            let mut marker_receipt = receipt(&marker_bytes, &marker_evidence, &marker_source);
            marker_receipt.module = marker;
            marker_receipt.dependency_witness_sha256 = sha(&marker_worker);
            let marker_packet = CertifiedReceipt {
                source_recipe: WorkerExecutionSource::Ordinary,
                finalization: fixture_finalization(Some(root.path()), &[marker_receipt.clone()]),
                modules: vec![marker_receipt],
                targets: BTreeMap::new(),
                packages: BTreeMap::new(),
            };
            let marker_certified = certify_products(
                None,
                &marker_packet,
                &marker_parsed,
                &marker_worker,
                &marker_input,
                root.path(),
                &CompletedSourceEvidence::from_normalized(marker_evidence, &marker_source).unwrap(),
                &marker_source,
                &producer,
                &include,
                None,
                None,
            )
            .unwrap();
            let marker_context = ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products(
                    canonical_producer,
                    &marker_certified.recovery_products,
                )
                .unwrap();
            let public_context = Arc::new(
                ExactDeclarationContext::new(&[], &[], vec![])
                    .unwrap()
                    .extend_interface_artifacts(marker_context.artifact_view())
                    .unwrap(),
            );
            assert!(public_context.recovery_products().is_empty());
            assert!(public_context
                .interface_owners()
                .iter()
                .all(|entry| entry.owner.module != "Fresh"));
            // Full original custody and executable group selection are independent.
            let inventory = ArtifactInventory::default();
            let private_view = inventory
                .admit_recovery_selection(
                    &inventory.empty_view(),
                    context.artifact_view().entries(),
                    &BTreeSet::new(),
                )
                .unwrap();
            let selection = CertifiedSourceSelection::from_compiler_projection(
                context.compiler_input_projection(),
                &private_view.metadata_snapshot(),
                &InventoryOperation::new(Default::default()),
            )
            .unwrap();
            let private_input =
                OriginalCompilerInputs::from_selection(&selection, &private_view).unwrap();
            let request = public_context
                .prepare_compilation(&directory.join("inputs"), &producer)
                .unwrap()
                .in_program_context_with_private_input(
                    &directory.join("private-inputs"),
                    public_context.clone(),
                    &private_input,
                )
                .unwrap();
            assert!(request.groups.is_empty());
            assert!(request
                .compiler_inputs()
                .unwrap()
                .metadata
                .selected_native_groups
                .is_empty());
            assert!(request.compiler_original_products().unwrap().is_empty());
            let effective = request.compiler_inputs().unwrap();
            assert_eq!(
                effective
                    .metadata
                    .artifacts
                    .values()
                    .filter_map(|entry| match &entry.payload {
                        crate::artifact_inventory::ArtifactPayload::Original(product) =>
                            Some(product.owner().clone()),
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>(),
                prior
                    .iter()
                    .map(|product| product.owner().clone())
                    .collect::<BTreeSet<_>>(),
            );
            let mut worker = admitted.clone();
            worker.sources[1].path = input.clone();
            let complete_evidence = serde_json::to_vec(&worker).unwrap();
            let receipt_root = directory.join(".exact-compilations/current");
            std::fs::create_dir_all(&receipt_root).unwrap();
            let snapshot = receipt_root.join("source.hs");
            std::fs::write(&snapshot, source).unwrap();
            let exact_receipt = Value::Array(vec![
                Value::Text("TPEXACTCOMPILE".into()),
                Value::Text("3".into()),
                Value::Text(request.request_sha256.clone()),
                Value::Text(hex(&request.semantic_sha256)),
                Value::Text(input.to_string_lossy().into_owned()),
                Value::Text(hex(&sha(source.as_bytes()))),
                Value::Text(snapshot.to_string_lossy().into_owned()),
                Value::Text(String::from_utf8(complete_evidence).unwrap()),
                Value::Array(vec![Value::Array(vec![
                    Value::Text("main".into()),
                    Value::Text("Fresh".into()),
                    Value::Bool(false),
                    Value::Array(vec![]),
                ])]),
                Value::Array(vec![Value::Array(vec![]), Value::Null]),
            ]);
            std::fs::write(
                receipt_root.join("receipt.cbor"),
                receipt_bytes(&exact_receipt),
            )
            .unwrap();
            worker.cache_safe = false;
            worker.selection_complete = false;
            let evidence_bytes = serde_json::to_vec(&worker).unwrap();
            packet.modules[0].dependency_witness_sha256 = sha(&evidence_bytes);
            let source_admission = request
                .admit_source(&input, source, &evidence_bytes)
                .unwrap();
            let exact = ExactProductAdmission {
                request: &request,
                source: &source_admission,
            };
            let native = &parsed.sidecars[0];
            let package_bytes =
                &parsed.package_imports.as_ref().unwrap()[&("main".into(), "Fresh".into())];
            let owner = CachedHomeOwner {
                unit: "main".into(),
                module: "Fresh".into(),
                module_version: crate::module_candidates::exact_module_version_for_product(
                    &producer,
                    &request.semantic_sha256,
                    "main",
                    "Fresh",
                    &packet.modules[0].source_sha256,
                    &parsed.products()[0].interface,
                    native,
                    package_bytes,
                ),
                skinny_iface_sha256: packet.modules[0].skinny_iface_sha256,
                product_sha256: sha(native),
            };
            assert!(prior.iter().all(|product| product.owner() != &owner));
            let owners = [owner.clone()];
            let fresh = BTreeSet::from([crate::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: "Fresh".into(),
            }]);
            let ExecutionSourceAdmission::Available(issued) =
                CertifiedExecutionSourceGraph::admit(ExecutionSourceGraphInput {
                    producer: CanonicalProducerIdentity::from_producer_bytes(&producer),
                    semantic_sha256: Some(request.semantic_sha256),
                    include: &include,
                    source_path: &input,
                    source,
                    evidence: &admitted,
                    exact_imports: &source_admission.exact_imports,
                    owners: &owners,
                    fresh_owners: &fresh,
                    retained_sources: &BTreeMap::new(),
                    packages: &BTreeMap::new(),
                })
                .unwrap()
            else {
                panic!("complete current source recipe")
            };
            packet.source_recipe = WorkerExecutionSource::ExactAvailable {
                digest: issued.digest(),
                bytes: issued.bytes().into(),
            };
            let certified = certify_products(
                None,
                &packet,
                &parsed,
                &evidence_bytes,
                &input,
                root.path(),
                &normalized,
                source,
                &producer,
                &include,
                Some(&exact),
                None,
            )
            .unwrap();
            let expected_custody = prior
                .iter()
                .map(|product| product.owner().clone())
                .chain(std::iter::once(owner.clone()))
                .collect::<BTreeSet<_>>();
            assert_eq!(
                certified
                    .recovery_products
                    .iter()
                    .map(|product| product.owner().clone())
                    .collect::<BTreeSet<_>>(),
                expected_custody,
                "private original custody lost: iteration={index}, nonempty={nonempty}"
            );
            let current = certified
                .recovery_products
                .iter()
                .find(|product| product.owner() == &owner)
                .unwrap();
            assert_eq!(current.execution_source().unwrap().bytes(), issued.bytes());
            assert_eq!(
                certified
                    .source_selection
                    .selected_original_owners()
                    .collect::<Vec<_>>(),
                vec![&owner]
            );
            if nonempty {
                let expected_owners = prior
                    .iter()
                    .map(|product| product.owner().clone())
                    .chain(std::iter::once(owner.clone()))
                    .collect::<BTreeSet<_>>();
                assert_eq!(expected_owners.len(), index + 2);
                assert_eq!(certified.groups.len(), 2);
                for group in &certified.groups {
                    assert_eq!(group.owner(), &owner);
                    let raw = parsed.products()[0]
                        .groups
                        .iter()
                        .find(|raw| raw.original_ordinal() == group.group().original_ordinal())
                        .unwrap();
                    assert_eq!(group.group(), raw);
                    assert!(group.imports().is_empty());
                }
                let expected_keys = expected_owners
                    .iter()
                    .flat_map(|owner| {
                        [7, 11].map(|ordinal| {
                            (
                                owner.clone(),
                                ordinal,
                                SymbolIdentity {
                                    unit: "main".into(),
                                    module: "Fresh".into(),
                                    namespace: "value".into(),
                                    occurrence: format!("entry_{ordinal}"),
                                    record_parent: None,
                                },
                            )
                        })
                    })
                    .collect::<BTreeSet<_>>();
                for selected_count in 0..=2 {
                    let sources = available_original_source_map(
                        &certified.recovery_products,
                        &certified.groups[..selected_count],
                        &InventoryOperation::new(Default::default()),
                    )
                    .unwrap();
                    assert_eq!(
                        sources.groups.keys().cloned().collect::<BTreeSet<_>>(),
                        expected_keys
                    );
                    let binder = SymbolIdentity {
                        unit: "main".into(),
                        module: "Fresh".into(),
                        namespace: "value".into(),
                        occurrence: "entry_11".into(),
                        record_parent: None,
                    };
                    let import = ReceiptImportOwner::Source {
                        unit: "main".into(),
                        module: "Fresh".into(),
                        module_version: None,
                        original_ordinal: 11,
                        binder: binder.clone(),
                    };
                    assert_eq!(
                        resolve_receipt_owner_with_validation(
                            import.clone(),
                            &sources,
                            Some(&certified.source_selection),
                            &BTreeMap::new(),
                            &mut PackageInterfaceValidation::default(),
                        )
                        .unwrap(),
                        PendingImportOwner::Source {
                            owner: owner.clone(),
                            original_ordinal: 11,
                            binder: binder.clone(),
                        }
                    );
                    let mut old_import = import;
                    let ReceiptImportOwner::Source { module_version, .. } = &mut old_import else {
                        unreachable!()
                    };
                    *module_version = Some(prior[0].owner().module_version.clone());
                    assert!(resolve_receipt_owner_with_validation(
                        old_import.clone(),
                        &sources,
                        Some(&certified.source_selection),
                        &BTreeMap::new(),
                        &mut PackageInterfaceValidation::default(),
                    )
                    .is_err());
                    assert_eq!(
                        resolve_receipt_owner(old_import, &sources, &BTreeMap::new()).unwrap(),
                        PendingImportOwner::Source {
                            owner: prior[0].owner().clone(),
                            original_ordinal: 11,
                            binder,
                        }
                    );
                }
                let substituted =
                    ParsedModuleProducts::decode(&nonempty_sidecar(1), &empty_package_bundle())
                        .unwrap();
                let mut wrong = certified.groups[0].clone();
                wrong.group = Arc::new(substituted.products()[0].groups[0].clone());
                assert!(matches!(
                    available_original_source_map(
                        &certified.recovery_products,
                        &[wrong],
                        &InventoryOperation::new(Default::default()),
                    ),
                    Err(CertificationError::Mismatch("shared original home groups"))
                ));
            }
            for old in &prior {
                let retained = certified
                    .recovery_products
                    .iter()
                    .find(|product| product.owner() == old.owner())
                    .unwrap();
                if nonempty {
                    let native = retained.original_native().unwrap();
                    assert_eq!(native.groups.len(), 2);
                    for (original, raw) in native.groups.iter().zip(&parsed.products()[0].groups) {
                        assert_eq!(original.owner, *old.owner());
                        assert_eq!(original.group.as_ref(), raw);
                        assert!(original.imports.is_empty());
                    }
                }
                assert!(old
                    .original_byte_anchors()
                    .iter()
                    .zip(retained.original_byte_anchors())
                    .all(|(old, current)| Arc::ptr_eq(old, current)));
                assert!(Arc::ptr_eq(
                    old.execution_source().unwrap(),
                    retained.execution_source().unwrap()
                ));
            }
            let projection = context.compiler_input_projection().interface_only();
            context = context
                .with_compiler_input_projection(projection)
                .unwrap()
                .extend_checked_original_products(canonical_producer, std::slice::from_ref(current))
                .unwrap();
            prior.push(current.clone());
            if nonempty {
                use crate::module_candidates::{
                    prepare_publication, publish_prepared, select, CandidateVersionOrigin,
                };
                unsafe {
                    std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", directory.join("cache"));
                }
                let (_, publication) = prepare_publication(
                    &producer,
                    &include,
                    &admitted,
                    parsed.copy_for_publication().unwrap(),
                    source,
                    CandidateVersionOrigin::Exact {
                        semantic_sha256: request.semantic_sha256,
                    },
                    &certified.recovery_products,
                );
                publish_prepared(publication);
                let candidate =
                    select(&producer, &include, &directory.join("cached-offer")).unwrap();
                let bundle = &candidate.by_owner[&("main".into(), "Fresh".into())];
                assert_eq!(bundle.owner, owner);
                let inventory = ArtifactInventory::default();
                let mut private_entries = request.compiler_inputs().unwrap().artifacts.entries();
                private_entries.extend(context.artifact_view().entries());
                let private_view = inventory
                    .admit_recovery_selection(
                        &inventory.empty_view(),
                        private_entries,
                        &BTreeSet::new(),
                    )
                    .unwrap();
                let private_input = OriginalCompilerInputs::from_selection(
                    &certified.source_selection,
                    &private_view,
                )
                .unwrap();
                let cached_request = public_context
                    .prepare_compilation(&directory.join("cached-inputs"), &producer)
                    .unwrap()
                    .in_program_context_with_private_input(
                        &directory.join("cached-private-inputs"),
                        public_context.clone(),
                        &private_input,
                    )
                    .unwrap();
                assert!(cached_request.groups.is_empty());
                assert!(cached_request
                    .compiler_inputs()
                    .unwrap()
                    .metadata
                    .selected_native_groups
                    .is_empty());
                let mut cached_complete = admitted.clone();
                cached_complete.sources[0].path = bundle.source.clone();
                cached_complete.modules[0].source = bundle.source.clone();
                let mut cached_worker = cached_complete.clone();
                cached_worker.sources[1].path = input.clone();
                let mut cached_exact = exact_receipt.clone();
                let fields = cached_exact.as_array_mut().unwrap();
                fields[2] = value_text(&cached_request.request_sha256);
                fields[3] = value_text(hex(&cached_request.semantic_sha256));
                fields[7] = value_text(serde_json::to_string(&cached_worker).unwrap());
                std::fs::write(
                    receipt_root.join("receipt.cbor"),
                    receipt_bytes(&cached_exact),
                )
                .unwrap();
                cached_worker.cache_safe = false;
                cached_worker.selection_complete = false;
                let cached_evidence = serde_json::to_vec(&cached_worker).unwrap();
                let cached_source = cached_request
                    .admit_source(&input, source, &cached_evidence)
                    .unwrap();
                let cached_exact = ExactProductAdmission {
                    request: &cached_request,
                    source: &cached_source,
                };
                let mut cached_packet = packet.clone();
                cached_packet.modules[0].origin = ProductOrigin::Cached;
                cached_packet.modules[0].module_version = Some(owner.module_version.clone());
                cached_packet.modules[0].product_sha256 = owner.product_sha256;
                cached_packet.modules[0].dependency_witness_sha256 =
                    sha(&serde_json::to_vec(&bundle.evidence).unwrap());
                cached_packet.source_recipe = WorkerExecutionSource::ExactUnavailable(
                    SourceRecipeUnavailable::NoFreshOriginals,
                );
                cached_packet.finalization =
                    fixture_finalization(Some(root.path()), &cached_packet.modules);
                let empty = ParsedModuleProducts::decode(
                    &receipt_bytes(&value_array([
                        value_text("TPMOD"),
                        Value::Integer(1.into()),
                        value_array([]),
                    ])),
                    &receipt_bytes(&value_array([
                        value_text("TPPKGBUNDLES"),
                        Value::Integer(1.into()),
                        value_array([]),
                    ])),
                )
                .unwrap();
                let complete =
                    CompletedSourceEvidence::from_normalized(cached_complete, source).unwrap();
                let certify_cached = |packet: &CertifiedReceipt| {
                    certify_products(
                        Some(&candidate),
                        packet,
                        &empty,
                        &cached_evidence,
                        &input,
                        root.path(),
                        &complete,
                        source,
                        &producer,
                        &include,
                        Some(&cached_exact),
                        None,
                    )
                };
                let cached = certify_cached(&cached_packet).unwrap();
                assert_eq!(
                    cached
                        .source_selection
                        .selected_original_owners()
                        .collect::<Vec<_>>(),
                    vec![&owner]
                );
                assert_eq!(cached.groups.len(), 2);
                for (group, raw) in cached.groups.iter().zip(&parsed.products()[0].groups) {
                    assert_eq!(group.owner(), &owner);
                    assert_eq!(group.group(), raw);
                    assert!(group.imports().is_empty());
                }
                assert_eq!(
                    cached
                        .recovery_products
                        .iter()
                        .map(|product| product.owner().clone())
                        .collect::<BTreeSet<_>>(),
                    prior
                        .iter()
                        .map(|product| product.owner().clone())
                        .collect::<BTreeSet<_>>()
                );
                for old in &prior {
                    let retained = cached
                        .recovery_products
                        .iter()
                        .find(|product| product.owner() == old.owner())
                        .unwrap();
                    assert!(old
                        .original_byte_anchors()
                        .iter()
                        .zip(retained.original_byte_anchors())
                        .all(|(old, new)| Arc::ptr_eq(old, new)));
                    assert!(Arc::ptr_eq(
                        old.execution_source().unwrap(),
                        retained.execution_source().unwrap()
                    ));
                }
                let mut wrong_owner = cached_packet.clone();
                wrong_owner.modules[0].module_version =
                    Some(prior[0].owner().module_version.clone());
                assert!(certify_cached(&wrong_owner).is_err());
                assert_eq!(
                    cached_request.compiler_original_products().unwrap()[0].owner(),
                    &owner
                );
            }
        }
        assert_eq!(prior.len(), 3);
    }

    #[test]
    fn fresh_authored_support_issues_sealed_execution_provenance() {
        use crate::artifact_inventory::CanonicalProducerIdentity;
        use crate::declaration_context::ExactDeclarationContext;
        let directory = tempfile::tempdir().unwrap();
        let source = "module Target where";
        let support_source = "module Fresh where";
        let source_directory = tempfile::tempdir().unwrap();
        let input = source_directory.path().join("Target.hs");
        let support = directory.path().join("Fresh.hs");
        std::fs::write(&input, source).unwrap();
        std::fs::write(&support, support_source).unwrap();
        let mut admitted = evidence(support_source);
        admitted.sources[0].path = support.clone();
        admitted.modules[0].source = support;
        admitted.sources.push(SourceEvidence {
            path: "@generated-source".into(),
            sha256: hex(&sha(source.as_bytes())),
        });
        let mut worker_evidence = admitted.clone();
        worker_evidence.sources[1].path = input.clone();
        let raw_evidence = serde_json::to_vec(&worker_evidence).unwrap();
        let bytes = sidecar();
        let parsed = ParsedModuleProducts::decode(&bytes, &empty_package_bundle()).unwrap();
        // The endpoint supplies a stable 32-byte producer identity. Exact
        // artifacts bind its canonical SHA-256, as prepare_compilation does.
        let producer = [3; 32];
        let canonical_producer = CanonicalProducerIdentity::from_producer_bytes(&producer).sha256();
        let baseline = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products(canonical_producer, &[])
                .unwrap(),
        );
        let request = baseline
            .prepare_compilation(&directory.path().join("initial-inputs"), &producer)
            .unwrap();
        assert_eq!(request.producer_sha256, canonical_producer);
        let mut accepted = receipt(&bytes, &admitted, support_source);
        accepted.dependency_witness_sha256 = sha(&raw_evidence);
        let certified = certify_products(
            None,
            &CertifiedReceipt {
                source_recipe: WorkerExecutionSource::Ordinary,
                finalization: fixture_finalization(Some(directory.path()), &vec![accepted.clone()]),
                modules: vec![accepted],
                targets: BTreeMap::new(),
                packages: BTreeMap::new(),
            },
            &parsed,
            &raw_evidence,
            &input,
            directory.path(),
            &CompletedSourceEvidence::from_normalized(admitted.clone(), source).unwrap(),
            source,
            &producer,
            &[directory.path().to_path_buf()],
            None,
            None,
        )
        .unwrap();
        let original = &certified.recovery_products[0];
        let graph = original.execution_source().unwrap();
        assert_eq!(graph.producer_sha256(), request.producer_sha256);
        assert!(graph.eligible_source_replay_root(original.owner()));
        assert!(original
            .original_native()
            .unwrap()
            .matches_original(original));
        assert_eq!(
            home_execution_source_digest_with_validation(
                original.certification_bytes(),
                original.owner(),
                &mut PackageInterfaceValidation::default(),
            )
            .unwrap(),
            Some(graph.digest())
        );
        let context = Arc::new(
            baseline
                .as_ref()
                .clone()
                .extend_checked_original_products(
                    request.producer_sha256,
                    &certified.recovery_products,
                )
                .unwrap(),
        );
        let effective = request
            .in_program_context(&directory.path().join("program-inputs"), context)
            .unwrap();
        assert_eq!(effective.artifacts.len(), 1);
        effective
            .context
            .validate_artifacts(&effective.artifacts)
            .unwrap();
        std::fs::create_dir(directory.path().join("wrong-producer")).unwrap();
        assert!(
            matches!(crate::recovery_artifacts::materialize_certified_products(
            &directory.path().join("wrong-producer"), producer, &certified.recovery_products,
        ), Err(crate::recovery_artifacts::RecoveryArtifactError::ExecutionSourceProducerMismatch {expected, actual, ..})
            if expected == producer && actual == canonical_producer)
        );
        let references = crate::recovery_artifacts::materialize_certified_products(
            directory.path(),
            canonical_producer,
            &certified.recovery_products,
        )
        .unwrap();
        assert!(references[0].execution_source.is_some());
        std::fs::remove_file(directory.path().join("Fresh.hs")).unwrap();
        assert!(crate::recovery_artifacts::verify_materialized_ref(
            directory.path(),
            &references[0],
        )
        .is_ok());
        let recovered = Arc::new(
            ExactDeclarationContext::capture_recovery(
                directory.path(),
                &references,
                &references
                    .iter()
                    .map(|reference| reference.module_interface.clone().unwrap())
                    .collect::<Vec<_>>(),
                &[],
                vec![],
            )
            .unwrap(),
        );
        assert_eq!(recovered.toolchain_identity_sha256(), canonical_producer);
        let recovered_request = recovered
            .prepare_compilation(&directory.path().join("recovered-inputs"), &producer)
            .unwrap();
        assert_eq!(recovered_request.producer_sha256, canonical_producer);
        assert_eq!(
            recovered.recovery_products()[0]
                .execution_source()
                .unwrap()
                .producer_sha256(),
            canonical_producer
        );
    }

    #[test]
    fn fresh_product_accepts_only_canonical_type_only_dependencies() {
        let source = "module Fresh where";
        let types_source = "module Types where";
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("Fresh.hs");
        let types_path = directory.path().join("Types.hs");
        std::fs::write(&input, source).unwrap();
        std::fs::write(&types_path, types_source).unwrap();
        let mut evidence = evidence(source);
        evidence.sources.push(SourceEvidence {
            path: types_path.clone(),
            sha256: hex(&sha(types_source.as_bytes())),
        });
        evidence.modules.push(ModuleEvidence {
            unit: "main".into(),
            module: "Types".into(),
            boot: false,
            source: types_path,
            imports: vec![],
            product: ProductAvailability::InterfaceOnly,
        });
        let mut worker_evidence = evidence.clone();
        worker_evidence.sources[0].path = input.clone();
        worker_evidence.modules[0].source = input.clone();
        let raw_evidence = serde_json::to_vec(&worker_evidence).unwrap();
        let bytes = sidecar();
        let parsed = ParsedModuleProducts::decode(&bytes, &empty_package_bundle()).unwrap();
        let mut accepted = receipt(&bytes, &evidence, source);
        accepted.dependency_witness_sha256 = sha(&raw_evidence);
        accepted
            .interface_requirements
            .insert(("main".into(), "Types".into()), sha(&[0x42]));
        let mut types = receipt(&bytes, &evidence, types_source);
        types.module = "Types".into();
        let mut receipt = CertifiedReceipt {
            source_recipe: WorkerExecutionSource::Ordinary,
            finalization: fixture_finalization(Some(directory.path()), &[accepted.clone(), types]),
            modules: vec![accepted],
            targets: BTreeMap::new(),
            packages: BTreeMap::new(),
        };
        let types_key = ("main".into(), "Types".into());
        receipt
            .finalization
            .modules
            .get_mut(&types_key)
            .unwrap()
            .core = None;
        let certify = |receipt: &CertifiedReceipt| {
            certify_products(
                None,
                receipt,
                &parsed,
                &raw_evidence,
                &input,
                input.parent().unwrap(),
                &CompletedSourceEvidence::from_normalized(evidence.clone(), source).unwrap(),
                source,
                b"producer",
                &[],
                None,
                None,
            )
        };
        let certified = certify(&receipt).unwrap();
        assert_eq!(certified.recovery_products.len(), 1);
        assert_eq!(certified.module_interfaces.len(), 2);
        assert!(certified
            .module_interfaces
            .iter()
            .any(|interface| interface.module() == "Types" && interface.core_bytes().is_none()));
        let mut missing = receipt.clone();
        missing.finalization.modules.remove(&types_key);
        assert!(matches!(
            certify(&missing),
            Err(CertificationError::FinalizedInterfaceRequirement {
                unit, module, required_unit, required_module, expected_sha256,
                selected_sha256: None,
            }) if unit == "main" && module == "Fresh" && required_unit == "main"
                && required_module == "Types" && expected_sha256 == hex(&sha(&[0x42]))
        ));
        let mut changed = receipt.clone();
        changed
            .finalization
            .modules
            .get_mut(&types_key)
            .unwrap()
            .interface
            .sha256 = [9; 32];
        assert!(matches!(
            certify(&changed),
            Err(CertificationError::FinalizedInterfaceRequirement {
                unit, module, required_unit, required_module, expected_sha256,
                selected_sha256: Some(selected),
            }) if unit == "main" && module == "Fresh" && required_unit == "main"
                && required_module == "Types" && expected_sha256 == hex(&sha(&[0x42]))
                && selected == hex(&[9; 32])
        ));
    }

    #[test]
    fn shared_product_publication_copies_are_charged_to_the_same_operation() {
        let body = sidecar();
        let packages = empty_package_bundle();
        let parsed = ParsedModuleProducts::decode(&body, &packages).unwrap();
        let spent = parsed.operation.work_usage().unwrap().0;
        let copy = parsed.copy_for_publication().unwrap();
        assert!(Arc::ptr_eq(&parsed.bytes, &copy.bytes));
        assert!(Arc::ptr_eq(&parsed.package_bundle, &copy.package_bundle));
        assert!(Arc::ptr_eq(&parsed.operation, &copy.operation));
        assert_eq!(parsed.products, copy.products);
        assert_eq!(parsed.sidecars, copy.sidecars);
        assert_eq!(parsed.package_imports, copy.package_imports);
        assert_ne!(parsed.sidecars[0].as_ptr(), copy.sidecars[0].as_ptr());
        assert!(parsed.operation.work_usage().unwrap().0 > spent);
        let remaining = parsed.operation.work_usage().unwrap().1;
        parsed.operation.charge(remaining).unwrap();
        assert!(matches!(
            parsed.copy_for_publication(),
            Err(CertificationError::Product(
                tidepool_repr::execution_schema::ParseError::LimitExceeded("work")
            ))
        ));
        assert_eq!(parsed.operation.work_usage().unwrap().1, 0);
    }

    #[test]
    fn fresh_product_requires_exact_sidecar_and_evidence() {
        let source = "module Fresh where";
        let bytes = sidecar();
        let package_bytes = empty_package_bundle();
        let evidence = evidence(source);
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("Fresh.hs");
        std::fs::write(&input, source).unwrap();
        let mut worker_evidence = evidence.clone();
        worker_evidence.sources[0].path = input.clone();
        worker_evidence.modules[0].source = input.clone();
        let raw_evidence = serde_json::to_vec(&worker_evidence).unwrap();
        let parsed = ParsedModuleProducts::decode(&bytes, &package_bytes).unwrap();
        assert!(ParsedModuleProducts::decode(b"malformed product", b"").is_err());
        let mut accepted = receipt(&bytes, &evidence, source);
        accepted.dependency_witness_sha256 = sha(&raw_evidence);
        let certified = certify_products(
            None,
            &CertifiedReceipt {
                source_recipe: WorkerExecutionSource::Ordinary,
                finalization: fixture_finalization(Some(directory.path()), &vec![accepted.clone()]),
                modules: vec![accepted.clone()],
                targets: BTreeMap::new(),
                packages: BTreeMap::new(),
            },
            &parsed,
            &raw_evidence,
            &input,
            input.parent().unwrap(),
            &CompletedSourceEvidence::from_normalized(evidence.clone(), source).unwrap(),
            source,
            b"producer",
            &[],
            None,
            None,
        )
        .unwrap();
        assert!(certified.groups.is_empty());
        assert_eq!(certified.recovery_products.len(), 1);
        let refs = crate::recovery_artifacts::materialize_certified_products(
            directory.path(),
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(b"producer")
                .sha256(),
            &certified.recovery_products,
        )
        .unwrap();
        let verified =
            crate::recovery_artifacts::verify_materialized_ref(directory.path(), &refs[0]).unwrap();
        assert_eq!(verified.product_bytes, bytes);
        assert!(!verified.package_imports_bytes.is_empty());
        assert_eq!(
            verified.reference.certification_path.extension().unwrap(),
            "owners"
        );
        let mut substituted = evidence.clone();
        substituted.packages.push("unrelated selection".into());
        assert!(matches!(
            certify_products(
                None,
                &CertifiedReceipt {
                    source_recipe: WorkerExecutionSource::Ordinary,
                    finalization: fixture_finalization(
                        Some(directory.path()),
                        &vec![accepted.clone()]
                    ),
                    modules: vec![accepted.clone()],
                    targets: BTreeMap::new(),
                    packages: BTreeMap::new(),
                },
                &parsed,
                &raw_evidence,
                &input,
                input.parent().unwrap(),
                &CompletedSourceEvidence::from_normalized(substituted.clone(), source).unwrap(),
                source,
                b"producer",
                &[],
                None,
                None,
            ),
            Err(CertificationError::Mismatch("fresh evidence bytes"))
        ));
        let mut changed = accepted.clone();
        changed.product_sha256 = [9; 32];
        assert!(matches!(
            certify_products(
                None,
                &CertifiedReceipt {
                    source_recipe: WorkerExecutionSource::Ordinary,
                    finalization: fixture_finalization(
                        Some(directory.path()),
                        &vec![changed.clone()]
                    ),
                    modules: vec![changed],
                    targets: BTreeMap::new(),
                    packages: BTreeMap::new()
                },
                &parsed,
                &raw_evidence,
                &input,
                input.parent().unwrap(),
                &CompletedSourceEvidence::from_normalized(evidence.clone(), source).unwrap(),
                source,
                b"producer",
                &[],
                None,
                None,
            ),
            Err(CertificationError::Mismatch(
                "product/iface/source/evidence digest"
            ))
        ));
        let mut changed = accepted;
        changed.dependency_witness_sha256 = [7; 32];
        assert!(certify_products(
            None,
            &CertifiedReceipt {
                source_recipe: WorkerExecutionSource::Ordinary,
                finalization: fixture_finalization(Some(directory.path()), &vec![changed.clone()]),
                modules: vec![changed],
                targets: BTreeMap::new(),
                packages: BTreeMap::new()
            },
            &parsed,
            &raw_evidence,
            &input,
            input.parent().unwrap(),
            &CompletedSourceEvidence::from_normalized(evidence.clone(), source).unwrap(),
            source,
            b"producer",
            &[],
            None,
            None,
        )
        .is_err());
    }

    #[test]
    fn receipt_dictionary_preserves_full_facts_and_refuses_invalid_references() {
        let mut legacy = empty_legacy_receipt();
        let global = dictionary_test_global();
        let Value::Array(header) = &mut legacy else {
            unreachable!()
        };
        header[3] = value_array([value_array([
            value_text("target"),
            value_array([value_global(&global), value_global(&global)]),
        ])]);
        let compact = dictionary_receipt(&legacy);
        let encoded = receipt_bytes(&compact);
        assert_eq!(
            decode_receipt(&encoded).unwrap().targets["target"],
            vec![global.clone(), global]
        );
        assert!(matches!(
            decode_receipt(&receipt_bytes(&legacy)),
            Err(CertificationError::UnsupportedVersion {
                format: CertificationFormat::ProductReceipt,
                found: 2,
                expected: 9
            })
        ));
        let mut trailing = encoded;
        trailing.push(0);
        assert!(matches!(
            decode_receipt(&trailing),
            Err(CertificationError::Receipt("trailing bytes"))
        ));
        for reference in [
            Value::Integer(1.into()),
            Value::Integer((-1).into()),
            Value::Null,
        ] {
            let mut altered = compact.clone();
            let Value::Array(header) = &mut altered else {
                unreachable!()
            };
            let Value::Array(targets) = &mut header[3] else {
                unreachable!()
            };
            let Value::Array(target) = &mut targets[0] else {
                unreachable!()
            };
            target[1] = value_array([reference]);
            assert!(decode_receipt(&receipt_bytes(&altered)).is_err());
        }
        let mut duplicate = compact.clone();
        let Value::Array(header) = &mut duplicate else {
            unreachable!()
        };
        let Value::Array(rows) = &mut header[5] else {
            unreachable!()
        };
        rows.push(rows[0].clone());
        assert!(matches!(
            decode_receipt(&receipt_bytes(&duplicate)),
            Err(CertificationError::Receipt(
                "duplicate global dictionary row"
            ))
        ));
        let mut unreferenced = compact;
        let Value::Array(header) = &mut unreferenced else {
            unreachable!()
        };
        header[3] = value_array([]);
        assert!(matches!(
            decode_receipt(&receipt_bytes(&unreferenced)),
            Err(CertificationError::Receipt(
                "unreferenced global dictionary row"
            ))
        ));
    }

    #[test]
    fn certification_size_limits_identify_the_owning_format() {
        let receipt = receipt_bytes(&dictionary_receipt(&empty_legacy_receipt()));
        let receipt_limit = receipt.len() - 1;
        let receipt_operation = InventoryOperation::new(InventoryDecodeLimits {
            max_bytes: receipt_limit,
            max_work: 1 << 20,
            ..InventoryDecodeLimits::default()
        });
        assert!(
            matches!(decode_receipt_with_operation(&receipt, None, &receipt_operation), Err(CertificationError::SizeLimit {
            format: CertificationFormat::ProductReceipt, actual, limit: receipt_limit,
        }) if actual == receipt.len())
        );

        let owner = inherited_owner("Original");
        let encoded = encode_home_certification(&owner, &[], &BTreeMap::new()).unwrap();
        let home_limit = encoded.len() - 1;
        let home_operation = InventoryOperation::new(InventoryDecodeLimits {
            max_module_bytes: home_limit,
            max_work: 1 << 20,
            ..InventoryDecodeLimits::default()
        });
        assert!(
            matches!(decode_home_witness_with_operation(&encoded, &home_operation), Err(CertificationError::SizeLimit {
            format: CertificationFormat::HomeOwners, actual, limit: home_limit,
        }) if actual == encoded.len())
        );

        let mut witness = decode_home_witness(&encoded).unwrap();
        witness.owner.module = "large".repeat(32);
        let encode_limit = 128;
        let encode_operation = InventoryOperation::new(InventoryDecodeLimits {
            max_module_bytes: encode_limit,
            max_work: 1 << 20,
            ..InventoryDecodeLimits::default()
        });
        assert!(
            matches!(encode_home_witness_with_operation(&witness, &encode_operation), Err(CertificationError::SizeLimit {
            format: CertificationFormat::HomeOwners, actual, limit: encode_limit,
        }) if actual > encode_limit)
        );
    }

    #[test]
    fn receipt_coordinates_preserve_alias_owners_and_refuse_invalid_references() {
        let mut source = dictionary_test_global();
        source.owner = ReceiptImportOwner::Source {
            unit: "foreign-home".into(),
            module: "AliasOwner".into(),
            module_version: Some(ModuleVersion([3; 32])),
            original_ordinal: 19,
            binder: source.identity.clone(),
        };
        let mut package = source.clone();
        package.owner = ReceiptImportOwner::Package {
            unit: package.identity.unit.clone(),
            module: package.identity.module.clone(),
            binder: package.identity.clone(),
            interface_digest: [4; 32],
        };
        let mut retained_package = package.clone();
        retained_package.owner = ReceiptImportOwner::RetainedPackage {
            unit: package.identity.unit.clone(),
            module: package.identity.module.clone(),
            binder: package.identity.clone(),
            interface_digest: [4; 32],
            generation: 0,
        };
        let expected = vec![source.clone(), package, retained_package, source];
        let mut legacy = empty_legacy_receipt();
        legacy.as_array_mut().unwrap()[3] = value_array([value_array([
            value_text("target"),
            value_array(expected.iter().map(value_global)),
        ])]);
        let compact = dictionary_receipt(&legacy);
        assert_eq!(
            decode_receipt(&receipt_bytes(&compact)).unwrap().targets["target"],
            expected
        );
        let header = array(&compact).unwrap();
        assert_eq!(array(&header[8]).unwrap().len(), 2);
        let source_index = array(&header[5])
            .unwrap()
            .iter()
            .position(|row| {
                string(&array(&array(row).unwrap()[4]).unwrap()[0]).unwrap() == "source"
            })
            .unwrap();
        let source_coordinate = number(
            &array(&array(&array(&header[5]).unwrap()[source_index]).unwrap()[4]).unwrap()[1],
        )
        .unwrap();
        let package_coordinate = 1 - source_coordinate;
        for reference in [
            Value::Integer(2.into()),
            Value::Integer((-1).into()),
            Value::Null,
            Value::Integer(package_coordinate.into()),
        ] {
            let mut invalid = compact.clone();
            invalid.as_array_mut().unwrap()[5].as_array_mut().unwrap()[source_index]
                .as_array_mut()
                .unwrap()[4]
                .as_array_mut()
                .unwrap()[1] = reference;
            assert!(decode_receipt(&receipt_bytes(&invalid)).is_err());
        }
        let mut duplicate = compact.clone();
        let coordinates = duplicate.as_array_mut().unwrap()[8].as_array_mut().unwrap();
        coordinates.push(coordinates[0].clone());
        assert!(matches!(
            decode_receipt(&receipt_bytes(&duplicate)),
            Err(CertificationError::Receipt("duplicate owner coordinate"))
        ));
        let mut unused = compact.clone();
        unused.as_array_mut().unwrap()[8]
            .as_array_mut()
            .unwrap()
            .push(value_array([
                value_text("source"),
                value_text("unused"),
                value_text("Unused"),
                Value::Null,
            ]));
        assert!(matches!(
            decode_receipt(&receipt_bytes(&unused)),
            Err(CertificationError::Receipt("unreferenced owner coordinate"))
        ));
        // Coordinate compression does not authenticate a foreign package or
        // permit its borrowed binder to disagree with the declaration owner.
        let mut foreign = compact;
        let coords = foreign.as_array_mut().unwrap()[8].as_array_mut().unwrap();
        coords[package_coordinate as usize].as_array_mut().unwrap()[1] =
            value_text("foreign-package");
        let foreign = decode_receipt(&receipt_bytes(&foreign)).unwrap();
        let package = &foreign.targets["target"][1];
        let declaration = GlobalDecl {
            identity: package.identity.clone(),
            rep: package.rep,
            entry_signature: None,
            required_evaluated: package.required_evaluated,
            required_generation: None,
        };
        assert!(matches!(
            validate_global_witness(&declaration, &[], package),
            Err(CertificationError::Mismatch("global owner"))
        ));
    }

    #[test]
    fn receipt_coordinates_share_dictionary_reconstruction_work_budget() {
        let package_global = |module: &str| {
            let mut global = dictionary_test_global();
            global.identity.module = module.into();
            global.identity.occurrence = format!("{module}-value");
            global.owner = ReceiptImportOwner::Package {
                unit: "library".into(),
                module: module.into(),
                interface_digest: sha(module.as_bytes()),
                binder: global.identity.clone(),
            };
            global
        };
        let first = package_global("Alpha");
        let second = package_global("Omega");
        let compact_dictionary = |globals: &[AcceptedGlobal]| {
            let full = value_array([
                value_text("TPCERT"),
                Value::Integer(7.into()),
                value_array([]),
                value_array([]),
                value_array([]),
                value_array(globals.iter().map(value_global)),
                Value::Null,
                Value::Null,
            ]);
            let compact = compact_receipt_coordinates(&full);
            let header = array(&compact).unwrap();
            (header[5].clone(), header[8].clone())
        };
        let (first_rows, first_coordinates) = compact_dictionary(std::slice::from_ref(&first));
        let (second_rows, second_coordinates) = compact_dictionary(std::slice::from_ref(&second));
        let decode = |rows: &Value, coordinates: &Value, max_work| {
            let mut coordinates = OwnerCoordinates::decode(coordinates).unwrap();
            let operation = InventoryOperation::new(InventoryDecodeLimits {
                max_work,
                ..InventoryDecodeLimits::default()
            });
            GlobalDictionary::decode_with_operation(rows, &mut coordinates, &operation)
        };

        const SEARCH_CAP: usize = 1 << 20;
        assert!(decode(&first_rows, &first_coordinates, SEARCH_CAP).is_ok());
        let mut low = 0;
        let mut high = SEARCH_CAP;
        while low + 1 < high {
            let middle = low + (high - low) / 2;
            match decode(&first_rows, &first_coordinates, middle) {
                Ok(_) => high = middle,
                Err(CertificationError::Product(
                    tidepool_repr::execution_schema::ParseError::LimitExceeded("work"),
                )) => low = middle,
                Err(error) => panic!("dictionary budget control failed: {error:?}"),
            }
        }
        let operation = InventoryOperation::new(InventoryDecodeLimits {
            max_work: high,
            ..InventoryDecodeLimits::default()
        });
        let mut first_coordinates = OwnerCoordinates::decode(&first_coordinates).unwrap();
        assert_eq!(
            GlobalDictionary::decode_with_operation(
                &first_rows,
                &mut first_coordinates,
                &operation,
            )
            .unwrap()
            .rows
            .len(),
            1
        );
        let mut decoded_second_coordinates = OwnerCoordinates::decode(&second_coordinates).unwrap();
        assert!(matches!(
            GlobalDictionary::decode_with_operation(
                &second_rows,
                &mut decoded_second_coordinates,
                &operation,
            ),
            Err(CertificationError::Product(
                tidepool_repr::execution_schema::ParseError::LimitExceeded("work")
            ))
        ));
        assert_eq!(
            decode(&second_rows, &second_coordinates, high)
                .unwrap()
                .rows
                .len(),
            1
        );
    }

    #[test]
    fn receipt_dictionary_references_share_expansion_work_budget() {
        let global = dictionary_test_global();
        let mut dictionary = test_dictionary(std::slice::from_ref(&global));
        let row_bytes = dictionary.rows[0].1;
        let one_reference_work = std::mem::size_of::<AcceptedGlobal>()
            + row_bytes * 4
            + std::mem::size_of::<(usize, usize, usize)>();
        let operation = InventoryOperation::new(InventoryDecodeLimits {
            max_work: one_reference_work,
            ..InventoryDecodeLimits::default()
        });
        let reference = [Value::Integer(0.into())];
        assert_eq!(
            dictionary
                .resolve_with_operation(&reference, &operation)
                .unwrap(),
            vec![global.clone()]
        );
        assert!(matches!(
            dictionary.resolve_with_operation(&reference, &operation),
            Err(CertificationError::Product(
                tidepool_repr::execution_schema::ParseError::LimitExceeded("work")
            ))
        ));
        let fresh_operation = InventoryOperation::new(InventoryDecodeLimits {
            max_work: one_reference_work,
            ..InventoryDecodeLimits::default()
        });
        assert_eq!(
            dictionary
                .resolve_with_operation(&reference, &fresh_operation)
                .unwrap(),
            vec![global]
        );
    }

    #[test]
    #[ignore = "requires the retained oversized production tools receipt"]
    fn receipt_dictionary_preserves_retained_production_facts() {
        let path = PathBuf::from(
            std::env::var_os("TIDEPOOL_RETAINED_PRODUCT_RECEIPT")
                .expect("explicit retained production receipt path"),
        );
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.len() > COMPILER_RECEIPT_BYTES_LIMIT,
            "actual bounded size refusal"
        );
        assert!(
            matches!(decode_receipt(&bytes), Err(CertificationError::SizeLimit {
            format: CertificationFormat::ProductReceipt, actual, limit: COMPILER_RECEIPT_BYTES_LIMIT,
        }) if actual == bytes.len())
        );
        let full: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        let header = array(&full).unwrap();
        assert_eq!(number(&header[1]).unwrap(), 7);
        let compact = compact_receipt_coordinates(&full);
        let encoded = receipt_bytes(&compact);
        let admitted = decode_receipt_in(&encoded, path.parent()).unwrap();
        let full_globals = array(&header[5])
            .unwrap()
            .iter()
            .map(accepted_global)
            .collect::<CertResult<Vec<_>>>()
            .unwrap();
        let compact_header = array(&compact).unwrap();
        for index in [0, 2, 3, 4, 6, 7] {
            assert_eq!(
                header[index], compact_header[index],
                "untouched receipt facts"
            );
        }
        let mut coordinates = OwnerCoordinates::decode(&compact_header[8]).unwrap();
        let dictionary = GlobalDictionary::decode(&compact_header[5], &mut coordinates).unwrap();
        assert_eq!(
            dictionary
                .rows
                .iter()
                .map(|(global, _)| global.clone())
                .collect::<Vec<_>>(),
            full_globals
        );
        let expand = |refs: &Value| {
            array(refs)
                .unwrap()
                .iter()
                .map(|index| full_globals[number(index).unwrap() as usize].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(admitted.modules.len(), array(&header[2]).unwrap().len());
        for (module, raw) in admitted.modules.iter().zip(array(&header[2]).unwrap()) {
            let raw = array(raw).unwrap();
            assert_eq!(module.groups.len(), array(&raw[8]).unwrap().len());
            for (group, raw) in module.groups.iter().zip(array(&raw[8]).unwrap()) {
                let raw = array(raw).unwrap();
                assert_eq!(group.original_ordinal, number(&raw[0]).unwrap() as u32);
                assert_eq!(group.globals, expand(&raw[1]));
            }
        }
        assert_eq!(admitted.targets.len(), array(&header[3]).unwrap().len());
        for raw in array(&header[3]).unwrap() {
            let raw = array(raw).unwrap();
            assert_eq!(admitted.targets[string(&raw[0]).unwrap()], expand(&raw[1]));
        }
        assert!(encoded.len() <= COMPILER_RECEIPT_BYTES_LIMIT);
        eprintln!(
            "retained-product-receipt original_bytes={} compact_bytes={} globals={} coordinates={}",
            bytes.len(),
            encoded.len(),
            full_globals.len(),
            coordinates.rows.len()
        );
    }

    #[test]
    fn exact_recipe_receipt_requires_owning_sidecar_and_preserves_issued_bytes() {
        let root = tempfile::tempdir().unwrap();
        let (graph, _) = crate::execution_source::test_graph(root.path());
        let mut value = dictionary_receipt(&empty_legacy_receipt());
        let Value::Array(header) = &mut value else {
            panic!("fixture receipt")
        };
        header[7] = value_array([
            value_text("exact-available"),
            value_text(hex(&graph.digest())),
        ]);
        let encoded = receipt_bytes(&value);
        assert!(
            decode_receipt(&encoded).is_err(),
            "no ambient output directory"
        );
        assert!(
            decode_receipt_in(&encoded, Some(root.path())).is_err(),
            "missing advertised capsule"
        );
        let path = root.path().join("execution-source.cbor");
        std::fs::write(&path, graph.bytes()).unwrap();
        let receipt = decode_receipt_in(&encoded, Some(root.path())).unwrap();
        let WorkerExecutionSource::ExactAvailable { digest, bytes } = receipt.source_recipe else {
            panic!("available receipt must preserve its route")
        };
        assert_eq!(digest, graph.digest());
        assert_eq!(bytes.as_ref(), graph.bytes());
        std::fs::write(&path, b"changed source recipe").unwrap();
        assert!(decode_receipt_in(&encoded, Some(root.path())).is_err());
        let Value::Array(header) = &mut value else {
            panic!("fixture receipt")
        };
        header[7] = value_array([value_text("exact-unavailable"), value_text("unknown")]);
        assert!(decode_receipt_in(&receipt_bytes(&value), Some(root.path())).is_err());
    }

    fn empty_legacy_receipt() -> Value {
        value_array([
            value_text("TPCERT"),
            Value::Integer(2.into()),
            value_array([]),
            value_array([]),
            value_array([]),
        ])
    }

    fn dictionary_test_global() -> AcceptedGlobal {
        let identity = testing::identity("Support", "value");
        AcceptedGlobal {
            owner: ReceiptImportOwner::Retained {
                identity: identity.clone(),
                generation: 7,
            },
            identity,
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
        }
    }

    fn receipt_bytes(value: &Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).unwrap();
        bytes
    }

    fn fixture_envelope_value(envelope: &FinalizationEnvelope) -> Value {
        finalized_module::encode_fixture_envelope(envelope)
    }

    // Test migration of already-issued full facts. Production accepts only v9.
    fn compact_receipt_coordinates(full: &Value) -> Value {
        let mut compact = full.clone();
        let Value::Array(header) = &mut compact else {
            panic!("receipt tuple")
        };
        assert_eq!(header[1], Value::Integer(7.into()));
        let mut coordinates = BTreeMap::new();
        for row in array(&header[5]).unwrap() {
            let row = array(row).unwrap();
            let owner = array(&row[4]).unwrap();
            let coordinate = match string(&owner[0]).unwrap() {
                "source" => {
                    assert_eq!(row[0], owner[5]);
                    Some(value_array(owner[..4].iter().cloned()))
                }
                "package" | "retained-package" => {
                    assert_eq!(row[0], owner[4]);
                    Some(value_array([
                        value_text("package"),
                        owner[1].clone(),
                        owner[2].clone(),
                        owner[3].clone(),
                    ]))
                }
                "retained" => {
                    assert_eq!(row[0], owner[1]);
                    None
                }
                _ => panic!("full witness owner"),
            };
            if let Some(coordinate) = coordinate {
                coordinates.insert(receipt_bytes(&coordinate), coordinate);
            }
        }
        let indexed = coordinates
            .into_iter()
            .enumerate()
            .map(|(index, (bytes, coordinate))| (bytes, (index, coordinate)))
            .collect::<BTreeMap<_, _>>();
        let index = |coordinate: Value| {
            Value::Integer((indexed[&receipt_bytes(&coordinate)].0 as u64).into())
        };
        let Value::Array(rows) = &mut header[5] else {
            panic!("global dictionary")
        };
        for value in rows {
            let Value::Array(row) = value else {
                panic!("global row")
            };
            let owner = array(&row[4]).unwrap();
            row[4] = match string(&owner[0]).unwrap() {
                "source" => value_array([
                    value_text("source"),
                    index(value_array(owner[..4].iter().cloned())),
                    owner[4].clone(),
                ]),
                "package" => value_array([
                    value_text("package"),
                    index(value_array([
                        value_text("package"),
                        owner[1].clone(),
                        owner[2].clone(),
                        owner[3].clone(),
                    ])),
                ]),
                "retained-package" => value_array([
                    value_text("retained-package"),
                    index(value_array([
                        value_text("package"),
                        owner[1].clone(),
                        owner[2].clone(),
                        owner[3].clone(),
                    ])),
                    owner[5].clone(),
                ]),
                "retained" => value_array([value_text("retained"), owner[2].clone()]),
                _ => panic!("full witness owner"),
            };
        }
        header[1] = Value::Integer(9.into());
        header.push(value_array(
            indexed.into_values().map(|(_, coordinate)| coordinate),
        ));
        compact
    }

    fn test_dictionary(globals: &[AcceptedGlobal]) -> GlobalDictionary {
        let full = value_array([
            value_text("TPCERT"),
            Value::Integer(7.into()),
            value_array([]),
            value_array([]),
            value_array([]),
            value_array(globals.iter().map(value_global)),
            Value::Null,
            Value::Null,
        ]);
        let compact = compact_receipt_coordinates(&full);
        let header = array(&compact).unwrap();
        let mut coordinates = OwnerCoordinates::decode(&header[8]).unwrap();
        GlobalDictionary::decode(&header[5], &mut coordinates).unwrap()
    }

    fn dictionary_receipt(legacy: &Value) -> Value {
        let mut compact = legacy.clone();
        let Value::Array(header) = &mut compact else {
            panic!("legacy receipt tuple")
        };
        assert_eq!(header[1], Value::Integer(2.into()));
        let mut dictionary = BTreeMap::new();
        let mut rows = |value: &Value| {
            for row in array(value).unwrap() {
                dictionary.insert(receipt_bytes(row), row.clone());
            }
        };
        for module in array(&header[2]).unwrap() {
            for group in array(&array(module).unwrap()[8]).unwrap() {
                rows(&array(group).unwrap()[1]);
            }
        }
        for target in array(&header[3]).unwrap() {
            rows(&array(target).unwrap()[1]);
        }
        let indexed = dictionary
            .into_iter()
            .enumerate()
            .map(|(index, (bytes, row))| (bytes, (index, row)))
            .collect::<BTreeMap<_, _>>();
        let rewrite = |value: &mut Value| {
            let Value::Array(rows) = value else {
                panic!("global rows")
            };
            for row in rows {
                *row = Value::Integer((indexed[&receipt_bytes(row)].0 as u64).into());
            }
        };
        let Value::Array(modules) = &mut header[2] else {
            panic!("modules")
        };
        for module in modules {
            let Value::Array(module) = module else {
                panic!("module")
            };
            let Value::Array(groups) = &mut module[8] else {
                panic!("groups")
            };
            for group in groups {
                let Value::Array(group) = group else {
                    panic!("group")
                };
                rewrite(&mut group[1]);
            }
            module.push(value_array([]));
        }
        let Value::Array(targets) = &mut header[3] else {
            panic!("targets")
        };
        for target in targets {
            let Value::Array(target) = target else {
                panic!("target")
            };
            rewrite(&mut target[1]);
        }
        let modules = array(&header[2])
            .unwrap()
            .iter()
            .map(|module| {
                let row = array(module).unwrap();
                CertifiedModuleReceipt {
                    origin: if string(&row[0]).unwrap() == "fresh" {
                        ProductOrigin::Fresh
                    } else {
                        ProductOrigin::Cached
                    },
                    unit: string(&row[1]).unwrap().into(),
                    module: string(&row[2]).unwrap().into(),
                    module_version: None,
                    source_sha256: digest(&row[4]).unwrap(),
                    skinny_iface_sha256: digest(&row[5]).unwrap(),
                    product_sha256: digest(&row[6]).unwrap(),
                    dependency_witness_sha256: digest(&row[7]).unwrap(),
                    groups: vec![],
                    interface_requirements: BTreeMap::new(),
                }
            })
            .collect::<Vec<_>>();
        header[1] = Value::Integer(7.into());
        header.push(value_array(indexed.into_values().map(|(_, row)| row)));
        header.push(fixture_envelope_value(&fixture_finalization(
            None, &modules,
        )));
        header.push(value_array([value_text("ordinary")]));
        compact_receipt_coordinates(&compact)
    }

    #[test]
    fn receipt_decoder_requires_bounded_exact_tuple() {
        let source = "module Fresh where";
        let bytes = sidecar();
        let source_evidence = evidence(source);
        let accepted = receipt(&bytes, &source_evidence, source);
        let value = Value::Array(vec![
            Value::Text("TPCERT".into()),
            Value::Integer(2.into()),
            Value::Array(vec![Value::Array(vec![
                Value::Text("fresh".into()),
                Value::Text("main".into()),
                Value::Text("Fresh".into()),
                Value::Null,
                Value::Text(hex(&accepted.source_sha256)),
                Value::Text(hex(&accepted.skinny_iface_sha256)),
                Value::Text(hex(&accepted.product_sha256)),
                Value::Text(hex(&accepted.dependency_witness_sha256)),
                Value::Array(vec![]),
            ])]),
            Value::Array(vec![Value::Array(vec![
                Value::Text("target".into()),
                Value::Array(vec![]),
            ])]),
            Value::Array(vec![Value::Array(vec![
                Value::Text("base".into()),
                Value::Text("Selected".into()),
                Value::Text("/tmp/Selected.hi".into()),
                Value::Text(hex(&[5; 32])),
            ])]),
        ]);
        let value = dictionary_receipt(&value);
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&value, &mut encoded).unwrap();
        assert_eq!(
            decode_receipt(&encoded).unwrap(),
            CertifiedReceipt {
                source_recipe: WorkerExecutionSource::Ordinary,
                finalization: fixture_finalization(None, &vec![accepted.clone()]),
                modules: vec![accepted],
                targets: BTreeMap::from([("target".into(), vec![])]),
                packages: BTreeMap::from([(
                    ("base".into(), "Selected".into()),
                    PackageInterfaceWitness {
                        selected_path: PathBuf::from("/tmp/Selected.hi"),
                        sha256: [5; 32],
                    }
                )]),
            }
        );

        let large_groups = |count: usize| {
            let mut receipt = value.clone();
            let Value::Array(header) = &mut receipt else {
                unreachable!()
            };
            let Value::Array(modules) = &mut header[2] else {
                unreachable!()
            };
            let Value::Array(module) = &mut modules[0] else {
                unreachable!()
            };
            module[8] = Value::Array(
                (0..count)
                    .map(|ordinal| {
                        Value::Array(vec![
                            Value::Integer((ordinal as u64).into()),
                            Value::Array(vec![]),
                        ])
                    })
                    .collect(),
            );
            let mut encoded = Vec::new();
            ciborium::ser::into_writer(&receipt, &mut encoded).unwrap();
            encoded
        };
        // A real resident Tidepool.Effects.Core product has 5,930 groups.
        assert_eq!(
            decode_receipt(&large_groups(5_930)).unwrap().modules[0]
                .groups
                .len(),
            5_930
        );
        let historical_group_boundary = 8_193;
        assert_eq!(
            decode_receipt(&large_groups(historical_group_boundary))
                .unwrap()
                .modules[0]
                .groups
                .len(),
            historical_group_boundary
        );

        for module_count in [127, 128, 129] {
            let modules = (0..module_count)
                .map(|index| {
                    let unit = format!("home-{index}");
                    let module = format!("Module{index}");
                    let source = format!("module {module} where");
                    let mut module_bytes: Value =
                        ciborium::de::from_reader(sidecar().as_slice()).unwrap();
                    let Value::Array(header) = &mut module_bytes else {
                        unreachable!()
                    };
                    let Value::Array(rows) = &mut header[2] else {
                        unreachable!()
                    };
                    let Value::Array(row) = &mut rows[0] else {
                        unreachable!()
                    };
                    row[0] = value_text(&unit);
                    row[1] = value_text(&module);
                    let module_bytes = receipt_bytes(&module_bytes);
                    let mut module_evidence = evidence(&source);
                    module_evidence.modules[0].unit = unit.clone();
                    module_evidence.modules[0].module = module.clone();
                    let mut accepted = receipt(&module_bytes, &module_evidence, &source);
                    accepted.unit = unit.clone();
                    accepted.module = module.clone();
                    Value::Array(vec![
                        value_text("fresh"),
                        value_text(&accepted.unit),
                        value_text(&accepted.module),
                        Value::Null,
                        value_text(hex(&accepted.source_sha256)),
                        value_text(hex(&accepted.skinny_iface_sha256)),
                        value_text(hex(&accepted.product_sha256)),
                        value_text(hex(&accepted.dependency_witness_sha256)),
                        value_array([value_array([Value::Integer(0.into()), value_array([])])]),
                    ])
                })
                .collect::<Vec<_>>();
            let mut legacy = empty_legacy_receipt();
            let Value::Array(header) = &mut legacy else {
                unreachable!()
            };
            header[2] = value_array(modules);
            let compact = dictionary_receipt(&legacy);
            let bytes = receipt_bytes(&compact);
            let operation = InventoryOperation::new(InventoryDecodeLimits {
                max_bytes: bytes.len(),
                max_work: 8 << 20,
                ..InventoryDecodeLimits::default()
            });
            let decoded = decode_receipt_with_operation(&bytes, None, &operation).unwrap();
            assert_eq!(decoded.modules.len(), module_count);
            assert_eq!(decoded.finalization.modules.len(), module_count);
            assert!(decoded.modules.iter().all(|module| {
                module.groups.len() == 1
                    && decoded
                        .finalization
                        .modules
                        .contains_key(&(module.unit.clone(), module.module.clone()))
            }));
        }
    }

    #[test]
    fn receipt_decoder_preserves_retained_core_origin_without_fresh_finalization() {
        let mut legacy = empty_legacy_receipt();
        legacy.as_array_mut().unwrap()[2] = value_array([value_array([
            value_text("retained-core"),
            value_text("main"),
            value_text("Original"),
            Value::Null,
            value_text(hex(&[1; 32])),
            value_text(hex(&[2; 32])),
            value_text(hex(&[3; 32])),
            value_text(hex(&[4; 32])),
            value_array([]),
        ])]);
        let mut compact = dictionary_receipt(&legacy);
        let decoded = decode_receipt(&receipt_bytes(&compact)).unwrap();
        assert_eq!(decoded.modules[0].origin, ProductOrigin::RetainedCore);
        assert_eq!(decoded.modules[0].module_version, None);
        assert!(decoded.finalization.modules.is_empty());
        compact.as_array_mut().unwrap()[2].as_array_mut().unwrap()[0]
            .as_array_mut()
            .unwrap()[0] = value_text("retained");
        assert!(matches!(
            decode_receipt(&receipt_bytes(&compact)),
            Err(CertificationError::Receipt("product origin")),
        ));
    }

    #[test]
    fn target_retained_owner_keeps_identity_and_generation_unresolved() {
        let mut wire = testing::wire_program();
        let identity = testing::identity("Val.G7", "retained");
        wire.globals.push(GlobalDecl {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: Some(7),
        });
        let prepared = testing::prepare(wire).unwrap();
        let selected = AcceptedGlobal {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            owner: ReceiptImportOwner::Retained {
                identity: identity.clone(),
                generation: 7,
            },
        };
        assert_eq!(
            certify_target_owners(&prepared, &[selected.clone()], &[], &BTreeMap::new()).unwrap(),
            vec![PendingImportOwner::Retained {
                identity,
                generation: 7,
            }]
        );
        let mut stale = selected;
        stale.owner = ReceiptImportOwner::Retained {
            identity: stale.identity.clone(),
            generation: 8,
        };
        assert!(certify_target_owners(&prepared, &[stale], &[], &BTreeMap::new()).is_err());
    }

    #[test]
    fn package_interface_accounting_refusal_is_not_stale_evidence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Package.hi");
        let bytes = b"selected interface";
        std::fs::write(&path, bytes).unwrap();
        let witness = PackageInterfaceWitness {
            selected_path: path.clone(),
            sha256: sha(bytes),
        };
        verify_package_interface(&mut PackageInterfaceValidation::default(), &witness).unwrap();
        let operation = std::sync::Arc::new(InventoryOperation::new(InventoryDecodeLimits {
            max_work: 0,
            ..InventoryDecodeLimits::default()
        }));
        let mut limited = PackageInterfaceValidation::with_inventory(operation);
        assert!(matches!(
            verify_package_interface(&mut limited, &witness),
            Err(CertificationError::CapturedModulePayload(
                crate::recovery_artifacts::RecoveryArtifactError::InventoryAccounting(
                    crate::recovery_artifacts::RecoveryAdmissionFailure::Decode(
                        tidepool_repr::execution_schema::ParseError::LimitExceeded(_)
                    )
                )
            ))
        ));
        std::fs::write(path, b"changed interface").unwrap();
        assert!(matches!(
            verify_package_interface(&mut PackageInterfaceValidation::default(), &witness),
            Err(CertificationError::StaleEvidence)
        ));
    }

    #[test]
    fn package_owner_requires_selected_interface_bytes_at_final_admission() {
        let directory = tempfile::tempdir().unwrap();
        let selected_path = directory.path().join("Selected.hi");
        std::fs::write(&selected_path, b"selected interface").unwrap();
        let interface_digest = sha(b"selected interface");
        let mut binder = testing::identity("Selected", "member");
        binder.unit = "base".into();
        let import = ReceiptImportOwner::Package {
            unit: "base".into(),
            module: "Selected".into(),
            binder: binder.clone(),
            interface_digest,
        };
        let packages = BTreeMap::from([(
            ("base".into(), "Selected".into()),
            PackageInterfaceWitness {
                selected_path: selected_path.clone(),
                sha256: interface_digest,
            },
        )]);
        assert!(matches!(
            resolve_receipt_owner(import.clone(), &SourceGroupMap::new(), &packages),
            Ok(PendingImportOwner::Package { .. })
        ));
        std::fs::write(&selected_path, b"changed interface").unwrap();
        assert!(matches!(
            resolve_receipt_owner(import, &SourceGroupMap::new(), &packages),
            Err(CertificationError::StaleEvidence)
        ));
    }
    #[test]
    fn retained_package_preserves_generation_and_authenticated_interface_without_home_edge() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Base.hi");
        std::fs::write(&path, b"exact package interface").unwrap();
        let digest = sha(b"exact package interface");
        let mut binder = testing::identity("GHC.Internal.Base", "map");
        binder.unit = "ghc-internal".into();
        let package = PendingImportOwner::RetainedPackage {
            unit: binder.unit.clone(),
            module: binder.module.clone(),
            binder: binder.clone(),
            generation: 0,
            interface_digest: digest,
        };
        let owner = inherited_owner("Records");
        let group = inherited_group(&owner, package.clone());
        let packages = BTreeMap::from([(
            (binder.unit.clone(), binder.module.clone()),
            PackageInterfaceWitness {
                selected_path: path.clone(),
                sha256: digest,
            },
        )]);
        let bytes = encode_home_certification(&owner, &[group.clone()], &packages).unwrap();
        let requirements = certified_native_requirements(&bytes, &owner).unwrap();
        assert!(requirements.artifact_edges.is_empty());
        assert_eq!(
            requirements.retained_packages,
            vec![crate::artifact_inventory::RetainedPackageDependency {
                dependent_ordinal: group.group().original_ordinal(),
                identity: binder.clone(),
                generation: 0,
                interface_digest: digest,
            }]
        );
        let original = original_witness_fixture("Records", Some(package.clone()), 9, &packages);
        let expected =
            certified_native_requirements(original.certification_bytes(), original.owner())
                .unwrap();
        let recovered = recovered_witness_fixtures(&[original]);
        let certificate_decodes = HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get);
        assert_eq!(
            original_native_requirements(&recovered[0].product).unwrap(),
            expected
        );
        assert_eq!(
            HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get),
            certificate_decodes
        );
        let witness = decode_home_witness(&bytes).unwrap();
        let selected = &witness.groups[0].2[0];
        assert_eq!(
            resolve_receipt_owner(selected.owner.clone(), &SourceGroupMap::new(), &packages)
                .unwrap(),
            package
        );
        assert_eq!(
            certify_inherited_inventory(vec![inherited_parsed(&group, &packages)], &[]).unwrap(),
            vec![group.clone()]
        );
        let encoded = value_import(&selected.owner);
        assert_eq!(owner_from_test_value(&encoded), selected.owner);
        let mut global = group.group().globals()[0].clone();
        global.required_generation = None;
        assert!(validate_global_witness(
            &global,
            group.group().definitions().signatures(),
            selected
        )
        .is_err());
        let mut forged = selected.owner.clone();
        if let ReceiptImportOwner::RetainedPackage {
            interface_digest, ..
        } = &mut forged
        {
            *interface_digest = [9; 32];
        }
        assert!(resolve_receipt_owner(forged, &SourceGroupMap::new(), &packages).is_err());
        let mut sources = SourceGroupMap::new();
        let mut home = owner.clone();
        home.unit = binder.unit.clone();
        home.module = binder.module.clone();
        sources.insert(
            (home.clone(), 1, binder.clone()),
            (home, ProductOrigin::Fresh),
        );
        assert!(matches!(
            resolve_receipt_owner(selected.owner.clone(), &sources, &packages),
            Err(CertificationError::Mismatch(
                "home owner downgraded to package"
            ))
        ));
        std::fs::write(&path, b"changed package interface").unwrap();
        assert!(matches!(
            resolve_receipt_owner(selected.owner.clone(), &SourceGroupMap::new(), &packages),
            Err(CertificationError::StaleEvidence)
        ));
    }

    fn owner_from_test_value(value: &Value) -> ReceiptImportOwner {
        owner(value).unwrap()
    }

    #[test]
    fn execution_source_seal_binds_original_owner_and_refuses_replacement() {
        let owner = inherited_owner("Original");
        let legacy = encode_home_certification(&owner, &[], &BTreeMap::new()).unwrap();
        let mut validation = PackageInterfaceValidation::default();
        assert_eq!(
            home_execution_source_digest_with_validation(&legacy, &owner, &mut validation).unwrap(),
            None
        );
        let sealed = bind_home_execution_source(&legacy, &owner, [7; 32], &mut validation).unwrap();
        assert_ne!(sha(&legacy), sha(&sealed));
        assert_eq!(
            home_execution_source_digest_with_validation(&sealed, &owner, &mut validation).unwrap(),
            Some([7; 32])
        );
        assert!(bind_home_execution_source(&sealed, &owner, [8; 32], &mut validation).is_err());
        assert!(bind_home_execution_source(&legacy, &owner, [0; 32], &mut validation).is_err());
        assert!(home_execution_source_digest_with_validation(
            &sealed,
            &inherited_owner("Other"),
            &mut validation,
        )
        .is_err());
        assert_eq!(
            bind_home_execution_source(&sealed, &owner, [7; 32], &mut validation).unwrap(),
            sealed
        );
    }

    #[test]
    fn source_only_canonical_interfaces_close_original_receipt_requirements() {
        let package_imports = |module: &str, interface: &[u8]| {
            let value = value_array([
                value_text("TPPKGROOTS"),
                value_text("2"),
                value_array([
                    value_text("main"),
                    value_text(module),
                    value_text(hex(&sha(interface))),
                ]),
                value_array([]),
                value_array([]),
            ]);
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&value, &mut bytes).unwrap();
            bytes
        };
        let dependency_bytes = b"source-only interface".to_vec();
        let fresh = fixture_interface_bytes(
            [3; 32],
            "main",
            "Aeson",
            dependency_bytes.clone(),
            package_imports("Aeson", &dependency_bytes),
        );
        let inherited_bytes = b"inherited source interface".to_vec();
        let inherited = fixture_interface_bytes(
            [3; 32],
            "main",
            "JsonSupport",
            inherited_bytes.clone(),
            package_imports("JsonSupport", &inherited_bytes),
        );
        let mut accepted = receipt(&[], &evidence("source"), "source");
        accepted.module = "Prelude".into();
        accepted.interface_requirements = BTreeMap::from([
            (("main".into(), "Aeson".into()), sha(&dependency_bytes)),
            (("main".into(), "JsonSupport".into()), sha(&inherited_bytes)),
        ]);

        // Aeson and JsonSupport have authenticated interface carriers but no
        // executable product rows. Fresh finalization and exact inherited
        // context are both valid sources for interface-only closure.
        let mut admitted = BTreeMap::new();
        admit_canonical_interface_owners(&mut admitted, [&fresh, &inherited]).unwrap();
        validate_original_interface_owner_closure(&[accepted], &admitted).unwrap();
    }

    #[test]
    fn source_only_interface_closure_rejects_missing_changed_and_conflicting_seals() {
        let package_imports = |interface: &[u8]| {
            let value = value_array([
                value_text("TPPKGROOTS"),
                value_text("2"),
                value_array([
                    value_text("main"),
                    value_text("Aeson"),
                    value_text(hex(&sha(interface))),
                ]),
                value_array([]),
                value_array([]),
            ]);
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&value, &mut bytes).unwrap();
            bytes
        };
        let original_bytes = b"canonical source-only interface".to_vec();
        let canonical = fixture_interface_bytes(
            [3; 32],
            "main",
            "Aeson",
            original_bytes.clone(),
            package_imports(&original_bytes),
        );
        let changed = fixture_interface_bytes(
            [3; 32],
            "main",
            "Aeson",
            b"different interface".to_vec(),
            package_imports(b"different interface"),
        );
        let mut accepted = receipt(&[], &evidence("source"), "source");
        accepted.module = "Prelude".into();
        accepted.interface_requirements =
            BTreeMap::from([(("main".into(), "Aeson".into()), sha(&original_bytes))]);

        let absent = BTreeMap::new();
        assert!(matches!(
            validate_original_interface_owner_closure(&[accepted.clone()], &absent),
            Err(CertificationError::OriginalInterfaceClosure { actual: None, .. })
        ));

        let mut stale_seal = BTreeMap::new();
        admit_canonical_interface_owners(&mut stale_seal, [&changed]).unwrap();
        assert!(matches!(
            validate_original_interface_owner_closure(&[accepted], &stale_seal),
            Err(CertificationError::OriginalInterfaceClosure { .. })
        ));

        let mut conflicting = BTreeMap::from([(("main".into(), "Aeson".into()), [9; 32])]);
        assert!(matches!(
            admit_canonical_interface_owners(&mut conflicting, [&canonical]),
            Err(CertificationError::Mismatch(
                "conflicting admitted interface owner"
            ))
        ));
    }

    #[test]
    fn original_interface_seals_survive_cold_codec_and_native_witness_without_executable_sources() {
        let owner = inherited_owner("Original");
        let encoded = encode_home_certification(&owner, &[], &BTreeMap::new()).unwrap();
        let mut witness = decode_home_witness(&encoded).unwrap();
        let requirements =
            BTreeMap::from([(("old-cohort".into(), "PrivateNominalOwner".into()), [7; 32])]);
        witness.interface_requirements = requirements.clone();
        let encoded = encode_home_witness(&witness).unwrap();
        let cold = decode_home_witness(&encoded).unwrap();
        assert_eq!(cold.interface_requirements, requirements);
        assert!(cold.sources.is_empty());
        assert!(native_requirements_from_witness(&cold)
            .artifact_edges
            .is_empty());
        let product = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            owner.clone(),
            vec![1],
            vec![2],
            vec![],
            encoded,
        );
        assert_eq!(
            original_interface_requirements(&product).unwrap(),
            requirements
        );
        let retained = retain_original_native(product, vec![], cold).unwrap();
        assert_eq!(
            original_interface_requirements(&retained).unwrap(),
            requirements
        );
        assert!(original_native_requirements(&retained)
            .unwrap()
            .artifact_edges
            .is_empty());
        let mut wrong_owner = witness;
        wrong_owner.owner.module = "AnotherOriginal".into();
        let wrong = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            owner,
            vec![1],
            vec![2],
            vec![],
            encode_home_witness(&wrong_owner).unwrap(),
        );
        assert!(original_interface_requirements(&wrong).is_err());
    }

    #[test]
    fn original_interface_seal_codec_refuses_missing_duplicate_unsorted_and_self_owners() {
        let owner = inherited_owner("Original");
        let current = encode_home_certification(&owner, &[], &BTreeMap::new()).unwrap();
        let value: Value = ciborium::de::from_reader(current.as_slice()).unwrap();
        let seal = |unit, module| {
            value_array([
                value_text(unit),
                value_text(module),
                value_text(hex(&[7; 32])),
            ])
        };
        for entries in [
            vec![seal("unit", "A"), seal("unit", "A")],
            vec![seal("unit", "Z"), seal("unit", "A")],
            vec![seal(&owner.unit, &owner.module)],
        ] {
            let mut changed = value.clone();
            changed.as_array_mut().unwrap()[7] = value_array(entries);
            assert!(decode_home_witness(&receipt_bytes(&changed)).is_err());
        }
        let mut missing = value;
        missing.as_array_mut().unwrap().pop();
        assert!(decode_home_witness(&receipt_bytes(&missing)).is_err());
    }

    #[test]
    fn certificate_versions_refuse_legacy_ownership_without_reinterpretation() {
        for version in [1, 2, 3, 4, 5, 6, 7, 8] {
            let mut receipt = dictionary_receipt(&empty_legacy_receipt());
            let Value::Array(rows) = &mut receipt else {
                unreachable!()
            };
            rows[1] = Value::Integer(version.into());
            assert!(matches!(
                decode_receipt(&receipt_bytes(&receipt)),
                Err(CertificationError::UnsupportedVersion {
                    format: CertificationFormat::ProductReceipt,
                    expected: 9,
                    ..
                })
            ));
        }
        let owner = inherited_owner("Original");
        let current = encode_home_certification(&owner, &[], &BTreeMap::new()).unwrap();
        let mut value: Value = ciborium::de::from_reader(current.as_slice()).unwrap();
        let Value::Array(rows) = &mut value else {
            unreachable!()
        };
        for version in [1, 2, 3, 4] {
            rows[1] = Value::Integer(version.into());
            assert!(matches!(
                decode_home_witness(&receipt_bytes(&Value::Array(rows.clone()))),
                Err(CertificationError::UnsupportedVersion {
                    format: CertificationFormat::HomeOwners,
                    expected: 5,
                    ..
                })
            ));
        }
    }
}
