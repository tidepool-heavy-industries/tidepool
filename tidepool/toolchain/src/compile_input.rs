//! Input continuity is distinct from native inventory identity. The digest
//! excludes compiler-assigned output identifiers; its private proof binds the
//! entire output bundle that consumed those inputs in this process.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tidepool_repr::{execution_schema::PreparedProgram, DataConTable};

use crate::artifacts::YieldSite;
use crate::cache::{DependencyEvidence, ModuleImportEvidence, ResolutionEvidence};
use crate::certified_products::{
    CertifiedTargetPackageInterfaces, PackageInterfaceWitness, PendingCertifiedGroup,
    PendingImportOwner,
};
use crate::CompileError;

/// Compiler-issued custody of one completed original source compilation.
/// Cloning this proof does not authorize a different native output bundle.
#[derive(Debug, Clone)]
pub struct SealedOriginalCompileInput {
    identity: String,
    source: Arc<str>,
    original_interfaces: Arc<crate::declaration_context::ExactDeclarationContext>,
    original_execution: Arc<crate::declaration_context::ExactDeclarationContext>,
    target: Arc<PreparedProgram>,
    groups: Arc<[PendingCertifiedGroup]>,
    target_owners: Arc<[PendingImportOwner]>,
    package_interfaces: CertifiedTargetPackageInterfaces,
    table: DataConTable,
    sites: Arc<[YieldSite]>,
    replay: SourceReplayEligibility,
}

/// Eligibility of the original consumed source evidence for a new source recipe.
/// Completed native output custody is independent of this decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceReplayEligibility {
    Eligible,
    UntrackedInputs,
    IncompleteSelection,
    UntrackedInputsAndIncompleteSelection,
}

impl SourceReplayEligibility {
    fn from_evidence(evidence: &DependencyEvidence) -> Self {
        match (evidence.cache_safe, evidence.selection_complete) {
            (true, true) => Self::Eligible,
            (false, true) => Self::UntrackedInputs,
            (true, false) => Self::IncompleteSelection,
            (false, false) => Self::UntrackedInputsAndIncompleteSelection,
        }
    }
}

impl SealedOriginalCompileInput {
    /// Exact generated source bytes consumed by this completed original.
    pub fn original_source(&self) -> &str {
        &self.source
    }

    pub fn source_replay_eligibility(&self) -> SourceReplayEligibility {
        self.replay
    }

    /// Source-recipe intent is available only when its original evidence permits
    /// replay. The returned text remains an observation, never execution authority.
    pub fn replay_eligible_identity(&self) -> Option<&str> {
        (self.replay == SourceReplayEligibility::Eligible).then_some(self.identity.as_str())
    }

    pub(crate) fn issued_original_execution(
        &self,
    ) -> Arc<crate::declaration_context::ExactDeclarationContext> {
        self.original_execution.clone()
    }

    /// Versioned observation of original source continuity. This string alone
    /// is not executable authority and cannot reconstruct the private proof.
    pub fn original_input_identity(&self) -> &str {
        &self.identity
    }

    /// Original interface custody is available only for the complete output
    /// bundle sealed by this compiler transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn original_interface_context(
        &self,
        prepared: &PreparedProgram,
        groups: &[PendingCertifiedGroup],
        target_owners: &[PendingImportOwner],
        package_interfaces: &CertifiedTargetPackageInterfaces,
        table: &DataConTable,
        yield_sites: &[YieldSite],
    ) -> Result<Arc<crate::declaration_context::ExactDeclarationContext>, CompileError> {
        if !self.matches_bundle(
            prepared,
            groups,
            target_owners,
            package_interfaces,
            table,
            yield_sites,
        ) {
            return Err(CompileError::ExtractFailed(
                "original interface context belongs to another compiler output bundle".into(),
            ));
        }
        Ok(self.original_interfaces.clone())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn original_execution_context(
        &self,
        prepared: &PreparedProgram,
        groups: &[PendingCertifiedGroup],
        target_owners: &[PendingImportOwner],
        package_interfaces: &CertifiedTargetPackageInterfaces,
        table: &DataConTable,
        yield_sites: &[YieldSite],
    ) -> Result<Arc<crate::declaration_context::ExactDeclarationContext>, CompileError> {
        self.original_interface_context(
            prepared,
            groups,
            target_owners,
            package_interfaces,
            table,
            yield_sites,
        )?;
        Ok(self.original_execution.clone())
    }

    /// Validate the exact output once before transferring both original contexts.
    #[allow(clippy::too_many_arguments)]
    pub fn original_contexts(
        &self,
        prepared: &PreparedProgram,
        groups: &[PendingCertifiedGroup],
        target_owners: &[PendingImportOwner],
        package_interfaces: &CertifiedTargetPackageInterfaces,
        table: &DataConTable,
        yield_sites: &[YieldSite],
    ) -> Result<
        (
            Arc<crate::declaration_context::ExactDeclarationContext>,
            Arc<crate::declaration_context::ExactDeclarationContext>,
        ),
        CompileError,
    > {
        let interfaces = self.original_interface_context(
            prepared,
            groups,
            target_owners,
            package_interfaces,
            table,
            yield_sites,
        )?;
        Ok((interfaces, self.original_execution.clone()))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn published_source_original_selection(
        &self,
        prepared: &PreparedProgram,
        groups: &[PendingCertifiedGroup],
        target_owners: &[PendingImportOwner],
        package_interfaces: &CertifiedTargetPackageInterfaces,
        table: &DataConTable,
        yield_sites: &[YieldSite],
        revision: &str,
        public_root: &crate::declaration_join::ExactModuleIdentity,
    ) -> Result<Arc<crate::declaration_context::PublishedSourceOriginalSelection>, CompileError>
    {
        self.original_execution_context(
            prepared,
            groups,
            target_owners,
            package_interfaces,
            table,
            yield_sites,
        )?
        .issue_published_source_original(revision, &self.identity, public_root)
    }

    pub fn matches_bundle(
        &self,
        prepared: &PreparedProgram,
        groups: &[PendingCertifiedGroup],
        target_owners: &[PendingImportOwner],
        package_interfaces: &CertifiedTargetPackageInterfaces,
        table: &DataConTable,
        yield_sites: &[YieldSite],
    ) -> bool {
        (std::ptr::eq(self.target.as_ref(), prepared) || self.target.as_ref() == prepared)
            && self.groups.as_ref() == groups
            && self.target_owners.as_ref() == target_owners
            && self
                .package_interfaces
                .matches_bundle(package_interfaces, prepared)
            && &self.table == table
            && same_sites(&self.sites, yield_sites)
    }
}

fn same_sites(left: &[YieldSite], right: &[YieldSite]) -> bool {
    crate::artifacts::yield_sites_metadata_digest(left)
        .ok()
        .zip(crate::artifacts::yield_sites_metadata_digest(right).ok())
        .is_some_and(|(left, right)| left == right)
}

/// Failure to authenticate the compiler's complete consumed package inputs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompileInputError {
    #[error("compiler input proof unavailable: {}", path.display())]
    Unavailable { path: PathBuf },
    #[error("malformed or oversized compiler input proof")]
    Malformed,
    #[error("unsupported compiler input proof version {found}; expected 1")]
    UnsupportedVersion { found: u64 },
    #[error("compiler input proof belongs to different dependency evidence")]
    DependencyMismatch,
    #[error("compiler input proof lacks exact checked module coverage")]
    OwnerCoverage,
    #[error("compiler input proof source differs for {unit}:{module}")]
    SourceMismatch { unit: String, module: String },
    #[error("compiler input proof does not support SOURCE boot owner {unit}:{module}")]
    UnsupportedBoot { unit: String, module: String },
    #[error("compiler input proof does not support compiler-provided direct import {imported_unit}:{imported_module} of {unit}:{module}")]
    UnsupportedWiredInput {
        unit: String,
        module: String,
        imported_unit: String,
        imported_module: String,
    },
    #[error("compiler input proof omits package import {imported} of {unit}:{module}")]
    MissingPackageImport {
        unit: String,
        module: String,
        imported: String,
    },
    #[error("compiler input proof has conflicting package selection for {unit}:{module}")]
    PackageConflict { unit: String, module: String },
    #[error("compiler input closure omits a checked package root")]
    IncompleteClosure,
    #[error("compiler input package interface changed: {}", path.display())]
    InterfaceChanged { path: PathBuf },
}

