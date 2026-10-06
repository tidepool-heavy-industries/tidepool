//! Materialization of exact compiler artifacts. Disposable request inputs are
//! verified without durability work; recovery manifests use fsynced run-owned
//! closures independently of the regenerable compile cache.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tidepool_repr::execution_schema::CachedHomeOwner;

/// Compiler requests own disposable inputs; recovery publications must survive
/// a crash. Both modes verify the same immutable bytes and path ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MaterializationMode {
    Scratch,
    Durable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecoveryArtifactRef {
    pub toolchain_identity_sha256: [u8; 32],
    pub unit: String,
    pub module: String,
    pub module_version: [u8; 32],
    pub skinny_iface_sha256: [u8; 32],
    pub product_sha256: [u8; 32],
    pub interface_path: PathBuf,
    pub package_imports_path: PathBuf,
    pub package_imports_sha256: [u8; 32],
    pub certification_path: PathBuf,
    pub certification_sha256: [u8; 32],
    pub product_path: PathBuf,
    pub module_interface: Option<RecoveryModuleInterfaceRef>,
    /// Legacy originals retain native/interface authority without GHC execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_source: Option<RecoveryExecutionSourceRef>,
}

/// Captured canonical interface and separately sealed compiler Core companion.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryModuleInterfaceRef {
    pub interface: RecoveryJoinRef,
    pub certificate_path: PathBuf,
    pub certificate_sha256: [u8; 32],
    pub core: Option<RecoveryCoreRef>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryCoreRef {
    pub path: PathBuf,
    pub sha256: [u8; 32],
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryExecutionSourceRef {
    pub path: PathBuf,
    pub sha256: [u8; 32],
}

/// A source-less public Join owns only an interface. Its implementation
/// modules remain independent `RecoveryArtifactRef` product pairs.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct RecoveryJoinRef {
    pub toolchain_identity_sha256: [u8; 32],
    pub unit: String,
    pub module: String,
    pub skinny_iface_sha256: [u8; 32],
    pub interface_path: PathBuf,
    pub package_imports_path: PathBuf,
    pub package_imports_sha256: [u8; 32],
}

pub struct RecoveryArtifactInput<'a> {
    pub owner: &'a CachedHomeOwner,
    pub interface_source: &'a Path,
    pub product_source: &'a Path,
    pub module_interface: (&'a Path, &'a RecoveryModuleInterfaceRef),
}

/// Exact original module bytes and Rust-admitted ownership, retained across a
/// temporary worker directory's lifetime. Only compiler certification creates
/// this bundle; materialization rechecks every member before publication.
#[derive(Clone, Debug)]
pub struct CertifiedRecoveryProduct {
    owner: CachedHomeOwner,
    source_sha256: Option<[u8; 32]>,
    interface_bytes: Arc<[u8]>,
    product_bytes: Arc<[u8]>,
    package_imports_bytes: Arc<[u8]>,
    certification_bytes: Arc<[u8]>,
    execution_source: Option<Arc<crate::execution_source::CertifiedExecutionSourceGraph>>,
    original_native: Option<Arc<crate::certified_products::OriginalNativeWitness>>,
    module_interface: Option<crate::certified_products::CertifiedModuleInterface>,
}

// The native witness is a derived immutable fact, not another product identity.
impl PartialEq for CertifiedRecoveryProduct {
    fn eq(&self, other: &Self) -> bool {
        self.source_sha256 == other.source_sha256 && self.same_durable_artifact(other)
    }
}
impl Eq for CertifiedRecoveryProduct {}

impl CertifiedRecoveryProduct {
    /// Inventory identity compares durable seals and payloads. The fresh source
    /// witness remains separate admission authority and is never recovered or
    /// promoted by reusing an already retained inventory entry.
    pub(crate) fn same_durable_artifact(&self, other: &Self) -> bool {
        self.owner == other.owner
            && self.interface_bytes == other.interface_bytes
            && self.product_bytes == other.product_bytes
            && self.package_imports_bytes == other.package_imports_bytes
            && self.certification_bytes == other.certification_bytes
            && self.execution_source == other.execution_source
            && self.module_interface == other.module_interface
    }

    pub(crate) fn from_certification(
        owner: CachedHomeOwner,
        interface_bytes: Vec<u8>,
        product_bytes: Vec<u8>,
        package_imports_bytes: Vec<u8>,
        certification_bytes: Vec<u8>,
    ) -> Self {
        Self {
            owner,
            source_sha256: None,
            interface_bytes: interface_bytes.into(),
            product_bytes: product_bytes.into(),
            package_imports_bytes: package_imports_bytes.into(),
            certification_bytes: certification_bytes.into(),
            execution_source: None,
            original_native: None,
            module_interface: None,
        }
    }

    pub(crate) fn from_finalized_certification(
        owner: CachedHomeOwner,
        product_bytes: Vec<u8>,
        certification_bytes: Vec<u8>,
        binding: crate::certified_products::ValidatedModuleBinding,
    ) -> Self {
        let interface = binding.into_interface();
        Self {
            owner,
            source_sha256: None,
            interface_bytes: interface.interface_anchor(),
            product_bytes: product_bytes.into(),
            package_imports_bytes: interface.package_imports_anchor(),
            certification_bytes: certification_bytes.into(),
            execution_source: None,
            original_native: None,
            module_interface: Some(interface),
        }
    }

    pub fn owner(&self) -> &CachedHomeOwner {
        &self.owner
    }

    pub fn source_sha256(&self) -> Option<[u8; 32]> {
        self.source_sha256
    }

    pub fn interface_bytes(&self) -> &[u8] {
        &self.interface_bytes
    }

    pub fn product_bytes(&self) -> &[u8] {
        &self.product_bytes
    }

    pub(crate) fn package_imports_bytes(&self) -> &[u8] {
        &self.package_imports_bytes
    }
    pub(crate) fn certification_bytes(&self) -> &[u8] {
        &self.certification_bytes
    }

    pub(crate) fn module_interface(
        &self,
    ) -> Option<&crate::certified_products::CertifiedModuleInterface> {
        self.module_interface.as_ref()
    }

    pub(crate) fn with_module_interface(
        mut self,
        interface: crate::certified_products::CertifiedModuleInterface,
    ) -> Result<Self, RecoveryArtifactError> {
        crate::certified_products::validate_original_module_interface(&self, &interface)
            .map_err(|_| RecoveryArtifactError::InvalidReference)?;
        self.interface_bytes = interface.interface_anchor();
        self.package_imports_bytes = interface.package_imports_anchor();
        self.module_interface = Some(interface);
        Ok(self)
    }

    pub(crate) fn original_byte_anchors(&self) -> [&Arc<[u8]>; 4] {
        [
            &self.interface_bytes,
            &self.product_bytes,
            &self.package_imports_bytes,
            &self.certification_bytes,
        ]
    }

    pub(crate) fn original_native(
        &self,
    ) -> Option<&Arc<crate::certified_products::OriginalNativeWitness>> {
        self.original_native.as_ref()
    }

