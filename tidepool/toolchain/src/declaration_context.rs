//! Exact declaration inputs retain original products independently of source
//! lookup, while their explicit virtual graph owns lexical visibility.

use ciborium::value::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::artifact_inventory::{
    admission_failure, ArtifactBinding, ArtifactBindingNode, ArtifactDependency, ArtifactEntry,
    ArtifactGraphSelection, ArtifactId, ArtifactInventory, ArtifactInventoryFailure,
    ArtifactPayload, ArtifactView, CanonicalProducerIdentity, CompilerArtifactMetadata,
    CompilerInputProjection, CompilerInputRole, JoinedInterfaceRole, NativeGroupKey,
};
use crate::certified_products::{
    certify_selected_owned_products_in_context_with_validation, PendingCertifiedGroup,
};
use crate::declaration_join::{
    AcceptedJoin, CertifiedAuthoredDeclaration, DeclarationArtifact, ExactIfaceArtifact,
    ExactInterfaceOwner, ExactLexicalNode, ExactModuleIdentity, ModuleSnapshot,
};
use crate::recovery_artifacts::{
    self, CertifiedJoinedInterface, CertifiedRecoveryProduct, CertifiedValueInterface,
    MaterializationMode, PackageInterfaceValidation, RecoveryArtifactRef, RecoveryJoinRef,
    RecoveryValueInterfaceRef,
};
use crate::CompileError;

pub(crate) mod original_inputs;
mod published_source;
pub(crate) use published_source::PublishedOriginalEndorsement;
pub use published_source::PublishedSourceOriginalSelection;

// Count actual cold semantic encodings on this test thread across owner clones.
// Per-owner counters alone cannot detect a second independently encoded owner.
#[cfg(test)]
thread_local! {
    static CONTEXT_OBSERVATION_WORK: std::cell::Cell<(usize, usize)> = const {
        std::cell::Cell::new((0, 0))
    };
    static CONTEXT_FACT_COMPARISONS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

pub struct ExactDeclarationContext {
    owner: Arc<DeclarationContextOwner>,
}

struct DeclarationContextOwner {
    facts: DeclarationContextFacts,
    observations: OnceLock<ContextObservation>,
    #[cfg(test)]
    semantic_issuances: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    display_issuances: std::sync::atomic::AtomicUsize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DeclarationContextFacts {
    producer: [u8; 32],
    compiler_support: crate::artifact_inventory::CompilerSourceSupport,
    lexical: crate::exact_context::ExactLexicalSnapshot,
    template_imports: Option<Arc<RetainedTemplateImports>>,
    original_instance_environment: OriginalInstanceEnvironment,
}

/// Derived observations belong to this exact immutable context, never its ancestors.
#[derive(Debug)]
struct ContextObservation {
    metadata: Arc<crate::artifact_inventory::RetainedArtifactMetadata>,
    semantics: Arc<[u8]>,
    digest: [u8; 32],
    display_prefix: OnceLock<sha2::Sha256>,
}

/// Borrow the original JSON shape without retaining a second metadata census.
#[derive(serde::Serialize)]
struct OriginalDisplayObservations<'a> {
    semantics: &'a [u8],
    descriptors: Vec<&'a crate::artifact_inventory::ArtifactDescriptor>,
    dependencies: &'a [(ArtifactId, ArtifactId, ArtifactDependency)],
    native_groups: &'a BTreeSet<NativeGroupKey>,
}

impl std::fmt::Debug for ExactDeclarationContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExactDeclarationContext")
            .field("producer", &self.owner.facts.producer)
            .field("inventory", &self.owner.facts.compiler_support.artifacts())
            .field(
                "compiler_projection",
                &self.owner.facts.compiler_support.projection(),
            )
            .field("lexical", &self.owner.facts.lexical)
            .field("template_imports", &self.owner.facts.template_imports)
            .field(
                "original_instance_environment",
                &self.owner.facts.original_instance_environment,
            )
            .finish()
    }
}

impl Clone for ExactDeclarationContext {
    fn clone(&self) -> Self {
        Self {
            owner: Arc::clone(&self.owner),
        }
    }
}

impl PartialEq for ExactDeclarationContext {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner) || {
            #[cfg(test)]
            CONTEXT_FACT_COMPARISONS.with(|count| count.set(count.get() + 1));
            self.owner.facts == other.owner.facts
        }
    }
}
impl Eq for ExactDeclarationContext {}

/// Value and lexical roots issue compiler selection; additional native carriers
/// remain independently owned byte custody for future execution.
pub(crate) struct RetainedValueSourceSurface {
    pub(crate) artifacts: ArtifactView,
    pub(crate) compiler_support: crate::artifact_inventory::CompilerSourceSupport,
    pub(crate) lexical: Vec<ExactLexicalNode>,
}

/// One completed cell's immutable source facts. A value selects its own exact
/// graph from this issuer; common type custody does not publish source roots.
pub(crate) struct PreparedValueSourceSupport {
    context: Arc<ExactDeclarationContext>,
    imports: BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    implementations: BTreeMap<ExactModuleIdentity, crate::artifact_inventory::ArtifactKind>,
    selected: BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>,
}

impl PreparedValueSourceSupport {
    pub(crate) fn context(&self) -> &Arc<ExactDeclarationContext> {
        &self.context
    }

    pub(crate) fn new(
        context: Arc<ExactDeclarationContext>,
        support: &[ExactLexicalNode],
    ) -> Result<Self, CompileError> {
        let imports = compose_lexical_nodes(
            context
                .owner
                .facts
                .lexical
                .iter()
                .chain(support)
                .filter(|node| !node.owner.module.starts_with("Tidepool.Session.")),
        )?
        .into_iter()
        .map(|node| (node.owner, node.imports))
        .collect();
        let implementations = context.artifact_view().source_implementation_roles();
        let selected = context.compiler_metadata_snapshot()?.entries;
        Ok(Self {
            context,
            imports,
            implementations,
            selected,
        })
    }

    pub(crate) fn retain_output(
        &self,
        value: &CertifiedValueInterface,
    ) -> Result<RetainedValueSourceSurface, CompileError> {
        let interface = value.interface();
        let owner = identity(interface.unit(), interface.module());
        let entry = self.selected.get(&owner).ok_or_else(|| {
            failure("produced value interface is absent from its prepared context")
        })?;
        if entry.descriptor.id != value.artifact_id()
            || entry.requirements != value.requirements()
            || !matches!(&entry.payload,
                ArtifactPayload::Interface(selected, JoinedInterfaceRole::ValueInterface)
                if selected == interface)
        {
            return Err(failure(
                "produced value interface differs from its prepared issuer",
            ));
        }
        let view = self
            .context
            .artifact_view()
            .select_roots(vec![value.artifact_id()])?;
        self.retain_surface(&view, &BTreeSet::from([owner]))
    }

    fn retain_surface(
        &self,
        value: &ArtifactView,
        value_owners: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<RetainedValueSourceSurface, CompileError> {
        let retained = value
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.owner)
            .collect::<BTreeSet<_>>();
        let roots = self
            .imports
            .keys()
            .filter(|owner| retained.contains(*owner))
            .cloned()
            .collect::<Vec<_>>();
        let lexical = crate::declaration_join::source_lexical_surface(
            &roots,
            &self.imports,
            &[],
            &self.implementations,
        )?
        .lexical;
        let entries = lexical
            .iter()
            .filter_map(|node| {
                self.selected
                    .get(&node.owner)
                    .map(|entry| (node.owner.clone(), entry.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        if entries.len() != lexical.len() {
            return Err(failure(
                "retained source surface lacks its exact artifact owner",
            ));
        }
        let source_owners = lexical
            .iter()
            .map(|node| node.owner.clone())
            .chain(value_owners.iter().cloned())
            .collect();
        let support = self
            .context
            .owner
            .facts
            .compiler_support
            .source_support(&source_owners)?;
        let compiler_support = support.retain_custody(value)?;
        let artifacts = self
            .context
            .artifact_view()
            .retain_original_custody(compiler_support.artifacts())?;
        Ok(RetainedValueSourceSurface {
            artifacts,
            compiler_support,
            lexical,
        })
    }
}

/// Only the original compiler output proof can establish complete instance
/// visibility. Type projections and generic declaration contexts have none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum OriginalInstanceEnvironment {
    #[default]
    Unknown,
    Complete {
        target: ExactModuleIdentity,
    },
    MissingOriginalOwners(Vec<ExactModuleIdentity>),
}

/// Trusted source recipe associated with request-local native type custody.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RequestHelperRecipe {
    #[default]
    None,
    ActorReply,
}

impl RequestHelperRecipe {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ActorReply => "actor-reply",
        }
    }
}

pub(crate) fn encode_request_authorization(
    signatures: &crate::checked_cell::RequestTypeSignatures,
    recipe: RequestHelperRecipe,
    purpose: Option<Value>,
) -> Value {
    Value::Array(vec![
        text("request-types2"),
        signatures.authorization_value(),
        text(recipe.as_str()),
        purpose.unwrap_or(Value::Null),
    ])
}

/// Request-local signatures together with their trusted helper recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestAnnotations {
    signatures: Arc<crate::checked_cell::RequestTypeSignatures>,
    helper_recipe: RequestHelperRecipe,
}

impl RequestAnnotations {
    fn new(
        signatures: Arc<crate::checked_cell::RequestTypeSignatures>,
        helper_recipe: RequestHelperRecipe,
    ) -> Self {
        Self {
            signatures,
            helper_recipe,
        }
    }

    pub fn signatures(&self) -> &Arc<crate::checked_cell::RequestTypeSignatures> {
        &self.signatures
    }

    pub fn helper_recipe(&self) -> RequestHelperRecipe {
        self.helper_recipe
    }
}

/// Request-local compiler inputs. Persistent publication retains declarations
/// separately, so native request annotations cannot enter a lexical snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactCompileContext {
    declarations: Arc<ExactDeclarationContext>,
    request_annotations: Option<RequestAnnotations>,
    protected_scaffold: ProtectedScaffoldRequirements,
}

/// Exact implementation inputs owned by a protected compiler recipe. These
/// roles never enter persistent declarations or grant authored lexical imports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProtectedScaffoldRequirements {
    projection: CompilerInputProjection,
    native: Vec<(Arc<CertifiedAuthoredDeclaration>, Arc<[String]>)>,
}

/// Complete private compiler inputs from one already certified output.
/// This retains exact native roles and their custody without publishing either
/// authored lexical rights or persistent source-reuse roles.
#[derive(Clone, Debug, Eq, PartialEq)]
enum OriginalInputDemand {
    RolePolicy,
    Issued(crate::artifact_inventory::IssuedInputNativeDecisions),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OriginalCompilerInputs {
    demand: OriginalInputDemand,
    projection: CompilerInputProjection,
    artifacts: ArtifactView,
    native_offers: Vec<crate::artifact_inventory::IssuedNativeOffer>,
}

#[cfg(test)]
fn native_requirement_facts(
    offers: &[crate::artifact_inventory::IssuedNativeOffer],
) -> Vec<crate::artifact_inventory::NativeOriginalRequirement> {
    let mut facts = Vec::new();
    for proof in offers.iter().filter_map(|offer| offer.proof()) {
        if !facts.contains(proof) {
            facts.push(proof.clone());
        }
    }
    facts
}

fn preserve_native_offers(
    projection: &CompilerInputProjection,
    namespace: &ArtifactView,
    offers: &[crate::artifact_inventory::IssuedNativeOffer],
) -> Result<Vec<crate::artifact_inventory::IssuedNativeOffer>, CompileError> {
    let mut retained = Vec::new();
    for offer in offers {
        if let Some(offer) = offer.within_projection(projection, namespace)? {
            if !retained.contains(&offer) {
                retained.push(offer);
            }
        }
    }
    Ok(retained)
}

fn retain_native_offers(
    context: &ArtifactView,
    offers: &[crate::artifact_inventory::IssuedNativeOffer],
) -> Result<ArtifactView, CompileError> {
    context.retain_issued_offer_custody(offers.iter().cloned())
}

/// Carry choices already issued by each input before their custody is merged.
/// An original with no bound group must remain availability only; the merged
/// view cannot supply a new choice for it.
fn carry_native_original_requirements(
    projection: &CompilerInputProjection,
    preferred: &[crate::artifact_inventory::NativeOriginalRequirement],
) -> Vec<crate::artifact_inventory::NativeOriginalRequirement> {
    use crate::artifact_inventory::NativeOriginalRequirement;
    let mut requirements = Vec::new();
    for role in projection.roles() {
        if let Some(requirement) = NativeOriginalRequirement::from_role(role.clone()) {
            if !requirements.contains(&requirement) {
                requirements.push(requirement);
            }
            continue;
        }
        let Some(original) = role.original() else {
            continue;
        };
        for requirement in preferred.iter().filter(|fact| fact.original == original) {
            if !requirements.contains(requirement) {
                requirements.push(requirement.clone());
            }
        }
    }
    requirements
}

/// Capture native choices for an offer before its private custody is merged.
/// The offer can be only part of the eventual compiler namespace; validate that
/// namespace after all inputs arrive rather than inferring choices from it.
fn capture_native_original_requirements(
    projection: &CompilerInputProjection,
    artifacts: &ArtifactView,
    preferred: &[crate::artifact_inventory::NativeOriginalRequirement],
) -> Result<Vec<crate::artifact_inventory::NativeOriginalRequirement>, CompileError> {
    let mut requirements = carry_native_original_requirements(projection, preferred);
    let bound = projection.within_view(artifacts);
    for role in bound.roles() {
        let Some(original) = role.original() else {
            continue;
        };
        if requirements.iter().any(|fact| fact.original == original) {
            continue;
        }
        if let Some(requirement) =
            crate::artifact_inventory::NativeOriginalRequirement::from_selected_view(
                original, artifacts,
            )?
        {
            requirements.push(requirement);
        }
    }
    Ok(requirements)
}

/// One strict output issuer binds source decisions and the target view to its
/// completed compiler inputs. Publication borrows this owner without reissuance.
#[derive(Debug)]
pub(crate) struct IssuedOriginalOutput {
    selection: crate::certified_products::CertifiedSourceSelection,
    artifacts: ArtifactView,
    inputs: Arc<OriginalCompilerInputs>,
}

impl IssuedOriginalOutput {
    pub(crate) fn issue(
        selection: crate::certified_products::CertifiedSourceSelection,
        artifacts: ArtifactView,
    ) -> Result<Self, CompileError> {
        let inputs = Arc::new(OriginalCompilerInputs::from_selection(
            &selection, &artifacts,
        )?);
        Ok(Self {
            selection,
            artifacts,
            inputs,
        })
    }

    pub(crate) fn source_selection(&self) -> &crate::certified_products::CertifiedSourceSelection {
        &self.selection
    }
    pub(crate) fn artifacts(&self) -> &ArtifactView {
        &self.artifacts
    }
    pub(crate) fn compiler_inputs(&self) -> &OriginalCompilerInputs {
        &self.inputs
    }
}

impl OriginalCompilerInputs {
    #[cfg(test)]
    pub(crate) fn from_checked_support_products(
        support: &crate::artifact_inventory::CompilerSourceSupport,
        producer: [u8; 32],
        products: &[CertifiedRecoveryProduct],
    ) -> Result<Self, CompileError> {
        Self::issue_complete_native_availability_with_source(
            support.artifacts(),
            support.projection(),
            producer,
            products,
            CompleteAvailabilitySource::Candidate(support),
        )
    }

    pub(crate) fn issued_input_decisions(
        &self,
    ) -> Option<&crate::artifact_inventory::IssuedInputNativeDecisions> {
        match &self.demand {
            OriginalInputDemand::RolePolicy => None,
            OriginalInputDemand::Issued(decisions) => Some(decisions),
        }
    }

    pub(crate) fn issued_offers(&self) -> &[crate::artifact_inventory::IssuedNativeOffer] {
        &self.native_offers
    }
    pub(crate) fn role_projection(&self) -> &CompilerInputProjection {
        &self.projection
    }
    pub(crate) fn issued_artifacts(&self) -> &ArtifactView {
        &self.artifacts
    }

    pub(crate) fn native_offers(&self) -> &[crate::artifact_inventory::IssuedNativeOffer] {
        &self.native_offers
    }

    /// Complete a physical segment's new-product promises while all of its
    /// checked type outputs remain in private custody. Item selections carry
    /// these issued offers; their narrower type roles never reissue the body.
    pub(crate) fn issue_segment_native_availability(
        selection: &mut crate::certified_products::CertifiedSourceSelection,
        baseline: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
    ) -> Result<ArtifactView, CompileError> {
        let physical = baseline.inventory().admit_certified_with_demand(
            baseline,
            entries,
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
            selection,
        )?;
        let issued = Self::from_selection(selection, &physical)?;
        let metadata = issued
            .projection
            .project_metadata(issued.artifacts.metadata_snapshot())?;
        let transport = NativeCompilerTransport::issue(
            &issued.projection,
            &metadata,
            &issued.artifacts,
            &issued.native_offers,
        )?;
        selection
            .finish_new_product_availability(&metadata.retained, &transport.availability)
            .map_err(compiler_evidence_failure)?;
        // Return only physical custody. Continuation decisions belong to a
        // later input admission against the completed selection.
        Ok(issued.artifacts)
    }

    /// Reuse exact authored availability already carried by the context.
    /// This path cannot issue or complete a body from retained payloads.
    pub(crate) fn reuse_selected_authored(
        context: &ExactDeclarationContext,
    ) -> Result<Option<Self>, CompileError> {
        let interfaces = context.owner.facts.compiler_support.selected_interfaces()?;
        let authored = interfaces
            .values()
            .filter_map(|entry| {
                let ArtifactPayload::Canonical(interface) = &entry.payload else {
                    return None;
                };
                matches!(
                    interface.origin(),
                    crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. }
                )
                .then(|| entry.descriptor.owner.clone())
            })
            .collect::<BTreeSet<_>>();
        let selected = context
            .compiler_input_projection()
            .snapshot_roles()
            .iter()
            .filter_map(|(owner, role)| {
                (authored.contains(owner) && role.original().is_some()).then(|| owner.clone())
            })
            .collect::<BTreeSet<_>>();
        let fallback = authored
            .difference(&selected)
            .cloned()
            .collect::<BTreeSet<_>>();
        let carried = if fallback.is_empty() {
            None
        } else {
            let source = context
                .compiler_source_support()
                .source_support(&fallback)?;
            let mut projection = source.projection().clone();
            for offer in source.native_offers() {
                let facts = offer.snapshot_facts();
                if !fallback.contains(&facts.owner) {
                    continue;
                }
                let Some(role) = context
                    .compiler_input_projection()
                    .snapshot_roles()
                    .get(&facts.owner)
                else {
                    continue;
                };
                if role.interface() != facts.interface
                    || role
                        .original()
                        .is_some_and(|original| original != facts.original)
                {
                    continue;
                }
                projection.admit_role(
                    facts.owner,
                    CompilerInputRole::ReusableOriginal {
                        interface: facts.interface,
                        original: facts.original,
                    },
                )?;
            }
            let native_offers =
                preserve_native_offers(&projection, source.artifacts(), source.native_offers())?;
            Some(Self {
                demand: OriginalInputDemand::RolePolicy,
                projection,
                artifacts: retain_native_offers(source.artifacts(), &native_offers)?,
                native_offers,
            })
        };
        let carried = carried.filter(|input| {
            input
                .projection
                .roles()
                .iter()
                .any(|role| role.original().is_some())
        });
        if selected.is_empty() {
            return Ok(carried);
        }
        let source = &context.owner.facts.compiler_support;
        let selected = Self {
            demand: OriginalInputDemand::Issued(source.input_decisions().clone()),
            projection: source.projection().for_source_owners(&selected),
            artifacts: source.artifacts().clone(),
            native_offers: source.native_offers().to_vec(),
        };
        Ok(Some(match carried {
            Some(carried) => selected.merge(&carried)?,
            None => selected,
        }))
    }

    pub(crate) fn from_candidates(
        context: &ExactDeclarationContext,
        producer: CanonicalProducerIdentity,
        candidates: &crate::module_candidates::CandidateSet,
    ) -> Result<Option<Self>, CompileError> {
        Self::with_checked_candidate_products(context, producer, &candidates.native_availability)
    }

    fn with_checked_candidate_products(
        context: &ExactDeclarationContext,
        producer: CanonicalProducerIdentity,
        configured: &[CertifiedRecoveryProduct],
    ) -> Result<Option<Self>, CompileError> {
        let mut carried = Self::reuse_selected_authored(context)?;
        let metadata = context.compiler_metadata_snapshot()?;
        let mut reused = BTreeSet::new();
        let mut fresh = Vec::new();
        for product in configured {
            let owner = identity(&product.owner().unit, &product.owner().module);
            let is_selected = metadata.entries.get(&owner).is_some_and(|entry| {
                matches!(&entry.payload, ArtifactPayload::Original(selected)
                    if selected.same_durable_artifact(product))
            });
            if is_selected {
                if metadata.entries[&owner].descriptor.producer_sha256 != producer.sha256() {
                    return Err(failure(
                        "reused candidate belongs to another canonical producer",
                    ));
                }
                reused.insert(owner);
            } else {
                fresh.push(product.clone());
            }
        }
        if !reused.is_empty() {
            let source = &context.owner.facts.compiler_support;
            let projection = source.projection().for_source_owners(&reused);
            let reused = Self {
                demand: OriginalInputDemand::Issued(source.input_decisions().clone()),
                projection,
                artifacts: source.artifacts().clone(),
                native_offers: source.native_offers().to_vec(),
            };
            carried = Some(match carried {
                Some(carried) => reused.merge(&carried)?,
                None => reused,
            });
        }
        if fresh.is_empty() {
            return Ok(carried);
        }
        let configured = Self::issue_complete_native_availability_with_source(
            context.owner.facts.compiler_support.artifacts(),
            context.owner.facts.compiler_support.projection(),
            producer.sha256(),
            &fresh,
            CompleteAvailabilitySource::Candidate(&context.owner.facts.compiler_support),
        )?;
        Ok(Some(match carried {
            Some(carried) => carried.merge(&configured)?,
            None => configured,
        }))
    }

    #[cfg(test)]
    pub(crate) fn from_selected_authored_declarations(
        context: &ExactDeclarationContext,
        producer: CanonicalProducerIdentity,
        configured: &[CertifiedRecoveryProduct],
    ) -> Result<Option<Self>, CompileError> {
        Self::with_checked_candidate_products(context, producer, configured)
    }

    pub(crate) fn from_selection(
        selection: &crate::certified_products::CertifiedSourceSelection,
        artifacts: &ArtifactView,
    ) -> Result<Self, CompileError> {
        let projection = selection
            .compiler_projection(artifacts)
            .map_err(compiler_evidence_failure)?;
        let continuation = ContinuationNativeInputs::from_selection(selection, &projection)?;
        let mut native_offers = Vec::new();
        for issued in continuation.availability_rows() {
            for offer in issued.offers() {
                if !native_offers.contains(offer) {
                    native_offers.push(offer.clone());
                }
            }
        }
        let artifacts = retain_native_offers(artifacts, &native_offers)?;
        let complete = selection
            .complete_native_products(&artifacts)
            .map_err(compiler_evidence_failure)?;
        let artifacts = if let Some(product) = complete.first() {
            let producer = product
                .module_interface()
                .ok_or_else(|| failure("complete native product has no canonical owner"))?
                .producer_sha256();
            let issued = Self::issue_complete_native_availability_with_source(
                &artifacts,
                &projection,
                producer,
                &complete,
                CompleteAvailabilitySource::Request(&continuation),
            )?;
            native_offers.extend(issued.native_offers);
            issued.artifacts
        } else {
            artifacts.clone()
        };
        for role in projection.roles() {
            if let Some(original) = role.original() {
                if !native_offers
                    .iter()
                    .any(|offer| offer.original() == original)
                {
                    return Err(failure(
                        "source selection lost its issued native availability",
                    ));
                }
            }
        }
        let artifacts = retain_native_offers(&artifacts, &native_offers)?;
        let native_offers = preserve_native_offers(&projection, &artifacts, &native_offers)?;
        Ok(Self {
            demand: OriginalInputDemand::Issued(continuation.decisions),
            projection,
            artifacts,
            native_offers,
        })
    }

    /// Supply compatible immutable code privately. Exact canonical equality
    /// upgrades an issued type role; it grants no lexical or instance authority.
    #[cfg(test)]
    pub(crate) fn from_native_availability(
        context: &ExactDeclarationContext,
        producer: CanonicalProducerIdentity,
        products: &[CertifiedRecoveryProduct],
    ) -> Result<Self, CompileError> {
        Self::from_checked_support_products(
            context.compiler_source_support(),
            producer.sha256(),
            products,
        )
    }

    fn issue_complete_native_availability_with_source(
        inventory: &ArtifactView,
        baseline: &CompilerInputProjection,
        producer_sha256: [u8; 32],
        products: &[CertifiedRecoveryProduct],
        source: CompleteAvailabilitySource<'_>,
    ) -> Result<Self, CompileError> {
        let selected = baseline.project_metadata(inventory.metadata_snapshot())?;
        let retained = inventory.metadata_snapshot();
        let mut validation = PackageInterfaceValidation::default();
        let mut entries = Vec::new();
        for product in products {
            let _ = crate::certified_products::original_available_groups(product)
                .map_err(compiler_evidence_failure)?;
            let owner = identity(&product.owner().unit, &product.owner().module);
            if let Some(previous) = selected.entries.get(&owner) {
                if canonical_source_interface(previous) != product.module_interface() {
                    return Err(failure(
                        "private native availability differs from selected canonical interface",
                    ));
                }
                if let ArtifactPayload::Original(original) = &previous.payload {
                    if !original.same_durable_artifact(product) {
                        return Err(failure(
                            "private native availability competes with selected original",
                        ));
                    }
                } else if retained.ambiguous_native_owners.contains(&owner) {
                    return Err(failure(
                        "private native availability has ambiguous retained originals",
                    ));
                }
            } else if retained.ambiguous_native_owners.contains(&owner) {
                return Err(failure(
                    "private native availability has ambiguous retained originals",
                ));
            }
            entries.push(Arc::new(ArtifactEntry::original_with_validation(
                producer_sha256,
                product.clone(),
                &mut validation,
            )?));
        }
        let projection = CompilerInputProjection::from_issued_entries(&entries)?;
        let (native_original_requirements, dependency_offers) = match source {
            CompleteAvailabilitySource::Candidate(source) => {
                // Candidate issuance consumes exact selected external demand.
                // An explicit empty decision cannot fall back to role policy.
                let preferred = baseline
                    .roles()
                    .into_iter()
                    .filter_map(|role| role.original())
                    .map(|original| {
                        crate::artifact_inventory::NativeOriginalRequirement::from_selected_view(
                            original,
                            source.artifacts(),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                let requirements =
                    capture_native_original_requirements(&projection, inventory, &preferred)?;
                let mut offers = Vec::new();
                for proof in preferred.iter().chain(&requirements) {
                    let offer = crate::artifact_inventory::IssuedNativeOffer::issue(
                        proof.original,
                        inventory,
                        Some(proof.clone()),
                    )?;
                    if !offers.contains(&offer) {
                        offers.push(offer);
                    }
                }
                (requirements, offers)
            }
            // The request already owns checked physical rows and their opaque
            // offers. Its selected view deliberately need not own their Groups.
            CompleteAvailabilitySource::Request(_) => (Vec::new(), Vec::new()),
        };
        let effective = baseline.merge(&projection)?;
        let dependencies = match source {
            CompleteAvailabilitySource::Candidate(source) => {
                crate::artifact_inventory::NativeDependencyCatalog::from_source(source)?
            }
            CompleteAvailabilitySource::Request(inputs) => {
                crate::artifact_inventory::NativeDependencyCatalog::from_request(inputs)
            }
        };
        // Physical request permissions are already frozen in its row catalog.
        // Raw parent proofs cannot become namespace grants during reissuance.
        let namespace_offers = match source {
            CompleteAvailabilitySource::Request(_) => &[][..],
            _ => dependency_offers.as_slice(),
        };
        // Available code retains custody without selecting executable groups.
        let mut artifacts = effective.admit_interfaces_with_native_offers(
            inventory,
            entries.clone(),
            namespace_offers,
            &dependencies,
        )?;
        let checked = effective.issue_complete_native_availability(
            &artifacts,
            entries,
            namespace_offers,
            &dependencies,
        )?;
        let native_offers = match source {
            CompleteAvailabilitySource::Candidate(_) => {
                let qualifications = native_original_requirements
                    .iter()
                    .map(|proof| proof.qualify_availability(&artifacts))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                checked.validate(
                    &selected.retained.selected_native_groups,
                    &qualifications,
                    &mut validation,
                )?
            }
            CompleteAvailabilitySource::Request(_) => checked.validate_request(
                &selected.retained.selected_native_groups,
                &dependencies,
                &effective,
                &artifacts,
                &mut validation,
            )?,
        };
        artifacts = artifacts.retain_issued_offer_custody(native_offers.iter().cloned())?;
        Ok(Self {
            demand: OriginalInputDemand::RolePolicy,
            projection,
            artifacts,
            native_offers,
        })
    }

    pub(crate) fn for_program_continuation(
        &self,
        support: &ArtifactView,
        admissions: &[ExactSourceAdmission],
        source: &str,
    ) -> Result<Self, CompileError> {
        let generated = ExactSourceAdmission::matching_generated_source_owner(admissions, source)?;
        let metadata = support.metadata_snapshot();
        let original = self.artifacts.metadata_snapshot();
        for role in self.projection.roles() {
            let owner = &original
                .artifacts
                .get(&role.interface())
                .ok_or_else(|| failure("private compiler input lost its issued interface"))?
                .descriptor
                .owner;
            if owner != &generated
                && (!metadata.artifacts.contains_key(&role.interface())
                    || role
                        .original()
                        .is_some_and(|id| !metadata.artifacts.contains_key(&id)))
            {
                return Err(failure(
                    "private compiler continuation omitted a selected original",
                ));
            }
        }
        let projection = self.projection.within_view(support);
        projection.validate(support)?;
        let native_offers = preserve_native_offers(&projection, support, &self.native_offers)?;
        let artifacts = retain_native_offers(support, &native_offers)?;
        let demand = match &self.demand {
            OriginalInputDemand::RolePolicy => OriginalInputDemand::RolePolicy,
            OriginalInputDemand::Issued(decisions) => {
                OriginalInputDemand::Issued(decisions.restricted(&projection, &artifacts))
            }
        };
        Ok(Self {
            demand,
            projection,
            artifacts,
            native_offers,
        })
    }

    fn merge(&self, other: &Self) -> Result<Self, CompileError> {
        let (artifacts, projection, decisions) =
            ArtifactView::merge_original_fragments(self, other)?;
        // This joins alternatives, rather than selecting a caller namespace.
        // Only an explicit request or continuation may narrow an issued offer.
        let mut native_offers = self.native_offers.clone();
        for offer in &other.native_offers {
            if !native_offers.contains(offer) {
                native_offers.push(offer.clone());
            }
        }
        let demand = OriginalInputDemand::Issued(decisions);
        Ok(Self {
            demand,
            projection,
            artifacts,
            native_offers,
        })
    }

    #[cfg(test)]
    pub(crate) fn native_original_requirements(
        &self,
    ) -> Vec<crate::artifact_inventory::NativeOriginalRequirement> {
        native_requirement_facts(&self.native_offers)
    }
}

pub(crate) struct RequestCompilerInputs {
    // Retained availability outlives the transport subset consumed by this request.
    source_support: Arc<crate::artifact_inventory::CompilerSourceSupport>,
    pub(crate) metadata: CompilerArtifactMetadata,
    native_transport: NativeCompilerTransport,
    declaration_semantic_sha256: [u8; 32],
    original_sources: crate::certified_products::RetainedOriginalSources,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeTransportAvailability {
    original: Arc<ArtifactEntry>,
    mode: NativeTransportMode,
    classification: NativeInputClassification,
    contributions: Arc<[Arc<crate::artifact_inventory::NativeOfferContribution>]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeInputClassification {
    CompilerRole,
    PrivateInput,
}

#[derive(Clone, Debug)]
enum NativeTransportMode {
    Full,
    IssuedGroups,
}

/// One validated continuation supplies both physical availability and choices.
/// No caller can combine rows from one request with another request's decisions.
pub(crate) struct ContinuationNativeInputs<'a> {
    _selection: &'a crate::certified_products::CertifiedSourceSelection,
    projection: CompilerInputProjection,
    rows: Vec<Arc<NativeTransportAvailability>>,
    decisions: crate::artifact_inventory::IssuedInputNativeDecisions,
}

impl<'a> ContinuationNativeInputs<'a> {
    fn from_selection(
        selection: &'a crate::certified_products::CertifiedSourceSelection,
        projection: &CompilerInputProjection,
    ) -> Result<Self, CompileError> {
        Ok(Self {
            _selection: selection,
            projection: projection.clone(),
            rows: selection.issued_native_availability().cloned().collect(),
            decisions:
                crate::artifact_inventory::IssuedInputNativeDecisions::from_certified_continuation(
                    selection, projection,
                )?,
        })
    }

    pub(crate) fn projection(&self) -> &CompilerInputProjection {
        &self.projection
    }

    pub(crate) fn availability_rows(&self) -> &[Arc<NativeTransportAvailability>] {
        &self.rows
    }
    pub(crate) fn input_decisions(&self) -> &crate::artifact_inventory::IssuedInputNativeDecisions {
        &self.decisions
    }
}

#[derive(Clone, Copy)]
enum CompleteAvailabilitySource<'a> {
    Candidate(&'a crate::artifact_inventory::CompilerSourceSupport),
    Request(&'a ContinuationNativeInputs<'a>),
}

impl NativeTransportAvailability {
    pub(crate) fn original_owner(&self) -> &tidepool_repr::execution_schema::CachedHomeOwner {
        let ArtifactPayload::Original(product) = &self.original.payload else {
            unreachable!("transport issuer checked original payload")
        };
        product.owner()
    }

    /// Borrow the checked physical row for one demanded exact owner. This
    /// supplies payload identity only; target permission remains a per-key query.
    pub(crate) fn target_original(
        &self,
        owner: &tidepool_repr::execution_schema::CachedHomeOwner,
    ) -> Option<&Arc<ArtifactEntry>> {
        (self.original_owner() == owner).then_some(&self.original)
    }

    /// Resolve one dependency through this physical request's issued row.
    /// A retained parent proof cannot grant another row's or ordinal's rights.
    pub(crate) fn dependency_support(
        &self,
        key: NativeGroupKey,
        owner: &tidepool_repr::execution_schema::CachedHomeOwner,
    ) -> Option<NativeDependencySupport<'_>> {
        (self.original.descriptor.id == key.artifact
            && self.original_owner() == owner
            && self.permits(key.original_ordinal))
        .then_some(NativeDependencySupport {
            availability: self,
            key,
        })
    }

    pub(crate) fn offers(
        &self,
    ) -> impl ExactSizeIterator<Item = &crate::artifact_inventory::IssuedNativeOffer> {
        self.contributions
            .iter()
            .map(|contribution| contribution.offer())
    }

    pub(crate) fn permits(&self, ordinal: u32) -> bool {
        match &self.mode {
            NativeTransportMode::Full => true,
            NativeTransportMode::IssuedGroups => self.contributions.iter().any(|contribution| {
                contribution.permits(crate::artifact_inventory::NativeGroupKey {
                    artifact: self.original.descriptor.id,
                    original_ordinal: ordinal,
                })
            }),
        }
    }

    fn issued_ordinals(&self) -> BTreeSet<u32> {
        self.contributions
            .iter()
            .flat_map(|contribution| contribution.ordinals(self.original.descriptor.id))
            .collect()
    }
}

/// A borrowed exact-key answer from an existing request availability owner.
/// Its construction cannot bypass the row's identity and availability mode.
pub(crate) struct NativeDependencySupport<'a> {
    availability: &'a NativeTransportAvailability,
    key: NativeGroupKey,
}

impl<'a> NativeDependencySupport<'a> {
    pub(crate) fn offer_for_group(
        &self,
        group: crate::artifact_inventory::NativeGroupBinding,
    ) -> Option<&'a crate::artifact_inventory::IssuedNativeOffer> {
        self.availability
            .contributions
            .iter()
            .find_map(|contribution| {
                contribution
                    .groups(self.key)
                    .any(|candidate| candidate == group)
                    .then(|| contribution.offer())
            })
    }

    pub(crate) fn groups(
        &self,
    ) -> impl Iterator<Item = crate::artifact_inventory::NativeGroupBinding> + '_ {
        self.availability
            .contributions
            .iter()
            .flat_map(|contribution| contribution.groups(self.key))
    }
}

/// Issuer-bound input availability, independent of output visibility and roles.
/// Only transport issuance constructs a nonempty component; clones retain its
/// exact payload owners and opaque availability handles.
#[derive(Clone, Debug, Default)]
pub(crate) struct RequestNativeAvailability(
    Arc<BTreeMap<ArtifactId, Arc<NativeTransportAvailability>>>,
);

impl RequestNativeAvailability {
    pub(crate) fn issued(&self) -> impl Iterator<Item = &Arc<NativeTransportAvailability>> {
        self.0
            .values()
            .filter(|row| row.classification == NativeInputClassification::PrivateInput)
    }

