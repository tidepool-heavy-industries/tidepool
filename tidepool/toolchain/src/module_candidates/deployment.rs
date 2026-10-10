//! Explicit deployment inputs to the existing module-candidate owner.
//! Original products retain their producing roots and version recipe. Current
//! source selection and interface admission still belong to the worker.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tidepool_repr::execution_schema::{InventoryOperation, ParseError};

use super::{absolute, sha, version_hash, CandidateProduct, CandidateRecord, Record, RECORD_LIMIT};
use crate::toolchain::CompilerDeploymentAuthority;

mod source_selection;
pub use source_selection::{NativeCatalogSourceSelection, NativeSourceRole};

#[derive(Debug, thiserror::Error)]
pub enum ModulePackageError {
    #[error("compiler host work interrupted: {0}")]
    Interrupted(#[source] std::io::Error),
    #[error("module package {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("module package format is invalid: {0}")]
    Format(&'static str),
    #[error("module package exceeds its declared bounds")]
    Bounds,
    #[error("module package differs from configured compiler deployment")]
    CompilerMismatch,
    #[error("module package requires configured compiler deployment authority")]
    UnknownCompiler,
    #[error("module package compiler configuration: {0}")]
    CompilerConfiguration(#[source] Box<crate::toolchain::ToolchainError>),
    #[error("module package path is aliased or its original source root moved")]
    RootMoved,
    #[error("module package requires final immutable Nix store source roots")]
    MutableRoot,
    #[error("module package source aliases another path: {}", .0.display())]
    SourceAlias(PathBuf),
    #[error("module package source revision changed")]
    SourceChanged,
    #[error("module package artifact changed: {}", .0.display())]
    ArtifactChanged(PathBuf),
    #[error("module package must contain a closed immutable source cohort")]
    OpenCohort,
    #[error(
        "module package original product is unavailable for {unit}:{module} ({availability:?})"
    )]
    IncompleteProduct {
        unit: String,
        module: String,
        availability: crate::cache::ProductAvailability,
    },
}

fn canonical_error(error: crate::recovery_artifacts::RecoveryArtifactError) -> ModulePackageError {
    use crate::recovery_artifacts::RecoveryArtifactError as Error;
    match error {
        Error::InventoryAccounting(_) => ModulePackageError::Bounds,
        Error::DigestMismatch(path)
        | Error::CertifiedOwnersDigestMismatch(path)
        | Error::InvalidCapturedPayload(path)
        | Error::InvalidModuleCertificate(path) => ModulePackageError::ArtifactChanged(path),
        Error::Unreadable { error, .. } if error.kind() == std::io::ErrorKind::Interrupted => {
            ModulePackageError::Interrupted(error)
        }
        Error::Unreadable { path, error } => io(&path, error),
        Error::Io(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            ModulePackageError::Interrupted(error)
        }
        _ => ModulePackageError::Format("canonical module interface"),
    }
}

fn decode_error(error: ParseError, format: &'static str) -> ModulePackageError {
    match error {
        ParseError::InventoryByteLimit { .. }
        | ParseError::ModuleByteLimit { .. }
        | ParseError::ByteLimit { .. }
        | ParseError::LimitExceeded(_) => ModulePackageError::Bounds,
        _ => ModulePackageError::Format(format),
    }
}

fn certification_error(
    error: crate::certified_products::CertificationError,
    format: &'static str,
) -> ModulePackageError {
    match error {
        crate::certified_products::CertificationError::Interrupted(error) => {
            ModulePackageError::Interrupted(error)
        }
        crate::certified_products::CertificationError::Product(error) => {
            decode_error(error, format)
        }
        crate::certified_products::CertificationError::SizeLimit { .. } => {
            ModulePackageError::Bounds
        }
        _ => ModulePackageError::Format(format),
    }
}

fn io(path: &Path, source: std::io::Error) -> ModulePackageError {
    if source.kind() == std::io::ErrorKind::Interrupted {
        return ModulePackageError::Interrupted(source);
    }
    ModulePackageError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Clone, Copy, Debug)]
enum RootPolicy {
    NixStore,
    #[cfg(test)]
    Fixture,
}

fn immutable_store_path(path: &Path) -> bool {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let Ok(relative) = path.strip_prefix("/nix/store") else {
        return false;
    };
    let Some(Component::Normal(output)) = relative.components().next() else {
        return false;
    };
    let Some(output) = output.to_str() else {
        return false;
    };
    let bytes = output.as_bytes();
    bytes.len() > 33
        && bytes[32] == b'-'
        && bytes[..32]
            .iter()
            .all(|c| b"0123456789abcdfghijklmnpqrsvwxyz".contains(c))
}

fn require_immutable_roots(policy: RootPolicy, source: &Path) -> Result<(), ModulePackageError> {
    #[cfg(test)]
    if matches!(policy, RootPolicy::Fixture) {
        return Ok(());
    }
    let _ = policy;
    if immutable_store_path(source) {
        Ok(())
    } else {
        Err(ModulePackageError::MutableRoot)
    }
}

pub(crate) fn prepare_build_roots(
    source: &Path,
    output: &Path,
) -> Result<NativeCatalogSourceSelection, ModulePackageError> {
    let selection = NativeCatalogSourceSelection::capture(source)?;
    if !output.is_absolute() || output.exists() {
        return Err(ModulePackageError::Format(
            "absent absolute output directory",
        ));
    }
    Ok(selection)
}

fn reject_source_aliases(root: &Path) -> Result<(), ModulePackageError> {
    let mut directories = vec![root.to_owned()];
    while let Some(directory) = directories.pop() {
        crate::host_work::checkpoint().map_err(ModulePackageError::Interrupted)?;
        for entry in fs::read_dir(&directory).map_err(|e| io(&directory, e))? {
            crate::host_work::checkpoint().map_err(ModulePackageError::Interrupted)?;
            let entry = entry.map_err(|e| io(&directory, e))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| io(&path, e))?;
            if kind.is_symlink() {
                return Err(ModulePackageError::SourceAlias(path));
            }
            if kind.is_dir() {
                directories.push(path)
            }
        }
    }
    Ok(())
}

fn read(
    path: &Path,
    limit: usize,
    inventory: &InventoryOperation,
) -> Result<Vec<u8>, ModulePackageError> {
    let limit = limit.min(inventory.limits().max_bytes);
    let length = fs::metadata(path).map_err(|e| io(path, e))?.len();
    if length > limit as u64 {
        return Err(ModulePackageError::Bounds);
    }
    crate::certified_products::read_bounded_with_operation(path, limit as u64, inventory).map_err(
        |error| match error {
            crate::certified_products::CertificationError::Interrupted(error) => {
                ModulePackageError::Interrupted(error)
            }
            crate::certified_products::CertificationError::Product(_) => ModulePackageError::Bounds,
            _ => ModulePackageError::ArtifactChanged(path.to_owned()),
        },
    )
}

fn decode_json<T: DeserializeOwned>(
    bytes: &[u8],
    inventory: &InventoryOperation,
    format: &'static str,
) -> Result<T, ModulePackageError> {
    // Empty String/PathBuf rows occupy 24 bytes from only three JSON bytes.
    // Reserve conservative container, payload and visit units for these catalog
    // shapes before serde allocates; retained typed copies are charged separately.
    inventory
        .charge(
            bytes
                .len()
                .checked_mul(32)
                .ok_or(ModulePackageError::Bounds)?,
        )
        .map_err(|_| ModulePackageError::Bounds)?;
    inventory
        .reserve::<T>(1)
        .map_err(|_| ModulePackageError::Bounds)?;
    serde_json::from_slice(bytes).map_err(|_| ModulePackageError::Format(format))
}