    pub(crate) fn with_original_native(
        mut self,
        witness: Arc<crate::certified_products::OriginalNativeWitness>,
    ) -> Result<Self, RecoveryArtifactError> {
        if !witness.matches_original(&self) {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        self.original_native = Some(witness);
        Ok(self)
    }

    pub(crate) fn with_source_sha256(mut self, source_sha256: [u8; 32]) -> Self {
        self.source_sha256 = Some(source_sha256);
        self
    }

    pub(crate) fn execution_source(
        &self,
    ) -> Option<&Arc<crate::execution_source::CertifiedExecutionSourceGraph>> {
        self.execution_source.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn with_execution_source(
        self,
        graph: Arc<crate::execution_source::CertifiedExecutionSourceGraph>,
    ) -> Result<Self, RecoveryArtifactError> {
        self.with_execution_source_with_validation(
            graph,
            &mut PackageInterfaceValidation::default(),
        )
    }

    pub(crate) fn with_execution_source_with_validation(
        mut self,
        graph: Arc<crate::execution_source::CertifiedExecutionSourceGraph>,
        validation: &mut PackageInterfaceValidation,
    ) -> Result<Self, RecoveryArtifactError> {
        if !graph.eligible_source_replay_root(&self.owner) {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        let sealed = crate::certified_products::original_execution_source_digest_with_validation(
            &self, validation,
        )
        .map_err(|_| RecoveryArtifactError::InvalidReference)?;
        if sealed != Some(graph.digest()) {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        self.execution_source = Some(graph);
        Ok(self)
    }
}

/// A source-less interface sealed by declaration certification or verified
/// recovery admission. Only the toolchain's owning workflows construct it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedJoinedInterface {
    toolchain_identity_sha256: [u8; 32],
    unit: String,
    module: String,
    interface_bytes: Arc<[u8]>,
    package_imports_bytes: Arc<[u8]>,
}

impl CertifiedJoinedInterface {
    pub(crate) fn from_certification(
        producer: [u8; 32],
        unit: String,
        module: String,
        interface_bytes: Vec<u8>,
        package_imports_bytes: Vec<u8>,
    ) -> Result<Self, RecoveryArtifactError> {
        Self::from_certification_with_validation(
            producer,
            unit,
            module,
            interface_bytes,
            package_imports_bytes,
            &mut PackageInterfaceValidation::default(),
        )
    }

    pub(crate) fn from_certification_with_validation(
        producer: [u8; 32],
        unit: String,
        module: String,
        interface_bytes: Vec<u8>,
        package_imports_bytes: Vec<u8>,
        validation: &mut PackageInterfaceValidation,
    ) -> Result<Self, RecoveryArtifactError> {
        if producer == [0; 32] || unit.is_empty() || module.is_empty() {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        let digest: [u8; 32] = Sha256::digest(&interface_bytes).into();
        validate_package_imports_with_validation(
            &package_imports_bytes,
            &unit,
            &module,
            &digest,
            Path::new("owned-join.hi.packages"),
            validation,
        )?;
        Ok(Self {
            toolchain_identity_sha256: producer,
            unit,
            module,
            interface_bytes: interface_bytes.into(),
            package_imports_bytes: package_imports_bytes.into(),
        })
    }
    pub fn unit(&self) -> &str {
        &self.unit
    }
    pub fn module(&self) -> &str {
        &self.module
    }
    pub fn interface_bytes(&self) -> &[u8] {
        &self.interface_bytes
    }
    pub fn package_imports_bytes(&self) -> &[u8] {
        &self.package_imports_bytes
    }
    pub fn toolchain_identity_sha256(&self) -> [u8; 32] {
        self.toolchain_identity_sha256
    }
    pub fn materialize(&self, root: &Path) -> Result<RecoveryJoinRef, RecoveryArtifactError> {
        self.materialize_with_work(root, &mut RecoveryArtifactWork::default())
    }

    pub(crate) fn materialize_with_work(
        &self,
        root: &Path,
        work: &mut RecoveryArtifactWork,
    ) -> Result<RecoveryJoinRef, RecoveryArtifactError> {
        with_artifact_work(work, |validation| {
            self.materialize_with_validation(root, validation, MaterializationMode::Durable)
        })
    }

    pub(crate) fn materialize_with_validation(
        &self,
        root: &Path,
        validation: &mut PackageInterfaceValidation,
        mode: MaterializationMode,
    ) -> Result<RecoveryJoinRef, RecoveryArtifactError> {
        materialize_owned_join(
            root,
            self.toolchain_identity_sha256,
            &self.unit,
            &self.module,
            &self.interface_bytes,
            &self.package_imports_bytes,
            validation,
            mode,
        )
    }
}

/// Retained typechecking evidence for an original Val module. This does not
/// authorize any native import; execution still requires a completed lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedValueInterface {
    interface: CertifiedJoinedInterface,
    requirements: Vec<crate::declaration_join::ExactModuleIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecoveryValueInterfaceRef {
    pub artifact_id: crate::artifact_inventory::ArtifactId,
    pub interface: RecoveryJoinRef,
    pub requirements: Vec<crate::declaration_join::ExactModuleIdentity>,
}

impl CertifiedValueInterface {
    /// Checked-cell and host-interface issuers call this after admitting the
    /// compiler's same-transaction output and original canonical Val identity.
    pub(crate) fn from_checked_compilation(
        producer: [u8; 32],
        owner: crate::declaration_join::ExactModuleIdentity,
        interface_bytes: Vec<u8>,
        package_imports_bytes: Vec<u8>,
        mut requirements: Vec<crate::declaration_join::ExactModuleIdentity>,
    ) -> Result<Self, RecoveryArtifactError> {
        requirements.sort();
        requirements.dedup();
        Ok(Self {
            interface: CertifiedJoinedInterface::from_certification(
                producer,
                owner.unit,
                owner.module,
                interface_bytes,
                package_imports_bytes,
            )?,
            requirements,
        })
    }
    pub(crate) fn from_admitted_interface(
        interface: CertifiedJoinedInterface,
        requirements: Vec<crate::declaration_join::ExactModuleIdentity>,
    ) -> Self {
        Self {
            interface,
            requirements,
        }
    }
    pub fn interface(&self) -> &CertifiedJoinedInterface {
        &self.interface
    }
    pub fn requirements(&self) -> &[crate::declaration_join::ExactModuleIdentity] {
        &self.requirements
    }
    pub fn artifact_id(&self) -> crate::artifact_inventory::ArtifactId {
        crate::artifact_inventory::ArtifactEntry::interface(
            self.interface.clone(),
            crate::artifact_inventory::JoinedInterfaceRole::ValueInterface,
            self.requirements.clone(),
        )
        .descriptor
        .id
    }
    pub fn materialize(
        &self,
        root: &Path,
    ) -> Result<RecoveryValueInterfaceRef, RecoveryArtifactError> {
        self.materialize_with_work(root, &mut RecoveryArtifactWork::default())
    }

    pub(crate) fn materialize_with_work(
        &self,
        root: &Path,
        work: &mut RecoveryArtifactWork,
    ) -> Result<RecoveryValueInterfaceRef, RecoveryArtifactError> {
        Ok(RecoveryValueInterfaceRef {
            artifact_id: self.artifact_id(),
            interface: self.interface.materialize_with_work(root, work)?,
            requirements: self.requirements.clone(),
        })
    }
}

#[derive(Debug)]
pub struct VerifiedRecoveryArtifact {
    pub reference: RecoveryArtifactRef,
    pub interface_path: PathBuf,
    pub package_imports_path: PathBuf,
    pub product_path: PathBuf,
    pub interface_bytes: Vec<u8>,
    pub package_imports_bytes: Vec<u8>,
    pub certification_path: PathBuf,
    pub certification_bytes: Vec<u8>,
    pub product_bytes: Vec<u8>,
    pub(crate) module_interface: crate::certified_products::CertifiedModuleInterface,
    pub(crate) execution_source:
        Option<Arc<crate::execution_source::CertifiedExecutionSourceGraph>>,
}

#[derive(Debug)]
pub struct VerifiedRecoveryJoin {
    pub reference: RecoveryJoinRef,
    pub interface_path: PathBuf,
    pub package_imports_path: PathBuf,
    pub interface_bytes: Vec<u8>,
    pub package_imports_bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryArtifactError {
    #[error("inventory accounting: {0}")]
    InventoryAccounting(#[from] tidepool_repr::execution_schema::ParseError),
    #[error("invalid recovery artifact reference")]
    InvalidReference,
    #[error("execution source producer differs for {unit}:{module}")]
    ExecutionSourceProducerMismatch {
        unit: String,
        module: String,
        expected: [u8; 32],
        actual: [u8; 32],
    },
    #[error("recovery artifact unavailable: {0}")]
    Unavailable(PathBuf),
    #[error("recovery artifact checksum mismatch: {0}")]
    DigestMismatch(PathBuf),
    #[error("invalid package import witness: {0}")]
    InvalidPackageImports(PathBuf),
    #[error("package import witness {} has version {found}; expected 2", path.display())]
    UnsupportedPackageImportsVersion { path: PathBuf, found: String },
    #[error("home certification unavailable: {0}")]
    CertifiedOwnersUnavailable(PathBuf),
    #[error("home certification checksum mismatch: {0}")]
    CertifiedOwnersDigestMismatch(PathBuf),
    #[error("invalid home certification: {0}")]
    InvalidCertifiedOwners(PathBuf),
    #[error("recovery artifact unreadable at {path}: {error}")]
    Unreadable {
        path: PathBuf,
        #[source]
        error: io::Error,
    },
    #[error("invalid captured artifact payload: {0}")]
    InvalidCapturedPayload(PathBuf),
    #[error("invalid finalized module certificate: {0}")]
    InvalidModuleCertificate(PathBuf),
    #[error("recovery artifact I/O: {0}")]
    Io(#[from] io::Error),
}

fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write;
    digest.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

const PACKAGE_IMPORTS_LIMIT: u64 = 4 * 1024 * 1024;
const PACKAGE_IMPORT_ROOT_LIMIT: usize = 16_384;
const PACKAGE_INTERFACE_LIMIT: u64 = 32 * 1024 * 1024;
const CERTIFICATION_LIMIT: u64 = 32 * 1024 * 1024;

// Captures belong to one validation stage, never to a later filesystem check.
// Limit retained bytes without rejecting an otherwise valid large closure:
// interfaces and execution graphs beyond the budget are read and verified again.
const PACKAGE_VALIDATION_RETAIN_LIMIT: usize = 64 * 1024 * 1024;

/// Payload work at the recovery materialization/verification boundary.
/// This excludes descriptor-ID hashing and unrelated compiler input work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecoveryArtifactWork {
    pub hash_bytes: u64,
    pub read_bytes: u64,
    pub written_bytes: u64,
    /// Package witnesses and execution graphs submitted to tracked decoders.
    pub decoded_bytes: u64,
}

pub(crate) fn with_artifact_work<T, E>(
    work: &mut RecoveryArtifactWork,
    operation: impl FnOnce(&mut PackageInterfaceValidation) -> Result<T, E>,
) -> Result<T, E> {
    let mut validation = PackageInterfaceValidation::default();
    let result = operation(&mut validation);
    work.hash_bytes += validation.hash_bytes;
    work.read_bytes += validation.read_bytes;
    work.written_bytes += validation.written_bytes;
    work.decoded_bytes += validation.decoded_bytes;
    result
}

/// Share bounded package captures across one filesystem validation operation.
/// A later operation starts fresh and rechecks the files.
pub fn with_recovery_artifact_verification<T, E>(
    root: &Path,
    work: &mut RecoveryArtifactWork,
    operation: impl FnOnce(&mut RecoveryArtifactVerification<'_>) -> Result<T, E>,
) -> Result<T, E> {
    with_artifact_work(work, |validation| {
        operation(&mut RecoveryArtifactVerification { root, validation })
    })
}

/// Available only inside one `with_recovery_artifact_verification` operation.
pub struct RecoveryArtifactVerification<'a> {
    root: &'a Path,
    validation: &'a mut PackageInterfaceValidation,
}

impl RecoveryArtifactVerification<'_> {
    pub fn verify_home(
        &mut self,
        reference: &RecoveryArtifactRef,
    ) -> Result<VerifiedRecoveryArtifact, RecoveryArtifactError> {
        verify_materialized_ref_with_validation(self.root, reference, self.validation)
    }

    pub fn verify_module_interface(
        &mut self,
        reference: &RecoveryModuleInterfaceRef,
    ) -> Result<(), RecoveryArtifactError> {
        recover_module_interface(self.root, reference, self.validation).map(|_| ())
    }

    pub fn verify_join(
        &mut self,
        reference: &RecoveryJoinRef,
    ) -> Result<VerifiedRecoveryJoin, RecoveryArtifactError> {
        verify_materialized_join_with_validation(self.root, reference, self.validation)
    }
}

// Count actual package payload I/O across all contexts on this test thread.
// A regression that accidentally creates hidden fresh contexts must be visible.
#[cfg(test)]
thread_local! {
    static PACKAGE_INTERFACE_IO: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(crate) fn package_interface_io() -> (u64, u64) {
    PACKAGE_INTERFACE_IO.with(std::cell::Cell::get)
}

pub(crate) struct PackageInterfaceValidation {
    pub(crate) inventory: Arc<tidepool_repr::execution_schema::InventoryOperation>,
    captured: BTreeMap<PathBuf, CapturedPackageInterface>,
    retained_bytes: usize,
    hash_bytes: u64,
    read_bytes: u64,
    written_bytes: u64,
    decoded_bytes: u64,
    #[cfg(test)]
    pub(crate) home_witness_validations: usize,
    execution_sources:
        BTreeMap<PathBuf, Arc<crate::execution_source::CertifiedExecutionSourceGraph>>,
}

impl Default for PackageInterfaceValidation {
    fn default() -> Self {
        Self::with_inventory(Arc::new(
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
        ))
    }
}

struct CapturedPackageInterface {
    _bytes: Vec<u8>,
    sha256: [u8; 32],
}

impl PackageInterfaceValidation {
    pub(crate) fn with_inventory(
        inventory: Arc<tidepool_repr::execution_schema::InventoryOperation>,
    ) -> Self {
        Self {
            inventory,
            captured: BTreeMap::new(),
            retained_bytes: 0,
            hash_bytes: 0,
            read_bytes: 0,
            written_bytes: 0,
            decoded_bytes: 0,
            #[cfg(test)]
            home_witness_validations: 0,
            execution_sources: BTreeMap::new(),
        }
    }

    pub(crate) fn work(&self) -> RecoveryArtifactWork {
        RecoveryArtifactWork {
            hash_bytes: self.hash_bytes,
            read_bytes: self.read_bytes,
            written_bytes: self.written_bytes,
            decoded_bytes: self.decoded_bytes,
        }
    }

    fn digest(&mut self, bytes: &[u8]) -> [u8; 32] {
        self.hash_bytes += bytes.len() as u64;
        Sha256::digest(bytes).into()
    }

    pub(crate) fn verify(
        &mut self,
        path: &Path,
        expected_sha256: &[u8; 32],
    ) -> Result<(), RecoveryArtifactError> {
        self.verify_with_budget(path, expected_sha256, PACKAGE_VALIDATION_RETAIN_LIMIT)
    }

    fn verify_with_budget(
        &mut self,
        path: &Path,
        expected_sha256: &[u8; 32],
        retain_limit: usize,
    ) -> Result<(), RecoveryArtifactError> {
        if !path.is_absolute() {
            return Err(RecoveryArtifactError::InvalidPackageImports(
                path.to_path_buf(),
            ));
        }
        if let Some(captured) = self.captured.get(path) {
            return if &captured.sha256 == expected_sha256 {
                Ok(())
            } else {
                Err(RecoveryArtifactError::DigestMismatch(path.to_path_buf()))
            };
        }
        let file = File::open(path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                RecoveryArtifactError::Unavailable(path.to_path_buf())
            } else {
                RecoveryArtifactError::Unreadable {
                    path: path.to_path_buf(),
                    error,
                }
            }
        })?;
        #[cfg(test)]
        PACKAGE_INTERFACE_IO.with(|work| {
            let (opens, bytes) = work.get();
            work.set((opens + 1, bytes));
        });
        let metadata = file
            .metadata()
            .map_err(|error| RecoveryArtifactError::Unreadable {
                path: path.to_path_buf(),
                error,
            })?;
        if !metadata.is_file() || metadata.len() > PACKAGE_INTERFACE_LIMIT {
            return Err(RecoveryArtifactError::InvalidPackageImports(
                path.to_path_buf(),
            ));
        }
        self.inventory.charge(metadata.len() as usize + 1)?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize + 1);
        file.take(metadata.len() + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| RecoveryArtifactError::Unreadable {
                path: path.to_path_buf(),
                error,
            })?;
        #[cfg(test)]
        PACKAGE_INTERFACE_IO.with(|work| {
            let (opens, read_bytes) = work.get();
            work.set((opens, read_bytes + bytes.len() as u64));
        });
        self.read_bytes += bytes.len() as u64;
        if bytes.len() as u64 != metadata.len() {
            return Err(RecoveryArtifactError::InvalidPackageImports(
                path.to_path_buf(),
            ));
        }
        let sha256 = self.digest(&bytes);
        if &sha256 != expected_sha256 {
            return Err(RecoveryArtifactError::DigestMismatch(path.to_path_buf()));
        }
        if bytes.len() <= retain_limit.saturating_sub(self.retained_bytes) {
            self.retained_bytes += bytes.len();
            self.captured.insert(
                path.to_path_buf(),
                CapturedPackageInterface {
                    _bytes: bytes,
                    sha256,
                },
            );
        }
        Ok(())
    }
}

fn package_sidecar_path(interface_path: &Path) -> PathBuf {
    let mut path = interface_path.as_os_str().to_os_string();
    path.push(".packages");
    PathBuf::from(path)
}

fn certification_sidecar_path(interface_path: &Path) -> PathBuf {
    let mut path = interface_path.as_os_str().to_os_string();
    path.push(".owners");
    PathBuf::from(path)
}

fn certified_owners_path(digest: &[u8; 32]) -> PathBuf {
    PathBuf::from("artifacts").join(format!("{}.owners", hex(digest)))
}

fn execution_source_path(digest: &[u8; 32]) -> PathBuf {
    PathBuf::from("artifacts").join(format!("execution-{}.cbor", hex(digest)))
}

fn verify_execution_source(
    root: &Path,
    reference: &RecoveryExecutionSourceRef,
    producer: [u8; 32],
    owner: &CachedHomeOwner,
    validation: &mut PackageInterfaceValidation,
) -> Result<Arc<crate::execution_source::CertifiedExecutionSourceGraph>, RecoveryArtifactError> {
    if reference.sha256 == [0; 32] || reference.path != execution_source_path(&reference.sha256) {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let path = resolve_owned(root, &reference.path)?;
    let graph = if let Some(graph) = validation.execution_sources.get(&path) {
        if graph.digest() != reference.sha256 {
            return Err(RecoveryArtifactError::DigestMismatch(path));
        }
        graph.clone()
    } else {
        let file = File::open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > crate::execution_source::GRAPH_BYTES_LIMIT as u64
        {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        let mut bytes = Vec::new();
        file.take(crate::execution_source::GRAPH_BYTES_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        validation.read_bytes += bytes.len() as u64;
        if bytes.len() > crate::execution_source::GRAPH_BYTES_LIMIT {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        let digest = validation.digest(&bytes);
        if digest != reference.sha256 {
            return Err(RecoveryArtifactError::DigestMismatch(path));
        }
        validation.decoded_bytes += bytes.len() as u64;
        let graph =
            crate::execution_source::CertifiedExecutionSourceGraph::recover_verified(bytes, digest)
                .map_err(|_| RecoveryArtifactError::InvalidReference)?;
        if graph.bytes().len()
            <= PACKAGE_VALIDATION_RETAIN_LIMIT.saturating_sub(validation.retained_bytes)
        {
            validation.retained_bytes += graph.bytes().len();
            validation.execution_sources.insert(path, graph.clone());
        }
        graph
    };
    if graph.producer_sha256() != producer {
        return Err(RecoveryArtifactError::ExecutionSourceProducerMismatch {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            expected: producer,
            actual: graph.producer_sha256(),
        });
    }
    if !graph.eligible_source_replay_root(owner) {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    Ok(graph)
}

fn home_interface_path(owner: &CachedHomeOwner) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update((owner.unit.len() as u64).to_be_bytes());
    hasher.update(owner.unit.as_bytes());
    hasher.update((owner.module.len() as u64).to_be_bytes());
    hasher.update(owner.module.as_bytes());
    hasher.update(owner.module_version.0);
    hasher.update(owner.skinny_iface_sha256);
    hasher.update(owner.product_sha256);
    let identity: [u8; 32] = hasher.finalize().into();
    PathBuf::from("artifacts").join(format!("{}.hi", hex(&identity)))
}

fn home_product_path(owner: &CachedHomeOwner) -> PathBuf {
    PathBuf::from("artifacts").join(format!("{}.products.cbor", hex(&owner.product_sha256)))
}

fn ref_owner(reference: &RecoveryArtifactRef) -> CachedHomeOwner {
    CachedHomeOwner {
        unit: reference.unit.clone(),
        module: reference.module.clone(),
        module_version: tidepool_repr::execution_schema::ModuleVersion(reference.module_version),
        skinny_iface_sha256: reference.skinny_iface_sha256,
        product_sha256: reference.product_sha256,
    }
}

struct VerifiedCertification {
    bytes: Vec<u8>,
    execution_source_digest: Option<[u8; 32]>,
    module_certificate_digest: Option<[u8; 32]>,
}

fn read_certification(
    path: &Path,
    expected_sha256: Option<&[u8; 32]>,
    owner: &CachedHomeOwner,
    validation: &mut PackageInterfaceValidation,
) -> Result<VerifiedCertification, RecoveryArtifactError> {
    let metadata = fs::metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            RecoveryArtifactError::CertifiedOwnersUnavailable(path.to_path_buf())
        } else {
            RecoveryArtifactError::Io(error)
        }
    })?;
    if !metadata.is_file() || metadata.len() > CERTIFICATION_LIMIT {
        return Err(RecoveryArtifactError::InvalidCertifiedOwners(
            path.to_path_buf(),
        ));
    }
    let bytes = read_admission_bytes(path, CERTIFICATION_LIMIT, validation)?;
    validation.read_bytes += bytes.len() as u64;
    if bytes.len() as u64 > CERTIFICATION_LIMIT {
        return Err(RecoveryArtifactError::InvalidCertifiedOwners(
            path.to_path_buf(),
        ));
    }
    if expected_sha256.is_some_and(|expected| &validation.digest(&bytes) != expected) {
        return Err(RecoveryArtifactError::CertifiedOwnersDigestMismatch(
            path.to_path_buf(),
        ));
    }
    let (execution_source_digest, module_certificate_digest) =
        crate::certified_products::home_certification_digests_with_validation(
            &bytes, owner, validation,
        )
        .map_err(|_| RecoveryArtifactError::InvalidCertifiedOwners(path.to_path_buf()))?;
    Ok(VerifiedCertification {
        bytes,
        execution_source_digest,
        module_certificate_digest,
    })
}

fn read_package_imports(
    path: &Path,
    expected_sha256: Option<&[u8; 32]>,
    unit: &str,
    module: &str,
    skinny_iface_sha256: &[u8; 32],
    validation: &mut PackageInterfaceValidation,
) -> Result<Vec<u8>, RecoveryArtifactError> {
    let metadata = fs::metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            RecoveryArtifactError::Unavailable(path.to_path_buf())
        } else {
            RecoveryArtifactError::Io(error)
        }
    })?;
    if !metadata.is_file() || metadata.len() > PACKAGE_IMPORTS_LIMIT {
        return Err(RecoveryArtifactError::InvalidPackageImports(
            path.to_path_buf(),
        ));
    }
    let bytes = read_admission_bytes(path, PACKAGE_IMPORTS_LIMIT, validation)?;
    validation.read_bytes += bytes.len() as u64;
    if bytes.len() as u64 > PACKAGE_IMPORTS_LIMIT {
        return Err(RecoveryArtifactError::InvalidPackageImports(
            path.to_path_buf(),
        ));
    }
    if let Some(expected) = expected_sha256 {
        if &validation.digest(&bytes) != expected {
            return Err(RecoveryArtifactError::DigestMismatch(path.to_path_buf()));
        }
    }
    validate_package_imports_with_validation(
        &bytes,
        unit,
        module,
        skinny_iface_sha256,
        path,
        validation,
    )?;
    Ok(bytes)
}

pub(crate) fn validate_package_imports(
    bytes: &[u8],
    unit: &str,
    module: &str,
    skinny_iface_sha256: &[u8; 32],
    sidecar_path: &Path,
) -> Result<BTreeMap<(String, String), (PathBuf, String)>, RecoveryArtifactError> {
    validate_package_imports_with_validation(
        bytes,
        unit,
        module,
        skinny_iface_sha256,
        sidecar_path,
        &mut PackageInterfaceValidation::default(),
    )
}

pub(crate) fn validate_package_imports_with_validation(
    bytes: &[u8],
    unit: &str,
    module: &str,
    skinny_iface_sha256: &[u8; 32],
    sidecar_path: &Path,
    validation: &mut PackageInterfaceValidation,
) -> Result<BTreeMap<(String, String), (PathBuf, String)>, RecoveryArtifactError> {
    Ok(validate_package_import_evidence_with_validation(
        bytes,
        unit,
        module,
        skinny_iface_sha256,
        sidecar_path,
        validation,
    )?
    .roots)
}

/// Read-only facts issued by the package sidecar validator. Compiler-provided
/// imports remain distinct from package interfaces and native code authority.
pub(crate) struct ValidatedPackageImports {
    roots: BTreeMap<(String, String), (PathBuf, String)>,
    compiler_provided: Vec<CompilerProvidedImport>,
}

impl ValidatedPackageImports {
    pub(crate) fn roots(&self) -> &BTreeMap<(String, String), (PathBuf, String)> {
        &self.roots
    }

