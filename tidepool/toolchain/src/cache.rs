//! Compiler source identity and validated dependency evidence.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::digest::frame;

/// Source snapshot identity, independent from compiler dependency selection.
#[derive(Clone, Debug)]
pub struct SourceRootManifest {
    files: Vec<(PathBuf, blake3::Hash)>,
}

impl SourceRootManifest {
    /// Reuse complete file evidence produced while capturing a source root.
    /// Header files may accompany the capture but do not contribute to the
    /// established Haskell source identity.
    pub fn from_file_digests(
        files: impl IntoIterator<Item = (PathBuf, String)>,
    ) -> Result<Self, SourceManifestError> {
        let mut sources = std::collections::BTreeMap::new();
        for (path, digest) in files {
            if !is_haskell_dependency_source(&path) {
                continue;
            }
            let digest = blake3::Hash::from_hex(&digest).map_err(|error| {
                source_error(
                    &path,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error),
                )
            })?;
            sources.insert(path, digest);
        }
        Ok(Self {
            files: sources.into_iter().collect(),
        })
    }

    pub fn files(&self) -> impl Iterator<Item = (&Path, &blake3::Hash)> {
        self.files
            .iter()
            .map(|(path, digest)| (path.as_path(), digest))
    }

    fn fingerprint(&self, prefix: &Path, hasher: &mut blake3::Hasher) {
        frame(hasher, &(self.files.len() as u64).to_le_bytes());
        for (rel, digest) in &self.files {
            frame(hasher, prefix.join(rel).as_os_str().as_encoded_bytes());
            // Preserve the source-identity framing for complete trees.
            let mut tagged = [1_u8; 33];
            tagged[1..].copy_from_slice(digest.as_bytes());
            frame(hasher, &tagged);
        }
    }
}

/// A source tree could not be completely inspected. No identity is published
/// from partial evidence; the failing path remains available to callers.
#[derive(Debug, thiserror::Error)]
#[error("cannot inspect source path {path}: {source}")]
pub struct SourceManifestError {
    pub path: PathBuf,
    #[source]
    pub source: std::io::Error,
}

fn source_error(path: &Path, source: std::io::Error) -> SourceManifestError {
    SourceManifestError {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Clone, Copy)]
enum SourceSelection {
    Dependencies,
    ShippedHaskell,
}

impl SourceSelection {
    fn includes_directory(self, path: &Path) -> bool {
        match self {
            Self::Dependencies => true,
            Self::ShippedHaskell => path.file_name().is_none_or(|name| name != "Prelude_cbor"),
        }
    }

    fn includes_file(self, path: &Path) -> bool {
        match self {
            Self::Dependencies => is_haskell_dependency_source(path),
            Self::ShippedHaskell => path.extension().is_some_and(|extension| extension == "hs"),
        }
    }
}

/// Complete evidence for the Haskell sources shipped by release bundles.
/// Every namespace is included; generated Prelude_cbor trees are excluded.
pub fn shipped_haskell_source_manifest(
    root: &Path,
) -> Result<SourceRootManifest, SourceManifestError> {
    selected_source_manifest(root, SourceSelection::ShippedHaskell)
}

/// Enumerate Haskell home-module sources by their visible relative paths.
fn dependency_source_manifest(root: &Path) -> Result<SourceRootManifest, SourceManifestError> {
    selected_source_manifest(root, SourceSelection::Dependencies)
}

// The ancestor set breaks cycles while retaining distinct directory aliases.
fn selected_source_manifest(
    root: &Path,
    selection: SourceSelection,
) -> Result<SourceRootManifest, SourceManifestError> {
    Ok(SourceRootManifest {
        files: selected_source_files(root, selection, blake3::hash)?,
    })
}

fn selected_source_files<D>(
    root: &Path,
    selection: SourceSelection,
    digest: impl Fn(&[u8]) -> D,
) -> Result<Vec<(PathBuf, D)>, SourceManifestError> {
    let mut files = Vec::new();
    let mut ancestors = std::collections::HashSet::new();
    collect_dependency_sources(root, root, &mut files, &mut ancestors, selection, &digest)?;
    files.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(files)
}

/// Complete content manifest for source revision identities. Every Haskell
/// source is paired with its relative path and content digest. Inspection
/// failures are errors, never omissions or empty digests.
pub fn source_root_manifest(root: &Path) -> Result<Vec<(PathBuf, String)>, SourceManifestError> {
    Ok(dependency_source_manifest(root)?
        .files
        .into_iter()
        .map(|(rel, digest)| (rel, digest.to_hex().to_string()))
        .collect())
}

