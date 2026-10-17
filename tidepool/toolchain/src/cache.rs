//! One recipe-keyed, atomically published artifact bundle per compilation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::artifacts::invocation_output::{InvocationOutputBundle, InvocationOutputLayout};
use crate::digest::frame;
use std::sync::Arc;
use tidepool_repr::execution_schema::InventoryOperation;

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

    /// A source-path witness cannot prove GHC's package lookup result. The
    /// existing invocation cache returns before entering the worker, so only
    /// a request with no package imports can reuse its output until lookup is
    /// revalidated by a worker in the same compile transaction.
    fn reusable_without_worker(&self, source: &str) -> bool {
        self.packages.is_empty()
            && self
                .resolutions
                .iter()
                .all(|resolution| resolution.selected.is_some())
            && self.valid(source)
    }
}

/// A recipe names the source and the complete ordered compiler invocation.
/// Dependency contents are validated from worker evidence, not directory scans.
/// Unknown options and mutable session inputs cannot form a cache recipe.
pub struct Invocation<'a> {
    pub source: &'a str,
    pub argv: &'a [OsString],
    pub input_path: &'a Path,
    pub include: &'a [PathBuf],
    /// The bound compiler's producer identity (frontend bytes + worker
    /// selection + worker bytes + GHC libdir) — stable across a daemon
    /// reboot. A frontend or worker rebuild changes it; a stdlib-only edit
    /// is caught separately by GHC's per-module interface hash, not by this
    /// key.
    pub endpoint_identity: &'a [u8],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationKey(String);

impl std::fmt::Display for InvocationKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn invocation_key(inv: &Invocation<'_>) -> Option<InvocationKey> {
    if inv.endpoint_identity.is_empty() {
        return None;
    }
    let mut hasher = blake3::Hasher::new();
    // The complete typed output bundle invalidates incomplete positional bundles.
    frame(&mut hasher, b"tidepool-compile-recipe-v4");
    frame(&mut hasher, inv.source.as_bytes());
    frame(&mut hasher, inv.input_path.file_name()?.as_encoded_bytes());
    let mut args = inv.argv.iter();
    let mut includes = Vec::new();
    let mut input_seen = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--output-dir") => {
                args.next()?;
            }
            Some("--build-products-dir") => {
                // Hash the requested logical root. The process boundary adds
                // an exclusive writer namespace only for mutable output
                // placement, after this recipe is formed; it introduces no
                // source inputs and is deliberately not a recipe dimension.
                frame(&mut hasher, b"--build-products-dir");
                let path = std::path::absolute(PathBuf::from(args.next()?)).ok()?;
                frame(&mut hasher, path.as_os_str().as_encoded_bytes());
            }
            Some("--include") => {
                includes.push(PathBuf::from(args.next()?));
            }
            Some(flag @ ("--target" | "--targets")) => {
                frame(&mut hasher, flag.as_bytes());
                frame(&mut hasher, args.next()?.as_encoded_bytes());
            }
            _ if arg.as_os_str() == inv.input_path.as_os_str() && !input_seen => {
                input_seen = true;
            }
            _ => return None,
        }
    }
    if !input_seen || includes != inv.include {
        return None;
    }
    frame(&mut hasher, &(inv.include.len() as u64).to_le_bytes());
    for root in inv.include {
        let absolute = std::path::absolute(root).ok()?;
        frame(&mut hasher, absolute.as_os_str().as_encoded_bytes());
    }
    frame(&mut hasher, inv.endpoint_identity);
    Some(InvocationKey(hasher.finalize().to_hex().to_string()))
}

fn take_frame<'a>(remaining: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (length, rest) = remaining.split_at_checked(8)?;
    let length = usize::try_from(u64::from_le_bytes(length.try_into().ok()?)).ok()?;
    let (value, rest) = rest.split_at_checked(length)?;
    *remaining = rest;
    Some(value)
}