fn encode_json<T: Serialize>(
    value: &T,
    inventory: &InventoryOperation,
    limit: usize,
    format: &'static str,
    pretty: bool,
) -> Result<Vec<u8>, ModulePackageError> {
    struct Size {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Size {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .filter(|size| *size <= self.limit)
                .ok_or_else(|| std::io::Error::other("catalog JSON byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut size = Size { bytes: 0, limit };
    let measured = if pretty {
        serde_json::to_writer_pretty(&mut size, value)
    } else {
        serde_json::to_writer(&mut size, value)
    };
    measured.map_err(|error| {
        if error.is_io() {
            ModulePackageError::Bounds
        } else {
            ModulePackageError::Format(format)
        }
    })?;
    inventory
        .charge(
            size.bytes
                .checked_mul(2)
                .ok_or(ModulePackageError::Bounds)?,
        )
        .map_err(|_| ModulePackageError::Bounds)?;
    let mut bytes = Vec::with_capacity(size.bytes);
    let encoded = if pretty {
        serde_json::to_writer_pretty(&mut bytes, value)
    } else {
        serde_json::to_writer(&mut bytes, value)
    };
    encoded.map_err(|_| ModulePackageError::Format(format))?;
    Ok(bytes)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRef {
    path: PathBuf,
    sha256: String,
    length: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleFiles {
    owner: FileRef,
    products: FileRef,
    interface: FileRef,
    packages: FileRef,
    evidence: FileRef,
    certification: FileRef,
    module_interface: crate::recovery_artifacts::RecoveryModuleInterfaceRef,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema: u32,
    source_selection: NativeCatalogSourceSelection,
    producer_identity: [u8; 32],
    consumed_worker_identity: [u8; 32],
    modules: Vec<ModuleFiles>,
    #[serde(default)]
    execution_graphs: Vec<FileRef>,
}

/// Exact references to one physical dependency-evidence file. Catalog rows may
/// share a path only when they agree on the complete authenticated identity.
fn evidence_references(
    catalog: &Catalog,
) -> Result<std::collections::BTreeMap<PathBuf, FileRef>, ModulePackageError> {
    let mut evidence = std::collections::BTreeMap::<PathBuf, FileRef>::new();
    let mut other_artifacts = BTreeSet::new();
    for reference in &catalog.execution_graphs {
        other_artifacts.insert(reference.path.clone());
    }
    for module in &catalog.modules {
        for reference in [
            &module.owner,
            &module.products,
            &module.interface,
            &module.packages,
            &module.certification,
        ] {
            other_artifacts.insert(reference.path.clone());
        }
        other_artifacts.insert(module.module_interface.interface.interface_path.clone());
        other_artifacts.insert(
            module
                .module_interface
                .interface
                .package_imports_path
                .clone(),
        );
        other_artifacts.insert(module.module_interface.certificate_path.clone());
        if let Some(core) = &module.module_interface.core {
            other_artifacts.insert(core.path.clone());
        }
        let reference = &module.evidence;
        if reference.path.as_os_str().is_empty()
            || !reference
                .path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            || reference.sha256.len() != 64
            || !reference
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ModulePackageError::Format("dependency evidence reference"));
        }
        match evidence.entry(reference.path.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(reference.clone());
            }
            std::collections::btree_map::Entry::Occupied(entry)
                if entry.get().sha256 == reference.sha256
                    && entry.get().length == reference.length => {}
            std::collections::btree_map::Entry::Occupied(_) => {
                return Err(ModulePackageError::Format(
                    "conflicting dependency evidence reference",
                ));
            }
        }
    }
    if evidence.keys().any(|path| other_artifacts.contains(path)) {
        return Err(ModulePackageError::Format("dependency evidence path alias"));
    }
    Ok(evidence)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    unit: String,
    module: String,
    source: PathBuf,
    source_sha256: String,
    include: Vec<PathBuf>,
    target_source: String,
    module_version: [u8; 32],
    #[serde(default)]
    original_owner: Option<super::OriginalOwner>,
    #[serde(default)]
    version_origin: Option<super::CandidateVersionOrigin>,
    #[serde(default)]
    execution_source_sha256: Option<[u8; 32]>,
}

/// Validated source provenance and file references, never native authority.
#[derive(Debug)]
pub struct DeploymentModulePackage {
    catalog: Catalog,
    artifact_root: PathBuf,
    catalog_identity: String,
    source_identity: String,
    source_policy: RootPolicy,
    records: Vec<Arc<DecodedDeploymentRecord>>,
}

/// One current configured selection; replacement never accumulates packages.
/// Failed loads and freshness checks cannot publish a new owner.
#[derive(Default)]
pub(crate) struct ConfiguredModulePackageOwner {
    current: Option<(
        PathBuf,
        CompilerDeploymentAuthority,
        Arc<DeploymentModulePackage>,
    )>,
}

impl ConfiguredModulePackageOwner {
    pub(crate) const fn new() -> Self {
        Self { current: None }
    }
    pub(crate) fn clear(&mut self) {
        self.current = None;
    }

    pub(crate) fn load(
        &mut self,
        path: &Path,
        authority: &CompilerDeploymentAuthority,
    ) -> Result<Arc<DeploymentModulePackage>, ModulePackageError> {
        self.load_under(path, authority, RootPolicy::NixStore)
    }

    fn load_under(
        &mut self,
        path: &Path,
        authority: &CompilerDeploymentAuthority,
        policy: RootPolicy,
    ) -> Result<Arc<DeploymentModulePackage>, ModulePackageError> {
        crate::host_work::checkpoint().map_err(ModulePackageError::Interrupted)?;
        let started = std::time::Instant::now();
        if let Some((selected, configured, package)) = &self.current {
            if selected == path && configured == authority {
                let work = package.revalidate(path, policy)?;
                tracing::info!(target: "tidepool_toolchain::module_candidates",
                    phase = "configured_package", reused = true,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    hydrated_modules = 0, retained_modules = package.records.len(),
                    reauthenticated_artifact_files = work.artifact_files,
                    reauthenticated_artifact_bytes = work.artifact_bytes,
                    source_read_attempts = work.evidence.source_read_attempts,
                    source_read_bytes = work.evidence.source_read_bytes,
                    negative_metadata_calls = work.evidence.negative_metadata_calls,
                    revalidated_source_proofs = package.records.len(),
                    revalidated_package_imports = package.records.len());
                return Ok(Arc::clone(package));
            }
        }
        let package = Arc::new(DeploymentModulePackage::load_under(
            path, authority, policy,
        )?);
        tracing::info!(target: "tidepool_toolchain::module_candidates",
            phase = "configured_package", reused = false,
            elapsed_ms = started.elapsed().as_millis() as u64,
            hydrated_modules = package.records.len(), retained_modules = package.records.len());
        self.current = Some((path.to_owned(), authority.clone(), Arc::clone(&package)));
        Ok(package)
    }
}

/// The loader owns both the original bytes and their single decoded product.
/// No mutable record is exposed after this relationship is established.
#[derive(Debug)]
#[cfg_attr(test, derive(Clone))]
pub(super) struct DecodedDeploymentRecord {
    record: Record,
    product: Arc<tidepool_repr::execution_schema::RawModuleProduct>,
}

impl DecodedDeploymentRecord {
    fn decode(record: Record, inventory: &InventoryOperation) -> Result<Self, ModulePackageError> {
        let requirements = crate::prepared_artifact::production_requirements()
            .map_err(|_| ModulePackageError::Format("host requirements"))?;
        let product = CandidateProduct::decode_product_with_operation(
            &record.products,
            &requirements,
            inventory,
        )
        .map_err(|error| decode_error(error, "original module products"))?
        .ok_or(ModulePackageError::Format("original module owner"))?;
        Ok(Self {
            record,
            product: Arc::new(product),
        })
    }

    pub(super) fn record(&self) -> &Record {
        &self.record
    }

    pub(super) fn product(&self) -> &Arc<tidepool_repr::execution_schema::RawModuleProduct> {
        &self.product
    }
}

impl std::ops::Deref for DecodedDeploymentRecord {
    type Target = Record;
    fn deref(&self) -> &Self::Target {
        &self.record
    }
}

#[derive(Debug)]
struct PackageRevalidationWork {
    artifact_files: u64,
    artifact_bytes: u64,
    evidence: crate::cache::DependencyEvidenceWork,
}

impl DeploymentModulePackage {
    pub fn catalog_path(&self) -> PathBuf {
        self.artifact_root.join("catalog.json")
    }

    pub fn validate_deployment(
        &self,
        deployment: &crate::toolchain::AdmittedCompilerDeployment,
    ) -> Result<(), ModulePackageError> {
        if self.catalog.producer_identity != deployment.producer_identity
            || self.catalog.consumed_worker_identity != deployment.consumed_worker_identity
        {
            return Err(ModulePackageError::CompilerMismatch);
        }
        Ok(())
    }

    pub(super) fn validate_current_sources(&self) -> Result<(), ModulePackageError> {
        self.catalog
            .source_selection
            .validate_under(self.source_policy)
    }

    /// Retained decoded objects do not make a mutable package path immutable.
    /// Reauthenticate physical bytes before reusing semantic admission, including
    /// canonical companions that are not listed as native module files.
    fn revalidate(
        &self,
        path: &Path,
        policy: RootPolicy,
    ) -> Result<PackageRevalidationWork, ModulePackageError> {
        let inventory = Arc::new(InventoryOperation::new(Default::default()));
        if absolute(path).as_ref() != Some(&self.artifact_root.join("catalog.json"))
            || absolute(&self.artifact_root).as_ref() != Some(&self.artifact_root)
        {
            return Err(ModulePackageError::RootMoved);
        }
        let catalog = read(path, inventory.limits().max_bytes, &inventory)?;
        if sha(&catalog) != self.catalog_identity {
            return Err(ModulePackageError::ArtifactChanged(path.to_owned()));
        }
        self.catalog.source_selection.validate_under(policy)?;
        let evidence = evidence_references(&self.catalog)?;
        let mut files = 1;
        let mut bytes = catalog.len() as u64;
        for (reference, limit) in self
            .catalog
            .execution_graphs
            .iter()
            .map(|reference| (reference, crate::execution_source::GRAPH_BYTES_LIMIT))
            .chain(self.catalog.modules.iter().flat_map(|module| {
                [
                    (&module.owner, RECORD_LIMIT),
                    (&module.products, inventory.limits().max_module_bytes),
                    (&module.interface, RECORD_LIMIT),
                    (&module.packages, RECORD_LIMIT),
                    (&module.certification, RECORD_LIMIT),
                ]
            }))
        {
            let observed = self.read_ref(reference, limit, &inventory)?;
            files += 1;
            bytes += observed.len() as u64;
        }
        for reference in evidence.values() {
            let observed = self.read_ref(reference, RECORD_LIMIT, &inventory)?;
            files += 1;
            bytes += observed.len() as u64;
        }
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(
            Arc::clone(&inventory),
        );
        for module in &self.catalog.modules {
            let reference = &module.module_interface;
            let companions = [
                (
                    &reference.interface.interface_path,
                    reference.interface.skinny_iface_sha256,
                    None,
                    crate::recovery_artifacts::PACKAGE_INTERFACE_LIMIT,
                ),
                (
                    &reference.interface.package_imports_path,
                    reference.interface.package_imports_sha256,
                    None,
                    crate::recovery_artifacts::PACKAGE_IMPORTS_LIMIT,
                ),
                (
                    &reference.certificate_path,
                    reference.certificate_sha256,
                    None,
                    crate::recovery_artifacts::CERTIFICATION_LIMIT,
                ),
            ];
            for (relative, digest, length, limit) in
                companions
                    .into_iter()
                    .chain(reference.core.iter().map(|core| {
                        (
                            &core.path,
                            core.sha256,
                            Some(core.bytes),
                            crate::recovery_artifacts::PACKAGE_INTERFACE_LIMIT,
                        )
                    }))
            {
                let path = self.artifact_root.join(relative);
                if absolute(&path).as_ref() != Some(&path) {
                    return Err(ModulePackageError::RootMoved);
                }
                let observed = crate::recovery_artifacts::capture_module_payload(
                    &self.artifact_root,
                    relative,
                    &digest,
                    length,
                    limit,
                    &mut validation,
                )
                .map_err(canonical_error)?;
                files += 1;
                bytes += observed.len() as u64;
            }
        }
        let mut evidence_validation = super::shared_evidence::ValidationStage::configured_package();
        for record in &self.records {
            validate_dependency_evidence(&mut evidence_validation, record)?;
            crate::recovery_artifacts::validate_package_imports_with_validation(
                &record.package_imports,
                &record.unit,
                &record.module,
                &record.original_owner.skinny_iface_sha256,
                &self.artifact_root,
                &mut validation,
            )
            .map_err(canonical_error)?;
        }
        crate::host_work::checkpoint().map_err(ModulePackageError::Interrupted)?;
        Ok(PackageRevalidationWork {
            artifact_files: files,
            artifact_bytes: bytes,
            evidence: evidence_validation.work(),
        })
    }

    pub fn source_selection(&self) -> &NativeCatalogSourceSelection {
        &self.catalog.source_selection
    }
    pub fn source_identity(&self) -> &str {
        &self.source_identity
    }
    pub fn producer_identity(&self) -> &[u8; 32] {
        &self.catalog.producer_identity
    }
    pub fn catalog_identity(&self) -> &str {
        &self.catalog_identity
    }

    pub(crate) fn load(
        path: &Path,
        authority: &CompilerDeploymentAuthority,
    ) -> Result<Self, ModulePackageError> {
        Self::load_under(path, authority, RootPolicy::NixStore)
    }

    fn load_under(
        path: &Path,
        authority: &CompilerDeploymentAuthority,
        policy: RootPolicy,
    ) -> Result<Self, ModulePackageError> {
        Self::load_with_inventory(
            path,
            authority,
            policy,
            Arc::new(InventoryOperation::new(Default::default())),
        )
    }

    fn load_with_inventory(
        path: &Path,
        authority: &CompilerDeploymentAuthority,
        policy: RootPolicy,
        inventory: Arc<InventoryOperation>,
    ) -> Result<Self, ModulePackageError> {
        crate::host_work::checkpoint().map_err(ModulePackageError::Interrupted)?;
        let mut package = Self::read_catalog_with_inventory(path, authority, policy, &inventory)?;
        // Validate configured products before candidate admission.
        package.records = package.read_records(package.producer_identity(), inventory)?;
        crate::host_work::checkpoint().map_err(ModulePackageError::Interrupted)?;
        Ok(package)
    }

    pub(crate) fn load_source_selection(
        path: &Path,
        authority: &CompilerDeploymentAuthority,
    ) -> Result<NativeCatalogSourceSelection, ModulePackageError> {
        Ok(
            Self::read_catalog_under(path, authority, RootPolicy::NixStore)?
                .catalog
                .source_selection,
        )
    }

    fn read_catalog_under(
        path: &Path,
        authority: &CompilerDeploymentAuthority,
        policy: RootPolicy,
    ) -> Result<Self, ModulePackageError> {
        Self::read_catalog_with_inventory(
            path,
            authority,
            policy,
            &InventoryOperation::new(Default::default()),
        )
    }

    fn read_catalog_with_inventory(
        path: &Path,
        authority: &CompilerDeploymentAuthority,
        policy: RootPolicy,
        inventory: &InventoryOperation,
    ) -> Result<Self, ModulePackageError> {
        let bytes = read(path, inventory.limits().max_bytes, inventory)?;
        let catalog: Catalog = decode_json(&bytes, inventory, "catalog JSON")?;
        inventory
            .reserve::<ModuleFiles>(catalog.modules.len())
            .and_then(|_| inventory.reserve::<FileRef>(catalog.execution_graphs.len()))
            .and_then(|_| {
                inventory.reserve::<source_selection::NativeCatalogSourceFile>(
                    catalog.source_selection.source_files.len(),
                )
            })
            .map_err(|_| ModulePackageError::Bounds)?;
        if catalog.schema != 4 {
            return Err(ModulePackageError::Format("catalog schema"));
        }
        catalog.source_selection.validate_under(policy)?;
        if catalog.producer_identity != authority.producer_identity
            || catalog.consumed_worker_identity != authority.consumed_worker_identity
            || authority.schema != 1
            || authority.producer_identity == [0; 32]
            || authority.consumed_worker_identity == [0; 32]
        {
            return Err(ModulePackageError::CompilerMismatch);
        }
        let artifact_root = path
            .parent()
            .ok_or(ModulePackageError::RootMoved)?
            .to_path_buf();
        if !path.is_absolute()
            || absolute(path).as_ref() != Some(&artifact_root.join("catalog.json"))
            || absolute(&artifact_root).as_ref() != Some(&artifact_root)
        {
            return Err(ModulePackageError::RootMoved);
        }
        if catalog.modules.is_empty() {
            return Err(ModulePackageError::Bounds);
        }
        let source_identity = sha(&encode_json(
            &catalog.source_selection,
            inventory,
            inventory.limits().max_bytes,
            "source selection",
            false,
        )?);
        Ok(Self {
            catalog,
            artifact_root,
            catalog_identity: sha(&bytes),
            source_identity,
            source_policy: policy,
            records: Vec::new(),
        })
    }

    fn read_ref(
        &self,
        reference: &FileRef,
        limit: usize,
        inventory: &InventoryOperation,
    ) -> Result<Vec<u8>, ModulePackageError> {
        if reference.path.as_os_str().is_empty()
            || reference
                .path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(ModulePackageError::Format("artifact relative path"));
        }
        let path = self.artifact_root.join(&reference.path);
        if absolute(&path).as_ref() != Some(&path) {
            return Err(ModulePackageError::RootMoved);
        }
        if reference.length > limit as u64 {
            return Err(ModulePackageError::Bounds);
        }
        let bytes = read(&path, limit, inventory)?;
        if bytes.len() as u64 != reference.length || sha(&bytes) != reference.sha256 {
            return Err(ModulePackageError::ArtifactChanged(path));
        }
        Ok(bytes)
    }

    #[cfg(test)]
    fn records(&self, producer: &[u8]) -> Result<Vec<Record>, ModulePackageError> {
        if producer != self.catalog.producer_identity {
            return Err(ModulePackageError::CompilerMismatch);
        }
        Ok(self
            .records
            .iter()
            .map(|record| record.record.clone())
            .collect())
    }

    pub(super) fn candidates(
        &self,
        producer: &[u8],
    ) -> Result<Vec<(CandidateRecord, super::CandidateOrigin)>, ModulePackageError> {
        if producer != self.catalog.producer_identity {
            return Err(ModulePackageError::CompilerMismatch);
        }
        Ok(self
            .records
            .iter()
            .zip(&self.catalog.modules)
            .map(|(record, files)| {
                (
                    CandidateRecord::Deployment(Arc::clone(record)),
                    super::CandidateOrigin::Deployment {
                        interface: self.artifact_root.join(&files.interface.path),
                        packages: self.artifact_root.join(&files.packages.path),
                    },
                )
            })
            .collect())
    }

    fn read_records(
        &self,
        producer: &[u8],
        inventory: Arc<InventoryOperation>,
    ) -> Result<Vec<Arc<DecodedDeploymentRecord>>, ModulePackageError> {
        if producer != self.catalog.producer_identity {
            return Err(ModulePackageError::CompilerMismatch);
        }
        let evidence_references = evidence_references(&self.catalog)?;
        inventory
            .reserve::<DecodedDeploymentRecord>(self.catalog.modules.len())
            .and_then(|_| {
                inventory.reserve::<((String, String), [usize; 4])>(self.catalog.modules.len())
            })
            .and_then(|_| {
                inventory.reserve::<([u8; 32], [usize; 8])>(self.catalog.execution_graphs.len())
            })
            .map_err(|_| ModulePackageError::Bounds)?;
        let mut records = Vec::with_capacity(self.catalog.modules.len());
        let mut owners = BTreeSet::new();
        // Every physical evidence file is authenticated even when its exact
        // immutable proof is already owned by another module in this package.
        let mut evidence_proofs = std::collections::BTreeMap::<
            (String, u64),
            super::shared_evidence::SharedEvidence,
        >::new();
        let mut evidence_paths =
            std::collections::BTreeMap::<PathBuf, super::shared_evidence::SharedEvidence>::new();
        let mut evidence_validation = super::shared_evidence::ValidationStage::configured_package();
        let mut graphs = std::collections::BTreeMap::new();
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(
            inventory.clone(),
        );
        for reference in &self.catalog.execution_graphs {
            crate::host_work::checkpoint().map_err(ModulePackageError::Interrupted)?;
            let bytes = self.read_ref(
                reference,
                crate::execution_source::GRAPH_BYTES_LIMIT,
                &inventory,
            )?;
            let wire = inventory
                .decode_value(&bytes, crate::execution_source::GRAPH_BYTES_LIMIT)
                .map_err(|error| decode_error(error, "execution graph"))?;
            inventory
                .charge_value_copies(&wire, 2)
                .map_err(|_| ModulePackageError::Bounds)?;
            drop(wire);
            let digest = super::parse_sha(&reference.sha256)
                .ok_or(ModulePackageError::Format("execution graph digest"))?;
            let graph = crate::execution_source::CertifiedExecutionSourceGraph::recover_verified(
                bytes, digest,
            )
            .map_err(|_| ModulePackageError::Format("execution graph"))?;
            if graphs.insert(digest, graph).is_some() {
                return Err(ModulePackageError::Format("duplicate execution graph"));
            }
        }
        for files in &self.catalog.modules {
            crate::host_work::checkpoint().map_err(ModulePackageError::Interrupted)?;
            let owner: Owner = decode_json(
                &self.read_ref(&files.owner, RECORD_LIMIT, &inventory)?,
                &inventory,
                "owner JSON",
            )?;
            inventory
                .charge(
                    owner
                        .unit
                        .len()
                        .checked_add(owner.module.len())
                        .ok_or(ModulePackageError::Bounds)?,
                )
                .and_then(|_| inventory.reserve::<PathBuf>(owner.include.len()))
                .map_err(|_| ModulePackageError::Bounds)?;
            if !owners.insert((owner.unit.clone(), owner.module.clone())) {
                return Err(ModulePackageError::Format("duplicate module owner"));
            }
            let evidence = if let Some(proof) = evidence_paths.get(&files.evidence.path) {
                proof.clone()
            } else {
                let reference = evidence_references
                    .get(&files.evidence.path)
                    .ok_or(ModulePackageError::Format("dependency evidence reference"))?;
                let evidence_bytes = self.read_ref(reference, RECORD_LIMIT, &inventory)?;
                let evidence_key = (reference.sha256.clone(), reference.length);
                let proof = if let Some(proof) = evidence_proofs.get(&evidence_key) {
                    proof.clone()
                } else {
                    inventory
                        .reserve::<((String, u64), super::shared_evidence::SharedEvidence)>(1)
                        .and_then(|_| inventory.charge(evidence_key.0.len()))
                        .map_err(|_| ModulePackageError::Bounds)?;
                    let proof: super::shared_evidence::SharedEvidence =
                        decode_json(&evidence_bytes, &inventory, "dependency evidence JSON")?;
                    evidence_proofs.insert(evidence_key, proof.clone());
                    proof
                };
                evidence_paths.insert(files.evidence.path.clone(), proof.clone());
                proof
            };
            let mut record = Record {
                evidence: evidence.clone(),
                module_interface_proof: None,
                execution_source: None,
                data: super::RecordData {
                    evidence: evidence
                        .reference()
                        .ok_or(ModulePackageError::Bounds)?
                        .clone(),
                    tag: "TPMCAN".into(),
                    version: super::RECORD_VERSION,
                    endpoint: producer.to_vec(),
                    include: owner.include,
                    unit: owner.unit,
                    module: owner.module,
                    source: owner.source,
                    source_sha256: owner.source_sha256,
                    target_source: owner.target_source,
                    products: self.read_ref(
                        &files.products,
                        inventory.limits().max_module_bytes,
                        &inventory,
                    )?,
                    interface: self.read_ref(&files.interface, RECORD_LIMIT, &inventory)?,
                    package_imports: self.read_ref(&files.packages, RECORD_LIMIT, &inventory)?,
                    version_origin: owner
                        .version_origin
                        .unwrap_or(super::CandidateVersionOrigin::Ordinary),
                    original_owner: owner.original_owner.unwrap_or(super::OriginalOwner {
                        unit: String::new(),
                        module: String::new(),
                        module_version: [0; 32],
                        skinny_iface_sha256: [0; 32],
                        product_sha256: [0; 32],
                    }),
                    original_certification: self.read_ref(
                        &files.certification,
                        RECORD_LIMIT,
                        &inventory,
                    )?,
                    module_interface: Some(files.module_interface.clone()),
                    execution_source_sha256: owner.execution_source_sha256,
                },
            };
            if record.original_owner.owner() != super::computed_owner(&record) {
                return Err(ModulePackageError::Format("original full owner"));
            }
            let reference = &files.module_interface;
            for relative in [
                &reference.interface.interface_path,
                &reference.interface.package_imports_path,
                &reference.certificate_path,
            ]
            .into_iter()
            .chain(reference.core.iter().map(|core| &core.path))
            {
                if relative.as_os_str().is_empty()
                    || !relative
                        .components()
                        .all(|part| matches!(part, Component::Normal(_)))
                {
                    return Err(ModulePackageError::Format(
                        "canonical artifact relative path",
                    ));
                }
                let path = self.artifact_root.join(relative);
                if absolute(&path).as_ref() != Some(&path) {
                    return Err(ModulePackageError::RootMoved);
                }
            }
            let canonical = crate::recovery_artifacts::recover_module_interface(
                &self.artifact_root,
                reference,
                &mut validation,
            )
            .map_err(canonical_error)?;
            if canonical.producer_sha256()
                != crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                    producer,
                )
                .sha256()
                || super::hex(&canonical.source_sha256()) != record.source_sha256
            {
                return Err(ModulePackageError::Format("canonical producer/source"));
            }
            crate::certified_products::validate_canonical_native_bytes_with_operation(
                &record.original_owner.owner(),
                &record.original_certification,
                &record.interface,
                &record.package_imports,
                Some(
                    super::parse_sha(&record.source_sha256)
                        .ok_or(ModulePackageError::Format("canonical source digest"))?,
                ),
                &canonical,
                &inventory,
            )
            .map_err(|error| certification_error(error, "canonical native binding"))?;
            record.module_interface_proof = Some(canonical);
            if let Some(digest) = record.execution_source_sha256 {
                let graph = graphs
                    .get(&digest)
                    .ok_or(ModulePackageError::Format("missing execution graph"))?
                    .clone();
                super::validate_original_execution(&record, graph.clone(), &mut validation)
                    .ok_or(ModulePackageError::Format("execution source owner"))?;
                record.execution_source = Some(graph);
            }
            validate_dependency_evidence(&mut evidence_validation, &record)?;
            if owner.module_version != version_hash(&record)
                || record.include != self.catalog.source_selection.include_roots()
                || !self
                    .catalog
                    .source_selection
                    .contains_source(&record.source)
                || absolute(&record.source).as_ref() != Some(&record.source)
                || !record.evidence.selection_complete
            {
                crate::host_work::checkpoint().map_err(ModulePackageError::Interrupted)?;
                return Err(ModulePackageError::OpenCohort);
            }
            let decoded = DecodedDeploymentRecord::decode(record, &inventory)?;
            let record = decoded.record();
            if decoded.product.unit != record.unit
                || decoded.product.module != record.module
                || decoded.product.interface != record.interface
                || record.interface.is_empty()
                || sha(&read(&record.source, RECORD_LIMIT, &inventory)?) != record.source_sha256
                || record
                    .evidence
                    .modules
                    .iter()
                    .filter(|n| {
                        n.unit == record.unit
                            && n.module == record.module
                            && !n.boot
                            && n.source == record.source
                            && n.product == crate::cache::ProductAvailability::Ready
                    })
                    .count()
                    != 1
                || !record
                    .evidence
                    .sources
                    .iter()
                    .any(|s| s.path == record.source && s.sha256 == record.source_sha256)
            {
                return Err(ModulePackageError::Format("original module owner"));
            }
            use sha2::Digest;
            let iface_sha: [u8; 32] = sha2::Sha256::digest(&record.interface).into();
            crate::recovery_artifacts::validate_package_imports_with_validation(
                &record.package_imports,
                &record.unit,
                &record.module,
                &iface_sha,
                &self.artifact_root,
                &mut validation,
            )
            .map_err(canonical_error)?;
            records.push(Arc::new(decoded));
        }
        for record in &records {
            require_complete_cohort(
                records.iter().map(|record| record.record()),
                &record.record().evidence,
            )?;
        }
        validate_closed(
            records.iter().map(|record| record.record()),
            &self.catalog.source_selection,
        )?;
        tracing::info!(target: "tidepool_toolchain::module_candidates",
            phase = "configured_package_evidence", evidence_files = self.catalog.modules.len(),
            decoded_proofs = evidence_proofs.len(),
            shared_proof_reuses = self.catalog.modules.len() - evidence_proofs.len());
        Ok(records)
    }
}

fn validate_dependency_evidence(
    stage: &mut super::shared_evidence::ValidationStage,
    record: &Record,
) -> Result<(), ModulePackageError> {
    crate::host_work::checkpoint().map_err(ModulePackageError::Interrupted)?;
    match stage.validate(&record.evidence, &record.target_source) {
        Ok(()) => Ok(()),
        Err(crate::cache::DependencyEvidenceFailure::Interrupted) => {
            Err(ModulePackageError::Interrupted(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "compiler host validation interrupted",
            )))
        }
        Err(_) => Err(ModulePackageError::OpenCohort),
    }
}