    pub(crate) fn receipt_original(
        &self,
        unit: &str,
        module: &str,
        version: &tidepool_repr::execution_schema::ModuleVersion,
    ) -> Result<Option<&NativeTransportAvailability>, CompileError> {
        let mut candidates = self.issued().filter(|row| {
            let owner = row.original_owner();
            owner.unit == unit && owner.module == module && &owner.module_version == version
        });
        let first = candidates.next();
        if candidates.next().is_some() {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeOwnerAmbiguity {
                    owner: first.unwrap().original.descriptor.owner.clone(),
                },
            ));
        }
        Ok(first.map(Arc::as_ref))
    }

    pub(crate) fn requirements(
        &self,
    ) -> impl Iterator<Item = &crate::artifact_inventory::NativeOriginalRequirement> {
        self.issued()
            .flat_map(|availability| availability.offers().filter_map(|offer| offer.proof()))
    }

    pub(crate) fn permits(
        &self,
        original: ArtifactId,
        owner: &tidepool_repr::execution_schema::CachedHomeOwner,
        ordinal: u32,
    ) -> Option<bool> {
        self.0
            .get(&original)
            .filter(|row| row.classification == NativeInputClassification::PrivateInput)
            .map(|availability| {
                availability.original_owner() == owner && availability.permits(ordinal)
            })
    }
}

/// Transport can own dependency bodies without issuing compiler namespace roles.
/// Exact issuer bindings remain in the offers; transport records body availability.
struct NativeCompilerTransport {
    metadata: CompilerArtifactMetadata,
    availability: Arc<BTreeMap<ArtifactId, Arc<NativeTransportAvailability>>>,
}

impl NativeCompilerTransport {
    /// An advertised body can be demanded by this compilation even if it was
    /// not an executable root of the inherited view. Preserve its authenticated
    /// historical identities so a missing value reaches the runtime resolver.
    /// These tags grant neither native availability nor retained heap roots.
    fn retained_generation_tags(
        &self,
        mut retained: BTreeMap<tidepool_extract_cmd::SymbolIdentity, u64>,
    ) -> Result<BTreeMap<tidepool_extract_cmd::SymbolIdentity, u64>, CompileError> {
        for (original, availability) in self.availability.iter() {
            let entry = self
                .metadata
                .retained
                .artifacts
                .get(original)
                .ok_or_else(|| failure("advertised native original is outside request custody"))?;
            let ArtifactPayload::Original(product) = &entry.payload else {
                return Err(failure("advertised native artifact is not an original"));
            };
            for ordinal in entry
                .native_group_ordinals
                .iter()
                .filter(|ordinal| availability.permits(**ordinal))
            {
                let imports =
                    crate::certified_products::original_native_group_imports(product, *ordinal)
                        .ok_or_else(|| {
                            failure("advertised native body lacks authenticated imports")
                        })?;
                retained = certified_retained_generation_tags(retained, imports.iter())?;
            }
        }
        Ok(retained)
    }

