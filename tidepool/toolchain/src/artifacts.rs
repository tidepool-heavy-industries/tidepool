//! The ONE policy-bearing `tidepool-extract` compile front door:
//! [`CompileInvocation`] + [`compile_invocation`]. `tidepool_runtime::compile_haskell`
//! and [`compile_targets`] are thin projections over that owner, so spawning
//! the extractor, reading its output directory, and deserializing typed
//! artifacts happen in exactly one place.
//!
//! All cacheable requests use one recipe and one named artifact bundle.
//! Compiler dependency evidence is validated on publication and on every hit.

use ciborium::value::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use crate::compile_input::{SealedOriginalCompileInput, SourceReplayEligibility};
pub use crate::declaration_context::PublishedSourceOriginalSelection;
pub use crate::turn_observations::{decode_turn_nominal_heads, decode_turn_yield_sites};
use serde::Deserialize;
use tempfile::TempDir;
use tidepool_extract_cmd::ExtractCmd;
pub use tidepool_extract_cmd::{
    with_compiler_transaction_cancellable_for_workload, with_compiler_transaction_for_workload,
    CompileWorkload,
};
use tidepool_repr::execution_schema::DecodeLimits;
use tidepool_repr::execution_schema::{PreparedProgram, RawModuleProduct};
use tidepool_repr::serial::{read_metadata, MetaWarnings};
use tidepool_repr::DataConTable;

use crate::prepared_artifact::{prepared_artifact_name, PreparedArtifact};
use crate::{
    cache, certified_products, diag, extract_module_name, extract_spawn_error, module_candidates,
    timing, CompileError,
};

mod catalog_inventory;
mod execution_diagnostics;
mod failure_sources;
mod production_entry;
pub use execution_diagnostics::{compiler_scratch_directory, CompilerDiagnosticCapture};
pub use production_entry::{
    build_production_entry, load_production_entry, load_selected_production_entry,
    load_selected_production_entry_with_catalog, prepare_frozen_production_entry,
    prepare_frozen_production_entry_with_catalog, FrozenEntrySources, ProductionEntryOutput,
    ProductionEntrySources,
};

static HOST_BINDING_INTERFACE_REQUESTS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Process-local logical interface requests, distinct from source/native
/// compiler requests. Like extract_spawn_count, this grants no authority.
pub fn host_binding_interface_request_count() -> u64 {
    HOST_BINDING_INTERFACE_REQUESTS.load(std::sync::atomic::Ordering::SeqCst)
}

/// Issue one fresh type-only binding interface from original compiler type evidence.
/// This session-specific artifact is never cached or compiled as authored source.
pub fn issue_host_binding_interface(
    prototype: Arc<crate::checked_cell::ExactHostBindingPrototype>,
    admission: [u8; 32],
    generation: u64,
    binding: &str,
    includes: &[PathBuf],
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<Arc<crate::checked_cell::ExactHostBindingInterface>, CompileError> {
    let _span = tracing::debug_span!("host_binding_interface", generation, binding).entered();
    let mut command = ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
    let bound = command
        .bind()
        .map_err(|error| CompileError::Io(extract_spawn_error(error.source)))?;
    let endpoint = crate::toolchain::AdmittedCompilerEndpoint::from_bound(bound)
        .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    let offer = prototype.prepare_interface_offer(
        endpoint.identity().producer_bytes(),
        admission,
        generation,
        binding,
    )?;
    let root = offer
        .request
        .manifest
        .parent()
        .expect("owned interface input directory")
        .parent()
        .expect("owned interface directory");
    let manifest = root.join("host-binding-interface.cbor");
    let receipt = root.join("host-binding-interface-receipt.cbor");
    std::fs::write(&manifest, &offer.encoded)?;
    command
        .input(&manifest)
        .includes(includes)
        .session_artifacts(&offer.request.manifest)
        .declaration_join(&manifest)
        .declaration_join_out(&receipt);
    crate::paths::apply_admitted_build_products_dir(&mut command, &endpoint);
    HOST_BINDING_INTERFACE_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let run = endpoint.execute_with_input_files(
        &command,
        offer.request.input_transport_files(),
        |close| settlement(close),
    )?;
    crate::diag::decode_extract_result(
        run.output.status.success(),
        &run.output.stdout,
        &run.output.stderr,
    )?;
    let bytes = std::fs::read(&receipt)?;
    if bytes.len() > 4 << 20 {
        return Err(CompileError::ExtractFailed(
            "host interface receipt exceeds byte bound".into(),
        ));
    }
    offer.seal(&bytes)
}

// ---------------------------------------------------------------------------
// Typed yield sites (`asks.json` on disk)
// ---------------------------------------------------------------------------

/// A complete authored module to typecheck through the admitted compiler.
///
/// Checking produces no prepared program, native product, or input-continuity
/// authority. The caller owns capture freshness and publication policy.
pub struct SourceCheckRequest<'a> {
    pub source: &'a str,
    pub include: &'a [PathBuf],
    pub fallback_module_name: &'a str,
}

/// Typecheck the supplied complete module against its admitted source graph.
///
/// This operation emits no prepared or native artifacts and grants no authority
/// to publish a mutable source graph. Diagnostics retain the selected inputs.
pub fn check_source(
    request: &SourceCheckRequest<'_>,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<(), CompileError> {
    let directory = compiler_scratch_directory()?;
    let module = extract_module_name(request.source)
        .unwrap_or_else(|| request.fallback_module_name.to_owned());
    let input = directory.path().join(format!("{module}.hs"));
    std::fs::write(&input, request.source)?;
    let mut command = ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
    command
        .input(&input)
        .check_source()
        .includes(request.include);
    let bound = command
        .bind()
        .map_err(|error| CompileError::Io(extract_spawn_error(error.source)))?;
    let endpoint = crate::toolchain::AdmittedCompilerEndpoint::from_bound(bound)
        .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    crate::paths::apply_admitted_build_products_dir(&mut command, &endpoint);
    let offer =
        ModuleCandidateOffer::select_admitted(&endpoint, request.include, directory.path())?;
    if let Some(manifest) = offer.manifest_path() {
        command.module_candidates(manifest);
    }
    let diagnostics = CompilerDiagnosticCapture::start(directory.path(), &command);
    let run = endpoint
        .execute_with_input_files(&command, offer.input_transport_files(), |close| {
            settlement(close)
        })
        .map_err(|error| offer.retain_execution_failure(directory.path(), &command, error))?;
    diagnostics.completed(
        directory.path(),
        &command,
        run.success(),
        &run.output.stderr,
    );
    diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)
        .map(|_| ())
        .map_err(|error| {
            offer.retain_failure(directory.path(), &command, &run.output.stderr, error)
        })
}

/// Parse the source before reserving original module and value identities.
/// This capability contains no checked types or native authority.
pub fn parse_cell_plan(
    specification: Arc<crate::checked_cell::CheckedCellSpecification>,
    include_paths: &[PathBuf],
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<Arc<crate::cell_plan::ParsedCellPlan>, CompileError> {
    crate::cell_plan::parse(specification, include_paths, settlement)
}

/// One compiler sidecar entry: a typed suspension-site id, its rendered answer
/// type, any live input types, and the defining modules needed to resolve each
/// type by name —
/// The compiler worker reports every defining module for tycons the type
/// mentions (the type's own head plus every type argument's head). It
/// has the type environment in hand at the call site, so it reports this
/// directly; `modules` is NOT `#[serde(default)]` — an extract binary old
/// enough not to emit it fails this deserialization loudly (`CompileError::
/// Asks`) rather than silently resolving with no modules, since a caller
/// that built an `AnswerContract` from an empty list would compile a shim
/// that cannot name the type at all.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct YieldSite {
    pub site: u64,
    pub origin: String,
    pub ordinal: u64,
    #[serde(rename = "type")]
    pub ty: String,
    pub modules: Vec<String>,
    pub heads: Vec<NominalHead>,
    pub inputs: Vec<SiteType>,
    // Legacy sidecars have no companion. Host activation must refuse that
    // absence; ordinary site identity and servicing remain unchanged.
    #[serde(default)]
    pub input_type_witnesses: Vec<Option<crate::checked_cell::CanonicalInputTypeWitness>>,
    #[serde(default)]
    pub reply_declaration: Option<String>,
    #[serde(deserialize_with = "deserialize_request_type_signatures")]
    pub request_type_signatures: Option<crate::checked_cell::RequestTypeSignatures>,
}

fn deserialize_request_type_signatures<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<crate::checked_cell::RequestTypeSignatures>, D::Error> {
    Option::deserialize(deserializer)
}

/// The compiler-issued request signature selects the ordinary or progress
/// layout. The final live input is response authority, never authored progress.
#[derive(Debug)]
pub struct RequestInputLayout<'a> {
    site: &'a YieldSite,
    signatures: &'a crate::checked_cell::RequestTypeSignatures,
}

impl<'a> RequestInputLayout<'a> {
    pub fn input(&self) -> &'a crate::checked_cell::CanonicalInputTypeWitness {
        self.site.input_type_witnesses[0].as_ref().unwrap()
    }

    pub fn progress(&self) -> Option<&'a crate::checked_cell::CanonicalInputTypeWitness> {
        self.signatures
            .progress()
            .map(|_| self.site.input_type_witnesses[1].as_ref().unwrap())
    }

    pub fn response_index(&self) -> usize {
        self.site.inputs.len() - 1
    }

    pub fn response(&self) -> &'a crate::checked_cell::CanonicalInputTypeWitness {
        self.site.input_type_witnesses[self.response_index()]
            .as_ref()
            .unwrap()
    }

    pub fn signatures(&self) -> &'a crate::checked_cell::RequestTypeSignatures {
        self.signatures
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RequestInputLayoutError {
    #[error("request site {site} has no compiler-issued request signatures")]
    MissingSignatures { site: u64 },
    #[error("request site {site} describes {actual} live input types, expected {expected} from its compiler-issued request signatures")]
    InputArity {
        site: u64,
        actual: usize,
        expected: usize,
    },
    #[error(
        "request site {site} describes {actual} canonical input witnesses, expected {expected}"
    )]
    WitnessArity {
        site: u64,
        actual: usize,
        expected: usize,
    },
    #[error("request site {site} has no canonical witness for live input {index}")]
    MissingWitness { site: u64, index: usize },
}

impl YieldSite {
    /// Parse an already issued request's live input layout. Runtime consumers
    /// still authenticate this metadata against the actual parked original.
    pub fn request_input_layout(&self) -> Result<RequestInputLayout<'_>, RequestInputLayoutError> {
        let site = self.site;
        let signatures = self
            .request_type_signatures
            .as_ref()
            .ok_or(RequestInputLayoutError::MissingSignatures { site })?;
        let expected = if signatures.progress().is_some() {
            3
        } else {
            2
        };
        if self.inputs.len() != expected {
            return Err(RequestInputLayoutError::InputArity {
                site,
                actual: self.inputs.len(),
                expected,
            });
        }
        if self.input_type_witnesses.len() != expected {
            return Err(RequestInputLayoutError::WitnessArity {
                site,
                actual: self.input_type_witnesses.len(),
                expected,
            });
        }
        for (index, witness) in self.input_type_witnesses.iter().enumerate() {
            if witness.is_none() {
                return Err(RequestInputLayoutError::MissingWitness { site, index });
            }
        }
        Ok(RequestInputLayout {
            site: self,
            signatures,
        })
    }
    pub fn same_metadata(&self, other: &Self) -> bool {
        self.metadata_digest() == other.metadata_digest()
    }
    pub(crate) fn metadata_digest(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        fn text(hasher: &mut Sha256, value: &str) {
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
        fn ty(hasher: &mut Sha256, value: &str, modules: &[String], heads: &[NominalHead]) {
            text(hasher, value);
            hasher.update((modules.len() as u64).to_le_bytes());
            for module in modules {
                text(hasher, module);
            }
            hasher.update((heads.len() as u64).to_le_bytes());
            for head in heads {
                for value in [&head.unit, &head.module, &head.name] {
                    text(hasher, value);
                }
            }
        }
        let mut hasher = Sha256::new();
        hasher.update(b"tidepool-typed-site-metadata-1");
        hasher.update(self.site.to_le_bytes());
        text(&mut hasher, &self.origin);
        hasher.update(self.ordinal.to_le_bytes());
        ty(&mut hasher, &self.ty, &self.modules, &self.heads);
        hasher.update((self.inputs.len() as u64).to_le_bytes());
        for input in &self.inputs {
            ty(&mut hasher, &input.ty, &input.modules, &input.heads);
        }
        hasher.update([u8::from(self.reply_declaration.is_some())]);
        if let Some(value) = &self.reply_declaration {
            text(&mut hasher, value);
        }
        hasher.update((self.input_type_witnesses.len() as u64).to_le_bytes());
        for witness in &self.input_type_witnesses {
            hasher.update([u8::from(witness.is_some())]);
            if let Some(witness) = witness {
                hasher.update(witness.metadata_digest());
            }
        }
        hasher.update([u8::from(self.request_type_signatures.is_some())]);
        if let Some(signatures) = &self.request_type_signatures {
            hasher.update(signatures.metadata_digest());
        }
        hasher.finalize().into()
    }
}

