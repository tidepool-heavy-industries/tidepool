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
    sources: NativeCatalogSourceSelection,
    files: BTreeMap<PathBuf, String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EntryPurpose {
    OriginalSource,
}

const MANIFEST: &str = "entry.json";
const MAX_FILES: usize = 32_768;
const MAX_FILE_BYTES: u64 = 256 << 20;
const MAX_CONTAINER_BYTES: u64 = 2 << 30;

fn invalid(detail: impl std::fmt::Display) -> CompileError {
    CompileError::ExtractFailed(format!("production entry: {detail}"))
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
    if !selection.contains_source(source)
        || !selection
            .source_files
            .iter()
            .any(|file| selection.snapshot_root.join(&file.path) == source)
    {
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
            source_selection: &selection,
        },
    )
}

pub(super) fn export(
    original: &Path,
    output: &Path,
    source: &Path,
    sources: &NativeCatalogSourceSelection,
    deployment: &AdmittedCompilerDeployment,
    targets: &[&str],
) -> Result<(), CompileError> {
    if targets != ["__prepared"] || output.exists() {
        return Err(invalid(
            "requires one original settled target and absent output",
        ));
    }
    let parent = output.parent().ok_or_else(|| invalid("output parent"))?;
    let staging = tempfile::tempdir_in(parent)?;
    let raw = staging.path().join("raw");
    std::fs::create_dir(&raw)?;
    let files = inventory(original)?;
    for relative in files.keys() {
        let destination = raw.join(relative);
        std::fs::create_dir_all(
            destination
                .parent()
                .ok_or_else(|| invalid("artifact parent"))?,
        )?;
        std::fs::copy(original.join(relative), destination)?;
    }
    if inventory(&raw)? != files {
        return Err(invalid("original container changed while copying"));
    }
    let manifest = EntryManifest {
        schema: 1,
        purpose: EntryPurpose::OriginalSource,
        producer: deployment.producer_identity,
        worker: deployment.consumed_worker_identity,
        target: "__prepared".into(),
        source: source.to_owned(),
        sources: sources.clone(),
        files,
    };
    std::fs::write(
        staging.path().join(MANIFEST),
        serde_json::to_vec(&manifest).map_err(invalid)?,
    )?;
    let CompilerDeploymentConfiguration::Configured(authority) =
        CompilerDeploymentConfiguration::from_env().map_err(invalid)?
    else {
        return Err(invalid("configured compiler deployment unavailable"));
    };
    load_production_entry(staging.path(), &authority, sources)?;
    std::fs::rename(staging.path(), output)?;
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
    let bytes = crate::checked_cell::read(directory.join(MANIFEST), 16 << 20)?;
    let manifest: EntryManifest = serde_json::from_slice(&bytes).map_err(invalid)?;
    if manifest.schema != 1
        || manifest.purpose != EntryPurpose::OriginalSource
        || manifest.target != "__prepared"
        || &manifest.sources != sources
    {
        return Err(invalid(
            "original purpose, target or configured source selection differs",
        ));
    }
    CompilerDeploymentConfiguration::Configured(authority.clone())
        .admit(manifest.producer, manifest.worker)
        .map_err(invalid)?;
    if NativeCatalogSourceSelection::capture(&sources.snapshot_root)? != *sources
        || !sources.contains_source(&manifest.source)
        || !sources
            .source_files
            .iter()
            .any(|file| sources.snapshot_root.join(&file.path) == manifest.source)
    {
        return Err(invalid("retained original source selection changed"));
    }
    let raw = directory.join("raw");
    if inventory(&raw)? != manifest.files {
        return Err(invalid("complete original container differs"));
    }
    let source = std::fs::read_to_string(&manifest.source)?;
    let prepared = Arc::new(tidepool_repr::execution_schema::parse_program(
        &crate::checked_cell::read(raw.join(prepared_artifact_name("__prepared")), 128 << 20)?,
        &crate::prepared_artifact::production_requirements()?,
        DecodeLimits::default(),
    )?);
    let metadata: Arc<[u8]> = crate::checked_cell::read(raw.join("meta.cbor"), 32 << 20)?.into();
    let (table, warnings) = read_metadata(&metadata)?;
    let sites = parse_asks(&crate::checked_cell::read(raw.join("asks.json"), 16 << 20)?)?;
    let offer = ModuleCandidateOffer {
        selected: None,
        producer: manifest.producer.to_vec(),
        include: sources.include_roots(),
        exact: None,
        checked_cell: None,
        planned_cell: None,
        checked_values: None,
        checked_projections: Vec::new(),
        checked: None,
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
    Ok(ProductionEntryOutput {
        prepared,
        table,
        warnings,
        sites,
        products: Arc::new(products),
        source,
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
        for entry in std::fs::read_dir(directory)? {
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
                format!("{:x}", Sha256::digest(std::fs::read(&path)?)),
            );
        }
    }
    Ok(files)
}
