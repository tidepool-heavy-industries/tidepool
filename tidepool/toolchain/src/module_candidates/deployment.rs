//! Explicit deployment inputs to the existing module-candidate owner.
//! Original products retain their producing roots and version recipe. Current
//! source selection and interface admission still belong to the worker.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{absolute, sha, version_hash, Record, CANDIDATE_LIMIT, RECORD_LIMIT};
use crate::toolchain::CompilerDeploymentAuthority;

const CATALOG_LIMIT: usize = 1 << 20;
const TOTAL_LIMIT: usize = 128 << 20;

#[derive(Debug, thiserror::Error)]
pub enum ModulePackageError {
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
    #[error("module package or source root moved from its producing path")]
    RootMoved,
    #[error("module package requires final immutable Nix store source and product roots")]
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
        Error::DigestMismatch(path)
        | Error::CertifiedOwnersDigestMismatch(path)
        | Error::InvalidCapturedPayload(path)
        | Error::InvalidModuleCertificate(path) => ModulePackageError::ArtifactChanged(path),
        Error::Unreadable { path, error } => io(&path, error),
        _ => ModulePackageError::Format("canonical module interface"),
    }
}

fn io(path: &Path, source: std::io::Error) -> ModulePackageError {
    ModulePackageError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Clone, Copy)]
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

fn require_immutable_roots(
    policy: RootPolicy,
    source: &Path,
    output: &Path,
) -> Result<(), ModulePackageError> {
    #[cfg(test)]
    if matches!(policy, RootPolicy::Fixture) {
        return Ok(());
    }
    let _ = policy;
    if immutable_store_path(source) && immutable_store_path(output) {
        Ok(())
    } else {
        Err(ModulePackageError::MutableRoot)
    }
}

pub(crate) fn prepare_build_roots(source: &Path, output: &Path) -> Result<(), ModulePackageError> {
    require_immutable_roots(RootPolicy::NixStore, source, output)?;
    if absolute(source).as_deref() != Some(source) {
        return Err(ModulePackageError::RootMoved);
    }
    reject_source_aliases(source)?;
    fs::create_dir_all(output).map_err(|e| io(output, e))?;
    if absolute(output).as_deref() != Some(output) {
        return Err(ModulePackageError::RootMoved);
    }
    if output.join("catalog.json").exists() {
        return Err(ModulePackageError::Format("catalog already exists"));
    }
    Ok(())
}

