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

pub use crate::compile_input::SealedCompileInputIdentity;
pub use crate::turn_observations::{decode_turn_nominal_heads, decode_turn_yield_sites};
use serde::Deserialize;
use tempfile::TempDir;
use tidepool_extract_cmd::ExtractCmd;
use tidepool_repr::execution_schema::DecodeLimits;
use tidepool_repr::execution_schema::{PreparedProgram, RawModuleProduct};
use tidepool_repr::serial::{read_metadata, MetaWarnings};
use tidepool_repr::DataConTable;

use crate::prepared_artifact::{prepared_artifact_name, PreparedArtifact};
use crate::{
    cache, certified_products, diag, extract_module_name, extract_spawn_error, module_candidates,
    timing, CompileError,
};

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
pub fn check_source(request: &SourceCheckRequest<'_>) -> Result<(), CompileError> {
    let directory = TempDir::new()?;
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
    let run = endpoint.execute(&command).map_err(|error| {
        offer.retain_execution_failure(
            directory.path(),
            &command,
            CompileError::Io(extract_spawn_error(error.source)),
        )
    })?;
    diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)
        .map(|_| ())
        .map_err(|error| offer.retain_failure(directory.path(), &run.output.stderr, error))
}

/// Parse the source before reserving original module and value identities.
/// This capability contains no checked types or native authority.
pub fn parse_cell_plan(
    specification: Arc<crate::checked_cell::CheckedCellSpecification>,
    include_paths: &[PathBuf],
) -> Result<Arc<crate::cell_plan::ParsedCellPlan>, CompileError> {
    crate::cell_plan::parse(specification, include_paths)
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

impl YieldSite {
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
    /// Bound compiler producer identity for this exact invocation. `None`
    /// means the bundle was assembled from bytes without an endpoint.
    pub producer_identity: Option<[u8; 32]>,
    /// Fresh post-downsweep source graph paired with these products. Immutable
    /// declaration owners have separate protected admission and do not acquire
    /// source lookup witnesses. A later compile must revalidate its own inputs.
    pub module_inventory: Option<Vec<cache::ModuleEvidence>>,
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
    checked_publication_context: Option<Arc<crate::declaration_context::ExactDeclarationContext>>,
    checked_item: Option<crate::checked_cell::CheckedItemOffer>,
    checked_display: Option<crate::checked_cell::CheckedDisplayOffer>,
}

// Protected compilation manifests bind the actual worker search order as well
// as the source and interface recipe. Unsupported path encodings refuse before
// the invocation builder can perform its ordinary lossy CLI conversion.
enum CheckedPurpose {
    Inspection,
    Cell,
    HostInputCell,
    Item,
    HostActivationInput,
    Display,
}

/// The original host witness travels only with its dedicated compiler purpose.
/// Authored and planned cells cannot attach host input signature authority.
pub enum CheckedCellPurpose<'a> {
    Authored,
    HostActivationInput(&'a crate::checked_cell::CanonicalInputTypeWitness),
}

fn checked_search_authorization(
    purpose: CheckedPurpose,
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
    fields[0] = Value::Text(
        match purpose {
            CheckedPurpose::Inspection => "inspection1",
            CheckedPurpose::Cell => "cell-check2",
            CheckedPurpose::HostInputCell => "host-input-check1",
            CheckedPurpose::Item => "checked-item2",
            CheckedPurpose::HostActivationInput => "host-activation-input1",
            CheckedPurpose::Display => "checked-display2",
        }
        .into(),
    );
    fields.push(Value::Array(paths));
    Ok(authorization)
}

fn checked_cell_authorization(
    purpose: CheckedCellPurpose<'_>,
    specification: &crate::checked_cell::CheckedCellSpecification,
    values: &crate::checked_cell::CheckedValueInputs,
    include: &[PathBuf],
) -> Result<Value, CompileError> {
    let mut authorization = specification.manifest_value()?;
    let Value::Array(fields) = &mut authorization else {
        unreachable!("closed cell authorization")
    };
    fields.push(values.baseline_authorization());
    let purpose = match purpose {
        CheckedCellPurpose::Authored => CheckedPurpose::Cell,
        CheckedCellPurpose::HostActivationInput(witness) => {
            if !specification.reserved_declaration_modules.is_empty()
                || specification.cell_source
                    != "sessionInput <- pure (undefined :: TidepoolActivationInput)"
            {
                return Err(CompileError::ExtractFailed(
                    "host input witness requires its reserved binder-only compiler slot".into(),
                ));
            }
            fields.push(crate::checked_cell::encode_signature(witness.signature()));
            CheckedPurpose::HostInputCell
        }
    };
    checked_search_authorization(purpose, authorization, include)
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
    let exclusions = module_candidates::ExactCandidateContext::new(protected, reserved)
        .with_originals(context.recovery_products())
        .with_interface_seals(
            context
                .artifact_view()
                .descriptors()
                .into_iter()
                .map(|descriptor| {
                    (
                        (descriptor.owner.unit, descriptor.owner.module),
                        descriptor.interface_sha256,
                    )
                })
                .collect(),
        );
    Ok(
        module_candidates::select_configured_in_context(producer, include, scratch, &exclusions)?
            .map(Arc::new),
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
                CheckedPlannedCellSlot::Expression {
                    capture, display, ..
                } => {
                    reserved.insert(SessionModule::val(Generation(*capture)).module_name());
                    reserved.insert(SessionModule::val(Generation(*display)).module_name());
                }
            }
        }
    }
    reserved
}

impl ModuleCandidateOffer {
    /// Select immutable source candidates under configured compiler authority.
    /// Executing the offer through its owning turn method can issue input continuity.
    pub fn select_admitted(
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
        include: &[PathBuf],
        scratch: &Path,
    ) -> Result<Self, CompileError> {
        Self::select(endpoint.identity().producer_bytes(), include, scratch)
    }

