//! Run-owned retention of exact compiler artifacts. The compile cache can be
//! regenerated; recovery manifests refer only to this fsynced closure.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tidepool_repr::execution_schema::CachedHomeOwner;

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
}

/// A source-less public Join owns only an interface. Its implementation
/// modules remain independent `RecoveryArtifactRef` product pairs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
}

/// Exact original module bytes and Rust-admitted ownership, retained across a
/// temporary worker directory's lifetime. Only compiler certification creates
/// this bundle; materialization rechecks every member before publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedRecoveryProduct {
    owner: CachedHomeOwner,
    source_sha256: Option<[u8; 32]>,
    interface_bytes: Arc<[u8]>,
    product_bytes: Arc<[u8]>,
    package_imports_bytes: Arc<[u8]>,
    certification_bytes: Arc<[u8]>,
}

impl CertifiedRecoveryProduct {
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

    pub(crate) fn with_source_sha256(mut self, source_sha256: [u8; 32]) -> Self {
        self.source_sha256 = Some(source_sha256);
        self
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
        if producer == [0; 32] || unit.is_empty() || module.is_empty() {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        let digest: [u8; 32] = Sha256::digest(&interface_bytes).into();
        validate_package_imports(
            &package_imports_bytes,
            &unit,
            &module,
            &digest,
            Path::new("owned-join.hi.packages"),
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
        self.materialize_with_validation(root, &mut PackageInterfaceValidation::default())
    }

    pub(crate) fn materialize_with_validation(
        &self,
        root: &Path,
        validation: &mut PackageInterfaceValidation,
    ) -> Result<RecoveryJoinRef, RecoveryArtifactError> {
        materialize_owned_join(
            root,
            self.toolchain_identity_sha256,
            &self.unit,
            &self.module,
            &self.interface_bytes,
            &self.package_imports_bytes,
            validation,
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
    /// Only the checked-cell issuer calls this after admitting the compiler's
    /// same-transaction output and original canonical Val identity.
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
            crate::artifact_inventory::ArtifactKind::ValueInterface,
            self.requirements.clone(),
        )
        .descriptor
        .id
    }
    pub fn materialize(
        &self,
        root: &Path,
    ) -> Result<RecoveryValueInterfaceRef, RecoveryArtifactError> {
        Ok(RecoveryValueInterfaceRef {
            artifact_id: self.artifact_id(),
            interface: self.interface.materialize(root)?,
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
    #[error("invalid recovery artifact reference")]
    InvalidReference,
    #[error("recovery artifact unavailable: {0}")]
    Unavailable(PathBuf),
    #[error("recovery artifact checksum mismatch: {0}")]
    DigestMismatch(PathBuf),
    #[error("invalid package import witness: {0}")]
    InvalidPackageImports(PathBuf),
    #[error("home certification unavailable: {0}")]
    CertifiedOwnersUnavailable(PathBuf),
    #[error("home certification checksum mismatch: {0}")]
    CertifiedOwnersDigestMismatch(PathBuf),
    #[error("invalid home certification: {0}")]
    InvalidCertifiedOwners(PathBuf),
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
const CERTIFICATION_LIMIT: u64 = 4 * 1024 * 1024;

// Captures belong to one validation stage, never to a later filesystem check.
// Limit retained bytes without rejecting an otherwise valid large closure:
// interfaces beyond the budget keep the existing read-and-verify behavior.
const PACKAGE_VALIDATION_RETAIN_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct PackageInterfaceValidation {
    captured: BTreeMap<PathBuf, CapturedPackageInterface>,
    retained_bytes: usize,
}

struct CapturedPackageInterface {
    _bytes: Vec<u8>,
    sha256: [u8; 32],
}

impl PackageInterfaceValidation {
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
                RecoveryArtifactError::Io(error)
            }
        })?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > PACKAGE_INTERFACE_LIMIT {
            return Err(RecoveryArtifactError::InvalidPackageImports(
                path.to_path_buf(),
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize + 1);
        file.take(PACKAGE_INTERFACE_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > PACKAGE_INTERFACE_LIMIT {
            return Err(RecoveryArtifactError::InvalidPackageImports(
                path.to_path_buf(),
            ));
        }
        let sha256 = Sha256::digest(&bytes).into();
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

fn read_certification(
    path: &Path,
    expected_sha256: Option<&[u8; 32]>,
    owner: &CachedHomeOwner,
    validation: &mut PackageInterfaceValidation,
) -> Result<Vec<u8>, RecoveryArtifactError> {
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
    let bytes = fs::read(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            RecoveryArtifactError::CertifiedOwnersUnavailable(path.to_path_buf())
        } else {
            RecoveryArtifactError::Io(error)
        }
    })?;
    if bytes.len() as u64 > CERTIFICATION_LIMIT {
        return Err(RecoveryArtifactError::InvalidCertifiedOwners(
            path.to_path_buf(),
        ));
    }
    if expected_sha256.is_some_and(|expected| Sha256::digest(&bytes).as_slice() != expected) {
        return Err(RecoveryArtifactError::CertifiedOwnersDigestMismatch(
            path.to_path_buf(),
        ));
    }
    crate::certified_products::validate_home_certification_with_validation(
        &bytes, owner, validation,
    )
    .map_err(|_| RecoveryArtifactError::InvalidCertifiedOwners(path.to_path_buf()))?;
    Ok(bytes)
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
    let bytes = fs::read(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            RecoveryArtifactError::Unavailable(path.to_path_buf())
        } else {
            RecoveryArtifactError::Io(error)
        }
    })?;
    if bytes.len() as u64 > PACKAGE_IMPORTS_LIMIT {
        return Err(RecoveryArtifactError::InvalidPackageImports(
            path.to_path_buf(),
        ));
    }
    if let Some(expected) = expected_sha256 {
        if Sha256::digest(&bytes).as_slice() != expected {
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
    use ciborium::value::Value;

    let invalid = || RecoveryArtifactError::InvalidPackageImports(sidecar_path.to_path_buf());
    let witness: Value = ciborium::de::from_reader(bytes).map_err(|_| invalid())?;
    let mut canonical = Vec::new();
    ciborium::ser::into_writer(&witness, &mut canonical).map_err(|_| invalid())?;
    if canonical != bytes {
        return Err(invalid());
    }
    let Value::Array(fields) = witness else {
        return Err(invalid());
    };
    if fields.len() != 4
        || fields[0].as_text() != Some("TPPKGROOTS")
        || fields[1].as_text() != Some("1")
    {
        return Err(invalid());
    }
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
    Ok(selected)
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

fn read_checked(path: &Path, expected: &[u8; 32]) -> Result<Vec<u8>, RecoveryArtifactError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(RecoveryArtifactError::Unavailable(path.to_path_buf()));
        }
        Err(error) => return Err(error.into()),
    };
    if Sha256::digest(&bytes).as_slice() != expected {
        return Err(RecoveryArtifactError::DigestMismatch(path.to_path_buf()));
    }
    Ok(bytes)
}

fn durable_copy(path: &Path, bytes: &[u8], digest: &[u8; 32]) -> Result<(), RecoveryArtifactError> {
    reject_symlink(path)?;
    if verify_existing_durable(path, bytes.len(), digest)? {
        return Ok(());
    }
    tidepool_atomic_write::write_durable_new(path, bytes).map_err(io::Error::from)?;
    reject_symlink(path)?;
    if verify_existing_durable(path, bytes.len(), digest)? {
        Ok(())
    } else {
        Err(RecoveryArtifactError::Unavailable(path.to_path_buf()))
    }
}

fn verify_existing_durable(
    path: &Path,
    expected_len: usize,
    digest: &[u8; 32],
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
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    if file.read(&mut buffer[..1])? != 0 || hasher.finalize().as_slice() != digest {
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
    // A readable existing file is not evidence that a previous publication's
    // durability completed. Confirm this descriptor and its directory now.
    file.sync_all()?;
    tidepool_atomic_write::sync_parent_directory(path).map_err(io::Error::from)?;
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

/// The run owner controls this directory while publishing; checking its
/// canonical identity before writing rejects preexisting redirects.
fn prepare_owned_directory(recovery_root: &Path) -> Result<PathBuf, RecoveryArtifactError> {
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
    File::open(&canonical_root)?.sync_all()?;
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
    let bytes = read_checked(interface_source, &skinny_iface_sha256)?;
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
) -> Result<RecoveryJoinRef, RecoveryArtifactError> {
    if toolchain_identity_sha256 == [0; 32] || unit.is_empty() || module.is_empty() {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let skinny_iface_sha256: [u8; 32] = Sha256::digest(bytes).into();
    validate_package_imports_with_validation(
        package_imports,
        unit,
        module,
        &skinny_iface_sha256,
        Path::new("owned-join.hi.packages"),
        validation,
    )?;
    let package_imports_sha256: [u8; 32] = Sha256::digest(&package_imports).into();
    let owned = prepare_owned_directory(recovery_root)?;
    let interface_path =
        PathBuf::from("artifacts").join(format!("{}.joined.hi", hex(&skinny_iface_sha256)));
    let package_imports_path = package_sidecar_path(&interface_path);
    durable_copy(
        &owned.join(
            interface_path
                .file_name()
                .ok_or(RecoveryArtifactError::InvalidReference)?,
        ),
        bytes,
        &skinny_iface_sha256,
    )?;
    durable_copy(
        &owned.join(
            package_imports_path
                .file_name()
                .ok_or(RecoveryArtifactError::InvalidReference)?,
        ),
        package_imports,
        &package_imports_sha256,
    )?;
    File::open(&owned)?.sync_all()?;
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

/// Materialize compiler-certified original module products into the run-owned
/// closure. The bundle owns its bytes, so no worker scratch path survives here.
pub fn materialize_certified_products(
    recovery_root: &Path,
    toolchain_identity_sha256: [u8; 32],
    products: &[CertifiedRecoveryProduct],
) -> Result<Vec<RecoveryArtifactRef>, RecoveryArtifactError> {
    materialize_certified_products_with_validation(
        recovery_root,
        toolchain_identity_sha256,
        products,
        &mut PackageInterfaceValidation::default(),
    )
}

pub(crate) fn materialize_certified_products_with_validation(
    recovery_root: &Path,
    toolchain_identity_sha256: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    validation: &mut PackageInterfaceValidation,
) -> Result<Vec<RecoveryArtifactRef>, RecoveryArtifactError> {
    if toolchain_identity_sha256 == [0; 32] {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let owned = prepare_owned_directory(recovery_root)?;
    let mut refs = Vec::with_capacity(products.len());
    for product in products {
        let owner = &product.owner;
        if owner.unit.is_empty() || owner.module.is_empty() {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        if Sha256::digest(&product.interface_bytes).as_slice() != owner.skinny_iface_sha256
            || Sha256::digest(&product.product_bytes).as_slice() != owner.product_sha256
        {
            return Err(RecoveryArtifactError::DigestMismatch(home_interface_path(
                owner,
            )));
        }
        let interface_path = home_interface_path(owner);
        let package_imports_path = package_sidecar_path(&interface_path);
        let certification_path = certification_sidecar_path(&interface_path);
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
                certification_path,
            ));
        }
        crate::certified_products::validate_home_certification_with_validation(
            &product.certification_bytes,
            owner,
            validation,
        )
        .map_err(|_| RecoveryArtifactError::InvalidCertifiedOwners(certification_path.clone()))?;
        let package_imports_sha256: [u8; 32] =
            Sha256::digest(&product.package_imports_bytes).into();
        let certification_sha256: [u8; 32] = Sha256::digest(&product.certification_bytes).into();
        durable_copy(
            &owned.join(
                interface_path
                    .file_name()
                    .ok_or(RecoveryArtifactError::InvalidReference)?,
            ),
            &product.interface_bytes,
            &owner.skinny_iface_sha256,
        )?;
        durable_copy(
            &owned.join(
                product_path
                    .file_name()
                    .ok_or(RecoveryArtifactError::InvalidReference)?,
            ),
            &product.product_bytes,
            &owner.product_sha256,
        )?;
        durable_copy(
            &owned.join(
                package_imports_path
                    .file_name()
                    .ok_or(RecoveryArtifactError::InvalidReference)?,
            ),
            &product.package_imports_bytes,
            &package_imports_sha256,
        )?;
        durable_copy(
            &owned.join(
                certification_path
                    .file_name()
                    .ok_or(RecoveryArtifactError::InvalidReference)?,
            ),
            &product.certification_bytes,
            &certification_sha256,
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
        });
    }
    File::open(&owned)?.sync_all()?;
    Ok(refs)
}

/// Read legacy path inputs into owned bundles, then use the same immutable
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
        let interface_bytes = read_checked(artifact.interface_source, &owner.skinny_iface_sha256)?;
        let product_bytes = read_checked(artifact.product_source, &owner.product_sha256)?;
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
        products.push(CertifiedRecoveryProduct::from_certification(
            owner.clone(),
            interface_bytes,
            product_bytes,
            package_imports_bytes,
            certification_bytes,
        ));
    }
    materialize_certified_products_with_validation(
        recovery_root,
        toolchain_identity_sha256,
        &products,
        &mut validation,
    )
}

/// Verify path confinement and both immutable bytes before using a durable
/// recovery ref. The captured bytes let a caller avoid a second unbound read.
pub fn verify_materialized_ref(
    recovery_root: &Path,
    reference: &RecoveryArtifactRef,
) -> Result<VerifiedRecoveryArtifact, RecoveryArtifactError> {
    verify_materialized_ref_with_validation(
        recovery_root,
        reference,
        &mut PackageInterfaceValidation::default(),
    )
}

pub(crate) fn verify_materialized_ref_with_validation(
    recovery_root: &Path,
    reference: &RecoveryArtifactRef,
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
        || reference.certification_path != certification_sidecar_path(&reference.interface_path)
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
    let interface_bytes = read_checked(&interface_path, &reference.skinny_iface_sha256)?;
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
    let certification_bytes = read_certification(
        &certification_path,
        Some(&reference.certification_sha256),
        &owner,
        validation,
    )?;
    let product_bytes = read_checked(&product_path, &reference.product_sha256)?;
    Ok(VerifiedRecoveryArtifact {
        reference: reference.clone(),
        interface_path,
        package_imports_path,
        product_path,
        interface_bytes,
        package_imports_bytes,
        certification_path,
        certification_bytes,
        product_bytes,
    })
}

/// Check a sealed Joined interface independently of retained implementation
/// products, returning the exact captured bytes used for rehydration.
pub fn verify_materialized_join(
    recovery_root: &Path,
    reference: &RecoveryJoinRef,
) -> Result<VerifiedRecoveryJoin, RecoveryArtifactError> {
    verify_materialized_join_with_validation(
        recovery_root,
        reference,
        &mut PackageInterfaceValidation::default(),
    )
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
    let interface_bytes = read_checked(&interface_path, &reference.skinny_iface_sha256)?;
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
    use tidepool_repr::execution_schema::ModuleVersion;

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
    fn durable_copy_existing_payload_refuses_wrong_digest_bytes_and_symlink() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned.cbor");
        let digest: [u8; 32] = Sha256::digest(b"owned").into();
        durable_copy(&path, b"owned", &digest).unwrap();
        durable_copy(&path, b"owned", &digest).unwrap();
        assert!(matches!(
            durable_copy(&path, b"owned", &[0; 32]),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
        fs::write(&path, b"other").unwrap();
        assert!(matches!(
            durable_copy(&path, b"owned", &digest),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), b"other");
        fs::write(&path, b"different-length").unwrap();
        assert!(durable_copy(&path, b"owned", &digest).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"different-length");
        #[cfg(unix)]
        {
            let target = root.path().join("target.cbor");
            fs::write(&target, b"owned").unwrap();
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(matches!(
                durable_copy(&path, b"owned", &digest),
                Err(RecoveryArtifactError::InvalidReference)
            ));
            assert_eq!(fs::read(&target).unwrap(), b"owned");
        }
    }

    #[cfg(unix)]
    #[test]
    fn durable_copy_nonregular_fifo_refuses_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the C string is terminated and valid for the call; the path
        // is a new entry in this test's private directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let digest: [u8; 32] = Sha256::digest(b"owned").into();
        let started = std::time::Instant::now();
        assert!(matches!(
            durable_copy(&path, b"owned", &digest),
            Err(RecoveryArtifactError::InvalidReference)
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert!(matches!(
            durable_copy(root.path(), b"owned", &digest),
            Err(RecoveryArtifactError::InvalidReference)
        ));
    }

    #[test]
    fn durable_copy_racing_publishers_verify_the_actual_winner() {
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
                    durable_copy(&path, b"owned", &digest)
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
                durable_copy(&path, b"owned", &digest)
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
            text("1"),
            ciborium::value::Value::Array(vec![text(unit), text(module), text(hex(iface_sha256))]),
            ciborium::value::Value::Array(root_values),
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

    fn write_certification(interface: &Path, owner: &CachedHomeOwner) -> Vec<u8> {
        let bytes = crate::certified_products::encode_home_certification(
            owner,
            &[],
            &std::collections::BTreeMap::new(),
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
                product_source: &product,
            }],
        )
        .unwrap();
        let verified = verify_materialized_ref(run.path(), &refs[0]).unwrap();
        assert_eq!(verified.interface_bytes, b"iface");
        assert!(!verified.package_imports_bytes.is_empty());
        assert!(!verified.certification_bytes.is_empty());
        assert!(verified
            .reference
            .certification_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".hi.owners")));
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
        let first_certification = crate::certified_products::encode_home_certification(
            &first,
            &[],
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        fs::write(certification_sidecar_path(&iface), &first_certification).unwrap();
        let first_ref = materialize_recovery_closure(
            run.path(),
            [1; 32],
            &[RecoveryArtifactInput {
                owner: &first,
                interface_source: &iface,
                product_source: &first_product,
            }],
        )
        .unwrap()
        .remove(0);
        assert!(verify_materialized_ref(run.path(), &first_ref).is_ok());

        let second_certification = crate::certified_products::encode_home_certification(
            &second,
            &[],
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        fs::write(certification_sidecar_path(&iface), &second_certification).unwrap();
        let second_ref = materialize_recovery_closure(
            run.path(),
            [1; 32],
            &[RecoveryArtifactInput {
                owner: &second,
                interface_source: &iface,
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
                    product_source: &product,
                }]
            ),
            Err(RecoveryArtifactError::InvalidPackageImports(_))
        ));
    }
}