fn reject_source_aliases(root: &Path) -> Result<(), ModulePackageError> {
    let mut directories = vec![root.to_owned()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory).map_err(|e| io(&directory, e))? {
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

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, ModulePackageError> {
    let length = fs::metadata(path).map_err(|e| io(path, e))?.len();
    if length > limit as u64 {
        return Err(ModulePackageError::Bounds);
    }
    let bytes = fs::read(path).map_err(|e| io(path, e))?;
    if bytes.len() > limit {
        return Err(ModulePackageError::Bounds);
    }
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
    output_root: PathBuf,
    source_root: PathBuf,
    source_files: Vec<(PathBuf, String)>,
    producer_identity: [u8; 32],
    consumed_worker_identity: [u8; 32],
    modules: Vec<ModuleFiles>,
    #[serde(default)]
    execution_graphs: Vec<FileRef>,
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
    catalog_identity: String,
    source_identity: String,
    records: Vec<Record>,
}

impl DeploymentModulePackage {
    pub fn source_root(&self) -> &Path {
        &self.catalog.source_root
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
        let bytes = read(path, CATALOG_LIMIT)?;
        let catalog: Catalog = serde_json::from_slice(&bytes)
            .map_err(|_| ModulePackageError::Format("catalog JSON"))?;
        if catalog.schema != 3 {
            return Err(ModulePackageError::Format("catalog schema"));
        }
        require_immutable_roots(policy, &catalog.source_root, &catalog.output_root)?;
        if catalog.producer_identity != authority.producer_identity
            || catalog.consumed_worker_identity != authority.consumed_worker_identity
            || authority.schema != 1
            || authority.producer_identity == [0; 32]
            || authority.consumed_worker_identity == [0; 32]
        {
            return Err(ModulePackageError::CompilerMismatch);
        }
        if !path.is_absolute()
            || !catalog.output_root.is_absolute()
            || !catalog.source_root.is_absolute()
            || absolute(path).as_ref() != Some(&catalog.output_root.join("catalog.json"))
            || absolute(&catalog.output_root).as_ref() != Some(&catalog.output_root)
            || absolute(&catalog.source_root).as_ref() != Some(&catalog.source_root)
        {
            return Err(ModulePackageError::RootMoved);
        }
        if catalog.modules.is_empty() || catalog.modules.len() > CANDIDATE_LIMIT {
            return Err(ModulePackageError::Bounds);
        }
        reject_source_aliases(&catalog.source_root)?;
        let source_files = crate::cache::source_root_manifest(&catalog.source_root)
            .map_err(|e| io(&e.path, e.source))?;
        if source_files != catalog.source_files {
            return Err(ModulePackageError::SourceChanged);
        }
        let source_identity = sha(&serde_json::to_vec(&source_files)
            .map_err(|_| ModulePackageError::Format("source manifest"))?);
        let mut package = Self {
            catalog,
            catalog_identity: sha(&bytes),
            source_identity,
            records: Vec::new(),
        };
        // Detect malformed or changed configured artifacts even when their
        // current source selection will prevent candidate acceptance.
        package.records = package.read_records(package.producer_identity())?;
        Ok(package)
    }

    fn read_ref(
        &self,
        reference: &FileRef,
        remaining: &mut usize,
    ) -> Result<Vec<u8>, ModulePackageError> {
        if reference.path.as_os_str().is_empty()
            || reference
                .path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(ModulePackageError::Format("artifact relative path"));
        }
        let path = self.catalog.output_root.join(&reference.path);
        if absolute(&path).as_ref() != Some(&path) {
            return Err(ModulePackageError::RootMoved);
        }
        let bytes = read(&path, RECORD_LIMIT.min(*remaining))?;
        *remaining = remaining
            .checked_sub(bytes.len())
            .ok_or(ModulePackageError::Bounds)?;
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
        Ok(self.records.clone())
    }

    pub(super) fn into_candidates(
        self,
        producer: &[u8],
    ) -> Result<Vec<(Record, super::CandidateOrigin)>, ModulePackageError> {
        if producer != self.catalog.producer_identity {
            return Err(ModulePackageError::CompilerMismatch);
        }
        Ok(self
            .records
            .into_iter()
            .zip(self.catalog.modules)
            .map(|(record, files)| {
                (
                    record,
                    super::CandidateOrigin::Deployment {
                        interface: self.catalog.output_root.join(files.interface.path),
                        packages: self.catalog.output_root.join(files.packages.path),
                    },
                )
            })
            .collect())
    }

    fn read_records(&self, producer: &[u8]) -> Result<Vec<Record>, ModulePackageError> {
        if producer != self.catalog.producer_identity {
            return Err(ModulePackageError::CompilerMismatch);
        }
        let mut remaining = TOTAL_LIMIT;
        let mut records = Vec::new();
        let mut owners = BTreeSet::new();
        let mut graphs = std::collections::BTreeMap::new();
        if self.catalog.execution_graphs.len() > CANDIDATE_LIMIT {
            return Err(ModulePackageError::Bounds);
        }
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        for reference in &self.catalog.execution_graphs {
            let bytes = self.read_ref(reference, &mut remaining)?;
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
            let owner: Owner =
                serde_json::from_slice(&self.read_ref(&files.owner, &mut remaining)?)
                    .map_err(|_| ModulePackageError::Format("owner JSON"))?;
            if !owners.insert((owner.unit.clone(), owner.module.clone())) {
                return Err(ModulePackageError::Format("duplicate module owner"));
            }
            let evidence: super::shared_evidence::SharedEvidence =
                serde_json::from_slice(&self.read_ref(&files.evidence, &mut remaining)?)
                    .map_err(|_| ModulePackageError::Format("dependency evidence JSON"))?;
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
                    products: self.read_ref(&files.products, &mut remaining)?,
                    interface: self.read_ref(&files.interface, &mut remaining)?,
                    package_imports: self.read_ref(&files.packages, &mut remaining)?,
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
                    original_certification: self.read_ref(&files.certification, &mut remaining)?,
                    module_interface: Some(files.module_interface.clone()),
                    execution_source_sha256: owner.execution_source_sha256,
                },
            };
            if record.original_owner.owner() != super::computed_owner(&record) {
                return Err(ModulePackageError::Format("original full owner"));
            }
            let reference = &files.module_interface;
            let mut captured_bytes = 0usize;
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
                let path = self.catalog.output_root.join(relative);
                if absolute(&path).as_ref() != Some(&path) {
                    return Err(ModulePackageError::RootMoved);
                }
                let length = fs::metadata(&path).map_err(|error| io(&path, error))?.len();
                captured_bytes = captured_bytes
                    .checked_add(usize::try_from(length).map_err(|_| ModulePackageError::Bounds)?)
                    .ok_or(ModulePackageError::Bounds)?;
            }
            remaining = remaining
                .checked_sub(captured_bytes)
                .ok_or(ModulePackageError::Bounds)?;
            let canonical = crate::recovery_artifacts::recover_module_interface(
                &self.catalog.output_root,
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
            crate::certified_products::validate_canonical_native_bytes(
                &record.original_owner.owner(),
                &record.original_certification,
                &record.interface,
                &record.package_imports,
                Some(
                    super::parse_sha(&record.source_sha256)
                        .ok_or(ModulePackageError::Format("canonical source digest"))?,
                ),
                &canonical,
            )
            .map_err(|_| ModulePackageError::Format("canonical native binding"))?;
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
            if owner.module_version != version_hash(&record)
                || record.include != [self.catalog.source_root.clone()]
                || !record.source.starts_with(&self.catalog.source_root)
                || absolute(&record.source).as_ref() != Some(&record.source)
                || !record.evidence.valid(&record.target_source)
                || !record.evidence.selection_complete
            {
                return Err(ModulePackageError::OpenCohort);
            }
            let requirements = crate::prepared_artifact::production_requirements()
                .map_err(|_| ModulePackageError::Format("host requirements"))?;
            let parsed = tidepool_repr::execution_schema::parse_module_products(
                &record.products,
                &requirements,
                super::product_decode_limits(),
            )
            .map_err(|_| ModulePackageError::Format("original module products"))?;
            if parsed.len() != 1
                || parsed[0].unit != record.unit
                || parsed[0].module != record.module
                || parsed[0].interface != record.interface
                || record.interface.is_empty()
                || sha(&read(&record.source, RECORD_LIMIT)?) != record.source_sha256
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
            crate::recovery_artifacts::validate_package_imports(
                &record.package_imports,
                &record.unit,
                &record.module,
                &iface_sha,
                &self.catalog.output_root,
            )
            .map_err(|_| ModulePackageError::Format("package imports"))?;
            records.push(record);
        }
        for record in &records {
            require_complete_cohort(&records, &record.evidence)?;
        }
        validate_closed(&records, &self.catalog.source_root)?;
        Ok(records)
    }
}