/// Catalog source witnesses use SHA-256 while sharing dependency traversal.
pub(crate) fn catalog_source_sha256_manifest(
    root: &Path,
) -> Result<Vec<(PathBuf, String)>, SourceManifestError> {
    selected_source_files(root, SourceSelection::Dependencies, |bytes| {
        hex_digest(&Sha256::digest(bytes))
    })
}

/// Content identity for ordered source roots. Root order determines module
/// shadowing; relative paths and bytes determine each root's identity.
/// Any incomplete source inspection refuses the identity.
pub fn source_roots_identity(
    domain: &[u8],
    roots: &[PathBuf],
) -> Result<String, SourceManifestError> {
    let mut hasher = source_identity_hasher(domain, roots.len());
    for root in roots {
        dependency_source_manifest(root)?.fingerprint(Path::new(""), &mut hasher);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Apply the same ordered-root framing to already inspected source evidence.
pub fn source_manifests_identity(domain: &[u8], roots: &[SourceRootManifest]) -> String {
    let mut hasher = source_identity_hasher(domain, roots.len());
    for root in roots {
        root.fingerprint(Path::new(""), &mut hasher);
    }
    hasher.finalize().to_hex().to_string()
}

fn source_identity_hasher(domain: &[u8], roots: usize) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    frame(&mut hasher, domain);
    frame(&mut hasher, &(roots as u64).to_le_bytes());
    hasher
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        // Writing to a String is infallible.
        write!(out, "{byte:02x}").ok();
        out
    })
}

fn collect_dependency_sources<D>(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(PathBuf, D)>,
    ancestors: &mut std::collections::HashSet<PathBuf>,
    selection: SourceSelection,
    digest: &impl Fn(&[u8]) -> D,
) -> Result<(), SourceManifestError> {
    crate::host_work::checkpoint().map_err(|error| source_error(dir, error))?;
    let canonical = fs::canonicalize(dir).map_err(|error| source_error(dir, error))?;
    if !ancestors.insert(canonical.clone()) {
        return Err(source_error(
            dir,
            std::io::Error::new(std::io::ErrorKind::InvalidData, "source directory cycle"),
        ));
    }
    let mut entries = fs::read_dir(dir)
        .map_err(|error| source_error(dir, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| source_error(dir, error))?;
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        crate::host_work::checkpoint().map_err(|error| source_error(dir, error))?;
        let path = entry.path();
        if !selection.includes_directory(&path) {
            continue;
        }
        let metadata = fs::metadata(&path).map_err(|error| source_error(&path, error))?;
        if metadata.is_dir() {
            collect_dependency_sources(root, &path, out, ancestors, selection, digest)?;
        } else if selection.includes_file(&path) {
            let bytes =
                crate::host_work::read(&path).map_err(|error| source_error(&path, error))?;
            let rel = path.strip_prefix(root).map_err(|error| {
                source_error(
                    &path,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error),
                )
            })?;
            out.push((rel.to_path_buf(), digest(&bytes)));
        }
    }
    ancestors.remove(&canonical);
    Ok(())
}

fn is_haskell_dependency_source(path: &Path) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    [b".hs".as_slice(), b".hs-boot", b".lhs", b".lhs-boot"]
        .iter()
        .any(|suffix| name.as_encoded_bytes().ends_with(suffix))
}

/// The worker's consumed source and import-resolution evidence. Completeness
/// for cache reuse is independent from completeness for test selection.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyEvidence {
    pub version: u32,
    pub cache_safe: bool,
    pub selection_complete: bool,
    pub sources: Vec<SourceEvidence>,
    pub resolutions: Vec<ResolutionEvidence>,
    pub packages: Vec<String>,
    pub modules: Vec<ModuleEvidence>,
}

/// Validated observations from one completed compilation. This proves the
/// consumed source bytes and import witnesses, not that executing source again
/// would reproduce its result. Only the owning validator constructs this proof.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct CompletedSourceEvidence(DependencyEvidence);

impl CompletedSourceEvidence {
    pub(crate) fn from_worker(
        bytes: &[u8],
        input: &Path,
        source: &str,
    ) -> Result<Self, DependencyEvidenceFailure> {
        let evidence = serde_json::from_slice(bytes).map_err(|error| {
            DependencyEvidenceFailure::WorkerDecode {
                message: error.to_string(),
            }
        })?;
        Self::from_worker_evidence(evidence, input, source)
    }

