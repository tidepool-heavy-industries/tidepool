//! Complete original entries, distinct from source-authority-free fixtures.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::*;
use crate::module_candidates::deployment::NativeCatalogSourceSelection;
use crate::toolchain::{
    AdmittedCompilerDeployment, CompilerDeploymentAuthority, CompilerDeploymentConfiguration,
};

/// Observations of an existing source owner's immutable ordered snapshot.
/// These are expected inputs, not compilation authority: loading still requires
/// the complete original output and the existing original-product validator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenEntrySources {
    roots: Vec<FrozenEntryRoot>,
    source: PathBuf,
    source_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenEntryRoot {
    path: PathBuf,
    files: Vec<(PathBuf, String)>,
}

impl FrozenEntrySources {
    pub fn capture(roots: &[PathBuf], source: &Path) -> Result<Self, CompileError> {
        let roots = roots
            .iter()
            .map(|root| {
                let path = std::fs::canonicalize(root)?;
                Ok(FrozenEntryRoot {
                    files: crate::cache::source_root_manifest(&path).map_err(|error| {
                        if error.source.kind() == std::io::ErrorKind::Interrupted {
                            CompileError::Io(error.source)
                        } else {
                            invalid(error)
                        }
                    })?,
                    path,
                })
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        let source = std::fs::canonicalize(source)?;
        let source_sha256 = hex_sha256(&crate::host_work::read(&source)?);
        Ok(Self {
            roots,
            source,
            source_sha256,
        })
    }

    /// Compare a generated wrapper without exposing or retaining partial bytes.
    /// Acquisition still authenticates the complete source and output closure.
    pub fn source_file_matches(source: &Path, expected: &[u8]) -> Result<bool, CompileError> {
        Ok(crate::host_work::read(source)? == expected)
    }

    fn revalidate(&self) -> Result<(), CompileError> {
        if Self::capture(&self.include_roots(), &self.source)? != *self {
            return Err(invalid("frozen original source snapshot changed"));
        }
        Ok(())
    }

    pub fn include_roots(&self) -> Vec<PathBuf> {
        self.roots.iter().map(|root| root.path.clone()).collect()
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Apply the existing ordered-root identity to these actual observations.
    /// This comparison does not issue compiler custody or source replay rights.
    pub fn source_revision(&self, domain: &[u8]) -> Result<String, CompileError> {
        let manifests = self
            .roots
            .iter()
            .map(|root| {
                crate::cache::SourceRootManifest::from_file_digests(root.files.iter().cloned())
                    .map_err(invalid)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(crate::cache::source_manifests_identity(domain, &manifests))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "selection", rename_all = "snake_case")]
pub enum ProductionEntrySources {
    NativeCatalog(NativeCatalogSourceSelection),
    FrozenWorkspace(FrozenEntrySources),
}

impl ProductionEntrySources {
    fn include_roots(&self) -> Vec<PathBuf> {
        match self {
            Self::NativeCatalog(selection) => selection.include_roots(),
            Self::FrozenWorkspace(selection) => selection.include_roots(),
        }
    }

    fn revalidate(&self, source: &Path) -> Result<(), CompileError> {
        match self {
            Self::NativeCatalog(selection) => {
                if NativeCatalogSourceSelection::capture(&selection.snapshot_root)? != *selection
                    || !selected_native_entry_source(selection, source)
                {
                    return Err(invalid("retained original source selection changed"));
                }
            }
            Self::FrozenWorkspace(selection) => {
                if source != selection.source() {
                    return Err(invalid("original frozen wrapper differs"));
                }
                selection.revalidate()?;
            }
        }
        Ok(())
    }
}

// A direct entry target may live beside the library roots. Its original bytes
// must be an exact member of the retained manifest; this does not add that
// directory to the import search order or widen native catalog module admission.
fn selected_native_entry_source(selection: &NativeCatalogSourceSelection, source: &Path) -> bool {
    source.is_absolute()
        && source.starts_with(&selection.snapshot_root)
        && !source.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
        && selection
            .source_files
            .iter()
            .any(|file| selection.snapshot_root.join(&file.path) == source)
}

/// Owned decoded program and complete original compiler custody. Opening this
/// entry does not execute source, select runtime candidates, or start a compiler.
#[derive(Debug)]
pub struct ProductionEntryOutput {
    prepared: Arc<PreparedProgram>,
    table: DataConTable,
    warnings: MetaWarnings,
    sites: Vec<YieldSite>,
    products: Arc<SealedTurnProducts>,
    source: String,
    source_path: PathBuf,
}

impl ProductionEntryOutput {
    pub fn target_owned(&self) -> Arc<PreparedProgram> {
        Arc::clone(&self.prepared)
    }
    pub fn table(&self) -> &DataConTable {
        &self.table
    }
    pub fn warnings(&self) -> &MetaWarnings {
        &self.warnings
    }
    pub fn products(&self) -> &SealedTurnProducts {
        &self.products
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    /// Exact named source from the validated original-entry provenance.
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }
    pub fn yield_sites(&self) -> &[YieldSite] {
        &self.sites
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryManifest {
    schema: u32,
    purpose: EntryPurpose,
    producer: [u8; 32],
    worker: [u8; 32],
    target: String,
    source: PathBuf,
    sources: ProductionEntrySources,
    files: BTreeMap<PathBuf, String>,
    dependencies: Option<CatalogDependencies>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogDependencies {
    catalog_path: PathBuf,
    catalog_identity: String,
    producer_identity: [u8; 32],
    source_identity: String,
    sources: NativeCatalogSourceSelection,
    selection: certified_products::EntryDependencySelection,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EntryPurpose {
    OriginalSource,
}

const MANIFEST: &str = "entry.json";
const ENTRY_SCHEMA: u32 = 3;
const MAX_FILES: usize = 32_768;
const MAX_FILE_BYTES: u64 = 256 << 20;
const MAX_CONTAINER_BYTES: u64 = 2 << 30;

fn invalid(detail: impl std::fmt::Display) -> CompileError {
    CompileError::ExtractFailed(format!("production entry: {detail}"))
}

fn hex_sha256(bytes: &[u8]) -> String {
    use std::fmt::Write;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            write!(hex, "{byte:02x}").ok();
            hex
        })
}

/// Execute fresh source into one complete retained entry through the runtime
/// compiler endpoint. Matched catalog originals can supply exact dependencies;
/// source replay and ordinary mutable module candidates are unavailable.
/// The caller durably establishes the output parent before requesting publication.
/// An existing unfinished reservation refuses another execution of that identity.
/// The validated output is handed off only after the complete entry and its
/// publication are durable. An unconfirmed publication must use the completed
/// loader to recover that original, rather than execute source again.
pub fn prepare_frozen_production_entry(
    sources: &FrozenEntrySources,
    scratch: &Path,
    output: &Path,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<ProductionEntryOutput, CompileError> {
    prepare_frozen_production_entry_with_catalog(
        sources,
        scratch,
        output,
        &crate::toolchain::CatalogSelection::FreshConfigured,
        settlement,
    )
}

pub fn prepare_frozen_production_entry_with_catalog(
    sources: &FrozenEntrySources,
    scratch: &Path,
    output: &Path,
    catalog: &crate::toolchain::CatalogSelection,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<ProductionEntryOutput, CompileError> {
    sources.revalidate()?;
    if output.exists() || !scratch.is_dir() {
        return Err(invalid(
            "requires private scratch and an absent entry output",
        ));
    }
    let source = std::fs::read_to_string(sources.source())?;
    let include = sources.include_roots();
    let invocation = CompileInvocation {
        source: &source,
        targets: &["__prepared"],
        include: &include,
        fallback_module_name: "Input",
    };
    let output = compile_invocation_inner(
        &invocation,
        &mut |_, _, _| {},
        CompilationPolicy::RetainedEntry {
            source_path: sources.source(),
            output,
            sources: &ProductionEntrySources::FrozenWorkspace(sources.clone()),
            catalog: catalog.acquire()?,
            settlement,
        },
    )?;
    output
        .original_entry
        .ok_or_else(|| invalid("fresh preparation lacks its published original entry"))
}

/// Compile a settled entry under one declared frozen native source selection.
/// The original source path must survive through the existing bundle retention
/// contract. This API does not export arbitrary transient workspace sources.
pub fn build_production_entry(
    source: &Path,
    source_root: &Path,
    scratch: &Path,
    output: &Path,
) -> Result<(), CompileError> {
    let selection = module_candidates::deployment::prepare_build_roots(source_root, output)?;
    if !selected_native_entry_source(&selection, source) {
        return Err(invalid(
            "entry source is outside the retained original source selection",
        ));
    }
    validate_build_action_request(
        source,
        &["__prepared"],
        &selection.include_roots(),
        scratch,
        output,
    )?;
    compile_build_action(
        source,
        &["__prepared"],
        &selection.include_roots(),
        scratch,
        BuildActionExport::ProductionEntry {
            output_root: output,
            source_selection: &ProductionEntrySources::NativeCatalog(selection.clone()),
        },
    )
}

/// One original preparation, exclusively reserved and made durable before the
/// compiler can execute. Drop preserves unfinished raw output. Only successful
/// full sealing moves this same container to the caller's ready path.
pub(super) struct EntryPreparation {
    staging: PathBuf,
    raw: PathBuf,
    output: PathBuf,
    submission: EntrySubmission,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EntrySubmission {
    DefinitelyUnsubmitted,
    MayHaveExecuted,
}

impl EntryPreparation {
    pub(super) fn reserve(output: &Path) -> Result<Self, CompileError> {
        require_absent_output(output)?;
        let parent =
            std::fs::canonicalize(output.parent().ok_or_else(|| invalid("output parent"))?)?;
        let _parent = tidepool_atomic_write::DirectoryAnchor::open_existing(&parent)
            .map_err(|error| CompileError::Io(error.into()))?;
        let output_name = output
            .file_name()
            .ok_or_else(|| invalid("output name"))?
            .to_os_string();
        let output = parent.join(&output_name);
        let mut name = output_name;
        name.push(".preparing");
        let staging = parent.join(name);
        // Confirm any previous known-unsubmitted removal before reusing its
        // absent identity. A failed barrier cannot fall through to execution.
        checkpoint(EntryCheckpoint::AbsenceConfirmation)?;
        tidepool_atomic_write::sync_parent_directory(&staging)
            .map_err(|error| CompileError::Io(error.into()))?;
        match std::fs::create_dir(&staging) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(CompileError::EntryPreparationUnfinished { path: staging });
            }
            Err(error) => return Err(error.into()),
        }
        // Even a failure to confirm this reservation retains its identity.
        // Compiler execution is allowed only after every sync has succeeded.
        checkpoint(EntryCheckpoint::ReservationSync)?;
        tidepool_atomic_write::sync_parent_directory(&staging)
            .map_err(|error| CompileError::Io(error.into()))?;
        let raw = staging.join("raw");
        std::fs::create_dir(&raw)?;
        std::fs::File::open(&raw)?.sync_all()?;
        std::fs::File::open(&staging)?.sync_all()?;
        Ok(Self {
            staging,
            raw,
            output,
            submission: EntrySubmission::DefinitelyUnsubmitted,
        })
    }

    pub(super) fn raw(&self) -> &Path {
        &self.raw
    }

    pub(super) fn begin_execution(&mut self) -> EntrySubmission {
        let previous = self.submission;
        self.submission = EntrySubmission::MayHaveExecuted;
        previous
    }

    pub(super) fn confirm_unsubmitted_attempt(&mut self, previous: EntrySubmission) {
        // A later unsubmitted attempt cannot erase an earlier ambiguous or
        // completed submission. Only this attempt's conservative arm is undone.
        self.submission = previous;
    }

    pub(super) fn mark_uncertain_submission(&mut self) {
        self.submission = EntrySubmission::MayHaveExecuted;
    }

    pub(super) fn release_if_unsubmitted(self) -> Result<(), CompileError> {
        if self.submission == EntrySubmission::MayHaveExecuted {
            return Ok(());
        }
        std::fs::remove_dir_all(&self.staging)
            .map_err(|source| tidepool_atomic_write::WriteError {
                path: self.staging.clone(),
                source,
            })
            .and_then(|()| {
                checkpoint(EntryCheckpoint::ReleaseSync).map_err(|source| {
                    tidepool_atomic_write::WriteError {
                        path: self.staging.clone(),
                        source,
                    }
                })?;
                tidepool_atomic_write::sync_parent_directory(&self.staging)
            })
            .map_err(|source| CompileError::EntryReservationReleaseUnconfirmed {
                path: self.staging.clone(),
                source,
            })
    }

    pub(super) fn seal(
        self,
        source: &Path,
        sources: &ProductionEntrySources,
        deployment: &AdmittedCompilerDeployment,
        targets: &[&str],
        catalog: Option<&Arc<crate::toolchain::DeploymentModulePackage>>,
        dependency_selection: Option<certified_products::EntryDependencySelection>,
    ) -> Result<ProductionEntryOutput, CompileError> {
        if targets != ["__prepared"] {
            return Err(invalid(
                "requires one original settled target and absent output",
            ));
        }
        require_absent_output(&self.output)?;
        let manifest = EntryManifest {
            schema: ENTRY_SCHEMA,
            purpose: EntryPurpose::OriginalSource,
            producer: deployment.producer_identity,
            worker: deployment.consumed_worker_identity,
            target: "__prepared".into(),
            source: source.to_owned(),
            sources: sources.clone(),
            files: inventory(self.raw())?,
            dependencies: match (catalog, dependency_selection) {
                (Some(catalog), Some(selection)) => Some(CatalogDependencies {
                    catalog_path: catalog.catalog_path(),
                    catalog_identity: catalog.catalog_identity().to_owned(),
                    producer_identity: *catalog.producer_identity(),
                    source_identity: catalog.source_identity().to_owned(),
                    sources: catalog.source_selection().clone(),
                    selection,
                }),
                (None, None) => None,
                _ => return Err(invalid("linked entry selection has no catalog owner")),
            },
        };
        checkpoint(EntryCheckpoint::ManifestWrite)?;
        std::fs::write(
            self.staging.join(MANIFEST),
            serde_json::to_vec(&manifest).map_err(invalid)?,
        )?;
        let CompilerDeploymentConfiguration::Configured(authority) =
            CompilerDeploymentConfiguration::from_env().map_err(invalid)?
        else {
            return Err(invalid("configured compiler deployment unavailable"));
        };
        let validated = load_selected_production_entry_with_catalog(
            &self.staging,
            &authority,
            sources,
            &crate::toolchain::CatalogSelection::Acquired(catalog.cloned()),
        )?;
        checkpoint(EntryCheckpoint::TreeSync)?;
        sync_entry_tree(&self.staging)?;
        checkpoint(EntryCheckpoint::ReadyRename)?;
        require_absent_output(&self.output)?;
        std::fs::rename(&self.staging, &self.output)?;
        // A failure here means the entry is visible. Confirm its durability
        // through the completed loader, never by repeating source execution.
        checkpoint(EntryCheckpoint::PublicationSync)
            .map_err(|source| tidepool_atomic_write::WriteError {
                path: self.output.parent().unwrap().to_owned(),
                source,
            })
            .and_then(|()| tidepool_atomic_write::sync_parent_directory(&self.output))
            .map_err(|source| CompileError::EntryPublicationUnconfirmed {
                path: self.output.clone(),
                source,
            })?;
        checkpoint(EntryCheckpoint::SourceRevalidation)?;
        sources.revalidate(source)?;
        checkpoint(EntryCheckpoint::Handoff)?;
        Ok(validated)
    }
}

fn require_absent_output(output: &Path) -> Result<(), CompileError> {
    match std::fs::symlink_metadata(output) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(invalid("requires an absent ready entry output")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EntryCheckpoint {
    AbsenceConfirmation,
    ReservationSync,
    ReleaseSync,
    ManifestWrite,
    TreeSync,
    ReadyRename,
    PublicationSync,
    SourceRevalidation,
    Handoff,
}

fn checkpoint(stage: EntryCheckpoint) -> std::io::Result<()> {
    #[cfg(test)]
    OBSERVER.with(|observer| {
        if let Some(observer) = observer.borrow_mut().as_mut() {
            observer(stage);
        }
    });
    #[cfg(test)]
    if FAILURE.with(|failure| {
        if failure.get().is_some_and(|(selected, _)| selected == stage) {
            failure.set(Some((stage, true)));
            true
        } else {
            false
        }
    }) {
        return Err(std::io::Error::other("injected entry preparation failure"));
    }
    let _ = stage;
    crate::host_work::checkpoint()
}

#[cfg(test)]
thread_local! { static FAILURE: std::cell::Cell<Option<(EntryCheckpoint, bool)>> = const { std::cell::Cell::new(None) }; }

#[cfg(test)]
thread_local! {
    static OBSERVER: std::cell::RefCell<Option<Box<dyn FnMut(EntryCheckpoint)>>> = const { std::cell::RefCell::new(None) };
    static ENTRY_LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn entry_load_count() -> usize {
    ENTRY_LOADS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(super) fn with_checkpoint_observer<T>(
    observer: impl FnMut(EntryCheckpoint) + 'static,
    action: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Box<dyn FnMut(EntryCheckpoint)>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OBSERVER.with(|observer| *observer.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(OBSERVER.with(|slot| slot.replace(Some(Box::new(observer)))));
    action()
}

#[cfg(test)]
pub(super) fn with_failure<T>(stage: EntryCheckpoint, action: impl FnOnce() -> T) -> T {
    struct Restore(Option<(EntryCheckpoint, bool)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAILURE.with(|failure| failure.set(self.0));
        }
    }
    let _restore = Restore(FAILURE.with(|failure| failure.replace(Some((stage, false)))));
    let result = action();
    assert!(
        FAILURE.with(|failure| failure.get() == Some((stage, true))),
        "preparation did not reach injected checkpoint {stage:?}"
    );
    result
}

fn sync_entry_tree(directory: &Path) -> Result<(), CompileError> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_entry_tree(&entry.path())?;
        } else {
            std::fs::File::open(entry.path())?.sync_all()?;
        }
    }
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

/// Validate the complete original container and issue custody through the same
/// original-output owner used after compilation. `sources` is the caller's
/// configured retained source selection, not an identity inferred from the file.
pub fn load_production_entry(
    directory: &Path,
    authority: &CompilerDeploymentAuthority,
    sources: &NativeCatalogSourceSelection,
) -> Result<ProductionEntryOutput, CompileError> {
    load_selected_production_entry(
        directory,
        authority,
        &ProductionEntrySources::NativeCatalog(sources.clone()),
    )
}

/// Open an explicitly selected completed artifact. Snapshot equality does not
/// claim that a fresh compilation with untracked inputs would yield this output.
pub fn load_selected_production_entry(
    directory: &Path,
    authority: &CompilerDeploymentAuthority,
    sources: &ProductionEntrySources,
) -> Result<ProductionEntryOutput, CompileError> {
    load_selected_production_entry_with_catalog(
        directory,
        authority,
        sources,
        &crate::toolchain::CatalogSelection::FreshConfigured,
    )
}

pub fn load_selected_production_entry_with_catalog(
    directory: &Path,
    authority: &CompilerDeploymentAuthority,
    sources: &ProductionEntrySources,
    catalog: &crate::toolchain::CatalogSelection,
) -> Result<ProductionEntryOutput, CompileError> {
    #[cfg(test)]
    ENTRY_LOADS.with(|loads| loads.set(loads.get() + 1));
    crate::host_work::checkpoint()?;
    let bytes = crate::checked_cell::read(directory.join(MANIFEST), 16 << 20)?;
    let manifest: EntryManifest = serde_json::from_slice(&bytes).map_err(invalid)?;
    if manifest.schema != ENTRY_SCHEMA
        || manifest.purpose != EntryPurpose::OriginalSource
        || manifest.target != "__prepared"
        || &manifest.sources != sources
    {
        return Err(invalid(
            "original purpose, target or configured source selection differs",
        ));
    }
    let deployment = CompilerDeploymentConfiguration::Configured(authority.clone())
        .admit(manifest.producer, manifest.worker)
        .map_err(invalid)?;
    sources.revalidate(&manifest.source)?;
    let selection_scratch = tempfile::tempdir()?;
    let selected = if let Some(dependencies) = &manifest.dependencies {
        if !matches!(sources, ProductionEntrySources::FrozenWorkspace(_)) {
            return Err(invalid(
                "native build entry cannot link catalog dependencies",
            ));
        }
        let package = catalog
            .acquire()?
            .ok_or_else(|| invalid("linked entry catalog is unavailable"))?;
        package.validate_deployment(&deployment)?;
        if package.catalog_path() != dependencies.catalog_path
            || package.catalog_identity() != dependencies.catalog_identity
            || package.producer_identity() != &dependencies.producer_identity
            || package.source_identity() != dependencies.source_identity
            || package.source_selection() != &dependencies.sources
            || dependencies.producer_identity != manifest.producer
        {
            return Err(invalid("linked entry catalog selection differs"));
        }
        let candidates = module_candidates::select_acquired_catalog(
            &package,
            &manifest.producer,
            &sources.include_roots(),
            selection_scratch.path(),
        )?
        .ok_or_else(|| invalid("linked entry catalog has no candidate selection"))?;
        Some(Arc::new(
            dependencies.selection.restrict_candidates(candidates)?,
        ))
    } else {
        None
    };
    let raw = directory.join("raw");
    if inventory(&raw)? != manifest.files {
        return Err(invalid("complete original container differs"));
    }
    let source = String::from_utf8(crate::host_work::read(&manifest.source)?)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let prepared = Arc::new(tidepool_repr::execution_schema::parse_program(
        &crate::checked_cell::read(raw.join(prepared_artifact_name("__prepared")), 128 << 20)?,
        &crate::prepared_artifact::production_requirements()?,
        DecodeLimits::default(),
    )?);
    let metadata: Arc<[u8]> = crate::checked_cell::read(raw.join("meta.cbor"), 32 << 20)?.into();
    let (table, warnings) = read_metadata(&metadata)?;
    let sites = parse_asks(&crate::checked_cell::read(raw.join("asks.json"), 16 << 20)?)?.sites();
    let offer = ModuleCandidateOffer {
        selected,
        producer: manifest.producer.to_vec(),
        include: sources.include_roots(),
        exact: None,
        checked_cell: None,
        planned_cell: None,
        checked_values: None,
        checked_projections: Vec::new(),
        checked: None,
        selected_session_values: Default::default(),
    };
    let products = seal_turn_outputs_inner(
        &offer,
        &raw,
        &manifest.source,
        &source,
        &prepared,
        "__prepared",
        Some((&table, &sites)),
        None,
        OriginalOutputPublication::RetainedEntry,
    )?
    .ok_or_else(|| invalid("original native custody unavailable"))?;
    if products.checked.is_some() || products.original_compile_input.is_none() {
        return Err(invalid("entry has no complete original source custody"));
    }
    if let Some(dependencies) = &manifest.dependencies {
        let issued = products
            .source_selection
            .entry_dependency_selection(
                &products.artifact_view,
                offer
                    .selected
                    .as_deref()
                    .expect("linked selection was admitted"),
            )
            .map_err(compiler_evidence_failure)?;
        if issued != dependencies.selection {
            return Err(invalid("linked entry issued dependency closure differs"));
        }
    }
    crate::host_work::checkpoint()?;
    Ok(ProductionEntryOutput {
        prepared,
        table,
        warnings,
        sites,
        products: Arc::new(products),
        source,
        source_path: manifest.source,
    })
}

fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, String>, CompileError> {
    if !std::fs::symlink_metadata(root)?.is_dir() {
        return Err(invalid("container root is not a directory"));
    }
    let mut remaining = vec![root.to_owned()];
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    while let Some(directory) = remaining.pop() {
        crate::host_work::checkpoint()?;
        for entry in std::fs::read_dir(directory)? {
            crate::host_work::checkpoint()?;
            let entry = entry?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                remaining.push(path);
                continue;
            }
            if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
                return Err(invalid(
                    "container contains an alias, special file or oversized artifact",
                ));
            }
            total = total
                .checked_add(metadata.len())
                .ok_or_else(|| invalid("container bound"))?;
            if total > MAX_CONTAINER_BYTES || files.len() >= MAX_FILES {
                return Err(invalid("container bound"));
            }
            let relative = path.strip_prefix(root).map_err(invalid)?.to_owned();
            files.insert(
                relative,
                format!("{:x}", Sha256::digest(crate::host_work::read(&path)?)),
            );
        }
    }
    Ok(files)
}

#[cfg(test)]
mod source_selection_tests {
    use super::*;
    use crate::certified_products::ProductOrigin;
    use proptest::prelude::*;
    use proptest::test_runner::FileFailurePersistence;

    #[test]
    #[ignore = "requires the matched native catalog producer"]
    #[serial_test::serial]
    fn linked_entry_reopens_exact_selection_in_new_process_without_source_replay() {
        use crate::toolchain::CatalogSelection;
        use tidepool_extract_cmd::extract_spawn_count;
        const CHILD_ENTRY: &str = "TIDEPOOL_LINKED_ENTRY_REOPEN";
        const CHILD_MISSING: &str = "TIDEPOOL_LINKED_ENTRY_MISSING_DEPENDENCY";
        let CompilerDeploymentConfiguration::Configured(authority) =
            CompilerDeploymentConfiguration::from_env().unwrap()
        else {
            panic!("requires configured matched compiler authority")
        };
        if let Some(directory) = std::env::var_os(CHILD_ENTRY) {
            let directory = PathBuf::from(directory);
            let manifest: EntryManifest =
                serde_json::from_slice(&std::fs::read(directory.join(MANIFEST)).unwrap()).unwrap();
            let counter = directory.parent().unwrap().join("quote-input.executions");
            let before = std::fs::read(&counter).unwrap();
            let reopened =
                load_selected_production_entry(&directory, &authority, &manifest.sources);
            if std::env::var_os(CHILD_MISSING).is_some() {
                assert!(
                    matches!(reopened, Err(CompileError::ModulePackage(_))),
                    "missing catalog artifact must refuse during fresh acquisition"
                );
            } else {
                assert!(reopened
                    .unwrap()
                    .products()
                    .original_compile_input
                    .is_some());
            }
            assert_eq!(
                extract_spawn_count(),
                0,
                "reopening cannot invoke the compiler"
            );
            assert_eq!(std::fs::read(counter).unwrap(), before);
            return;
        }
        let first_use_started = std::time::Instant::now();
        let package = crate::toolchain::configured_module_package()
            .unwrap()
            .expect("requires the matched native catalog");
        eprintln!(
            "linked entry cold catalog acquisition: {:?}",
            first_use_started.elapsed()
        );
        let catalog = CatalogSelection::Acquired(Some(Arc::clone(&package)));
        let mut root = match std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
            Some(path) => tempfile::Builder::new()
                .prefix("linked-entry-")
                .tempdir_in(path)
                .unwrap(),
            None => tempfile::tempdir().unwrap(),
        };
        root.disable_cleanup(std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT").is_some());
        let authored = root.path().join("sources");
        std::fs::create_dir(&authored).unwrap();
        let input = root.path().join("quote-input");
        let counter = root.path().join("quote-input.executions");
        std::fs::write(&input, "37").unwrap();
        std::fs::write(
            authored.join("QuotedProvider.hs"),
            include_str!("../../tests/fixtures/completed-source/QuotedProvider.hs"),
        )
        .unwrap();
        std::fs::write(
            authored.join("QuotedOriginal.hs"),
            include_str!("../../tests/fixtures/completed-source/QuotedOriginal.hs")
                .replace("QUOTE_INPUT_PATH", input.to_str().unwrap()),
        )
        .unwrap();
        let source = authored.join("LinkedPreparedOriginal.hs");
        std::fs::write(
            &source,
            include_str!("../../tests/fixtures/completed-source/LinkedPreparedOriginal.hs"),
        )
        .unwrap();
        let mut roots = vec![authored];
        roots.extend(package.source_selection().include_roots());
        let sources = FrozenEntrySources::capture(&roots, &source).unwrap();
        let selected = ProductionEntrySources::FrozenWorkspace(sources.clone());
        let output = root.path().join("entry");
        let started = std::time::Instant::now();
        let entry = crate::artifacts::test_support::prepare_frozen_production_entry_with_catalog(
            &sources,
            root.path(),
            &output,
            &catalog,
        )
        .unwrap();
        eprintln!("linked entry cold preparation: {:?}", started.elapsed());
        eprintln!(
            "linked entry cold first use: {:?}",
            first_use_started.elapsed()
        );
        let executions = std::fs::read(&counter).unwrap();
        assert_eq!(
            executions, b"37\n",
            "fresh entry executes its quotation once"
        );
        let receipt = certified_products::decode_receipt_in(
            &std::fs::read(output.join("raw/certified-products.cbor")).unwrap(),
            Some(&output.join("raw")),
        )
        .unwrap();
        assert!(receipt
            .modules
            .iter()
            .any(|module| module.origin == ProductOrigin::Cached
                && module.module == "Tidepool.Prelude"));
        let original = std::fs::read(output.join(MANIFEST)).unwrap();
        let mut manifest: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(manifest["schema"], 3);
        assert_eq!(
            manifest["dependencies"]["catalog_path"],
            package.catalog_path().to_str().unwrap()
        );
        assert!(manifest["dependencies"]["selection"]["linked"]
            .as_array()
            .is_some_and(|selected| !selected.is_empty()));
        let linked_modules = manifest["dependencies"]["selection"]["linked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|original| original["module"].as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            entry
                .products()
                .certified_groups
                .iter()
                .all(
                    |group| !linked_modules.contains(group.owner().module.as_str())
                        || group.origin() == ProductOrigin::Cached
                ),
            "compatible selected catalog originals cannot be freshly rebuilt"
        );
        let fresh =
            decode_fresh_products(&std::fs::read(output.join("raw/module-products.cbor")).unwrap())
                .unwrap();
        assert!(
            fresh
                .iter()
                .all(|product| !linked_modules.contains(product.module.as_str())),
            "entry-owned bytes exclude linked catalog original products"
        );
        std::fs::write(&input, "91").unwrap();
        let before = extract_spawn_count();
        load_selected_production_entry_with_catalog(&output, &authority, &selected, &catalog)
            .unwrap();
        assert!(load_selected_production_entry_with_catalog(
            &output,
            &authority,
            &selected,
            &CatalogSelection::Acquired(None)
        )
        .is_err());
        for field in ["catalog_identity", "catalog_path", "source_identity"] {
            let saved = manifest["dependencies"][field].clone();
            manifest["dependencies"][field] = serde_json::json!("mismatched");
            std::fs::write(
                output.join(MANIFEST),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            assert!(load_selected_production_entry_with_catalog(
                &output, &authority, &selected, &catalog
            )
            .is_err());
            manifest["dependencies"][field] = saved;
        }
        let saved = manifest["dependencies"]["selection"]["linked"][0]["module_version"].clone();
        manifest["dependencies"]["selection"]["linked"][0]["module_version"] =
            serde_json::json!(vec![0; 32]);
        std::fs::write(
            output.join(MANIFEST),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(load_selected_production_entry_with_catalog(
            &output, &authority, &selected, &catalog
        )
        .is_err());
        manifest["dependencies"]["selection"]["linked"][0]["module_version"] = saved;
        for field in ["artifacts", "native_groups"] {
            let saved = manifest["dependencies"]["selection"]["native_closure"][field].clone();
            assert!(!saved.as_array().unwrap().is_empty());
            manifest["dependencies"]["selection"]["native_closure"][field] = serde_json::json!([]);
            std::fs::write(
                output.join(MANIFEST),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            assert!(load_selected_production_entry_with_catalog(
                &output, &authority, &selected, &catalog
            )
            .is_err());
            manifest["dependencies"]["selection"]["native_closure"][field] = saved;
        }
        let linked_count = manifest["dependencies"]["selection"]["linked"]
            .as_array()
            .unwrap()
            .len();
        let mut config = property_config();
        config.cases = 16;
        let mut config = proptest::test_runner::contextualize_config(config);
        config.source_file = Some("tidepool/toolchain/src/artifacts/production_entry.rs");
        config.test_name = Some(concat!(
            module_path!(),
            "::linked_entry_reopens_exact_selection_in_new_process_without_source_replay"
        ));
        let mut runner = proptest::test_runner::TestRunner::new(config);
        runner
            .run(
                &(0..linked_count, 0usize..32, 0u8..8, 0usize..3),
                |(owner, byte, bit, identity)| {
                    let mut changed = manifest.clone();
                    let field = ["module_version", "interface_sha256", "product_sha256"][identity];
                    let original = changed["dependencies"]["selection"]["linked"][owner][field]
                        [byte]
                        .as_u64()
                        .unwrap();
                    changed["dependencies"]["selection"]["linked"][owner][field][byte] =
                        serde_json::json!(original ^ (1 << bit));
                    std::fs::write(output.join(MANIFEST), serde_json::to_vec(&changed).unwrap())
                        .unwrap();
                    prop_assert!(
                        load_selected_production_entry_with_catalog(
                            &output, &authority, &selected, &catalog
                        )
                        .is_err(),
                        "changed selected original cannot acquire catalog custody"
                    );
                    Ok(())
                },
            )
            .unwrap();
        std::fs::write(output.join(MANIFEST), &original).unwrap();
        let mut wrong_authority = authority.clone();
        wrong_authority.producer_identity[0] ^= 1;
        assert!(load_selected_production_entry_with_catalog(
            &output,
            &wrong_authority,
            &selected,
            &catalog
        )
        .is_err());
        assert_eq!(extract_spawn_count(), before);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "artifacts::production_entry::source_selection_tests::linked_entry_reopens_exact_selection_in_new_process_without_source_replay", "--ignored", "--nocapture"])
            .env(CHILD_ENTRY, &output).output().unwrap();
        assert!(
            child.status.success(),
            "fresh process reopen failed: {} {}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
        let catalog_root = package.catalog_path().parent().unwrap().to_owned();
        let copied = root.path().join("missing-dependency-catalog");
        std::fs::create_dir(&copied).unwrap();
        for relative in inventory(&catalog_root).unwrap().keys() {
            let destination = copied.join(relative);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(catalog_root.join(relative), destination).unwrap();
        }
        let catalog_manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(copied.join("catalog.json")).unwrap()).unwrap();
        let required = catalog_manifest["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|module| {
                let owner: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(copied.join(module["owner"]["path"].as_str().unwrap())).unwrap(),
                )
                .unwrap();
                linked_modules.contains(owner["module"].as_str().unwrap())
            })
            .unwrap();
        for relative in [
            required["products"]["path"].as_str().unwrap(),
            required["module_interface"]["core"]["path"]
                .as_str()
                .unwrap(),
        ] {
            let missing = copied.join(relative);
            std::fs::remove_file(&missing).unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "artifacts::production_entry::source_selection_tests::linked_entry_reopens_exact_selection_in_new_process_without_source_replay", "--ignored", "--nocapture"])
                .env(CHILD_ENTRY, &output).env(CHILD_MISSING, "1")
                .env(crate::toolchain::ENV_COMPILER_MODULES, copied.join("catalog.json"))
                .output().unwrap();
            assert!(
                child.status.success(),
                "missing dependency refusal failed: {} {}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
            assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
            std::fs::copy(catalog_root.join(relative), missing).unwrap();
        }
        assert_eq!(extract_spawn_count(), before);
        let source_bytes = std::fs::read(&source).unwrap();
        std::fs::write(
            &source,
            "module LinkedPreparedOriginal where\n__prepared = (91 :: Int)\n",
        )
        .unwrap();
        assert!(load_selected_production_entry_with_catalog(
            &output, &authority, &selected, &catalog
        )
        .is_err());
        std::fs::write(&source, source_bytes).unwrap();
        load_selected_production_entry_with_catalog(&output, &authority, &selected, &catalog)
            .unwrap();
        assert_eq!(extract_spawn_count(), before);
        assert_eq!(std::fs::read(counter).unwrap(), executions);
        let fresh = crate::artifacts::test_support::prepare_frozen_production_entry_with_catalog(
            &sources,
            root.path(),
            &root.path().join("fresh-control"),
            &CatalogSelection::Acquired(None),
        )
        .unwrap();
        let candidates = module_candidates::select_acquired_catalog(
            &package,
            &authority.producer_identity,
            &roots,
            &root.path().join("control-offer"),
        )
        .unwrap()
        .unwrap();
        let projection = fresh
            .products()
            .source_selection
            .compiler_projection(&fresh.products().artifact_view)
            .unwrap();
        assert!(
            candidates.by_owner.values().any(|candidate| {
                let interface = crate::artifact_inventory::ArtifactEntry::canonical(
                    candidate.original_module_interface.clone(),
                )
                .descriptor
                .id;
                projection
                    .roles()
                    .iter()
                    .any(|role| role.interface() == interface)
            }),
            "fresh control must have an interface identical to an available catalog original"
        );
        let issued = fresh
            .products()
            .source_selection
            .entry_dependency_selection(&fresh.products().artifact_view, &candidates)
            .unwrap();
        assert!(
            !issued.has_linked_originals(),
            "fresh compiler issuance cannot borrow identity-equal available originals"
        );
        assert_eq!(
            std::fs::read(root.path().join("quote-input.executions")).unwrap(),
            b"37\n91\n",
            "the independent fresh control executes its own quotation exactly once"
        );
    }

    #[derive(Clone, Debug)]
    enum ReservationOperation {
        Reserve,
        Submit,
        Release { fail_sync: bool },
        Abandon,
    }

    fn reservation_operation() -> impl Strategy<Value = ReservationOperation> {
        prop_oneof![
            Just(ReservationOperation::Reserve),
            Just(ReservationOperation::Submit),
            any::<bool>().prop_map(|fail_sync| ReservationOperation::Release { fail_sync }),
            Just(ReservationOperation::Abandon),
        ]
    }

    fn property_config() -> ProptestConfig {
        let mut config = ProptestConfig::default();
        if std::env::var_os("PROPTEST_CASES").is_none() {
            config.cases = 128;
        }
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
        }
        config
    }

    proptest! {
        #![proptest_config(property_config())]
        #[test]
        fn reservation_histories_preserve_submitted_originals(
            operations in prop::collection::vec(reservation_operation(), 1..48),
        ) {
            let root = tempfile::tempdir().unwrap();
            let output = root.path().join("entry");
            let staging = root.path().join("entry.preparing");
            let mut reservation = None;
            let mut reserved = false;
            let mut submitted = false;
            for operation in operations {
                match operation {
                    ReservationOperation::Reserve => {
                        let result = EntryPreparation::reserve(&output);
                        if reserved {
                            prop_assert!(matches!(result, Err(CompileError::EntryPreparationUnfinished { .. })), "unfinished original cannot be reserved again");
                        } else {
                            reservation = Some(result.unwrap());
                            reserved = true;
                            submitted = false;
                        }
                    }
                    ReservationOperation::Submit => {
                        if let Some(owner) = reservation.as_mut() {
                            owner.begin_execution();
                            std::fs::write(owner.raw().join("original"), b"issued bytes").unwrap();
                            submitted = true;
                        }
                    }
                    ReservationOperation::Release { fail_sync } => {
                        if let Some(owner) = reservation.take() {
                            let result = if fail_sync && !submitted {
                                with_failure(EntryCheckpoint::ReleaseSync, || owner.release_if_unsubmitted())
                            } else {
                                owner.release_if_unsubmitted()
                            };
                            if fail_sync && !submitted {
                                prop_assert!(matches!(result, Err(CompileError::EntryReservationReleaseUnconfirmed { .. })), "failed sync cannot confirm release");
                            } else {
                                prop_assert!(result.is_ok());
                            }
                            if !submitted {
                                reserved = false;
                            }
                        }
                    }
                    ReservationOperation::Abandon => { drop(reservation.take()); }
                }
                prop_assert_eq!(staging.exists(), reserved);
                prop_assert!(!output.exists(), "reservation never issues a published handoff");
                if submitted {
                    prop_assert_eq!(std::fs::read(staging.join("raw/original")).unwrap(), b"issued bytes");
                }
            }
        }
    }
    use crate::toolchain::NativeSourceRole;

    #[test]
    fn frozen_original_authentication_interrupts_without_source_or_container_fallback() {
        use tidepool_extract_cmd::{
            with_compiler_transaction_cancellable, CompilerTransactionCancellation,
            CompilerTransactionClose,
        };
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Original.hs");
        std::fs::write(&source, b"module Original where\nvalue = 1\n").unwrap();
        let cancellation = CompilerTransactionCancellation::new();
        cancellation.cancel();
        let stopped = with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || FrozenEntrySources::capture(&[root.path().to_owned()], &source),
        );
        assert!(
            matches!(stopped.action, Err(CompileError::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted)
        );
        assert_eq!(stopped.close, CompilerTransactionClose::NotStarted);
        let fresh = with_compiler_transaction_cancellable(
            CompilerTransactionCancellation::new(),
            |_| {},
            || {
                let sources = FrozenEntrySources::capture(&[root.path().to_owned()], &source)?;
                assert!(FrozenEntrySources::source_file_matches(
                    &source,
                    b"module Original where\nvalue = 1\n"
                )?);
                sources.revalidate()
            },
        );
        fresh.action.unwrap();
        assert_eq!(fresh.close, CompilerTransactionClose::NotStarted);
    }

    #[test]
    fn direct_entry_source_remains_outside_library_import_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        for role in NativeSourceRole::ORDERED {
            std::fs::create_dir_all(root.join(role.relative_root())).unwrap();
        }
        let direct = root.join("Entry.hs");
        let library = root.join("actors/Actor.hs");
        std::fs::write(&direct, "module Entry where\n").unwrap();
        std::fs::write(&library, "module Actor where\n").unwrap();
        let selection = NativeCatalogSourceSelection {
            source_files: NativeCatalogSourceSelection::source_manifest(&root).unwrap(),
            snapshot_root: root.clone(),
            roles: NativeSourceRole::ORDERED,
        };
        assert!(selected_native_entry_source(&selection, &direct));
        assert!(selected_native_entry_source(&selection, &library));
        assert!(!selection.contains_source(&direct));
        assert!(selection.contains_source(&library));
        assert_eq!(
            selection.include_roots(),
            NativeSourceRole::ORDERED.map(|role| root.join(role.relative_root()))
        );
        let unselected = root.join("Unselected.hs");
        std::fs::write(&unselected, "module Unselected where\n").unwrap();
        assert!(!selected_native_entry_source(&selection, &unselected));
        assert!(!selected_native_entry_source(
            &selection,
            &root.join("actors/../Entry.hs")
        ));
        assert!(!selected_native_entry_source(
            &selection,
            &root.with_file_name("sibling").join("Entry.hs")
        ));
        let mut incomplete = selection;
        incomplete
            .source_files
            .retain(|file| file.path != Path::new("Entry.hs"));
        assert!(!selected_native_entry_source(&incomplete, &direct));
    }
}