#[derive(Debug)]
pub(crate) struct ValidatedInputPackages {
    interfaces: BTreeMap<(String, String), PackageInterfaceWitness>,
    direct: Vec<((String, String), Vec<(String, String)>)>,
}

impl ValidatedInputPackages {
    #[cfg(test)]
    pub(crate) fn fixture_observations(
        &self,
    ) -> (
        Vec<(String, String)>,
        Vec<((String, String), Vec<(String, String)>)>,
    ) {
        (
            self.interfaces.keys().cloned().collect(),
            self.direct.clone(),
        )
    }

    pub(crate) fn read_supported(
        path: &Path,
        evidence_bytes: &[u8],
        evidence: &DependencyEvidence,
    ) -> Result<Option<Self>, CompileInputError> {
        match Self::read(path, evidence_bytes, evidence) {
            Ok(proof) => Ok(Some(proof)),
            Err(
                failure @ (CompileInputError::UnsupportedBoot { .. }
                | CompileInputError::UnsupportedWiredInput { .. }),
            ) => {
                tracing::debug!(
                    ?failure,
                    "stable compiler input proof unavailable for unsupported input category"
                );
                Ok(None)
            }
            Err(failure) => Err(failure),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(interfaces: BTreeMap<(String, String), PackageInterfaceWitness>) -> Self {
        Self {
            interfaces,
            direct: Vec::new(),
        }
    }

    pub(crate) fn read(
        path: &Path,
        evidence_bytes: &[u8],
        evidence: &DependencyEvidence,
    ) -> Result<Self, CompileInputError> {
        Self::decode(&Self::read_bytes(path)?, evidence_bytes, evidence)
    }

    fn read_bytes(path: &Path) -> Result<Vec<u8>, CompileInputError> {
        use std::io::Read;
        let file = std::fs::File::open(path)
            .map_err(|_| CompileInputError::Unavailable { path: path.into() })?;
        let mut bytes = Vec::new();
        file.take((4 << 20) + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| CompileInputError::Unavailable { path: path.into() })?;
        Ok(bytes)
    }

    #[cfg(test)]
    pub(crate) fn fixture_read_observed(
        path: &Path,
        evidence_bytes: &[u8],
        evidence: &DependencyEvidence,
    ) -> Result<(Result<Self, CompileInputError>, [u8; 32]), CompileInputError> {
        let bytes = Self::read_bytes(path)?;
        let mut body_sha256 = None;
        let result = Self::decode_observed(&bytes, evidence_bytes, evidence, |body| {
            let mut canonical = vec![];
            ciborium::ser::into_writer(body, &mut canonical).expect("closed input body encoding");
            body_sha256 = Some(Sha256::digest(&canonical).into());
        });
        match body_sha256 {
            Some(digest) => Ok((result, digest)),
            None => Err(result.err().unwrap_or(CompileInputError::Malformed)),
        }
    }

    fn decode(
        bytes: &[u8],
        evidence_bytes: &[u8],
        evidence: &DependencyEvidence,
    ) -> Result<Self, CompileInputError> {
        Self::decode_observed(bytes, evidence_bytes, evidence, |_| {})
    }

    fn decode_observed(
        bytes: &[u8],
        evidence_bytes: &[u8],
        evidence: &DependencyEvidence,
        observe_body: impl FnOnce(&ciborium::value::Value),
    ) -> Result<Self, CompileInputError> {
        use ciborium::value::Value;
        if bytes.len() > 4 << 20 {
            return Err(CompileInputError::Malformed);
        }
        let mut cursor = std::io::Cursor::new(bytes);
        let value: Value = ciborium::de::from_reader_with_recursion_limit(&mut cursor, 16)
            .map_err(|_| CompileInputError::Malformed)?;
        if cursor.position() != bytes.len() as u64 {
            return Err(CompileInputError::Malformed);
        }
        let header = proof_row(&value, 4)?;
        if proof_string(&header[0])? != "TPCINPUT" {
            return Err(CompileInputError::Malformed);
        }
        let version: u64 = header[1]
            .as_integer()
            .and_then(|n| n.try_into().ok())
            .ok_or(CompileInputError::Malformed)?;
        if version != 1 {
            return Err(CompileInputError::UnsupportedVersion { found: version });
        }
        if proof_hash(&header[2])? != <[u8; 32]>::from(Sha256::digest(evidence_bytes)) {
            return Err(CompileInputError::DependencyMismatch);
        }
        let body = header[3].as_array().ok_or(CompileInputError::Malformed)?;
        observe_body(&header[3]);
        if body.first().and_then(Value::as_text) == Some("unsupported-wired") {
            let row = proof_row(&header[3], 4)?;
            let owner = proof_owner(&row[1])?;
            let checked: Vec<_> = evidence
                .modules
                .iter()
                .filter(|module| {
                    (module.unit.as_str(), module.module.as_str())
                        == (owner.0.as_str(), owner.1.as_str())
                        && !module.boot
                })
                .collect();
            if checked.len() != 1 {
                return Err(CompileInputError::OwnerCoverage);
            }
            let source = evidence
                .sources
                .iter()
                .filter(|source| source.path == checked[0].source)
                .collect::<Vec<_>>();
            if source.len() != 1 || source[0].sha256 != proof_string(&row[2])? {
                return Err(CompileInputError::SourceMismatch {
                    unit: owner.0,
                    module: owner.1,
                });
            }
            let provided = crate::recovery_artifacts::decode_compiler_provided_imports(&row[3])
                .map_err(|_| CompileInputError::Malformed)?;
            if provided != [crate::recovery_artifacts::CompilerProvidedImport::Primitive] {
                return Err(CompileInputError::Malformed);
            }
            return Err(CompileInputError::UnsupportedWiredInput {
                unit: owner.0,
                module: owner.1,
                imported_unit: "ghc-prim".into(),
                imported_module: "GHC.Prim".into(),
            });
        }
        if body.first().and_then(Value::as_text) == Some("unsupported-boot") {
            let owners = proof_row(&header[3], 2)?[1]
                .as_array()
                .ok_or(CompileInputError::Malformed)?;
            if owners.is_empty() || owners.len() > 4096 {
                return Err(CompileInputError::Malformed);
            }
            let mut selected = std::collections::BTreeSet::new();
            for owner in owners {
                if !selected.insert(proof_owner(owner)?) {
                    return Err(CompileInputError::OwnerCoverage);
                }
            }
            let expected: std::collections::BTreeSet<_> = evidence
                .modules
                .iter()
                .filter(|module| module.boot)
                .map(|module| (module.unit.clone(), module.module.clone()))
                .collect();
            if selected != expected {
                return Err(CompileInputError::OwnerCoverage);
            }
            let owner = selected
                .into_iter()
                .next()
                .ok_or(CompileInputError::Malformed)?;
            return Err(CompileInputError::UnsupportedBoot {
                unit: owner.0,
                module: owner.1,
            });
        }
        let body = proof_row(&header[3], 3)?;
        if proof_string(&body[0])? != "checked" {
            return Err(CompileInputError::Malformed);
        }
        let rows = body[1].as_array().ok_or(CompileInputError::Malformed)?;
        if rows.len() > 4096 {
            return Err(CompileInputError::Malformed);
        }
        let sources: BTreeMap<_, _> = evidence
            .sources
            .iter()
            .map(|source| (&source.path, &source.sha256))
            .collect();
        let mut expected = BTreeMap::new();
        for module in &evidence.modules {
            if module.boot {
                return Err(CompileInputError::OwnerCoverage);
            }
            if expected
                .insert((module.unit.clone(), module.module.clone()), module)
                .is_some()
            {
                return Err(CompileInputError::OwnerCoverage);
            }
        }
        let interfaces = proof_packages(&body[2])?;
        let mut direct = BTreeMap::new();
        for row in rows {
            let row = proof_row(row, 3)?;
            let owner = proof_owner(&row[0])?;
            let module = expected
                .get(&owner)
                .ok_or(CompileInputError::OwnerCoverage)?;
            if sources.get(&module.source).copied().map(String::as_str)
                != Some(proof_string(&row[1])?)
            {
                return Err(CompileInputError::SourceMismatch {
                    unit: owner.0,
                    module: owner.1,
                });
            }
            let roots = proof_packages(&row[2])?;
            for (package, witness) in &roots {
                if interfaces.get(package) != Some(witness) {
                    return Err(CompileInputError::IncompleteClosure);
                }
            }
            for imported in module
                .imports
                .iter()
                .filter(|imported| imported.selected.is_none())
            {
                let matches = roots
                    .keys()
                    .any(|(unit, name)| package_import_matches(imported, unit, name));
                if !matches {
                    return Err(CompileInputError::MissingPackageImport {
                        unit: owner.0.clone(),
                        module: owner.1.clone(),
                        imported: imported.module.clone(),
                    });
                }
            }
            if direct
                .insert(owner, roots.into_keys().collect::<Vec<_>>())
                .is_some()
            {
                return Err(CompileInputError::OwnerCoverage);
            }
        }
        if direct.keys().ne(expected.keys()) {
            return Err(CompileInputError::OwnerCoverage);
        }
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        for witness in interfaces.values() {
            validation
                .verify(&witness.selected_path, &witness.sha256)
                .map_err(|_| CompileInputError::InterfaceChanged {
                    path: witness.selected_path.clone(),
                })?;
        }
        Ok(Self {
            interfaces,
            direct: direct.into_iter().collect(),
        })
    }
}

fn package_import_matches(import: &ModuleImportEvidence, unit: &str, module: &str) -> bool {
    module == import.module
        && match &import.qualifier {
            crate::cache::ImportQualifier::Unqualified => true,
            crate::cache::ImportQualifier::ThisUnit(required)
            | crate::cache::ImportQualifier::OtherUnit(required) => unit == required,
        }
}

fn proof_row(
    value: &ciborium::value::Value,
    len: usize,
) -> Result<&[ciborium::value::Value], CompileInputError> {
    value
        .as_array()
        .filter(|row| row.len() == len)
        .map(Vec::as_slice)
        .ok_or(CompileInputError::Malformed)
}
fn proof_string(value: &ciborium::value::Value) -> Result<&str, CompileInputError> {
    value
        .as_text()
        .filter(|s| !s.is_empty())
        .ok_or(CompileInputError::Malformed)
}
fn proof_owner(value: &ciborium::value::Value) -> Result<(String, String), CompileInputError> {
    let row = proof_row(value, 2)?;
    Ok((proof_string(&row[0])?.into(), proof_string(&row[1])?.into()))
}
fn proof_hash(value: &ciborium::value::Value) -> Result<[u8; 32], CompileInputError> {
    let text = proof_string(value)?;
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(CompileInputError::Malformed);
    }
    let mut hash = [0; 32];
    for (i, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
            .map_err(|_| CompileInputError::Malformed)?;
    }
    Ok(hash)
}
fn proof_packages(
    value: &ciborium::value::Value,
) -> Result<BTreeMap<(String, String), PackageInterfaceWitness>, CompileInputError> {
    let rows = value
        .as_array()
        .filter(|rows| rows.len() <= 16384)
        .ok_or(CompileInputError::Malformed)?;
    let mut packages = BTreeMap::new();
    for row in rows {
        let row = proof_row(row, 4)?;
        let owner = (
            proof_string(&row[0])?.to_owned(),
            proof_string(&row[1])?.to_owned(),
        );
        let path = PathBuf::from(proof_string(&row[2])?);
        if !path.is_absolute() {
            return Err(CompileInputError::Malformed);
        }
        let witness = PackageInterfaceWitness {
            selected_path: path,
            sha256: proof_hash(&row[3])?,
        };
        if packages.insert(owner.clone(), witness).is_some() {
            return Err(CompileInputError::PackageConflict {
                unit: owner.0,
                module: owner.1,
            });
        }
    }
    Ok(packages)
}

#[derive(Serialize)]
struct InputRecipe<'a> {
    version: &'static str,
    producer: &'a [u8],
    source: &'a str,
    target: &'a str,
    include: &'a [PathBuf],
    sources: Vec<&'a crate::cache::SourceEvidence>,
    modules: Vec<ModuleInput<'a>>,
    resolutions: Vec<&'a ResolutionEvidence>,
    packages: Vec<&'a str>,
    package_roots: &'a [((String, String), Vec<(String, String)>)],
    package_interfaces: Vec<(&'a (String, String), &'a Path, [u8; 32])>,
}

