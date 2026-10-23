//! Owned execution intent and compiler receipts for paired publication.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidepool_codegen::binding_table::{PreparedWorldExecutionSelection, SourceLeaseKey};
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::{Generation, SessionModule, SessionVarId};
use tidepool_toolchain::compiler_world::CompletedPrivateCompilerProgress;
use tidepool_toolchain::declaration_join::{
    AcceptedJoin, CertifiedAuthoredDeclaration, CertifiedPublicationJoin, DeclarationExport,
    DeclarationWrite, ExactDeclarationContext, ExactModuleIdentity, ExportIdentity,
    ExportNamespace, InstanceInventory, JoinDecision, JoinRejection, ModuleSnapshot,
    PublicationComposition, RejectedJoin, ReservedJoin,
};

use super::persistent::PersistentSession;
use super::render::{
    extend_exports_by_head, AdmittedDeclarationSurface, DeclTurn, JoinedDeclaration,
};
use super::{
    recovery, DeclarationSource, PublicManifestBase, PublicVisibilitySnapshot, RecoveryPublicOwner,
    SessionError, SessionLib, StagedPublicManifest,
};

#[derive(Clone)]
pub(super) struct DeclarationTip {
    pub(super) generation: Generation,
    pub(super) owner: ExactModuleIdentity,
    pub(super) turn: DeclTurn,
    pub(super) context: Arc<ExactDeclarationContext>,
    pub(super) surface: AdmittedDeclarationSurface,
    pub(super) exports: Vec<DeclarationExport>,
    pub(super) instances: InstanceInventory,
    pub(super) families: Vec<ExportIdentity>,
}

#[derive(Clone)]
struct AuthoredWrite {
    generation: Generation,
    turn: DeclTurn,
    evidence: Arc<CertifiedAuthoredDeclaration>,
    retractions: Vec<ExportIdentity>,
}

/// Immutable final execution writes. A public retry changes only its merge
/// baseline, never this private suffix, exact winners, leases, or effects.
/// Each semantic certification owns a causal join reservation; storage retries
/// retain that identity and its receipt.
/// The private scope keeps live roots owned until publication or abandonment.
pub struct FrozenWorldExecution {
    owner_stamp: super::OwnerStamp,
    library: uuid::Uuid,
    private_view: super::PublicationViewWitness,
    durable_owner: Option<RecoveryPublicOwner>,
    _private_scope_lease: Arc<super::RuntimeLexicalScopeLease>,
    admitted: PublicVisibilitySnapshot,
    admitted_context: Option<Arc<ExactDeclarationContext>>,
    private: PublicVisibilitySnapshot,
    private_base: Option<DeclarationTip>,
    declarations: FrozenDeclarations,
    write_ids: Vec<SessionVarId>,
    source_keys: Vec<SourceLeaseKey>,
    head_replacements: Vec<DeclarationHeadReplacement>,
    _completed_values: Vec<super::admission::CertifiedPrivateValueProof>,
    world: WorldExecutionIntent,
    pub(super) binding_policy: super::BindingPublicationPolicy,
}

pub(super) enum FrozenBindingSelection<'a> {
    Completed,
    ActivationInput(&'a super::RuntimeActivationPreviewAdmission),
}

/// Declaration facts are retained together; binding-only execution carries no
/// declaration projection or surface assembled independently of its writes.
enum FrozenDeclarations {
    Bindings,
    Declarations {
        writes: Vec<AuthoredWrite>,
        projection: FrozenDeclarationProjection,
        surface: AdmittedDeclarationSurface,
    },
}

enum FrozenDeclarationProjection {
    Retained(Arc<super::CertifiedDeclarationProjection>),
    NeedsCertification,
}

/// The exact published World association and admitted source facts a final
/// publication must revalidate before installing anything. Cloning shares the
/// same association, source and lease Arcs; it issues no compiler authority.
#[derive(Clone)]
pub(super) struct WorldExecutionIntent {
    pub(super) association: Arc<super::inspection::InspectionWorldAssociation>,
    pub(super) source: Arc<super::GenerationArtifactReference>,
    pub(super) original_source: Arc<super::RuntimeOriginalSource>,
    pub(super) target: Arc<super::RuntimeLexicalScopeLease>,
    pub(super) private: Arc<super::inspection::InspectionWorldAssociation>,
    /// The exact typed progress the private execution actually produced.
    /// An installed no-cell invocation is `Unchanged`; a completed cell is
    /// `Checked`; started, refused or uncertain work cannot issue this evidence.
    pub(super) progress: CompletedPrivateCompilerProgress,
    /// The exact private source-selection suffix, issued from the private
    /// execution's actual admissions. Publication composes it over the latest
    /// public selection without treating the frozen public base as current.
    pub(super) selection: PreparedWorldExecutionSelection,
}

/// Name-winning publication is separate from an exact withdrawal. A runtime
/// value replaces the current term head even when another execution replaced
/// the admitted declaration while this execution was suspended.
#[derive(Clone)]
struct DeclarationHeadReplacement {
    namespace: ExportNamespace,
    occurrence: String,
}

/// Captured causal facts before selecting whether this attempt needs a join.
struct CapturedDeclarationPublication {
    public: PublicManifestBase,
    intent: Arc<FrozenWorldExecution>,
    current_public: Option<DeclarationTip>,
    public_context: Option<Arc<ExactDeclarationContext>>,
    surface: AdmittedDeclarationSurface,
}

/// Issued once and consumed by a declaration certificate. Abandonment burns it.
pub struct JoinReservation {
    generation: Generation,
}
static_assertions::assert_not_impl_any!(JoinReservation: Clone, Copy);

/// One current public graph paired with the unchanged final execution intent.
pub struct DeclarationPublicationBase {
    pub(super) public: PublicManifestBase,
    reservation: JoinReservation,
    intent: Arc<FrozenWorldExecution>,
    current_public: Option<DeclarationTip>,
    public_context: Option<Arc<ExactDeclarationContext>>,
    surface: AdmittedDeclarationSurface,
    includes: Vec<PathBuf>,
    session_root: PathBuf,
    expected_public_version: String,
}

/// One stage for the same fixed final intent and atomic publication owner.
pub enum ExecutionPublication {
    Bindings(PublicManifestBase),
    Declarations(DeclarationPublicationBase),
}

pub enum CertifiedDeclarationPublication {
    Accepted(AcceptedDeclarationPublication),
    Rejected(RejectedDeclarationPublication),
}

pub struct AcceptedDeclarationPublication {
    pub(super) base: DeclarationPublicationBase,
    receipt: PublicationEvidence,
}

/// A publication keeps the compiler's nominal interface identity. Reusing a
/// projection binds it to a new logical publication generation, not a new
/// compiler request or a renamed interface.
#[derive(Clone, Debug)]
pub(crate) enum PublicationEvidence {
    NewJoin(PublicationComposition),
    ReusedProjection {
        generation: Generation,
        projection: Arc<super::CertifiedDeclarationProjection>,
    },
}