    pub(crate) fn compiler_provided(&self) -> &[CompilerProvidedImport] {
        &self.compiler_provided
    }
}

pub(crate) fn validate_package_import_evidence_with_validation(
    bytes: &[u8],
    unit: &str,
    module: &str,
    skinny_iface_sha256: &[u8; 32],
    sidecar_path: &Path,
    validation: &mut PackageInterfaceValidation,
) -> Result<ValidatedPackageImports, RecoveryArtifactError> {
    use ciborium::value::Value;

    let invalid = || RecoveryArtifactError::InvalidPackageImports(sidecar_path.to_path_buf());
    validation.decoded_bytes += bytes.len() as u64;
    let witness: Value = validation.inventory.decode_value(bytes, 32 << 20)?;
    validation.inventory.charge_value_copies(&witness, 2)?;
    validation.inventory.charge(bytes.len())?;
    let mut canonical = Vec::new();
    ciborium::ser::into_writer(&witness, &mut canonical).map_err(|_| invalid())?;
    if canonical != bytes {
        return Err(invalid());
    }
    let Value::Array(fields) = witness else {
        return Err(invalid());
    };
    if fields.first().and_then(Value::as_text) != Some("TPPKGROOTS") {
        return Err(invalid());
    }
    let version = fields.get(1).and_then(Value::as_text).ok_or_else(invalid)?;
    if version != "2" {
        return Err(RecoveryArtifactError::UnsupportedPackageImportsVersion {
            path: sidecar_path.into(),
            found: version.into(),
        });
    }
    if fields.len() != 5 {
        return Err(invalid());
    }
    let compiler_provided = decode_compiler_provided_imports(&fields[4]).map_err(|_| invalid())?;
    let Value::Array(owner) = &fields[2] else {
        return Err(invalid());
    };
    if owner.len() != 3
        || owner[0].as_text() != Some(unit)
        || owner[1].as_text() != Some(module)
        || owner[2].as_text() != Some(hex(skinny_iface_sha256).as_str())
    {
        return Err(invalid());
    }
    let Value::Array(roots) = &fields[3] else {
        return Err(invalid());
    };
    if roots.len() > PACKAGE_IMPORT_ROOT_LIMIT {
        return Err(invalid());
    }
    let mut selected = BTreeMap::new();
    for root in roots {
        let Value::Array(fields) = root else {
            return Err(invalid());
        };
        if fields.len() != 4 {
            return Err(invalid());
        }
        let Some(package_unit) = fields[0].as_text().filter(|text| !text.is_empty()) else {
            return Err(invalid());
        };
        let Some(package_module) = fields[1].as_text().filter(|text| !text.is_empty()) else {
            return Err(invalid());
        };
        let Some(package_path) = fields[2].as_text() else {
            return Err(invalid());
        };
        let package_path = Path::new(package_path);
        let Some(package_sha256) = fields[3].as_text() else {
            return Err(invalid());
        };
        if !package_path.is_absolute()
            || package_sha256.len() != 64
            || !package_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid());
        }
        if selected
            .insert(
                (package_unit.to_owned(), package_module.to_owned()),
                (package_path.to_path_buf(), package_sha256.to_owned()),
            )
            .is_some()
        {
            return Err(invalid());
        }
        let mut expected = [0; 32];
        for (index, pair) in package_sha256.as_bytes().chunks_exact(2).enumerate() {
            let digit = |byte: u8| {
                if byte <= b'9' {
                    byte - b'0'
                } else {
                    byte - b'a' + 10
                }
            };
            expected[index] = digit(pair[0]) * 16 + digit(pair[1]);
        }
        validation
            .verify(package_path, &expected)
            .map_err(|error| {
                if matches!(error, RecoveryArtifactError::InvalidPackageImports(_)) {
                    invalid()
                } else {
                    error
                }
            })?;
    }
    Ok(ValidatedPackageImports {
        roots: selected,
        compiler_provided,
    })
}

/// Closed compiler-owned input category. This attestation supplies neither a
/// package interface file nor native code authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompilerProvidedImport {
    Primitive,
}

pub(crate) fn decode_compiler_provided_imports(
    value: &ciborium::value::Value,
) -> Result<Vec<CompilerProvidedImport>, ()> {
    let rows = value.as_array().filter(|rows| rows.len() <= 1).ok_or(())?;
    rows.iter()
        .map(|row| {
            let row = row.as_array().filter(|row| row.len() == 3).ok_or(())?;
            // These fields are the identity of the positively tagged v2 category,
            // never a heuristic for classifying an ordinary package interface.
            if row[0].as_text() == Some("primitive")
                && row[1].as_text() == Some("ghc-prim")
                && row[2].as_text() == Some("GHC.Prim")
            {
                Ok(CompilerProvidedImport::Primitive)
            } else {
                Err(())
            }
        })
        .collect()
}

fn checked_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn resolve_owned(recovery_root: &Path, relative: &Path) -> Result<PathBuf, RecoveryArtifactError> {
    if !checked_relative(relative) {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let canonical_root = fs::canonicalize(recovery_root)?;
    let candidate = recovery_root.join(relative);
    let mut component = canonical_root.clone();
    for part in relative.components() {
        component.push(part);
        match fs::symlink_metadata(&component) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(RecoveryArtifactError::InvalidCapturedPayload(candidate));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(RecoveryArtifactError::Unavailable(candidate));
            }
            Err(error) => {
                return Err(RecoveryArtifactError::Unreadable {
                    path: candidate,
                    error,
                });
            }
        }
    }
    let canonical = match fs::canonicalize(&candidate) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(RecoveryArtifactError::Unavailable(candidate));
        }
        Err(error) => return Err(error.into()),
    };
    if !canonical.starts_with(&canonical_root) {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    Ok(canonical)
}

fn classify_certification_path_error(error: RecoveryArtifactError) -> RecoveryArtifactError {
    match error {
        RecoveryArtifactError::Unavailable(path) => {
            RecoveryArtifactError::CertifiedOwnersUnavailable(path)
        }
        other => other,
    }
}

fn read_admission_bytes(
    path: &Path,
    limit: u64,
    validation: &PackageInterfaceValidation,
) -> Result<Vec<u8>, RecoveryArtifactError> {
    let file = File::open(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            RecoveryArtifactError::Unavailable(path.to_path_buf())
        } else {
            RecoveryArtifactError::Unreadable {
                path: path.to_path_buf(),
                error,
            }
        }
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(RecoveryArtifactError::InvalidCapturedPayload(
            path.to_path_buf(),
        ));
    }
    validation.inventory.charge(metadata.len() as usize + 1)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize + 1);
    file.take(metadata.len() + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(RecoveryArtifactError::InvalidCapturedPayload(
            path.to_path_buf(),
        ));
    }
    Ok(bytes)
}

fn read_checked(
    path: &Path,
    expected: &[u8; 32],
    validation: &mut PackageInterfaceValidation,
) -> Result<Vec<u8>, RecoveryArtifactError> {
    let bytes = read_admission_bytes(path, 32 << 20, validation)?;
    validation.read_bytes += bytes.len() as u64;
    if &validation.digest(&bytes) != expected {
        return Err(RecoveryArtifactError::DigestMismatch(path.to_path_buf()));
    }
    Ok(bytes)
}

fn materialize_copy_with_validation(
    path: &Path,
    bytes: &[u8],
    digest: &[u8; 32],
    mode: MaterializationMode,
    validation: &mut PackageInterfaceValidation,
) -> Result<(), RecoveryArtifactError> {
    reject_symlink(path)?;
    if verify_existing_materialization(path, bytes.len(), digest, mode, validation)? {
        return Ok(());
    }
    match mode {
        MaterializationMode::Durable => {
            tidepool_atomic_write::write_durable_new(path, bytes).map_err(io::Error::from)?;
            validation.written_bytes += bytes.len() as u64;
        }
        MaterializationMode::Scratch => {
            let mut temporary = tempfile::NamedTempFile::new_in(
                path.parent()
                    .ok_or(RecoveryArtifactError::InvalidReference)?,
            )?;
            temporary.write_all(bytes)?;
            validation.written_bytes += bytes.len() as u64;
            match temporary.persist_noclobber(path) {
                Ok(_) => {}
                Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.error.into()),
            }
        }
    }
    reject_symlink(path)?;
    if verify_existing_materialization(path, bytes.len(), digest, mode, validation)? {
        Ok(())
    } else {
        Err(RecoveryArtifactError::Unavailable(path.to_path_buf()))
    }
}

#[cfg(test)]
fn materialize_copy(
    path: &Path,
    bytes: &[u8],
    digest: &[u8; 32],
    mode: MaterializationMode,
) -> Result<(), RecoveryArtifactError> {
    materialize_copy_with_validation(
        path,
        bytes,
        digest,
        mode,
        &mut PackageInterfaceValidation::default(),
    )
}

#[cfg(test)]
fn durable_copy(path: &Path, bytes: &[u8], digest: &[u8; 32]) -> Result<(), RecoveryArtifactError> {
    materialize_copy(path, bytes, digest, MaterializationMode::Durable)
}

fn verify_existing_materialization(
    path: &Path,
    expected_len: usize,
    digest: &[u8; 32],
    mode: MaterializationMode,
    validation: &mut PackageInterfaceValidation,
) -> Result<bool, RecoveryArtifactError> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    if metadata.len() != expected_len as u64 {
        return Err(RecoveryArtifactError::DigestMismatch(path.to_path_buf()));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; expected_len.clamp(1, 64 * 1024)];
    let mut remaining = metadata.len();
    while remaining != 0 {
        let limit = buffer.len().min(remaining as usize);
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(RecoveryArtifactError::DigestMismatch(path.to_path_buf()));
        }
        validation.read_bytes += read as u64;
        validation.hash_bytes += read as u64;
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    let trailing = file.read(&mut buffer[..1])?;
    validation.read_bytes += trailing as u64;
    if trailing != 0 || hasher.finalize().as_slice() != digest {
        return Err(RecoveryArtifactError::DigestMismatch(path.to_path_buf()));
    }
    let current = fs::symlink_metadata(path)?;
    if !current.is_file() {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
            return Err(RecoveryArtifactError::InvalidReference);
        }
    }
    // A readable existing file is not evidence that a previous durable
    // publication completed. Durable callers confirm it and its directory.
    if mode == MaterializationMode::Durable {
        file.sync_all()?;
        tidepool_atomic_write::sync_parent_directory(path).map_err(io::Error::from)?;
    }
    Ok(true)
}