// The site map owns metadata identity. Normalize its existing duplicate/id
// semantics once; every opaque bundle proof uses this same complete digest.
pub(crate) fn yield_sites_metadata_digest(sites: &[YieldSite]) -> Result<[u8; 32], CompileError> {
    use sha2::{Digest, Sha256};
    let mut entries = std::collections::BTreeMap::new();
    for site in sites {
        let digest = site.metadata_digest();
        if entries
            .insert(site.site, digest)
            .is_some_and(|prior| prior != digest)
        {
            return Err(CompileError::ExtractFailed(
                "typed-site metadata collision".into(),
            ));
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(b"tidepool-typed-site-map-1");
    hasher.update((entries.len() as u64).to_le_bytes());
    for (site, digest) in entries {
        hasher.update(site.to_le_bytes());
        hasher.update(digest);
    }
    Ok(hasher.finalize().into())
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NominalHead {
    pub unit: String,
    pub module: String,
    pub name: String,
}

/// One GHC-rendered live input type attached to a suspension site.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SiteType {
    #[serde(rename = "type")]
    pub ty: String,
    pub modules: Vec<String>,
    pub heads: Vec<NominalHead>,
}

/// The typed-suspension sidecar indexed by site id, covering every typed
/// suspension site rather than only `ask`s; the on-disk filename (`asks.json`)
/// is a naming holdover and does not constrain what it holds.
#[derive(Debug, Clone, Default)]
pub struct YieldSites {
    by_site: HashMap<u64, YieldSite>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("typed-site collision at {site}: {first:?} != {second:?}")]
pub struct YieldSiteCollision {
    pub site: u64,
    pub first: Box<YieldSite>,
    pub second: Box<YieldSite>,
}

impl YieldSites {
    /// Test convenience for answer-only sites with no module information.
    pub fn from_pairs(pairs: Vec<(u64, String)>) -> Self {
        YieldSites {
            by_site: pairs
                .into_iter()
                .map(|(site, ty)| {
                    (
                        site,
                        YieldSite {
                            reply_declaration: None,
                            request_type_signatures: None,
                            site,
                            origin: "<test>".into(),
                            ordinal: site,
                            ty,
                            modules: Vec::new(),
                            heads: Vec::new(),
                            inputs: Vec::new(),
                            input_type_witnesses: Vec::new(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// Build an answer-only lookup from `(site, type, modules)` triples, for tests
    /// that need output-type module resolution but no live inputs.
    pub fn from_entries(entries: Vec<(u64, String, Vec<String>)>) -> Self {
        YieldSites {
            by_site: entries
                .into_iter()
                .map(|(site, ty, modules)| {
                    (
                        site,
                        YieldSite {
                            reply_declaration: None,
                            request_type_signatures: None,
                            site,
                            origin: "<test>".into(),
                            ordinal: site,
                            ty,
                            modules,
                            heads: Vec::new(),
                            inputs: Vec::new(),
                            input_type_witnesses: Vec::new(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// Build a lookup from the compiler's complete typed-site records.
    pub fn from_sites(sites: Vec<YieldSite>) -> Result<Self, YieldSiteCollision> {
        let mut by_site: HashMap<u64, YieldSite> = HashMap::new();
        for site in sites {
            match by_site.get(&site.site) {
                Some(previous) if !previous.same_metadata(&site) => {
                    return Err(YieldSiteCollision {
                        site: site.site,
                        first: Box::new(previous.clone()),
                        second: Box::new(site),
                    });
                }
                _ => {
                    by_site.insert(site.site, site);
                }
            }
        }
        Ok(Self { by_site })
    }

    /// The rendered answer type for a yield-site id, if the site is known.
    pub fn type_of(&self, site: u64) -> Option<&str> {
        self.by_site.get(&site).map(|entry| entry.ty.as_str())
    }

    /// The defining modules a shim must import to resolve `site`'s answer
    /// type by name — empty when the site is unknown or the extract that
    /// produced this sidecar recorded no modules (e.g. a `Prelude`-only
    /// type).
    pub fn modules_of(&self, site: u64) -> &[String] {
        self.by_site
            .get(&site)
            .map(|entry| entry.modules.as_slice())
            .unwrap_or(&[])
    }

    /// GHC-derived live input types attached to this suspension site.
    pub fn inputs_of(&self, site: u64) -> &[SiteType] {
        self.by_site
            .get(&site)
            .map(|entry| entry.inputs.as_slice())
            .unwrap_or(&[])
    }

    /// Every recorded answer `(site, type, modules)` entry, in no particular
    /// order. Live-input metadata remains available through [`Self::inputs_of`].
    pub fn iter(&self) -> impl Iterator<Item = (u64, &str, &[String])> {
        self.by_site
            .iter()
            .map(|(site, entry)| (*site, entry.ty.as_str(), entry.modules.as_slice()))
    }

    /// Complete compiler records in stable site-id order. Use this when a
    /// compiled program crosses into a provenance-owning runtime API; compact
    /// lookup consumers should continue to use [`Self::iter`].
    #[must_use]
    pub fn sites(&self) -> Vec<YieldSite> {
        let mut sites: Vec<_> = self.by_site.values().cloned().collect();
        sites.sort_by_key(|site| site.site);
        sites
    }

    /// Number of recorded sites.
    pub fn len(&self) -> usize {
        self.by_site.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_site.is_empty()
    }
}

// ---------------------------------------------------------------------------
// The artifact bundle
// ---------------------------------------------------------------------------

/// One target's checked prepared program and typed-yield sidecar. The
/// constructor table and warnings are shared across every
/// target in the same [`CompiledArtifacts`] (one GHC session, one merged
/// `meta.cbor`).
pub struct TargetArtifact {
    pub asks: YieldSites,
    /// Versioned execution program decoded under the exact host contract.
    pub prepared: PreparedArtifact,
    pub pending_imports: Vec<certified_products::PendingImportOwner>,
    pub package_interfaces: certified_products::CertifiedTargetPackageInterfaces,
}

/// The full output of one `tidepool-extract` invocation: a shared constructor
/// table + warnings, and one [`TargetArtifact`] per requested target.
pub struct CompiledArtifacts {
    pub artifact_view: crate::artifact_inventory::ArtifactView,
    /// DataCon metadata the JIT needs to dispatch on constructors — shared by
    /// every target (they compiled in the same GHC session).
    pub table: DataConTable,
    /// Compile warnings (e.g. `has_io`, captured type) — shared by every
    /// target, same reason.
    pub warnings: MetaWarnings,
    /// Per-target prepared program and asks sidecar, keyed by target name.
    pub targets: BTreeMap<String, TargetArtifact>,
    /// Entry-free definitions and skinny interfaces from the same worker
    /// transaction. These are raw products until graph evidence assigns
    /// exact module versions and every import owner.
    pub module_products: Vec<RawModuleProduct>,
    pub certified_groups: Vec<certified_products::PendingCertifiedGroup>,
    pub recovery_products: Vec<crate::recovery_artifacts::CertifiedRecoveryProduct>,
    pub(crate) source_selection: Option<certified_products::CertifiedSourceSelection>,
    /// Bound compiler producer identity for this exact invocation. `None`
    /// means the bundle was assembled from bytes without an endpoint.
    pub producer_identity: Option<[u8; 32]>,
    /// Fresh post-downsweep source graph paired with these products. Immutable
    /// declaration owners have separate protected admission and do not acquire
    /// source lookup witnesses. A later compile must revalidate its own inputs.
    pub module_inventory: Option<Vec<cache::ModuleEvidence>>,
    /// Authenticated consumed bytes and resolutions from this transaction,
    /// including sources that supplied interfaces without native originals.
    pub(crate) completed_source_evidence: Option<cache::CompletedSourceEvidence>,
    pub(crate) exact_source_admission: Option<crate::declaration_context::ExactSourceAdmission>,
}

/// Candidate suggestions for a worker compile whose source is rendered by the
/// worker after the request starts (the resident turn lane).
/// The same offer must be passed to final sealing; a manifest path alone is
/// never authority for a cached product.
pub struct ModuleCandidateOffer {
    selected: Option<Arc<module_candidates::CandidateSet>>,
    producer: Vec<u8>,
    include: Vec<PathBuf>,
    exact: Option<crate::declaration_context::ExactCompilationRequest>,
    checked_cell: Option<crate::checked_cell::CheckedCellSpecification>,
    planned_cell: Option<crate::checked_cell::CheckedPlannedCellSpecification>,
    checked_values: Option<Arc<crate::checked_cell::CheckedValueInputs>>,
    checked_projections: Vec<Arc<crate::declaration_join::AcceptedJoin>>,
    checked: Option<NativeCheckedOffer>,
    selected_session_values: std::sync::OnceLock<Vec<tidepool_repr::SessionModule>>,
}

fn selected_session_value_modules(
    command: &ExtractCmd,
) -> Result<Vec<tidepool_repr::SessionModule>, CompileError> {
    let mut selected = Vec::new();
    for name in command.selected_session_values() {
        let owner = name
            .to_str()
            .and_then(tidepool_repr::SessionModule::from_module_name)
            .filter(|owner| owner.kind == tidepool_repr::SessionModuleKind::Val && owner.gen.0 != 0)
            .ok_or_else(|| {
                CompileError::ExtractFailed("invalid selected session value owner".into())
            })?;
        if selected.contains(&owner) || selected.len() >= 128 {
            return Err(CompileError::ExtractFailed(
                "duplicate or excessive selected session values".into(),
            ));
        }
        selected.push(owner);
    }
    Ok(selected)
}

enum NativeCheckedOffer {
    Item(crate::checked_cell::CheckedItemOffer),
    ActivationPreview(crate::activation_preview::ActivationPreviewOffer),
}

// Protected compilation manifests bind the actual worker search order as well
// as the source and interface recipe. Unsupported path encodings refuse before
// the invocation builder can perform its ordinary lossy CLI conversion.
pub(crate) enum CheckedPurpose {
    Inspection,
    ReloadInspection,
    Cell,
    Item,
    ActivationPreview,
    Program,
}

impl CheckedPurpose {
    /// Every owning encoder emits its complete wire purpose directly.
    pub(crate) const fn wire_tag(self) -> &'static str {
        match self {
            Self::Inspection => "inspection1",
            Self::ReloadInspection => "reload-inspection1",
            Self::Cell => "cell-check4",
            Self::Item => "checked-item5",
            Self::ActivationPreview => "host-activation-renderer1",
            Self::Program => "cell-program3",
        }
    }
}

/// Authored whole-cell checking is independent of live-input binding and preview.
pub enum CheckedCellPurpose {
    Authored,
}

/// Attach search roots to an already tagged, owner-issued authorization.
/// This boundary never selects or rewrites the payload's purpose.
pub(crate) fn checked_search_authorization(
    mut authorization: Value,
    include: &[PathBuf],
) -> Result<Value, CompileError> {
    if include.len() > 4096 {
        return Err(CompileError::ExtractFailed(
            "checked search inputs exceed the request bound".into(),
        ));
    }
    let paths = include
        .iter()
        .map(|path| {
            path.as_os_str()
                .to_str()
                .filter(|value| path.is_absolute() && value.len() <= 65536)
                .map(|path| Value::Text(path.to_owned()))
                .ok_or_else(|| {
                    CompileError::ExtractFailed(
                        "checked search input is not absolute bounded UTF-8".into(),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Value::Array(fields) = &mut authorization else {
        unreachable!("closed checked authorization")
    };
    fields.push(Value::Array(paths));
    Ok(authorization)
}

fn checked_cell_authorization(
    purpose: CheckedCellPurpose,
    specification: &crate::checked_cell::CheckedCellSpecification,
    values: &crate::checked_cell::CheckedValueInputs,
    context: &crate::declaration_context::ExactDeclarationContext,
    include: &[PathBuf],
) -> Result<Value, CompileError> {
    checked_search_authorization(
        encode_cell_authorization(
            match purpose {
                CheckedCellPurpose::Authored => {
                    crate::checked_cell::CheckedCellManifestPurpose::Authored
                }
            },
            specification,
            values.baseline_authorization(),
            &context.selected_template_imports(&specification.template_sources())?,
        )?,
        include,
    )
}

pub(crate) fn encode_cell_authorization(
    purpose: crate::checked_cell::CheckedCellManifestPurpose,
    specification: &crate::checked_cell::CheckedCellSpecification,
    baseline: Value,
    templates: &crate::declaration_context::SelectedTemplateImports,
) -> Result<Value, CompileError> {
    let mut authorization = specification.manifest_value(purpose)?;
    let Value::Array(fields) = &mut authorization else {
        unreachable!("closed cell authorization")
    };
    fields.push(baseline);
    fields.push(templates.authorization_value());
    Ok(authorization)
}

fn encode_inspection_authorization_for(
    purpose: CheckedPurpose,
    owners: &BTreeSet<String>,
    baseline: Value,
) -> Value {
    Value::Array(vec![
        Value::Text(purpose.wire_tag().into()),
        Value::Array(owners.iter().cloned().map(Value::Text).collect()),
        baseline,
    ])
}

#[cfg(test)]
fn encode_inspection_authorization(owners: &BTreeSet<String>, baseline: Value) -> Value {
    encode_inspection_authorization_for(CheckedPurpose::Inspection, owners, baseline)
}

#[cfg(test)]
pub(crate) fn fixture_inspection_authorization() -> Value {
    encode_inspection_authorization(&BTreeSet::new(), Value::Array(vec![]))
}

fn compile_context_with_declarations(
    context: Option<Arc<crate::declaration_context::ExactCompileContext>>,
    declarations: Arc<crate::declaration_join::ExactDeclarationContext>,
) -> crate::declaration_context::ExactCompileContext {
    match context {
        Some(context) => (*context).clone().with_declarations(declarations),
        None => crate::declaration_context::ExactCompileContext::new(declarations),
    }
}

fn checked_offer_context(
    context: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
) -> Result<Arc<crate::declaration_join::ExactDeclarationContext>, CompileError> {
    match context {
        Some(context) => Ok(context),
        None => Ok(Arc::new(
            crate::declaration_join::ExactDeclarationContext::new(&[], &[], Vec::new())?,
        )),
    }
}

fn checked_value_context(
    context: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
    inputs: &crate::checked_cell::CheckedValueInputs,
) -> Result<Arc<crate::declaration_join::ExactDeclarationContext>, CompileError> {
    let context = checked_offer_context(context)?;
    Ok(Arc::new(
        (*context)
            .clone()
            .extend_retained_value_artifacts(&inputs.certified_artifacts())?,
    ))
}

fn empty_exact_context(context: &crate::declaration_join::ExactDeclarationContext) -> bool {
    context.recovery_products().is_empty()
        && context.joined_interfaces().is_empty()
        && context.lexical_graph().is_empty()
        && context.interface_owners().is_empty()
}

fn immutable_candidates_in_context(
    context: &crate::declaration_join::ExactDeclarationContext,
    producer: &[u8],
    include: &[PathBuf],
    scratch: &Path,
    reserved: BTreeSet<String>,
) -> Result<Option<Arc<module_candidates::CandidateSet>>, CompileError> {
    immutable_candidates_in_context_with_catalog(
        context,
        producer,
        include,
        scratch,
        reserved,
        &crate::toolchain::CatalogSelection::FreshConfigured,
    )
}

fn immutable_candidates_in_context_with_catalog(
    context: &crate::declaration_join::ExactDeclarationContext,
    producer: &[u8],
    include: &[PathBuf],
    scratch: &Path,
    reserved: BTreeSet<String>,
    catalog: &crate::toolchain::CatalogSelection,
) -> Result<Option<Arc<module_candidates::CandidateSet>>, CompileError> {
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
    let selected = context.compiler_metadata_snapshot()?;
    let canonical_interfaces = selected
        .entries
        .values()
        .filter_map(|entry| {
            let canonical = match &entry.payload {
                crate::artifact_inventory::ArtifactPayload::Canonical(interface) => interface,
                crate::artifact_inventory::ArtifactPayload::Original(product) => {
                    product.module_interface()?
                }
                crate::artifact_inventory::ArtifactPayload::Interface(_, _) => return None,
            };
            Some((
                (canonical.unit().to_owned(), canonical.module().to_owned()),
                canonical.clone(),
            ))
        })
        .collect();
    let ambiguous = context
        .artifact_view()
        .metadata_snapshot()
        .ambiguous_native_owners
        .into_iter()
        .map(|owner| (owner.unit, owner.module))
        .collect();
    let exclusions = module_candidates::ExactCandidateContext::new(protected, reserved)
        .with_originals(context.compiler_original_products()?)
        .with_canonical_interfaces(canonical_interfaces, ambiguous)
        .with_interface_seals(
            selected
                .entries
                .values()
                .map(|entry| &entry.descriptor)
                .map(|descriptor| {
                    (
                        (
                            descriptor.owner.unit.clone(),
                            descriptor.owner.module.clone(),
                        ),
                        descriptor.interface_sha256,
                    )
                })
                .collect(),
        );
    Ok(module_candidates::select_with_catalog(
        catalog,
        producer,
        include,
        scratch,
        Some(&exclusions),
    )?
    .map(Arc::new))
}

fn private_native_availability(
    context: &crate::declaration_join::ExactDeclarationContext,
    producer: &[u8],
    selected: Option<&module_candidates::CandidateSet>,
) -> Result<Option<crate::declaration_context::OriginalCompilerInputs>, CompileError> {
    crate::declaration_context::OriginalCompilerInputs::from_selected_authored_declarations(
        context,
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer),
        selected.map_or(&[], |selected| selected.native_availability.as_slice()),
    )
}

fn checked_candidate_reservations(
    specification: &crate::checked_cell::CheckedCellSpecification,
    planned: Option<&crate::checked_cell::CheckedPlannedCellSpecification>,
) -> BTreeSet<String> {
    use crate::checked_cell::CheckedPlannedCellSlot;
    use tidepool_repr::{Generation, SessionModule};
    let mut reserved = specification
        .injected_modules
        .iter()
        .chain(&specification.reserved_declaration_modules)
        .cloned()
        .collect::<BTreeSet<_>>();
    if let Some(planned) = planned {
        for slot in &planned.slots {
            match slot {
                CheckedPlannedCellSlot::Prologue { declaration }
                | CheckedPlannedCellSlot::Declaration { declaration } => {
                    reserved.insert(SessionModule::lib(Generation(*declaration)).module_name());
                }
                CheckedPlannedCellSlot::Bind { value } => {
                    reserved.insert(SessionModule::val(Generation(*value)).module_name());
                }
                CheckedPlannedCellSlot::Expression { capture, .. } => {
                    reserved.insert(SessionModule::val(Generation(*capture)).module_name());
                }
            }
        }
    }
    reserved
}

impl ModuleCandidateOffer {
    /// Physical transport leases carry no selection or compiler authority.
    /// Execution transfers these exact files into the compiler close owner.
    pub fn input_transport_files(&self) -> Vec<Arc<std::fs::File>> {
        let mut files = self
            .exact
            .as_ref()
            .map_or_else(Vec::new, |request| request.input_transport_files());
        if let Some(selected) = &self.selected {
            files.extend(
                selected
                    .input_transport
                    .iter()
                    .map(|slice| slice.compiler_file_lease()),
            );
        }
        files
    }

    pub fn select_admitted(
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
        include: &[PathBuf],
        scratch: &Path,
    ) -> Result<Self, CompileError> {
        Self::select_admitted_with_catalog(
            endpoint,
            include,
            scratch,
            &crate::toolchain::CatalogSelection::FreshConfigured,
        )
    }

    pub fn select_admitted_with_catalog(
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
        include: &[PathBuf],
        scratch: &Path,
        catalog: &crate::toolchain::CatalogSelection,
    ) -> Result<Self, CompileError> {
        let catalog = catalog.for_deployment(endpoint.deployment())?;
        Self::select_originals_with_catalog(
            endpoint.identity().producer_bytes(),
            include,
            scratch,
            &catalog,
        )
    }

    /// Execute an ordinary turn into fresh, privately owned outputs and seal
    /// input continuity before returning those outputs to a consumer.
    /// Keep the relocated command available for completed-response diagnostics.
    pub fn execute_admitted_turn(
        &self,
        endpoint: crate::toolchain::AdmittedCompilerEndpoint,
        command: &mut ExtractCmd,
        settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
    ) -> Result<AdmittedTurnOutput, CompileError> {
        let request = tidepool_extract_cmd::ExtractRequest::decode(&command.request_bytes())
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        if self.exact.is_some()
            || !request.is_turn()
            || self.producer != endpoint.identity().producer_bytes()
            || request.include_paths()
                != self
                    .include
                    .iter()
                    .map(PathBuf::as_path)
                    .collect::<Vec<_>>()
            || request
                .target_names()
                .iter()
                .any(|target| target != "__prepared")
        {
            return Err(CompileError::ExtractFailed(
                "admitted turn recipe differs from selected offer".into(),
            ));
        }
        let directory = compiler_scratch_directory()?;
        command.relocate_turn_outputs(directory.path());
        let diagnostics = CompilerDiagnosticCapture::start(directory.path(), command);
        let run = endpoint
            .execute_with_input_files(command, self.input_transport_files(), |close| {
                settlement(close)
            })
            .map_err(|error| self.retain_execution_failure(directory.path(), command, error))?;
        diagnostics.completed(directory.path(), command, run.success(), &run.output.stderr);
        let (turn, native) = if run.success()
            && diag::decode_extract_result(true, &run.output.stdout, &run.output.stderr).is_ok()
        {
            self.read_admitted_turn(directory.path(), request.supports_compile_input_identity())
                .map_err(|error| {
                    self.retain_failure(directory.path(), command, &run.output.stderr, error)
                })?
        } else {
            (None, None)
        };
        Ok(AdmittedTurnOutput {
            directory,
            run,
            turn,
            native,
        })
    }

    fn read_admitted_turn(
        &self,
        directory: &Path,
        issue_identity: bool,
    ) -> Result<(Option<Arc<[u8]>>, Option<NativeTurnOutput>), CompileError> {
        let turn: Arc<[u8]> =
            crate::checked_cell::read(directory.join("turn.cbor"), 32 << 20)?.into();
        let value = crate::checked_cell::decode(&turn)?;
        let row = crate::checked_cell::row(&value, 2)?;
        let (source, site_observations) = match crate::checked_cell::string(&row[0])? {
            "Bind" => {
                let fields = crate::checked_cell::row(&row[1], 5)?;
                (crate::checked_cell::string(&fields[4])?, &fields[3])
            }
            "Expr" => {
                let fields = crate::checked_cell::row(&row[1], 3)?;
                (crate::checked_cell::string(&fields[2])?, &fields[1])
            }
            "Decl" => return Ok((Some(turn), None)),
            _ => {
                return Err(CompileError::ExtractFailed(
                    "admitted turn result kind".into(),
                ));
            }
        };
        let sites = decode_turn_yield_sites(site_observations)?;
        let mut output = read_native_turn_artifacts(directory, turn.clone(), source.to_owned())?;
        let deserialize_start = Instant::now();
        let (table, warnings) =
            tidepool_repr::serial::read_metadata_for_program(&output.metadata, &output.target)?;
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_CBOR_DESERIALIZE,
            deserialize_start.elapsed(),
            0,
        );
        let module = extract_module_name(source).ok_or_else(|| {
            CompileError::ExtractFailed("admitted turn source module missing".into())
        })?;
        output.products = seal_turn_outputs_inner(
            self,
            directory,
            &directory.join(format!("{module}.hs")),
            source,
            &output.target,
            "__prepared",
            issue_identity.then_some((&table, sites.as_slice())),
            None,
            OriginalOutputPublication::Transaction,
        )?
        .map(Arc::new);
        Ok((
            Some(turn),
            Some(NativeTurnOutput {
                output,
                table,
                warnings,
            }),
        ))
    }

    pub fn select(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
    ) -> Result<Self, CompileError> {
        Self::select_originals_with_catalog(
            producer,
            include,
            scratch,
            &crate::toolchain::CatalogSelection::FreshConfigured,
        )
    }

    fn select_originals_with_catalog(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        catalog: &crate::toolchain::CatalogSelection,
    ) -> Result<Self, CompileError> {
        Ok(Self {
            selected: module_candidates::select_with_catalog(
                catalog, producer, include, scratch, None,
            )?
            .map(Arc::new),
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: None,
            checked_cell: None,
            planned_cell: None,
            checked_values: None,
            checked_projections: Vec::new(),
            checked: None,
            selected_session_values: Default::default(),
        })
    }

    pub fn select_in_context(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Arc<crate::declaration_context::ExactCompileContext>,
    ) -> Result<Self, CompileError> {
        Self::select_originals_in_context_with_catalog(
            producer,
            include,
            scratch,
            context,
            &crate::toolchain::CatalogSelection::FreshConfigured,
        )
    }

    pub fn select_in_context_with_catalog(
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
        include: &[PathBuf],
        scratch: &Path,
        context: Arc<crate::declaration_context::ExactCompileContext>,
        catalog: &crate::toolchain::CatalogSelection,
    ) -> Result<Self, CompileError> {
        let catalog = catalog.for_deployment(endpoint.deployment())?;
        Self::select_originals_in_context_with_catalog(
            endpoint.identity().producer_bytes(),
            include,
            scratch,
            context,
            &catalog,
        )
    }

    fn select_originals_in_context_with_catalog(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Arc<crate::declaration_context::ExactCompileContext>,
        catalog: &crate::toolchain::CatalogSelection,
    ) -> Result<Self, CompileError> {
        let selected = immutable_candidates_in_context_with_catalog(
            context.declarations(),
            producer,
            include,
            scratch,
            BTreeSet::new(),
            catalog,
        )?;
        let private =
            private_native_availability(context.declarations(), producer, selected.as_deref())?;
        Ok(Self {
            selected: None,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(
                context
                    .prepare_compilation_with_private_input(
                        &scratch.join("exact-scope"),
                        producer,
                        None,
                        private,
                    )?
                    .with_source_search_context(include),
            ),
            checked_cell: None,
            planned_cell: None,
            checked_values: None,
            checked_projections: Vec::new(),
            checked: None,
            selected_session_values: Default::default(),
        })
    }

    /// Inspection may consume certified live values but cannot publish new ones.
    pub fn select_inspection(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
        values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
        retained: &[Arc<crate::checked_cell::CheckedValueArtifact>],
    ) -> Result<Self, CompileError> {
        Self::select_inspection_for(
            CheckedPurpose::Inspection,
            producer,
            include,
            scratch,
            context,
            values,
            retained,
        )
    }

    pub fn select_inspection_with_catalog(
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
        values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
        retained: &[Arc<crate::checked_cell::CheckedValueArtifact>],
        catalog: &crate::toolchain::CatalogSelection,
    ) -> Result<Self, CompileError> {
        let catalog = catalog.for_deployment(endpoint.deployment())?;
        Self::select_inspection_for_with_catalog(
            CheckedPurpose::Inspection,
            endpoint.identity().producer_bytes(),
            include,
            scratch,
            context,
            values,
            retained,
            &catalog,
        )
    }

    /// Reload compares candidate source for every original in the retained
    /// namespace, independently of whether the inspection query uses it.
    pub fn select_reload_inspection(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
        values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
        retained: &[Arc<crate::checked_cell::CheckedValueArtifact>],
    ) -> Result<Self, CompileError> {
        Self::select_inspection_for(
            CheckedPurpose::ReloadInspection,
            producer,
            include,
            scratch,
            context,
            values,
            retained,
        )
    }

    fn select_inspection_for(
        purpose: CheckedPurpose,
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
        values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
        retained: &[Arc<crate::checked_cell::CheckedValueArtifact>],
    ) -> Result<Self, CompileError> {
        Self::select_inspection_for_with_catalog(
            purpose,
            producer,
            include,
            scratch,
            context,
            values,
            retained,
            &crate::toolchain::CatalogSelection::FreshConfigured,
        )
    }

    fn select_inspection_for_with_catalog(
        purpose: CheckedPurpose,
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
        values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
        retained: &[Arc<crate::checked_cell::CheckedValueArtifact>],
        catalog: &crate::toolchain::CatalogSelection,
    ) -> Result<Self, CompileError> {
        let owners = values
            .iter()
            .map(|(owner, _)| owner.module_name())
            .collect::<BTreeSet<_>>();
        if owners.len() != values.len() || retained.len() != values.len() {
            return Err(CompileError::ExtractFailed(
                "inspection value inventory lacks exact checked certificates".into(),
            ));
        }
        let inputs = crate::checked_cell::CheckedValueInputs::capture_checked(values, retained)?;
        let context = checked_value_context(context, &inputs)?;
        let authorization = checked_search_authorization(
            encode_inspection_authorization_for(purpose, &owners, inputs.baseline_authorization()),
            include,
        )?;
        let selected = immutable_candidates_in_context_with_catalog(
            &context, producer, include, scratch, owners, catalog,
        )?;
        let private = private_native_availability(&context, producer, selected.as_deref())?;
        Ok(Self {
            selected,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(
                context
                    .prepare_compilation_with_private_input(
                        &scratch.join("exact-scope"),
                        producer,
                        Some(authorization),
                        private,
                    )?
                    .with_source_search_context(include)
                    .with_checked_value_imports(inputs.import_authority()),
            ),
            checked_cell: None,
            planned_cell: None,
            checked_values: Some(inputs),
            checked_projections: Vec::new(),
            checked: None,
            selected_session_values: Default::default(),
        })
    }

    pub fn select_checked_cell(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_context::ExactCompileContext>>,
        specification: crate::checked_cell::CheckedCellSpecification,
        purpose: CheckedCellPurpose,
        checked_values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
        retained_interfaces: &[Arc<crate::checked_cell::CheckedValueArtifact>],
        retained_projections: &[Arc<crate::declaration_join::AcceptedJoin>],
    ) -> Result<Self, CompileError> {
        let expected = specification
            .injected_modules
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let actual = checked_values
            .iter()
            .map(|(module, _)| module.module_name())
            .collect::<BTreeSet<_>>();
        if actual.len() != checked_values.len() || actual != expected {
            return Err(CompileError::ExtractFailed(
                "checked initial interface inventory differs from injected owners".into(),
            ));
        }
        let compile_context = context;
        let publication_context = checked_offer_context(
            compile_context
                .as_ref()
                .map(|context| context.declarations().clone()),
        )?;
        let checked_values = crate::checked_cell::CheckedValueInputs::capture_checked(
            checked_values,
            retained_interfaces,
        )?;
        let context = checked_value_context(Some(publication_context), &checked_values)?;
        let checked_projections =
            crate::checked_cell::retain_projection_inputs(&context, retained_projections)?;
        let authorization = checked_cell_authorization(
            purpose,
            &specification,
            &checked_values,
            &context,
            include,
        )?;
        let selected = immutable_candidates_in_context(
            &context,
            producer,
            include,
            scratch,
            checked_candidate_reservations(&specification, None),
        )?;
        let private = private_native_availability(&context, producer, selected.as_deref())?;
        Ok(Self {
            selected,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(
                compile_context_with_declarations(compile_context, context.clone())
                    .prepare_compilation_with_private_input(
                        &scratch.join("exact-scope"),
                        producer,
                        Some(authorization),
                        private,
                    )?
                    .with_source_search_context(include)
                    .with_checked_value_imports(checked_values.import_authority())
                    .with_initial_template_interfaces(
                        context.clone(),
                        &specification.template_sources(),
                    )?,
            ),
            checked_cell: Some(specification),
            planned_cell: None,
            checked_values: Some(checked_values),
            checked_projections,
            checked: None,
            selected_session_values: Default::default(),
        })
    }

    /// Reserve the complete original identity inventory before compilation.
    pub fn select_cell_program(
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_context::ExactCompileContext>>,
        specification: crate::checked_cell::CheckedCellSpecification,
        values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
        planned: crate::checked_cell::CheckedPlannedCellSpecification,
        retained_interfaces: &[Arc<crate::checked_cell::CheckedValueArtifact>],
        retained_projections: &[Arc<crate::declaration_join::AcceptedJoin>],
    ) -> Result<Self, CompileError> {
        Self::select_cell_program_with_catalog(
            endpoint,
            include,
            scratch,
            context,
            specification,
            values,
            planned,
            retained_interfaces,
            retained_projections,
            &crate::toolchain::CatalogSelection::FreshConfigured,
        )
    }

    pub fn select_cell_program_with_catalog(
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_context::ExactCompileContext>>,
        specification: crate::checked_cell::CheckedCellSpecification,
        values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
        planned: crate::checked_cell::CheckedPlannedCellSpecification,
        retained_interfaces: &[Arc<crate::checked_cell::CheckedValueArtifact>],
        retained_projections: &[Arc<crate::declaration_join::AcceptedJoin>],
        catalog: &crate::toolchain::CatalogSelection,
    ) -> Result<Self, CompileError> {
        let catalog = catalog.for_deployment(endpoint.deployment())?;
        let producer = endpoint.identity().producer_bytes();
        let expected = specification
            .injected_modules
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let actual = values
            .iter()
            .map(|(module, _)| module.module_name())
            .collect::<BTreeSet<_>>();
        if actual.len() != values.len() || actual != expected {
            return Err(CompileError::ExtractFailed(
                "compiled initial interface inventory differs from injected owners".into(),
            ));
        }
        let extension = planned.authorization(&specification, producer, include, scratch)?;
        let mut authorization = specification
            .manifest_value(crate::checked_cell::CheckedCellManifestPurpose::Program)?;
        let compile_context = context;
        let publication_context = checked_offer_context(
            compile_context
                .as_ref()
                .map(|context| context.declarations().clone()),
        )?;
        let inputs =
            crate::checked_cell::CheckedValueInputs::capture_checked(values, retained_interfaces)?;
        let context = checked_value_context(Some(publication_context), &inputs)?;
        let checked_projections =
            crate::checked_cell::retain_projection_inputs(&context, retained_projections)?;
        let Value::Array(fields) = &mut authorization else {
            unreachable!("closed authorization")
        };
        fields.push(inputs.baseline_authorization());
        fields.push(
            context
                .selected_template_imports(&specification.template_sources())?
                .authorization_value(),
        );
        fields.extend(extension);
        fields.push(Value::Array(
            include
                .iter()
                .map(|path| Value::Text(path.to_string_lossy().into_owned()))
                .collect(),
        ));
        let selected = immutable_candidates_in_context_with_catalog(
            &context,
            producer,
            include,
            scratch,
            checked_candidate_reservations(&specification, Some(&planned)),
            &catalog,
        )?;
        let private = private_native_availability(&context, producer, selected.as_deref())?;
        Ok(Self {
            selected,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(
                compile_context_with_declarations(compile_context, context.clone())
                    .prepare_compilation_with_private_input(
                        &scratch.join("exact-scope"),
                        producer,
                        Some(authorization),
                        private,
                    )?
                    .with_source_search_context(include)
                    .with_checked_value_imports(inputs.import_authority())
                    .with_initial_template_interfaces(
                        context.clone(),
                        &specification.template_sources(),
                    )?,
            ),
            checked_cell: Some(specification),
            planned_cell: Some(planned),
            checked_values: Some(inputs),
            checked_projections,
            checked: None,
            selected_session_values: Default::default(),
        })
    }

    pub fn select_checked_item(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_context::ExactCompileContext>>,
        item: crate::checked_cell::ExactCheckedItem,
        prefix: crate::checked_cell::ExactCompiledPrefix,
        runtime_prefix_digest: [u8; 32],
        generation: u64,
        observation_name: Option<&str>,
        templates: &[(String, String)],
        settled_bindings: Vec<(
            String,
            tidepool_repr::execution_schema::SymbolIdentity,
            u64,
            u64,
        )>,
    ) -> Result<Self, CompileError> {
        let compile_context = context;
        let context = prefix.with_value_context(checked_offer_context(
            compile_context
                .as_ref()
                .map(|context| context.declarations().clone()),
        )?)?;
        let settled_values = prefix.select_settled_values(settled_bindings)?;
        let checked_item = crate::checked_cell::CheckedItemOffer {
            item,
            prefix,
            runtime_prefix_digest,
            generation,
            observation_name: observation_name.map(str::to_owned),
            is_program: false,
            settled_values,
        };
        checked_item.validate_templates(templates)?;
        checked_item.validate_include(include)?;
        let mut selected = None;
        let exact = compile_context_with_declarations(compile_context, context.clone())
            .with_generated_planned_imports(
                checked_item
                    .prefix
                    .planned_declaration_proof()
                    .map(|proof| &proof.certificate),
                checked_item
                    .item
                    .turn_templates()
                    .iter()
                    .map(|(_, source)| source.as_str()),
            )?
            .prepare_compilation_authorizing_with_private_input(
                &scratch.join("exact-scope"),
                producer,
                |semantic_sha256| {
                    let authorization = checked_search_authorization(
                        checked_item.authorization(producer, semantic_sha256)?,
                        include,
                    )?;
                    let mut reserved = checked_item
                        .prefix
                        .injected_modules()
                        .into_iter()
                        .collect::<BTreeSet<_>>();
                    reserved.extend(
                        checked_item
                            .item
                            .reserved_declaration_modules()
                            .iter()
                            .cloned(),
                    );
                    reserved.insert(
                        tidepool_repr::SessionModule::val(tidepool_repr::Generation(
                            checked_item.generation,
                        ))
                        .module_name(),
                    );
                    selected = immutable_candidates_in_context(
                        &context, producer, include, scratch, reserved,
                    )?;
                    let private =
                        private_native_availability(&context, producer, selected.as_deref())?;
                    Ok((authorization, private))
                },
            )?;
        Ok(Self {
            selected,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(
                exact
                    .with_source_search_context(include)
                    .with_checked_value_imports(checked_item.prefix.import_authority()?)
                    .with_initial_template_interfaces(
                        checked_item.item.template_context(),
                        &checked_item.item.template_sources(),
                    )?,
            ),
            checked_cell: None,
            planned_cell: None,
            checked_values: None,
            checked_projections: Vec::new(),
            checked: Some(NativeCheckedOffer::Item(checked_item)),
            selected_session_values: Default::default(),
        })
    }

    /// Specialize a pure renderer against the original input type and context.
    pub fn select_activation_preview(
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
        include: &[PathBuf],
        scratch: &Path,
        context: Arc<crate::declaration_context::ExactCompileContext>,
        prototype: Arc<crate::checked_cell::ExactHostBindingPrototype>,
        specification: crate::activation_preview::ActivationPreviewSpecification,
        catalog: &crate::toolchain::CatalogSelection,
    ) -> Result<crate::activation_preview::ActivationPreviewSelection, CompileError> {
        use crate::activation_preview::{ActivationPreviewOffer, ActivationPreviewSelection};
        let catalog = catalog.for_deployment(endpoint.deployment())?;
        let producer = endpoint.identity().producer_bytes();
        if crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
            .sha256()
            != prototype.producer()
        {
            return Err(CompileError::ExtractFailed(
                "activation preview worker differs from the original producer".into(),
            ));
        }
        if !crate::activation_preview::validate_original_display_context(
            &prototype,
            context.declarations(),
        )? {
            return Ok(ActivationPreviewSelection::OriginalDisplayEvidenceUnavailable);
        }
        let declarations = Arc::new(
            (**context.declarations())
                .clone()
                .extend_interface_context(prototype.context())?,
        );
        let selected = immutable_candidates_in_context_with_catalog(
            &declarations,
            producer,
            include,
            scratch,
            BTreeSet::new(),
            &catalog,
        )?;
        let offer = ActivationPreviewOffer {
            specification,
            prototype,
            original_execution: context.declarations().clone(),
        };
        let original_interfaces = Value::Array(
            offer
                .original_execution
                .original_preview_interface_graph()?
                .into_iter()
                .map(|(owner, node)| {
                    Value::Array(vec![
                        Value::Text(owner.unit),
                        Value::Text(owner.module),
                        Value::Text(crate::checked_cell::hex(&node.interface_sha256)),
                        Value::Array(
                            node.imports
                                .into_iter()
                                .map(|owner| {
                                    Value::Array(vec![
                                        Value::Text(owner.unit),
                                        Value::Text(owner.module),
                                    ])
                                })
                                .collect(),
                        ),
                    ])
                })
                .collect(),
        );
        let authorization =
            checked_search_authorization(offer.authorization(original_interfaces)?, include)?;
        let private = crate::activation_preview::original_native_declaration_inputs(
            &offer.original_execution,
            producer,
        )?;
        let exact = (*context)
            .clone()
            .with_declarations(declarations)
            .prepare_compilation_with_private_input(
                &scratch.join("exact-scope"),
                producer,
                Some(authorization),
                private,
            )?
            .with_source_search_context(include);
        Ok(ActivationPreviewSelection::Ready(Self {
            selected,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(exact),
            checked_cell: None,
            planned_cell: None,
            checked_values: None,
            checked_projections: Vec::new(),
            checked: Some(NativeCheckedOffer::ActivationPreview(offer)),
            selected_session_values: Default::default(),
        }))
    }

    /// Read the compiler's distinct no-program result after a successful
    /// preview request. Missing native display evidence cannot become Opaque.
    pub fn activation_preview_unavailable(&self, root: &Path) -> Result<bool, CompileError> {
        match &self.checked {
            Some(NativeCheckedOffer::ActivationPreview(preview)) => {
                let exact = self.exact.as_ref().expect("preview has exact offer");
                exact.validate_artifacts()?;
                preview.unavailable(root, &exact.request_sha256)
            }
            _ => {
                if root.join("activation-preview-unavailable.cbor").exists() {
                    return Err(CompileError::ExtractFailed(
                        "non-preview output advertised preview evidence".into(),
                    ));
                }
                Ok(false)
            }
        }
    }

    /// Retained checked-cell artifact root. Only the sealed request manifest
    /// authorizes inputs from this directory; new directory members do not.
    pub fn checked_value_root(&self) -> Option<&Path> {
        self.checked_values
            .as_ref()
            .map(|inputs| inputs.root())
            .or_else(|| match self.checked.as_ref()? {
                NativeCheckedOffer::Item(offer) => Some(offer.item.value_input_root()),
                NativeCheckedOffer::ActivationPreview(_) => None,
            })
    }

    /// Retain this request and its selected checked inputs for diagnosis.
    /// The saved files are evidence only; they cannot issue compiler authority.
    /// `command` is the executed request, including any owner output relocation.
    pub fn retain_failure(
        &self,
        directory: &Path,
        command: &ExtractCmd,
        stderr: &[u8],
        error: CompileError,
    ) -> CompileError {
        retain_compiler_failure_inner(directory, stderr, error, Some(self), command)
    }

    /// Preserve the typed request and selected inputs when transport fails
    /// before compiler diagnostics can be returned. Evidence grants no authority.
    pub fn retain_execution_failure(
        &self,
        directory: &Path,
        command: &ExtractCmd,
        error: CompileError,
    ) -> CompileError {
        retain_compiler_failure_inner(directory, &[], error, Some(self), command)
    }

    fn retain_checked_inputs(&self, destination: &Path) -> std::io::Result<()> {
        if let Some(inputs) = &self.checked_values {
            inputs.retain_diagnostics(None, destination)
        } else {
            match &self.checked {
                Some(NativeCheckedOffer::Item(offer)) => offer
                    .item
                    .retain_input_diagnostics(&offer.prefix, destination),
                Some(NativeCheckedOffer::ActivationPreview(_)) => Ok(()),
                None => Ok(()),
            }
        }
    }

    pub fn admit_checked_cell(
        &self,
        root: &Path,
    ) -> Result<Arc<crate::checked_cell::ExactCheckedCell>, CompileError> {
        let exact = self.exact.as_ref().ok_or_else(|| {
            CompileError::ExtractFailed(
                "checked-cell authority requires an exact compiler offer".into(),
            )
        })?;
        let specification = self.checked_cell.as_ref().ok_or_else(|| {
            CompileError::ExtractFailed("ordinary compile offer cannot admit a checked cell".into())
        })?;
        let planned = self.admit_planned_declaration(
            root,
            exact,
            specification,
            None,
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )?;
        let admissions = exact.validate_outputs_with_planned(root, planned.as_ref())?;
        crate::checked_cell::admit_checked_cell(
            root,
            &self.producer,
            exact.semantic_sha256,
            exact.context().clone(),
            self.checked_projections.clone(),
            &exact.request_sha256,
            specification,
            &admissions,
            &self.include,
            planned,
            self.checked_values.clone().ok_or_else(|| {
                CompileError::ExtractFailed("checked values owner is absent".into())
            })?,
            BTreeMap::new(),
            None,
        )
    }

    /// Admit every prepared segment and target before issuing execution data.
    /// The runtime can consume this immutable result without compiling a prefix.
    #[tracing::instrument(
        target = "exomonad_harness::timing",
        name = "products.cell_program",
        level = "debug",
        skip_all,
        fields(inclusive = true)
    )]
    pub fn admit_cell_program(
        &self,
        root: &Path,
    ) -> Result<Arc<crate::checked_cell::CellProgram>, CompileError> {
        use crate::cell_plan::ParsedCellPlanKind;
        use crate::checked_cell::{
            self, CellProgram, CellProgramItem, CellProgramObservations, CheckedItemKind,
            CheckedPlannedCellSlot,
        };
        let specification = self.checked_cell.as_ref().ok_or_else(|| {
            CompileError::ExtractFailed("program has no source specification".into())
        })?;
        let planned = self.planned_cell.as_ref().ok_or_else(|| {
            CompileError::ExtractFailed("ordinary checking cannot issue a complete program".into())
        })?;
        let initial = self.exact.as_ref().ok_or_else(|| {
            CompileError::ExtractFailed("program has no exact compiler offer".into())
        })?;
        let values = self
            .checked_values
            .as_ref()
            .ok_or_else(|| CompileError::ExtractFailed("program has no interface owner".into()))?;
        let typed_segments = checked_cell::read_program_typed_segments(
            root,
            &initial.request_sha256,
            specification,
            planned,
        )?;
        let mut context = initial.context().clone();
        let mut program_request = initial.clone();
        // One host admission observes package bytes once across all original
        // outputs. The next admission starts a fresh filesystem observation.
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let mut declarations = BTreeMap::new();
        let mut pending_segments = Vec::new();
        let mut outputs = BTreeMap::new();
        let mut segment = 0usize;
        let mut index = 0usize;
        while index < planned.parsed_plan.items().len() {
            let item = &planned.parsed_plan.items()[index];
            let declaration = matches!(
                item.kind(),
                ParsedCellPlanKind::Declaration | ParsedCellPlanKind::Prologue
            );
            let segment_root = root.join(format!("segment-{segment}"));
            if declaration {
                let generation = match planned.slots[index] {
                    CheckedPlannedCellSlot::Prologue { declaration }
                    | CheckedPlannedCellSlot::Declaration { declaration } => declaration,
                    _ => {
                        return Err(CompileError::ExtractFailed(
                            "declaration has another reserved slot".into(),
                        ));
                    }
                };
                program_request = program_request
                    .in_program_context(&root.join("program-inputs"), context.clone())?;
                let effective = self.program_offer(program_request.clone());
                let mut source_spec = specification.clone();
                source_spec.reserved_declaration_modules =
                    vec![
                        tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation))
                            .module_name(),
                    ];
                let produced_types = values.capture_produced_types(
                    initial.producer_sha256,
                    planned,
                    index..index,
                    &mut validation,
                )?;
                let original = effective
                    .admit_planned_declaration(
                        &segment_root,
                        effective.exact.as_ref().expect("exact program offer"),
                        &source_spec,
                        Some(&produced_types),
                        &mut validation,
                    )?
                    .ok_or_else(|| {
                        CompileError::ExtractFailed(
                            "program original declaration output is absent".into(),
                        )
                    })?;
                let mut lexical = context
                    .lexical_graph()
                    .iter()
                    .map(|node| (node.owner.clone(), node.imports.clone()))
                    .collect::<BTreeMap<_, _>>();
                // The certificate retains complete interface requirements.
                // Fresh source owners join the lexical graph; captured value
                // and hidden type owners remain exact hydration dependencies.
                let selected = lexical
                    .keys()
                    .cloned()
                    .chain(
                        original
                            .certificate
                            .original_home_imports()
                            .map(|(owner, _)| owner.clone()),
                    )
                    .chain(
                        original
                            .certificate
                            .source_lexical_imports()
                            .iter()
                            .map(|node| node.owner.clone()),
                    )
                    .collect::<BTreeSet<_>>();
                for (owner, imports) in original.certificate.original_home_imports() {
                    let imports = imports
                        .iter()
                        .filter(|owner| selected.contains(owner))
                        .cloned()
                        .collect::<Vec<_>>();
                    if lexical
                        .insert(owner.clone(), imports.clone())
                        .is_some_and(|old| old != imports)
                    {
                        return Err(CompileError::ExtractFailed(
                            "program changed an admitted original import".into(),
                        ));
                    }
                }
                for node in original.certificate.source_lexical_imports() {
                    let imports = node
                        .imports
                        .iter()
                        .filter(|owner| selected.contains(*owner))
                        .cloned()
                        .collect::<Vec<_>>();
                    if lexical
                        .insert(node.owner.clone(), imports.clone())
                        .is_some_and(|old| old != imports)
                    {
                        return Err(CompileError::ExtractFailed(
                            "program changed authenticated inherited source imports".into(),
                        ));
                    }
                }
                context = Arc::new(
                    (*context).clone().extend(
                        std::slice::from_ref(&original.certificate),
                        &[],
                        lexical
                            .into_iter()
                            .map(
                                |(owner, imports)| crate::declaration_join::ExactLexicalNode {
                                    owner,
                                    imports,
                                },
                            )
                            .collect(),
                    )?,
                );
                program_request = program_request.in_program_context_with_private_input(
                    &root.join("program-inputs"),
                    context.clone(),
                    &original.compiler_input,
                )?;
                declarations.insert(index, original);
                index += 1;
            } else {
                let end = planned.parsed_plan.items()[index..]
                    .iter()
                    .position(|item| {
                        matches!(
                            item.kind(),
                            ParsedCellPlanKind::Declaration | ParsedCellPlanKind::Prologue
                        )
                    })
                    .map_or(planned.parsed_plan.items().len(), |offset| index + offset);
                let segment_input = program_request
                    .in_program_context(&root.join("program-inputs"), context.clone())?;
                let mut source_segment = segment_input.admit_program_segment(&segment_root)?;
                let produced_types = values.capture_produced_types(
                    initial.producer_sha256,
                    planned,
                    index..end,
                    &mut validation,
                )?;
                let segment_offer = self.program_offer(segment_input);
                segment_offer.admit_segment_original_products(
                    &segment_root,
                    &mut source_segment,
                    &produced_types,
                    &mut validation,
                )?;
                while index < end {
                    let item_produced_types = produced_types.for_item(index)?;
                    program_request = program_request
                        .in_program_context(&root.join("program-inputs"), context.clone())?;
                    let effective = self.program_offer(program_request.clone());
                    let directory = root.join(format!("item-{index}"));
                    let output = effective.read_program_output(
                        &directory,
                        &segment_root,
                        &item_produced_types,
                        &typed_segments,
                        index,
                        specification.admission_digest,
                        &source_segment,
                        &mut validation,
                    )?;
                    context = self.admit_program_support(
                        &mut program_request,
                        context,
                        &output,
                        &source_segment,
                        Some(&item_produced_types),
                    )?;
                    let generation = match &planned.slots[index] {
                        CheckedPlannedCellSlot::Bind { value } => *value,
                        CheckedPlannedCellSlot::Expression { capture, .. } => *capture,
                        _ => {
                            return Err(CompileError::ExtractFailed(
                                "native item has another reserved slot".into(),
                            ));
                        }
                    };
                    context =
                        self.admit_program_value(context, values.root(), generation, &output.turn)?;
                    outputs.insert(index, output);
                    index += 1;
                }
                pending_segments.push((segment_offer, source_segment));
            }
            segment += 1;
        }
        let cell = checked_cell::admit_checked_cell(
            root,
            &self.producer,
            initial.semantic_sha256,
            initial.context().clone(),
            self.checked_projections.clone(),
            &initial.request_sha256,
            specification,
            pending_segments
                .iter()
                .flat_map(|(_, segment)| segment.admissions()),
            &self.include,
            None,
            values.clone(),
            declarations,
            Some(planned),
        )?;
        let mut prefix = if cell.item_count() == 0 {
            None
        } else {
            Some(cell.item(0)?.initial_prefix()?)
        };
        let mut items = Vec::with_capacity(cell.item_count());
        for index in 0..cell.item_count() {
            let _item_span = tracing::debug_span!(target: "exomonad_harness::timing", "products.checked_item", index, inclusive = true).entered();
            let item = cell.item(index)?;
            let completed = prefix.as_ref().expect("nonempty program prefix");
            if item.kind() == CheckedItemKind::Declaration {
                prefix = Some(completed.append_program_original(item.clone())?);
                items.push(CellProgramItem {
                    checked: item,
                    native: None,
                    native_observations: None,
                });
                continue;
            }
            let output = outputs.remove(&index).ok_or_else(|| {
                CompileError::ExtractFailed("program native target is missing".into())
            })?;
            let (generation, observation) = match &planned.slots[index] {
                CheckedPlannedCellSlot::Bind { value } => (*value, None),
                CheckedPlannedCellSlot::Expression {
                    capture,
                    observation_name,
                    ..
                } => (*capture, Some(observation_name.clone())),
                _ => {
                    return Err(CompileError::ExtractFailed(
                        "program native slot differs".into(),
                    ));
                }
            };
            let native = checked_cell::CheckedItemOffer {
                item: item.clone(),
                prefix: completed.clone(),
                runtime_prefix_digest: cell.admission_digest(),
                generation,
                observation_name: observation,
                is_program: true,
                settled_values: completed.prepared_value_selection()?,
            }
            .seal(
                &output.directory,
                &initial.request_sha256,
                &output.source,
                &output.target,
                &context,
                program_request.program_source_lexical(),
                output
                    .products
                    .as_ref()
                    .and_then(|products| products.original_execution.clone())
                    .ok_or_else(|| {
                        CompileError::ExtractFailed(
                            "program output lacks original execution evidence".into(),
                        )
                    })?,
                output
                    .products
                    .as_ref()
                    .and_then(|products| products.typed_entry.as_ref()),
                &output
                    .products
                    .as_ref()
                    .expect("program output was sealed")
                    .pending_imports,
            )?;
            let next = completed.append(native.clone())?;
            let observation = CellProgramObservations {
                turn: output.turn,
                metadata: output.metadata,
                products: output.products.expect("program output was sealed"),
            };
            items.push(CellProgramItem {
                checked: item,
                native: Some(native),
                native_observations: Some(observation),
            });
            prefix = Some(next);
        }
        if !outputs.is_empty() {
            return Err(CompileError::ExtractFailed(
                "program contains unowned prepared targets".into(),
            ));
        }
        let program = Arc::new(CellProgram {
            checked: cell,
            parsed: planned.parsed_plan.clone(),
            slots: planned.slots.clone(),
            items,
        });
        for (offer, segment) in pending_segments {
            offer.publish_segment_original_products(&segment, &mut validation)?;
        }
        Ok(program)
    }

    fn program_offer(&self, exact: crate::declaration_context::ExactCompilationRequest) -> Self {
        Self {
            selected: self.selected.clone(),
            producer: self.producer.clone(),
            include: self.include.clone(),
            exact: Some(exact.with_source_search_context(&self.include)),
            checked_cell: None,
            planned_cell: None,
            checked_values: None,
            checked_projections: Vec::new(),
            checked: None,
            selected_session_values: self.selected_session_values.clone(),
        }
    }

    fn admit_segment_original_products(
        &self,
        root: &Path,
        segment: &mut crate::declaration_context::ExactProgramSegmentAdmission,
        produced: &crate::checked_cell::ProducedValueTypeInterfaces,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<(), CompileError> {
        let request = self.exact.as_ref().ok_or_else(|| {
            CompileError::ExtractFailed("segment originals lack exact request".into())
        })?;
        let source_paths = segment
            .admissions()
            .iter()
            .filter(|source| source.generated_source_owner().is_ok())
            .map(|source| source.witness.source_path())
            .collect::<BTreeSet<_>>();
        let source_paths = source_paths.into_iter().collect::<Vec<_>>();
        let source_path = match source_paths.as_slice() {
            [path] => path.to_path_buf(),
            _ => {
                return Err(CompileError::ExtractFailed(
                    "segment originals lack one generated source".into(),
                ))
            }
        };
        let source = std::fs::read_to_string(&source_path)?;
        let products = CompilerSidecar::ModuleProducts.read(root, &validation.inventory)?;
        let packages = CompilerSidecar::ModulePackageImports.read(root, &validation.inventory)?;
        let evidence = CompilerSidecar::Dependencies.read(root, &validation.inventory)?;
        let receipt = CompilerSidecar::CertifiedProducts.read(root, &validation.inventory)?;
        timing::record_inventory_work("segment.originals.before", root, &validation.inventory);
        let parsed = certified_products::ParsedModuleProducts::decode_with_operation(
            &products,
            &packages,
            validation.inventory.clone(),
        )
        .map_err(compiler_evidence_failure)?;
        let receipt = certified_products::decode_segment_originals_with_operation(
            &receipt,
            root,
            &validation.inventory,
        )
        .map_err(compiler_evidence_failure)?;
        let exact = segment.product_admission(request, &source_path, &source, &evidence)?;
        ensure_ready_module_inventory(&receipt.modules, &exact.source.evidence)?;
        let originals = certified_products::CertifiedSegmentOriginals::issue(
            parsed,
            receipt,
            &evidence,
            &source_path,
            root,
            &source,
            &self.producer,
            &self.include,
            self.selected.as_deref(),
            &exact,
            self.selected_session_values
                .get()
                .map_or(&[], Vec::as_slice),
            Some(produced),
            validation,
        )
        .map_err(compiler_evidence_failure)?;
        timing::record_inventory_work("segment.originals.after", root, &validation.inventory);
        segment.install_original_products(originals)
    }

    fn publish_segment_original_products(
        &self,
        segment: &crate::declaration_context::ExactProgramSegmentAdmission,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<(), CompileError> {
        let request = segment.physical_request();
        let originals = segment.original_products(request, &validation.inventory)?;
        let source = segment
            .admissions()
            .iter()
            .find(|source| source.generated_source_owner().is_ok())
            .ok_or_else(|| {
                CompileError::ExtractFailed("segment publication lacks generated source".into())
            })?;
        let bytes = std::fs::read_to_string(source.witness.source_path())?;
        if !source
            .witness
            .matches_source(source.witness.source_path(), &bytes)
            || source.evidence.revalidate(&bytes).is_err()
        {
            return Err(CompileError::ExtractFailed(
                "segment source changed before publication".into(),
            ));
        }
        if empty_exact_context(request.context()) {
            let parsed = originals
                .raw()
                .copy_for_publication()
                .map_err(compiler_evidence_failure)?;
            let (_, publication) = module_candidates::prepare_publication(
                &self.producer,
                &self.include,
                &source.evidence,
                parsed,
                &bytes,
                module_candidates::CandidateVersionOrigin::Exact {
                    semantic_sha256: request.semantic_sha256,
                },
                originals.recovery_products(),
            );
            module_candidates::publish_prepared(publication);
        } else {
            module_candidates::record_exact_context_publication_skip(originals.raw().products());
        }
        module_candidates::record_deployment_acceptance(
            self.selected.as_deref(),
            originals.receipt(),
        );
        Ok(())
    }

    fn read_program_output(
        &self,
        directory: &Path,
        source_directory: &Path,
        produced_types: &crate::checked_cell::ProducedValueTypeInterfaces,
        typed_segments: &[crate::checked_cell::CheckedTypedSegmentPlan],
        item_index: usize,
        admission_digest: [u8; 32],
        source_segment: &crate::declaration_context::ExactProgramSegmentAdmission,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<ProgramNativeOutput, CompileError> {
        let turn: Arc<[u8]> =
            crate::checked_cell::read(directory.join("turn.cbor"), 32 << 20)?.into();
        let value = crate::checked_cell::decode(&turn)?;
        let record = crate::checked_cell::row(&value, 2)?;
        if crate::checked_cell::string(&record[0])? != "Bind" {
            return Err(CompileError::ExtractFailed(
                "program output is not a bind".into(),
            ));
        }
        let source =
            crate::checked_cell::string(&crate::checked_cell::row(&record[1], 5)?[4])?.to_owned();
        let mut output = read_native_turn_artifacts(directory, turn, source)?;
        let module = extract_module_name(&output.source).ok_or_else(|| {
            CompileError::ExtractFailed("program native source has no module owner".into())
        })?;
        output.products = Some(Arc::new(
            seal_turn_outputs_with_validation(
                self,
                directory,
                // Entries are projections of one compiled segment. Its exact
                // receipt binds the consumed source path, not the item copy.
                &source_directory.join(format!("{module}.hs")),
                &output.source,
                &output.target,
                "__prepared",
                None,
                None,
                OriginalOutputPublication::Transaction,
                Some(produced_types),
                Some((typed_segments, item_index, admission_digest)),
                Some(source_segment),
                validation,
            )?
            .ok_or_else(|| {
                CompileError::ExtractFailed("program native products are not sealed".into())
            })?,
        ));
        Ok(output)
    }

    fn admit_program_support(
        &self,
        request: &mut crate::declaration_context::ExactCompilationRequest,
        context: Arc<crate::declaration_join::ExactDeclarationContext>,
        output: &ProgramNativeOutput,
        source_segment: &crate::declaration_context::ExactProgramSegmentAdmission,
        produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
    ) -> Result<Arc<crate::declaration_join::ExactDeclarationContext>, CompileError> {
        let generated =
            crate::declaration_context::ExactSourceAdmission::matching_generated_source_owner(
                source_segment.admissions(),
                &output.source,
            )?;
        let support = program_support_artifacts(
            &output
                .products
                .as_ref()
                .expect("program output was sealed")
                .artifact_view,
            &generated,
        )?;
        let sealed = output.products.as_ref().expect("program output was sealed");
        let private = crate::declaration_context::OriginalCompilerInputs::from_selection(
            &sealed.source_selection,
            &sealed.artifact_view,
        )?
        .for_program_continuation(
            &support,
            source_segment.admissions(),
            &output.source,
        )?;
        let mut continued = request.in_program_context_with_private_input(
            &output.directory.join("private-compiler-inputs"),
            context.clone(),
            &private,
        )?;
        let published = continued.admit_program_segment_support_with_selection(
            context,
            &support,
            source_segment,
            produced_types,
            &output
                .products
                .as_ref()
                .expect("program output was sealed")
                .source_selection,
        )?;
        *request = continued;
        Ok(published)
    }

    fn admit_program_value(
        &self,
        context: Arc<crate::declaration_join::ExactDeclarationContext>,
        root: &Path,
        generation: u64,
        turn: &[u8],
    ) -> Result<Arc<crate::declaration_join::ExactDeclarationContext>, CompileError> {
        let value = crate::checked_cell::decode(turn)?;
        let record = crate::checked_cell::row(&value, 2)?;
        let fields = crate::checked_cell::row(&record[1], 5)?;
        if matches!(&fields[2], Value::Array(rows) if rows.is_empty()) {
            return Ok(context);
        }
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation));
        let path = root.join(owner.relative_hi_path());
        let bytes = crate::checked_cell::read(&path, 32 << 20)?;
        let interface = crate::checked_cell::certify_value_interface(
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                &self.producer,
            )
            .sha256(),
            owner,
            &path,
            &bytes,
        )?;
        Ok(Arc::new(
            (*context)
                .clone()
                .extend_program_value_interface(interface)?,
        ))
    }

    fn admit_planned_declaration(
        &self,
        root: &Path,
        exact: &crate::declaration_context::ExactCompilationRequest,
        specification: &crate::checked_cell::CheckedCellSpecification,
        produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Option<crate::checked_cell::PlannedCheckedDeclaration>, CompileError> {
        use sha2::{Digest, Sha256};
        let receipt_path = root.join("planned-declaration.cbor");
        if !receipt_path.exists() {
            return Ok(None);
        }
        let bytes = crate::checked_cell::read(&receipt_path, 16 * 1024 * 1024)?;
        let receipt = crate::checked_cell::decode(&bytes)?;
        let fields = crate::checked_cell::row(&receipt, 8)?;
        let string = crate::checked_cell::string;
        let fail = || {
            CompileError::ExtractFailed(
                "planned declaration differs from its same compiler offer".into(),
            )
        };
        let module_name = string(&fields[3])?;
        let module = module_name
            .strip_prefix("Tidepool.Session.Lib.G")
            .and_then(|generation| generation.parse::<u64>().ok())
            .filter(|generation| *generation > 0)
            .map(|generation| {
                tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation))
            })
            .ok_or_else(fail)?;
        let source = string(&fields[4])?;
        let directory = root.join("planned-declaration");
        let source_path = directory.join(format!("{module_name}.hs"));
        if string(&fields[0])? != "TPEXACTDECL"
            || string(&fields[1])? != "1"
            || string(&fields[2])? != exact.request_sha256
            || specification.reserved_declaration_modules.as_slice() != [module_name]
            || module.module_name() != module_name
            || string(&fields[7])? != "planned-declaration"
            || extract_module_name(source).as_deref() != Some(module_name)
            || crate::checked_cell::read(&source_path, 32 * 1024 * 1024)? != source.as_bytes()
        {
            return Err(fail());
        }
        // The exact original source transaction was issued before enriching the
        // checking scope. It therefore validates only against the captured baseline.
        let admission = exact.admit_source(
            &source_path,
            source,
            &crate::checked_cell::read(directory.join("dependencies.json"), 32 * 1024 * 1024)?,
        )?;
        let requirements = crate::prepared_artifact::production_requirements()?;
        let target = Arc::new(
            tidepool_repr::execution_schema::parse_program(
                &crate::checked_cell::read(
                    directory.join("__result.prepared.cbor"),
                    128 * 1024 * 1024,
                )?,
                &requirements,
                DecodeLimits::default(),
            )
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?,
        );
        let authored = crate::declaration_join::NativeAuthoredDeclarationAdmission::from_planned(
            &module, &admission,
        )?;
        let sealed = seal_turn_outputs_with_validation(
            self,
            &directory,
            &source_path,
            source,
            &target,
            "__result",
            None,
            Some(&authored),
            OriginalOutputPublication::Transaction,
            produced_types,
            None,
            None,
            validation,
        )?
        .ok_or_else(fail)?;
        let original_owner = admission.generated_source_owner()?;
        if original_owner.module != module_name {
            return Err(fail());
        }
        let selected_originals = sealed
            .source_selection
            .selected_authored_original_closure(
                &sealed.artifact_view,
                &original_owner.unit,
                &original_owner.module,
            )
            .map_err(compiler_evidence_failure)?;
        let products = selected_originals
            .products()
            .iter()
            .filter(|product| product.owner().module == module_name)
            .collect::<Vec<_>>();
        let [original] = products.as_slice() else {
            return Err(fail());
        };
        let interface = original.module_interface().ok_or_else(fail)?;
        let iface = interface.interface_bytes();
        let digest = crate::checked_cell::hash(iface);
        if string(&fields[5])? != digest || original.interface_bytes() != iface {
            return Err(fail());
        }
        let baseline = exact.context();
        let empty = baseline.recovery_products().is_empty()
            && baseline.joined_interfaces().is_empty()
            && baseline.lexical_graph().is_empty()
            && baseline.interface_owners().is_empty();
        let certificate = crate::declaration_join::certify_same_offer_planned_declaration(
            module,
            source,
            &self.producer,
            &selected_originals,
            &sealed.artifact_view,
            &crate::declaration_context::ExactProductAdmission {
                request: exact,
                source: &admission,
            },
            &self.include,
            (!empty).then_some(baseline),
            string(&fields[6])?.as_bytes(),
        )?;
        let inventory: serde_json::Value = serde_json::from_str(string(&fields[6])?)
            .map_err(|error| CompileError::ExtractFailed(format!("planned inventory: {error}")))?;
        let interface_fingerprint = inventory
            .get("interface_fingerprint")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(fail)?
            .to_owned();
        if std::fs::read(&receipt_path)? != bytes {
            return Err(fail());
        }
        Ok(Some(crate::checked_cell::PlannedCheckedDeclaration {
            source: source.to_owned(),
            interface_fingerprint,
            certificate: Arc::new(certificate),
            receipt_digest: Sha256::digest(&bytes).into(),
            compiler_input: crate::declaration_context::OriginalCompilerInputs::from_selection(
                &sealed.source_selection,
                &sealed.artifact_view,
            )?,
        }))
    }

    pub fn exact_scope_path(&self) -> Option<&Path> {
        self.exact
            .as_ref()
            .map(|request| request.manifest.as_path())
    }

    /// Attach this offer's exact compiler inputs and original native demand
    /// tags. Runtime live-value admission remains a separate authority.
    pub fn apply_to(&self, command: &mut ExtractCmd) -> Result<(), CompileError> {
        if let Some(exact) = &self.exact {
            let retained_policy = match &self.checked {
                Some(NativeCheckedOffer::ActivationPreview(_)) => {
                    crate::declaration_context::RetainedGenerationPolicy::PureActivationPreview
                }
                _ => crate::declaration_context::RetainedGenerationPolicy::PreserveCertifiedDemand,
            };
            exact.apply_to(command, retained_policy)?;
        }
        if let Some(manifest) = self.manifest_path() {
            command.module_candidates(manifest);
        }
        let selected = selected_session_value_modules(command)?;
        if let Some(previous) = self.selected_session_values.get() {
            if previous != &selected {
                return Err(CompileError::ExtractFailed(
                    "compiler offer session selection changed".into(),
                ));
            }
        } else {
            self.selected_session_values.set(selected).map_err(|_| {
                CompileError::ExtractFailed("compiler offer session selection raced".into())
            })?;
        }
        Ok(())
    }

    /// Validate successful source transactions under one actual source parent.
    /// A request using several source directories validates each directory.
    pub fn validate_exact_outputs(
        &self,
        source_directory: &Path,
    ) -> Result<Vec<crate::declaration_join::ExactSourceWitness>, CompileError> {
        let request = self.exact.as_ref().ok_or_else(|| {
            CompileError::ExtractFailed("source-only offer cannot admit exact outputs".into())
        })?;
        Ok(request
            .validate_outputs(source_directory)?
            .into_iter()
            .map(|source| source.witness)
            .collect())
    }

    pub fn manifest_path(&self) -> Option<&Path> {
        self.selected
            .as_ref()
            .map(|set| set.manifest_path.as_path())
    }

    pub fn has_candidates(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|set| !set.by_owner.is_empty())
    }
}

#[derive(Debug)]
struct ProgramNativeOutput {
    directory: PathBuf,
    target: Arc<PreparedProgram>,
    turn: Arc<[u8]>,
    metadata: Arc<[u8]>,
    products: Option<Arc<SealedTurnProducts>>,
    source: String,
}

fn read_native_turn_artifacts(
    directory: &Path,
    turn: Arc<[u8]>,
    source: String,
) -> Result<ProgramNativeOutput, CompileError> {
    let prepared_read_start = Instant::now();
    let prepared_bytes =
        crate::checked_cell::read(directory.join("__prepared.prepared.cbor"), 128 << 20)?;
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_PREPARED_READ,
        prepared_read_start.elapsed(),
        prepared_bytes.len() as u64,
    );
    let prepared_decode_start = Instant::now();
    let target = Arc::new(tidepool_repr::execution_schema::parse_program(
        &prepared_bytes,
        &crate::prepared_artifact::production_requirements()?,
        DecodeLimits::default(),
    )?);
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        "native.prepared_decode",
        prepared_decode_start.elapsed(),
        prepared_bytes.len() as u64,
    );
    let metadata_read_start = Instant::now();
    let metadata: Arc<[u8]> =
        crate::checked_cell::read(directory.join("meta.cbor"), 32 << 20)?.into();
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_CBOR_READ,
        metadata_read_start.elapsed(),
        metadata.len() as u64,
    );
    Ok(ProgramNativeOutput {
        directory: directory.to_owned(),
        target,
        turn,
        metadata,
        products: None,
        source,
    })
}

/// Immutable native products and observations retained by the execution owner.
/// Reading the diagnostic directory cannot change this compiled bundle.
#[derive(Debug)]
pub struct NativeTurnOutput {
    output: ProgramNativeOutput,
    table: DataConTable,
    warnings: MetaWarnings,
}

impl NativeTurnOutput {
    pub fn target_owned(&self) -> Arc<PreparedProgram> {
        self.output.target.clone()
    }
    pub fn table(&self) -> &DataConTable {
        &self.table
    }
    pub fn warnings(&self) -> &MetaWarnings {
        &self.warnings
    }
    pub fn turn_bytes(&self) -> &[u8] {
        &self.output.turn
    }
    pub fn metadata_bytes(&self) -> &[u8] {
        &self.output.metadata
    }
    pub fn products(&self) -> Option<&SealedTurnProducts> {
        self.output.products.as_deref()
    }
    pub fn source(&self) -> &str {
        &self.output.source
    }
}

#[derive(Debug)]
pub struct SealedTurnProducts {
    // Issued for this exact output before support selection; later program
    // items cannot enlarge an earlier item's original instance environment.
    original_execution: Option<Arc<crate::declaration_context::ExactDeclarationContext>>,
    pub artifact_view: crate::artifact_inventory::ArtifactView,
    pub(crate) source_selection: certified_products::CertifiedSourceSelection,
    typed_entry: Option<crate::checked_cell::CheckedTypedEntry>,
    pub original_compile_input: Option<Arc<SealedOriginalCompileInput>>,
    pub certified_groups: Arc<[certified_products::PendingCertifiedGroup]>,
    pub pending_imports: Vec<certified_products::PendingImportOwner>,
    pub recovery_products: Arc<[crate::recovery_artifacts::CertifiedRecoveryProduct]>,
    pub(crate) retained_core_products: certified_products::CertifiedRetainedCoreProducts,
    pub package_interfaces: certified_products::CertifiedTargetPackageInterfaces,
    pub checked: Option<CheckedNativeProof>,
}

/// One compiler-issued checked native purpose. Its original proof remains the
/// authority for the output; bundle identity is independent evidence.
#[derive(Clone, Debug)]
pub enum CheckedNativeProof {
    Execution(Arc<crate::checked_cell::ExactCompiledItem>),
    ActivationPreview(Arc<crate::activation_preview::ExactCompiledActivationPreview>),
}

fn has_ready_home_module(evidence: &cache::DependencyEvidence) -> bool {
    evidence
        .modules
        .iter()
        .any(|module| !module.boot && module.product == cache::ProductAvailability::Ready)
}

fn ensure_ready_module_inventory(
    modules: &[certified_products::CertifiedModuleReceipt],
    evidence: &cache::DependencyEvidence,
) -> Result<(), CompileError> {
    let seen: std::collections::HashSet<_> = modules
        .iter()
        .map(|module| (module.unit.as_str(), module.module.as_str()))
        .collect();
    for module in &evidence.modules {
        if !module.boot
            && module.product == cache::ProductAvailability::Ready
            && !seen.contains(&(module.unit.as_str(), module.module.as_str()))
        {
            return Err(CompileError::ExtractFailed(format!(
                "ready module {}:{} lacks certified product",
                module.unit, module.module,
            )));
        }
    }
    Ok(())
}

fn ensure_no_uncertified_globals(artifacts: &CompiledArtifacts) -> Result<(), CompileError> {
    if artifacts
        .targets
        .values()
        .any(|target| !target.prepared.prepared().globals().is_empty())
    {
        return Err(CompileError::ExtractFailed(
            "prepared target globals require certified owners".into(),
        ));
    }
    Ok(())
}

/// Outputs of one actual admitted execution. The source and native proof were
/// sealed before this owning directory became observable by the caller.
#[derive(Debug)]
pub struct AdmittedTurnOutput {
    directory: TempDir,
    run: tidepool_extract_cmd::ExtractRun,
    turn: Option<Arc<[u8]>>,
    native: Option<NativeTurnOutput>,
}

impl AdmittedTurnOutput {
    pub fn directory(&self) -> &Path {
        self.directory.path()
    }
    pub fn compiler_output(&self) -> &std::process::Output {
        &self.run.output
    }
    pub fn elapsed(&self) -> Duration {
        self.run.elapsed
    }
    pub fn original_compile_input(&self) -> Option<&Arc<SealedOriginalCompileInput>> {
        self.native
            .as_ref()?
            .products()?
            .original_compile_input
            .as_ref()
    }
    pub fn native_output(&self) -> Option<&NativeTurnOutput> {
        self.native.as_ref()
    }
    pub fn turn_bytes(&self) -> Option<&[u8]> {
        self.turn.as_deref()
    }
}

/// Seal the exact worker-authored source and product sidecars of a successful
/// resident turn. The runtime supplies the selected template's source path
/// and the source text echoed by TurnOut, not the unspliced cell text.
pub fn seal_turn_outputs(
    offer: &ModuleCandidateOffer,
    output_dir: &Path,
    source_path: &Path,
    source: &str,
    prepared: &Arc<PreparedProgram>,
    target: &str,
) -> Result<Option<SealedTurnProducts>, CompileError> {
    seal_turn_outputs_inner(
        offer,
        output_dir,
        source_path,
        source,
        prepared,
        target,
        None,
        None,
        OriginalOutputPublication::Transaction,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OriginalOutputPublication {
    Transaction,
    RetainedEntry,
}

#[derive(Clone, Copy)]
enum CompilerSidecar {
    ModuleProducts,
    ModulePackageImports,
    Dependencies,
    CertifiedProducts,
}

impl CompilerSidecar {
    fn filename(self) -> &'static str {
        match self {
            Self::ModuleProducts => "module-products.cbor",
            Self::ModulePackageImports => "module-package-imports.cbor",
            Self::Dependencies => "dependencies.json",
            Self::CertifiedProducts => "certified-products.cbor",
        }
    }

    fn read(
        self,
        root: &Path,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<Vec<u8>, CompileError> {
        let limit = match self {
            Self::Dependencies => {
                certified_products::COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_bytes)
            }
            Self::ModuleProducts | Self::ModulePackageImports | Self::CertifiedProducts => {
                operation.limits().max_bytes
            }
        };
        certified_products::read_bounded_with_operation(
            &root.join(self.filename()),
            limit as u64,
            operation,
        )
        .map_err(compiler_evidence_failure)
    }
}

fn compiler_evidence_failure(error: certified_products::CertificationError) -> CompileError {
    CompileError::CompilerEvidence(Box::new(error))
}

#[allow(clippy::too_many_arguments)]
fn seal_turn_outputs_inner(
    offer: &ModuleCandidateOffer,
    output_dir: &Path,
    source_path: &Path,
    source: &str,
    prepared: &Arc<PreparedProgram>,
    target: &str,
    identity_metadata: Option<(&DataConTable, &[YieldSite])>,
    authored: Option<&crate::declaration_join::NativeAuthoredDeclarationAdmission>,
    publication: OriginalOutputPublication,
) -> Result<Option<SealedTurnProducts>, CompileError> {
    // Standalone turns own their own admission; program turns share theirs.
    seal_turn_outputs_with_validation(
        offer,
        output_dir,
        source_path,
        source,
        prepared,
        target,
        identity_metadata,
        authored,
        publication,
        None,
        None,
        None,
        &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
    )
}

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    target = "exomonad_harness::timing",
    name = "products.seal",
    level = "debug",
    skip_all,
    fields(inclusive = true)
)]
fn seal_turn_outputs_with_validation(
    offer: &ModuleCandidateOffer,
    output_dir: &Path,
    source_path: &Path,
    source: &str,
    prepared: &Arc<PreparedProgram>,
    target: &str,
    identity_metadata: Option<(&DataConTable, &[YieldSite])>,
    authored: Option<&crate::declaration_join::NativeAuthoredDeclarationAdmission>,
    publication: OriginalOutputPublication,
    produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
    typed_item: Option<(
        &[crate::checked_cell::CheckedTypedSegmentPlan],
        usize,
        [u8; 32],
    )>,
    source_segment: Option<&crate::declaration_context::ExactProgramSegmentAdmission>,
    validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<Option<SealedTurnProducts>, CompileError> {
    if std::fs::read_to_string(source_path)? != source {
        return Err(CompileError::ExtractFailed(
            "turn source changed after worker compile".into(),
        ));
    }
    let segment_originals = source_segment
        .map(|segment| {
            segment.original_products(
                offer.exact.as_ref().ok_or_else(|| {
                    CompileError::ExtractFailed("segment inventory lacks exact request".into())
                })?,
                &validation.inventory,
            )
        })
        .transpose()?;
    let receipt_bytes =
        CompilerSidecar::CertifiedProducts.read(output_dir, &validation.inventory)?;
    let ordinary_inputs = if source_segment.is_none() {
        let product_bytes =
            CompilerSidecar::ModuleProducts.read(output_dir, &validation.inventory)?;
        let packages =
            CompilerSidecar::ModulePackageImports.read(output_dir, &validation.inventory)?;
        let evidence = CompilerSidecar::Dependencies.read(output_dir, &validation.inventory)?;
        timing::record_inventory_work("products.decode.before", output_dir, &validation.inventory);
        let start = Instant::now();
        let products = certified_products::ParsedModuleProducts::decode_with_operation(
            &product_bytes,
            &packages,
            validation.inventory.clone(),
        )
        .map_err(compiler_evidence_failure)?;
        timing::record_inventory_work("products.decode.after", output_dir, &validation.inventory);
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            "products.decode",
            start.elapsed(),
            product_bytes.len() as u64,
        );
        Some((products, evidence))
    } else {
        None
    };
    let exact_source_owned = if source_segment.is_none() {
        offer
            .exact
            .as_ref()
            .map(|request| {
                request.admit_source_with_validation(
                    source_path,
                    source,
                    &ordinary_inputs.as_ref().expect("ordinary products").1,
                    validation,
                )
            })
            .transpose()?
    } else {
        None
    };
    let exact = match source_segment {
        Some(segment) => Some(segment.item_admission(
            offer.exact.as_ref().expect("segment exact request"),
            source_path,
            source,
        )?),
        None => offer
            .exact
            .as_ref()
            .zip(exact_source_owned.as_ref())
            .map(
                |(request, source)| crate::declaration_context::ExactProductAdmission {
                    request,
                    source,
                },
            ),
    };
    let exact_source = exact.as_ref().map(|admission| admission.source);
    let evidence_bytes = match exact_source.filter(|_| source_segment.is_some()) {
        Some(source) => source.evidence_bytes.as_slice(),
        None => ordinary_inputs
            .as_ref()
            .expect("ordinary products")
            .1
            .as_slice(),
    };
    let ordinary_evidence = if exact_source.is_none() {
        Some(
            cache::CompletedSourceEvidence::from_worker(evidence_bytes, source_path, source)
                .map_err(|failure| {
                    CompileError::CompilerEvidence(Box::new(
                        crate::certified_products::CertificationError::CompletedSourceEvidence {
                            input: source_path.to_path_buf(),
                            failure: Box::new(failure),
                        },
                    ))
                })?,
        )
    } else {
        None
    };
    let evidence = exact_source
        .map(|source| source.evidence.as_ref())
        .or(ordinary_evidence.as_ref());
    let fresh_products = segment_originals
        .map(|originals| originals.raw())
        .or_else(|| ordinary_inputs.as_ref().map(|(products, _)| products))
        .ok_or_else(|| CompileError::ExtractFailed("original inventory absent".into()))?;
    let needs_certificate = offer.has_candidates()
        || offer.exact.is_some()
        || !fresh_products.products().is_empty()
        || !prepared.globals().is_empty()
        || evidence.is_some_and(|evidence| has_ready_home_module(evidence));
    if receipt_bytes.is_empty() {
        if needs_certificate {
            return Err(CompileError::ExtractFailed(
                "turn required product certificate unavailable".into(),
            ));
        }
        return Ok(None);
    }
    let ordinary_receipt = if source_segment.is_none() {
        Some(
            certified_products::decode_receipt_with_operation(
                &receipt_bytes,
                Some(output_dir),
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?,
        )
    } else {
        None
    };
    let valid =
        evidence.filter(|evidence| source_segment.is_some() || evidence.revalidate(source).is_ok());
    let Some(valid) = valid else {
        if needs_certificate
            || ordinary_receipt.as_ref().is_some_and(|receipt| {
                receipt
                    .modules
                    .iter()
                    .any(|module| module.origin == certified_products::ProductOrigin::Cached)
            })
        {
            return Err(CompileError::ExtractFailed(
                "turn required product certificate lacks valid final dependency evidence".into(),
            ));
        }
        return Ok(None);
    };
    let (mut certified, item_globals, item_packages) = match segment_originals {
        Some(originals) => {
            let item = certified_products::decode_segment_item_with_operation(
                &receipt_bytes,
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?;
            let (products, globals, packages) = originals
                .select_item(
                    item,
                    prepared,
                    offer
                        .selected_session_values
                        .get()
                        .map_or(&[], Vec::as_slice),
                    Some(produced_types.ok_or_else(|| {
                        CompileError::ExtractFailed(
                            "segment item lacks its checked type selection".into(),
                        )
                    })?),
                    validation,
                )
                .map_err(compiler_evidence_failure)?;
            (products, Some(globals), Some(packages))
        }
        None => {
            validation
                .inventory
                .charge(evidence_bytes.len().checked_mul(64).ok_or_else(|| {
                    CompileError::ExtractFailed("source evidence accounting overflow".into())
                })?)
                .map_err(|error| compiler_evidence_failure(error.into()))?;
            timing::record_inventory_work(
                "products.certify.before",
                output_dir,
                &validation.inventory,
            );
            let products = certified_products::certify_products_with_validation(
                offer.selected.as_deref(),
                ordinary_receipt.as_ref().expect("ordinary receipt"),
                fresh_products,
                evidence_bytes,
                source_path,
                output_dir,
                valid,
                source,
                &offer.producer,
                &offer.include,
                exact.as_ref(),
                authored,
                offer
                    .selected_session_values
                    .get()
                    .map_or(&[], Vec::as_slice),
                produced_types,
                validation,
            )
            .map_err(compiler_evidence_failure)?;
            timing::record_inventory_work(
                "products.certify.after",
                output_dir,
                &validation.inventory,
            );
            (products, None, None)
        }
    };
    let target_admission_start = Instant::now();
    let accepted = if let Some(globals) = &item_globals {
        globals.as_slice()
    } else {
        let receipt = ordinary_receipt.as_ref().expect("ordinary receipt");
        ensure_ready_module_inventory(&receipt.modules, valid)?;
        if receipt.targets.len() != 1 {
            return Err(CompileError::ExtractFailed(
                "turn target product receipt count".into(),
            ));
        }
        receipt
            .targets
            .get(target)
            .ok_or_else(|| {
                CompileError::ExtractFailed("turn target product receipt missing".into())
            })?
            .as_slice()
    };
    let receipt_packages = item_packages
        .as_ref()
        .or_else(|| ordinary_receipt.as_ref().map(|receipt| &receipt.packages))
        .expect("admitted package selection");
    let package_catalog = match segment_originals {
        Some(originals) => segment_package_availability(originals, receipt_packages),
        None => package_availability_with_validation(receipt_packages, &certified, validation)?,
    };
    enum TargetDemand {
        Checked {
            entry: crate::checked_cell::CheckedTypedEntry,
            imports: Vec<certified_products::PendingImportOwner>,
        },
        Ordinary(Vec<certified_products::PendingImportOwner>),
    }
    let original_sources = match segment_originals {
        Some(originals) => originals.original_sources().clone(),
        None => certified_products::AvailableOriginalSources::authenticate(
            &certified.recovery_products,
            &validation.inventory,
        )
        .map_err(compiler_evidence_failure)?,
    };
    // Shared original membership already authenticates every current group.
    // The per-item lookup needs only that immutable index; execution selection
    // is still admitted below from the checked entry and exact target imports.
    let selected_membership = if segment_originals.is_some() {
        &[][..]
    } else {
        certified.groups.as_ref()
    };
    let target_demand = match typed_item {
        Some((plans, index, admission_digest)) => {
            let admission = exact.as_ref().ok_or_else(|| {
                CompileError::ExtractFailed("typed entry lacks exact source admission".into())
            })?;
            let entry = crate::checked_cell::CheckedTypedEntry::issue_program_item(
                output_dir,
                &admission.request.request_sha256,
                admission_digest,
                plans,
                index,
                source,
                prepared,
                &admission.source.generated_source_owner()?,
                &certified.recovery_products,
                crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                    &offer.producer,
                )
                .sha256(),
            )?;
            let imports = certified_products::certify_target_available_owners_from_membership_with_validation(
                prepared,
                accepted,
                &original_sources,
                selected_membership,
                &certified.source_selection,
                &package_catalog,
                validation,
            )
            .map_err(compiler_evidence_failure)?;
            TargetDemand::Checked { entry, imports }
        }
        None => TargetDemand::Ordinary(
            certified_products::certify_target_available_owners_from_membership_with_validation(
                prepared,
                accepted,
                &original_sources,
                selected_membership,
                &certified.source_selection,
                &package_catalog,
                validation,
            )
            .map_err(compiler_evidence_failure)?,
        ),
    };
    let demand = match &target_demand {
        TargetDemand::Checked { entry, imports } => {
            crate::artifact_inventory::NativeArtifactDemand::VerifiedTarget { entry, imports }
        }
        TargetDemand::Ordinary(imports) => {
            crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(imports)
        }
    };
    let compiler_inputs = offer
        .exact
        .as_ref()
        .map(|request| request.compiler_inputs());
    let producer =
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&offer.producer)
            .sha256();
    let artifact_view = match segment_originals {
        Some(originals) => {
            crate::declaration_context::certified_segment_artifact_view_with_validation(
                producer,
                originals,
                &certified.value_interfaces,
                compiler_inputs.as_ref().map(|input| &input.artifacts),
                demand,
                validation,
            )?
        }
        None => crate::declaration_context::certified_product_artifact_view_with_validation(
            producer,
            &certified.recovery_products,
            &certified.module_interfaces,
            &certified.value_interfaces,
            compiler_inputs.as_ref().map(|input| &input.artifacts),
            demand,
            validation,
        )?,
    };
    certified.groups = crate::declaration_context::certify_artifact_view_groups_with_validation(
        &artifact_view,
        &certified.groups,
        offer
            .exact
            .as_ref()
            .map_or(&[], |request| request.groups.as_ref()),
        validation,
    )?
    .into();
    let target_imports = match &target_demand {
        TargetDemand::Checked { imports, .. } | TargetDemand::Ordinary(imports) => {
            imports.as_slice()
        }
    };
    let package_closure =
        package_catalog.select(&certified.groups, target_imports, &validation.inventory)?;
    let typed_entry = match target_demand {
        TargetDemand::Checked { entry, .. } => Some(entry),
        TargetDemand::Ordinary(_) => None,
    };
    let pending_imports = certified_products::certify_target_owners_with_validation(
        prepared,
        accepted,
        &certified.groups,
        &certified.source_selection,
        &package_closure,
        validation,
    )
    .map_err(compiler_evidence_failure)?;
    let package_interfaces = certified_products::certify_target_package_interfaces_with_validation(
        prepared,
        &package_closure,
        validation,
    )
    .map_err(compiler_evidence_failure)?;
    timing::record_stage_with_owners(
        timing::NO_NODE,
        timing::NO_ROUND,
        "products.target_admission",
        target_admission_start.elapsed(),
        0,
        1,
    );
    let post_target_span = tracing::debug_span!(target: "exomonad_harness::timing", "products.post_target", inclusive = true).entered();
    let certified_groups: Arc<[_]> = certified.groups.into();
    let original_compile_input =
        if let Some((table, sites)) = identity_metadata.filter(|_| offer.exact.is_none()) {
            let input_packages = crate::compile_input::ValidatedInputPackages::read_supported(
                &output_dir.join("compiler-inputs.cbor"),
                &evidence_bytes,
                valid,
            )?;
            input_packages
                .map(|input_packages| {
                    crate::compile_input::seal(
                        &offer.producer,
                        &offer.include,
                        valid,
                        &input_packages,
                        source,
                        target,
                        prepared,
                        &certified_groups,
                        &pending_imports,
                        &package_interfaces,
                        table.clone(),
                        sites.to_vec(),
                        &artifact_view,
                        &certified
                            .source_selection
                            .compiler_projection(&artifact_view)
                            .map_err(compiler_evidence_failure)?,
                    )
                })
                .transpose()?
                .flatten()
                .map(Arc::new)
        } else {
            None
        };
    let original_execution_span = tracing::debug_span!(target: "exomonad_harness::timing", "products.original_execution", inclusive = true).entered();
    let original_execution = match exact.as_ref() {
        Some(admission) => Some(admission.original_execution_context(
            &crate::declaration_context::OriginalCompilerInputs::from_selection(
                &certified.source_selection,
                &artifact_view,
            )?,
        )?),
        None => original_compile_input
            .as_ref()
            .map(|proof| proof.issued_original_execution()),
    };
    drop(original_execution_span);
    let checked_span = tracing::debug_span!(target: "exomonad_harness::timing", "products.checked_proof", inclusive = true).entered();
    let checked = if let Some(checked) = &offer.checked {
        let (context, lexical) = checked_output_context(
            offer,
            &artifact_view,
            exact_source
                .as_ref()
                .expect("checked output has exact source"),
            produced_types,
            &certified.source_selection,
        )?;
        Some(match checked {
            NativeCheckedOffer::ActivationPreview(preview) => {
                CheckedNativeProof::ActivationPreview(
                    preview.seal(
                        output_dir,
                        &offer
                            .exact
                            .as_ref()
                            .expect("preview has exact scope")
                            .request_sha256,
                        source,
                        prepared,
                        &context,
                    )?,
                )
            }
            NativeCheckedOffer::Item(item) => {
                let request_sha256 = &offer
                    .exact
                    .as_ref()
                    .expect("checked offer has exact scope")
                    .request_sha256;
                CheckedNativeProof::Execution(
                    item.seal(
                        output_dir,
                        request_sha256,
                        source,
                        prepared,
                        &context,
                        &lexical,
                        original_execution
                            .clone()
                            .expect("checked output has full original execution evidence"),
                        None,
                        &pending_imports,
                    )?,
                )
            }
        })
    } else {
        None
    };
    drop(checked_span);
    drop(post_target_span);
    let _publication_span = tracing::debug_span!(target: "exomonad_harness::timing", "products.publication", inclusive = true).entered();
    if source_segment.is_none()
        && publication == OriginalOutputPublication::Transaction
        && offer
            .exact
            .as_ref()
            .is_none_or(|exact| empty_exact_context(exact.context()))
    {
        let publication_products = ordinary_inputs.expect("ordinary product inputs").0;
        let (_, publication) = module_candidates::prepare_publication(
            &offer.producer,
            &offer.include,
            valid,
            publication_products,
            source,
            exact.as_ref().map_or(
                module_candidates::CandidateVersionOrigin::Ordinary,
                |admission| module_candidates::CandidateVersionOrigin::Exact {
                    semantic_sha256: admission.request.semantic_sha256,
                },
            ),
            &certified.recovery_products,
        );
        module_candidates::publish_prepared(publication);
    } else if source_segment.is_none() && publication == OriginalOutputPublication::Transaction {
        module_candidates::record_exact_context_publication_skip(fresh_products.products());
    }
    if let Some(receipt) =
        ordinary_receipt.filter(|_| publication == OriginalOutputPublication::Transaction)
    {
        module_candidates::record_deployment_acceptance(offer.selected.as_deref(), &receipt);
    }
    Ok(Some(SealedTurnProducts {
        artifact_view,
        typed_entry,
        original_execution,
        original_compile_input,
        checked,
        certified_groups,
        pending_imports,
        recovery_products: certified.recovery_products.clone(),
        retained_core_products: certified.retained_core_products,
        source_selection: certified.source_selection,
        package_interfaces,
    }))
}

/// Preserve all compiler-issued support carriers while excluding the exact
/// generated scaffold authenticated by the consumed source receipt.
fn program_support_artifacts(
    artifacts: &crate::artifact_inventory::ArtifactView,
    generated: &crate::declaration_join::ExactModuleIdentity,
) -> Result<crate::artifact_inventory::ArtifactView, CompileError> {
    let support = artifacts.select_roots(
        artifacts
            .descriptors()
            .into_iter()
            .filter(|entry| &entry.owner != generated)
            .map(|entry| entry.id)
            .collect(),
    )?;
    if support
        .descriptors()
        .iter()
        .any(|entry| &entry.owner == generated)
    {
        return Err(CompileError::ExtractFailed(
            "program support depends on its generated scaffold".into(),
        ));
    }
    Ok(support)
}

fn checked_output_context(
    offer: &ModuleCandidateOffer,
    artifacts: &crate::artifact_inventory::ArtifactView,
    source_admission: &crate::declaration_context::ExactSourceAdmission,
    produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
    selection: &certified_products::CertifiedSourceSelection,
) -> Result<
    (
        Arc<crate::declaration_join::ExactDeclarationContext>,
        Vec<crate::declaration_join::ExactLexicalNode>,
    ),
    CompileError,
> {
    let exact = offer.exact.as_ref().ok_or_else(|| {
        CompileError::ExtractFailed("checked output lacks current exact context".into())
    })?;
    let generated = source_admission.generated_source_owner()?;
    let support = program_support_artifacts(artifacts, &generated)?;
    let mut request = exact.clone();
    let context = request.admit_program_support_with_selection(
        exact.context().clone(),
        &support,
        std::slice::from_ref(source_admission),
        produced_types,
        selection,
    )?;
    Ok((context, request.program_source_lexical().to_vec()))
}

struct PackageInterfaceCatalog<'a> {
    current: &'a BTreeMap<(String, String), certified_products::PackageInterfaceWitness>,
    interfaces: std::borrow::Cow<
        'a,
        BTreeMap<(String, String), certified_products::PackageInterfaceWitness>,
    >,
}

impl certified_products::PackageInterfaceLookup for PackageInterfaceCatalog<'_> {
    fn get(
        &self,
        owner: &(String, String),
    ) -> Option<&certified_products::PackageInterfaceWitness> {
        self.current
            .get(owner)
            .or_else(|| self.interfaces.get(owner))
    }
}

impl PackageInterfaceCatalog<'_> {
    /// Availability supports owner admission; only authenticated target imports
    /// and selected native imports transfer inherited package authority.
    fn select(
        &self,
        groups: &[certified_products::PendingCertifiedGroup],
        target_imports: &[certified_products::PendingImportOwner],
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<BTreeMap<(String, String), certified_products::PackageInterfaceWitness>, CompileError>
    {
        let mut demanded = BTreeMap::new();
        for import in groups
            .iter()
            .flat_map(|group| group.imports())
            .chain(target_imports)
        {
            let (unit, module, digest) = match import {
                certified_products::PendingImportOwner::Package {
                    unit,
                    module,
                    interface_digest,
                    ..
                }
                | certified_products::PendingImportOwner::RetainedPackage {
                    unit,
                    module,
                    interface_digest,
                    ..
                } => (unit, module, interface_digest),
                _ => continue,
            };
            operation
                .charge(unit.len() + module.len())
                .map_err(|error| compiler_evidence_failure(error.into()))?;
            let owner = (unit.clone(), module.clone());
            if !demanded.contains_key(&owner) {
                operation
                    .reserve::<((String, String), [u8; 32], [usize; 4])>(1)
                    .map_err(|error| compiler_evidence_failure(error.into()))?;
            }
            if demanded
                .insert(owner, *digest)
                .is_some_and(|old| old != *digest)
            {
                return Err(CompileError::ExtractFailed(
                    "target package demand has conflicting interface digests".into(),
                ));
            }
        }
        for (owner, witness) in self.current {
            certified_products::charge_package_copy(owner, witness, operation)
                .map_err(compiler_evidence_failure)?;
        }
        let mut selected = self.current.clone();
        for (owner, digest) in demanded {
            let witness = self
                .current
                .get(&owner)
                .or_else(|| self.interfaces.get(&owner))
                .filter(|witness| witness.sha256 == digest)
                .ok_or_else(|| {
                    CompileError::ExtractFailed(
                        "target package demand lacks matching available interface".into(),
                    )
                })?;
            if !selected.contains_key(&owner) {
                certified_products::charge_package_copy(&owner, witness, operation)
                    .map_err(compiler_evidence_failure)?;
                selected.insert(owner, witness.clone());
            }
        }
        Ok(selected)
    }
}

fn segment_package_availability<'a>(
    originals: &'a certified_products::CertifiedSegmentOriginals,
    additions: &'a BTreeMap<(String, String), certified_products::PackageInterfaceWitness>,
) -> PackageInterfaceCatalog<'a> {
    let interfaces = std::borrow::Cow::Borrowed(originals.package_availability());
    PackageInterfaceCatalog {
        current: additions,
        interfaces,
    }
}

fn package_availability_with_validation<'a>(
    packages: &'a BTreeMap<(String, String), certified_products::PackageInterfaceWitness>,
    certified: &certified_products::CertifiedProducts,
    validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<PackageInterfaceCatalog<'a>, CompileError> {
    let mut selected = packages.clone();
    let inherited = certified_products::inherited_package_witnesses_with_validation(
        &certified.recovery_products,
        validation,
    )
    .map_err(compiler_evidence_failure)?;
    for (owner, witness) in inherited {
        if selected
            .insert(owner, witness.clone())
            .is_some_and(|old| old != witness)
        {
            return Err(CompileError::ExtractFailed(
                "exact inherited package selection differs from current target".into(),
            ));
        }
    }
    Ok(PackageInterfaceCatalog {
        current: packages,
        interfaces: std::borrow::Cow::Owned(selected),
    })
}

// ---------------------------------------------------------------------------
// The one front door
// ---------------------------------------------------------------------------

/// One typed compiler invocation shared by single-target and batch callers.
pub struct CompileInvocation<'a> {
    pub source: &'a str,
    pub targets: &'a [&'a str],
    pub include: &'a [PathBuf],
    /// Fallback module name (sans `.hs`) when `source` has no `module`
    /// header. GHC derives the module name from the filename
    /// (`capitalize(basename)`); the caller selects the name its source
    /// assembly expects.
    pub fallback_module_name: &'a str,
}

/// Compile a [`CompileInvocation`] against ONE `tidepool-extract` spawn:
/// `targets.len()` prepared programs over a single shared merged `meta.cbor`
/// / [`DataConTable`], returning one [`TargetArtifact`] per
/// target inside a shared [`CompiledArtifacts`]. Drives the extract's
/// `--targets a,b` mode, which handles a single-element list identically.
///
/// A REQUESTED target is a contract: a nonzero exit fails the WHOLE spawn if
/// ANY target can't translate, rather than silently emitting the targets
/// that succeeded.
///
/// Asks sidecar shape: exactly one target writes the plain `asks.json`
/// array; more than one additionally writes `<target>.asks.json` per target,
/// so two targets' different `runLLMTurn`/`runLLMTurnFork` sites never
/// collapse into one ambiguous file.
///
/// **MEMOIZED** per [`CompileInvocation::cache`] — see the module doc for why
/// both cacheable entry points share the same recipe and bundle.
///
/// `on_stage(name, elapsed, bytes)` fires once per measured stage —
/// [`timing::STAGE_EXTRACT_SPAWN`], each forwarded `extract.<phase>` row
/// parsed from the extract's stderr, [`timing::STAGE_CBOR_READ`],
/// [`timing::STAGE_CBOR_DESERIALIZE`], [`timing::STAGE_ASKS_PARSE`] — so a
/// caller with its own attribution vocabulary (node/round ids) can record
/// through its own collector without this crate needing to know what a
/// "node" or "round" is. A caller with no use for timing passes `|_, _, _|
/// {}`.
///
/// `settlement` belongs to the calling operation and must retain uncertain
/// close evidence through operation unwind. Each standalone attempt reports
/// its independent close; scoped calls keep the existing transaction owner.
/// The recipient is borrowed across known-unsubmitted retries.
pub fn compile_invocation(
    inv: &CompileInvocation<'_>,
    mut on_stage: impl FnMut(&str, Duration, u64),
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<CompiledArtifacts, CompileError> {
    compile_invocation_inner(
        inv,
        &mut on_stage,
        CompilationPolicy::Runtime { settlement },
    )
    .map(|output| output.artifacts)
}

/// Compile fresh source against immutable declaration owners through the same
/// artifact front door. Ordinary source candidates and artifact memo are
/// unavailable; successful products require context-bound compiler receipts.
pub fn compile_invocation_in_context(
    inv: &CompileInvocation<'_>,
    context: Arc<crate::declaration_join::ExactDeclarationContext>,
    mut on_stage: impl FnMut(&str, Duration, u64),
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<CompiledArtifacts, CompileError> {
    compile_invocation_inner(
        inv,
        &mut on_stage,
        CompilationPolicy::Exact {
            context,
            settlement,
        },
    )
    .map(|output| output.artifacts)
}

/// Compile a declaration probe in full-home-product mode, which produces
/// original products for every home module, including modules with no
/// executable references from the probe. The probe bypasses memo lookup and
/// candidate publication while retaining the shared certification front door.
pub(crate) fn compile_authored_products(
    source: &str,
    target: &str,
    include: &[PathBuf],
    session_root: &Path,
    context: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
    authored: &crate::declaration_join::NativeAuthoredDeclarationAdmission,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<CompiledArtifacts, CompileError> {
    let inv = CompileInvocation {
        source,
        targets: &[target],
        include,
        fallback_module_name: AUTHORED_PRODUCT_PROBE_MODULE,
    };
    compile_invocation_inner(
        &inv,
        &mut |_, _, _| {},
        CompilationPolicy::Authored {
            session_root,
            context,
            admission: authored,
            settlement,
        },
    )
    .map(|output| output.artifacts)
}

/// Build an explicitly selected immutable source cohort at its final deployment
/// path. The declared probe and ordered targets run under build-action isolation;
/// only authenticated originals are exported. Schema 4 retains original source
/// paths while allowing the complete product container to move unchanged.
pub fn build_deployment_module_package(
    source_path: &Path,
    targets: &[&str],
    source_root: &Path,
    scratch: &Path,
    output_root: &Path,
) -> Result<crate::toolchain::DeploymentModulePackage, CompileError> {
    let (authority, source_selection) =
        prepare_deployment_module_action(source_path, targets, source_root, scratch, output_root)?;
    compile_build_action(
        source_path,
        targets,
        &source_selection.include_roots(),
        scratch,
        BuildActionExport::DeploymentPackage {
            output_root,
            source_selection: &source_selection,
        },
    )?;
    Ok(crate::toolchain::DeploymentModulePackage::load(
        &output_root.join("catalog.json"),
        &authority,
    )?)
}

/// Inspect actual compiler capabilities without publishing a deployment catalog.
/// Both successful and refused transactions retain their original raw outputs.
pub fn inspect_deployment_module_package(
    source_path: &Path,
    targets: &[&str],
    source_root: &Path,
    scratch: &Path,
    output_root: &Path,
) -> Result<PathBuf, CompileError> {
    let (_, source_selection) =
        prepare_deployment_module_action(source_path, targets, source_root, scratch, output_root)?;
    catalog_inventory::request(
        output_root,
        source_path,
        targets,
        scratch,
        &source_selection,
    )?;
    let result = compile_build_action(
        source_path,
        targets,
        &source_selection.include_roots(),
        scratch,
        BuildActionExport::CatalogInventory {
            output_root,
            source_selection: &source_selection,
        },
    );
    let report = catalog_inventory::outcome(output_root, &result)?;
    result?;
    Ok(report)
}

fn prepare_deployment_module_action(
    source_path: &Path,
    targets: &[&str],
    source_root: &Path,
    scratch: &Path,
    output_root: &Path,
) -> Result<
    (
        crate::toolchain::CompilerDeploymentAuthority,
        module_candidates::deployment::NativeCatalogSourceSelection,
    ),
    CompileError,
> {
    let source_selection =
        module_candidates::deployment::prepare_build_roots(source_root, output_root)?;
    if source_path != source_selection.snapshot_root.join("TidepoolCatalog.hs") {
        return Err(
            crate::toolchain::ModulePackageError::Format("native catalog probe path").into(),
        );
    }
    validate_build_action_request(
        source_path,
        targets,
        &source_selection.include_roots(),
        scratch,
        output_root,
    )?;
    let configuration = crate::toolchain::CompilerDeploymentConfiguration::from_env()
        .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    let crate::toolchain::CompilerDeploymentConfiguration::Configured(authority) = configuration
    else {
        return Err(crate::toolchain::ModulePackageError::UnknownCompiler.into());
    };
    Ok((authority, source_selection))
}

/// Authority selected by the compilation owner, independently of cache hints.
enum CompilationPolicy<'a> {
    Runtime {
        settlement: &'a mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
    },
    RetainedEntry {
        source_path: &'a Path,
        output: &'a Path,
        sources: &'a ProductionEntrySources,
        catalog: Option<Arc<crate::toolchain::DeploymentModulePackage>>,
        settlement: &'a mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
    },
    Exact {
        context: Arc<crate::declaration_join::ExactDeclarationContext>,
        settlement: &'a mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
    },
    Authored {
        session_root: &'a Path,
        context: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
        admission: &'a crate::declaration_join::NativeAuthoredDeclarationAdmission,
        settlement: &'a mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
    },
    BuildAction {
        source_path: &'a Path,
        scratch: &'a Path,
        export: BuildActionExport<'a>,
    },
}

#[derive(Clone)]
enum BuildActionExport<'a> {
    PreparedFixture {
        output: &'a Path,
    },
    DeploymentPackage {
        output_root: &'a Path,
        source_selection: &'a module_candidates::deployment::NativeCatalogSourceSelection,
    },
    CatalogInventory {
        output_root: &'a Path,
        source_selection: &'a module_candidates::deployment::NativeCatalogSourceSelection,
    },
    ProductionEntry {
        output_root: &'a Path,
        source_selection: &'a ProductionEntrySources,
    },
}

/// Disposable requests release scratch on return. Original entries reserve
/// durable custody before execution and preserve it through every failure.
enum CompilationOutputOwner {
    Scratch(TempDir),
    Original(production_entry::EntryPreparation),
}

impl CompilationOutputOwner {
    fn path(&self) -> &Path {
        match self {
            Self::Scratch(directory) => directory.path(),
            Self::Original(preparation) => preparation.raw(),
        }
    }

    fn begin_execution(&mut self) -> Option<production_entry::EntrySubmission> {
        match self {
            Self::Original(preparation) => Some(preparation.begin_execution()),
            Self::Scratch(_) => None,
        }
    }

    fn endpoint_failure(
        &mut self,
        error: &tidepool_extract_cmd::SpawnError,
        previous: Option<production_entry::EntrySubmission>,
    ) {
        if let Self::Original(preparation) = self {
            if error.definitely_unsubmitted() {
                if let Some(previous) = previous {
                    preparation.confirm_unsubmitted_attempt(previous);
                }
            } else {
                preparation.mark_uncertain_submission();
            }
        }
    }
}

/// Compile one declared module and target set for an immutable build action.
/// The action owns its current-directory scratch and an absent output directory.
/// This policy uses a
/// direct configured compiler and cannot read or publish runtime memo entries,
/// module candidates, deployment catalogs or mutable runtime build products.
/// Only portable code and metadata are exported; source-bound authority stays
/// in its original compiler transaction and is never relocated or restamped.
pub fn build_prepared_fixture(
    source_path: &Path,
    targets: &[&str],
    include: &[PathBuf],
    scratch: &Path,
    output: &Path,
) -> Result<(), CompileError> {
    validate_build_action_request(source_path, targets, include, scratch, output)?;
    compile_build_action(
        source_path,
        targets,
        include,
        scratch,
        BuildActionExport::PreparedFixture { output },
    )
}

fn validate_build_action_request(
    source_path: &Path,
    targets: &[&str],
    include: &[PathBuf],
    scratch: &Path,
    output: &Path,
) -> Result<(), CompileError> {
    if targets.is_empty()
        || targets.iter().any(|target| {
            target.is_empty()
                || target.contains([',', '/', '\\'])
                || *target == "."
                || *target == ".."
        })
        || targets
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != targets.len()
    {
        return Err(CompileError::ExtractFailed(
            "invalid build-action target set".into(),
        ));
    }
    if !scratch.is_dir()
        || output.exists()
        || !source_path.is_file()
        || include.iter().any(|root| !root.is_dir())
        || std::fs::canonicalize(std::env::current_dir()?)? != std::fs::canonicalize(scratch)?
    {
        return Err(CompileError::ExtractFailed("build action requires a declared source, include roots, private current-directory scratch and absent output".into()));
    }
    Ok(())
}

fn compile_build_action(
    source_path: &Path,
    targets: &[&str],
    include: &[PathBuf],
    scratch: &Path,
    export: BuildActionExport<'_>,
) -> Result<(), CompileError> {
    let source = std::fs::read_to_string(source_path)?;
    let fallback = source_path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CompileError::ExtractFailed("build-action module filename".into()))?;
    let invocation = CompileInvocation {
        source: &source,
        targets,
        include,
        fallback_module_name: fallback,
    };
    compile_invocation_inner(
        &invocation,
        &mut |_, _, _| {},
        CompilationPolicy::BuildAction {
            source_path,
            scratch,
            export,
        },
    )
    .map(|_| ())
}

pub(crate) const AUTHORED_PRODUCT_PROBE_MODULE: &str = "TidepoolAuthoredProductProbe";

/// Validate consumed bytes and import witnesses against a build action's
/// declared module and complete readable source trees. This shares the worker
/// dependency reader with ordinary artifact admission and grants no execution.
pub fn validate_prepared_fixture_sources(
    evidence_bytes: &[u8],
    source: &Path,
    include: &[PathBuf],
) -> Result<(), CompileError> {
    validate_build_action_source_closure(evidence_bytes, source, include)?;
    cache::DependencyEvidence::from_worker(
        evidence_bytes,
        source,
        &std::fs::read_to_string(source)?,
    )
    .ok_or_else(|| {
        CompileError::ExtractFailed(
            "build-action consumed bytes or import witnesses changed".into(),
        )
    })?;
    Ok(())
}

/// Validate observations from a fresh corpus compilation owned by a native
/// test run. This admits the completed output only: compile-time execution
/// remains ineligible for source replay and does not prove a hermetic action.
/// Observed Haskell sources must still belong to the declared source trees.
pub fn validate_completed_corpus_sources(
    evidence_bytes: &[u8],
    source: &Path,
    include: &[PathBuf],
) -> Result<(), CompileError> {
    let evidence: cache::DependencyEvidence =
        serde_json::from_slice(evidence_bytes).map_err(|error| {
            CompileError::ExtractFailed(format!("corpus dependency evidence: {error}"))
        })?;
    validate_declared_source_closure(&evidence, source, include)?;
    cache::CompletedSourceEvidence::from_worker_evidence(
        evidence,
        source,
        &std::fs::read_to_string(source)?,
    )
    .map_err(|failure| {
        CompileError::CompilerEvidence(Box::new(
            crate::certified_products::CertificationError::CompletedSourceEvidence {
                input: source.to_path_buf(),
                failure: Box::new(failure),
            },
        ))
    })?;
    Ok(())
}

fn validate_build_action_source_closure(
    evidence_bytes: &[u8],
    source: &Path,
    include: &[PathBuf],
) -> Result<(), CompileError> {
    let evidence: cache::DependencyEvidence =
        serde_json::from_slice(evidence_bytes).map_err(|error| {
            CompileError::ExtractFailed(format!("build-action dependency evidence: {error}"))
        })?;
    if !evidence.cache_safe || !evidence.selection_complete {
        return Err(CompileError::ExtractFailed(
            "build-action source evidence is incomplete".into(),
        ));
    }
    validate_declared_source_closure(&evidence, source, include)
}

fn validate_declared_source_closure(
    evidence: &cache::DependencyEvidence,
    source: &Path,
    include: &[PathBuf],
) -> Result<(), CompileError> {
    let mut declared = std::collections::BTreeSet::from([std::fs::canonicalize(source)?]);
    for root in include {
        let manifest = cache::source_root_manifest(root)
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        for (relative, _) in manifest {
            declared.insert(std::fs::canonicalize(root.join(relative))?);
        }
    }
    for consumed in &evidence.sources {
        if !declared.contains(&std::fs::canonicalize(&consumed.path)?) {
            return Err(CompileError::ExtractFailed(format!(
                "build action consumed an undeclared source: {}",
                consumed.path.display()
            )));
        }
    }
    Ok(())
}

struct CompilationOutput {
    artifacts: CompiledArtifacts,
    original_entry: Option<ProductionEntryOutput>,
}

fn compile_invocation_inner(
    inv: &CompileInvocation<'_>,
    mut on_stage: &mut impl FnMut(&str, Duration, u64),
    mut policy: CompilationPolicy<'_>,
) -> Result<CompilationOutput, CompileError> {
    assert!(
        !inv.targets.is_empty(),
        "compile_invocation: at least one target is required"
    );
    let multi = inv.targets.len() > 1;

    let (session_root, exact_context, deployment_export, authored) = match &policy {
        CompilationPolicy::Runtime { .. }
        | CompilationPolicy::RetainedEntry { .. }
        | CompilationPolicy::BuildAction {
            export:
                BuildActionExport::PreparedFixture { .. }
                | BuildActionExport::CatalogInventory { .. }
                | BuildActionExport::ProductionEntry { .. },
            ..
        } => (None, None, None, None),
        CompilationPolicy::Exact { context, .. } => (None, Some(Arc::clone(context)), None, None),
        CompilationPolicy::Authored {
            session_root,
            context,
            admission,
            ..
        } => (Some(*session_root), context.clone(), None, Some(*admission)),
        CompilationPolicy::BuildAction {
            export:
                BuildActionExport::DeploymentPackage {
                    output_root,
                    source_selection,
                },
            ..
        } => (None, None, Some((*output_root, *source_selection)), None),
    };
    let inventory_export = match &policy {
        CompilationPolicy::BuildAction {
            export:
                BuildActionExport::CatalogInventory {
                    output_root,
                    source_selection,
                },
            ..
        } => Some((*output_root, *source_selection)),
        _ => None,
    };
    let allow_candidates = matches!(&policy, CompilationPolicy::Runtime { .. });
    let mut output_owner = match &policy {
        CompilationPolicy::RetainedEntry { output, .. }
        | CompilationPolicy::BuildAction {
            export:
                BuildActionExport::ProductionEntry {
                    output_root: output,
                    ..
                },
            ..
        } => CompilationOutputOwner::Original(production_entry::EntryPreparation::reserve(output)?),
        CompilationPolicy::BuildAction {
            export: BuildActionExport::CatalogInventory { output_root, .. },
            ..
        } => {
            let raw = output_root.join("raw");
            std::fs::create_dir(&raw)?;
            CompilationOutputOwner::Scratch(TempDir::new_in(raw)?)
        }
        CompilationPolicy::BuildAction { scratch, .. } => {
            CompilationOutputOwner::Scratch(TempDir::new_in(scratch)?)
        }
        _ => CompilationOutputOwner::Scratch(compiler_scratch_directory()?),
    };
    if let (Some(_), CompilationOutputOwner::Scratch(directory)) =
        (inventory_export, &mut output_owner)
    {
        directory.disable_cleanup(true);
    }
    // GHC derives the module name from the filename (capitalize(basename));
    // see `CompileInvocation::fallback_module_name`'s doc for why this
    // differs per lane.
    let module =
        extract_module_name(inv.source).unwrap_or_else(|| inv.fallback_module_name.to_string());
    let input_path = match &policy {
        CompilationPolicy::BuildAction { source_path, .. }
        | CompilationPolicy::RetainedEntry { source_path, .. } => source_path.to_path_buf(),
        _ => {
            let path = output_owner.path().join(format!("{module}.hs"));
            std::fs::write(&path, inv.source)?;
            path
        }
    };

    let mut cmd = ExtractCmd::new().map_err(|e| CompileError::Io(e.into()))?;
    cmd.input(&input_path)
        .output_dir(output_owner.path())
        .targets(inv.targets)
        .includes(inv.include);
    if let Some(root) = session_root {
        cmd.session_root(root).certify_home_products();
    }
    if matches!(&policy, CompilationPolicy::RetainedEntry { .. })
        || deployment_export.is_some()
        || inventory_export.is_some()
        || matches!(
            &policy,
            CompilationPolicy::BuildAction {
                export: BuildActionExport::ProductionEntry { .. },
                ..
            }
        )
    {
        cmd.certify_home_products();
    }
    let exact_request = if let Some(ref context) = exact_context {
        let endpoint = cmd
            .bind()
            .map_err(|error| CompileError::Io(extract_spawn_error(error.source)))?;
        crate::toolchain::admit_bound_endpoint(&endpoint)
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        let producer = endpoint.identity().producer_bytes();
        let reserved = match &policy {
            CompilationPolicy::Authored { admission, .. } => admission.owner().module.clone(),
            _ => module.clone(),
        };
        // A lexical join can hide an already selected authored body. Every
        // exact executable request retains that native closure privately.
        let selected = immutable_candidates_in_context(
            context,
            producer,
            inv.include,
            output_owner.path(),
            BTreeSet::from([reserved]),
        )?;
        let private = private_native_availability(context, producer, selected.as_deref())?;
        let request = crate::declaration_context::ExactCompileContext::new(Arc::clone(context))
            .prepare_compilation_with_private_input(
                &output_owner.path().join("exact-scope"),
                producer,
                None,
                private,
            )?;
        let request = request.with_source_search_context(inv.include);
        request.apply_to(
            &mut cmd,
            crate::declaration_context::RetainedGenerationPolicy::PreserveCertifiedDemand,
        )?;
        Some(request)
    } else {
        None
    };

    // Bind the logical build-products root before recipe construction. The
    // process boundary privately places mutable GHC outputs by daemon epoch
    // and worker slot; its placement does not alter the logical recipe or
    // authorize additional source inputs. Reaped slot rotations retain disk
    // warmth, while new daemons and direct invocations use fresh namespaces.
    let names = artifact_names(inv.targets, multi);
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let base_cmd = cmd;
    let attempt = retry_bounded(
        || {
            let mut cmd = base_cmd.clone();
            let endpoint = match &policy {
                CompilationPolicy::BuildAction { .. } => cmd.bind_direct(),
                _ => cmd.bind(),
            }
            .map_err(|error| {
                output_owner.endpoint_failure(&error, None);
                CompileAttemptError::Endpoint(error)
            })?;
            if let Some((output, _)) = inventory_export {
                catalog_inventory::endpoint(
                    output,
                    endpoint.identity().producer_bytes(),
                    &cmd.argv(),
                    output_owner.path(),
                    catalog_inventory::Phase::CompilerDeploymentAdmission,
                )
                .map_err(CompileAttemptError::Diagnostic)?;
            }
            let deployment = crate::toolchain::admit_bound_endpoint(&endpoint)
                .map_err(CompileAttemptError::Deployment)?;
            match &policy {
                CompilationPolicy::BuildAction { .. } => {
                    cmd.build_products_dir(output_owner.path().join("build-products"));
                }
                _ => crate::paths::apply_build_products_dir(&mut cmd, &endpoint),
            }

            let candidate_set = if allow_candidates {
                module_candidates::select_configured(
                    endpoint.identity().producer_bytes(),
                    inv.include,
                    output_owner.path(),
                )
                .map_err(CompileAttemptError::ModulePackage)?
            } else if let CompilationPolicy::RetainedEntry {
                catalog: Some(package),
                ..
            } = &policy
            {
                package
                    .validate_deployment(&deployment)
                    .map_err(CompileAttemptError::ModulePackage)?;
                module_candidates::select_acquired_catalog(
                    package,
                    endpoint.identity().producer_bytes(),
                    inv.include,
                    output_owner.path(),
                )
                .map_err(CompileAttemptError::ModulePackage)?
            } else {
                None
            };

            let inv_key = if allow_candidates {
                let argv = cmd.argv();
                let key = cache::invocation_key(&cache::Invocation {
                    source: inv.source,
                    argv: &argv,
                    input_path: &input_path,
                    include: inv.include,
                    endpoint_identity: endpoint.identity().producer_bytes(),
                });
                if let Some(key) = &key {
                    let load_start = Instant::now();
                    if let Some((meta_bytes, raw, product_bytes, evidence)) = allow_candidates
                        .then(|| load_memo(key, &name_refs, inv.targets, inv.source))
                        .transpose()
                        .map_err(|error| CompileAttemptError::Diagnostic(error.into()))?
                        .flatten()
                    {
                        // Direct invocation memo lacks a worker group/target
                        // certificate. Home products must use the exact
                        // module-candidate transaction instead.
                        if !evidence
                            .modules
                            .iter()
                            .any(|module| module.product == cache::ProductAvailability::Ready)
                        {
                            if let Ok(mut artifacts) = decode_fresh_products(&product_bytes)
                                .and_then(|fresh| {
                                    assemble_with_products(
                                        &meta_bytes,
                                        &raw,
                                        fresh,
                                        Vec::new(),
                                        Some(&evidence),
                                        None,
                                        None,
                                        &mut on_stage,
                                    )
                                })
                            {
                                artifacts.producer_identity =
                                    Some(*endpoint.identity().producer_bytes());
                                on_stage(
                                    timing::STAGE_CBOR_READ,
                                    load_start.elapsed(),
                                    total_bytes(&meta_bytes, &raw, &product_bytes),
                                );
                                return Ok(CompileAttempt::Cached(Box::new(artifacts)));
                            }
                            // An interrupted memo admission is a refusal, not
                            // permission to retry with physical compiler work.
                            crate::host_work::checkpoint()
                                .map_err(|error| CompileAttemptError::Diagnostic(error.into()))?;
                        }
                    }
                }
                key
            } else {
                None
            };

            if let Some(selected) = &candidate_set {
                cmd.module_candidates(&selected.manifest_path);
            }
            let producer = endpoint.identity().producer_bytes().to_vec();

            if let Some((output, _)) = inventory_export {
                catalog_inventory::endpoint(
                    output,
                    &producer,
                    &cmd.argv(),
                    output_owner.path(),
                    catalog_inventory::Phase::CompilerExecution,
                )
                .map_err(CompileAttemptError::Diagnostic)?;
            }
            let diagnostics = CompilerDiagnosticCapture::start(output_owner.path(), &cmd);
            crate::host_work::checkpoint()
                .map_err(|error| CompileAttemptError::Diagnostic(error.into()))?;
            let previous_submission = output_owner.begin_execution();
            let execution = match &mut policy {
                CompilationPolicy::Runtime {
                    settlement: recipient,
                }
                | CompilationPolicy::Exact {
                    settlement: recipient,
                    ..
                }
                | CompilationPolicy::Authored {
                    settlement: recipient,
                    ..
                }
                | CompilationPolicy::RetainedEntry {
                    settlement: recipient,
                    ..
                } => {
                    let mut files = exact_request
                        .as_ref()
                        .map_or_else(Vec::new, |request| request.input_transport_files());
                    if let Some(selected) = &candidate_set {
                        files.extend(
                            selected
                                .input_transport
                                .iter()
                                .map(|slice| slice.compiler_file_lease()),
                        );
                    }
                    CompileError::compiler_invocation_result(endpoint.execute_with_input_files(
                        &cmd,
                        files,
                        |close| recipient(close),
                    ))
                }
                CompilationPolicy::BuildAction { .. } => endpoint
                    .execute(&cmd)
                    .map_err(CompileError::CompilerEndpoint),
            }
            .map_err(|error| match error {
                CompileError::CompilerEndpoint(error) => {
                    output_owner.endpoint_failure(&error, previous_submission);
                    CompileAttemptError::Endpoint(error)
                }
                error => {
                    if let CompilationOutputOwner::Original(preparation) = &mut output_owner {
                        preparation.mark_uncertain_submission();
                    }
                    CompileAttemptError::Diagnostic(error)
                }
            });
            execution.map(|run| {
                diagnostics.completed(output_owner.path(), &cmd, run.success(), &run.output.stderr);
                CompileAttempt::Executed((cmd, run, inv_key, producer, candidate_set, deployment))
            })
        },
        |error| matches!(error,CompileAttemptError::Endpoint(error) if error.permits_rebind()),
    );
    let attempt = match attempt {
        Ok(attempt) => attempt,
        Err(error) => {
            if matches!(&error, CompileAttemptError::Endpoint(error) if error.definitely_unsubmitted())
                || matches!(&error, CompileAttemptError::Diagnostic(CompileError::Io(error))
                    | CompileAttemptError::ModulePackage(crate::toolchain::ModulePackageError::Interrupted(error))
                    if error.kind() == std::io::ErrorKind::Interrupted)
            {
                if let CompilationOutputOwner::Original(preparation) = output_owner {
                    preparation.release_if_unsubmitted()?;
                }
            }
            return Err(error.into_compile_error());
        }
    };
    let (cmd, run, inv_key, producer, candidate_set, deployment) = match attempt {
        CompileAttempt::Cached(artifacts) => {
            return Ok(CompilationOutput {
                artifacts: *artifacts,
                original_entry: None,
            });
        }
        CompileAttempt::Executed(executed) => executed,
    };

    if let Some(request) = exact_request.as_ref() {
        let actual =
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&producer)
                .sha256();
        if actual != request.context().toolchain_identity_sha256() {
            return Err(CompileError::ExtractFailed(
                "exact compile rebound to a different producer".into(),
            ));
        }
    }

    // The full spawn argv, rendered once: DEBUG on every spawn, and attached
    // to the failure WARN below. This is the record of what include set /
    // session-root / flags an individual spawn actually received — without
    // it, a spawn-specific resolution failure (a module present on disk that
    // one compile couldn't find, sprint-25 crash class) leaves nothing to
    // diff against a succeeding sibling invocation.
    let argv_render: String = cmd
        .argv()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    tracing::debug!(
        targets = %inv.targets.join(","),
        argv = %argv_render,
        "extract spawn"
    );

    if inventory_export.is_some()
        || matches!(&output_owner, CompilationOutputOwner::Original(_))
        || matches!(
            &policy,
            CompilationPolicy::BuildAction {
                export: BuildActionExport::PreparedFixture { .. },
                ..
            }
        )
    {
        std::fs::write(
            output_owner.path().join("compiler.stdout"),
            &run.output.stdout,
        )?;
        std::fs::write(
            output_owner.path().join("compiler.stderr"),
            &run.output.stderr,
        )?;
        std::fs::write(
            output_owner.path().join("compiler-status.json"),
            serde_json::to_vec(&serde_json::json!({
                "success": run.output.status.success(), "exit_code": run.output.status.code(),
            }))
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?,
        )?;
        if let Some((output, _)) = inventory_export {
            catalog_inventory::phase(output, catalog_inventory::Phase::CompilerOutputDecode)?;
        }
    }
    let compiler_stderr = run.output.stderr.clone();
    let extracted = extract_and_read(
        run,
        output_owner.path(),
        inv.targets,
        multi,
        &mut on_stage,
        |stderr, success| {
            if !success && !stderr.is_empty() {
                tracing::warn!(targets = %inv.targets.join(","), "extract failed:\n{stderr}");
            }
        },
    );
    // Candidate admission misses are handled before source execution by the
    // worker. A completed request is never replayed because of its response.
    let (meta_bytes, raw, product_bytes, inventory_operation) =
        extracted.map_err(|error| match &policy {
            CompilationPolicy::BuildAction {
                export: BuildActionExport::PreparedFixture { .. },
                ..
            } => retain_compiler_failure(output_owner.path(), &cmd, &compiler_stderr, error),
            CompilationPolicy::BuildAction { .. } => error,
            _ => retain_compiler_failure(output_owner.path(), &cmd, &compiler_stderr, error),
        })?;

    // Store only what DESERIALIZED, so a malformed artifact set is never
    // memoized into a permanently-failing entry. Best-effort: an unwritable
    // memo costs a recompile, it never fails a compile.
    let assembled = (|| {
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(
            inventory_operation.clone(),
        );
        let evidence_bytes =
            CompilerSidecar::Dependencies.read(output_owner.path(), &inventory_operation)?;
        let package_bundle_bytes = CompilerSidecar::ModulePackageImports
            .read(output_owner.path(), &inventory_operation)?;
        if let Some((output, _)) = inventory_export {
            catalog_inventory::phase(output, catalog_inventory::Phase::SourceEvidenceValidation)?;
        }
        inventory_operation
            .charge(evidence_bytes.len().checked_mul(64).ok_or_else(|| {
                CompileError::ExtractFailed("source evidence accounting overflow".into())
            })?)
            .map_err(|error| compiler_evidence_failure(error.into()))?;
        if matches!(&policy, CompilationPolicy::BuildAction { .. }) {
            validate_prepared_fixture_sources(&evidence_bytes, &input_path, inv.include)?;
        }
        let exact_source = exact_request
            .as_ref()
            .map(|request| {
                request.admit_source_with_validation(
                    &input_path,
                    inv.source,
                    &evidence_bytes,
                    &mut validation,
                )
            })
            .transpose()?;
        let exact = exact_request
            .as_ref()
            .zip(exact_source.as_ref())
            .map(
                |(request, source)| crate::declaration_context::ExactProductAdmission {
                    request,
                    source,
                },
            );
        let evidence = match exact_source.as_ref() {
            Some(source) => Some((*source.evidence).clone()),
            None => Some(
                cache::CompletedSourceEvidence::from_worker(
                    &evidence_bytes,
                    &input_path,
                    inv.source,
                )
                .map_err(|failure| {
                    CompileError::CompilerEvidence(Box::new(
                        crate::certified_products::CertificationError::CompletedSourceEvidence {
                            input: input_path.clone(),
                            failure: Box::new(failure),
                        },
                    ))
                })?,
            ),
        };
        if let Some((output, _)) = inventory_export {
            catalog_inventory::phase(output, catalog_inventory::Phase::ProductCertification)?;
        }
        let receipt_bytes =
            CompilerSidecar::CertifiedProducts.read(output_owner.path(), &validation.inventory)?;
        if receipt_bytes.is_empty() {
            if inventory_export.is_some() {
                return Err(CompileError::ExtractFailed(
                    "catalog inventory product certificate unavailable".into(),
                ));
            }
            if exact_request.is_some() {
                return Err(CompileError::ExtractFailed(
                    "exact compile product certificate unavailable".into(),
                ));
            }
            if candidate_set
                .as_ref()
                .is_some_and(|set| !set.by_owner.is_empty())
            {
                return Err(CompileError::ExtractFailed(
                    "candidate product certificate unavailable".into(),
                ));
            }
            let requirements = crate::prepared_artifact::production_requirements()
                .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
            let fresh_products = inventory_operation
                .parse_module_products(&product_bytes, &requirements)
                .map_err(|error| compiler_evidence_failure(error.into()))?;
            if !fresh_products.is_empty() {
                return Err(CompileError::ExtractFailed(
                    "fresh module product certificate unavailable".into(),
                ));
            }
            if evidence.as_deref().is_some_and(has_ready_home_module) {
                return Err(CompileError::ExtractFailed(
                    "ready module product certificate unavailable".into(),
                ));
            }
            let artifacts = assemble_with_products(
                &meta_bytes,
                &raw,
                fresh_products,
                Vec::new(),
                evidence.as_deref(),
                None,
                None,
                &mut *on_stage,
            )?;
            ensure_no_uncertified_globals(&artifacts)?;
            return Ok(artifacts);
        }
        let receipt = certified_products::decode_receipt_with_operation(
            &receipt_bytes,
            Some(output_owner.path()),
            &validation.inventory,
        )
        .map_err(compiler_evidence_failure)?;
        let fresh_products = certified_products::ParsedModuleProducts::decode_with_operation(
            &product_bytes,
            &package_bundle_bytes,
            validation.inventory.clone(),
        )
        .map_err(compiler_evidence_failure)?;
        let valid_evidence = evidence
            .as_ref()
            .filter(|value| value.revalidate(inv.source).is_ok());
        let cached_receipts: Vec<_> = receipt
            .modules
            .iter()
            .filter(|module| module.origin == certified_products::ProductOrigin::Cached)
            .collect();
        let mut certified = if let Some(valid) = valid_evidence {
            let certified = certified_products::certify_products_with_validation(
                candidate_set.as_ref(),
                &receipt,
                &fresh_products,
                &evidence_bytes,
                &input_path,
                output_owner.path(),
                valid,
                inv.source,
                &producer,
                inv.include,
                exact.as_ref(),
                authored,
                &selected_session_value_modules(&cmd)?,
                None,
                &mut validation,
            )
            .map_err(compiler_evidence_failure)?;
            if let Some((output, selection)) = inventory_export {
                catalog_inventory::inventory(output, selection, valid, &certified)?;
            }
            ensure_ready_module_inventory(&receipt.modules, valid)?;
            certified
        } else {
            if !fresh_products.products().is_empty()
                || !receipt.modules.is_empty()
                || receipt
                    .targets
                    .values()
                    .any(|accepted| !accepted.is_empty())
            {
                return Err(CompileError::ExtractFailed(
                    "module or target owner lacks valid final dependency evidence".into(),
                ));
            }
            certified_products::CertifiedProducts {
                groups: Arc::from([]),
                recovery_products: Arc::from([]),
                module_interfaces: Arc::from([]),
                value_interfaces: Arc::from([]),
                retained_core_products: Default::default(),
                source_selection: Default::default(),
            }
        };
        let extra_products: Vec<_> = cached_receipts
            .iter()
            .map(|module| {
                candidate_set
                    .as_ref()
                    .and_then(|selected| {
                        selected
                            .by_owner
                            .get(&(module.unit.clone(), module.module.clone()))
                    })
                    .map(|bundle| bundle.product.clone())
                    .ok_or_else(|| {
                        CompileError::ExtractFailed("cached product owner missing".into())
                    })
            })
            .collect::<Result<_, _>>()?;
        let fresh_count = fresh_products.products().len();
        let (fresh_products, publication) = if (!matches!(
            &policy,
            CompilationPolicy::BuildAction { .. } | CompilationPolicy::RetainedEntry { .. }
        ) || deployment_export.is_some())
            && (exact_request.is_none() || deployment_export.is_some())
            && evidence.is_some()
        {
            let (products, publication) = module_candidates::prepare_publication(
                &producer,
                inv.include,
                evidence.as_ref().ok_or_else(|| {
                    CompileError::ExtractFailed("candidate publication evidence unavailable".into())
                })?,
                fresh_products,
                inv.source,
                exact.as_ref().map_or(
                    module_candidates::CandidateVersionOrigin::Ordinary,
                    |admission| module_candidates::CandidateVersionOrigin::Exact {
                        semantic_sha256: admission.request.semantic_sha256,
                    },
                ),
                &certified.recovery_products,
            );
            (products, Some(publication))
        } else {
            (fresh_products.into_products(), None)
        };
        let mut artifacts = assemble_with_products(
            &meta_bytes,
            &raw,
            fresh_products,
            extra_products,
            evidence.as_deref(),
            exact_request.as_ref(),
            Some(&certified.retained_core_products),
            &mut on_stage,
        )?;
        if valid_evidence.is_none() {
            ensure_no_uncertified_globals(&artifacts)?;
        }
        if receipt.targets.len() != artifacts.targets.len() {
            return Err(CompileError::ExtractFailed(
                "target product receipt count".into(),
            ));
        }
        let target_admission_start = Instant::now();
        let package_catalog =
            package_availability_with_validation(&receipt.packages, &certified, &mut validation)?;
        let mut target_imports = Vec::new();
        for (name, target) in &artifacts.targets {
            let accepted = receipt.targets.get(name).ok_or_else(|| {
                CompileError::ExtractFailed("target product receipt missing".into())
            })?;
            if valid_evidence.is_some() {
                target_imports.extend(
                    certified_products::certify_target_available_owners_with_validation(
                        target.prepared.prepared(),
                        accepted,
                        &certified.recovery_products,
                        &certified.groups,
                        &certified.source_selection,
                        &package_catalog,
                        &mut validation,
                    )
                    .map_err(compiler_evidence_failure)?,
                );
            }
        }
        let compiler_inputs = exact_request
            .as_ref()
            .map(|request| request.compiler_inputs());
        artifacts.artifact_view =
            crate::declaration_context::certified_product_artifact_view_with_validation(
                crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                    &producer,
                )
                .sha256(),
                &certified.recovery_products,
                &certified.module_interfaces,
                &certified.value_interfaces,
                compiler_inputs.as_ref().map(|input| &input.artifacts),
                crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(
                    &target_imports,
                ),
                &mut validation,
            )?;
        certified.groups =
            crate::declaration_context::certify_artifact_view_groups_with_validation(
                &artifacts.artifact_view,
                &certified.groups,
                exact_request
                    .as_ref()
                    .map_or(&[], |request| request.groups.as_ref()),
                &mut validation,
            )?
            .into();
        let package_closure =
            package_catalog.select(&certified.groups, &target_imports, &validation.inventory)?;
        for (name, target) in &mut artifacts.targets {
            let accepted = receipt.targets.get(name).ok_or_else(|| {
                CompileError::ExtractFailed("target product receipt missing".into())
            })?;
            if valid_evidence.is_some() {
                target.pending_imports = certified_products::certify_target_owners_with_validation(
                    target.prepared.prepared(),
                    accepted,
                    &certified.groups,
                    &certified.source_selection,
                    &package_closure,
                    &mut validation,
                )
                .map_err(compiler_evidence_failure)?;
                target.package_interfaces =
                    certified_products::certify_target_package_interfaces_with_validation(
                        target.prepared.prepared_shared(),
                        &package_closure,
                        &mut validation,
                    )
                    .map_err(compiler_evidence_failure)?;
            }
        }
        timing::record_stage_with_owners(
            timing::NO_NODE,
            timing::NO_ROUND,
            "products.target_admission",
            target_admission_start.elapsed(),
            0,
            receipt.targets.len(),
        );
        artifacts.source_selection = Some(certified.source_selection);
        artifacts.certified_groups = certified.groups.to_vec();
        artifacts.recovery_products = certified.recovery_products.to_vec();
        artifacts.completed_source_evidence = valid_evidence.cloned();
        artifacts.exact_source_admission = exact_source;
        artifacts.producer_identity = Some(producer.as_slice().try_into().map_err(|_| {
            CompileError::ExtractFailed("bound compiler producer identity length".into())
        })?);
        module_candidates::record_deployment_acceptance(candidate_set.as_ref(), &receipt);
        if let Some((output_root, source_selection)) = deployment_export {
            valid_evidence.ok_or_else(|| {
                CompileError::ModulePackage(crate::toolchain::ModulePackageError::OpenCohort)
            })?;
            let publication = publication.as_ref().ok_or_else(|| {
                CompileError::ExtractFailed("module package publication unavailable".into())
            })?;
            module_candidates::deployment::export(
                output_root,
                source_selection,
                &deployment,
                publication,
            )?;
        }
        if exact_request.is_none()
            && !matches!(
                &policy,
                CompilationPolicy::BuildAction { .. } | CompilationPolicy::RetainedEntry { .. }
            )
        {
            if let Some(publication) = publication {
                module_candidates::publish_prepared(publication);
            }
        } else if exact_request.is_some() {
            module_candidates::record_exact_context_publication_skip(
                &artifacts.module_products[..fresh_count],
            );
        }
        // A memo hit has no certified source group owner mapping. The module
        // store owns reuse for invocations with home products.
        if allow_candidates
            && exact_request.is_none()
            && cached_receipts.is_empty()
            && fresh_count == 0
        {
            if let (Some(key), Some(evidence)) = (&inv_key, evidence.as_ref()) {
                store_memo(
                    key,
                    &name_refs,
                    &meta_bytes,
                    &raw,
                    &product_bytes,
                    evidence,
                    inv.source,
                );
            }
        }
        Ok(artifacts)
    })();
    let artifacts = assembled.map_err(|error| match &policy {
        CompilationPolicy::BuildAction {
            export: BuildActionExport::PreparedFixture { .. },
            ..
        } => retain_compiler_failure(output_owner.path(), &cmd, &compiler_stderr, error),
        CompilationPolicy::BuildAction { .. } => error,
        _ => retain_compiler_failure(output_owner.path(), &cmd, &compiler_stderr, error),
    })?;
    if let CompilationPolicy::BuildAction {
        export: BuildActionExport::PreparedFixture { output },
        ..
    } = &policy
    {
        std::fs::create_dir(output)?;
        // Portable code has no authority to hydrate the source-bound
        // native products or certificates from this transaction.
        std::fs::write(output.join("meta.cbor"), &meta_bytes)?;
        for target in &raw {
            std::fs::write(
                output.join(prepared_artifact_name(&target.target)),
                target.prepared_bytes.as_slice(),
            )?;
            std::fs::write(
                output.join(format!("{}.asks.json", target.target)),
                &target.asks_bytes,
            )?;
        }
    }
    let original_entry = if let CompilationOutputOwner::Original(preparation) = output_owner {
        match &policy {
            CompilationPolicy::RetainedEntry {
                source_path,
                sources,
                catalog,
                ..
            } => {
                let selection = match (&catalog, &candidate_set) {
                    (Some(_), Some(candidates)) => Some(
                        artifacts
                            .source_selection
                            .as_ref()
                            .ok_or_else(|| {
                                CompileError::ExtractFailed(
                                    "entry source selection unavailable".into(),
                                )
                            })?
                            .entry_dependency_selection(&artifacts.artifact_view, candidates)
                            .map_err(compiler_evidence_failure)?,
                    ),
                    (_, None) => None,
                    _ => {
                        return Err(CompileError::ExtractFailed(
                            "entry catalog selection unavailable".into(),
                        ))
                    }
                };
                let selection = selection.filter(|selection| selection.has_linked_originals());
                let selected_catalog = selection.as_ref().and(catalog.as_ref());
                Some(preparation.seal(
                    source_path,
                    sources,
                    &deployment,
                    inv.targets,
                    selected_catalog,
                    selection,
                )?)
            }
            CompilationPolicy::BuildAction {
                source_path,
                export:
                    BuildActionExport::ProductionEntry {
                        source_selection: sources,
                        ..
                    },
                ..
            } => Some(preparation.seal(
                source_path,
                sources,
                &deployment,
                inv.targets,
                None,
                None,
            )?),
            _ => {
                return Err(CompileError::ExtractFailed(
                    "original output owner has a different compilation policy".into(),
                ))
            }
        }
    } else {
        None
    };
    Ok(CompilationOutput {
        artifacts,
        original_entry,
    })
}