impl PublicationEvidence {
    pub(super) fn composition(&self) -> &PublicationComposition {
        match self {
            Self::NewJoin(composition) => composition,
            Self::ReusedProjection { projection, .. } => projection.composition(),
        }
    }

    pub(super) fn receipt(&self) -> &Arc<AcceptedJoin> {
        match self {
            Self::NewJoin(composition) => composition.join(),
            Self::ReusedProjection { projection, .. } => projection.receipt(),
        }
    }

    pub(super) fn matches_generation(&self, generation: Generation) -> bool {
        match self {
            Self::NewJoin(composition) => {
                composition.join().reserved().module == SessionModule::lib(generation).module_name()
            }
            Self::ReusedProjection {
                generation: selected,
                ..
            } => *selected == generation,
        }
    }

    pub(super) fn projection(&self) -> Option<&Arc<super::CertifiedDeclarationProjection>> {
        match self {
            Self::ReusedProjection { projection, .. } => Some(projection),
            Self::NewJoin(_) => None,
        }
    }
}

impl std::ops::Deref for PublicationEvidence {
    type Target = AcceptedJoin;

    fn deref(&self) -> &Self::Target {
        self.receipt()
    }
}

pub struct RejectedDeclarationPublication {
    base: DeclarationPublicationBase,
    receipt: RejectedJoin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeclarationPublicationRejection {
    Stale,
    Rejected {
        reason: JoinRejection,
        diagnostic: String,
    },
}

pub(super) struct PreparedResidentDeclaration {
    scope: ScopeId,
    tip: super::ScopeDeclarationState,
    log: super::render::PreparedDeclarationCommit,
}

pub(super) struct PreparedDeclarationPublication {
    pub(super) generation: Generation,
    pub(super) joined: JoinedDeclaration,
    pub(super) declared_names: Vec<String>,
    pub(super) source_domain_owners: Vec<tidepool_repr::execution_schema::CachedHomeOwner>,
}

fn invalid_at(path: &Path, detail: impl Into<String>) -> SessionError {
    SessionError::RecoveryManifest {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

fn invalid(base: &PublicManifestBase, detail: impl Into<String>) -> SessionError {
    invalid_at(&base.path, detail)
}

fn module_owner(certificate: &CertifiedAuthoredDeclaration) -> ExactModuleIdentity {
    let owner = certificate.product().owner();
    ExactModuleIdentity {
        unit: owner.unit.clone(),
        module: owner.module.clone(),
    }
}

pub(super) fn tip(
    lib: &SessionLib,
    generation: Generation,
) -> Result<Option<DeclarationTip>, SessionError> {
    if generation == Generation(0) {
        return Ok(None);
    }
    let turn = lib
        .log
        .turn(generation)
        .ok_or(SessionError::StaleStagedDeclaration)?
        .clone();
    let context = lib
        .log
        .joined_context_at(generation)
        .ok_or_else(|| invalid_at(&lib.root, "declaration tip lacks a protected exact context"))?;
    let surface = lib
        .log
        .admitted_surface_at(generation)
        .ok_or_else(|| {
            invalid_at(
                &lib.root,
                "declaration tip lacks its admitted lexical surface",
            )
        })?
        .clone();
    let (owner, exports, instances, families) = if let Some(joined) = lib.log.joined_at(generation)
    {
        (
            ExactModuleIdentity {
                unit: joined.evidence.reserved().unit.clone(),
                module: joined.evidence.reserved().module.clone(),
            },
            joined.evidence.exports().to_vec(),
            joined.evidence.instances().clone(),
            joined.evidence.family_closure().to_vec(),
        )
    } else if let Some(recovered) = lib.log.recovered_at(generation) {
        (
            recovered.evidence.root().clone(),
            recovered.evidence.exports().to_vec(),
            recovered.evidence.instances().clone(),
            recovered.evidence.family_closure().to_vec(),
        )
    } else if let Some(authored) = lib.log.certified_authored_at(generation) {
        let projection = lib.log.projection_at(generation).ok_or_else(|| {
            invalid_at(
                &lib.root,
                "authored tip lacks its admitted lexical projection",
            )
        })?;
        (
            module_owner(authored),
            projection.receipt().exports().to_vec(),
            projection.receipt().instances().clone(),
            projection.receipt().family_closure().to_vec(),
        )
    } else {
        return Err(invalid_at(
            &lib.root,
            "declaration tip has no compiler certificate",
        ));
    };
    Ok(Some(DeclarationTip {
        generation,
        owner,
        turn,
        context,
        surface,
        exports,
        instances,
        families,
    }))
}

fn snapshot_digest(snapshot: &PublicVisibilitySnapshot) -> serde_json::Value {
    serde_json::json!({
        "scope": snapshot.scope.0, "epoch": snapshot.epoch,
        "declaration_tip": snapshot.declaration_tip.0,
        "machine_incarnation": snapshot.machine_incarnation.map(|id| id.0),
        "bindings": snapshot.bindings.iter().map(|(name, id)| (name, id.raw())).collect::<Vec<_>>(),
        "source_instances": snapshot.source_instances.iter().map(|key| {
            serde_json::json!({"instance": key.instance.raw(), "value": key.value.0})
        }).collect::<Vec<_>>(),
    })
}

fn paired_version(base: &PublicManifestBase) -> String {
    let target = match &base.target {
        super::PublicPublicationBaseline::Durable(write) => serde_json::json!({
            "mode": "durable", "owner": write.owner, "domain": write.domain }),
        super::PublicPublicationBaseline::Ephemeral => serde_json::json!({ "mode": "ephemeral" }),
    };
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema": "tidepool-paired-declaration-baseline-v4", "session": base.session.0,
        "owner": base.owner_stamp.owner.identity(), "epoch": base.owner_stamp.epoch,
        "target": target, "library": base.semantic.library,
        "public": snapshot_digest(&base.semantic.expected_public), "private": snapshot_digest(&base.semantic.expected_private),
        "public_view": base.semantic.public_view.version_fact(), "private_view": base.semantic.private_view.version_fact(),
    })).expect("paired baseline contains serializable identities");
    blake3::hash(&bytes).to_hex().to_string()
}

pub(super) fn exact_retractions(
    lib: &SessionLib,
    parent: Generation,
    names: &[super::DeclarationRetraction],
) -> Result<Vec<ExportIdentity>, SessionError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let parent = tip(lib, parent)?
        .ok_or_else(|| invalid_at(&lib.root, "retraction has no exact declaration baseline"))?;
    let mut exact = Vec::new();
    for name in names {
        let selected = parent
            .exports
            .iter()
            .filter(|export| name.selects(export.head.namespace, &export.head.occurrence))
            .map(|export| export.head.clone())
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Err(invalid_at(
                &lib.root,
                "retraction lacks its selected exact head",
            ));
        }
        exact.extend(selected);
    }
    exact.sort();
    exact.dedup();
    Ok(exact)
}