fn validate_closed<'a>(
    records: impl IntoIterator<Item = &'a Record> + Clone,
    selection: &NativeCatalogSourceSelection,
) -> Result<(), ModulePackageError> {
    let mut evidence_validation = super::shared_evidence::ValidationStage::configured_package();
    let owners: std::collections::BTreeMap<_, _> = records
        .clone()
        .into_iter()
        .map(|r| ((r.unit.as_str(), r.module.as_str()), r.source.as_path()))
        .collect();
    for record in records {
        validate_dependency_evidence(&mut evidence_validation, record)?;
        if record.include != selection.include_roots()
            || !selection.contains_source(&record.source)
            || absolute(&record.source).as_ref() != Some(&record.source)
            || !record.evidence.selection_complete
            || record.evidence.modules.iter().any(|module| {
                module.source != Path::new("@generated-source")
                    && (!selection.contains_source(&module.source)
                        || absolute(&module.source).as_ref() != Some(&module.source))
            })
        {
            return Err(ModulePackageError::OpenCohort);
        }
        if record.evidence.sources.iter().any(|s| {
            s.path != Path::new("@generated-source")
                && (!selection.contains_source(&s.path)
                    || absolute(&s.path).as_ref() != Some(&s.path))
        }) {
            return Err(ModulePackageError::OpenCohort);
        }
        let node = record
            .evidence
            .modules
            .iter()
            .find(|n| n.unit == record.unit && n.module == record.module && !n.boot)
            .ok_or(ModulePackageError::OpenCohort)?;
        if node.imports.iter().any(|i| {
            i.selected.as_ref().is_some_and(|p| {
                !selection.contains_source(p)
                    || absolute(p).as_ref() != Some(p)
                    || i.boot
                    || owners
                        .get(&(record.unit.as_str(), i.module.as_str()))
                        .copied()
                        != Some(p.as_path())
            })
        }) {
            return Err(ModulePackageError::OpenCohort);
        }
    }
    Ok(())
}