/// Retain worker outputs and stderr after compilation or final sealing fails.
/// The existing test-log policy is optional, and retention never replaces the
/// original error. Call before the request's temporary directory is dropped.
pub fn retain_compiler_failure(
    directory: &Path,
    command: &ExtractCmd,
    stderr: &[u8],
    error: CompileError,
) -> CompileError {
    retain_compiler_failure_inner(directory, stderr, error, None, command)
}

fn retain_compiler_failure_inner(
    directory: &Path,
    stderr: &[u8],
    error: CompileError,
    offer: Option<&ModuleCandidateOffer>,
    command: &ExtractCmd,
) -> CompileError {
    if std::env::var("TIDEPOOL_KEEP_TEST_LOGS").as_deref() != Ok("1") {
        return error;
    }
    match retain_failed_compiler_artifacts(directory, offer, command) {
        Ok(retained) => {
            if let Err(failure) = std::fs::write(retained.join("compiler.stderr"), stderr) {
                tracing::warn!(%failure, "could not retain compiler stderr");
            }
            tracing::warn!(path = %retained.display(), "retained failed compiler artifacts");
            match error {
                CompileError::ArtifactInventory(mut error) => {
                    if let Err(failure) = error.retain_owner_conflict(&retained) {
                        tracing::warn!(%failure, "could not retain exact owner conflict evidence");
                    }
                    error.diagnostic_artifacts = Some(retained);
                    CompileError::ArtifactInventory(error)
                }
                CompileError::ExtractFailed(message) => CompileError::ExtractFailed(format!(
                    "{message}; compiler artifacts retained at {}",
                    retained.display()
                )),
                CompileError::WorkerFailure(mut diagnostics) => {
                    diagnostics.push(diag::ExtractDiag {
                        span: None,
                        severity: diag::DiagnosticSeverity::Error,
                        message: format!("compiler artifacts retained at {}", retained.display()),
                    });
                    CompileError::WorkerFailure(diagnostics)
                }
                other => other,
            }
        }
        Err(failure) => {
            tracing::warn!(%failure, "could not retain failed compiler artifacts");
            error
        }
    }
}