    /// Execute an ordinary turn into fresh, privately owned outputs and seal
    /// input continuity before returning those outputs to a consumer.
    pub fn execute_admitted_turn(
        &self,
        endpoint: crate::toolchain::AdmittedCompilerEndpoint,
        mut command: ExtractCmd,
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
        let directory = tempfile::tempdir()?;
        command.relocate_turn_outputs(directory.path());
        let run = endpoint.execute(&command).map_err(|error| {
            self.retain_execution_failure(
                directory.path(),
                &command,
                CompileError::ExtractFailed(error.to_string()),
            )
        })?;
        let (turn, native) = if run.success()
            && diag::decode_extract_result(true, &run.output.stdout, &run.output.stderr).is_ok()
        {
            self.read_admitted_turn(directory.path(), request.supports_compile_input_identity())
                .map_err(|error| self.retain_failure(directory.path(), &run.output.stderr, error))?
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
        let (table, warnings) = read_metadata(&output.metadata)?;
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
        Ok(Self {
            selected: module_candidates::select_configured(producer, include, scratch)?
                .map(Arc::new),
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: None,
            checked_cell: None,
            planned_cell: None,
            checked_values: None,
            checked_publication_context: None,
            checked_item: None,
            checked_display: None,
        })
    }

    pub fn select_in_context(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Arc<crate::declaration_context::ExactCompileContext>,
    ) -> Result<Self, CompileError> {
        Ok(Self {
            selected: None,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(
                context
                    .prepare_compilation(&scratch.join("exact-scope"), producer)?
                    .with_source_search_context(include),
            ),
            checked_cell: None,
            planned_cell: None,
            checked_values: None,
            checked_publication_context: None,
            checked_item: None,
            checked_display: None,
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
            CheckedPurpose::Inspection,
            Value::Array(vec![
                Value::Text("inspection1".into()),
                Value::Array(owners.iter().cloned().map(Value::Text).collect()),
                inputs.baseline_authorization(),
            ]),
            include,
        )?;
        Ok(Self {
            selected: immutable_candidates_in_context(
                &context, producer, include, scratch, owners,
            )?,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(
                context
                    .prepare_compilation_with_authorization(
                        &scratch.join("exact-scope"),
                        producer,
                        Some(authorization),
                    )?
                    .with_source_search_context(include)
                    .with_checked_value_imports(inputs.import_authority()),
            ),
            checked_cell: None,
            planned_cell: None,
            checked_values: Some(inputs),
            checked_publication_context: None,
            checked_item: None,
            checked_display: None,
        })
    }

    pub fn select_checked_cell(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_context::ExactCompileContext>>,
        specification: crate::checked_cell::CheckedCellSpecification,
        purpose: CheckedCellPurpose<'_>,
        checked_values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
        retained_interfaces: &[Arc<crate::checked_cell::CheckedValueArtifact>],
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
        let context = checked_value_context(Some(publication_context.clone()), &checked_values)?;
        let authorization =
            checked_cell_authorization(purpose, &specification, &checked_values, include)?;
        Ok(Self {
            selected: immutable_candidates_in_context(
                &context,
                producer,
                include,
                scratch,
                checked_candidate_reservations(&specification, None),
            )?,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(
                compile_context_with_declarations(compile_context, context.clone())
                    .prepare_compilation_with_authorization(
                        &scratch.join("exact-scope"),
                        producer,
                        Some(authorization),
                    )?
                    .with_source_search_context(include)
                    .with_checked_value_imports(checked_values.import_authority())
                    .with_generated_scaffold_imports(
                        std::iter::once(specification.template_source.as_str()).chain(
                            specification
                                .turn_templates
                                .iter()
                                .map(|(_, source)| source.as_str()),
                        ),
                    ),
            ),
            checked_cell: Some(specification),
            planned_cell: None,
            checked_values: Some(checked_values),
            checked_publication_context: Some(publication_context),
            checked_item: None,
            checked_display: None,
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
    ) -> Result<Self, CompileError> {
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
        let mut authorization = specification.manifest_value()?;
        let compile_context = context;
        let publication_context = checked_offer_context(
            compile_context
                .as_ref()
                .map(|context| context.declarations().clone()),
        )?;
        let inputs =
            crate::checked_cell::CheckedValueInputs::capture_checked(values, retained_interfaces)?;
        let context = checked_value_context(Some(publication_context.clone()), &inputs)?;
        let Value::Array(fields) = &mut authorization else {
            unreachable!("closed authorization")
        };
        fields[0] = Value::Text("cell-program1".into());
        fields.push(inputs.baseline_authorization());
        fields.extend(extension);
        fields.push(Value::Array(
            include
                .iter()
                .map(|path| Value::Text(path.to_string_lossy().into_owned()))
                .collect(),
        ));
        Ok(Self {
            selected: immutable_candidates_in_context(
                &context,
                producer,
                include,
                scratch,
                checked_candidate_reservations(&specification, Some(&planned)),
            )?,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(
                compile_context_with_declarations(compile_context, context.clone())
                    .prepare_compilation_with_authorization(
                        &scratch.join("exact-scope"),
                        producer,
                        Some(authorization),
                    )?
                    .with_source_search_context(include)
                    .with_checked_value_imports(inputs.import_authority())
                    .with_generated_scaffold_imports(
                        std::iter::once(specification.template_source.as_str()).chain(
                            specification
                                .turn_templates
                                .iter()
                                .map(|(_, source)| source.as_str()),
                        ),
                    ),
            ),
            checked_cell: Some(specification),
            planned_cell: Some(planned),
            checked_values: Some(inputs),
            checked_publication_context: Some(publication_context),
            checked_item: None,
            checked_display: None,
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
        Self::select_checked_item_for_purpose(
            producer,
            include,
            scratch,
            context,
            item,
            prefix,
            runtime_prefix_digest,
            generation,
            observation_name,
            templates,
            settled_bindings,
            crate::checked_cell::CheckedItemPurpose::Authored,
        )
    }

    /// Compile the protected input interface and preview without issuing an
    /// authored execution item. The runtime owns the affine input admission.
    pub fn select_checked_activation_item(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_context::ExactCompileContext>>,
        item: crate::checked_cell::ExactCheckedItem,
        prefix: crate::checked_cell::ExactCompiledPrefix,
        runtime_prefix_digest: [u8; 32],
        generation: u64,
        templates: &[(String, String)],
        settled_bindings: Vec<(
            String,
            tidepool_repr::execution_schema::SymbolIdentity,
            u64,
            u64,
        )>,
    ) -> Result<Self, CompileError> {
        Self::select_checked_item_for_purpose(
            producer,
            include,
            scratch,
            context,
            item,
            prefix,
            runtime_prefix_digest,
            generation,
            None,
            templates,
            settled_bindings,
            crate::checked_cell::CheckedItemPurpose::HostActivationInput,
        )
    }

    fn select_checked_item_for_purpose(
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
        purpose: crate::checked_cell::CheckedItemPurpose,
    ) -> Result<Self, CompileError> {
        let compile_context = context;
        let context = prefix.with_value_context(checked_offer_context(
            compile_context
                .as_ref()
                .map(|context| context.declarations().clone()),
        )?)?;
        let settled_values = prefix.select_settled_values(settled_bindings)?;
        let checked_item = crate::checked_cell::CheckedItemOffer {
            purpose,
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
        if purpose == crate::checked_cell::CheckedItemPurpose::HostActivationInput {
            checked_item.validate_activation_input()?;
        }
        let mut selected = None;
        let exact = compile_context_with_declarations(compile_context, context.clone())
            .prepare_compilation_authorizing(
                &scratch.join("exact-scope"),
                producer,
                |semantic_sha256| {
                    let authorization = checked_search_authorization(
                        match purpose {
                            crate::checked_cell::CheckedItemPurpose::Authored => {
                                CheckedPurpose::Item
                            }
                            crate::checked_cell::CheckedItemPurpose::HostActivationInput => {
                                CheckedPurpose::HostActivationInput
                            }
                        },
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
                    Ok(authorization)
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
                    .with_generated_scaffold_imports(
                        checked_item
                            .item
                            .turn_templates()
                            .iter()
                            .map(|(_, source)| source.as_str()),
                    ),
            ),
            checked_cell: None,
            planned_cell: None,
            checked_values: None,
            checked_publication_context: None,
            checked_item: Some(checked_item),
            checked_display: None,
        })
    }

    pub fn select_checked_display(
        producer: &[u8],
        include: &[PathBuf],
        scratch: &Path,
        context: Option<Arc<crate::declaration_context::ExactCompileContext>>,
        capture: Arc<crate::checked_cell::ExactCompiledItem>,
        prefix: crate::checked_cell::ExactCompiledPrefix,
        generation: u64,
        admission_digest: [u8; 32],
        budget: u64,
        presented: Vec<String>,
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
        if !crate::checked_cell::same_include_paths(include, capture.item().cell_include()) {
            return Err(CompileError::ExtractFailed(
                "checked display include search order changed".into(),
            ));
        }
        let settled_values = prefix.select_settled_values(settled_bindings)?;
        let display = crate::checked_cell::CheckedDisplayOffer {
            capture,
            prefix,
            generation,
            admission_digest,
            budget,
            presented,
            settled_values,
            is_program: false,
        };
        let mut selected = None;
        let exact = compile_context_with_declarations(compile_context, context.clone())
            .prepare_compilation_authorizing(
                &scratch.join("exact-scope"),
                producer,
                |semantic_sha256| {
                    let authorization = checked_search_authorization(
                        CheckedPurpose::Display,
                        display.authorization(producer, semantic_sha256)?,
                        include,
                    )?;
                    let mut reserved = display
                        .prefix
                        .injected_modules()
                        .into_iter()
                        .collect::<BTreeSet<_>>();
                    reserved.extend(
                        display
                            .capture
                            .item()
                            .reserved_declaration_modules()
                            .iter()
                            .cloned(),
                    );
                    reserved.insert(
                        tidepool_repr::SessionModule::val(tidepool_repr::Generation(
                            display.generation,
                        ))
                        .module_name(),
                    );
                    selected = immutable_candidates_in_context(
                        &context, producer, include, scratch, reserved,
                    )?;
                    Ok(authorization)
                },
            )?;
        Ok(Self {
            selected,
            producer: producer.to_vec(),
            include: include.to_vec(),
            exact: Some(
                exact
                    .with_source_search_context(include)
                    .with_checked_value_imports(display.prefix.import_authority()?)
                    .with_generated_scaffold_imports(
                        display
                            .capture
                            .item()
                            .turn_templates()
                            .iter()
                            .map(|(_, source)| source.as_str()),
                    ),
            ),
            checked_cell: None,
            planned_cell: None,
            checked_values: None,
            checked_publication_context: None,
            checked_item: None,
            checked_display: Some(display),
        })
    }

    /// Retained checked-cell artifact root. Only the sealed request manifest
    /// authorizes inputs from this directory; new directory members do not.
    pub fn checked_value_root(&self) -> Option<&Path> {
        self.checked_values
            .as_ref()
            .map(|inputs| inputs.root())
            .or_else(|| {
                self.checked_item
                    .as_ref()
                    .map(|offer| offer.item.value_input_root())
            })
            .or_else(|| {
                self.checked_display
                    .as_ref()
                    .map(|offer| offer.capture.item().value_input_root())
            })
    }

    /// Retain this request and its selected checked inputs for diagnosis.
    /// The saved files are evidence only; they cannot issue compiler authority.
    pub fn retain_failure(
        &self,
        directory: &Path,
        stderr: &[u8],
        error: CompileError,
    ) -> CompileError {
        retain_compiler_failure_inner(directory, stderr, error, Some(self), None)
    }

    /// Preserve the typed request and selected inputs when transport fails
    /// before compiler diagnostics can be returned. Evidence grants no authority.
    pub fn retain_execution_failure(
        &self,
        directory: &Path,
        command: &ExtractCmd,
        error: CompileError,
    ) -> CompileError {
        retain_compiler_failure_inner(directory, &[], error, Some(self), Some(command))
    }

    fn retain_checked_inputs(&self, destination: &Path) -> std::io::Result<()> {
        if let Some(inputs) = &self.checked_values {
            inputs.retain_diagnostics(None, destination)
        } else if let Some(offer) = &self.checked_item {
            offer
                .item
                .retain_input_diagnostics(&offer.prefix, destination)
        } else if let Some(offer) = &self.checked_display {
            offer
                .capture
                .item()
                .retain_input_diagnostics(&offer.prefix, destination)
        } else {
            Ok(())
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
        let planned = self.admit_planned_declaration(root, exact, specification)?;
        crate::checked_cell::admit_checked_cell(
            root,
            &self.producer,
            exact.semantic_sha256,
            exact.context.clone(),
            self.checked_publication_context.clone().ok_or_else(|| {
                CompileError::ExtractFailed(
                    "checked declaration publication baseline is absent".into(),
                )
            })?,
            &exact.request_sha256,
            specification,
            exact.validate_outputs_with_planned(
                root,
                planned.as_ref().map(|planned| planned.certificate.as_ref()),
            )?,
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
        let mut context = initial.context.clone();
        let mut program_request = initial.clone();
        let mut declarations = BTreeMap::new();
        let mut admissions = Vec::new();
        let mut outputs = BTreeMap::new();
        let mut display_outputs = BTreeMap::new();
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
                let original = effective
                    .admit_planned_declaration(
                        &segment_root,
                        effective.exact.as_ref().expect("exact program offer"),
                        &source_spec,
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
                admissions
                    .extend(program_request.validate_outputs_in_context(&segment_root, &context)?);
                while index < end {
                    program_request = program_request
                        .in_program_context(&root.join("program-inputs"), context.clone())?;
                    let effective = self.program_offer(program_request.clone());
                    let directory = root.join(format!("item-{index}"));
                    let output = effective.read_program_output(&directory)?;
                    let source_admissions = effective
                        .exact
                        .as_ref()
                        .expect("exact program offer")
                        .validate_outputs(&directory)?;
                    context = self.admit_program_support(
                        &mut program_request,
                        context,
                        &output,
                        &source_admissions,
                    )?;
                    admissions.extend(source_admissions);
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
                    if let CheckedPlannedCellSlot::Expression { display, .. } =
                        &planned.slots[index]
                    {
                        program_request = program_request
                            .in_program_context(&root.join("program-inputs"), context.clone())?;
                        let effective = self.program_offer(program_request.clone());
                        let directory = root.join(format!("display-{index}"));
                        let output = effective.read_program_output(&directory)?;
                        let source_admissions = effective
                            .exact
                            .as_ref()
                            .expect("exact program offer")
                            .validate_outputs(&directory)?;
                        context = self.admit_program_support(
                            &mut program_request,
                            context,
                            &output,
                            &source_admissions,
                        )?;
                        admissions.extend(source_admissions);
                        context = self.admit_program_value(
                            context,
                            values.root(),
                            *display,
                            &output.turn,
                        )?;
                        display_outputs.insert(index, output);
                    }
                    index += 1;
                }
            }
            segment += 1;
        }
        let cell = checked_cell::admit_checked_cell(
            root,
            &self.producer,
            initial.semantic_sha256,
            initial.context.clone(),
            self.checked_publication_context.clone().ok_or_else(|| {
                CompileError::ExtractFailed("checked program publication baseline is absent".into())
            })?,
            &initial.request_sha256,
            specification,
            admissions,
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
            let item = cell.item(index)?;
            let completed = prefix.as_ref().expect("nonempty program prefix");
            if item.kind() == CheckedItemKind::Declaration {
                prefix = Some(completed.append_declaration(item.clone())?);
                items.push(CellProgramItem {
                    checked: item,
                    native: None,
                    display: None,
                    native_observations: None,
                    display_observations: None,
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
                purpose: checked_cell::CheckedItemPurpose::Authored,
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
            )?;
            let mut next = completed.append(native.clone())?;
            let display =
                if let CheckedPlannedCellSlot::Expression { display, .. } = planned.slots[index] {
                    let output = display_outputs.get(&index).ok_or_else(|| {
                        CompileError::ExtractFailed("program presentation target is missing".into())
                    })?;
                    let proof = checked_cell::CheckedDisplayOffer {
                        capture: native.clone(),
                        prefix: next.clone(),
                        generation: display,
                        admission_digest: cell.admission_digest(),
                        budget: 0,
                        presented: Vec::new(),
                        settled_values: next.prepared_value_selection()?,
                        is_program: true,
                    }
                    .seal(
                        &output.directory,
                        &initial.request_sha256,
                        &output.source,
                        &output.target,
                        &context,
                        program_request.program_source_lexical(),
                    )?;
                    next = next.append_display(proof.clone())?;
                    Some(proof)
                } else {
                    None
                };
            let observation = CellProgramObservations {
                turn: output.turn,
                metadata: output.metadata,
                products: output.products.expect("program output was sealed"),
            };
            let display_observations =
                display_outputs
                    .remove(&index)
                    .map(|output| CellProgramObservations {
                        turn: output.turn,
                        metadata: output.metadata,
                        products: output.products.expect("program output was sealed"),
                    });
            items.push(CellProgramItem {
                checked: item,
                native: Some(native),
                display,
                native_observations: Some(observation),
                display_observations,
            });
            prefix = Some(next);
        }
        if !outputs.is_empty() || !display_outputs.is_empty() {
            return Err(CompileError::ExtractFailed(
                "program contains unowned prepared targets".into(),
            ));
        }
        Ok(Arc::new(CellProgram {
            checked: cell,
            parsed: planned.parsed_plan.clone(),
            slots: planned.slots.clone(),
            items,
        }))
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
            checked_publication_context: None,
            checked_item: None,
            checked_display: None,
        }
    }

    fn read_program_output(&self, directory: &Path) -> Result<ProgramNativeOutput, CompileError> {
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
            seal_turn_outputs(
                self,
                directory,
                &directory.join(format!("{module}.hs")),
                &output.source,
                &output.target,
                "__prepared",
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
        admissions: &[crate::declaration_context::ExactSourceAdmission],
    ) -> Result<Arc<crate::declaration_join::ExactDeclarationContext>, CompileError> {
        let module = extract_module_name(&output.source).ok_or_else(|| {
            CompileError::ExtractFailed("program support output has no target owner".into())
        })?;
        let support = output
            .products
            .as_ref()
            .expect("program output was sealed")
            .recovery_products
            .iter()
            .filter(|product| product.owner().module != module)
            .cloned()
            .collect::<Vec<_>>();
        request.admit_program_support(context, &support, admissions)
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
        let sealed = seal_turn_outputs_inner(
            self,
            &directory,
            &source_path,
            source,
            &target,
            "__result",
            None,
            Some(&authored),
        )?
        .ok_or_else(fail)?;
        let products = sealed
            .recovery_products
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
        let baseline = &exact.context;
        let empty = baseline.recovery_products().is_empty()
            && baseline.joined_interfaces().is_empty()
            && baseline.lexical_graph().is_empty()
            && baseline.interface_owners().is_empty();
        let certificate = crate::declaration_join::certify_same_offer_planned_declaration(
            module,
            source,
            &self.producer,
            &sealed,
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
            exact.apply_to(command)?;
        }
        if let Some(manifest) = self.manifest_path() {
            command.module_candidates(manifest);
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
    pub artifact_view: crate::artifact_inventory::ArtifactView,
    pub compile_input_identity: Option<Arc<SealedCompileInputIdentity>>,
    pub certified_groups: Arc<[certified_products::PendingCertifiedGroup]>,
    pub pending_imports: Vec<certified_products::PendingImportOwner>,
    pub recovery_products: Vec<crate::recovery_artifacts::CertifiedRecoveryProduct>,
    pub package_interfaces: certified_products::CertifiedTargetPackageInterfaces,
    pub checked_execution: Option<Arc<crate::checked_cell::ExactCompiledItem>>,
    pub checked_activation_input: Option<Arc<crate::checked_cell::ExactCompiledActivationInput>>,
    pub checked_display: Option<Arc<crate::checked_cell::ExactCompiledDisplay>>,
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
    pub fn compile_input_identity(&self) -> Option<&Arc<SealedCompileInputIdentity>> {
        self.native
            .as_ref()?
            .products()?
            .compile_input_identity
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
    )
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
) -> Result<Option<SealedTurnProducts>, CompileError> {
    if std::fs::read_to_string(source_path)? != source {
        return Err(CompileError::ExtractFailed(
            "turn source changed after worker compile".into(),
        ));
    }
    let read_sidecar = |name: &str| {
        let path = output_dir.join(name);
        std::fs::read(&path).map_err(|error| {
            CompileError::ExtractFailed(format!("turn sidecar {}: {error}", path.display()))
        })
    };
    let product_bytes = read_sidecar("module-products.cbor")?;
    let package_bundle_bytes = read_sidecar("module-package-imports.cbor")?;
    let evidence_bytes = read_sidecar("dependencies.json")?;
    let receipt_bytes = read_sidecar("certified-products.cbor")?;
    let product_decode_start = Instant::now();
    let fresh_products =
        certified_products::ParsedModuleProducts::decode(&product_bytes, &package_bundle_bytes)
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        "products.decode",
        product_decode_start.elapsed(),
        product_bytes.len() as u64,
    );
    let exact_source = offer
        .exact
        .as_ref()
        .map(|request| request.admit_source(source_path, source, &evidence_bytes))
        .transpose()?;
    let exact = offer
        .exact
        .as_ref()
        .zip(exact_source.as_ref())
        .map(
            |(request, source)| crate::declaration_context::ExactProductAdmission {
                request,
                source,
            },
        );
    let evidence = match exact_source.as_ref() {
        Some(source) => Some(source.evidence.clone()),
        None => cache::DependencyEvidence::from_worker(&evidence_bytes, source_path, source),
    };
    let needs_certificate = offer.has_candidates()
        || offer.exact.is_some()
        || !fresh_products.products().is_empty()
        || !prepared.globals().is_empty()
        || evidence.as_ref().is_some_and(has_ready_home_module);
    if receipt_bytes.is_empty() {
        if needs_certificate {
            return Err(CompileError::ExtractFailed(
                "turn required product certificate unavailable".into(),
            ));
        }
        return Ok(None);
    }
    let receipt_decode_start = Instant::now();
    let receipt = certified_products::decode_receipt(&receipt_bytes)
        .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        "products.receipt_decode",
        receipt_decode_start.elapsed(),
        receipt_bytes.len() as u64,
    );
    let Some(valid) = evidence.as_ref().filter(|evidence| evidence.valid(source)) else {
        if needs_certificate
            || receipt
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
    let certified = certified_products::certify_products(
        offer.selected.as_deref(),
        &receipt,
        &fresh_products,
        &evidence_bytes,
        source_path,
        valid,
        source,
        &offer.producer,
        &offer.include,
        exact.as_ref(),
        authored,
    )
    .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    let target_admission_start = Instant::now();
    ensure_ready_module_inventory(&receipt.modules, valid)?;
    let accepted = receipt
        .targets
        .get(target)
        .ok_or_else(|| CompileError::ExtractFailed("turn target product receipt missing".into()))?;
    if receipt.targets.len() != 1 {
        return Err(CompileError::ExtractFailed(
            "turn target product receipt count".into(),
        ));
    }
    let package_closure = merge_package_closure(&receipt.packages, offer.exact.as_ref())?;
    let pending_imports = certified_products::certify_target_owners(
        prepared,
        accepted,
        &certified.groups,
        &package_closure,
    )
    .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    let package_interfaces =
        certified_products::certify_target_package_interfaces(prepared, &package_closure)
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    timing::record_stage_with_owners(
        timing::NO_NODE,
        timing::NO_ROUND,
        "products.target_admission",
        target_admission_start.elapsed(),
        0,
        receipt.targets.len(),
    );
    module_candidates::record_deployment_acceptance(offer.selected.as_deref(), &receipt);
    if offer
        .exact
        .as_ref()
        .is_none_or(|exact| empty_exact_context(&exact.context))
    {
        let (_, publication) = module_candidates::prepare_publication(
            &offer.producer,
            &offer.include,
            valid,
            fresh_products,
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
    } else {
        module_candidates::record_exact_context_publication_skip(fresh_products.products());
    }
    let certified_groups: Arc<[_]> = certified.groups.into();
    let compile_input_identity =
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
                    )
                })
                .transpose()?
                .flatten()
                .map(Arc::new)
        } else {
            None
        };
    let artifact_view = crate::declaration_context::certified_product_artifact_view(
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&offer.producer)
            .sha256(),
        &certified.recovery_products,
        &certified.module_interfaces,
        exact
            .as_ref()
            .map(|admission| admission.request.context.as_ref()),
    )?;
    let checked_context = if offer.checked_item.is_some() || offer.checked_display.is_some() {
        Some(checked_output_context(
            offer,
            &certified.recovery_products,
            exact_source
                .as_ref()
                .expect("checked output has exact source"),
            source,
        )?)
    } else {
        None
    };
    Ok(Some(SealedTurnProducts {
        artifact_view,
        compile_input_identity,
        checked_display: offer
            .checked_display
            .as_ref()
            .map(|display| {
                display.seal(
                    output_dir,
                    &offer
                        .exact
                        .as_ref()
                        .expect("checked display has exact scope")
                        .request_sha256,
                    source,
                    prepared,
                    &checked_context.as_ref().expect("checked output context").0,
                    &checked_context.as_ref().expect("checked output context").1,
                )
            })
            .transpose()?,
        checked_execution: offer
            .checked_item
            .as_ref()
            .filter(|item| item.purpose == crate::checked_cell::CheckedItemPurpose::Authored)
            .map(|item| {
                item.seal(
                    output_dir,
                    &offer
                        .exact
                        .as_ref()
                        .expect("checked offer has exact scope")
                        .request_sha256,
                    source,
                    prepared,
                    &checked_context.as_ref().expect("checked output context").0,
                    &checked_context.as_ref().expect("checked output context").1,
                )
            })
            .transpose()?,
        checked_activation_input: offer
            .checked_item
            .as_ref()
            .filter(|item| {
                item.purpose == crate::checked_cell::CheckedItemPurpose::HostActivationInput
            })
            .map(|item| {
                item.seal_activation_input(
                    output_dir,
                    &offer
                        .exact
                        .as_ref()
                        .expect("host activation has exact scope")
                        .request_sha256,
                    source,
                    prepared,
                    &checked_context.as_ref().expect("checked output context").0,
                    &checked_context.as_ref().expect("checked output context").1,
                )
            })
            .transpose()?,
        certified_groups,
        pending_imports,
        recovery_products: certified.recovery_products,
        package_interfaces,
    }))
}

fn checked_output_context(
    offer: &ModuleCandidateOffer,
    products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
    source_admission: &crate::declaration_context::ExactSourceAdmission,
    source: &str,
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
    let module = extract_module_name(source).ok_or_else(|| {
        CompileError::ExtractFailed("checked output lacks original source module".into())
    })?;
    let support = products
        .iter()
        .filter(|product| product.owner().module != module)
        .cloned()
        .collect::<Vec<_>>();
    let mut request = exact.clone();
    let context = request.admit_program_support(
        exact.context.clone(),
        &support,
        std::slice::from_ref(source_admission),
    )?;
    Ok((context, request.program_source_lexical().to_vec()))
}

fn merge_package_closure(
    packages: &BTreeMap<(String, String), certified_products::PackageInterfaceWitness>,
    exact: Option<&crate::declaration_context::ExactCompilationRequest>,
) -> Result<BTreeMap<(String, String), certified_products::PackageInterfaceWitness>, CompileError> {
    let mut selected = packages.clone();
    if let Some(request) = exact {
        let inherited =
            certified_products::inherited_package_witnesses(&request.context.recovery_products())
                .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
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
    }
    Ok(selected)
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
pub fn compile_invocation(
    inv: &CompileInvocation<'_>,
    mut on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    compile_invocation_inner(inv, &mut on_stage, true, None, None, None, None)
}

/// Compile fresh source against immutable declaration owners through the same
/// artifact front door. Ordinary source candidates and artifact memo are
/// unavailable; successful products require context-bound compiler receipts.
pub fn compile_invocation_in_context(
    inv: &CompileInvocation<'_>,
    context: Arc<crate::declaration_join::ExactDeclarationContext>,
    mut on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    compile_invocation_inner(inv, &mut on_stage, false, None, Some(context), None, None)
}

/// Compile a declaration probe in full-home-product mode, which produces
/// original products for every home module, including modules with no
/// executable references from the probe. The probe bypasses the memo and
/// candidate path while retaining the shared certification front door.
pub(crate) fn compile_authored_products(
    source: &str,
    target: &str,
    include: &[PathBuf],
    session_root: &Path,
    context: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
    authored: &crate::declaration_join::NativeAuthoredDeclarationAdmission,
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
        false,
        Some(session_root),
        context,
        None,
        Some(authored),
    )
}

/// Build an immutable source cohort at its final deployment path. The compile
/// bypasses candidate input and memo and exports only authenticated originals.
pub fn build_deployment_module_package(
    source_root: &Path,
    output_root: &Path,
) -> Result<crate::toolchain::DeploymentModulePackage, CompileError> {
    module_candidates::deployment::prepare_build_roots(source_root, output_root)?;
    let source_root = source_root.to_owned();
    let configuration = crate::toolchain::CompilerDeploymentConfiguration::from_env()
        .map_err(|e| CompileError::ExtractFailed(e.to_string()))?;
    let crate::toolchain::CompilerDeploymentConfiguration::Configured(authority) = configuration
    else {
        return Err(crate::toolchain::ModulePackageError::UnknownCompiler.into());
    };
    let source = include_str!("../tests/fixtures/deployment-module-package/PreludePackage.hs");
    let roots = [source_root.clone()];
    let invocation = CompileInvocation {
        source,
        targets: &["packageSentinel"],
        include: &roots,
        fallback_module_name: "TidepoolPreludePackage",
    };
    compile_invocation_inner(
        &invocation,
        &mut |_, _, _| {},
        false,
        None,
        None,
        Some((output_root, &source_root)),
        None,
    )?;
    Ok(crate::toolchain::DeploymentModulePackage::load(
        &output_root.join("catalog.json"),
        &authority,
    )?)
}

pub(crate) const AUTHORED_PRODUCT_PROBE_MODULE: &str = "TidepoolAuthoredProductProbe";

fn compile_invocation_inner(
    inv: &CompileInvocation<'_>,
    mut on_stage: &mut impl FnMut(&str, Duration, u64),
    allow_candidates: bool,
    session_root: Option<&Path>,
    exact_context: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
    deployment_export: Option<(&Path, &Path)>,
    authored: Option<&crate::declaration_join::NativeAuthoredDeclarationAdmission>,
) -> Result<CompiledArtifacts, CompileError> {
    assert!(
        !inv.targets.is_empty(),
        "compile_invocation: at least one target is required"
    );
    let multi = inv.targets.len() > 1;

    let temp_dir = TempDir::new()?;
    // GHC derives the module name from the filename (capitalize(basename));
    // see `CompileInvocation::fallback_module_name`'s doc for why this
    // differs per lane.
    let module =
        extract_module_name(inv.source).unwrap_or_else(|| inv.fallback_module_name.to_string());
    let input_path = temp_dir.path().join(format!("{module}.hs"));
    std::fs::write(&input_path, inv.source)?;

    let mut cmd = ExtractCmd::new().map_err(|e| CompileError::Io(e.into()))?;
    cmd.input(&input_path)
        .output_dir(temp_dir.path())
        .targets(inv.targets)
        .includes(inv.include);
    if let Some(root) = session_root {
        cmd.session_root(root).certify_home_products();
    }
    if deployment_export.is_some() {
        cmd.certify_home_products();
    }
    let exact_request = if let Some(context) = exact_context.as_ref() {
        let endpoint = cmd
            .bind()
            .map_err(|error| CompileError::Io(extract_spawn_error(error.source)))?;
        crate::toolchain::admit_bound_endpoint(&endpoint)
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        let request = context
            .prepare_compilation(
                &temp_dir.path().join("exact-scope"),
                endpoint.identity().producer_bytes(),
            )?
            .with_source_search_context(
                &inv.include
                    .iter()
                    .map(|path| path.to_path_buf())
                    .collect::<Vec<_>>(),
            );
        request.apply_to(&mut cmd)?;
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
            let endpoint = cmd.bind().map_err(CompileAttemptError::Endpoint)?;
            let deployment = crate::toolchain::admit_bound_endpoint(&endpoint)
                .map_err(CompileAttemptError::Deployment)?;
            crate::paths::apply_build_products_dir(&mut cmd, &endpoint);

            let candidate_set = if allow_candidates {
                module_candidates::select_configured(
                    endpoint.identity().producer_bytes(),
                    inv.include,
                    temp_dir.path(),
                )
                .map_err(CompileAttemptError::ModulePackage)?
            } else {
                None
            };

            let inv_key = {
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
                        }
                    }
                }
                key
            };

            if let Some(selected) = &candidate_set {
                cmd.module_candidates(&selected.manifest_path);
            }
            let producer = endpoint.identity().producer_bytes().to_vec();

            endpoint
                .execute(&cmd)
                .map(|run| {
                    CompileAttempt::Executed((
                        cmd,
                        run,
                        inv_key,
                        producer,
                        candidate_set,
                        deployment,
                    ))
                })
                .map_err(CompileAttemptError::Endpoint)
        },
        |error| matches!(error,CompileAttemptError::Endpoint(error) if error.permits_rebind()),
    )
    .map_err(CompileAttemptError::into_compile_error)?;
    let (cmd, run, inv_key, producer, candidate_set, deployment) = match attempt {
        CompileAttempt::Cached(artifacts) => return Ok(*artifacts),
        CompileAttempt::Executed(executed) => executed,
    };

    if let Some(request) = exact_request.as_ref() {
        let actual =
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&producer)
                .sha256();
        if actual != request.context.toolchain_identity_sha256() {
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

    let compiler_stderr = run.output.stderr.clone();
    let extracted = extract_and_read(
        run,
        temp_dir.path(),
        inv.targets,
        multi,
        &mut on_stage,
        |stderr, success| {
            if !success && !stderr.is_empty() {
                tracing::warn!(targets = %inv.targets.join(","), "extract failed:\n{stderr}");
            }
        },
    );
    let (meta_bytes, raw, product_bytes) = match extracted {
        Ok(output) => output,
        Err(error)
            if allow_candidates
                && candidate_set
                    .as_ref()
                    .is_some_and(|set| !set.by_owner.is_empty()) =>
        {
            tracing::warn!(%error, "candidate compile failed; retrying without candidates");
            return compile_invocation_inner(
                inv,
                on_stage,
                false,
                session_root,
                exact_context,
                deployment_export,
                authored,
            );
        }
        Err(error) => {
            return Err(retain_compiler_failure(
                temp_dir.path(),
                &compiler_stderr,
                error,
            ));
        }
    };

    // Store only what DESERIALIZED, so a malformed artifact set is never
    // memoized into a permanently-failing entry. Best-effort: an unwritable
    // memo costs a recompile, it never fails a compile.
    let assembled = (|| {
        let evidence_bytes = std::fs::read(temp_dir.path().join("dependencies.json"))?;
        let package_bundle_bytes =
            std::fs::read(temp_dir.path().join("module-package-imports.cbor"))?;
        let exact_source = exact_request
            .as_ref()
            .map(|request| request.admit_source(&input_path, inv.source, &evidence_bytes))
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
            Some(source) => Some(source.evidence.clone()),
            None => {
                cache::DependencyEvidence::from_worker(&evidence_bytes, &input_path, inv.source)
            }
        };
        let receipt_bytes = std::fs::read(temp_dir.path().join("certified-products.cbor"))?;
        if receipt_bytes.is_empty() {
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
            let fresh_products = decode_fresh_products(&product_bytes)?;
            if !fresh_products.is_empty() {
                return Err(CompileError::ExtractFailed(
                    "fresh module product certificate unavailable".into(),
                ));
            }
            if evidence.as_ref().is_some_and(has_ready_home_module) {
                return Err(CompileError::ExtractFailed(
                    "ready module product certificate unavailable".into(),
                ));
            }
            let artifacts = assemble_with_products(
                &meta_bytes,
                &raw,
                fresh_products,
                Vec::new(),
                evidence.as_ref(),
                None,
                &mut *on_stage,
            )?;
            ensure_no_uncertified_globals(&artifacts)?;
            return Ok(artifacts);
        }
        let receipt = certified_products::decode_receipt(&receipt_bytes)
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        let fresh_products =
            certified_products::ParsedModuleProducts::decode(&product_bytes, &package_bundle_bytes)
                .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        let valid_evidence = evidence.as_ref().filter(|value| value.valid(inv.source));
        let cached_receipts: Vec<_> = receipt
            .modules
            .iter()
            .filter(|module| module.origin == certified_products::ProductOrigin::Cached)
            .collect();
        let certified = if let Some(valid) = valid_evidence {
            let certified = certified_products::certify_products(
                candidate_set.as_ref(),
                &receipt,
                &fresh_products,
                &evidence_bytes,
                &input_path,
                valid,
                inv.source,
                &producer,
                inv.include,
                exact.as_ref(),
                authored,
            )
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
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
                groups: Vec::new(),
                recovery_products: Vec::new(),
                module_interfaces: Vec::new(),
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
        let (fresh_products, publication) = if (exact_request.is_none()
            || deployment_export.is_some())
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
            evidence.as_ref(),
            exact_request.as_ref(),
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
        let package_closure = merge_package_closure(&receipt.packages, exact_request.as_ref())?;
        for (name, target) in &mut artifacts.targets {
            let accepted = receipt.targets.get(name).ok_or_else(|| {
                CompileError::ExtractFailed("target product receipt missing".into())
            })?;
            if valid_evidence.is_some() {
                target.pending_imports = certified_products::certify_target_owners(
                    target.prepared.prepared(),
                    accepted,
                    &certified.groups,
                    &package_closure,
                )
                .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
                target.package_interfaces = certified_products::certify_target_package_interfaces(
                    target.prepared.prepared_shared(),
                    &package_closure,
                )
                .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
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
        artifacts.artifact_view = crate::declaration_context::certified_product_artifact_view(
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&producer)
                .sha256(),
            &certified.recovery_products,
            &certified.module_interfaces,
            exact_request
                .as_ref()
                .map(|request| request.context.as_ref()),
        )?;
        artifacts.certified_groups = certified.groups;
        artifacts.recovery_products = certified.recovery_products;
        artifacts.exact_source_admission = exact_source;
        artifacts.producer_identity = Some(producer.as_slice().try_into().map_err(|_| {
            CompileError::ExtractFailed("bound compiler producer identity length".into())
        })?);
        module_candidates::record_deployment_acceptance(candidate_set.as_ref(), &receipt);
        if let Some((output_root, source_root)) = deployment_export {
            valid_evidence.ok_or_else(|| {
                CompileError::ModulePackage(crate::toolchain::ModulePackageError::OpenCohort)
            })?;
            let publication = publication.as_ref().ok_or_else(|| {
                CompileError::ExtractFailed("module package publication unavailable".into())
            })?;
            module_candidates::deployment::export(
                output_root,
                source_root,
                &deployment,
                publication,
            )?;
        }
        if exact_request.is_none() {
            if let Some(publication) = publication {
                module_candidates::publish_prepared(publication);
            }
        } else {
            module_candidates::record_exact_context_publication_skip(
                &artifacts.module_products[..fresh_count],
            );
        }
        // A memo hit has no certified source group owner mapping. The module
        // store owns reuse for invocations with home products.
        if exact_request.is_none() && cached_receipts.is_empty() && fresh_count == 0 {
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
    match assembled {
        Err(error)
            if allow_candidates
                && candidate_set
                    .as_ref()
                    .is_some_and(|set| !set.by_owner.is_empty()) =>
        {
            tracing::warn!(%error, "candidate certification failed; retrying without candidates");
            compile_invocation_inner(
                inv,
                on_stage,
                false,
                session_root,
                exact_context,
                deployment_export,
                authored,
            )
        }
        result => result
            .map_err(|error| retain_compiler_failure(temp_dir.path(), &compiler_stderr, error)),
    }
}

/// Retain worker outputs and stderr after compilation or final sealing fails.
/// The existing test-log policy is optional, and retention never replaces the
/// original error. Call before the request's temporary directory is dropped.
pub fn retain_compiler_failure(
    directory: &Path,
    stderr: &[u8],
    error: CompileError,
) -> CompileError {
    retain_compiler_failure_inner(directory, stderr, error, None, None)
}

fn retain_compiler_failure_inner(
    directory: &Path,
    stderr: &[u8],
    error: CompileError,
    offer: Option<&ModuleCandidateOffer>,
    command: Option<&ExtractCmd>,
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
    command: Option<&ExtractCmd>,
) -> std::io::Result<PathBuf> {
    let retained_root = match std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir()?.join("target/tidepool-test-runs"),
    }
    .join("compiler-failures");
    std::fs::create_dir_all(&retained_root)?;
    let retained = TempDir::new_in(&retained_root)?;
    if let Some(offer) = offer {
        if let Some(exact) = &offer.exact {
            if let Err(failure) = exact.retain_input_diagnostics(retained.path()) {
                tracing::warn!(%failure, "could not retain original exact request diagnostics");
            }
        }
        if let Some(selected) = &offer.selected {
            if let Err(failure) = selected.retain_evidence_diagnostics(retained.path()) {
                tracing::warn!(%failure, "could not retain original selected candidate evidence");
            }
        }
        if let Err(failure) = offer.retain_checked_inputs(retained.path()) {
            tracing::warn!(%failure, "could not retain selected checked input diagnostics");
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
        }
    }
    if let Err(failure) = retain_program_compile_diagnostics(directory, retained.path()) {
        tracing::warn!(%failure, "could not retain complete program diagnostics");
    }
    if let Ok(bytes) = std::fs::read(directory.join("dependencies.json")) {
        if let Ok(evidence) = serde_json::from_slice::<cache::DependencyEvidence>(&bytes) {
            let sources = retained.path().join("consumed-sources");
            std::fs::create_dir(&sources)?;
            let mut captured = Vec::new();
            for (ordinal, source) in evidence.sources.iter().enumerate() {
                let path = PathBuf::from(format!("consumed-sources/{ordinal}.hs"));
                if std::fs::copy(&source.path, retained.path().join(&path)).is_ok() {
                    captured.push((source, path));
                }
            }
            std::fs::write(
                retained.path().join("consumed-sources.json"),
                serde_json::to_vec(&captured).map_err(std::io::Error::other)?,
            )?;
        }
    }
    if let Some(command) = command {
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
    }
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
        let selected = ["segment-", "item-", "display-"]
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
}
impl CompileAttemptError {
    fn into_compile_error(self) -> CompileError {
        match self {
            Self::Endpoint(error) => CompileError::Io(extract_spawn_error(error.source)),
            Self::Deployment(error) => CompileError::ExtractFailed(error.to_string()),
            Self::ModulePackage(error) => CompileError::ModulePackage(error),
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
    compile_invocation(&inv, on_stage)
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
) -> Result<(Vec<u8>, Vec<RawTargetOutput>, Vec<u8>), CompileError> {
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
    let meta_bytes = std::fs::read(&meta_path)?;
    let products_path = temp_dir.join("module-products.cbor");
    let product_bytes = std::fs::read(&products_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CompileError::MissingOutput(products_path)
        } else {
            CompileError::Io(error)
        }
    })?;

    let mut raw = Vec::with_capacity(targets.len());
    for target in targets {
        let prepared_path = temp_dir.join(prepared_artifact_name(target));
        if !prepared_path.exists() {
            return Err(CompileError::MissingOutput(prepared_path));
        }
        let prepared_bytes = std::fs::read(&prepared_path)?;
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
    Ok((meta_bytes, raw, product_bytes))
}

/// A prepared program's constructor declaration RESOLVES in the accompanying
/// `DataConTable` (by `host_id`) but disagrees with the entry it resolves
/// to — a different occurrence name, or a different field count. Both sides
/// mint `host_id` identically from `dataConWorkId`, so
/// a correctly paired artifact and table can never produce this — it is
/// exactly the signal that the two were NOT compiled together (e.g. metadata
/// from one compile assembled with a prepared program from another), caught
/// at the moment it is dangerous: `host_id` coincidentally resolving to some
/// OTHER real constructor a handler would then silently misinterpret fields
/// under, rather than failing to resolve at all. See
/// `check_constructor_identity_agreement`'s doc for why a host_id that
/// resolves to NOTHING is deliberately not a variant here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConstructorIdentityMismatch {
    /// The `host_id` resolves, but to a differently-named constructor —
    /// nominal disagreement, not merely a missing entry.
    #[error(
        "target {target:?} constructor {prepared_name:?} (host_id {:#018x}) resolves in the \
         DataConTable to {table_name:?} instead",
        host_id.0
    )]
    NameMismatch {
        target: String,
        host_id: tidepool_repr::DataConId,
        prepared_name: String,
        table_name: String,
    },
    /// The `host_id` resolves to the right name, but the two sides disagree
    /// on field count — a corrupt or mismatched metadata source, not a
    /// harmless re-encounter (mirrors `DataConTable::insert_checked`'s
    /// tag/rep_arity agreement guard on the metadata side alone).
    #[error(
        "target {target:?} constructor {name:?} (host_id {:#018x}) declares {prepared_fields} \
         field(s) in the prepared program but {table_rep_arity} in the DataConTable",
        host_id.0
    )]
    ArityMismatch {
        target: String,
        host_id: tidepool_repr::DataConId,
        name: String,
        prepared_fields: usize,
        table_rep_arity: u32,
    },
}

/// Verify every constructor `target`'s prepared program declares that
/// RESOLVES in `table` agrees with the entry it resolves to. `PreparedProgram`
/// validation (`tidepool_repr::execution_schema::validate_program`) already
/// enforces `host_id` uniqueness WITHIN one prepared program; it has no way
/// to check those ids against a metadata table compiled elsewhere, since the
/// two are parsed independently in `assemble`.
///
/// SCOPE: a `host_id` with NO table entry at all is deliberately not
/// rejected here — only a `host_id` that resolves to a DIFFERENT constructor
/// is. Two things independently justify drawing the line there rather than
/// at "every declared constructor must resolve":
///
/// - It would reject currently-valid, exercised pipelines. The STG
///   projection's closure (`Tidepool.ExecutionProjection.projectPreparedTarget`)
///   pulls in whatever the entry's REAL reachable STG needs, and GHC's own
///   exception-raising sites transitively need real `SomeException`/
///   `Typeable` evidence (`TrNameS`/`TrNameD` and friends) for native
///   exception settlement — reachable from virtually any nontrivial entry
///   (any partial pattern match, `error`, div-by-zero, ...), independent of
///   whether the user's source ever mentions `Typeable`. The legacy
///   metadata's constructor collection sources
///   (`wiredInDataCons`/`collectDataCons`/`collectUsedDataCons`/
///   `collectTransitiveDCons`) were never built against that
///   requirement and do not reliably cover it — confirmed empirically: a
///   real compile (`tidepool-runtime`'s
///   `build_products_dir_differential` fixture) declares a prepared
///   `TrNameS` with no metadata entry despite artifact and table coming
///   from the exact same extractor invocation.
/// - It is not the dangerous case. `tidepool_runtime::render::con_name`
///   already renders an unresolved id as `"<unknown>"` rather than
///   panicking or guessing — a `host_id` genuinely absent from the table
///   degrades exactly as gracefully whether that absence comes from this
///   legitimate coverage gap or a cross-paired table. The case that
///   silently misinterprets data — `host_id` coincidentally present in the
///   WRONG table under a different constructor's shape — has no such
///   fallback, which is exactly what this check catches instead.
///
/// Field count is the one arity fact both sides carry in genuinely
/// comparable form: `DataConTable::rep_arity` is `length
/// (dataConRepArgTys dc)`, and `ConstructorDecl::field_reps` is built by
/// mapping each of those same `dataConRepArgTys` entries through
/// `repsForType` and concatenating — an ordinary heap-constructor field (the
/// only kind `internConstructor` accepts; see its `result_rep` check) always
/// has exactly one representation component, so the flatten never actually
/// changes the count. Tag is deliberately NOT compared here: it is already
/// pinned by each side independently (both read `dataConTag` directly), and
/// disagreeing tags at agreeing name+id would indicate the SAME bug this
/// check exists to catch, just observed through a different field.
fn check_constructor_identity_agreement(
    target: &str,
    artifact: &PreparedArtifact,
    table: &DataConTable,
) -> Result<(), ConstructorIdentityMismatch> {
    for constructor in artifact.prepared().constructors() {
        let Some(dc) = table.get(constructor.host_id) else {
            continue;
        };
        let occurrence = &constructor.identity.occurrence;
        if &dc.name != occurrence {
            return Err(ConstructorIdentityMismatch::NameMismatch {
                target: target.to_string(),
                host_id: constructor.host_id,
                prepared_name: occurrence.clone(),
                table_name: dc.name.clone(),
            });
        }
        if usize::try_from(dc.rep_arity).unwrap_or(usize::MAX) != constructor.field_reps.len() {
            return Err(ConstructorIdentityMismatch::ArityMismatch {
                target: target.to_string(),
                host_id: constructor.host_id,
                name: dc.name.clone(),
                prepared_fields: constructor.field_reps.len(),
                table_rep_arity: dc.rep_arity,
            });
        }
    }
    Ok(())
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
    let (table, warnings) = read_metadata(meta_bytes)?;
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
    for (r, artifact) in raw.iter().zip(&prepared) {
        check_constructor_identity_agreement(&r.target, artifact, &table)?;
    }
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
        producer_identity: None,
        module_inventory: None,
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
    on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    let mut artifacts = assemble(meta_bytes, raw, on_stage)?;
    artifacts.module_products = fresh_products;
    artifacts.module_products.extend(certified_cached);
    if let Some(evidence) = evidence {
        let protected: BTreeMap<_, _> = exact
            .into_iter()
            .flat_map(|request| request.context.recovery_products())
            .map(|product| {
                (
                    (product.owner().unit.clone(), product.owner().module.clone()),
                    product,
                )
            })
            .collect();
        let mut protected_groups = BTreeMap::<_, BTreeMap<_, _>>::new();
        for group in exact.into_iter().flat_map(|request| request.groups.iter()) {
            protected_groups
                .entry((group.owner().unit.as_str(), group.owner().module.as_str()))
                .or_default()
                .insert(group.group().original_ordinal(), group.group());
        }
        let mut emitted = std::collections::HashSet::new();
        for product in &artifacts.module_products {
            let key = (product.unit.as_str(), product.module.as_str());
            let admitted = if let Some(original) =
                protected.get(&(product.unit.clone(), product.module.clone()))
            {
                let expected = protected_groups.get(&key);
                product.interface == original.interface_bytes()
                    && product.groups.len() == expected.map_or(0, BTreeMap::len)
                    && product.groups.iter().all(|group| {
                        expected
                            .and_then(|groups| groups.get(&group.original_ordinal()))
                            .is_some_and(|original| *original == group)
                    })
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
                    "module product {}:{} lacks a unique fresh graph or protected original inventory",
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
) -> Option<(
    Vec<u8>,
    Vec<RawTargetOutput>,
    Vec<u8>,
    cache::DependencyEvidence,
)> {
    let (loaded, evidence) = cache::artifacts_load(key, names, source)?;
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
mod typed_site_tests {
    use super::*;

    fn host_input_witness(payload: &[u8]) -> crate::checked_cell::CanonicalInputTypeWitness {
        let text = |value: &str| Value::Text(value.into());
        let mut structure = Vec::new();
        ciborium::into_writer(
            &Value::Array(vec![text("literal"), text("nat"), text("1")]),
            &mut structure,
        )
        .unwrap();
        let mut bytes = Vec::new();
        // This codec fixture carries no executable GHC IfaceType.
        ciborium::into_writer(
            &Value::Array(vec![
                text("TPCANONICALINPUTTYPE1"),
                text("1"),
                Value::Array(vec![
                    text("TPCHECKEDSIGNATURE2"),
                    text("activation-input"),
                    text("presentation"),
                    Value::Bytes(payload.to_vec()),
                    Value::Array(vec![]),
                ]),
                Value::Bytes(structure),
                Value::Array(vec![]),
            ]),
            &mut bytes,
        )
        .unwrap();
        crate::checked_cell::CanonicalInputTypeWitness::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn host_input_authorization_preserves_native_payload_and_refuses_authored_or_planned_recipe() {
        let witness = host_input_witness(&[1, 2, 3]);
        let substituted = host_input_witness(&[3, 2, 1]);
        assert_eq!(witness, substituted);
        assert_eq!(witness.commitment(), substituted.commitment());
        assert_ne!(witness.metadata_digest(), substituted.metadata_digest());
        let specification = crate::checked_cell::CheckedCellSpecification {
            admission_digest: [7; 32],
            cell_source: "sessionInput <- pure (undefined :: TidepoolActivationInput)".into(),
            template_source: "protected template".into(),
            turn_templates: Vec::new(),
            injected_modules: Vec::new(),
            reserved_declaration_modules: Vec::new(),
        };
        let values = crate::checked_cell::CheckedValueInputs::capture_raw(Vec::new()).unwrap();
        let include = [PathBuf::from("/source/original")];
        let authorization = checked_cell_authorization(
            CheckedCellPurpose::HostActivationInput(&witness),
            &specification,
            &values,
            &include,
        )
        .unwrap();
        let fields = authorization.as_array().unwrap();
        assert_eq!(fields.len(), 10);
        assert_eq!(fields[0], Value::Text("host-input-check1".into()));
        assert_eq!(
            fields[8],
            crate::checked_cell::encode_signature(witness.signature())
        );
        assert_eq!(
            fields[9],
            Value::Array(vec![Value::Text("/source/original".into())])
        );
        let substituted = checked_cell_authorization(
            CheckedCellPurpose::HostActivationInput(&substituted),
            &specification,
            &values,
            &include,
        )
        .unwrap();
        assert_ne!(authorization, substituted);
        let authored = checked_cell_authorization(
            CheckedCellPurpose::Authored,
            &specification,
            &values,
            &include,
        )
        .unwrap();
        assert_eq!(authored.as_array().unwrap().len(), 9);
        assert_eq!(
            authored.as_array().unwrap()[0],
            Value::Text("cell-check2".into())
        );
        let mut authored_recipe = specification.clone();
        authored_recipe.cell_source = "let authored = 1".into();
        assert!(checked_cell_authorization(
            CheckedCellPurpose::HostActivationInput(&witness),
            &authored_recipe,
            &values,
            &include
        )
        .is_err());
        let mut planned_recipe = specification;
        planned_recipe
            .reserved_declaration_modules
            .push("Tidepool.Session.Lib.G1".into());
        assert!(checked_cell_authorization(
            CheckedCellPurpose::HostActivationInput(&witness),
            &planned_recipe,
            &values,
            &include
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn protected_search_authorization_refuses_lossy_path_encoding() {
        use std::os::unix::ffi::OsStringExt;
        let authorization = || Value::Array(vec![Value::Text("cell-check".into())]);
        let invalid = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]));
        assert!(
            checked_search_authorization(CheckedPurpose::Cell, authorization(), &[invalid])
                .is_err()
        );
        let paths = [
            PathBuf::from("/exact//first"),
            PathBuf::from("/exact/../second"),
        ];
        let encoded =
            checked_search_authorization(CheckedPurpose::Cell, authorization(), &paths).unwrap();
        let Value::Array(fields) = encoded else {
            panic!("authorization must be an array")
        };
        assert_eq!(fields[0], Value::Text("cell-check2".into()));
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
        let error = compile_targets(
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
        let retained =
            retain_failed_compiler_artifacts(scratch.path(), None, Some(&command)).unwrap();
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
            |_, _, _| {}
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
        let compiled = compile_targets(&source, &["consume"], &[root], |_, _, _| {}).unwrap();
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
            .expect("requires the Nix-built runtime-stdlib-products catalog");
        let source_root = package.source_root().to_owned();
        let catalog: serde_json::Value = serde_json::from_slice(
            &std::fs::read(std::env::var_os(crate::toolchain::ENV_COMPILER_MODULES).unwrap())
                .unwrap(),
        )
        .unwrap();
        let cohort: std::collections::BTreeSet<_> = catalog["modules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|module| {
                let root = Path::new(catalog["output_root"].as_str().unwrap());
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
        let original = compile_targets(source, &["result"], &[source_root.clone()], |_, _, _| {})
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
        let mut changed = std::fs::read(source_root.join("Tidepool/FilePath.hs")).unwrap();
        changed.extend_from_slice(b"\n-- higher-priority source witness\n");
        std::fs::write(shadow.path().join("Tidepool/FilePath.hs"), changed).unwrap();
        let shadowed = compile_targets(
            source,
            &["result"],
            &[shadow.path().to_owned(), source_root],
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
        let first_artifacts =
            compile_targets(&first, &["consume"], &[root.clone()], |_, _, _| {}).unwrap();
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
        let second =
            compile_targets(&changed, &["consume"], &[root.clone()], |_, _, _| {}).unwrap();
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
        let third = compile_targets(&changed_again, &["consume"], &[root], |_, _, _| {}).unwrap();
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

    /// A real, GHC-produced prepared program — `M3Vertical.hs`'s `entry`,
    /// which allocates a user `Box` constructor. `PreparedArtifact` has no
    /// Rust-side encoder (the wire format is Haskell-authored, decode-only
    /// here — see `execution_schema::decode::parse_program`), so this
    /// checked-in fixture is the only way to exercise `assemble` against a
    /// real prepared program without shelling out to GHC.
    fn prepared_fixture_bytes() -> Vec<u8> {
        include_bytes!("../../../bridge/haskell/test-prepared-stg/fixtures/m3-vertical.cbor")
            .to_vec()
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
            table.insert(dc);
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

    /// TEST 1: a table built to agree with every constructor the real
    /// prepared fixture declares — exactly what one compile's own metadata
    /// and prepared output look like paired together — must assemble.
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
            table.insert(matching_dc(decl));
        }
        let meta_bytes = write_metadata(&table, &MetaWarnings::default()).unwrap();
        let raw = vec![raw_target("entry", prepared_bytes)];

        assemble(&meta_bytes, &raw, |_, _, _| {}).expect("correctly paired artifact must assemble");
    }

    /// TEST 3 (scope boundary, primary case): a table missing the fixture's
    /// constructor ENTIRELY — an otherwise-empty table — must still assemble.
    /// This is the empirically-forced scope line documented on
    /// `check_constructor_identity_agreement`: a real compile
    /// (`tidepool-runtime`'s `build_products_dir_differential` fixture, a
    /// plain Tidepool eval with no explicit `Typeable`/exception use) was
    /// caught by an earlier, blanket "every declared constructor must
    /// resolve" version of this check over a prepared `TrNameS` — real GHC
    /// exception-settlement evidence with no metadata entry, from an
    /// artifact and table that came from the exact same compile. Requiring
    /// resolution would reject that currently-valid pipeline, so it is
    /// deliberately not enforced.
    #[test]
    fn table_missing_a_declared_constructor_entirely_is_outside_this_checks_scope() {
        let prepared_bytes = prepared_fixture_bytes();
        let artifact =
            PreparedArtifact::parse(prepared_bytes.clone(), DecodeLimits::default()).unwrap();
        assert!(
            !artifact.prepared().constructors().is_empty(),
            "fixture must declare at least one constructor for this test to mean anything"
        );

        // Totally empty: no host_id in the fixture's prepared program
        // resolves in this table at all.
        let table = DataConTable::new();
        let meta_bytes = write_metadata(&table, &MetaWarnings::default()).unwrap();
        let raw = vec![raw_target("entry", prepared_bytes)];

        assemble(&meta_bytes, &raw, |_, _, _| {}).expect(
            "a host_id absent from the table entirely must not reject assembly — only a \
             host_id that resolves to a DIFFERENT constructor does",
        );
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
        });
        let meta_bytes = write_metadata(&table, &MetaWarnings::default()).unwrap();
        let raw = vec![raw_target("entry", prepared_bytes)];

        let err = match assemble(&meta_bytes, &raw, |_, _, _| {}) {
            Ok(_) => panic!("a same-id, different-name table entry must reject assembly"),
            Err(e) => e,
        };
        assert!(matches!(
            err,
            CompileError::ConstructorIdentity(ConstructorIdentityMismatch::NameMismatch { .. })
        ));
    }

    /// A second, narrower scope line: tag is deliberately NOT one of the
    /// compared facts (see `check_constructor_identity_agreement`'s doc) — id,
    /// name, and field count all still agree, so a table entry disagreeing
    /// ONLY on tag must still assemble. Pins that the check does not
    /// duplicate `DataConTable::insert_checked`'s own tag/rep_arity guard,
    /// and is not accidentally stricter than the facts both wire formats
    /// actually carry in agreeing form.
    #[test]
    fn tag_disagreement_alone_is_outside_this_checks_scope() {
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
            dc.tag = dc.tag.wrapping_add(1).max(1);
        });
        let meta_bytes = write_metadata(&table, &MetaWarnings::default()).unwrap();
        let raw = vec![raw_target("entry", prepared_bytes)];

        assemble(&meta_bytes, &raw, |_, _, _| {})
            .expect("a tag-only disagreement is out of this check's scope and must not reject");
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
            CompileError::ConstructorIdentity(ConstructorIdentityMismatch::ArityMismatch { .. })
        ));
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
            compile_invocation(&invocation, |stage, _, _| {
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