#[derive(Serialize)]
struct ModuleInput<'a> {
    unit: &'a str,
    module: &'a str,
    boot: bool,
    source: &'a Path,
    imports: Vec<&'a ModuleImportEvidence>,
}

fn canonical_rows<T: Serialize>(mut rows: Vec<T>) -> Vec<T> {
    // These closed evidence types serialize without fallible values.
    rows.sort_by_cached_key(|row| serde_json::to_vec(row).expect("closed input evidence"));
    rows
}

fn input_identity(
    producer: &[u8],
    include: &[PathBuf],
    evidence: &DependencyEvidence,
    packages: &ValidatedInputPackages,
    source: &str,
    target: &str,
) -> Result<String, CompileError> {
    let mut package_units: Vec<_> = evidence.packages.iter().map(String::as_str).collect();
    package_units.sort_unstable();
    package_units.dedup();
    // Map tuple keys cannot be serialized as a JSON object. Preserve the full
    // authenticated closure, including instance/family-only package imports.
    let recipe = InputRecipe {
        version: "tidepool-compile-input-v2",
        producer,
        source,
        target,
        include,
        sources: canonical_rows(evidence.sources.iter().collect()),
        modules: canonical_rows(
            evidence
                .modules
                .iter()
                .map(|module| ModuleInput {
                    unit: &module.unit,
                    module: &module.module,
                    boot: module.boot,
                    source: &module.source,
                    imports: canonical_rows(module.imports.iter().collect()),
                })
                .collect(),
        ),
        resolutions: canonical_rows(evidence.resolutions.iter().collect()),
        packages: package_units,
        package_roots: &packages.direct,
        package_interfaces: packages
            .interfaces
            .iter()
            .map(|(owner, witness)| (owner, witness.selected_path.as_path(), witness.sha256))
            .collect(),
    };
    let bytes = serde_json::to_vec(&recipe)
        .map_err(|error| CompileError::ExtractFailed(format!("compile input recipe: {error}")))?;
    Ok(format!(
        "tidepool-compile-input-v2:{:x}",
        Sha256::digest(bytes)
    ))
}