fn retain_failed_compiler_artifacts(
    directory: &Path,
    offer: Option<&ModuleCandidateOffer>,
    command: &ExtractCmd,
) -> std::io::Result<PathBuf> {
    let retained_root = match std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir()?.join("target/tidepool-test-runs"),
    }
    .join("compiler-failures");
    std::fs::create_dir_all(&retained_root)?;
    let retained = TempDir::new_in(&retained_root)?;
    let mut source_retention_issues = Vec::new();
    if let Some(offer) = offer {
        if let Some(exact) = &offer.exact {
            if let Err(failure) = exact.retain_input_diagnostics(retained.path()) {
                tracing::warn!(%failure, "could not retain original exact request diagnostics");
                source_retention_issues.push(format!("exact request diagnostics: {failure}"));
            }
        }
        if let Some(selected) = &offer.selected {
            if let Err(failure) = selected.retain_evidence_diagnostics(retained.path()) {
                tracing::warn!(%failure, "could not retain original selected candidate evidence");
                source_retention_issues.push(format!("selected candidate diagnostics: {failure}"));
            }
        }
        if let Err(failure) = offer.retain_checked_inputs(retained.path()) {
            tracing::warn!(%failure, "could not retain selected checked input diagnostics");
            source_retention_issues.push(format!("checked input diagnostics: {failure}"));
        }
    }
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), retained.path().join(entry.file_name()))?;
        }
    }
    let planned = directory.join("planned-declaration");
    if planned.is_dir() {
        let destination = retained.path().join("planned-declaration");
        std::fs::create_dir(&destination)?;
        for entry in std::fs::read_dir(&planned)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                std::fs::copy(entry.path(), destination.join(entry.file_name()))?;
            }
        }
    }
    for (source, destination) in [
        (directory.to_path_buf(), retained.path().to_path_buf()),
        (planned, retained.path().join("planned-declaration")),
    ] {
        if let Err(failure) = retain_exact_compile_receipts(&source, &destination) {
            tracing::warn!(%failure, "could not retain exact compilation receipts");
            source_retention_issues.push(format!("exact compilation receipts: {failure}"));
        }
    }
    if let Err(failure) = retain_program_compile_diagnostics(directory, retained.path()) {
        tracing::warn!(%failure, "could not retain complete program diagnostics");
        source_retention_issues.push(format!("program diagnostics: {failure}"));
    }
    if let Err(failure) = failure_sources::retain(retained.path(), source_retention_issues) {
        tracing::warn!(%failure, "could not retain declared compiler sources");
    }
    std::fs::write(
        retained.path().join("compiler-request.bin"),
        command.request_bytes(),
    )?;
    // Compiler transport uses the caller's CWD; Rust compilation owners
    // do not change process CWD while a request is outstanding.
    std::fs::write(
        retained.path().join("compiler-cwd.bin"),
        std::env::current_dir()?.as_os_str().as_encoded_bytes(),
    )?;
    Ok(retained.keep())
}

