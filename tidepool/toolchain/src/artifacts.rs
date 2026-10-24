//! The ONE policy-bearing `tidepool-extract` compile front door:
//! [`CompileInvocation`] + [`compile_invocation`]. `tidepool_runtime::compile_haskell`
//! and [`compile_targets`] are thin projections over that owner, so spawning
//! the extractor, reading its output directory, and deserializing typed
//! artifacts happen in exactly one place.
//!
//! Each request acquires one complete named physical output. Native reuse
//! retains its source evidence and compiler-issued product authority.

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
use tidepool_repr::serial::MetaWarnings;
use tidepool_repr::DataConTable;

use crate::prepared_artifact::{prepared_artifact_name, PreparedArtifact};
use crate::{
    cache, certified_products, diag, extract_module_name, extract_spawn_error, module_candidates,
    timing, CompileError,
};

mod catalog_inventory;
mod execution_diagnostics;
mod failure_sources;
pub(crate) mod invocation_output;
mod production_entry;
mod selected_session_values;
#[cfg(test)]
mod world_value_selection_tests;
pub(crate) use execution_diagnostics::InputArtifactDiagnostics;
pub use execution_diagnostics::{compiler_scratch_directory, CompilerDiagnosticCapture};
pub use production_entry::{
    build_production_entry, load_production_entry, load_selected_production_entry,
    load_selected_production_entry_with_catalog, prepare_frozen_production_entry,
    prepare_frozen_production_entry_with_catalog, FrozenEntrySources, ProductionEntryOutput,
    ProductionEntrySources,
};
pub use selected_session_values::{SelectedSessionValueOwners, SessionValueSelectionError};

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
    let offer = offer.apply_to(command, directory.path())?;
    let command = offer.command();
    let diagnostics = CompilerDiagnosticCapture::start(directory.path(), command);
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
    specification: Arc<crate::checked_cell::CellRecipe>,
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
    input: &'a crate::checked_cell::CanonicalInputTypeWitness,
    progress: Option<&'a crate::checked_cell::CanonicalInputTypeWitness>,
    response: &'a crate::checked_cell::CanonicalInputTypeWitness,
    signatures: &'a crate::checked_cell::RequestTypeSignatures,
}

impl<'a> RequestInputLayout<'a> {
    pub fn input(&self) -> &'a crate::checked_cell::CanonicalInputTypeWitness {
        self.input
    }

    pub fn progress(&self) -> Option<&'a crate::checked_cell::CanonicalInputTypeWitness> {
        self.progress
    }

    pub fn response_index(&self) -> usize {
        if self.progress.is_some() {
            2
        } else {
            1
        }
    }

    pub fn response(&self) -> &'a crate::checked_cell::CanonicalInputTypeWitness {
        self.response
    }

    pub fn signatures(&self) -> &'a crate::checked_cell::RequestTypeSignatures {
        self.signatures
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RequestInputLayoutError {
    #[error("request site {site} has no compiler-issued request signatures")]
    MissingSignatures { site: u64 },
    #[error(
        "request site {site} describes {actual} live input types, expected {expected} from its compiler-issued request signatures"
    )]
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
        let witness = |index: usize| {
            self.input_type_witnesses[index]
                .as_ref()
                .ok_or(RequestInputLayoutError::MissingWitness { site, index })
        };
        Ok(RequestInputLayout {
            input: witness(0)?,
            progress: if signatures.progress().is_some() {
                Some(witness(1)?)
            } else {
                None
            },
            response: witness(expected - 1)?,
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
    native_inputs: Option<crate::artifact_inventory::CertifiedTargetNativeInputs>,
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
    /// Rows certified by this physical output's demand issuer. Inherited
    /// source implementations remain with their completed target owner and
    /// are inspected through `native_source_products`.
    pub certified_groups: Arc<[certified_products::PendingCertifiedGroup]>,
    pub recovery_products: Vec<crate::recovery_artifacts::CertifiedRecoveryProduct>,
    pub(crate) issued_output: Option<crate::declaration_context::IssuedOriginalOutput>,
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
    checked_values: Option<Arc<crate::checked_cell::CheckedValueInputs>>,
    activation_preview: Option<crate::activation_preview::ActivationPreviewOffer>,
}

/// Required complete session-value selection for output certification.
/// Construction is restricted to live request application and validated retained entries.
pub struct SelectedModuleCandidateOffer {
    offer: ModuleCandidateOffer,
    selected_session_values: SelectedSessionValueOwners,
}

impl std::ops::Deref for SelectedModuleCandidateOffer {
    type Target = ModuleCandidateOffer;
    fn deref(&self) -> &Self::Target {
        &self.offer
    }
}

/// One final physical request and its required output-selection evidence.
/// The request cannot be mutated after application.
pub struct AppliedModuleCandidateOffer {
    selected: SelectedModuleCandidateOffer,
    command: ExtractCmd,
    output_owner: Option<Arc<TempDir>>,
}

impl std::ops::Deref for AppliedModuleCandidateOffer {
    type Target = SelectedModuleCandidateOffer;
    fn deref(&self) -> &Self::Target {
        &self.selected
    }
}

impl AppliedModuleCandidateOffer {
    pub fn command(&self) -> &ExtractCmd {
        &self.command
    }

    pub fn execute_admitted_turn(
        &self,
        endpoint: crate::toolchain::AdmittedCompilerEndpoint,
        settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
    ) -> Result<AdmittedTurnOutput, CompileError> {
        let directory = self.output_owner.clone().ok_or_else(|| {
            CompileError::ExtractFailed("ordinary turn lacks its prepared output owner".into())
        })?;
        let request = tidepool_extract_cmd::ExtractRequest::decode(&self.command.request_bytes())
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        if self.producer != endpoint.identity().producer_bytes() {
            return Err(CompileError::ExtractFailed(
                "admitted turn endpoint differs from applied request".into(),
            ));
        }
        let deployment = endpoint.deployment().clone();
        if deployment.package_source_association().is_none() {
            return Err(CompileError::ExtractFailed(
                "native turn requires an admitted package service profile".into(),
            ));
        }
        let diagnostics = CompilerDiagnosticCapture::start(directory.path(), &self.command);
        let run = endpoint
            .execute_with_input_files(&self.command, self.input_transport_files(), |close| {
                settlement(close)
            })
            .map_err(|error| {
                self.retain_execution_failure(directory.path(), &self.command, error)
            })?;
        diagnostics.completed(
            directory.path(),
            &self.command,
            run.success(),
            &run.output.stderr,
        );
        let (turn, native) = if run.success()
            && diag::decode_extract_result(true, &run.output.stdout, &run.output.stderr).is_ok()
        {
            if self
                .activation_preview_unavailable(directory.path())
                .map_err(|error| {
                    self.retain_failure(directory.path(), &self.command, &run.output.stderr, error)
                })?
            {
                (None, None)
            } else {
                self.read_admitted_turn(
                    directory.path(),
                    request.supports_compile_input_identity(),
                    &run,
                    &deployment,
                )
                .map_err(|error| {
                    self.retain_failure(directory.path(), &self.command, &run.output.stderr, error)
                })?
            }
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
}

/// Compiler-authenticated physical outputs. This owner carries no endpoint;
/// contextual sealing continues after the physical transaction has settled.
pub struct PreparedCellProgram {
    checked: Arc<crate::checked_cell::ExactCheckedCell>,
    world: Arc<crate::compiler_world::CheckedWorldCellProgram>,
    slots: Vec<crate::checked_cell::CheckedPlannedCellSlot>,
    items: Vec<PreparedProgramItem>,
    produced_outputs: crate::checked_cell::ProducedValueTypeInterfaces,
}

pub(crate) struct ProgramItemAdmission {
    pub(crate) context: crate::checked_cell::CellProgramItemContext,
    pub(crate) output: Option<ProgramNativeOutput>,
}

struct PreparedProgramItem {
    context: crate::checked_cell::CellProgramItemContext,
    native: Option<PreparedProgramNativeItem>,
}

struct PreparedProgramNativeItem {
    output: ProgramNativeOutput,
    checked: crate::checked_cell::PreparedCheckedItemOutput,
}

impl std::fmt::Debug for PreparedCellProgram {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedCellProgram")
            .field("items", &self.checked.item_count())
            .finish_non_exhaustive()
    }
}

impl PreparedCellProgram {
    #[cfg(test)]
    pub(crate) fn assert_world_cursor_controls(
        &self,
        program: &Arc<crate::compiler_world::CheckedWorldCellProgram>,
    ) {
        let outputs = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(ordinal, item)| {
                item.native
                    .as_ref()
                    .map(|native| (ordinal, native.output.clone()))
            })
            .collect::<Vec<_>>();
        crate::declaration_context::assert_world_program_cursor_refusals(program, &outputs);
    }

    pub fn checked_cell(&self) -> &Arc<crate::checked_cell::ExactCheckedCell> {
        &self.checked
    }

    #[tracing::instrument(
        target = "exomonad_harness::timing",
        name = "products.cell_program.seal",
        level = "debug",
        skip_all,
        fields(inclusive = true)
    )]
    pub fn seal(
        self,
        cancellation: &tidepool_extract_cmd::HostAdmissionCancellation,
    ) -> Result<Arc<crate::checked_cell::CellProgram>, CompileError> {
        use crate::checked_cell::{
            self, CellProgram, CellProgramItem, CellProgramObservations, CheckedItemKind,
            CheckedPlannedCellSlot,
        };
        cancellation.checkpoint()?;
        let Self {
            checked: cell,
            world,
            slots,
            items: prepared,
            produced_outputs,
        } = self;
        if prepared.len() != cell.item_count() {
            return Err(CompileError::ExtractFailed(
                "program item admissions differ from checked items".into(),
            ));
        }
        let mut prefix = if cell.item_count() == 0 {
            None
        } else {
            Some(cell.item(0)?.initial_prefix()?)
        };
        let mut items = Vec::with_capacity(cell.item_count());
        for (index, prepared) in prepared.into_iter().enumerate() {
            cancellation.checkpoint()?;
            let _item_span = tracing::debug_span!(target: "exomonad_harness::timing", "products.checked_item", index, inclusive = true).entered();
            let item = cell.item(index)?;
            let completed = prefix.as_ref().expect("nonempty program prefix");
            let item_context = prepared.context;
            if item.kind() == CheckedItemKind::Declaration {
                if prepared.native.is_some() {
                    return Err(CompileError::ExtractFailed(
                        "program declaration owns a native target".into(),
                    ));
                }
                prefix = Some(completed.append_program_original(item.clone())?);
                items.push(CellProgramItem {
                    context: item_context,
                    checked: item,
                    native: None,
                    native_observations: None,
                });
                continue;
            }
            let context = &item_context.context;
            let source_lexical = &item_context.source_lexical;
            let value_support = crate::declaration_context::PreparedValueSourceSupport::new(
                context.clone(),
                source_lexical,
            )?;
            let prepared_native = prepared.native.ok_or_else(|| {
                CompileError::ExtractFailed("program native target is missing".into())
            })?;
            let output = prepared_native.output;
            let (generation, observation) = match &slots[index] {
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
                generation,
                observation_name: observation,
                settled_values: completed.prepared_value_selection()?,
            }
            .seal_prepared(
                prepared_native.checked,
                context,
                &value_support,
                &produced_outputs.for_segment(index..index + 1)?,
            )?;
            let next = completed.append(native.clone())?;
            let observation = CellProgramObservations {
                turn: output.turn,
                products: output.products.expect("program output was sealed"),
            };
            items.push(CellProgramItem {
                context: item_context,
                checked: item,
                native: Some(native),
                native_observations: Some(observation),
            });
            prefix = Some(next);
        }
        let program = Arc::new(CellProgram {
            checked: cell,
            world,
            slots,
            items,
        });
        cancellation.checkpoint()?;
        Ok(program)
    }
}

/// Prepare native/compiler evidence from the same checked World output.
/// Runtime settlement still owns END/origin/cancellation before `seal`.
pub fn prepare_world_cell_program(
    program: Arc<crate::compiler_world::CheckedWorldCellProgram>,
) -> Result<PreparedCellProgram, CompileError> {
    prepare_world_cell_program_inner(
        program,
        #[cfg(test)]
        &mut |_, _| {},
        #[cfg(test)]
        &mut |_| {},
    )
}

#[cfg(test)]
pub(crate) fn prepare_world_cell_program_observing_home_contracts(
    program: Arc<crate::compiler_world::CheckedWorldCellProgram>,
    observe: &mut dyn FnMut(usize, &certified_products::CertifiedSegmentOriginals),
) -> Result<PreparedCellProgram, CompileError> {
    prepare_world_cell_program_inner(program, observe, &mut |_| {})
}

#[cfg(test)]
pub(crate) fn prepare_world_cell_program_observing_source_receipts(
    program: Arc<crate::compiler_world::CheckedWorldCellProgram>,
    observe: &mut dyn FnMut(&mut crate::declaration_context::DecodedWorldSourceReceipt<'_>),
) -> Result<PreparedCellProgram, CompileError> {
    prepare_world_cell_program_inner(program, &mut |_, _| {}, observe)
}

fn prepare_world_cell_program_inner(
    program: Arc<crate::compiler_world::CheckedWorldCellProgram>,
    #[cfg(test)] observe: &mut dyn FnMut(usize, &certified_products::CertifiedSegmentOriginals),
    #[cfg(test)] observe_source: &mut dyn FnMut(
        &mut crate::declaration_context::DecodedWorldSourceReceipt<'_>,
    ),
) -> Result<PreparedCellProgram, CompileError> {
    use crate::checked_cell::{self, CheckedPlannedCellSlot};
    crate::host_work::checkpoint()?;
    let root = program.directory().path();
    let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(
        program.validation().inventory.clone(),
    );
    let produced_outputs =
        checked_cell::CheckedValueInputs::capture_world_produced_types(&program, &mut validation)?;
    let mut continuation = crate::declaration_context::WorldProgramContinuation::begin(&program);
    let mut stages = Vec::new();
    for stage in program.stages() {
        crate::host_work::checkpoint()?;
        let first = *stage
            .ordinals()
            .first()
            .ok_or_else(|| CompileError::ExtractFailed("World stage has no item".into()))?;
        let end = stage.ordinals().last().expect("nonempty stage") + 1;
        let original = stage.identity().original_owner().is_some();
        let stage_produced =
            produced_outputs.for_segment(first..if original { first } else { end })?;
        let pending = continuation.open_next(
            &program,
            &stage_produced,
            &mut validation,
            #[cfg(test)]
            observe_source,
        )?;
        // The observer sees an actual issued owner; it cannot issue or replace it.
        #[cfg(test)]
        observe(stage.index(), &pending.segment().originals);
        let (next, sealed) = match pending {
            crate::declaration_context::PendingWorldStage::Original(pending) => {
                pending.seal(&program, &stage_produced)?
            }
            crate::declaration_context::PendingWorldStage::Executable(mut pending) => {
                let segment = pending.segment();
                let mut artifacts =
                    crate::declaration_context::bind_certified_segment_artifacts_with_validation(
                        &segment.originals,
                        &mut validation,
                    )?;
                for &index in stage.ordinals() {
                    let segment = pending.segment();
                    crate::host_work::checkpoint()?;
                    let produced = produced_outputs.for_item(index)?;
                    let item = program.item(index).ok_or_else(|| {
                        CompileError::ExtractFailed("World output lacks its reserved item".into())
                    })?;
                    let prepared_path = item
                        .executable()
                        .ok_or_else(|| {
                            CompileError::ExtractFailed(
                                "World original has no executable manifest".into(),
                            )
                        })?
                        .manifest()
                        .keys()
                        .find(|path| {
                            path.to_str()
                                .is_some_and(|path| path.ends_with(".prepared.cbor"))
                        })
                        .ok_or_else(|| {
                            CompileError::ExtractFailed(
                                "World item lacks its prepared program".into(),
                            )
                        })?;
                    let directory = root.join(format!("item-{index}"));
                    let relative = PathBuf::from(format!("item-{index}"));
                    let bytes =
                        checked_cell::world_output_bytes(&program, &relative.join(prepared_path))?;
                    validation
                        .inventory
                        .charge(bytes.len().checked_mul(64).ok_or_else(|| {
                            CompileError::ExtractFailed(
                                "World prepared program accounting overflow".into(),
                            )
                        })?)
                        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                    let program_artifact =
                        crate::prepared_artifact::PreparedArtifact::parse_shared(
                            Arc::new(bytes.as_ref().to_vec()),
                            &crate::prepared_artifact::production_requirements()?,
                            DecodeLimits::default(),
                        )?;
                    let target = Arc::clone(program_artifact.prepared_shared());
                    let package_program = program_artifact.bind_package_program();
                    let item_receipt = certified_products::decode_segment_item_with_operation(
                        checked_cell::world_output_bytes(
                            &program,
                            &relative.join("certified-products.cbor"),
                        )?,
                        &validation.inventory,
                    )
                    .map_err(compiler_evidence_failure)?;
                    let (certified, _, packages, item_selection) = segment
                        .originals
                        .select_item(
                            &mut artifacts,
                            item_receipt,
                            &package_program,
                            Some(&produced),
                            &mut validation,
                        )
                        .map_err(compiler_evidence_failure)?;
                    let generated = segment.source().generated_source_owner();
                    let entry = checked_cell::CheckedTypedEntry::issue_world_item(
                        &program,
                        index,
                        &target,
                        &generated,
                        &certified.data().recovery_products,
                    )?;
                    let prefix_artifacts = segment.native_prefix_artifacts()?;
                    let cursor = SegmentItemDemand {
                        view: &prefix_artifacts,
                        groups: segment.group_baseline(),
                    };
                    let admitted = admit_native_target_products(
                        &invocation_output::CapturedNativeOutput::from_world(&program),
                        &certified,
                        segment.package_products(),
                        item_selection,
                        segment
                            .product_admission()
                            .compiler_inputs()
                            .compiler_source_support(),
                        &segment_package_availability(&segment.originals, &packages),
                        Some(&segment.originals),
                        Some(entry),
                        None,
                        Some(cursor),
                        &mut validation,
                    )?;
                    let original = SealedOriginalEvidence::issue_exact(
                        &segment.product_admission(),
                        &admitted,
                    )?;
                    let products = Arc::new(SealedTurnProducts {
                        prepared: target.clone(),
                        original,
                        target: admitted.target,
                        original_compile_input: None,
                        checked: None,
                        certified_groups: admitted.certified_groups,
                        recovery_products: certified.data().recovery_products.clone(),
                        retained_core_products: certified.data().retained_core_products.clone(),
                        package_interfaces: admitted.package_interfaces,
                    });
                    let output = ProgramNativeOutput {
                        origin: ProgramNativeOutputOrigin::World {
                            program: Arc::clone(&program),
                            ordinal: index,
                        },
                        directory,
                        target,
                        program: package_program,
                        turn: checked_cell::world_output_bytes(
                            &program,
                            &relative.join("turn.cbor"),
                        )?
                        .clone(),
                        metadata: checked_cell::world_output_bytes(
                            &program,
                            &relative.join("meta.cbor"),
                        )?
                        .clone(),
                        products: Some(products.clone()),
                        source: segment.source_text().to_owned(),
                    };
                    pending.admit_item(&program, output, &stage_produced)?;
                }
                pending.seal()?
            }
        };
        continuation = next;
        stages.push(sealed);
    }
    continuation.complete(&program)?;
    let (checked, admissions) = checked_cell::admit_world_cell(program.clone(), stages)?;
    let mut items = Vec::with_capacity(admissions.len());
    for (index, admission) in admissions.into_iter().enumerate() {
        let native = match admission.output {
            None => None,
            Some(output) => {
                let slot = program.reservation().items()[index].slot();
                let (generation, observation) = match &slot {
                    CheckedPlannedCellSlot::Bind { value } => (*value, None),
                    CheckedPlannedCellSlot::Expression {
                        capture,
                        observation_name,
                    } => (*capture, Some(observation_name.as_str())),
                    _ => {
                        return Err(CompileError::ExtractFailed(
                            "World original owns an executable output".into(),
                        ))
                    }
                };
                let checked = checked_cell::PreparedCheckedItemOutput::capture(
                    &checked.item(index)?,
                    generation,
                    observation,
                    &output,
                    &program,
                )?;
                Some(PreparedProgramNativeItem { output, checked })
            }
        };
        items.push(PreparedProgramItem {
            context: admission.context,
            native,
        });
    }
    let slots = program.reservation().slots().collect();
    Ok(PreparedCellProgram {
        checked,
        world: program,
        slots,
        items,
        produced_outputs,
    })
}

struct SegmentItemDemand<'a> {
    view: &'a crate::artifact_inventory::ArtifactView,
    groups: crate::declaration_context::CertifiedNativeGroupBaseline<'a>,
}

// Protected compilation manifests bind the actual worker search order as well
// as the source and interface recipe. Unsupported path encodings refuse before
// the invocation builder can perform its ordinary lossy CLI conversion.
pub(crate) enum CheckedPurpose {
    Inspection,
    ReloadInspection,
    ActivationPreview,
}

impl CheckedPurpose {
    /// Every owning encoder emits its complete wire purpose directly.
    pub(crate) const fn wire_tag(self) -> &'static str {
        match self {
            Self::Inspection => "inspection1",
            Self::ReloadInspection => "reload-inspection1",
            Self::ActivationPreview => "host-activation-renderer1",
        }
    }
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
pub(crate) fn encode_inspection_authorization(owners: &BTreeSet<String>, baseline: Value) -> Value {
    encode_inspection_authorization_for(CheckedPurpose::Inspection, owners, baseline)
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
            crate::declaration_join::ExactDeclarationContext::empty(),
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
                    product.module_interface()
                }
                crate::artifact_inventory::ArtifactPayload::Interface(_, _)
                | crate::artifact_inventory::ArtifactPayload::PackageCode(_) => return None,
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
        .iter()
        .cloned()
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
    match selected {
        Some(selected) => crate::declaration_context::OriginalCompilerInputs::from_candidates(
            context,
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer),
            selected,
        ),
        None => {
            crate::declaration_context::OriginalCompilerInputs::reuse_selected_authored(context)
        }
    }
}

impl ModuleCandidateOffer {
    /// Capture a selected compiler context, checked bindings, source bytes and
    /// import syntax. Empty and TYPE-only contexts are valid.
    ///
    /// Compose published sources with
    /// [`ExactDeclarationContext::with_published_source_originals`](crate::declaration_context::ExactDeclarationContext::with_published_source_originals)
    /// before capture so their lexical proof stays with the selected roles.
    /// Notebook declarations also retain their issued Joined or Recovered facade
    /// in `lexical`; import text alone cannot supply either proof.
    pub fn compiler_world_inputs(
        primary: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
        bindings: crate::compiler_world::VisibleWorldBindings,
        sources: Arc<crate::compiler_world::CapturedCompilerSources>,
        lexical: crate::compiler_world::CompilerWorldLexicalSurface,
    ) -> Result<crate::compiler_world::CompilerWorldOpenInputs, CompileError> {
        let primary = checked_offer_context(primary)?;
        let directory = Arc::new(compiler_scratch_directory()?);
        crate::compiler_world::CompilerWorldOpenInputs::from_primary_selection(
            primary, bindings, sources, lexical, directory,
        )
    }

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

            checked_values: None,
            activation_preview: None,
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