pub(super) fn declaration_value_members(
    lib: &SessionLib,
    generation: Generation,
) -> Result<BTreeSet<String>, SessionError> {
    Ok(tip(lib, generation)?
        .into_iter()
        .flat_map(|tip| tip.exports)
        .filter(|export| export.head.namespace == ExportNamespace::Type)
        .flat_map(|export| export.children)
        .filter(|identity| identity.namespace == ExportNamespace::Value)
        .map(|identity| identity.occurrence)
        .collect())
}

pub(super) fn recovery_instances(
    instances: &InstanceInventory,
    closure: &[ExportIdentity],
) -> recovery::RecoveryInstanceInventory {
    recovery::RecoveryInstanceInventory {
        classes: instances
            .classes
            .iter()
            .map(|instance| recovery::RecoveryInstanceEvidence {
                dfun: super::authored_identity(&instance.dfun),
                class: super::authored_identity(&instance.class),
                selected: true,
                selected_axioms: instance
                    .selected_axioms
                    .iter()
                    .map(super::authored_identity)
                    .collect(),
            })
            .collect(),
        selected_family_axioms: instances
            .families
            .iter()
            .map(super::authored_identity)
            .collect(),
        family_consistency_closure: closure.iter().map(super::authored_identity).collect(),
    }
}

pub(super) fn merge_instances(
    path: &Path,
    mut selected: InstanceInventory,
    addition: &InstanceInventory,
) -> Result<InstanceInventory, SessionError> {
    for instance in &addition.classes {
        if let Some(prior) = selected
            .classes
            .iter()
            .find(|prior| prior.dfun == instance.dfun)
        {
            if prior != instance {
                return Err(invalid_at(
                    path,
                    "exact dfun has conflicting class or associated-family evidence",
                ));
            }
        } else {
            selected.classes.push(instance.clone());
        }
    }
    selected.classes.sort();
    selected.families.extend_from_slice(&addition.families);
    selected.families.sort();
    selected.families.dedup();
    Ok(selected)
}

impl FrozenWorldExecution {
    fn writes(&self) -> &[AuthoredWrite] {
        match &self.declarations {
            FrozenDeclarations::Bindings => &[],
            FrozenDeclarations::Declarations { writes, .. } => writes,
        }
    }

    /// Names observed from the exact native identities selected for publication.
    pub fn native_binding_names(&self) -> Vec<String> {
        self.private
            .bindings
            .iter()
            .filter(|(_, id)| self.write_ids.contains(id))
            .map(|(name, _)| name.clone())
            .collect()
    }

    #[cfg(test)]
    pub(super) fn native_write_ids(&self) -> &[SessionVarId] {
        &self.write_ids
    }

    pub fn admitted_public(&self) -> &PublicVisibilitySnapshot {
        &self.admitted
    }
    pub fn private_scope(&self) -> ScopeId {
        self.private.scope
    }
    pub(super) fn admitted_scope(&self) -> ScopeId {
        self.admitted.scope
    }
    pub(super) fn admitted_context(&self) -> Option<&Arc<ExactDeclarationContext>> {
        self.admitted_context.as_ref()
    }
    pub(super) fn private_snapshot(&self) -> &PublicVisibilitySnapshot {
        &self.private
    }
    pub(super) fn private_view_is_current(&self, session: &PersistentSession) -> bool {
        self.private_view.is_current(session, self.private.scope)
    }
    pub(super) fn library(&self) -> uuid::Uuid {
        self.library
    }
    pub(super) fn retained_private_scope(&self) -> &Arc<super::RuntimeLexicalScopeLease> {
        &self._private_scope_lease
    }

    pub(super) fn world(&self) -> &WorldExecutionIntent {
        &self.world
    }
    pub(super) fn owner_matches(
        &self,
        owner: &Arc<super::admission::RuntimeAdmissionOwner>,
    ) -> bool {
        self.owner_stamp.matches(owner)
    }
}

impl PersistentSession {
    /// Freeze the final writes selected by the completed private execution owner.
    pub fn freeze_private_execution(
        &mut self,
        admission: &super::PrivateExecutionAdmission,
    ) -> Result<Arc<FrozenWorldExecution>, SessionError> {
        self.freeze_private_execution_with_selection(admission, FrozenBindingSelection::Completed)
    }

    pub(super) fn freeze_private_execution_with_selection(
        &mut self,
        admission: &super::PrivateExecutionAdmission,
        selection: FrozenBindingSelection<'_>,
    ) -> Result<Arc<FrozenWorldExecution>, SessionError> {
        if !Arc::ptr_eq(&admission.owner, self.admission_owner())
            || admission.owner_epoch != self.admission_owner().epoch()
            || !self.has_lib()
            || admission.session() != self.lib().session_id()
            || admission.library != self.lib().compile_view_identity
        {
            return Err(super::SessionTransitionReason::OwnerChanged.into());
        }
        if !self.scope_tree().is_live(admission.private_scope()) {
            return Err(super::SessionTransitionReason::ScopeRevoked.into());
        }
        let transition = admission.begin_freeze()?;
        self.reconcile_private_visible_hosts(admission, true)?;
        let world = self
            .lib()
            .tips
            .get(&admission.private_scope())
            .and_then(|tip| tip.inspection_world.as_ref())
            .ok_or(SessionError::WorldCellPublicationBaseStale)?
            .world
            .clone();
        let completed = admission.completed_frontier(&world)?;
        let facts = admission.world_execution_facts();
        let private_lease = completed.scope_lease().clone();
        let result = super::RuntimeOriginalSource::with_sources_and_targets(
            [facts.original_source],
            [private_lease, facts.public_target],
            |sources, _| {
                super::RuntimeSourceAdmission::with_open_admissions(
                    sources.iter().map(|source| source.source_admission()),
                    || self.freeze_completed_execution(completed, selection),
                )
            },
        )?;
        if result.is_ok() {
            if let Some(transition) = transition {
                transition.commit();
            }
        }
        result
    }