fn retain_program_compile_diagnostics(source: &Path, destination: &Path) -> std::io::Result<()> {
    fn entries(path: &Path) -> std::io::Result<Vec<std::fs::DirEntry>> {
        let mut entries = std::fs::read_dir(path)?
            .take(4097)
            .collect::<Result<Vec<_>, _>>()?;
        if entries.len() > 4096 {
            return Err(std::io::Error::other(
                "excessive program diagnostic entries",
            ));
        }
        entries.sort_by_key(std::fs::DirEntry::file_name);
        Ok(entries)
    }
    fn copy_files(source: &Path, destination: &Path, remaining: &mut u64) -> std::io::Result<()> {
        std::fs::create_dir_all(destination)?;
        for entry in entries(source)? {
            if !entry.file_type()?.is_file() {
                continue;
            }
            let length = entry.metadata()?.len();
            if length > *remaining {
                return Err(std::io::Error::other(
                    "program diagnostics exceed byte bound",
                ));
            }
            *remaining -= length;
            std::fs::copy(entry.path(), destination.join(entry.file_name()))?;
        }
        Ok(())
    }
    fn copy_receipts(
        source: &Path,
        destination: &Path,
        remaining: &mut u64,
    ) -> std::io::Result<()> {
        let receipts = source.join(".exact-compilations");
        if !std::fs::symlink_metadata(&receipts).is_ok_and(|metadata| metadata.file_type().is_dir())
        {
            return Ok(());
        }
        for entry in entries(&receipts)? {
            if entry.file_type()?.is_dir() {
                copy_files(
                    &entry.path(),
                    &destination
                        .join(".exact-compilations")
                        .join(entry.file_name()),
                    remaining,
                )?;
            }
        }
        Ok(())
    }
    fn copy_scope(
        source: &Path,
        destination: &Path,
        remaining: &mut u64,
        entries_left: &mut usize,
        depth: usize,
    ) -> std::io::Result<()> {
        if depth > 16 {
            return Err(std::io::Error::other(
                "exact scope diagnostics exceed depth bound",
            ));
        }
        let children = entries(source)?;
        *entries_left = entries_left
            .checked_sub(children.len())
            .ok_or_else(|| std::io::Error::other("exact scope diagnostics exceed entry bound"))?;
        std::fs::create_dir_all(destination)?;
        for entry in children {
            let kind = entry.file_type()?;
            if kind.is_file() {
                let length = entry.metadata()?.len();
                if length > *remaining {
                    return Err(std::io::Error::other(
                        "program diagnostics exceed byte bound",
                    ));
                }
                *remaining -= length;
                std::fs::copy(entry.path(), destination.join(entry.file_name()))?;
            } else if kind.is_dir() {
                copy_scope(
                    &entry.path(),
                    &destination.join(entry.file_name()),
                    remaining,
                    entries_left,
                    depth + 1,
                )?;
            }
        }
        Ok(())
    }
    let mut remaining = 128 * 1024 * 1024;
    let exact_scope = source.join("exact-scope");
    if std::fs::symlink_metadata(&exact_scope).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        copy_scope(
            &exact_scope,
            &destination.join("exact-scope"),
            &mut remaining,
            &mut 4096,
            0,
        )?;
    }
    let mut admitted = 0;
    for entry in entries(source)? {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let selected = ["segment-", "item-"]
            .iter()
            .filter_map(|prefix| name.strip_prefix(prefix))
            .any(|ordinal| {
                !ordinal.is_empty() && ordinal.bytes().all(|byte| byte.is_ascii_digit())
            });
        if !selected || !entry.file_type()?.is_dir() {
            continue;
        }
        admitted += 1;
        if admitted > 4096 {
            return Err(std::io::Error::other(
                "excessive program diagnostic outputs",
            ));
        }
        let output = destination.join(name);
        copy_files(&entry.path(), &output, &mut remaining)?;
        copy_receipts(&entry.path(), &output, &mut remaining)?;
        let planned = entry.path().join("planned-declaration");
        if std::fs::symlink_metadata(&planned).is_ok_and(|metadata| metadata.file_type().is_dir()) {
            let output = output.join("planned-declaration");
            copy_files(&planned, &output, &mut remaining)?;
            copy_receipts(&planned, &output, &mut remaining)?;
        }
    }
    Ok(())
}

fn retain_exact_compile_receipts(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = source.join(".exact-compilations");
    if !source.try_exists()? {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(&source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let destination = destination.join(".exact-compilations");
    std::fs::create_dir_all(&destination)?;
    let mut remaining = 64 * 1024 * 1024_u64;
    if entries.len() > 4096 {
        return Err(std::io::Error::other("excessive exact receipt diagnostics"));
    }
    for entry in entries {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let output = destination.join(entry.file_name());
        std::fs::create_dir(&output)?;
        for name in ["receipt.cbor", "source.hs"] {
            let input = entry.path().join(name);
            let metadata = std::fs::symlink_metadata(&input)?;
            if !metadata.is_file() || metadata.len() > remaining {
                return Err(std::io::Error::other(
                    "exact receipt diagnostics exceed bound",
                ));
            }
            remaining -= metadata.len();
            std::fs::copy(input, output.join(name))?;
        }
    }
    Ok(())
}

enum CompileAttemptError {
    Endpoint(tidepool_extract_cmd::SpawnError),
    Deployment(crate::toolchain::ToolchainError),
    ModulePackage(crate::toolchain::ModulePackageError),
    Diagnostic(CompileError),
}
impl CompileAttemptError {
    fn into_compile_error(self) -> CompileError {
        match self {
            Self::Endpoint(error) => CompileError::CompilerEndpoint(error),
            Self::Deployment(error) => CompileError::ExtractFailed(error.to_string()),
            Self::ModulePackage(error) => CompileError::ModulePackage(error),
            Self::Diagnostic(error) => error,
        }
    }
}

enum CompileAttempt<T> {
    Cached(Box<CompiledArtifacts>),
    Executed(T),
}

fn retry_bounded<T, E>(
    mut attempt: impl FnMut() -> Result<T, E>,
    permits_retry: impl Fn(&E) -> bool,
) -> Result<T, E> {
    let mut rebinds = 0;
    loop {
        match attempt() {
            Err(error) if permits_retry(&error) && rebinds < 2 => rebinds += 1,
            result => return result,
        }
    }
}

/// Compile `source` against multiple named targets through
/// [`compile_invocation`]. See its doc for the full contract
/// (target semantics, asks sidecar shape, memoization, timing hook).
pub fn compile_targets(
    source: &str,
    targets: &[&str],
    include: &[PathBuf],
    on_stage: impl FnMut(&str, Duration, u64),
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<CompiledArtifacts, CompileError> {
    assert!(
        !targets.is_empty(),
        "compile_targets: at least one target is required"
    );
    let inv = CompileInvocation {
        source,
        targets,
        include,
        fallback_module_name: "Expr",
    };
    compile_invocation(&inv, on_stage, settlement)
}

// ---------------------------------------------------------------------------
// Shared spawn + read + deserialize (also used by `tidepool_runtime::compile_haskell`)
// ---------------------------------------------------------------------------

/// One target's raw (pre-deserialize) bytes.
pub(crate) struct RawTargetOutput {
    pub(crate) target: String,
    asks_bytes: Vec<u8>,
    prepared_bytes: Arc<Vec<u8>>,
}

/// Spawn `cmd` (already fully configured — input, output-dir, target(s),
/// includes) and pull `targets`' bytes off `temp_dir`, forwarding every
/// measured stage through `on_stage`. `log_stderr(text, success)` is called
/// once after the spawn, whatever the outcome, so each caller can apply its
/// own logging policy (`compile_haskell` always echoes non-empty stderr;
/// `compile_targets` only warns on failure).
///
/// A nonzero exit is read through the structured diagnostics contract
/// ([`diag::decode_extract_result`]) — the SAME reading `tidepool_runtime::compile_haskell`
/// already gives an ordinary eval compile, so a bad target name or any other
/// GHC-detectable failure here reports real spans, not an opaque stdout/stderr
/// dump.
pub(crate) fn extract_and_read(
    run: tidepool_extract_cmd::ExtractRun,
    temp_dir: &Path,
    targets: &[&str],
    multi: bool,
    mut on_stage: impl FnMut(&str, Duration, u64),
    log_stderr: impl FnOnce(&str, bool),
) -> Result<
    (
        Vec<u8>,
        Vec<RawTargetOutput>,
        Vec<u8>,
        Arc<tidepool_repr::execution_schema::InventoryOperation>,
    ),
    CompileError,
> {
    let inventory_operation = Arc::new(tidepool_repr::execution_schema::InventoryOperation::new(
        Default::default(),
    ));
    on_stage(timing::STAGE_EXTRACT_SPAWN, run.elapsed, 0);

    let stderr = run.stderr_lossy();
    let extract_timing = timing::ExtractTiming::parse(&stderr);
    for (phase, ms) in &extract_timing.phases {
        on_stage(
            &timing::extract_stage_name(phase),
            Duration::from_millis(*ms),
            0,
        );
    }
    // Default-on per-compile summary (compile-attribution lane): unlike
    // `extract_timing` above, this is emitted by the extract UNCONDITIONALLY
    // (no `TIDEPOOL_TIMING` required) — see `Tidepool.Timing.emitCompileSummary`.
    // Logged at INFO so it lands in the ordinary compile log; absent on
    // a memo hit (this function isn't reached) or on a compile that threw
    // before reaching the summary line.
    if let Some(summary) = timing::CompileSummary::parse(&stderr) {
        timing::log_compile_summary(&summary);
    }
    // Full per-module breakdown (compile-attribution lane): DEBUG-gated,
    // present only when `TIDEPOOL_TIMING=1` reached the extract — see
    // `timing::log_module_timings`'s doc for why this stays a level below
    // the always-on summary above.
    let module_timings = timing::parse_module_timings(&stderr);
    if !module_timings.is_empty() {
        timing::log_module_timings(&module_timings);
    }
    log_stderr(&stderr, run.success());

    diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)?;

    let cbor_read_start = Instant::now();
    let meta_path = temp_dir.join("meta.cbor");
    if !meta_path.exists() {
        return Err(CompileError::MissingOutput(meta_path));
    }
    let meta_bytes = certified_products::read_bounded_with_operation(
        &meta_path,
        inventory_operation.limits().max_bytes as u64,
        &inventory_operation,
    )
    .map_err(compiler_evidence_failure)?;
    let products_path = temp_dir.join("module-products.cbor");
    if !products_path.exists() {
        return Err(CompileError::MissingOutput(products_path));
    }
    let product_bytes = CompilerSidecar::ModuleProducts.read(temp_dir, &inventory_operation)?;

    let mut raw = Vec::with_capacity(targets.len());
    for target in targets {
        let prepared_path = temp_dir.join(prepared_artifact_name(target));
        if !prepared_path.exists() {
            return Err(CompileError::MissingOutput(prepared_path));
        }
        let prepared_bytes = certified_products::read_bounded_with_operation(
            &prepared_path,
            DecodeLimits::default().max_bytes as u64,
            &inventory_operation,
        )
        .map_err(compiler_evidence_failure)?;
        let asks_path = if multi {
            temp_dir.join(format!("{target}.asks.json"))
        } else {
            temp_dir.join("asks.json")
        };
        let asks_bytes = read_asks_bytes(&asks_path)?;
        raw.push(RawTargetOutput {
            target: (*target).to_string(),
            asks_bytes,
            prepared_bytes: Arc::new(prepared_bytes),
        });
    }
    on_stage(
        timing::STAGE_CBOR_READ,
        cbor_read_start.elapsed(),
        total_bytes(&meta_bytes, &raw, &product_bytes),
    );
    Ok((meta_bytes, raw, product_bytes, inventory_operation))
}

/// A prepared output and its complete constructor table disagree.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("target {target:?}: {mismatch}")]
pub struct ConstructorIdentityMismatch {
    pub target: String,
    pub mismatch: tidepool_repr::ConstructorMetadataMismatch,
}

/// All emitted constructors, including settlement and external declarations,
/// receive metadata from their owning projection transaction.
pub fn check_constructor_identity_agreement(
    target: &str,
    artifact: &PreparedArtifact,
    table: &DataConTable,
) -> Result<(), ConstructorIdentityMismatch> {
    table
        .validate_program(artifact.prepared())
        .map_err(|mismatch| ConstructorIdentityMismatch {
            target: target.to_owned(),
            mismatch,
        })
}

/// Deserialize a `(meta_bytes, raw)` pair — from a fresh spawn or a memo hit
/// — into a [`CompiledArtifacts`]. Both paths landing here (rather than each
/// deserializing separately) is what makes a cache hit observationally
/// identical to a cold compile.
pub(crate) fn assemble(
    meta_bytes: &[u8],
    raw: &[RawTargetOutput],
    mut on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    let deserialize_start = Instant::now();
    let requirements = crate::prepared_artifact::production_requirements()?;
    let prepared: Vec<PreparedArtifact> = raw
        .iter()
        .map(|r| {
            PreparedArtifact::parse_shared(
                Arc::clone(&r.prepared_bytes),
                &requirements,
                DecodeLimits::default(),
            )
        })
        .collect::<Result<_, _>>()?;
    let programs: Vec<_> = prepared.iter().map(PreparedArtifact::prepared).collect();
    let (table, warnings) =
        tidepool_repr::serial::read_metadata_for_programs(meta_bytes, &programs)?;
    on_stage(
        timing::STAGE_CBOR_DESERIALIZE,
        deserialize_start.elapsed(),
        0,
    );
    // Register varId → name pairs so runtime unresolved-variable errors can
    // name the symbol, and sentinel-slot → external-name pairs so a forced
    // kind-4 poison names the symbol it replaced — once, over the shared
    // merged table.

    let asks_start = Instant::now();
    let mut targets = BTreeMap::new();
    for (r, prepared) in raw.iter().zip(prepared) {
        let asks = parse_asks(&r.asks_bytes)?;
        targets.insert(
            r.target.clone(),
            TargetArtifact {
                asks,
                prepared,
                pending_imports: Vec::new(),
                package_interfaces: Default::default(),
            },
        );
    }
    on_stage(timing::STAGE_ASKS_PARSE, asks_start.elapsed(), 0);

    Ok(CompiledArtifacts {
        artifact_view: crate::artifact_inventory::ArtifactInventory::default().empty_view(),
        table,
        warnings,
        targets,
        module_products: Vec::new(),
        certified_groups: Vec::new(),
        recovery_products: Vec::new(),
        source_selection: None,
        producer_identity: None,
        module_inventory: None,
        completed_source_evidence: None,
        exact_source_admission: None,
    })
}

fn decode_fresh_products(bytes: &[u8]) -> Result<Vec<RawModuleProduct>, CompileError> {
    Ok(tidepool_repr::execution_schema::parse_module_products(
        bytes,
        &crate::prepared_artifact::production_requirements()?,
        module_candidates::product_decode_limits(),
    )?)
}

fn assemble_with_products(
    meta_bytes: &[u8],
    raw: &[RawTargetOutput],
    fresh_products: Vec<RawModuleProduct>,
    certified_cached: Vec<RawModuleProduct>,
    evidence: Option<&cache::DependencyEvidence>,
    exact: Option<&crate::declaration_context::ExactCompilationRequest>,
    certified_retained: Option<&certified_products::CertifiedRetainedCoreProducts>,
    on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    let mut artifacts = assemble(meta_bytes, raw, on_stage)?;
    artifacts.module_products = fresh_products;
    artifacts.module_products.extend(certified_cached);
    if let Some(evidence) = evidence {
        let protected: BTreeMap<_, _> = exact
            .map(|request| request.compiler_original_products())
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|product| {
                (
                    (product.owner().unit.clone(), product.owner().module.clone()),
                    product,
                )
            })
            .collect();
        let mut protected_groups = std::collections::HashMap::<_, BTreeMap<_, _>>::new();
        for group in exact.into_iter().flat_map(|request| request.groups.iter()) {
            protected_groups
                .entry(group.owner())
                .or_default()
                .insert(group.group().original_ordinal(), group.group());
        }
        let mut emitted = std::collections::HashSet::new();
        for product in &artifacts.module_products {
            let admitted = if let Some(original) =
                protected.get(&(product.unit.clone(), product.module.clone()))
            {
                let expected = protected_groups.get(original.owner());
                product.interface == original.interface_bytes()
                    && product.groups.len() == expected.map_or(0, BTreeMap::len)
                    && product.groups.iter().all(|group| {
                        expected
                            .and_then(|groups| groups.get(&group.original_ordinal()))
                            .is_some_and(|original| *original == group)
                    })
            } else if let Some(admitted) =
                certified_retained.and_then(|proof| proof.matches_emitted(product))
            {
                admitted
            } else {
                evidence.modules.iter().any(|module| {
                    !module.boot
                        && module.product == cache::ProductAvailability::Ready
                        && module.unit == product.unit
                        && module.module == product.module
                })
            };
            if !emitted.insert((&product.unit, &product.module)) || !admitted {
                return Err(CompileError::ExtractFailed(format!(
                    "module product {}:{} lacks a unique fresh graph, protected original inventory or certified Core promotion",
                    product.unit, product.module
                )));
            }
        }
        for module in &evidence.modules {
            if module.product == cache::ProductAvailability::Ready
                && !emitted.contains(&(&module.unit, &module.module))
            {
                return Err(CompileError::ExtractFailed(format!(
                    "ready graph node {}:{} has no module product",
                    module.unit, module.module
                )));
            }
        }
        artifacts.module_inventory = Some(evidence.modules.clone());
    }
    Ok(artifacts)
}

/// Read the required typed-yield sidecar. The extractor writes `[]` for a
/// program with no sites; absence therefore means an incomplete or stale
/// compiler artifact, never "no effects".
fn read_asks_bytes(path: &Path) -> Result<Vec<u8>, CompileError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(CompileError::MissingOutput(path.to_path_buf()))
        }
        Err(e) => Err(CompileError::Io(e)),
    }
}