fn require_complete_cohort<'a>(
    records: impl IntoIterator<Item = &'a Record>,
    evidence: &crate::cache::DependencyEvidence,
) -> Result<(), ModulePackageError> {
    let owners: BTreeSet<_> = records
        .into_iter()
        .map(|r| (r.unit.as_str(), r.module.as_str()))
        .collect();
    for module in &evidence.modules {
        if !module.boot
            && module.source != Path::new("@generated-source")
            && !owners.contains(&(module.unit.as_str(), module.module.as_str()))
        {
            return Err(ModulePackageError::IncompleteProduct {
                unit: module.unit.clone(),
                module: module.module.clone(),
                availability: module.product,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    mod product_carry;

    use super::super::tests::{
        package_bundle, package_bundle_with_sidecars, package_imports_with_roots, product_bytes,
    };
    use super::*;
    use crate::cache::{DependencyEvidence, ModuleEvidence, ProductAvailability, SourceEvidence};
    use tidepool_repr::execution_schema::InventoryDecodeLimits;

    struct Fixture {
        _root: tempfile::TempDir,
        source: PathBuf,
        output: PathBuf,
        authority: CompilerDeploymentAuthority,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_modules(&["Library"])
        }

        fn with_modules(names: &[&str]) -> Self {
            Self::with_modules_and_execution(names, false)
        }

        fn with_modules_and_execution(names: &[&str], with_execution: bool) -> Self {
            Self::with_modules_execution_and_packages(names, with_execution, &Default::default())
        }

        fn with_modules_execution_and_packages(
            names: &[&str],
            with_execution: bool,
            package_witnesses: &std::collections::BTreeMap<
                (String, String),
                crate::certified_products::PackageInterfaceWitness,
            >,
        ) -> Self {
            Self::with_modules_execution_packages_and_shadow(
                names,
                with_execution,
                package_witnesses,
                None,
            )
        }

        fn with_modules_execution_packages_and_shadow(
            names: &[&str],
            with_execution: bool,
            package_witnesses: &std::collections::BTreeMap<
                (String, String),
                crate::certified_products::PackageInterfaceWitness,
            >,
            shadow: Option<&Path>,
        ) -> Self {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("sources");
            let output = root.path().join("products");
            fs::create_dir(&source).unwrap();
            for role in NativeSourceRole::ORDERED {
                fs::create_dir_all(source.join(role.relative_root())).unwrap();
            }
            fs::write(
                source.join("TidepoolCatalog.hs"),
                "module TidepoolCatalog where\n",
            )
            .unwrap();
            let source = absolute(&source).unwrap();
            let module_source = |name: &str| {
                let role = match name {
                    "A" => NativeSourceRole::StableEffects,
                    "B" => NativeSourceRole::Actors,
                    "Jev" => NativeSourceRole::Jev,
                    _ => NativeSourceRole::Stdlib,
                };
                source.join(role.relative_root()).join(format!("{name}.hs"))
            };
            for name in names {
                fs::write(
                    module_source(name),
                    format!("module {name} where\nvalue = 7\n"),
                )
                .unwrap();
            }
            let producer_identity = [3; 32];
            let authority = CompilerDeploymentAuthority {
                schema: 1,
                producer_identity,
                consumed_worker_identity: [4; 32],
                frontend_path: "/configured/frontend".into(),
                worker_path: "/configured/worker".into(),
                ghc_libdir: "/configured/ghc".into(),
            };
            let evidence = DependencyEvidence {
                version: 4,
                cache_safe: true,
                selection_complete: true,
                sources: std::iter::once(SourceEvidence {
                    path: "@generated-source".into(),
                    sha256: sha(b"target"),
                })
                .chain(names.iter().map(|name| {
                    let path = module_source(name);
                    SourceEvidence {
                        sha256: sha(&fs::read(&path).unwrap()),
                        path,
                    }
                }))
                .collect(),
                modules: names
                    .iter()
                    .map(|name| ModuleEvidence {
                        unit: "u".into(),
                        module: (*name).into(),
                        boot: false,
                        source: module_source(name),
                        imports: vec![],
                        product: ProductAvailability::Ready,
                    })
                    .collect(),
                packages: vec![],
                resolutions: shadow
                    .into_iter()
                    .map(|shadow| {
                        let selected = module_source(names[0]);
                        crate::cache::ResolutionEvidence {
                            qualifier: crate::cache::ImportQualifier::Unqualified,
                            module: names[0].to_owned(),
                            boot: false,
                            selected: Some(selected.clone()),
                            candidates: vec![shadow.to_owned(), selected],
                        }
                    })
                    .collect(),
            };
            let bytes = combine_rows(
                names
                    .iter()
                    .map(|name| product_bytes("u", name, name.as_bytes())),
            );
            let packages = combine_rows(names.iter().map(|name| {
                if package_witnesses.is_empty() {
                    return package_bundle("u", name, name.as_bytes());
                }
                let roots = package_witnesses
                    .iter()
                    .map(|((unit, module), witness)| {
                        ciborium::value::Value::Array(vec![
                            ciborium::value::Value::Text(unit.clone()),
                            ciborium::value::Value::Text(module.clone()),
                            ciborium::value::Value::Text(
                                witness.selected_path.to_str().unwrap().into(),
                            ),
                            ciborium::value::Value::Text(super::super::hex(&witness.sha256)),
                        ])
                    })
                    .collect();
                package_bundle_with_sidecars(vec![(
                    "u".into(),
                    (*name).into(),
                    package_imports_with_roots("u", name, name.as_bytes(), roots),
                )])
            }));
            let parsed =
                crate::certified_products::ParsedModuleProducts::decode(&bytes, &packages).unwrap();
            let selection =
                NativeCatalogSourceSelection::capture_under(&source, RootPolicy::Fixture).unwrap();
            let include = selection.include_roots();
            let (_, mut prepared) = super::super::prepare_publication(
                &producer_identity,
                &include,
                &evidence,
                parsed,
                "target",
                super::super::CandidateVersionOrigin::Ordinary,
                &[],
            );
            for record in &mut prepared.records {
                let owner = record.original_owner.owner();
                let product =
                    crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
                        owner.clone(),
                        record.interface.clone(),
                        record.products.clone(),
                        record.package_imports.clone(),
                        crate::certified_products::encode_home_certification(
                            &owner,
                            &[],
                            package_witnesses,
                        )
                        .unwrap(),
                    )
                    .with_source_sha256(super::super::parse_sha(&record.source_sha256).unwrap());
                let product = crate::certified_products::fixture_finalized_product(
                    product,
                    crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                        &producer_identity,
                    )
                    .sha256(),
                );
                record.original_certification = product.certification_bytes().to_vec();
                record.module_interface_proof = product.module_interface().cloned();
            }
            if with_execution {
                use crate::execution_source::{
                    CertifiedExecutionSourceGraph, ExecutionSourceAdmission,
                    ExecutionSourceGraphInput,
                };
                let input = root.path().join("Input.hs");
                fs::write(&input, b"target").unwrap();
                let owners = prepared
                    .records
                    .iter()
                    .map(|record| super::super::computed_owner(&record.data))
                    .collect::<Vec<_>>();
                let fresh = owners
                    .iter()
                    .map(|owner| crate::declaration_join::ExactModuleIdentity {
                        unit: owner.unit.clone(),
                        module: owner.module.clone(),
                    })
                    .collect();
                let ExecutionSourceAdmission::Available(graph) = CertifiedExecutionSourceGraph::admit(ExecutionSourceGraphInput {
                    producer: crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&producer_identity),
                    semantic_sha256: None, include: &include, source_path: &input, source: "target", evidence: &evidence,
                    exact_imports: &std::collections::BTreeMap::new(), owners: &owners, fresh_owners: &fresh,
                    retained_sources: &std::collections::BTreeMap::new(), packages: package_witnesses,
                }).unwrap() else { panic!("graph"); };
                for record in &mut prepared.records {
                    let owner = record.original_owner.owner();
                    let seal = &record.original_certification;
                    record.original_certification =
                        crate::certified_products::bind_home_execution_source(
                            seal,
                            &owner,
                            graph.digest(),
                            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                        )
                        .unwrap();
                    record.execution_source_sha256 = Some(graph.digest());
                    record.execution_source = Some(graph.clone());
                }
                prepared.graphs.insert(graph.digest(), graph);
            }
            export_under(
                &output,
                &selection,
                &crate::toolchain::AdmittedCompilerDeployment {
                    producer_identity,
                    consumed_worker_identity: [4; 32],
                },
                &prepared,
                RootPolicy::Fixture,
            )
            .unwrap();
            Self {
                _root: root,
                source,
                output,
                authority,
            }
        }

        fn load(&self) -> Result<DeploymentModulePackage, ModulePackageError> {
            DeploymentModulePackage::load_under(
                &self.output.join("catalog.json"),
                &self.authority,
                RootPolicy::Fixture,
            )
        }

        fn catalog(&self) -> Catalog {
            serde_json::from_slice(&fs::read(self.output.join("catalog.json")).unwrap()).unwrap()
        }
    }

    fn combine_rows(encoded: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
        use ciborium::value::Value;
        let mut header = None;
        let mut rows = Vec::new();
        for bytes in encoded {
            let Value::Array(mut fields) = ciborium::de::from_reader(bytes.as_slice()).unwrap()
            else {
                panic!("fixture array")
            };
            let Value::Array(mut next) = fields.pop().unwrap() else {
                panic!("fixture rows")
            };
            rows.append(&mut next);
            header.get_or_insert(fields);
        }
        let mut fields = header.unwrap();
        fields.push(Value::Array(rows));
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&Value::Array(fields), &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn interrupted_package_authentication_refuses_and_fresh_load_recovers() {
        let fixture = Fixture::new();
        let cancellation = tidepool_extract_cmd::CompilerTransactionCancellation::new();
        cancellation.cancel();
        let outcome = tidepool_extract_cmd::with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || fixture.load(),
        );
        assert!(
            matches!(outcome.action, Err(ModulePackageError::Interrupted(error)) if error.kind() == std::io::ErrorKind::Interrupted)
        );
        assert_eq!(
            outcome.close,
            tidepool_extract_cmd::CompilerTransactionClose::NotStarted
        );
        let outcome = tidepool_extract_cmd::with_compiler_transaction_cancellable(
            tidepool_extract_cmd::CompilerTransactionCancellation::new(),
            |_| {},
            || fixture.load(),
        );
        assert!(!outcome.action.unwrap().records.is_empty());
        assert_eq!(
            outcome.close,
            tidepool_extract_cmd::CompilerTransactionClose::NotStarted
        );
    }

    #[test]
    fn complete_catalog_preserves_more_than_optional_candidate_capacity() {
        let names = (0..129)
            .map(|index| format!("Owner{index:03}"))
            .collect::<Vec<_>>();
        let expected = names.iter().cloned().collect::<BTreeSet<_>>();
        let fixture = Fixture::with_modules(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let path = fixture.output.join("catalog.json");
        let catalog = fixture.catalog();
        assert_eq!(catalog.modules.len(), expected.len());
        let package = fixture.load().unwrap();
        assert_eq!(package.records.len(), expected.len());
        assert_eq!(
            package
                .records
                .iter()
                .map(|record| record.module.clone())
                .collect::<BTreeSet<_>>(),
            expected
        );

        // JSON whitespace changes no receipt or source selection. Exact metadata
        // uses the enclosing inventory budget, not a small cache-manifest cap.
        let mut bytes = fs::read(&path).unwrap();
        bytes.resize(bytes.len().max((1 << 20) + 1), b' ');
        fs::write(&path, &bytes).unwrap();
        let package = fixture.load().unwrap();
        assert_eq!(package.records.len(), expected.len());
        assert_eq!(
            package
                .records
                .iter()
                .map(|record| record.module.clone())
                .collect::<BTreeSet<_>>(),
            expected
        );

        let mut incomplete = catalog.clone();
        incomplete.modules.pop().unwrap();
        fs::write(&path, serde_json::to_vec(&incomplete).unwrap()).unwrap();
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::IncompleteProduct { .. })
        ));
        fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        let last = catalog.modules.last().unwrap();
        fs::write(
            fixture.output.join(&last.products.path),
            b"changed original",
        )
        .unwrap();
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::ArtifactChanged(_))
        ));
    }

    #[test]
    fn complete_catalog_reads_share_work_and_refuse_byte_limits() {
        let fixture = Fixture::with_modules(&["A", "B"]);
        let path = fixture.output.join("catalog.json");
        let package = DeploymentModulePackage::read_catalog_under(
            &path,
            &fixture.authority,
            RootPolicy::Fixture,
        )
        .unwrap();
        let first = &package.catalog.modules[0].products;
        let second = &package.catalog.modules[1].products;
        let limits = InventoryDecodeLimits {
            max_work: usize::try_from(first.length + second.length + 1).unwrap(),
            ..Default::default()
        };
        for reference in [first, second] {
            assert!(package
                .read_ref(
                    reference,
                    limits.max_module_bytes,
                    &InventoryOperation::new(limits)
                )
                .is_ok());
        }
        let shared = InventoryOperation::new(limits);
        assert!(package
            .read_ref(first, limits.max_module_bytes, &shared)
            .is_ok());
        assert!(matches!(
            package.read_ref(second, limits.max_module_bytes, &shared),
            Err(ModulePackageError::Bounds)
        ));

        let bytes = fs::metadata(&path).unwrap().len();
        for limits in [
            InventoryDecodeLimits {
                max_bytes: usize::try_from(bytes - 1).unwrap(),
                ..Default::default()
            },
            InventoryDecodeLimits {
                max_work: 1,
                ..Default::default()
            },
        ] {
            assert!(matches!(
                DeploymentModulePackage::load_with_inventory(
                    &path,
                    &fixture.authority,
                    RootPolicy::Fixture,
                    Arc::new(InventoryOperation::new(limits))
                ),
                Err(ModulePackageError::Bounds)
            ));
        }
        assert_eq!(fixture.load().unwrap().records.len(), 2);
    }

    #[test]
    fn complete_catalog_keeps_record_and_module_file_bounds() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        for (reference, limit) in [
            (&catalog.modules[0].owner, RECORD_LIMIT),
            (
                &catalog.modules[0].products,
                InventoryDecodeLimits::default().max_module_bytes,
            ),
        ] {
            let path = fixture.output.join(&reference.path);
            let original = fs::read(&path).unwrap();
            fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len((limit as u64) + 1)
                .unwrap();
            assert!(matches!(fixture.load(), Err(ModulePackageError::Bounds)));
            fs::write(&path, original).unwrap();
        }
        assert_eq!(fixture.load().unwrap().records.len(), 1);
    }

    #[test]
    fn independent_files_preserve_original_owner_across_current_include_changes() {
        let fixture = Fixture::new();
        let package = fixture.load().unwrap();
        assert_eq!(package.source_selection().snapshot_root, fixture.source);
        let records = package.records(&[3; 32]).unwrap();
        let original = version_hash(&records[0]);
        let scratch = tempfile::tempdir().unwrap();
        let mut current = vec![scratch.path().to_path_buf()];
        current.extend(package.source_selection().include_roots());
        let selected = super::super::select_records_inner(
            &[3; 32],
            &current,
            scratch.path(),
            package.candidates(&[3; 32]).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(selected.by_owner.len(), 1);
        assert_eq!(
            selected.by_owner[&("u".into(), "Library".into())]
                .owner
                .module_version
                .0,
            original
        );
        let ordinary = super::super::select_records(
            &[3; 32],
            &current,
            scratch.path(),
            fixture
                .load()
                .unwrap()
                .records(&[3; 32])
                .unwrap()
                .into_iter()
                .map(|r| (r, super::super::CandidateOrigin::Ordinary))
                .collect(),
        )
        .unwrap();
        assert!(ordinary.by_owner.is_empty());
        let catalog = fixture.catalog();
        let files = &catalog.modules[0];
        let paths = [
            &files.owner.path,
            &files.products.path,
            &files.interface.path,
            &files.packages.path,
            &files.evidence.path,
        ];
        assert_eq!(paths.into_iter().collect::<BTreeSet<_>>().len(), 5);
    }

    #[test]
    fn deployment_execution_proof_is_shared_and_advertised_corruption_refuses() {
        let fixture = Fixture::with_modules_and_execution(&["A", "B"], true);
        let catalog = fixture.catalog();
        assert_eq!(catalog.schema, 4);
        assert_eq!(catalog.execution_graphs.len(), 1);
        assert!(catalog
            .modules
            .iter()
            .all(|files| files.certification.length > 0));
        let package = fixture.load().unwrap();
        let records = package.records(&[3; 32]).unwrap();
        let a = records[0].execution_source.as_ref().unwrap();
        let b = records[1].execution_source.as_ref().unwrap();
        assert!(std::sync::Arc::ptr_eq(a, b));
        let path = fixture.output.join(&catalog.execution_graphs[0].path);
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, b"corrupt").unwrap();
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::ArtifactChanged(_))
        ));
        fs::remove_file(&path).unwrap();
        assert!(matches!(fixture.load(), Err(ModulePackageError::RootMoved)));
        fs::write(&path, bytes).unwrap();
        let seal = fixture
            .output
            .join(catalog.modules[0].certification.path.clone());
        fs::write(seal, b"corrupt seal").unwrap();
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::ArtifactChanged(_))
        ));
    }

    #[test]
    fn strict_catalog_preserves_core_and_refuses_legacy_and_changed_companions() {
        let fixture = Fixture::new();
        let mut catalog = fixture.catalog();
        assert_eq!(catalog.schema, 4);
        let core = catalog.modules[0].module_interface.core.as_ref().unwrap();
        let core_path = fixture.output.join(&core.path);
        let original = fs::read(&core_path).unwrap();
        fs::write(&core_path, b"changed core").unwrap();
        assert!(
            matches!(fixture.load(), Err(ModulePackageError::ArtifactChanged(path)) if path == core_path)
        );
        fs::write(&core_path, original).unwrap();
        let mut legacy: serde_json::Value = serde_json::to_value(&catalog).unwrap();
        legacy.as_object_mut().unwrap().remove("source_selection");
        legacy["source_root"] = serde_json::to_value(&fixture.source).unwrap();
        legacy["source_files"] =
            serde_json::to_value(&catalog.source_selection.source_files).unwrap();
        fs::write(
            fixture.output.join("catalog.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::Format("catalog JSON"))
        ));
        for schema in [1, 2, 3] {
            catalog.schema = schema;
            fs::write(
                fixture.output.join("catalog.json"),
                serde_json::to_vec(&catalog).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                fixture.load(),
                Err(ModulePackageError::Format("catalog schema"))
            ));
        }
    }

    #[test]
    fn configured_compiler_and_worker_are_independently_checked() {
        let fixture = Fixture::new();
        for worker in [false, true] {
            let mut authority = fixture.authority.clone();
            if worker {
                authority.consumed_worker_identity = [8; 32]
            } else {
                authority.producer_identity = [8; 32]
            }
            assert!(matches!(
                DeploymentModulePackage::load_under(
                    &fixture.output.join("catalog.json"),
                    &authority,
                    RootPolicy::Fixture
                ),
                Err(ModulePackageError::CompilerMismatch)
            ));
        }
    }

    #[test]
    fn product_container_relocation_preserves_original_source_and_proof_bytes() {
        let fixture = Fixture::new();
        let original = fixture.load().unwrap();
        let catalog = fs::read(fixture.output.join("catalog.json")).unwrap();
        let other = fixture._root.path().join("relocated");
        fs::rename(&fixture.output, &other).unwrap();
        let relocated = DeploymentModulePackage::load_under(
            &other.join("catalog.json"),
            &fixture.authority,
            RootPolicy::Fixture,
        )
        .unwrap();
        assert_eq!(fs::read(other.join("catalog.json")).unwrap(), catalog);
        assert_eq!(original.catalog_identity(), relocated.catalog_identity());
        assert_eq!(original.source_selection(), relocated.source_selection());
        assert_eq!(original.source_identity(), relocated.source_identity());
        let before = original.records(&[3; 32]).unwrap();
        let after = relocated.records(&[3; 32]).unwrap();
        assert_eq!(before[0].source, after[0].source);
        assert_eq!(before[0].include, after[0].include);
        assert_eq!(before[0].products, after[0].products);
        assert_eq!(before[0].interface, after[0].interface);
        assert_eq!(
            before[0].original_certification,
            after[0].original_certification
        );
        assert_eq!(
            before[0].original_owner.owner(),
            after[0].original_owner.owner()
        );

        let candidates = relocated.candidates(&[3; 32]).unwrap();
        let super::super::CandidateOrigin::Deployment {
            interface,
            packages,
        } = &candidates[0].1
        else {
            panic!("deployment candidate");
        };
        assert!(interface.starts_with(&other));
        assert!(packages.starts_with(&other));

        let alias = fixture._root.path().join("product-alias");
        std::os::unix::fs::symlink(&other, &alias).unwrap();
        assert!(matches!(
            DeploymentModulePackage::load_under(
                &alias.join("catalog.json"),
                &fixture.authority,
                RootPolicy::Fixture
            ),
            Err(ModulePackageError::RootMoved)
        ));

        fs::rename(&fixture.source, fixture._root.path().join("moved-sources")).unwrap();
        assert!(matches!(
            DeploymentModulePackage::load_under(
                &other.join("catalog.json"),
                &fixture.authority,
                RootPolicy::Fixture
            ),
            Err(ModulePackageError::RootMoved)
        ));
    }

    #[test]
    fn source_addition_edit_and_removal_refuse_frozen_provenance() {
        let fixture = Fixture::new();
        let source = fixture.source.join("lib/Library.hs");
        let original = fs::read(&source).unwrap();
        fs::write(
            fixture.source.join("actors/Added.hs"),
            "module Added where\n",
        )
        .unwrap();
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::SourceChanged)
        ));
        fs::remove_file(fixture.source.join("actors/Added.hs")).unwrap();
        fs::write(&source, "module Library where\nvalue = 9\n").unwrap();
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::SourceChanged)
        ));
        fs::remove_file(&source).unwrap();
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::SourceChanged)
        ));
        fs::write(&source, original).unwrap();
        assert!(fixture.load().is_ok());
    }

    #[test]
    fn ordered_roles_and_complete_original_snapshot_are_required() {
        let fixture = Fixture::with_modules(&["A", "Library", "B", "Jev"]);
        let original = fixture.catalog();
        let selection = &original.source_selection;
        assert_eq!(selection.roles, NativeSourceRole::ORDERED);
        assert_eq!(
            selection.include_roots(),
            ["effects", "lib", "actors", "jev/core"].map(|relative| fixture.source.join(relative))
        );
        assert!(selection
            .source_files
            .iter()
            .any(|file| file.path == Path::new("TidepoolCatalog.hs")));
        assert_eq!(fixture.load().unwrap().records.len(), 4);
        for roles in [
            [
                NativeSourceRole::Stdlib,
                NativeSourceRole::StableEffects,
                NativeSourceRole::Actors,
                NativeSourceRole::Jev,
            ],
            [
                NativeSourceRole::StableEffects,
                NativeSourceRole::Stdlib,
                NativeSourceRole::Stdlib,
                NativeSourceRole::Jev,
            ],
        ] {
            let mut altered = original.clone();
            altered.source_selection.roles = roles;
            fs::write(
                fixture.output.join("catalog.json"),
                serde_json::to_vec(&altered).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                fixture.load(),
                Err(ModulePackageError::Format("native source role order"))
            ));
        }
        fs::write(
            fixture.output.join("catalog.json"),
            serde_json::to_vec(&original).unwrap(),
        )
        .unwrap();
        for relative in ["effects", "lib", "actors", "jev/core"] {
            let role_root = fixture.source.join(relative);
            let saved = fixture._root.path().join("saved-role");
            fs::rename(&role_root, &saved).unwrap();
            assert!(matches!(
                fixture.load(),
                Err(ModulePackageError::Format("native source role directory"))
            ));
            fs::rename(&saved, &role_root).unwrap();
        }
        fs::write(fixture.source.join("TidepoolCatalog.hs"), "changed probe").unwrap();
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::SourceChanged)
        ));
    }

    #[test]
    fn source_projection_shares_authority_and_provenance_validation() {
        let fixture = Fixture::new();
        let path = fixture.output.join("catalog.json");
        let catalog = fixture.catalog();
        fs::write(
            fixture.output.join(&catalog.modules[0].products.path),
            "broken product",
        )
        .unwrap();
        let projected = DeploymentModulePackage::read_catalog_under(
            &path,
            &fixture.authority,
            RootPolicy::Fixture,
        )
        .unwrap();
        assert_eq!(projected.source_selection(), &catalog.source_selection);
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::ArtifactChanged(_))
        ));
        let mut wrong_authority = fixture.authority.clone();
        wrong_authority.consumed_worker_identity = [8; 32];
        assert!(matches!(
            DeploymentModulePackage::read_catalog_under(
                &path,
                &wrong_authority,
                RootPolicy::Fixture
            ),
            Err(ModulePackageError::CompilerMismatch)
        ));
        fs::write(fixture.source.join("effects/New.hs"), "module New where\n").unwrap();
        assert!(matches!(
            DeploymentModulePackage::read_catalog_under(
                &path,
                &fixture.authority,
                RootPolicy::Fixture
            ),
            Err(ModulePackageError::SourceChanged)
        ));
    }

    #[test]
    fn closed_cohort_accepts_cross_role_imports_and_refuses_snapshot_siblings() {
        use crate::cache::{ImportQualifier, ModuleImportEvidence, ResolutionEvidence};
        let fixture = Fixture::with_modules(&["A", "Library", "B", "Jev"]);
        let package = fixture.load().unwrap();
        let selection = package.source_selection().clone();
        let mut records = package.records(&[3; 32]).unwrap();
        let imported = records
            .iter()
            .find(|record| record.module == "B")
            .unwrap()
            .source
            .clone();
        let record = records
            .iter_mut()
            .find(|record| record.module == "A")
            .unwrap();
        let evidence = record.evidence.make_mut();
        evidence
            .modules
            .iter_mut()
            .find(|module| module.module == "A")
            .unwrap()
            .imports
            .push(ModuleImportEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "B".into(),
                boot: false,
                selected: Some(imported.clone()),
            });
        evidence.resolutions.push(ResolutionEvidence {
            qualifier: ImportQualifier::Unqualified,
            module: "B".into(),
            boot: false,
            selected: Some(imported.clone()),
            candidates: vec![imported],
        });
        assert!(validate_closed(&records, &selection).is_ok());
        let outside = fixture.source.join("Sibling.hs");
        fs::write(&outside, "module Sibling where\n").unwrap();
        records[0].evidence.make_mut().sources.push(SourceEvidence {
            path: outside.clone(),
            sha256: sha(&fs::read(&outside).unwrap()),
        });
        assert!(matches!(
            validate_closed(&records, &selection),
            Err(ModulePackageError::OpenCohort)
        ));
        assert!(!selection.contains_source(&fixture.source.join("lib/../Sibling.hs")));
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::SourceChanged)
        ));
        fs::remove_file(outside).unwrap();
        records = fixture.load().unwrap().records(&[3; 32]).unwrap();
        records[0].data.include.swap(0, 1);
        assert!(matches!(
            validate_closed(&records, &selection),
            Err(ModulePackageError::OpenCohort)
        ));
    }

    #[test]
    fn independent_product_corruption_refuses_before_candidate_selection() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let product = fixture.output.join(&catalog.modules[0].products.path);
        fs::write(&product, b"corrupt").unwrap();
        assert!(
            matches!(fixture.load(),Err(ModulePackageError::ArtifactChanged(path)) if path == product)
        );
    }

    #[test]
    fn mutable_author_or_dev_roots_cannot_become_deployment_roots() {
        let fixture = Fixture::new();
        assert!(matches!(
            DeploymentModulePackage::load(&fixture.output.join("catalog.json"), &fixture.authority),
            Err(ModulePackageError::MutableRoot)
        ));
        assert!(immutable_store_path(Path::new(
            "/nix/store/00000000000000000000000000000000-source/lib"
        )));
        assert!(!immutable_store_path(Path::new(
            "/nix/store-lookalike/00000000000000000000000000000000-source/lib"
        )));
        assert!(!immutable_store_path(Path::new("/nix/store/source/lib")));
        assert!(!immutable_store_path(Path::new(
            "/nix/store/00000000000000000000000000000000-source/../../tmp"
        )));
    }

    #[test]
    fn source_file_and_empty_directory_aliases_refuse_complete_frozen_root() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let mutable = fixture._root.path().join("mutable");
        fs::create_dir(&mutable).unwrap();
        let alias = fixture.source.join("empty-alias");
        symlink(&mutable, &alias).unwrap();
        assert!(matches!(fixture.load(),Err(ModulePackageError::SourceAlias(p)) if p == alias));
        fs::remove_file(&alias).unwrap();
        let alias = fixture.source.join("Unselected.hs");
        symlink(fixture.source.join("lib/Library.hs"), &alias).unwrap();
        assert!(matches!(fixture.load(),Err(ModulePackageError::SourceAlias(p)) if p == alias));
    }

    #[test]
    #[serial_test::serial]
    fn invalid_configured_catalog_refuses_at_the_candidate_front_door() {
        struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (name, value) in self.0.drain(..) {
                    match value {
                        Some(value) => unsafe { std::env::set_var(name, value) },
                        None => unsafe { std::env::remove_var(name) },
                    }
                }
            }
        }
        let fixture = Fixture::new();
        let names = [
            crate::toolchain::ENV_COMPILER_MODULES,
            crate::toolchain::ENV_COMPILER_DEPLOYMENT,
        ];
        let _restore = Restore(
            names
                .into_iter()
                .map(|name| (name, std::env::var_os(name)))
                .collect(),
        );
        let configured = fixture._root.path().join("compiler.json");
        fs::write(&configured, serde_json::to_vec(&fixture.authority).unwrap()).unwrap();
        let malformed = fixture._root.path().join("catalog.json");
        fs::write(&malformed, b"not a catalog").unwrap();
        unsafe {
            std::env::set_var(crate::toolchain::ENV_COMPILER_MODULES, &malformed);
            std::env::set_var(crate::toolchain::ENV_COMPILER_DEPLOYMENT, &configured);
        }
        assert!(matches!(
            crate::artifacts::ModuleCandidateOffer::select(
                &[3; 32],
                &[fixture.source.clone()],
                &fixture.output
            ),
            Err(crate::CompileError::ModulePackage(
                ModulePackageError::Format("catalog JSON")
            ))
        ));
        unsafe { std::env::remove_var(crate::toolchain::ENV_COMPILER_DEPLOYMENT) }
        assert!(matches!(
            crate::toolchain::configured_module_package(),
            Err(ModulePackageError::UnknownCompiler)
        ));
    }

    #[test]
    fn changed_original_owner_and_open_home_closure_refuse_package_hydration() {
        use crate::cache::{ImportQualifier, ModuleImportEvidence};
        let fixture = Fixture::new();
        let mut catalog = fixture.catalog();
        let files = &mut catalog.modules[0];
        let path = fixture.output.join(&files.owner.path);
        let original = fs::read(&path).unwrap();
        let mut owner: Owner = serde_json::from_slice(&original).unwrap();
        owner.module_version = [9; 32];
        let altered = serde_json::to_vec(&owner).unwrap();
        fs::write(&path, &altered).unwrap();
        files.owner.sha256 = sha(&altered);
        files.owner.length = altered.len() as u64;
        fs::write(
            fixture.output.join("catalog.json"),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::OpenCohort)
        ));

        let other = Fixture::new();
        let mut records = other.load().unwrap().records(&[3; 32]).unwrap();
        records[0].evidence.make_mut().modules[0]
            .imports
            .push(ModuleImportEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "Missing".into(),
                boot: false,
                selected: Some(other.source.join("actors/Missing.hs")),
            });
        assert!(matches!(
            validate_closed(&records, &other.load().unwrap().catalog.source_selection),
            Err(ModulePackageError::OpenCohort)
        ));
        records[0].evidence.make_mut().modules[0].imports[0].module = "Library".into();
        records[0].evidence.make_mut().modules[0].imports[0].selected =
            Some(other.source.join("effects/WrongSource.hs"));
        assert!(matches!(
            validate_closed(&records, &other.load().unwrap().catalog.source_selection),
            Err(ModulePackageError::OpenCohort)
        ));
    }

    #[test]
    fn strict_producer_cannot_hide_missing_prelude_behind_closed_leaves() {
        let fixture = Fixture::new();
        let records = fixture.load().unwrap().records(&[3; 32]).unwrap();
        let mut evidence = (*records[0].evidence).clone();
        evidence.modules.push(ModuleEvidence {
            unit: "u".into(),
            module: "Tidepool.Prelude".into(),
            source: fixture.source.join("lib/Tidepool/Prelude.hs"),
            boot: false,
            imports: vec![],
            product: ProductAvailability::ProjectionRejected,
        });
        assert!(matches!(require_complete_cohort(&records,&evidence),
            Err(ModulePackageError::IncompleteProduct { module, availability: ProductAvailability::ProjectionRejected, .. })
                if module == "Tidepool.Prelude"));
        evidence.modules.last_mut().unwrap().product = ProductAvailability::Ready;
        assert!(matches!(
            require_complete_cohort(&records, &evidence),
            Err(ModulePackageError::IncompleteProduct {
                availability: ProductAvailability::Ready,
                ..
            })
        ));
    }

    #[test]
    fn catalog_shrinking_refuses_the_retained_full_producing_cohort() {
        let fixture = Fixture::with_modules(&["Library", "Tidepool.Prelude"]);
        assert_eq!(fixture.load().unwrap().records.len(), 2);
        let mut catalog = fixture.catalog();
        catalog.modules.retain(|files| {
            let owner: Owner =
                serde_json::from_slice(&fs::read(fixture.output.join(&files.owner.path)).unwrap())
                    .unwrap();
            owner.module != "Tidepool.Prelude"
        });
        assert_eq!(catalog.modules.len(), 1);
        fs::write(
            fixture.output.join("catalog.json"),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        assert!(
            matches!(fixture.load(), Err(ModulePackageError::IncompleteProduct {
            module, availability: ProductAvailability::Ready, ..
        }) if module == "Tidepool.Prelude")
        );
    }
}