fn reject_symlink(path: &Path) -> Result<(), RecoveryArtifactError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(RecoveryArtifactError::InvalidReference)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// The materialization owner controls this directory; checking its
/// canonical identity before writing rejects preexisting redirects.
fn prepare_owned_directory(
    recovery_root: &Path,
    mode: MaterializationMode,
) -> Result<PathBuf, RecoveryArtifactError> {
    let canonical_root = fs::canonicalize(recovery_root)?;
    let owned = recovery_root.join("artifacts");
    match fs::symlink_metadata(&owned) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(&owned) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        },
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(&owned)?;
    if !metadata.file_type().is_dir() {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let canonical_owned = fs::canonicalize(&owned)?;
    if canonical_owned != canonical_root.join("artifacts") {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    if mode == MaterializationMode::Durable {
        File::open(&canonical_root)?.sync_all()?;
    }
    Ok(canonical_owned)
}

/// Seal a validated source-less Joined interface in the run's durable root.
pub fn materialize_joined_interface(
    recovery_root: &Path,
    toolchain_identity_sha256: [u8; 32],
    unit: &str,
    module: &str,
    interface_source: &Path,
    skinny_iface_sha256: [u8; 32],
) -> Result<RecoveryJoinRef, RecoveryArtifactError> {
    materialize_joined_interface_with_validation(
        recovery_root,
        toolchain_identity_sha256,
        unit,
        module,
        interface_source,
        skinny_iface_sha256,
        &mut PackageInterfaceValidation::default(),
    )
}

pub(crate) fn materialize_joined_interface_with_validation(
    recovery_root: &Path,
    toolchain_identity_sha256: [u8; 32],
    unit: &str,
    module: &str,
    interface_source: &Path,
    skinny_iface_sha256: [u8; 32],
    validation: &mut PackageInterfaceValidation,
) -> Result<RecoveryJoinRef, RecoveryArtifactError> {
    if toolchain_identity_sha256 == [0; 32] || unit.is_empty() || module.is_empty() {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let bytes = read_checked(interface_source, &skinny_iface_sha256, validation)?;
    let package_imports_source = package_sidecar_path(interface_source);
    let package_imports = read_package_imports(
        &package_imports_source,
        None,
        unit,
        module,
        &skinny_iface_sha256,
        validation,
    )?;
    materialize_owned_join(
        recovery_root,
        toolchain_identity_sha256,
        unit,
        module,
        &bytes,
        &package_imports,
        validation,
        MaterializationMode::Durable,
    )
}

fn materialize_owned_join(
    recovery_root: &Path,
    toolchain_identity_sha256: [u8; 32],
    unit: &str,
    module: &str,
    bytes: &[u8],
    package_imports: &[u8],
    validation: &mut PackageInterfaceValidation,
    mode: MaterializationMode,
) -> Result<RecoveryJoinRef, RecoveryArtifactError> {
    if toolchain_identity_sha256 == [0; 32] || unit.is_empty() || module.is_empty() {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let skinny_iface_sha256 = validation.digest(bytes);
    validate_package_imports_with_validation(
        package_imports,
        unit,
        module,
        &skinny_iface_sha256,
        Path::new("owned-join.hi.packages"),
        validation,
    )?;
    let package_imports_sha256 = validation.digest(package_imports);
    let owned = prepare_owned_directory(recovery_root, mode)?;
    let interface_path =
        PathBuf::from("artifacts").join(format!("{}.joined.hi", hex(&skinny_iface_sha256)));
    let package_imports_path = package_sidecar_path(&interface_path);
    materialize_copy_with_validation(
        &owned.join(
            interface_path
                .file_name()
                .ok_or(RecoveryArtifactError::InvalidReference)?,
        ),
        bytes,
        &skinny_iface_sha256,
        mode,
        validation,
    )?;
    materialize_copy_with_validation(
        &owned.join(
            package_imports_path
                .file_name()
                .ok_or(RecoveryArtifactError::InvalidReference)?,
        ),
        package_imports,
        &package_imports_sha256,
        mode,
        validation,
    )?;
    if mode == MaterializationMode::Durable {
        File::open(&owned)?.sync_all()?;
    }
    Ok(RecoveryJoinRef {
        toolchain_identity_sha256,
        unit: unit.to_owned(),
        module: module.to_owned(),
        skinny_iface_sha256,
        interface_path,
        package_imports_path,
        package_imports_sha256,
    })
}

pub(crate) fn materialize_module_interface(
    root: &Path,
    interface: &crate::certified_products::CertifiedModuleInterface,
    validation: &mut PackageInterfaceValidation,
    mode: MaterializationMode,
) -> Result<RecoveryModuleInterfaceRef, RecoveryArtifactError> {
    let anchor = materialize_owned_join(
        root,
        interface.producer_sha256(),
        interface.unit(),
        interface.module(),
        interface.interface_bytes(),
        interface.package_imports_bytes(),
        validation,
        mode,
    )?;
    let certificate_sha256 = validation.digest(interface.certificate_bytes());
    let certificate_path =
        PathBuf::from("artifacts").join(format!("{}.finalized.cbor", hex(&certificate_sha256)));
    let owned = prepare_owned_directory(root, mode)?;
    materialize_copy_with_validation(
        &root.join(&certificate_path),
        interface.certificate_bytes(),
        &certificate_sha256,
        mode,
        validation,
    )?;
    let core = interface
        .core_bytes()
        .map(|bytes| {
            let sha256 = validation.digest(bytes);
            let path = PathBuf::from("artifacts").join(format!("{}.finalized.core", hex(&sha256)));
            materialize_copy_with_validation(&root.join(&path), bytes, &sha256, mode, validation)?;
            Ok::<_, RecoveryArtifactError>(RecoveryCoreRef {
                path,
                sha256,
                bytes: bytes.len() as u64,
            })
        })
        .transpose()?;
    if mode == MaterializationMode::Durable {
        File::open(&owned)?.sync_all()?;
    }
    Ok(RecoveryModuleInterfaceRef {
        interface: anchor,
        certificate_path,
        certificate_sha256,
        core,
    })
}

pub(crate) fn capture_module_payload(
    root: &Path,
    relative: &Path,
    expected: &[u8; 32],
    size: Option<u64>,
    limit: u64,
    validation: &mut PackageInterfaceValidation,
) -> Result<Vec<u8>, RecoveryArtifactError> {
    let path = resolve_owned(root, relative)?;
    let file = File::open(&path).map_err(|error| RecoveryArtifactError::Unreadable {
        path: path.clone(),
        error,
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| RecoveryArtifactError::Unreadable {
            path: path.clone(),
            error,
        })?;
    if !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > limit
        || size.is_some_and(|size| size != metadata.len())
    {
        return Err(RecoveryArtifactError::InvalidCapturedPayload(path));
    }
    validation.inventory.charge(metadata.len() as usize + 1)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize + 1);
    file.take(metadata.len() + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| RecoveryArtifactError::Unreadable {
            path: path.clone(),
            error,
        })?;
    validation.read_bytes += bytes.len() as u64;
    if bytes.len() as u64 != metadata.len()
        || size.is_some_and(|size| size != bytes.len() as u64)
        || validation.digest(&bytes) != *expected
    {
        return Err(RecoveryArtifactError::DigestMismatch(path));
    }
    Ok(bytes)
}

pub(crate) fn recover_module_interface(
    root: &Path,
    reference: &RecoveryModuleInterfaceRef,
    validation: &mut PackageInterfaceValidation,
) -> Result<crate::certified_products::CertifiedModuleInterface, RecoveryArtifactError> {
    let anchor = &reference.interface;
    let interface_bytes = capture_module_payload(
        root,
        &anchor.interface_path,
        &anchor.skinny_iface_sha256,
        None,
        PACKAGE_INTERFACE_LIMIT,
        validation,
    )?;
    let package_imports_bytes = capture_module_payload(
        root,
        &anchor.package_imports_path,
        &anchor.package_imports_sha256,
        None,
        PACKAGE_IMPORTS_LIMIT,
        validation,
    )?;
    validate_package_imports_with_validation(
        &package_imports_bytes,
        &anchor.unit,
        &anchor.module,
        &anchor.skinny_iface_sha256,
        &root.join(&anchor.package_imports_path),
        validation,
    )?;
    let certificate = capture_module_payload(
        root,
        &reference.certificate_path,
        &reference.certificate_sha256,
        None,
        CERTIFICATION_LIMIT,
        validation,
    )?;
    let core = reference
        .core
        .as_ref()
        .map(|core| {
            capture_module_payload(
                root,
                &core.path,
                &core.sha256,
                Some(core.bytes),
                PACKAGE_INTERFACE_LIMIT,
                validation,
            )
        })
        .transpose()?;
    let interface = crate::certified_products::recover_module_interface(
        reference.interface.toolchain_identity_sha256,
        certificate,
        interface_bytes,
        package_imports_bytes,
        core,
        validation,
    )
    .map_err(|_| {
        RecoveryArtifactError::InvalidModuleCertificate(root.join(&reference.certificate_path))
    })?;
    if interface.unit() != anchor.unit
        || interface.module() != anchor.module
        || interface.producer_sha256() != anchor.toolchain_identity_sha256
        || interface.interface_sha256() != anchor.skinny_iface_sha256
        || interface.package_imports_sha256() != anchor.package_imports_sha256
    {
        return Err(RecoveryArtifactError::InvalidModuleCertificate(
            root.join(&reference.certificate_path),
        ));
    }
    Ok(interface)
}

/// Materialize compiler-certified original module products into the run-owned
/// closure. The bundle owns its bytes, so no worker scratch path survives here.
pub fn materialize_certified_products(
    recovery_root: &Path,
    toolchain_identity_sha256: [u8; 32],
    products: &[CertifiedRecoveryProduct],
) -> Result<Vec<RecoveryArtifactRef>, RecoveryArtifactError> {
    materialize_certified_products_with_work(
        recovery_root,
        toolchain_identity_sha256,
        products,
        &mut RecoveryArtifactWork::default(),
    )
}

pub fn materialize_certified_products_with_work(
    recovery_root: &Path,
    toolchain_identity_sha256: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    work: &mut RecoveryArtifactWork,
) -> Result<Vec<RecoveryArtifactRef>, RecoveryArtifactError> {
    with_artifact_work(work, |validation| {
        materialize_certified_products_with_validation(
            recovery_root,
            toolchain_identity_sha256,
            products,
            validation,
            MaterializationMode::Durable,
        )
    })
}

pub(crate) fn materialize_certified_products_with_validation(
    recovery_root: &Path,
    toolchain_identity_sha256: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    validation: &mut PackageInterfaceValidation,
    mode: MaterializationMode,
) -> Result<Vec<RecoveryArtifactRef>, RecoveryArtifactError> {
    if toolchain_identity_sha256 == [0; 32] {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let owned = prepare_owned_directory(recovery_root, mode)?;
    let mut source_graphs = BTreeMap::new();
    for product in products {
        if let Some(graph) = product.execution_source() {
            if graph.producer_sha256() != toolchain_identity_sha256 {
                return Err(RecoveryArtifactError::ExecutionSourceProducerMismatch {
                    unit: product.owner().unit.clone(),
                    module: product.owner().module.clone(),
                    expected: toolchain_identity_sha256,
                    actual: graph.producer_sha256(),
                });
            }
            if !graph.eligible_source_replay_root(product.owner()) {
                return Err(RecoveryArtifactError::InvalidReference);
            }
            if let Some(previous) = source_graphs.insert(graph.digest(), graph.clone()) {
                if !Arc::ptr_eq(&previous, graph) && previous.bytes() != graph.bytes() {
                    return Err(RecoveryArtifactError::InvalidReference);
                }
            }
        }
    }
    for (digest, graph) in &source_graphs {
        let path = execution_source_path(digest);
        materialize_copy_with_validation(
            &owned.join(
                path.file_name()
                    .ok_or(RecoveryArtifactError::InvalidReference)?,
            ),
            graph.bytes(),
            digest,
            mode,
            validation,
        )?;
    }
    let mut refs = Vec::with_capacity(products.len());
    for product in products {
        let owner = &product.owner;
        if owner.unit.is_empty() || owner.module.is_empty() {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        if validation.digest(&product.interface_bytes) != owner.skinny_iface_sha256
            || validation.digest(&product.product_bytes) != owner.product_sha256
        {
            return Err(RecoveryArtifactError::DigestMismatch(home_interface_path(
                owner,
            )));
        }
        let interface_path = home_interface_path(owner);
        let package_imports_path = package_sidecar_path(&interface_path);
        let product_path = home_product_path(owner);
        if product.package_imports_bytes.len() as u64 > PACKAGE_IMPORTS_LIMIT {
            return Err(RecoveryArtifactError::InvalidPackageImports(
                package_imports_path,
            ));
        }
        validate_package_imports_with_validation(
            &product.package_imports_bytes,
            &owner.unit,
            &owner.module,
            &owner.skinny_iface_sha256,
            &package_imports_path,
            validation,
        )?;
        if product.certification_bytes.len() as u64 > CERTIFICATION_LIMIT {
            return Err(RecoveryArtifactError::InvalidCertifiedOwners(
                certification_sidecar_path(&interface_path),
            ));
        }
        // One native owner can retain multiple authenticated source contexts.
        // Its ownership certificates therefore have independent byte identity.
        let certification_sha256: [u8; 32] = validation.digest(&product.certification_bytes);
        let certification_path = certified_owners_path(&certification_sha256);
        let source_digest =
            crate::certified_products::home_execution_source_digest_with_validation(
                &product.certification_bytes,
                owner,
                validation,
            )
            .map_err(|_| {
                RecoveryArtifactError::InvalidCertifiedOwners(certification_path.clone())
            })?;
        if source_digest != product.execution_source().map(|graph| graph.digest()) {
            return Err(RecoveryArtifactError::InvalidCertifiedOwners(
                certification_path,
            ));
        }
        let execution_source = source_digest.map(|sha256| RecoveryExecutionSourceRef {
            path: execution_source_path(&sha256),
            sha256,
        });
        let package_imports_sha256: [u8; 32] = validation.digest(&product.package_imports_bytes);
        materialize_copy_with_validation(
            &owned.join(
                interface_path
                    .file_name()
                    .ok_or(RecoveryArtifactError::InvalidReference)?,
            ),
            &product.interface_bytes,
            &owner.skinny_iface_sha256,
            mode,
            validation,
        )?;
        materialize_copy_with_validation(
            &owned.join(
                product_path
                    .file_name()
                    .ok_or(RecoveryArtifactError::InvalidReference)?,
            ),
            &product.product_bytes,
            &owner.product_sha256,
            mode,
            validation,
        )?;
        materialize_copy_with_validation(
            &owned.join(
                package_imports_path
                    .file_name()
                    .ok_or(RecoveryArtifactError::InvalidReference)?,
            ),
            &product.package_imports_bytes,
            &package_imports_sha256,
            mode,
            validation,
        )?;
        materialize_copy_with_validation(
            &owned.join(
                certification_path
                    .file_name()
                    .ok_or(RecoveryArtifactError::InvalidReference)?,
            ),
            &product.certification_bytes,
            &certification_sha256,
            mode,
            validation,
        )?;
        refs.push(RecoveryArtifactRef {
            toolchain_identity_sha256,
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            module_version: owner.module_version.0,
            skinny_iface_sha256: owner.skinny_iface_sha256,
            product_sha256: owner.product_sha256,
            interface_path,
            package_imports_path,
            package_imports_sha256,
            certification_path,
            certification_sha256,
            product_path,
            module_interface: Some(materialize_module_interface(
                recovery_root,
                product
                    .module_interface()
                    .ok_or(RecoveryArtifactError::InvalidReference)?,
                validation,
                mode,
            )?),
            execution_source,
        });
    }
    if mode == MaterializationMode::Durable {
        File::open(&owned)?.sync_all()?;
    }
    Ok(refs)
}

/// Read captured path inputs into owned bundles, then use the same immutable
/// validation and publication path as compiler-certified products.
pub fn materialize_recovery_closure(
    recovery_root: &Path,
    toolchain_identity_sha256: [u8; 32],
    artifacts: &[RecoveryArtifactInput<'_>],
) -> Result<Vec<RecoveryArtifactRef>, RecoveryArtifactError> {
    let mut validation = PackageInterfaceValidation::default();
    let mut products = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let owner = artifact.owner;
        if owner.unit.is_empty() || owner.module.is_empty() {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        let interface_bytes = read_checked(
            artifact.interface_source,
            &owner.skinny_iface_sha256,
            &mut validation,
        )?;
        let product_bytes = read_checked(
            artifact.product_source,
            &owner.product_sha256,
            &mut validation,
        )?;
        let package_imports_bytes = read_package_imports(
            &package_sidecar_path(artifact.interface_source),
            None,
            &owner.unit,
            &owner.module,
            &owner.skinny_iface_sha256,
            &mut validation,
        )?;
        let certification_bytes = read_certification(
            &certification_sidecar_path(artifact.interface_source),
            None,
            owner,
            &mut validation,
        )?;
        let module_interface = recover_module_interface(
            artifact.module_interface.0,
            artifact.module_interface.1,
            &mut validation,
        )?;
        products.push(
            CertifiedRecoveryProduct::from_certification(
                owner.clone(),
                interface_bytes,
                product_bytes,
                package_imports_bytes,
                certification_bytes.bytes,
            )
            .with_module_interface(module_interface)?,
        );
    }
    materialize_certified_products_with_validation(
        recovery_root,
        toolchain_identity_sha256,
        &products,
        &mut validation,
        MaterializationMode::Durable,
    )
}

/// Verify path confinement and both immutable bytes before using a durable
/// recovery ref. The captured bytes let a caller avoid a second unbound read.
pub fn verify_materialized_ref(
    recovery_root: &Path,
    reference: &RecoveryArtifactRef,
) -> Result<VerifiedRecoveryArtifact, RecoveryArtifactError> {
    verify_materialized_ref_with_work(
        recovery_root,
        reference,
        &mut RecoveryArtifactWork::default(),
    )
}

pub fn verify_materialized_ref_with_work(
    recovery_root: &Path,
    reference: &RecoveryArtifactRef,
    work: &mut RecoveryArtifactWork,
) -> Result<VerifiedRecoveryArtifact, RecoveryArtifactError> {
    with_recovery_artifact_verification(recovery_root, work, |verification| {
        verification.verify_home(reference)
    })
}

pub(crate) fn verify_materialized_ref_with_validation(
    recovery_root: &Path,
    reference: &RecoveryArtifactRef,
    validation: &mut PackageInterfaceValidation,
) -> Result<VerifiedRecoveryArtifact, RecoveryArtifactError> {
    let interface = recover_module_interface(
        recovery_root,
        reference
            .module_interface
            .as_ref()
            .ok_or(RecoveryArtifactError::InvalidReference)?,
        validation,
    )?;
    verify_materialized_ref_with_module_interface(recovery_root, reference, &interface, validation)
}

pub(crate) fn verify_materialized_ref_with_module_interface(
    recovery_root: &Path,
    reference: &RecoveryArtifactRef,
    canonical: &crate::certified_products::CertifiedModuleInterface,
    validation: &mut PackageInterfaceValidation,
) -> Result<VerifiedRecoveryArtifact, RecoveryArtifactError> {
    if reference.toolchain_identity_sha256 == [0; 32]
        || reference.unit.is_empty()
        || reference.module.is_empty()
        || !checked_relative(&reference.interface_path)
        || !checked_relative(&reference.package_imports_path)
        || !checked_relative(&reference.certification_path)
        || !checked_relative(&reference.product_path)
        || reference.package_imports_path != package_sidecar_path(&reference.interface_path)
        || reference.certification_path != certified_owners_path(&reference.certification_sha256)
    {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let owner = ref_owner(reference);
    if reference.interface_path != home_interface_path(&owner)
        || reference.product_path != home_product_path(&owner)
    {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let interface_path = resolve_owned(recovery_root, &reference.interface_path)?;
    let product_path = resolve_owned(recovery_root, &reference.product_path)?;
    let interface_bytes =
        read_checked(&interface_path, &reference.skinny_iface_sha256, validation)?;
    let package_imports_path = resolve_owned(recovery_root, &reference.package_imports_path)?;
    let package_imports_bytes = read_package_imports(
        &package_imports_path,
        Some(&reference.package_imports_sha256),
        &reference.unit,
        &reference.module,
        &reference.skinny_iface_sha256,
        validation,
    )?;
    let certification_path = resolve_owned(recovery_root, &reference.certification_path)
        .map_err(classify_certification_path_error)?;
    let certification = read_certification(
        &certification_path,
        Some(&reference.certification_sha256),
        &owner,
        validation,
    )?;
    if certification.module_certificate_digest
        != reference
            .module_interface
            .as_ref()
            .map(|reference| reference.certificate_sha256)
    {
        return Err(RecoveryArtifactError::InvalidCertifiedOwners(
            certification_path,
        ));
    }
    if certification.execution_source_digest
        != reference
            .execution_source
            .as_ref()
            .map(|source| source.sha256)
    {
        return Err(RecoveryArtifactError::InvalidCertifiedOwners(
            certification_path,
        ));
    }
    let execution_source = reference
        .execution_source
        .as_ref()
        .map(|source| {
            verify_execution_source(
                recovery_root,
                source,
                reference.toolchain_identity_sha256,
                &owner,
                validation,
            )
        })
        .transpose()?;
    let product_bytes = read_checked(&product_path, &reference.product_sha256, validation)?;
    let module_interface = canonical.clone();
    let canonical_ref = reference
        .module_interface
        .as_ref()
        .ok_or(RecoveryArtifactError::InvalidReference)?;
    if validation.digest(module_interface.certificate_bytes()) != canonical_ref.certificate_sha256 {
        return Err(RecoveryArtifactError::InvalidModuleCertificate(
            recovery_root.join(&canonical_ref.certificate_path),
        ));
    }
    if module_interface.producer_sha256() != reference.toolchain_identity_sha256
        || module_interface.unit() != reference.unit
        || module_interface.module() != reference.module
        || module_interface.interface_bytes() != interface_bytes
        || module_interface.package_imports_bytes() != package_imports_bytes
    {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    Ok(VerifiedRecoveryArtifact {
        module_interface,
        reference: reference.clone(),
        interface_path,
        package_imports_path,
        product_path,
        interface_bytes,
        package_imports_bytes,
        certification_path,
        certification_bytes: certification.bytes,
        product_bytes,
        execution_source,
    })
}

/// Check a sealed Joined interface independently of retained implementation
/// products, returning the exact captured bytes used for rehydration.
pub fn verify_materialized_join(
    recovery_root: &Path,
    reference: &RecoveryJoinRef,
) -> Result<VerifiedRecoveryJoin, RecoveryArtifactError> {
    verify_materialized_join_with_work(
        recovery_root,
        reference,
        &mut RecoveryArtifactWork::default(),
    )
}

pub fn verify_materialized_join_with_work(
    recovery_root: &Path,
    reference: &RecoveryJoinRef,
    work: &mut RecoveryArtifactWork,
) -> Result<VerifiedRecoveryJoin, RecoveryArtifactError> {
    with_recovery_artifact_verification(recovery_root, work, |verification| {
        verification.verify_join(reference)
    })
}

pub(crate) fn verify_materialized_join_with_validation(
    recovery_root: &Path,
    reference: &RecoveryJoinRef,
    validation: &mut PackageInterfaceValidation,
) -> Result<VerifiedRecoveryJoin, RecoveryArtifactError> {
    if reference.toolchain_identity_sha256 == [0; 32]
        || reference.unit.is_empty()
        || reference.module.is_empty()
        || !checked_relative(&reference.interface_path)
        || !checked_relative(&reference.package_imports_path)
        || reference.package_imports_path != package_sidecar_path(&reference.interface_path)
    {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let interface_path = resolve_owned(recovery_root, &reference.interface_path)?;
    let interface_bytes =
        read_checked(&interface_path, &reference.skinny_iface_sha256, validation)?;
    let package_imports_path = resolve_owned(recovery_root, &reference.package_imports_path)?;
    let package_imports_bytes = read_package_imports(
        &package_imports_path,
        Some(&reference.package_imports_sha256),
        &reference.unit,
        &reference.module,
        &reference.skinny_iface_sha256,
        validation,
    )?;
    Ok(VerifiedRecoveryJoin {
        reference: reference.clone(),
        interface_path,
        package_imports_path,
        interface_bytes,
        package_imports_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ciborium::value::Value;
    use tidepool_repr::execution_schema::ModuleVersion;

    #[test]
    fn same_native_owner_preserves_distinct_source_context_certificates() {
        let source = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let (first_graph, owners) = crate::execution_source::test_graph(source.path());
        let second_graph = crate::execution_source::test_graph_with_large_origin(&first_graph, 80);
        assert_ne!(first_graph.digest(), second_graph.digest());
        let owner = &owners[0];
        let packages = package_witness(
            &owner.unit,
            &owner.module,
            &owner.skinny_iface_sha256,
            vec![],
        );
        let canonical = crate::certified_products::fixture_interface_bytes(
            [7; 32],
            &owner.unit,
            &owner.module,
            b"iface".to_vec(),
            packages.clone(),
        );
        let seal = crate::certified_products::encode_home_certification_with_module(
            owner,
            &[],
            &BTreeMap::new(),
            &BTreeMap::new(),
            Sha256::digest(canonical.certificate_bytes()).into(),
        )
        .unwrap();
        let products = [first_graph, second_graph].map(|graph| {
            let certification = crate::certified_products::bind_home_execution_source(
                &seal,
                owner,
                graph.digest(),
                &mut PackageInterfaceValidation::default(),
            )
            .unwrap();
            CertifiedRecoveryProduct::from_certification(
                owner.clone(),
                b"iface".to_vec(),
                b"product".to_vec(),
                packages.clone(),
                certification,
            )
            .with_module_interface(canonical.clone())
            .unwrap()
            .with_execution_source(graph)
            .unwrap()
        });
        assert_eq!(products[0].owner(), products[1].owner());
        assert_ne!(
            products[0].certification_bytes(),
            products[1].certification_bytes()
        );
        let references = products.each_ref().map(|product| {
            materialize_certified_products(run.path(), [7; 32], std::slice::from_ref(product))
                .unwrap()
                .remove(0)
        });
        assert_eq!(references[0].interface_path, references[1].interface_path);
        assert_eq!(references[0].product_path, references[1].product_path);
        assert_ne!(
            references[0].certification_path,
            references[1].certification_path
        );
        for (reference, product) in references.iter().zip(&products) {
            let verified = verify_materialized_ref(run.path(), reference).unwrap();
            assert_eq!(verified.certification_bytes, product.certification_bytes());
            assert_eq!(
                reference.certification_path,
                certified_owners_path(&Sha256::digest(product.certification_bytes()).into())
            );
        }
        let mut substituted = references[0].clone();
        substituted.certification_path = references[1].certification_path.clone();
        substituted.certification_sha256 = references[1].certification_sha256;
        assert!(matches!(
            verify_materialized_ref(run.path(), &substituted),
            Err(RecoveryArtifactError::InvalidCertifiedOwners(_))
        ));
        fs::write(
            run.path().join(&references[0].certification_path),
            b"corrupt",
        )
        .unwrap();
        assert!(matches!(
            verify_materialized_ref(run.path(), &references[0]),
            Err(RecoveryArtifactError::CertifiedOwnersDigestMismatch(_))
        ));
        assert!(matches!(
            materialize_certified_products(run.path(), [7; 32], &products[..1]),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
        assert!(verify_materialized_ref(run.path(), &references[1]).is_ok());
    }

    #[test]
    fn execution_source_sidecar_is_shared_and_verified_once() {
        let source = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let (graph, owners) = crate::execution_source::test_graph(source.path());
        let mut validation = PackageInterfaceValidation::default();
        let products = owners[..2]
            .iter()
            .map(|owner| {
                let seal = crate::certified_products::encode_home_certification(
                    owner,
                    &[],
                    &BTreeMap::new(),
                )
                .unwrap();
                let seal = crate::certified_products::bind_home_execution_source(
                    &seal,
                    owner,
                    graph.digest(),
                    &mut validation,
                )
                .unwrap();
                CertifiedRecoveryProduct::from_certification(
                    owner.clone(),
                    b"iface".to_vec(),
                    b"product".to_vec(),
                    package_witness(
                        &owner.unit,
                        &owner.module,
                        &owner.skinny_iface_sha256,
                        vec![],
                    ),
                    seal,
                )
                .with_execution_source(graph.clone())
                .unwrap()
            })
            .collect::<Vec<_>>();
        let mut materialization = PackageInterfaceValidation::default();
        let references = materialize_certified_products_with_validation(
            run.path(),
            [7; 32],
            &products,
            &mut materialization,
            MaterializationMode::Durable,
        )
        .unwrap();
        assert_eq!(materialization.home_witness_validations, products.len());
        assert_eq!(
            references[0].execution_source,
            references[1].execution_source
        );
        assert_eq!(
            fs::read_dir(run.path().join("artifacts"))
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("execution-")
                })
                .count(),
            1
        );
        let mut validation = PackageInterfaceValidation::default();
        let first =
            verify_materialized_ref_with_validation(run.path(), &references[0], &mut validation)
                .unwrap();
        let second =
            verify_materialized_ref_with_validation(run.path(), &references[1], &mut validation)
                .unwrap();
        assert!(Arc::ptr_eq(
            first.execution_source.as_ref().unwrap(),
            second.execution_source.as_ref().unwrap()
        ));
        assert_eq!(validation.execution_sources.len(), 1);
        assert_eq!(validation.home_witness_validations, references.len());
        let mut graph_validation = PackageInterfaceValidation::default();
        for (reference, owner) in references.iter().zip(&owners) {
            verify_execution_source(
                run.path(),
                reference.execution_source.as_ref().unwrap(),
                [7; 32],
                owner,
                &mut graph_validation,
            )
            .unwrap();
        }
        assert_eq!(graph_validation.hash_bytes, graph.bytes().len() as u64);
        let mut uncaptured = PackageInterfaceValidation {
            retained_bytes: PACKAGE_VALIDATION_RETAIN_LIMIT,
            ..Default::default()
        };
        for (reference, owner) in references.iter().zip(&owners) {
            verify_execution_source(
                run.path(),
                reference.execution_source.as_ref().unwrap(),
                [7; 32],
                owner,
                &mut uncaptured,
            )
            .unwrap();
        }
        assert!(uncaptured.execution_sources.is_empty());
        assert_eq!(uncaptured.hash_bytes, 2 * graph.bytes().len() as u64);
        fs::remove_file(source.path().join("A.hs")).unwrap();
        assert!(verify_materialized_ref(run.path(), &references[0]).is_ok());
        let mut mismatched = references[0].clone();
        mismatched.execution_source = None;
        assert!(verify_materialized_ref(run.path(), &mismatched).is_err());
        let path = run
            .path()
            .join(&references[0].execution_source.as_ref().unwrap().path);
        fs::write(&path, b"corrupt").unwrap();
        assert!(matches!(
            verify_materialized_ref(run.path(), &references[0]),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
        fs::remove_file(&path).unwrap();
        assert!(
            verify_materialized_ref(run.path(), &references[0]).is_err(),
            "a declared but missing supplement is corruption"
        );
    }

    #[test]
    fn execution_source_sidecar_rejects_wrong_owner_producer_and_malformed_bytes() {
        let source = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let (graph, owners) = crate::execution_source::test_graph(source.path());
        let owned = prepare_owned_directory(run.path(), MaterializationMode::Scratch).unwrap();
        let reference = RecoveryExecutionSourceRef {
            path: execution_source_path(&graph.digest()),
            sha256: graph.digest(),
        };
        fs::write(
            owned.join(reference.path.file_name().unwrap()),
            graph.bytes(),
        )
        .unwrap();
        let mut other = owners[0].clone();
        other.module_version = ModuleVersion([99; 32]);
        assert!(verify_execution_source(
            run.path(),
            &reference,
            [7; 32],
            &other,
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
        assert!(verify_execution_source(
            run.path(),
            &reference,
            [8; 32],
            &owners[0],
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
        let digest = Sha256::digest(b"malformed").into();
        let malformed = RecoveryExecutionSourceRef {
            path: execution_source_path(&digest),
            sha256: digest,
        };
        fs::write(
            owned.join(malformed.path.file_name().unwrap()),
            b"malformed",
        )
        .unwrap();
        assert!(verify_execution_source(
            run.path(),
            &malformed,
            [7; 32],
            &owners[0],
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
    }

    #[test]
    fn package_import_v2_marker_is_not_a_file_and_refuses_legacy_or_unknown_identity() {
        let bytes = package_witness("main", "Owner", &[7; 32], vec![]);
        let mut value: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        value.as_array_mut().unwrap()[4] = Value::Array(vec![Value::Array(vec![
            text("primitive"),
            text("ghc-prim"),
            text("GHC.Prim"),
        ])]);
        let encode = |value: &Value| {
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(value, &mut bytes).unwrap();
            bytes
        };
        let path = Path::new("marker.packages");
        assert!(
            validate_package_imports(&encode(&value), "main", "Owner", &[7; 32], path)
                .unwrap()
                .is_empty()
        );
        assert!(
            validate_package_imports(&encode(&value), "main", "AnotherOwner", &[7; 32], path)
                .is_err()
        );
        assert!(
            validate_package_imports(&encode(&value), "main", "Owner", &[8; 32], path).is_err()
        );
        let mut legacy = value.clone();
        legacy.as_array_mut().unwrap()[1] = text("1");
        legacy.as_array_mut().unwrap().pop();
        assert!(
            matches!(validate_package_imports(&encode(&legacy), "main", "Owner", &[7; 32], path), Err(RecoveryArtifactError::UnsupportedPackageImportsVersion { found, .. }) if found == "1")
        );
        let mut unknown = value.clone();
        unknown.as_array_mut().unwrap()[4].as_array_mut().unwrap()[0]
            .as_array_mut()
            .unwrap()[2] = text("GHC.Prim.Ext");
        assert!(matches!(
            validate_package_imports(&encode(&unknown), "main", "Owner", &[7; 32], path),
            Err(RecoveryArtifactError::InvalidPackageImports(_))
        ));
        unknown.as_array_mut().unwrap()[4].as_array_mut().unwrap()[0]
            .as_array_mut()
            .unwrap()[0] = text("unknown");
        assert!(matches!(
            validate_package_imports(&encode(&unknown), "main", "Owner", &[7; 32], path),
            Err(RecoveryArtifactError::InvalidPackageImports(_))
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "fault-injection child; invoked by materialization_modes_preserve_durable_sync"]
    fn materialization_sync_fault_child() {
        let root = PathBuf::from(std::env::var_os("MATERIALIZATION_FAULT_ROOT").unwrap());
        let mode = match std::env::var("MATERIALIZATION_FAULT_MODE")
            .unwrap()
            .as_str()
        {
            "scratch" => MaterializationMode::Scratch,
            "durable" => MaterializationMode::Durable,
            other => panic!("unknown materialization mode {other}"),
        };
        let digest: [u8; 32] = Sha256::digest(b"owned").into();
        let operation = std::env::var("MATERIALIZATION_OPERATION").unwrap();
        let result = if operation == "copy" {
            materialize_copy(&root.join("value"), b"owned", &digest, mode)
                .map_err(|error| error.to_string())
        } else {
            use crate::declaration_context::ExactDeclarationContext;
            use crate::declaration_join::ExactModuleIdentity;
            let producer = b"materialization-test-producer";
            let value = Arc::new(
                CertifiedValueInterface::from_checked_compilation(
                    Sha256::digest(producer).into(),
                    ExactModuleIdentity {
                        unit: "main".into(),
                        module: "Tidepool.Session.Val.G1".into(),
                    },
                    b"owned".to_vec(),
                    package_witness("main", "Tidepool.Session.Val.G1", &digest, Vec::new()),
                    Vec::new(),
                )
                .unwrap(),
            );
            let empty = Arc::new(ExactDeclarationContext::new(&[], &[], Vec::new()).unwrap());
            let context = Arc::new(
                (*empty)
                    .clone()
                    .extend_program_value_interface(value)
                    .unwrap(),
            );
            match operation.as_str() {
                "request" => context
                    .prepare_compilation(&root.join("request"), producer)
                    .map(|_| ()),
                "growth" => empty
                    .prepare_compilation_with_authorization(
                        &root.join("request"),
                        producer,
                        Some(ciborium::value::Value::Null),
                    )
                    .and_then(|request| request.in_program_context(&root.join("growth"), context))
                    .map(|_| ()),
                "public" => context.materialize(&root).map(|_| ()),
                "inspection" => {
                    let directory = tempfile::tempdir_in(&root).unwrap();
                    context.materialize_scratch(&directory).map(|_| ())
                }
                other => panic!("unknown operation {other}"),
            }
            .map_err(|error| error.to_string())
        };
        let expected_ok = std::env::var("MATERIALIZATION_EXPECT_OK").unwrap() == "yes";
        assert_eq!(result.is_ok(), expected_ok, "{result:?}");
        if expected_ok && operation == "copy" {
            assert_eq!(fs::read(root.join("value")).unwrap(), b"owned");
            assert_eq!(fs::read_dir(root).unwrap().count(), 1);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "short synchronous C fault-library compilation and child test invocation"
    )]
    fn materialization_modes_preserve_durable_sync() {
        use std::process::Command;
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("fault.c");
        fs::write(
            &source,
            include_str!("../tests/fixtures/materialization-fault.c"),
        )
        .unwrap();
        let library = temporary.path().join("fault.so");
        assert!(Command::new("cc")
            .args(["-shared", "-fPIC", "-Wall", "-Werror"])
            .arg(source)
            .arg("-o")
            .arg(&library)
            .arg("-ldl")
            .status()
            .unwrap()
            .success());
        for (index, (operation, mode, existing, fault, ok, file_syncs, directory_syncs)) in [
            ("copy", "scratch", false, "trace", true, 0, 0),
            ("copy", "durable", false, "trace", true, 2, 2),
            ("copy", "scratch", true, "trace", true, 0, 0),
            ("copy", "durable", true, "trace", true, 1, 1),
            ("copy", "scratch", false, "file", true, 0, 0),
            ("copy", "durable", false, "file", false, 1, 0),
            ("copy", "scratch", true, "directory", true, 0, 0),
            ("copy", "durable", true, "directory", false, 1, 1),
            ("request", "scratch", false, "file", true, 0, 0),
            ("growth", "scratch", false, "file", true, 0, 0),
            ("public", "durable", false, "trace", true, 4, 6),
            ("public", "durable", false, "directory", false, 0, 1),
            ("inspection", "scratch", false, "directory", true, 0, 0),
        ]
        .into_iter()
        .enumerate()
        {
            let root = temporary.path().join(index.to_string());
            fs::create_dir(&root).unwrap();
            if existing {
                // A readable winner is deliberately not an established durable receipt.
                fs::write(root.join("value"), b"owned").unwrap();
            }
            let log = temporary.path().join(format!("syncs-{index}"));
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "recovery_artifacts::tests::materialization_sync_fault_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("LD_PRELOAD", &library)
                .env("MATERIALIZATION_FAULT_ROOT", &root)
                .env("MATERIALIZATION_FAULT_KIND", fault)
                .env("MATERIALIZATION_FAULT_LOG", &log)
                .env("MATERIALIZATION_FAULT_MODE", mode)
                .env("MATERIALIZATION_OPERATION", operation)
                .env("MATERIALIZATION_EXPECT_OK", if ok { "yes" } else { "no" })
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed; 0 failed"),
                "{operation}/{mode}/{existing}/{fault}: {stdout} {}",
                String::from_utf8_lossy(&output.stderr),
            );
            let syncs = fs::read_to_string(log).unwrap_or_default();
            assert_eq!(
                syncs.lines().filter(|line| *line == "file").count(),
                file_syncs
            );
            assert_eq!(
                syncs.lines().filter(|line| *line == "directory").count(),
                directory_syncs,
            );
        }
    }

    #[test]
    #[ignore = "requires an exact retained original declaration packet"]
    fn retained_original_durable_copy_cost() {
        use std::time::Instant;

        fn io_counters() -> (u64, u64, u64) {
            let counters = fs::read_to_string("/proc/thread-self/io").unwrap();
            let value = |key: &str| {
                counters
                    .lines()
                    .find_map(|line| line.strip_prefix(key))
                    .unwrap()
                    .trim()
                    .parse::<u64>()
                    .unwrap()
            };
            (value("wchar:"), value("syscw:"), value("write_bytes:"))
        }

        let packet = PathBuf::from(std::env::var_os("TIDEPOOL_PACKAGE_VALIDATION_PACKET").unwrap());
        for (name, repetitions) in [
            ("original.hi", vec![1, 10, 100]),
            ("module-products.cbor", vec![2]),
        ] {
            let bytes = fs::read(packet.join(name)).unwrap();
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            for count in repetitions {
                let root = tempfile::tempdir().unwrap();
                let destination = root.path().join(name);
                let creation_before = io_counters();
                let creation_started = Instant::now();
                durable_copy(&destination, &bytes, &digest).unwrap();
                let creation_elapsed = creation_started.elapsed();
                let creation_after = io_counters();
                println!(
                    "DURABLE_COPY_CREATE_COST {}",
                    serde_json::json!({
                        "artifact": name, "artifact_sha256": hex(&digest), "artifact_bytes": bytes.len(),
                        "following_existing_repetitions": count, "nanoseconds": creation_elapsed.as_nanos(),
                        "thread_wchar": creation_after.0-creation_before.0, "thread_write_syscalls": creation_after.1-creation_before.1,
                        "thread_write_bytes": creation_after.2-creation_before.2,
                        "qualification": "first creation plus full verification and durability, measured separately from reuse"
                    })
                );
                let before = io_counters();
                let started = Instant::now();
                for _ in 0..count {
                    durable_copy(&destination, &bytes, &digest).unwrap();
                }
                let elapsed = started.elapsed();
                let after = io_counters();
                assert_eq!(fs::read(&destination).unwrap(), bytes);
                assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
                println!(
                    "DURABLE_COPY_COST {}",
                    serde_json::json!({
                        "artifact": name, "artifact_sha256": hex(&digest), "artifact_bytes": bytes.len(),
                        "existing_repetitions": count, "nanoseconds": elapsed.as_nanos(),
                        "thread_wchar": after.0-before.0, "thread_write_syscalls": after.1-before.1,
                        "thread_write_bytes": after.2-before.2,
                        "qualification": "actual captured payload; first publication excluded, exact existing path reused; no compiler/native authority issued"
                    })
                );
            }
        }
    }

    #[test]
    fn materialization_modes_existing_payload_refuses_wrong_digest_bytes_and_symlink() {
        for mode in [MaterializationMode::Scratch, MaterializationMode::Durable] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("owned.cbor");
            let digest: [u8; 32] = Sha256::digest(b"owned").into();
            materialize_copy(&path, b"owned", &digest, mode).unwrap();
            materialize_copy(&path, b"owned", &digest, mode).unwrap();
            assert!(matches!(
                materialize_copy(&path, b"owned", &[0; 32], mode),
                Err(RecoveryArtifactError::DigestMismatch(_))
            ));
            fs::write(&path, b"other").unwrap();
            assert!(matches!(
                materialize_copy(&path, b"owned", &digest, mode),
                Err(RecoveryArtifactError::DigestMismatch(_))
            ));
            assert_eq!(fs::read(&path).unwrap(), b"other");
            fs::write(&path, b"different-length").unwrap();
            assert!(materialize_copy(&path, b"owned", &digest, mode).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"different-length");
            #[cfg(unix)]
            {
                let target = root.path().join("target.cbor");
                fs::write(&target, b"owned").unwrap();
                fs::remove_file(&path).unwrap();
                std::os::unix::fs::symlink(&target, &path).unwrap();
                assert!(matches!(
                    materialize_copy(&path, b"owned", &digest, mode),
                    Err(RecoveryArtifactError::InvalidReference)
                ));
                assert_eq!(fs::read(&target).unwrap(), b"owned");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn materialization_modes_nonregular_fifo_refuses_without_blocking() {
        for mode in [MaterializationMode::Scratch, MaterializationMode::Durable] {
            use std::os::unix::ffi::OsStrExt;
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("fifo");
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: the C string is terminated and valid for the call; the path
            // is a new entry in this test's private directory.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            let digest: [u8; 32] = Sha256::digest(b"owned").into();
            assert!(matches!(
                materialize_copy(&path, b"owned", &digest, mode),
                Err(RecoveryArtifactError::InvalidReference)
            ));
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
            assert!(matches!(
                materialize_copy(root.path(), b"owned", &digest, mode),
                Err(RecoveryArtifactError::InvalidReference)
            ));
        }
    }

    #[test]
    fn materialization_modes_racing_publishers_verify_the_actual_winner() {
        for mode in [MaterializationMode::Scratch, MaterializationMode::Durable] {
            use std::sync::{Arc, Barrier};
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("same.cbor");
            let digest: [u8; 32] = Sha256::digest(b"owned").into();
            let barrier = Arc::new(Barrier::new(2));
            let threads: Vec<_> = (0..2)
                .map(|_| {
                    let path = path.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        materialize_copy(&path, b"owned", &digest, mode)
                    })
                })
                .collect();
            for thread in threads {
                thread.join().unwrap().unwrap();
            }
            assert_eq!(fs::read(&path).unwrap(), b"owned");
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);

            let path = root.path().join("mixed.cbor");
            let barrier = Arc::new(Barrier::new(2));
            let wrong = {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    tidepool_atomic_write::write_durable_new(&path, b"other").unwrap()
                })
            };
            let correct = {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    materialize_copy(&path, b"owned", &digest, mode)
                })
            };
            if wrong.join().unwrap() {
                assert!(matches!(
                    correct.join().unwrap(),
                    Err(RecoveryArtifactError::DigestMismatch(_))
                ));
                assert_eq!(fs::read(&path).unwrap(), b"other");
            } else {
                correct.join().unwrap().unwrap();
                assert_eq!(fs::read(&path).unwrap(), b"owned");
            }
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
        }
    }

    #[test]
    fn package_payload_read_work_does_not_depend_on_optional_expected_digest() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Owner.hi.packages");
        let bytes = package_witness("main", "Owner", &[7; 32], vec![]);
        fs::write(&path, &bytes).unwrap();
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        for expected in [None, Some(&digest)] {
            let mut work = RecoveryArtifactWork::default();
            let actual = with_artifact_work(&mut work, |validation| {
                read_package_imports(&path, expected, "main", "Owner", &[7; 32], validation)
            })
            .unwrap();
            assert_eq!(actual, bytes);
            assert_eq!(work.read_bytes, bytes.len() as u64);
            assert_eq!(work.decoded_bytes, bytes.len() as u64);
            assert_eq!(work.written_bytes, 0);
            assert_eq!(
                work.hash_bytes,
                if expected.is_some() {
                    bytes.len() as u64
                } else {
                    0
                }
            );
        }
    }

    #[test]
    fn artifact_work_counts_consumed_bytes_and_preserves_validation_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("payload");
        let bytes = b"abc";
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        let mut work = RecoveryArtifactWork::default();
        with_artifact_work(&mut work, |validation| {
            materialize_copy_with_validation(
                &path,
                bytes,
                &digest,
                MaterializationMode::Scratch,
                validation,
            )
        })
        .unwrap();
        assert_eq!(
            work.hash_bytes, 3,
            "new file verification hashes exactly the stored bytes"
        );
        assert_eq!(work.read_bytes, 3);
        assert_eq!(work.written_bytes, 3);
        assert_eq!(work.decoded_bytes, 0);
        with_artifact_work(&mut work, |validation| {
            materialize_copy_with_validation(
                &path,
                bytes,
                &digest,
                MaterializationMode::Scratch,
                validation,
            )
        })
        .unwrap();
        assert_eq!(work.hash_bytes, 6, "existing bytes are freshly verified");
        assert_eq!(work.read_bytes, 6);
        assert_eq!(
            work.written_bytes, 3,
            "existing materialization performs no payload write"
        );
        let refused = with_artifact_work(&mut work, |validation| {
            materialize_copy_with_validation(
                &path,
                bytes,
                &[0; 32],
                MaterializationMode::Scratch,
                validation,
            )
        });
        assert!(matches!(
            refused,
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
        assert_eq!(
            work.hash_bytes, 9,
            "a refused digest still consumed actual hash input"
        );
        fs::write(&path, b"").unwrap();
        assert!(with_artifact_work(&mut work, |validation| {
            materialize_copy_with_validation(
                &path,
                bytes,
                &digest,
                MaterializationMode::Scratch,
                validation,
            )
        })
        .is_err());
        assert_eq!(work.hash_bytes, 9, "length refusal precedes hashing");
        assert_eq!(work.read_bytes, 9);
        assert_eq!(work.written_bytes, 3);
        fs::write(&path, bytes).unwrap();
        let mut cached = PackageInterfaceValidation::default();
        cached.verify(&path, &digest).unwrap();
        cached.verify(&path, &digest).unwrap();
        assert_eq!(
            cached.hash_bytes, 3,
            "existing within-stage capture avoids a second hash"
        );
        assert_eq!(
            cached.read_bytes, 3,
            "stage capture also avoids another payload read"
        );
        let mut uncached = PackageInterfaceValidation::default();
        uncached.verify_with_budget(&path, &digest, 0).unwrap();
        uncached.verify_with_budget(&path, &digest, 0).unwrap();
        assert_eq!(
            uncached.hash_bytes, 6,
            "budget fallback measures both real file hashes"
        );
    }

    #[test]
    fn package_validation_stage_keeps_one_capture_and_refuses_conflicting_digest() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("package.hi");
        fs::write(&path, b"first").unwrap();
        let first: [u8; 32] = Sha256::digest(b"first").into();
        let second: [u8; 32] = Sha256::digest(b"second").into();
        let mut stage = PackageInterfaceValidation::default();
        stage.verify(&path, &first).unwrap();
        fs::write(&path, b"second").unwrap();
        stage.verify(&path, &first).unwrap();
        assert_eq!(stage.captured[&path]._bytes, b"first");
        assert!(matches!(
            stage.verify(&path, &second),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
        assert!(PackageInterfaceValidation::default()
            .verify(&path, &first)
            .is_err());
        PackageInterfaceValidation::default()
            .verify(&path, &second)
            .unwrap();
    }

    #[test]
    fn package_validation_new_stage_rechecks_path_and_inode_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("package.hi");
        let replacement = root.path().join("replacement.hi");
        fs::write(&path, b"first").unwrap();
        fs::write(&replacement, b"other").unwrap();
        let first: [u8; 32] = Sha256::digest(b"first").into();
        let mut stage = PackageInterfaceValidation::default();
        stage.verify(&path, &first).unwrap();
        fs::rename(&replacement, &path).unwrap();
        stage.verify(&path, &first).unwrap();
        assert!(PackageInterfaceValidation::default()
            .verify(&path, &first)
            .is_err());
        #[cfg(unix)]
        {
            fs::write(&replacement, b"first").unwrap();
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&replacement, &path).unwrap();
            PackageInterfaceValidation::default()
                .verify(&path, &first)
                .unwrap();
            fs::write(&replacement, b"other").unwrap();
            assert!(PackageInterfaceValidation::default()
                .verify(&path, &first)
                .is_err());
        }
    }

    #[test]
    fn package_validation_budget_falls_back_to_fresh_reads() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("package.hi");
        fs::write(&path, b"first").unwrap();
        let first: [u8; 32] = Sha256::digest(b"first").into();
        let mut stage = PackageInterfaceValidation::default();
        stage.verify_with_budget(&path, &first, 0).unwrap();
        assert!(stage.captured.is_empty());
        fs::write(&path, b"other").unwrap();
        assert!(stage.verify_with_budget(&path, &first, 0).is_err());
        assert_eq!(stage.retained_bytes, 0);
    }

    #[test]
    fn package_validation_keeps_regular_file_and_size_bounds() {
        let root = tempfile::tempdir().unwrap();
        let mut stage = PackageInterfaceValidation::default();
        assert!(stage.verify(root.path(), &[0; 32]).is_err());
        assert!(stage.verify(Path::new("relative.hi"), &[0; 32]).is_err());
        assert!(stage
            .verify(&root.path().join("absent.hi"), &[0; 32])
            .is_err());
        let path = root.path().join("oversized.hi");
        File::create(&path)
            .unwrap()
            .set_len(PACKAGE_INTERFACE_LIMIT + 1)
            .unwrap();
        assert!(stage.verify(&path, &[0; 32]).is_err());
        assert!(stage.captured.is_empty());
    }

    fn text(value: impl Into<String>) -> ciborium::value::Value {
        ciborium::value::Value::Text(value.into())
    }

    fn package_witness(
        unit: &str,
        module: &str,
        iface_sha256: &[u8; 32],
        roots: Vec<[String; 4]>,
    ) -> Vec<u8> {
        let root_values = roots
            .into_iter()
            .map(|root| ciborium::value::Value::Array(root.into_iter().map(text).collect()))
            .collect();
        let value = ciborium::value::Value::Array(vec![
            text("TPPKGROOTS"),
            text("2"),
            ciborium::value::Value::Array(vec![text(unit), text(module), text(hex(iface_sha256))]),
            ciborium::value::Value::Array(root_values),
            Value::Array(vec![]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn write_interface_and_sidecar(
        interface: &Path,
        unit: &str,
        module: &str,
        contents: &[u8],
    ) -> [u8; 32] {
        fs::write(interface, contents).unwrap();
        let iface_sha256: [u8; 32] = Sha256::digest(contents).into();
        fs::write(
            package_sidecar_path(interface),
            package_witness(unit, module, &iface_sha256, Vec::new()),
        )
        .unwrap();
        iface_sha256
    }

    fn fixture_module_reference(
        interface: &Path,
        owner: &CachedHomeOwner,
    ) -> RecoveryModuleInterfaceRef {
        let bytes = fs::read(interface).unwrap();
        let packages = fs::read(package_sidecar_path(interface))
            .ok()
            .filter(|bytes| {
                validate_package_imports(
                    bytes,
                    &owner.unit,
                    &owner.module,
                    &owner.skinny_iface_sha256,
                    interface,
                )
                .is_ok()
            })
            .unwrap_or_else(|| {
                package_witness(
                    &owner.unit,
                    &owner.module,
                    &owner.skinny_iface_sha256,
                    Vec::new(),
                )
            });
        let proof = crate::certified_products::fixture_interface_bytes(
            [1; 32],
            &owner.unit,
            &owner.module,
            bytes,
            packages,
        );
        materialize_module_interface(
            interface.parent().unwrap(),
            &proof,
            &mut PackageInterfaceValidation::default(),
            MaterializationMode::Durable,
        )
        .unwrap()
    }

    fn write_certification(interface: &Path, owner: &CachedHomeOwner) -> Vec<u8> {
        let canonical = fixture_module_reference(interface, owner);
        let bytes = crate::certified_products::encode_home_certification_with_module(
            owner,
            &[],
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            canonical.certificate_sha256,
        )
        .unwrap();
        fs::write(certification_sidecar_path(interface), &bytes).unwrap();
        bytes
    }

    #[test]
    fn materialized_pair_is_confined_and_checksum_verified() {
        let run = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let iface = source.path().join("A.hi");
        let product = source.path().join("products.cbor");
        let iface_sha256 = write_interface_and_sidecar(&iface, "home", "A", b"iface");
        fs::write(&product, b"product").unwrap();
        let owner = CachedHomeOwner {
            unit: "home".into(),
            module: "A".into(),
            module_version: ModuleVersion([3; 32]),
            skinny_iface_sha256: iface_sha256,
            product_sha256: Sha256::digest(b"product").into(),
        };
        write_certification(&iface, &owner);
        let refs = materialize_recovery_closure(
            run.path(),
            [1; 32],
            &[RecoveryArtifactInput {
                owner: &owner,
                interface_source: &iface,
                module_interface: (source.path(), &fixture_module_reference(&iface, &owner)),
                product_source: &product,
            }],
        )
        .unwrap();
        let verified = verify_materialized_ref(run.path(), &refs[0]).unwrap();
        assert_eq!(verified.interface_bytes, b"iface");
        assert!(!verified.package_imports_bytes.is_empty());
        assert!(!verified.certification_bytes.is_empty());
        assert_eq!(
            verified.reference.certification_path,
            certified_owners_path(&verified.reference.certification_sha256)
        );
        assert!(verified
            .reference
            .package_imports_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".hi.packages")));
        let mut escaped = refs[0].clone();
        escaped.product_path = PathBuf::from("../outside");
        assert!(matches!(
            verify_materialized_ref(run.path(), &escaped),
            Err(RecoveryArtifactError::InvalidReference)
        ));
        let mut misplaced = refs[0].clone();
        misplaced.product_path = PathBuf::from("artifacts/other.products.cbor");
        assert!(matches!(
            verify_materialized_ref(run.path(), &misplaced),
            Err(RecoveryArtifactError::InvalidReference)
        ));
        misplaced = refs[0].clone();
        misplaced.certification_path = PathBuf::from("artifacts/other.hi.owners");
        assert!(matches!(
            verify_materialized_ref(run.path(), &misplaced),
            Err(RecoveryArtifactError::InvalidReference)
        ));
        misplaced.certification_path = certification_sidecar_path(&misplaced.interface_path);
        assert!(matches!(
            verify_materialized_ref(run.path(), &misplaced),
            Err(RecoveryArtifactError::InvalidReference)
        ));
        fs::write(&verified.product_path, b"tampered").unwrap();
        assert!(matches!(
            verify_materialized_ref(run.path(), &refs[0]),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
        fs::write(&verified.package_imports_path, b"tampered").unwrap();
        assert!(matches!(
            verify_materialized_ref(run.path(), &refs[0]),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
        fs::write(
            &verified.package_imports_path,
            &verified.package_imports_bytes,
        )
        .unwrap();
        fs::write(&verified.product_path, &verified.product_bytes).unwrap();
        fs::remove_file(&verified.certification_path).unwrap();
        assert!(matches!(
            verify_materialized_ref(run.path(), &refs[0]),
            Err(RecoveryArtifactError::CertifiedOwnersUnavailable(_))
        ));
        fs::write(&verified.certification_path, &verified.certification_bytes).unwrap();
        fs::write(&verified.certification_path, b"tampered").unwrap();
        assert!(matches!(
            verify_materialized_ref(run.path(), &refs[0]),
            Err(RecoveryArtifactError::CertifiedOwnersDigestMismatch(_))
        ));
        let mut legacy = serde_json::to_value(&refs[0]).unwrap();
        let record = legacy.as_object_mut().unwrap();
        record.remove("certification_path");
        record.remove("certification_sha256");
        assert!(serde_json::from_value::<RecoveryArtifactRef>(legacy).is_err());
    }

    #[test]
    fn same_interface_bytes_keep_distinct_home_owner_certificates() {
        let run = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let iface = source.path().join("A.hi");
        let iface_sha256 = write_interface_and_sidecar(&iface, "home", "A", b"same iface");
        let first_product = source.path().join("first.products.cbor");
        let second_product = source.path().join("second.products.cbor");
        fs::write(&first_product, b"first product").unwrap();
        fs::write(&second_product, b"second product").unwrap();
        let first = CachedHomeOwner {
            unit: "home".into(),
            module: "A".into(),
            module_version: ModuleVersion([3; 32]),
            skinny_iface_sha256: iface_sha256,
            product_sha256: Sha256::digest(b"first product").into(),
        };
        let second = CachedHomeOwner {
            module_version: ModuleVersion([4; 32]),
            ..first.clone()
        };
        let third = CachedHomeOwner {
            product_sha256: Sha256::digest(b"second product").into(),
            ..first.clone()
        };
        assert_ne!(
            home_interface_path(&first),
            home_interface_path(&CachedHomeOwner {
                unit: "other-home".into(),
                ..first.clone()
            })
        );
        assert_ne!(
            home_interface_path(&first),
            home_interface_path(&CachedHomeOwner {
                module: "Other".into(),
                ..first.clone()
            })
        );
        let first_certification = write_certification(&iface, &first);
        fs::write(certification_sidecar_path(&iface), &first_certification).unwrap();
        let first_ref = materialize_recovery_closure(
            run.path(),
            [1; 32],
            &[RecoveryArtifactInput {
                owner: &first,
                interface_source: &iface,
                module_interface: (source.path(), &fixture_module_reference(&iface, &first)),
                product_source: &first_product,
            }],
        )
        .unwrap()
        .remove(0);
        assert!(verify_materialized_ref(run.path(), &first_ref).is_ok());

        let second_certification = write_certification(&iface, &second);
        fs::write(certification_sidecar_path(&iface), &second_certification).unwrap();
        let second_ref = materialize_recovery_closure(
            run.path(),
            [1; 32],
            &[RecoveryArtifactInput {
                owner: &second,
                interface_source: &iface,
                module_interface: (source.path(), &fixture_module_reference(&iface, &second)),
                product_source: &first_product,
            }],
        )
        .unwrap()
        .remove(0);
        assert!(verify_materialized_ref(run.path(), &second_ref).is_ok());
        assert_ne!(first_ref.interface_path, second_ref.interface_path);
        assert_ne!(first_ref.certification_path, second_ref.certification_path);
        assert_eq!(
            verify_materialized_ref(run.path(), &first_ref)
                .unwrap()
                .certification_bytes,
            first_certification
        );

        write_certification(&iface, &third);
        let third_ref = materialize_recovery_closure(
            run.path(),
            [1; 32],
            &[RecoveryArtifactInput {
                owner: &third,
                interface_source: &iface,
                module_interface: (source.path(), &fixture_module_reference(&iface, &third)),
                product_source: &second_product,
            }],
        )
        .unwrap()
        .remove(0);
        assert!(verify_materialized_ref(run.path(), &third_ref).is_ok());
        assert_ne!(first_ref.interface_path, third_ref.interface_path);
        assert!(verify_materialized_ref(run.path(), &first_ref).is_ok());
        assert!(verify_materialized_ref(run.path(), &second_ref).is_ok());
    }

    #[test]
    fn conflicting_witness_cannot_replace_an_existing_owner_reference() {
        let run = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let iface = source.path().join("A.hi");
        let iface_sha256 = write_interface_and_sidecar(&iface, "home", "A", b"same iface");
        let product = source.path().join("A.products.cbor");
        fs::write(&product, b"same product").unwrap();
        let owner = CachedHomeOwner {
            unit: "home".into(),
            module: "A".into(),
            module_version: ModuleVersion([3; 32]),
            skinny_iface_sha256: iface_sha256,
            product_sha256: Sha256::digest(b"same product").into(),
        };
        write_certification(&iface, &owner);
        let input = [RecoveryArtifactInput {
            owner: &owner,
            interface_source: &iface,
            module_interface: (source.path(), &fixture_module_reference(&iface, &owner)),
            product_source: &product,
        }];
        let first = materialize_recovery_closure(run.path(), [1; 32], &input)
            .unwrap()
            .remove(0);
        let original = verify_materialized_ref(run.path(), &first).unwrap();
        assert_eq!(
            materialize_recovery_closure(run.path(), [1; 32], &input)
                .unwrap()
                .remove(0),
            first
        );

        let package_iface = source.path().join("base.hi");
        fs::write(&package_iface, b"package interface").unwrap();
        let package_sha256: [u8; 32] = Sha256::digest(b"package interface").into();
        fs::write(
            package_sidecar_path(&iface),
            package_witness(
                "home",
                "A",
                &iface_sha256,
                vec![[
                    "base-unit".into(),
                    "Data.Base".into(),
                    package_iface.display().to_string(),
                    hex(&package_sha256),
                ]],
            ),
        )
        .unwrap();
        assert!(matches!(
            materialize_recovery_closure(run.path(), [1; 32], &input),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
        let retained = verify_materialized_ref(run.path(), &first).unwrap();
        assert_eq!(
            retained.package_imports_bytes,
            original.package_imports_bytes
        );
        assert_eq!(retained.certification_bytes, original.certification_bytes);
    }

    #[test]
    fn joined_interface_has_no_dummy_product() {
        let run = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let iface = source.path().join("Join.hi");
        let digest = write_interface_and_sidecar(&iface, "home", "Joined", b"joined");
        let reference =
            materialize_joined_interface(run.path(), [1; 32], "home", "Joined", &iface, digest)
                .unwrap();
        let verified = verify_materialized_join(run.path(), &reference).unwrap();
        assert_eq!(verified.interface_bytes, b"joined");
        assert!(!verified.package_imports_bytes.is_empty());
        assert_eq!(reference.interface_path.extension().unwrap(), "hi");
        let mut escaped = reference.clone();
        escaped.interface_path = PathBuf::from("../outside");
        assert!(matches!(
            verify_materialized_join(run.path(), &escaped),
            Err(RecoveryArtifactError::InvalidReference)
        ));
        fs::write(&verified.package_imports_path, b"corrupt").unwrap();
        assert!(matches!(
            verify_materialized_join(run.path(), &reference),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn materialization_rejects_redirected_artifact_directory_before_writing() {
        use std::os::unix::fs::symlink;

        let run = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        symlink(outside.path(), run.path().join("artifacts")).unwrap();

        let iface = source.path().join("A.hi");
        let iface_sha256 = write_interface_and_sidecar(&iface, "home", "A", b"iface");
        let product_path = source.path().join("A.products.cbor");
        fs::write(&product_path, b"product").unwrap();
        let owner = CachedHomeOwner {
            unit: "home".into(),
            module: "A".into(),
            module_version: ModuleVersion([3; 32]),
            skinny_iface_sha256: iface_sha256,
            product_sha256: Sha256::digest(b"product").into(),
        };
        let certification = write_certification(&iface, &owner);
        let product = CertifiedRecoveryProduct::from_certification(
            owner.clone(),
            b"iface".to_vec(),
            b"product".to_vec(),
            fs::read(package_sidecar_path(&iface)).unwrap(),
            certification,
        );
        assert!(matches!(
            materialize_certified_products(run.path(), [1; 32], &[product]),
            Err(RecoveryArtifactError::InvalidReference)
        ));
        assert!(matches!(
            materialize_recovery_closure(
                run.path(),
                [1; 32],
                &[RecoveryArtifactInput {
                    owner: &owner,
                    interface_source: &iface,
                    module_interface: (source.path(), &fixture_module_reference(&iface, &owner)),
                    product_source: &product_path,
                }],
            ),
            Err(RecoveryArtifactError::InvalidReference)
        ));
        assert!(matches!(
            materialize_joined_interface(run.path(), [1; 32], "home", "A", &iface, iface_sha256),
            Err(RecoveryArtifactError::InvalidReference)
        ));
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[test]
    fn package_sidecar_binds_owner_and_selected_package_bytes() {
        let run = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let iface = source.path().join("A.hi");
        fs::write(&iface, b"iface").unwrap();
        let iface_sha256: [u8; 32] = Sha256::digest(b"iface").into();
        let package_iface = source.path().join("base.hi");
        fs::write(&package_iface, b"package interface").unwrap();
        let package_sha256: [u8; 32] = Sha256::digest(b"package interface").into();
        let package_root = [
            "base-unit".to_owned(),
            "Data.Base".to_owned(),
            package_iface.display().to_string(),
            hex(&package_sha256),
        ];
        fs::write(
            package_sidecar_path(&iface),
            package_witness("home", "A", &iface_sha256, vec![package_root]),
        )
        .unwrap();
        let product = source.path().join("products.cbor");
        fs::write(&product, b"product").unwrap();
        let owner = CachedHomeOwner {
            unit: "home".into(),
            module: "A".into(),
            module_version: ModuleVersion([3; 32]),
            skinny_iface_sha256: iface_sha256,
            product_sha256: Sha256::digest(b"product").into(),
        };
        let certification = write_certification(&iface, &owner);
        let input = [RecoveryArtifactInput {
            owner: &owner,
            interface_source: &iface,
            module_interface: (source.path(), &fixture_module_reference(&iface, &owner)),
            product_source: &product,
        }];
        let refs = materialize_recovery_closure(run.path(), [1; 32], &input).unwrap();
        assert!(verify_materialized_ref(run.path(), &refs[0]).is_ok());

        fs::remove_file(certification_sidecar_path(&iface)).unwrap();
        assert!(matches!(
            materialize_recovery_closure(run.path(), [1; 32], &input),
            Err(RecoveryArtifactError::CertifiedOwnersUnavailable(_))
        ));
        let mut wrong_owner = owner.clone();
        wrong_owner.module_version = ModuleVersion([9; 32]);
        write_certification(&iface, &wrong_owner);
        assert!(matches!(
            materialize_recovery_closure(run.path(), [1; 32], &input),
            Err(RecoveryArtifactError::InvalidCertifiedOwners(_))
        ));
        fs::write(certification_sidecar_path(&iface), &certification).unwrap();

        fs::write(
            &source.path().join("A.hi.owners"),
            certification.iter().copied().chain([0]).collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(matches!(
            materialize_recovery_closure(run.path(), [1; 32], &input),
            Err(RecoveryArtifactError::InvalidCertifiedOwners(_))
        ));

        fs::write(&package_iface, b"changed package interface").unwrap();
        assert!(matches!(
            verify_materialized_ref(run.path(), &refs[0]),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
        assert!(matches!(
            materialize_recovery_closure(run.path(), [1; 32], &input),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
    }

    #[test]
    fn missing_or_wrong_owner_package_sidecar_cannot_be_recovered() {
        let run = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let iface = source.path().join("A.hi");
        fs::write(&iface, b"iface").unwrap();
        let product = source.path().join("products.cbor");
        fs::write(&product, b"product").unwrap();
        let owner = CachedHomeOwner {
            unit: "home".into(),
            module: "A".into(),
            module_version: ModuleVersion([3; 32]),
            skinny_iface_sha256: Sha256::digest(b"iface").into(),
            product_sha256: Sha256::digest(b"product").into(),
        };
        write_certification(&iface, &owner);
        assert!(matches!(
            materialize_recovery_closure(
                run.path(),
                [1; 32],
                &[RecoveryArtifactInput {
                    owner: &owner,
                    interface_source: &iface,
                    module_interface: (source.path(), &fixture_module_reference(&iface, &owner)),
                    product_source: &product,
                }]
            ),
            Err(RecoveryArtifactError::Unavailable(_))
        ));

        let wrong_owner: [u8; 32] = owner.skinny_iface_sha256;
        fs::write(
            package_sidecar_path(&iface),
            package_witness("other-unit", "A", &wrong_owner, Vec::new()),
        )
        .unwrap();
        assert!(matches!(
            materialize_recovery_closure(
                run.path(),
                [1; 32],
                &[RecoveryArtifactInput {
                    owner: &owner,
                    interface_source: &iface,
                    module_interface: (source.path(), &fixture_module_reference(&iface, &owner)),
                    product_source: &product,
                }]
            ),
            Err(RecoveryArtifactError::InvalidPackageImports(_))
        ));
    }
}