            checked_values: None,
            activation_preview: None,
        })
    }

    /// Inspection may consume certified live values but cannot publish new ones.
    pub fn select_inspection(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_join::ExactCompileContext>>,
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
        context: Option<Arc<crate::declaration_join::ExactCompileContext>>,
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
        .inspect_err(|error| {
            tracing::debug!(target: "tidepool::artifact_inventory::ambiguity_trace",
                stage = "inspection_offer", outcome = "error", %error,
                "native input construction refused");
        })
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
            context.map(|context| {
                Arc::new(crate::declaration_join::ExactCompileContext::new(context))
            }),
            values,
            retained,
        )
    }

    fn select_inspection_for(
        purpose: CheckedPurpose,
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_join::ExactCompileContext>>,
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
        context: Option<Arc<crate::declaration_join::ExactCompileContext>>,
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
        crate::declaration_context::trace_native_input_roles(
            "inspection_before_value_extension",
            context
                .as_ref()
                .map(|context| context.declarations().compiler_input_projection()),
        );
        let declarations = checked_value_context(
            context
                .as_ref()
                .map(|context| context.declarations().clone()),
            &inputs,
        )?;
        crate::declaration_context::trace_native_input_roles(
            "inspection_after_value_extension",
            Some(declarations.compiler_input_projection()),
        );
        let context = compile_context_with_declarations(context, declarations.clone());
        let authorization = checked_search_authorization(
            encode_inspection_authorization_for(purpose, &owners, inputs.baseline_authorization()),
            include,
        )?;
        let selected = immutable_candidates_in_context_with_catalog(
            &declarations,
            producer,
            include,
            scratch,
            owners,
            catalog,
        )?;
        let private = private_native_availability(&declarations, producer, selected.as_deref())?;
        crate::declaration_context::trace_native_input_roles(
            "inspection_private_fragment",
            private.as_ref().map(|input| input.role_projection()),
        );
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

            checked_values: Some(inputs),
            activation_preview: None,
        })
    }

    /// Specialize a pure renderer against its compiler-issued display facts.
    /// This boundary admits no request continuation annotations or protected
    /// cell scaffold. Its original type, instance graph and native dependencies
    /// belong to the opaque display context and preview authorization.
    pub fn select_activation_preview(
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
        include: &[PathBuf],
        scratch: &Path,
        display_context: Arc<crate::activation_preview::OriginalDisplayContext>,
        prototype: Arc<crate::checked_cell::ExactHostBindingPrototype>,
        specification: crate::activation_preview::ActivationPreviewSpecification,
        catalog: &crate::toolchain::CatalogSelection,
    ) -> Result<Self, CompileError> {
        use crate::activation_preview::ActivationPreviewOffer;
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
        if !display_context.matches_prototype(&prototype) {
            return Err(CompileError::ExtractFailed(
                "activation preview has another issued display input".into(),
            ));
        }
        let declarations = display_context.declarations().clone();
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
            display_context,
        };
        let original_interfaces = Value::Array(
            offer
                .display_context
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
            &offer.display_context,
            producer,
        )?;
        let exact = crate::declaration_context::ExactCompileContext::new(declarations)
            .prepare_compilation_with_private_input(
                &scratch.join("exact-scope"),
                producer,
                Some(authorization),
                private,
            )?
            .with_source_search_context(include);
        Ok(Self {
            selected,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(exact),

            checked_values: None,
            activation_preview: Some(offer),
        })
    }

    /// Read the compiler's distinct no-program result after a successful
    /// preview request. Missing native display evidence cannot become Opaque.
    pub fn activation_preview_unavailable(&self, root: &Path) -> Result<bool, CompileError> {
        match &self.activation_preview {
            Some(preview) => {
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
        self.checked_values.as_ref().map(|inputs| inputs.root())
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
        retain_compiler_failure_inner(directory, Some(stderr), error, Some(self), command)
    }

    /// Preserve the typed request and selected inputs when transport fails
    /// before compiler diagnostics can be returned. Evidence grants no authority.
    pub fn retain_execution_failure(
        &self,
        directory: &Path,
        command: &ExtractCmd,
        error: CompileError,
    ) -> CompileError {
        retain_compiler_failure_inner(directory, None, error, Some(self), command)
    }

    fn retain_checked_inputs(&self, destination: &Path) -> std::io::Result<()> {
        if let Some(inputs) = &self.checked_values {
            inputs.retain_diagnostics(destination)
        } else {
            Ok(())
        }
    }

    /// Apply all offer-owned fields, capture the complete selection, and freeze
    /// the physical request. Refusal diagnostics never claim worker execution.
    pub fn apply_to(
        self,
        mut command: ExtractCmd,
        preparation_directory: &Path,
    ) -> Result<AppliedModuleCandidateOffer, CompileError> {
        let mut timing =
            timing::InvocationTiming::start_scoped("products.offer.apply", "diagnostics_preparing");
        let capture = CompilerDiagnosticCapture::preparing(preparation_directory, &command);
        timing.advance("apply_exact_input");
        let result = (|| {
            if let Some(exact) = &self.exact {
                let retained_policy = if self.activation_preview.is_some() {
                    crate::declaration_context::RetainedGenerationPolicy::PureActivationPreview
                } else {
                    crate::declaration_context::RetainedGenerationPolicy::PreserveCertifiedDemand
                };
                exact.apply_to(&mut command, retained_policy)?;
            }
            timing.advance("select_session_value_owners");
            if let Some(manifest) = self.manifest_path() {
                command.module_candidates(manifest);
            }
            SelectedSessionValueOwners::from_command(&command).map_err(CompileError::from)
        })();
        match result {
            Ok(selected_session_values) => {
                timing.advance("diagnostics_prepared");
                capture.prepared(preparation_directory, &command);
                timing.finish(true);
                Ok(AppliedModuleCandidateOffer {
                    selected: SelectedModuleCandidateOffer {
                        offer: self,
                        selected_session_values,
                    },
                    command,
                    output_owner: None,
                })
            }
            Err(error) => {
                timing.advance("retain_refusal");
                capture.admission_refused(preparation_directory, &command, &error);
                Err(self.retain_execution_failure(preparation_directory, &command, error))
            }
        }
    }

    /// Place ordinary-turn outputs before the final request is frozen.
    pub fn prepare_admitted_turn(
        self,
        mut command: ExtractCmd,
        preparation_directory: &Path,
    ) -> Result<AppliedModuleCandidateOffer, CompileError> {
        let capture = CompilerDiagnosticCapture::preparing(preparation_directory, &command);
        let prepared = (|| -> Result<_, CompileError> {
            let request = tidepool_extract_cmd::ExtractRequest::decode(&command.request_bytes())
                .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
            if !request.is_turn()
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
            let directory = Arc::new(compiler_scratch_directory()?);
            command.relocate_turn_outputs(directory.path());
            Ok(directory)
        })();
        let directory = match prepared {
            Ok(directory) => directory,
            Err(error) => {
                capture.admission_refused(preparation_directory, &command, &error);
                return Err(self.retain_execution_failure(preparation_directory, &command, error));
            }
        };
        let mut applied = self.apply_to(command, preparation_directory)?;
        applied.output_owner = Some(directory);
        Ok(applied)
    }

    pub fn exact_scope_path(&self) -> Option<&Path> {
        self.exact
            .as_ref()
            .map(|request| request.manifest.as_path())
    }

    /// Attach this offer's exact compiler inputs and original native demand
    /// tags. Runtime live-value admission remains a separate authority.

    /// Validate successful source transactions under one actual source parent.
    /// A request using several source directories validates each directory.
    pub fn validate_exact_outputs(
        &self,
        source_directory: &Path,
    ) -> Result<Vec<crate::declaration_join::ExactSourceWitness>, CompileError> {
        Ok(self
            .admit_exact_outputs(source_directory)?
            .into_iter()
            .map(|source| source.into_witness())
            .collect())
    }

    pub(crate) fn admit_exact_outputs(
        &self,
        source_directory: &Path,
    ) -> Result<Vec<crate::declaration_context::ExactSourceAdmission>, CompileError> {
        let request = self.exact.as_ref().ok_or_else(|| {
            CompileError::ExtractFailed("source-only offer cannot admit exact outputs".into())
        })?;
        request.validate_outputs(source_directory)
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

impl SelectedModuleCandidateOffer {
    fn read_admitted_turn(
        &self,
        directory: &Path,
        issue_identity: bool,
        run: &tidepool_extract_cmd::ExtractRun,
        deployment: &crate::toolchain::AdmittedCompilerDeployment,
    ) -> Result<(Option<Arc<[u8]>>, Option<NativeTurnOutput>), CompileError> {
        let acquisition = invocation_output::extract_turn(run, deployment, directory)?;
        let turn = acquisition.turn_owned()?;
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
        let native = acquisition.complete()?;
        let requirements = crate::prepared_artifact::production_requirements()?;
        let target = Arc::new(tidepool_repr::execution_schema::parse_program(
            native.bundle.prepared(),
            &requirements,
            DecodeLimits::default(),
        )?);
        let program = crate::prepared_artifact::PreparedArtifact::bind_package_program_from_bytes(
            Arc::new(native.bundle.prepared().to_vec()),
            Arc::clone(&target),
            &requirements,
            DecodeLimits::default(),
        )?;
        let mut output = ProgramNativeOutput {
            origin: ProgramNativeOutputOrigin::Ordinary,
            directory: directory.to_path_buf(),
            target,
            program,
            turn: turn.clone(),
            metadata: native.bundle.metadata().into(),
            source: source.to_owned(),
            products: None,
        };
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
        output.products = seal_turn_outputs_with_validation(
            self,
            self.exact
                .as_ref()
                .map(|request| request.receipt_admission()),
            directory,
            &directory.join(format!("{module}.hs")),
            source,
            &output.program,
            "__prepared",
            issue_identity.then_some((&table, sites.as_slice())),
            OriginalOutputPublication::Transaction,
            (&native.native_output(), &native.receipt),
            &mut crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(Arc::clone(
                native.bundle.operation(),
            )),
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
}

#[derive(Clone, Debug)]
pub(crate) struct ProgramNativeOutput {
    origin: ProgramNativeOutputOrigin,
    directory: PathBuf,
    target: Arc<PreparedProgram>,
    program: crate::prepared_artifact::BoundPackageProgram,
    turn: Arc<[u8]>,
    metadata: Arc<[u8]>,
    products: Option<Arc<SealedTurnProducts>>,
    source: String,
}

#[derive(Clone)]
enum ProgramNativeOutputOrigin {
    Ordinary,
    World {
        program: Arc<crate::compiler_world::CheckedWorldCellProgram>,
        ordinal: usize,
    },
}

impl std::fmt::Debug for ProgramNativeOutputOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ordinary => formatter.write_str("Ordinary"),
            Self::World { ordinal, .. } => formatter
                .debug_struct("World")
                .field("ordinal", ordinal)
                .finish_non_exhaustive(),
        }
    }
}

impl ProgramNativeOutput {
    pub(crate) fn belongs_to_world_item(
        &self,
        program: &crate::compiler_world::CheckedWorldCellProgram,
        ordinal: usize,
    ) -> bool {
        matches!(&self.origin, ProgramNativeOutputOrigin::World { program: issuer, ordinal: issued }
            if std::ptr::eq(issuer.as_ref(), program) && *issued == ordinal)
    }
    pub(crate) fn sealed_products_shared(&self) -> Result<&Arc<SealedTurnProducts>, CompileError> {
        self.products
            .as_ref()
            .ok_or_else(|| CompileError::ExtractFailed("program output was not sealed".into()))
    }
    pub(crate) fn sealed_products(&self) -> Result<&SealedTurnProducts, CompileError> {
        self.products
            .as_deref()
            .ok_or_else(|| CompileError::ExtractFailed("program output was not sealed".into()))
    }
    pub(crate) fn turn_bytes(&self) -> &[u8] {
        &self.turn
    }
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }
    pub(crate) fn source(&self) -> &str {
        &self.source
    }
    pub(crate) fn target(&self) -> &Arc<PreparedProgram> {
        &self.target
    }
    pub(crate) fn decode_metadata(&self) -> Result<(DataConTable, MetaWarnings), CompileError> {
        let start = Instant::now();
        let (table, warnings) =
            tidepool_repr::serial::read_metadata_for_program(&self.metadata, &self.target)?;
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            "native.checked_metadata_decode",
            start.elapsed(),
            self.metadata.len() as u64,
        );
        Ok((table, warnings))
    }
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
    pub fn products(&self) -> Option<&Arc<SealedTurnProducts>> {
        self.output.products.as_ref()
    }
    pub fn source(&self) -> &str {
        &self.output.source
    }
}

#[derive(Debug)]
pub struct SealedTurnProducts {
    prepared: Arc<PreparedProgram>,
    original: SealedOriginalEvidence,
    target: crate::artifact_inventory::CertifiedTargetNativeInputs,
    pub original_compile_input: Option<Arc<SealedOriginalCompileInput>>,
    pub certified_groups: Arc<[certified_products::PendingCertifiedGroup]>,
    pub recovery_products: Arc<[crate::recovery_artifacts::CertifiedRecoveryProduct]>,
    pub(crate) retained_core_products: certified_products::CertifiedRetainedCoreProducts,
    pub package_interfaces: certified_products::CertifiedTargetPackageInterfaces,
    pub checked: Option<CheckedNativeProof>,
}

/// Exact inputs and their execution context share one output issuer. Later
/// program items cannot enlarge this output's original instance environment.
#[derive(Debug)]
enum SealedOriginalEvidence {
    Exact {
        output: crate::declaration_context::IssuedOriginalOutput,
        execution: Arc<crate::declaration_context::OriginalExecutionEvidence>,
    },
    Ordinary {
        execution: Arc<crate::declaration_context::OriginalExecutionEvidence>,
        artifacts: crate::artifact_inventory::ArtifactView,
        selection: certified_products::CertifiedSourceSelection,
    },
    Absent {
        artifacts: crate::artifact_inventory::ArtifactView,
        selection: certified_products::CertifiedSourceSelection,
    },
}

impl SealedOriginalEvidence {
    fn issue_exact(
        admission: &crate::declaration_context::ExactProductAdmission<'_>,
        issued: &AdmittedNativeTargetProducts,
    ) -> Result<Self, CompileError> {
        let execution = admission.issue_original_execution(issued)?;
        Ok(Self::Exact {
            output: issued.output.clone(),
            execution,
        })
    }

    fn execution(&self) -> Option<&Arc<crate::declaration_context::OriginalExecutionEvidence>> {
        match self {
            Self::Exact { execution, .. } | Self::Ordinary { execution, .. } => Some(execution),
            Self::Absent { .. } => None,
        }
    }
}

/// One requested group's membership in its actual physical output. This
/// observes native bytes; only the runtime producer can pair its source issuer.
pub struct PhysicalCurrentNativeGroup<'a> {
    group: &'a certified_products::PendingCertifiedGroup,
}

impl PhysicalCurrentNativeGroup<'_> {
    pub fn sites(&self) -> impl Iterator<Item = u64> + '_ {
        self.group
            .group()
            .definitions()
            .sites()
            .iter()
            .map(|site| site.site)
    }
}

/// Read-only sites of the physical target completed by this output issuer.
pub struct PhysicalCurrentNativeTarget<'a> {
    prepared: &'a PreparedProgram,
}

impl PhysicalCurrentNativeTarget<'_> {
    pub(crate) fn definitions(&self) -> tidepool_repr::execution_schema::DefinitionsView<'_> {
        self.prepared.definitions()
    }

    pub fn sites(&self) -> impl Iterator<Item = u64> + '_ {
        self.prepared.sites().iter().map(|site| site.site)
    }
}

/// Borrowed rows from one exact target resolution and its physical output.
/// A row cannot be supplied independently of that operation's selected issuer.
pub struct ResolvedNativeSourceProducts<'a> {
    _resolved: crate::artifact_inventory::ResolvedNativeSourceTarget<'a>,
    prepared: &'a PreparedProgram,
    rows: Vec<ResolvedNativeSourceProduct<'a>>,
}

/// Compiler-only source membership, coissued with the exact chosen binding.
pub struct ResolvedNativeSourceProduct<'a> {
    group: &'a Arc<certified_products::PendingCertifiedGroup>,
    binding: crate::artifact_inventory::NativeGroupBinding,
    source_association: crate::artifact_inventory::ResolvedSourceAssociation,
    imports: &'a Arc<[certified_products::SelectedNativeImport]>,
}

impl<'a> ResolvedNativeSourceProduct<'a> {
    pub fn group(&self) -> &'a certified_products::PendingCertifiedGroup {
        self.group
    }
    pub(crate) fn shared_group(&self) -> &'a Arc<certified_products::PendingCertifiedGroup> {
        self.group
    }
    pub fn imports(&self) -> &'a [certified_products::SelectedNativeImport] {
        self.imports
    }

    pub(crate) fn source_association(
        &self,
    ) -> crate::artifact_inventory::ResolvedSourceAssociation {
        self.source_association
    }

    pub fn binding(&self) -> crate::artifact_inventory::NativeGroupBinding {
        self.binding
    }
    pub fn physical_current(&self) -> Option<PhysicalCurrentNativeGroup<'a>> {
        matches!(
            self.source_association,
            crate::artifact_inventory::ResolvedSourceAssociation::Current
                | crate::artifact_inventory::ResolvedSourceAssociation::PriorProgram
        )
        .then_some(PhysicalCurrentNativeGroup { group: self.group })
    }
}

impl<'a> ResolvedNativeSourceProducts<'a> {
    pub(crate) fn entry_binding(&self) -> Option<crate::artifact_inventory::NativeGroupBinding> {
        self._resolved.entry_binding()
    }

    pub fn linked_native_target(
        &self,
    ) -> Option<&'a Arc<crate::linked_native::LinkedNativeTarget>> {
        self._resolved.linked_native_target()
    }

    pub(crate) fn acquired_artifacts(&self) -> &crate::artifact_inventory::ArtifactView {
        self._resolved.artifacts()
    }

    /// All ordered target slots, including ordinary live capture requirements.
    pub fn target_requirements(&self) -> &'a [certified_products::SelectedNativeTargetImport] {
        self._resolved.target_requirements()
    }

    pub fn target(&self) -> PhysicalCurrentNativeTarget<'a> {
        PhysicalCurrentNativeTarget {
            prepared: self.prepared,
        }
    }

    pub fn groups(&self) -> impl Iterator<Item = &ResolvedNativeSourceProduct<'a>> {
        self.rows.iter()
    }

    pub(crate) fn acquired_homes(
        &self,
    ) -> Result<
        BTreeMap<
            crate::artifact_inventory::NativeGroupBinding,
            crate::certified_products::AcquiredHomeCodeGrant,
        >,
        CompileError,
    > {
        self._resolved.acquired_homes()
    }

    pub fn validate_source_target(
        &self,
        selection: &Arc<crate::linked_native::LinkedNativeTarget>,
    ) -> Result<(), CompileError> {
        let own = self.linked_native_target().ok_or_else(|| {
            CompileError::ExtractFailed("native target lacks its completed source selection".into())
        })?;
        if !Arc::ptr_eq(own, selection)
            || !std::ptr::eq(self.prepared, selection.prepared().as_ref())
        {
            return Err(CompileError::ExtractFailed(
                "native source receipt belongs to another prepared target".into(),
            ));
        }
        Ok(())
    }

    /// Consume this target's source receipt without reconstructing membership
    /// from code custody or another target's equal linkage completion.
    pub fn physical_current_for<'s>(
        &self,
        selection: &'s Arc<crate::linked_native::LinkedNativeTarget>,
        selected: &crate::linked_native::AcquiredNativeCode,
    ) -> Result<Option<PhysicalCurrentNativeGroup<'s>>, CompileError> {
        self.validate_source_target(selection)?;
        if !selection.owns_acquired(selected) {
            return Err(CompileError::ExtractFailed(
                "native source receipt belongs to another receiving target".into(),
            ));
        }
        let crate::linked_native::NativeCodeGrant::Home {
            binding, product, ..
        } = selected.grant()
        else {
            return Err(CompileError::ExtractFailed(
                "native source receipt is not HOME code".into(),
            ));
        };
        match selection.sources.get(&binding) {
            Some(
                crate::linked_native::TargetHomeSource::CurrentStage(group)
                | crate::linked_native::TargetHomeSource::PriorProgramStage(group),
            ) => {
                if !group.same_issuance(product) {
                    return Err(CompileError::ExtractFailed(
                        "native source receipt names another physical row".into(),
                    ));
                }
                Ok(Some(PhysicalCurrentNativeGroup { group }))
            }
            Some(crate::linked_native::TargetHomeSource::HistoricalInput) => Ok(None),
            None => Err(CompileError::ExtractFailed(
                "selected HOME occurrence lacks target source classification".into(),
            )),
        }
    }

    pub(crate) fn issue(
        resolved: crate::artifact_inventory::ResolvedNativeSourceTarget<'a>,
        prepared: &'a PreparedProgram,
    ) -> Result<Self, CompileError> {
        let mut rows = Vec::with_capacity(resolved.groups().len());
        for binding in resolved.groups().iter().copied() {
            let group = resolved.shared_group(binding)?;
            let imports = resolved.shared_group_imports(binding)?;
            let source_association = resolved.source_association(binding)?;
            rows.push(ResolvedNativeSourceProduct {
                group,
                binding,
                source_association,
                imports,
            });
        }
        Ok(Self {
            _resolved: resolved,
            prepared,
            rows,
        })
    }
}

impl CompiledArtifacts {
    /// Resolve only the target issued in this bundle, preserving its exact roots.
    /// Raw decoded programs have no selecting owner and return no source proof.
    pub fn native_source_products(
        &self,
        name: &str,
    ) -> Result<Option<ResolvedNativeSourceProducts<'_>>, CompileError> {
        let target = self
            .targets
            .get(name)
            .ok_or_else(|| CompileError::ExtractFailed("native source target is absent".into()))?;
        target.native_source_products()
    }
}

impl TargetArtifact {
    /// Retain the prepared owner paired with this target's certification.
    pub fn prepared_owner(&self) -> &Arc<tidepool_repr::execution_schema::PreparedProgram> {
        self.prepared.prepared_shared()
    }

    /// Borrow the completion issued for this exact physical target. It is
    /// separate from broader Original availability and retained byte custody.
    pub fn linked_native_target(&self) -> Option<&Arc<crate::linked_native::LinkedNativeTarget>> {
        self.native_inputs.as_ref()?.linked_native_target()
    }

    pub fn native_source_products(
        &self,
    ) -> Result<Option<ResolvedNativeSourceProducts<'_>>, CompileError> {
        let Some(inputs) = &self.native_inputs else {
            return Ok(None);
        };
        ResolvedNativeSourceProducts::issue(inputs.resolve_sources()?, self.prepared.prepared())
            .map(Some)
    }
}

impl SealedTurnProducts {
    /// Borrow this output's already issued target links without resolving them again.
    pub fn linked_native_target(&self) -> Option<&Arc<crate::linked_native::LinkedNativeTarget>> {
        self.target.linked_native_target()
    }

    pub(crate) fn target_native_inputs(
        &self,
    ) -> &crate::artifact_inventory::CertifiedTargetNativeInputs {
        &self.target
    }

    /// The execution context belongs to this completed output, including its
    /// issued native custody independently of the original compilation inputs.
    /// A cell's cumulative continuation belongs to `CellProgramItem::compiler_context`;
    /// it may also retain targets completed by earlier items.
    pub fn original_execution_context(
        &self,
    ) -> Result<Arc<crate::declaration_context::ExactDeclarationContext>, CompileError> {
        self.original_execution()
            .map(|evidence| Arc::clone(evidence.context()))
    }

    pub fn original_execution_evidence(
        &self,
    ) -> Result<Arc<crate::declaration_context::OriginalExecutionEvidence>, CompileError> {
        self.original_execution().map(Arc::clone)
    }

    pub fn published_source_original_selection(
        &self,
        revision: &str,
        public_root: &crate::declaration_join::ExactModuleIdentity,
    ) -> Result<Arc<crate::declaration_context::PublishedSourceOriginalSelection>, CompileError>
    {
        let original = self.original_compile_input.as_ref().ok_or_else(|| {
            CompileError::ExtractFailed(
                "published source output lacks original input authority".into(),
            )
        })?;
        self.original_execution()?
            .completed()?
            .issue_published_source_original(revision, original, public_root)
    }