fn append_frame(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Read one immutable bundle; the evidence participates in the same integrity
/// manifest as every named artifact. Old layouts and malformed bundles miss.
pub(crate) fn artifacts_load(
    key: &InvocationKey,
    layout: &InvocationOutputLayout,
    source: &str,
) -> std::io::Result<Option<(InvocationOutputBundle, DependencyEvidence)>> {
    crate::host_work::checkpoint()?;
    let operation = Arc::new(InventoryOperation::new(Default::default()));
    let path = match std::path::absolute(
        crate::paths::compile_cache_dir().join(format!("{key}.bundle")),
    ) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    // This is an acquisition/work bound, not a cache retention limit. Use the
    // same aggregate owner for the physical bundle, captured members and parsers.
    let bytes = match crate::certified_products::read_bounded_with_operation(
        &path,
        operation.limits().max_bytes as u64,
        &operation,
    ) {
        Ok(bytes) => bytes,
        Err(crate::certified_products::CertificationError::Interrupted(error)) => {
            return Err(error)
        }
        Err(_) => {
            crate::host_work::checkpoint()?;
            return Ok(None);
        }
    };
    decode_bundle(&bytes, layout, source, operation)
}

fn decode_bundle(
    bytes: &[u8],
    layout: &InvocationOutputLayout,
    source: &str,
    operation: Arc<InventoryOperation>,
) -> std::io::Result<Option<(InvocationOutputBundle, DependencyEvidence)>> {
    use std::io::{Error, ErrorKind};
    let invalid = || Error::from(ErrorKind::InvalidData);
    let decode = || -> std::io::Result<_> {
        crate::host_work::checkpoint()?;
        let mut remaining = bytes;
        let manifest = tidepool_extract_report::artifact_manifest::ArtifactManifest::decode(
            take_frame(&mut remaining).ok_or_else(invalid)?,
        )
        .map_err(|_| invalid())?;
        if manifest.entries().len() != layout.len() + 1 {
            return Err(invalid());
        }
        let mut entries = manifest.entries().iter();
        let bundle = layout.capture(operation.clone(), |name, prepared| {
            crate::host_work::checkpoint()?;
            let entry = entries.next().ok_or_else(invalid)?;
            if entry.name() != name {
                return Err(invalid());
            }
            let value = take_frame(&mut remaining).ok_or_else(invalid)?;
            let limit = if prepared {
                tidepool_repr::execution_schema::DecodeLimits::default().max_bytes
            } else {
                operation.limits().max_bytes
            };
            if value.len() > limit || !entry.matches(value) {
                return Err(invalid());
            }
            operation.charge(value.len()).map_err(|_| invalid())?;
            Ok(value.to_vec())
        })?;
        let entry = entries.next().ok_or_else(invalid)?;
        let value = take_frame(&mut remaining).ok_or_else(invalid)?;
        if entry.name() != "dependencies.json"
            || !entry.matches(value)
            || entries.next().is_some()
            || !remaining.is_empty()
        {
            return Err(invalid());
        }
        // Match the fresh compiler front door's conservative dependency-JSON
        // decoding charge, before serde may expand its strings and containers.
        operation
            .charge(value.len().checked_mul(64).ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        let evidence: DependencyEvidence = serde_json::from_slice(value).map_err(|_| invalid())?;
        if !evidence.reusable_without_worker(source) {
            return Err(invalid());
        }
        // Completion cutoff for the fully authenticated immutable memo.
        crate::host_work::checkpoint()?;
        Ok((bundle, evidence))
    };
    match decode() {
        Ok(result) => Ok(Some(result)),
        Err(error) if error.kind() == ErrorKind::Interrupted => Err(error),
        Err(_) => {
            crate::host_work::checkpoint()?;
            Ok(None)
        }
    }
}

fn encode_bundle(
    bundle: &InvocationOutputBundle,
    evidence: &DependencyEvidence,
) -> Option<Vec<u8>> {
    let dependencies = serde_json::to_vec(evidence).ok()?;
    let mut artifacts: Vec<_> = bundle.named_artifacts().collect();
    artifacts.push(("dependencies.json".into(), dependencies.as_slice()));
    let manifest = tidepool_extract_report::artifact_manifest::ArtifactManifest::from_artifacts(
        artifacts
            .iter()
            .map(|(name, bytes)| (name.as_str(), Some(*bytes))),
    );
    let mut encoded = Vec::new();
    append_frame(&mut encoded, &manifest.encode());
    for (_, bytes) in artifacts {
        append_frame(&mut encoded, bytes);
    }
    Some(encoded)
}

pub(crate) fn artifacts_store(
    key: &InvocationKey,
    bundle: &InvocationOutputBundle,
    evidence: &DependencyEvidence,
    source: &str,
) {
    if !evidence.valid(source) {
        return;
    }
    let Some(encoded) = encode_bundle(bundle, evidence) else {
        return;
    };
    let dir = crate::paths::compile_cache_dir();
    if fs::create_dir_all(&dir).is_err() || !evidence.valid(source) {
        return;
    }
    // Best-effort publication: an unwritable memo costs a fresh compilation.
    tidepool_atomic_write::write_best_effort(&dir.join(format!("{key}.bundle")), &encoded).ok();
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
    fn incomplete_incompatible_or_malformed_evidence_cannot_hit() {
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
        assert!(!evidence.reusable_without_worker("target"));
        fs::write(candidate, "module Package where").unwrap();
        assert!(!evidence.valid("target"));
    }

    #[test]
    fn package_import_needs_worker_validation_before_a_direct_cache_hit() {
        let root = tempfile::tempdir().unwrap();
        let mut evidence = evidence(root.path());
        evidence.packages.push("Data.List".into());
        assert!(
            evidence.valid("target"),
            "fresh worker inventory remains usable"
        );
        assert!(!evidence.reusable_without_worker("target"));
        evidence.packages.clear();
        assert!(evidence.reusable_without_worker("target"));
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
        assert!(!evidence.reusable_without_worker("target"));
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

    fn key(source: &str, input: &Path, roots: &[PathBuf], endpoint: &[u8]) -> InvocationKey {
        let mut argv = vec![
            input.as_os_str().to_owned(),
            "--target".into(),
            "result".into(),
        ];
        for root in roots {
            argv.extend(["--include".into(), root.as_os_str().to_owned()]);
        }
        invocation_key(&Invocation {
            source,
            argv: &argv,
            input_path: input,
            include: roots,
            endpoint_identity: endpoint,
        })
        .unwrap()
    }

    #[test]
    fn recipe_binds_source_logical_location_order_and_endpoint() {
        let a = key(
            "target",
            Path::new("/scratch/a/Generated.hs"),
            &[],
            b"endpoint",
        );
        assert_eq!(
            a,
            key(
                "target",
                Path::new("/scratch/b/Generated.hs"),
                &[],
                b"endpoint"
            )
        );
        assert_ne!(
            a,
            key("target", Path::new("/scratch/a/Other.hs"), &[], b"endpoint")
        );
        assert_ne!(
            a,
            key(
                "changed",
                Path::new("/scratch/a/Generated.hs"),
                &[],
                b"endpoint"
            )
        );
        assert_ne!(
            a,
            key(
                "target",
                Path::new("/scratch/a/Generated.hs"),
                &[],
                b"rebound"
            )
        );
        let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        assert_ne!(
            key("target", Path::new("Generated.hs"), &roots, b"endpoint"),
            key(
                "target",
                Path::new("Generated.hs"),
                &roots.into_iter().rev().collect::<Vec<_>>(),
                b"endpoint"
            )
        );
    }

    #[test]
    fn unknown_options_and_session_requests_are_uncacheable() {
        for option in ["--session-root", "--inject-val", "--future-transform"] {
            let argv = vec!["Generated.hs".into(), option.into(), "value".into()];
            assert!(invocation_key(&Invocation {
                source: "target",
                argv: &argv,
                input_path: Path::new("Generated.hs"),
                include: &[],
                endpoint_identity: b"endpoint",
            })
            .is_none());
        }
    }

    #[test]
    fn interrupted_bundle_lookup_is_a_refusal_and_fresh_decode_recovers() {
        use tidepool_extract_cmd::{
            with_compiler_transaction_cancellable, CompilerTransactionCancellation,
            CompilerTransactionClose,
        };
        let cancellation = CompilerTransactionCancellation::new();
        cancellation.cancel();
        let stopped = with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || {
                artifacts_load(
                    &InvocationKey("cancelled-lookup-must-not-read".into()),
                    &InvocationOutputLayout::new(&["result"]).unwrap(),
                    "target",
                )
            },
        );
        assert_eq!(
            stopped.action.unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(stopped.close, CompilerTransactionClose::NotStarted);
        let recovered = with_compiler_transaction_cancellable(
            CompilerTransactionCancellation::new(),
            |_| {},
            || {
                decode_bundle(
                    b"malformed",
                    &InvocationOutputLayout::new(&["result"]).unwrap(),
                    "target",
                    Arc::new(InventoryOperation::new(Default::default())),
                )
            },
        );
        assert!(recovered.action.unwrap().is_none());
        assert_eq!(recovered.close, CompilerTransactionClose::NotStarted);
    }

    fn output_fixture(
        root: &Path,
        targets: &[&str],
    ) -> (InvocationOutputLayout, InvocationOutputBundle) {
        let layout = InvocationOutputLayout::new(targets).unwrap();
        let operation = Arc::new(InventoryOperation::new(Default::default()));
        // Representation-only fixtures: disk acquisition owns these bytes;
        // they do not manufacture compiler certificates or source authority.
        let bundle = layout
            .capture(operation, |name, _| {
                fs::write(root.join(name), name.as_bytes())?;
                fs::read(root.join(name))
            })
            .unwrap();
        (layout, bundle)
    }

    fn decode_output(
        bytes: &[u8],
        layout: &InvocationOutputLayout,
        source: &str,
    ) -> Option<(InvocationOutputBundle, DependencyEvidence)> {
        decode_bundle(
            bytes,
            layout,
            source,
            Arc::new(InventoryOperation::new(Default::default())),
        )
        .unwrap()
    }

    /// Deliberate storage corruption, retaining valid integrity checksums so
    /// membership/order refusals cannot pass merely because a digest is wrong.
    fn wire_bundle(artifacts: &[(String, Vec<u8>)]) -> Vec<u8> {
        let manifest = tidepool_extract_report::artifact_manifest::ArtifactManifest::from_artifacts(
            artifacts
                .iter()
                .map(|(name, bytes)| (name.as_str(), Some(bytes.as_slice()))),
        );
        let mut bytes = Vec::new();
        append_frame(&mut bytes, &manifest.encode());
        for (_, value) in artifacts {
            append_frame(&mut bytes, value);
        }
        bytes
    }

    #[test]
    fn invocation_bundle_acquisition_enforces_inventory_work_and_member_read_bounds() {
        use tidepool_repr::execution_schema::InventoryDecodeLimits;
        let root = tempfile::tempdir().unwrap();
        let evidence = evidence(root.path());
        let (layout, _) = output_fixture(root.path(), &["result"]);
        let bundle = layout
            .read(
                root.path(),
                Arc::new(InventoryOperation::new(Default::default())),
            )
            .unwrap();
        let bytes = encode_bundle(&bundle, &evidence).unwrap();
        assert!(decode_output(&bytes, &layout, "target").is_some());
        let bounded = InventoryOperation::new(InventoryDecodeLimits {
            max_work: 1,
            ..Default::default()
        });
        assert!(decode_bundle(&bytes, &layout, "target", Arc::new(bounded))
            .unwrap()
            .is_none());
        fs::write(root.path().join("asks.json"), [0; 128]).unwrap();
        let bounded = InventoryOperation::new(InventoryDecodeLimits {
            max_bytes: 127,
            ..Default::default()
        });
        let Err(crate::CompileError::CompilerEvidence(error)) =
            layout.read(root.path(), Arc::new(bounded))
        else {
            panic!("required asks must respect the aggregate admission byte bound");
        };
        assert!(matches!(
            *error,
            crate::certified_products::CertificationError::EvidenceRead {
                failure: crate::certified_products::EvidenceReadFailure::SizeLimit {
                    actual: 128,
                    limit: 127
                },
                ..
            }
        ));
    }

    #[test]
    fn package_free_runtime_invocation_reuses_complete_bundle_and_refuses_bad_package_sidecar() {
        use crate::artifacts::{test_support, CompileInvocation};
        let root = tempfile::tempdir().unwrap();
        let includes = [root.path().to_owned()];
        let source = include_str!("../tests/fixtures/invocation-memo/NoPackageMemo.hs");
        let targets = ["second", "first"];
        let invocation = CompileInvocation {
            source,
            targets: &targets,
            include: &includes,
            fallback_module_name: "NoPackageMemo",
        };
        let compile = || {
            let mut executions = 0;
            let output = test_support::compile_invocation(&invocation, |stage, _, _| {
                executions += usize::from(stage == crate::timing::STAGE_EXTRACT_SPAWN);
            })
            .expect("supported package-free runtime request");
            assert!(
                output.module_products.is_empty(),
                "witness requires zero native home products"
            );
            assert!(output
                .module_inventory
                .as_ref()
                .unwrap()
                .iter()
                .all(|module| module.product != ProductAvailability::Ready));
            assert_eq!(
                output
                    .targets
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["first", "second"]
            );
            (executions, output)
        };
        let cache_root = crate::paths::compile_cache_dir();
        let inventory = || -> std::collections::BTreeSet<_> {
            fs::read_dir(&cache_root)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "bundle")
                })
                .collect()
        };
        fs::create_dir_all(&cache_root).unwrap();
        let before = inventory();
        let (executions, _) = compile();
        assert_eq!(executions, 1, "fresh request must execute the compiler");
        let added = inventory().difference(&before).cloned().collect::<Vec<_>>();
        assert_eq!(added.len(), 1, "eligible frontdoor must publish one bundle");
        let path = &added[0];
        let bytes = fs::read(path).unwrap();
        let layout = InvocationOutputLayout::new(&targets).unwrap();
        let (_, evidence) =
            decode_output(&bytes, &layout, source).expect("frontdoor published reusable evidence");
        assert!(evidence.packages.is_empty());
        let (executions, _) = compile();
        assert_eq!(
            executions, 0,
            "repeat must reuse memo without entering the worker"
        );

        // Deliberate cache corruption with a valid checksum tests the semantic
        // package-sidecar consumer independently of integrity authentication.
        let mut remaining = bytes.as_slice();
        let manifest = tidepool_extract_report::artifact_manifest::ArtifactManifest::decode(
            take_frame(&mut remaining).unwrap(),
        )
        .unwrap();
        let mut artifacts = manifest
            .entries()
            .iter()
            .map(|entry| {
                (
                    entry.name().to_owned(),
                    take_frame(&mut remaining).unwrap().to_vec(),
                )
            })
            .collect::<Vec<_>>();
        let package = artifacts
            .iter_mut()
            .find(|(name, _)| name == "module-package-imports.cbor")
            .unwrap();
        package.1 = b"malformed package inventory".to_vec();
        fs::write(path, wire_bundle(&artifacts)).unwrap();
        assert_eq!(
            compile().0,
            1,
            "invalid paired package evidence must force fresh acquisition"
        );
        assert_eq!(
            compile().0,
            0,
            "fresh compiler output restores a complete reusable bundle"
        );
    }

    #[test]
    fn invocation_bundle_round_trip_binds_required_sidecars_and_target_order() {
        for targets in [&["second"][..], &["second", "first"][..]] {
            let root = tempfile::tempdir().unwrap();
            let evidence = evidence(root.path());
            let (layout, bundle) = output_fixture(root.path(), targets);
            let bytes = encode_bundle(&bundle, &evidence).unwrap();
            let (loaded, _) = decode_output(&bytes, &layout, "target")
                .expect("production writer/reader round trip");
            let expected_names = if targets.len() == 1 {
                vec![
                    "meta.cbor",
                    "second.prepared.cbor",
                    "asks.json",
                    "module-products.cbor",
                    "module-package-imports.cbor",
                ]
            } else {
                vec![
                    "meta.cbor",
                    "second.prepared.cbor",
                    "second.asks.json",
                    "first.prepared.cbor",
                    "first.asks.json",
                    "module-products.cbor",
                    "module-package-imports.cbor",
                ]
            };
            let mut remaining = bytes.as_slice();
            let manifest = tidepool_extract_report::artifact_manifest::ArtifactManifest::decode(
                take_frame(&mut remaining).unwrap(),
            )
            .unwrap();
            let mut expected_manifest = expected_names.clone();
            expected_manifest.push("dependencies.json");
            assert_eq!(
                manifest
                    .entries()
                    .iter()
                    .map(|entry| entry.name())
                    .collect::<Vec<_>>(),
                expected_manifest
            );
            let members: Vec<_> = loaded.named_artifacts().collect();
            assert_eq!(
                members
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>(),
                expected_names
            );
            for (name, bytes) in members {
                assert_eq!(bytes, fs::read(root.path().join(name)).unwrap());
            }
            assert_eq!(loaded.package_imports(), b"module-package-imports.cbor");
            assert_eq!(
                loaded
                    .targets()
                    .iter()
                    .map(|target| target.target.as_str())
                    .collect::<Vec<_>>(),
                targets
            );
            assert_eq!(
                loaded.bytes_len(),
                expected_names
                    .iter()
                    .map(|name| name.len() as u64)
                    .sum::<u64>()
            );
            assert_eq!(loaded.bytes_len(), bundle.bytes_len());

            let mut members: Vec<_> = bundle
                .named_artifacts()
                .map(|(name, bytes)| (name, bytes.to_vec()))
                .collect();
            members.push((
                "dependencies.json".into(),
                serde_json::to_vec(&evidence).unwrap(),
            ));
            // Exactly the old producer mismatch: required final package sidecar
            // omitted, while dependency evidence and all other members survive.
            let package_index = members
                .iter()
                .position(|(name, _)| name == "module-package-imports.cbor")
                .unwrap();
            let mut missing = members.clone();
            missing.remove(package_index);
            assert!(decode_output(&wire_bundle(&missing), &layout, "target").is_none());
            for index in 0..members.len() {
                let mut missing = members.clone();
                missing.remove(index);
                assert!(decode_output(&wire_bundle(&missing), &layout, "target").is_none());
            }
            let mut extra = members.clone();
            extra.push(("unexpected".into(), vec![1]));
            assert!(decode_output(&wire_bundle(&extra), &layout, "target").is_none());
            let mut reordered = members.clone();
            reordered.swap(0, package_index);
            assert!(decode_output(&wire_bundle(&reordered), &layout, "target").is_none());
            let mut duplicate = members.clone();
            duplicate[package_index] = duplicate[0].clone();
            assert!(decode_output(&wire_bundle(&duplicate), &layout, "target").is_none());
            let mut trailing = bytes.clone();
            append_frame(&mut trailing, b"extra frame");
            assert!(decode_output(&trailing, &layout, "target").is_none());
            let mut corrupt = bytes.clone();
            *corrupt.last_mut().unwrap() ^= 1;
            assert!(decode_output(&corrupt, &layout, "target").is_none());
            assert!(decode_output(&bytes[..bytes.len() - 1], &layout, "target").is_none());
            assert!(decode_output(&bytes, &layout, "different source").is_none());
            let other = InvocationOutputLayout::new(&["other"]).unwrap();
            assert!(decode_output(&bytes, &other, "target").is_none());

            let mut package_evidence = evidence.clone();
            package_evidence.packages.push("Data.List".into());
            assert!(decode_output(
                &encode_bundle(&bundle, &package_evidence).unwrap(),
                &layout,
                "target"
            )
            .is_none());
            fs::write(root.path().join("first/Library.hs"), "shadow").unwrap();
            assert!(decode_output(&bytes, &layout, "target").is_none());
            fs::remove_file(root.path().join("first/Library.hs")).unwrap();
            assert!(decode_output(&bytes, &layout, "target").is_some());
            fs::write(root.path().join("later/Library.hs"), "changed").unwrap();
            assert!(decode_output(&bytes, &layout, "target").is_none());
        }
        assert!(InvocationOutputLayout::new(&["same", "same"]).is_err());
        assert!(InvocationOutputLayout::new(&[]).is_err());
        for target in ["", "../outside", "/outside", "nested/target"] {
            assert!(
                InvocationOutputLayout::new(&[target]).is_err(),
                "invalid target {target:?}"
            );
        }
    }
}