fn parse_asks(bytes: &[u8]) -> Result<YieldSites, CompileError> {
    let sites: Vec<YieldSite> =
        serde_json::from_slice(bytes).map_err(|e| CompileError::Asks(e.to_string()))?;
    YieldSites::from_sites(sites).map_err(|error| CompileError::Asks(error.to_string()))
}

/// Read the compiler's required typed-yield sidecar for session-turn callers.
/// This is the sole path-level reader; multi-target compilation uses the same
/// byte parser after its cache layer has captured the artifact set.
pub fn read_yield_sites(path: &Path) -> Result<Vec<YieldSite>, CompileError> {
    let bytes = read_asks_bytes(path)?;
    serde_json::from_slice(&bytes).map_err(|e| CompileError::Asks(e.to_string()))
}

// ---------------------------------------------------------------------------
// Invocation-keyed memo glue
// ---------------------------------------------------------------------------

/// The logical artifact names of one invocation's output set — exactly the
/// filenames [`compile_targets`] reads out of the extract's output dir, in a
/// fixed order: shared `meta.cbor`, then per target `<target>.cbor`,
/// `<target>.prepared.cbor`, and its asks sidecar. `multi` picks the sidecar SHAPE on the same
/// `targets.len() > 1` test the Haskell side uses to decide which shape to
/// write, so the memo's names track the extract's own contract.
fn artifact_names(targets: &[&str], multi: bool) -> Vec<String> {
    let mut names = Vec::with_capacity(2 + targets.len() * 2);
    names.push("meta.cbor".to_string());
    for target in targets {
        names.push(prepared_artifact_name(target));
        names.push(if multi {
            format!("{target}.asks.json")
        } else {
            "asks.json".to_string()
        });
    }
    names.push("module-products.cbor".to_string());
    names.push("module-package-imports.cbor".to_string());
    names
}

/// Total bytes read for this compile, for the `cbor_read` timing stage —
/// identical on the memo path and the spawn path, since both count the same
/// artifacts.
fn total_bytes(meta_bytes: &[u8], raw: &[RawTargetOutput], product_bytes: &[u8]) -> u64 {
    let total = meta_bytes.len()
        + product_bytes.len()
        + raw
            .iter()
            .map(|r| r.prepared_bytes.len() + r.asks_bytes.len())
            .sum::<usize>();
    total as u64
}

/// Reassemble a memoized artifact set into the same `(meta, raw)` pair the
/// spawn path produces. `None` — any absent-but-required artifact, or a set
/// the memo declines — falls through to a cold compile.
fn load_memo(
    key: &cache::InvocationKey,
    names: &[&str],
    targets: &[&str],
    source: &str,
) -> std::io::Result<
    Option<(
        Vec<u8>,
        Vec<RawTargetOutput>,
        Vec<u8>,
        cache::DependencyEvidence,
    )>,
> {
    let Some((loaded, evidence)) = cache::artifacts_load(key, names, source)? else {
        return Ok(None);
    };
    Ok((|| {
        let mut it = loaded.into_iter();
        // Every artifact is required. An extractor represents a target with no
        // typed suspension sites by writing an `[]` sidecar.
        let meta_bytes = it.next()??;
        let mut raw = Vec::with_capacity(targets.len());
        for target in targets {
            let prepared_bytes = it.next()??;
            let asks_bytes = it.next()??;
            raw.push(RawTargetOutput {
                target: (*target).to_string(),
                asks_bytes,
                prepared_bytes: Arc::new(prepared_bytes),
            });
        }
        Some((meta_bytes, raw, it.next()??, evidence))
    })())
}

/// Store this invocation's full artifact set under `names`, in the order
/// [`artifact_names`] fixed.
fn store_memo(
    key: &cache::InvocationKey,
    names: &[&str],
    meta_bytes: &[u8],
    raw: &[RawTargetOutput],
    product_bytes: &[u8],
    evidence: &cache::DependencyEvidence,
    source: &str,
) {
    let mut artifacts: Vec<(&str, Option<&[u8]>)> = Vec::with_capacity(names.len());
    let mut names = names.iter();
    if let Some(name) = names.next() {
        artifacts.push((name, Some(meta_bytes)));
    }
    for r in raw {
        let (Some(prepared_name), Some(asks_name)) = (names.next(), names.next()) else {
            return;
        };
        artifacts.push((prepared_name, Some(r.prepared_bytes.as_slice())));
        artifacts.push((asks_name, Some(r.asks_bytes.as_slice())));
    }
    if let Some(name) = names.next() {
        artifacts.push((name, Some(product_bytes)));
    }
    cache::artifacts_store(key, &artifacts, evidence, source);
}

#[cfg(test)]
#[path = "artifacts/tests/private_package_input_history.rs"]
mod private_package_input_history;

#[cfg(test)]
mod compiler_sidecar_tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use tidepool_repr::execution_schema::{InventoryDecodeLimits, InventoryOperation, ParseError};

    #[test]
    fn aggregate_package_sidecar_reaches_decoder_above_individual_record_bound() {
        let root = tempfile::tempdir().unwrap();
        let mut packages = root.path().join("packages");
        for _ in 0..8 {
            packages.push("p".repeat(128));
        }
        std::fs::create_dir_all(&packages).unwrap();
        let interface = b"fixture interface";
        let interface_sha = Sha256::digest(interface)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut package_roots = Vec::new();
        for index in 0..2400 {
            let path = packages.join(format!("Package{index}.hi"));
            std::fs::write(&path, interface).unwrap();
            package_roots.push(Value::Array(vec![
                Value::Text("fixture-package".into()),
                Value::Text(format!("Fixture.Package{index}")),
                Value::Text(path.to_str().unwrap().into()),
                Value::Text(interface_sha.clone()),
            ]));
        }
        let mut sidecars = Vec::new();
        let mut products = Vec::new();
        for index in 0..13 {
            let module = format!("Owner{index}");
            let sidecar = module_candidates::tests::package_imports_with_roots(
                "main",
                &module,
                interface,
                package_roots.clone(),
            );
            assert!(sidecar.len() < 4 << 20);
            sidecars.push(("main".into(), module.clone(), sidecar));
            products.push(RawModuleProduct {
                unit: "main".into(),
                module,
                interface: interface.to_vec(),
                groups: vec![],
            });
        }
        let bytes = module_candidates::tests::package_bundle_with_sidecars(sidecars);
        assert!(bytes.len() > certified_products::COMPILER_RECEIPT_BYTES_LIMIT);
        let path = root
            .path()
            .join(CompilerSidecar::ModulePackageImports.filename());
        std::fs::write(&path, &bytes).unwrap();
        let operation = Arc::new(InventoryOperation::new(Default::default()));
        let read = CompilerSidecar::ModulePackageImports
            .read(root.path(), &operation)
            .unwrap();
        let decoded =
            module_candidates::split_package_imports_with_operation(&read, &products, &operation)
                .unwrap()
                .unwrap();
        assert_eq!(decoded.len(), products.len());
        let mut validation =
            crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(operation);
        for product in &products {
            assert_eq!(
                crate::recovery_artifacts::validate_package_imports_with_validation(
                    &decoded[&(product.unit.clone(), product.module.clone())],
                    &product.unit,
                    &product.module,
                    &Sha256::digest(interface).into(),
                    &path,
                    &mut validation,
                )
                .unwrap()
                .len(),
                package_roots.len()
            );
        }

        let restricted = InventoryOperation::new(InventoryDecodeLimits {
            max_bytes: bytes.len() - 1,
            ..Default::default()
        });
        let refusal = CompilerSidecar::ModulePackageImports
            .read(root.path(), &restricted)
            .unwrap_err();
        assert!(matches!(&refusal, CompileError::CompilerEvidence(_)));
        assert_eq!(
            crate::failclass::classify_compile(&refusal).class,
            crate::failclass::FailureClass::Infra
        );
        let exhausted = InventoryOperation::new(InventoryDecodeLimits {
            max_work: bytes.len(),
            ..Default::default()
        });
        assert!(
            matches!(CompilerSidecar::ModulePackageImports.read(root.path(), &exhausted),
            Err(CompileError::CompilerEvidence(error)) if matches!(error.as_ref(),
                certified_products::CertificationError::Product(ParseError::LimitExceeded("work"))))
        );
        let mut malformed = read;
        malformed.push(0);
        assert!(module_candidates::split_package_imports_with_operation(
            &malformed,
            &products,
            &InventoryOperation::new(Default::default()),
        )
        .unwrap()
        .is_none());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "refusal cannot alter compiler output"
        );
        std::fs::remove_file(&path).unwrap();
        let absent = CompilerSidecar::ModulePackageImports
            .read(root.path(), &InventoryOperation::new(Default::default()))
            .unwrap_err();
        assert!(
            matches!(&absent, CompileError::CompilerEvidence(error) if matches!(error.as_ref(),
            certified_products::CertificationError::EvidenceRead { failure: certified_products::EvidenceReadFailure::Io { .. }, .. }))
        );
        assert_eq!(
            crate::failclass::classify_compile(&absent).class,
            crate::failclass::FailureClass::Infra
        );
    }
}

#[cfg(test)]
mod typed_site_tests {
    use super::*;

    #[test]
    fn program_support_projection_retains_interface_only_exact_owners_and_excludes_scaffold() {
        let interfaces = [
            crate::certified_products::fixture_module_interface(
                [2; 32],
                "main",
                "Tidepool.Agent.Ref",
                BTreeMap::new(),
            ),
            crate::certified_products::fixture_module_interface(
                [2; 32],
                "main",
                "Generated",
                BTreeMap::new(),
            ),
            crate::certified_products::fixture_module_interface(
                [2; 32],
                "foreign",
                "Generated",
                BTreeMap::new(),
            ),
        ];
        let view = crate::declaration_context::certified_product_artifact_view(
            [2; 32],
            &[],
            &interfaces,
            None,
        )
        .unwrap();
        let support = program_support_artifacts(
            &view,
            &crate::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: "Generated".into(),
            },
        )
        .unwrap();
        assert_eq!(
            support
                .descriptors()
                .into_iter()
                .map(|entry| (entry.owner.unit, entry.owner.module, entry.kind))
                .collect::<Vec<_>>(),
            vec![
                (
                    "foreign".to_owned(),
                    "Generated".to_owned(),
                    crate::artifact_inventory::ArtifactKind::CanonicalModuleInterface
                ),
                (
                    "main".to_owned(),
                    "Tidepool.Agent.Ref".to_owned(),
                    crate::artifact_inventory::ArtifactKind::CanonicalModuleInterface
                ),
            ]
        );
        assert!(support.source_implementation_roles().is_empty());
    }

    #[test]
    fn program_support_projection_refuses_required_generated_scaffold() {
        let generated = crate::certified_products::fixture_module_interface(
            [2; 32],
            "main",
            "Generated",
            BTreeMap::new(),
        );
        let dependent = crate::certified_products::fixture_module_interface(
            [2; 32],
            "main",
            "Support",
            BTreeMap::from([(
                ("main".into(), "Generated".into()),
                generated.interface_sha256(),
            )]),
        );
        let view = crate::declaration_context::certified_product_artifact_view(
            [2; 32],
            &[],
            &[generated, dependent],
            None,
        )
        .unwrap();
        assert!(program_support_artifacts(
            &view,
            &crate::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: "Generated".into(),
            }
        )
        .is_err());
        assert_eq!(view.descriptors().len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn protected_search_authorization_refuses_lossy_path_encoding() {
        use std::os::unix::ffi::OsStringExt;
        let authorization =
            || Value::Array(vec![Value::Text(CheckedPurpose::Cell.wire_tag().into())]);
        let invalid = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]));
        assert!(checked_search_authorization(authorization(), &[invalid]).is_err());
        let paths = [
            PathBuf::from("/exact//first"),
            PathBuf::from("/exact/../second"),
        ];
        let encoded = checked_search_authorization(authorization(), &paths).unwrap();
        let Value::Array(fields) = encoded else {
            panic!("authorization must be an array")
        };
        assert_eq!(
            fields[0],
            Value::Text(CheckedPurpose::Cell.wire_tag().into())
        );
        assert_eq!(
            fields[1],
            Value::Array(vec![
                Value::Text("/exact//first".into()),
                Value::Text("/exact/../second".into())
            ])
        );
        assert!(!crate::checked_cell::same_include_paths(
            &[PathBuf::from("/exact//first")],
            &[PathBuf::from("/exact/first")]
        ));
    }

    fn site(id: u64, ty: &str) -> YieldSite {
        YieldSite {
            reply_declaration: None,
            request_type_signatures: None,
            site: id,
            origin: "M.program".into(),
            ordinal: 0,
            ty: ty.into(),
            modules: Vec::new(),
            heads: Vec::new(),
            inputs: Vec::new(),
            input_type_witnesses: Vec::new(),
        }
    }

    #[test]
    fn missing_sidecar_is_not_an_empty_site_set() {
        let root = tempfile::tempdir().expect("temporary artifact root");
        let path = root.path().join("asks.json");
        assert!(matches!(
            read_yield_sites(&path),
            Err(CompileError::MissingOutput(missing)) if missing == path
        ));
    }

    #[test]
    fn empty_sidecar_is_the_only_empty_site_set() {
        let root = tempfile::tempdir().expect("temporary artifact root");
        let path = root.path().join("asks.json");
        std::fs::write(&path, b"[]").expect("write empty sidecar");
        assert_eq!(read_yield_sites(&path).expect("read empty sidecar"), vec![]);
    }

    #[test]
    fn conflicting_site_id_is_a_structured_error() {
        let collision = YieldSites::from_sites(vec![site(7, "Int"), site(7, "Bool")])
            .expect_err("conflicting metadata must fail");
        assert_eq!(collision.site, 7);
        assert_eq!(collision.first.ty, "Int");
        assert_eq!(collision.second.ty, "Bool");
    }

    #[test]
    #[serial_test::serial]
    fn observed_compiler_cannot_compile_without_configured_deployment() {
        use std::os::unix::fs::PermissionsExt;
        struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (name, value) in &self.0 {
                    match value {
                        Some(value) => unsafe { std::env::set_var(name, value) },
                        None => unsafe { std::env::remove_var(name) },
                    }
                }
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let frontend = directory.path().join("frontend");
        let executed = directory.path().join("executed");
        std::fs::write(
            &frontend,
            format!(
                "#!/bin/sh\nprintf 'TPCID002{}{}'\nif IFS= read -r row; then touch '{}'; fi\n",
                "a".repeat(32),
                "b".repeat(32),
                executed.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&frontend, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _restore = Restore(
            [
                crate::toolchain::ENV_EXTRACT,
                crate::toolchain::ENV_COMPILER_DEPLOYMENT,
                tidepool_extract_cmd::DAEMON_SOCKET_ENV,
            ]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect(),
        );
        unsafe {
            std::env::set_var(crate::toolchain::ENV_EXTRACT, &frontend);
            std::env::remove_var(crate::toolchain::ENV_COMPILER_DEPLOYMENT);
            std::env::remove_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV);
        }
        let error = crate::artifacts::test_support::compile_targets(
            "result = 1",
            &["result"],
            &[directory.path().into()],
            |_, _, _| {},
        )
        .err()
        .expect("unconfigured observed endpoint must be refused");
        assert!(
            matches!(error,CompileError::ExtractFailed(ref message) if message.contains("no configured deployment authority"))
        );
        assert!(
            !executed.exists(),
            "an observed endpoint executed unauthorised compiler work"
        );
    }

    #[test]
    #[serial_test::serial]
    fn safe_refusal_rederives_cache_and_build_products_identity() {
        use std::os::unix::fs::PermissionsExt;

        struct RestoreDaemon(Option<std::ffi::OsString>);
        impl Drop for RestoreDaemon {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(socket) => unsafe {
                        std::env::set_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV, socket);
                    },
                    None => unsafe {
                        std::env::remove_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV);
                    },
                }
            }
        }

        let _restore_daemon =
            RestoreDaemon(std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV));
        // This test binds two explicit producers. A surrounding battery may
        // provide a resident daemon, which would otherwise replace both and
        // make the producer identities identical.
        unsafe {
            std::env::remove_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV);
        }

        let root = tempfile::tempdir().expect("temporary endpoint root");
        let endpoints = [root.path().join("refused"), root.path().join("rebound")];
        for (path, identity_byte) in endpoints.iter().zip([b'a', b'b']) {
            let identity = String::from_utf8(vec![identity_byte; 32]).unwrap();
            std::fs::write(
                path,
                format!("#!/bin/sh\nprintf 'TPCID002{identity}{identity}'\ncat >/dev/null\n"),
            )
            .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut attempt_index = 0;
        let mut selections = Vec::new();
        let selected = retry_bounded(
            || {
                let mut cmd = ExtractCmd::with_bin(
                    tidepool_extract_cmd::ResolvedExtractBin::assume_resolved(
                        &endpoints[attempt_index],
                    ),
                );
                cmd.input("Input.hs").target("result");
                let endpoint = cmd.bind().expect("fake endpoint must bind");
                attempt_index += 1;
                crate::paths::apply_build_products_dir(&mut cmd, &endpoint);
                let argv = cmd.argv();
                let key = cache::invocation_key(&cache::Invocation {
                    source: "result = 1",
                    argv: &argv,
                    input_path: Path::new("Input.hs"),
                    include: &[],
                    endpoint_identity: endpoint.identity().producer_bytes(),
                })
                .expect("test invocation is cacheable");
                let products = argv
                    .windows(2)
                    .find(|pair| pair[0] == "--build-products-dir")
                    .map(|pair| PathBuf::from(&pair[1]))
                    .expect("bound attempt must select build products");
                selections.push((key, products));
                if attempt_index == 1 {
                    Err(true)
                } else {
                    Ok(attempt_index - 1)
                }
            },
            |known_unsubmitted| *known_unsubmitted,
        )
        .expect("known-unsubmitted refusal must rebind");

        assert_eq!(selected, 1);
        assert_eq!(selections.len(), 2);
        assert_ne!(selections[0].0, selections[1].0);
        assert_ne!(selections[0].1, selections[1].1);
    }
}

#[cfg(test)]
mod module_product_tests {
    use super::*;
    use crate::certified_products::ProductOrigin;
    use std::io::Write;

    struct RestoreEnvironment(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl Drop for RestoreEnvironment {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                match value {
                    Some(value) => unsafe { std::env::set_var(name, value) },
                    None => unsafe { std::env::remove_var(name) },
                }
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn worker_failure_retains_diagnostics_and_executed_request_after_scratch_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let _restore = RestoreEnvironment(
            ["TIDEPOOL_KEEP_TEST_LOGS", "TIDEPOOL_TEST_ARTIFACT_ROOT"]
                .into_iter()
                .map(|name| (name, std::env::var_os(name)))
                .collect(),
        );
        unsafe {
            std::env::set_var("TIDEPOOL_KEEP_TEST_LOGS", "1");
            std::env::set_var("TIDEPOOL_TEST_ARTIFACT_ROOT", root.path());
        }
        let scratch = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let input = scratch.path().join("turn.txt");
        let source = b"value <- pure (41 :: Int)\n";
        std::fs::write(&input, source).unwrap();
        std::fs::write(output.path().join("turn-attempt.hs"), source).unwrap();
        let mut command = ExtractCmd::with_bin(
            tidepool_extract_cmd::ResolvedExtractBin::assume_resolved("unused-worker"),
        );
        command
            .input(&input)
            .turn()
            .turn_out(scratch.path().join("turn.cbor"))
            .output_dir(scratch.path());
        let before_relocation = command.request_bytes();
        command.relocate_turn_outputs(output.path());
        let executed_request = command.request_bytes();
        assert_ne!(executed_request, before_relocation);
        let stderr = format!(
            "{}\nfull retained diagnostic tail\n",
            "warning\n".repeat(800)
        );
        let error = diag::decode_extract_result(
            false,
            br#"{"version":2,"outcome":"worker-failure","diagnostics":[{"span":null,"severity":"error","message":"native product certification failed"}]}"#,
            stderr.as_bytes(),
        )
        .unwrap_err();
        let before = crate::classify_compile(&error);
        let CompileError::WorkerFailure(original) = &error else {
            panic!("worker report must retain its typed failure cause");
        };
        let original = original.clone();
        let error = retain_compiler_failure(output.path(), &command, stderr.as_bytes(), error);
        drop(output);
        drop(scratch);

        let retained = std::fs::read_dir(root.path().join("compiler-failures"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1);
        let retained = &retained[0];
        let CompileError::WorkerFailure(diagnostics) = &error else {
            panic!("retention must not replace the worker failure variant");
        };
        assert_eq!(&diagnostics[..original.len()], original.as_slice());
        assert_eq!(diagnostics.len(), original.len() + 1);
        assert_eq!(diagnostics.last().unwrap().span, None);
        let after = crate::classify_compile(&error);
        assert_eq!(after.class, before.class);
        assert_eq!(after.phase, before.phase);
        assert_eq!(after.cause, before.cause);
        assert!(after.message.contains(&retained.display().to_string()));
        assert!(!before.message.contains("full retained diagnostic tail"));
        assert_eq!(
            std::fs::read(retained.join("compiler.stderr")).unwrap(),
            stderr.as_bytes()
        );
        assert_eq!(
            std::fs::read(retained.join("compiler-request.bin")).unwrap(),
            executed_request
        );
        assert_eq!(
            std::fs::read(retained.join("turn-attempt.hs")).unwrap(),
            source
        );
        assert_eq!(
            std::fs::read(retained.join("compiler-cwd.bin")).unwrap(),
            std::env::current_dir()
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
        );
    }

    #[test]
    fn failed_execution_diagnostics_preserve_typed_request_after_scratch_cleanup() {
        let scratch = tempfile::tempdir().unwrap();
        let input = scratch.path().join("CellCheck.hs");
        let source = b"module CellCheck where\nvalue = 41 :: Int\n";
        std::fs::write(&input, source).unwrap();
        let mut command = ExtractCmd::with_bin(
            tidepool_extract_cmd::ResolvedExtractBin::assume_resolved("missing-worker"),
        );
        command
            .input(&input)
            .output_dir(scratch.path())
            .check_source();
        let request = command.request_bytes();
        let retained = retain_failed_compiler_artifacts(scratch.path(), None, &command).unwrap();
        drop(scratch);
        let captured = std::fs::read(retained.join("compiler-request.bin")).unwrap();
        assert_eq!(captured, request);
        assert_eq!(
            tidepool_extract_cmd::ExtractRequest::decode(&captured)
                .unwrap()
                .encode(),
            request
        );
        assert_eq!(
            std::fs::read(retained.join("CellCheck.hs")).unwrap(),
            source
        );
        assert_eq!(
            std::fs::read(retained.join("compiler-cwd.bin")).unwrap(),
            std::env::current_dir()
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
        );
        std::fs::remove_dir_all(retained).unwrap();
    }

    #[test]
    fn program_failure_diagnostics_retain_nested_receipts_without_following_links() {
        use std::os::unix::fs::symlink;
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let planned = source.path().join("segment-0/planned-declaration");
        let receipt = planned.join(".exact-compilations/checked");
        std::fs::create_dir_all(&receipt).unwrap();
        std::fs::write(planned.join("dependencies.json"), b"typed source evidence").unwrap();
        std::fs::write(receipt.join("receipt.cbor"), b"exact receipt").unwrap();
        std::fs::write(receipt.join("source.hs"), b"module Original where").unwrap();
        let item = source.path().join("item-0");
        std::fs::create_dir(&item).unwrap();
        std::fs::write(item.join("value.hi.requirements"), b"private type owners").unwrap();
        symlink(
            planned.join("dependencies.json"),
            item.join("foreign-input"),
        )
        .unwrap();
        symlink(&planned, source.path().join("segment-1")).unwrap();
        std::fs::create_dir(source.path().join("unrelated")).unwrap();
        std::fs::write(source.path().join("unrelated/secret"), b"unrelated").unwrap();
        let exact = source.path().join("exact-scope/artifacts/owner");
        std::fs::create_dir_all(&exact).unwrap();
        std::fs::write(exact.join("scope.cbor"), b"exact scope").unwrap();
        std::fs::write(exact.join("original.hi"), b"interface bytes").unwrap();
        symlink(source.path().join("unrelated"), exact.join("foreign-root")).unwrap();
        retain_program_compile_diagnostics(source.path(), destination.path()).unwrap();
        assert_eq!(
            std::fs::read(
                destination
                    .path()
                    .join("exact-scope/artifacts/owner/original.hi")
            )
            .unwrap(),
            b"interface bytes"
        );
        assert!(!destination
            .path()
            .join("exact-scope/artifacts/owner/foreign-root")
            .exists());
        assert_eq!(
            std::fs::read(
                destination
                    .path()
                    .join("segment-0/planned-declaration/dependencies.json")
            )
            .unwrap(),
            b"typed source evidence"
        );
        assert_eq!(
            std::fs::read(
                destination
                    .path()
                    .join("segment-0/planned-declaration/.exact-compilations/checked/receipt.cbor")
            )
            .unwrap(),
            b"exact receipt"
        );
        assert!(destination
            .path()
            .join("item-0/value.hi.requirements")
            .is_file());
        assert!(!destination.path().join("item-0/foreign-input").exists());
        assert!(!destination.path().join("segment-1").exists());
        assert!(!destination.path().join("unrelated").exists());
    }

    #[test]
    fn ready_zero_group_module_requires_a_receipt_even_without_products() {
        let evidence = cache::DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![],
            resolutions: vec![],
            packages: vec![],
            modules: vec![cache::ModuleEvidence {
                unit: "main".into(),
                module: "InstanceOnly".into(),
                boot: false,
                source: PathBuf::from("InstanceOnly.hs"),
                imports: vec![],
                product: cache::ProductAvailability::Ready,
            }],
        };
        assert!(has_ready_home_module(&evidence));
        assert!(matches!(
            ensure_ready_module_inventory(&[], &evidence),
            Err(CompileError::ExtractFailed(_))
        ));
        let owner = certified_products::CertifiedModuleReceipt {
            origin: ProductOrigin::Cached,
            unit: "main".into(),
            module: "InstanceOnly".into(),
            module_version: None,
            skinny_iface_sha256: [1; 32],
            product_sha256: [2; 32],
            source_sha256: [3; 32],
            dependency_witness_sha256: [4; 32],
            interface_requirements: BTreeMap::new(),
            groups: vec![],
        };
        ensure_ready_module_inventory(&[owner], &evidence).unwrap();
    }

    fn assembly_product(module: &str, ordinals: &[u32]) -> RawModuleProduct {
        use tidepool_repr::execution_schema::{testing, Group};
        RawModuleProduct {
            unit: "main".into(),
            module: module.into(),
            interface: module.as_bytes().to_vec(),
            groups: ordinals
                .iter()
                .map(|ordinal| {
                    let mut wire = testing::wire_program();
                    let Group::NonRecursive(top) = &mut wire.bindings[0] else {
                        unreachable!()
                    };
                    top.identity.unit = "main".into();
                    top.identity.module = module.into();
                    top.identity.occurrence = format!("entry{ordinal}");
                    testing::projected_group(wire, *ordinal).unwrap()
                })
                .collect(),
        }
    }

    fn assembly_evidence(modules: &[&str]) -> cache::DependencyEvidence {
        cache::DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: Vec::new(),
            resolutions: Vec::new(),
            packages: Vec::new(),
            modules: modules
                .iter()
                .map(|module| cache::ModuleEvidence {
                    unit: "main".into(),
                    module: (*module).into(),
                    boot: false,
                    source: PathBuf::from(format!("{module}.hs")),
                    imports: Vec::new(),
                    product: cache::ProductAvailability::Ready,
                })
                .collect(),
        }
    }