    pub fn native_source_products(&self) -> Result<ResolvedNativeSourceProducts<'_>, CompileError> {
        let resolved = self.target.resolve_sources()?;
        resolved.ensure_original_execution_custody(self.original_execution()?.context())?;
        ResolvedNativeSourceProducts::issue(resolved, &self.prepared)
    }

    pub fn artifact_view(&self) -> &crate::artifact_inventory::ArtifactView {
        match &self.original {
            SealedOriginalEvidence::Exact { output, .. } => output.artifacts(),
            SealedOriginalEvidence::Ordinary { artifacts, .. }
            | SealedOriginalEvidence::Absent { artifacts, .. } => artifacts,
        }
    }

    pub(crate) fn source_selection(&self) -> &certified_products::CertifiedSourceSelection {
        match &self.original {
            SealedOriginalEvidence::Exact { output, .. } => output.source_selection(),
            SealedOriginalEvidence::Ordinary { selection, .. }
            | SealedOriginalEvidence::Absent { selection, .. } => selection,
        }
    }

    pub(crate) fn planned_original_output(
        &self,
    ) -> Result<&crate::declaration_context::IssuedOriginalOutput, CompileError> {
        match &self.original {
            SealedOriginalEvidence::Exact { output, .. } => Ok(output),
            _ => Err(CompileError::ExtractFailed(
                "planned output lacks exact original issuance".into(),
            )),
        }
    }

    pub fn pending_imports(&self) -> &[certified_products::PendingImportOwner] {
        self.target.imports()
    }
    pub(crate) fn typed_entry(&self) -> Option<&crate::checked_cell::CheckedTypedEntry> {
        self.target.entry()
    }
    pub(crate) fn original_execution(
        &self,
    ) -> Result<&Arc<crate::declaration_context::OriginalExecutionEvidence>, CompileError> {
        self.original.execution().ok_or_else(|| {
            CompileError::ExtractFailed("program output lacks original execution evidence".into())
        })
    }
    pub(crate) fn resolve_native_execution(
        &self,
    ) -> Result<crate::artifact_inventory::ResolvedNativeTarget<'_>, CompileError> {
        let mut timing = timing::InvocationTiming::start_scoped(
            "products.native_execution.resolve",
            "full_target_resolution",
        );
        let resolved = self.target.resolve()?;
        timing.advance("original_execution_custody");
        resolved.ensure_original_execution_custody(self.original_execution()?.context())?;
        timing.finish(true);
        Ok(resolved)
    }
    fn original_compiler_inputs(
        &self,
    ) -> Result<&crate::declaration_context::OriginalCompilerInputs, CompileError> {
        match &self.original {
            SealedOriginalEvidence::Exact { output, .. } => Ok(output.compiler_inputs()),
            _ => Err(CompileError::ExtractFailed(
                "program output lacks exact compiler inputs".into(),
            )),
        }
    }
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
    directory: Arc<TempDir>,
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

    fn read_observed(
        self,
        root: &Path,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Vec<u8>, CompileError> {
        let path = root.join(self.filename());
        let bytes = self.read(root, &validation.inventory)?;
        validation
            .record_captured_bytes(&path, &bytes)
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        Ok(bytes)
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
#[tracing::instrument(
    target = "exomonad_harness::timing",
    name = "products.seal",
    level = "debug",
    skip_all,
    fields(inclusive = true)
)]
fn seal_turn_outputs_with_validation(
    offer: &SelectedModuleCandidateOffer,
    request_admission: Option<crate::declaration_context::ExactReceiptAdmission<'_>>,
    output_dir: &Path,
    source_path: &Path,
    source: &str,
    program: &crate::prepared_artifact::BoundPackageProgram,
    target: &str,
    identity_metadata: Option<(&DataConTable, &[YieldSite])>,
    publication: OriginalOutputPublication,
    native_output: (
        &invocation_output::CapturedNativeOutput<'_>,
        &certified_products::CertifiedReceipt,
    ),
    validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<Option<SealedTurnProducts>, CompileError> {
    let prepared = program.prepared();
    if std::fs::read_to_string(source_path)? != source {
        return Err(CompileError::ExtractFailed(
            "turn source changed after worker compile".into(),
        ));
    }
    let product_bytes = CompilerSidecar::ModuleProducts.read_observed(output_dir, validation)?;
    let packages = CompilerSidecar::ModulePackageImports.read_observed(output_dir, validation)?;
    let evidence_bytes = CompilerSidecar::Dependencies.read_observed(output_dir, validation)?;
    timing::record_inventory_work("products.decode.before", output_dir, &validation.inventory);
    let start = Instant::now();
    let ordinary_products = certified_products::ParsedModuleProducts::decode_with_operation(
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
    let inventory = validation.inventory.clone();
    let exact_source_owned = request_admission
        .map(|request| {
            request.admit_source_receipt_with_validation(source_path, source, validation)
        })
        .transpose()?;
    let source_admission = match request_admission.zip(exact_source_owned.as_ref()) {
        Some((request, admitted)) => SourceEvidenceAdmission::from_exact(request.match_product(
            admitted,
            source_path,
            source,
            &evidence_bytes,
            &inventory,
        )?),
        None => admit_compiler_source(&inventory, &evidence_bytes, source_path, source)?,
    };
    let exact = source_admission.exact();
    let exact_source = exact.map(|admission| admission.source());
    let evidence = source_admission.completed();
    let fresh_products = &ordinary_products;
    let needs_certificate = offer.has_candidates()
        || offer.exact.is_some()
        || !fresh_products.products().is_empty()
        || !prepared.globals().is_empty()
        || has_ready_home_module(evidence);
    let (physical_native_output, ordinary_receipt) = native_output;
    let captured_package_interfaces =
        certified_products::PackageInterfaceCustody::from_native_output(
            physical_native_output,
            ordinary_receipt,
            validation,
        )
        .map_err(compiler_evidence_failure)?;
    let package_products = certify_native_package_products(
        physical_native_output,
        ordinary_receipt,
        &captured_package_interfaces,
        validation,
    )?;
    let Some(valid) = evidence.revalidate(source).is_ok().then_some(evidence) else {
        if needs_certificate
            || ordinary_receipt
                .modules
                .iter()
                .any(|module| module.origin == certified_products::ProductOrigin::Cached)
        {
            return Err(CompileError::ExtractFailed(
                "turn required product certificate lacks valid final dependency evidence".into(),
            ));
        }
        return Ok(None);
    };
    timing::record_inventory_work("products.certify.before", output_dir, &validation.inventory);
    let products = certified_products::certify_products_with_validation(
        offer.selected.as_deref(),
        ordinary_receipt,
        fresh_products,
        &source_admission,
        output_dir,
        &offer.producer,
        &offer.include,
        None,
        &offer.selected_session_values,
        None,
        &captured_package_interfaces,
        validation,
    )
    .map_err(compiler_evidence_failure)?;
    let certified = crate::declaration_context::issue_certified_product_native_availability(
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&offer.producer)
            .sha256(),
        products,
        request_admission.map(|request| request.compiler_inputs().artifacts()),
        validation,
    )?;
    timing::record_inventory_work("products.certify.after", output_dir, &validation.inventory);
    ensure_ready_module_inventory(&ordinary_receipt.modules, valid)?;
    if ordinary_receipt.targets.len() != 1 {
        return Err(CompileError::ExtractFailed(
            "turn target product receipt count".into(),
        ));
    }
    ordinary_receipt
        .targets
        .get(target)
        .ok_or_else(|| CompileError::ExtractFailed("turn target product receipt missing".into()))?;
    let package_catalog = package_availability_with_validation(
        &ordinary_receipt.packages,
        certified.data(),
        validation,
    )?;
    let inherited_inputs = request_admission.map(|request| request.compiler_inputs());
    let no_inherited_inputs = crate::artifact_inventory::CompilerSourceSupport::empty();
    let inherited_support = inherited_inputs
        .as_ref()
        .map(|inputs| inputs.compiler_source_support())
        .unwrap_or(&no_inherited_inputs);
    let package_selection =
        physical_native_output.ordinary_package_selection(target, program, certified.data())?;
    let admitted = admit_native_target_products(
        physical_native_output,
        &certified,
        &package_products,
        package_selection,
        inherited_support,
        &package_catalog,
        None,
        None,
        request_admission.map(|request| request.certified_groups()),
        None,
        validation,
    )?;
    let target_native = &admitted.target;
    let artifact_view = target_native.artifacts();
    let certified_groups = &admitted.certified_groups;
    let package_interfaces = &admitted.package_interfaces;
    let post_target_span = tracing::debug_span!(target: "exomonad_harness::timing", "products.post_target", inclusive = true).entered();
    let original_compile_input = if let Some((table, sites)) =
        identity_metadata.filter(|_| offer.exact.is_none())
    {
        let input_path = output_dir.join("compiler-inputs.cbor");
        let input_bytes = crate::compile_input::ValidatedInputPackages::read_bytes(&input_path)?;
        validation
            .record_captured_bytes(&input_path, &input_bytes)
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        let input_packages = crate::compile_input::ValidatedInputPackages::decode_supported(
            &input_bytes,
            &source_admission,
        )?;
        input_packages
            .map(|input_packages| {
                crate::compile_input::seal(
                    &offer.producer,
                    &offer.include,
                    &source_admission,
                    &input_packages,
                    source,
                    target,
                    prepared,
                    &certified_groups,
                    target_native.imports(),
                    &package_interfaces,
                    table.clone(),
                    sites.to_vec(),
                    target_native.resolve()?.native_sites()?,
                    &admitted,
                )
            })
            .transpose()?
            .flatten()
            .map(Arc::new)
    } else {
        None
    };
    let original_execution_span = tracing::debug_span!(target: "exomonad_harness::timing", "products.original_execution", inclusive = true).entered();
    let original = match exact.as_ref() {
        Some(admission) => SealedOriginalEvidence::issue_exact(admission, &admitted)?,
        None => match original_compile_input.as_ref() {
            Some(proof) => {
                let execution = proof.issued_original_execution();
                SealedOriginalEvidence::Ordinary {
                    execution,
                    artifacts: artifact_view.clone(),
                    selection: certified.source_selection().clone(),
                }
            }
            None => SealedOriginalEvidence::Absent {
                artifacts: artifact_view.clone(),
                selection: certified.source_selection().clone(),
            },
        },
    };
    drop(original_execution_span);
    let checked_span = tracing::debug_span!(target: "exomonad_harness::timing", "products.checked_proof", inclusive = true).entered();
    let checked = if let Some(preview) = &offer.activation_preview {
        validate_activation_preview_output_context(
            offer,
            &artifact_view,
            exact_source
                .as_ref()
                .expect("checked output has exact source"),
            None,
            &certified.source_selection(),
        )?;
        Some(CheckedNativeProof::ActivationPreview(
            preview.seal(
                output_dir,
                &offer
                    .exact
                    .as_ref()
                    .expect("preview has exact scope")
                    .request_sha256,
                source,
                prepared,
            )?,
        ))
    } else {
        None
    };
    drop(checked_span);
    drop(post_target_span);
    let _publication_span = tracing::debug_span!(target: "exomonad_harness::timing", "products.publication", inclusive = true).entered();
    if publication == OriginalOutputPublication::Transaction
        && request_admission.is_none_or(|admission| empty_exact_context(admission.context()))
    {
        let version_origin = exact.map_or(
            module_candidates::CandidateVersionOrigin::Ordinary,
            |admission| module_candidates::CandidateVersionOrigin::Exact {
                semantic_sha256: admission.product_context_digest(),
            },
        );
        let publication_products = ordinary_products;
        let (_, publication) = module_candidates::prepare_publication(
            &offer.producer,
            &offer.include,
            valid,
            publication_products,
            source,
            version_origin,
            &certified.data().recovery_products,
        );
        module_candidates::publish_prepared(publication)?;
    } else if publication == OriginalOutputPublication::Transaction {
        module_candidates::record_exact_context_publication_skip(fresh_products.products());
    }
    if publication == OriginalOutputPublication::Transaction {
        module_candidates::record_deployment_acceptance(
            offer.selected.as_deref(),
            &ordinary_receipt,
        );
    }
    Ok(Some(SealedTurnProducts {
        prepared: prepared.clone(),
        original,
        target: admitted.target,
        original_compile_input,
        checked,
        certified_groups: admitted.certified_groups,
        recovery_products: certified.data().recovery_products.clone(),
        retained_core_products: certified.data().retained_core_products.clone(),
        package_interfaces: admitted.package_interfaces,
    }))
}

fn certify_native_package_products(
    output: &invocation_output::CapturedNativeOutput<'_>,
    receipt: &certified_products::CertifiedReceipt,
    interfaces: &certified_products::PackageInterfaceCustody,
    validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<Vec<Arc<certified_products::CertifiedPackageCodeProduct>>, CompileError> {
    output.validate_interfaces(receipt, validation)?;
    validation
        .inventory
        .reserve::<Arc<certified_products::CertifiedPackageCodeProduct>>(
            receipt.package_products.len(),
        )
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    receipt
        .package_products
        .iter()
        .map(|product| {
            certified_products::CertifiedPackageCodeProduct::from_native_output(
                output,
                Path::new(""),
                product,
                receipt,
                interfaces,
                validation,
            )
            .map_err(compiler_evidence_failure)
        })
        .collect()
}

/// The sole native target issuer pairs its actual output projection and target.
/// Consumers cannot assemble or rebind these fields independently.
pub(crate) struct AdmittedNativeTargetProducts {
    output: crate::declaration_context::IssuedOriginalOutput,
    target: crate::artifact_inventory::CertifiedTargetNativeInputs,
    certified_groups: Arc<[certified_products::PendingCertifiedGroup]>,
    package_interfaces: certified_products::CertifiedTargetPackageInterfaces,
}

impl AdmittedNativeTargetProducts {
    pub(crate) fn output(&self) -> &crate::declaration_context::IssuedOriginalOutput {
        &self.output
    }

    pub(crate) fn target(&self) -> &crate::artifact_inventory::CertifiedTargetNativeInputs {
        &self.target
    }
}

/// The native target issuer shared by ordinary and held-World outputs.
/// Its inputs have already passed their owning source and product admission.
#[allow(clippy::too_many_arguments)]
fn admit_native_target_products(
    output: &invocation_output::CapturedNativeOutput<'_>,
    certified: &certified_products::CapturedProducts,
    packages: &[Arc<certified_products::CertifiedPackageCodeProduct>],
    package_selection: certified_products::BoundPackageSelection,
    inherited: &crate::artifact_inventory::CompilerSourceSupport,
    package_catalog: &PackageInterfaceCatalog<'_>,
    segment_originals: Option<&certified_products::CertifiedSegmentOriginals>,
    typed_entry: Option<crate::checked_cell::CheckedTypedEntry>,
    inherited_groups: Option<&crate::declaration_context::CertifiedArtifactGroupBindings>,
    segment_cursor: Option<SegmentItemDemand<'_>>,
    validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<AdmittedNativeTargetProducts, CompileError> {
    let prepared = package_selection.prepared_program();
    let physical = certified.physical_cohort();
    let target_admission_start = Instant::now();
    let observed = package_selection.authenticated_imports().clone();
    let selected_target = match segment_originals {
        Some(originals) => {
            let cursor = segment_cursor.as_ref().ok_or_else(|| {
                CompileError::ExtractFailed("segment item lacks its demand cursor".into())
            })?;
            let cohort = certified.physical_cohort();
            let physical = cohort.artifacts();
            let values = originals
                .selected_value_entries(&certified.data().value_interfaces)
                .cloned()
                .collect::<Vec<_>>();
            physical.inventory().select_native_target(
                cohort,
                observed,
                typed_entry,
                Some(cursor.view),
                &values,
                package_catalog,
                validation,
            )?
        }
        None => {
            let physical = certified.physical_cohort();
            physical.artifacts().inventory().select_native_target(
                physical,
                observed,
                typed_entry,
                None,
                &[],
                package_catalog,
                validation,
            )?
        }
    };
    let baseline = segment_cursor
        .as_ref()
        .map(|cursor| cursor.groups)
        .or_else(|| {
            inherited_groups.map(crate::declaration_context::CertifiedNativeGroupBaseline::Target)
        });
    let selected_groups = Arc::new(physical.certify_target_groups(
        &selected_target,
        &certified.data().groups,
        baseline,
        validation,
    )?);
    let package_closure = package_catalog.select(
        selected_groups.groups(),
        selected_target.imports(),
        &validation.inventory,
    )?;
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
    let issued_output = if segment_originals.is_some() {
        crate::declaration_context::IssuedOriginalOutput::from_segment_item(certified)?
    } else {
        certified.captured_stage().output().clone()
    };
    Ok(AdmittedNativeTargetProducts {
        output: issued_output,
        target: selected_target
            .with_certified_groups(Arc::clone(&selected_groups))?
            .link_native_target(
                output,
                packages,
                package_selection,
                inherited,
                &validation.inventory,
            )?,
        certified_groups: Arc::clone(selected_groups.groups()),
        package_interfaces,
    })
}

/// Preserve all compiler-issued support carriers while excluding the exact
/// generated scaffold authenticated by the consumed source receipt.
fn program_support_artifacts(
    artifacts: &crate::artifact_inventory::ArtifactView,
    generated: &crate::declaration_join::ExactModuleIdentity,
) -> Result<crate::artifact_inventory::ArtifactView, CompileError> {
    let support = artifacts.support_without_owner(generated)?;
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

fn validate_activation_preview_output_context(
    offer: &SelectedModuleCandidateOffer,
    artifacts: &crate::artifact_inventory::ArtifactView,
    source_admission: &crate::declaration_context::ExactSourceAdmission,
    produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
    selection: &certified_products::CertifiedSourceSelection,
) -> Result<(), CompileError> {
    let exact = offer.exact.as_ref().ok_or_else(|| {
        CompileError::ExtractFailed("checked output lacks current exact context".into())
    })?;
    let generated = source_admission.generated_source_owner();
    let support = program_support_artifacts(artifacts, &generated)?;
    let mut request = exact.program_admission();
    request.admit_program_support_with_selection(
        exact.context().clone(),
        &support,
        std::slice::from_ref(source_admission),
        produced_types,
        selection,
    )?;
    Ok(())
}

struct PackageInterfaceCatalog<'a> {
    current: std::borrow::Cow<
        'a,
        BTreeMap<(String, String), certified_products::PackageInterfaceWitness>,
    >,
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
        for (owner, witness) in self.current.iter() {
            certified_products::charge_package_copy(owner, witness, operation)
                .map_err(compiler_evidence_failure)?;
        }
        let mut selected = self.current.as_ref().clone();
        for (owner, digest) in demanded {
            let witness = self
                .current
                .get(&owner)
                .or_else(|| self.interfaces.get(&owner))
                .filter(|witness| witness.sha256 == digest)
                .ok_or_else(|| {
                    tracing::debug!(
                        target: "tidepool::artifacts::package_selection",
                        ?owner,
                        requested_sha256 = ?digest,
                        current_sha256 = ?self.current.get(&owner).map(|witness| witness.sha256),
                        inherited_sha256 = ?self.interfaces.get(&owner).map(|witness| witness.sha256),
                        current_path = ?self.current.get(&owner).map(|witness| &witness.selected_path),
                        inherited_path = ?self.interfaces.get(&owner).map(|witness| &witness.selected_path),
                        demanding_groups = ?groups.iter().filter(|group| group.imports().iter().any(|import|
                            matches!(import, certified_products::PendingImportOwner::Package { unit, module, interface_digest, .. }
                                if (&owner.0, &owner.1, &digest) == (unit, module, interface_digest))))
                            .map(|group| (group.owner(), group.group().original_ordinal(), group.origin()))
                            .collect::<Vec<_>>(),
                        target_demand = target_imports.iter().any(|import|
                            matches!(import, certified_products::PendingImportOwner::Package { unit, module, interface_digest, .. }
                                if (&owner.0, &owner.1, &digest) == (unit, module, interface_digest))),
                        "selected target package interface unavailable",
                    );
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
    PackageInterfaceCatalog {
        current: std::borrow::Cow::Borrowed(additions),
        interfaces: std::borrow::Cow::Owned(originals.package_witnesses()),
    }
}

fn package_availability_with_validation<'a>(
    packages: &'a BTreeMap<(String, String), certified_products::PackageInterfaceWitness>,
    certified: &certified_products::CertifiedProducts,
    validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<PackageInterfaceCatalog<'a>, CompileError> {
    let reconciled = certified_products::reconcile_ordinary_package_interfaces(
        &certified.recovery_products,
        packages,
        validation,
    )
    .map_err(compiler_evidence_failure)?;
    let (mut interfaces, decisions) = reconciled.into_parts();
    let mut current = BTreeMap::new();
    for decision in &decisions {
        interfaces.insert(decision.owner().clone(), decision.selected().clone());
        if decision.is_new() {
            current.insert(decision.owner().clone(), decision.selected().clone());
        }
    }
    Ok(PackageInterfaceCatalog {
        current: std::borrow::Cow::Owned(current),
        interfaces: std::borrow::Cow::Owned(interfaces),
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
    .and_then(CompilationOutput::into_artifacts)
}

/// Compile fresh source against immutable declaration owners through the same
/// artifact front door. Ordinary source candidates are unavailable; successful
/// products require context-bound compiler receipts.
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
    .and_then(CompilationOutput::into_artifacts)
}

/// Compile a declaration probe in full-home-product mode, which produces
/// original products for every home module, including modules with no
/// executable references from the probe. The probe bypasses candidate
/// publication while retaining the shared certification front door.
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
    .and_then(CompilationOutput::into_artifacts)
}

/// Build an explicitly selected immutable source cohort at its final deployment
/// path. The declared probe and ordered targets run under build-action isolation;
/// only authenticated originals are exported. Schema 5 retains original source
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
    Inspection(PathBuf),
    Original {
        preparation: production_entry::EntryPreparation,
        diagnostics: OriginalFailureDiagnostics,
    },
}

#[derive(Clone, Copy)]
enum OriginalFailureDiagnostics {
    Hosted,
    BuildAction,
}

impl CompilationOutputOwner {
    fn path(&self) -> &Path {
        match self {
            Self::Scratch(directory) => directory.path(),
            Self::Inspection(directory) => directory,
            Self::Original { preparation, .. } => preparation.raw(),
        }
    }

    fn retain_failure(
        &self,
        command: &ExtractCmd,
        stderr: &[u8],
        error: CompileError,
    ) -> CompileError {
        let metadata_failure = if !matches!(self, Self::Scratch(_))
            && std::env::var("TIDEPOOL_KEEP_TEST_LOGS").as_deref() == Ok("1")
        {
            retain_compiler_failure_metadata(self.path(), command, stderr).err()
        } else {
            None
        };
        match self {
            Self::Scratch(directory) => {
                retain_compiler_failure(directory.path(), command, stderr, error)
            }
            Self::Original {
                diagnostics: OriginalFailureDiagnostics::Hosted,
                ..
            } => {
                if let Some(failure) = metadata_failure {
                    tracing::warn!(%failure, path = %self.path().display(), "compiler diagnostic metadata is incomplete");
                }
                tracing::warn!(path = %self.path().display(), "retained failed compiler artifacts");
                error
            }
            Self::Inspection(_)
            | Self::Original {
                diagnostics: OriginalFailureDiagnostics::BuildAction,
                ..
            } => {
                // Build CLIs have no tracing subscriber. A diagnostic write
                // cannot replace the typed primary failure.
                use std::io::Write;
                if let Some(failure) = metadata_failure {
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "compiler diagnostic metadata is incomplete: {failure}"
                    );
                }
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "compiler artifacts retained at {}",
                    self.path().display()
                );
                error
            }
        }
    }

    fn begin_execution(&mut self) -> Option<production_entry::EntrySubmission> {
        match self {
            Self::Original { preparation, .. } => Some(preparation.begin_execution()),
            Self::Scratch(_) | Self::Inspection(_) => None,
        }
    }

    fn endpoint_failure(
        &mut self,
        error: &tidepool_extract_cmd::SpawnError,
        previous: Option<production_entry::EntrySubmission>,
    ) {
        if let Self::Original { preparation, .. } = self {
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
/// direct configured compiler and cannot read or publish runtime module
/// candidates, deployment catalogs or mutable runtime build products.
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

pub(crate) fn charge_source_evidence(
    operation: &tidepool_repr::execution_schema::InventoryOperation,
    bytes: usize,
) -> Result<(), CompileError> {
    operation
        .charge(bytes.checked_mul(64).ok_or_else(|| {
            CompileError::ExtractFailed("source evidence accounting overflow".into())
        })?)
        .map_err(|error| compiler_evidence_failure(error.into()))
}

/// One captured worker dependency observation and its authenticated source facts.
/// Borrowing the raw input keeps digest authentication tied to the same parse.
/// Constructors belong to the physical source-admission pipeline.
pub(crate) struct SourceEvidenceAdmission<'a> {
    worker_bytes: &'a [u8],
    input_path: &'a Path,
    target_source: &'a str,
    inventory: &'a tidepool_repr::execution_schema::InventoryOperation,
    completion: SourceEvidenceCompletion<'a>,
}

enum SourceEvidenceCompletion<'a> {
    Ordinary(Arc<cache::CompletedSourceEvidence>),
    Exact(crate::declaration_context::ExactProductAdmission<'a>),
}

impl<'a> SourceEvidenceAdmission<'a> {
    pub(crate) fn worker_bytes(&self) -> &[u8] {
        self.worker_bytes
    }

    pub(crate) fn input_path(&self) -> &Path {
        self.input_path
    }

    pub(crate) fn target_source(&self) -> &str {
        self.target_source
    }

    pub(crate) fn inventory(&self) -> &tidepool_repr::execution_schema::InventoryOperation {
        self.inventory
    }

    pub(crate) fn completed(&self) -> &cache::CompletedSourceEvidence {
        match &self.completion {
            SourceEvidenceCompletion::Ordinary(completed) => completed,
            SourceEvidenceCompletion::Exact(exact) => exact.source().evidence().as_ref(),
        }
    }

    pub(crate) fn shared_completed(&self) -> Arc<cache::CompletedSourceEvidence> {
        match &self.completion {
            SourceEvidenceCompletion::Ordinary(completed) => Arc::clone(completed),
            SourceEvidenceCompletion::Exact(exact) => Arc::clone(exact.source().evidence()),
        }
    }

    pub(crate) fn exact(&self) -> Option<&crate::declaration_context::ExactProductAdmission<'a>> {
        match &self.completion {
            SourceEvidenceCompletion::Ordinary(_) => None,
            SourceEvidenceCompletion::Exact(exact) => Some(exact),
        }
    }

    pub(crate) fn from_exact(
        matched: crate::declaration_context::MatchedExactProductAdmission<'a>,
    ) -> Self {
        let (exact, worker_bytes, input_path, target_source, inventory) = matched.into_parts();
        Self {
            worker_bytes,
            input_path,
            target_source,
            inventory,
            completion: SourceEvidenceCompletion::Exact(exact),
        }
    }
}

/// Admission of ordinary compiler evidence uses the same parser and physical
/// source validator as the compile front door, including its accounting owner.
pub(crate) fn admit_compiler_source<'a>(
    operation: &'a tidepool_repr::execution_schema::InventoryOperation,
    bytes: &'a [u8],
    input: &'a Path,
    source: &'a str,
) -> Result<SourceEvidenceAdmission<'a>, CompileError> {
    ParsedSourceEvidence::parse(operation, bytes, SourceEvidenceUse::Compiler, input)?
        .authenticate_for_certification(input, source)
}

/// Parsed worker observations, without consumed-byte or import-witness authority.
/// The owning admission charges its existing inventory before the sole JSON decode.
#[cfg_attr(test, derive(Clone))]
struct ParsedSourceEvidence<'a>(
    cache::DependencyEvidence,
    &'a [u8],
    &'a tidepool_repr::execution_schema::InventoryOperation,
);

#[derive(Clone, Copy)]
enum SourceEvidenceUse {
    BuildAction,
    CompletedCorpus,
    Compiler,
}

impl<'a> ParsedSourceEvidence<'a> {
    fn parse(
        operation: &'a tidepool_repr::execution_schema::InventoryOperation,
        bytes: &'a [u8],
        usage: SourceEvidenceUse,
        input: &Path,
    ) -> Result<Self, CompileError> {
        charge_source_evidence(operation, bytes.len())?;
        #[cfg(test)]
        SOURCE_EVIDENCE_DECODES.with(|count| count.set(count.get() + 1));
        serde_json::from_slice(bytes)
            .map(|facts| Self(facts, bytes, operation))
            .map_err(|error| match usage {
                SourceEvidenceUse::BuildAction => CompileError::ExtractFailed(format!(
                    "build-action dependency evidence: {error}"
                )),
                SourceEvidenceUse::CompletedCorpus => {
                    CompileError::ExtractFailed(format!("corpus dependency evidence: {error}"))
                }
                SourceEvidenceUse::Compiler => source_evidence_failure(
                    input,
                    cache::DependencyEvidenceFailure::WorkerDecode {
                        message: error.to_string(),
                    },
                ),
            })
    }

    fn validate_build_action_closure(
        &self,
        source: &Path,
        include: &[PathBuf],
    ) -> Result<(), CompileError> {
        if !self.0.cache_safe || !self.0.selection_complete {
            return Err(CompileError::ExtractFailed(
                "build-action source evidence is incomplete".into(),
            ));
        }
        validate_declared_source_closure(&self.0, source, include)
    }

    fn authenticate_for_certification(
        self,
        input: &'a Path,
        source: &'a str,
    ) -> Result<SourceEvidenceAdmission<'a>, CompileError> {
        let completed = cache::CompletedSourceEvidence::from_worker_evidence(self.0, input, source)
            .map_err(|failure| source_evidence_failure(input, failure))?;
        Ok(SourceEvidenceAdmission {
            worker_bytes: self.1,
            input_path: input,
            target_source: source,
            inventory: self.2,
            completion: SourceEvidenceCompletion::Ordinary(Arc::new(completed)),
        })
    }

    fn admit_build_action_for_certification(
        self,
        input: &'a Path,
        include: &[PathBuf],
        source: &'a str,
    ) -> Result<SourceEvidenceAdmission<'a>, CompileError> {
        let (worker_bytes, inventory) = (self.1, self.2);
        let completed = self.admit_build_action(input, include, source)?;
        Ok(SourceEvidenceAdmission {
            worker_bytes,
            input_path: input,
            target_source: source,
            inventory,
            completion: SourceEvidenceCompletion::Ordinary(Arc::new(completed)),
        })
    }

    fn authenticate(
        self,
        input: &Path,
        source: &str,
    ) -> Result<cache::CompletedSourceEvidence, CompileError> {
        cache::CompletedSourceEvidence::from_worker_evidence(self.0, input, source)
            .map_err(|failure| source_evidence_failure(input, failure))
    }

    fn authenticate_build_input(
        self,
        input: &Path,
    ) -> Result<cache::CompletedSourceEvidence, CompileError> {
        cache::CompletedSourceEvidence::from_worker_evidence(
            self.0,
            input,
            &std::fs::read_to_string(input)?,
        )
        .map_err(|_| {
            CompileError::ExtractFailed(
                "build-action consumed bytes or import witnesses changed".into(),
            )
        })
    }

    fn admit_build_action(
        self,
        input: &Path,
        include: &[PathBuf],
        expected_source: &str,
    ) -> Result<cache::CompletedSourceEvidence, CompileError> {
        self.validate_build_action_closure(input, include)?;
        let completed = self.authenticate_build_input(input)?;
        completed
            .validate_expected_source(expected_source)
            .map_err(|failure| source_evidence_failure(input, failure))?;
        Ok(completed)
    }
}

fn source_evidence_failure(
    input: &Path,
    failure: cache::DependencyEvidenceFailure,
) -> CompileError {
    compiler_evidence_failure(
        crate::certified_products::CertificationError::CompletedSourceEvidence {
            input: input.to_path_buf(),
            failure: Box::new(failure),
        },
    )
}