    fn freeze_completed_execution(
        &mut self,
        completed: super::admission::CompletedPrivateFrontier<'_>,
        selection: FrozenBindingSelection<'_>,
    ) -> Result<Arc<FrozenWorldExecution>, SessionError> {
        let admission = completed.admission();
        if !Arc::ptr_eq(&admission.owner, self.admission_owner())
            || admission.owner_epoch != self.admission_owner().epoch()
            || !self.has_lib()
            || admission.session() != self.lib().session_id()
            || admission.library != self.lib().compile_view_identity
        {
            return Err(super::SessionTransitionReason::OwnerChanged.into());
        }
        let private_scope = admission.private_scope();
        let private_scope_lease = completed.scope_lease().clone();
        let facts = admission.world_execution_facts();
        let private_world = self
            .lib()
            .tips
            .get(&private_scope)
            .and_then(|tip| tip.inspection_world.clone())
            .ok_or(SessionError::WorldCellPublicationBaseStale)?;
        if !self.inspection_world_base_is_current(
            &private_world.base,
            &facts.source,
            &facts.original_source,
        ) || !private_world.base.target.matches(&private_scope_lease)
        {
            return Err(SessionError::WorldCellPublicationBaseStale);
        }
        let private_visibility = self
            .public_visibility_snapshot_in(private_scope)
            .ok_or(super::SessionTransitionReason::ScopeRevoked)?;
        let completed_values = admission.completed_values.lock();
        let (mut write_ids, binding_policy) = match selection {
            FrozenBindingSelection::Completed => {
                let mut writes = Vec::new();
                for (_, id) in &private_visibility.bindings {
                    let entry = self
                        .bindings()
                        .get(*id)
                        .ok_or(super::SessionTransitionReason::RetainedValueUnavailable)?;
                    if entry.scope != private_scope {
                        continue;
                    }
                    if let Some(proof) = completed_values.get(id) {
                        if !proof.matches(entry) {
                            return Err(super::SessionTransitionReason::PreparedValueChanged.into());
                        }
                        writes.push(*id);
                    }
                }
                (writes, super::BindingPublicationPolicy::ExactPromotion)
            }
            FrozenBindingSelection::ActivationInput(input) => {
                if input.source_lease().scope() != private_scope {
                    return Err(super::SessionTransitionReason::ScopeRevoked.into());
                }
                (
                    vec![input.binding()],
                    super::BindingPublicationPolicy::ActivationInput,
                )
            }
        };
        write_ids.sort_by_key(|id| id.raw());
        write_ids.dedup();
        let source_keys = private_visibility.source_instances.clone();
        if let Some(intent) = admission.final_intent.get() {
            if intent.private != private_visibility
                || !intent.private_view_is_current(self)
                || !Arc::ptr_eq(&intent.world.private, &private_world)
                || intent.binding_policy != binding_policy
                || intent.write_ids != write_ids
            {
                return Err(super::SessionTransitionReason::WrongPhase.into());
            }
            return Ok(intent.clone());
        }
        let world_selection = self
            .bindings()
            .prepare_world_execution_selection_delta(
                self.scope_tree(),
                private_scope,
                &private_visibility.source_selection,
            )
            .map_err(SessionError::InvalidPublicBindingPromotion)?;
        let world = WorldExecutionIntent {
            association: facts.published,
            source: facts.source,
            original_source: facts.original_source,
            target: facts.public_target,
            private: private_world,
            progress: completed.publication(),
            selection: world_selection,
        };
        let admitted = &facts.admitted;
        let admitted_context = facts.admitted_context;
        let durable_owner = admission.durable_owner.clone();
        if private_scope == admitted.scope || !self.scope_tree().is_live(private_scope) {
            return Err(SessionError::DeadScope(private_scope));
        }
        let private = private_visibility;
        let lib = self.lib();
        let chain = lib.log.chain_from_root(private.declaration_tip);
        let start = if admitted.declaration_tip == Generation(0) {
            0
        } else {
            chain
                .iter()
                .position(|generation| *generation == admitted.declaration_tip)
                .ok_or_else(|| {
                    invalid_at(
                        &lib.root,
                        "private declaration suffix does not descend from its admitted base",
                    )
                })?
                + 1
        };
        let private_base = tip(lib, admitted.declaration_tip)?;
        let mut writes = Vec::new();
        for generation in &chain[start..] {
            let evidence = lib
                .log
                .certified_authored_arc_at(*generation)
                .ok_or_else(|| {
                    invalid_at(
                        &lib.root,
                        "private suffix contains an uncertified authored declaration",
                    )
                })?;
            let turn = lib
                .log
                .turn(*generation)
                .ok_or(SessionError::StaleStagedDeclaration)?
                .clone();
            let retractions =
                exact_retractions(lib, turn.parent.unwrap_or(Generation(0)), &turn.retracts)?;
            writes.push(AuthoredWrite {
                generation: *generation,
                turn,
                evidence,
                retractions,
            });
        }
        for id in &write_ids {
            if self
                .bindings()
                .get(*id)
                .is_none_or(|entry| entry.scope != private_scope)
            {
                return Err(SessionError::InvalidPublicBindingPromotion(
                    tidepool_codegen::binding_table::BindingPromotionError::MissingOrForeignBinding,
                ));
            }
        }
        let current_ids = private
            .bindings
            .iter()
            .map(|(_, id)| id.raw())
            .collect::<BTreeSet<_>>();
        let mut write_ids = write_ids
            .into_iter()
            .filter(|id| current_ids.contains(&id.raw()))
            .collect::<Vec<_>>();
        write_ids.sort_by_key(|id| id.raw());
        write_ids.dedup();
        let final_declarations = tip(lib, private.declaration_tip)?;
        let final_projection = lib
            .log
            .projection_at(private.declaration_tip)
            .filter(|projection| {
                lib.current_exact_context_in(private_scope)
                    .as_ref()
                    .is_some_and(|current| current.same_authenticated_context(projection.context()))
            })
            .cloned();
        if write_ids.iter().any(|id| {
            self.bindings().get(*id).is_some_and(|entry| {
                final_declarations.as_ref().is_some_and(|tip| {
                    tip.exports.iter().any(|export| {
                        std::iter::once(&export.head)
                            .chain(export.children.iter())
                            .any(|identity| {
                                identity.namespace != ExportNamespace::Type
                                    && identity.occurrence == entry.name.0
                            })
                    })
                }) && completed_values
                    .get(id)
                    .is_none_or(|proof| !proof.matches(entry))
            })
        }) {
            return Err(invalid_at(
                &lib.root,
                "a final value write overlaps a retained declaration without certified private Value ownership or an exact retraction",
            ));
        }
        let source_keys = source_keys
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let head_replacements = write_ids
            .iter()
            .map(|id| {
                let entry = self
                    .bindings()
                    .get(*id)
                    .expect("final binding was validated");
                DeclarationHeadReplacement {
                    namespace: ExportNamespace::Value,
                    occurrence: entry.name.0.clone(),
                }
            })
            .collect();
        self.bindings()
            .validate_exact_publication_in(
                self.scope_tree(),
                private_scope,
                admitted.scope,
                &write_ids,
                &source_keys,
            )
            .map_err(SessionError::InvalidPublicBindingPromotion)?;
        let intent = Arc::new(FrozenWorldExecution {
            owner_stamp: super::OwnerStamp::capture(self.admission_owner()),
            library: lib.compile_view_identity,
            private_view: super::PublicationViewWitness::World {
                key: world.private.base.key.clone(),
                logical: *world.private.world.logical_id(),
            },
            durable_owner,
            _private_scope_lease: private_scope_lease,
            admitted: admitted.clone(),
            admitted_context,
            private,
            private_base,
            declarations: if writes.is_empty() {
                FrozenDeclarations::Bindings
            } else {
                FrozenDeclarations::Declarations {
                    writes,
                    projection: final_projection.map_or(
                        FrozenDeclarationProjection::NeedsCertification,
                        FrozenDeclarationProjection::Retained,
                    ),
                    surface: final_declarations
                        .ok_or(SessionError::StaleStagedDeclaration)?
                        .surface,
                }
            },
            _completed_values: write_ids
                .iter()
                .filter_map(|id| completed_values.get(id).map(|proof| proof.proof.clone()))
                .collect(),
            write_ids,
            source_keys,
            head_replacements,
            world,
            binding_policy,
        });
        assert!(
            admission.final_intent.set(intent.clone()).is_ok(),
            "exclusive session checkout finalizes an admission once"
        );
        Ok(intent)
    }