    #[test]
    fn build_action_source_closure_accepts_declared_links_and_refuses_ambient_sources() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Main.hs");
        let dependency = root.path().join("Dependency.hs");
        let inputs = root.path().join("inputs");
        std::fs::write(&source, "module Main where\nresult = 42\n").unwrap();
        std::fs::write(&dependency, "module Dependency where\nvalue = 1\n").unwrap();
        std::fs::create_dir(&inputs).unwrap();
        std::os::unix::fs::symlink(&dependency, inputs.join("Dependency.hs")).unwrap();
        let mut evidence = assembly_evidence(&["Main", "Dependency"]);
        evidence.sources = [source.clone(), dependency.clone()]
            .into_iter()
            .map(|path| cache::SourceEvidence {
                path,
                sha256: "unused-by-closure-validator".into(),
            })
            .collect();
        let encode = |evidence: &cache::DependencyEvidence| serde_json::to_vec(evidence).unwrap();
        validate_build_action_source_closure(&encode(&evidence), &source, &[inputs]).unwrap();
        assert!(validate_build_action_source_closure(&encode(&evidence), &source, &[]).is_err());
        evidence.sources.pop();
        evidence.selection_complete = false;
        assert!(validate_build_action_source_closure(&encode(&evidence), &source, &[]).is_err());
    }

    fn product_metadata() -> Vec<u8> {
        tidepool_repr::serial::write_metadata(&DataConTable::new(), &MetaWarnings::default())
            .unwrap()
    }

    #[test]
    fn fresh_product_assembly_reuses_validated_module_and_group_allocations() {
        let fresh = vec![
            assembly_product("A", &[0, 1]),
            assembly_product("B", &[2, 3]),
        ];
        let modules = fresh.as_ptr();
        let groups = fresh
            .iter()
            .map(|product| product.groups.as_ptr())
            .collect::<Vec<_>>();
        let signatures = fresh
            .iter()
            .flat_map(|product| &product.groups)
            .map(|group| group.definitions().signatures().as_ptr())
            .collect::<Vec<_>>();
        let artifacts = assemble_with_products(
            &product_metadata(),
            &[],
            fresh,
            Vec::new(),
            Some(&assembly_evidence(&["A", "B"])),
            None,
            None,
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(artifacts.module_products.as_ptr(), modules);
        assert_eq!(
            artifacts
                .module_products
                .iter()
                .map(|product| product.groups.as_ptr())
                .collect::<Vec<_>>(),
            groups
        );
        assert_eq!(
            artifacts
                .module_products
                .iter()
                .flat_map(|product| &product.groups)
                .map(|group| group.definitions().signatures().as_ptr())
                .collect::<Vec<_>>(),
            signatures
        );
    }

    #[test]
    fn fresh_product_assembly_preserves_fresh_prefix_and_cached_only_inventory() {
        let cached = assembly_product("Cached", &[8, 9]);
        let cached_signatures = cached.groups[0].definitions().signatures().as_ptr();
        let fresh = vec![assembly_product("Fresh", &[0, 1])];
        let artifacts = assemble_with_products(
            &product_metadata(),
            &[],
            fresh,
            vec![cached],
            Some(&assembly_evidence(&["Fresh", "Cached"])),
            None,
            None,
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            artifacts
                .module_products
                .iter()
                .map(|product| product.module.as_str())
                .collect::<Vec<_>>(),
            ["Fresh", "Cached"]
        );
        assert_eq!(
            artifacts.module_products[1].groups[0]
                .definitions()
                .signatures()
                .as_ptr(),
            cached_signatures
        );
        let cached = vec![assembly_product("Cached", &[8, 9])];
        let artifacts = assemble_with_products(
            &product_metadata(),
            &[],
            Vec::new(),
            cached,
            Some(&assembly_evidence(&["Cached"])),
            None,
            None,
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(artifacts.module_products[0].module, "Cached");
    }

    #[test]
    fn fresh_product_assembly_handles_empty_products_and_refuses_duplicate_owners() {
        assert!(assemble_with_products(
            &product_metadata(),
            &[],
            Vec::new(),
            Vec::new(),
            Some(&assembly_evidence(&[])),
            None,
            None,
            |_, _, _| {}
        )
        .unwrap()
        .module_products
        .is_empty());
        assert!(assemble_with_products(
            &product_metadata(),
            &[],
            vec![assembly_product("Owner", &[0])],
            vec![assembly_product("Owner", &[1])],
            Some(&assembly_evidence(&["Owner"])),
            None,
            None,
            |_, _, _| {}
        )
        .is_err());
        assert!(assemble_with_products(
            &product_metadata(),
            &[],
            vec![assembly_product("Other", &[0])],
            Vec::new(),
            Some(&assembly_evidence(&["Owner"])),
            None,
            None,
            |_, _, _| {}
        )
        .is_err());
    }

    #[test]
    fn retained_core_assembly_requires_certifier_proof() {
        let proof = certified_products::CertifiedRetainedCoreProducts::default();
        assert!(assemble_with_products(
            &product_metadata(),
            &[],
            vec![assembly_product("Retained", &[0])],
            Vec::new(),
            Some(&assembly_evidence(&[])),
            None,
            Some(&proof),
            |_, _, _| {},
        )
        .is_err());
    }

    #[test]
    fn fresh_product_byte_admission_refuses_malformed_and_duplicate_wire_owners() {
        let rows = |names: &[&str]| {
            Value::Array(vec![
                Value::Text("TPMOD".into()),
                Value::Integer(1.into()),
                Value::Array(
                    names
                        .iter()
                        .map(|name| {
                            Value::Array(vec![
                                Value::Text("main".into()),
                                Value::Text((*name).into()),
                                Value::Bytes(vec![1]),
                                Value::Array(Vec::new()),
                            ])
                        })
                        .collect(),
                ),
            ])
        };
        let encode = |value| {
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&value, &mut bytes).unwrap();
            bytes
        };
        assert!(decode_fresh_products(b"malformed product").is_err());
        assert!(decode_fresh_products(&encode(rows(&["Duplicate", "Duplicate"]))).is_err());
        let fresh = decode_fresh_products(&encode(rows(&["First", "Second"]))).unwrap();
        let allocation = fresh.as_ptr();
        let artifacts = assemble_with_products(
            &product_metadata(),
            &[],
            fresh,
            Vec::new(),
            Some(&assembly_evidence(&["First", "Second"])),
            None,
            None,
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(artifacts.module_products.as_ptr(), allocation);
        assert!(decode_fresh_products(&encode(rows(&[])))
            .unwrap()
            .is_empty());
    }

    /// This test intentionally crosses the matched Rust frontend, Haskell
    /// worker, entry-free wire reader and invocation bundle. Run it with
    /// `TIDEPOOL_EXTRACT` and `TIDEPOOL_EXTRACT_WORKER` from this checkout.
    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn real_worker_products_survive_the_production_compile_front_door() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bridge/haskell/test-prepared-stg");
        let source = std::fs::read_to_string(root.join("ModuleProductB.hs")).unwrap();
        let compiled = crate::artifacts::test_support::compile_targets(
            &source,
            &["consume"],
            &[root],
            |_, _, _| {},
        )
        .unwrap();
        let modules: std::collections::BTreeSet<_> = compiled
            .module_products
            .iter()
            .map(|product| product.module.as_str())
            .collect();
        assert!(modules.contains("ModuleProductA"));
        assert!(modules.contains("ModuleProductB"));
        assert!(compiled
            .module_products
            .iter()
            .any(|product| product.module == "ModuleProductA" && product.groups.len() >= 2));
        let inventory = compiled
            .module_inventory
            .expect("safe worker graph must accompany its module products");
        let consumer = inventory
            .iter()
            .find(|node| node.module == "ModuleProductB" && !node.boot)
            .expect("consumer graph node");
        assert_eq!(consumer.product, cache::ProductAvailability::Ready);
        assert!(consumer.imports.iter().any(|imported| {
            imported.module == "ModuleProductA" && imported.selected.is_some()
        }));
    }

    #[test]
    #[ignore = "requires matched Haskell worker and frontend"]
    #[serial_test::serial]
    fn real_deployment_package_reuses_cohort_and_invalidates_source_shadow() {
        let package = crate::toolchain::configured_module_package()
            .unwrap()
            .expect("requires the matched native runtime catalog");
        let source_selection = package.source_selection().clone();
        let roots = source_selection.include_roots();
        let catalog_path =
            PathBuf::from(std::env::var_os(crate::toolchain::ENV_COMPILER_MODULES).unwrap());
        let catalog: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&catalog_path).unwrap()).unwrap();
        let root = catalog_path.parent().unwrap();
        let cohort: std::collections::BTreeSet<_> = catalog["modules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|module| {
                let owner: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(root.join(module["owner"]["path"].as_str().unwrap())).unwrap(),
                )
                .unwrap();
                owner["module"].as_str().unwrap().to_owned()
            })
            .collect();
        assert!(cohort.contains("Tidepool.Prelude"));
        assert!(cohort.contains("Tidepool.FilePath"));
        for variable in ["TIDEPOOL_COMPILE_CACHE_DIR", "TIDEPOOL_BUILD_PRODUCTS_DIR"] {
            let directory =
                PathBuf::from(std::env::var_os(variable).expect("isolated empty cache"));
            assert_eq!(std::fs::read_dir(directory).unwrap().count(), 0);
        }
        let source = include_str!("../tests/fixtures/deployment-module-package/Consumer.hs");
        let original = crate::artifacts::test_support::compile_targets(
            source,
            &["result"],
            &roots,
            |_, _, _| {},
        )
        .expect("fresh-process packaged cohort reuse");
        let cached: std::collections::BTreeSet<_> = original
            .certified_groups
            .iter()
            .filter(|group| group.origin() == ProductOrigin::Cached)
            .map(|group| group.owner().module.clone())
            .collect();
        assert!(cached.contains("Tidepool.Prelude"));
        assert!(cached.contains("Tidepool.FilePath"));
        assert!(original
            .certified_groups
            .iter()
            .all(|group| !cohort.contains(&group.owner().module)
                || group.origin() == ProductOrigin::Cached));
        let owners: std::collections::BTreeMap<_, _> = original
            .certified_groups
            .iter()
            .filter(|group| cohort.contains(&group.owner().module))
            .map(|group| (group.owner().module.clone(), group.owner().clone()))
            .collect();
        let shadow = tempfile::tempdir().unwrap();
        std::fs::create_dir(shadow.path().join("Tidepool")).unwrap();
        let mut changed = std::fs::read(
            source_selection
                .root(crate::toolchain::NativeSourceRole::Stdlib)
                .join("Tidepool/FilePath.hs"),
        )
        .unwrap();
        changed.extend_from_slice(b"\n-- higher-priority source witness\n");
        std::fs::write(shadow.path().join("Tidepool/FilePath.hs"), changed).unwrap();
        let mut shadow_roots = vec![shadow.path().to_owned()];
        shadow_roots.extend(roots);
        let shadowed = crate::artifacts::test_support::compile_targets(
            source,
            &["result"],
            &shadow_roots,
            |_, _, _| {},
        )
        .unwrap();
        for module in ["Tidepool.FilePath", "Tidepool.Prelude"] {
            assert!(
                shadowed
                    .certified_groups
                    .iter()
                    .any(|group| group.owner().module == module
                        && group.origin() == ProductOrigin::Fresh),
                "{module} and its importer must be rebuilt"
            );
            assert!(!shadowed
                .certified_groups
                .iter()
                .any(|group| group.owner().module == module
                    && group.origin() == ProductOrigin::Cached));
        }
        assert!(
            shadowed
                .certified_groups
                .iter()
                .any(|group| group.origin() == ProductOrigin::Cached
                    && owners.get(&group.owner().module) == Some(group.owner())),
            "an unaffected original owner survives source shadowing"
        );
        eprintln!(
            "deployment catalog {}: {} packaged modules, {} cached owners; shadowed FilePath and Prelude rebuilt",
            package.catalog_identity(),
            cohort.len(),
            cached.len()
        );
    }

    #[test]
    #[serial_test::serial]
    fn production_entry_ambiguous_submission_cannot_be_released_by_later_refusal() {
        use production_entry::{EntryPreparation, EntrySubmission};
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("entry");
        let mut preparation = EntryPreparation::reserve(&output).unwrap();
        preparation.mark_uncertain_submission();
        let previous = preparation.begin_execution();
        assert_eq!(previous, EntrySubmission::MayHaveExecuted);
        preparation.confirm_unsubmitted_attempt(previous);
        let original = preparation.raw().join("original-partial-output");
        std::fs::write(&original, b"original bytes").unwrap();
        let cancellation = tidepool_extract_cmd::CompilerTransactionCancellation::new();
        cancellation.cancel();
        let stopped = tidepool_extract_cmd::with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || crate::host_work::read(&original),
        );
        assert_eq!(
            stopped.action.unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(
            stopped.close,
            tidepool_extract_cmd::CompilerTransactionClose::NotStarted
        );
        preparation.release_if_unsubmitted().unwrap();
        assert_eq!(std::fs::read(&original).unwrap(), b"original bytes");
        assert!(matches!(
            EntryPreparation::reserve(&output),
            Err(CompileError::EntryPreparationUnfinished { .. })
        ));
    }

    #[test]
    #[serial_test::serial]
    fn production_entry_unsubmitted_release_requires_confirmed_absence_before_retry() {
        use production_entry::{EntryCheckpoint, EntryPreparation};
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("entry");
        let preparation = EntryPreparation::reserve(&output).unwrap();
        let error = production_entry::with_failure(EntryCheckpoint::ReleaseSync, || {
            preparation.release_if_unsubmitted()
        })
        .unwrap_err();
        assert!(matches!(
            error,
            CompileError::EntryReservationReleaseUnconfirmed { .. }
        ));
        let error = production_entry::with_failure(EntryCheckpoint::AbsenceConfirmation, || {
            EntryPreparation::reserve(&output)
        })
        .err()
        .unwrap();
        assert!(matches!(error, CompileError::Io(_)));
        assert!(!root.path().join("entry.preparing").exists());
        EntryPreparation::reserve(&output)
            .unwrap()
            .release_if_unsubmitted()
            .unwrap();
        assert!(!root.path().join("entry.preparing").exists());
    }

    /// The counted runner's owned-resident mode owns one real daemon worker.
    /// Its foreground reservation must reject Preparation before any request.
    #[test]
    #[serial_test::serial]
    fn retained_entry_known_preparation_refusal_allows_same_original_foreground() {
        use tidepool_extract_cmd::{extract_spawn_count, CompilerTransactionClose};
        assert!(
            std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV).is_some(),
            "this admission control requires --compiler-mode owned-resident"
        );
        let root = tempfile::tempdir().unwrap();
        let authored = root.path().join("sources");
        std::fs::create_dir(&authored).unwrap();
        let source = authored.join("Input.hs");
        std::fs::write(&source, "module Input where\n__prepared = (37 :: Int)\n").unwrap();
        let sources = FrozenEntrySources::capture(&[authored], &source).unwrap();
        let output = root.path().join("entry");
        let before = extract_spawn_count();
        let refusal = with_compiler_transaction_for_workload(
            CompileWorkload::Preparation,
            |_| {},
            || {
                crate::artifacts::test_support::prepare_frozen_production_entry(
                    &sources,
                    root.path(),
                    &output,
                )
            },
        );
        assert!(matches!(refusal.action,
            Err(CompileError::Io(ref error)) if error.kind() == std::io::ErrorKind::WouldBlock));
        assert_eq!(refusal.close, CompilerTransactionClose::NotStarted);
        assert_eq!(extract_spawn_count(), before);
        assert!(!output.exists());
        assert!(!root.path().join("entry.preparing").exists());
        let foreground = with_compiler_transaction_for_workload(
            CompileWorkload::Foreground,
            |_| {},
            || {
                crate::artifacts::test_support::prepare_frozen_production_entry(
                    &sources,
                    root.path(),
                    &output,
                )
            },
        );
        foreground.action.unwrap();
        assert_eq!(foreground.close, CompilerTransactionClose::Clean);
        assert_eq!(extract_spawn_count(), before + 1);
        assert!(output.join("raw/certified-products.cbor").is_file());
        assert!(!root.path().join("entry.preparing").exists());
    }

    #[test]
    #[serial_test::serial]
    #[cfg(unix)]
    fn production_entry_dangling_output_refuses_reservation_before_compilation() {
        let root = tempfile::tempdir().unwrap();
        let authored = root.path().join("sources");
        std::fs::create_dir(&authored).unwrap();
        let source = authored.join("Input.hs");
        std::fs::write(&source, "module Input where\n__prepared = (1 :: Int)\n").unwrap();
        let sources = FrozenEntrySources::capture(&[authored], &source).unwrap();
        let output = root.path().join("entry");
        let absent_target = root.path().join("absent-target");
        std::os::unix::fs::symlink(&absent_target, &output).unwrap();
        assert!(matches!(
            crate::artifacts::test_support::prepare_frozen_production_entry(
                &sources,
                root.path(),
                &output
            ),
            Err(CompileError::ExtractFailed(_))
        ));
        assert_eq!(std::fs::read_link(&output).unwrap(), absent_target);
        assert!(!root.path().join("entry.preparing").exists());
    }

    #[test]
    #[serial_test::serial]
    fn production_entry_reservation_sync_failure_refuses_retry_before_compilation() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Input.hs");
        std::fs::write(&source, "module Input where\n__prepared = (1 :: Int)\n").unwrap();
        let sources = FrozenEntrySources::capture(&[root.path().to_owned()], &source).unwrap();
        let output = root.path().join("entry");
        let error = production_entry::with_failure(
            production_entry::EntryCheckpoint::ReservationSync,
            || {
                crate::artifacts::test_support::prepare_frozen_production_entry(
                    &sources,
                    root.path(),
                    &output,
                )
            },
        )
        .unwrap_err();
        assert!(matches!(error, CompileError::Io(_)));
        assert!(!output.exists());
        let unfinished = root.path().join("entry.preparing");
        assert!(unfinished.is_dir());
        assert!(!unfinished.join("raw").exists());
        assert!(matches!(
            crate::artifacts::test_support::prepare_frozen_production_entry(&sources, root.path(), &output),
            Err(CompileError::EntryPreparationUnfinished { path }) if path == unfinished
        ));
        assert_eq!(std::fs::read_dir(unfinished).unwrap().count(), 0);
    }

    #[test]
    #[serial_test::serial]
    fn retained_entry_sealing_failures_preserve_original_quotations_without_reexecution() {
        use crate::toolchain::CompilerDeploymentConfiguration;
        use production_entry::EntryCheckpoint;
        let root = tempfile::tempdir().unwrap();
        let authored = root.path().join("sources");
        std::fs::create_dir(&authored).unwrap();
        let input = root.path().join("quote-input");
        let counter = root.path().join("quote-input.executions");
        std::fs::write(&input, "37").unwrap();
        std::fs::write(
            authored.join("QuotedProvider.hs"),
            include_str!("../tests/fixtures/completed-source/QuotedProvider.hs"),
        )
        .unwrap();
        std::fs::write(
            authored.join("QuotedOriginal.hs"),
            include_str!("../tests/fixtures/completed-source/QuotedOriginal.hs")
                .replace("QUOTE_INPUT_PATH", input.to_str().unwrap()),
        )
        .unwrap();
        let source = authored.join("PreparedOriginal.hs");
        std::fs::write(
            &source,
            include_str!("../tests/fixtures/completed-source/PreparedOriginal.hs"),
        )
        .unwrap();
        let sources = FrozenEntrySources::capture(&[authored], &source).unwrap();
        let selected = ProductionEntrySources::FrozenWorkspace(sources.clone());
        let CompilerDeploymentConfiguration::Configured(authority) =
            CompilerDeploymentConfiguration::from_env().unwrap()
        else {
            panic!("entry failure qualification requires configured matched compiler authority")
        };
        let mut previous_executions = String::new();
        for (index, stage) in [
            EntryCheckpoint::ManifestWrite,
            EntryCheckpoint::TreeSync,
            EntryCheckpoint::ReadyRename,
            EntryCheckpoint::PublicationSync,
            EntryCheckpoint::Handoff,
        ]
        .into_iter()
        .enumerate()
        {
            let output = root.path().join(format!("entry-{index}"));
            let unfinished = root.path().join(format!("entry-{index}.preparing"));
            let error = production_entry::with_failure(stage, || {
                crate::artifacts::test_support::prepare_frozen_production_entry(
                    &sources,
                    root.path(),
                    &output,
                )
            })
            .unwrap_err();
            let executions = std::fs::read_to_string(&counter).unwrap();
            assert!(
                executions.len() > previous_executions.len(),
                "an explicit fresh preparation executes its original quotation"
            );
            let published = matches!(
                stage,
                EntryCheckpoint::PublicationSync | EntryCheckpoint::Handoff
            );
            let raw = if published {
                output.join("raw")
            } else {
                unfinished.join("raw")
            };
            let original = std::fs::read(raw.join("module-products.cbor")).unwrap();
            assert!(!original.is_empty());
            if published {
                if stage == EntryCheckpoint::PublicationSync {
                    assert!(
                        matches!(error, CompileError::EntryPublicationUnconfirmed { ref path, .. } if path == &output)
                    );
                } else {
                    assert!(matches!(error, CompileError::Io(_)));
                }
                assert!(!unfinished.exists());
                let entry = load_selected_production_entry(&output, &authority, &selected).unwrap();
                assert!(entry.products().original_compile_input.is_some());
                tidepool_atomic_write::sync_parent_directory(&output).unwrap();
            } else {
                assert!(matches!(error, CompileError::Io(_)));
                assert!(!output.exists());
                assert!(matches!(
                    crate::artifacts::test_support::prepare_frozen_production_entry(&sources, root.path(), &output),
                    Err(CompileError::EntryPreparationUnfinished { ref path }) if path == &unfinished
                ));
            }
            assert!(raw.join("certified-products.cbor").is_file());
            assert!(raw.join("dependencies.json").is_file());
            for diagnostic in ["compiler.stdout", "compiler.stderr", "compiler-status.json"] {
                assert!(raw.join(diagnostic).is_file());
            }
            assert_eq!(
                std::fs::read_to_string(&counter).unwrap(),
                executions,
                "sealing failure, refusal and completed loading cannot invoke the compiler again"
            );
            assert_eq!(
                std::fs::read(raw.join("module-products.cbor")).unwrap(),
                original
            );
            previous_executions = executions;
        }
    }

    #[test]
    #[serial_test::serial]
    fn retained_entry_final_source_drift_refuses_handoff_without_reexecution() {
        use crate::toolchain::CompilerDeploymentConfiguration;
        use production_entry::EntryCheckpoint;
        let root = tempfile::tempdir().unwrap();
        let authored = root.path().join("sources");
        std::fs::create_dir(&authored).unwrap();
        let source = authored.join("Input.hs");
        let original = "module Input where\n__prepared = (37 :: Int)\n";
        std::fs::write(&source, original).unwrap();
        let sources = FrozenEntrySources::capture(&[authored], &source).unwrap();
        let output = root.path().join("entry");
        let changed_source = source.clone();
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reached = Arc::clone(&observed);
        let error = production_entry::with_checkpoint_observer(
            move |stage| {
                if stage == EntryCheckpoint::SourceRevalidation {
                    reached.store(true, std::sync::atomic::Ordering::SeqCst);
                    std::fs::write(
                        &changed_source,
                        "module Input where\n__prepared = (91 :: Int)\n",
                    )
                    .unwrap();
                }
            },
            || {
                crate::artifacts::test_support::prepare_frozen_production_entry(
                    &sources,
                    root.path(),
                    &output,
                )
            },
        )
        .unwrap_err();
        assert!(
            observed.load(std::sync::atomic::Ordering::SeqCst),
            "drift occurs after entry publication, before returning validated custody"
        );
        assert!(matches!(error, CompileError::ExtractFailed(_)));
        assert!(output.join("raw/certified-products.cbor").is_file());
        let CompilerDeploymentConfiguration::Configured(authority) =
            CompilerDeploymentConfiguration::from_env().unwrap()
        else {
            panic!("entry drift test requires matched compiler authority")
        };
        let selected = ProductionEntrySources::FrozenWorkspace(sources);
        assert!(load_selected_production_entry(&output, &authority, &selected).is_err());
        let spawns = tidepool_extract_cmd::extract_spawn_count();
        std::fs::write(source, original).unwrap();
        let recovered = load_selected_production_entry(&output, &authority, &selected).unwrap();
        assert_eq!(recovered.source(), original);
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), spawns);
    }

    #[test]
    #[serial_test::serial]
    fn retained_entry_late_cancellation_refuses_handoff_and_preserves_completed_original() {
        use crate::toolchain::CompilerDeploymentConfiguration;
        use production_entry::EntryCheckpoint;
        use tidepool_extract_cmd::{
            with_compiler_transaction_cancellable, CompilerTransactionCancellation,
        };
        let root = tempfile::tempdir().unwrap();
        let authored = root.path().join("sources");
        std::fs::create_dir(&authored).unwrap();
        let source = authored.join("Input.hs");
        std::fs::write(&source, "module Input where\n__prepared = (37 :: Int)\n").unwrap();
        let sources = FrozenEntrySources::capture(&[authored], &source).unwrap();
        let selected = ProductionEntrySources::FrozenWorkspace(sources.clone());
        let CompilerDeploymentConfiguration::Configured(authority) =
            CompilerDeploymentConfiguration::from_env().unwrap()
        else {
            panic!("entry cancellation test requires matched compiler authority")
        };
        for (index, stage) in [
            EntryCheckpoint::TreeSync,
            EntryCheckpoint::PublicationSync,
            EntryCheckpoint::Handoff,
        ]
        .into_iter()
        .enumerate()
        {
            let output = root.path().join(format!("entry-{index}"));
            let cancellation = CompilerTransactionCancellation::new();
            let stop = cancellation.clone();
            let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let reached = Arc::clone(&observed);
            let loads = production_entry::entry_load_count();
            let outcome = production_entry::with_checkpoint_observer(
                move |actual| {
                    if actual == stage {
                        reached.store(true, std::sync::atomic::Ordering::SeqCst);
                        stop.cancel();
                    }
                },
                || {
                    with_compiler_transaction_cancellable(
                        cancellation,
                        |_| {},
                        || {
                            crate::artifacts::test_support::prepare_frozen_production_entry(
                                &sources,
                                root.path(),
                                &output,
                            )
                        },
                    )
                },
            );
            assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(
                production_entry::entry_load_count() - loads,
                1,
                "cancellation follows complete staging validation"
            );
            let error = outcome.action.unwrap_err();
            if stage == EntryCheckpoint::PublicationSync {
                assert!(matches!(
                    error,
                    CompileError::EntryPublicationUnconfirmed { .. }
                ));
            } else {
                assert!(matches!(error, CompileError::Io(ref error)
                    if error.kind() == std::io::ErrorKind::Interrupted));
            }
            let raw = if stage == EntryCheckpoint::TreeSync {
                assert!(!output.exists());
                let unfinished = root.path().join(format!("entry-{index}.preparing"));
                assert!(matches!(
                    crate::artifacts::test_support::prepare_frozen_production_entry(
                        &sources,
                        root.path(),
                        &output
                    ),
                    Err(CompileError::EntryPreparationUnfinished { .. })
                ));
                unfinished.join("raw")
            } else {
                assert!(output.exists());
                output.join("raw")
            };
            let bytes = std::fs::read(raw.join("module-products.cbor")).unwrap();
            let spawns = tidepool_extract_cmd::extract_spawn_count();
            let recovered = if stage == EntryCheckpoint::TreeSync {
                raw.parent().unwrap()
            } else {
                output.as_path()
            };
            let entry = load_selected_production_entry(recovered, &authority, &selected).unwrap();
            assert!(entry.products().original_compile_input.is_some());
            assert_eq!(tidepool_extract_cmd::extract_spawn_count(), spawns);
            assert_eq!(
                std::fs::read(raw.join("module-products.cbor")).unwrap(),
                bytes
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn retained_complete_entries_load_original_quotations_without_compiler_or_source_replay() {
        use crate::toolchain::CompilerDeploymentConfiguration;
        let root = tempfile::tempdir().unwrap();
        let authored = root.path().join("sources");
        std::fs::create_dir(&authored).unwrap();
        let input = root.path().join("quote-input");
        let counter = root.path().join("quote-input.executions");
        std::fs::write(&input, "37").unwrap();
        std::fs::write(
            authored.join("QuotedProvider.hs"),
            include_str!("../tests/fixtures/completed-source/QuotedProvider.hs"),
        )
        .unwrap();
        std::fs::write(
            authored.join("QuotedOriginal.hs"),
            include_str!("../tests/fixtures/completed-source/QuotedOriginal.hs")
                .replace("QUOTE_INPUT_PATH", input.to_str().unwrap()),
        )
        .unwrap();
        let source = authored.join("PreparedOriginal.hs");
        std::fs::write(
            &source,
            include_str!("../tests/fixtures/completed-source/PreparedOriginal.hs"),
        )
        .unwrap();
        let sources = FrozenEntrySources::capture(&[authored.clone()], &source).unwrap();
        let output = root.path().join("entry");
        let before = production_entry::entry_load_count();
        let first = crate::artifacts::test_support::prepare_frozen_production_entry(
            &sources,
            root.path(),
            &output,
        )
        .unwrap();
        assert_eq!(
            production_entry::entry_load_count() - before,
            1,
            "fresh preparation hands off its staging validation without loading again"
        );
        let CompilerDeploymentConfiguration::Configured(authority) =
            CompilerDeploymentConfiguration::from_env().unwrap()
        else {
            panic!("native entry qualification requires configured matched compiler authority")
        };
        let reopened = load_selected_production_entry(
            &output,
            &authority,
            &ProductionEntrySources::FrozenWorkspace(sources.clone()),
        )
        .unwrap();
        assert_eq!(first.target_owned(), reopened.target_owned());
        assert_eq!(first.source(), reopened.source());
        assert_eq!(
            first.products().artifact_view.descriptors(),
            reopened.products().artifact_view.descriptors()
        );
        assert_eq!(
            first
                .products()
                .original_compile_input
                .as_ref()
                .unwrap()
                .original_input_identity(),
            reopened
                .products()
                .original_compile_input
                .as_ref()
                .unwrap()
                .original_input_identity()
        );
        let proof = first.products().original_compile_input.as_ref().unwrap();
        assert!(proof.replay_eligible_identity().is_none());
        let compiled_executions = std::fs::read_to_string(&counter).unwrap();
        let _restore = RestoreEnvironment(
            ["TIDEPOOL_EXTRACT", tidepool_extract_cmd::DAEMON_SOCKET_ENV]
                .into_iter()
                .map(|name| (name, std::env::var_os(name)))
                .collect(),
        );
        unsafe {
            std::env::set_var("TIDEPOOL_EXTRACT", root.path().join("unavailable-compiler"));
            std::env::remove_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV);
        }
        std::fs::write(&input, "91").unwrap();
        let selected = ProductionEntrySources::FrozenWorkspace(sources.clone());
        let retained = load_selected_production_entry(&output, &authority, &selected).unwrap();
        assert_eq!(first.target_owned(), retained.target_owned());
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap(),
            compiled_executions,
            "loading completed original custody cannot execute untracked source inputs"
        );
        let moved = root.path().join("moved-entry");
        std::fs::rename(&output, &moved).unwrap();
        assert!(
            load_selected_production_entry(&moved, &authority, &selected).is_ok(),
            "complete output container can move while original snapshot paths remain retained"
        );
        let manifest_path = moved.join("entry.json");
        let original_manifest = std::fs::read(&manifest_path).unwrap();
        let mut obsolete: serde_json::Value = serde_json::from_slice(&original_manifest).unwrap();
        obsolete["schema"] = 1.into();
        std::fs::write(&manifest_path, serde_json::to_vec(&obsolete).unwrap()).unwrap();
        assert!(load_selected_production_entry(&moved, &authority, &selected).is_err());
        for field in ["producer", "worker"] {
            let mut mismatched: serde_json::Value =
                serde_json::from_slice(&original_manifest).unwrap();
            let original_byte = mismatched[field][0].as_u64().unwrap();
            mismatched[field][0] = ((original_byte + 1) % 256).into();
            std::fs::write(&manifest_path, serde_json::to_vec(&mismatched).unwrap()).unwrap();
            assert!(load_selected_production_entry(&moved, &authority, &selected).is_err());
        }
        std::fs::write(manifest_path, original_manifest).unwrap();
        let metadata = moved.join("raw/meta.cbor");
        let original = std::fs::read(&metadata).unwrap();
        std::fs::write(&metadata, b"changed").unwrap();
        assert!(load_selected_production_entry(&moved, &authority, &selected).is_err());
        std::fs::write(metadata, original).unwrap();
        let native_products = moved.join("raw/module-products.cbor");
        let original = std::fs::read(&native_products).unwrap();
        std::fs::remove_file(&native_products).unwrap();
        assert!(
            load_selected_production_entry(&moved, &authority, &selected).is_err(),
            "missing original native products cannot produce a ready entry"
        );
        std::fs::write(native_products, original).unwrap();
        std::fs::write(
            &source,
            "module PreparedOriginal where\n__prepared = (92 :: Int)\n",
        )
        .unwrap();
        assert!(
            load_selected_production_entry(&moved, &authority, &selected).is_err(),
            "mutable original wrapper changes cannot become consumed snapshot authority"
        );
    }

    #[test]
    #[serial_test::serial]
    fn completed_quotations_admit_originals_without_source_replay() {
        use crate::declaration_join::{ExactDeclarationContext, ExactModuleIdentity};
        use tidepool_repr::execution_schema::{Atom, Group, HeapRhs, ScalarLiteral};

        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let _restore = RestoreEnvironment(
            [
                "TIDEPOOL_COMPILE_CACHE_DIR",
                tidepool_extract_cmd::DAEMON_SOCKET_ENV,
            ]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect(),
        );
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
            std::env::remove_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV);
        }
        let provider = root.path().join("QuotedProvider.hs");
        let original_path = root.path().join("QuotedOriginal.hs");
        let input = root.path().join("quote-input");
        let counter = root.path().join("quote-input.executions");
        std::fs::write(
            &provider,
            include_str!("../tests/fixtures/completed-source/QuotedProvider.hs"),
        )
        .unwrap();
        std::fs::write(
            &original_path,
            include_str!("../tests/fixtures/completed-source/QuotedOriginal.hs")
                .replace("QUOTE_INPUT_PATH", input.to_str().unwrap()),
        )
        .unwrap();
        let source = include_str!("../tests/fixtures/completed-source/QuotedConsumer.hs");
        let earlier = root.path().join("earlier");
        std::fs::create_dir(&earlier).unwrap();
        let include = [earlier.clone(), root.path().to_owned()];
        let invocation = CompileInvocation {
            source,
            targets: &["result"],
            include: &include,
            fallback_module_name: "QuotedConsumer",
        };
        let quoted_owner = ExactModuleIdentity {
            unit: "main".into(),
            module: "QuotedOriginal".into(),
        };
        let context_owner = ExactModuleIdentity {
            unit: "main".into(),
            module: "QuotedContextProvider".into(),
        };
        let context_products = crate::artifacts::test_support::compile_invocation(
            &CompileInvocation {
                source:
                    "module QuotedContextProvider where\ncontextValue :: Int\ncontextValue = 0\n",
                targets: &["contextValue"],
                include: &include,
                fallback_module_name: "QuotedContextProvider",
            },
            |_, _, _| {},
        )
        .expect("ordinary compiler producer issues the exact context's interface");
        let context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_interface_artifacts(
                &context_products
                    .artifact_view
                    .interface_projection(&[context_owner])
                    .unwrap(),
            )
            .unwrap();
        assert_ne!(context.toolchain_identity_sha256(), [0; 32]);
        let assert_quoted_value = |compiled: &CompiledArtifacts, value: i64| {
            let values = compiled
                .certified_groups
                .iter()
                .filter(|group| {
                    group.owner().unit == "main" && group.owner().module == "QuotedOriginal"
                })
                .flat_map(|group| group.group().definitions().bindings().iter())
                .flat_map(|binding| match binding {
                    Group::NonRecursive(binding) => std::slice::from_ref(binding),
                    Group::Recursive(bindings) => bindings.as_slice(),
                })
                .filter(|binding| {
                    binding.identity.module == "QuotedOriginal"
                        && binding.identity.occurrence == "value"
                })
                .collect::<Vec<_>>();
            let [binding] = values.as_slice() else {
                panic!("one certified original quoted value")
            };
            let HeapRhs::Constructor { fields, .. } = &binding.binding.rhs else {
                panic!("quoted value must be an actual boxed Int constant")
            };
            assert!(
                matches!(fields.as_slice(), [Atom::Scalar(ScalarLiteral::Int { bits: 64, bytes })]
                if bytes.as_slice() == value.to_be_bytes()),
                "quoted boxed Int must retain {value} as a big-endian Int64 scalar: {fields:?}"
            );
        };
        let mut observed = String::new();
        let mut retained = None;
        for (exact, value) in [(false, 37_i64), (false, 91), (true, 37)] {
            std::fs::write(&input, value.to_string()).unwrap();
            let mut completed_requests = 0;
            let mut on_stage = |stage: &str, _, _| {
                if stage == timing::STAGE_EXTRACT_SPAWN {
                    completed_requests += 1;
                }
            };
            let compiled = if exact {
                crate::artifacts::test_support::compile_invocation_in_context(
                    &invocation,
                    Arc::new(context.clone()),
                    &mut on_stage,
                )
            } else {
                crate::artifacts::test_support::compile_invocation(&invocation, &mut on_stage)
            }
            .expect("completed quoted source must retain genuine canonical/native originals");
            assert_eq!(completed_requests, 1, "each explicit request compiles once");
            observed.push_str(&format!("{value}\n"));
            assert_eq!(std::fs::read_to_string(&counter).unwrap(), observed);
            assert!(!compiled.recovery_products.is_empty());
            assert!(compiled
                .recovery_products
                .iter()
                .all(|product| product.execution_source().is_none()));
            assert!(compiled
                .certified_groups
                .iter()
                .all(|group| group.origin() == ProductOrigin::Fresh));
            let quoted = compiled
                .module_products
                .iter()
                .find(|product| product.unit == "main" && product.module == "QuotedOriginal")
                .expect("actual native quoted owner");
            assert!(!quoted.groups.is_empty());
            assert_quoted_value(&compiled, value);
            let selected = module_candidates::select_configured(
                compiled.producer_identity.as_ref().unwrap(),
                &include,
                &root.path().join(format!("offer-{exact}-{value}")),
            )
            .unwrap();
            assert!(selected.is_none_or(|selected| {
                !selected
                    .by_owner
                    .contains_key(&(quoted_owner.unit.clone(), quoted_owner.module.clone()))
            }));
            if exact {
                let admission = compiled.exact_source_admission.as_ref().unwrap();
                assert!(!admission.evidence.cache_safe && !admission.evidence.selection_complete);
                assert!(admission.evidence.revalidate(source).is_ok());
                let request = Arc::new(context.clone())
                    .prepare_compilation(
                        &root.path().join("original-execution-scope"),
                        compiled.producer_identity.as_ref().unwrap(),
                    )
                    .unwrap()
                    .with_source_search_context(&include);
                let original = crate::declaration_context::ExactProductAdmission {
                    request: &request,
                    source: admission,
                }
                .original_execution_fixture(&compiled.artifact_view)
                .unwrap();
                let template = ["module Protected where\nimport QuotedOriginal\n".to_owned()];
                assert!(
                    crate::declaration_context::RetainedTemplateImports::capture(
                        &original, &template,
                    )
                    .unwrap()
                    .is_some(),
                    "the real completed original issues template custody"
                );
                let missing = ExactModuleIdentity {
                    unit: "main".into(),
                    module: "MissingOriginalInstanceOwner".into(),
                };
                let mut incomplete_graph = original.lexical_graph().to_vec();
                incomplete_graph
                    .iter_mut()
                    .find(|node| node.owner == quoted_owner)
                    .unwrap()
                    .imports
                    .push(missing.clone());
                let incomplete = ExactDeclarationContext::from_authenticated_execution(
                    original.toolchain_identity_sha256(),
                    original.artifact_view(),
                    incomplete_graph,
                    original.original_instance_target().unwrap().clone(),
                    std::slice::from_ref(&missing),
                )
                .unwrap();
                assert!(matches!(incomplete.original_instance_environment(),
                    crate::declaration_context::OriginalInstanceEnvironment::MissingOriginalOwners(owners)
                    if owners.contains(&missing)));
                assert!(
                    incomplete
                        .lexical_graph()
                        .iter()
                        .all(|node| !node.imports.contains(&missing)),
                    "the negative exercises the owning constructor's pruned graph"
                );
                assert!(
                    crate::declaration_context::RetainedTemplateImports::capture(
                        &incomplete,
                        &template,
                    )
                    .is_err(),
                    "a pruned original graph cannot issue template custody"
                );
                // The native test owns this completed quotation output; it
                // does not issue a replay recipe or a hermetic build artifact.
                let consumer = root.path().join("QuotedConsumer.hs");
                std::fs::write(&consumer, source).unwrap();
                let mut observations = (*admission.evidence).clone().into_evidence();
                for consumed in &mut observations.sources {
                    if consumed.path == Path::new(cache::GENERATED_SOURCE) {
                        consumed.path = consumer.clone();
                    }
                }
                for module in &mut observations.modules {
                    if module.source == Path::new(cache::GENERATED_SOURCE) {
                        module.source = consumer.clone();
                    }
                }
                let observations = serde_json::to_vec(&observations).unwrap();
                validate_completed_corpus_sources(&observations, &consumer, &include).unwrap();
                assert!(
                    validate_prepared_fixture_sources(&observations, &consumer, &include).is_err(),
                    "completed TH output cannot become an immutable build recipe"
                );
                assert!(
                    validate_completed_corpus_sources(
                        &observations,
                        &consumer,
                        std::slice::from_ref(&earlier),
                    )
                    .is_err(),
                    "an observed provider outside the declared trees is refused"
                );
                let provider_bytes = std::fs::read(&provider).unwrap();
                let mut changed = provider_bytes.clone();
                changed.extend_from_slice(b"\n-- changed after compilation\n");
                std::fs::write(&provider, changed).unwrap();
                assert!(
                    validate_completed_corpus_sources(&observations, &consumer, &include).is_err(),
                    "completed output still validates consumed source bytes"
                );
                std::fs::write(&provider, provider_bytes).unwrap();
                let shadow = earlier.join("QuotedOriginal.hs");
                std::fs::write(&shadow, "module QuotedOriginal where\nvalue = 0\n").unwrap();
                assert!(
                    validate_completed_corpus_sources(&observations, &consumer, &include).is_err(),
                    "completed output still validates negative import witnesses"
                );
                std::fs::remove_file(shadow).unwrap();
                validate_completed_corpus_sources(&observations, &consumer, &include).unwrap();
                let surface = crate::declaration_join::source_lexical_closure(
                    std::slice::from_ref(&quoted_owner),
                    &admission.home_imports().unwrap(),
                    &[],
                    &compiled.artifact_view.source_implementation_roles(),
                )
                .unwrap();
                let owners = surface
                    .lexical
                    .iter()
                    .map(|node| node.owner.clone())
                    .collect::<Vec<_>>();
                let native = compiled
                    .recovery_products
                    .iter()
                    .filter(|product| {
                        product.owner().unit == quoted_owner.unit
                            && product.owner().module == quoted_owner.module
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                assert_eq!(native.len(), 1);
                let owner = native[0].owner().clone();
                let producer =
                    crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                        compiled.producer_identity.as_ref().unwrap(),
                    )
                    .sha256();
                let context = ExactDeclarationContext::new(&[], &[], vec![])
                    .unwrap()
                    .extend_interface_artifacts(
                        &compiled
                            .artifact_view
                            .interface_projection(&owners)
                            .unwrap(),
                    )
                    .unwrap()
                    .extend_checked_original_products(producer, &native)
                    .unwrap()
                    .extend(&[], &[], surface.lexical)
                    .unwrap();
                retained = Some((Arc::new(context), owner));
            }
        }
        std::fs::remove_file(&original_path).unwrap();
        std::fs::remove_file(&provider).unwrap();
        std::fs::write(&input, "99").unwrap();
        let (context, original_owner) = retained.unwrap();
        let consumed = crate::artifacts::test_support::compile_invocation_in_context(
            &invocation,
            context,
            |_, _, _| {},
        )
        .expect("source-less original use must not replay its quotation");
        assert_eq!(std::fs::read_to_string(&counter).unwrap(), observed);
        assert_quoted_value(&consumed, 37);
        assert!(consumed.targets["result"]
            .pending_imports
            .iter()
            .any(|import| {
                matches!(import, certified_products::PendingImportOwner::Source { owner, .. }
                if owner == &original_owner)
            }));
    }

    #[test]
    #[ignore = "requires matched Haskell worker and frontend"]
    #[serial_test::serial]
    fn real_worker_retained_core_promotion_joins_across_unrelated_context_extension() {
        use crate::artifact_inventory::{ArtifactKind, ArtifactView};
        use crate::declaration_join::{
            ExactDeclarationContext, ExactLexicalNode, ExactModuleIdentity,
        };

        let work = tempfile::tempdir().unwrap();
        let include = [work.path().to_path_buf()];
        let owner = ExactModuleIdentity {
            unit: "main".into(),
            module: "RetainedPromotionOwner".into(),
        };
        let support = ExactModuleIdentity {
            unit: "main".into(),
            module: "RetainedPromotionSupport".into(),
        };
        let unrelated = ExactModuleIdentity {
            unit: "main".into(),
            module: "RetainedPromotionUnrelated".into(),
        };
        let owner_source = work.path().join("RetainedPromotionOwner.hs");
        let support_source = work.path().join("RetainedPromotionSupport.hs");
        let unrelated_source = work.path().join("RetainedPromotionUnrelated.hs");
        let owner_bytes =
            include_str!("../tests/fixtures/retained-promotion/RetainedPromotionOwner.hs");
        let support_bytes =
            include_str!("../tests/fixtures/retained-promotion/RetainedPromotionSupport.hs");
        let unrelated_bytes =
            include_str!("../tests/fixtures/retained-promotion/RetainedPromotionUnrelated.hs");
        let probe_bytes =
            include_str!("../tests/fixtures/retained-promotion/RetainedPromotionProbe.hs");
        let consumer_bytes =
            include_str!("../tests/fixtures/retained-promotion/RetainedPromotionConsumer.hs");
        let issue = |support_bytes: &str| {
            std::fs::write(&owner_source, owner_bytes).unwrap();
            std::fs::write(&support_source, support_bytes).unwrap();
            std::fs::write(&unrelated_source, unrelated_bytes).unwrap();
            let original = crate::artifacts::test_support::compile_invocation(
                &CompileInvocation {
                    source: probe_bytes,
                    targets: &["result"],
                    include: &include,
                    fallback_module_name: "RetainedPromotionProbe",
                },
                |_, _, _| {},
            )
            .expect("genuine source-only original issuance");
            let modules = original
                .module_inventory
                .as_ref()
                .expect("actual original source graph");
            let lexical = [&owner, &support, &unrelated]
                .into_iter()
                .map(|selected| {
                    let module = modules
                        .iter()
                        .find(|module| {
                            !module.boot
                                && module.unit == selected.unit
                                && module.module == selected.module
                        })
                        .expect("issued original owner");
                    assert_eq!(module.product, cache::ProductAvailability::InterfaceOnly);
                    let imports = module
                        .imports
                        .iter()
                        .filter_map(|import| {
                            let path = import.selected.as_ref()?;
                            let imported = modules
                                .iter()
                                .find(|module| {
                                    module.source == *path
                                        && module.module == import.module
                                        && module.boot == import.boot
                                })
                                .expect("original selected home import");
                            Some(ExactModuleIdentity {
                                unit: imported.unit.clone(),
                                module: imported.module.clone(),
                            })
                        })
                        .collect::<Vec<_>>();
                    ExactLexicalNode {
                        owner: selected.clone(),
                        imports,
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(lexical[0].imports, vec![support.clone()]);
            assert!(lexical[1].imports.is_empty() && lexical[2].imports.is_empty());
            assert!(original.certified_groups.iter().all(|group| {
                ![&owner, &support, &unrelated].iter().any(|selected| {
                    group.owner().unit == selected.unit && group.owner().module == selected.module
                })
            }));
            let interfaces = original
                .artifact_view
                .interface_projection(&[owner.clone(), support.clone()])
                .unwrap();
            let context = ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_interface_artifacts(&interfaces)
                .unwrap()
                .extend(&[], &[], lexical[..2].to_vec())
                .unwrap();
            let extended = context
                .clone()
                .extend_interface_artifacts(
                    &original
                        .artifact_view
                        .interface_projection(&[unrelated.clone()])
                        .unwrap(),
                )
                .unwrap()
                .extend(&[], &[], lexical)
                .unwrap();
            assert!(
                context.recovery_products().is_empty() && extended.recovery_products().is_empty()
            );
            assert_eq!(context.lexical_graph().len(), 2);
            assert_eq!(extended.lexical_graph().len(), 3);
            for path in [&owner_source, &support_source, &unrelated_source] {
                std::fs::remove_file(path).unwrap();
            }
            (context, extended)
        };
        let consume = |context: ExactDeclarationContext| {
            crate::artifacts::test_support::compile_invocation_in_context(
                &CompileInvocation {
                    source: consumer_bytes,
                    targets: &["result"],
                    include: &include,
                    fallback_module_name: "RetainedPromotionConsumer",
                },
                Arc::new(context),
                |_, _, _| {},
            )
            .expect("production promotion after original sources are absent")
        };
        let native_root = |compiled: &CompiledArtifacts| -> ArtifactView {
            let roots = compiled
                .artifact_view
                .descriptors()
                .into_iter()
                .filter(|descriptor| {
                    descriptor.kind == ArtifactKind::OriginalModule && descriptor.owner == owner
                })
                .map(|descriptor| descriptor.id)
                .collect::<Vec<_>>();
            assert_eq!(roots.len(), 1);
            compiled.artifact_view.select_roots(roots).unwrap()
        };
        let promoted_owner = |compiled: &CompiledArtifacts| {
            let product = compiled
                .recovery_products
                .iter()
                .find(|product| {
                    product.owner().unit == owner.unit && product.owner().module == owner.module
                })
                .expect("promoted original root");
            let groups = compiled
                .certified_groups
                .iter()
                .filter(|group| group.owner() == product.owner())
                .collect::<Vec<_>>();
            assert!(!groups.is_empty());
            assert!(groups
                .iter()
                .all(|group| group.origin() == ProductOrigin::RetainedCore));
            product.owner().clone()
        };
        let (context, extended) = issue(support_bytes);
        let first = consume(context);
        let second = consume(extended);
        assert_eq!(promoted_owner(&first), promoted_owner(&second));
        let first_native = native_root(&first);
        let second_native = native_root(&second);
        assert_eq!(first_native.descriptors(), second_native.descriptors());
        let joined = first_native
            .merge(&second_native)
            .expect("same original native owners join across unrelated context extension");
        assert_eq!(joined.descriptors(), first_native.descriptors());

        // Reissue unchanged owner source against a genuinely changed demanded
        // dependency. The original canonical import seal may change too; this
        // control does not claim to vary a dependency under one fixed seal.
        let (changed_context, _) = issue(include_str!(
            "../tests/fixtures/retained-promotion/RetainedPromotionSupportChanged.hs"
        ));
        let changed = consume(changed_context);
        let changed_owner = promoted_owner(&changed);
        assert_ne!(
            promoted_owner(&first).module_version,
            changed_owner.module_version
        );
        assert!(first_native.merge(&native_root(&changed)).is_err());
    }

    #[test]
    #[ignore = "requires matched Haskell worker and frontend"]
    #[serial_test::serial]
    fn real_worker_reuses_dependency_after_consumer_changes() {
        let cache = tempfile::tempdir().unwrap();
        let old_cache = std::env::var_os("TIDEPOOL_COMPILE_CACHE_DIR");
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        }
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bridge/haskell/test-prepared-stg");
        let work = tempfile::tempdir().unwrap();
        let root = work.path().to_path_buf();
        std::fs::copy(
            fixtures.join("ModuleProductA.hs"),
            root.join("ModuleProductA.hs"),
        )
        .unwrap();
        let first = std::fs::read_to_string(fixtures.join("ModuleProductB.hs")).unwrap();
        let first_artifacts = crate::artifacts::test_support::compile_targets(
            &first,
            &["consume"],
            &[root.clone()],
            |_, _, _| {},
        )
        .unwrap();
        assert!(!first_artifacts.certified_groups.is_empty());
        assert_eq!(
            first_artifacts.targets["consume"].pending_imports.len(),
            first_artifacts.targets["consume"]
                .prepared
                .prepared()
                .globals()
                .len()
        );
        let changed = first.replace("produce + 1", "produce + 2");
        assert_ne!(changed, first);
        let second = crate::artifacts::test_support::compile_targets(
            &changed,
            &["consume"],
            &[root.clone()],
            |_, _, _| {},
        )
        .unwrap();
        assert!(second
            .module_products
            .iter()
            .any(|product| product.module == "ModuleProductA"));
        assert!(second
            .certified_groups
            .iter()
            .any(|group| group.origin() == ProductOrigin::Cached
                && group.owner().module == "ModuleProductA"));
        assert_eq!(
            second.targets["consume"].pending_imports.len(),
            second.targets["consume"]
                .prepared
                .prepared()
                .globals()
                .len()
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(root.join("ModuleProductA.hs"))
            .unwrap()
            .write_all(b"\n-- changed original dependency\n")
            .unwrap();
        let changed_again = first.replace("produce + 1", "produce + 3");
        let third = crate::artifacts::test_support::compile_targets(
            &changed_again,
            &["consume"],
            &[root],
            |_, _, _| {},
        )
        .unwrap();
        assert!(third
            .certified_groups
            .iter()
            .any(|group| group.origin() == ProductOrigin::Fresh
                && group.owner().module == "ModuleProductA"));
        assert!(!third
            .certified_groups
            .iter()
            .any(|group| group.origin() == ProductOrigin::Cached
                && group.owner().module == "ModuleProductA"));
        match old_cache {
            Some(path) => unsafe { std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", path) },
            None => unsafe { std::env::remove_var("TIDEPOOL_COMPILE_CACHE_DIR") },
        }
    }
}

#[cfg(test)]
mod constructor_identity_tests {
    use super::*;
    use tidepool_repr::execution_schema::ConstructorDecl;
    use tidepool_repr::serial::{write_metadata, MetaWarnings};
    use tidepool_repr::{DataCon, SrcBang};

    /// Typed constructor input for artifact assembly and custody checks.
    fn prepared_fixture_bytes() -> Vec<u8> {
        tidepool_test_data::prepared_encode::encode_wire_program(
            &tidepool_test_data::prepared::constructor_program(),
        )
    }

    #[test]
    fn immutable_native_turn_output_ignores_mutated_diagnostic_files() {
        let directory = tempfile::tempdir().unwrap();
        let metadata = write_metadata(&DataConTable::new(), &MetaWarnings::default()).unwrap();
        std::fs::write(
            directory.path().join("__prepared.prepared.cbor"),
            prepared_fixture_bytes(),
        )
        .unwrap();
        std::fs::write(directory.path().join("meta.cbor"), &metadata).unwrap();
        let turn: Arc<[u8]> = b"retained turn observation".as_slice().into();
        let output =
            read_native_turn_artifacts(directory.path(), turn.clone(), "module Input where".into())
                .unwrap();
        let (table, warnings) = read_metadata(&output.metadata).unwrap();
        let retained = NativeTurnOutput {
            output,
            table,
            warnings,
        };
        let target = retained.target_owned();
        std::fs::write(
            directory.path().join("__prepared.prepared.cbor"),
            b"invalid",
        )
        .unwrap();
        std::fs::write(directory.path().join("meta.cbor"), b"invalid").unwrap();
        drop(directory);
        assert!(Arc::ptr_eq(&target, &retained.target_owned()));
        assert_eq!(retained.turn_bytes(), turn.as_ref());
        assert_eq!(retained.metadata_bytes(), metadata.as_slice());
        assert_eq!(retained.source(), "module Input where");
        assert!(retained.products().is_none());
    }

    /// A table entry that agrees with `decl` on every fact this crate checks
    /// (id, occurrence name, field count) — what a table from the SAME
    /// compile as `decl`'s prepared program necessarily contains.
    fn matching_dc(decl: &ConstructorDecl) -> DataCon {
        DataCon {
            identity: decl.identity.clone(),
            id: decl.host_id,
            name: decl.identity.occurrence.clone(),
            tag: decl.tag,
            rep_arity: decl.field_reps.len() as u32,
            field_bangs: vec![SrcBang::NoSrcBang; decl.field_reps.len()],
            qualified_name: None,
            type_name: decl.family.occurrence.clone(),
        }
    }

    /// A table that agrees with EVERY constructor `artifact` declares, except
    /// `decl`'s entry, which is passed through `mutate` first. Anchors a
    /// single deliberate disagreement without leaving every OTHER
    /// constructor unresolved (which would fail for an unrelated reason: the
    /// check walks every declared constructor, not just the one under test).
    fn table_with_one_entry_mutated(
        artifact: &PreparedArtifact,
        decl: &ConstructorDecl,
        mutate: impl Fn(&mut DataCon),
    ) -> DataConTable {
        let mut table = DataConTable::new();
        for candidate in artifact.prepared().constructors() {
            let mut dc = matching_dc(candidate);
            if candidate.host_id == decl.host_id {
                mutate(&mut dc);
            }
            table.insert_checked(dc).expect("valid fixture metadata");
        }
        table
    }

    fn raw_target(target: &str, prepared_bytes: Vec<u8>) -> RawTargetOutput {
        RawTargetOutput {
            target: target.to_string(),
            asks_bytes: b"[]".to_vec(),
            prepared_bytes: Arc::new(prepared_bytes),
        }
    }

    /// Every paired constructor identity agrees in the valid structural control.
    #[test]
    fn correctly_paired_artifact_and_table_assembles() {
        let prepared_bytes = prepared_fixture_bytes();
        let artifact =
            PreparedArtifact::parse(prepared_bytes.clone(), DecodeLimits::default()).unwrap();
        assert!(
            !artifact.prepared().constructors().is_empty(),
            "fixture must declare at least one constructor for this test to mean anything"
        );

        let mut table = DataConTable::new();
        for decl in artifact.prepared().constructors() {
            table
                .insert_checked(matching_dc(decl))
                .expect("valid fixture metadata");
        }
        let meta_bytes = write_metadata(&table, &MetaWarnings::default()).unwrap();
        let raw = vec![raw_target("entry", prepared_bytes)];

        assemble(&meta_bytes, &raw, |_, _, _| {}).expect("correctly paired artifact must assemble");
    }

    #[test]
    fn removing_a_required_constructor_rejects_joint_admission() {
        let prepared_bytes = prepared_fixture_bytes();
        let artifact =
            PreparedArtifact::parse(prepared_bytes.clone(), DecodeLimits::default()).unwrap();
        let missing = artifact
            .prepared()
            .constructors()
            .first()
            .expect("fixture has constructors");
        let mut table = DataConTable::new();
        for declared in artifact.prepared().constructors() {
            if declared.host_id != missing.host_id {
                table.insert_checked(matching_dc(declared)).unwrap();
            }
        }
        let metadata = write_metadata(&table, &MetaWarnings::default()).unwrap();
        let raw = vec![raw_target("entry", prepared_bytes)];
        assert!(matches!(assemble(&metadata, &raw, |_,_,_| {}),
            Err(CompileError::ReadError(tidepool_repr::serial::ReadError::ConstructorMetadata(
                tidepool_repr::ConstructorMetadataMismatch::Missing { host_id, .. }))) if host_id == missing.host_id));
    }

    /// TEST 2 (cross-pairing rejects): the id resolves to the WRONG
    /// constructor — the dangerous case a totally-missing entry is not,
    /// since a handler would silently misinterpret fields under the wrong
    /// name rather than merely finding nothing. Must fail assembly with the
    /// typed error, before `CompiledArtifacts` (and therefore any handler)
    /// ever exists.
    #[test]
    fn cross_paired_table_wrong_name_at_same_id_rejects() {
        let prepared_bytes = prepared_fixture_bytes();
        let artifact =
            PreparedArtifact::parse(prepared_bytes.clone(), DecodeLimits::default()).unwrap();
        let decl = artifact
            .prepared()
            .constructors()
            .first()
            .expect("fixture declares at least one constructor")
            .clone();

        let table = table_with_one_entry_mutated(&artifact, &decl, |dc| {
            dc.name = format!("{}NotThis", dc.name);
            dc.identity.occurrence = dc.name.clone();
        });
        let meta_bytes = write_metadata(&table, &MetaWarnings::default()).unwrap();
        let raw = vec![raw_target("entry", prepared_bytes)];

        let err = match assemble(&meta_bytes, &raw, |_, _, _| {}) {
            Ok(_) => panic!("a same-id, different-name table entry must reject assembly"),
            Err(e) => e,
        };
        assert!(matches!(
            err,
            CompileError::ReadError(tidepool_repr::serial::ReadError::ConstructorMetadata(
                tidepool_repr::ConstructorMetadataMismatch::Identity { .. }
            ))
        ));
    }

    #[test]
    fn tag_disagreement_rejects_joint_admission() {
        let prepared_bytes = prepared_fixture_bytes();
        let artifact =
            PreparedArtifact::parse(prepared_bytes.clone(), DecodeLimits::default()).unwrap();
        let declared = artifact.prepared().constructors().first().unwrap();
        let table = table_with_one_entry_mutated(&artifact, declared, |dc| dc.tag += 1);
        let metadata = write_metadata(&table, &MetaWarnings::default()).unwrap();
        let raw = vec![raw_target("entry", prepared_bytes)];
        assert!(matches!(
            assemble(&metadata, &raw, |_, _, _| {}),
            Err(CompileError::ReadError(
                tidepool_repr::serial::ReadError::ConstructorMetadata(
                    tidepool_repr::ConstructorMetadataMismatch::Shape { .. }
                )
            ))
        ));
    }

    #[test]
    fn cross_paired_table_unit_only_difference_rejects() {
        let prepared_bytes = prepared_fixture_bytes();
        let artifact =
            PreparedArtifact::parse(prepared_bytes.clone(), DecodeLimits::default()).unwrap();
        let declared = artifact.prepared().constructors().first().unwrap();
        let table = table_with_one_entry_mutated(&artifact, declared, |dc| {
            dc.identity.unit.push_str("-other")
        });
        let metadata = write_metadata(&table, &MetaWarnings::default()).unwrap();
        let raw = vec![raw_target("entry", prepared_bytes)];
        assert!(matches!(
            assemble(&metadata, &raw, |_, _, _| {}),
            Err(CompileError::ReadError(
                tidepool_repr::serial::ReadError::ConstructorMetadata(
                    tidepool_repr::ConstructorMetadataMismatch::Identity { .. }
                )
            ))
        ));
    }

    /// Arity IS checked: a table entry agreeing on id and name but declaring
    /// a different field count must reject.
    #[test]
    fn field_count_disagreement_rejects() {
        let prepared_bytes = prepared_fixture_bytes();
        let artifact =
            PreparedArtifact::parse(prepared_bytes.clone(), DecodeLimits::default()).unwrap();
        let decl = artifact
            .prepared()
            .constructors()
            .first()
            .expect("fixture declares at least one constructor")
            .clone();

        let table = table_with_one_entry_mutated(&artifact, &decl, |dc| {
            dc.rep_arity += 1;
        });
        let meta_bytes = write_metadata(&table, &MetaWarnings::default()).unwrap();
        let raw = vec![raw_target("entry", prepared_bytes)];

        let err = match assemble(&meta_bytes, &raw, |_, _, _| {}) {
            Ok(_) => panic!("a field-count disagreement at agreeing id+name must reject assembly"),
            Err(e) => e,
        };
        assert!(matches!(
            err,
            CompileError::ReadError(tidepool_repr::serial::ReadError::ConstructorMetadata(
                tidepool_repr::ConstructorMetadataMismatch::Shape { .. }
            ))
        ));
    }
}

#[cfg(test)]
mod completed_response_tests {
    use super::*;

    const DEPENDENCY: &str =
        include_str!("../tests/fixtures/completed-response/ResponseDependency.hs");
    const CONSUMER: &str = include_str!("../tests/fixtures/completed-response/ResponseConsumer.hs");
    const REJECTED: &str = include_str!("../tests/fixtures/completed-response/RejectedConsumer.hs");

    struct CandidateFixture {
        _directory: TempDir,
        include: [PathBuf; 1],
        dependency: PathBuf,
    }

    impl CandidateFixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let include = [directory.path().to_owned()];
            let dependency = directory.path().join("ResponseDependency.hs");
            std::fs::write(&dependency, DEPENDENCY).unwrap();
            let compiled = crate::artifacts::test_support::compile_invocation(
                &CompileInvocation {
                    source: CONSUMER,
                    targets: &["result"],
                    include: &include,
                    fallback_module_name: "ResponseConsumer",
                },
                |_, _, _| {},
            )
            .expect("production compiler issues the candidate control");
            let selected = module_candidates::select_configured(
                compiled.producer_identity.as_ref().unwrap(),
                &include,
                &directory.path().join("offer"),
            )
            .expect("candidate selection")
            .expect("genuine candidate was published");
            assert!(selected
                .by_owner
                .contains_key(&("main".into(), "ResponseDependency".into())));
            Self {
                _directory: directory,
                include,
                dependency,
            }
        }

        fn invocation<'a>(&'a self, source: &'a str) -> CompileInvocation<'a> {
            CompileInvocation {
                source,
                targets: &["result"],
                include: &self.include,
                fallback_module_name: "ResponseConsumer",
            }
        }
    }

    #[test]
    fn owned_candidate_request_uses_captured_companions_after_alias_drift() {
        let fixture = CandidateFixture::new();
        let root = fixture._directory.path();
        let command = ExtractCmd::new().unwrap();
        let endpoint =
            crate::toolchain::AdmittedCompilerEndpoint::from_bound(command.bind().unwrap())
                .unwrap();
        let output = root.join("captured-candidate-output");
        std::fs::create_dir(&output).unwrap();
        let offer =
            ModuleCandidateOffer::select_admitted(&endpoint, &fixture.include, &output).unwrap();
        let selected = offer.selected.as_ref().expect("production candidate owner");
        let envelope: Value =
            ciborium::de::from_reader(std::fs::read(&selected.manifest_path).unwrap().as_slice())
                .unwrap();
        let acquisition = envelope.as_array().unwrap()[7].as_array().unwrap();
        assert_eq!(acquisition[0].as_text(), Some("continue-originals"));
        let mut aliases = BTreeSet::new();
        for image in acquisition[2].as_array().unwrap() {
            for part in image.as_array().unwrap()[4].as_array().unwrap() {
                let path = PathBuf::from(part.as_array().unwrap()[1].as_text().unwrap());
                if aliases.insert(path.clone()) {
                    if aliases.len() % 2 == 0 {
                        std::fs::remove_file(path).unwrap();
                    } else {
                        std::fs::write(path, b"changed after candidate capture").unwrap();
                    }
                }
            }
        }
        assert!(aliases.len() >= 5);
        let input = root.join("ResponseConsumer.hs");
        std::fs::write(&input, CONSUMER).unwrap();
        let mut command = ExtractCmd::new().unwrap();
        command
            .input(input)
            .target("result")
            .includes(&fixture.include)
            .output_dir(&output);
        offer.apply_to(&mut command).unwrap();
        let run = crate::artifacts::test_support::with_settlement(|recipient| {
            endpoint.execute_with_input_files(&command, offer.input_transport_files(), |close| {
                recipient(close)
            })
        })
        .unwrap();
        diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)
            .expect("cold worker consumes captured candidate companions");
        let receipt = certified_products::decode_receipt_in(
            &std::fs::read(output.join("certified-products.cbor")).unwrap(),
            Some(&output),
        )
        .unwrap();
        assert!(
            receipt
                .modules
                .iter()
                .any(|module| module.module == "ResponseDependency"
                    && module.origin == certified_products::ProductOrigin::Cached),
            "alias drift cannot silently fall back to source recompilation"
        );
    }

    #[test]
    fn offered_candidates_do_not_replay_source_failure() {
        let fixture = CandidateFixture::new();
        let mut completed_requests = 0;
        let result = crate::artifacts::test_support::compile_invocation(
            &fixture.invocation(REJECTED),
            |stage, _, _| {
                if stage == timing::STAGE_EXTRACT_SPAWN {
                    completed_requests += 1;
                }
            },
        );
        assert_eq!(completed_requests, 1, "completed source failures are final");
        let Err(CompileError::Diagnostics(diagnostics)) = result else {
            panic!("expected the original source failure");
        };
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == diag::DiagnosticSeverity::Error
                && diagnostic
                    .span
                    .as_ref()
                    .is_some_and(|span| span.file.ends_with("ResponseConsumer.hs"))
        }));
        crate::artifacts::test_support::compile_invocation(
            &fixture.invocation(CONSUMER),
            |_, _, _| {},
        )
        .expect("an explicit repaired request can still compile");
    }

    #[test]
    fn offered_candidates_do_not_replay_product_admission_failure() {
        let fixture = CandidateFixture::new();
        let mut completed_requests = 0;
        let mut output_read = false;
        let result = crate::artifacts::test_support::compile_invocation(
            &fixture.invocation(CONSUMER),
            |stage, _, _| {
                if stage == timing::STAGE_EXTRACT_SPAWN {
                    completed_requests += 1;
                }
                if stage == timing::STAGE_CBOR_READ && !output_read {
                    output_read = true;
                    // The worker has completed, but Rust has not admitted its
                    // dependency witnesses or product receipt yet.
                    std::fs::write(&fixture.dependency, DEPENDENCY.replace("41", "42")).unwrap();
                }
            },
        );
        assert!(
            output_read,
            "the failure must follow completed output reading"
        );
        assert_eq!(
            completed_requests, 1,
            "product refusal cannot replay source"
        );
        let Err(CompileError::CompilerEvidence(evidence)) = result else {
            panic!(
                "expected typed current-source refusal, got {}",
                match result {
                    Ok(_) => "accepted changed current source".to_owned(),
                    Err(error) => format!("{error:?}"),
                }
            );
        };
        let certified_products::CertificationError::CompletedSourceEvidence { failure, .. } =
            *evidence
        else {
            panic!("expected completed source evidence refusal");
        };
        let cache::DependencyEvidenceFailure::Source {
            reason: cache::SourceWitnessFailure::Changed { expected, actual },
            ..
        } = *failure
        else {
            panic!("expected changed authored source evidence");
        };
        use sha2::{Digest, Sha256};
        assert_eq!(
            expected,
            format!("{:x}", Sha256::digest(DEPENDENCY.as_bytes()))
        );
        assert_eq!(
            actual,
            format!(
                "{:x}",
                Sha256::digest(DEPENDENCY.replace("41", "42").as_bytes())
            )
        );
        crate::artifacts::test_support::compile_invocation(
            &fixture.invocation(CONSUMER),
            |_, _, _| {},
        )
        .expect("an explicit request can compile the changed dependency");
    }
}