#[cfg(test)]
thread_local! {
    static SOURCE_EVIDENCE_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Validate consumed bytes and import witnesses against a build action's
/// declared module and complete source namespaces. Unused declared contents
/// need not be readable. Parsing shares the caller's inventory; parsed
/// observations grant no execution authority.
pub fn validate_prepared_fixture_sources(
    operation: &tidepool_repr::execution_schema::InventoryOperation,
    evidence_bytes: &[u8],
    source: &Path,
    include: &[PathBuf],
) -> Result<(), CompileError> {
    let parsed = ParsedSourceEvidence::parse(
        operation,
        evidence_bytes,
        SourceEvidenceUse::BuildAction,
        source,
    )?;
    parsed.validate_build_action_closure(source, include)?;
    parsed.authenticate_build_input(source).map(|_| ())
}

/// Validate observations from a fresh corpus compilation owned by a native
/// test run. This admits the completed output only: compile-time execution
/// remains ineligible for source replay and does not prove a hermetic action.
/// Observed Haskell sources must still belong to the declared source trees.
pub fn validate_completed_corpus_sources(
    operation: &tidepool_repr::execution_schema::InventoryOperation,
    evidence_bytes: &[u8],
    source: &Path,
    include: &[PathBuf],
) -> Result<(), CompileError> {
    let parsed = ParsedSourceEvidence::parse(
        operation,
        evidence_bytes,
        SourceEvidenceUse::CompletedCorpus,
        source,
    )?;
    validate_declared_source_closure(&parsed.0, source, include)?;
    parsed.authenticate(source, &std::fs::read_to_string(source)?)?;
    Ok(())
}

fn validate_declared_source_closure(
    evidence: &cache::DependencyEvidence,
    source: &Path,
    include: &[PathBuf],
) -> Result<(), CompileError> {
    let declared = DeclaredSourceSet::observe(source, include)?;
    for consumed in &evidence.sources {
        if !declared.0.contains(&std::fs::canonicalize(&consumed.path)?) {
            return Err(CompileError::ExtractFailed(format!(
                "build action consumed an undeclared source: {}",
                consumed.path.display()
            )));
        }
    }
    Ok(())
}

/// Declaration membership is a namespace fact. Actual consumed bytes and
/// import witnesses are authenticated separately by CompletedSourceEvidence.
struct DeclaredSourceSet(std::collections::BTreeSet<PathBuf>);

impl DeclaredSourceSet {
    fn observe(source: &Path, include: &[PathBuf]) -> Result<Self, CompileError> {
        let mut declared = std::collections::BTreeSet::from([std::fs::canonicalize(source)?]);
        for root in include {
            let paths = cache::dependency_source_paths(root)
                .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
            for path in paths.canonical_paths() {
                declared.insert(path?);
            }
        }
        Ok(Self(declared))
    }
}

enum CompilationOutput {
    Ordinary(CompiledArtifacts),
    Original(ProductionEntryOutput),
}

impl CompilationOutput {
    fn into_artifacts(self) -> Result<CompiledArtifacts, CompileError> {
        match self {
            Self::Ordinary(artifacts) => Ok(artifacts),
            Self::Original(_) => Err(CompileError::ExtractFailed(
                "original output is not an ordinary artifact result".into(),
            )),
        }
    }

    fn into_original(self) -> Result<ProductionEntryOutput, CompileError> {
        match self {
            Self::Original(original) => Ok(original),
            Self::Ordinary(_) => Err(CompileError::ExtractFailed(
                "fresh preparation lacks its published original entry".into(),
            )),
        }
    }
}

fn compile_invocation_inner(
    inv: &CompileInvocation<'_>,
    mut on_stage: &mut impl FnMut(&str, Duration, u64),
    mut policy: CompilationPolicy<'_>,
) -> Result<CompilationOutput, CompileError> {
    let invocation_span = tracing::debug_span!(
        target: "exomonad_harness::timing", "compile.invocation",
        inclusive = true, source_bytes = inv.source.len(), target_count = inv.targets.len(),
        output_dir = tracing::field::Empty, input_path = tracing::field::Empty,
    );
    let _invocation_span = invocation_span.enter();
    let mut invocation_timing = timing::InvocationTiming::start("output_preparation");
    assert!(
        !inv.targets.is_empty(),
        "compile_invocation: at least one target is required"
    );
    crate::host_work::checkpoint()?;
    let layout = invocation_output::InvocationOutputLayout::new(inv.targets)?;

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
        CompilationPolicy::RetainedEntry { output, .. } => CompilationOutputOwner::Original {
            preparation: production_entry::EntryPreparation::reserve(output)?,
            diagnostics: OriginalFailureDiagnostics::Hosted,
        },
        CompilationPolicy::BuildAction {
            export:
                BuildActionExport::ProductionEntry {
                    output_root: output,
                    ..
                },
            ..
        } => CompilationOutputOwner::Original {
            preparation: production_entry::EntryPreparation::reserve(output)?,
            diagnostics: OriginalFailureDiagnostics::BuildAction,
        },
        CompilationPolicy::BuildAction {
            export: BuildActionExport::CatalogInventory { output_root, .. },
            ..
        } => {
            let raw = output_root.join("raw");
            std::fs::create_dir(&raw)?;
            CompilationOutputOwner::Inspection(TempDir::new_in(raw)?.keep())
        }
        CompilationPolicy::BuildAction { scratch, .. } => {
            CompilationOutputOwner::Scratch(TempDir::new_in(scratch)?)
        }
        _ => CompilationOutputOwner::Scratch(compiler_scratch_directory()?),
    };
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
    invocation_span.record(
        "output_dir",
        tracing::field::display(output_owner.path().display()),
    );
    invocation_span.record("input_path", tracing::field::display(input_path.display()));

    let mut cmd = ExtractCmd::new().map_err(|e| CompileError::Io(e.into()))?;
    cmd.input(&input_path)
        .output_dir(output_owner.path())
        .targets(inv.targets)
        .includes(inv.include);
    if matches!(
        &policy,
        CompilationPolicy::BuildAction {
            export: BuildActionExport::PreparedFixture { .. },
            ..
        }
    ) {
        cmd.portable_output();
    }
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
    invocation_timing.advance("exact_input_preparation");
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

    // Select build products for each bound endpoint. The process boundary
    // privately places mutable GHC outputs by daemon epoch and worker slot;
    // this placement never authorizes additional source inputs. Reaped slot
    // rotations retain disk warmth; new daemons use fresh writer namespaces.
    let base_cmd = cmd;
    let mut first_attempt = true;
    let attempt = retry_bounded(
        || {
            if first_attempt {
                invocation_timing.advance("endpoint_selection");
                first_attempt = false;
            } else {
                invocation_timing.retry("endpoint_selection");
            }
            let mut cmd = base_cmd.clone();
            let portable = matches!(
                &policy,
                CompilationPolicy::BuildAction {
                    export: BuildActionExport::PreparedFixture { .. },
                    ..
                }
            );
            let endpoint = match &policy {
                CompilationPolicy::BuildAction { .. } if portable => cmd.bind_direct(),
                CompilationPolicy::BuildAction { .. } => cmd.bind_owned(),
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
            if !portable && deployment.package_source_association().is_none() {
                return Err(CompileAttemptError::Deployment(
                    crate::toolchain::DeploymentAdmissionError::PackageSourceRequired.into(),
                ));
            }
            match &policy {
                CompilationPolicy::BuildAction { .. } => {
                    cmd.build_products_dir(output_owner.path().join("build-products"));
                }
                _ => crate::paths::apply_build_products_dir(&mut cmd, &endpoint),
            }

            invocation_timing.advance("candidate_selection");
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
            invocation_timing.advance("submission_preparation");
            let preparation = CompilerDiagnosticCapture::preparing(output_owner.path(), &cmd);
            let selected_session_values = SelectedSessionValueOwners::from_command(&cmd)
                .map_err(CompileError::from)
                .map_err(|error| {
                    preparation.admission_refused(output_owner.path(), &cmd, &error);
                    CompileAttemptError::Diagnostic(retain_compiler_failure_inner(
                        output_owner.path(),
                        None,
                        error,
                        None,
                        &cmd,
                    ))
                })?;
            preparation.prepared(output_owner.path(), &cmd);
            let diagnostics = CompilerDiagnosticCapture::start(output_owner.path(), &cmd);
            crate::host_work::checkpoint()
                .map_err(|error| CompileAttemptError::Diagnostic(error.into()))?;
            let previous_submission = output_owner.begin_execution();
            invocation_timing.advance("endpoint_execution");
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
                    if let CompilationOutputOwner::Original { preparation, .. } = &mut output_owner
                    {
                        preparation.mark_uncertain_submission();
                    }
                    CompileAttemptError::Diagnostic(error)
                }
            });
            execution.map(|run| {
                diagnostics.completed(output_owner.path(), &cmd, run.success(), &run.output.stderr);
                (
                    cmd,
                    selected_session_values,
                    run,
                    producer,
                    candidate_set,
                    deployment,
                )
            })
        },
        |error| matches!(error,CompileAttemptError::Endpoint(error) if error.permits_rebind()),
    );
    let attempt = match attempt {
        Ok(attempt) => attempt,
        Err(error) => {
            if matches!(&error, CompileAttemptError::Endpoint(error) if error.definitely_unsubmitted())
                || matches!(&error, CompileAttemptError::Deployment(_))
                || matches!(&error, CompileAttemptError::Diagnostic(CompileError::Io(error))
                    | CompileAttemptError::ModulePackage(crate::toolchain::ModulePackageError::Interrupted(error))
                    if error.kind() == std::io::ErrorKind::Interrupted)
            {
                if let CompilationOutputOwner::Original { preparation, .. } = output_owner {
                    preparation.release_if_unsubmitted()?;
                }
            }
            return Err(error.into_compile_error());
        }
    };
    let (cmd, selected_session_values, run, producer, candidate_set, deployment) = attempt;
    invocation_timing.advance("output_acquisition");

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
        || matches!(&output_owner, CompilationOutputOwner::Original { .. })
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
    if let CompilationPolicy::BuildAction {
        export: BuildActionExport::PreparedFixture { output },
        ..
    } = &policy
    {
        let portable = (|| {
            let layout = invocation_output::InvocationOutputLayout::portable(inv.targets)?;
            let bundle = extract_and_read(
                &run,
                output_owner.path(),
                &layout,
                &mut on_stage,
                |stderr, success| {
                    if !success && !stderr.is_empty() {
                        tracing::warn!(targets = %inv.targets.join(","), "extract failed:\n{stderr}");
                    }
                },
            )?;
            ParsedSourceEvidence::parse(
                bundle.operation(),
                bundle.dependencies(),
                SourceEvidenceUse::BuildAction,
                &input_path,
            )?
            .admit_build_action(&input_path, inv.include, inv.source)?;
            let raw = bundle.targets();
            let artifacts = assemble_portable(bundle.metadata(), &raw, &mut on_stage)?;
            std::fs::create_dir(output)?;
            std::fs::write(output.join("meta.cbor"), bundle.metadata())?;
            for target in &raw {
                std::fs::write(
                    output.join(prepared_artifact_name(&target.target)),
                    target.prepared_bytes.as_slice(),
                )?;
                std::fs::write(
                    output.join(format!("{}.asks.json", target.target)),
                    target.asks_bytes.as_slice(),
                )?;
            }
            Ok(CompilationOutput::Ordinary(artifacts))
        })();
        let portable = portable.map_err(|error| {
            invocation_timing.retry("failure_retention");
            let error = retain_compiler_failure(output_owner.path(), &cmd, &compiler_stderr, error);
            invocation_timing.advance("refusal_return");
            error
        });
        invocation_timing.finish(portable.is_ok());
        return portable;
    }
    if let CompilationOutputOwner::Original { preparation, .. } = &output_owner {
        let original_layout = invocation_output::InvocationOutputLayout::original(inv.targets)?;
        let extracted = invocation_output::extract_native(
            &run,
            &deployment,
            preparation.raw(),
            &original_layout,
            &mut on_stage,
            |stderr, success| {
                if !success && !stderr.is_empty() {
                    tracing::warn!(targets = %inv.targets.join(","), "extract failed:\n{stderr}");
                }
            },
        );
        // Candidate admission misses are handled before source execution by the
        // worker. A completed request is never replayed because of its response.
        let native = extracted
            .map_err(|error| output_owner.retain_failure(&cmd, &compiler_stderr, error))?;
        let bundle = &native.bundle;
        let admitted = (|| {
            if let CompilationPolicy::BuildAction { source_path, .. } = &policy {
                let evidence =
                    CompilerSidecar::Dependencies.read(preparation.raw(), bundle.operation())?;
                ParsedSourceEvidence::parse(
                    bundle.operation(),
                    &evidence,
                    SourceEvidenceUse::BuildAction,
                    source_path,
                )?
                .admit_build_action(source_path, inv.include, inv.source)?;
            }
            let offer = SelectedModuleCandidateOffer {
                offer: ModuleCandidateOffer {
                    selected: candidate_set.map(Arc::new),
                    producer: producer.to_vec(),
                    include: inv.include.to_vec(),
                    exact: None,

                    checked_values: None,
                    activation_preview: None,
                },
                selected_session_values,
            };
            preparation.admit(&input_path, inv.source, &native, &offer)
        })()
        .map_err(|error| output_owner.retain_failure(&cmd, &compiler_stderr, error))?;
        invocation_timing.advance("retained_entry_seal");
        let (source_path, sources, catalog) = match &policy {
            CompilationPolicy::RetainedEntry {
                source_path,
                sources,
                catalog,
                ..
            } => (*source_path, *sources, catalog.as_ref()),
            CompilationPolicy::BuildAction {
                source_path,
                export:
                    BuildActionExport::ProductionEntry {
                        source_selection, ..
                    },
                ..
            } => (*source_path, *source_selection, None),
            _ => {
                return Err(CompileError::ExtractFailed(
                    "original output owner has a different compilation policy".into(),
                ))
            }
        };
        let CompilationOutputOwner::Original { preparation, .. } = output_owner else {
            unreachable!("original output owner was borrowed for admission")
        };
        let original = preparation.seal(
            admitted,
            source_path,
            sources,
            &deployment,
            inv.targets,
            catalog,
        )?;
        invocation_timing.finish(true);
        return Ok(CompilationOutput::Original(original));
    }
    let extracted = invocation_output::extract_native(
        &run,
        &deployment,
        output_owner.path(),
        &layout,
        &mut on_stage,
        |stderr, success| {
            if !success && !stderr.is_empty() {
                tracing::warn!(targets = %inv.targets.join(","), "extract failed:\n{stderr}");
            }
        },
    );
    // Candidate admission misses are handled before source execution by the
    // worker. A completed request is never replayed because of its response.
    let native =
        extracted.map_err(|error| output_owner.retain_failure(&cmd, &compiler_stderr, error))?;
    let bundle = &native.bundle;
    let native_output = native.native_output();
    let meta_bytes = bundle.metadata();
    let raw = bundle.targets();
    let product_bytes = bundle.products();
    let package_bundle_bytes = bundle.package_imports();
    let inventory_operation = bundle.operation();

    let assembled = (|| {
        invocation_timing.advance("source_admission");
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(
            inventory_operation.clone(),
        );
        let evidence_bytes =
            CompilerSidecar::Dependencies.read(output_owner.path(), &inventory_operation)?;
        if let Some((output, _)) = inventory_export {
            catalog_inventory::phase(output, catalog_inventory::Phase::SourceEvidenceValidation)?;
        }
        let exact_source = exact_request
            .as_ref()
            .map(|request| {
                request.admit_source_receipt_with_validation(
                    &input_path,
                    inv.source,
                    &mut validation,
                )
            })
            .transpose()?;
        let source_admission = match exact_request.as_ref().zip(exact_source.as_ref()) {
            Some((request, admitted)) => {
                SourceEvidenceAdmission::from_exact(admitted.match_product(
                    request,
                    &input_path,
                    inv.source,
                    &evidence_bytes,
                    &inventory_operation,
                )?)
            }
            None => {
                let build_action = matches!(&policy, CompilationPolicy::BuildAction { .. });
                let parsed = ParsedSourceEvidence::parse(
                    &inventory_operation,
                    &evidence_bytes,
                    if build_action {
                        SourceEvidenceUse::BuildAction
                    } else {
                        SourceEvidenceUse::Compiler
                    },
                    &input_path,
                )?;
                if build_action {
                    parsed.admit_build_action_for_certification(
                        &input_path,
                        inv.include,
                        inv.source,
                    )?
                } else {
                    parsed.authenticate_for_certification(&input_path, inv.source)?
                }
            }
        };
        let exact = source_admission.exact();
        let evidence = Some(source_admission.completed());
        let evidence_facts = evidence.map(|completed| &**completed);
        if let Some((output, _)) = inventory_export {
            catalog_inventory::phase(output, catalog_inventory::Phase::ProductCertification)?;
        }
        let receipt = &native.receipt;
        let captured_package_interfaces =
            certified_products::PackageInterfaceCustody::from_native_output(
                &native_output,
                receipt,
                &mut validation,
            )
            .map_err(compiler_evidence_failure)?;
        let package_products = certify_native_package_products(
            &native_output,
            receipt,
            &captured_package_interfaces,
            &mut validation,
        )?;
        invocation_timing.advance("product_decode_and_freshness");
        let fresh_products = certified_products::ParsedModuleProducts::decode_with_operation(
            &product_bytes,
            &package_bundle_bytes,
            validation.inventory.clone(),
        )
        .map_err(compiler_evidence_failure)?;
        let valid_evidence = evidence.filter(|value| value.revalidate(inv.source).is_ok());
        let cached_receipts: Vec<_> = receipt
            .modules
            .iter()
            .filter(|module| module.origin == certified_products::ProductOrigin::Cached)
            .collect();
        invocation_timing.advance("product_certification");
        let certified = if let Some(valid) = valid_evidence {
            let certified = certified_products::certify_products_with_validation(
                candidate_set.as_ref(),
                &receipt,
                &fresh_products,
                &source_admission,
                output_owner.path(),
                &producer,
                inv.include,
                authored,
                &selected_session_values,
                None,
                &captured_package_interfaces,
                &mut validation,
            )
            .map_err(compiler_evidence_failure)?;
            if let Some((output, selection)) = inventory_export {
                catalog_inventory::inventory(output, selection, valid, certified.data())?;
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
            certified_products::PendingProducts::empty()
        };
        invocation_timing.advance("output_assembly");
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
                        semantic_sha256: admission.product_context_digest(),
                    },
                ),
                &certified.data().recovery_products,
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
            evidence_facts,
            exact_request.as_ref(),
            Some(&certified.data().retained_core_products),
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
        let compiler_inputs = exact_request
            .as_ref()
            .map(|request| request.compiler_inputs());
        let certified = crate::declaration_context::issue_certified_product_native_availability(
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&producer)
                .sha256(),
            certified,
            compiler_inputs.as_ref().map(|input| input.artifacts()),
            &mut validation,
        )?;
        let physical_inputs = certified.captured_stage();
        let target_admission_start = Instant::now();
        invocation_timing.advance("target_admission");
        let package_catalog = package_availability_with_validation(
            &receipt.packages,
            certified.data(),
            &mut validation,
        )?;
        let physical = physical_inputs.issued_artifacts();
        artifacts.artifact_view = physical.clone();
        let mut inherited_completion_support = compiler_inputs
            .as_ref()
            .map(|inputs| inputs.compiler_source_support().clone())
            .unwrap_or_else(crate::artifact_inventory::CompilerSourceSupport::empty);
        for (name, target) in &mut artifacts.targets {
            receipt.targets.get(name).ok_or_else(|| {
                CompileError::ExtractFailed("target product receipt missing".into())
            })?;
            if valid_evidence.is_some() {
                let package_selection = native_output.ordinary_package_selection(
                    name,
                    &target.prepared.bind_package_program(),
                    certified.data(),
                )?;
                let observed = package_selection.authenticated_imports().clone();
                let selected_target = physical.inventory().select_native_target(
                    physical_inputs.physical_cohort(),
                    observed,
                    None,
                    None,
                    &[],
                    &package_catalog,
                    &mut validation,
                )?;
                let selected_groups =
                    Arc::new(physical_inputs.physical_cohort().certify_target_groups(
                        &selected_target,
                        &certified.data().groups,
                        exact_request.as_ref().map(|request| {
                            crate::declaration_context::CertifiedNativeGroupBaseline::Target(
                                request.groups.as_ref(),
                            )
                        }),
                        &mut validation,
                    )?);
                let package_closure = package_catalog.select(
                    selected_groups.groups(),
                    selected_target.imports(),
                    &validation.inventory,
                )?;
                target.pending_imports = selected_target.imports().to_vec();
                target.package_interfaces =
                    certified_products::certify_target_package_interfaces_with_validation(
                        target.prepared.prepared_shared(),
                        &package_closure,
                        &mut validation,
                    )
                    .map_err(compiler_evidence_failure)?;
                let native_inputs = selected_target
                    .with_certified_groups(Arc::clone(&selected_groups))?
                    .link_native_target(
                        &native_output,
                        &package_products,
                        package_selection,
                        &inherited_completion_support,
                        &validation.inventory,
                    )?;
                artifacts.artifact_view = artifacts
                    .artifact_view
                    .retain_custody(native_inputs.artifacts())?;
                let completed = native_inputs.linked_native_target().ok_or_else(|| {
                    CompileError::ExtractFailed(
                        "ordinary target lacks its issued completion".into(),
                    )
                })?;
                inherited_completion_support =
                    inherited_completion_support.with_linked_native_target(completed)?;
                target.native_inputs = Some(native_inputs);
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
        invocation_timing.advance("candidate_publication");
        artifacts.issued_output = Some(physical_inputs.output().clone());
        artifacts.certified_groups = physical_inputs.physical_cohort().demanded_products()?;
        artifacts.recovery_products = certified.data().recovery_products.to_vec();
        artifacts.completed_source_evidence = valid_evidence.cloned();
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
                module_candidates::publish_prepared(publication)?;
            }
        } else if exact_request.is_some() {
            module_candidates::record_exact_context_publication_skip(
                &artifacts.module_products[..fresh_count],
            );
        }
        artifacts.exact_source_admission = exact_source;
        Ok(artifacts)
    })();
    let artifacts = assembled.map_err(|error| {
        invocation_timing.retry("failure_retention");
        let error = output_owner.retain_failure(&cmd, &compiler_stderr, error);
        invocation_timing.advance("refusal_return");
        error
    })?;
    invocation_timing.advance("retained_entry_seal");
    invocation_timing.finish(true);
    Ok(CompilationOutput::Ordinary(artifacts))
}

// Persistent output owners already hold all raw payloads. Add only diagnostic
// metadata absent from that owner, using the existing bounded source capture.
fn retain_compiler_failure_metadata(
    directory: &Path,
    command: &ExtractCmd,
    stderr: &[u8],
) -> std::io::Result<()> {
    fn write_missing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => file.write_all(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::symlink_metadata(path)?.is_file() {
                    Ok(())
                } else {
                    Err(std::io::Error::other(
                        "diagnostic metadata is not a regular file",
                    ))
                }
            }
            Err(error) => Err(error),
        }
    }
    write_missing(
        &directory.join("compiler-request.bin"),
        &command.request_bytes(),
    )?;
    write_missing(
        &directory.join("compiler-cwd.bin"),
        std::env::current_dir()?.as_os_str().as_encoded_bytes(),
    )?;
    write_missing(&directory.join("compiler.stderr"), stderr)?;
    if !directory.join("consumed-sources.json").try_exists()? {
        failure_sources::retain(directory, Vec::new())?;
    }
    Ok(())
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
    retain_compiler_failure_inner(directory, Some(stderr), error, None, command)
}