fn validate_closed(records: &[Record], source_root: &Path) -> Result<(), ModulePackageError> {
    let owners: std::collections::BTreeMap<_, _> = records
        .iter()
        .map(|r| ((r.unit.as_str(), r.module.as_str()), r.source.as_path()))
        .collect();
    for record in records {
        if record
            .evidence
            .sources
            .iter()
            .any(|s| s.path != Path::new("@generated-source") && !s.path.starts_with(source_root))
        {
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
                !p.starts_with(source_root)
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

fn require_complete_cohort(
    records: &[Record],
    evidence: &crate::cache::DependencyEvidence,
) -> Result<(), ModulePackageError> {
    let owners: BTreeSet<_> = records
        .iter()
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
    use super::super::tests::{package_bundle, product_bytes};
    use super::*;
    use crate::cache::{DependencyEvidence, ModuleEvidence, ProductAvailability, SourceEvidence};

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
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("sources");
            let output = root.path().join("products");
            fs::create_dir(&source).unwrap();
            for name in names {
                fs::write(
                    source.join(format!("{name}.hs")),
                    format!("module {name} where\nvalue = 7\n"),
                )
                .unwrap();
            }
            let source = absolute(&source).unwrap();
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
                    let path = source.join(format!("{name}.hs"));
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
                        source: source.join(format!("{name}.hs")),
                        imports: vec![],
                        product: ProductAvailability::Ready,
                    })
                    .collect(),
                packages: vec![],
                resolutions: vec![],
            };
            let bytes = combine_rows(
                names
                    .iter()
                    .map(|name| product_bytes("u", name, name.as_bytes())),
            );
            let packages = combine_rows(
                names
                    .iter()
                    .map(|name| package_bundle("u", name, name.as_bytes())),
            );
            let parsed =
                crate::certified_products::ParsedModuleProducts::decode(&bytes, &packages).unwrap();
            let include = [source.clone()];
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
                            &std::collections::BTreeMap::new(),
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
                    retained_sources: &std::collections::BTreeMap::new(), packages: &std::collections::BTreeMap::new(),
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
                &source,
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
    fn independent_files_preserve_original_owner_across_current_include_changes() {
        let fixture = Fixture::new();
        let package = fixture.load().unwrap();
        assert_eq!(package.source_root(), fixture.source);
        let records = package.records(&[3; 32]).unwrap();
        let original = version_hash(&records[0]);
        let scratch = tempfile::tempdir().unwrap();
        let current = [scratch.path().to_path_buf(), fixture.source.clone()];
        let selected = super::super::select_records(
            &[3; 32],
            &current,
            scratch.path(),
            package.into_candidates(&[3; 32]).unwrap(),
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
        assert_eq!(catalog.schema, 3);
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
        assert_eq!(catalog.schema, 3);
        let core = catalog.modules[0].module_interface.core.as_ref().unwrap();
        let core_path = fixture.output.join(&core.path);
        let original = fs::read(&core_path).unwrap();
        fs::write(&core_path, b"changed core").unwrap();
        assert!(
            matches!(fixture.load(), Err(ModulePackageError::ArtifactChanged(path)) if path == core_path)
        );
        fs::write(&core_path, original).unwrap();
        for schema in [1, 2] {
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
    fn package_relocation_refuses_without_rewriting_original_paths() {
        let fixture = Fixture::new();
        let other = fixture._root.path().join("relocated");
        fs::create_dir(&other).unwrap();
        fs::copy(
            fixture.output.join("catalog.json"),
            other.join("catalog.json"),
        )
        .unwrap();
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
        let source = fixture.source.join("Library.hs");
        let original = fs::read(&source).unwrap();
        fs::write(fixture.source.join("Added.hs"), "module Added where\n").unwrap();
        assert!(matches!(
            fixture.load(),
            Err(ModulePackageError::SourceChanged)
        ));
        fs::remove_file(fixture.source.join("Added.hs")).unwrap();
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
        symlink(fixture.source.join("Library.hs"), &alias).unwrap();
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
                selected: Some(other.source.join("Missing.hs")),
            });
        assert!(matches!(
            validate_closed(&records, &other.source),
            Err(ModulePackageError::OpenCohort)
        ));
        records[0].evidence.make_mut().modules[0].imports[0].module = "Library".into();
        records[0].evidence.make_mut().modules[0].imports[0].selected =
            Some(other.source.join("WrongSource.hs"));
        assert!(matches!(
            validate_closed(&records, &other.source),
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
            source: fixture.source.join("Tidepool/Prelude.hs"),
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

fn write_ref(root: &Path, relative: PathBuf, bytes: &[u8]) -> Result<FileRef, ModulePackageError> {
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
    source_root: &Path,
    authority: &crate::toolchain::AdmittedCompilerDeployment,
    prepared: &super::PreparedPublication<'_>,
) -> Result<(), ModulePackageError> {
    export_under(
        output_root,
        source_root,
        authority,
        prepared,
        RootPolicy::NixStore,
    )
}

fn export_under(
    output_root: &Path,
    source_root: &Path,
    authority: &crate::toolchain::AdmittedCompilerDeployment,
    prepared: &super::PreparedPublication<'_>,
    policy: RootPolicy,
) -> Result<(), ModulePackageError> {
    let producer = prepared.endpoint_identity;
    let include = prepared.include;
    let evidence = prepared.evidence;
    require_immutable_roots(policy, source_root, output_root)?;
    if producer != authority.producer_identity {
        return Err(ModulePackageError::CompilerMismatch);
    }
    let source_root = absolute(source_root).ok_or(ModulePackageError::RootMoved)?;
    if include != [source_root.clone()] {
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
    require_complete_cohort(&records, evidence)?;
    if records.is_empty() || records.len() > CANDIDATE_LIMIT {
        return Err(ModulePackageError::Bounds);
    }
    validate_closed(&records, &source_root)?;
    let mut modules = Vec::new();
    let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
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
        let module_interface = crate::recovery_artifacts::materialize_module_interface(
            output_root,
            canonical,
            &mut validation,
            crate::recovery_artifacts::MaterializationMode::Durable,
        )
        .map_err(|_| ModulePackageError::Format("canonical module materialization"))?;
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
        modules.push(ModuleFiles {
            module_interface,
            owner: write_ref(
                output_root,
                directory.join("owner.json"),
                &serde_json::to_vec(&owner)
                    .map_err(|_| ModulePackageError::Format("owner encoding"))?,
            )?,
            products: write_ref(
                output_root,
                directory.join("products.cbor"),
                &record.products,
            )?,
            interface: write_ref(output_root, directory.join("skinny.hi"), &record.interface)?,
            packages: write_ref(
                output_root,
                directory.join("packages.cbor"),
                &record.package_imports,
            )?,
            evidence: write_ref(
                output_root,
                directory.join("dependencies.json"),
                &serde_json::to_vec(&record.evidence)
                    .map_err(|_| ModulePackageError::Format("evidence encoding"))?,
            )?,
            certification: write_ref(
                output_root,
                directory.join("home-certification.cbor"),
                &record.original_certification,
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
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let catalog = Catalog {
        schema: 3,
        output_root: output_root.to_owned(),
        source_root: source_root.clone(),
        source_files: crate::cache::source_root_manifest(&source_root)
            .map_err(|e| io(&e.path, e.source))?,
        producer_identity: authority.producer_identity,
        consumed_worker_identity: authority.consumed_worker_identity,
        modules,
        execution_graphs,
    };
    let bytes = serde_json::to_vec_pretty(&catalog)
        .map_err(|_| ModulePackageError::Format("catalog encoding"))?;
    if bytes.len() > CATALOG_LIMIT {
        return Err(ModulePackageError::Bounds);
    }
    write_ref(output_root, PathBuf::from("catalog.json"), &bytes)?;
    Ok(())
}