fn write_ref(
    root: &Path,
    relative: PathBuf,
    bytes: &[u8],
    limit: usize,
    inventory: &InventoryOperation,
) -> Result<FileRef, ModulePackageError> {
    let limit = limit.min(inventory.limits().max_bytes);
    if bytes.len() > limit {
        return Err(ModulePackageError::Bounds);
    }
    inventory
        .charge(bytes.len())
        .and_then(|_| inventory.reserve::<FileRef>(1))
        .map_err(|_| ModulePackageError::Bounds)?;
    let path = root.join(&relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io(parent, e))?
    }
    tidepool_atomic_write::write_best_effort(&path, bytes).map_err(|e| io(&path, e.into()))?;
    Ok(FileRef {
        path: relative,
        sha256: sha(bytes),
        length: bytes.len() as u64,
    })
}

/// Export only after the shared front door has authenticated worker receipts.
pub(crate) fn export(
    output_root: &Path,
    source_selection: &NativeCatalogSourceSelection,
    authority: &crate::toolchain::AdmittedCompilerDeployment,
    prepared: &super::PreparedPublication<'_>,
) -> Result<(), ModulePackageError> {
    export_under(
        output_root,
        source_selection,
        authority,
        prepared,
        RootPolicy::NixStore,
    )
}