fn retain_compiler_failure_inner(
    directory: &Path,
    stderr: Option<&[u8]>,
    error: CompileError,
    offer: Option<&ModuleCandidateOffer>,
    command: &ExtractCmd,
) -> CompileError {
    if std::env::var("TIDEPOOL_KEEP_TEST_LOGS").as_deref() != Ok("1") {
        return error;
    }
    match retain_failed_compiler_artifacts(directory, offer, command) {
        Ok(retained) => {
            if let Some(stderr) = stderr {
                if let Err(failure) = std::fs::write(retained.join("compiler.stderr"), stderr) {
                    tracing::warn!(%failure, "could not retain compiler stderr");
                }
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
    fn copy_native_products(
        source: &Path,
        destination: &Path,
        remaining: &mut u64,
        entries_left: &mut usize,
    ) -> std::io::Result<()> {
        for name in ["package-interfaces", "package-products"] {
            let directory = source.join(name);
            match std::fs::symlink_metadata(&directory) {
                Ok(metadata) if metadata.file_type().is_dir() => copy_scope(
                    &directory,
                    &destination.join(name),
                    remaining,
                    entries_left,
                    0,
                )?,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    let mut remaining = 128 * 1024 * 1024;
    let mut entries_left = 4096;
    copy_native_products(source, destination, &mut remaining, &mut entries_left)?;
    let exact_scope = source.join("exact-scope");
    if std::fs::symlink_metadata(&exact_scope).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        copy_scope(
            &exact_scope,
            &destination.join("exact-scope"),
            &mut remaining,
            &mut entries_left,
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
        copy_native_products(&entry.path(), &output, &mut remaining, &mut entries_left)?;
        copy_receipts(&entry.path(), &output, &mut remaining)?;
        let planned = entry.path().join("planned-declaration");
        if std::fs::symlink_metadata(&planned).is_ok_and(|metadata| metadata.file_type().is_dir()) {
            let output = output.join("planned-declaration");
            copy_files(&planned, &output, &mut remaining)?;
            copy_native_products(&planned, &output, &mut remaining, &mut entries_left)?;
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
/// (target semantics, asks sidecar shape, timing hook).
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
    asks_bytes: Arc<Vec<u8>>,
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
pub(crate) fn extract_and_read<P>(
    run: &tidepool_extract_cmd::ExtractRun,
    temp_dir: &Path,
    layout: &invocation_output::InvocationOutputLayout<P>,
    mut on_stage: impl FnMut(&str, Duration, u64),
    log_stderr: impl FnOnce(&str, bool),
) -> Result<invocation_output::InvocationOutputBundle<P>, CompileError> {
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
    // Logged at INFO so it lands in the ordinary compile log; absent when
    // compilation threw before reaching the summary line.
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
    let bundle = layout.read(temp_dir, inventory_operation)?;
    on_stage(
        timing::STAGE_CBOR_READ,
        cbor_read_start.elapsed(),
        bundle.bytes_len(),
    );
    Ok(bundle)
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

/// Deserialize physical metadata and ordered target outputs into checked artifacts.
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
                native_inputs: None,
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
        certified_groups: Arc::from([]),
        recovery_products: Vec::new(),
        issued_output: None,
        producer_identity: None,
        module_inventory: None,
        completed_source_evidence: None,
        exact_source_admission: None,
    })
}

/// A portable fixture has no external native, package or retained-value owner.
/// Every executable import must have been closed by its compiler transaction.
fn assemble_portable(
    meta_bytes: &[u8],
    raw: &[RawTargetOutput],
    on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    let artifacts = assemble(meta_bytes, raw, on_stage)?;
    ensure_no_uncertified_globals(&artifacts)?;
    for (target, artifact) in &artifacts.targets {
        check_constructor_identity_agreement(target, &artifact.prepared, &artifacts.table)
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    }
    Ok(artifacts)
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
        assert!(support.retained_source_implementations().is_empty());
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
        let authorization = || {
            Value::Array(vec![Value::Text(
                CheckedPurpose::Inspection.wire_tag().into(),
            )])
        };
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
            Value::Text(CheckedPurpose::Inspection.wire_tag().into())
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
        let mut closes = Vec::new();
        let error = crate::artifacts::compile_targets(
            "result = 1",
            &["result"],
            &[directory.path().into()],
            |_, _, _| {},
            &mut |close| closes.push(close),
        )
        .err()
        .expect("unconfigured observed endpoint must be refused");
        assert!(
            closes.is_empty(),
            "a refused endpoint must not close a compiler transaction: {closes:?}"
        );
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
    fn safe_refusal_rederives_producer_and_build_products_identity() {
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
                let products = argv
                    .windows(2)
                    .find(|pair| pair[0] == "--build-products-dir")
                    .map(|pair| PathBuf::from(&pair[1]))
                    .expect("bound attempt must select build products");
                selections.push((*endpoint.identity().producer_bytes(), products));
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

    fn unexecuted_offer(scratch: &Path) -> ModuleCandidateOffer {
        ModuleCandidateOffer::select_originals_with_catalog(
            b"unexecuted preparation",
            &[],
            scratch,
            &crate::toolchain::CatalogSelection::Acquired(None),
        )
        .unwrap()
    }

    #[test]
    #[serial_test::serial]
    fn applied_selection_refusals_retain_unsubmitted_requests_and_primary_errors() {
        let root = tempfile::tempdir().unwrap();
        let _restore = RestoreEnvironment(
            [
                "TIDEPOOL_KEEP_TEST_LOGS",
                "TIDEPOOL_TEST_ARTIFACT_ROOT",
                "TIDEPOOL_TEST_DIAGNOSTIC_SCOPE",
            ]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect(),
        );
        std::env::set_var("TIDEPOOL_KEEP_TEST_LOGS", "1");
        std::env::set_var("TIDEPOOL_TEST_ARTIFACT_ROOT", root.path());
        std::env::set_var("TIDEPOOL_TEST_DIAGNOSTIC_SCOPE", "1");
        let before = tidepool_extract_cmd::extract_spawn_count();
        for (names, duplicate) in [
            (
                vec!["Tidepool.Session.Val.G1", "Tidepool.Session.Val.G01"],
                true,
            ),
            (vec!["Tidepool.Session.Lib.G1"], false),
        ] {
            let scratch = compiler_scratch_directory().unwrap();
            let mut command = ExtractCmd::with_bin(
                tidepool_extract_cmd::ResolvedExtractBin::assume_resolved("must-not-run"),
            );
            command.inject_vals(names);
            let offer = unexecuted_offer(scratch.path());
            let mut expected_command = command.clone();
            if let Some(manifest) = offer.manifest_path() {
                expected_command.module_candidates(manifest);
            }
            let expected = expected_command.request_bytes();
            let error = offer
                .apply_to(command.clone(), scratch.path())
                .err()
                .unwrap();
            assert!(
                matches!(
                    &error,
                    CompileError::SessionValueSelection(
                        SessionValueSelectionError::DuplicateOwner { .. }
                    )
                ) == duplicate
            );
            assert!(matches!(&error, CompileError::SessionValueSelection(_)));
            let report: serde_json::Value = serde_json::from_slice(
                &std::fs::read(scratch.path().join("transaction.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(report["phase"], "preparation_refused");
            assert_eq!(report["submission"], "not_attempted");
            assert!(report["compiler_process_success"].is_null());
            assert_eq!(
                std::fs::read(scratch.path().join("compiler-request.bin")).unwrap(),
                expected
            );
            assert!(!scratch.path().join("compiler.stderr").exists());
            assert!(!scratch.path().join("compiler.stdout").exists());
            // A diagnostic destination failure must not replace the typed refusal.
            let unavailable = scratch.path().join("not-a-directory");
            std::fs::write(&unavailable, b"blocked diagnostics").unwrap();
            let CompileError::SessionValueSelection(expected_error) = error else {
                panic!("selection refusal changed its primary type");
            };
            let refused = unexecuted_offer(scratch.path())
                .apply_to(command.clone(), &unavailable)
                .err()
                .unwrap();
            let CompileError::SessionValueSelection(actual_error) = refused else {
                panic!("retention failure replaced the primary selection refusal");
            };
            assert_eq!(actual_error, expected_error);
        }
        let scratch = compiler_scratch_directory().unwrap();
        let command = ExtractCmd::with_bin(
            tidepool_extract_cmd::ResolvedExtractBin::assume_resolved("must-not-run"),
        );
        assert!(unexecuted_offer(scratch.path())
            .prepare_admitted_turn(command.clone(), scratch.path())
            .is_err());
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(scratch.path().join("transaction.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(report["phase"], "preparation_refused");
        assert!(!scratch.path().join("compiler.stderr").exists());
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before);
        for retained in std::fs::read_dir(root.path().join("compiler-failures")).unwrap() {
            assert!(!retained.unwrap().path().join("compiler.stderr").exists());
        }
    }

    #[test]
    fn applied_ordinary_turn_freezes_selection_and_placed_output_lifetime() {
        let scratch = tempfile::tempdir().unwrap();
        let mut command = ExtractCmd::with_bin(
            tidepool_extract_cmd::ResolvedExtractBin::assume_resolved("must-not-run"),
        );
        command
            .turn()
            .turn_out(scratch.path().join("old-turn.cbor"))
            .output_dir(scratch.path())
            .inject_val("Tidepool.Session.Val.G1");
        let applied = unexecuted_offer(scratch.path())
            .prepare_admitted_turn(command, scratch.path())
            .unwrap();
        let output = applied.output_owner.as_ref().unwrap().path().to_owned();
        let lifetime = Arc::downgrade(applied.output_owner.as_ref().unwrap());
        assert_ne!(output, scratch.path());
        let argv = applied.command().argv();
        assert!(argv
            .windows(2)
            .any(|pair| pair[0] == "--output-dir" && pair[1] == output));
        let bytes = applied.command().request_bytes();
        let mut derivative = applied.command().clone();
        derivative
            .inject_val("Tidepool.Session.Val.G2")
            .output_dir(scratch.path());
        assert_eq!(applied.command().request_bytes(), bytes);
        assert_eq!(
            applied.selected_session_values.as_slice(),
            &[tidepool_repr::SessionModule::val(
                tidepool_repr::Generation(1)
            )]
        );
        drop(scratch);
        assert!(
            output.is_dir(),
            "the frozen request retains its relocated output owner"
        );
        assert!(lifetime.upgrade().is_some());
        drop(applied);
        assert!(
            lifetime.upgrade().is_none(),
            "physical custody ends independently of diagnostic path retention"
        );
    }

    #[test]
    #[serial_test::serial]
    fn retained_entry_explicit_empty_selection_refuses_injected_format() {
        use crate::toolchain::CompilerDeploymentConfiguration;
        let root = tempfile::tempdir().unwrap();
        let authored = root.path().join("sources");
        std::fs::create_dir(&authored).unwrap();
        let source = authored.join("Input.hs");
        std::fs::write(&source, "module Input where\n__prepared = (37 :: Int)\n").unwrap();
        let sources = FrozenEntrySources::capture(&[authored], &source).unwrap();
        let output = root.path().join("entry");
        let entry = crate::artifacts::test_support::prepare_frozen_production_entry(
            &sources,
            root.path(),
            &output,
        )
        .unwrap();
        assert!(entry.products().original_compile_input.is_some());
        let CompilerDeploymentConfiguration::Configured(authority) =
            CompilerDeploymentConfiguration::from_env().unwrap()
        else {
            panic!("requires matched compiler authority")
        };
        let selected = ProductionEntrySources::FrozenWorkspace(sources);
        let before = tidepool_extract_cmd::extract_spawn_count();
        load_selected_production_entry(&output, &authority, &selected).unwrap();
        let path = output.join("entry.json");
        let original = std::fs::read(&path).unwrap();
        let mut manifest: serde_json::Value = serde_json::from_slice(&original).unwrap();
        manifest["selected_session_values"] = serde_json::json!(["Tidepool.Session.Val.G1"]);
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(load_selected_production_entry(&output, &authority, &selected).is_err());
        std::fs::write(path, original).unwrap();
        load_selected_production_entry(&output, &authority, &selected).unwrap();
        assert_eq!(
            tidepool_extract_cmd::extract_spawn_count(),
            before,
            "retained certification never executes source"
        );
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
        let packets = [
            ("certified-products.cbor", b"receipt".as_slice()),
            ("module-package-imports.cbor", b"imports".as_slice()),
            ("package-interfaces/interface.hi", b"interface".as_slice()),
            ("package-products/product.cbor", b"package".as_slice()),
            (
                "item-0/package-interfaces/interface.hi",
                b"nested-interface".as_slice(),
            ),
        ];
        for &(name, bytes) in &packets {
            let path = output.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
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
        let output = CompilationOutputOwner::Scratch(output);
        let error = output.retain_failure(&command, stderr.as_bytes(), error);
        drop(output);
        drop(scratch);

        let retained = std::fs::read_dir(root.path().join("compiler-failures"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1);
        let retained = &retained[0];
        for &(name, bytes) in &packets {
            assert_eq!(std::fs::read(retained.join(name)).unwrap(), bytes);
        }
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
        // An inaccessible retention destination cannot replace the worker failure.
        unsafe {
            std::env::set_var(
                "TIDEPOOL_TEST_ARTIFACT_ROOT",
                retained.join("compiler-request.bin"),
            );
        }
        let error = retain_compiler_failure(
            retained,
            &command,
            stderr.as_bytes(),
            CompileError::WorkerFailure(original.clone()),
        );
        let CompileError::WorkerFailure(diagnostics) = error else {
            panic!("failed retention must preserve the primary worker failure");
        };
        assert_eq!(diagnostics, original);
        let declared_source = root.path().join("Owned.hs");
        let source_bytes = b"module Owned where\n";
        std::fs::write(&declared_source, source_bytes).unwrap();
        let source_hash = crate::checked_cell::hash(source_bytes);
        let evidence = crate::cache::DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![crate::cache::SourceEvidence {
                path: declared_source.clone(),
                sha256: source_hash.clone(),
            }],
            resolutions: vec![],
            packages: vec![],
            modules: vec![],
        };
        let mut kept_paths = Vec::new();
        let inspection = tempfile::tempdir_in(root.path()).unwrap().keep();
        let preparation =
            production_entry::EntryPreparation::reserve(&root.path().join("entry")).unwrap();
        let hosted =
            production_entry::EntryPreparation::reserve(&root.path().join("hosted-entry")).unwrap();
        for owner in [
            CompilationOutputOwner::Inspection(inspection),
            CompilationOutputOwner::Original {
                preparation: hosted,
                diagnostics: OriginalFailureDiagnostics::Hosted,
            },
            CompilationOutputOwner::Original {
                preparation,
                diagnostics: OriginalFailureDiagnostics::BuildAction,
            },
        ] {
            let path = owner.path().to_owned();
            std::fs::write(path.join("owned-raw-packet"), b"already retained").unwrap();
            std::fs::write(
                path.join("dependencies.json"),
                serde_json::to_vec(&evidence).unwrap(),
            )
            .unwrap();
            // Existing compiler output remains in place while missing metadata is added.
            std::fs::write(path.join("compiler.stderr"), stderr.as_bytes()).unwrap();
            let error = owner.retain_failure(
                &command,
                stderr.as_bytes(),
                CompileError::WorkerFailure(original.clone()),
            );
            let CompileError::WorkerFailure(diagnostics) = error else {
                panic!("durable reporting must preserve the primary worker failure");
            };
            assert_eq!(diagnostics, original);
            drop(owner);
            assert_eq!(
                std::fs::read(path.join("owned-raw-packet")).unwrap(),
                b"already retained"
            );
            assert_eq!(
                std::fs::read(path.join("compiler-request.bin")).unwrap(),
                executed_request
            );
            assert_eq!(
                std::fs::read(path.join("compiler.stderr")).unwrap(),
                stderr.as_bytes()
            );
            assert_eq!(
                std::fs::read(path.join("compiler-cwd.bin")).unwrap(),
                std::env::current_dir()
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
            );
            assert!(path.join("consumed-sources.json").is_file());
            kept_paths.push(path);
        }
        std::fs::remove_file(declared_source).unwrap();
        for path in kept_paths {
            assert_eq!(
                std::fs::read(path.join("consumed-sources").join(&source_hash)).unwrap(),
                source_bytes
            );
        }
        assert_eq!(
            std::fs::read_dir(root.path().join("compiler-failures"))
                .unwrap()
                .count(),
            1,
            "already retained owners do not create diagnostic copies"
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
        let operation =
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
        let bytes = serde_json::to_vec(&evidence).unwrap();
        let parsed = ParsedSourceEvidence::parse(
            &operation,
            &bytes,
            SourceEvidenceUse::BuildAction,
            &source,
        )
        .unwrap();
        parsed
            .validate_build_action_closure(&source, &[inputs])
            .unwrap();
        assert!(parsed.validate_build_action_closure(&source, &[]).is_err());
        evidence.sources.pop();
        evidence.selection_complete = false;
        let bytes = serde_json::to_vec(&evidence).unwrap();
        let parsed = ParsedSourceEvidence::parse(
            &operation,
            &bytes,
            SourceEvidenceUse::BuildAction,
            &source,
        )
        .unwrap();
        assert!(parsed.validate_build_action_closure(&source, &[]).is_err());
    }

    #[test]
    fn prepared_source_admission_parses_once_and_charges_before_decode() {
        use sha2::{Digest, Sha256};
        use tidepool_repr::execution_schema::{InventoryDecodeLimits, InventoryOperation};

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Main.hs");
        let text = "module Main where\nresult = 42\n";
        std::fs::write(&source, text).unwrap();
        let mut evidence = assembly_evidence(&["Main"]);
        evidence.modules[0].source = source.clone();
        evidence.sources.push(cache::SourceEvidence {
            path: source.clone(),
            sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        });
        // Completed observations do not acquire replay eligibility by parsing.
        for cache_safe in [false, true] {
            for selection_complete in [false, true] {
                evidence.cache_safe = cache_safe;
                evidence.selection_complete = selection_complete;
                let bytes = serde_json::to_vec(&evidence).unwrap();
                let operation = InventoryOperation::new(Default::default());
                SOURCE_EVIDENCE_DECODES.with(|count| count.set(0));
                assert_eq!(
                    validate_prepared_fixture_sources(&operation, &bytes, &source, &[]).is_ok(),
                    cache_safe && selection_complete,
                );
                assert_eq!(SOURCE_EVIDENCE_DECODES.with(|count| count.replace(0)), 1);
                assert_eq!(operation.work_usage().unwrap().0, bytes.len() * 64);
                validate_completed_corpus_sources(&operation, &bytes, &source, &[]).unwrap();
                assert_eq!(SOURCE_EVIDENCE_DECODES.with(|count| count.replace(0)), 1);
                assert_eq!(operation.work_usage().unwrap().0, bytes.len() * 128);
            }
        }
        let restricted = InventoryOperation::new(InventoryDecodeLimits {
            max_work: 63,
            ..Default::default()
        });
        SOURCE_EVIDENCE_DECODES.with(|count| count.set(0));
        // Malformed JSON would reach a different failure if decode preceded charge.
        assert!(matches!(
            validate_prepared_fixture_sources(&restricted, b"{", &source, &[]),
            Err(CompileError::CompilerEvidence(_)),
        ));
        assert_eq!(SOURCE_EVIDENCE_DECODES.with(|count| count.replace(0)), 0);
        let operation = InventoryOperation::new(Default::default());
        assert!(matches!(
            validate_prepared_fixture_sources(&operation, b"{", &source, &[]),
            Err(CompileError::ExtractFailed(message)) if message.starts_with("build-action dependency evidence:"),
        ));
        assert_eq!(SOURCE_EVIDENCE_DECODES.with(|count| count.replace(0)), 1);
        assert!(charge_source_evidence(&operation, usize::MAX).is_err());
        assert_eq!(SOURCE_EVIDENCE_DECODES.with(|count| count.replace(0)), 0);
    }

    #[test]
    fn parsed_build_observations_preserve_physical_and_invocation_authentication() {
        use sha2::{Digest, Sha256};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Main.hs");
        let text = "module Main where\nresult = 42\n";
        std::fs::write(&source, text).unwrap();
        let mut evidence = assembly_evidence(&["Main"]);
        evidence.modules[0].source = source.clone();
        evidence.sources.push(cache::SourceEvidence {
            path: source.clone(),
            sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        });
        let operation =
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
        let bytes = serde_json::to_vec(&evidence).unwrap();
        SOURCE_EVIDENCE_DECODES.with(|count| count.set(0));
        let parsed = ParsedSourceEvidence::parse(
            &operation,
            &bytes,
            SourceEvidenceUse::BuildAction,
            &source,
        )
        .unwrap();
        parsed.validate_build_action_closure(&source, &[]).unwrap();
        parsed.clone().authenticate_build_input(&source).unwrap();
        assert!(parsed
            .authenticate(&source, "different invocation text")
            .is_err());
        assert_eq!(SOURCE_EVIDENCE_DECODES.with(|count| count.replace(0)), 1);
        let parsed = ParsedSourceEvidence::parse(
            &operation,
            &bytes,
            SourceEvidenceUse::BuildAction,
            &source,
        )
        .unwrap();
        let mismatch = parsed.admit_build_action(&source, &[], "different invocation text");
        assert!(matches!(mismatch,
            Err(CompileError::CompilerEvidence(error)) if matches!(&*error,
                certified_products::CertificationError::CompletedSourceEvidence { failure, .. }
                if matches!(&**failure, cache::DependencyEvidenceFailure::Source {
                    index: 0,
                    reason: cache::SourceWitnessFailure::Changed { expected, actual }
                } if expected == &format!("{:x}", Sha256::digest(text.as_bytes()))
                    && actual == &format!("{:x}", Sha256::digest(b"different invocation text"))))
        ));
        assert_eq!(SOURCE_EVIDENCE_DECODES.with(|count| count.replace(0)), 1);
        let parsed = ParsedSourceEvidence::parse(
            &operation,
            &bytes,
            SourceEvidenceUse::BuildAction,
            &source,
        )
        .unwrap();
        std::fs::write(&source, "changed physical input").unwrap();
        assert!(matches!(
            parsed.admit_build_action(&source, &[], "different invocation text"),
            Err(CompileError::ExtractFailed(message))
                if message == "build-action consumed bytes or import witnesses changed",
        ));
        assert_eq!(SOURCE_EVIDENCE_DECODES.with(|count| count.replace(0)), 1);
    }

    #[test]
    fn build_admission_observes_one_closure_and_keeps_later_freshness_independent() {
        use sha2::{Digest, Sha256};
        use tidepool_extract_cmd::{
            with_compiler_transaction_cancellable, CompilerTransactionCancellation,
        };
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Main.hs");
        let dependency = root.path().join("Dependency.hs");
        let shadow = root.path().join("earlier/Dependency.hs");
        std::fs::create_dir(shadow.parent().unwrap()).unwrap();
        let text = "module Main where\nresult = 42\n";
        let dependency_bytes = b"module Dependency where\nvalue = 1\n";
        std::fs::write(&source, text).unwrap();
        std::fs::write(&dependency, dependency_bytes).unwrap();
        let mut evidence = assembly_evidence(&["Main", "Dependency"]);
        evidence.modules[0].source = source.clone();
        evidence.modules[1].source = dependency.clone();
        evidence.sources = [
            (&source, text.as_bytes()),
            (&dependency, dependency_bytes.as_slice()),
        ]
        .into_iter()
        .map(|(path, bytes)| cache::SourceEvidence {
            path: path.clone(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
        })
        .collect();
        evidence.modules[0]
            .imports
            .push(cache::ModuleImportEvidence {
                qualifier: cache::ImportQualifier::Unqualified,
                module: "Dependency".into(),
                boot: false,
                selected: Some(dependency.clone()),
            });
        evidence.resolutions.push(cache::ResolutionEvidence {
            qualifier: cache::ImportQualifier::Unqualified,
            module: "Dependency".into(),
            boot: false,
            selected: Some(dependency.clone()),
            candidates: vec![shadow.clone(), dependency.clone()],
        });
        let bytes = serde_json::to_vec(&evidence).unwrap();
        let operation =
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
        let parsed = ParsedSourceEvidence::parse(
            &operation,
            &bytes,
            SourceEvidenceUse::BuildAction,
            &source,
        )
        .unwrap();
        cache::take_source_authentication_work();
        let completed = parsed
            .admit_build_action(&source, &[root.path().to_path_buf()], text)
            .unwrap();
        let observed = cache::take_source_authentication_work();
        assert_eq!(
            observed,
            cache::DependencyEvidenceWork {
                source_read_attempts: 1,
                source_read_bytes: dependency_bytes.len() as u64,
                negative_metadata_calls: 1,
            }
        );
        // Independent old route and full recomputation preserve semantic facts;
        // its repeated actual reads also sensitize the work observation.
        let first = cache::CompletedSourceEvidence::from_worker(&bytes, &source, text).unwrap();
        let second = cache::CompletedSourceEvidence::from_worker(&bytes, &source, text).unwrap();
        assert_eq!(completed, first);
        assert_eq!(completed, second);
        let repeated = cache::take_source_authentication_work();
        assert_eq!(repeated.source_read_attempts, 2);
        assert_eq!(
            repeated.source_read_bytes,
            dependency_bytes.len() as u64 * 2
        );
        assert_eq!(repeated.negative_metadata_calls, 2);
        let cancellation = CompilerTransactionCancellation::new();
        cancellation.cancel();
        // A new invocation-text check still refuses cancellation without
        // revoking the already completed owner.
        let cancelled = with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || completed.validate_expected_source(text),
        );
        assert_eq!(
            cancelled.action.unwrap_err(),
            cache::DependencyEvidenceFailure::Interrupted
        );
        assert_eq!(
            cache::take_source_authentication_work(),
            cache::DependencyEvidenceWork::default()
        );
        std::fs::write(&dependency, b"changed after completed admission").unwrap();
        completed.validate_expected_source(text).unwrap();
        assert_eq!(
            cache::take_source_authentication_work(),
            cache::DependencyEvidenceWork::default()
        );
        assert!(
            completed.revalidate(text).is_err(),
            "later consumers retain their own freshness check"
        );
        std::fs::write(&dependency, dependency_bytes).unwrap();
        std::fs::write(&shadow, b"higher priority provider").unwrap();
        let parsed = ParsedSourceEvidence::parse(
            &operation,
            &bytes,
            SourceEvidenceUse::BuildAction,
            &source,
        )
        .unwrap();
        assert!(
            matches!(parsed.admit_build_action(&source, &[root.path().to_path_buf()], "wrong"),
                Err(CompileError::ExtractFailed(message))
                    if message == "build-action consumed bytes or import witnesses changed",
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn declared_source_membership_avoids_unused_reads_but_authenticates_consumed_sources() {
        use cache::source_inspection_work;
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Main.hs");
        let dependency = temporary.path().join("outside.txt");
        let declared = temporary.path().join("declared");
        std::fs::create_dir(&declared).unwrap();
        std::fs::write(&source, "module Main where\nvalue = 42\n").unwrap();
        std::fs::write(&dependency, "module Dependency where\nvalue = 1\n").unwrap();
        symlink(&dependency, declared.join("Alias.hs")).unwrap();
        let unused = declared.join("Unused.hs");
        std::fs::write(&unused, "unused = 1\n").unwrap();
        std::fs::set_permissions(&unused, std::fs::Permissions::from_mode(0)).unwrap();
        let ignored = declared.join("ignored.txt");
        std::fs::write(&ignored, "not a selected source").unwrap();
        let hardlink_target = temporary.path().join("other-outside.hs");
        std::fs::write(&hardlink_target, "different canonical path").unwrap();
        std::fs::hard_link(&hardlink_target, declared.join("Hardlink.hs")).unwrap();
        let mut evidence = assembly_evidence(&["Main", "Dependency"]);
        evidence.sources = [&source, &dependency]
            .into_iter()
            .map(|path| cache::SourceEvidence {
                path: path.to_path_buf(),
                sha256: format!("{:x}", Sha256::digest(std::fs::read(path).unwrap())),
            })
            .collect();
        evidence.modules[0].source = source.clone();
        evidence.modules[1].source = dependency.clone();
        let bytes = serde_json::to_vec(&evidence).unwrap();
        let operation =
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
        source_inspection_work::take();
        validate_prepared_fixture_sources(
            &operation,
            &bytes,
            &source,
            std::slice::from_ref(&declared),
        )
        .unwrap();
        let work = source_inspection_work::take();
        assert_eq!(work.content_read_attempts, 0);
        assert_eq!(work.hashes, 0);
        assert!(work.entries >= 4);
        let full_error = cache::source_root_manifest(&declared).unwrap_err();
        assert_eq!(full_error.path, unused);
        assert_eq!(
            full_error.source.kind(),
            std::io::ErrorKind::PermissionDenied
        );

        for undeclared in [&ignored, &hardlink_target] {
            let mut other = evidence.clone();
            other.sources[1].path = undeclared.clone();
            assert!(validate_declared_source_closure(
                &other,
                &source,
                std::slice::from_ref(&declared)
            )
            .is_err());
        }
        std::fs::set_permissions(&dependency, std::fs::Permissions::from_mode(0)).unwrap();
        assert!(validate_prepared_fixture_sources(
            &operation,
            &bytes,
            &source,
            std::slice::from_ref(&declared)
        )
        .is_err());
        std::fs::set_permissions(&dependency, std::fs::Permissions::from_mode(0o600)).unwrap();
        validate_prepared_fixture_sources(
            &operation,
            &bytes,
            &source,
            std::slice::from_ref(&declared),
        )
        .unwrap();

        // Namespace observation remains complete after finding all consumed paths.
        let missing = temporary.path().join("missing-root");
        assert!(
            validate_declared_source_closure(&evidence, &source, &[declared.clone(), missing])
                .is_err()
        );
        let dangling = declared.join("dangling.txt");
        symlink(temporary.path().join("absent"), &dangling).unwrap();
        assert!(validate_declared_source_closure(
            &evidence,
            &source,
            std::slice::from_ref(&declared)
        )
        .is_err());
        std::fs::remove_file(dangling).unwrap();
        symlink(&declared, declared.join("back")).unwrap();
        assert!(validate_declared_source_closure(
            &evidence,
            &source,
            std::slice::from_ref(&declared)
        )
        .is_err());
        std::fs::remove_file(declared.join("back")).unwrap();
        std::fs::set_permissions(&unused, std::fs::Permissions::from_mode(0o600)).unwrap();
        let before = cache::source_roots_identity(
            b"declared-membership-control",
            std::slice::from_ref(&declared),
        )
        .unwrap();
        std::fs::write(&unused, "unused = 2\n").unwrap();
        let after = cache::source_roots_identity(
            b"declared-membership-control",
            std::slice::from_ref(&declared),
        )
        .unwrap();
        assert_ne!(before, after);
        validate_prepared_fixture_sources(
            &operation,
            &bytes,
            &source,
            std::slice::from_ref(&declared),
        )
        .unwrap();
        eprintln!(
            "declared-source work: namespace_entries={} unused_content_reads={} hashes={}",
            work.entries, work.content_read_attempts, work.hashes
        );
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
        fn decode_fresh_products(bytes: &[u8]) -> Result<Vec<RawModuleProduct>, CompileError> {
            Ok(tidepool_repr::execution_schema::parse_module_products(
                bytes,
                &crate::prepared_artifact::production_requirements()?,
                module_candidates::product_decode_limits(),
            )?)
        }
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

    /// Compiler evidence only: runtime source leases and executable admission
    /// remain the responsibility of the runtime's joined two-cell history.
    #[test]
    #[ignore = "requires the declared rooted compiler service; toolchain_world_native_seal_test owns it"]
    fn real_world_output_seals_native_items_after_its_matching_compiler_end() {
        assert_real_world_native_history(false, false);
    }

    #[test]
    #[ignore = "requires the declared rooted compiler service; toolchain_world_original_seal_test owns it"]
    fn real_world_original_output_seals_its_actual_native_declaration() {
        assert_real_world_native_history(false, true);
    }

    #[test]
    #[ignore = "requires the declared rooted compiler service; toolchain_world_mixed_seal_test owns it"]
    fn real_world_mixed_output_retains_prior_native_stages() {
        assert_real_world_native_history(true, false);
    }

    fn assert_real_world_native_history(mixed: bool, declaration_only: bool) {
        use crate::cell_plan::WorldCellAdmissionSubject;
        use crate::checked_cell::CheckedPlannedCellSlot;
        use crate::compiler_world::{
            CapturedCompilerSourceRoot, CapturedCompilerSources, VisibleWorldBindings,
            WorldSourceSelection,
        };
        use tidepool_extract_cmd::{
            settle_compiler_transaction_cancellable, CompilerTransactionCancellation,
            CompilerTransactionOrigin,
        };
        // Retain existing exact-binding diagnostics only for a failed real history.
        struct NativeHistoryTrace {
            rows: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
            next_span: std::sync::atomic::AtomicU64,
        }
        impl tracing::Subscriber for NativeHistoryTrace {
            fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
                matches!(
                    metadata.target(),
                    "tidepool::artifact_inventory::ambiguity_trace"
                        | "tidepool::certified_products::source_closure_trace"
                )
            }
            fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
                Some(tracing::metadata::LevelFilter::DEBUG)
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(
                    self.next_span
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                )
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
            fn event(&self, event: &tracing::Event<'_>) {
                struct Fields(String);
                impl std::fmt::Write for Fields {
                    fn write_str(&mut self, text: &str) -> std::fmt::Result {
                        let mut remaining = 8192usize.saturating_sub(self.0.len()).min(text.len());
                        while !text.is_char_boundary(remaining) {
                            remaining -= 1;
                        }
                        self.0.push_str(&text[..remaining]);
                        Ok(())
                    }
                }
                impl tracing::field::Visit for Fields {
                    fn record_debug(
                        &mut self,
                        field: &tracing::field::Field,
                        value: &dyn std::fmt::Debug,
                    ) {
                        use std::fmt::Write;
                        let _ = write!(self, " {}={value:?}", field.name());
                    }
                }
                if !self.enabled(event.metadata()) {
                    return;
                }
                let mut fields = Fields(event.metadata().target().to_owned());
                event.record(&mut fields);
                let mut rows = self.rows.lock().unwrap();
                if rows.len() == 128 {
                    rows.pop_front();
                }
                rows.push_back(fields.0);
            }
        }
        let native_trace = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let dispatcher = tracing::Dispatch::new(NativeHistoryTrace {
            rows: Arc::clone(&native_trace),
            next_span: std::sync::atomic::AtomicU64::new(1),
        });
        let _trace_guard = tracing::dispatcher::set_default(&dispatcher);
        let initial = if mixed {
            let producer = PathBuf::from(
                std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
                    .expect("the rooted case owns its initial ordinary producer output"),
            )
            .join("initial-published-source");
            std::fs::create_dir(&producer).unwrap();
            let sources = producer.join("sources");
            std::fs::create_dir(&sources).unwrap();
            std::fs::write(
                sources.join("StageInputG.hs"),
                include_str!("../tests/fixtures/world-native/StageInputG.hs"),
            )
            .unwrap();
            let wrapper = sources.join("StageInputEntry.hs");
            std::fs::write(
                &wrapper,
                "module StageInputEntry where\nimport qualified StageInputG\n__prepared :: Int\n__prepared = StageInputG.g\n",
            )
            .unwrap();
            let frozen = FrozenEntrySources::capture(&[sources], &wrapper).unwrap();
            let entry = crate::artifacts::test_support::prepare_frozen_production_entry(
                &frozen,
                &producer,
                &producer.join("entry"),
            )
            .expect("actual frozen entry producer for the initial World Original");
            let publication = entry
                .products()
                .published_source_original_selection(
                    &frozen
                        .source_revision(b"World mixed history source")
                        .unwrap(),
                    &crate::declaration_join::ExactModuleIdentity {
                        unit: "main".into(),
                        module: "StageInputG".into(),
                    },
                )
                .expect("the same ordinary output issues its published source selection");
            let context =
                crate::declaration_context::ExactDeclarationContext::new(&[], &[], Vec::new())
                    .unwrap()
                    .with_published_source_originals(&publication)
                    .expect("carry the published source's exact lexical and compiler selection");
            assert!(context
                .lexical_graph()
                .iter()
                .any(|node| node.owner.unit == "main" && node.owner.module == "StageInputG"));
            let support = context.compiler_source_support();
            let row = support
                .native_availability()
                .rows()
                .find(|row| row.original_owner().module == "StageInputG")
                .expect("ordinary producer retained its own Original availability");
            let crate::artifact_inventory::ArtifactPayload::Original(product) =
                &row.target_original(row.original_owner()).unwrap().payload
            else {
                unreachable!()
            };
            let native = product.original_native();
            let ordinal_for = |name: &str| {
                native
                    .inventory_facts()
                    .native_group_ordinals
                    .iter()
                    .copied()
                    .find(|ordinal| {
                        certified_products::original_native_group_definitions(product, *ordinal)
                            .unwrap()
                            .bindings()
                            .iter()
                            .any(|group| match group {
                                tidepool_repr::execution_schema::Group::NonRecursive(binding) => {
                                    binding.identity.occurrence == name
                                }
                                tidepool_repr::execution_schema::Group::Recursive(bindings) => {
                                    bindings
                                        .iter()
                                        .any(|binding| binding.identity.occurrence == name)
                                }
                            })
                    })
                    .expect("actual named Original group")
            };
            let used = ordinal_for("g");
            let unused = ordinal_for("unusedSibling");
            assert_ne!(
                used, unused,
                "unused sibling has independent physical membership"
            );
            assert!(
                row.permits(used) && row.permits(unused),
                "initial genuine availability includes the unused sibling"
            );
            Some(Arc::new(context))
        } else {
            None
        };
        let mut prior_world_output = None;
        let mut terminal_refusal_control = None;
        for cancel_after_end in [false, true] {
            let cancellation = CompilerTransactionCancellation::new();
            let mut observed_close = None;
            let settled = settle_compiler_transaction_cancellable(
                cancellation.clone(),
                |close| observed_close = Some(close),
                || {
                    let configured = crate::toolchain::CompilerDeploymentConfiguration::from_env()
                        .expect("declared compiler deployment");
                    let (endpoint, _, _) =
                        crate::toolchain::bind_admitted_extract_endpoint(&configured)
                            .expect("configured compiler endpoint");
                    let endpoint = crate::toolchain::AdmittedCompilerEndpoint::from_bound(endpoint)
                        .expect("admitted compiler endpoint");
                    let root = Arc::new(tempfile::tempdir()?);
                    let source_path = root.path().join("Module.hs");
                    std::fs::write(&source_path, "module Module where\n")?;
                    let body = std::fs::read(source_path)?;
                    let mut roots = vec![CapturedCompilerSourceRoot {
                        manifest: crate::cache::SourceRootManifest::from_file_digests([(
                            PathBuf::from("Module.hs"),
                            blake3::hash(&body).to_hex().to_string(),
                        )])
                        .expect("actual captured source manifest"),
                        bodies: BTreeMap::from([(
                            PathBuf::from("Module.hs"),
                            Arc::<[u8]>::from(body),
                        )]),
                    }];
                    if mixed || declaration_only {
                        let prelude = PathBuf::from(
                            std::env::var_os("TIDEPOOL_PRELUDE_DIR").ok_or_else(|| {
                                CompileError::ExtractFailed(
                                    "missing declared renderer source resource".into(),
                                )
                            })?,
                        );
                        let mut pending = vec![prelude.clone()];
                        let mut bodies = BTreeMap::new();
                        let mut remaining = CapturedCompilerSources::capture_byte_allowance()?;
                        while let Some(directory) = pending.pop() {
                            for entry in std::fs::read_dir(directory)? {
                                let entry = entry?;
                                let path = entry.path();
                                if path.is_dir() {
                                    pending.push(path);
                                } else if path
                                    .extension()
                                    .is_some_and(|extension| extension == "hs")
                                {
                                    let body = std::fs::read(&path)?;
                                    remaining = remaining.checked_sub(body.len() as u64).ok_or_else(||
                                            CompileError::ExtractFailed("declared renderer source capture exceeds allowance".into()))?;
                                    bodies.insert(
                                        path.strip_prefix(&prelude)
                                            .expect("source under declared root")
                                            .to_path_buf(),
                                        Arc::<[u8]>::from(body),
                                    );
                                }
                            }
                        }
                        assert!(bodies.contains_key(Path::new("Tidepool/Inspection/Display.hs")));
                        let manifest = crate::cache::SourceRootManifest::from_file_digests(
                            bodies.iter().map(|(path, body)| {
                                (path.clone(), blake3::hash(body).to_hex().to_string())
                            }),
                        )
                        .expect("actual declared renderer source manifest");
                        roots.push(CapturedCompilerSourceRoot { manifest, bodies });
                    }
                    let sources = Arc::new(CapturedCompilerSources::from_captured_roots(
                        roots,
                        Arc::from(&b"native-world-source"[..]),
                        root,
                    )?);
                    let source = WorldSourceSelection::new(*sources.seal(), "native-world-source")?;
                    let inputs = ModuleCandidateOffer::compiler_world_inputs(
                        initial.clone(),
                        VisibleWorldBindings::from_published_bindings(0, std::iter::empty())?,
                        sources,
                        crate::compiler_world::CompilerWorldLexicalSurface::from_import_lines(
                            if mixed {
                                vec!["import qualified StageInputG".into()]
                            } else {
                                Vec::new()
                            },
                            None,
                        )?,
                    )?;
                    let world = crate::compiler_world::open_world(inputs, &endpoint)?;
                    let plan = crate::compiler_world::plan_cell(
                        &endpoint,
                        &world,
                        Arc::new(tidepool_extract_cmd::WorldCellRecipe {
                            source: if declaration_only {
                                include_str!("../tests/fixtures/world-native/DeclarationHistory.hs")
                            } else if mixed {
                                include_str!("../tests/fixtures/world-native/MixedHistory.hs")
                            } else {
                                "let answer = (1 :: Int)"
                            }
                            .into(),
                            template: include_str!(
                                "../tests/fixtures/world-native/CellTemplate.hs"
                            )
                            .into(),
                            turn_templates: Vec::new(),
                        }),
                        None,
                    )?;
                    assert_eq!(plan.items().len(), if mixed { 5 } else { 1 });
                    let origin =
                        CompilerTransactionOrigin::current().expect("actual compiler operation");
                    let subject = WorldCellAdmissionSubject::for_source_transaction(
                        &origin,
                        [1; 16],
                        0,
                        1,
                        1,
                        [b"compiler-only native output control".to_vec()],
                    )?;
                    let value_rows = if declaration_only {
                        vec![]
                    } else if mixed {
                        vec![
                            (0, 31, "baseStage"),
                            (2, 33, "answerStage"),
                            (4, 35, "resultStage"),
                        ]
                    } else {
                        vec![(0, 1, "answer")]
                    };
                    let mut binders = vec![Vec::new(); plan.items().len()];
                    for (index, generation, name) in value_rows {
                        binders[index].push((
                            name.into(),
                            tidepool_repr::SessionVarId::from_extract(
                                tidepool_codegen::prepared_program::session_var_id(
                                    &tidepool_repr::SessionModule::val(tidepool_repr::Generation(
                                        generation,
                                    ))
                                    .module_name(),
                                    name,
                                ),
                            ),
                        ));
                    }
                    let slots = if declaration_only {
                        vec![CheckedPlannedCellSlot::Declaration { declaration: 21 }]
                    } else if mixed {
                        vec![
                            CheckedPlannedCellSlot::Bind { value: 31 },
                            CheckedPlannedCellSlot::Declaration { declaration: 32 },
                            CheckedPlannedCellSlot::Bind { value: 33 },
                            CheckedPlannedCellSlot::Bind { value: 34 },
                            CheckedPlannedCellSlot::Bind { value: 35 },
                        ]
                    } else {
                        vec![CheckedPlannedCellSlot::Bind { value: 1 }]
                    };
                    let token = plan.issue_admission_token(
                        subject,
                        slots
                            .into_iter()
                            .zip(binders)
                            .map(|(slot, bindings)| {
                                use crate::cell_plan::WorldCellReservationRequest as Request;
                                use crate::checked_cell::CheckedPlannedCellSlot as Slot;
                                match slot {
                                    Slot::Prologue { declaration } => {
                                        assert!(bindings.is_empty());
                                        Request::Prologue { declaration }
                                    }
                                    Slot::Declaration { declaration } => {
                                        assert!(bindings.is_empty());
                                        Request::Declaration { declaration }
                                    }
                                    Slot::Bind { value } => Request::Bind { value, bindings },
                                    Slot::Expression {
                                        capture,
                                        observation_name,
                                    } => {
                                        assert_eq!(bindings.len(), 1);
                                        let binding = bindings.into_iter().next().unwrap();
                                        assert_eq!(binding.0, observation_name);
                                        Request::Expression { capture, binding }
                                    }
                                }
                            })
                            .collect(),
                    )?;
                    let reservation = plan.reserve(token)?;
                    let checked = crate::compiler_world::admit_world_cell_program(
                        &endpoint,
                        reservation,
                        source.clone(),
                        source,
                        &origin,
                    )?
                    .map_err(|refusal| {
                        CompileError::ExtractFailed(format!(
                            "actual World producer refused: {refusal:?}"
                        ))
                    })?;
                    let binding =
                        crate::declaration_context::WorldSourceCompilationBinding::from_output(
                            &checked, 0,
                        )?;
                    let identity =
                        crate::declaration_context::WorldCompilationIdentity::from_binding(
                            &binding,
                        );
                    let base = checked.reservation().plan().world();
                    let domain = base.restoration().captured_source_domain();
                    assert!(identity.matches_source_recipe(base.logical_id(), domain));
                    let mut wrong_base = *base.logical_id();
                    wrong_base[0] ^= 1;
                    let mut wrong_domain = *domain;
                    wrong_domain[0] ^= 1;
                    assert!(!identity.matches_source_recipe(&wrong_base, domain));
                    assert!(!identity.matches_source_recipe(base.logical_id(), &wrong_domain));
                    if mixed && !cancel_after_end {
                        crate::compiler_world::native_demand::assert_captured_stage_input_refusals(
                            &checked,
                        );
                    }
                    let prepared = prepare_world_cell_program(Arc::clone(&checked))?;
                    Ok::<_, CompileError>((prepared, checked, origin))
                },
            );
            assert_eq!(observed_close.as_ref(), Some(settled.close()));
            if let Err(error) = settled.action() {
                for row in native_trace.lock().unwrap().iter() {
                    eprintln!("native history trace: {row}");
                }
                panic!(
                        "actual World native preparation failed (mixed={mixed}, declaration_only={declaration_only}, cancel={cancel_after_end}): {error:?}; compiler close: {:?}",
                        settled.close()
                    );
            }
            assert!(matches!(
                settled.close(),
                tidepool_extract_cmd::CompilerTransactionClose::Clean
            ));
            let admission = settled
                .host_admission_cancellation()
                .expect("owner-issued post-END admission")
                .clone();
            if let Ok((_, _, origin)) = settled.action() {
                assert!(settled.belongs_to(origin));
            }
            let (prepared, checked, _) = settled
                .into_outcome()
                .action
                .expect("actual World native preparation");
            if mixed {
                if let Some(foreign) = &prior_world_output {
                    crate::declaration_context::assert_world_foreign_output_refusal(
                        &checked,
                        foreign,
                        &prepared.items[0]
                            .native
                            .as_ref()
                            .expect("actual first native item")
                            .output,
                    );
                } else {
                    prior_world_output = Some(
                        prepared.items[0]
                            .native
                            .as_ref()
                            .expect("actual first native item")
                            .output
                            .clone(),
                    );
                }
            }
            if mixed && !cancel_after_end {
                terminal_refusal_control = Some(Arc::clone(&checked));
                crate::declaration_context::assert_world_repeated_source_refusals(
                    &checked,
                    prepared.items[4]
                        .native
                        .as_ref()
                        .expect("actual final native item")
                        .output
                        .products
                        .as_ref()
                        .expect("actual final World target")
                        .target_native_inputs(),
                );
                use crate::compiler_world::WorldCellRuntimeInputOrigin;
                assert_eq!(checked.stages().len(), 3);
                assert_eq!(checked.stages()[0].ordinals(), &[0]);
                assert_eq!(checked.stages()[1].ordinals(), &[1]);
                assert_eq!(checked.stages()[2].ordinals(), &[2, 3, 4]);
                assert!(checked.item(3).unwrap().binders().is_empty());
                let first = checked.item(0).expect("actual first binding");
                let [base] = first.binders() else {
                    panic!("first item captures exactly baseStage");
                };
                assert_eq!(base.name(), "baseStage");
                let base = tidepool_repr::SessionVarId::from_extract(base.varid());
                let demand = checked.runtime_binding_demand();
                assert!(demand.existing().is_empty());
                assert_eq!(demand.prior_stage(), &[base]);
                assert!(checked.demanded_rows().is_empty());
                assert!(first.selected_runtime_inputs().inputs().is_empty());
                for ordinal in [1, 2] {
                    let inputs = checked
                        .item(ordinal)
                        .expect("actual later stage")
                        .selected_runtime_inputs();
                    assert!(!inputs.inputs().is_empty());
                    for input in inputs.inputs() {
                        assert_eq!(input.session_var(), base);
                        assert!(input.checked_row().is_none());
                        assert!(matches!(
                            input.origin(),
                            WorldCellRuntimeInputOrigin::PriorStage {
                                producer: 0,
                                consumer: 1 | 2,
                            }
                        ));
                    }
                }
                checked.assert_captured_stage_output_refusals();
            }
            let mut metadata_drift = BTreeMap::new();
            if mixed && !cancel_after_end {
                for (ordinal, item) in prepared.items.iter().enumerate() {
                    let Some(native) = &item.native else {
                        continue;
                    };
                    let path = native.output.directory().join("meta.cbor");
                    let bytes = std::fs::read(&path).expect("actual World item's metadata file");
                    let (table, mut warnings) =
                        tidepool_repr::serial::read_metadata(&bytes).unwrap();
                    let original_warnings = warnings.clone();
                    warnings.captured_type = Some("later physical file observation".into());
                    let replacement =
                        tidepool_repr::serial::write_metadata(&table, &warnings).unwrap();
                    assert_ne!(bytes, replacement);
                    std::fs::write(&path, replacement).unwrap();
                    assert!(metadata_drift
                        .insert(ordinal, (path, bytes, table, original_warnings))
                        .is_none());
                }
                assert!(
                    metadata_drift.len() >= 2,
                    "drift multiple real World item files after capture and END"
                );
            }
            if cancel_after_end {
                cancellation.cancel_host_admission();
                let Err(CompileError::Io(error)) = prepared.seal(&admission) else {
                    panic!("cancelled compiler admission must refuse native sealing");
                };
                assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
                continue;
            }
            let program = prepared
                .seal(&admission)
                .expect("same actual output after END");
            assert_eq!(program.items().len(), if mixed { 5 } else { 1 });
            for (ordinal, (path, bytes, table, warnings)) in metadata_drift {
                let native = program.items()[ordinal]
                    .native()
                    .expect("same captured native item after sealing");
                assert_eq!(&table, native.table());
                assert_eq!(warnings.captured_type, native.warnings().captured_type);
                assert_eq!(warnings.has_io, native.warnings().has_io);
                assert_eq!(warnings.warnings, native.warnings().warnings);
                assert_ne!(
                    native.warnings().captured_type.as_deref(),
                    Some("later physical file observation")
                );
                std::fs::write(path, bytes).unwrap();
            }

            if !declaration_only {
                let mut coverage = crate::linked_native::tests::AuthenticationCoverage::default();
                let mut targets = 0;
                for linked in program
                    .items()
                    .iter()
                    .filter_map(|item| item.native_products())
                    .filter_map(|products| products.linked_native_target())
                {
                    coverage.merge(
                        crate::linked_native::tests::assert_actual_target_authentication(linked),
                    );
                    targets += 1;
                }
                assert!(targets > 0, "actual native history has linked targets");
                if mixed {
                    assert!(
                        coverage.home_scope
                            && coverage.source_selector
                            && coverage.retained_selector
                            && coverage.package_leaf,
                        "mixed history must exercise every acquisition refusal: {coverage:?}"
                    );
                }
            }
            let mut completed = Vec::new();
            for item in program.items() {
                if let Some(products) = item.native_products() {
                    let linked = products
                        .linked_native_target()
                        .expect("actual World linked target");
                    completed.push(Arc::clone(linked.closure()));
                }
                for prior in &completed {
                    assert!(
                        item.compiler_context()
                            .compiler_source_support()
                            .acquired_native_closures()
                            .iter()
                            .any(|held| Arc::ptr_eq(held, prior)),
                        "every item context retains its actual completed target prefix"
                    );
                }
            }
            if declaration_only {
                let item = &program.items()[0];
                assert!(item.native().is_none());
                let certificate = item
                    .checked_item()
                    .planned_declaration()
                    .expect("actual authored stage");
                assert_eq!(
                    certificate.product().owner().module,
                    "Tidepool.Session.Lib.G21"
                );
                let native = certificate.product().original_native();
                assert!(!native.inventory_facts().native_group_ordinals.is_empty());
                assert!(certificate
                    .lexical_exports()
                    .iter()
                    .flat_map(|export| std::iter::once(&export.head).chain(export.children.iter()))
                    .any(|identity| identity.occurrence == "readOriginalStage"));
                continue;
            }
            if mixed {
                let declaration = &program.items()[1];
                assert!(declaration.native().is_none());
                let certificate = declaration
                    .checked_item()
                    .planned_declaration()
                    .expect("actual authored stage certificate");
                assert_eq!(
                    certificate.product().owner().module,
                    "Tidepool.Session.Lib.G32"
                );
                let native = certificate.product().original_native();
                let references = native
                    .inventory_facts()
                    .native_group_ordinals
                    .iter()
                    .flat_map(|ordinal| {
                        certified_products::original_native_group_imports(
                            certificate.product(), *ordinal,
                        )
                        .unwrap()
                    })
                    .filter(|reference| matches!(reference,
                        certified_products::ReceiptImportOwner::OriginalBinding { module, binder, .. }
                        if module == "StageInputG" && binder.occurrence == "g"))
                    .collect::<BTreeSet<_>>();
                assert!(
                    !references.is_empty(),
                    "actual dormant declaration retains its exact Original import"
                );
                for stage in checked.stages() {
                    let validation =
                        crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(
                            Arc::clone(&checked.validation().inventory),
                        );
                    let captured = checked
                        .native_demand_completion()
                        .checked_stage(&checked, stage, &validation)
                        .unwrap();
                    let uses = captured
                        .selection()
                        .into_iter()
                        .flat_map(|selection| selection.batches())
                        .flat_map(|batch| batch.original_uses())
                        .filter(|reference| references.contains(reference))
                        .collect::<Vec<_>>();
                    eprintln!(
                        "actual dormant Original stage={} ordinals={:?} uses={uses:?}",
                        stage.index(),
                        stage.ordinals()
                    );
                }
                assert!(program.items()[2].native().is_some());
                assert!(program.items()[3].native().is_some());
                assert!(program.items()[3]
                    .native()
                    .unwrap()
                    .item()
                    .binders()
                    .is_empty());
                assert!(program.items()[4].native().is_some());
                assert!(
                    program.items()[0]
                        .compiler_context()
                        .compiler_source_support()
                        .projection()
                        .snapshot_roles()
                        .keys()
                        .all(|owner| owner.module != "Tidepool.Session.Lib.G32"),
                    "early item cannot see later declaration"
                );
                assert!(
                    program.items()[4]
                        .compiler_context()
                        .compiler_source_support()
                        .projection()
                        .snapshot_roles()
                        .keys()
                        .any(|owner| owner.module == "Tidepool.Session.Lib.G32"),
                    "later item retains its earlier declaration"
                );
            }
            let native = program.items()[0]
                .native()
                .expect("actual binding native program");
            assert_eq!(
                native.item().binders(),
                &[if mixed { "baseStage" } else { "answer" }]
            );
            assert!(native.value_interface().is_some());
            let value = native.value_interface_certificate().unwrap();
            let [signature] = native.item().signatures() else {
                panic!("the actual Int binding has one checked signature")
            };
            let types =
                crate::declaration_context::SelectedTypeInterfaces::from_checked_value_signature(
                    &value, signature,
                )
                .expect("actual checked-value type projection");
            assert!(
                types
                    .compiler_source_support()
                    .projection()
                    .roles()
                    .is_empty(),
                "Int has only package type owners"
            );
            assert_eq!(
                types.toolchain_identity_sha256(),
                value
                    .certified_interface()
                    .interface()
                    .toolchain_identity_sha256()
            );
            assert!(
                Arc::ptr_eq(&value, &native.value_interface_certificate().unwrap()),
                "type projection preserves the actual compiled value owner"
            );
            value
                .world_type_parts()
                .expect("the original value retains its actual row input owners");
            program
                .validate_typed_entries()
                .expect("actual native entry belongs to normalized item");
        }
        if let Some(program) = terminal_refusal_control {
            // Retirement invalidates the compiler realization. Keep this terminal
            // phase after every equal-facts comparison between request issuers.
            crate::compiler_world::native_demand::assert_refusal_latched_before_postresolution_checkpoint(&program);
        }
    }

    #[test]
    #[ignore = "requires the declared rooted compiler service; toolchain_world_lifetime_test owns it"]
    fn real_world_shadow_successors_release_transport_ancestors_before_cold_restore() {
        assert_eq!(std::env::var("TIDEPOOL_TEST_COMPILER_DIAGNOSTICS").as_deref(), Ok("off"),
            "physical lifetime control requires --compiler-diagnostics off; capture intentionally retains scratch");
        use crate::cell_plan::WorldCellAdmissionSubject;
        use crate::compiler_world::{
            CapturedCompilerSourceRoot, CapturedCompilerSources, CompilerWorldBinding,
            CompilerWorldLexicalSurface, PublicModuleQuestion, VisibleWorldBindings,
            WorldSourceSelection,
        };
        use tidepool_extract_cmd::{
            settle_compiler_transaction_cancellable, CompilerTransactionCancellation,
            CompilerTransactionClose, CompilerTransactionOrigin, InspectionNamespace,
        };
        let arena_fd_count = || -> usize {
            std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(Result::ok)
                .filter_map(|entry| std::fs::read_link(entry.path()).ok())
                .filter(|target| {
                    target
                        .to_string_lossy()
                        .contains("memfd:tidepool-owned-compiler-inputs")
                })
                .count()
        };
        let mut constant_frontier = None;
        let observations = PathBuf::from(
            std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
                .expect("retained case observation root"),
        )
        .join("world-writer-observations");
        std::fs::create_dir_all(&observations).unwrap();
        for item_count in [2, 8] {
            let settled = settle_compiler_transaction_cancellable(
                CompilerTransactionCancellation::new(),
                |_| {},
                || {
                    let configured = crate::toolchain::CompilerDeploymentConfiguration::from_env()
                        .expect("declared compiler deployment");
                    let (endpoint, _, _) =
                        crate::toolchain::bind_admitted_extract_endpoint(&configured)
                            .expect("configured compiler endpoint");
                    let endpoint = crate::toolchain::AdmittedCompilerEndpoint::from_bound(endpoint)
                        .expect("admitted compiler endpoint");
                    let source_directory = Arc::new(tempfile::tempdir()?);
                    let body = b"module Module where\n";
                    let sources = Arc::new(CapturedCompilerSources::from_captured_roots(
                        vec![CapturedCompilerSourceRoot {
                            manifest: crate::cache::SourceRootManifest::from_file_digests([(
                                PathBuf::from("Module.hs"),
                                blake3::hash(body).to_hex().to_string(),
                            )])
                            .expect("actual captured source manifest"),
                            bodies: BTreeMap::from([(
                                PathBuf::from("Module.hs"),
                                Arc::<[u8]>::from(body.as_slice()),
                            )]),
                        }],
                        Arc::from(&b"world-shadow-lifetime-source"[..]),
                        source_directory,
                    )?);
                    let source =
                        WorldSourceSelection::new(*sources.seal(), "world-shadow-lifetime-source")?;
                    let world = crate::compiler_world::open_world(
                        ModuleCandidateOffer::compiler_world_inputs(
                            None,
                            VisibleWorldBindings::from_published_bindings(0, std::iter::empty())?,
                            sources,
                            CompilerWorldLexicalSurface::from_import_lines(Vec::new(), None)?,
                        )?,
                        &endpoint,
                    )?;
                    let plan = crate::compiler_world::plan_cell(
                        &endpoint,
                        &world,
                        Arc::new(tidepool_extract_cmd::WorldCellRecipe {
                            source: std::iter::repeat_n("let answer = (1 :: Int)", item_count)
                                .collect::<Vec<_>>()
                                .join("\n"),
                            template: include_str!(
                                "../tests/fixtures/world-native/CellTemplate.hs"
                            )
                            .into(),
                            turn_templates: Vec::new(),
                        }),
                        None,
                    )?;
                    assert_eq!(plan.items().len(), item_count);
                    let origin =
                        CompilerTransactionOrigin::current().expect("actual compiler transaction");
                    let subject = WorldCellAdmissionSubject::for_source_transaction(
                        &origin,
                        [2; 16],
                        0,
                        1,
                        1,
                        [b"compiler-only shadow lifetime control".to_vec()],
                    )?;
                    let requests = (1..=item_count)
                        .map(
                            |generation| crate::cell_plan::WorldCellReservationRequest::Bind {
                                value: generation as u64,
                                bindings: vec![(
                                    "answer".into(),
                                    tidepool_repr::SessionVarId::from_extract(
                                        tidepool_codegen::prepared_program::session_var_id(
                                            &tidepool_repr::SessionModule::val(
                                                tidepool_repr::Generation(generation as u64),
                                            )
                                            .module_name(),
                                            "answer",
                                        ),
                                    ),
                                )],
                            },
                        )
                        .collect();
                    let reservation =
                        plan.reserve(plan.issue_admission_token(subject, requests)?)?;
                    let checked = crate::compiler_world::admit_world_cell_program(
                        &endpoint,
                        reservation,
                        source.clone(),
                        source,
                        &origin,
                    )?
                    .map_err(|refusal| {
                        CompileError::ExtractFailed(format!(
                            "actual World shadow producer refused: {refusal:?}"
                        ))
                    })?;
                    Ok::<_, CompileError>((prepare_world_cell_program(checked)?, endpoint, origin))
                },
            );
            if let Err(error) = settled.action() {
                panic!(
                    "actual shadow preparation failed: {error:?}; close {:?}",
                    settled.close()
                );
            }
            assert!(matches!(settled.close(), CompilerTransactionClose::Clean));
            let admission = settled
                .host_admission_cancellation()
                .expect("same issued post-END observer")
                .clone();
            assert!(settled.belongs_to(&settled.action().as_ref().unwrap().2));
            let (prepared, endpoint, _) = settled.into_outcome().action.unwrap();
            let program = prepared
                .seal(&admission)
                .expect("actual shadow output seal");
            let checked = program.world_program();
            let checked = checked.clone();
            let mut current = Arc::clone(checked.reservation().plan().world_owned());
            let history_observations = observations.join(format!("history-{item_count}"));
            current
                .restoration()
                .export_transport_observation_for_test(&history_observations.join("initial"))
                .expect("observe actual initial cold writer");
            let mut predecessor_worlds = Vec::new();
            let mut predecessor_directories = Vec::new();
            let mut value_rows = Vec::new();
            let mut value_files = Vec::new();
            let mut value_paths = Vec::new();
            let mut value_arena_bytes = Vec::new();
            for (ordinal, item) in program.items().iter().enumerate() {
                predecessor_worlds.push(Arc::downgrade(current.restoration()));
                let directory = current.restoration().input_directory_for_test();
                predecessor_directories
                    .push((Arc::downgrade(directory), directory.path().to_owned()));
                let observed = checked.item(ordinal).unwrap();
                let binder = &observed.binders()[0];
                let row = item
                    .native()
                    .unwrap()
                    .value_interface_certificate()
                    .unwrap();
                let (entry, parts) = row.world_type_parts().unwrap();
                value_rows.push(Arc::downgrade(&row));
                value_paths.push(parts.iter().find(|part| part.kind == crate::declaration_context::original_inputs::OriginalInputKind::Interface).unwrap().path.clone());
                value_files.push(
                    parts
                        .iter()
                        .map(|part| Arc::downgrade(&part.transport.compiler_file_lease()))
                        .collect::<Vec<_>>(),
                );
                value_arena_bytes.push(
                    parts
                        .iter()
                        .map(|part| (part.transport.endpoint(), part.transport.arena_len()))
                        .collect::<BTreeMap<_, _>>()
                        .values()
                        .sum::<u64>(),
                );
                let symbol = tidepool_repr::execution_schema::SymbolIdentity {
                    unit: entry.descriptor.owner.unit.clone(),
                    module: binder.module().into(),
                    namespace: "value".into(),
                    occurrence: binder.name().into(),
                    record_parent: None,
                };
                let binding = |name| {
                    CompilerWorldBinding::from_checked_value(
                        name,
                        &symbol,
                        tidepool_repr::SessionVarId::from_extract(binder.varid()),
                        1,
                        row.clone(),
                    )
                };
                assert!(VisibleWorldBindings::from_published_bindings(
                    1,
                    [binding(binder.name()), binding(binder.name())]
                )
                .is_err());
                let aliases = VisibleWorldBindings::from_published_bindings(
                    1,
                    [binding(binder.name()), binding("sameRowAlias")],
                )
                .unwrap();
                assert_eq!(aliases.names().count(), 2);
                aliases.assert_shared_row_alias_removal_for_test();
                let mut foreign = symbol.clone();
                foreign.module.push_str(".Foreign");
                assert!(VisibleWorldBindings::from_published_bindings(
                    1,
                    [CompilerWorldBinding::from_checked_value(
                        binder.name(),
                        &foreign,
                        tidepool_repr::SessionVarId::from_extract(binder.varid()),
                        1,
                        row.clone(),
                    )]
                )
                .is_err());
                current = program
                    .item_successor(
                        &current,
                        item.checked_item(),
                        [CompilerWorldBinding::from_checked_value(
                            binder.name(),
                            &symbol,
                            tidepool_repr::SessionVarId::from_extract(binder.varid()),
                            1,
                            row,
                        )],
                    )
                    .expect("same checked item's compiler successor")
                    .world()
                    .clone();
                current
                    .restoration()
                    .export_transport_observation_for_test(
                        &history_observations.join(format!("delta-{:02}", ordinal + 1)),
                    )
                    .expect("observe actual tuple6 successor writer");
            }
            drop(checked);
            drop(program);
            let old_rows = value_rows[..item_count - 1]
                .iter()
                .filter(|row| row.upgrade().is_some())
                .count();
            let old_row_fds = value_files[..item_count - 1]
                .iter()
                .flatten()
                .filter_map(std::sync::Weak::upgrade)
                .map(|file| std::os::fd::AsRawFd::as_raw_fd(file.as_ref()))
                .collect::<BTreeSet<_>>()
                .len();
            let old_directories = predecessor_directories
                .iter()
                .filter(|(directory, _)| directory.upgrade().is_some())
                .count();
            let frontier = arena_fd_count();
            let descriptor_bytes = std::fs::metadata(
                current
                    .restoration()
                    .input_directory_for_test()
                    .path()
                    .join("successor.cbor"),
            )
            .unwrap()
            .len();
            eprintln!("world shadow lifetime: items={item_count}, live_bindings={}, old_rows={old_rows}, old_row_fds={old_row_fds}, predecessor_directories={old_directories}, owned_arena_fds={frontier}, current_row_arena_bytes={}, successor_descriptor_bytes={descriptor_bytes}", current.restoration().binding_names().count(), value_arena_bytes[item_count - 1]);
            eprintln!(
                "world shadow descriptor paths (weak owners observed): {:?}",
                predecessor_directories
                    .iter()
                    .map(|(_, path)| path)
                    .collect::<Vec<_>>()
            );
            eprintln!("world shadow row paths (only last should survive): {value_paths:?}");
            assert!(predecessor_worlds
                .iter()
                .all(|world| world.upgrade().is_none()));
            assert!(
                predecessor_directories
                    .iter()
                    .all(|(directory, path)| directory.upgrade().is_none() && !path.exists()),
                "latest successor must not retain predecessor descriptor directories"
            );
            assert!(
                value_rows[..item_count - 1]
                    .iter()
                    .all(|row| row.upgrade().is_none()),
                "independent shadowed values must release their dedicated rows"
            );
            assert!(
                value_files[..item_count - 1]
                    .iter()
                    .flatten()
                    .all(|file| file.upgrade().is_none()),
                "old arena FDs must not survive through a predecessor transport frontier"
            );
            assert!(value_paths[..item_count - 1]
                .iter()
                .all(|path| !path.exists()));
            assert!(value_rows[item_count - 1].upgrade().is_some());
            assert!(value_files[item_count - 1]
                .iter()
                .all(|file| file.upgrade().is_some()));
            assert!(value_paths[item_count - 1].exists());
            assert_eq!(current.restoration().binding_names().count(), 1);
            if let Some(expected) = constant_frontier {
                assert_eq!(
                    frontier, expected,
                    "latest-only physical arena frontier is independent of shadow history length"
                );
            }
            constant_frontier = Some(frontier);
            current
                .restoration()
                .export_cold_observation_for_test(&history_observations.join("cold-final"))
                .expect("observe same-product cold writer after predecessor owners drop");
            let restoration = current.restoration().clone();
            let logical = *current.logical_id();
            drop(current);
            let settled = settle_compiler_transaction_cancellable(
                CompilerTransactionCancellation::new(),
                |_| {},
                || {
                    crate::compiler_world::restore_world(
                        &restoration,
                        &endpoint,
                        PublicModuleQuestion::structured_type(
                            "GHC.Types".into(),
                            InspectionNamespace::Type,
                            "Int".into(),
                        ),
                    )
                },
            );
            if let Err(error) = settled.action() {
                panic!(
                    "predecessor-free same-product cold restoration failed: {error:?}; close {:?}",
                    settled.close()
                );
            }
            assert!(matches!(settled.close(), CompilerTransactionClose::Clean));
            let restored = settled.into_outcome().action.unwrap();
            assert_eq!(restored.world().logical_id(), &logical);
            assert!(matches!(
                restored.answer(),
                crate::compiler_world::WorldAnswer::Type { .. }
            ));
        }
        std::fs::write(observations.join("bundle.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "schema":1,"authority":false,"histories":[
                {"directory":"history-2","initial":"initial","successors":["delta-01","delta-02"],"same_product_cold":"cold-final"},
                {"directory":"history-8","initial":"initial","successors":(1..=8).map(|n|format!("delta-{n:02}")).collect::<Vec<_>>(),"same_product_cold":"cold-final"},
            ]
        })).unwrap()).unwrap();
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    #[serial_test::serial]
    fn real_worker_ordinary_package_interface_relocation_reuses_retained_token() {
        use tidepool_repr::execution_schema::testing;

        let fixture_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bridge/haskell/test-prepared-stg");
        let source =
            "module PackageWitness where\nimport Data.Char (toUpper)\nvalue :: Char\nvalue = toUpper 'a'\n";
        let compiled = crate::artifacts::test_support::compile_targets(
            source,
            &["value"],
            &[fixture_root],
            |_, _, _| {},
        )
        .unwrap();
        let inherited = certified_products::inherited_package_witnesses_with_validation(
            &compiled.recovery_products,
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
        .unwrap();
        let (owner, selected) = inherited
            .iter()
            .next()
            .expect("a tuple program retains its package interface");
        let token = compiled.recovery_products.iter().find_map(|product| {
            product
                .original_native()
                .package_interfaces()
                .get(owner)
                .filter(|token| token.sha256() == &selected.sha256)
                .cloned()
        });
        let token = token.expect("the real compiler output retains package custody");

        // The producer's captured copy, not an ambient GHC path, is the first
        // issued location. Remove it after retaining the exact token bytes.
        std::fs::remove_file(&selected.selected_path)
            .expect("remove the completed transaction's package copy");
        let retained = tempfile::tempdir().unwrap();
        let relocated = retained.path().join("External.hi");
        std::fs::write(&relocated, token.bytes().as_ref()).unwrap();
        let observed = certified_products::PackageInterfaceWitness {
            selected_path: relocated.clone(),
            sha256: selected.sha256,
        };
        let certified = certified_products::CertifiedProducts {
            home_contracts: None,
            groups: certified_products::IssuedGroupCandidates::empty(),
            recovery_products: Arc::from(compiled.recovery_products.clone()),
            module_interfaces: Arc::from([]),
            value_interfaces: Arc::from([]),
            retained_core_products: Default::default(),
        };
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let receipt = BTreeMap::from([(owner.clone(), observed.clone())]);
        let reconciled = certified_products::reconcile_ordinary_package_interfaces(
            &compiled.recovery_products,
            &receipt,
            &mut validation,
        )
        .unwrap();
        let [reuse] = reconciled.decisions() else {
            panic!("one ordinary package observation expected");
        };
        assert_eq!(
            reuse.kind(),
            certified_products::PackageInterfaceSelectionKind::Reuse
        );
        assert_eq!(reuse.selected(), selected);
        assert_eq!(reuse.observed(), &observed);
        assert!(reuse
            .token()
            .is_some_and(|token| token.sha256() == &selected.sha256));
        assert!(matches!(
            reuse.issuer(),
            Some(certified_products::PackageInterfaceIssuer::Original(issuer))
                if compiled
                    .recovery_products
                    .iter()
                    .any(|product| product.owner() == issuer)
        ));

        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let catalog =
            package_availability_with_validation(&receipt, &certified, &mut validation).unwrap();
        assert!(catalog.current.is_empty());
        assert_eq!(&catalog.interfaces[owner], selected);

        let binder = tidepool_repr::execution_schema::SymbolIdentity {
            unit: owner.0.clone(),
            ..testing::identity(&owner.1, "entry")
        };
        let selected_for_target = catalog
            .select(
                &[],
                &[certified_products::PendingImportOwner::Package {
                    unit: owner.0.clone(),
                    module: owner.1.clone(),
                    binder,
                    interface_digest: selected.sha256,
                }],
                &validation.inventory,
            )
            .unwrap();
        assert_eq!(&selected_for_target[owner], selected);
    }

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

    /// Construct the reachable interval where the frontend captured A but the
    /// physical file supplied to the real worker is already B.
    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn real_worker_portable_build_refuses_changed_captured_source_before_export() {
        let fixture_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bridge/haskell/test-prepared-stg");
        let physical = std::fs::read_to_string(fixture_root.join("ModuleProductB.hs")).unwrap();
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("ModuleProductB.hs");
        std::fs::write(&input, &physical).unwrap();
        let roots = [fixture_root];
        let histories = [
            physical.clone(),
            format!("{physical}\n-- captured before physical replacement\n"),
            physical.replace("module ModuleProductB", "module PriorProductB"),
        ];
        for (index, captured) in histories.iter().enumerate() {
            let output = root.path().join(format!("export-{index}"));
            let invocation = CompileInvocation {
                source: captured,
                targets: &["consume"],
                include: &roots,
                fallback_module_name: "ModuleProductB",
            };
            let result = compile_invocation_inner(
                &invocation,
                &mut |_, _, _| {},
                CompilationPolicy::BuildAction {
                    source_path: &input,
                    scratch: root.path(),
                    export: BuildActionExport::PreparedFixture { output: &output },
                },
            );
            if index == 0 {
                assert!(result.is_ok(), "unchanged worker input: {:?}", result.err());
                assert!(output.join("consume.prepared.cbor").is_file());
                assert!(output.join("meta.cbor").is_file());
            } else {
                assert!(
                    !output.exists(),
                    "a refused source cannot publish portable output"
                );
                assert!(matches!(result,
                    Err(CompileError::CompilerEvidence(error)) if matches!(&*error,
                        certified_products::CertificationError::CompletedSourceEvidence { failure, .. }
                        if matches!(&**failure, cache::DependencyEvidenceFailure::Source {
                            reason: cache::SourceWitnessFailure::Changed { .. }, ..
                        }))
                ));
            }
        }
    }

    #[test]
    #[ignore = "requires matched Haskell worker and frontend"]
    #[serial_test::serial]
    fn real_deployment_package_reuses_cohort_and_invalidates_source_shadow() {
        use sha2::{Digest, Sha256};

        let package = crate::toolchain::configured_module_package()
            .unwrap()
            .expect("requires the matched native runtime catalog");
        package.assert_no_catalog_payload_files();
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
        let compile_cache = PathBuf::from(
            std::env::var_os("TIDEPOOL_COMPILE_CACHE_DIR").expect("isolated runner cache"),
        );
        let (endpoint, _) = crate::toolchain::bind_extract_endpoint().unwrap();
        let products = crate::paths::build_products_dir(&endpoint.identity().producer_hex());
        assert!(products.starts_with(&compile_cache));
        match std::fs::read_dir(&products) {
            Ok(entries) => assert_eq!(entries.count(), 0, "no prior module products"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!(
                "isolated product inspection {}: {error}",
                products.display()
            ),
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
        std::fs::write(shadow.path().join("Tidepool/FilePath.hs"), &changed).unwrap();
        let mut shadow_roots = vec![shadow.path().to_owned()];
        shadow_roots.extend(roots);
        let shadowed = crate::artifacts::test_support::compile_targets(
            source,
            &["result"],
            &shadow_roots,
            |_, _, _| {},
        )
        .unwrap();
        let source_witness = shadowed.completed_source_evidence.as_ref().unwrap();
        let shadow_path = shadow.path().join("Tidepool/FilePath.hs");
        assert!(source_witness.sources.iter().any(|source| {
            source.path == shadow_path && source.sha256 == format!("{:x}", Sha256::digest(&changed))
        }));
        assert!(source_witness.modules.iter().any(|module| {
            module.unit == "main"
                && module.module == "Tidepool.FilePath"
                && module.source == shadow_path
                && module.product == crate::cache::ProductAvailability::InterfaceOnly
        }));
        assert!(shadowed
            .certified_groups
            .iter()
            .all(|group| group.owner().module != "Tidepool.FilePath"));
        let selected = shadowed
            .issued_output
            .as_ref()
            .expect("issued compiler output")
            .compiler_inputs();
        let roles = selected.role_projection().snapshot_roles();
        let file_path_role = &roles[&crate::declaration_join::ExactModuleIdentity {
            unit: "main".into(),
            module: "Tidepool.FilePath".into(),
        }];
        assert!(file_path_role.original().is_none());
        assert!(matches!(
            &selected.issued_artifacts().metadata_snapshot().artifacts[&file_path_role.interface()]
                .payload,
            crate::artifact_inventory::ArtifactPayload::Canonical(_)
        ));
        for module in ["Tidepool.FilePath", "Tidepool.Prelude"] {
            let issued_groups = shadowed
                .certified_groups
                .iter()
                .filter(|group| group.owner().module == module)
                .map(|group| (group.origin(), group.owner()))
                .fold(Vec::new(), |mut issuers, issuer| {
                    if !issuers.contains(&issuer) {
                        issuers.push(issuer);
                    }
                    issuers
                });
            let source_witnesses = shadowed.completed_source_evidence.as_ref().map(|evidence| {
                evidence
                    .modules
                    .iter()
                    .filter(|owner| owner.module == module)
                    .collect::<Vec<_>>()
            });
            eprintln!(
                "source shadow {module}: prior_owner={:?}; issued_groups={issued_groups:?}; source_witnesses={source_witnesses:?}",
                owners.get(module)
            );
        }
        for module in ["Tidepool.Prelude"] {
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
            let selected = shadowed
                .issued_output
                .as_ref()
                .expect("issued compiler output")
                .compiler_inputs();
            let original = shadowed
                .recovery_products
                .iter()
                .find(|product| product.owner().module == module)
                .expect("fresh source has a certified Original");
            let original_id = crate::artifact_inventory::ArtifactEntry::original_artifact_id(
                original.module_interface().producer_sha256(),
                original,
            );
            assert!(
                selected
                    .role_projection()
                    .snapshot_roles()
                    .iter()
                    .any(|(owner, role)| {
                        owner.module == module && role.original() == Some(original_id)
                    }),
                "the completed worker, not optional availability, selects the fresh Original"
            );
        }
        assert!(
            shadowed
                .certified_groups
                .iter()
                .any(|group| group.origin() == ProductOrigin::Cached
                    && owners.get(&group.owner().module) == Some(group.owner())),
            "an unaffected original owner survives source shadowing"
        );

        let native_shadow = tempfile::tempdir().unwrap();
        std::fs::create_dir(native_shadow.path().join("Tidepool")).unwrap();
        let mut native_source = changed;
        assert!(!std::str::from_utf8(&native_source)
            .unwrap()
            .contains("{-# NOINLINE normalise #-}"));
        native_source.extend_from_slice(b"\n{-# NOINLINE normalise #-}\n");
        std::fs::write(
            native_shadow.path().join("Tidepool/FilePath.hs"),
            native_source,
        )
        .unwrap();
        let mut native_roots = vec![native_shadow.path().to_owned()];
        native_roots.extend(source_selection.include_roots());
        let native = crate::artifacts::test_support::compile_targets(
            include_str!("../tests/fixtures/deployment-module-package/NativeConsumer.hs"),
            &["result"],
            &native_roots,
            |_, _, _| {},
        )
        .unwrap();
        assert!(native.certified_groups.iter().any(|group| {
            group.owner().module == "Tidepool.FilePath" && group.origin() == ProductOrigin::Fresh
        }));
        assert!(native.certified_groups.iter().all(|group| {
            group.owner().module != "Tidepool.FilePath" || group.origin() != ProductOrigin::Cached
        }));
        let native_selection = native
            .issued_output
            .as_ref()
            .expect("issued compiler output")
            .compiler_inputs();
        let native_original = native
            .recovery_products
            .iter()
            .find(|product| product.owner().module == "Tidepool.FilePath")
            .unwrap();
        let native_id = crate::artifact_inventory::ArtifactEntry::original_artifact_id(
            native_original.module_interface().producer_sha256(),
            native_original,
        );
        assert_eq!(
            native_selection.role_projection().snapshot_roles()
                [&crate::declaration_join::ExactModuleIdentity {
                    unit: "main".into(),
                    module: "Tidepool.FilePath".into(),
                }]
                .original(),
            Some(native_id)
        );
        eprintln!(
            "deployment catalog {}: {} packaged modules, {} cached owners; shadowed FilePath type evidence, fresh Prelude and demanded FilePath Original",
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
        let root = test_support::compiler_input_directory().unwrap();
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
            || prepare_frozen_production_entry(&sources, root.path(), &output, &mut |_| {}),
        );
        let Err(CompileError::CompilerEndpoint(error)) = &refusal.action else {
            panic!(
                "unexpected preparation refusal: {:?}",
                refusal.action.as_ref().err()
            );
        };
        assert_eq!(error.source.kind(), std::io::ErrorKind::WouldBlock);
        assert!(error.definitely_unsubmitted());
        assert!(!error.permits_rebind());
        assert_eq!(refusal.close, CompilerTransactionClose::NotStarted);
        assert_eq!(extract_spawn_count(), before);
        assert!(!output.exists());
        assert!(!root.path().join("entry.preparing").exists());
        let foreground = with_compiler_transaction_for_workload(
            CompileWorkload::Foreground,
            |_| {},
            || prepare_frozen_production_entry(&sources, root.path(), &output, &mut |_| {}),
        );
        foreground.action.unwrap();
        assert_eq!(foreground.close, CompilerTransactionClose::Clean);
        assert_eq!(extract_spawn_count(), before + 1);
        assert!(output.join("raw/certified-products.cbor").is_file());
        assert!(!root.path().join("entry.preparing").exists());

        let _restore = RestoreEnvironment(vec![(
            "TIDEPOOL_KEEP_TEST_LOGS",
            std::env::var_os("TIDEPOOL_KEEP_TEST_LOGS"),
        )]);
        std::env::set_var("TIDEPOOL_KEEP_TEST_LOGS", "1");
        let invalid_root = root.path().join("invalid-sources");
        std::fs::create_dir(&invalid_root).unwrap();
        let invalid_source = invalid_root.join("Broken.hs");
        let invalid_bytes = b"module Broken where\n__prepared = (\n";
        std::fs::write(&invalid_source, invalid_bytes).unwrap();
        let invalid_sources =
            FrozenEntrySources::capture(&[invalid_root], &invalid_source).unwrap();
        let failed_output = root.path().join("invalid-entry");
        let failed = with_compiler_transaction_for_workload(
            CompileWorkload::Foreground,
            |_| {},
            || {
                prepare_frozen_production_entry(
                    &invalid_sources,
                    root.path(),
                    &failed_output,
                    &mut |_| {},
                )
            },
        );
        let Err(CompileError::Diagnostics(diagnostics)) = &failed.action else {
            panic!(
                "malformed source must yield source diagnostics: {:?}",
                failed.action
            );
        };
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, diag::DiagnosticSeverity::Error);
        let span = diagnostics[0].span.as_ref().unwrap();
        assert_eq!(Path::new(&span.file), invalid_source);
        assert_eq!((span.start_line, span.start_col), (3, 1));
        assert_eq!(failed.close, CompilerTransactionClose::Clean);
        let failed_raw = root.path().join("invalid-entry.preparing/raw");
        assert!(!failed_output.exists());
        let status: serde_json::Value = serde_json::from_slice(
            &std::fs::read(failed_raw.join("compiler-status.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(status["success"], false);

        let cancellation = tidepool_extract_cmd::CompilerTransactionCancellation::new();
        cancellation.cancel();
        let cancelled = tidepool_extract_cmd::with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || {
                prepare_frozen_production_entry(
                    &sources,
                    root.path(),
                    &root.path().join("cancelled-entry"),
                    &mut |_| {},
                )
            },
        );
        assert!(matches!(cancelled.action, Err(CompileError::Io(error))
            if error.kind() == std::io::ErrorKind::Interrupted));
        assert_eq!(cancelled.close, CompilerTransactionClose::NotStarted);
        assert!(!root.path().join("cancelled-entry.preparing").exists());
        let retained = root.path().to_owned();
        drop(root);
        assert_eq!(std::fs::read(invalid_source).unwrap(), invalid_bytes);
        assert!(failed_raw.join("compiler.stderr").is_file());
        assert!(failed_raw.join("compiler.stdout").is_file());
        assert!(failed_raw.join("compiler-request.bin").is_file());
        let retained_report = tidepool_extract_report::decode_report(
            &std::fs::read(failed_raw.join("compiler.stdout")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            retained_report.outcome,
            tidepool_extract_report::ExtractOutcome::SourceFailure
        );
        assert_eq!(&retained_report.diagnostics, diagnostics);
        assert!(retained.join("inputs.json").is_file());
        assert!(output.join("raw/certified-products.cbor").is_file());
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
        let mut closes = Vec::new();
        let refusal =
            prepare_frozen_production_entry(&sources, root.path(), &output, &mut |close| {
                closes.push(close)
            });
        assert!(matches!(refusal, Err(CompileError::ExtractFailed(_))));
        assert!(
            closes.is_empty(),
            "reservation refusal must precede any compiler transaction: {closes:?}"
        );
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
        let mut closes = Vec::new();
        let error = production_entry::with_failure(
            production_entry::EntryCheckpoint::ReservationSync,
            || {
                prepare_frozen_production_entry(&sources, root.path(), &output, &mut |close| {
                    closes.push(close)
                })
            },
        )
        .unwrap_err();
        assert!(
            closes.is_empty(),
            "reservation sync failure must precede any compiler transaction: {closes:?}"
        );
        assert!(matches!(error, CompileError::Io(_)));
        assert!(!output.exists());
        let unfinished = root.path().join("entry.preparing");
        assert!(unfinished.is_dir());
        assert!(!unfinished.join("raw").exists());
        let mut retry_closes = Vec::new();
        assert!(matches!(
            prepare_frozen_production_entry(
                &sources,
                root.path(),
                &output,
                &mut |close| retry_closes.push(close),
            ),
            Err(CompileError::EntryPreparationUnfinished { path }) if path == unfinished
        ));
        assert!(
            retry_closes.is_empty(),
            "unfinished reservation refusal must precede any compiler transaction: {retry_closes:?}"
        );
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
                let mut closes = Vec::new();
                let refused =
                    prepare_frozen_production_entry(&sources, root.path(), &output, &mut |close| {
                        closes.push(close)
                    });
                assert!(
                    closes.is_empty(),
                    "unfinished entry refusal must precede any compiler transaction: {closes:?}"
                );
                assert!(matches!(
                    refused,
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
    #[cfg(unix)]
    fn retained_entry_deferred_products_preserve_missing_and_inaccessible_failures() {
        use production_entry::EntryCheckpoint;
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let authored = root.path().join("sources");
        std::fs::create_dir(&authored).unwrap();
        let source = authored.join("Input.hs");
        std::fs::write(&source, "module Input where\n__prepared = (37 :: Int)\n").unwrap();
        let sources = FrozenEntrySources::capture(&[authored], &source).unwrap();
        for (index, (member, missing)) in [
            (CompilerSidecar::ModuleProducts, true),
            (CompilerSidecar::ModulePackageImports, true),
            (CompilerSidecar::ModuleProducts, false),
        ]
        .into_iter()
        .enumerate()
        {
            let output = root.path().join(format!("entry-{index}"));
            let path = output
                .with_extension("preparing")
                .join("raw")
                .join(member.filename());
            let changed = path.clone();
            let error = production_entry::with_checkpoint_observer(
                move |stage| {
                    if stage == EntryCheckpoint::OriginalAdmission {
                        if missing {
                            std::fs::remove_file(&changed).unwrap();
                        } else {
                            std::fs::set_permissions(&changed, std::fs::Permissions::from_mode(0))
                                .unwrap();
                        }
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
            if missing {
                assert!(
                    matches!(error, CompileError::MissingOutput(ref actual) if actual == &path),
                    "{error:?}"
                );
            } else {
                assert!(
                    matches!(&error, CompileError::CompilerEvidence(cause)
                    if matches!(cause.as_ref(), certified_products::CertificationError::EvidenceRead {
                        path: actual, failure: certified_products::EvidenceReadFailure::Io { error, .. },
                    } if actual == &path && error.kind() == std::io::ErrorKind::PermissionDenied)),
                    "{error:?}"
                );
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            assert!(!output.exists());
        }
    }

    #[test]
    #[serial_test::serial]
    fn retained_entry_post_manifest_changes_refuse_before_publication() {
        use production_entry::EntryCheckpoint;
        let root = tempfile::tempdir().unwrap();
        let authored = root.path().join("sources");
        std::fs::create_dir(&authored).unwrap();
        let source = authored.join("Input.hs");
        std::fs::write(&source, "module Input where\n__prepared = (37 :: Int)\n").unwrap();
        let sources = FrozenEntrySources::capture(&[authored], &source).unwrap();
        #[derive(Clone, Copy)]
        enum Mutation {
            Captured,
            Extra,
            Source,
        }
        for mutation in [Mutation::Captured, Mutation::Extra, Mutation::Source] {
            let name = match mutation {
                Mutation::Captured => "captured",
                Mutation::Extra => "extra",
                Mutation::Source => "source",
            };
            let output = root.path().join(name);
            let raw = output.with_extension("preparing").join("raw");
            let changed = raw.clone();
            let changed_source = source.clone();
            let loads = production_entry::entry_load_count();
            let admissions = production_entry::entry_admission_count();
            let error = production_entry::with_checkpoint_observer(
                move |stage| {
                    if stage == EntryCheckpoint::ManifestWrite {
                        let path = match mutation {
                            Mutation::Captured => changed.join("meta.cbor"),
                            Mutation::Extra => changed.join("unconsumed-extra"),
                            Mutation::Source => changed_source.clone(),
                        };
                        std::fs::write(path, b"changed after semantic admission").unwrap();
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
            assert!(matches!(error, CompileError::ExtractFailed(_)));
            assert!(!output.exists());
            assert!(raw.join("certified-products.cbor").exists());
            assert_eq!(production_entry::entry_load_count() - loads, 0);
            assert_eq!(production_entry::entry_admission_count() - admissions, 1);
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
            let admissions = production_entry::entry_admission_count();
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
                            prepare_frozen_production_entry(
                                &sources,
                                root.path(),
                                &output,
                                &mut |_| {},
                            )
                        },
                    )
                },
            );
            assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(
                production_entry::entry_load_count() - loads,
                0,
                "fresh staging never invokes the independent cold reader"
            );
            assert_eq!(
                production_entry::entry_admission_count() - admissions,
                1,
                "cancellation follows the one complete original admission"
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
                let mut closes = Vec::new();
                let refused =
                    prepare_frozen_production_entry(&sources, root.path(), &output, &mut |close| {
                        closes.push(close)
                    });
                assert!(
                    closes.is_empty(),
                    "cancelled unfinished entry must not reopen a compiler transaction: {closes:?}"
                );
                assert!(matches!(
                    refused,
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
        use crate::artifact_inventory::ArtifactDescriptor;
        use crate::declaration_context::{RecoveredArtifactAcquisition, RecoveryPayloadReference};
        use crate::exact_context::{hydrate_context, Materializer};
        use crate::toolchain::CompilerDeploymentConfiguration;
        use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
        let root = test_support::compiler_input_directory().unwrap();
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
        let admissions = production_entry::entry_admission_count();
        let first = crate::artifacts::test_support::prepare_frozen_production_entry(
            &sources,
            root.path(),
            &output,
        )
        .unwrap();
        assert_eq!(
            production_entry::entry_load_count() - before,
            0,
            "fresh preparation hands off the original issuer without cold loading"
        );
        assert_eq!(
            production_entry::entry_admission_count() - admissions,
            1,
            "fresh output is admitted exactly once"
        );
        let observed = production_entry::entry_observed_inputs();
        for name in [
            prepared_artifact_name("__prepared"),
            "meta.cbor".into(),
            "asks.json".into(),
            "module-products.cbor".into(),
            "module-package-imports.cbor".into(),
            "dependencies.json".into(),
            "certified-products.cbor".into(),
            "compiler-inputs.cbor".into(),
        ] {
            assert!(
                observed.contains(Path::new(&name)),
                "actual entry admission did not observe {name}"
            );
        }
        let original = first
            .products()
            .recovery_products
            .first()
            .expect("real retained entry fixture must issue an original product");
        crate::recovery_artifacts::check_entry_unsealed_input_observations(original);
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
        assert_eq!(production_entry::entry_load_count() - before, 1);
        assert_eq!(
            production_entry::entry_admission_count() - admissions,
            2,
            "independent cold reopen authenticates its own observation once"
        );
        assert_eq!(first.target_owned(), reopened.target_owned());
        assert_eq!(first.source(), reopened.source());
        assert_eq!(
            first.products().artifact_view().descriptors(),
            reopened.products().artifact_view().descriptors()
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
        let original = first.products().original_execution_evidence().unwrap();
        let template = ["module Protected where\nimport QuotedOriginal\n".to_owned()];
        assert!(
            crate::declaration_context::CapturedTemplateImports::capture(&original, &template)
                .unwrap()
                .is_some(),
            "the real sealed output issues original template custody"
        );
        let original_identity = original.identity();
        let captured =
            crate::declaration_context::CapturedTemplateImports::capture(&original, &template)
                .unwrap()
                .unwrap();
        let (mut retained, template_evidence) = captured
            .into_value_support(crate::artifact_inventory::CompilerSourceSupport::empty())
            .unwrap();
        let expected_graph = retained.artifacts().capture_graph_selection();
        let expected_roles = retained.projection().roles();
        let expected_offers = retained.native_offers().to_vec();
        assert!(
            !expected_roles.is_empty(),
            "actual template capture selects canonical interfaces"
        );
        assert!(
            expected_graph.roots.iter().all(|root| matches!(
                root,
                crate::artifact_inventory::ArtifactBindingNode::Artifact(_)
            )),
            "template availability must not create executable group demand"
        );
        for _ in 0..3 {
            let capture =
                crate::declaration_context::CapturedTemplateImports::capture(&original, &template)
                    .unwrap()
                    .unwrap();
            let (next, evidence) = capture.into_value_support(retained).unwrap();
            assert_eq!(*evidence, *template_evidence);
            assert_eq!(next.artifacts().capture_graph_selection(), expected_graph);
            assert_eq!(next.projection().roles(), expected_roles);
            assert_eq!(next.native_offers().len(), expected_offers.len());
            assert!(next
                .native_offers()
                .iter()
                .zip(&expected_offers)
                .all(|(retained, issued)| retained.same_issuance(issued)));
            retained = next;
        }
        assert_eq!(original.identity(), original_identity);
        let retained_context = crate::declaration_context::ExactDeclarationContext::empty()
            .extend_issued_compiler_support(&retained)
            .unwrap();
        assert!(
            retained_context.source_scope().rows().is_empty(),
            "template custody cannot issue SOURCE permission"
        );
        drop(retained);
        drop(template_evidence);
        assert_eq!(
            retained_context.artifact_view().capture_graph_selection(),
            expected_graph,
            "the retained owner preserves exact roots after temporary captures are released"
        );
        drop(retained_context);
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
        let snapshot = original.context().snapshot();
        let store = MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let published = materializer.materialize_context(None, &snapshot).unwrap();
        for (cid, bytes) in materializer.finish().unwrap().into_blocks() {
            store.put_keyed(&cid, &bytes).unwrap();
        }
        let restored = hydrate_context(&store, published.root()).unwrap();
        let graph = restored.snapshot().graph_selection().unwrap();
        let mut artifacts = graph
            .bindings
            .iter()
            .map(|binding| binding.artifact)
            .collect::<BTreeSet<_>>();
        artifacts.extend(snapshot.native_package_artifacts_introduced(None));
        let recovery = root.path().join("recovered-original");
        std::fs::create_dir(&recovery).unwrap();
        let references = original
            .context()
            .materialize_recovery_artifacts(&recovery, &artifacts)
            .unwrap();
        let descriptors = references
            .iter()
            .map(|reference| match reference {
                RecoveryPayloadReference::Original(reference) => {
                    ArtifactDescriptor::from_recovery_product(reference)
                }
                RecoveryPayloadReference::Canonical(reference) => {
                    ArtifactDescriptor::from_recovery_module_interface(reference)
                }
                RecoveryPayloadReference::Join(reference) => {
                    ArtifactDescriptor::from_recovery_join(reference)
                }
                RecoveryPayloadReference::Value(reference) => {
                    ArtifactDescriptor::from_recovery_value_interface(reference)
                }
                RecoveryPayloadReference::PackageCode(reference) => {
                    ArtifactDescriptor::from_recovery_package_code(reference)
                }
            })
            .collect::<Vec<_>>();
        let recovered = RecoveredArtifactAcquisition::new(&recovery)
            .capture_payloads_bound(
                &references,
                &descriptors,
                &graph.bindings,
                &restored.snapshot().binding_dependencies().unwrap(),
            )
            .unwrap()
            .context_for_expected_snapshot(published.root(), &restored)
            .unwrap();
        assert_eq!(
            recovered.lexical_graph(),
            original.context().lexical_graph()
        );
        assert_eq!(
            recovered.semantic_sha256(),
            original.context().semantic_sha256()
        );
        assert!(
            crate::declaration_context::CapturedTemplateImports::capture(&original, &[])
                .unwrap()
                .is_none(),
            "no selected template roots require no original completion"
        );
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
        let source_validation_operation =
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
        use crate::declaration_join::{ExactDeclarationContext, ExactModuleIdentity};
        use tidepool_repr::execution_schema::{Atom, Group, HeapRhs, ScalarLiteral};

        let root = test_support::compiler_input_directory().unwrap();
        let cache = root.path().join("compile-cache");
        std::fs::create_dir(&cache).unwrap();
        let _restore = RestoreEnvironment(
            ["TIDEPOOL_COMPILE_CACHE_DIR"]
                .into_iter()
                .map(|name| (name, std::env::var_os(name)))
                .collect(),
        );
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", &cache);
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
        let context_support =
            crate::artifact_inventory::CompilerSourceSupport::close_complete_output(
                context_products
                    .issued_output
                    .as_ref()
                    .unwrap()
                    .compiler_inputs(),
            )
            .unwrap();
        let context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(
                &context_support
                    .selected_interfaces()
                    .unwrap()
                    .support_for_owners(vec![context_owner])
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
                assert!(
                    !admission.evidence().cache_safe && !admission.evidence().selection_complete
                );
                assert!(admission.evidence().revalidate(source).is_ok());
                // The native test owns this completed quotation output; it
                // does not issue a replay recipe or a hermetic build artifact.
                let consumer = root.path().join("QuotedConsumer.hs");
                std::fs::write(&consumer, source).unwrap();
                let mut observations = admission.evidence().as_ref().clone().into_evidence();
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
                validate_completed_corpus_sources(
                    &source_validation_operation,
                    &observations,
                    &consumer,
                    &include,
                )
                .unwrap();
                assert!(
                    validate_prepared_fixture_sources(
                        &source_validation_operation,
                        &observations,
                        &consumer,
                        &include
                    )
                    .is_err(),
                    "completed TH output cannot become an immutable build recipe"
                );
                assert!(
                    validate_completed_corpus_sources(
                        &source_validation_operation,
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
                    validate_completed_corpus_sources(
                        &source_validation_operation,
                        &observations,
                        &consumer,
                        &include
                    )
                    .is_err(),
                    "completed output still validates consumed source bytes"
                );
                std::fs::write(&provider, provider_bytes).unwrap();
                let shadow = earlier.join("QuotedOriginal.hs");
                std::fs::write(&shadow, "module QuotedOriginal where\nvalue = 0\n").unwrap();
                assert!(
                    validate_completed_corpus_sources(
                        &source_validation_operation,
                        &observations,
                        &consumer,
                        &include
                    )
                    .is_err(),
                    "completed output still validates negative import witnesses"
                );
                std::fs::remove_file(shadow).unwrap();
                validate_completed_corpus_sources(
                    &source_validation_operation,
                    &observations,
                    &consumer,
                    &include,
                )
                .unwrap();
                let surface = crate::declaration_join::source_lexical_closure(
                    std::slice::from_ref(&quoted_owner),
                    admission.consumed_home_adjacency(),
                    &[],
                    &compiled.artifact_view.retained_source_implementations(),
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
                let output = compiled
                    .issued_output
                    .as_ref()
                    .expect("issued compiler output")
                    .compiler_inputs();
                let compiled_support =
                    crate::artifact_inventory::CompilerSourceSupport::close_complete_output(output)
                        .unwrap();
                let interfaces = compiled_support
                    .selected_interfaces()
                    .unwrap()
                    .support_for_owners(owners)
                    .unwrap();
                let original = compiled_support
                    .source_support(&BTreeSet::from([quoted_owner.clone()]))
                    .unwrap();
                let context = ExactDeclarationContext::new(&[], &[], vec![])
                    .unwrap()
                    .extend_issued_compiler_support(&interfaces)
                    .unwrap()
                    .extend_issued_compiler_support(&original)
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
            let original_support =
                crate::artifact_inventory::CompilerSourceSupport::close_complete_output(
                    original.issued_output.as_ref().unwrap().compiler_inputs(),
                )
                .unwrap();
            let interfaces = original_support
                .selected_interfaces()
                .unwrap()
                .support_for_owners(vec![owner.clone(), support.clone()])
                .unwrap();
            let context = ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_issued_compiler_support(&interfaces)
                .unwrap()
                .extend(&[], &[], lexical[..2].to_vec())
                .unwrap();
            let extended = context
                .clone()
                .extend_issued_compiler_support(
                    &original_support
                        .selected_interfaces()
                        .unwrap()
                        .support_for_owners(vec![unrelated.clone()])
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
            .merge_fixture_selections(&second_native)
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
        assert!(first_native
            .merge_fixture_selections(&native_root(&changed))
            .is_err());
    }

    #[test]
    #[ignore = "requires matched Haskell worker and frontend"]
    #[serial_test::serial]
    fn real_worker_reuses_dependency_after_consumer_changes() {
        fn compile_observed(
            request: &str,
            source: &str,
            root: &Path,
        ) -> (CompiledArtifacts, usize) {
            use crate::module_candidates::publication_observer::{self, Stage};
            use sha2::{Digest, Sha256};
            let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let observed = events.clone();
            let compiled = publication_observer::with_observer(
                move |event| observed.borrow_mut().push(event),
                || {
                    crate::artifacts::test_support::compile_targets(
                        source,
                        &["consume"],
                        &[root.to_owned()],
                        |_, _, _| {},
                    )
                },
            )
            .unwrap();
            // Count actual successful serialization calls and completed writes,
            // associated by the issuer's exact proof reference in this request.
            let mut work = std::collections::BTreeMap::<String, (u64, u64, u64, u64)>::new();
            let mut publications = 0;
            for event in events.borrow().iter() {
                match event {
                    Stage::ProofEncoded { sha256, bytes } => {
                        let row = work.entry(sha256.clone()).or_default();
                        row.0 += 1;
                        row.1 += bytes;
                    }
                    Stage::ProofPublished {
                        sha256,
                        bytes,
                        path,
                    } => {
                        let written = std::fs::read(path).expect("observed proof write exists");
                        assert_eq!(written.len() as u64, *bytes);
                        assert_eq!(format!("{:x}", Sha256::digest(&written)), sha256.as_str());
                        if let Some(artifacts) = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
                            let directory = PathBuf::from(artifacts)
                                .join("publication-proofs")
                                .join(request);
                            std::fs::create_dir_all(&directory).unwrap();
                            std::fs::write(directory.join(format!("{sha256}.cbor")), written)
                                .unwrap();
                        }
                        let row = work.entry(sha256.clone()).or_default();
                        row.2 += 1;
                        row.3 += bytes;
                        publications += 1;
                    }
                    _ => {}
                }
            }
            for (sha256, (encodes, encoded_bytes, writes, written_bytes)) in &work {
                if *writes != 0 {
                    assert!(
                        *encodes != 0,
                        "published proof encoding must be observed on this request"
                    );
                }
                eprintln!("publication-proof-work request={request} sha256={sha256} successful_encodes={encodes} encoded_bytes={encoded_bytes} completed_writes={writes} written_bytes={written_bytes}");
            }
            (compiled, publications)
        }
        let cache = tempfile::tempdir().unwrap();
        let old_cache = std::env::var_os("TIDEPOOL_COMPILE_CACHE_DIR");
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        }
        let work = tempfile::tempdir().unwrap();
        let root = work.path().to_path_buf();
        std::fs::write(
            root.join("ModuleProductA.hs"),
            include_str!("../../../bridge/haskell/test-prepared-stg/ModuleProductA.hs"),
        )
        .unwrap();
        let first = include_str!("../../../bridge/haskell/test-prepared-stg/ModuleProductB.hs");
        let (first_artifacts, first_artifacts_publications) =
            compile_observed("first", &first, &root);
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
        let (second, second_publications) = compile_observed("consumer-changed", &changed, &root);
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
        let (third, third_publications) =
            compile_observed("dependency-changed", &changed_again, &root);
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
        assert!(
            first_artifacts_publications + second_publications + third_publications > 0,
            "real candidate control must observe a completed ordinary proof publication"
        );
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
    fn portable_assembly_refuses_all_unexported_import_owners() {
        use tidepool_repr::execution_schema::testing;
        let metadata = write_metadata(&DataConTable::new(), &MetaWarnings::default()).unwrap();
        let target = |wire: &tidepool_repr::execution_schema::WireProgram| RawTargetOutput {
            target: "entry".into(),
            asks_bytes: Arc::new(b"[]".to_vec()),
            prepared_bytes: Arc::new(tidepool_test_data::prepared_encode::encode_wire_program(
                wire,
            )),
        };
        let closed = testing::wire_program();
        assemble_portable(&metadata, &[target(&closed)], |_, _, _| {}).unwrap();
        for (unit, generation) in [("main", None), ("package-unit", None), ("main", Some(9))] {
            let mut open = tidepool_test_data::prepared::callable_import_program();
            open.globals[0].identity.unit = unit.into();
            open.globals[0].required_generation = generation;
            assemble(&metadata, &[target(&open)], |_, _, _| {}).unwrap();
            assert!(
                matches!(
                    assemble_portable(&metadata, &[target(&open)], |_, _, _| {}),
                    Err(CompileError::ExtractFailed(message))
                        if message == "prepared target globals require certified owners"
                ),
                "portable output cannot borrow any absent external authority"
            );
        }
    }

    #[test]
    fn captured_invocation_output_ignores_mutated_diagnostic_files() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = prepared_fixture_bytes();
        let artifact = PreparedArtifact::parse(bytes.clone(), DecodeLimits::default()).unwrap();
        let mut table = DataConTable::new();
        for decl in artifact.prepared().constructors() {
            table.insert_checked(matching_dc(decl)).unwrap();
        }
        let mut warnings = MetaWarnings::default();
        warnings.captured_type = Some("captured observation".into());
        let metadata = write_metadata(&table, &warnings).unwrap();
        std::fs::write(directory.path().join("__prepared.prepared.cbor"), bytes).unwrap();
        std::fs::write(directory.path().join("meta.cbor"), &metadata).unwrap();
        std::fs::write(directory.path().join("asks.json"), b"[]").unwrap();
        // Layout capture retains untrusted bytes. Only completed native
        // acquisition can authenticate the receipt and issue native products.
        std::fs::write(
            directory.path().join("certified-products.cbor"),
            b"unadmitted receipt observation",
        )
        .unwrap();
        let layout = invocation_output::InvocationOutputLayout::original(&["__prepared"]).unwrap();
        let captured = layout
            .read(
                directory.path(),
                Arc::new(tidepool_repr::execution_schema::InventoryOperation::new(
                    Default::default(),
                )),
            )
            .unwrap();
        let retained = assemble(captured.metadata(), &captured.targets(), |_, _, _| {}).unwrap();
        let target = Arc::clone(retained.targets["__prepared"].prepared.prepared_shared());
        std::fs::write(
            directory.path().join("__prepared.prepared.cbor"),
            b"invalid",
        )
        .unwrap();
        std::fs::write(directory.path().join("meta.cbor"), b"invalid").unwrap();
        drop(directory);
        assert!(Arc::ptr_eq(
            &target,
            retained.targets["__prepared"].prepared.prepared_shared()
        ));
        assert_eq!(captured.metadata(), metadata.as_slice());
        let assembled = assemble(captured.metadata(), &captured.targets(), |_, _, _| {}).unwrap();
        assert_eq!(assembled.table, table);
        assert_eq!(assembled.warnings.captured_type, warnings.captured_type);
        for output in [&retained, &assembled] {
            assert!(output.producer_identity.is_none());
            assert!(output.issued_output.is_none());
        }
    }

    #[test]
    fn captured_invocation_metadata_never_reopens_a_changed_file_for_the_same_target() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = prepared_fixture_bytes();
        let artifact = PreparedArtifact::parse(bytes.clone(), DecodeLimits::default()).unwrap();
        let mut table = DataConTable::new();
        for decl in artifact.prepared().constructors() {
            table.insert_checked(matching_dc(decl)).unwrap();
        }
        let mut warnings = MetaWarnings::default();
        warnings.captured_type = Some("captured observation".into());
        let original = write_metadata(&table, &warnings).unwrap();
        std::fs::write(directory.path().join("__prepared.prepared.cbor"), &bytes).unwrap();
        std::fs::write(directory.path().join("asks.json"), b"[]").unwrap();
        std::fs::write(
            directory.path().join("certified-products.cbor"),
            b"unadmitted receipt observation",
        )
        .unwrap();
        let layout = invocation_output::InvocationOutputLayout::original(&["__prepared"]).unwrap();
        // Both table-only and warning-only changes remain valid for exactly P.
        for changed_table in [false, true] {
            std::fs::write(directory.path().join("meta.cbor"), &original).unwrap();
            let captured = layout
                .read(
                    directory.path(),
                    Arc::new(tidepool_repr::execution_schema::InventoryOperation::new(
                        Default::default(),
                    )),
                )
                .unwrap();
            let output = assemble(captured.metadata(), &captured.targets(), |_, _, _| {}).unwrap();
            assert!(output.producer_identity.is_none());
            assert!(output.issued_output.is_none());
            let target = Arc::clone(output.targets["__prepared"].prepared.prepared_shared());
            let mut alternate = table.clone();
            if changed_table {
                let decl = artifact.prepared().constructors().first().unwrap();
                alternate = table_with_one_entry_mutated(&artifact, decl, |dc| {
                    dc.qualified_name = Some("another.display.alias".into())
                });
            }
            let changed_warnings = if changed_table {
                warnings.clone()
            } else {
                MetaWarnings::default()
            };
            let replacement = write_metadata(&alternate, &changed_warnings).unwrap();
            assert_ne!(original, replacement);
            let (replacement_table, replacement_warnings) =
                tidepool_repr::serial::read_metadata_for_program(&replacement, &target).unwrap();
            assert_eq!(replacement_table == table, !changed_table);
            assert_eq!(
                replacement_warnings.captured_type == warnings.captured_type,
                changed_table
            );
            std::fs::write(directory.path().join("meta.cbor"), replacement).unwrap();
            let decoded = assemble(captured.metadata(), &captured.targets(), |_, _, _| {}).unwrap();
            assert!(Arc::ptr_eq(
                &target,
                output.targets["__prepared"].prepared.prepared_shared()
            ));
            assert_eq!(captured.metadata(), original.as_slice());
            assert_eq!(decoded.table, table);
            assert_eq!(decoded.warnings.captured_type, warnings.captured_type);
            std::fs::remove_file(directory.path().join("meta.cbor")).unwrap();
            assert_eq!(
                assemble(captured.metadata(), &captured.targets(), |_, _, _| {})
                    .unwrap()
                    .table,
                table
            );
        }
        // A later valid file cannot repair the actual invalid observation.
        std::fs::write(directory.path().join("meta.cbor"), b"invalid").unwrap();
        let invalid = layout
            .read(
                directory.path(),
                Arc::new(tidepool_repr::execution_schema::InventoryOperation::new(
                    Default::default(),
                )),
            )
            .unwrap();
        assert!(assemble(invalid.metadata(), &invalid.targets(), |_, _, _| {}).is_err());
        std::fs::write(directory.path().join("meta.cbor"), original).unwrap();
        assert!(assemble(invalid.metadata(), &invalid.targets(), |_, _, _| {}).is_err());
        let repaired = layout
            .read(
                directory.path(),
                Arc::new(tidepool_repr::execution_schema::InventoryOperation::new(
                    Default::default(),
                )),
            )
            .unwrap();
        let assembled = assemble(repaired.metadata(), &repaired.targets(), |_, _, _| {}).unwrap();
        assert_eq!(assembled.table, table);
        assert_eq!(assembled.warnings.captured_type, warnings.captured_type);
        assert!(assembled.producer_identity.is_none());
        assert!(assembled.issued_output.is_none());
    }

    /// A table entry that agrees with `decl` on every fact this crate checks
    /// (id, occurrence name, field count) — what a table from the SAME
    /// compile as `decl`'s prepared program necessarily contains.
    fn matching_dc(decl: &ConstructorDecl) -> DataCon {
        DataCon {
            identity: decl.identity.clone(),
            id: decl.host_id,
            name: decl.identity.symbol.occurrence.clone(),
            tag: decl.tag,
            rep_arity: decl.field_reps.len() as u32,
            field_bangs: vec![SrcBang::NoSrcBang; decl.field_reps.len()],
            qualified_name: None,
            type_name: decl.family().symbol.occurrence.clone(),
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
            asks_bytes: Arc::new(b"[]".to_vec()),
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
            dc.identity.symbol.occurrence = dc.name.clone();
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
            dc.identity.symbol.unit.push_str("-other")
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
        let offer = offer.apply_to(command, &output).unwrap();
        let command = offer.command();
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
    /// for package selection across repeated requests and home shadow changes.
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
        assert!(compile(), "fresh request compiles");
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
    fn continuation_support_preserves_hidden_exact_dependencies_and_backedges() {
        use crate::artifact_inventory::{ArtifactBindingNode, ArtifactEntry, ArtifactInventory};
        use crate::certified_products::fixture_module_interface;
        let producer = [1; 32];
        let generated_interface =
            fixture_module_interface(producer, "fixture", "Generated", BTreeMap::new());
        let support_interface =
            fixture_module_interface(producer, "fixture", "Support", BTreeMap::new());
        for backedge in [false, true] {
            let generated = ArtifactEntry::canonical(fixture_module_interface(
                producer,
                "fixture",
                "Generated",
                BTreeMap::from([(
                    ("fixture".into(), "Support".into()),
                    support_interface.interface_sha256(),
                )]),
            ));
            let support = ArtifactEntry::canonical(if backedge {
                fixture_module_interface(
                    producer,
                    "fixture",
                    "Support",
                    BTreeMap::from([(
                        ("fixture".into(), "Generated".into()),
                        generated_interface.interface_sha256(),
                    )]),
                )
            } else {
                support_interface.clone()
            });
            let generated_id = generated.descriptor.id;
            let support_id = support.descriptor.id;
            let inventory = ArtifactInventory::default();
            let all = inventory
                .admit(&inventory.empty_view(), vec![generated, support])
                .unwrap();
            let detached = all.select_roots(vec![generated_id]).unwrap();
            let graph = detached.capture_graph_selection();
            assert_eq!(graph.roots.len(), 1);
            assert!(graph
                .bindings
                .iter()
                .any(|binding| binding.artifact == support_id));
            assert!(
                matches!(graph.roots[0], ArtifactBindingNode::Artifact(binding)
                if binding.artifact == generated_id)
            );
            let selected = program_support_artifacts(
                &detached,
                &crate::declaration_join::ExactModuleIdentity {
                    unit: "fixture".into(),
                    module: "Generated".into(),
                },
            );
            if backedge {
                assert!(matches!(selected, Err(CompileError::ExtractFailed(_))));
            } else {
                let selected = selected.unwrap();
                assert_eq!(selected.artifact_ids(), vec![support_id]);
                assert_eq!(
                    selected.capture_graph_selection().bindings,
                    graph
                        .bindings
                        .into_iter()
                        .filter(|binding| binding.artifact == support_id)
                        .collect::<Vec<_>>()
                );
            }
        }
    }

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
                assert!(matches!(result, Err(CompileError::ExtractFailed(_))));
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
        let directory = test_support::compiler_input_directory().unwrap();
        {
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
            let product = &products[0];
            let requirements =
                crate::certified_products::original_native_requirements(product).unwrap();
            for (required_owner, dependency) in &requirements.artifact_edges {
                assert_eq!(required_owner.unit, product.owner().unit);
                assert_eq!(required_owner.module, product.owner().module);
                let crate::artifact_inventory::ArtifactDependency::NativeGroup {
                    dependent_ordinal,
                    required_ordinal,
                } = dependency
                else {
                    panic!("independent support retains a nonlocal native binding: {dependency:?}");
                };
                assert!(requirements.group_ordinals.contains(dependent_ordinal));
                assert!(requirements.group_ordinals.contains(required_ordinal));
            }
            let issued = certificate
                .compiler_source_support()
                .source_support(&std::collections::BTreeSet::from([owner.clone()]))
                .expect("select the actual compiler's issued support and physical cohort");
            let selected = issued.projection().snapshot_roles();
            assert_eq!(selected.len(), 1);
            assert!(selected
                .get(&owner)
                .and_then(|role| role.original())
                .is_some());
            let persisted =
                crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
                    .unwrap()
                    .extend_issued_compiler_support(&issued)
                    .unwrap();
            let authored_id = crate::artifact_inventory::ArtifactEntry::original_artifact_id(
                certificate.toolchain_identity_sha256(),
                certificate.product(),
            );
            // Origin lookup observes custody; compiler roles and executable
            // demand remain independently narrowed to the support owner.
            assert_eq!(persisted.authored_native_root(1).unwrap(), authored_id);
            let authored_owner = crate::declaration_join::ExactModuleIdentity {
                unit: certificate.product().owner().unit.clone(),
                module: certificate.product().owner().module.clone(),
            };
            assert!(!persisted
                .compiler_input_projection()
                .snapshot_roles()
                .contains_key(&authored_owner));
            assert!(persisted
                .artifact_view()
                .selected_native_groups()
                .iter()
                .all(|group| group.artifact != authored_id));
            let (endpoint, _) = crate::toolchain::bind_extract_endpoint().unwrap();
            let offer = ModuleCandidateOffer::select_inspection(
                endpoint.identity().producer_bytes(),
                &includes,
                &root.join("scope"),
                Some(Arc::new(crate::declaration_join::ExactCompileContext::new(
                    Arc::new(persisted),
                ))),
                Vec::new(),
                &[],
            )
            .expect("inspection owns the checked purpose and current source roots");
            let request = offer
                .exact
                .as_ref()
                .expect("inspection supplies its exact request")
                .clone();
            let selected_products = request.compiler_original_products().unwrap();
            assert_eq!(selected_products.len(), 1);
            assert_eq!(selected_products[0].owner().unit, owner.unit);
            assert_eq!(selected_products[0].owner().module, owner.module);
            crate::declaration_context::assert_source_selected_receipt_pairing(
                root, module, original, support, request, endpoint, offer,
            );
        }
    }
}

#[cfg(test)]
#[path = "artifacts/tests/exact_source_fixture.rs"]
mod exact_source_fixture;

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use tidepool_extract_cmd::CompilerTransactionClose;

    pub(crate) use super::exact_source_fixture::ExactSourceFixture;
    pub(crate) use super::world_value_selection_tests::prepare_checked_value_fixture;

    /// These callers declare an owned-resident compiler, so the isolated runner
    /// owns the input tree through case settlement, independently of automatic
    /// compiler capture. Local drop, unwind and cancellation cannot erase it.
    /// Only the runner disposes successful evidence after confirmed cleanup.
    pub(crate) fn compiler_input_directory() -> std::io::Result<TempDir> {
        let root = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
            .map(PathBuf::from)
            .ok_or_else(|| {
                std::io::Error::other("compiler inputs require runner artifact custody")
            })?;
        if !root.is_absolute() || !std::fs::symlink_metadata(&root)?.is_dir() {
            return Err(std::io::Error::other(
                "compiler input artifact root must be an existing absolute directory",
            ));
        }
        let inputs = root.join("compiler-inputs");
        std::fs::create_dir_all(&inputs)?;
        let mut directory = TempDir::new_in(inputs)?;
        directory.disable_cleanup(true);
        std::fs::write(
            directory.path().join("inputs.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1, "kind": "compiler-test-inputs", "authority": false,
                "original_directory": directory.path(), "case_artifact_root": root,
                "cleanup_owner": "isolated-libtest-case",
            }))
            .map_err(std::io::Error::other)?,
        )?;
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "compiler inputs owned by case artifact root: {}",
            directory.path().display()
        );
        Ok(directory)
    }

    /// One actual ordinary compile retained through its sealed output owner.
    /// Metadata names the captured source products; it grants no completion.
    pub(crate) struct SourcePublicationEntryFixture {
        _directory: TempDir,
        pub(crate) entry: ProductionEntryOutput,
        pub(crate) root: Arc<crate::artifact_inventory::ArtifactEntry>,
        pub(crate) helper: Arc<crate::artifact_inventory::ArtifactEntry>,
        pub(crate) root_ordinals: [u32; 2],
        pub(crate) producer_bytes: Vec<u8>,
        pub(crate) includes: Vec<PathBuf>,
    }

    pub(crate) fn prepare_source_publication_entry_fixture(
    ) -> Result<SourcePublicationEntryFixture, CompileError> {
        use crate::artifact_inventory::ArtifactPayload;
        let directory = compiler_input_directory()?;
        let sources = directory.path().join("sources");
        std::fs::create_dir(&sources)?;
        for (name, source) in [
            (
                "PublicationHelper.hs",
                include_str!("../tests/fixtures/sealed-publication/PublicationHelper.hs"),
            ),
            (
                "SourceOriginalSupport.hs",
                include_str!("../tests/fixtures/sealed-publication/SourceOriginalSupport.hs"),
            ),
            (
                "PublicationTarget.hs",
                include_str!("../tests/fixtures/sealed-publication/PublicationTarget.hs"),
            ),
        ] {
            std::fs::write(sources.join(name), source)?;
        }
        let target = sources.join("PublicationTarget.hs");
        let includes = vec![sources];
        let frozen = FrozenEntrySources::capture(&includes, &target)?;
        let endpoint = ExtractCmd::new()
            .map_err(|error| CompileError::Io(error.into()))?
            .bind()
            .map_err(|error| CompileError::Io(extract_spawn_error(error.source)))?;
        let producer_bytes = endpoint.identity().producer_bytes().to_vec();
        let entry = prepare_frozen_production_entry(
            &frozen,
            directory.path(),
            &directory.path().join("entry"),
        )?;
        let context = entry.products().original_execution_context()?;
        let original = |module: &str| {
            context
                .artifact_view()
                .entries()
                .into_iter()
                .find(|entry| {
                    entry.descriptor.owner.unit == "main"
                        && entry.descriptor.owner.module == module
                        && matches!(&entry.payload, ArtifactPayload::Original(_))
                })
                .unwrap_or_else(|| panic!("the actual producer captured {module}"))
        };
        let root = original("SourceOriginalSupport");
        let helper = original("PublicationHelper");
        assert_eq!(
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                &producer_bytes
            )
            .sha256(),
            root.descriptor.producer_sha256,
            "metadata and sealed entry use the same actual producer",
        );
        let ArtifactPayload::Original(product) = &root.payload else {
            unreachable!()
        };
        let ordinal = |name: &str| {
            let ordinals = root
                .native_group_ordinals
                .iter()
                .copied()
                .filter(|ordinal| {
                    crate::certified_products::original_native_group_definitions(product, *ordinal)
                        .unwrap()
                        .export_contracts()
                        .any(|contract| {
                            let contract = contract.unwrap();
                            contract.identity.unit == root.descriptor.owner.unit
                                && contract.identity.module == root.descriptor.owner.module
                                && contract.identity.occurrence == name
                        })
                })
                .collect::<Vec<_>>();
            assert_eq!(ordinals.len(), 1, "one actual group exports {name}");
            ordinals[0]
        };
        let root_ordinals = [ordinal("first"), ordinal("second")];
        assert_ne!(root_ordinals[0], root_ordinals[1]);
        Ok(SourcePublicationEntryFixture {
            _directory: directory,
            entry,
            root,
            helper,
            root_ordinals,
            producer_bytes,
            includes,
        })
    }

    // The operation owns this observation outside the induced unwind boundary.
    // These tests expect settled compilers; uncertainty is inspected as a failure.
    pub(crate) fn with_settlement<T, E: std::fmt::Debug>(
        action: impl FnOnce(&mut dyn FnMut(CompilerTransactionClose)) -> Result<T, E>,
    ) -> Result<T, E> {
        let observation = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recipient = Arc::clone(&observation);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            action(&mut |close| recipient.lock().unwrap().push(close))
        }));
        let result = match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        let closes = observation.lock().unwrap();
        if result.is_ok() {
            assert!(
                !closes.is_empty(),
                "compiler close was not observed: {closes:?}"
            );
        }
        assert!(
            closes.iter().all(|close| matches!(
                close,
                CompilerTransactionClose::Clean | CompilerTransactionClose::NotStarted
            )),
            "compiler close is unconfirmed: {closes:?}; action error: {:?}",
            result.as_ref().err()
        );
        drop(closes);
        result
    }

    #[cfg(test)]
    mod tests {
        use super::with_settlement;
        use std::panic::{catch_unwind, AssertUnwindSafe};
        use tidepool_extract_cmd::{
            CompilerTransactionClose, CompilerTransactionCloseEvidence,
            CompilerTransactionCloseReason, CompilerTransactionRetirement,
        };

        fn panic_text(panic: Box<dyn std::any::Any + Send>) -> String {
            if let Some(text) = panic.downcast_ref::<&str>() {
                (*text).to_string()
            } else if let Some(text) = panic.downcast_ref::<String>() {
                text.clone()
            } else {
                "non-string panic".into()
            }
        }

        #[test]
        fn settlement_rejects_an_unobserved_close() {
            let panic = catch_unwind(AssertUnwindSafe(|| with_settlement(|_| Ok::<_, ()>(()))))
                .expect_err("empty close observations are not settled");
            let text = panic_text(panic);
            assert!(text.contains("compiler close was not observed"), "{text}");
            assert!(!text.contains("compiler close is unconfirmed"), "{text}");
        }

        #[test]
        fn settlement_distinguishes_an_unconfirmed_close() {
            let close =
                CompilerTransactionClose::Unconfirmed(CompilerTransactionCloseEvidence::new(
                    CompilerTransactionCloseReason::Abandoned,
                    CompilerTransactionRetirement::DaemonUnobserved { disconnect: None },
                ));
            let panic = catch_unwind(AssertUnwindSafe(|| {
                with_settlement(|recipient| {
                    recipient(close);
                    Ok::<_, ()>(())
                })
            }))
            .expect_err("an unconfirmed close is not settled");
            let text = panic_text(panic);
            assert!(text.contains("compiler close is unconfirmed"), "{text}");
            assert!(!text.contains("compiler close was not observed"), "{text}");
        }

        #[test]
        fn settlement_preserves_an_early_action_error_without_a_close() {
            let result = with_settlement(|_| Err::<(), _>("pre-submission refusal"));
            assert_eq!(result, Err("pre-submission refusal"));
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