    /// Capture only the latest public merge baseline for a fixed execution.
    pub fn restage_execution_publication(
        &mut self,
        owner: RecoveryPublicOwner,
        intent: Arc<FrozenWorldExecution>,
    ) -> Result<ExecutionPublication, SessionError> {
        self.restage_execution_target(Some(owner), intent)
    }

    pub fn restage_ephemeral_execution_publication(
        &mut self,
        intent: Arc<FrozenWorldExecution>,
    ) -> Result<ExecutionPublication, SessionError> {
        self.restage_execution_target(None, intent)
    }

    fn restage_execution_target(
        &mut self,
        owner: Option<RecoveryPublicOwner>,
        intent: Arc<FrozenWorldExecution>,
    ) -> Result<ExecutionPublication, SessionError> {
        let base = self.capture_declaration_target(owner, intent)?;
        let declaration_conflict = base.current_public.as_ref().is_some_and(|tip| {
            tip.exports.iter().any(|export| {
                base.intent.head_replacements.iter().any(|replacement| {
                    replacement.namespace == export.head.namespace
                        && replacement.occurrence == export.head.occurrence
                })
            })
        });
        if base.intent.writes().is_empty() && !declaration_conflict {
            Ok(ExecutionPublication::Bindings(base.public))
        } else {
            Ok(ExecutionPublication::Declarations(
                self.reserve_declaration_target(base)?,
            ))
        }
    }

    /// Capture a declaration join baseline when the fixed intent requires one.
    pub fn restage_declaration_publication(
        &mut self,
        owner: RecoveryPublicOwner,
        intent: Arc<FrozenWorldExecution>,
    ) -> Result<DeclarationPublicationBase, SessionError> {
        self.restage_declaration_target(Some(owner), intent)
    }

    fn restage_declaration_target(
        &mut self,
        owner: Option<RecoveryPublicOwner>,
        intent: Arc<FrozenWorldExecution>,
    ) -> Result<DeclarationPublicationBase, SessionError> {
        let captured = self.capture_declaration_target(owner, intent)?;
        self.reserve_declaration_target(captured)
    }

    fn capture_declaration_target(
        &mut self,
        owner: Option<RecoveryPublicOwner>,
        intent: Arc<FrozenWorldExecution>,
    ) -> Result<CapturedDeclarationPublication, SessionError> {
        self.validate_world_execution_base(&intent)?;
        if intent.durable_owner != owner {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        let mut public = self.snapshot_publication_target_with_world(
            owner,
            intent.admitted.scope,
            intent.private.scope,
            intent.write_ids.clone(),
            intent.source_keys.clone(),
            Some(intent.world()),
        )?;
        public.intent = super::PublicationIntent::Final(intent.clone());
        if public.semantic.expected_private != intent.private {
            return Err(super::SessionTransitionReason::PreparedValueChanged.into());
        }
        let current_public = tip(self.lib(), public.semantic.expected_public.declaration_tip)?;
        let public_context = self
            .lib()
            .current_exact_context_in(public.semantic.expected_public.scope);
        let mut surface = current_public
            .as_ref()
            .map(|tip| tip.surface.clone())
            .unwrap_or_default();
        for write in intent.writes() {
            surface = write.evidence.extend_source_surface(&surface)?;
        }
        Ok(CapturedDeclarationPublication {
            public,
            intent,
            current_public,
            public_context,
            surface,
        })
    }

    fn reserve_declaration_target(
        &mut self,
        captured: CapturedDeclarationPublication,
    ) -> Result<DeclarationPublicationBase, SessionError> {
        let CapturedDeclarationPublication {
            mut public,
            intent,
            current_public,
            public_context,
            surface,
        } = captured;
        self.admit_source_work_in(public.semantic.expected_public.scope)?;
        let reserved = self.lib_mut().reserve_join_for_publication(&mut public)?;
        let expected_public_version = paired_version(&public);
        // World joins resolve only their owned interfaces and declared packages.
        let includes = Vec::new();
        Ok(DeclarationPublicationBase {
            reservation: JoinReservation {
                generation: reserved,
            },
            public,
            intent,
            current_public,
            public_context,
            surface,
            includes,
            session_root: self.lib().root.clone(),
            expected_public_version,
        })
    }

    pub fn revalidate_declaration_rejection(
        &self,
        rejected: &RejectedDeclarationPublication,
    ) -> Result<DeclarationPublicationRejection, SessionError> {
        if !self.declaration_baseline_is_current(&rejected.base.public)? {
            return Ok(DeclarationPublicationRejection::Stale);
        }
        match &rejected.receipt.outcome().decision {
            JoinDecision::Rejected { reason, diagnostic } => {
                Ok(DeclarationPublicationRejection::Rejected {
                    reason: *reason,
                    diagnostic: diagnostic.clone(),
                })
            }
            JoinDecision::Accepted => unreachable!("protected rejected receipt"),
        }
    }

    fn declaration_baseline_is_current(
        &self,
        base: &PublicManifestBase,
    ) -> Result<bool, SessionError> {
        if !base.owner_stamp.matches(self.admission_owner())
            || !self.scope_tree().is_live(base.public_scope)
            || !self.scope_tree().is_live(base.private_scope)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        if !self.has_lib() {
            return Err(SessionError::MissingDeclarationLibrary);
        }
        if base.semantic.library != self.lib().compile_view_identity {
            return Err(SessionError::StaleStagedDeclaration);
        }
        if self
            .public_visibility_snapshot_in(base.private_scope)
            .as_ref()
            != Some(&base.semantic.expected_private)
            || !base
                .semantic
                .private_view
                .is_current(self, base.private_scope)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        Ok(self
            .public_visibility_snapshot_in(base.public_scope)
            .as_ref()
            == Some(&base.semantic.expected_public)
            && base
                .semantic
                .public_view
                .is_current(self, base.public_scope))
    }
}

struct PublicationSelection {
    exports: Vec<DeclarationExport>,
    instances: InstanceInventory,
    families: Vec<ExportIdentity>,
}

fn canonical_exports(exports: &[DeclarationExport]) -> Vec<DeclarationExport> {
    let mut exports = exports.to_vec();
    for export in &mut exports {
        export.children.sort();
    }
    exports.sort_by(|left, right| left.head.cmp(&right.head));
    exports
}

impl DeclarationPublicationBase {
    fn selected_facts(&self) -> Result<PublicationSelection, SessionError> {
        let mut exports = self
            .current_public
            .as_ref()
            .map(|tip| tip.exports.clone())
            .unwrap_or_default();
        let mut instances = self
            .current_public
            .as_ref()
            .map(|tip| tip.instances.clone())
            .unwrap_or_default();
        let mut families = self
            .current_public
            .as_ref()
            .map(|tip| tip.families.clone())
            .unwrap_or_default();
        for write in self.intent.writes() {
            exports.retain(|export| !write.retractions.contains(&export.head));
            for introduced in write.evidence.introduced_exports() {
                exports.retain(|export| {
                    export.head.namespace != introduced.head.namespace
                        || export.head.occurrence != introduced.head.occurrence
                });
                exports.push(introduced.clone());
            }
            // Instances remain additive independently of lexical head removal.
            instances = merge_instances(&self.public.path, instances, write.evidence.instances())?;
            families.extend_from_slice(write.evidence.family_closure());
        }
        exports.retain(|export| {
            !self.intent.head_replacements.iter().any(|replacement| {
                replacement.namespace == export.head.namespace
                    && replacement.occurrence == export.head.occurrence
            })
        });
        families.sort();
        families.dedup();
        Ok(PublicationSelection {
            exports: canonical_exports(&exports),
            instances,
            families,
        })
    }