fn original_source_lexical(
    evidence: &DependencyEvidence,
    artifacts: &crate::artifact_inventory::ArtifactView,
) -> Result<Vec<crate::declaration_join::ExactLexicalNode>, CompileError> {
    let imports =
        crate::declaration_context::consumed_source_home_imports(evidence, &BTreeMap::new())?;
    // Presence does not select an owner. These roots were actually compiled
    // under this original input proof and retain its exact source adjacency.
    let roots = artifacts
        .descriptors()
        .into_iter()
        .map(|descriptor| descriptor.owner)
        .filter(|owner| imports.contains_key(owner))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    Ok(crate::declaration_join::source_lexical_surface(
        &roots,
        &imports,
        &[],
        &artifacts.source_implementation_roles(),
    )?
    .lexical)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn seal(
    producer: &[u8],
    include: &[PathBuf],
    evidence: &DependencyEvidence,
    packages: &ValidatedInputPackages,
    source: &str,
    target: &str,
    prepared: &Arc<PreparedProgram>,
    groups: &Arc<[PendingCertifiedGroup]>,
    target_owners: &[PendingImportOwner],
    package_interfaces: &CertifiedTargetPackageInterfaces,
    table: DataConTable,
    sites: Vec<YieldSite>,
    artifacts: &crate::artifact_inventory::ArtifactView,
    compiler_projection: &crate::artifact_inventory::CompilerInputProjection,
) -> Result<Option<SealedOriginalCompileInput>, CompileError> {
    // Original completed source output can retain compile-time execution.
    // Mutable resident Val/Lib interfaces belong to their exact context owner.
    if target_owners
        .iter()
        .chain(groups.iter().flat_map(PendingCertifiedGroup::imports))
        .any(|owner| {
            matches!(
                owner,
                PendingImportOwner::Retained { .. } | PendingImportOwner::RetainedPackage { .. }
            )
        })
        || evidence.modules.iter().any(|module| {
            module.module.starts_with("Tidepool.Session.")
                || module
                    .imports
                    .iter()
                    .any(|import| import.module.starts_with("Tidepool.Session."))
        })
    {
        return Ok(None);
    }
    let completed =
        crate::cache::CompletedSourceEvidence::from_normalized(evidence.clone(), source);
    if producer.is_empty() || completed.is_err() || !package_interfaces.matches_target(prepared) {
        return Err(CompileError::ExtractFailed(
            "compile input identity lacks validated input or output ownership".into(),
        ));
    }
    crate::artifacts::YieldSites::from_sites(sites.clone())
        .map_err(|error| CompileError::Asks(error.to_string()))?;
    let include = include
        .iter()
        .map(std::path::absolute)
        .collect::<Result<Vec<_>, _>>()?;
    let original_targets = evidence
        .modules
        .iter()
        .filter(|module| !module.boot && module.source == Path::new(crate::cache::GENERATED_SOURCE))
        .map(|module| crate::declaration_join::ExactModuleIdentity {
            unit: module.unit.clone(),
            module: module.module.clone(),
        })
        .collect::<Vec<_>>();
    let [original_target] = original_targets.as_slice() else {
        return Err(CompileError::ExtractFailed(
            "original input lacks one authenticated generated target owner".into(),
        ));
    };
    let lexical = original_source_lexical(evidence, artifacts)?;
    let required_instance_owners = evidence
        .modules
        .iter()
        .filter(|module| !module.boot)
        .map(|module| crate::declaration_join::ExactModuleIdentity {
            unit: module.unit.clone(),
            module: module.module.clone(),
        })
        .collect::<Vec<_>>();
    Ok(Some(SealedOriginalCompileInput {
        identity: input_identity(producer, &include, evidence, packages, source, target)?,
        source: source.into(),
        original_interfaces: Arc::new(
            crate::declaration_context::ExactDeclarationContext::from_authenticated_interfaces(
                crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
                    .sha256(),
                artifacts,
                compiler_projection.clone(),
            )?,
        ),
        original_execution: Arc::new(
            crate::declaration_context::ExactDeclarationContext::from_authenticated_execution(
                crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
                    .sha256(),
                artifacts,
                compiler_projection.clone(),
                lexical,
                original_target.clone(),
                &required_instance_owners,
            )?,
        ),
        target: prepared.clone(),
        groups: groups.clone(),
        target_owners: target_owners.to_vec().into(),
        package_interfaces: package_interfaces.clone(),
        table,
        sites: sites.into(),
        replay: SourceReplayEligibility::from_evidence(evidence),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{ImportQualifier, ModuleEvidence, ProductAvailability, SourceEvidence};

    fn identity(
        producer: &[u8],
        include: &[PathBuf],
        evidence: &DependencyEvidence,
        packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
        source: &str,
        target: &str,
    ) -> Result<String, CompileError> {
        input_identity(
            producer,
            include,
            evidence,
            &ValidatedInputPackages {
                interfaces: packages.clone(),
                direct: Vec::new(),
            },
            source,
            target,
        )
    }

    fn package_context_proof(producer: &[u8]) -> SealedOriginalCompileInput {
        package_context_proof_with_eligibility(producer, true, true)
    }

    fn package_context_proof_with_eligibility(
        producer: &[u8],
        cache_safe: bool,
        selection_complete: bool,
    ) -> SealedOriginalCompileInput {
        use tidepool_repr::execution_schema::testing;
        let source = "module Root where root = 42";
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe,
            selection_complete,
            sources: vec![SourceEvidence {
                path: "@generated-source".into(),
                sha256: format!("{:x}", Sha256::digest(source)),
            }],
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "Root".into(),
                boot: false,
                source: "@generated-source".into(),
                product: ProductAvailability::Ready,
                imports: vec![],
            }],
            resolutions: vec![],
            packages: vec![],
        };
        let target = Arc::new(testing::prepare(testing::wire_program()).unwrap());
        let package_interfaces =
            crate::certified_products::certify_target_package_interfaces(&target, &BTreeMap::new())
                .unwrap();
        seal(
            producer,
            &[],
            &evidence,
            &ValidatedInputPackages::fixture(BTreeMap::new()),
            source,
            "root",
            &target,
            &Arc::from([]),
            &[],
            &package_interfaces,
            DataConTable::new(),
            vec![],
            &crate::artifact_inventory::ArtifactInventory::default().empty_view(),
            &crate::artifact_inventory::CompilerInputProjection::default(),
        )
        .unwrap()
        .expect("ordinary compiler issuer admits complete fixture inputs")
    }

    fn original_context(
        proof: &SealedOriginalCompileInput,
    ) -> Arc<crate::declaration_context::ExactDeclarationContext> {
        proof
            .original_interface_context(
                &proof.target,
                &proof.groups,
                &proof.target_owners,
                &proof.package_interfaces,
                &proof.table,
                &proof.sites,
            )
            .unwrap()
    }

    #[test]
    fn completed_original_custody_preserves_each_source_replay_refusal() {
        for (cache_safe, selection_complete, eligibility) in [
            (true, true, SourceReplayEligibility::Eligible),
            (false, true, SourceReplayEligibility::UntrackedInputs),
            (true, false, SourceReplayEligibility::IncompleteSelection),
            (
                false,
                false,
                SourceReplayEligibility::UntrackedInputsAndIncompleteSelection,
            ),
        ] {
            let proof = package_context_proof_with_eligibility(
                b"original compiler producer",
                cache_safe,
                selection_complete,
            );
            assert_eq!(proof.source_replay_eligibility(), eligibility);
            assert_eq!(
                proof.replay_eligible_identity().is_some(),
                cache_safe && selection_complete
            );
            original_context(&proof);
            assert!(proof.matches_bundle(
                &proof.target,
                &proof.groups,
                &proof.target_owners,
                &proof.package_interfaces,
                &proof.table,
                &proof.sites,
            ));
        }
    }

    #[test]
    fn sealed_input_missing_generated_owner_cannot_prove_absent_instances() {
        let proof = package_context_proof(b"original compiler producer");
        let execution = proof
            .original_execution_context(
                &proof.target,
                &proof.groups,
                &proof.target_owners,
                &proof.package_interfaces,
                &proof.table,
                &proof.sites,
            )
            .unwrap();
        assert_eq!(
            execution.original_instance_environment(),
            &crate::declaration_context::OriginalInstanceEnvironment::MissingOriginalOwners(vec![
                crate::declaration_join::ExactModuleIdentity {
                    unit: "main".into(),
                    module: "Root".into()
                },
            ]),
            "a consumed generated owner without its original interface is missing evidence",
        );
        let projected = execution.select_interface_roots(Vec::new()).unwrap();
        assert_eq!(
            projected.original_instance_environment(),
            &crate::declaration_context::OriginalInstanceEnvironment::Unknown
        );
        assert_ne!(
            execution.semantic_sha256(),
            projected.semantic_sha256(),
            "original scope completeness is part of the authenticated context commitment"
        );
        assert_eq!(
            original_context(&proof).original_instance_environment(),
            &crate::declaration_context::OriginalInstanceEnvironment::Unknown,
            "type-only original interface custody cannot prove instance absence"
        );
    }

    #[test]
    fn paired_original_contexts_preserve_issuer_and_individual_observations() {
        for producer in [
            b"first compiler producer".as_slice(),
            b"second compiler producer".as_slice(),
        ] {
            let proof = package_context_proof(producer);
            let (interfaces, execution) = proof
                .original_contexts(
                    &proof.target,
                    &proof.groups,
                    &proof.target_owners,
                    &proof.package_interfaces,
                    &proof.table,
                    &proof.sites,
                )
                .unwrap();
            assert_eq!(interfaces, original_context(&proof));
            assert_eq!(
                execution,
                proof
                    .original_execution_context(
                        &proof.target,
                        &proof.groups,
                        &proof.target_owners,
                        &proof.package_interfaces,
                        &proof.table,
                        &proof.sites
                    )
                    .unwrap()
            );
            assert_ne!(interfaces.semantic_sha256(), execution.semantic_sha256());
        }
    }

    #[test]
    fn sealed_package_context_preserves_original_producer_after_empty_projection() {
        let proof = package_context_proof(b"original compiler producer");
        let original = original_context(&proof);
        let selected = original.select_interface_roots(Vec::new()).unwrap();
        assert!(selected.artifact_view().descriptors().is_empty());
        assert_eq!(selected, *original);
        let expected = crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
            b"original compiler producer",
        )
        .sha256();
        assert_eq!(selected.toolchain_identity_sha256(), expected);
        let empty =
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], Vec::new()).unwrap();
        let merged = empty.clone().extend_interface_context(&selected).unwrap();
        assert_eq!(merged, selected);
        assert_ne!(merged.semantic_sha256(), empty.semantic_sha256());
        let directory = tempfile::tempdir().unwrap();
        Arc::new(merged)
            .prepare_compilation(directory.path(), b"original compiler producer")
            .unwrap();
    }

    #[test]
    fn sealed_package_context_refuses_different_producer_and_edited_output() {
        let first_proof = package_context_proof(b"first compiler producer");
        let second_proof = package_context_proof(b"second compiler producer");
        let first = original_context(&first_proof)
            .select_interface_roots(Vec::new())
            .unwrap();
        let second = original_context(&second_proof)
            .select_interface_roots(Vec::new())
            .unwrap();
        assert_ne!(first, second);
        assert_ne!(first.semantic_sha256(), second.semantic_sha256());
        assert!(first.clone().extend_interface_context(&second).is_err());
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("refused");
        assert!(Arc::new(first)
            .prepare_compilation(&output, b"second compiler producer")
            .is_err());
        assert!(
            !output.exists(),
            "producer refusal precedes interface materialization"
        );
        let mut changed = tidepool_repr::execution_schema::testing::wire_program();
        changed
            .globals
            .push(tidepool_repr::execution_schema::GlobalDecl {
                identity: tidepool_repr::execution_schema::testing::identity("Other", "value"),
                rep: tidepool_repr::execution_schema::RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            });
        let changed = tidepool_repr::execution_schema::testing::prepare(changed).unwrap();
        assert!(first_proof
            .original_contexts(
                &changed,
                &first_proof.groups,
                &first_proof.target_owners,
                &first_proof.package_interfaces,
                &first_proof.table,
                &first_proof.sites,
            )
            .is_err());
        let mut edited_table = DataConTable::new();
        edited_table
            .insert_checked(tidepool_repr::DataCon {
                identity: tidepool_repr::execution_schema::SymbolIdentity {
                    unit: "fixture".into(),
                    module: "Fixture".into(),
                    namespace: "constructor".into(),
                    occurrence: "Edited".into(),
                    record_parent: None,
                },
                id: tidepool_repr::DataConId(99),
                name: "Edited".into(),
                tag: 1,
                rep_arity: 0,
                field_bangs: vec![],
                qualified_name: None,
                type_name: "Edited".into(),
            })
            .expect("valid fixture metadata");
        assert!(first_proof
            .original_contexts(
                &first_proof.target,
                &first_proof.groups,
                &first_proof.target_owners,
                &first_proof.package_interfaces,
                &edited_table,
                &first_proof.sites,
            )
            .is_err());
        let sites = vec![YieldSite {
            site: 1,
            origin: "Root".into(),
            ordinal: 0,
            ty: "Int".into(),
            modules: vec![],
            heads: vec![],
            inputs: vec![],
            input_type_witnesses: vec![],
            reply_declaration: None,
            request_type_signatures: None,
        }];
        assert!(first_proof
            .original_contexts(
                &first_proof.target,
                &first_proof.groups,
                &first_proof.target_owners,
                &first_proof.package_interfaces,
                &first_proof.table,
                &sites,
            )
            .is_err());
    }

    fn evidence() -> DependencyEvidence {
        DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![
                SourceEvidence {
                    path: "@generated-source".into(),
                    sha256: "a".repeat(64),
                },
                SourceEvidence {
                    path: "/source/Driver.hs".into(),
                    sha256: "b".repeat(64),
                },
            ],
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "Root".into(),
                boot: false,
                source: "@generated-source".into(),
                product: ProductAvailability::Ready,
                imports: vec![ModuleImportEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: "Driver".into(),
                    boot: false,
                    selected: Some("/source/Driver.hs".into()),
                }],
            }],
            resolutions: vec![ResolutionEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "Driver".into(),
                boot: false,
                selected: Some("/source/Driver.hs".into()),
                candidates: vec!["/negative/Driver.hs".into(), "/source/Driver.hs".into()],
            }],
            packages: vec!["base".into(), "text".into()],
        }
    }

    #[test]
    fn compile_input_generated_source_relocation_is_stable() {
        let source = "module Root where root = 42";
        let input_at = |root: &Path| {
            let input = root.join("Root.hs");
            std::fs::write(&input, source).unwrap();
            let evidence = DependencyEvidence {
                version: 4,
                cache_safe: true,
                selection_complete: true,
                sources: vec![SourceEvidence {
                    path: input.clone(),
                    sha256: format!("{:x}", Sha256::digest(source)),
                }],
                modules: vec![ModuleEvidence {
                    unit: "main".into(),
                    module: "Root".into(),
                    boot: false,
                    source: input.clone(),
                    imports: vec![],
                    product: ProductAvailability::Ready,
                }],
                resolutions: vec![],
                packages: vec![],
            };
            let bytes = serde_json::to_vec(&evidence).unwrap();
            DependencyEvidence::from_worker(&bytes, &input, source).unwrap()
        };
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first = input_at(first.path());
        let second = input_at(second.path());
        assert_eq!(
            identity(b"producer", &[], &first, &BTreeMap::new(), source, "root").unwrap(),
            identity(b"producer", &[], &second, &BTreeMap::new(), source, "root").unwrap()
        );
    }

    #[test]
    fn compile_input_canonical_order_preserves_source_and_resolution_identity() {
        let evidence = evidence();
        let include = vec!["/negative".into(), "/source".into()];
        let packages = BTreeMap::from([(
            ("base".into(), "Instances".into()),
            PackageInterfaceWitness {
                selected_path: "/package/Instances.hi".into(),
                sha256: [3; 32],
            },
        )]);
        let identity = |evidence: &DependencyEvidence,
                        include: &[PathBuf],
                        producer: &[u8],
                        packages: &BTreeMap<_, _>| {
            identity(
                producer,
                include,
                evidence,
                packages,
                "complete source",
                "root",
            )
            .unwrap()
        };
        let expected = identity(&evidence, &include, b"producer", &packages);
        let mut reordered = evidence.clone();
        reordered.sources.reverse();
        reordered.packages.reverse();
        reordered.modules[0].product = ProductAvailability::InterfaceOnly;
        assert_eq!(
            expected,
            identity(&reordered, &include, b"producer", &packages)
        );
        let mut changed = evidence.clone();
        changed.sources[1].sha256 = "c".repeat(64);
        assert_ne!(
            expected,
            identity(&changed, &include, b"producer", &packages)
        );
        let mut changed = evidence.clone();
        changed.resolutions[0].candidates.reverse();
        assert_ne!(
            expected,
            identity(&changed, &include, b"producer", &packages)
        );
        let mut changed_include = include.clone();
        changed_include.reverse();
        assert_ne!(
            expected,
            identity(&evidence, &changed_include, b"producer", &packages)
        );
        assert_ne!(
            expected,
            identity(&evidence, &include, b"other producer", &packages)
        );
        let mut changed_packages = packages.clone();
        changed_packages.values_mut().next().unwrap().sha256 = [4; 32];
        assert_ne!(
            expected,
            identity(&evidence, &include, b"producer", &changed_packages)
        );
    }
    fn package_packet(
        evidence: &DependencyEvidence,
        raw: &[u8],
        packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    ) -> ciborium::value::Value {
        use ciborium::value::Value;
        let text = |s: String| Value::Text(s);
        let package_rows = || {
            Value::Array(
                packages
                    .iter()
                    .map(|((unit, module), witness)| {
                        Value::Array(vec![
                            text(unit.clone()),
                            text(module.clone()),
                            text(witness.selected_path.to_string_lossy().into()),
                            text(format!(
                                "{:x}",
                                Sha256::digest(std::fs::read(&witness.selected_path).unwrap())
                            )),
                        ])
                    })
                    .collect(),
            )
        };
        let rows = evidence
            .modules
            .iter()
            .map(|module| {
                Value::Array(vec![
                    Value::Array(vec![text(module.unit.clone()), text(module.module.clone())]),
                    text(
                        evidence
                            .sources
                            .iter()
                            .find(|source| source.path == module.source)
                            .unwrap()
                            .sha256
                            .clone(),
                    ),
                    Value::Array(
                        package_rows()
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|row| row.as_array().unwrap()[1].as_text() == Some("Facade"))
                            .cloned()
                            .collect(),
                    ),
                ])
            })
            .collect();
        Value::Array(vec![
            text("TPCINPUT".into()),
            Value::Integer(1.into()),
            text(format!("{:x}", Sha256::digest(raw))),
            Value::Array(vec![
                text("checked".into()),
                Value::Array(rows),
                package_rows(),
            ]),
        ])
    }
    fn packet_bytes(value: &ciborium::value::Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).unwrap();
        bytes
    }
    fn packet_fixture(
        root: &Path,
    ) -> (
        DependencyEvidence,
        BTreeMap<(String, String), PackageInterfaceWitness>,
    ) {
        let mut evidence = evidence();
        evidence.modules[0].imports.clear();
        evidence.resolutions.clear();
        evidence.modules[0].imports.push(ModuleImportEvidence {
            qualifier: ImportQualifier::OtherUnit("package".into()),
            module: "Facade".into(),
            boot: false,
            selected: None,
        });
        let packages = ["Facade", "Orphan", "Family"]
            .into_iter()
            .map(|module| {
                let path = root.join(format!("{module}.hi"));
                std::fs::write(&path, module).unwrap();
                (
                    ("package".into(), module.into()),
                    PackageInterfaceWitness {
                        selected_path: path,
                        sha256: Sha256::digest(module).into(),
                    },
                )
            })
            .collect();
        (evidence, packages)
    }
    #[test]
    fn compile_input_packet_binds_output_but_ignores_optional_product_availability() {
        let root = tempfile::tempdir().unwrap();
        let (cold, packages) = packet_fixture(root.path());
        let mut warm = cold.clone();
        warm.modules[0].product = ProductAvailability::InterfaceOnly;
        let cold_raw = serde_json::to_vec(&cold).unwrap();
        let warm_raw = serde_json::to_vec(&warm).unwrap();
        let cold_packet = packet_bytes(&package_packet(&cold, &cold_raw, &packages));
        let warm_packet = packet_bytes(&package_packet(&warm, &warm_raw, &packages));
        assert_ne!(cold_packet, warm_packet);
        let cold_proof = ValidatedInputPackages::decode(&cold_packet, &cold_raw, &cold).unwrap();
        let warm_proof = ValidatedInputPackages::decode(&warm_packet, &warm_raw, &warm).unwrap();
        assert_eq!(
            input_identity(b"producer", &[], &cold, &cold_proof, "same source", "root").unwrap(),
            input_identity(b"producer", &[], &warm, &warm_proof, "same source", "root").unwrap()
        );
        assert!(matches!(
            ValidatedInputPackages::decode(&cold_packet, &warm_raw, &warm),
            Err(CompileInputError::DependencyMismatch)
        ));
        let mut changed = warm.clone();
        changed.modules[0].imports[0].qualifier =
            ImportQualifier::OtherUnit("other-package".into());
        assert_ne!(
            input_identity(b"producer", &[], &cold, &cold_proof, "same source", "root").unwrap(),
            input_identity(
                b"producer",
                &[],
                &changed,
                &warm_proof,
                "same source",
                "root"
            )
            .unwrap()
        );
    }
    #[test]
    fn compile_input_packet_refuses_missing_owners_roots_and_versions() {
        use ciborium::value::Value;
        let root = tempfile::tempdir().unwrap();
        let (evidence, packages) = packet_fixture(root.path());
        let raw = serde_json::to_vec(&evidence).unwrap();
        let value = package_packet(&evidence, &raw, &packages);
        let mut missing = value.clone();
        missing.as_array_mut().unwrap()[3].as_array_mut().unwrap()[1] = Value::Array(vec![]);
        assert!(matches!(
            ValidatedInputPackages::decode(&packet_bytes(&missing), &raw, &evidence),
            Err(CompileInputError::OwnerCoverage)
        ));
        let mut missing = value.clone();
        missing.as_array_mut().unwrap()[3].as_array_mut().unwrap()[1]
            .as_array_mut()
            .unwrap()[0]
            .as_array_mut()
            .unwrap()[2] = Value::Array(vec![]);
        assert!(matches!(
            ValidatedInputPackages::decode(&packet_bytes(&missing), &raw, &evidence),
            Err(CompileInputError::MissingPackageImport { .. })
        ));
        let mut missing = value.clone();
        missing.as_array_mut().unwrap()[3].as_array_mut().unwrap()[2] = Value::Array(vec![]);
        assert!(matches!(
            ValidatedInputPackages::decode(&packet_bytes(&missing), &raw, &evidence),
            Err(CompileInputError::IncompleteClosure)
        ));
        let mut version = value.clone();
        version.as_array_mut().unwrap()[1] = Value::Integer(2.into());
        assert!(matches!(
            ValidatedInputPackages::decode(&packet_bytes(&version), &raw, &evidence),
            Err(CompileInputError::UnsupportedVersion { found: 2 })
        ));
        assert!(matches!(
            ValidatedInputPackages::decode(b"not cbor", &raw, &evidence),
            Err(CompileInputError::Malformed)
        ));
        assert!(matches!(
            ValidatedInputPackages::read(&root.path().join("missing"), &raw, &evidence),
            Err(CompileInputError::Unavailable { .. })
        ));
        let mut source = value.clone();
        source.as_array_mut().unwrap()[3].as_array_mut().unwrap()[1]
            .as_array_mut()
            .unwrap()[0]
            .as_array_mut()
            .unwrap()[1] = Value::Text("0".repeat(64));
        assert!(matches!(
            ValidatedInputPackages::decode(&packet_bytes(&source), &raw, &evidence),
            Err(CompileInputError::SourceMismatch { .. })
        ));
        let mut duplicate = value.clone();
        let closure = duplicate.as_array_mut().unwrap()[3].as_array_mut().unwrap()[2]
            .as_array_mut()
            .unwrap();
        closure.push(closure[0].clone());
        assert!(matches!(
            ValidatedInputPackages::decode(&packet_bytes(&duplicate), &raw, &evidence),
            Err(CompileInputError::PackageConflict { .. })
        ));
    }

    #[test]
    fn compile_input_optional_proof_withholds_only_explicit_unsupported_categories() {
        use ciborium::value::Value;
        let root = tempfile::tempdir().unwrap();
        let (evidence, packages) = packet_fixture(root.path());
        let raw = serde_json::to_vec(&evidence).unwrap();
        let owner = Value::Array(vec![
            Value::Text(evidence.modules[0].unit.clone()),
            Value::Text(evidence.modules[0].module.clone()),
        ]);
        let mut value = package_packet(&evidence, &raw, &packages);
        value.as_array_mut().unwrap()[3] = Value::Array(vec![
            Value::Text("unsupported-wired".into()),
            owner.clone(),
            Value::Text(
                evidence
                    .sources
                    .iter()
                    .find(|source| source.path == evidence.modules[0].source)
                    .unwrap()
                    .sha256
                    .clone(),
            ),
            Value::Array(vec![Value::Array(vec![
                Value::Text("primitive".into()),
                Value::Text("ghc-prim".into()),
                Value::Text("GHC.Prim".into()),
            ])]),
        ]);
        let path = root.path().join("compiler-inputs.cbor");
        std::fs::write(&path, packet_bytes(&value)).unwrap();
        assert!(matches!(
            ValidatedInputPackages::read(&path, &raw, &evidence),
            Err(CompileInputError::UnsupportedWiredInput { .. })
        ));
        assert!(
            ValidatedInputPackages::read_supported(&path, &raw, &evidence)
                .unwrap()
                .is_none()
        );
        let mut bare = value.clone();
        bare.as_array_mut().unwrap()[3]
            .as_array_mut()
            .unwrap()
            .pop();
        std::fs::write(&path, packet_bytes(&bare)).unwrap();
        assert!(matches!(
            ValidatedInputPackages::read_supported(&path, &raw, &evidence),
            Err(CompileInputError::Malformed)
        ));
        let mut changed = value.clone();
        changed.as_array_mut().unwrap()[3].as_array_mut().unwrap()[2] = Value::Text("0".repeat(64));
        std::fs::write(&path, packet_bytes(&changed)).unwrap();
        assert!(matches!(
            ValidatedInputPackages::read_supported(&path, &raw, &evidence),
            Err(CompileInputError::SourceMismatch { .. })
        ));
        value.as_array_mut().unwrap()[3] = Value::Array(vec![
            Value::Text("unsupported-boot".into()),
            Value::Array(vec![owner]),
        ]);
        std::fs::write(&path, packet_bytes(&value)).unwrap();
        assert!(matches!(
            ValidatedInputPackages::read_supported(&path, &raw, &evidence),
            Err(CompileInputError::OwnerCoverage)
        ));
        let mut boot = evidence.clone();
        boot.modules[0].boot = true;
        let boot_raw = serde_json::to_vec(&boot).unwrap();
        value.as_array_mut().unwrap()[2] = Value::Text(format!("{:x}", Sha256::digest(&boot_raw)));
        std::fs::write(&path, packet_bytes(&value)).unwrap();
        assert!(
            ValidatedInputPackages::read_supported(&path, &boot_raw, &boot)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            ValidatedInputPackages::read_supported(&path, b"another offer", &evidence),
            Err(CompileInputError::DependencyMismatch)
        ));
        std::fs::write(&path, b"malformed").unwrap();
        assert!(matches!(
            ValidatedInputPackages::read_supported(&path, &raw, &evidence),
            Err(CompileInputError::Malformed)
        ));
    }
    #[test]
    fn compile_input_packet_checks_transitive_orphan_and_family_bytes() {
        let root = tempfile::tempdir().unwrap();
        let (evidence, packages) = packet_fixture(root.path());
        let raw = serde_json::to_vec(&evidence).unwrap();
        let bytes = packet_bytes(&package_packet(&evidence, &raw, &packages));
        for module in ["Orphan", "Family"] {
            let path = root.path().join(format!("{module}.hi"));
            std::fs::write(&path, "changed").unwrap();
            assert!(
                matches!(ValidatedInputPackages::decode(&bytes,&raw,&evidence),Err(CompileInputError::InterfaceChanged{path:p}) if p==path)
            );
            std::fs::write(&path, module).unwrap();
        }
    }
}