fn export_under(
    output_root: &Path,
    source_selection: &NativeCatalogSourceSelection,
    authority: &crate::toolchain::AdmittedCompilerDeployment,
    prepared: &super::PreparedPublication<'_>,
    policy: RootPolicy,
) -> Result<(), ModulePackageError> {
    let inventory = &prepared.inventory;
    let producer = prepared.endpoint_identity;
    let include = prepared.include;
    let evidence = prepared.evidence;
    source_selection.validate_under(policy)?;
    if producer != authority.producer_identity {
        return Err(ModulePackageError::CompilerMismatch);
    }
    if include != source_selection.include_roots() {
        return Err(ModulePackageError::OpenCohort);
    }
    if !output_root.is_absolute() {
        return Err(ModulePackageError::RootMoved);
    }
    fs::create_dir_all(output_root).map_err(|e| io(output_root, e))?;
    if absolute(output_root).as_deref() != Some(output_root) {
        return Err(ModulePackageError::RootMoved);
    }
    if output_root.join("catalog.json").exists() {
        return Err(ModulePackageError::Format("catalog already exists"));
    }
    let records = &prepared.records;
    require_complete_cohort(records, evidence)?;
    if records.is_empty() {
        return Err(ModulePackageError::Bounds);
    }
    for record in records {
        require_complete_cohort(records, &record.evidence)?;
    }
    validate_closed(records, source_selection)?;
    inventory
        .reserve::<ModuleFiles>(records.len())
        .and_then(|_| inventory.reserve::<FileRef>(prepared.graphs.len()))
        .map_err(|_| ModulePackageError::Bounds)?;
    let mut modules = Vec::with_capacity(records.len());
    let mut shared_evidence = std::collections::BTreeMap::<String, (Vec<u8>, FileRef)>::new();
    let mut validation =
        crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(inventory.clone());
    for record in records {
        let canonical =
            record
                .module_interface_proof
                .as_ref()
                .ok_or(ModulePackageError::Format(
                    "missing canonical module interface",
                ))?;
        if canonical.producer_sha256()
            != crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
                .sha256()
            || super::hex(&canonical.source_sha256()) != record.source_sha256
        {
            return Err(ModulePackageError::Format("canonical producer/source"));
        }
        if record.original_certification.is_empty() {
            return Err(ModulePackageError::Format("missing native certification"));
        }
        for (bytes, limit) in [
            (
                record.products.as_slice(),
                inventory.limits().max_module_bytes,
            ),
            (record.interface.as_slice(), RECORD_LIMIT),
            (record.package_imports.as_slice(), RECORD_LIMIT),
            (record.original_certification.as_slice(), RECORD_LIMIT),
        ] {
            if bytes.len() > limit {
                return Err(ModulePackageError::Bounds);
            }
        }
        let module_interface = crate::recovery_artifacts::materialize_module_interface(
            output_root,
            canonical,
            &mut validation,
            crate::recovery_artifacts::MaterializationMode::Durable,
        )
        .map_err(canonical_error)?;
        let directory =
            PathBuf::from("modules")
                .join(sha(format!("{}:{}", record.unit, record.module).as_bytes()));
        let owner = Owner {
            unit: record.unit.clone(),
            module: record.module.clone(),
            source: record.source.clone(),
            source_sha256: record.source_sha256.clone(),
            include: record.include.clone(),
            target_source: record.target_source.clone(),
            module_version: version_hash(&record),
            original_owner: Some(record.original_owner.clone()),
            version_origin: Some(record.version_origin.clone()),
            execution_source_sha256: record.execution_source_sha256,
        };
        let evidence_bytes = encode_json(
            &record.evidence,
            inventory,
            RECORD_LIMIT,
            "evidence encoding",
            false,
        )?;
        let evidence_sha256 = sha(&evidence_bytes);
        let evidence = if let Some((existing_bytes, reference)) =
            shared_evidence.get(&evidence_sha256)
        {
            if existing_bytes.len() != evidence_bytes.len() || existing_bytes != &evidence_bytes {
                return Err(ModulePackageError::Format(
                    "dependency evidence digest collision",
                ));
            }
            reference.clone()
        } else {
            inventory
                .reserve::<(Vec<u8>, FileRef)>(1)
                .and_then(|_| inventory.charge(evidence_bytes.len()))
                .map_err(|_| ModulePackageError::Bounds)?;
            let reference = write_ref(
                output_root,
                PathBuf::from("evidence").join(format!("{evidence_sha256}.json")),
                &evidence_bytes,
                RECORD_LIMIT,
                inventory,
            )?;
            shared_evidence.insert(evidence_sha256, (evidence_bytes, reference.clone()));
            reference
        };
        modules.push(ModuleFiles {
            module_interface,
            owner: write_ref(
                output_root,
                directory.join("owner.json"),
                &encode_json(&owner, inventory, RECORD_LIMIT, "owner encoding", false)?,
                RECORD_LIMIT,
                inventory,
            )?,
            products: write_ref(
                output_root,
                directory.join("products.cbor"),
                &record.products,
                inventory.limits().max_module_bytes,
                inventory,
            )?,
            interface: write_ref(
                output_root,
                directory.join("skinny.hi"),
                &record.interface,
                RECORD_LIMIT,
                inventory,
            )?,
            packages: write_ref(
                output_root,
                directory.join("packages.cbor"),
                &record.package_imports,
                RECORD_LIMIT,
                inventory,
            )?,
            evidence,
            certification: write_ref(
                output_root,
                directory.join("home-certification.cbor"),
                &record.original_certification,
                RECORD_LIMIT,
                inventory,
            )?,
        });
    }
    let execution_graphs = prepared
        .graphs
        .iter()
        .map(|(digest, graph)| {
            write_ref(
                output_root,
                PathBuf::from(format!("execution-{}.cbor", super::hex(digest))),
                graph.bytes(),
                crate::execution_source::GRAPH_BYTES_LIMIT,
                inventory,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let catalog = Catalog {
        schema: 4,
        source_selection: source_selection.clone(),
        producer_identity: authority.producer_identity,
        consumed_worker_identity: authority.consumed_worker_identity,
        modules,
        execution_graphs,
    };
    let bytes = encode_json(
        &catalog,
        inventory,
        inventory.limits().max_bytes,
        "catalog encoding",
        true,
    )?;
    write_ref(
        output_root,
        PathBuf::from("catalog.json"),
        &bytes,
        inventory.limits().max_bytes,
        inventory,
    )?;
    Ok(())
}