#[cfg(test)]
mod dependency_cache_tests {
    use super::*;

    /// A package-importing graph, including implicit Prelude, must enter GHC
    /// for package selection even if its paired bundle remains available.
    #[test]
    fn package_imports_force_worker_selection_for_home_shadow_changes() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let library = second.join("CacheDependency.hs");
        std::fs::write(
            &library,
            "module CacheDependency where\nvalue = (41 :: Int)\n",
        )
        .unwrap();
        let roots = [first.clone(), second.clone()];
        let invocation = CompileInvocation {
            source: "module CacheConsumer where\nimport CacheDependency\nresult = value + 1\n",
            targets: &["result"],
            include: &roots,
            fallback_module_name: "CacheConsumer",
        };
        let compile = || {
            let mut prepared = false;
            crate::artifacts::test_support::compile_invocation(&invocation, |stage, _, _| {
                prepared |= stage == timing::STAGE_EXTRACT_SPAWN;
            })
            .expect("fixture compiler request");
            prepared
        };
        assert!(compile(), "fresh recipe compiles");
        assert!(
            compile(),
            "package lookup requires the worker on a repeated request"
        );
        std::fs::write(
            &library,
            "module CacheDependency where\nvalue = (42 :: Int)\n",
        )
        .unwrap();
        assert!(compile(), "consumed source bytes invalidate");
        let shadow = first.join("CacheDependency.hs");
        std::fs::write(
            &shadow,
            "module CacheDependency where\nvalue = (43 :: Int)\n",
        )
        .unwrap();
        assert!(compile(), "a new higher-priority home module invalidates");
        std::fs::remove_file(shadow).unwrap();
        assert!(compile(), "removing the selected module invalidates");
    }
}

#[cfg(test)]
mod program_support_tests {
    use super::*;

    #[test]
    fn continuation_support_refuses_backedges_to_its_exact_generated_owner() {
        use crate::certified_products::tests::{
            original_groups_fixture_with_interface, recovered_witness_fixtures,
        };
        let generated = certified_products::fixture_finalized_product(
            original_groups_fixture_with_interface(
                "Generated",
                vec![(3, vec![])],
                7,
                &BTreeMap::new(),
                b"Generated interface".to_vec(),
            ),
            [1; 32],
        );
        let generated = recovered_witness_fixtures(&[generated]).remove(0).product;
        let required = certified_products::PendingImportOwner::Source {
            owner: generated.owner().clone(),
            original_ordinal: 3,
            binder: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "Generated".into(),
                namespace: "value".into(),
                occurrence: "entry_3".into(),
                record_parent: None,
            },
        };
        for backedge in [false, true] {
            let support = certified_products::fixture_finalized_product(
                original_groups_fixture_with_interface(
                    "Support",
                    vec![(
                        3,
                        if backedge {
                            vec![required.clone()]
                        } else {
                            vec![]
                        },
                    )],
                    7,
                    &BTreeMap::new(),
                    b"Support interface".to_vec(),
                ),
                [1; 32],
            );
            assert_ne!(
                generated.owner().skinny_iface_sha256,
                support.owner().skinny_iface_sha256,
            );
            let products = recovered_witness_fixtures(&[generated.clone(), support])
                .into_iter()
                .map(|row| row.product)
                .collect::<Vec<_>>();
            let view = crate::declaration_context::certified_product_artifact_view(
                [1; 32],
                &products,
                &[],
                None,
            )
            .unwrap();
            let exact = crate::declaration_join::ExactModuleIdentity {
                unit: "fixture".into(),
                module: "Generated".into(),
            };
            let result = program_support_artifacts(&view, &exact);
            if backedge {
                assert!(matches!(result, Err(CompileError::ExtractFailed(message))
                    if message == "program support depends on its generated scaffold"));
            } else {
                let support = result.unwrap();
                assert!(support
                    .descriptors()
                    .iter()
                    .all(|entry| entry.owner != exact));
                assert!(support
                    .descriptors()
                    .iter()
                    .any(|entry| entry.owner.module == "Support"));
            }
            let foreign = crate::declaration_join::ExactModuleIdentity {
                unit: "foreign".into(),
                module: "Generated".into(),
            };
            let unexcluded = program_support_artifacts(&view, &foreign).unwrap();
            assert_eq!(
                unexcluded.descriptors(),
                view.descriptors(),
                "excluding a foreign-unit name cannot hide the actual generated owner"
            );
        }
    }
}

#[cfg(test)]
mod source_proof_pairing_tests {
    use super::*;

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn source_selected_receipt_pairs_prior_program_support_with_actual_original_proof() {
        let directory = tempfile::tempdir().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let root = directory.path();
            let module = tidepool_repr::SessionModule::lib(tidepool_repr::Generation(1));
            let original = include_str!("../tests/fixtures/checked-prefix-publication/G1.hs");
            let support = include_str!(
                "../tests/fixtures/checked-prefix-publication/PrefixSelectedSupport.hs"
            );
            let original_path = root.join(module.relative_hs_path());
            std::fs::create_dir_all(original_path.parent().unwrap()).unwrap();
            std::fs::write(&original_path, original).unwrap();
            let support_path = root.join("PrefixSelectedSupport.hs");
            std::fs::write(&support_path, support).unwrap();
            let includes = vec![root.to_path_buf()];
            let certificate = crate::artifacts::test_support::certify_authored_declaration(
                module,
                &original_path,
                original,
                &includes,
                root,
            )
            .expect("actual compiler issues the original support proof");
            let owner = crate::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: "PrefixSelectedSupport".into(),
            };
            // Retain the prior compiler's original native/canonical support custody
            // for the persisted-versus-local source-selection receipt comparison.
            let products = certificate
                .recovery_products()
                .into_iter()
                .filter(|product| {
                    product.owner().unit == owner.unit && product.owner().module == owner.module
                })
                .collect::<Vec<_>>();
            assert_eq!(products.len(), 1);
            let persisted =
                crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
                    .unwrap()
                    .extend_checked_original_products(
                        certificate.toolchain_identity_sha256(),
                        &products,
                    )
                    .unwrap();
            assert!(persisted.authored_native_root(1).is_err());
            let (endpoint, _) = crate::toolchain::bind_extract_endpoint().unwrap();
            let offer = ModuleCandidateOffer::select_inspection(
                endpoint.identity().producer_bytes(),
                &includes,
                &root.join("scope"),
                Some(Arc::new(persisted)),
                Vec::new(),
                &[],
            )
            .expect("inspection owns the checked purpose and current source roots");
            let request = offer
                .exact
                .as_ref()
                .expect("inspection supplies its exact request")
                .clone();
            crate::declaration_context::assert_source_selected_receipt_pairing(
                root, module, original, support, request, endpoint, &offer,
            );
        }));
        if let Err(panic) = result {
            let retained = directory.keep();
            eprintln!(
                "source-proof pairing inputs retained at {}",
                retained.display()
            );
            std::panic::resume_unwind(panic);
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use tidepool_extract_cmd::CompilerTransactionClose;

    // The operation owns this observation outside the induced unwind boundary.
    // These tests expect settled compilers; uncertainty is inspected as a failure.
    pub(crate) fn with_settlement<T>(
        action: impl FnOnce(&mut dyn FnMut(CompilerTransactionClose)) -> T,
    ) -> T {
        let observation = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recipient = Arc::clone(&observation);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            action(&mut |close| recipient.lock().unwrap().push(close))
        }));
        let closes = observation.lock().unwrap();
        assert!(
            closes.iter().all(|close| matches!(
                close,
                CompilerTransactionClose::Clean | CompilerTransactionClose::NotStarted
            )),
            "compiler close is unconfirmed: {closes:?}"
        );
        drop(closes);
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    pub(crate) fn compile_invocation(
        inv: &CompileInvocation<'_>,
        stage: impl FnMut(&str, Duration, u64),
    ) -> Result<CompiledArtifacts, CompileError> {
        with_settlement(|recipient| super::compile_invocation(inv, stage, recipient))
    }
    pub(crate) fn compile_invocation_in_context(
        inv: &CompileInvocation<'_>,
        context: Arc<crate::declaration_join::ExactDeclarationContext>,
        stage: impl FnMut(&str, Duration, u64),
    ) -> Result<CompiledArtifacts, CompileError> {
        with_settlement(|recipient| {
            super::compile_invocation_in_context(inv, context, stage, recipient)
        })
    }
    pub(crate) fn compile_targets(
        source: &str,
        targets: &[&str],
        include: &[PathBuf],
        stage: impl FnMut(&str, Duration, u64),
    ) -> Result<CompiledArtifacts, CompileError> {
        with_settlement(|recipient| {
            super::compile_targets(source, targets, include, stage, recipient)
        })
    }
    pub(crate) fn certify_authored_declaration(
        module: tidepool_repr::SessionModule,
        path: &Path,
        source: &str,
        includes: &[PathBuf],
        root: &Path,
    ) -> Result<crate::declaration_join::CertifiedAuthoredDeclaration, CompileError> {
        with_settlement(|recipient| {
            crate::declaration_join::certify_authored_declaration(
                module, path, source, includes, root, recipient,
            )
        })
    }
    pub(crate) fn certify_authored_declaration_in_context(
        module: tidepool_repr::SessionModule,
        path: &Path,
        source: &str,
        includes: &[PathBuf],
        root: &Path,
        context: Arc<crate::declaration_join::ExactDeclarationContext>,
    ) -> Result<crate::declaration_join::CertifiedAuthoredDeclaration, CompileError> {
        with_settlement(|recipient| {
            crate::declaration_join::certify_authored_declaration_in_context(
                module, path, source, includes, root, context, recipient,
            )
        })
    }
    pub(crate) fn prepare_frozen_production_entry_with_catalog(
        sources: &FrozenEntrySources,
        scratch: &Path,
        output: &Path,
        catalog: &crate::toolchain::CatalogSelection,
    ) -> Result<ProductionEntryOutput, CompileError> {
        with_settlement(|recipient| {
            super::prepare_frozen_production_entry_with_catalog(
                sources, scratch, output, catalog, recipient,
            )
        })
    }
    pub(crate) fn prepare_frozen_production_entry(
        sources: &FrozenEntrySources,
        scratch: &Path,
        output: &Path,
    ) -> Result<ProductionEntryOutput, CompileError> {
        with_settlement(|recipient| {
            super::prepare_frozen_production_entry(sources, scratch, output, recipient)
        })
    }
}