    fn reusable_projection(
        &self,
        selection: &PublicationSelection,
    ) -> Option<Arc<super::CertifiedDeclarationProjection>> {
        if self.public.semantic.expected_public != self.intent.admitted
            || self.public_context != self.intent.admitted_context
        {
            return None;
        }
        let FrozenDeclarations::Declarations {
            projection: FrozenDeclarationProjection::Retained(projection),
            surface,
            ..
        } = &self.intent.declarations
        else {
            return None;
        };
        let receipt = projection.receipt();
        let final_write = self.intent.writes().last()?;
        if final_write.generation != self.intent.private.declaration_tip
            || selection.exports != canonical_exports(receipt.exports())
            || selection.instances != *receipt.instances()
            || selection.families != receipt.family_closure()
            || surface != &self.surface
        {
            return None;
        }
        let products = projection.context().recovery_products();
        if self.intent.writes().iter().any(|write| {
            !products
                .iter()
                .any(|product| product.owner() == write.evidence.product().owner())
        }) {
            return None;
        }
        Some(projection.clone())
    }
    pub fn reserved_generation(&self) -> Generation {
        self.reservation.generation
    }
    pub fn intent(&self) -> &Arc<FrozenWorldExecution> {
        &self.intent
    }

    pub fn certify(
        self,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<CertifiedDeclarationPublication, SessionError> {
        let selection = self.selected_facts()?;
        if let Some(projection) = self.reusable_projection(&selection) {
            let generation = self.reservation.generation;
            return Ok(CertifiedDeclarationPublication::Accepted(
                AcceptedDeclarationPublication {
                    base: self,
                    receipt: PublicationEvidence::ReusedProjection {
                        generation,
                        projection,
                    },
                },
            ));
        }
        self.certify_new_join(selection, settlement)
    }

    fn certify_new_join(
        self,
        selection: PublicationSelection,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<CertifiedDeclarationPublication, SessionError> {
        let authored = self
            .intent
            .writes()
            .iter()
            .map(|write| write.evidence.clone())
            .collect::<Vec<_>>();
        let owner = authored
            .last()
            .map(|certificate| module_owner(certificate))
            .or_else(|| self.current_public.as_ref().map(|tip| tip.owner.clone()))
            .ok_or_else(|| {
                invalid(
                    &self.public,
                    "empty declaration intent uses binding-only publication",
                )
            })?;
        let context = self
            .public_context
            .as_deref()
            .cloned()
            .unwrap_or_else(ExactDeclarationContext::empty)
            .admit_authored_publication(&authored, &self.surface)?;
        let scratch = Arc::new(tempfile::tempdir()?);
        let join_inputs = context.publication_join_inputs(Arc::clone(&scratch), &self.surface)?;
        let anchor = |identity: &ExactModuleIdentity| -> Result<ModuleSnapshot, SessionError> {
            let artifact = join_inputs.interface(identity).ok_or_else(|| {
                invalid(
                    &self.public,
                    "owned context lacks a selected exact interface",
                )
            })?;
            Ok(ModuleSnapshot {
                module: identity.module.clone(),
                path: artifact.path.clone(),
                sha256: artifact.sha256.clone(),
            })
        };
        let mut writes = Vec::new();
        for write in self.intent.writes() {
            writes.push(DeclarationWrite {
                generation: write.generation.0,
                module: anchor(&module_owner(&write.evidence))?,
                exports: write.evidence.introduced_exports().to_vec(),
                retractions: write.retractions.clone(),
            });
        }
        let input = tidepool_toolchain::declaration_join::DeclarationJoinSpec {
            expected_public_version: self.expected_public_version.clone(),
            public_module: self
                .current_public
                .as_ref()
                .map(|tip| anchor(&tip.owner))
                .transpose()?,
            private_base: self
                .intent
                .private_base
                .as_ref()
                .map(|tip| anchor(&tip.owner))
                .transpose()?,
            private_tip: self
                .intent
                .writes()
                .last()
                .map(|write| module_owner(&write.evidence))
                .or_else(|| {
                    self.intent
                        .private_base
                        .as_ref()
                        .map(|tip| tip.owner.clone())
                })
                .as_ref()
                .map(anchor)
                .transpose()?,
            writes,
            reserved: ReservedJoin {
                unit: owner.unit.clone(),
                module: SessionModule::lib(self.reservation.generation).module_name(),
                path: scratch.path().join("joined.hi"),
            },
            family_closure: selection.families,
            expected_exports: selection.exports,
            expected_instances: selection.instances,
        };
        match tidepool_toolchain::declaration_join::certify_publication_join(
            input,
            join_inputs,
            &self.includes,
            &self.session_root,
            settlement,
        )? {
            CertifiedPublicationJoin::Accepted(composition) => Ok(
                CertifiedDeclarationPublication::Accepted(AcceptedDeclarationPublication {
                    base: self,
                    receipt: PublicationEvidence::NewJoin(composition),
                }),
            ),
            CertifiedPublicationJoin::Rejected(receipt) => Ok(
                CertifiedDeclarationPublication::Rejected(RejectedDeclarationPublication {
                    base: self,
                    receipt,
                }),
            ),
        }
    }
}

impl AcceptedDeclarationPublication {
    #[cfg(test)]
    pub(crate) fn accepted_join_for_test(&self) -> &Arc<AcceptedJoin> {
        self.receipt.receipt()
    }
    pub fn reserved_generation(&self) -> Generation {
        self.base.reservation.generation
    }
    pub fn intent(&self) -> &Arc<FrozenWorldExecution> {
        &self.base.intent
    }
    pub fn stage(self) -> Result<StagedPublicManifest, SessionError> {
        super::CertifiedPublication::Declarations(self).stage()
    }
    pub(super) fn stage_materialized(&self) -> Result<StagedPublicManifest, SessionError> {
        let base = &self.base;
        let receipt = self.receipt.clone();
        let mut public = base.public.clone();
        let context = receipt.composition().context().clone();
        let binding_custody = receipt.native_binding_custody_requirements()?;
        let error_path = public.path.clone();
        let mut prepared_context = None;
        if let super::PublicPublicationBaseline::Durable(super::DurableWriteBase {
            owner,
            graph,
            artifact_root,
            ..
        }) = &mut public.target
        {
            let root = artifact_root.as_path();
            let previous = graph
                .surface(owner)
                .map_err(|error| recovery::graph_error(&error_path, error))?;
            let (admitted, prepared) = graph.stage_compiler_context(
                root,
                &recovery::ScopedCompilerContext::new(context.clone()),
                previous
                    .as_ref()
                    .and_then(|surface| surface.context_root.as_ref()),
            )?;
            let mut candidate = admitted.candidate();
            let producer = receipt.artifact_id();
            if !admitted.contains_artifact(producer) {
                candidate
                    .insert_artifact(recovery::RecoveryArtifactClosure::Join(
                        receipt
                            .materialize_interface(root)
                            .map_err(|error| invalid_at(&error_path, error.to_string()))?,
                    ))
                    .map_err(|error| recovery::graph_error(&error_path, error))?;
            }
            let exports = receipt
                .exports()
                .iter()
                .map(super::certified_recovery_export)
                .collect();
            let mut implementation_refs = base
                .intent
                .writes()
                .iter()
                .map(|write| write.generation)
                .collect::<Vec<_>>();
            if let Some(public) = &base.current_public {
                implementation_refs.push(public.generation);
            }
            let live_dependencies =
                super::native_binding_dependencies(binding_custody.iter().cloned());
            let mut workbench_imports = base
                .current_public
                .as_ref()
                .map(|tip| tip.turn.workbench_imports.clone())
                .unwrap_or_default();
            for write in base.intent.writes() {
                workbench_imports.extend(&write.turn.workbench_imports);
            }
            let context_root = prepared.root().clone();
            prepared_context = Some(prepared);
            candidate
                .insert_node(recovery::RecoveryNode {
                    id: base.reservation.generation,
                    parent: None,
                    kind: recovery::RecoveryNodeKind::Join,
                    implementation_refs,
                    artifact_refs: vec![producer],
                    context_root: context_root.clone(),
                    exports,
                    lexical_roots: vec![ExactModuleIdentity {
                        unit: receipt.reserved().unit.clone(),
                        module: receipt.reserved().module.clone(),
                    }],
                    retracts: Vec::new(),
                    workbench_imports: workbench_imports.specs().to_vec(),
                    instances: recovery_instances(receipt.instances(), receipt.family_closure()),
                    state: if live_dependencies.is_empty() {
                        recovery::RecoveryNodeState::ExactArtifactClosure
                    } else {
                        recovery::RecoveryNodeState::LiveValueDependency {
                            reason: "joined declaration retains exact live dependencies".into(),
                        }
                    },
                    live_dependencies,
                })
                .map_err(|error| recovery::graph_error(&error_path, error))?;
            let mut surface = previous
                .map(|surface| (*surface).clone())
                .unwrap_or_else(|| recovery::RecoveryPublicSurface::empty(owner.clone()));
            surface.declaration_root = Some(base.reservation.generation);
            surface.context_root = Some(context_root);
            candidate
                .replace_surface(surface)
                .map_err(|e| recovery::graph_error(&error_path, e))?;
            *graph = candidate
                .seal()
                .map_err(|e| recovery::graph_error(&error_path, e))?;
        }

        let mut turn = base
            .current_public
            .as_ref()
            .map(|tip| tip.turn.clone())
            .unwrap_or_else(|| base.intent.writes()[0].turn.clone());
        if base.current_public.is_none() {
            turn.items.clear();
            turn.value_types.clear();
            turn.workbench_imports = super::SourceImports::new();
        }
        for write in base.intent.writes() {
            turn.items.retain(|item| {
                !write
                    .retractions
                    .iter()
                    .any(|identity| item.head_name() == identity.occurrence)
            });
            turn.value_types.retain(|name, _| {
                !write
                    .turn
                    .retracts
                    .iter()
                    .any(|retraction| retraction.selects(ExportNamespace::Value, name))
            });
            extend_exports_by_head(&mut turn.items, &write.turn.items);
            turn.value_types.extend(write.turn.value_types.clone());
            turn.workbench_imports.extend(&write.turn.workbench_imports);
        }
        // Exact compiler winners own the final source surface. An exact
        // retraction of an old head cannot erase a concurrently replaced head
        // merely because its printed occurrence happens to match.
        turn.items = receipt
            .exports()
            .iter()
            .map(super::ExportItem::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let private_owners = base
            .intent
            .writes()
            .iter()
            .map(|write| module_owner(&write.evidence))
            .collect::<BTreeSet<_>>();
        let mut declared_names = receipt
            .exports()
            .iter()
            .filter(|export| {
                private_owners.contains(&ExactModuleIdentity {
                    unit: export.head.unit.clone(),
                    module: export.head.module.clone(),
                })
            })
            .flat_map(|export| std::iter::once(&export.head).chain(export.children.iter()))
            .filter(|identity| identity.namespace != ExportNamespace::Type)
            .map(|identity| identity.occurrence.clone())
            .collect::<Vec<_>>();
        let final_bindings = base
            .intent
            .write_ids
            .iter()
            .filter_map(|id| {
                base.intent
                    .private
                    .bindings
                    .iter()
                    .find(|(_, current)| current == id)
            })
            .map(|(name, _)| name)
            .collect::<BTreeSet<_>>();
        declared_names.retain(|name| !final_bindings.contains(name));
        declared_names.sort();
        declared_names.dedup();
        turn.value_types.retain(|name, _| {
            turn.items
                .iter()
                .any(|item| matches!(item, super::ExportItem::Value { name: head } if head == name))
        });
        turn.parent = None;
        turn.sources.clear();
        turn.retracts.clear();
        turn.normalized = DeclarationSource {
            prologue: Default::default(),
            body: String::new(),
        };
        let source_domain_owners = base
            .intent
            .writes()
            .iter()
            .map(|write| write.evidence.product().owner().clone())
            .collect();
        let declaration = PreparedDeclarationPublication {
            source_domain_owners,
            generation: base.reservation.generation,
            declared_names,
            joined: JoinedDeclaration {
                turn,
                evidence: receipt,
                surface: base.surface.clone(),
            },
        };
        public
            .final_bindings
            .retain(|(name, _)| !declaration.declared_names.contains(name));
        let mut ticket = public.stage_materialized(prepared_context)?;
        ticket.declaration = Some(declaration);
        Ok(ticket)
    }
}

impl RejectedDeclarationPublication {
    pub fn intent(&self) -> &Arc<FrozenWorldExecution> {
        &self.base.intent
    }
}

impl SessionLib {
    pub(super) fn declaration_publication_is_ready(&self, ticket: &StagedPublicManifest) -> bool {
        ticket.declaration.as_ref().is_none_or(|declaration| {
            self.log.is_reserved(declaration.generation)
                && declaration.joined.turn.parent.is_none()
                && self.scope_tip(ticket.public_scope)
                    == ticket.semantic.expected_public.declaration_tip
                && self.tips.contains_key(&ticket.public_scope)
                && self.source_selections_preserved(
                    ticket.public_scope,
                    Some(declaration.joined.evidence.composition().context()),
                )
                && declaration
                    .joined
                    .evidence
                    .matches_generation(declaration.generation)
        })
    }
    pub(super) fn prepare_resident_declaration(
        &self,
        scope: ScopeId,
        declaration: PreparedDeclarationPublication,
    ) -> Result<PreparedResidentDeclaration, SessionError> {
        let mut tip = self
            .tips
            .get(&scope)
            .cloned()
            .ok_or(SessionError::StaleStagedDeclaration)?;
        tip.tip = declaration.generation;
        tip.exact_context = Some(recovery::ScopedCompilerContext::new(
            declaration.joined.evidence.composition().context().clone(),
        ));
        let log = self
            .log
            .prepare_joined_commit(declaration.generation, declaration.joined)?;
        Ok(PreparedResidentDeclaration { scope, tip, log })
    }
    pub(super) fn commit_prepared_declaration(&mut self, declaration: PreparedResidentDeclaration) {
        self.log.activate_prepared_commit(declaration.log);
        *self
            .tips
            .get_mut(&declaration.scope)
            .expect("prepared declaration tip") = declaration.tip;
    }
}
#[cfg(test)]
pub(in crate::session) mod semantic_currency_tests;

#[cfg(test)]
pub(in crate::session) mod tests {
    use super::*;

    pub(in crate::session) fn reject_missing_export(
        first: AcceptedDeclarationPublication,
        authored: &Arc<tidepool_toolchain::declaration_join::CertifiedAuthoredDeclaration>,
    ) -> RejectedDeclarationPublication {
        let surface = authored
            .extend_source_surface(&AdmittedDeclarationSurface::default())
            .unwrap();
        let context = ExactDeclarationContext::empty()
            .admit_authored_publication(std::slice::from_ref(authored), &surface)
            .unwrap();
        let scratch = Arc::new(tempfile::tempdir().unwrap());
        let join_inputs = context
            .publication_join_inputs(scratch.clone(), &surface)
            .unwrap();
        let original = join_inputs
            .interface(&ExactModuleIdentity {
                unit: authored.product().owner().unit.clone(),
                module: authored.product().owner().module.clone(),
            })
            .unwrap();
        let anchor = ModuleSnapshot {
            module: original.module.clone(),
            path: original.path.clone(),
            sha256: original.sha256.clone(),
        };
        let mut input = first.receipt.input().clone();
        input.private_tip = Some(anchor.clone());
        input.writes[0].module = anchor;
        input.reserved.path = scratch.path().join("rejected.hi");
        input.expected_exports[0]
            .head
            .occurrence
            .push_str("Missing");
        let CertifiedPublicationJoin::Rejected(receipt) =
            tidepool_testing::with_settlement(|settlement| {
                tidepool_toolchain::declaration_join::certify_publication_join(
                    tidepool_toolchain::declaration_join::DeclarationJoinSpec {
                        expected_public_version: input.expected_public_version,
                        public_module: input.public_module,
                        private_base: input.private_base,
                        private_tip: input.private_tip,
                        writes: input.writes,
                        reserved: input.reserved,
                        family_closure: input.family_closure,
                        expected_exports: input.expected_exports,
                        expected_instances: input.expected_instances,
                    },
                    join_inputs,
                    &first.base.includes,
                    &first.base.session_root,
                    settlement,
                )
            })
            .unwrap()
        else {
            panic!("expected protected inventory rejection");
        };
        RejectedDeclarationPublication {
            base: first.base,
            receipt,
        }
    }

    pub(in crate::session) fn assert_original_writes(
        intent: &FrozenWorldExecution,
        lib: &SessionLib,
        expected: &[(Generation, usize)],
    ) {
        assert_eq!(intent.writes().len(), expected.len());
        for (write, (generation, retractions)) in intent.writes().iter().zip(expected) {
            assert_eq!(write.generation, *generation);
            assert_eq!(write.retractions.len(), *retractions);
            assert!(Arc::ptr_eq(
                &lib.log.certified_authored_arc_at(*generation).unwrap(),
                &write.evidence,
            ));
        }
    }

    pub(in crate::session) fn reused_projection(
        publication: &AcceptedDeclarationPublication,
    ) -> &Arc<super::super::CertifiedDeclarationProjection> {
        match &publication.receipt {
            PublicationEvidence::ReusedProjection { projection, .. } => projection,
            PublicationEvidence::NewJoin(_) => {
                panic!("publication must reuse its exact projection")
            }
        }
    }

    pub(in crate::session) fn assert_allocation_preserves_publication_semantics(
        publication: &AcceptedDeclarationPublication,
        lib: &mut SessionLib,
    ) {
        let semantic = paired_version(&publication.base.public);
        let revision = lib.log.publication_revision();
        lib.log.reserve();
        assert_eq!(paired_version(&publication.base.public), semantic);
        assert_eq!(lib.log.publication_revision(), revision);
    }

    pub(in crate::session) fn assert_tip_instance_counts(
        lib: &SessionLib,
        generation: Generation,
        classes: usize,
        families: usize,
    ) {
        let cumulative = tip(lib, generation).unwrap().unwrap();
        assert_eq!(cumulative.instances.classes.len(), classes);
        assert_eq!(cumulative.instances.families.len(), families);
    }

    pub(in crate::session) fn assert_unfrozen_operator_withdrawal(
        lib: &SessionLib,
        scope: ScopeId,
    ) {
        let generation = lib.scope_tip(scope);
        let declaration = tip(lib, generation).unwrap().unwrap();
        assert!(!declaration
            .exports
            .iter()
            .any(|export| export.head.occurrence == "%%"));
        let turn = lib.log.turn(generation).unwrap();
        let retractions =
            exact_retractions(lib, turn.parent.unwrap_or(Generation(0)), &turn.retracts).unwrap();
        assert_eq!(retractions.len(), 2);
        assert!(retractions
            .iter()
            .any(|identity| identity.namespace == ExportNamespace::Type));
        assert!(retractions
            .iter()
            .any(|identity| identity.namespace == ExportNamespace::Value));
    }

    pub(in crate::session) fn force_new_join(
        base: DeclarationPublicationBase,
        report: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> AcceptedDeclarationPublication {
        let selection = base.selected_facts().unwrap();
        match base.certify_new_join(selection, report).unwrap() {
            CertifiedDeclarationPublication::Accepted(accepted) => accepted,
            CertifiedDeclarationPublication::Rejected(_) => panic!("independent GHC join refused"),
        }
    }

    pub(in crate::session) fn canonical_join_exports(
        publication: &AcceptedDeclarationPublication,
    ) -> Vec<DeclarationExport> {
        canonical_exports(publication.receipt.exports())
    }

    pub(in crate::session) fn projection_is_reused(
        publication: &AcceptedDeclarationPublication,
    ) -> bool {
        matches!(
            &publication.receipt,
            PublicationEvidence::ReusedProjection { .. }
        )
    }
}