    fn issue(
        roles: &CompilerInputProjection,
        projection: &CompilerArtifactMetadata,
        artifacts: &ArtifactView,
        offers: &[crate::artifact_inventory::IssuedNativeOffer],
    ) -> Result<Self, CompileError> {
        let mut metadata = CompilerArtifactMetadata {
            retained: artifacts.metadata_snapshot(),
            entries: projection.entries.clone(),
        };
        let mut availability = BTreeMap::<
            ArtifactId,
            Vec<Arc<crate::artifact_inventory::NativeOfferContribution>>,
        >::new();
        for entry in projection
            .entries
            .values()
            .filter(|entry| matches!(entry.payload, ArtifactPayload::Original(_)))
        {
            availability.insert(entry.descriptor.id, Vec::new());
        }
        let offered_roots = availability.keys().copied().collect::<BTreeSet<_>>();
        let mut full = BTreeSet::new();
        let mut originals_by_owner = BTreeMap::new();
        for offer in offers {
            if offer
                .qualified_private_availability(roles, artifacts)
                .is_none()
            {
                continue;
            }
            let entry = metadata
                .retained
                .artifacts
                .get(&offer.original())
                .ok_or_else(|| failure("qualified native original is outside request custody"))?;
            if originals_by_owner
                .insert(entry.descriptor.owner.clone(), offer.original())
                .is_some_and(|previous| previous != offer.original())
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeOwnerAmbiguity {
                        owner: entry.descriptor.owner.clone(),
                    },
                ));
            }
        }
        for offer in offers {
            let private = offer.qualified_private_availability(roles, artifacts);
            let mut contribution_roots = offered_roots.clone();
            if private.is_some() {
                contribution_roots.insert(offer.original());
            }
            let Some(contribution) = offer.contribution_for(&contribution_roots)? else {
                continue;
            };
            let consumed = if offered_roots.contains(&offer.original()) {
                offer
                    .within_projection(roles, artifacts)?
                    .ok_or_else(|| failure("selected native offer has no request contribution"))?
            } else {
                offer.clone()
            };
            // Qualification can demote Full, but cannot change exact membership.
            let contribution = if consumed.is_full() == offer.is_full() {
                contribution
            } else {
                consumed
                    .contribution_for(&contribution_roots)?
                    .expect("same checked contribution")
            };
            if (offered_roots.contains(&consumed.original()) && consumed.is_full())
                || private.is_some()
            {
                if !contribution.originals().contains(&consumed.original()) {
                    return Err(failure("full native offer has no issued carrier"));
                }
                full.insert(consumed.original());
            }
            let contribution = Arc::new(contribution);
            if offered_roots.contains(&consumed.original()) || private.is_some() {
                let contributions = availability.entry(consumed.original()).or_default();
                if !contributions
                    .iter()
                    .any(|prior| prior.offer() == contribution.offer())
                {
                    contributions.push(Arc::clone(&contribution));
                }
            }
            for original in contribution.originals() {
                let entry = metadata
                    .retained
                    .artifacts
                    .get(original)
                    .ok_or_else(|| failure("issued native dependency is outside request custody"))?
                    .clone();
                crate::artifact_inventory::validate_consumed_native_namespace(
                    &entry,
                    &metadata.entries,
                )?;
                metadata
                    .entries
                    .insert(entry.descriptor.owner.clone(), entry);
                let contributions = availability.entry(*original).or_default();
                if !contributions
                    .iter()
                    .any(|prior| prior.offer() == contribution.offer())
                {
                    contributions.push(Arc::clone(&contribution));
                }
            }
        }
        for selected in &projection.retained.selected_native_groups {
            if !projection
                .entries
                .values()
                .any(|entry| entry.descriptor.id == selected.artifact)
            {
                continue;
            }
            let Some(contributions) = availability.get(&selected.artifact) else {
                continue;
            };
            if !full.contains(&selected.artifact)
                && !contributions
                    .iter()
                    .any(|contribution| contribution.permits(*selected))
            {
                return Err(failure(
                    "selected native group is outside issued availability",
                ));
            }
        }
        let role_originals = roles
            .roles()
            .into_iter()
            .filter_map(|role| role.original())
            .collect::<BTreeSet<_>>();
        let availability = availability
            .into_iter()
            .map(|(id, contributions)| {
                let original = metadata
                    .retained
                    .artifacts
                    .get(&id)
                    .ok_or_else(|| failure("issued native original is outside request custody"))?
                    .clone();
                if original.descriptor.id != id
                    || !matches!(original.payload, ArtifactPayload::Original(_))
                {
                    return Err(failure(
                        "issued native availability lacks its exact original payload",
                    ));
                }
                let mode = if full.contains(&id) {
                    NativeTransportMode::Full
                } else {
                    NativeTransportMode::IssuedGroups
                };
                Ok((
                    id,
                    Arc::new(NativeTransportAvailability {
                        original,
                        mode,
                        classification: if role_originals.contains(&id) {
                            NativeInputClassification::CompilerRole
                        } else {
                            NativeInputClassification::PrivateInput
                        },
                        contributions: contributions.into(),
                    }),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        let availability = Arc::new(availability);
        Ok(Self {
            metadata,
            availability,
        })
    }
}

/// Materialization may privately contain execution graph bytes for dependency
/// originals. Only the unchanged compiler roles issue their replay roots.
fn compiler_execution_scope(
    scope: &Value,
    compiler: &CompilerArtifactMetadata,
) -> Result<Value, CompileError> {
    let fields = row(scope, 2)?;
    for graph in list(&fields[0], 4096)? {
        let graph = row(graph, 2)?;
        string(&graph[0])?;
        string(&graph[1])?;
    }
    let allowed = compiler
        .entries
        .values()
        .filter_map(|entry| match &entry.payload {
            ArtifactPayload::Original(product) => Some(product.owner()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut roots = Vec::new();
    for root in list(&fields[1], 4096)? {
        let fields = row(root, 6)?;
        let unit = string(&fields[0])?;
        let module = string(&fields[1])?;
        let version = string(&fields[2])?;
        let interface = string(&fields[3])?;
        let product = string(&fields[4])?;
        string(&fields[5])?;
        if allowed.iter().any(|owner| {
            owner.unit == unit
                && owner.module == module
                && hex(&owner.module_version.0) == version
                && hex(&owner.skinny_iface_sha256) == interface
                && hex(&owner.product_sha256) == product
        }) {
            roots.push(root.clone());
        }
    }
    Ok(Value::Array(vec![fields[0].clone(), Value::Array(roots)]))
}

impl RequestCompilerInputs {
    pub(crate) fn shared_compiler_source_support(
        &self,
    ) -> Arc<crate::artifact_inventory::CompilerSourceSupport> {
        Arc::clone(&self.source_support)
    }

    pub(crate) fn compiler_source_support(
        &self,
    ) -> &crate::artifact_inventory::CompilerSourceSupport {
        &self.source_support
    }

    pub(crate) fn projection(&self) -> &CompilerInputProjection {
        self.source_support.projection()
    }

    pub(crate) fn artifacts(&self) -> &ArtifactView {
        self.source_support.artifacts()
    }

    #[cfg(test)]
    pub(crate) fn native_original_requirements(
        &self,
    ) -> Vec<crate::artifact_inventory::NativeOriginalRequirement> {
        let mut facts = Vec::new();
        for proof in self
            .native_transport
            .availability
            .values()
            .flat_map(|row| row.offers())
            .filter_map(|offer| offer.proof())
        {
            if !facts.contains(proof) {
                facts.push(proof.clone());
            }
        }
        facts
    }

    pub(crate) fn native_availability(
        &self,
        original: ArtifactId,
    ) -> Option<Arc<NativeTransportAvailability>> {
        self.native_transport.availability.get(&original).cloned()
    }

    pub(crate) fn private_native_availability(&self) -> RequestNativeAvailability {
        RequestNativeAvailability(Arc::clone(&self.native_transport.availability))
    }

    pub(crate) fn available_original_sources(
        &self,
        products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
        operation: &Arc<tidepool_repr::execution_schema::InventoryOperation>,
    ) -> Result<
        Arc<crate::certified_products::AvailableOriginalSources>,
        crate::certified_products::CertificationError,
    > {
        self.original_sources.admit(products, operation)
    }
}

#[cfg(test)]
thread_local! {
    static REQUEST_COMPILER_INPUT_COMPUTATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl ProtectedScaffoldRequirements {
    pub(crate) fn role_requirements(&self) -> &CompilerInputProjection {
        &self.projection
    }

    #[tracing::instrument(target = "exomonad_harness::timing", name = "exact.compiler_inputs", level = "debug", skip_all, fields(inclusive = true, private = private.is_some()))]
    fn compiler_inputs(
        &self,
        context: &ExactDeclarationContext,
        private: Option<&OriginalCompilerInputs>,
    ) -> Result<RequestCompilerInputs, CompileError> {
        #[cfg(test)]
        REQUEST_COMPILER_INPUT_COMPUTATIONS.with(|count| count.set(count.get() + 1));
        let declaration_semantic_sha256 = context.semantic_sha256();
        let support = context
            .compiler_source_support()
            .close_request(private, self)?;
        let metadata = support
            .projection()
            .project_metadata(support.artifacts().metadata_snapshot())?;
        let native_transport = NativeCompilerTransport::issue(
            support.projection(),
            &metadata,
            support.artifacts(),
            support.native_offers(),
        )?;
        Ok(RequestCompilerInputs {
            source_support: Arc::new(support),
            metadata,
            native_transport,
            declaration_semantic_sha256,
            original_sources: Default::default(),
        })
    }
}

impl ExactCompileContext {
    pub fn new(declarations: Arc<ExactDeclarationContext>) -> Self {
        Self {
            declarations,
            request_annotations: None,
            protected_scaffold: Default::default(),
        }
    }

    pub fn declarations(&self) -> &Arc<ExactDeclarationContext> {
        &self.declarations
    }

    pub fn request_types(&self) -> Option<&Arc<crate::checked_cell::RequestTypeSignatures>> {
        self.request_annotations
            .as_ref()
            .map(RequestAnnotations::signatures)
    }

    pub fn request_annotations(&self) -> Option<&RequestAnnotations> {
        self.request_annotations.as_ref()
    }

    /// Replace signature authentication while preserving the chosen helper mode.
    pub fn with_request_types(
        mut self,
        signatures: Arc<crate::checked_cell::RequestTypeSignatures>,
    ) -> Self {
        self.request_annotations = Some(RequestAnnotations::new(
            signatures,
            self.request_helper_recipe(),
        ));
        self
    }

    pub fn with_request_helper_recipe(
        mut self,
        recipe: RequestHelperRecipe,
    ) -> Result<Self, CompileError> {
        match &mut self.request_annotations {
            Some(annotations) => annotations.helper_recipe = recipe,
            None if recipe == RequestHelperRecipe::None => {}
            None => {
                return Err(CompileError::ExtractFailed(
                    "actor reply helpers require original request type signatures".into(),
                ));
            }
        }
        Ok(self)
    }

    pub fn request_helper_recipe(&self) -> RequestHelperRecipe {
        self.request_annotations
            .as_ref()
            .map_or(RequestHelperRecipe::None, RequestAnnotations::helper_recipe)
    }

    pub(crate) fn with_declarations(mut self, declarations: Arc<ExactDeclarationContext>) -> Self {
        self.declarations = declarations;
        self
    }

    pub(crate) fn with_generated_planned_imports<'a>(
        self,
        certificate: Option<&Arc<CertifiedAuthoredDeclaration>>,
        templates: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, CompileError> {
        let Some(certificate) = certificate else {
            return Ok(self);
        };
        if certificate.toolchain_identity_sha256() != self.declarations.owner.facts.producer {
            return Err(failure(
                "checked recipe original differs from its admitted certificate",
            ));
        }
        let metadata = self.declarations.compiler_metadata_snapshot()?;
        let matching = metadata
            .retained
            .artifacts
            .values()
            .filter(|entry| {
                matches!(&entry.payload, ArtifactPayload::Original(product)
                if product == certificate.product())
            })
            .collect::<Vec<_>>();
        let [original] = matching.as_slice() else {
            return Err(failure(
                "checked recipe original differs from its admitted certificate",
            ));
        };
        let original = Arc::clone(original);
        let templates = templates
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .into();
        self.with_planned_scaffold_role(original, Arc::clone(certificate), templates, &metadata)
    }

    /// Issue one compiler-only role against the selected canonical interface.
    /// Validate the complete request projection so sealed home dependencies use
    /// the same already issued roles as materialization and the compiler.
    fn with_planned_scaffold_role(
        mut self,
        original: Arc<ArtifactEntry>,
        certificate: Arc<CertifiedAuthoredDeclaration>,
        templates: Arc<[String]>,
        metadata: &CompilerArtifactMetadata,
    ) -> Result<Self, CompileError> {
        let ArtifactPayload::Original(product) = &original.payload else {
            return Err(failure(
                "protected scaffold lacks an original native carrier",
            ));
        };
        let selected = metadata
            .entries
            .get(&identity(&product.owner().unit, &product.owner().module))
            .ok_or_else(|| failure("protected scaffold lacks a selected canonical interface"))?;
        if canonical_source_interface(selected) != product.module_interface()
            || certificate.product().owner() != product.owner()
        {
            return Err(failure(
                "protected scaffold differs from its selected canonical interface",
            ));
        }
        let projection = CompilerInputProjection::from_issued_entries(&[Arc::clone(&original)])?;
        let protected_projection = self.protected_scaffold.projection.merge(&projection)?;
        let effective = self
            .declarations
            .owner
            .facts
            .compiler_support
            .projection()
            .merge(&protected_projection)?;
        crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
            &effective,
            &metadata.retained,
            &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
        )
        .map_err(compiler_evidence_failure)?;
        self.protected_scaffold.projection = protected_projection;
        self.protected_scaffold
            .native
            .push((certificate, templates));
        Ok(self)
    }

    fn authorization(&self, purpose: Option<Value>) -> Option<Value> {
        match &self.request_annotations {
            Some(annotations) => Some(encode_request_authorization(
                &annotations.signatures,
                annotations.helper_recipe,
                purpose,
            )),
            None => purpose,
        }
    }

    pub(crate) fn prepare_compilation(
        &self,
        root: &Path,
        producer: &[u8],
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.prepare_compilation_with_authorization(root, producer, None)
    }

    pub(crate) fn prepare_compilation_with_authorization(
        &self,
        root: &Path,
        producer: &[u8],
        authorization: Option<Value>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.prepare_compilation_with_private_input(root, producer, authorization, None)
    }

    pub(crate) fn prepare_compilation_with_private_input(
        &self,
        root: &Path,
        producer: &[u8],
        authorization: Option<Value>,
        private: Option<OriginalCompilerInputs>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.declarations.prepare_compilation_with_scaffold(
            root,
            producer,
            self.authorization(authorization),
            &self.protected_scaffold,
            private,
        )
    }

    pub(crate) fn prepare_compilation_authorizing_with_private_input(
        &self,
        root: &Path,
        producer: &[u8],
        authorize: impl FnOnce(
            [u8; 32],
        ) -> Result<(Value, Option<OriginalCompilerInputs>), CompileError>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.declarations
            .prepare_compilation_authorizing_with_scaffold(
                root,
                producer,
                &self.protected_scaffold,
                |semantic| {
                    let (authorization, private) = authorize(semantic)?;
                    Ok((
                        self.authorization(Some(authorization))
                            .expect("purpose authorization"),
                        private,
                    ))
                },
            )
    }
}

impl From<Arc<ExactDeclarationContext>> for ExactCompileContext {
    fn from(declarations: Arc<ExactDeclarationContext>) -> Self {
        Self::new(declarations)
    }
}

const EXACT_SCOPE_BYTES_LIMIT: usize = 4 << 20;
const EXACT_SCOPE_SCHEMA_VERSION: &str = "16";
const EXACT_SCOPE_SCHEMA_FIELDS: usize = 11;
const EXACT_SCOPE_GRAPHS_LIMIT: usize = 4096;

fn validate_execution_graph_budget<'a>(
    graphs: impl IntoIterator<Item = &'a crate::execution_source::CertifiedExecutionSourceGraph>,
) -> Result<(), CompileError> {
    let fits = graphs
        .into_iter()
        .try_fold((0usize, 0usize), |(count, bytes), graph| {
            let count = count.checked_add(1)?;
            let bytes = bytes.checked_add(graph.bytes().len())?;
            (count <= EXACT_SCOPE_GRAPHS_LIMIT
                && bytes <= crate::execution_source::GRAPH_BYTES_LIMIT)
                .then_some((count, bytes))
        })
        .is_some();
    if fits {
        Ok(())
    } else {
        Err(failure(
            "original execution graphs exceed their 64 MiB or 4096 graph bound",
        ))
    }
}

fn canonical_source_interface(
    entry: &crate::artifact_inventory::ArtifactEntry,
) -> Option<&crate::certified_products::CertifiedModuleInterface> {
    match &entry.payload {
        ArtifactPayload::Canonical(interface) => Some(interface),
        ArtifactPayload::Original(product) => product.module_interface(),
        ArtifactPayload::Interface(_, _) => None,
    }
}

#[cfg(test)]
fn original_products(entries: &[Arc<ArtifactEntry>]) -> Vec<&CertifiedRecoveryProduct> {
    entries
        .iter()
        .filter_map(|entry| match &entry.payload {
            ArtifactPayload::Original(product) => Some(product),
            _ => None,
        })
        .collect()
}

fn original_products_by_id(
    entries: &[Arc<ArtifactEntry>],
) -> BTreeMap<ArtifactId, &CertifiedRecoveryProduct> {
    entries
        .iter()
        .filter_map(|entry| match &entry.payload {
            ArtifactPayload::Original(product) => Some((entry.descriptor.id, product)),
            _ => None,
        })
        .collect()
}

fn materialization_bytes(entries: &[Arc<ArtifactEntry>]) -> u64 {
    entries
        .iter()
        .map(|entry| match &entry.payload {
            ArtifactPayload::Original(product) => {
                product.interface_bytes().len() as u64
                    + product.product_bytes().len() as u64
                    + product.package_imports_bytes().len() as u64
                    + product.certification_bytes().len() as u64
            }
            ArtifactPayload::Canonical(interface) => {
                interface.interface_bytes().len() as u64
                    + interface.package_imports_bytes().len() as u64
                    + interface.certificate_bytes().len() as u64
                    + interface.core_bytes().map_or(0, |bytes| bytes.len() as u64)
            }
            ArtifactPayload::Interface(interface, _) => {
                interface.interface_bytes().len() as u64
                    + interface.package_imports_bytes().len() as u64
            }
        })
        .sum()
}

/// Execution recipes are selected by original artifact custody. A digest can
/// check a selected graph but cannot discover a source owner or grant visibility.
#[cfg(test)]
fn execution_scope_value(
    entries: &[Arc<ArtifactEntry>],
    root: &Path,
) -> Result<Option<Value>, CompileError> {
    execution_scope_value_with_graph_paths(entries, root, &mut BTreeMap::new(), &mut 0)
}

fn execution_scope_value_with_graph_paths(
    entries: &[Arc<ArtifactEntry>],
    root: &Path,
    graph_paths: &mut BTreeMap<[u8; 32], PathBuf>,
    written_bytes: &mut u64,
) -> Result<Option<Value>, CompileError> {
    let originals = entries
        .iter()
        .filter_map(|entry| match &entry.payload {
            ArtifactPayload::Original(product) => Some((
                (product.owner().unit.clone(), product.owner().module.clone()),
                product,
            )),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let graphs = originals
        .values()
        .filter_map(|product| product.execution_source())
        .map(|graph| (graph.digest(), graph))
        .collect::<BTreeMap<_, _>>();
    if graphs.is_empty() {
        return Ok(None);
    }
    validate_execution_graph_budget(graphs.values().map(|graph| graph.as_ref()))?;
    let mut roots = Vec::new();
    let mut admitted_graphs = BTreeSet::new();
    for product in originals.values() {
        let Some(graph) = product.execution_source() else {
            continue;
        };
        let mut pending = vec![(product.owner().clone(), graph.digest())];
        let mut seen = BTreeSet::new();
        let mut closed = true;
        while let Some((owner, digest)) = pending.pop() {
            let key = (
                owner.unit.clone(),
                owner.module.clone(),
                owner.module_version.0,
                owner.skinny_iface_sha256,
                owner.product_sha256,
                digest,
            );
            if !seen.insert(key) {
                continue;
            }
            let Some(required) = originals.get(&(owner.unit.clone(), owner.module.clone())) else {
                closed = false;
                break;
            };
            let Some(required_graph) = required.execution_source() else {
                closed = false;
                break;
            };
            if required_graph
                .required_source_owners(&owner)
                .iter()
                .any(|source| {
                    originals
                        .get(&(source.unit.clone(), source.module.clone()))
                        .is_none_or(|original| original.owner() != source)
                })
                || required.owner() != &owner
                || required_graph.digest() != digest
                || !required_graph.eligible_source_replay_root(&owner)
            {
                closed = false;
                break;
            }
            pending.extend(required_graph.required_original_graphs(&owner));
        }
        if closed {
            admitted_graphs.extend(seen.into_iter().map(|entry| entry.5));
            let owner = product.owner();
            roots.push(Value::Array(vec![
                text(&owner.unit),
                text(&owner.module),
                text(hex(&owner.module_version.0)),
                text(hex(&owner.skinny_iface_sha256)),
                text(hex(&owner.product_sha256)),
                text(hex(&graph.digest())),
            ]));
        }
    }
    Ok(Some(Value::Array(vec![
        Value::Array(
            graphs
                .into_iter()
                .filter(|(digest, _)| admitted_graphs.contains(digest))
                .map(|(digest, graph)| {
                    let path = if let Some(file) = graph_paths.get(&digest) {
                        file.clone()
                    } else {
                        let path = root.join(format!("execution-{}.cbor", hex(&digest)));
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)?;
                        file.write_all(graph.bytes())?;
                        *written_bytes += graph.bytes().len() as u64;
                        graph_paths.insert(digest, path.clone());
                        path
                    };
                    Ok(Value::Array(vec![text(hex(&digest)), path_value(&path)?]))
                })
                .collect::<Result<Vec<_>, CompileError>>()?,
        ),
        Value::Array(roots),
    ])))
}

#[cfg(test)]
fn encode_scope_manifest(
    fields: Vec<Value>,
    execution_scope: Option<Value>,
    authorization: Option<Value>,
) -> Result<Vec<u8>, CompileError> {
    encode_scope_manifest_with_published(
        fields,
        execution_scope,
        authorization,
        Value::Array(vec![]),
        Value::Array(vec![text("fresh-files")]),
    )
}

fn encode_scope_manifest_with_published(
    mut fields: Vec<Value>,
    execution_scope: Option<Value>,
    authorization: Option<Value>,
    published: Value,
    acquisition: Value,
) -> Result<Vec<u8>, CompileError> {
    fields[1] = text(EXACT_SCOPE_SCHEMA_VERSION);
    fields.push(execution_scope.unwrap_or(Value::Null));
    fields.push(authorization.unwrap_or(Value::Null));
    fields.push(published);
    fields.push(acquisition);
    if fields.len() != EXACT_SCOPE_SCHEMA_FIELDS {
        return Err(failure("invalid current exact scope field count"));
    }
    let value = Value::Array(fields);
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut bytes).map_err(failure)?;
    if bytes.len() > EXACT_SCOPE_BYTES_LIMIT {
        return Err(failure("scope manifest exceeds four MiB"));
    }
    Ok(bytes)
}

/// Immutable payload custody for one recovery manifest. Graph selections and
/// compiler authority are authenticated separately for every exact context.
pub struct RecoveredArtifactAcquisition {
    root: PathBuf,
    payloads: BTreeMap<ArtifactId, AcquiredRecoveryPayload>,
    work: recovery_artifacts::RecoveryArtifactWork,
}

#[derive(Clone)]
struct AcquiredRecoveryPayload {
    reference: RecoveryPayloadReference,
    entry: Arc<ArtifactEntry>,
}

#[derive(Clone, Eq, PartialEq)]
enum RecoveryPayloadReference {
    Original(RecoveryArtifactRef),
    Canonical(recovery_artifacts::RecoveryModuleInterfaceRef),
    Join(RecoveryJoinRef),
    Value(RecoveryValueInterfaceRef),
}

impl RecoveryPayloadReference {
    fn id(&self) -> ArtifactId {
        use crate::artifact_inventory::ArtifactDescriptor;
        match self {
            Self::Original(reference) => ArtifactDescriptor::from_recovery_product(reference).id,
            Self::Canonical(reference) => {
                ArtifactDescriptor::from_recovery_module_interface(reference).id
            }
            Self::Join(reference) => ArtifactDescriptor::from_recovery_join(reference).id,
            Self::Value(reference) => {
                ArtifactDescriptor::from_recovery_value_interface(reference).id
            }
        }
    }
}

impl RecoveredArtifactAcquisition {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            payloads: BTreeMap::new(),
            work: Default::default(),
        }
    }

    fn references(
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
    ) -> Result<BTreeMap<ArtifactId, RecoveryPayloadReference>, RecoveryInventoryError> {
        let mut references = BTreeMap::new();
        let mut originals = BTreeSet::new();
        for reference in products {
            let reference = RecoveryPayloadReference::Original(reference.clone());
            if !originals.insert(reference.id()) {
                return Err(failure("duplicate recovery original").into());
            }
        }
        for reference in products
            .iter()
            .cloned()
            .map(RecoveryPayloadReference::Original)
            .chain(
                module_interfaces
                    .iter()
                    .cloned()
                    .map(RecoveryPayloadReference::Canonical),
            )
            .chain(
                products
                    .iter()
                    .filter_map(|product| product.module_interface.clone())
                    .map(RecoveryPayloadReference::Canonical),
            )
            .chain(joins.iter().cloned().map(RecoveryPayloadReference::Join))
            .chain(values.iter().cloned().map(RecoveryPayloadReference::Value))
        {
            if references
                .get(&reference.id())
                .is_some_and(|previous| previous != &reference)
            {
                return Err(
                    failure("recovery payload identity has conflicting exact references").into(),
                );
            }
            references.insert(reference.id(), reference);
        }
        Ok(references)
    }

    /// Copy only demanded immutable handles before leaving the owner's short
    /// publication lock. Physical acquisition runs on this local stage.
    pub fn stage_inputs(
        &self,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
    ) -> Result<Self, RecoveryInventoryError> {
        let references = Self::references(products, module_interfaces, joins, values)?;
        let mut stage = Self::new(&self.root);
        for (id, reference) in references {
            if let Some(held) = self.payloads.get(&id) {
                if held.reference != reference {
                    return Err(failure("held recovery payload has another exact reference").into());
                }
                stage.payloads.insert(id, held.clone());
            }
        }
        Ok(stage)
    }

    /// Authenticate missing payloads, then validate this exact graph without
    /// borrowing another snapshot's edges, roles or lexical requirements.
    pub fn capture_bound(
        &mut self,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        descriptors: &[crate::artifact_inventory::ArtifactDescriptor],
        bindings: &[ArtifactBinding],
        edges: &[(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)],
    ) -> Result<RecoveredArtifactInventory, RecoveryInventoryError> {
        let references = Self::references(products, module_interfaces, joins, values)?;
        let mut missing_products = Vec::new();
        let mut missing_interfaces = Vec::new();
        let mut missing_joins = Vec::new();
        let mut missing_values = Vec::new();
        let mut held_interfaces = BTreeMap::new();
        for (id, reference) in &references {
            if let Some(held) = self.payloads.get(id) {
                if &held.reference != reference {
                    return Err(failure("held recovery payload has another exact reference").into());
                }
                if let (
                    RecoveryPayloadReference::Canonical(reference),
                    ArtifactPayload::Canonical(interface),
                ) = (reference, &held.entry.payload)
                {
                    held_interfaces.insert(reference.clone(), interface.clone());
                }
                continue;
            }
            match reference {
                RecoveryPayloadReference::Original(reference) => {
                    missing_products.push(reference.clone())
                }
                RecoveryPayloadReference::Canonical(reference) => {
                    missing_interfaces.push(reference.clone())
                }
                RecoveryPayloadReference::Join(reference) => missing_joins.push(reference.clone()),
                RecoveryPayloadReference::Value(reference) => {
                    missing_values.push(reference.clone())
                }
            }
        }
        let captured = if missing_products.is_empty()
            && missing_interfaces.is_empty()
            && missing_joins.is_empty()
            && missing_values.is_empty()
        {
            BTreeMap::new()
        } else {
            recovery_artifacts::with_artifact_work(&mut self.work, |validation| {
                RecoveredArtifactInventory::capture_inputs_with_validation(
                    &self.root,
                    &missing_products,
                    &missing_interfaces,
                    &missing_joins,
                    &missing_values,
                    None,
                    validation,
                    &held_interfaces,
                )
            })?
            .entries
        };
        if captured.keys().any(|id| !references.contains_key(id)) {
            return Err(failure("unexpected acquired recovery payload").into());
        }
        let mut entries = BTreeMap::new();
        let mut producer = [0; 32];
        for id in references.keys() {
            let entry = self
                .payloads
                .get(id)
                .map(|held| held.entry.clone())
                .or_else(|| captured.get(id).cloned())
                .ok_or_else(|| failure("recovered payload acquisition omitted requested bytes"))?;
            if entry.descriptor.producer_sha256 == [0; 32]
                || (producer != [0; 32] && producer != entry.descriptor.producer_sha256)
            {
                return Err(failure("producer identity differs").into());
            }
            producer = entry.descriptor.producer_sha256;
            entries.insert(*id, entry);
        }
        let bound = RecoveredArtifactInventory::bind_captured(
            RecoveredArtifactInventory {
                producer,
                entries,
                recorded_inventory: false,
                bindings: BTreeSet::new(),
                binding_edges: Vec::new(),
                interfaces: Vec::new(),
            },
            descriptors,
            bindings,
            edges,
        )?;
        for (id, entry) in captured {
            self.payloads
                .entry(id)
                .or_insert_with(|| AcquiredRecoveryPayload {
                    reference: references[&id].clone(),
                    entry,
                });
        }
        Ok(bound)
    }

    /// Publish staged additions only after the caller rechecks its current run
    /// and store owners. A concurrent identical admission keeps its held owner.
    pub fn append(&mut self, stage: Self) -> Result<(), RecoveryInventoryError> {
        if self.root != stage.root
            || stage.payloads.iter().any(|(id, incoming)| {
                self.payloads
                    .get(id)
                    .is_some_and(|held| held.reference != incoming.reference)
            })
        {
            return Err(
                failure("staged recovery payload conflicts with its acquisition owner").into(),
            );
        }
        for (id, payload) in stage.payloads {
            self.payloads.entry(id).or_insert(payload);
        }
        self.work.hash_bytes += stage.work.hash_bytes;
        self.work.read_bytes += stage.work.read_bytes;
        self.work.written_bytes += stage.work.written_bytes;
        self.work.decoded_bytes += stage.work.decoded_bytes;
        Ok(())
    }

    /// Work observed at this owner's actual payload validation boundary.
    /// A discarded stage's attempted work stays with that stage, not its parent.
    pub fn work(&self) -> recovery_artifacts::RecoveryArtifactWork {
        self.work
    }
}

/// One recovery admission owns authenticated original bytes. Scoped contexts
/// share these immutable entries without reopening or decoding artifacts.
pub struct RecoveredArtifactInventory {
    producer: [u8; 32],
    entries: BTreeMap<crate::artifact_inventory::ArtifactId, Arc<ArtifactEntry>>,
    recorded_inventory: bool,
    bindings: BTreeSet<ArtifactBinding>,
    binding_edges: Vec<(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)>,
    interfaces: Vec<(
        crate::artifact_inventory::ArtifactId,
        crate::artifact_inventory::ArtifactId,
        crate::artifact_inventory::ArtifactDependency,
    )>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryInventoryError {
    #[error("recovery artifact verification failed")]
    Artifacts(
        Vec<(
            crate::artifact_inventory::ArtifactId,
            recovery_artifacts::RecoveryArtifactError,
        )>,
    ),
    #[error(transparent)]
    Certification(#[from] CompileError),
}

pub(crate) enum RecoveredCompilerIssuance {
    CheckedInput,
    RecordedRoles,
}

pub(crate) struct RecoveredCompilerSelection {
    artifacts: ArtifactView,
    projection: CompilerInputProjection,
    issuance: RecoveredCompilerIssuance,
}

impl RecoveredCompilerSelection {
    pub(crate) fn into_parts(
        self,
    ) -> (
        ArtifactView,
        CompilerInputProjection,
        RecoveredCompilerIssuance,
    ) {
        (self.artifacts, self.projection, self.issuance)
    }
}

impl RecoveredArtifactInventory {
    pub fn capture(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        descriptors: &[crate::artifact_inventory::ArtifactDescriptor],
        interfaces: &[(
            crate::artifact_inventory::ArtifactId,
            crate::artifact_inventory::ArtifactId,
            crate::artifact_inventory::ArtifactDependency,
        )],
    ) -> Result<Self, RecoveryInventoryError> {
        Self::capture_inputs(
            root,
            products,
            module_interfaces,
            joins,
            values,
            Some((descriptors, interfaces)),
        )
    }

    /// Authenticate payload custody once and retain its exact selection-bound
    /// graph. Payload identity never reconstructs a dependency selection.
    pub fn capture_bound(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        descriptors: &[crate::artifact_inventory::ArtifactDescriptor],
        bindings: &[ArtifactBinding],
        edges: &[(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)],
    ) -> Result<Self, RecoveryInventoryError> {
        Self::bind_captured(
            Self::capture_inputs(root, products, module_interfaces, joins, values, None)?,
            descriptors,
            bindings,
            edges,
        )
    }

    fn bind_captured(
        mut captured: Self,
        descriptors: &[crate::artifact_inventory::ArtifactDescriptor],
        bindings: &[ArtifactBinding],
        edges: &[(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)],
    ) -> Result<Self, RecoveryInventoryError> {
        crate::certified_products::validate_recovery_original_owners(
            captured
                .entries
                .values()
                .filter_map(|entry| match &entry.payload {
                    ArtifactPayload::Original(product) => Some(product.owner()),
                    _ => None,
                }),
        )
        .map_err(failure)?;
        let actual = captured
            .entries
            .iter()
            .map(|(id, entry)| (*id, entry.descriptor.clone()))
            .collect::<BTreeMap<_, _>>();
        let supplied = descriptors
            .iter()
            .map(|entry| (entry.id, entry.clone()))
            .collect::<BTreeMap<_, _>>();
        if supplied.len() != descriptors.len() || supplied != actual {
            return Err(failure("bound recovery descriptors differ from certified bytes").into());
        }
        let available = bindings.iter().copied().collect::<BTreeSet<_>>();
        let has_endpoint = |node: ArtifactBindingNode| match node {
            ArtifactBindingNode::Artifact(binding) => available.contains(&binding),
            ArtifactBindingNode::Group(group) => captured
                .entries
                .get(&group.binding.artifact())
                .is_some_and(|entry| match &entry.payload {
                    ArtifactPayload::Original(product) => {
                        crate::certified_products::original_native_group_definitions(
                            product,
                            group.original_ordinal,
                        )
                        .is_some()
                    }
                    _ => false,
                }),
        };
        if available.len() != bindings.len()
            || available
                .iter()
                .any(|binding| !actual.contains_key(&binding.artifact))
            || available
                .iter()
                .map(|binding| binding.artifact)
                .collect::<BTreeSet<_>>()
                != actual.keys().copied().collect()
            || edges.iter().cloned().collect::<BTreeSet<_>>().len() != edges.len()
            || edges
                .iter()
                .any(|(from, to, _)| !has_endpoint(*from) || !has_endpoint(*to))
        {
            return Err(failure("bound recovery has duplicate or missing exact endpoints").into());
        }
        // Lexical joins carry no original certificate. Their recorded interface
        // owner requirements must agree across every binding of the same bytes.
        for (id, entry) in &mut captured.entries {
            if !matches!(
                entry.descriptor.kind,
                crate::artifact_inventory::ArtifactKind::LexicalJoin
            ) {
                continue;
            }
            let mut expected = None;
            for binding in available.iter().filter(|binding| binding.artifact == *id) {
                let requirements = edges
                    .iter()
                    .filter_map(|(from, to, dependency)| {
                        if *from != ArtifactBindingNode::Artifact(*binding)
                            || *dependency != ArtifactDependency::Interface
                        {
                            return None;
                        }
                        let ArtifactBindingNode::Artifact(target) = to else {
                            return None;
                        };
                        let owner = &actual[&target.artifact].owner;
                        (owner != &entry.descriptor.owner).then(|| owner.clone())
                    })
                    .collect::<BTreeSet<_>>();
                if expected
                    .as_ref()
                    .is_some_and(|previous| previous != &requirements)
                {
                    return Err(failure(
                        "lexical recovery bindings disagree on owner requirements",
                    )
                    .into());
                }
                expected = Some(requirements);
            }
            *entry = Arc::new((**entry).clone().finalize_joined_requirements(
                expected.unwrap_or_default().into_iter().collect(),
            )?);
        }
        captured.bindings = available;
        captured.binding_edges = edges.to_vec();
        Ok(captured)
    }

    /// Restore a versioned publication's exact bound graph after authenticating
    /// all immutable custody. A fresh inventory makes refusal failure-atomic.
    pub(crate) fn authenticate_graph_selection(
        &self,
        selection: &ArtifactGraphSelection,
        roles: &[CompilerInputRole],
    ) -> Result<RecoveredCompilerSelection, CompileError> {
        let namespace = selection
            .namespace
            .iter()
            .map(|binding| binding.artifact)
            .collect::<BTreeSet<_>>();
        let issued_roles = roles
            .iter()
            .flat_map(|role| std::iter::once(role.interface()).chain(role.original()))
            .collect::<BTreeSet<_>>();
        if namespace.len() != selection.namespace.len() || namespace != issued_roles {
            return Err(failure(
                "bound recovery namespace differs from recorded compiler roles",
            ));
        }
        let selected = selection.bindings.iter().copied().collect::<BTreeSet<_>>();
        if selected.len() != selection.bindings.len() || !selected.is_subset(&self.bindings) {
            return Err(failure("missing or duplicate recovered selection binding"));
        }
        let ids = selected
            .iter()
            .map(|binding| binding.artifact)
            .collect::<BTreeSet<_>>();
        let entries = ids
            .iter()
            .map(|id| {
                self.entries
                    .get(id)
                    .cloned()
                    .ok_or_else(|| failure("missing bound recovery payload"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let groups = selection
            .native_groups
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let edges = self
            .binding_edges
            .iter()
            .filter(|(from, _, _)| from.is_retained_in(&selected, &groups))
            .cloned()
            .collect::<Vec<_>>();
        if edges
            .iter()
            .any(|(_, to, _)| !to.is_retained_in(&selected, &groups))
        {
            return Err(failure(
                "bound recovery selection omits exact dependency scope",
            ));
        }
        let inventory = ArtifactInventory::default();
        let restored =
            inventory.recover_selection(&inventory.empty_view(), entries, selection, &edges)?;
        let compiler_projection = CompilerInputProjection::restore(&restored, roles)?;
        compiler_projection.validate_associations(&restored)?;
        Ok(RecoveredCompilerSelection {
            artifacts: restored,
            projection: compiler_projection,
            issuance: RecoveredCompilerIssuance::RecordedRoles,
        })
    }

    /// Authenticate a checked input's exact graph before its first availability
    /// issuance. Recorded-role recovery uses the separate preservation path.
    #[cfg(test)]
    fn authenticate_checked_graph_selection(
        &self,
        selection: &ArtifactGraphSelection,
    ) -> Result<RecoveredCompilerSelection, CompileError> {
        let selected = selection
            .namespace
            .iter()
            .map(|binding| {
                self.entries
                    .get(&binding.artifact)
                    .cloned()
                    .ok_or_else(|| failure("checked graph namespace lacks its issued payload"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let projection = CompilerInputProjection::from_issued_entries(&selected)?;
        let mut authenticated =
            self.authenticate_graph_selection(selection, &projection.roles())?;
        authenticated.issuance = RecoveredCompilerIssuance::CheckedInput;
        Ok(authenticated)
    }

    pub fn context_with_graph_selection(
        &self,
        selection: &ArtifactGraphSelection,
        roles: &[CompilerInputRole],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<ExactDeclarationContext, CompileError> {
        let compiler_support =
            crate::artifact_inventory::CompilerSourceSupport::from_recovered_selection(
                self.authenticate_graph_selection(selection, roles)?,
            )?;
        let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
            compiler_support,

            producer: self.producer,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,

            lexical: crate::exact_context::ExactLexicalSnapshot::default().replace(lexical)?,
        });
        context.finish()
    }

    pub fn context_with_snapshot(
        &self,
        snapshot: &crate::exact_context::ExactContextSnapshot,
    ) -> Result<ExactDeclarationContext, CompileError> {
        let graph = snapshot.graph_selection().map_err(failure)?;
        let mut context = self.context_with_graph_selection(
            &graph,
            &snapshot.compiler_roles(),
            snapshot.lexical(),
        )?;
        let updated = context
            .owner
            .facts
            .compiler_support
            .restore_snapshot_evidence(snapshot)?;
        context = context.rebuild(|facts| facts.compiler_support = updated);
        Ok(context)
    }

    fn capture_inputs(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        inventory: Option<(
            &[crate::artifact_inventory::ArtifactDescriptor],
            &[(
                crate::artifact_inventory::ArtifactId,
                crate::artifact_inventory::ArtifactId,
                crate::artifact_inventory::ArtifactDependency,
            )],
        )>,
    ) -> Result<Self, RecoveryInventoryError> {
        Self::capture_inputs_with_validation(
            root,
            products,
            module_interfaces,
            joins,
            values,
            inventory,
            &mut PackageInterfaceValidation::default(),
            &BTreeMap::new(),
        )
    }

    fn capture_inputs_with_validation(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        inventory: Option<(
            &[crate::artifact_inventory::ArtifactDescriptor],
            &[(
                crate::artifact_inventory::ArtifactId,
                crate::artifact_inventory::ArtifactId,
                crate::artifact_inventory::ArtifactDependency,
            )],
        )>,
        validation: &mut PackageInterfaceValidation,
        held_interfaces: &BTreeMap<
            recovery_artifacts::RecoveryModuleInterfaceRef,
            crate::certified_products::CertifiedModuleInterface,
        >,
    ) -> Result<Self, RecoveryInventoryError> {
        let mut context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
            compiler_support: crate::artifact_inventory::CompilerSourceSupport::empty(),

            producer: [0; 32],
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,

            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
        });
        let mut entries = Vec::new();
        let mut losses = Vec::new();
        let mut verified_modules = Vec::new();
        let mut selected_modules = BTreeSet::new();
        // Native references carry canonical evidence themselves. Explicit rows
        // add standalone type owners; both inputs cross the same admission.
        for reference in module_interfaces.iter().chain(
            products
                .iter()
                .filter_map(|product| product.module_interface.as_ref()),
        ) {
            if !selected_modules.insert(reference) {
                continue;
            }
            if let Some(interface) = held_interfaces.get(reference) {
                verified_modules.push((reference, interface.clone()));
                continue;
            }
            match recovery_artifacts::recover_module_interface(root, reference, validation) {
                Ok(interface) => verified_modules.push((reference, interface)),
                Err(error) => losses.push((
                    crate::artifact_inventory::ArtifactDescriptor::from_recovery_module_interface(
                        reference,
                    )
                    .id,
                    error,
                )),
            }
        }
        let mut verified = Vec::new();
        for reference in products {
            let captured = reference.module_interface.as_ref().and_then(|required| {
                verified_modules
                    .iter()
                    .find(|(supplied, _)| *supplied == required)
            });
            let result = match captured {
                Some((_, interface)) => {
                    recovery_artifacts::verify_materialized_ref_with_module_interface(
                        root, reference, interface, validation,
                    )
                }
                None => Err(recovery_artifacts::RecoveryArtifactError::InvalidReference),
            };
            match result {
                Ok(artifact) => verified.push(artifact),
                Err(error) => losses.push((
                    crate::artifact_inventory::ArtifactDescriptor::from_recovery_product(reference)
                        .id,
                    error,
                )),
            }
        }
        let mut verified_joins = Vec::new();
        for reference in joins {
            match recovery_artifacts::verify_materialized_join_with_validation(
                root, reference, validation,
            ) {
                Ok(artifact) => verified_joins.push(artifact),
                Err(error) => losses.push((
                    crate::artifact_inventory::ArtifactDescriptor::from_recovery_join(reference).id,
                    error,
                )),
            }
        }
        let mut verified_values = Vec::new();
        for reference in values {
            match recovery_artifacts::verify_materialized_join_with_validation(
                root,
                &reference.interface,
                validation,
            ) {
                Ok(artifact) => verified_values.push(artifact),
                Err(error) => losses.push((reference.artifact_id, error)),
            }
        }
        if !losses.is_empty() {
            return Err(RecoveryInventoryError::Artifacts(losses));
        }
        let certified = crate::certified_products::certify_recovery_products_with_validation(
            verified, validation,
        )
        .map_err(failure)?;
        for original in certified {
            context = context.admit_producer(original.producer_sha256)?;
            let product = original.product;
            let requirements = original.requirements;
            if product.owner() != &requirements.owner {
                return Err(failure("recovered original owner differs").into());
            }
            let entry = ArtifactEntry::original_with_validation(
                context.owner.facts.producer,
                product,
                validation,
            )?;
            entries.push(entry);
        }
        for (reference, artifact) in joins.iter().zip(verified_joins) {
            context = context.admit_producer(reference.toolchain_identity_sha256)?;
            let join = CertifiedJoinedInterface::from_certification(
                reference.toolchain_identity_sha256,
                reference.unit.clone(),
                reference.module.clone(),
                artifact.interface_bytes,
                artifact.package_imports_bytes,
            )
            .map_err(failure)?;
            entries.push(ArtifactEntry::interface(
                join,
                JoinedInterfaceRole::LexicalJoin,
                Vec::new(),
            ));
        }
        for (reference, artifact) in values.iter().zip(verified_values) {
            context = context.admit_producer(reference.interface.toolchain_identity_sha256)?;
            let interface = CertifiedJoinedInterface::from_certification(
                reference.interface.toolchain_identity_sha256,
                reference.interface.unit.clone(),
                reference.interface.module.clone(),
                artifact.interface_bytes,
                artifact.package_imports_bytes,
            )
            .map_err(failure)?;
            let entry = ArtifactEntry::interface(
                interface,
                JoinedInterfaceRole::ValueInterface,
                reference.requirements.clone(),
            );
            if entry.descriptor.id != reference.artifact_id {
                return Err(failure("value interface artifact identity differs").into());
            }
            entries.push(entry);
        }
        for (_, interface) in verified_modules {
            context = context.admit_producer(interface.producer_sha256())?;
            entries.push(ArtifactEntry::canonical(interface));
        }
        // Native references carry their same canonical interface; standalone type
        // owners arrive through the explicit manifest rows above.
        let canonical = entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => product.module_interface().cloned(),
                _ => None,
            })
            .map(ArtifactEntry::canonical)
            .collect::<Vec<_>>();
        entries.extend(canonical);
        let mut unique = BTreeMap::new();
        for entry in entries {
            if let Some(previous) = unique.insert(entry.descriptor.id, entry.clone()) {
                if previous != entry {
                    return Err(failure("conflicting recovered canonical carrier").into());
                }
            }
        }
        let mut entries = unique.into_values().collect::<Vec<_>>();
        let mut interfaces = Vec::new();
        if let Some((descriptors, dependencies)) = inventory {
            crate::artifact_inventory::restore_recovery_interface_dependencies(
                &mut entries,
                descriptors,
                dependencies,
            )?;
            interfaces = dependencies.to_vec();
        }
        for entry in &mut entries {
            entry.requirements.sort();
            entry.requirements.dedup();
        }
        let entries = entries
            .into_iter()
            .map(|entry| (entry.descriptor.id, Arc::new(entry)))
            .collect::<BTreeMap<_, _>>();
        Ok(Self {
            producer: context.owner.facts.producer,
            entries,
            recorded_inventory: inventory.is_some(),
            bindings: BTreeSet::new(),
            binding_edges: Vec::new(),
            interfaces,
        })
    }

    /// Restore persisted selection only after full products were authenticated.
    /// The raw keys grant no checked-entry or live binding authority.
    pub fn context(
        &self,
        ids: &[crate::artifact_inventory::ArtifactId],
        groups: &[crate::artifact_inventory::NativeGroupKey],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<ExactDeclarationContext, CompileError> {
        let selected = groups.iter().copied().collect::<BTreeSet<_>>();
        if selected.len() != groups.len() {
            return Err(failure("duplicate recovered native group selection"));
        }
        self.context_with_selection(ids, Some(&selected), None, lexical)
    }

    /// Restore explicit compiler roles after all referenced immutable inventory
    /// proofs have been authenticated. Serialized roles alone grant no reuse.
    pub fn context_with_roles(
        &self,
        ids: &[ArtifactId],
        groups: &[NativeGroupKey],
        roles: &[CompilerInputRole],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<ExactDeclarationContext, CompileError> {
        if roles
            .iter()
            .any(CompilerInputRole::is_published_source_original)
        {
            return Err(failure(
                "published source roles require the durable publication owner",
            ));
        }
        let selected = groups.iter().copied().collect::<BTreeSet<_>>();
        if selected.len() != groups.len() {
            return Err(failure("duplicate recovered native group selection"));
        }
        self.context_with_selection(ids, Some(&selected), Some(roles), lexical)
    }

    /// Standalone callers explicitly request every authenticated original group.
    /// Exact durable restoration uses `context_with_graph_selection`.
    pub fn context_all_groups(
        &self,
        ids: &[crate::artifact_inventory::ArtifactId],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<ExactDeclarationContext, CompileError> {
        self.context_with_selection(ids, None, None, lexical)
    }

    pub(crate) fn authenticate_unbound_selection(
        &self,
        ids: &[ArtifactId],
        groups: Option<&BTreeSet<crate::artifact_inventory::NativeGroupKey>>,
        roles: Option<&[CompilerInputRole]>,
    ) -> Result<RecoveredCompilerSelection, CompileError> {
        let selected = ids.iter().copied().collect::<BTreeSet<_>>();
        if selected.len() != ids.len() {
            return Err(failure("duplicate recovered artifact selection"));
        }
        let entries = ids
            .iter()
            .map(|id| {
                self.entries
                    .get(id)
                    .cloned()
                    .ok_or_else(|| failure("missing recovered artifact selection"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (from, to, _) in &self.interfaces {
            if selected.contains(from) && !selected.contains(to) {
                return Err(failure("recovered interface closure is incomplete"));
            }
        }
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let compiler_projection = if roles.is_none() {
            Some(CompilerInputProjection::from_issued_entries(&entries)?)
        } else {
            None
        };
        let restored = match groups {
            Some(groups) => inventory.admit_recovery_selection(&empty, entries, groups)?,
            None => inventory.admit_shared(&empty, entries)?,
        };
        if restored
            .descriptors()
            .iter()
            .map(|row| row.id)
            .collect::<BTreeSet<_>>()
            != selected
        {
            return Err(failure(
                "recovered artifact closure differs from recorded selection",
            ));
        }
        if self.recorded_inventory {
            let expected = self
                .interfaces
                .iter()
                .filter(|(from, _, _)| selected.contains(from))
                .cloned()
                .collect::<BTreeSet<_>>();
            let actual = restored
                .interface_dependencies()
                .into_iter()
                .collect::<BTreeSet<_>>();
            if actual != expected {
                return Err(failure(
                    "recovered interface facts differ from recorded selection",
                ));
            }
        }
        let compiler_projection = match roles {
            Some(roles) => CompilerInputProjection::restore(&restored, roles)?,
            None => compiler_projection.expect("direct input issuer"),
        };
        compiler_projection.validate_associations(&restored)?;
        Ok(RecoveredCompilerSelection {
            artifacts: restored,
            projection: compiler_projection,
            issuance: if roles.is_none() {
                RecoveredCompilerIssuance::CheckedInput
            } else {
                RecoveredCompilerIssuance::RecordedRoles
            },
        })
    }

    fn context_with_selection(
        &self,
        ids: &[crate::artifact_inventory::ArtifactId],
        groups: Option<&BTreeSet<crate::artifact_inventory::NativeGroupKey>>,
        roles: Option<&[CompilerInputRole]>,
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<ExactDeclarationContext, CompileError> {
        let compiler_support =
            crate::artifact_inventory::CompilerSourceSupport::from_recovered_selection(
                self.authenticate_unbound_selection(ids, groups, roles)?,
            )?;
        let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
            compiler_support,

            producer: self.producer,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,

            lexical: crate::exact_context::ExactLexicalSnapshot::default().replace(lexical)?,
        });
        context.finish()
    }
}

/// The paths live under the caller's owned artifact directory. Their content
/// and requirements are always derived from the protected context.
pub struct MaterializedExactDeclarationContext {
    pub artifacts: Vec<DeclarationArtifact>,
    pub lexical: Vec<ExactLexicalNode>,
}

/// An exact context's source-free interface inspection. The owner retains the
/// selected namespace and private files; no caller can substitute rows or turn
/// omission of native payloads into an availability claim.
struct CertifiedInterfaceInputs {
    context: Arc<ExactDeclarationContext>,
    metadata: CompilerArtifactMetadata,
    directory: Arc<tempfile::TempDir>,
    artifacts: Vec<DeclarationArtifact>,
}

pub(crate) struct RecoveryInspectionInputs(CertifiedInterfaceInputs);

/// Owner-issued interface observation; native bindings remain in its context.
pub struct DeclarationJoinInputs(CertifiedInterfaceInputs);
impl DeclarationJoinInputs {
    pub fn interface(&self, owner: &ExactModuleIdentity) -> Option<&ExactIfaceArtifact> {
        self.0.interface(owner)
    }
    pub(crate) fn validate(&self) -> Result<(), CompileError> {
        self.0.validate()
    }
    pub(crate) fn context(&self) -> &ExactDeclarationContext {
        &self.0.context
    }
    pub(crate) fn artifacts(&self) -> &[DeclarationArtifact] {
        &self.0.artifacts
    }
    pub(crate) fn directory(&self) -> Arc<tempfile::TempDir> {
        self.0.directory.clone()
    }
}

impl RecoveryInspectionInputs {
    pub(crate) fn interface(&self, owner: &ExactModuleIdentity) -> Option<&ExactIfaceArtifact> {
        self.0.interface(owner)
    }
    pub(crate) fn inspect(
        &self,
        includes: &[PathBuf],
        settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
    ) -> Result<crate::declaration_join::DeclarationInventoryOutcome, CompileError> {
        self.0.inspect(includes, settlement)
    }
}

impl CertifiedInterfaceInputs {
    pub(crate) fn interface(&self, owner: &ExactModuleIdentity) -> Option<&ExactIfaceArtifact> {
        self.artifacts
            .iter()
            .find(|artifact| {
                artifact.interface.unit == owner.unit && artifact.interface.module == owner.module
            })
            .map(|artifact| &artifact.interface)
    }

    fn validate(&self) -> Result<(), CompileError> {
        if self.artifacts.len() != self.metadata.entries.len() {
            return Err(failure(
                "recovery inspection interface closure is incomplete",
            ));
        }
        for artifact in &self.artifacts {
            let exact = &artifact.interface;
            let owner = identity(&exact.unit, &exact.module);
            let entry = self
                .metadata
                .entries
                .get(&owner)
                .ok_or_else(|| failure("recovery inspection interface is outside its owner"))?;
            let (bytes, packages) = match &entry.payload {
                ArtifactPayload::Original(product) => {
                    (product.interface_bytes(), product.package_imports_bytes())
                }
                ArtifactPayload::Canonical(interface) => (
                    interface.interface_bytes(),
                    interface.package_imports_bytes(),
                ),
                ArtifactPayload::Interface(interface, _) => (
                    interface.interface_bytes(),
                    interface.package_imports_bytes(),
                ),
            };
            if artifact.product.is_some()
                || exact.sha256 != hex(&entry.descriptor.interface_sha256)
                || exact.requirements.iter().cloned().collect::<BTreeSet<_>>()
                    != entry
                        .requirements
                        .iter()
                        .map(|owner| (owner.unit.clone(), owner.module.clone()))
                        .collect()
                || std::fs::read(&exact.path)? != bytes
            {
                return Err(failure(
                    "recovery inspection interface differs from its issuing context",
                ));
            }
            let mut package_path = exact.path.as_os_str().to_os_string();
            package_path.push(".packages");
            if std::fs::read(PathBuf::from(package_path))? != packages {
                return Err(failure(
                    "recovery inspection package witness differs from owned bytes",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn inspect(
        &self,
        includes: &[PathBuf],
        settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
    ) -> Result<crate::declaration_join::DeclarationInventoryOutcome, CompileError> {
        self.validate()?;
        let result = crate::declaration_join::inspect_declaration_artifacts_with_producer(
            &self.artifacts,
            includes,
            self.directory.path(),
            Some(self.context.toolchain_identity_sha256()),
            self.directory.clone(),
            settlement,
        )?;
        self.validate()?;
        Ok(result)
    }
}

/// Private files and newly certified entries added by one immutable graph view.
/// Parents retain inherited rows and group payloads; requests assemble borrowed
/// selections without storing a copy of every ancestor in each descendant.
/// A detached selection shares each row and graph file's issuing directory,
/// independently of historical materialization metadata.
pub(crate) struct RetainedArtifactMaterialization {
    _directory: Option<Arc<tempfile::TempDir>>,
    _parents: Vec<Arc<Self>>,
    rows: BTreeMap<ArtifactId, Arc<RetainedArtifactRow>>,
    groups: Arc<[PendingCertifiedGroup]>,
    execution_scope: Option<Value>,
    graph_paths: BTreeMap<[u8; 32], Arc<OwnedExecutionGraphFile>>,
    #[cfg(test)]
    payload_work: recovery_artifacts::RecoveryArtifactWork,
}

/// Issued only when the owning materialization writes an admitted immutable
/// graph. Its path is transported by the sealed exact scope, while the request
/// retains the issuing directory through worker use, including detached captures.
struct OwnedExecutionGraphFile {
    path: PathBuf,
    transport: crate::owned_input_arena::OwnedInputSlice,
    _directory: Arc<tempfile::TempDir>,
}

struct MaterializationFacts<'a> {
    rows: BTreeMap<ArtifactId, &'a Arc<RetainedArtifactRow>>,
    groups: Vec<&'a PendingCertifiedGroup>,
    graph_paths: BTreeMap<[u8; 32], &'a Arc<OwnedExecutionGraphFile>>,
    observed: Vec<&'a RetainedArtifactMaterialization>,
}

#[derive(Clone, Default)]
struct MaterializationCoverage {
    facts: imbl::OrdSet<usize>,
    excluded: bool,
}

impl MaterializationFacts<'_> {
    fn detach(self) -> Arc<RetainedArtifactMaterialization> {
        Arc::new(RetainedArtifactMaterialization {
            _directory: None,
            _parents: Vec::new(),
            rows: self
                .rows
                .into_iter()
                .map(|(id, row)| (id, Arc::clone(row)))
                .collect(),
            groups: self.groups.into_iter().cloned().collect::<Vec<_>>().into(),
            execution_scope: None,
            graph_paths: self
                .graph_paths
                .into_iter()
                .map(|(digest, file)| (digest, Arc::clone(file)))
                .collect(),
            #[cfg(test)]
            payload_work: recovery_artifacts::RecoveryArtifactWork::default(),
        })
    }
}

impl RetainedArtifactMaterialization {
    /// Preserve complete physical owners, discarding only exact issued subsets.
    /// A narrowed residual owns fact handles, never a historical parent graph.
    pub(crate) fn select_custody_inputs(
        owners: &[Arc<Self>],
        metadata: &crate::artifact_inventory::RetainedArtifactMetadata,
    ) -> Vec<Arc<Self>> {
        let Some(mut selected) = Self::select_custody_facts(
            owners,
            &metadata.artifacts,
            &metadata.selected_native_groups,
        ) else {
            return Vec::new();
        };
        let available = metadata
            .artifacts
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some((product.owner(), entry.descriptor.id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        // Slots name selected facts only for this operation. They are neither
        // durable identities nor a registry of physical issuances.
        let mut next = 0;
        let mut slot = || {
            let current = next;
            next += 1;
            current
        };
        let row_slots = selected
            .rows
            .keys()
            .map(|id| (*id, slot()))
            .collect::<BTreeMap<_, _>>();
        let group_slots = selected
            .groups
            .iter()
            .map(|group| {
                let key =
                    Self::selected_group_key(group, &available, &metadata.selected_native_groups)
                        .expect("selected group has its exact metadata key");
                (key, (slot(), *group))
            })
            .collect::<BTreeMap<_, _>>();
        let graph_slots = selected
            .graph_paths
            .keys()
            .map(|id| (*id, slot()))
            .collect::<BTreeMap<_, _>>();
        let indices = selected
            .observed
            .iter()
            .enumerate()
            .map(|(index, owner)| (*owner as *const Self as usize, index))
            .collect::<BTreeMap<_, _>>();
        let mut coverage = Vec::<MaterializationCoverage>::new();
        // Postorder computes each node's entire retained footprint once. A
        // shadowed or unselected ancestor fact excludes whole-owner reuse even
        // when a later fact has the same logical key.
        for owner in &selected.observed {
            let mut inherited = owner
                ._parents
                .iter()
                .map(|parent| &coverage[indices[&(Arc::as_ptr(parent) as usize)]]);
            let MaterializationCoverage {
                mut facts,
                mut excluded,
            } = inherited.next().cloned().unwrap_or_default();
            for parent in inherited {
                facts.extend(parent.facts.iter().copied());
                excluded |= parent.excluded;
            }
            for (id, row) in &owner.rows {
                if selected
                    .rows
                    .get(id)
                    .is_some_and(|selected| Arc::ptr_eq(row, selected))
                {
                    facts.insert(row_slots[id]);
                } else {
                    excluded = true;
                }
            }
            for group in owner.groups.iter() {
                let selected =
                    Self::selected_group_key(group, &available, &metadata.selected_native_groups)
                        .and_then(|key| group_slots.get(&key));
                if let Some((slot, _)) =
                    selected.filter(|(_, selected)| group.same_issuance(selected))
                {
                    facts.insert(*slot);
                } else {
                    excluded = true;
                }
            }
            for (id, file) in &owner.graph_paths {
                if selected
                    .graph_paths
                    .get(id)
                    .is_some_and(|selected| Arc::ptr_eq(file, selected))
                {
                    facts.insert(graph_slots[id]);
                } else {
                    excluded = true;
                }
            }
            coverage.push(MaterializationCoverage { facts, excluded });
        }
        let mut seen = BTreeSet::new();
        let mut candidates = owners
            .iter()
            .enumerate()
            .filter_map(|(position, owner)| {
                let identity = Arc::as_ptr(owner) as usize;
                let index = indices[&identity];
                let covered = &coverage[index];
                (seen.insert(identity) && !covered.excluded && !covered.facts.is_empty())
                    .then_some((position, index, owner))
            })
            .collect::<Vec<_>>();
        // Larger exact footprints subsume subsets. For identical facts prefer
        // an actual writer, then the later postorder node (an actual descendant
        // follows its ancestors). No distinct issuer can pass fact identity.
        candidates.sort_by(|left, right| {
            coverage[right.1]
                .facts
                .len()
                .cmp(&coverage[left.1].facts.len())
                .then_with(|| {
                    right
                        .2
                        ._directory
                        .is_some()
                        .cmp(&left.2._directory.is_some())
                })
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.0.cmp(&right.0))
        });
        let mut frontier = Vec::<(usize, usize, &Arc<Self>)>::new();
        let mut covered = imbl::OrdSet::<usize>::new();
        for candidate in candidates {
            if frontier.iter().any(|retained| {
                coverage[candidate.1]
                    .facts
                    .is_subset(&coverage[retained.1].facts)
            }) {
                continue;
            }
            covered.extend(coverage[candidate.1].facts.iter().copied());
            frontier.push(candidate);
        }
        // Restore source order for incomparable owners. Logical winner
        // precedence was fixed before any physical subset reduction.
        frontier.sort_by_key(|candidate| candidate.0);
        let mut inputs = frontier
            .into_iter()
            .map(|(_, _, owner)| Arc::clone(owner))
            .collect::<Vec<_>>();
        selected
            .rows
            .retain(|id, _| !covered.contains(&row_slots[id]));
        selected.groups.retain(|group| {
            let key = Self::selected_group_key(group, &available, &metadata.selected_native_groups)
                .expect("selected group has its exact metadata key");
            !covered.contains(&group_slots[&key].0)
        });
        selected
            .graph_paths
            .retain(|id, _| !covered.contains(&graph_slots[id]));
        if !selected.rows.is_empty()
            || !selected.groups.is_empty()
            || !selected.graph_paths.is_empty()
        {
            inputs.push(selected.detach());
        }
        inputs
    }

    /// Opaque availability retains physical files, not compilation cache history.
    /// Checked available groups remain distinct from executable request roots.
    pub(crate) fn capture_custody(
        owners: &[Arc<Self>],
        artifacts: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
        available_groups: &BTreeSet<NativeGroupKey>,
    ) -> Option<Arc<Self>> {
        Some(Self::select_custody_facts(owners, artifacts, available_groups)?.detach())
    }

    fn select_custody_facts<'a>(
        owners: &'a [Arc<Self>],
        artifacts: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
        selected_native_groups: &BTreeSet<NativeGroupKey>,
    ) -> Option<MaterializationFacts<'a>> {
        let selected_graphs = artifacts
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => {
                    product.execution_source().map(|graph| graph.digest())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let mut rows = BTreeMap::new();
        let mut graph_paths = BTreeMap::new();
        let mut observed = Vec::new();
        let mut visited = BTreeSet::new();
        for owner in owners {
            owner.visit_owners(&mut visited, &mut |owner| observed.push(owner));
        }
        let available = artifacts
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some((product.owner(), entry.descriptor.id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut seen_groups = BTreeSet::new();
        let groups: Vec<&PendingCertifiedGroup> = observed
            .iter()
            .flat_map(|owner| owner.groups.iter())
            .filter(|group| {
                Self::selected_group_key(group, &available, selected_native_groups)
                    .is_some_and(|key| seen_groups.insert(key))
            })
            .collect();
        for &owner in &observed {
            rows.extend(
                owner
                    .rows
                    .iter()
                    .filter(|(id, _)| artifacts.contains_key(*id))
                    .map(|(id, row)| (*id, row)),
            );
            graph_paths.extend(
                owner
                    .graph_paths
                    .iter()
                    .filter(|(digest, _)| selected_graphs.contains(*digest))
                    .map(|(digest, file)| (*digest, file)),
            );
        }
        if rows.is_empty() && groups.is_empty() && graph_paths.is_empty() {
            return None;
        }
        Some(MaterializationFacts {
            rows,
            groups,
            graph_paths,
            observed,
        })
    }

    #[cfg(test)]
    fn directory(&self) -> &tempfile::TempDir {
        self._directory
            .as_ref()
            .expect("writing materialization")
            .as_ref()
    }
    fn visit_owners<'a>(&'a self, seen: &mut BTreeSet<usize>, visit: &mut impl FnMut(&'a Self)) {
        let mut pending = vec![(self, false)];
        while let Some((owner, parents_visited)) = pending.pop() {
            if parents_visited {
                visit(owner);
            } else if seen.insert(owner as *const Self as usize) {
                pending.push((owner, true));
                pending.extend(
                    owner
                        ._parents
                        .iter()
                        .rev()
                        .map(|parent| (parent.as_ref(), false)),
                );
            }
        }
    }

    fn selected_rows<'a>(
        &'a self,
        metadata: &CompilerArtifactMetadata,
    ) -> BTreeMap<ArtifactId, &'a RetainedArtifactRow> {
        let selected = metadata
            .entries
            .values()
            .map(|entry| entry.descriptor.id)
            .collect::<BTreeSet<_>>();
        let mut rows = BTreeMap::new();
        self.visit_owners(&mut BTreeSet::new(), &mut |owner| {
            for (id, row) in &owner.rows {
                if selected.contains(id) {
                    rows.insert(*id, row.as_ref());
                }
            }
        });
        rows
    }

    fn selected_group_refs<'a>(
        owners: impl IntoIterator<Item = &'a Self>,
        metadata: &crate::artifact_inventory::RetainedArtifactMetadata,
    ) -> Vec<&'a PendingCertifiedGroup> {
        Self::selected_group_refs_in(
            owners,
            &metadata.artifacts,
            &metadata.selected_native_groups,
        )
    }

    fn selected_group_key(
        group: &PendingCertifiedGroup,
        available: &[(
            &tidepool_repr::execution_schema::CachedHomeOwner,
            ArtifactId,
        )],
        selected_native_groups: &BTreeSet<NativeGroupKey>,
    ) -> Option<NativeGroupKey> {
        available.iter().find_map(|(selected, artifact)| {
            let key = NativeGroupKey {
                artifact: *artifact,
                original_ordinal: group.group().original_ordinal(),
            };
            (**selected == *group.owner() && selected_native_groups.contains(&key)).then_some(key)
        })
    }

    fn selected_group_refs_in<'a>(
        owners: impl IntoIterator<Item = &'a Self>,
        artifacts: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
        selected_native_groups: &BTreeSet<NativeGroupKey>,
    ) -> Vec<&'a PendingCertifiedGroup> {
        let available = artifacts
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some((product.owner(), entry.descriptor.id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut groups = Vec::new();
        let mut seen = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for owner in owners {
            owner.visit_owners(&mut visited, &mut |owner| {
                for group in owner.groups.iter() {
                    if Self::selected_group_key(group, &available, selected_native_groups)
                        .is_some_and(|key| seen.insert(key))
                    {
                        groups.push(group);
                    }
                }
            });
        }
        groups
    }

    fn graph_path_refs<'a>(&'a self) -> BTreeMap<[u8; 32], &'a OwnedExecutionGraphFile> {
        let mut paths = BTreeMap::new();
        self.visit_owners(&mut BTreeSet::new(), &mut |owner| {
            paths.extend(
                owner
                    .graph_paths
                    .iter()
                    .map(|(digest, path)| (*digest, path.as_ref())),
            );
        });
        paths
    }
}

struct RetainedArtifactRow {
    owned_inputs:
        BTreeMap<original_inputs::OriginalInputKind, crate::owned_input_arena::OwnedInputSlice>,
    _directory: Arc<tempfile::TempDir>,
    interface: ExactIfaceArtifact,
    interface_evidence: Value,
    payload: RetainedArtifactPayload,
    canonical_custody: Option<crate::certified_products::CertifiedModuleInterface>,
}

#[derive(Clone)]
enum RetainedArtifactPayload {
    InterfaceOnly,
    Native {
        product: ModuleSnapshot,
        certification_path: PathBuf,
        certification_sha256: [u8; 32],
    },
}

impl RetainedArtifactRow {
    fn original_input_parts(
        &self,
        entry: &ArtifactEntry,
    ) -> Result<Vec<original_inputs::OriginalInputPart>, CompileError> {
        use original_inputs::{OriginalInputKind as Kind, OriginalInputPart};
        let (interface, packages) = match &entry.payload {
            ArtifactPayload::Original(product) => {
                (product.interface_bytes(), product.package_imports_bytes())
            }
            ArtifactPayload::Canonical(interface) => (
                interface.interface_bytes(),
                interface.package_imports_bytes(),
            ),
            ArtifactPayload::Interface(interface, _) => (
                interface.interface_bytes(),
                interface.package_imports_bytes(),
            ),
        };
        let mut parts = vec![
            OriginalInputPart {
                kind: Kind::Interface,
                transport: self.owned_inputs[&Kind::Interface].clone(),
                path: self.interface.path.clone(),
                sha256: self.interface.sha256.clone(),
                bytes: interface.len() as u64,
            },
            OriginalInputPart {
                kind: Kind::Packages,
                transport: self.owned_inputs[&Kind::Packages].clone(),
                path: self.interface.path.with_extension("hi.packages"),
                sha256: hex(&entry.descriptor.package_imports_sha256),
                bytes: packages.len() as u64,
            },
        ];
        if let Some(canonical) = &self.canonical_custody {
            let fields = row(&self.interface_evidence, 5)?;
            parts.push(OriginalInputPart {
                kind: Kind::Certificate,
                transport: self.owned_inputs[&Kind::Certificate].clone(),
                path: PathBuf::from(string(&fields[1])?),
                sha256: string(&fields[2])?.to_owned(),
                bytes: canonical.certificate_bytes().len() as u64,
            });
            if let Some(core) = canonical.core_bytes() {
                parts.push(OriginalInputPart {
                    kind: Kind::Core,
                    transport: self.owned_inputs[&Kind::Core].clone(),
                    path: PathBuf::from(string(&fields[3])?),
                    sha256: string(&fields[4])?.to_owned(),
                    bytes: core.len() as u64,
                });
            }
        }
        if let (
            ArtifactPayload::Original(original),
            RetainedArtifactPayload::Native {
                product,
                certification_path,
                certification_sha256,
            },
        ) = (&entry.payload, &self.payload)
        {
            parts.push(OriginalInputPart {
                kind: Kind::Native,
                transport: self.owned_inputs[&Kind::Native].clone(),
                path: product.path.clone(),
                sha256: hex(&entry
                    .descriptor
                    .product_sha256
                    .ok_or_else(|| failure("original native seal absent"))?),
                bytes: original.product_bytes().len() as u64,
            });
            parts.push(OriginalInputPart {
                kind: Kind::Census,
                transport: self.owned_inputs[&Kind::Census].clone(),
                path: certification_path.clone(),
                sha256: hex(certification_sha256),
                bytes: original.certification_bytes().len() as u64,
            });
        }
        Ok(parts)
    }

    fn artifact(&self) -> DeclarationArtifact {
        DeclarationArtifact {
            interface: self.interface.clone(),
            product: match &self.payload {
                RetainedArtifactPayload::InterfaceOnly => None,
                RetainedArtifactPayload::Native { product, .. } => Some(product.clone()),
            },
        }
    }

    fn native_certification(&self) -> Result<Value, CompileError> {
        match &self.payload {
            RetainedArtifactPayload::Native {
                certification_path,
                certification_sha256,
                ..
            } => Ok(Value::Array(vec![
                path_value(certification_path)?,
                text(hex(certification_sha256)),
            ])),
            RetainedArtifactPayload::InterfaceOnly => {
                Err(failure("original certificate anchor is missing"))
            }
        }
    }
}

/// Native generation demands belong to executable requests. A pure preview
/// observes only its mounted input and must not inherit unrelated heap inputs.
#[derive(Clone, Copy)]
pub(crate) enum RetainedGenerationPolicy {
    PreserveCertifiedDemand,
    PureActivationPreview,
}

#[derive(Clone)]
pub(crate) struct ExactCompilationRequest {
    inputs: Arc<ExactRequestInputs>,
    pub(crate) manifest: PathBuf,
    pub(crate) request_sha256: String,
    // Checked-prefix identity is the persistent declaration baseline. The
    // encoded manifest/request hash additionally binds protected native roles.
    pub(crate) semantic_sha256: [u8; 32],
    pub(crate) producer_sha256: [u8; 32],
    pub(crate) artifacts: Vec<DeclarationArtifact>,
    pub(crate) groups: Arc<[PendingCertifiedGroup]>,
    materialization: Option<Arc<RetainedArtifactMaterialization>>,
    // Only current-program source-selected support can add these roots.
    program_support: Option<ProgramSourceSupport>,
    program_source_lexical: Vec<ExactLexicalNode>,
    source_selected_support: BTreeSet<ExactModuleIdentity>,
    source_search_include: Option<Arc<[PathBuf]>>,
    checked_value_imports: crate::checked_cell::CheckedValueImportAuthority,
    generated_scaffold_imports: Vec<GeneratedScaffoldImportAuthority>,
}

/// One immutable request selection retains its already validated compiler view.
/// Replacing the context or private roles issues a new input owner atomically.
struct ExactRequestInputs {
    context: Arc<ExactDeclarationContext>,
    protected_scaffold: ProtectedScaffoldRequirements,
    private_compiler_input: Option<OriginalCompilerInputs>,
    compiler: Arc<RequestCompilerInputs>,
}

impl ExactRequestInputs {
    fn issue(
        context: Arc<ExactDeclarationContext>,
        protected_scaffold: ProtectedScaffoldRequirements,
        private_compiler_input: Option<OriginalCompilerInputs>,
        compiler: RequestCompilerInputs,
    ) -> Arc<Self> {
        Arc::new(Self {
            context,
            protected_scaffold,
            private_compiler_input,
            compiler: Arc::new(compiler),
        })
    }

    #[cfg(test)]
    fn for_context(context: Arc<ExactDeclarationContext>) -> Arc<Self> {
        let scaffold = ProtectedScaffoldRequirements::default();
        let compiler = scaffold.compiler_inputs(&context, None).unwrap();
        Self::issue(context, scaffold, None, compiler)
    }
}

/// Same-offer support keeps the consumed import graph with its exact custody.
/// Artifact dependencies alone cannot recover original instance scope.
#[derive(Clone)]
struct ProgramSourceSupport {
    compiler_support: crate::artifact_inventory::CompilerSourceSupport,
    imports: Arc<BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>>,
}

impl ProgramSourceSupport {
    fn extend(
        previous: Option<&Self>,
        compiler_support: crate::artifact_inventory::CompilerSourceSupport,
        imports: impl IntoIterator<Item = (ExactModuleIdentity, Vec<ExactModuleIdentity>)>,
    ) -> Result<Self, CompileError> {
        let compiler_support = match previous {
            Some(previous) => previous.compiler_support.merge(&compiler_support)?,
            None => compiler_support,
        };
        let mut retained =
            previous.map_or_else(BTreeMap::new, |value| value.imports.as_ref().clone());
        for (owner, mut requirements) in imports {
            requirements.sort();
            requirements.dedup();
            if retained
                .insert(owner, requirements.clone())
                .is_some_and(|old| old != requirements)
            {
                return Err(failure("program support changed its original import graph"));
            }
        }
        Ok(Self {
            compiler_support,
            imports: Arc::new(retained),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TemplateInterfaceNode {
    pub(crate) interface_sha256: [u8; 32],
    pub(crate) imports: Vec<ExactModuleIdentity>,
}

/// Only direct imports selected by an original checked template can become
/// roots of a later protected template. Supporting graph nodes retain types
/// and instances without acquiring an independent import capability.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SelectedTemplateImports {
    roots: BTreeSet<ExactModuleIdentity>,
    graph: BTreeMap<ExactModuleIdentity, TemplateInterfaceNode>,
}

impl SelectedTemplateImports {
    pub(crate) fn authorization_value(&self) -> Value {
        Value::Array(vec![
            Value::Array(self.roots.iter().map(module_value).collect()),
            Value::Array(
                self.graph
                    .iter()
                    .map(|(owner, node)| {
                        Value::Array(vec![
                            text(&owner.unit),
                            text(&owner.module),
                            text(hex(&node.interface_sha256)),
                            Value::Array(node.imports.iter().map(module_value).collect()),
                        ])
                    })
                    .collect(),
            ),
        ])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RetainedTemplateNode {
    canonical: ArtifactId,
    interface: TemplateInterfaceNode,
}

/// Live checked-value custody, issued from the original compiler receipt.
/// Neither a type projection nor recovered native availability can issue it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RetainedTemplateImports {
    producer: [u8; 32],
    roots: BTreeSet<ExactModuleIdentity>,
    graph: BTreeMap<ExactModuleIdentity, RetainedTemplateNode>,
    compiler_support: crate::artifact_inventory::CompilerSourceSupport,
}

fn template_selects_owner(templates: &[String], owner: &ExactModuleIdentity) -> bool {
    templates.iter().any(|template| {
        template
            .lines()
            .any(|line| template_import_line(line, owner))
    })
}

fn template_import_line(line: &str, owner: &ExactModuleIdentity) -> bool {
    let Some(rest) = line.trim_start().strip_prefix("import") else {
        return false;
    };
    if !rest.chars().next().is_some_and(char::is_whitespace) {
        return false;
    }
    let mut rest = rest.trim_start();
    let qualified = if let Some(qualified) = rest.strip_prefix("qualified") {
        if !qualified.chars().next().is_some_and(char::is_whitespace) {
            return false;
        }
        rest = qualified.trim_start();
        true
    } else {
        false
    };
    let Some(after_module) = rest.strip_prefix(&owner.module) else {
        return false;
    };
    if after_module.is_empty() {
        return true;
    }
    if !after_module.chars().next().is_some_and(char::is_whitespace) {
        return false;
    }
    let mut tail = after_module.trim_start();
    if qualified {
        if let Some(alias) = tail.strip_prefix("as ") {
            let alias_end = alias
                .char_indices()
                .find(|(_, character)| character.is_whitespace())
                .map_or(alias.len(), |(index, _)| index);
            let name = &alias[..alias_end];
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            {
                return false;
            }
            tail = alias[alias_end..].trim_start();
        }
    }
    tail.is_empty()
        || tail.starts_with('(')
        || tail.strip_prefix("hiding").is_some_and(|items| {
            items.chars().next().is_some_and(char::is_whitespace)
                && items.trim_start().starts_with('(')
        })
}

impl RetainedTemplateImports {
    pub(crate) fn compiler_source_support(
        &self,
    ) -> &crate::artifact_inventory::CompilerSourceSupport {
        &self.compiler_support
    }

    pub(crate) fn retain_in(&self, artifacts: &ArtifactView) -> Result<ArtifactView, CompileError> {
        artifacts.retain_custody(self.compiler_support.artifacts())
    }

    pub(crate) fn capture(
        original: &ExactDeclarationContext,
        templates: &[String],
    ) -> Result<Option<Arc<Self>>, CompileError> {
        let interfaces = original.compiler_source_support().selected_interfaces()?;
        let canonical = interfaces
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Canonical(interface)
                    if matches!(
                        interface.origin(),
                        crate::certified_products::CanonicalOrigin::SourceOriginal { .. }
                    ) =>
                {
                    Some((entry.descriptor.owner.clone(), entry.clone()))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let roots = original
            .lexical_graph()
            .iter()
            .filter(|node| {
                canonical.contains_key(&node.owner)
                    && !node.owner.module.starts_with("Tidepool.Session.")
                    && template_selects_owner(templates, &node.owner)
            })
            .map(|node| node.owner.clone())
            .collect::<BTreeSet<_>>();
        if roots.is_empty() {
            return Ok(None);
        }
        if !matches!(
            original.owner.facts.original_instance_environment,
            OriginalInstanceEnvironment::Complete { .. }
        ) {
            return Err(failure(
                "retained template imports require complete original compiler execution evidence",
            ));
        }
        let lexical = original
            .lexical_graph()
            .iter()
            .map(|node| (&node.owner, node))
            .collect::<BTreeMap<_, _>>();
        let mut graph = BTreeMap::new();
        let mut pending = roots.iter().cloned().collect::<Vec<_>>();
        while let Some(owner) = pending.pop() {
            if graph.contains_key(&owner) {
                continue;
            }
            let lexical = lexical
                .get(&owner)
                .ok_or_else(|| failure("checked template lexical closure is incomplete"))?;
            let entry = canonical
                .get(&owner)
                .ok_or_else(|| failure("original template graph lacks canonical source custody"))?;
            pending.extend(lexical.imports.iter().cloned());
            graph.insert(
                owner,
                RetainedTemplateNode {
                    canonical: entry.descriptor.id,
                    interface: TemplateInterfaceNode {
                        interface_sha256: entry.descriptor.interface_sha256,
                        imports: lexical.imports.clone(),
                    },
                },
            );
        }
        let compiler_support = interfaces.support_for_owners(graph.keys().cloned().collect())?;
        let retained = Arc::new(Self {
            producer: original.owner.facts.producer,
            roots,
            graph,
            compiler_support,
        });
        retained.validate(original.owner.facts.producer, original.artifact_view())?;
        Ok(Some(retained))
    }

    fn validate(&self, producer: [u8; 32], artifacts: &ArtifactView) -> Result<(), CompileError> {
        if producer != self.producer
            || self.roots.is_empty()
            || self.graph.len() > 4096
            || self
                .graph
                .values()
                .map(|node| node.interface.imports.len())
                .sum::<usize>()
                > 65536
            || self.roots.iter().any(|root| !self.graph.contains_key(root))
        {
            return Err(failure("retained template roots or producer differ"));
        }
        let entries = artifacts.entries();
        let mut reachable = BTreeSet::new();
        let mut pending = self.roots.iter().cloned().collect::<Vec<_>>();
        while let Some(owner) = pending.pop() {
            if !reachable.insert(owner.clone()) {
                continue;
            }
            let node = self
                .graph
                .get(&owner)
                .ok_or_else(|| failure("retained template graph is not closed"))?;
            if !entries.iter().any(|entry| entry.descriptor.id == node.canonical
                && entry.descriptor.owner == owner
                && entry.descriptor.producer_sha256 == producer
                && entry.descriptor.interface_sha256 == node.interface.interface_sha256
                && matches!(&entry.payload, ArtifactPayload::Canonical(interface)
                    if matches!(interface.origin(), crate::certified_products::CanonicalOrigin::SourceOriginal { .. }))) {
                return Err(failure("retained template canonical identity differs"));
            }
            let imports = node.interface.imports.iter().collect::<BTreeSet<_>>();
            if imports.len() != node.interface.imports.len() {
                return Err(failure("duplicate retained template edge"));
            }
            pending.extend(node.interface.imports.iter().cloned());
        }
        if reachable.len() != self.graph.len() {
            return Err(failure("retained template graph has unselected owners"));
        }
        Ok(())
    }

    fn merge(left: &Arc<Self>, right: &Arc<Self>) -> Result<Arc<Self>, CompileError> {
        if Arc::ptr_eq(left, right) {
            return Ok(left.clone());
        }
        if left.producer != right.producer {
            return Err(failure("template producers differ"));
        }
        let mut graph = left.graph.clone();
        for (owner, node) in &right.graph {
            if graph
                .insert(owner.clone(), node.clone())
                .is_some_and(|prior| prior != *node)
            {
                return Err(failure("retained templates select conflicting originals"));
            }
        }
        let merged = Arc::new(Self {
            producer: left.producer,
            roots: left.roots.union(&right.roots).cloned().collect(),
            graph,
            compiler_support: left.compiler_support.merge(&right.compiler_support)?,
        });
        merged.validate(merged.producer, merged.compiler_support.artifacts())?;
        Ok(merged)
    }
}

/// The compiler-only import belongs to a hash-sealed checked template and one
/// original native/interface owner. It never grants authored lexical visibility.
#[derive(Clone)]
struct GeneratedScaffoldImportAuthority {
    role: GeneratedScaffoldRole,
    protected_templates: Arc<[String]>,
}

#[derive(Clone)]
enum GeneratedScaffoldRole {
    PlannedDeclaration(Arc<CertifiedAuthoredDeclaration>),
    InitialTemplateInterfaces {
        producer: [u8; 32],
        roots: BTreeSet<ExactModuleIdentity>,
        graph: Arc<BTreeMap<ExactModuleIdentity, TemplateInterfaceNode>>,
    },
}

impl GeneratedScaffoldImportAuthority {
    /// Match the virtual graph installed by the compiler from this protected
    /// recipe. This is implementation/instance evidence, not authored imports.
    fn original_instance_graph(&self) -> Vec<ExactLexicalNode> {
        match &self.role {
            GeneratedScaffoldRole::PlannedDeclaration(certificate) => {
                let owner = certificate.product().owner();
                vec![ExactLexicalNode {
                    owner: identity(&owner.unit, &owner.module),
                    imports: Vec::new(),
                }]
            }
            GeneratedScaffoldRole::InitialTemplateInterfaces { graph, .. } => graph
                .iter()
                .map(|(owner, node)| ExactLexicalNode {
                    owner: owner.clone(),
                    imports: node.imports.clone(),
                })
                .collect(),
        }
    }

    fn permits(
        &self,
        context: &ExactDeclarationContext,
        source: &str,
        target: &Path,
        module_source: &Path,
        unit: &str,
        module: &str,
        qualifier: &str,
        boot: bool,
    ) -> bool {
        let certificate = match &self.role {
            GeneratedScaffoldRole::PlannedDeclaration(certificate) => certificate,
            GeneratedScaffoldRole::InitialTemplateInterfaces {
                producer,
                roots,
                graph,
            } => {
                if target != module_source
                    || boot
                    || qualifier != "none"
                    || *producer != context.owner.facts.producer
                {
                    return false;
                }
                let key = identity(unit, module);
                if !roots.contains(&key) {
                    return false;
                }
                let Some(node) = graph.get(&key) else {
                    return false;
                };
                let occurrences = |text: &str| {
                    let mut counts = BTreeMap::new();
                    for line in text.lines().filter(|line| template_import_line(line, &key)) {
                        *counts.entry(line.to_owned()).or_insert(0usize) += 1;
                    }
                    counts
                };
                let rendered = occurrences(source);
                // GHC owns per-occurrence AST/span admission. This receipt
                // guard requires the whole protected owner census unchanged;
                // extra authored imports need their ordinary source proof.
                return !rendered.is_empty()
                    && self
                        .protected_templates
                        .iter()
                        .any(|template| occurrences(template) == rendered)
                    && context.artifact_view().entries().iter().any(|entry| {
                        entry.descriptor.owner == key
                            && entry.descriptor.interface_sha256 == node.interface_sha256
                    });
            }
        };
        let owner = certificate.product().owner();
        if target != module_source
            || boot
            || qualifier != "none"
            || unit != owner.unit
            || module != owner.module
        {
            return false;
        }
        let compiler_import = format!("import {}", owner.module);
        if source
            .lines()
            .filter(|line| *line == compiler_import)
            .count()
            != 1
        {
            return false;
        }
        if !self.protected_templates.iter().any(|template| {
            template
                .lines()
                .filter(|line| *line == "-- tidepool-preamble-imports-v1")
                .count()
                == 1
        }) {
            return false;
        }
        context.artifact_view().entries().iter().any(|entry| {
            matches!(&entry.payload, ArtifactPayload::Original(product)
                if product == certificate.product()
                    && certificate.toolchain_identity_sha256() == context.owner.facts.producer)
        })
    }
}

/// A successful compiler transaction's actual generated source, bound to its
/// immutable declaration context. It is not ordinary source-cache evidence.
#[derive(Clone, Debug)]
pub struct ExactSourceWitness {
    source_path: PathBuf,
    source_sha256: [u8; 32],
}

impl ExactSourceWitness {
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }
    pub fn source_sha256(&self) -> &[u8; 32] {
        &self.source_sha256
    }
    pub fn matches_source(&self, path: &Path, source: &str) -> bool {
        use sha2::Digest;
        self.source_path == path
            && self.source_sha256 == <[u8; 32]>::from(sha2::Sha256::digest(source.as_bytes()))
    }
}

/// The existing compiler request identity, borrowed for comparison and retained
/// only by source admissions issued after receipt validation.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ExactRequestIdentity<'a> {
    request_sha256: std::borrow::Cow<'a, str>,
    semantic_sha256: [u8; 32],
    producer_sha256: [u8; 32],
}

impl<'a> ExactRequestIdentity<'a> {
    fn from_request(request: &'a ExactCompilationRequest) -> Self {
        Self {
            request_sha256: std::borrow::Cow::Borrowed(&request.request_sha256),
            semantic_sha256: request.semantic_sha256,
            producer_sha256: request.producer_sha256,
        }
    }

    fn into_owned(self) -> ExactRequestIdentity<'static> {
        ExactRequestIdentity {
            request_sha256: std::borrow::Cow::Owned(self.request_sha256.into_owned()),
            semantic_sha256: self.semantic_sha256,
            producer_sha256: self.producer_sha256,
        }
    }
}

pub(crate) struct ExactSourceAdmission {
    request_identity: ExactRequestIdentity<'static>,
    witness: ExactSourceWitness,
    evidence: Arc<crate::cache::CompletedSourceEvidence>,
    evidence_bytes: Vec<u8>,
    exact_imports: BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    // Roots accepted by each retained typed protected-recipe authority.
    scaffold_roots: BTreeMap<usize, BTreeSet<ExactModuleIdentity>>,
    exact_source_imports:
        BTreeMap<ExactModuleIdentity, Vec<crate::certified_products::CanonicalSourceImport>>,
    selected_originals:
        BTreeMap<ExactModuleIdentity, crate::execution_source::SourceSelectedOriginal>,
}

/// One physical segment's source receipt is classified only against its
/// immutable input selection. Native item projections may grow output custody.
pub(crate) struct ExactProgramSegmentAdmission {
    request: ExactCompilationRequest,
    admissions: Vec<ExactSourceAdmission>,
    original_products: std::sync::OnceLock<crate::certified_products::CertifiedSegmentOriginals>,
}
impl ExactProgramSegmentAdmission {
    fn validate_request(&self, request: &ExactCompilationRequest) -> Result<(), CompileError> {
        if ExactRequestIdentity::from_request(&self.request)
            != ExactRequestIdentity::from_request(request)
        {
            return Err(failure(
                "segment source authority has another physical request",
            ));
        }
        Ok(())
    }
    pub(crate) fn product_admission<'a>(
        &'a self,
        request: &ExactCompilationRequest,
        path: &'a Path,
        source: &'a str,
        evidence: &'a [u8],
        inventory: &'a tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<MatchedExactProductAdmission<'a>, CompileError> {
        self.validate_request(request)?;
        let matched = self
            .admissions
            .iter()
            .filter(|admission| admission.witness.matches_source(path, source))
            .collect::<Vec<_>>();
        let first = matched
            .first()
            .ok_or_else(|| failure("projected item lacks its physical segment source receipt"))?;
        crate::artifacts::charge_source_evidence(inventory, evidence.len())?;
        for admission in &matched {
            admission.validate_request(request)?;
            admission.validate_ineligible_evidence(evidence)?;
        }
        // Repeated projected receipt rows are equal authority, not new source
        // compilation; retain every validated row for final conflict checks.
        if matched.iter().any(|admission| {
            admission.evidence_bytes != first.evidence_bytes
                || admission.exact_imports != first.exact_imports
                || admission.exact_source_imports != first.exact_source_imports
                || admission.scaffold_roots != first.scaffold_roots
                || admission.selected_originals.len() != first.selected_originals.len()
                || admission
                    .selected_originals
                    .iter()
                    .any(|(owner, original)| {
                        first.selected_originals.get(owner).is_none_or(|expected| {
                            original.interface() != expected.interface()
                                || original.imports() != expected.imports()
                        })
                    })
        }) {
            return Err(failure("projected segment source receipts disagree"));
        }
        Ok(MatchedExactProductAdmission {
            exact: ExactProductAdmission {
                request: &self.request,
                source: first,
            },
            worker_bytes: evidence,
            input_path: path,
            target_source: source,
            inventory,
        })
    }
    pub(crate) fn physical_request(&self) -> &ExactCompilationRequest {
        &self.request
    }

    pub(crate) fn item_admission<'a>(
        &'a self,
        request: &ExactCompilationRequest,
        path: &Path,
        source: &str,
    ) -> Result<ExactProductAdmission<'a>, CompileError> {
        self.validate_request(request)?;
        let source = self
            .admissions
            .iter()
            .find(|admission| admission.witness.matches_source(path, source))
            .ok_or_else(|| failure("item source differs from its admitted segment"))?;
        Ok(ExactProductAdmission {
            request: &self.request,
            source,
        })
    }

    pub(crate) fn install_original_products(
        &mut self,
        products: crate::certified_products::CertifiedSegmentOriginals,
    ) -> Result<(), CompileError> {
        self.original_products
            .set(products)
            .map_err(|_| failure("segment originals already admitted"))
    }

    pub(crate) fn original_products(
        &self,
        request: &ExactCompilationRequest,
        operation: &Arc<tidepool_repr::execution_schema::InventoryOperation>,
    ) -> Result<&crate::certified_products::CertifiedSegmentOriginals, CompileError> {
        self.validate_request(request)?;
        let products = self
            .original_products
            .get()
            .ok_or_else(|| failure("segment original facts not admitted"))?;
        if !Arc::ptr_eq(products.operation(), operation) {
            return Err(failure("segment originals have another accounting owner"));
        }
        Ok(products)
    }

    pub(crate) fn admissions(&self) -> &[ExactSourceAdmission] {
        &self.admissions
    }
}

pub(crate) struct ExactProductAdmission<'a> {
    pub(crate) request: &'a ExactCompilationRequest,
    pub(crate) source: &'a ExactSourceAdmission,
}

/// Captured product evidence matched by the exact receipt issuer. The source
/// and request remain those retained by the issuer; input and bytes belong to
/// the current physical output and its accounting operation.
pub(crate) struct MatchedExactProductAdmission<'a> {
    exact: ExactProductAdmission<'a>,
    worker_bytes: &'a [u8],
    input_path: &'a Path,
    target_source: &'a str,
    inventory: &'a tidepool_repr::execution_schema::InventoryOperation,
}

impl<'a> MatchedExactProductAdmission<'a> {
    pub(crate) fn exact(&self) -> &ExactProductAdmission<'a> {
        &self.exact
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        ExactProductAdmission<'a>,
        &'a [u8],
        &'a Path,
        &'a str,
        &'a tidepool_repr::execution_schema::InventoryOperation,
    ) {
        (
            self.exact,
            self.worker_bytes,
            self.input_path,
            self.target_source,
            self.inventory,
        )
    }
}

/// Resolve home adjacency from the compiler's complete consumed-source graph.
/// This helper carries no admission authority; its original issuer validates
/// the evidence and interface custody before retaining the lexical surface.
pub(crate) fn consumed_source_home_imports(
    evidence: &crate::cache::DependencyEvidence,
    exact_imports: &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
) -> Result<BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>, CompileError> {
    let source_paths = evidence
        .sources
        .iter()
        .map(|source| &source.path)
        .collect::<BTreeSet<_>>();
    let mut selected_owners = BTreeMap::new();
    for node in &evidence.modules {
        if selected_owners
            .insert((&node.unit, &node.module, node.boot, &node.source), node)
            .is_some()
        {
            return Err(failure("duplicate captured source import owner"));
        }
    }
    let mut imports = BTreeMap::new();
    for node in evidence.modules.iter().filter(|node| !node.boot) {
        if !source_paths.contains(&node.source) {
            return Err(failure(
                "original source import owner lacks its captured source",
            ));
        }
        let owner = identity(&node.unit, &node.module);
        let mut requirements = exact_imports
            .get(&owner)
            .into_iter()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>();
        for edge in &node.imports {
            let Some(selected) = &edge.selected else {
                continue;
            };
            if !matches!(&edge.qualifier, crate::cache::ImportQualifier::Unqualified)
                && !matches!(&edge.qualifier,
                        crate::cache::ImportQualifier::ThisUnit(unit) if unit == &node.unit)
            {
                return Err(failure("selected home import has another unit qualifier"));
            }
            let Some(selected_owner) =
                selected_owners.get(&(&node.unit, &edge.module, edge.boot, selected))
            else {
                return Err(failure(
                    "selected home import lacks one captured source owner",
                ));
            };
            if !source_paths.contains(selected) {
                return Err(failure("selected home import lacks its captured source"));
            }
            requirements.insert(identity(&selected_owner.unit, &selected_owner.module));
        }
        if imports
            .insert(owner, requirements.into_iter().collect())
            .is_some()
        {
            return Err(failure("duplicate original source import owner"));
        }
    }
    Ok(imports)
}

impl ExactSourceAdmission {
    pub(crate) fn evidence_bytes(&self) -> &[u8] {
        &self.evidence_bytes
    }
    pub(crate) fn selected_originals(
        &self,
    ) -> &BTreeMap<ExactModuleIdentity, crate::execution_source::SourceSelectedOriginal> {
        &self.selected_originals
    }
    pub(crate) fn witness(&self) -> &ExactSourceWitness {
        &self.witness
    }
    pub(crate) fn into_witness(self) -> ExactSourceWitness {
        self.witness
    }
    pub(crate) fn evidence(&self) -> &Arc<crate::cache::CompletedSourceEvidence> {
        &self.evidence
    }
    pub(crate) fn exact_imports(&self) -> &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>> {
        &self.exact_imports
    }
    pub(crate) fn exact_source_imports(
        &self,
    ) -> &BTreeMap<ExactModuleIdentity, Vec<crate::certified_products::CanonicalSourceImport>> {
        &self.exact_source_imports
    }

    pub(crate) fn match_product<'a>(
        &'a self,
        request: &'a ExactCompilationRequest,
        input_path: &'a Path,
        target_source: &'a str,
        worker_bytes: &'a [u8],
        inventory: &'a tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<MatchedExactProductAdmission<'a>, CompileError> {
        self.validate_request(request)?;
        if !self.witness.matches_source(input_path, target_source) {
            return Err(failure(
                "product source differs from its exact source receipt",
            ));
        }
        crate::artifacts::charge_source_evidence(inventory, worker_bytes.len())?;
        self.validate_ineligible_evidence(worker_bytes)?;
        Ok(MatchedExactProductAdmission {
            exact: ExactProductAdmission {
                request,
                source: self,
            },
            worker_bytes,
            input_path,
            target_source,
            inventory,
        })
    }

    pub(crate) fn validate_request(
        &self,
        request: &ExactCompilationRequest,
    ) -> Result<(), CompileError> {
        if self.request_identity != ExactRequestIdentity::from_request(request) {
            return Err(failure("completed source has another physical request"));
        }
        Ok(())
    }

    /// Item projections can publish separate receipts for one source owner.
    /// Only the scaffold identity is combined; support policy retains every
    /// validated admission and its import and original-selection evidence.
    pub(crate) fn matching_generated_source_owner(
        admissions: &[Self],
        source: &str,
    ) -> Result<ExactModuleIdentity, CompileError> {
        let owners = admissions
            .iter()
            .filter(|admission| {
                admission
                    .witness
                    .matches_source(admission.witness.source_path(), source)
            })
            .map(Self::generated_source_owner)
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut owners = owners.into_iter();
        match (owners.next(), owners.next()) {
            (Some(owner), None) => Ok(owner),
            _ => Err(CompileError::ExtractFailed(
                "program support lacks one authenticated generated source owner".into(),
            )),
        }
    }

    /// Completed-source validation binds GENERATED_SOURCE to this receipt's
    /// hash-verified witness. Resolve its owner without parsing source text.
    pub(crate) fn generated_source_owner(&self) -> Result<ExactModuleIdentity, CompileError> {
        let owners = self
            .evidence
            .modules
            .iter()
            .filter(|node| {
                !node.boot
                    && (node.source == Path::new(crate::cache::GENERATED_SOURCE)
                        || node.source == self.witness.source_path())
            })
            .map(|node| identity(&node.unit, &node.module))
            .collect::<Vec<_>>();
        match owners.as_slice() {
            [owner] => Ok(owner.clone()),
            _ => Err(failure(
                "generated source lacks one authenticated consumed owner",
            )),
        }
    }

    pub(crate) fn home_imports(
        &self,
    ) -> Result<BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>, CompileError> {
        let mut imports = consumed_source_home_imports(&self.evidence, &self.exact_imports)?;
        for (owner, original) in &self.selected_originals {
            if imports
                .insert(owner.clone(), original.imports().to_vec())
                .is_some()
            {
                return Err(failure(
                    "fresh source collides with a source-selected original",
                ));
            }
        }
        Ok(imports)
    }

    pub(crate) fn validate_ineligible_evidence(&self, bytes: &[u8]) -> Result<(), CompileError> {
        let mut expected: crate::cache::DependencyEvidence =
            serde_json::from_slice(&self.evidence_bytes).map_err(failure)?;
        expected.cache_safe = false;
        expected.selection_complete = false;
        let actual: crate::cache::DependencyEvidence =
            serde_json::from_slice(bytes).map_err(failure)?;
        if serde_json::to_value(expected).map_err(failure)?
            != serde_json::to_value(actual).map_err(failure)?
        {
            return Err(failure(
                "source-cache-ineligible evidence differs from exact fresh proof",
            ));
        }
        Ok(())
    }
}

impl ExactProductAdmission<'_> {
    // Fixture roles are explicit request/support roles plus the authenticated
    // generated target. They enter the same native issuer as production output.
    #[cfg(test)]
    pub(crate) fn original_execution_fixture(
        &self,
        artifacts: &ArtifactView,
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        let generated = self.source.generated_source_owner()?;
        let entries = artifacts
            .entries()
            .into_iter()
            .filter(|entry| {
                entry.descriptor.owner == generated
                    && matches!(entry.payload, ArtifactPayload::Original(_))
            })
            .collect::<Vec<_>>();
        let mut projection = self
            .request
            .compiler_inputs()
            .projection()
            .within_view(artifacts)
            .merge(&CompilerInputProjection::from_issued_entries(&entries)?)?;
        if let Some(support) = &self.request.program_support {
            projection =
                projection.merge(&support.compiler_support.projection().within_view(artifacts))?;
        }
        let selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .map_err(compiler_evidence_failure)?;
        self.original_execution_context(&OriginalCompilerInputs::from_selection(
            &selection, artifacts,
        )?)
    }

    /// Retain the complete original compiler selection before publication.
    /// The output issuer owns every exact native role, including cached support.
    pub(crate) fn original_execution_context(
        &self,
        inputs: &OriginalCompilerInputs,
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        let support = crate::artifact_inventory::CompilerSourceSupport::from_original_output(
            self.request,
            inputs,
        )?;
        let artifacts = support.artifacts();
        let projection = support.projection();
        let imports = self.source.home_imports()?;
        let inherited = self.request.context().as_ref();
        for (owner, original) in &self.source.selected_originals {
            if self
                .request
                .compiler_inputs()
                .metadata
                .entries
                .get(owner)
                .and_then(|entry| canonical_source_interface(entry))
                != Some(original.interface())
            {
                return Err(failure(
                    "original instance source differs from selected canonical owner",
                ));
            }
        }
        // Shared source visibility omits mounted values; authenticate that
        // projection before using the receipt's complete instance adjacency.
        if !self.source.selected_originals.is_empty() {
            crate::declaration_join::source_lexical_surface(
                &self
                    .source
                    .selected_originals
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                &imports,
                &inherited
                    .lexical_graph()
                    .iter()
                    .filter(|node| !node.owner.module.starts_with("Tidepool.Session."))
                    .cloned()
                    .collect::<Vec<_>>(),
                &artifacts.source_implementation_roles(),
            )?;
        }
        let exact_roots = self
            .source
            .exact_imports
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut scaffold = Vec::new();
        if let Some(support) = &self.request.program_support {
            let roots = support
                .compiler_support
                .artifacts()
                .root_entries()
                .iter()
                .map(|entry| entry.descriptor.owner.clone())
                .collect::<BTreeSet<_>>();
            let inherited_owners = inherited
                .lexical_graph()
                .iter()
                .map(|node| &node.owner)
                .collect::<BTreeSet<_>>();
            let mut pending = exact_roots
                .intersection(&roots)
                .cloned()
                .collect::<Vec<_>>();
            let mut seen = BTreeSet::new();
            while let Some(owner) = pending.pop() {
                if !seen.insert(owner.clone())
                    || imports.contains_key(&owner)
                    || inherited_owners.contains(&owner)
                {
                    continue;
                }
                // A missing row remains missing original evidence. Never
                // replace its unknown home imports with an empty leaf.
                if let Some(requirements) = support.imports.get(&owner) {
                    pending.extend(requirements.iter().cloned());
                    scaffold.push(ExactLexicalNode {
                        owner,
                        imports: requirements.clone(),
                    });
                }
            }
        }
        for (index, authority) in self.request.generated_scaffold_imports.iter().enumerate() {
            let graph = authority
                .original_instance_graph()
                .into_iter()
                .map(|node| (node.owner.clone(), node))
                .collect::<BTreeMap<_, _>>();
            let mut pending = self
                .source
                .scaffold_roots
                .get(&index)
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            let mut seen = BTreeSet::new();
            while let Some(owner) = pending.pop() {
                if !seen.insert(owner.clone()) || imports.contains_key(&owner) {
                    continue;
                }
                let node = graph
                    .get(&owner)
                    .ok_or_else(|| failure("protected scaffold instance graph is incomplete"))?;
                pending.extend(node.imports.iter().cloned());
                // A real consumed source row owns its actual adjacency; native
                // protected scaffold leaves apply only to source-less owners.
                scaffold.push(node.clone());
            }
        }
        self.request.checked_value_imports.validate()?;
        let retained_interfaces = projection
            .project_metadata(artifacts.metadata_snapshot())?
            .entries;
        for (unit, module) in self.request.checked_value_imports.owners() {
            let owner = identity(unit, module);
            if !exact_roots.contains(&owner) || imports.contains_key(&owner) {
                continue;
            }
            if let Some(entry) = retained_interfaces.get(&owner) {
                match &entry.payload {
                    ArtifactPayload::Interface(interface, JoinedInterfaceRole::ValueInterface)
                        if self
                            .request
                            .checked_value_imports
                            .matches_interface(interface) =>
                    {
                        // Compiler-issued thin value interfaces define no
                        // instances. Nominal requirements are not scope edges.
                        scaffold.push(ExactLexicalNode {
                            owner,
                            imports: Vec::new(),
                        });
                    }
                    _ => {
                        return Err(failure(
                            "checked value instance evidence differs from its exact interface authority",
                        ));
                    }
                }
            }
        }
        // Selected-original receipts authenticate complete compiler adjacency.
        // Their shared lexical rows omit mounted session interfaces and cannot
        // stand in for that original instance graph.
        let lexical = compose_lexical_nodes(
            inherited
                .lexical_graph()
                .iter()
                .filter(|node| !self.source.selected_originals.contains_key(&node.owner))
                .cloned()
                .chain(scaffold.iter().cloned())
                .chain(imports.iter().map(|(owner, imports)| ExactLexicalNode {
                    owner: owner.clone(),
                    imports: imports.clone(),
                }))
                .collect::<Vec<_>>()
                .iter(),
        )?;
        let mut required = imports
            .keys()
            .cloned()
            .chain(scaffold.iter().map(|node| node.owner.clone()))
            .chain(
                inherited
                    .lexical_graph()
                    .iter()
                    .map(|node| node.owner.clone()),
            )
            .collect::<BTreeSet<_>>();
        if let OriginalInstanceEnvironment::MissingOriginalOwners(owners) =
            inherited.original_instance_environment()
        {
            required.extend(owners.iter().cloned());
        }
        let context = ExactDeclarationContext::from_authenticated_execution_support(
            self.request.producer_sha256,
            support,
            lexical,
            self.source.generated_source_owner()?,
            &required.into_iter().collect::<Vec<_>>(),
        )?;
        Ok(Arc::new(context))
    }
}

impl ExactCompilationRequest {
    pub(crate) fn input_transport_files(&self) -> Vec<Arc<std::fs::File>> {
        let Some(materialization) = &self.materialization else {
            return Vec::new();
        };
        let metadata = &self.inputs.compiler.native_transport.metadata;
        let mut files = BTreeMap::new();
        for row in materialization.selected_rows(metadata).into_values() {
            for slice in row.owned_inputs.values() {
                let file = slice.compiler_file_lease();
                files.insert(Arc::as_ptr(&file) as usize, file);
            }
        }
        let graph_paths = materialization.graph_path_refs();
        for entry in metadata.entries.values() {
            if let ArtifactPayload::Original(product) = &entry.payload {
                if let Some(graph) = product.execution_source() {
                    if let Some(owned) = graph_paths.get(&graph.digest()) {
                        let file = owned.transport.compiler_file_lease();
                        files.insert(Arc::as_ptr(&file) as usize, file);
                    }
                }
            }
        }
        files.into_values().collect()
    }

    pub(crate) fn context(&self) -> &Arc<ExactDeclarationContext> {
        &self.inputs.context
    }

    pub(crate) fn compiler_inputs(&self) -> &RequestCompilerInputs {
        &self.inputs.compiler
    }

    /// Validate the issued request roles, including private native availability.
    /// Persistent declarations retain their independent lexical and type roles.
    pub(crate) fn validate_artifacts(&self) -> Result<(), CompileError> {
        self.context()
            .validate_artifacts_from_metadata(&self.artifacts, &self.compiler_inputs().metadata)
    }

    /// Grow a same-offer request from its already certified private selection.
    /// New roles materialize in the same atomic transition as declaration growth.
    pub(crate) fn in_program_context_with_private_input(
        &self,
        root: &Path,
        context: Arc<ExactDeclarationContext>,
        input: &OriginalCompilerInputs,
    ) -> Result<Self, CompileError> {
        let private_span = tracing::debug_span!(target: "exomonad_harness::timing", "exact.program_context.private_input", inclusive = true).entered();
        let private = match &self.inputs.private_compiler_input {
            Some(previous) => previous.merge(input)?,
            None => input.clone(),
        };
        drop(private_span);
        self.in_program_context_with_inputs(root, context, Some(private))
    }

    /// Original offers authenticated by this request's compiler namespace.
    /// Full retained recovery custody is deliberately a separate inventory.
    pub(crate) fn compiler_original_products(
        &self,
    ) -> Result<Vec<CertifiedRecoveryProduct>, CompileError> {
        Ok(self
            .compiler_inputs()
            .metadata
            .entries
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some(product.clone()),
                _ => None,
            })
            .collect())
    }
    /// Capture metadata from this still-live offer, never from reconstructed
    /// source or current cache state. The copy is diagnostic, not authority.
    pub(crate) fn retain_input_diagnostics(&self, destination: &Path) -> std::io::Result<()> {
        use std::io::Read;
        #[derive(serde::Serialize)]
        struct Facts<'a> {
            scope: &'static str,
            original_manifest: &'a Path,
            retained_manifest: &'static str,
            request_sha256: &'a str,
            observed_manifest_sha256: String,
            manifest_matches_request: bool,
            semantic_sha256: String,
            producer_sha256: String,
            selected_lexical_graph: &'a [ExactLexicalNode],
            retained_interfaces: &'a [DeclarationArtifact],
            source_selected_support: Vec<crate::artifact_inventory::ArtifactDescriptor>,
            checked_value_imports: Vec<(&'a str, &'a str)>,
        }
        struct BoundedBytes(Vec<u8>);
        impl Write for BoundedBytes {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > EXACT_SCOPE_BYTES_LIMIT - self.0.len() {
                    return Err(std::io::Error::other(
                        "exact request diagnostics exceed four MiB",
                    ));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        // Read with a limit so a changed/growing manifest cannot cause an
        // unbounded diagnostic allocation. Keep its actual bytes and hash.
        let mut bytes = Vec::new();
        std::fs::File::open(&self.manifest)?
            .take((EXACT_SCOPE_BYTES_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > EXACT_SCOPE_BYTES_LIMIT {
            return Err(std::io::Error::other(
                "exact request manifest exceeds four MiB",
            ));
        }
        let directory = destination.join("exact-request");
        std::fs::create_dir(&directory)?;
        std::fs::write(directory.join("manifest.cbor"), &bytes)?;
        let support = self
            .program_support
            .as_ref()
            .map(|support| support.compiler_support.artifacts().root_entries())
            .unwrap_or_default();
        let checked_value_imports = self.checked_value_imports.owners().collect::<Vec<_>>();
        if self.artifacts.len() > EXACT_SCOPE_GRAPHS_LIMIT
            || self.context().owner.facts.lexical.len() > EXACT_SCOPE_GRAPHS_LIMIT
            || support.len() > EXACT_SCOPE_GRAPHS_LIMIT
            || checked_value_imports.len() > EXACT_SCOPE_GRAPHS_LIMIT
        {
            return Err(std::io::Error::other(
                "exact request diagnostic owners exceed 4096",
            ));
        }
        let observed_manifest_sha256 = crate::checked_cell::hash(&bytes);
        let facts = Facts {
            scope: "original compiler offer; diagnostic only; never compiler authority",
            original_manifest: &self.manifest,
            retained_manifest: "manifest.cbor",
            manifest_matches_request: observed_manifest_sha256 == self.request_sha256,
            observed_manifest_sha256,
            request_sha256: &self.request_sha256,
            semantic_sha256: hex(&self.semantic_sha256),
            producer_sha256: hex(&self.producer_sha256),
            selected_lexical_graph: &self.context().owner.facts.lexical,
            retained_interfaces: &self.artifacts,
            source_selected_support: support
                .iter()
                .map(|entry| entry.descriptor.clone())
                .collect(),
            checked_value_imports,
        };
        let mut encoded = BoundedBytes(Vec::new());
        serde_json::to_writer_pretty(&mut encoded, &facts).map_err(std::io::Error::other)?;
        std::fs::write(directory.join("facts.json"), encoded.0)?;
        let mut capture =
            crate::artifacts::InputArtifactDiagnostics::new(directory.join("input-artifacts"))?;
        let inputs = self
            .compiler_inputs()
            .metadata
            .entries
            .values()
            .map(|entry| {
                let observed = match capture.payload(&entry.payload) {
                    Ok(files) => serde_json::json!({"files": files}),
                    Err(error) => serde_json::json!({"error": error.to_string()}),
                };
                serde_json::json!({"descriptor": entry.descriptor, "observed": observed})
            })
            .collect::<Vec<_>>();
        std::fs::write(
            directory.join("input-artifacts.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "authority": false, "artifact_bytes": capture.bytes_written(), "inputs": inputs
            }))?,
        )
    }

    pub(crate) fn with_source_search_context(mut self, include: &[PathBuf]) -> Self {
        self.source_search_include = Some(Arc::from(include));
        self
    }

    pub(crate) fn apply_to(
        &self,
        command: &mut tidepool_extract_cmd::ExtractCmd,
        retained_policy: RetainedGenerationPolicy,
    ) -> Result<(), CompileError> {
        let request = tidepool_extract_cmd::ExtractRequest::decode(&command.request_bytes())
            .map_err(failure)?;
        let retained = retained_generation_inputs(
            retained_policy,
            request.retained_generations(),
            self.groups.iter().flat_map(PendingCertifiedGroup::imports),
        )?;
        let retained = match retained_policy {
            RetainedGenerationPolicy::PreserveCertifiedDemand => self
                .inputs
                .compiler
                .native_transport
                .retained_generation_tags(retained)?,
            RetainedGenerationPolicy::PureActivationPreview => retained,
        };
        // These authenticated tags keep original native demands intact through
        // projection. They grant neither lexical imports nor live heap roots.
        for (identity, generation) in retained {
            command.retained_generation(identity, generation);
        }
        command.session_artifacts(&self.manifest);
        Ok(())
    }

    pub(crate) fn with_checked_value_imports(
        mut self,
        authority: crate::checked_cell::CheckedValueImportAuthority,
    ) -> Self {
        self.checked_value_imports = authority;
        self
    }
    pub(crate) fn with_initial_template_interfaces(
        mut self,
        initial: Arc<ExactDeclarationContext>,
        templates: &[String],
    ) -> Result<Self, CompileError> {
        let selected = initial.selected_template_imports(templates)?;
        if selected.roots.is_empty() {
            return Ok(self);
        }
        if initial.owner.facts.producer != self.producer_sha256 {
            return Err(failure("checked template interface producer differs"));
        }
        let entries = self.context().artifact_view().entries();
        for (owner, node) in &selected.graph {
            if !entries.iter().any(|entry| {
                entry.descriptor.owner == *owner
                    && entry.descriptor.interface_sha256 == node.interface_sha256
            }) || self
                .context()
                .lexical_graph()
                .iter()
                .any(|current| current.owner == *owner && current.imports != node.imports)
            {
                return Err(failure(
                    "checked template interface differs from its initial selection",
                ));
            }
            if initial
                .owner
                .facts
                .template_imports
                .as_ref()
                .and_then(|retained| retained.graph.get(owner))
                .is_some_and(|retained| {
                    !entries
                        .iter()
                        .any(|entry| entry.descriptor.id == retained.canonical)
                })
            {
                return Err(failure(
                    "checked template canonical original differs from its retained selection",
                ));
            }
        }
        self.generated_scaffold_imports
            .push(GeneratedScaffoldImportAuthority {
                role: GeneratedScaffoldRole::InitialTemplateInterfaces {
                    producer: initial.owner.facts.producer,
                    roots: selected.roots,
                    graph: Arc::new(selected.graph),
                },
                protected_templates: templates.to_vec().into(),
            });
        Ok(self)
    }

    pub(crate) fn in_program_context(
        &self,
        root: &Path,
        context: Arc<ExactDeclarationContext>,
    ) -> Result<Self, CompileError> {
        self.in_program_context_with_inputs(
            root,
            context,
            self.inputs.private_compiler_input.clone(),
        )
    }

    #[tracing::instrument(
        target = "exomonad_harness::timing",
        name = "exact.program_context",
        level = "debug",
        skip_all,
        fields(inclusive = true)
    )]
    fn in_program_context_with_inputs(
        &self,
        root: &Path,
        context: Arc<ExactDeclarationContext>,
        private: Option<OriginalCompilerInputs>,
    ) -> Result<Self, CompileError> {
        let preparation_span = tracing::debug_span!(target: "exomonad_harness::timing", "exact.program_context.prepare", inclusive = true).entered();
        if context.toolchain_identity_sha256() != self.producer_sha256
            && !(context.toolchain_identity_sha256() == [0; 32]
                && context.artifact_view().is_empty())
        {
            return Err(failure("program context has another producer"));
        }
        // Inventory custody grows by immutable artifact ID, but materialized
        // inputs follow its selected owner projection. An original product can
        // become available for an already retained canonical interface.
        let baseline = &self.compiler_inputs().metadata;
        let current_inputs = self
            .inputs
            .protected_scaffold
            .compiler_inputs(&context, private.as_ref())?;
        let current = &current_inputs.metadata;
        if current
            .entries
            .values()
            .any(|entry| entry.descriptor.producer_sha256 != self.producer_sha256)
        {
            return Err(failure("private compiler input has another producer"));
        }
        if self
            .context()
            .owner
            .facts
            .compiler_support
            .projection()
            .merge(&context.owner.facts.compiler_support.projection())?
            != *context.owner.facts.compiler_support.projection()
        {
            return Err(failure("program context removed an issued compiler role"));
        }

        let baseline_ids = baseline
            .retained
            .artifacts
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        let current_ids = current
            .retained
            .artifacts
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        if !baseline_ids.is_subset(&current_ids) {
            return Err(failure("program context removed an admitted artifact"));
        }
        if !baseline
            .retained
            .selected_native_groups
            .is_subset(&current.retained.selected_native_groups)
        {
            return Err(failure("program context removed an admitted native group"));
        }
        let new_entries = current
            .entries
            .values()
            .filter(|entry| {
                baseline
                    .entries
                    .get(&entry.descriptor.owner)
                    .is_none_or(|previous| previous.descriptor.id != entry.descriptor.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let replaced_owners = new_entries
            .iter()
            .filter(|entry| baseline.entries.contains_key(&entry.descriptor.owner))
            .map(|entry| entry.descriptor.owner.clone())
            .collect::<BTreeSet<_>>();
        if !replaced_owners.is_empty() {
            // Displaced paths were consumed by earlier segments. Validate them
            // before replacing their rows so promotion cannot hide tampering.
            self.context()
                .validate_artifacts_from_metadata(&self.artifacts, baseline)?;
        }
        let delta_bytes = materialization_bytes(&new_entries);
        if !new_entries.is_empty() {
            std::fs::create_dir_all(root)?;
        }
        drop(preparation_span);
        let delta_start = std::time::Instant::now();
        let mut validation = PackageInterfaceValidation::default();
        let mut materializer = (*context).clone();
        materializer = materializer.admit_producer(self.producer_sha256)?;
        let (mut materialized, _) = materializer.materialize_entries_with_validation(
            root,
            &new_entries,
            &mut validation,
            MaterializationMode::Scratch,
        )?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.program_delta_materialize",
            delta_start.elapsed(),
            delta_bytes,
        );
        materialized.artifacts.extend(
            self.artifacts
                .iter()
                .filter(|artifact| {
                    !replaced_owners.contains(&identity(
                        &artifact.interface.unit,
                        &artifact.interface.module,
                    ))
                })
                .cloned(),
        );
        let certify_start = std::time::Instant::now();
        let current_entries = current
            .retained
            .artifacts
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let available = original_products_by_id(&current_entries);
        let additional = certify_selected_owned_products_in_context_with_validation(
            &available,
            &self.groups,
            &current.retained.selected_native_groups,
            &mut validation,
        )
        .map_err(failure)?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.program_delta_certify",
            certify_start.elapsed(),
            delta_bytes,
        );
        let groups = if additional.is_empty() {
            Arc::clone(&self.groups)
        } else {
            let mut groups = self.groups.to_vec();
            groups.extend(additional);
            groups.into()
        };
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.program_delta",
            delta_start.elapsed(),
            delta_bytes,
        );
        Ok(Self {
            inputs: ExactRequestInputs::issue(
                context,
                self.inputs.protected_scaffold.clone(),
                private,
                current_inputs,
            ),
            manifest: self.manifest.clone(),
            request_sha256: self.request_sha256.clone(),
            semantic_sha256: self.semantic_sha256,
            producer_sha256: self.producer_sha256,
            artifacts: materialized.artifacts,
            groups: groups.into(),
            materialization: self.materialization.clone(),
            program_support: self.program_support.clone(),
            program_source_lexical: self.program_source_lexical.clone(),
            source_selected_support: self.source_selected_support.clone(),
            source_search_include: self.source_search_include.clone(),
            checked_value_imports: self.checked_value_imports.clone(),
            generated_scaffold_imports: self.generated_scaffold_imports.clone(),
        })
    }

    pub(crate) fn admit_program_segment(
        &self,
        root: &Path,
    ) -> Result<ExactProgramSegmentAdmission, CompileError> {
        Ok(ExactProgramSegmentAdmission {
            request: self.clone(),
            admissions: self.validate_outputs(root)?,
            original_products: std::sync::OnceLock::new(),
        })
    }
    pub(crate) fn admit_program_segment_support_with_selection(
        &mut self,
        context: Arc<ExactDeclarationContext>,
        support: &ArtifactView,
        segment: &ExactProgramSegmentAdmission,
        produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
        selection: &crate::certified_products::CertifiedSourceSelection,
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        segment.validate_request(self)?;
        self.admit_program_support_inner(
            context,
            support,
            segment.admissions(),
            produced_types,
            Some(segment.request.context()),
            selection,
        )
    }
    pub(crate) fn admit_program_support_with_selection(
        &mut self,
        context: Arc<ExactDeclarationContext>,
        support: &ArtifactView,
        admissions: &[ExactSourceAdmission],
        produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
        selection: &crate::certified_products::CertifiedSourceSelection,
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        self.admit_program_support_inner(
            context,
            support,
            admissions,
            produced_types,
            None,
            selection,
        )
    }
    fn admit_program_support_inner(
        &mut self,
        context: Arc<ExactDeclarationContext>,
        support: &ArtifactView,
        admissions: &[ExactSourceAdmission],
        produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
        segment_input: Option<&ExactDeclarationContext>,
        selection: &crate::certified_products::CertifiedSourceSelection,
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        let mut imports = BTreeMap::new();
        let mut selected_originals = BTreeMap::new();
        for admission in admissions {
            for (owner, original) in &admission.selected_originals {
                if selected_originals
                    .insert(owner.clone(), original.clone())
                    .is_some_and(
                        |previous: crate::execution_source::SourceSelectedOriginal| {
                            previous.interface() != original.interface()
                        },
                    )
                {
                    return Err(failure(
                        "program support changed a source-selected original",
                    ));
                }
            }
            for (owner, requirements) in admission.home_imports()? {
                if imports
                    .insert(owner, requirements.clone())
                    .is_some_and(|old| old != requirements)
                {
                    return Err(failure(
                        "program support original changed selected home imports",
                    ));
                }
            }
        }
        let issued = selection
            .compiler_support_projection(support, admissions)
            .map_err(compiler_evidence_failure)?;
        let supplied = issued
            .project_metadata(support.metadata_snapshot())?
            .entries;
        let retained = context.compiler_metadata_snapshot()?.entries;
        let source_input = segment_input.unwrap_or(&context);
        let retained_source_input = source_input.compiler_metadata_snapshot()?.entries;
        let fresh = supplied
            .keys()
            .filter(|owner| {
                imports.contains_key(*owner) && !selected_originals.contains_key(*owner)
            })
            .cloned()
            .collect::<Vec<_>>();
        for (owner, entry) in &supplied {
            // Reserved same-request output certificates retain type closure.
            // They do not supply source admissions or become lexical roots.
            if produced_types.is_some_and(|outputs| outputs.matches_artifact(entry)) {
                if imports.contains_key(owner) || selected_originals.contains_key(owner) {
                    return Err(failure("produced type output became source support"));
                }
                continue;
            }
            if imports.contains_key(owner) {
                if let Some(selected) = selected_originals.get(owner) {
                    let Some(previous) = retained.get(owner) else {
                        return Err(failure(
                            "source-selected support is not an existing original",
                        ));
                    };
                    if canonical_source_interface(previous) != Some(selected.interface())
                        || canonical_source_interface(entry) != Some(selected.interface())
                    {
                        return Err(failure(
                            "source-selected support has another original owner",
                        ));
                    }
                } else if retained_source_input
                    .get(owner)
                    .is_some_and(|entry| matches!(&entry.payload, ArtifactPayload::Original(_)))
                {
                    return Err(failure(
                        "program support cannot select a retained hidden owner",
                    ));
                } else if retained.get(owner).is_some_and(|previous| {
                    canonical_source_interface(previous) != canonical_source_interface(entry)
                }) {
                    return Err(failure(
                        "projected segment source changed its admitted canonical owner",
                    ));
                } else if canonical_source_interface(entry).is_none() {
                    return Err(failure(
                        "fresh program support lacks a canonical source seal",
                    ));
                }
            } else if canonical_source_interface(entry).is_none()
                && retained
                    .get(owner)
                    .is_none_or(|previous| previous.descriptor.id != entry.descriptor.id)
            {
                return Err(failure(
                    "program support changed an inherited synthetic interface",
                ));
            }
        }
        let extend_start = std::time::Instant::now();
        let mut context = (*context).clone();
        context = context.admit_producer(self.producer_sha256)?;
        for descriptor in support.descriptors() {
            context = context.admit_producer(descriptor.producer_sha256)?;
        }
        let updated = context
            .owner
            .facts
            .compiler_support
            .extend_program_source(selection, support, admissions)?;
        context = context.rebuild(|facts| facts.compiler_support = updated);
        let context = Arc::new(context.finish()?);
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.program_support_extend",
            extend_start.elapsed(),
            0,
        );
        if fresh.is_empty() && selected_originals.is_empty() {
            return Ok(context);
        }
        let mut inherited = context
            .lexical_graph()
            .iter()
            .filter(|node| !node.owner.module.starts_with("Tidepool.Session."))
            .map(|node| (node.owner.clone(), node.imports.clone()))
            .collect::<BTreeMap<_, _>>();
        for node in &self.program_source_lexical {
            if inherited
                .insert(node.owner.clone(), node.imports.clone())
                .is_some_and(|previous| previous != node.imports)
            {
                return Err(failure("program support changed its selected source graph"));
            }
        }
        let roots = fresh
            .iter()
            .cloned()
            // Selected originals may be source roots without any fresh product;
            // their adjacency comes from the validated current-source receipt.
            .chain(selected_originals.keys().cloned())
            .collect::<Vec<_>>();
        let implementations = context.artifact_view().source_implementation_roles();
        let program_source_lexical = crate::declaration_join::source_lexical_surface(
            &roots,
            &imports,
            &inherited
                .into_iter()
                .map(|(owner, imports)| ExactLexicalNode { owner, imports })
                .collect::<Vec<_>>(),
            &implementations,
        )?
        .lexical;
        let selected = context.compiler_metadata_snapshot()?.entries;
        let entries = fresh
            .iter()
            .chain(selected_originals.keys())
            .filter_map(|owner| {
                selected
                    .get(owner)
                    .map(|entry| (owner.clone(), entry.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        for (owner, selected) in &selected_originals {
            let Some(entry) = entries.get(owner) else {
                return Err(failure(
                    "source-selected support lacks its original inventory entry",
                ));
            };
            if canonical_source_interface(entry) != Some(selected.interface()) {
                return Err(failure(
                    "source-selected support cannot replace an original",
                ));
            }
        }
        let fresh = context
            .artifact_view()
            .select_roots(entries.values().map(|entry| entry.descriptor.id).collect())?;
        let fresh_owners = fresh
            .descriptors()
            .into_iter()
            .map(|entry| entry.owner)
            .collect::<BTreeSet<_>>();
        let program_support = ProgramSourceSupport::extend(
            self.program_support.as_ref(),
            context.owner.facts.compiler_support.select_support_roots(
                entries.values().map(|entry| entry.descriptor.id).collect(),
            )?,
            imports
                .into_iter()
                .filter(|(owner, _)| fresh_owners.contains(owner)),
        )?;
        self.program_source_lexical = program_source_lexical;
        self.program_support = Some(program_support);
        self.source_selected_support
            .extend(selected_originals.into_keys());
        Ok(context)
    }

    pub(crate) fn program_source_lexical(&self) -> &[ExactLexicalNode] {
        &self.program_source_lexical
    }

    pub(crate) fn admit_source(
        &self,
        source_path: &Path,
        source: &str,
        fresh_evidence: &[u8],
    ) -> Result<ExactSourceAdmission, CompileError> {
        let mut validation = PackageInterfaceValidation::default();
        let admitted =
            self.admit_source_receipt_with_validation(source_path, source, &mut validation)?;
        admitted.match_product(
            self,
            source_path,
            source,
            fresh_evidence,
            &validation.inventory,
        )?;
        Ok(admitted)
    }

    pub(crate) fn admit_source_receipt_with_validation(
        &self,
        source_path: &Path,
        source: &str,
        validation: &mut PackageInterfaceValidation,
    ) -> Result<ExactSourceAdmission, CompileError> {
        use sha2::Digest;
        let expected: [u8; 32] = sha2::Sha256::digest(source.as_bytes()).into();
        let admitted = self
            .validate_outputs_selected_with_validation(
                source_path
                    .parent()
                    .ok_or_else(|| failure("source has no directory"))?,
                None,
                self.context(),
                validation,
            )?
            .into_iter()
            .find(|admitted| {
                admitted.witness.source_path() == source_path
                    && admitted.witness.source_sha256() == &expected
            })
            .ok_or_else(|| {
                failure("source and final product evidence lack their exact consumed receipt")
            })?;
        Ok(admitted)
    }
    pub(crate) fn validate_outputs(
        &self,
        root: &Path,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        self.validate_outputs_with_planned(root, None)
    }

    pub(crate) fn validate_outputs_with_planned(
        &self,
        root: &Path,
        planned: Option<&crate::checked_cell::PlannedCheckedDeclaration>,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        let Some(planned) = planned else {
            return self.validate_outputs_selected(root, None, self.context());
        };
        let mut request = self.in_program_context_with_private_input(
            &root.join("private-compiler-inputs"),
            self.context().clone(),
            &planned.compiler_input,
        )?;
        let planned = &planned.certificate;
        if planned.toolchain_identity_sha256() != self.producer_sha256 {
            return Err(failure("planned source support has another producer"));
        }
        // The worker checks the remaining cell against these same-request
        // fresh originals. Retained hidden dependencies are not selected roots.
        let view = planned.artifact_view();
        let retained = self.context().compiler_metadata_snapshot()?.entries;
        let selected = planned
            .compiler_input_projection()
            .project_metadata(view.metadata_snapshot())?
            .entries;
        let entries = planned
            .original_home_imports()
            .filter(|(owner, _)| !retained.contains_key(*owner))
            .filter_map(|(owner, _)| {
                selected
                    .get(owner)
                    .map(|entry| (owner.clone(), entry.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        request.program_support = Some(ProgramSourceSupport::extend(
            self.program_support.as_ref(),
            planned.compiler_source_support().select_support_roots(
                entries.values().map(|entry| entry.descriptor.id).collect(),
            )?,
            planned
                .original_home_imports()
                .map(|(owner, imports)| (owner.clone(), imports.to_vec()))
                .chain(
                    planned
                        .source_lexical_imports()
                        .iter()
                        .map(|node| (node.owner.clone(), node.imports.clone())),
                ),
        )?);
        let owner = identity(
            &planned.product().owner().unit,
            &planned.product().owner().module,
        );
        request.validate_outputs_selected(root, Some(&owner), self.context())
    }

    fn validate_outputs_selected(
        &self,
        root: &Path,
        planned: Option<&ExactModuleIdentity>,
        context: &ExactDeclarationContext,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        self.validate_outputs_selected_with_validation(
            root,
            planned,
            context,
            &mut PackageInterfaceValidation::default(),
        )
    }
    fn validate_outputs_selected_with_validation(
        &self,
        root: &Path,
        planned: Option<&ExactModuleIdentity>,
        context: &ExactDeclarationContext,
        validation: &mut PackageInterfaceValidation,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        let context_validate_start = std::time::Instant::now();
        self.validate_artifacts()?;
        self.checked_value_imports.validate()?;
        if sha256(
            &crate::certified_products::read_bounded_with_operation(
                &self.manifest,
                EXACT_SCOPE_BYTES_LIMIT as u64,
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?,
        ) != self.request_sha256
        {
            return Err(failure("scope request changed during compilation"));
        }
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.context_validate",
            context_validate_start.elapsed(),
            0,
        );
        let receipt_validate_start = std::time::Instant::now();
        let directory = root.join(".exact-compilations");
        let mut receipts = std::fs::read_dir(&directory)
            .map_err(|error| {
                failure(format!(
                    "exact compile receipts {}: {error}",
                    directory.display()
                ))
            })?
            .map(|entry| {
                validation
                    .inventory
                    .reserve::<PathBuf>(1)
                    .map_err(|error| compiler_evidence_failure(error.into()))?;
                let path = entry?.path();
                validation
                    .inventory
                    .charge(path.as_os_str().len())
                    .map_err(|error| compiler_evidence_failure(error.into()))?;
                Ok::<_, CompileError>(path)
            })
            .collect::<Result<Vec<_>, _>>()?;
        receipts.sort();
        if receipts.is_empty() || receipts.len() > 4096 {
            return Err(failure("missing or excessive successful compile receipts"));
        }
        let admitted = receipts
            .iter()
            .map(|path| {
                self.validate_receipt_with_validation(
                    &path.join("receipt.cbor"),
                    planned,
                    context,
                    validation,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.receipt_validate",
            receipt_validate_start.elapsed(),
            0,
        );
        Ok(admitted)
    }

    #[cfg(test)]
    fn validate_receipt(
        &self,
        path: &Path,
        planned: Option<&ExactModuleIdentity>,
        context: &ExactDeclarationContext,
    ) -> Result<ExactSourceAdmission, CompileError> {
        self.validate_receipt_with_validation(
            path,
            planned,
            context,
            &mut PackageInterfaceValidation::default(),
        )
    }

    fn validate_receipt_with_validation(
        &self,
        path: &Path,
        planned: Option<&ExactModuleIdentity>,
        context: &ExactDeclarationContext,
        validation: &mut PackageInterfaceValidation,
    ) -> Result<ExactSourceAdmission, CompileError> {
        let DecodedExactCompilationReceipt {
            source_path,
            source_sha256,
            source,
            evidence_bytes,
            evidence,
            edges,
            claims,
            selection_evidence,
        } = decode_exact_compilation_receipt_with_operation(
            path,
            Some((&self.request_sha256, self.semantic_sha256)),
            &validation.inventory,
        )?;
        let evidence = crate::cache::CompletedSourceEvidence::from_worker_evidence(
            evidence,
            &source_path,
            &source,
        )
        .map_err(|failure| {
            CompileError::CompilerEvidence(Box::new(
                crate::certified_products::CertificationError::CompletedSourceEvidence {
                    input: source_path.clone(),
                    failure: Box::new(failure),
                },
            ))
        })?;
        // Retained proof custody may contain interfaces that this request must
        // compile freshly. Only selected compiler roles are admitted owners.
        let admitted_compiler_owners = context.compiler_metadata_snapshot()?.entries;
        let mut exact_owners: BTreeSet<_> = admitted_compiler_owners
            .keys()
            .map(|owner| (owner.unit.as_str(), owner.module.as_str()))
            .collect();
        if let Some(planned) = planned {
            exact_owners.insert((planned.unit.as_str(), planned.module.as_str()));
        }
        exact_owners.extend(self.checked_value_imports.owners());
        let source_owners: BTreeSet<_> = evidence
            .modules
            .iter()
            .map(|module| (module.unit.as_str(), module.module.as_str(), module.boot))
            .collect();
        if source_owners
            .iter()
            .any(|(unit, module, _)| exact_owners.contains(&(*unit, *module)))
        {
            return Err(failure("fresh module replaced an admitted exact owner"));
        }
        let mut selected: BTreeSet<_> = context
            .owner
            .facts
            .lexical
            .iter()
            .map(|node| (node.owner.unit.as_str(), node.owner.module.as_str()))
            .collect();
        let support_entries = self
            .program_support
            .as_ref()
            .map(|support| support.compiler_support.artifacts().root_entries())
            .unwrap_or_default();
        selected.extend(
            support_entries
                .iter()
                .filter(|entry| {
                    !self
                        .source_selected_support
                        .contains(&entry.descriptor.owner)
                })
                .map(|entry| {
                    (
                        entry.descriptor.owner.unit.as_str(),
                        entry.descriptor.owner.module.as_str(),
                    )
                }),
        );
        selected.extend(self.checked_value_imports.owners());
        if let Some(planned) = planned {
            selected.insert((planned.unit.as_str(), planned.module.as_str()));
        }
        let source_selection_roots = edges
            .iter()
            .flat_map(|module| {
                module.imports.iter().filter(|edge| !edge.boot).map(|edge| {
                    (
                        module.owner.clone(),
                        edge.qualifier.clone(),
                        identity(&edge.unit, &edge.module),
                    )
                })
            })
            .collect::<Vec<_>>();
        let selected_originals = if claims.is_empty() {
            BTreeMap::new()
        } else {
            let include = self.source_search_include.as_deref().ok_or_else(|| {
                failure("source-selected originals lack trusted current import roots")
            })?;
            let selection_evidence = selection_evidence
                .as_ref()
                .ok_or_else(|| failure("selected originals lack source evidence"))?;
            // Earlier admitted program support carries the same original proof
            // as persisted context. This receipt cannot authorize itself.
            let source_support = match &self.program_support {
                Some(support) => context
                    .compiler_source_support()
                    .merge(&support.compiler_support)?,
                None => context.compiler_source_support().clone(),
            };
            let selected_interfaces = source_support.selected_interfaces()?;
            let entries = selected_interfaces.values().cloned().collect::<Vec<_>>();
            let canonical = entries
                .iter()
                .filter_map(|entry| canonical_source_interface(entry))
                .map(|interface| {
                    (
                        identity(interface.unit(), interface.module()),
                        interface.clone(),
                    )
                })
                .collect::<BTreeMap<_, _>>()
                .into_values()
                .collect::<Vec<_>>();
            let exact_interfaces = entries
                .iter()
                .map(|entry| {
                    (
                        entry.descriptor.owner.clone(),
                        entry.descriptor.interface_sha256,
                    )
                })
                .collect();
            let independent = selected
                .iter()
                .map(|(unit, module)| identity(unit, module))
                .collect();
            let source_less = entries
                .iter()
                .filter(|entry| match &entry.payload {
                    ArtifactPayload::Interface(..) => true,
                    ArtifactPayload::Canonical(interface) => matches!(
                        interface.origin(),
                        crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. }
                    ),
                    ArtifactPayload::Original(product) => product.module_interface().is_some_and(
                        |interface| matches!(
                            interface.origin(),
                            crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. }
                        ),
                    ),
                })
                .map(|entry| entry.descriptor.owner.clone())
                .collect();
            crate::execution_source::validate_source_selected_originals_with_validation(
                claims,
                selection_evidence,
                crate::execution_source::SourceSelectionContext {
                    producer: self.producer_sha256,
                    interfaces: &canonical,
                    exact_interfaces: &exact_interfaces,
                    include,
                    fresh: &evidence,
                    roots: &source_selection_roots,
                    independent: &independent,
                    source_less: &source_less,
                },
                validation,
            )?
        };
        let mut seen = BTreeSet::new();
        let mut exact_imports = BTreeMap::new();
        let mut exact_source_imports = BTreeMap::new();
        let mut scaffold_roots: BTreeMap<usize, BTreeSet<ExactModuleIdentity>> = BTreeMap::new();
        for module in &edges {
            let owner = (
                module.owner.unit.as_str(),
                module.owner.module.as_str(),
                module.boot,
            );
            if !source_owners.contains(&owner) || !seen.insert(owner) {
                return Err(failure(
                    "exact import witness has another fresh source owner",
                ));
            }
            let mut imported = BTreeSet::new();
            let mut resolved = BTreeSet::new();
            let mut source_imports = Vec::new();
            for edge in &module.imports {
                let qualifier = String::from(edge.qualifier.clone());
                let name = edge.module.as_str();
                let boot = edge.boot;
                let unit = edge.unit.as_str();
                let mut scaffold_import = false;
                for (index, authority) in self.generated_scaffold_imports.iter().enumerate() {
                    let permitted = evidence
                        .modules
                        .iter()
                        .find(|module| {
                            module.unit == owner.0
                                && module.module == owner.1
                                && module.boot == owner.2
                        })
                        .is_some_and(|module| {
                            // Completed-source validation bound this marker
                            // to the hash-verified receipt target snapshot.
                            let module_source =
                                if module.source == Path::new(crate::cache::GENERATED_SOURCE) {
                                    &source_path
                                } else {
                                    &module.source
                                };
                            authority.permits(
                                context,
                                &source,
                                &source_path,
                                module_source,
                                unit,
                                name,
                                &qualifier,
                                boot,
                            )
                        });
                    if permitted {
                        scaffold_roots
                            .entry(index)
                            .or_default()
                            .insert(identity(unit, name));
                        scaffold_import = true;
                    }
                }
                if boot
                    || !(selected.contains(&(unit, name))
                        || scaffold_import
                        || selected_originals.contains_key(&identity(unit, name)))
                    || (qualifier != "none" && qualifier != format!("this:{unit}"))
                    || !imported.insert((qualifier.clone(), name.to_owned(), boot, unit.to_owned()))
                {
                    return Err(failure(format!(
                        "exact import witness leaves selected lexical graph: source {}:{}, import {unit}:{name}, qualifier {qualifier}, boot {boot}, selected {}",
                        owner.0,
                        owner.1,
                        selected.contains(&(unit, name)),
                    )));
                }
                resolved.insert(identity(unit, name));
                source_imports.push(crate::certified_products::CanonicalSourceImport {
                    qualifier: edge.qualifier.clone(),
                    module: name.to_owned(),
                    boot,
                    home_unit: Some(unit.to_owned()),
                });
            }
            exact_source_imports.insert(identity(owner.0, owner.1), source_imports);
            exact_imports.insert(identity(owner.0, owner.1), resolved.into_iter().collect());
        }
        if seen != source_owners {
            return Err(failure("exact import witness omits a fresh source module"));
        }
        validation
            .inventory
            .charge(self.request_sha256.len())
            .map_err(|error| compiler_evidence_failure(error.into()))?;
        Ok(ExactSourceAdmission {
            request_identity: ExactRequestIdentity::from_request(self).into_owned(),
            witness: ExactSourceWitness {
                source_path,
                source_sha256,
            },
            evidence: Arc::new(evidence),
            evidence_bytes,
            exact_imports,
            scaffold_roots,
            exact_source_imports,
            selected_originals,
        })
    }
}

/// Decoded wire facts. Admission separately authenticates the request and selected owners.
pub(crate) struct DecodedExactCompilationReceipt {
    pub(crate) source_path: PathBuf,
    source_sha256: [u8; 32],
    source: String,
    evidence_bytes: Vec<u8>,
    pub(crate) evidence: crate::cache::DependencyEvidence,
    edges: Vec<ExactReceiptModule>,
    pub(crate) claims: Vec<crate::execution_source::SourceSelectedOriginalClaim>,
    selection_evidence: Option<crate::cache::DependencyEvidence>,
}
struct ExactReceiptModule {
    owner: ExactModuleIdentity,
    boot: bool,
    imports: Vec<ExactReceiptImport>,
}
struct ExactReceiptImport {
    qualifier: crate::cache::ImportQualifier,
    module: String,
    boot: bool,
    unit: String,
}

#[cfg(test)]
pub(crate) fn read_exact_compilation_receipt(
    path: &Path,
) -> Result<DecodedExactCompilationReceipt, CompileError> {
    decode_exact_compilation_receipt(path, None)
}

fn decode_exact_compilation_receipt(
    path: &Path,
    expected: Option<(&str, [u8; 32])>,
) -> Result<DecodedExactCompilationReceipt, CompileError> {
    decode_exact_compilation_receipt_with_operation(
        path,
        expected,
        &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
    )
}
fn decode_exact_compilation_receipt_with_operation(
    path: &Path,
    expected: Option<(&str, [u8; 32])>,
    operation: &tidepool_repr::execution_schema::InventoryOperation,
) -> Result<DecodedExactCompilationReceipt, CompileError> {
    use sha2::Digest;
    // A receipt retains complete fresh and source-selected dependency
    // inventories, rather than the exact scope's bounded owner descriptors.
    let limit =
        crate::certified_products::COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_bytes);
    let bytes =
        crate::certified_products::read_bounded_with_operation(path, limit as u64, operation)
            .map_err(compiler_evidence_failure)?;
    let value = operation
        .decode_value(&bytes, limit)
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    operation
        .charge_value_copies(&value, 3)
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    let header = row(&value, 10)?;
    if string(&header[0])? != "TPEXACTCOMPILE" || string(&header[1])? != "3" {
        return Err(failure(
            "compile receipt belongs to another context or version",
        ));
    }
    let request_sha256 = string(&header[2])?.to_owned();
    crate::execution_source::parse_digest(&request_sha256)?;
    let semantic_sha256 = crate::execution_source::parse_digest(string(&header[3])?)?;
    if expected
        .is_some_and(|(request, semantic)| request_sha256 != request || semantic_sha256 != semantic)
    {
        return Err(failure(
            "compile receipt belongs to another context or version",
        ));
    }
    let source_path = PathBuf::from(string(&header[4])?);
    let snapshot = PathBuf::from(string(&header[6])?);
    if !source_path.is_absolute()
        || !snapshot.is_absolute()
        || snapshot
            != path
                .parent()
                .ok_or_else(|| failure("compile receipt has no owner directory"))?
                .join("source.hs")
    {
        return Err(failure("compile source snapshot has another owner"));
    }
    let source_bytes =
        crate::certified_products::read_bounded_with_operation(&snapshot, 32 << 20, operation)
            .map_err(compiler_evidence_failure)?;
    operation
        .charge(source_bytes.len())
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    let source_sha256: [u8; 32] = sha2::Sha256::digest(&source_bytes).into();
    if string(&header[5])? != hex(&source_sha256) {
        return Err(failure("compile source snapshot changed"));
    }
    let source = std::str::from_utf8(&source_bytes)
        .map_err(failure)?
        .to_owned();
    let evidence_bytes = string(&header[7])?.as_bytes().to_vec();
    operation
        .charge(
            evidence_bytes
                .len()
                .checked_mul(32)
                .ok_or_else(|| failure("source evidence accounting overflow"))?,
        )
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    let evidence: crate::cache::DependencyEvidence =
        serde_json::from_slice(&evidence_bytes).map_err(failure)?;
    if evidence.version != 4 {
        return Err(failure("compile receipt dependency evidence version"));
    }
    let edges = list(&header[8], 4096)?
        .iter()
        .map(|module| {
            let module = row(module, 4)?;
            let owner = identity(string(&module[0])?, string(&module[1])?);
            let boot = boolean(&module[2])?;
            let imports = list(&module[3], 4096)?
                .iter()
                .map(|edge| {
                    let edge = row(edge, 4)?;
                    Ok(ExactReceiptImport {
                        qualifier: crate::cache::ImportQualifier::try_from(
                            string(&edge[0])?.to_owned(),
                        )
                        .map_err(failure)?,
                        module: string(&edge[1])?.to_owned(),
                        boot: boolean(&edge[2])?,
                        unit: string(&edge[3])?.to_owned(),
                    })
                })
                .collect::<Result<Vec<_>, CompileError>>()?;
            Ok(ExactReceiptModule {
                owner,
                boot,
                imports,
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let source_selection = row(&header[9], 2)?;
    let claims = list(&source_selection[0], 4096)?
        .iter()
        .map(|claim| {
            let claim = row(claim, 5)?;
            Ok(crate::execution_source::SourceSelectedOriginalClaim {
                owner: identity(string(&claim[0])?, string(&claim[1])?),
                certificate_sha256: crate::execution_source::parse_digest(string(&claim[2])?)?,
                interface_sha256: crate::execution_source::parse_digest(string(&claim[3])?)?,
                source_sha256: crate::execution_source::parse_digest(string(&claim[4])?)?,
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let selection_evidence = if claims.is_empty() {
        if source_selection[1] != Value::Null {
            return Err(failure(
                "empty original selection has nonempty source evidence",
            ));
        }
        None
    } else {
        let json = string(&source_selection[1])?;
        operation
            .charge(
                json.len()
                    .checked_mul(32)
                    .ok_or_else(|| failure("selection evidence accounting overflow"))?,
            )
            .map_err(|error| compiler_evidence_failure(error.into()))?;
        Some(serde_json::from_str(json).map_err(failure)?)
    };
    Ok(DecodedExactCompilationReceipt {
        source_path,
        source_sha256,
        source,
        evidence_bytes,
        evidence,
        edges,
        claims,
        selection_evidence,
    })
}

fn bounded_read(path: &Path, limit: u64) -> Result<Vec<u8>, CompileError> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| failure(format!("exact artifact {}: {error}", path.display())))?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(failure("exact artifact exceeds byte bound"));
    }
    Ok(bytes)
}

fn retained_generation_inputs<'a>(
    policy: RetainedGenerationPolicy,
    retained: BTreeMap<tidepool_extract_cmd::SymbolIdentity, u64>,
    imports: impl Iterator<Item = &'a crate::certified_products::PendingImportOwner>,
) -> Result<BTreeMap<tidepool_extract_cmd::SymbolIdentity, u64>, CompileError> {
    match policy {
        RetainedGenerationPolicy::PreserveCertifiedDemand => {
            certified_retained_generation_tags(retained, imports)
        }
        RetainedGenerationPolicy::PureActivationPreview => {
            if !retained.is_empty() {
                return Err(failure(
                    "pure activation preview cannot carry retained heap generations",
                ));
            }
            Ok(BTreeMap::new())
        }
    }
}

fn certified_retained_generation_tags<'a>(
    mut retained: BTreeMap<tidepool_extract_cmd::SymbolIdentity, u64>,
    imports: impl Iterator<Item = &'a crate::certified_products::PendingImportOwner>,
) -> Result<BTreeMap<tidepool_extract_cmd::SymbolIdentity, u64>, CompileError> {
    use crate::certified_products::PendingImportOwner;
    for import in imports {
        let (identity, generation) = match import {
            PendingImportOwner::Retained {
                identity,
                generation,
            } => (identity, generation),
            PendingImportOwner::RetainedPackage {
                binder, generation, ..
            } => (binder, generation),
            PendingImportOwner::Source { .. } | PendingImportOwner::Package { .. } => continue,
        };
        let identity = tidepool_extract_cmd::SymbolIdentity {
            unit: identity.unit.clone(),
            module: identity.module.clone(),
            namespace: identity.namespace.clone(),
            occurrence: identity.occurrence.clone(),
            record_parent: identity.record_parent.clone(),
        };
        if retained
            .insert(identity, *generation)
            .is_some_and(|old| old != *generation)
        {
            return Err(failure(
                "certified native demand has conflicting retained generations",
            ));
        }
    }
    Ok(retained)
}
fn row(value: &Value, length: usize) -> Result<&[Value], CompileError> {
    let values = list(value, length)?;
    if values.len() != length {
        return Err(failure("invalid exact compile row"));
    }
    Ok(values)
}
fn list(value: &Value, limit: usize) -> Result<&[Value], CompileError> {
    match value {
        Value::Array(values) if values.len() <= limit => Ok(values),
        _ => Err(failure("invalid exact compile inventory")),
    }
}
fn string(value: &Value) -> Result<&str, CompileError> {
    match value {
        Value::Text(value) => Ok(value),
        _ => Err(failure("invalid exact compile text")),
    }
}
fn boolean(value: &Value) -> Result<bool, CompileError> {
    match value {
        Value::Bool(value) => Ok(*value),
        _ => Err(failure("invalid exact compile boolean")),
    }
}

fn scope_interface_evidence(
    entry: &ArtifactEntry,
    root: &Path,
    validation: &mut PackageInterfaceValidation,
) -> Result<Value, CompileError> {
    if entry.descriptor.kind != entry.payload.artifact_kind() {
        return Err(failure("interface evidence kind differs from payload"));
    }
    let interface = match &entry.payload {
        ArtifactPayload::Canonical(interface) => interface,
        ArtifactPayload::Original(product) => product
            .module_interface()
            .ok_or_else(|| failure("native interface evidence is missing"))?,
        ArtifactPayload::Interface(_, role) => {
            let tag = match role {
                JoinedInterfaceRole::LexicalJoin => "join",
                JoinedInterfaceRole::ValueInterface => "value",
            };
            return Ok(Value::Array(vec![text(tag)]));
        }
    };
    if interface.unit() != entry.descriptor.owner.unit
        || interface.module() != entry.descriptor.owner.module
        || interface.producer_sha256() != entry.descriptor.producer_sha256
        || interface.interface_sha256() != entry.descriptor.interface_sha256
        || interface.package_imports_sha256() != entry.descriptor.package_imports_sha256
    {
        return Err(failure(
            "module evidence differs from its selected artifact",
        ));
    }
    let reference = recovery_artifacts::materialize_module_interface(
        root,
        interface,
        validation,
        MaterializationMode::Scratch,
    )
    .map_err(materialization_failure)?;
    Ok(Value::Array(vec![
        text(match interface.origin() {
            crate::certified_products::CanonicalOrigin::SourceOriginal { .. } => "module",
            crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. } => {
                "native-declaration"
            }
        }),
        path_value(&root.join(reference.certificate_path))?,
        text(hex(&reference.certificate_sha256)),
        reference
            .core
            .as_ref()
            .map(|core| path_value(&root.join(&core.path)))
            .transpose()?
            .unwrap_or(Value::Null),
        reference
            .core
            .as_ref()
            .map_or(Value::Null, |core| text(hex(&core.sha256))),
    ]))
}

fn failure(message: impl std::fmt::Display) -> CompileError {
    CompileError::ExtractFailed(format!("exact declaration context: {message}"))
}

fn materialization_failure(error: recovery_artifacts::RecoveryArtifactError) -> CompileError {
    match error {
        recovery_artifacts::RecoveryArtifactError::InputDecision { owner, reason } => {
            compiler_evidence_failure(
                crate::certified_products::CertificationError::InputDecision { owner, reason },
            )
        }
        recovery_artifacts::RecoveryArtifactError::Io(error)
        | recovery_artifacts::RecoveryArtifactError::Unreadable { error, .. }
            if error.kind() == std::io::ErrorKind::Interrupted =>
        {
            CompileError::Io(error)
        }
        error @ recovery_artifacts::RecoveryArtifactError::CompletedSourceEvidence { .. } => {
            compiler_evidence_failure(
                crate::certified_products::CertificationError::CapturedModulePayload(error),
            )
        }
        error => failure(error),
    }
}

fn compiler_evidence_failure(error: crate::certified_products::CertificationError) -> CompileError {
    CompileError::CompilerEvidence(Box::new(error))
}

fn identity(unit: &str, module: &str) -> ExactModuleIdentity {
    ExactModuleIdentity {
        unit: unit.to_owned(),
        module: module.to_owned(),
    }
}

enum RequestInputTransport {
    OwnedArena,
    #[cfg(test)]
    DurableFiles,
}

impl ExactDeclarationContext {
    fn from_facts(facts: DeclarationContextFacts) -> Self {
        Self {
            owner: Arc::new(DeclarationContextOwner {
                facts,
                observations: OnceLock::new(),
                #[cfg(test)]
                semantic_issuances: Default::default(),
                #[cfg(test)]
                display_issuances: Default::default(),
            }),
        }
    }

    /// Publish a fresh owner after editing facts; derived observations never
    /// survive a change to their authenticated inputs.
    fn rebuild(self, update: impl FnOnce(&mut DeclarationContextFacts)) -> Self {
        let mut facts = self.owner.facts.clone();
        update(&mut facts);
        Self::from_facts(facts)
    }

    pub fn new(
        authored: &[Arc<CertifiedAuthoredDeclaration>],
        joins: &[Arc<AcceptedJoin>],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        Self::from_facts(DeclarationContextFacts {
            compiler_support: crate::artifact_inventory::CompilerSourceSupport::empty(),

            producer: [0; 32],

            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,
        })
        .extend(authored, joins, lexical)
    }

    /// Admit original references with their embedded canonical proofs, plus
    /// separately supplied canonical type owners and joined interfaces.
    pub fn capture_recovery(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        Self::capture_recovery_with_value_interfaces(
            root,
            products,
            module_interfaces,
            joins,
            &[],
            lexical,
        )
    }

    pub fn capture_recovery_with_value_interfaces(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        Self::capture_recovery_inputs(
            root,
            products,
            module_interfaces,
            joins,
            values,
            None,
            lexical,
        )
    }
    pub fn capture_recovery_with_inventory(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        descriptors: &[crate::artifact_inventory::ArtifactDescriptor],
        dependencies: &[(
            crate::artifact_inventory::ArtifactId,
            crate::artifact_inventory::ArtifactId,
            crate::artifact_inventory::ArtifactDependency,
        )],
        native_groups: &[crate::artifact_inventory::NativeGroupKey],
        compiler_roles: &[CompilerInputRole],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        let inventory = RecoveredArtifactInventory::capture_inputs(
            root,
            products,
            module_interfaces,
            joins,
            values,
            Some((descriptors, dependencies)),
        )
        .map_err(failure)?;
        inventory.context_with_roles(
            &inventory.entries.keys().copied().collect::<Vec<_>>(),
            native_groups,
            compiler_roles,
            lexical,
        )
    }
    fn capture_recovery_inputs(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        inventory: Option<(
            &[crate::artifact_inventory::ArtifactDescriptor],
            &[(
                crate::artifact_inventory::ArtifactId,
                crate::artifact_inventory::ArtifactId,
                crate::artifact_inventory::ArtifactDependency,
            )],
        )>,
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        let inventory = RecoveredArtifactInventory::capture_inputs(
            root,
            products,
            module_interfaces,
            joins,
            values,
            inventory,
        )
        .map_err(failure)?;
        inventory.context_all_groups(
            &inventory.entries.keys().copied().collect::<Vec<_>>(),
            lexical,
        )
    }

    /// Merge immutable custody and issued source roles, replacing the selected
    /// lexical graph. One compiler namespace offers at most one original.
    pub fn extend(
        mut self,
        authored: &[Arc<CertifiedAuthoredDeclaration>],
        joins: &[Arc<AcceptedJoin>],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        let retained_publications = self.published_source_original_selections()?;
        let lexical = if retained_publications.is_empty() {
            lexical
        } else {
            compose_lexical_nodes(
                lexical.iter().chain(
                    retained_publications
                        .iter()
                        .flat_map(|selection| selection.context().owner.facts.lexical.iter()),
                ),
            )?
        };
        let selected_sources = lexical
            .iter()
            .map(|node| node.owner.clone())
            .collect::<BTreeSet<_>>();
        for certificate in authored {
            self = self.admit_producer(certificate.toolchain_identity_sha256())?;
            let incoming = certificate
                .compiler_source_support()
                .for_source_owners(&selected_sources)?;
            let updated = self.owner.facts.compiler_support.merge(&incoming)?;
            self = self.rebuild(|facts| facts.compiler_support = updated);
        }
        for join in joins {
            self = self.admit_producer(join.toolchain_identity_sha256())?;
            let incoming = join
                .compiler_source_support()
                .for_source_owners(&selected_sources)?;
            let updated = self.owner.facts.compiler_support.merge(&incoming)?;
            self = self.rebuild(|facts| facts.compiler_support = updated);
        }
        {
            let updated = self.owner.facts.lexical.replace(lexical)?;
            self = self.rebuild(|facts| facts.lexical = updated);
        }
        {
            self = self.rebuild(|facts| {
                facts.original_instance_environment = OriginalInstanceEnvironment::Unknown
            });
        }
        self.finish()
    }

    /// Add issued joins and their selected lexical nodes without duplicating
    /// an already retained owner. Equal import sets are idempotent; different
    /// selections for the same owner remain a conflict.
    pub fn extend_lexical_joins(
        self,
        joins: &[Arc<AcceptedJoin>],
        lexical: &[ExactLexicalNode],
    ) -> Result<Self, CompileError> {
        let lexical = compose_lexical_nodes(self.owner.facts.lexical.iter().chain(lexical))?;
        self.extend(&[], joins, lexical)
    }

    /// Admit the original supporting homes sealed by the same checked-cell
    /// transaction. Lexical exposure remains a separate caller-selected graph.
    #[cfg(test)]
    pub(crate) fn extend_checked_original_products(
        mut self,
        producer_sha256: [u8; 32],
        products: &[CertifiedRecoveryProduct],
    ) -> Result<Self, CompileError> {
        self = self.admit_producer(producer_sha256)?;
        let updated = self
            .owner
            .facts
            .compiler_support
            .admit_checked_products(producer_sha256, products)?;
        self = self.rebuild(|facts| facts.compiler_support = updated);
        self.finish()
    }

    pub fn toolchain_identity_sha256(&self) -> [u8; 32] {
        self.owner.facts.producer
    }
    pub fn artifact_view(&self) -> &ArtifactView {
        &self.owner.facts.compiler_support.artifacts()
    }
    pub(crate) fn compiler_original_products(
        &self,
    ) -> Result<Vec<CertifiedRecoveryProduct>, CompileError> {
        Ok(self
            .compiler_metadata_snapshot()?
            .entries
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some(product.clone()),
                _ => None,
            })
            .collect())
    }
    pub(crate) fn compiler_source_support(
        &self,
    ) -> &crate::artifact_inventory::CompilerSourceSupport {
        &self.owner.facts.compiler_support
    }
    pub(crate) fn retain_issued_native_offers(
        mut self,
        offers: impl IntoIterator<Item = crate::artifact_inventory::IssuedNativeOffer>,
    ) -> Result<Self, CompileError> {
        let updated = self.owner.facts.compiler_support.retain_offers(offers)?;
        self = self.rebuild(|facts| facts.compiler_support = updated);
        self.finish()
    }
    pub fn compiler_input_projection(&self) -> &CompilerInputProjection {
        &self.owner.facts.compiler_support.projection()
    }
    pub fn compiler_input_roles(&self) -> Vec<CompilerInputRole> {
        self.owner.facts.compiler_support.projection().roles()
    }
    #[cfg(test)]
    pub(crate) fn recover_fixture_projection(
        mut self,
        projection: CompilerInputProjection,
    ) -> Result<Self, CompileError> {
        let updated = recover_fixture_compiler_support(
            projection,
            self.artifact_view().clone(),
            self.compiler_source_support()
                .native_offers()
                .iter()
                .cloned(),
        )?;
        self = self.rebuild(|facts| facts.compiler_support = updated);
        self.finish()
    }
    pub(crate) fn compiler_metadata_snapshot(
        &self,
    ) -> Result<CompilerArtifactMetadata, CompileError> {
        self.owner
            .facts
            .compiler_support
            .projection()
            .project_metadata(
                self.owner
                    .facts
                    .compiler_support
                    .artifacts()
                    .metadata_snapshot(),
            )
    }
    /// Carry a compiler issuer's exact role selection without deriving roles
    /// from the broader artifact custody retained by its view.
    pub(crate) fn extend_issued_compiler_support(
        mut self,
        support: &crate::artifact_inventory::CompilerSourceSupport,
    ) -> Result<Self, CompileError> {
        let artifacts = support.artifacts();
        for descriptor in artifacts.descriptors() {
            self = self.admit_producer(descriptor.producer_sha256)?;
        }
        let support = self.owner.facts.compiler_support.merge(support)?;
        self = self.rebuild(|facts| facts.compiler_support = support);
        self.finish()
    }

    /// Retain the interface evidence admitted by an original compiler proof.
    /// Only compiler-owned issuers may supply the producer identity.
    pub(crate) fn original_interface_context(&self) -> Result<Self, CompileError> {
        let support = self
            .owner
            .facts
            .compiler_support
            .selected_interfaces()?
            .support_for_owners(
                self.compiler_input_projection()
                    .snapshot_roles()
                    .keys()
                    .cloned()
                    .collect(),
            )?;
        Ok(Self::from_facts(DeclarationContextFacts {
            compiler_support: support,
            producer: self.owner.facts.producer,
            lexical: Default::default(),
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,
        }))
    }

    #[cfg(test)]
    pub(crate) fn from_authenticated_interfaces(
        producer: [u8; 32],
        artifacts: &ArtifactView,
        projection: CompilerInputProjection,
    ) -> Result<Self, CompileError> {
        let mut context = Self::new(&[], &[], Vec::new())?;
        context = context.admit_producer(producer)?;
        let projection = projection.interface_only();
        projection.validate(artifacts)?;
        let interfaces = artifacts.select_roots(
            projection
                .roles()
                .iter()
                .map(CompilerInputRole::interface)
                .collect(),
        )?;
        for descriptor in interfaces.descriptors() {
            context = context.admit_producer(descriptor.producer_sha256)?;
        }
        let updated = recover_fixture_compiler_support(projection, interfaces, [])?;
        context = context.rebuild(|facts| facts.compiler_support = updated);
        context.finish()
    }

    /// Retain already authenticated original code owners and their original
    /// lexical graph. Only the original compiler output proof issues this.
    pub(crate) fn from_source_selection(
        producer: [u8; 32],
        selection: &crate::certified_products::CertifiedSourceSelection,
        artifacts: &ArtifactView,
        lexical: Vec<ExactLexicalNode>,
        target: ExactModuleIdentity,
        required_instance_owners: &[ExactModuleIdentity],
    ) -> Result<Self, CompileError> {
        let output = OriginalCompilerInputs::from_selection(selection, artifacts)?;
        let support =
            crate::artifact_inventory::CompilerSourceSupport::close_complete_output(&output)?;
        Self::from_authenticated_execution_support(
            producer,
            support,
            lexical,
            target,
            required_instance_owners,
        )
    }

    #[cfg(test)]
    pub(crate) fn from_authenticated_execution(
        producer: [u8; 32],
        artifacts: &ArtifactView,
        projection: CompilerInputProjection,
        lexical: Vec<ExactLexicalNode>,
        target: ExactModuleIdentity,
        required_instance_owners: &[ExactModuleIdentity],
    ) -> Result<Self, CompileError> {
        let support = issue_fixture_compiler_support(projection, artifacts.clone())?;
        Self::from_authenticated_execution_support(
            producer,
            support,
            lexical,
            target,
            required_instance_owners,
        )
    }

    fn from_authenticated_execution_support(
        producer: [u8; 32],
        support: crate::artifact_inventory::CompilerSourceSupport,
        lexical: Vec<ExactLexicalNode>,
        target: ExactModuleIdentity,
        required_instance_owners: &[ExactModuleIdentity],
    ) -> Result<Self, CompileError> {
        let mut context = Self::new(&[], &[], Vec::new())?;
        context = context.admit_producer(producer)?;
        for descriptor in support.artifacts().descriptors() {
            context = context.admit_producer(descriptor.producer_sha256)?;
        }
        context = context.rebuild(|facts| facts.compiler_support = support);
        context
            .owner
            .facts
            .compiler_support
            .projection()
            .validate(&context.owner.facts.compiler_support.artifacts())?;
        {
            let updated = context.owner.facts.lexical.replace(lexical)?;
            context = context.rebuild(|facts| facts.lexical = updated);
        }
        let selected_metadata = context.compiler_metadata_snapshot()?;
        let interfaces = selected_metadata
            .entries
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let selected = context
            .owner
            .facts
            .lexical
            .iter()
            .map(|row| &row.owner)
            .collect::<BTreeSet<_>>();
        let canonical_target = selected_metadata.entries.get(&target).is_some_and(|entry| {
            matches!(
                entry.payload,
                ArtifactPayload::Canonical(_) | ArtifactPayload::Original(_)
            )
        });
        let missing = required_instance_owners
            .iter()
            .chain(std::iter::once(&target))
            .filter(|owner| !interfaces.contains(*owner) || !selected.contains(owner))
            .cloned()
            .chain((!canonical_target).then_some(target.clone()))
            .chain(context.owner.facts.lexical.iter().flat_map(|node| {
                std::iter::once(&node.owner)
                    .chain(node.imports.iter())
                    .filter(|owner| !interfaces.contains(*owner) || !selected.contains(owner))
                    .cloned()
            }))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        // A consumed owner without a retained interface is explicit missing
        // evidence, not a selected lexical interface or a negative instance
        // result. Retain only the usable graph and commit the missing census.
        let mut lexical = context.owner.facts.lexical.to_vec();
        lexical.retain(|node| interfaces.contains(&node.owner));
        let retained = lexical
            .iter()
            .map(|node| node.owner.clone())
            .collect::<BTreeSet<_>>();
        for node in &mut lexical {
            node.imports.retain(|owner| retained.contains(owner));
        }
        {
            let updated = context.owner.facts.lexical.replace(lexical)?;
            context = context.rebuild(|facts| facts.lexical = updated);
        }
        {
            let updated = if !missing.is_empty() {
                OriginalInstanceEnvironment::MissingOriginalOwners(missing)
            } else {
                OriginalInstanceEnvironment::Complete { target }
            };
            context = context.rebuild(|facts| facts.original_instance_environment = updated);
        }
        context.finish()
    }

    pub(crate) fn original_instance_environment(&self) -> &OriginalInstanceEnvironment {
        &self.owner.facts.original_instance_environment
    }

    /// The original compiler target owns the orphan import census in its
    /// authenticated canonical interface. Retention alone does not grant it.
    pub(crate) fn original_instance_target(&self) -> Result<&ExactModuleIdentity, CompileError> {
        match &self.owner.facts.original_instance_environment {
            OriginalInstanceEnvironment::Complete { target } => Ok(target),
            _ => Err(failure(
                "activation preview lacks its original target interface",
            )),
        }
    }

    /// Select original type evidence through the issuing compiler roles. Names
    /// outside known home units remain obligations of the pinned compiler
    /// transaction; this projection grants no package or native authority.
    pub fn select_original_type_interfaces(
        &self,
        witnesses: &[&crate::checked_cell::CanonicalInputTypeWitness],
        signatures: &[&crate::checked_cell::ExactCheckedSignature],
    ) -> Result<Self, CompileError> {
        Self::type_interfaces_from_support(
            &self.owner.facts.compiler_support,
            self.owner.facts.producer,
            witnesses,
            signatures,
        )
    }

    pub(crate) fn checked_value_type_interfaces(
        value: &crate::checked_cell::CheckedValueArtifact,
        signature: &crate::checked_cell::ExactCheckedSignature,
    ) -> Result<Self, CompileError> {
        Self::type_interfaces_from_support(
            value.compiler_source_support(),
            value
                .certified_interface()
                .interface()
                .toolchain_identity_sha256(),
            &[],
            &[signature],
        )
    }

    fn type_interfaces_from_support(
        support: &crate::artifact_inventory::CompilerSourceSupport,
        producer: [u8; 32],
        witnesses: &[&crate::checked_cell::CanonicalInputTypeWitness],
        signatures: &[&crate::checked_cell::ExactCheckedSignature],
    ) -> Result<Self, CompileError> {
        if producer == [0; 32] {
            return Err(failure("original type context lacks its compiler producer"));
        }
        let selected = support.selected_interfaces()?;
        let mut owners = BTreeSet::new();
        for witness in witnesses {
            for (unit, module, seal) in witness.interface_seals() {
                let owner = ExactModuleIdentity {
                    unit: unit.to_owned(),
                    module: module.to_owned(),
                };
                match selected.get(&owner) {
                    Some(entry) => {
                        if entry.descriptor.producer_sha256 != producer
                            || hex(&entry.descriptor.interface_sha256) != seal
                        {
                            return Err(failure(
                                "original input differs from its certified interface seal",
                            ));
                        }
                        owners.insert(owner);
                    }
                    None if unit == "main" || selected.is_home_unit(unit) => {
                        return Err(failure("original input lacks its certified home interface"));
                    }
                    None => {}
                }
            }
        }
        for name in witnesses
            .iter()
            .map(|witness| witness.signature())
            .chain(signatures.iter().copied())
            .flat_map(|signature| signature.names())
        {
            let owner = ExactModuleIdentity {
                unit: name.unit().to_owned(),
                module: name.module().to_owned(),
            };
            match selected.get(&owner) {
                Some(entry) => {
                    if entry.descriptor.producer_sha256 != producer {
                        return Err(failure(
                            "original type interface differs from its compiler producer",
                        ));
                    }
                    owners.insert(owner);
                }
                None if name.unit() == "main" || selected.is_home_unit(name.unit()) => {
                    return Err(failure(
                        "original native signature lacks its certified Name owner",
                    ));
                }
                None => {}
            }
        }
        Ok(Self::from_facts(DeclarationContextFacts {
            compiler_support: selected.support_for_owners(owners.into_iter().collect())?,
            producer,
            lexical: Default::default(),
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,
        }))
    }

    /// Merge original type-interface evidence without granting lexical imports.
    /// Even an empty home closure must agree on its compiler producer.
    pub fn extend_interface_context(mut self, context: &Self) -> Result<Self, CompileError> {
        if context.owner.facts.producer != [0; 32] {
            self = self.admit_producer(context.owner.facts.producer)?;
        }
        let support = context.compiler_source_support();
        let interfaces = support.selected_interfaces()?;
        let selected = self
            .owner
            .facts
            .compiler_support
            .extend_interfaces(&interfaces)?;
        self = self.rebuild(|facts| facts.compiler_support = selected);
        self.finish()
    }

    /// Project only interfaces selected by the original checked template. The
    /// immutable context owns their seals; template text selects imports, never
    /// manufactures interface or native authority.
    #[cfg(test)]
    fn template_interface_graph(
        &self,
        templates: &[String],
    ) -> Result<BTreeMap<ExactModuleIdentity, TemplateInterfaceNode>, CompileError> {
        Ok(self.selected_template_imports(templates)?.graph)
    }

    pub(crate) fn selected_template_imports(
        &self,
        templates: &[String],
    ) -> Result<SelectedTemplateImports, CompileError> {
        let mut roots = self
            .lexical_graph()
            .iter()
            .filter(|node| template_selects_owner(templates, &node.owner))
            .map(|node| node.owner.clone())
            .collect::<BTreeSet<_>>();
        let mut graph = self.interface_graph_for_roots(roots.iter().cloned().collect())?;
        if let Some(retained) = &self.owner.facts.template_imports {
            retained.validate(self.owner.facts.producer, self.artifact_view())?;
            let selected = retained
                .roots
                .iter()
                .filter(|owner| template_selects_owner(templates, owner))
                .cloned()
                .collect::<BTreeSet<_>>();
            let mut pending = selected.iter().cloned().collect::<Vec<_>>();
            let mut seen = BTreeSet::new();
            while let Some(owner) = pending.pop() {
                if !seen.insert(owner.clone()) {
                    continue;
                }
                let node = &retained.graph[&owner].interface;
                if graph
                    .insert(owner.clone(), node.clone())
                    .is_some_and(|old| old != *node)
                {
                    return Err(failure(
                        "template and lexical closures select different interfaces",
                    ));
                }
                pending.extend(node.imports.iter().cloned());
            }
            roots.extend(selected);
        }
        Ok(SelectedTemplateImports { roots, graph })
    }

    /// A pure preview observes every instance visible to the original request,
    /// including orphan instances unrelated to the input's nominal type owner.
    /// This compiler-issued context grants graph edges, never authored imports.
    pub(crate) fn original_preview_interface_graph(
        &self,
    ) -> Result<BTreeMap<ExactModuleIdentity, TemplateInterfaceNode>, CompileError> {
        let target = self.original_instance_target()?;
        if self
            .lexical_graph()
            .iter()
            .any(|node| node.owner.unit != "main")
        {
            return Err(failure(
                "activation preview original graph belongs to another home unit",
            ));
        }
        self.normalize()?;
        let graph = self.interface_graph_for_roots(
            self.lexical_graph()
                .iter()
                .map(|node| node.owner.clone())
                .collect(),
        )?;
        if !graph.contains_key(target) {
            return Err(failure(
                "activation preview original target is outside its sealed graph",
            ));
        }
        Ok(graph)
    }

    /// The preview observes the complete original instance environment and
    /// native availability, but cannot read a value-interface root that is
    /// outside that environment, its input type, and every retained dependency.
    /// Selection follows the issued artifact edges, including historical native
    /// captures; a value dependency is never removed just because it is private.
    pub(crate) fn original_display_projection(&self, input: &Self) -> Result<Self, CompileError> {
        let graph = self.original_preview_interface_graph()?;
        let mut context = self.clone().extend_interface_context(input)?;
        let input_ids = input.artifact_view().artifact_ids();
        let excluded_value_roots =
            context
                .owner
                .facts
                .compiler_support
                .artifacts()
                .entries()
                .into_iter()
                .filter_map(|entry| {
                    let value_interface = matches!(
                        &entry.payload,
                        ArtifactPayload::Interface(_, JoinedInterfaceRole::ValueInterface)
                    );
                    let retained =
                        graph.contains_key(&entry.descriptor.owner)
                            || input_ids.contains(&entry.descriptor.id)
                            || context.owner.facts.template_imports.as_ref().is_some_and(
                                |imports| imports.graph.contains_key(&entry.descriptor.owner),
                            );
                    (value_interface && !retained).then_some(entry.descriptor.id)
                })
                .collect::<BTreeSet<_>>();
        let updated = context
            .owner
            .facts
            .compiler_support
            .without_value_roots(&excluded_value_roots)?;
        context = context.rebuild(|facts| facts.compiler_support = updated);
        let context = context.finish()?;
        if context.original_preview_interface_graph()? != graph {
            return Err(failure(
                "display projection changed original instance evidence",
            ));
        }
        Ok(context)
    }

    fn interface_graph_for_roots(
        &self,
        mut pending: Vec<ExactModuleIdentity>,
    ) -> Result<BTreeMap<ExactModuleIdentity, TemplateInterfaceNode>, CompileError> {
        let metadata = self.compiler_metadata_snapshot()?;
        let lexical = self
            .lexical_graph()
            .iter()
            .map(|node| (&node.owner, node))
            .collect::<BTreeMap<_, _>>();
        let mut selected = BTreeMap::new();
        while let Some(owner) = pending.pop() {
            if selected.contains_key(&owner) {
                continue;
            }
            let node = lexical
                .get(&owner)
                .ok_or_else(|| failure("checked template lexical closure is incomplete"))?;
            let interface = metadata.entries.get(&owner).ok_or_else(|| {
                failure("checked template selection lacks one exact interface seal")
            })?;
            pending.extend(node.imports.iter().cloned());
            selected.insert(
                owner,
                TemplateInterfaceNode {
                    interface_sha256: interface.descriptor.interface_sha256,
                    imports: node.imports.clone(),
                },
            );
        }
        Ok(selected)
    }

    pub(crate) fn interface_owners(&self) -> Vec<ExactInterfaceOwner> {
        self.owner
            .facts
            .compiler_support
            .artifacts()
            .interface_owners()
    }
    pub(crate) fn module_interfaces(
        &self,
    ) -> Vec<crate::certified_products::CertifiedModuleInterface> {
        self.owner
            .facts
            .compiler_support
            .artifacts()
            .entries()
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Canonical(interface) => Some(interface.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn materialize_module_interfaces(
        &self,
        root: &Path,
    ) -> Result<Vec<recovery_artifacts::RecoveryModuleInterfaceRef>, CompileError> {
        let mut validation = PackageInterfaceValidation::default();
        self.module_interfaces()
            .iter()
            .map(|interface| {
                recovery_artifacts::materialize_module_interface(
                    root,
                    interface,
                    &mut validation,
                    MaterializationMode::Durable,
                )
                .map_err(failure)
            })
            .collect()
    }

    /// Materialize owned native products once and reuse their exact canonical
    /// references when publishing the same immutable compiler context.
    pub fn materialize_recovery_products_and_interfaces(
        &self,
        root: &Path,
    ) -> Result<
        (
            Vec<RecoveryArtifactRef>,
            Vec<recovery_artifacts::RecoveryModuleInterfaceRef>,
        ),
        CompileError,
    > {
        self.materialize_recovery_entries(
            root,
            &self.owner.facts.compiler_support.artifacts().entries(),
        )
    }

    /// Materialize only newly referenced immutable payloads from this exact view.
    /// Retained references are owned by the run and need no repeated file reads.
    pub fn materialize_recovery_products_and_interfaces_for_artifacts(
        &self,
        root: &Path,
        artifacts: &BTreeSet<ArtifactId>,
    ) -> Result<
        (
            Vec<RecoveryArtifactRef>,
            Vec<recovery_artifacts::RecoveryModuleInterfaceRef>,
        ),
        CompileError,
    > {
        if artifacts.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        self.materialize_recovery_entries(
            root,
            &self
                .owner
                .facts
                .compiler_support
                .artifacts()
                .entries_for_artifacts(artifacts)?,
        )
    }

    fn materialize_recovery_entries(
        &self,
        root: &Path,
        entries: &[Arc<ArtifactEntry>],
    ) -> Result<
        (
            Vec<RecoveryArtifactRef>,
            Vec<recovery_artifacts::RecoveryModuleInterfaceRef>,
        ),
        CompileError,
    > {
        let originals = entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some(product.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut validation = PackageInterfaceValidation::default();
        let products = if originals.is_empty() {
            Vec::new()
        } else {
            recovery_artifacts::materialize_certified_products_with_validation(
                root,
                self.toolchain_identity_sha256(),
                &originals,
                &mut validation,
                MaterializationMode::Durable,
            )
            .map_err(materialization_failure)?
        };
        let emitted = products
            .iter()
            .filter_map(|product| product.module_interface.as_ref())
            .map(|reference| {
                (
                    crate::artifact_inventory::ArtifactDescriptor::from_recovery_module_interface(
                        reference,
                    )
                    .id,
                    reference,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let interfaces = entries
            .iter()
            .filter_map(|entry| {
                let ArtifactPayload::Canonical(interface) = &entry.payload else {
                    return None;
                };
                Some(match emitted.get(&entry.descriptor.id) {
                    Some(reference) => Ok((*reference).clone()),
                    None => recovery_artifacts::materialize_module_interface(
                        root,
                        interface,
                        &mut validation,
                        MaterializationMode::Durable,
                    )
                    .map_err(materialization_failure),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((products, interfaces))
    }

    /// Seal requested joined and value interfaces from the issuing exact view.
    /// Native and canonical payloads use the product materializer above.
    pub fn materialize_joined_and_value_interfaces_for_artifacts(
        &self,
        root: &Path,
        artifacts: &BTreeSet<ArtifactId>,
    ) -> Result<(Vec<RecoveryJoinRef>, Vec<RecoveryValueInterfaceRef>), CompileError> {
        if artifacts.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let entries = self
            .owner
            .facts
            .compiler_support
            .artifacts()
            .entries_for_artifacts(artifacts)?;
        let mut work = recovery_artifacts::RecoveryArtifactWork::default();
        let mut joined = Vec::new();
        let mut values = Vec::new();
        for entry in entries {
            match &entry.payload {
                ArtifactPayload::Interface(interface, JoinedInterfaceRole::LexicalJoin) => {
                    joined.push(
                        interface
                            .materialize_with_work(root, &mut work)
                            .map_err(materialization_failure)?,
                    );
                }
                ArtifactPayload::Interface(interface, JoinedInterfaceRole::ValueInterface) => {
                    values.push(
                        CertifiedValueInterface::from_admitted_interface(
                            interface.clone(),
                            entry.requirements.clone(),
                        )
                        .materialize_with_work(root, &mut work)
                        .map_err(materialization_failure)?,
                    );
                }
                ArtifactPayload::Canonical(_) | ArtifactPayload::Original(_) => {}
            }
        }
        Ok((joined, values))
    }

    /// Select the original native owner from its compiler-issued origin. A
    /// lexical projection may have a different owner and grants no native root.
    pub fn authored_native_root(
        &self,
        generation: u64,
    ) -> Result<crate::artifact_inventory::ArtifactId, CompileError> {
        use crate::certified_products::CanonicalOrigin;
        let entries = self.owner.facts.compiler_support.artifacts().entries();
        let roots = entries
            .iter()
            .filter(|entry| match &entry.payload {
                ArtifactPayload::Original(product) if generation != 0 => {
                    product
                        .module_interface()
                        .map(|interface| interface.origin())
                        == Some(CanonicalOrigin::NativeAuthoredDeclaration { generation })
                }
                _ => false,
            })
            .map(|entry| entry.descriptor.id)
            .collect::<Vec<_>>();
        match roots.as_slice() {
            [root] => Ok(*root),
            _ => Err(admission_failure(
                ArtifactInventoryFailure::AuthoredNativeRoot {
                    generation,
                    found: roots.len(),
                },
            )),
        }
    }

    /// Read-only lifetime requirements for full original authored bodies.
    /// This preserves future entry support without selecting executable groups.
    pub fn authored_native_binding_custody_requirements(
        &self,
        owners: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<Vec<crate::artifact_inventory::NativeBindingRequirement>, CompileError> {
        self.owner
            .facts
            .compiler_support
            .authored_native_binding_custody_requirements(owners)
    }

    pub fn recovery_products(&self) -> Vec<CertifiedRecoveryProduct> {
        self.owner
            .facts
            .compiler_support
            .artifacts()
            .entries()
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some(product.clone()),
                _ => None,
            })
            .collect()
    }
    pub fn joined_interfaces(&self) -> Vec<CertifiedJoinedInterface> {
        self.owner
            .facts
            .compiler_support
            .artifacts()
            .entries()
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Interface(interface, JoinedInterfaceRole::LexicalJoin) => {
                    Some(interface.clone())
                }
                ArtifactPayload::Interface(_, JoinedInterfaceRole::ValueInterface)
                | ArtifactPayload::Original(_)
                | ArtifactPayload::Canonical(_) => None,
            })
            .collect()
    }
    pub fn snapshot(&self) -> crate::exact_context::ExactContextSnapshot {
        crate::exact_context::ExactContextSnapshot {
            producer: self.owner.facts.producer,
            selection: self
                .owner
                .facts
                .compiler_support
                .artifacts()
                .selection_snapshot()
                .clone(),
            roles: self
                .owner
                .facts
                .compiler_support
                .projection()
                .snapshot_roles()
                .clone(),
            offers: self
                .owner
                .facts
                .compiler_support
                .native_offers()
                .iter()
                .map(|offer| {
                    let facts = offer.snapshot_facts();
                    (facts.identity().expect("issued offer identity"), facts)
                })
                .collect(),
            lexical: self.owner.facts.lexical.clone(),
            input_decisions: self
                .owner
                .facts
                .compiler_support
                .recorded_input_decisions()
                .clone(),
        }
    }

    /// Bind authenticated restored facts to their actual persistent map bases.
    /// This preserves the authority and instance origin already held here.
    pub fn bind_snapshot_baseline(
        &mut self,
        baseline: &crate::exact_context::MaterializedContext,
    ) -> Result<(), CompileError> {
        let snapshot = baseline.snapshot();
        if self.owner.facts.producer != snapshot.producer
            || self.owner.facts.lexical != snapshot.lexical
        {
            return Err(failure(
                "restored compiler context differs from exact snapshot",
            ));
        }
        if self.snapshot().offers != snapshot.offers
            || self.owner.facts.compiler_support.recorded_input_decisions()
                != &snapshot.input_decisions
        {
            return Err(failure(
                "restored compiler availability differs from exact snapshot",
            ));
        }
        let mut facts = self.owner.facts.clone();
        facts.compiler_support = facts.compiler_support.bind_snapshot_baseline(snapshot)?;
        facts.lexical = snapshot.lexical.clone();
        *self = Self::from_facts(facts);
        Ok(())
    }

    pub fn lexical_graph(&self) -> &[ExactLexicalNode] {
        &self.owner.facts.lexical
    }

    pub fn value_interfaces(&self) -> Vec<CertifiedValueInterface> {
        self.owner
            .facts
            .compiler_support
            .artifacts()
            .entries()
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Interface(interface, JoinedInterfaceRole::ValueInterface) => {
                    Some(CertifiedValueInterface::from_admitted_interface(
                        interface.clone(),
                        entry.requirements.clone(),
                    ))
                }
                ArtifactPayload::Interface(_, JoinedInterfaceRole::LexicalJoin)
                | ArtifactPayload::Original(_)
                | ArtifactPayload::Canonical(_) => None,
            })
            .collect()
    }
    pub fn extend_with_value_interfaces(
        mut self,
        values: &[Arc<CertifiedValueInterface>],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        for value in values {
            self = self.admit_producer(value.interface().toolchain_identity_sha256())?;
        }
        let updated = self
            .owner
            .facts
            .compiler_support
            .admit_value_interfaces(values)?;
        self = self.rebuild(|facts| facts.compiler_support = updated);
        {
            let updated = self.owner.facts.lexical.replace(lexical)?;
            self = self.rebuild(|facts| facts.lexical = updated);
        }
        {
            let updated = OriginalInstanceEnvironment::Unknown;
            self = self.rebuild(|facts| facts.original_instance_environment = updated);
        }
        self.finish()
    }

    pub(crate) fn extend_retained_value_artifacts(
        mut self,
        values: &[Arc<crate::checked_cell::CheckedValueArtifact>],
    ) -> Result<Self, CompileError> {
        let mut lexical = self.owner.facts.lexical.to_vec();
        for value in values {
            let interface = value.certified_interface();
            lexical.extend_from_slice(value.source_lexical());
            self = self.admit_producer(interface.interface().toolchain_identity_sha256())?;
            let support = self
                .owner
                .facts
                .compiler_support
                .merge(value.compiler_source_support())?;
            self = self.rebuild(|facts| facts.compiler_support = support);
            if let Some(retained) = value.template_imports() {
                let updated = self
                    .owner
                    .facts
                    .compiler_support
                    .retain_custody(retained.compiler_support.artifacts())?;
                self = self.rebuild(|facts| facts.compiler_support = updated);
                {
                    let updated = Some(match &self.owner.facts.template_imports {
                        Some(existing) => RetainedTemplateImports::merge(existing, retained)?,
                        None => retained.clone(),
                    });
                    self = self.rebuild(|facts| facts.template_imports = updated);
                }
            }
        }
        {
            let updated = self
                .owner
                .facts
                .lexical
                .replace(compose_lexical_nodes(&lexical)?)?;
            self = self.rebuild(|facts| facts.lexical = updated);
        }
        {
            let updated = OriginalInstanceEnvironment::Unknown;
            self = self.rebuild(|facts| facts.original_instance_environment = updated);
        }
        self.finish()
    }

    #[cfg(test)]
    pub(crate) fn retain_value_source_surface(
        &self,
        value: &ArtifactView,
        support: &[ExactLexicalNode],
    ) -> Result<RetainedValueSourceSurface, CompileError> {
        PreparedValueSourceSupport::new(Arc::new(self.clone()), support)?
            .retain_surface(value, &BTreeSet::new())
    }

    pub(crate) fn extend_program_value_interface(
        self,
        value: Arc<CertifiedValueInterface>,
    ) -> Result<Self, CompileError> {
        let selected = self
            .owner
            .facts
            .lexical
            .iter()
            .map(|node| node.owner.clone())
            .collect::<BTreeSet<_>>();
        let mut lexical = self.owner.facts.lexical.to_vec();
        lexical.push(ExactLexicalNode {
            owner: identity(value.interface().unit(), value.interface().module()),
            // Type-owner evidence retains hydration dependencies without
            // granting their names or instances public lexical selection.
            imports: value
                .requirements()
                .iter()
                .filter(|owner| selected.contains(*owner))
                .cloned()
                .collect(),
        });
        self.extend_with_value_interfaces(&[value], lexical)
    }

    /// Logical identity is independent of materialization paths and of the
    /// optional source bytes retained only by a fresh authored certificate.
    pub fn semantic_sha256(&self) -> [u8; 32] {
        self.observation().digest
    }

    fn observation(&self) -> &ContextObservation {
        self.owner.observations.get_or_init(|| {
            #[cfg(test)]
            self.owner
                .semantic_issuances
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            use sha2::Digest;
            let metadata = self
                .owner
                .facts
                .compiler_support
                .artifacts()
                .metadata_snapshot();
            let semantics: Arc<[u8]> = self.semantic_bytes_from_metadata(&metadata).into();
            #[cfg(test)]
            CONTEXT_OBSERVATION_WORK.with(|work| {
                let (encodings, bytes) = work.get();
                work.set((encodings + 1, bytes + semantics.len()));
            });
            let digest = sha2::Sha256::digest(&semantics).into();
            ContextObservation {
                metadata,
                semantics,
                digest,
                display_prefix: OnceLock::new(),
            }
        })
    }

    /// Immutable owner equality includes exact dependency edges. Equal payload
    /// rows alone cannot substitute another binding's captured dependencies.
    pub fn same_authenticated_context(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
            || (self == other
                && self
                    .owner
                    .facts
                    .compiler_support
                    .artifacts()
                    .metadata_snapshot()
                    .dependencies()
                    == other
                        .owner
                        .facts
                        .compiler_support
                        .artifacts()
                        .metadata_snapshot()
                        .dependencies())
    }

    pub(crate) fn display_identity(&self, witness: &[u8]) -> [u8; 32] {
        use sha2::Digest;
        let observation = self.observation();
        let mut hash = observation
            .display_prefix
            .get_or_init(|| {
                #[cfg(test)]
                self.owner
                    .display_issuances
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let metadata = &observation.metadata;
                let wire = OriginalDisplayObservations {
                    semantics: &observation.semantics,
                    descriptors: metadata.descriptors(),
                    dependencies: metadata.dependencies(),
                    native_groups: &metadata.selected_native_groups,
                };
                let encoded = serde_json::to_vec(&wire).expect("issued display observations");
                let mut hash = sha2::Sha256::new();
                hash.update(b"TPEXACTDISPLAYCONTEXT1");
                hash.update((encoded.len() as u64).to_le_bytes());
                hash.update(&encoded);
                hash
            })
            .clone();
        hash.update(witness);
        hash.finalize().into()
    }

    pub(crate) fn same_display_observations(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.owner, &other.owner) {
            return true;
        }
        let left = self.observation();
        let right = other.observation();
        left.semantics == right.semantics
            && left.metadata.descriptors() == right.metadata.descriptors()
            && left.metadata.dependencies() == right.metadata.dependencies()
            && left.metadata.selected_native_groups == right.metadata.selected_native_groups
    }

    #[cfg(test)]
    fn original_semantic_bytes_oracle(
        &self,
        metadata: &crate::artifact_inventory::RetainedArtifactMetadata,
    ) -> Vec<u8> {
        use sha2::Digest;
        let mut lexical = self.owner.facts.lexical.iter().collect::<Vec<_>>();
        lexical.sort_by_key(|node| &node.owner);
        let mut native = metadata
            .artifacts
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some((&entry.descriptor, product)),
                _ => None,
            })
            .collect::<Vec<_>>();
        native.sort_by_key(|(descriptor, _)| (&descriptor.owner, descriptor.id));
        let mut fields = vec![
            text("TPEXACTCONTEXT"),
            text(
                if matches!(
                    self.owner.facts.original_instance_environment,
                    OriginalInstanceEnvironment::Unknown
                ) {
                    "10"
                } else {
                    "11"
                },
            ),
            text(hex(&sha2::Sha256::digest(
                serde_json::to_vec(&(
                    metadata.descriptors(),
                    metadata.dependencies(),
                    &metadata.selected_native_groups,
                    self.owner
                        .facts
                        .compiler_support
                        .projection()
                        .semantic_observations()
                        .expect("issued role observation"),
                    self.owner
                        .facts
                        .compiler_support
                        .native_offer_observations()
                        .expect("issued native offer observation"),
                    self.owner
                        .facts
                        .compiler_support
                        .recorded_input_decisions()
                        .iter()
                        .map(|(owner, fact)| {
                            (
                                owner,
                                fact.groups
                                    .iter()
                                    .map(|(ordinal, group)| (*ordinal, *group))
                                    .collect::<Vec<_>>(),
                            )
                        })
                        .collect::<Vec<_>>(),
                ))
                .expect("inventory encoding"),
            )
            .into())),
            text(hex(&self.owner.facts.producer)),
            Value::Array(
                native
                    .into_iter()
                    .map(|(descriptor, product)| {
                        let owner = product.owner();
                        Value::Array(vec![
                            text(&owner.unit),
                            text(&owner.module),
                            text(hex(&owner.module_version.0)),
                            text(hex(&owner.skinny_iface_sha256)),
                            text(hex(&owner.product_sha256)),
                            text(hex(&descriptor.package_imports_sha256)),
                            text(hex(&descriptor
                                .certification_sha256
                                .expect("original certification digest"))),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                metadata
                    .artifacts
                    .values()
                    .filter_map(|entry| match &entry.payload {
                        ArtifactPayload::Interface(interface, JoinedInterfaceRole::LexicalJoin) => {
                            Some((&entry.descriptor, interface))
                        }
                        ArtifactPayload::Interface(_, JoinedInterfaceRole::ValueInterface)
                        | ArtifactPayload::Original(_)
                        | ArtifactPayload::Canonical(_) => None,
                    })
                    .map(|(descriptor, join)| {
                        Value::Array(vec![
                            text(join.unit()),
                            text(join.module()),
                            text(hex(&descriptor.interface_sha256)),
                            text(hex(&descriptor.package_imports_sha256)),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                lexical
                    .into_iter()
                    .map(|node| {
                        let mut imports = node.imports.iter().collect::<Vec<_>>();
                        imports.sort();
                        Value::Array(vec![
                            module_value(&node.owner),
                            Value::Array(imports.into_iter().map(module_value).collect()),
                        ])
                    })
                    .collect(),
            ),
        ];
        match &self.owner.facts.original_instance_environment {
            OriginalInstanceEnvironment::Unknown => {}
            OriginalInstanceEnvironment::Complete { target } => fields.push(Value::Array(vec![
                text("original-instances-complete"),
                module_value(target),
            ])),
            OriginalInstanceEnvironment::MissingOriginalOwners(owners) => {
                fields.push(Value::Array(vec![
                    text("original-instances-missing"),
                    Value::Array(owners.iter().map(module_value).collect()),
                ]))
            }
        }
        if let Some(retained) = &self.owner.facts.template_imports {
            fields.push(Value::Array(vec![
                text("retained-template-imports1"),
                Value::Array(retained.roots.iter().map(module_value).collect()),
                Value::Array(
                    retained
                        .graph
                        .iter()
                        .map(|(owner, node)| {
                            Value::Array(vec![
                                module_value(owner),
                                text(hex(&node.canonical.0)),
                                text(hex(&node.interface.interface_sha256)),
                                Value::Array(
                                    node.interface.imports.iter().map(module_value).collect(),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ]));
        }
        let value = Value::Array(fields);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).expect("owned value encoding");
        bytes
    }

    fn semantic_bytes_from_metadata(
        &self,
        metadata: &crate::artifact_inventory::RetainedArtifactMetadata,
    ) -> Vec<u8> {
        use sha2::Digest;
        let span = tracing::debug_span!(target: "exomonad_harness::timing", "exact.semantic_encoding", inclusive = true, artifacts = metadata.artifacts.len(), selected_native_groups = metadata.selected_native_groups.len(), lexical_nodes = self.owner.facts.lexical.len());
        let _entered = span.enter();
        let mut semantic_timing = crate::timing::InvocationTiming::start_scoped(
            "exact.semantic_encoding",
            "construct_metadata_json_digest_and_semantic_value",
        );
        let mut lexical = self.owner.facts.lexical.iter().collect::<Vec<_>>();
        lexical.sort_by_key(|node| &node.owner);
        let mut native = metadata
            .artifacts
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some((&entry.descriptor, product)),
                _ => None,
            })
            .collect::<Vec<_>>();
        native.sort_by_key(|(descriptor, _)| (&descriptor.owner, descriptor.id));
        let mut fields = vec![
            text("TPEXACTCONTEXT"),
            text(
                if matches!(
                    self.owner.facts.original_instance_environment,
                    OriginalInstanceEnvironment::Unknown
                ) {
                    "10"
                } else {
                    "11"
                },
            ),
            text(hex(&sha2::Sha256::digest(
                serde_json::to_vec(&(
                    metadata.descriptors(),
                    metadata.dependencies(),
                    &metadata.selected_native_groups,
                    self.owner
                        .facts
                        .compiler_support
                        .projection()
                        .semantic_observations()
                        .expect("issued role observation"),
                    self.owner
                        .facts
                        .compiler_support
                        .native_offer_observations()
                        .expect("issued native offer observation"),
                    self.owner
                        .facts
                        .compiler_support
                        .recorded_input_decisions()
                        .iter()
                        .map(|(owner, fact)| {
                            (
                                owner,
                                fact.groups
                                    .iter()
                                    .map(|(ordinal, group)| (*ordinal, *group))
                                    .collect::<Vec<_>>(),
                            )
                        })
                        .collect::<Vec<_>>(),
                ))
                .expect("inventory encoding"),
            )
            .into())),
            text(hex(&self.owner.facts.producer)),
            Value::Array(
                native
                    .into_iter()
                    .map(|(descriptor, product)| {
                        let owner = product.owner();
                        Value::Array(vec![
                            text(&owner.unit),
                            text(&owner.module),
                            text(hex(&owner.module_version.0)),
                            text(hex(&owner.skinny_iface_sha256)),
                            text(hex(&owner.product_sha256)),
                            text(hex(&descriptor.package_imports_sha256)),
                            text(hex(&descriptor
                                .certification_sha256
                                .expect("original certification digest"))),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                metadata
                    .artifacts
                    .values()
                    .filter_map(|entry| match &entry.payload {
                        ArtifactPayload::Interface(interface, JoinedInterfaceRole::LexicalJoin) => {
                            Some((&entry.descriptor, interface))
                        }
                        ArtifactPayload::Interface(_, JoinedInterfaceRole::ValueInterface)
                        | ArtifactPayload::Original(_)
                        | ArtifactPayload::Canonical(_) => None,
                    })
                    .map(|(descriptor, join)| {
                        Value::Array(vec![
                            text(join.unit()),
                            text(join.module()),
                            text(hex(&descriptor.interface_sha256)),
                            text(hex(&descriptor.package_imports_sha256)),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                lexical
                    .into_iter()
                    .map(|node| {
                        let mut imports = node.imports.iter().collect::<Vec<_>>();
                        imports.sort();
                        Value::Array(vec![
                            module_value(&node.owner),
                            Value::Array(imports.into_iter().map(module_value).collect()),
                        ])
                    })
                    .collect(),
            ),
        ];
        match &self.owner.facts.original_instance_environment {
            OriginalInstanceEnvironment::Unknown => {}
            OriginalInstanceEnvironment::Complete { target } => fields.push(Value::Array(vec![
                text("original-instances-complete"),
                module_value(target),
            ])),
            OriginalInstanceEnvironment::MissingOriginalOwners(owners) => {
                fields.push(Value::Array(vec![
                    text("original-instances-missing"),
                    Value::Array(owners.iter().map(module_value).collect()),
                ]))
            }
        }
        if let Some(retained) = &self.owner.facts.template_imports {
            fields.push(Value::Array(vec![
                text("retained-template-imports1"),
                Value::Array(retained.roots.iter().map(module_value).collect()),
                Value::Array(
                    retained
                        .graph
                        .iter()
                        .map(|(owner, node)| {
                            Value::Array(vec![
                                module_value(owner),
                                text(hex(&node.canonical.0)),
                                text(hex(&node.interface.interface_sha256)),
                                Value::Array(
                                    node.interface.imports.iter().map(module_value).collect(),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ]));
        }
        let value = Value::Array(fields);
        semantic_timing.advance("encode_semantic_value");
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).expect("owned value encoding");
        tracing::debug!(target: "exomonad_harness::timing", encoded_bytes = bytes.len(), "semantic encoded bytes");
        semantic_timing.advance("temporary_semantic_value_drop");
        drop(value);
        semantic_timing.finish(true);
        bytes
    }

    pub(crate) fn validate_artifacts(
        &self,
        artifacts: &[DeclarationArtifact],
    ) -> Result<(), CompileError> {
        self.validate_artifacts_from_metadata(artifacts, &self.compiler_metadata_snapshot()?)
    }

    fn validate_artifacts_from_metadata(
        &self,
        artifacts: &[DeclarationArtifact],
        metadata: &CompilerArtifactMetadata,
    ) -> Result<(), CompileError> {
        let retained = self
            .owner
            .facts
            .compiler_support
            .artifacts()
            .retained_materialization(metadata)?;
        let rows = retained
            .as_ref()
            .map(|retained| retained.selected_rows(metadata))
            .unwrap_or_default();
        self.validate_artifacts_with_owned_rows(artifacts, metadata, &rows)
    }

    fn validate_artifacts_with_owned_rows(
        &self,
        artifacts: &[DeclarationArtifact],
        metadata: &CompilerArtifactMetadata,
        owned: &BTreeMap<ArtifactId, &RetainedArtifactRow>,
    ) -> Result<(), CompileError> {
        let mut seen = BTreeSet::new();
        for artifact in artifacts {
            let exact = &artifact.interface;
            let owner = identity(&exact.unit, &exact.module);
            if !seen.insert(owner.clone()) || !exact.path.is_absolute() {
                return Err(failure("duplicate owner or relative interface path"));
            }
            let entry = metadata
                .entries
                .get(&owner)
                .ok_or_else(|| failure("artifact is not owned by the context"))?;
            if exact.requirements.iter().cloned().collect::<BTreeSet<_>>()
                != entry
                    .requirements
                    .iter()
                    .map(|owner| (owner.unit.clone(), owner.module.clone()))
                    .collect()
            {
                return Err(failure("artifact requirements differ from owned context"));
            }
            if entry.descriptor.kind != entry.payload.artifact_kind() {
                return Err(failure("interface evidence kind differs from payload"));
            }
            if owned
                .get(&entry.descriptor.id)
                .is_some_and(|row| row.artifact() == *artifact)
            {
                continue;
            }
            let (iface, packages) = match &entry.payload {
                ArtifactPayload::Original(product) => {
                    let snapshot = artifact
                        .product
                        .as_ref()
                        .ok_or_else(|| failure("original product is missing"))?;
                    if !snapshot.path.is_absolute()
                        || snapshot.module != exact.module
                        || snapshot.sha256 != hex(&product.owner().product_sha256)
                        || std::fs::read(&snapshot.path)? != product.product_bytes()
                    {
                        return Err(failure("original product differs from owned bytes"));
                    }
                    (product.interface_bytes(), product.package_imports_bytes())
                }
                ArtifactPayload::Canonical(interface) => {
                    if artifact.product.is_some() {
                        return Err(failure("type interface has an executable product"));
                    }
                    (
                        interface.interface_bytes(),
                        interface.package_imports_bytes(),
                    )
                }
                ArtifactPayload::Interface(join, _) => {
                    if artifact.product.is_some() {
                        return Err(failure("synthetic anchor has an implementation product"));
                    }
                    (join.interface_bytes(), join.package_imports_bytes())
                }
            };
            if sha256(iface) != exact.sha256 || std::fs::read(&exact.path)? != iface {
                return Err(failure("interface differs from owned bytes"));
            }
            let mut packages_path = exact.path.as_os_str().to_os_string();
            packages_path.push(".packages");
            if std::fs::read(std::path::PathBuf::from(packages_path))? != packages {
                return Err(failure("package witness differs from owned bytes"));
            }
        }
        if seen.len() != metadata.entries.len() {
            return Err(failure("owned artifact closure is incomplete"));
        }
        Ok(())
    }

    pub(crate) fn admit_producer(mut self, producer: [u8; 32]) -> Result<Self, CompileError> {
        if producer == [0; 32]
            || (self.owner.facts.producer != [0; 32] && producer != self.owner.facts.producer)
        {
            return Err(failure("producer identity differs"));
        }
        if self.owner.facts.producer != producer {
            self = self.rebuild(|facts| facts.producer = producer);
        }
        Ok(self)
    }

    /// Finalize a context at its issuing boundary. Historical custody remains
    /// complete; the compiler namespace contains exactly the issued roles.
    fn finish(mut self) -> Result<Self, CompileError> {
        let updated = self.owner.facts.compiler_support.finalize_namespace()?;
        self = self.rebuild(|facts| facts.compiler_support = updated);
        self.normalize()?;
        Ok(self)
    }

    fn normalize(&self) -> Result<(), CompileError> {
        let metadata = self.compiler_metadata_snapshot()?;
        self.validate_published_source_originals()?;
        if let Some(retained) = &self.owner.facts.template_imports {
            retained.validate(self.owner.facts.producer, self.artifact_view())?;
        }
        let owners = metadata.entries.keys().collect::<BTreeSet<_>>();
        let lexical_owners = self
            .owner
            .facts
            .lexical
            .iter()
            .map(|node| &node.owner)
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        for node in &self.owner.facts.lexical {
            if !seen.insert(&node.owner) {
                return Err(failure(format!(
                    "duplicate selected lexical owner {}:{}",
                    node.owner.unit, node.owner.module
                )));
            }
            if !owners.contains(&node.owner) {
                return Err(failure(format!(
                    "selected lexical owner lacks its interface {}:{}",
                    node.owner.unit, node.owner.module
                )));
            }
            let mut imports = BTreeSet::new();
            for imported in &node.imports {
                if !imports.insert(imported) {
                    return Err(failure(format!(
                        "duplicate selected lexical import {}:{} -> {}:{}",
                        node.owner.unit, node.owner.module, imported.unit, imported.module
                    )));
                }
                if !lexical_owners.contains(imported) {
                    return Err(failure(format!(
                        "selected lexical edge leaves its graph {}:{} -> {}:{}",
                        node.owner.unit, node.owner.module, imported.unit, imported.module
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn declaration_join_inputs(
        &self,
        directory: Arc<tempfile::TempDir>,
    ) -> Result<DeclarationJoinInputs, CompileError> {
        Arc::new(self.clone())
            .materialize_interface_inputs(directory, self.compiler_metadata_snapshot()?)
            .map(DeclarationJoinInputs)
    }

    pub(crate) fn recovery_inspection_inputs(
        self: &Arc<Self>,
    ) -> Result<RecoveryInspectionInputs, CompileError> {
        let metadata = self.compiler_metadata_snapshot()?;
        self.materialize_interface_inputs(Arc::new(tempfile::tempdir()?), metadata)
            .map(RecoveryInspectionInputs)
    }

    pub(crate) fn inherited_declaration_interface_entries(
        &self,
        authored: &[CertifiedRecoveryProduct],
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        let mut metadata = self.compiler_metadata_snapshot()?;
        metadata.entries.retain(|owner, _| {
            !authored.iter().any(|product| {
                product.owner().unit == owner.unit && product.owner().module == owner.module
            })
        });
        Ok(metadata.entries)
    }

    fn materialize_interface_inputs(
        self: &Arc<Self>,
        directory: Arc<tempfile::TempDir>,
        metadata: CompilerArtifactMetadata,
    ) -> Result<CertifiedInterfaceInputs, CompileError> {
        let mut validation = PackageInterfaceValidation::default();
        let mut artifacts = Vec::with_capacity(metadata.entries.len());
        for (owner, entry) in &metadata.entries {
            crate::host_work::checkpoint()?;
            let files = recovery_artifacts::materialize_selected_interface(
                directory.path(),
                owner,
                entry.clone(),
                self.toolchain_identity_sha256(),
                &mut validation,
            )
            .map_err(materialization_failure)?;
            artifacts.push(DeclarationArtifact {
                interface: ExactIfaceArtifact {
                    unit: owner.unit.clone(),
                    module: owner.module.clone(),
                    path: directory.path().join(files.interface_path),
                    sha256: hex(&files.skinny_iface_sha256),
                    requirements: entry
                        .requirements
                        .iter()
                        .map(|required| (required.unit.clone(), required.module.clone()))
                        .collect(),
                },
                product: None,
            });
        }
        let inputs = CertifiedInterfaceInputs {
            context: self.clone(),
            metadata,
            directory,
            artifacts,
        };
        inputs.validate()?;
        Ok(inputs)
    }

    pub fn materialize(
        &self,
        root: &Path,
    ) -> Result<MaterializedExactDeclarationContext, CompileError> {
        self.materialize_with_validation(
            root,
            &mut PackageInterfaceValidation::default(),
            MaterializationMode::Durable,
        )
        .map(|(materialized, _)| materialized)
    }

    /// Materialize inspection inputs for the supplied temporary owner. Keep
    /// that owner alive while using the returned paths; recovery publication
    /// continues through the durable `materialize` entry point.
    pub fn materialize_scratch(
        &self,
        directory: &tempfile::TempDir,
    ) -> Result<MaterializedExactDeclarationContext, CompileError> {
        self.materialize_with_validation(
            directory.path(),
            &mut PackageInterfaceValidation::default(),
            MaterializationMode::Scratch,
        )
        .map(|(materialized, _)| materialized)
    }

    fn materialize_with_validation(
        &self,
        root: &Path,
        validation: &mut PackageInterfaceValidation,
        mode: MaterializationMode,
    ) -> Result<
        (
            MaterializedExactDeclarationContext,
            Vec<RecoveryArtifactRef>,
        ),
        CompileError,
    > {
        let metadata = self.compiler_metadata_snapshot()?;
        self.materialize_entries_with_validation(
            root,
            &metadata.entries.values().cloned().collect::<Vec<_>>(),
            validation,
            mode,
        )
    }

    fn materialize_entries_with_validation(
        &self,
        root: &Path,
        entries: &[Arc<ArtifactEntry>],
        validation: &mut PackageInterfaceValidation,
        mode: MaterializationMode,
    ) -> Result<
        (
            MaterializedExactDeclarationContext,
            Vec<RecoveryArtifactRef>,
        ),
        CompileError,
    > {
        crate::host_work::checkpoint()?;
        let mut native_owners = BTreeMap::new();
        for entry in entries
            .iter()
            .filter(|entry| matches!(entry.payload, ArtifactPayload::Original(_)))
        {
            crate::host_work::checkpoint()?;
            if let Some(previous) =
                native_owners.insert(entry.descriptor.owner.clone(), entry.descriptor.id)
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::CompilerOriginalOfferConflict {
                        owner: entry.descriptor.owner.clone(),
                        existing: previous,
                        incoming: entry.descriptor.id,
                    },
                ));
            }
        }
        let products = entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some(product.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let references = if products.is_empty() {
            Vec::new()
        } else {
            recovery_artifacts::materialize_certified_products_with_validation(
                root,
                self.owner.facts.producer,
                &products,
                validation,
                mode,
            )
            .map_err(materialization_failure)?
        };
        let native_owners = products
            .iter()
            .map(|product| identity(&product.owner().unit, &product.owner().module))
            .collect::<BTreeSet<_>>();
        let canonical = entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Canonical(interface)
                    if !native_owners.contains(&entry.descriptor.owner) =>
                {
                    Some(interface)
                }
                _ => None,
            })
            .map(|interface| {
                recovery_artifacts::materialize_module_interface(root, interface, validation, mode)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(materialization_failure)?;
        let joined = entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Interface(interface, _) => Some(interface),
                _ => None,
            })
            .map(|interface| interface.materialize_with_validation(root, validation, mode))
            .collect::<Result<Vec<_>, _>>()
            .map_err(materialization_failure)?;
        let requirements = entries
            .iter()
            .map(|entry| (&entry.descriptor.owner, &entry.requirements))
            .collect::<BTreeMap<_, _>>();
        let mut artifacts = Vec::new();
        for reference in &references {
            crate::host_work::checkpoint()?;
            let owner = identity(&reference.unit, &reference.module);
            artifacts.push(DeclarationArtifact {
                interface: ExactIfaceArtifact {
                    unit: reference.unit.clone(),
                    module: reference.module.clone(),
                    path: root.join(&reference.interface_path),
                    sha256: hex(&reference.skinny_iface_sha256),
                    requirements: requirements[&owner]
                        .iter()
                        .map(|owner| (owner.unit.clone(), owner.module.clone()))
                        .collect(),
                },
                product: Some(ModuleSnapshot {
                    module: reference.module.clone(),
                    path: root.join(&reference.product_path),
                    sha256: hex(&reference.product_sha256),
                }),
            });
        }
        for reference in canonical
            .into_iter()
            .map(|reference| reference.interface)
            .chain(joined)
        {
            let owner = identity(&reference.unit, &reference.module);
            artifacts.push(DeclarationArtifact {
                interface: ExactIfaceArtifact {
                    unit: reference.unit,
                    module: reference.module,
                    path: root.join(reference.interface_path),
                    sha256: hex(&reference.skinny_iface_sha256),
                    requirements: requirements[&owner]
                        .iter()
                        .map(|owner| (owner.unit.clone(), owner.module.clone()))
                        .collect(),
                },
                product: None,
            });
        }
        Ok((
            MaterializedExactDeclarationContext {
                artifacts,
                lexical: self.owner.facts.lexical.to_vec(),
            },
            references,
        ))
    }

    pub(crate) fn prepare_compilation(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.prepare_compilation_with_authorization(root, producer, None)
    }

    pub(crate) fn prepare_compilation_with_authorization(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
        authorization: Option<Value>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.prepare_compilation_with_private_input(root, producer, authorization, None)
    }

    pub(crate) fn prepare_compilation_with_private_input(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
        authorization: Option<Value>,
        private: Option<OriginalCompilerInputs>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.prepare_compilation_with_scaffold(
            root,
            producer,
            authorization,
            &ProtectedScaffoldRequirements::default(),
            private,
        )
    }

    fn prepare_compilation_with_scaffold(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
        authorization: Option<Value>,
        scaffold: &ProtectedScaffoldRequirements,
        private: Option<OriginalCompilerInputs>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        let inputs = scaffold.compiler_inputs(self, private.as_ref())?;

        self.prepare_compilation_from_metadata(
            root,
            producer,
            authorization,
            inputs,
            scaffold,
            private,
            RequestInputTransport::OwnedArena,
        )
    }

    /// Bind authorization and the request to one observation of this immutable
    /// context. The caller cannot supply an unrelated semantic identity.
    #[cfg(test)]
    pub(crate) fn prepare_compilation_authorizing(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
        authorize: impl FnOnce([u8; 32]) -> Result<Value, CompileError>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.prepare_compilation_authorizing_with_scaffold(
            root,
            producer,
            &ProtectedScaffoldRequirements::default(),
            |semantic| Ok((authorize(semantic)?, None)),
        )
    }

    fn prepare_compilation_authorizing_with_scaffold(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
        scaffold: &ProtectedScaffoldRequirements,
        authorize: impl FnOnce(
            [u8; 32],
        ) -> Result<(Value, Option<OriginalCompilerInputs>), CompileError>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        let baseline = scaffold.compiler_inputs(self, None)?;

        let (authorization, private) = authorize(baseline.declaration_semantic_sha256)?;
        let inputs = match private.as_ref() {
            Some(private) => scaffold.compiler_inputs(self, Some(private))?,
            None => baseline,
        };

        self.prepare_compilation_from_metadata(
            root,
            producer,
            Some(authorization),
            inputs,
            scaffold,
            private,
            RequestInputTransport::OwnedArena,
        )
    }

    fn materialize_retained_artifacts(
        &self,
        metadata: &CompilerArtifactMetadata,
        parents: Vec<Arc<RetainedArtifactMaterialization>>,
        directory: tempfile::TempDir,
    ) -> Result<RetainedArtifactMaterialization, CompileError> {
        crate::host_work::checkpoint()?;
        let directory = Arc::new(directory);
        let root = directory.path();
        let mut inherited_rows = BTreeMap::new();
        for parent in &parents {
            inherited_rows.extend(parent.selected_rows(metadata));
        }
        let inherited_row_count = inherited_rows.len();
        let entries = metadata.entries.values().cloned().collect::<Vec<_>>();
        let new_entries = entries
            .iter()
            .filter(|entry| !inherited_rows.contains_key(&entry.descriptor.id))
            .cloned()
            .collect::<Vec<_>>();
        let mut rows = BTreeMap::<ArtifactId, RetainedArtifactRow>::new();
        let mut arena_builder = crate::owned_input_arena::OwnedInputArenaBuilder::new(
            tidepool_repr::execution_schema::InventoryDecodeLimits::default().max_bytes as u64,
        )
        .map_err(failure)?;
        let mut pending_inputs = BTreeMap::new();
        let mut acquired_inputs = BTreeMap::new();
        for entry in &new_entries {
            let canonical = match &entry.payload {
                ArtifactPayload::Canonical(interface) => Some(interface),
                ArtifactPayload::Original(product) => product.module_interface(),
                ArtifactPayload::Interface(..) => None,
            };
            if let Some(custody) = canonical.and_then(|canonical| canonical.catalog_input_custody())
            {
                acquired_inputs.insert(entry.descriptor.id, custody.transports());
                continue;
            }
            for (kind, bytes) in original_inputs::entry_payloads(entry) {
                pending_inputs.insert(
                    (entry.descriptor.id, kind),
                    arena_builder.append(bytes).map_err(failure)?,
                );
            }
        }
        let mut validation = PackageInterfaceValidation::default();
        let context_bytes = materialization_bytes(&new_entries);
        let start = std::time::Instant::now();
        let (materialized, references) = self.materialize_entries_with_validation(
            root,
            &new_entries,
            &mut validation,
            MaterializationMode::Scratch,
        )?;
        let references = references
            .iter()
            .map(|reference| (identity(&reference.unit, &reference.module), reference))
            .collect::<BTreeMap<_, _>>();
        for artifact in materialized.artifacts {
            crate::host_work::checkpoint()?;
            let entry =
                &metadata.entries[&identity(&artifact.interface.unit, &artifact.interface.module)];
            let interface_evidence = scope_interface_evidence(entry, root, &mut validation)?;
            let payload = match artifact.product {
                Some(product) => {
                    let reference = references
                        .get(&entry.descriptor.owner)
                        .ok_or_else(|| failure("original certificate reference is missing"))?;
                    RetainedArtifactPayload::Native {
                        product,
                        certification_path: root.join(&reference.certification_path),
                        certification_sha256: reference.certification_sha256,
                    }
                }
                None => RetainedArtifactPayload::InterfaceOnly,
            };
            rows.insert(
                entry.descriptor.id,
                RetainedArtifactRow {
                    owned_inputs: acquired_inputs
                        .remove(&entry.descriptor.id)
                        .unwrap_or_default(),
                    _directory: Arc::clone(&directory),
                    interface: artifact.interface,
                    interface_evidence,
                    payload,
                    canonical_custody: match &entry.payload {
                        ArtifactPayload::Canonical(interface) => Some(interface.clone()),
                        ArtifactPayload::Original(product) => product.module_interface().cloned(),
                        ArtifactPayload::Interface(..) => None,
                    },
                },
            );
        }
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.context_materialize",
            start.elapsed(),
            context_bytes,
        );
        let mut selected_rows = inherited_rows;
        selected_rows.extend(rows.iter().map(|(id, row)| (*id, row)));
        let artifacts = entries
            .iter()
            .map(|entry| selected_rows[&entry.descriptor.id].artifact())
            .collect::<Vec<_>>();
        let start = std::time::Instant::now();
        // The recovery writer issued every row from owned immutable bytes and
        // checked its private file once. Validate selection metadata without
        // rereading those same private payloads.
        self.validate_artifacts_with_owned_rows(&artifacts, metadata, &selected_rows)?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.context_validate",
            start.elapsed(),
            context_bytes,
        );
        let available_entries = metadata
            .retained
            .artifacts
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let available = original_products_by_id(&available_entries);
        let inherited_groups = RetainedArtifactMaterialization::selected_group_refs(
            parents.iter().map(Arc::as_ref),
            &metadata.retained,
        )
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
        let inherited_group_handle_clones = inherited_groups.len();
        let start = std::time::Instant::now();
        let groups = certify_selected_owned_products_in_context_with_validation(
            &available,
            &inherited_groups,
            &metadata.retained.selected_native_groups,
            &mut validation,
        )
        .map_err(compiler_evidence_failure)?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.inherited_groups",
            start.elapsed(),
            context_bytes,
        );
        let mut graph_paths = parents
            .iter()
            .flat_map(|parent| parent.graph_path_refs())
            .map(|(digest, path)| (digest, path.path.clone()))
            .collect::<BTreeMap<_, _>>();
        let inherited_graphs = graph_paths.keys().copied().collect::<BTreeSet<_>>();
        let mut scope_written_bytes = 0;
        let execution_scope = execution_scope_value_with_graph_paths(
            &entries,
            root,
            &mut graph_paths,
            &mut scope_written_bytes,
        )?;
        let mut pending_graphs = BTreeMap::new();
        let mut graph_transports = BTreeMap::new();
        for entry in &entries {
            if let ArtifactPayload::Original(product) = &entry.payload {
                if let Some(graph) = product.execution_source() {
                    if !inherited_graphs.contains(&graph.digest())
                        && graph_paths.contains_key(&graph.digest())
                        && !pending_graphs.contains_key(&graph.digest())
                    {
                        if let Some(slice) = product
                            .module_interface()
                            .and_then(|canonical| canonical.catalog_input_custody())
                            .and_then(|custody| {
                                custody
                                    .transport(original_inputs::OriginalInputKind::Graph)
                                    .cloned()
                            })
                        {
                            graph_transports.insert(graph.digest(), slice);
                        } else if !graph_transports.contains_key(&graph.digest()) {
                            pending_graphs.insert(
                                graph.digest(),
                                arena_builder.append(graph.bytes()).map_err(failure)?,
                            );
                        }
                    }
                }
            }
        }
        let arena = arena_builder.finish().map_err(failure)?;
        for ((id, kind), pending) in pending_inputs {
            rows.get_mut(&id)
                .expect("new materialization row")
                .owned_inputs
                .insert(kind, arena.issue_slice(pending).map_err(failure)?);
        }
        for (digest, pending) in pending_graphs {
            graph_transports.insert(digest, arena.issue_slice(pending).map_err(failure)?);
        }
        let work = validation.work();
        tracing::info!(target: "tidepool_toolchain::artifacts", phase = "exact_immutable_materialization",
            retained_entries = inherited_row_count, new_entries = new_entries.len(),
            retained_metadata_row_clones = 0, inherited_row_lookups = inherited_row_count, inherited_group_handle_clones,
            transient_graph_path_clones = inherited_graphs.len(),
            retained_graph_paths = graph_paths.len() - inherited_graphs.len(),
            recovery_read_bytes = work.read_bytes, recovery_written_bytes = work.written_bytes,
            recovery_decoded_bytes = work.decoded_bytes, recovery_hash_bytes = work.hash_bytes,
            scope_written_bytes, "completed private artifact ownership");
        crate::host_work::checkpoint()?;
        Ok(RetainedArtifactMaterialization {
            _directory: Some(Arc::clone(&directory)),
            _parents: parents,
            rows: rows.into_iter().map(|(id, row)| (id, Arc::new(row))).collect(),
            groups: groups.into(),
            execution_scope,
            graph_paths: graph_paths
                .into_iter()
                .filter(|(digest, _)| !inherited_graphs.contains(digest))
                .map(|(digest, path)| {
                    (
                        digest,
                        Arc::new(OwnedExecutionGraphFile {
                            transport: graph_transports[&digest].clone(),
                            path,
                            _directory: Arc::clone(&directory),
                        }),
                    )
                })
                .collect(),
            #[cfg(test)]
            payload_work: work,
        })
    }

    fn prepare_compilation_from_metadata(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
        authorization: Option<Value>,
        inputs: RequestCompilerInputs,
        scaffold: &ProtectedScaffoldRequirements,
        private: Option<OriginalCompilerInputs>,
        transport: RequestInputTransport,
    ) -> Result<ExactCompilationRequest, CompileError> {
        let metadata = &inputs.native_transport.metadata;
        let compiler_metadata = &inputs.metadata;
        let semantic_sha256 = inputs.declaration_semantic_sha256;
        let admitted_empty = authorization.is_some()
            && self.owner.facts.producer == [0; 32]
            && metadata.entries.is_empty()
            && self.owner.facts.lexical.is_empty();
        if !root.is_absolute()
            || (!admitted_empty
                && (self.owner.facts.producer == [0; 32]
                    || self.owner.facts.producer != crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer).sha256()))
        {
            return Err(failure(
                "compile request has a different producer or invalid root",
            ));
        }
        std::fs::create_dir_all(root)?;
        let reused = self
            .owner
            .facts
            .compiler_support
            .artifacts()
            .retained_materialization(metadata)?
            .is_some();
        let retained = self
            .owner
            .facts
            .compiler_support
            .artifacts()
            .retain_materialization(&metadata, |parents| {
                let directory = tempfile::Builder::new()
                    .prefix("tidepool-exact-artifacts-")
                    .tempdir()?;
                self.materialize_retained_artifacts(metadata, parents, directory)
            })?;
        let selected_rows = retained.selected_rows(metadata);
        let transported_artifacts = metadata
            .entries
            .values()
            .map(|entry| selected_rows[&entry.descriptor.id].artifact())
            .collect::<Vec<_>>();
        let groups: Arc<[PendingCertifiedGroup]> =
            RetainedArtifactMaterialization::selected_group_refs(
                [retained.as_ref()],
                &metadata.retained,
            )
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
            .into();
        let artifacts_by_owner = transported_artifacts
            .iter()
            .map(|artifact| {
                (
                    (
                        artifact.interface.unit.as_str(),
                        artifact.interface.module.as_str(),
                    ),
                    artifact,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let execution_scope = retained
            .execution_scope
            .as_ref()
            .map(|scope| compiler_execution_scope(scope, compiler_metadata))
            .transpose()?;
        let artifacts = transported_artifacts
            .iter()
            .filter_map(|artifact| {
                let entry = compiler_metadata.entries.get(&identity(
                    &artifact.interface.unit,
                    &artifact.interface.module,
                ))?;
                let mut artifact = artifact.clone();
                if !matches!(entry.payload, ArtifactPayload::Original(_)) {
                    artifact.product = None;
                }
                Some(artifact)
            })
            .collect::<Vec<_>>();
        let fields = vec![
            text("TPEXACTSCOPE"),
            text(EXACT_SCOPE_SCHEMA_VERSION),
            text(hex(&semantic_sha256)),
            text(
                crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
                    .hex(),
            ),
            Value::Array(
                transported_artifacts
                    .iter()
                    .map(|artifact| {
                        let iface = &artifact.interface;
                        let packages = iface.path.with_extension("hi.packages");
                        Ok(Value::Array(vec![
                            text(&iface.unit),
                            text(&iface.module),
                            path_value(&iface.path)?,
                            text(&iface.sha256),
                            Value::Array(
                                iface
                                    .requirements
                                    .iter()
                                    .map(|(unit, module)| {
                                        Value::Array(vec![text(unit), text(module)])
                                    })
                                    .collect(),
                            ),
                            path_value(&packages)?,
                            text(hex(&metadata.entries
                                [&identity(&iface.unit, &iface.module)]
                                .descriptor
                                .package_imports_sha256)),
                            selected_rows[&metadata.entries[&identity(&iface.unit, &iface.module)]
                                .descriptor
                                .id]
                                .interface_evidence
                                .clone(),
                        ]))
                    })
                    .collect::<Result<Vec<_>, CompileError>>()?,
            ),
            Value::Array(
                self.owner
                    .facts
                    .lexical
                    .iter()
                    .map(|node| {
                        Value::Array(vec![
                            module_value(&node.owner),
                            Value::Array(node.imports.iter().map(module_value).collect()),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                metadata
                    .entries
                    .values()
                    .filter_map(|entry| match &entry.payload {
                        ArtifactPayload::Original(product) => Some(product),
                        _ => None,
                    })
                    .map(|product| {
                        let owner = product.owner();
                        let artifact = artifacts_by_owner
                            .get(&(owner.unit.as_str(), owner.module.as_str()))
                            .and_then(|artifact| artifact.product.as_ref())
                            .ok_or_else(|| failure("original product anchor is missing"))?;
                        let selected = metadata.entries[&identity(&owner.unit, &owner.module)]
                            .descriptor
                            .id;
                        let independent = compiler_metadata
                            .entries
                            .get(&identity(&owner.unit, &owner.module))
                            .is_some_and(|entry| entry.descriptor.id == selected);
                        let census = groups
                            .iter()
                            .filter(|group| independent && group.owner() == owner)
                            .map(PendingCertifiedGroup::group)
                            .collect::<Vec<_>>();
                        let availability = inputs.native_transport.availability[&selected].as_ref();
                        let mode = match &availability.mode {
                            NativeTransportMode::Full => Value::Array(vec![text("full-carrier")]),
                            NativeTransportMode::IssuedGroups => Value::Array(vec![
                                text("issued-groups"),
                                Value::Array(
                                    availability
                                        .issued_ordinals()
                                        .into_iter()
                                        .map(|ordinal| Value::Integer(ordinal.into()))
                                        .collect(),
                                ),
                            ]),
                        };
                        Ok(Value::Array(vec![
                            text(&owner.unit),
                            text(&owner.module),
                            text(hex(&owner.module_version.0)),
                            text(hex(&owner.skinny_iface_sha256)),
                            text(hex(&owner.product_sha256)),
                            path_value(&artifact.path)?,
                            Value::Array(
                                census
                                    .into_iter()
                                    .map(|group| Value::Integer(group.original_ordinal().into()))
                                    .collect(),
                            ),
                            selected_rows[&metadata.entries[&identity(&owner.unit, &owner.module)]
                                .descriptor
                                .id]
                                .native_certification()?,
                            mode,
                            text(match availability.classification {
                                NativeInputClassification::CompilerRole => "compiler-original",
                                NativeInputClassification::PrivateInput => "private-native",
                            }),
                        ]))
                    })
                    .collect::<Result<Vec<_>, CompileError>>()?,
            ),
        ];
        let graph_paths = retained.graph_path_refs();
        let admitted_graphs = execution_scope
            .as_ref()
            .map(|scope| {
                let fields = row(scope, 2)?;
                list(&fields[0], 4096)?
                    .iter()
                    .map(|value| {
                        let fields = row(value, 2)?;
                        Ok(string(&fields[0])?.to_owned())
                    })
                    .collect::<Result<BTreeSet<_>, CompileError>>()
            })
            .transpose()?
            .unwrap_or_default();
        let acquisition = match transport {
            #[cfg(test)]
            RequestInputTransport::DurableFiles => Value::Array(vec![text("fresh-files")]),
            RequestInputTransport::OwnedArena => {
                let image_producer = CanonicalProducerIdentity::from_producer_bytes(producer).hex();
                let mut arenas = original_inputs::OriginalInputArenaTable::default();
                let images = metadata
                    .entries
                    .values()
                    .map(|entry| {
                        let row = selected_rows[&entry.descriptor.id];
                        let mut parts = row.original_input_parts(entry)?;
                        if let ArtifactPayload::Original(product) = &entry.payload {
                            if let Some(graph) = product
                                .execution_source()
                                .filter(|graph| admitted_graphs.contains(&hex(&graph.digest())))
                            {
                                parts.push(original_inputs::OriginalInputPart {
                                    kind: original_inputs::OriginalInputKind::Graph,
                                    transport: graph_paths[&graph.digest()].transport.clone(),
                                    path: graph_paths[&graph.digest()].path.clone(),
                                    sha256: hex(&graph.digest()),
                                    bytes: graph.bytes().len() as u64,
                                });
                            }
                        }
                        // Inherited rows own the unchanged materialized aliases. A new
                        // admission can strengthen provenance for equal bytes, so the
                        // receiving image observes the current selected payload owner.
                        let original_origins = match &entry.payload {
                            ArtifactPayload::Canonical(interface) => {
                                interface.original_input_origins()
                            }
                            ArtifactPayload::Original(product) => product
                                .module_interface()
                                .and_then(|interface| interface.original_input_origins()),
                            ArtifactPayload::Interface(..) => None,
                        };
                        original_inputs::encode_image(
                            &image_producer,
                            &entry.descriptor.owner.unit,
                            &entry.descriptor.owner.module,
                            parts,
                            original_origins,
                            &mut arenas,
                        )
                    })
                    .collect::<Result<Vec<_>, CompileError>>()?;
                arenas.acquisition(images)?
            }
        };
        let bytes = encode_scope_manifest_with_published(
            fields,
            execution_scope,
            authorization,
            self.published_scope_value()?,
            acquisition,
        )?;
        let manifest = root.join("exact-declaration-scope.cbor");
        use std::io::Write;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest)?;
        output.write_all(&bytes)?;
        tracing::info!(target: "tidepool_toolchain::artifacts", phase = "exact_request_references",
            reused_materialization = reused, manifest_written_bytes = bytes.len() as u64,
            selected_artifacts = artifacts.len(), "exact request references retained immutable products");
        Ok(ExactCompilationRequest {
            inputs: ExactRequestInputs::issue(self.clone(), scaffold.clone(), private, inputs),
            manifest,
            request_sha256: sha256(&bytes),
            semantic_sha256,
            producer_sha256:
                crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
                    .sha256(),
            artifacts,
            groups,
            materialization: Some(retained),
            program_support: None,
            program_source_lexical: Vec::new(),
            source_selected_support: BTreeSet::new(),
            source_search_include: None,
            checked_value_imports: Default::default(),
            generated_scaffold_imports: scaffold
                .native
                .iter()
                .map(|(role, templates)| GeneratedScaffoldImportAuthority {
                    role: GeneratedScaffoldRole::PlannedDeclaration(Arc::clone(role)),
                    protected_templates: Arc::clone(templates),
                })
                .collect(),
        })
    }
}

#[cfg(test)]
pub(crate) fn certified_product_artifact_view(
    producer: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    interfaces: &[crate::certified_products::CertifiedModuleInterface],
    baseline: Option<&ExactDeclarationContext>,
) -> Result<ArtifactView, CompileError> {
    certified_product_artifact_view_with_validation(
        producer,
        products,
        interfaces,
        &[],
        baseline.map(ExactDeclarationContext::artifact_view),
        crate::artifact_inventory::NativeArtifactDemand::AllGroups,
        &mut PackageInterfaceValidation::default(),
    )
}

// Product, target and inventory admission compare every witness against the
// same bounded capture; inventory construction must not create one filesystem
// observation per original module.
pub(crate) fn certified_product_artifact_view_with_validation(
    producer: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    interfaces: &[crate::certified_products::CertifiedModuleInterface],
    values: &[CertifiedValueInterface],
    baseline: Option<&ArtifactView>,
    demand: crate::artifact_inventory::NativeArtifactDemand<'_>,
    validation: &mut PackageInterfaceValidation,
) -> Result<ArtifactView, CompileError> {
    certified_product_artifact_view_inner(
        producer, products, interfaces, values, baseline, demand, validation, None,
    )
    .map(|admission| admission.artifacts)
}

pub(crate) fn certified_selected_product_artifact_view_with_validation(
    producer: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    interfaces: &[crate::certified_products::CertifiedModuleInterface],
    values: &[CertifiedValueInterface],
    baseline: Option<&ArtifactView>,
    demand: crate::artifact_inventory::NativeArtifactDemand<'_>,
    validation: &mut PackageInterfaceValidation,
    selection: &crate::certified_products::CertifiedSourceSelection,
) -> Result<crate::artifact_inventory::AdmittedTargetArtifacts, CompileError> {
    if !matches!(
        demand,
        crate::artifact_inventory::NativeArtifactDemand::VerifiedTarget { .. }
            | crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(_)
    ) {
        return Err(failure(
            "selected target requires authenticated target demand",
        ));
    }
    certified_product_artifact_view_inner(
        producer,
        products,
        interfaces,
        values,
        baseline,
        demand,
        validation,
        Some(selection),
    )?
    .into_target()
}

pub(crate) fn certified_product_artifact_view_inner(
    producer: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    interfaces: &[crate::certified_products::CertifiedModuleInterface],
    values: &[CertifiedValueInterface],
    baseline: Option<&ArtifactView>,
    demand: crate::artifact_inventory::NativeArtifactDemand<'_>,
    validation: &mut PackageInterfaceValidation,
    selection: Option<&crate::certified_products::CertifiedSourceSelection>,
) -> Result<crate::artifact_inventory::ArtifactAdmission, CompileError> {
    let view = baseline.map_or_else(|| ArtifactInventory::default().empty_view(), Clone::clone);
    let mut entries = interfaces
        .iter()
        .cloned()
        .map(ArtifactEntry::canonical)
        .map(Arc::new)
        .collect::<Vec<_>>();
    for value in values {
        if value.interface().toolchain_identity_sha256() != producer {
            return Err(failure("captured value has another compiler producer"));
        }
        entries.push(Arc::new(ArtifactEntry::interface(
            value.interface().clone(),
            JoinedInterfaceRole::ValueInterface,
            value.requirements().to_vec(),
        )));
    }
    let mut reused_originals = 0;
    let original_entry_work_start = validation.inventory.work_usage().ok().map(|work| work.0);
    for product in products {
        let entry =
            match view.retained_original_entry_with_validation(producer, product, validation)? {
                Some(entry) => {
                    reused_originals += 1;
                    entry
                }
                None => Arc::new(ArtifactEntry::original_with_validation(
                    producer,
                    product.clone(),
                    validation,
                )?),
            };
        entries.push(entry);
    }
    if let (Some(before), Ok((after, _))) =
        (original_entry_work_start, validation.inventory.work_usage())
    {
        tracing::debug!(target: "exomonad_harness::timing", phase = "products.original_entry_custody",
            originals = products.len(), reused_originals, original_entry_work = after - before,
            "original artifact entry custody");
    }
    view.inventory()
        .admit_product_artifacts(&view, entries, demand, selection)
}

/// Retain the physical request's complete private output custody and its
/// compiler-selected native resolver once. No executable group is demanded.
pub(crate) fn bind_certified_segment_artifacts_with_validation(
    originals: &crate::certified_products::CertifiedSegmentOriginals,
    baseline: &ArtifactView,
    validation: &mut PackageInterfaceValidation,
) -> Result<crate::artifact_inventory::BoundArtifactSelection, CompileError> {
    if !Arc::ptr_eq(originals.operation(), &validation.inventory) {
        return Err(failure("segment artifacts have another accounting owner"));
    }
    validation
        .inventory
        .reserve::<Arc<ArtifactEntry>>(
            originals.artifact_entries().len() + originals.value_entries().len(),
        )
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    let entries = originals
        .artifact_entries()
        .iter()
        .chain(originals.value_entries())
        .cloned()
        .collect();
    let offers = originals
        .source_selection()
        .issued_native_availability()
        .flat_map(|availability| availability.offers())
        .cloned()
        .collect::<Vec<_>>();
    let baseline = retain_native_offers(baseline, &offers)?;
    baseline
        .inventory()
        .bind_segment(&baseline, entries, originals.source_selection())
}

/// Each item adds only ordinal-visible values to its admitted prefix, then
/// resolves executable demand against the fixed physical selection.
pub(crate) fn certified_segment_artifact_view_with_validation(
    producer: [u8; 32],
    originals: &crate::certified_products::CertifiedSegmentOriginals,
    bound: &mut crate::artifact_inventory::BoundArtifactSelection,
    values: &[CertifiedValueInterface],
    cursor: &ArtifactView,
    demand: crate::artifact_inventory::NativeArtifactDemand<'_>,
    validation: &mut PackageInterfaceValidation,
    selection: &crate::certified_products::CertifiedSourceSelection,
) -> Result<crate::artifact_inventory::AdmittedTargetArtifacts, CompileError> {
    if !Arc::ptr_eq(originals.operation(), &validation.inventory) {
        return Err(failure("segment artifacts have another accounting owner"));
    }
    if values
        .iter()
        .any(|value| value.interface().toolchain_identity_sha256() != producer)
    {
        return Err(failure("captured value has another compiler producer"));
    }
    validation
        .inventory
        .reserve::<Arc<ArtifactEntry>>(values.len())
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    let admitted = cursor.artifact_ids().into_iter().collect::<BTreeSet<_>>();
    let visible = originals
        .selected_value_entries(values)
        .filter(|entry| !admitted.contains(&entry.descriptor.id))
        .cloned()
        .collect();
    cursor
        .inventory()
        .admit_bound_target(bound, cursor, demand, visible, selection)
}

/// Check the inventory's selected dependency closure without issuing group rows.
pub(crate) fn validate_artifact_view_groups_with_validation(
    view: &ArtifactView,
    validation: &mut PackageInterfaceValidation,
) -> Result<(), CompileError> {
    let metadata = view.metadata_snapshot();
    let available = metadata
        .artifacts
        .iter()
        .filter_map(|(id, entry)| match &entry.payload {
            ArtifactPayload::Original(product) => Some((*id, product)),
            _ => None,
        })
        .collect();
    crate::certified_products::validate_selected_owned_products_in_context_with_validation(
        &available,
        &metadata.selected_native_groups,
        validation,
    )
    .map_err(failure)
}

/// Admit only the inventory's selected dependency closure. Original byte custody
/// remains complete; previously admitted groups must survive unchanged.
#[cfg(test)]
pub(crate) fn certify_artifact_view_groups_with_validation(
    view: &ArtifactView,
    candidates: &[PendingCertifiedGroup],
    baseline: &[PendingCertifiedGroup],
    validation: &mut PackageInterfaceValidation,
) -> Result<Vec<PendingCertifiedGroup>, CompileError> {
    certify_artifact_view_groups(view, candidates, None, baseline, validation)
}

pub(crate) fn certify_issued_artifact_view_groups_with_validation(
    view: &ArtifactView,
    candidates: &crate::certified_products::IssuedGroupCandidates,
    baseline: &[PendingCertifiedGroup],
    validation: &mut PackageInterfaceValidation,
) -> Result<Vec<PendingCertifiedGroup>, CompileError> {
    certify_artifact_view_groups(view, candidates, Some(candidates), baseline, validation)
}

fn certify_artifact_view_groups(
    view: &ArtifactView,
    candidates: &[PendingCertifiedGroup],
    issued: Option<&crate::certified_products::IssuedGroupCandidates>,
    baseline: &[PendingCertifiedGroup],
    validation: &mut PackageInterfaceValidation,
) -> Result<Vec<PendingCertifiedGroup>, CompileError> {
    let metadata = view.metadata_snapshot();
    let entries = metadata.artifacts.values().cloned().collect::<Vec<_>>();
    let available = original_products_by_id(&entries);
    let mut original_ids = BTreeMap::new();
    for (artifact, product) in &available {
        original_ids
            .entry(product.owner())
            .or_insert_with(Vec::new)
            .push(*artifact);
    }
    #[derive(Clone, Copy, Debug)]
    enum GroupAdmissionPhase {
        Candidate,
        Baseline,
    }
    let selected_key = |phase: GroupAdmissionPhase, group: &PendingCertifiedGroup| {
        let artifact = match original_ids.get(group.owner()).map(Vec::as_slice) {
            Some([artifact]) => *artifact,
            Some(artifacts) => {
                return Err(failure(
                    crate::certified_products::CertificationError::OriginalSelectionConflict(
                        Box::new(crate::certified_products::OriginalSelectionConflict {
                            selected: group.owner().clone(),
                            artifacts: artifacts.to_vec(),
                        }),
                    ),
                ));
            }
            None => return Err(failure(format!(
                "certified group has no exact original artifact: phase={phase:?} owner={:?} ordinal={} selected_groups={} available_originals={}",
                group.owner(), group.group().original_ordinal(), metadata.selected_native_groups.len(), available.len(),
            ))),
        };
        Ok(crate::artifact_inventory::NativeGroupKey {
            artifact,
            original_ordinal: group.group().original_ordinal(),
        })
    };
    let mut current = Vec::new();
    let mut current_positions = BTreeMap::new();
    for group in candidates {
        let key = match issued {
            Some(issued) => crate::artifact_inventory::NativeGroupKey {
                artifact: issued.artifact(group),
                original_ordinal: group.group().original_ordinal(),
            },
            None => selected_key(GroupAdmissionPhase::Candidate, group)?,
        };
        if metadata.selected_native_groups.contains(&key) {
            if selected_key(GroupAdmissionPhase::Candidate, group)? != key {
                return Err(failure(
                    "selected candidate differs from its original issuer",
                ));
            }
            // Baseline comparison uses the first candidate; certification below
            // retains responsibility for duplicate-current refusal.
            current_positions.entry(key).or_insert(current.len());
            current.push(group.clone());
        }
    }
    let mut baseline_keys = BTreeSet::new();
    for group in baseline {
        let key = selected_key(GroupAdmissionPhase::Baseline, group)?;
        if !baseline_keys.insert(key) || !metadata.selected_native_groups.contains(&key) {
            return Err(failure(
                "artifact view removed or duplicated a previously selected group",
            ));
        }
        if let Some(position) = current_positions.get(&key) {
            let candidate = &current[*position];
            if candidate.group() != group.group() || candidate.imports() != group.imports() {
                return Err(failure("artifact view changed a previously selected group"));
            }
        } else {
            current_positions.insert(key, current.len());
            current.push(group.clone());
        }
    }
    let additional = certify_selected_owned_products_in_context_with_validation(
        &available,
        &current,
        &metadata.selected_native_groups,
        validation,
    )
    .map_err(failure)?;
    current.extend(additional);
    Ok(current)
}

#[tracing::instrument(
    target = "exomonad_harness::timing",
    name = "products.certified_artifact_view",
    level = "debug",
    skip_all,
    fields(inclusive = true)
)]
pub(crate) fn certified_artifact_view(
    originals: &crate::certified_products::SelectedOriginalClosure,
    baseline: &ExactDeclarationContext,
) -> Result<crate::artifact_inventory::CompilerSourceSupport, CompileError> {
    let _span = tracing::debug_span!(target: "exomonad_harness::timing", "products.certified_artifact_view.merge", inclusive = true).entered();
    baseline
        .compiler_source_support()
        .certify_original_closure(originals)
}

fn text(value: impl Into<String>) -> Value {
    Value::Text(value.into())
}
fn module_value(owner: &ExactModuleIdentity) -> Value {
    Value::Array(vec![text(&owner.unit), text(&owner.module)])
}
fn path_value(path: &Path) -> Result<Value, CompileError> {
    path.to_str()
        .map(text)
        .ok_or_else(|| failure("non-UTF-8 artifact path"))
}
fn symbol_value(symbol: &tidepool_repr::execution_schema::SymbolIdentity) -> Value {
    Value::Array(vec![
        text(&symbol.unit),
        text(&symbol.module),
        text(&symbol.namespace),
        text(&symbol.occurrence),
        symbol.record_parent.as_ref().map_or(Value::Null, text),
    ])
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// Compose selections by exact owner and unordered import set. Duplicate
/// edges inside an individual input remain malformed, and differing sets
/// for one owner never acquire authority by union.
fn compose_lexical_nodes<'a>(
    nodes: impl IntoIterator<Item = &'a ExactLexicalNode>,
) -> Result<Vec<ExactLexicalNode>, CompileError> {
    let mut selected: BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>> = BTreeMap::new();
    for node in nodes {
        let mut imports = node.imports.clone();
        imports.sort();
        if imports.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(failure("duplicate selected lexical import"));
        }
        if let Some(previous) = selected.get(&node.owner) {
            if previous != &imports {
                return Err(failure(format!(
                    "conflicting selected lexical imports for {}:{}",
                    node.owner.unit, node.owner.module
                )));
            }
        } else {
            selected.insert(node.owner.clone(), imports);
        }
    }
    Ok(selected
        .into_iter()
        .map(|(owner, imports)| ExactLexicalNode { owner, imports })
        .collect())
}

#[cfg(test)]
pub(crate) use tests::{
    assert_source_selected_receipt_pairing, generated_source_admission_fixture,
};

#[cfg(test)]
pub(crate) use native_availability_tests::assert_selected_authored_private_inputs;

#[cfg(test)]
mod native_availability_tests;

#[cfg(test)]
mod candidate_offer_history_tests;

#[cfg(test)]
pub(crate) fn issue_fixture_compiler_support(
    projection: CompilerInputProjection,
    artifacts: ArtifactView,
) -> Result<crate::artifact_inventory::CompilerSourceSupport, CompileError> {
    let mut graph = artifacts.capture_graph_selection();
    let selected = projection
        .roles()
        .iter()
        .flat_map(|role| std::iter::once(role.interface()).chain(role.original()))
        .collect::<BTreeSet<_>>();
    graph
        .namespace
        .retain(|binding| selected.contains(&binding.artifact));
    let inventory = RecoveredArtifactInventory {
        producer: artifacts
            .descriptors()
            .first()
            .map_or([0; 32], |entry| entry.producer_sha256),
        entries: artifacts
            .entries()
            .into_iter()
            .map(|entry| (entry.descriptor.id, entry))
            .collect(),
        recorded_inventory: true,
        bindings: graph.bindings.iter().copied().collect(),
        binding_edges: artifacts.binding_dependencies(),
        interfaces: artifacts.interface_dependencies(),
    };
    let checked = inventory.authenticate_checked_graph_selection(&graph)?;
    if checked.projection != projection {
        return Err(failure(
            "checked fixture projection differs from issued namespace",
        ));
    }
    crate::artifact_inventory::CompilerSourceSupport::from_recovered_selection(checked)
}

/// Feed fixture-issued payloads and scoped facts through the recorded recovery
/// ingress; malformed associations remain untrusted until authentication.
#[cfg(test)]
pub(crate) fn recover_fixture_compiler_support(
    projection: CompilerInputProjection,
    artifacts: ArtifactView,
    offers: impl IntoIterator<Item = crate::artifact_inventory::IssuedNativeOffer>,
) -> Result<crate::artifact_inventory::CompilerSourceSupport, CompileError> {
    let mut graph = artifacts.capture_graph_selection();
    let roles = projection.roles();
    let selected = roles
        .iter()
        .flat_map(|role| std::iter::once(role.interface()).chain(role.original()))
        .collect::<BTreeSet<_>>();
    graph
        .namespace
        .retain(|binding| selected.contains(&binding.artifact));
    let inventory = RecoveredArtifactInventory {
        producer: artifacts
            .descriptors()
            .first()
            .map_or([0; 32], |entry| entry.producer_sha256),
        entries: artifacts
            .entries()
            .into_iter()
            .map(|entry| (entry.descriptor.id, entry))
            .collect(),
        recorded_inventory: true,
        bindings: graph.bindings.iter().copied().collect(),
        binding_edges: artifacts.binding_dependencies(),
        interfaces: artifacts.interface_dependencies(),
    };
    crate::artifact_inventory::CompilerSourceSupport::from_recovered_selection(
        inventory.authenticate_graph_selection(&graph, &roles)?,
    )?
    .retain_offers(offers)
}

#[cfg(test)]
mod tests;