    pub(crate) fn from_worker_evidence(
        evidence: DependencyEvidence,
        input: &Path,
        source: &str,
    ) -> Result<Self, DependencyEvidenceFailure> {
        Self::from_normalized(evidence.normalize_worker_paths(input)?, source)
    }

    pub(crate) fn from_normalized(
        evidence: DependencyEvidence,
        source: &str,
    ) -> Result<Self, DependencyEvidenceFailure> {
        evidence.validate_consumed_sources(source)?;
        Ok(Self(evidence))
    }

    pub(crate) fn revalidate(&self, source: &str) -> Result<(), DependencyEvidenceFailure> {
        self.0.validate_consumed_sources(source)
    }

    pub(crate) fn into_evidence(self) -> DependencyEvidence {
        self.0
    }
}

impl std::ops::Deref for CompletedSourceEvidence {
    type Target = DependencyEvidence;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleEvidence {
    pub unit: String,
    pub module: String,
    pub boot: bool,
    pub source: PathBuf,
    pub imports: Vec<ModuleImportEvidence>,
    pub product: ProductAvailability,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductAvailability {
    Ready,
    Boot,
    InterfaceOnly,
    MissingInterface,
    ProjectionRejected,
}

impl ProductAvailability {
    /// The compiler retained an ordinary source owner and an exact interface.
    /// Native group projection may still have been refused independently.
    pub fn has_canonical_source_interface(self) -> bool {
        match self {
            Self::Ready | Self::InterfaceOnly | Self::ProjectionRejected => true,
            Self::Boot | Self::MissingInterface => false,
        }
    }

    /// Only a complete native product can enter execution-product admission.
    pub fn has_native_product(self) -> bool {
        match self {
            Self::Ready => true,
            Self::Boot
            | Self::InterfaceOnly
            | Self::MissingInterface
            | Self::ProjectionRejected => false,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleImportEvidence {
    pub qualifier: ImportQualifier,
    pub module: String,
    pub boot: bool,
    pub selected: Option<PathBuf>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEvidence {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionEvidence {
    pub qualifier: ImportQualifier,
    pub module: String,
    pub boot: bool,
    pub selected: Option<PathBuf>,
    pub candidates: Vec<PathBuf>,
}

#[derive(Debug, Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum ImportQualifier {
    Unqualified,
    ThisUnit(String),
    OtherUnit(String),
}

impl TryFrom<String> for ImportQualifier {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == "none" {
            return Ok(Self::Unqualified);
        }
        if let Some(unit) = value.strip_prefix("this:") {
            if !unit.is_empty() {
                return Ok(Self::ThisUnit(unit.to_owned()));
            }
        }
        if let Some(unit) = value.strip_prefix("other:") {
            if !unit.is_empty() {
                return Ok(Self::OtherUnit(unit.to_owned()));
            }
        }
        Err("invalid import qualifier")
    }
}

impl From<ImportQualifier> for String {
    fn from(value: ImportQualifier) -> Self {
        match value {
            ImportQualifier::Unqualified => "none".into(),
            ImportQualifier::ThisUnit(unit) => format!("this:{unit}"),
            ImportQualifier::OtherUnit(unit) => format!("other:{unit}"),
        }
    }
}

impl ImportQualifier {
    fn valid(&self) -> bool {
        match self {
            Self::Unqualified => true,
            Self::ThisUnit(unit) | Self::OtherUnit(unit) => !unit.is_empty(),
        }
    }

    fn is_external_package(&self) -> bool {
        matches!(self, Self::OtherUnit(_))
    }
}

pub(crate) const GENERATED_SOURCE: &str = "@generated-source";

impl ModuleEvidence {
    /// In evidence issued by `from_worker`, this marks the exact request
    /// source after consumed-byte and final-graph validation.
    pub(crate) fn is_generated_source(&self) -> bool {
        self.source == Path::new(GENERATED_SOURCE)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DependencyEvidenceFailure {
    Interrupted,
    WorkerDecode {
        message: String,
    },
    WorkerInput {
        path: PathBuf,
        message: String,
    },
    Header,
    Source {
        index: usize,
        reason: SourceWitnessFailure,
    },
    GeneratedSourceMissing,
    Module {
        index: usize,
    },
    Import {
        module: usize,
        index: usize,
    },
    Resolution {
        index: usize,
        reason: ResolutionWitnessFailure,
    },
    ImportResolution {
        module: usize,
        index: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceWitnessFailure {
    Malformed,
    Unavailable,
    Changed { expected: String, actual: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolutionWitnessFailure {
    Malformed,
    Duplicate,
    SelectedSource,
    NonAbsoluteCandidate { index: usize },
    NegativeCandidateUnavailable { index: usize },
}

/// Physical source observations made by a fresh dependency validation stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DependencyEvidenceWork {
    pub source_read_attempts: u64,
    pub source_read_bytes: u64,
    pub negative_metadata_calls: u64,
}

impl DependencyEvidence {
    /// Admit replay-eligible worker evidence after validating consumed bytes.
    /// Authored dependencies retain their path identity.
    pub(crate) fn from_worker(bytes: &[u8], input: &Path, source: &str) -> Option<Self> {
        let completed = CompletedSourceEvidence::from_worker(bytes, input, source).ok()?;
        completed.cache_safe.then(|| completed.into_evidence())
    }

    fn normalize_worker_paths(mut self, input: &Path) -> Result<Self, DependencyEvidenceFailure> {
        let evidence = &mut self;
        let input =
            fs::canonicalize(input).map_err(|error| DependencyEvidenceFailure::WorkerInput {
                path: input.to_path_buf(),
                message: error.to_string(),
            })?;
        for item in &mut evidence.sources {
            if fs::canonicalize(&item.path).ok().as_ref() == Some(&input) {
                item.path = GENERATED_SOURCE.into();
            }
        }
        for module in &mut evidence.modules {
            if fs::canonicalize(&module.source).ok().as_ref() == Some(&input) {
                module.source = GENERATED_SOURCE.into();
            }
            for imported in &mut module.imports {
                if imported
                    .selected
                    .as_ref()
                    .and_then(|path| fs::canonicalize(path).ok())
                    .as_ref()
                    == Some(&input)
                {
                    imported.selected = Some(GENERATED_SOURCE.into());
                }
            }
        }
        Ok(self)
    }

    /// Validate replay eligibility, consumed bytes and negative witnesses. IO
    /// errors are misses: candidate absence must be known, not guessed.
    pub fn valid(&self, source: &str) -> bool {
        self.validate(source).is_ok()
    }

    pub(crate) fn validate(&self, source: &str) -> Result<(), DependencyEvidenceFailure> {
        self.validate_with_work(source, &mut DependencyEvidenceWork::default())
    }

    pub(crate) fn validate_with_work(
        &self,
        source: &str,
        work: &mut DependencyEvidenceWork,
    ) -> Result<(), DependencyEvidenceFailure> {
        if !self.cache_safe {
            return Err(DependencyEvidenceFailure::Header);
        }
        self.validate_consumed_sources_with_work(source, work)
    }

    fn validate_consumed_sources(&self, source: &str) -> Result<(), DependencyEvidenceFailure> {
        self.validate_consumed_sources_with_work(source, &mut DependencyEvidenceWork::default())
    }

    fn validate_consumed_sources_with_work(
        &self,
        source: &str,
        work: &mut DependencyEvidenceWork,
    ) -> Result<(), DependencyEvidenceFailure> {
        if self.version != 4 || self.sources.is_empty() || self.modules.is_empty() {
            return Err(DependencyEvidenceFailure::Header);
        }
        let mut paths = std::collections::HashSet::new();
        let mut target = false;
        for (index, item) in self.sources.iter().enumerate() {
            crate::host_work::checkpoint().map_err(|_| DependencyEvidenceFailure::Interrupted)?;
            if !paths.insert(&item.path) || item.sha256.len() != 64 {
                return Err(DependencyEvidenceFailure::Source {
                    index,
                    reason: SourceWitnessFailure::Malformed,
                });
            }
            let digest = if item.path == Path::new(GENERATED_SOURCE) {
                target = true;
                hex_digest(&Sha256::digest(source.as_bytes()))
            } else {
                if !item.path.is_absolute() {
                    return Err(DependencyEvidenceFailure::Source {
                        index,
                        reason: SourceWitnessFailure::Malformed,
                    });
                }
                work.source_read_attempts += 1;
                let bytes = match crate::host_work::read(&item.path) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        return Err(DependencyEvidenceFailure::Interrupted);
                    }
                    Err(_) => {
                        return Err(DependencyEvidenceFailure::Source {
                            index,
                            reason: SourceWitnessFailure::Unavailable,
                        })
                    }
                };
                work.source_read_bytes += bytes.len() as u64;
                hex_digest(&Sha256::digest(bytes))
            };
            if digest != item.sha256 {
                return Err(DependencyEvidenceFailure::Source {
                    index,
                    reason: SourceWitnessFailure::Changed {
                        expected: item.sha256.clone(),
                        actual: digest,
                    },
                });
            }
        }
        if !target {
            return Err(DependencyEvidenceFailure::GeneratedSourceMissing);
        }
        let mut modules = std::collections::HashSet::new();
        for (module_index, module) in self.modules.iter().enumerate() {
            crate::host_work::checkpoint().map_err(|_| DependencyEvidenceFailure::Interrupted)?;
            if module.unit.is_empty()
                || module.module.is_empty()
                || !paths.contains(&module.source)
                || !modules.insert((&module.unit, &module.module, module.boot))
                || module.boot != (module.product == ProductAvailability::Boot)
            {
                return Err(DependencyEvidenceFailure::Module {
                    index: module_index,
                });
            }
            let mut imports = std::collections::HashSet::new();
            for (import_index, imported) in module.imports.iter().enumerate() {
                if imported.module.is_empty()
                    || !imported.qualifier.valid()
                    || (imported.qualifier.is_external_package() && imported.selected.is_some())
                    || !imports.insert((&imported.qualifier, &imported.module, imported.boot))
                    || imported
                        .selected
                        .as_ref()
                        .is_some_and(|path| !paths.contains(path))
                {
                    return Err(DependencyEvidenceFailure::Import {
                        module: module_index,
                        index: import_index,
                    });
                }
            }
        }
        let mut resolutions = std::collections::HashMap::new();
        for (index, resolution) in self.resolutions.iter().enumerate() {
            crate::host_work::checkpoint().map_err(|_| DependencyEvidenceFailure::Interrupted)?;
            if resolution.module.is_empty()
                || !resolution.qualifier.valid()
                || (resolution.qualifier.is_external_package() != resolution.candidates.is_empty())
                || (resolution.qualifier.is_external_package() && resolution.selected.is_some())
            {
                return Err(DependencyEvidenceFailure::Resolution {
                    index,
                    reason: ResolutionWitnessFailure::Malformed,
                });
            }
            if resolutions
                .insert(
                    (&resolution.qualifier, &resolution.module, resolution.boot),
                    &resolution.selected,
                )
                .is_some()
            {
                return Err(DependencyEvidenceFailure::Resolution {
                    index,
                    reason: ResolutionWitnessFailure::Duplicate,
                });
            }
            if let Some(selected) = &resolution.selected {
                if resolution.candidates.last() != Some(selected) || !paths.contains(selected) {
                    return Err(DependencyEvidenceFailure::Resolution {
                        index,
                        reason: ResolutionWitnessFailure::SelectedSource,
                    });
                }
            }
            for (candidate_index, candidate) in resolution.candidates.iter().enumerate() {
                crate::host_work::checkpoint()
                    .map_err(|_| DependencyEvidenceFailure::Interrupted)?;
                if !candidate.is_absolute() {
                    return Err(DependencyEvidenceFailure::Resolution {
                        index,
                        reason: ResolutionWitnessFailure::NonAbsoluteCandidate {
                            index: candidate_index,
                        },
                    });
                }
                if Some(candidate) == resolution.selected.as_ref() {
                    continue;
                }
                work.negative_metadata_calls += 1;
                match fs::metadata(candidate) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => {
                        return Err(DependencyEvidenceFailure::Resolution {
                            index,
                            reason: ResolutionWitnessFailure::NegativeCandidateUnavailable {
                                index: candidate_index,
                            },
                        })
                    }
                }
            }
        }
        for (module_index, module) in self.modules.iter().enumerate() {
            crate::host_work::checkpoint().map_err(|_| DependencyEvidenceFailure::Interrupted)?;
            for (import_index, imported) in module.imports.iter().enumerate() {
                if resolutions
                    .get(&(&imported.qualifier, &imported.module, imported.boot))
                    .copied()
                    != Some(&imported.selected)
                {
                    return Err(DependencyEvidenceFailure::ImportResolution {
                        module: module_index,
                        index: import_index,
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod source_manifest_properties;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_availability_separates_native_products_from_canonical_interfaces() {
        for availability in [
            ProductAvailability::Ready,
            ProductAvailability::InterfaceOnly,
            ProductAvailability::ProjectionRejected,
        ] {
            assert!(availability.has_canonical_source_interface());
        }
        for availability in [
            ProductAvailability::Boot,
            ProductAvailability::MissingInterface,
        ] {
            assert!(!availability.has_canonical_source_interface());
        }
        assert!(ProductAvailability::Ready.has_native_product());
        for availability in [
            ProductAvailability::Boot,
            ProductAvailability::InterfaceOnly,
            ProductAvailability::MissingInterface,
            ProductAvailability::ProjectionRejected,
        ] {
            assert!(!availability.has_native_product());
        }
    }

    fn digest(bytes: &[u8]) -> String {
        hex_digest(&Sha256::digest(bytes))
    }

    fn evidence(root: &Path) -> DependencyEvidence {
        let selected = root.join("later/Library.hs");
        fs::create_dir_all(selected.parent().unwrap()).unwrap();
        fs::create_dir_all(root.join("first")).unwrap();
        fs::write(&selected, "library = 1").unwrap();
        DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![
                SourceEvidence {
                    path: GENERATED_SOURCE.into(),
                    sha256: digest(b"target"),
                },
                SourceEvidence {
                    path: selected.clone(),
                    sha256: digest(b"library = 1"),
                },
            ],
            resolutions: vec![ResolutionEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "Library".into(),
                boot: false,
                selected: Some(selected.clone()),
                candidates: vec![root.join("first/Library.hs"), selected.clone()],
            }],
            packages: vec![],
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "Target".into(),
                boot: false,
                source: GENERATED_SOURCE.into(),
                imports: vec![ModuleImportEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: "Library".into(),
                    boot: false,
                    selected: Some(selected),
                }],
                product: ProductAvailability::Ready,
            }],
        }
    }

    #[test]
    fn source_manifest_missing_root_is_not_an_empty_tree() {
        let root = tempfile::tempdir().unwrap();
        let absent = root.path().join("absent");
        let error = source_root_manifest(&absent).unwrap_err();
        assert_eq!(error.path, absent);
        assert_eq!(error.source.kind(), std::io::ErrorKind::NotFound);
        assert!(source_roots_identity(b"test", &[absent]).is_err());
        assert!(source_root_manifest(root.path()).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn source_manifest_preserves_aliases_and_rejects_cycles() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let sources = root.path().join("A");
        fs::create_dir(&sources).unwrap();
        fs::write(sources.join("Module.hs"), "module A.Module where").unwrap();
        let before = source_roots_identity(b"test", &[root.path().to_path_buf()]).unwrap();
        symlink(&sources, root.path().join("B")).unwrap();
        let manifest = source_root_manifest(root.path()).unwrap();
        assert_eq!(
            manifest
                .iter()
                .map(|(path, _)| path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("A/Module.hs"), Path::new("B/Module.hs")]
        );
        assert_eq!(manifest[0].1, manifest[1].1);
        assert_ne!(
            before,
            source_roots_identity(b"test", &[root.path().to_path_buf()]).unwrap()
        );
        symlink(root.path(), sources.join("cycle")).unwrap();
        let error = source_root_manifest(root.path()).unwrap_err();
        assert_eq!(error.path, sources.join("cycle"));
        assert_eq!(error.source.kind(), std::io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn source_manifest_dangling_source_link_refuses_identity() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Missing.hs");
        std::os::unix::fs::symlink(root.path().join("absent"), &source).unwrap();
        assert_eq!(source_root_manifest(root.path()).unwrap_err().path, source);
        assert!(source_roots_identity(b"test", &[root.path().to_path_buf()]).is_err());
    }

    #[test]
    fn evidence_tracks_consumed_bytes_and_shadowing_not_unrelated_edits() {
        let root = tempfile::tempdir().unwrap();
        let evidence = evidence(root.path());
        assert!(evidence.valid("target"));
        fs::write(root.path().join("later/Unrelated.hs"), "anything").unwrap();
        assert!(evidence.valid("target"));
        let shadow = root.path().join("first/Library.hs");
        fs::write(&shadow, "library = 1").unwrap();
        assert!(!evidence.valid("target"));
        fs::remove_file(shadow).unwrap();
        assert!(evidence.valid("target"));
        fs::write(root.path().join("later/Library.hs"), "library = 2").unwrap();
        assert!(!evidence.valid("target"));
    }

    #[test]
    fn completed_sources_preserve_replay_refusal_and_revalidate_bytes_and_resolution() {
        let root = tempfile::tempdir().unwrap();
        let mut raw = evidence(root.path());
        raw.cache_safe = false;
        raw.selection_complete = false;
        let bytes = serde_json::to_vec(&raw).unwrap();
        let completed = CompletedSourceEvidence::from_normalized(raw.clone(), "target").unwrap();
        assert_eq!(serde_json::to_vec(&completed).unwrap(), bytes);
        assert!(!completed.valid("target"));
        assert!(completed.revalidate("target").is_ok());
        assert!(completed.revalidate("changed target").is_err());

        let input = root.path().join("Target.hs");
        fs::write(&input, "target").unwrap();
        let mut worker = raw;
        for item in &mut worker.sources {
            if item.path == Path::new(GENERATED_SOURCE) {
                item.path = input.clone();
            }
        }
        for module in &mut worker.modules {
            if module.source == Path::new(GENERATED_SOURCE) {
                module.source = input.clone();
            }
        }
        let worker_bytes = serde_json::to_vec(&worker).unwrap();
        assert!(DependencyEvidence::from_worker(&worker_bytes, &input, "target").is_none());
        assert_eq!(
            CompletedSourceEvidence::from_worker(&worker_bytes, &input, "target").unwrap(),
            completed
        );
        let shadow = root.path().join("first/Library.hs");
        fs::write(&shadow, "library = 1").unwrap();
        assert!(completed.revalidate("target").is_err());
        fs::remove_file(shadow).unwrap();
        assert!(completed.revalidate("target").is_ok());
        fs::write(root.path().join("later/Library.hs"), "library = 2").unwrap();
        assert!(completed.revalidate("target").is_err());
        assert!(CompletedSourceEvidence::from_worker(&worker_bytes, &input, "target").is_err());
    }

    #[test]
    fn completed_worker_evidence_retains_decode_input_and_resolution_failures() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("Target.hs");
        fs::write(&input, "target").unwrap();
        let mut worker = evidence(root.path());
        worker.sources[0].path = input.clone();
        worker.modules[0].source = input.clone();
        let bytes = serde_json::to_vec(&worker).unwrap();
        assert!(matches!(
            CompletedSourceEvidence::from_worker(b"{", &input, "target"),
            Err(DependencyEvidenceFailure::WorkerDecode { .. })
        ));
        let missing = root.path().join("Missing.hs");
        assert!(
            matches!(CompletedSourceEvidence::from_worker(&bytes, &missing, "target"),
            Err(DependencyEvidenceFailure::WorkerInput { path, .. }) if path == missing)
        );
        fs::write(root.path().join("first/Library.hs"), "library = 1").unwrap();
        let expected = DependencyEvidenceFailure::Resolution {
            index: 0,
            reason: ResolutionWitnessFailure::NegativeCandidateUnavailable { index: 0 },
        };
        assert_eq!(
            CompletedSourceEvidence::from_worker(&bytes, &input, "target"),
            Err(expected)
        );
    }

    proptest::proptest! {
        #[test]
        fn completed_source_history_refuses_changed_bytes_and_real_shadow_candidates(
            history in proptest::collection::vec((proptest::bool::ANY, proptest::bool::ANY), 1..32)
        ) {
            let root = tempfile::tempdir().unwrap();
            let completed = CompletedSourceEvidence::from_normalized(evidence(root.path()), "target").unwrap();
            let shadow = root.path().join("first/Library.hs");
            let selected = root.path().join("later/Library.hs");
            for (shadow_present, source_changed) in history {
                if shadow_present {
                    // Identical bytes do not authorize an existing earlier source.
                    fs::write(&shadow, "library = 1").unwrap();
                } else if shadow.exists() {
                    fs::remove_file(&shadow).unwrap();
                }
                fs::write(&selected, if source_changed { "library = 2" } else { "library = 1" }).unwrap();
                let actual = completed.revalidate("target");
                if source_changed {
                    proptest::prop_assert!(matches!(actual, Err(DependencyEvidenceFailure::Source {
                        index: 1, reason: SourceWitnessFailure::Changed { .. },
                    })), "changed consumed source must retain its typed refusal");
                } else if shadow_present {
                    proptest::prop_assert_eq!(actual, Err(DependencyEvidenceFailure::Resolution {
                        index: 0, reason: ResolutionWitnessFailure::NegativeCandidateUnavailable { index: 0 },
                    }));
                } else {
                    proptest::prop_assert_eq!(actual, Ok(()));
                }
            }
        }
    }

    #[test]
    fn incomplete_incompatible_or_malformed_evidence_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let good = evidence(root.path());
        assert!(!good.valid("changed target"));
        let mut bad = good.clone();
        bad.cache_safe = false;
        assert!(!bad.valid("target"));
        bad = good.clone();
        bad.version += 1;
        assert!(!bad.valid("target"));
        bad = good.clone();
        bad.sources.remove(0);
        assert!(!bad.valid("target"));
        bad = good.clone();
        bad.sources.push(bad.sources[0].clone());
        assert!(!bad.valid("target"));
        bad = good.clone();
        bad.resolutions[0].selected = Some(root.path().join("untracked.hs"));
        assert!(!bad.valid("target"));
        bad = good;
        bad.selection_complete = false;
        assert!(bad.valid("target"), "selection completeness is independent");
    }

    #[test]
    fn package_selection_tracks_absent_home_candidates() {
        let root = tempfile::tempdir().unwrap();
        let mut evidence = evidence(root.path());
        let candidate = root.path().join("first/Package.hs");
        evidence.resolutions.push(ResolutionEvidence {
            qualifier: ImportQualifier::Unqualified,
            module: "Package".into(),
            boot: false,
            selected: None,
            candidates: vec![candidate.clone()],
        });
        assert!(evidence.valid("target"));
        fs::write(candidate, "module Package where").unwrap();
        assert!(!evidence.valid("target"));
    }

    #[test]
    fn package_qualifier_cannot_alias_an_unqualified_home_import() {
        let root = tempfile::tempdir().unwrap();
        let mut evidence = evidence(root.path());
        evidence.modules[0].imports[0].qualifier =
            ImportQualifier::OtherUnit("package-unit".into());
        assert!(!evidence.valid("target"));
        evidence.modules[0].imports[0].selected = None;
        evidence.resolutions.push(ResolutionEvidence {
            qualifier: ImportQualifier::OtherUnit("package-unit".into()),
            module: "Library".into(),
            boot: false,
            selected: None,
            candidates: vec![],
        });
        assert!(evidence.valid("target"));
        evidence.modules[0].imports[0].qualifier = ImportQualifier::OtherUnit("other-unit".into());
        assert!(!evidence.valid("target"));
        assert!(serde_json::from_str::<ImportQualifier>("\"other:\"").is_err());
        assert!(serde_json::from_str::<ImportQualifier>("\"unknown\"").is_err());
    }

    #[test]
    fn module_graph_cannot_pair_an_import_with_unrecorded_source() {
        let root = tempfile::tempdir().unwrap();
        let good = evidence(root.path());
        assert!(good.valid("target"));
        let mut wrong_source = good.clone();
        wrong_source.modules[0].source = root.path().join("unrecorded.hs");
        assert!(!wrong_source.valid("target"));
        let mut wrong_import = good.clone();
        wrong_import.modules[0].imports[0].selected = Some(root.path().join("unrecorded.hs"));
        assert!(!wrong_import.valid("target"));
        wrong_import.modules[0].imports[0].selected = Some(GENERATED_SOURCE.into());
        assert!(
            !wrong_import.valid("target"),
            "graph edge must match resolution witness"
        );
        let mut duplicate = good;
        duplicate.modules.push(duplicate.modules[0].clone());
        assert!(!duplicate.valid("target"));
    }

    #[test]
    fn generated_source_identity_is_stable_and_publication_detects_races() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("Generated.hs");
        fs::write(&input, "target").unwrap();
        let mut evidence = evidence(root.path());
        evidence.sources[0].path = input.clone();
        let bytes = serde_json::to_vec(&evidence).unwrap();
        let normalized = DependencyEvidence::from_worker(&bytes, &input, "target").unwrap();
        assert_eq!(normalized.sources[0].path, Path::new(GENERATED_SOURCE));
        fs::write(
            root.path().join("later/Library.hs"),
            "changed during compile",
        )
        .unwrap();
        assert!(DependencyEvidence::from_worker(&bytes, &input, "target").is_none());
    }
}
