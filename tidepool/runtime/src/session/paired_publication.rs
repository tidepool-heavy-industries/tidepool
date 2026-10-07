//! Owned execution intent and compiler receipts for paired publication.

#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidepool_codegen::binding_table::SourceLeaseKey;
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::{Generation, SessionModule, SessionVarId};
use tidepool_toolchain::declaration_join::{
    AcceptedJoin, CertifiedAuthoredDeclaration, CertifiedDeclarationJoin, DeclarationExport,
    DeclarationJoinInput, DeclarationWrite, ExactDeclarationContext, ExactLexicalNode,
    ExactModuleIdentity, ExportIdentity, ExportNamespace, InstanceInventory, JoinDecision,
    JoinRejection, ModuleSnapshot, RejectedJoin, ReservedJoin,
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
/// baseline, never this private suffix, exact winners, leases, or reserved ID.
/// The private scope keeps live roots owned until publication or abandonment.
pub struct FinalExecutionIntent {
    owner: Arc<super::admission::RuntimeAdmissionOwner>,
    owner_epoch: u64,
    durable_owner: Option<RecoveryPublicOwner>,
    _private_scope_lease: Option<Arc<super::RuntimeLexicalScopeLease>>,
    admitted: PublicVisibilitySnapshot,
    private: PublicVisibilitySnapshot,
    private_base: Option<DeclarationTip>,
    writes: Vec<AuthoredWrite>,
    write_ids: Vec<SessionVarId>,
    source_keys: Vec<SourceLeaseKey>,
    head_replacements: Vec<DeclarationHeadReplacement>,
    _completed_values: Vec<super::admission::CertifiedPrivateValueProof>,
    reserved: Generation,
}

/// Name-winning publication is separate from an exact withdrawal. A runtime
/// value replaces the current term head even when another execution replaced
/// the admitted declaration while this execution was suspended.
#[derive(Clone)]
struct DeclarationHeadReplacement {
    namespace: ExportNamespace,
    occurrence: String,
}

/// One current public graph paired with the unchanged final execution intent.
pub struct DeclarationPublicationBase {
    public: PublicManifestBase,
    reserved: Generation,
    intent: Arc<FinalExecutionIntent>,
    current_public: Option<DeclarationTip>,
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
    base: DeclarationPublicationBase,
    receipt: Arc<AcceptedJoin>,
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

pub(super) struct PreparedDeclarationPublication {
    pub(super) generation: Generation,
    pub(super) joined: JoinedDeclaration,
    pub(super) declared_names: Vec<String>,
    pub(super) source_domain_owners: Vec<tidepool_repr::execution_schema::CachedHomeOwner>,
    pub(super) binding_custody:
        Vec<tidepool_toolchain::artifact_inventory::NativeBindingRequirement>,
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
            let identity = &key.binder.binder;
            serde_json::json!({"instance": key.instance.raw(), "module_version": key.binder.version.0,
                "unit": identity.unit, "module": identity.module, "namespace": identity.namespace,
                "occurrence": identity.occurrence, "record_parent": identity.record_parent})
        }).collect::<Vec<_>>(),
    })
}

fn paired_version(base: &PublicManifestBase) -> String {
    let target = match &base.target {
        super::PublicPublicationBaseline::Durable { owner, graph } => serde_json::json!({
            "mode": "durable", "owner": owner, "graph": graph.checksum(), "high_water": graph.high_water().0 }),
        super::PublicPublicationBaseline::Ephemeral => serde_json::json!({ "mode": "ephemeral" }),
    };
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema": "tidepool-paired-declaration-baseline-v2", "session": base.session.0,
        "target": target,
        "log_revision": base.log_revision,
        "public": snapshot_digest(&base.expected_public), "private": snapshot_digest(&base.expected_private),
    })).expect("paired baseline contains serializable scalar identities");
    blake3::hash(&bytes).to_hex().to_string()
}

pub(super) fn extend_admitted_surface(
    authored: &CertifiedAuthoredDeclaration,
    mut prior: AdmittedDeclarationSurface,
) -> Result<AdmittedDeclarationSurface, SessionError> {
    let addition = authored.shared_source_lexical_surface(&prior.lexical)?;
    prior.roots.extend(addition.roots);
    prior.roots.sort();
    prior.roots.dedup();
    prior.lexical = addition.lexical;
    Ok(prior)
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

impl FinalExecutionIntent {
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

    pub fn reserved_generation(&self) -> Generation {
        self.reserved
    }
    pub fn admitted_public(&self) -> &PublicVisibilitySnapshot {
        &self.admitted
    }
    pub fn private_scope(&self) -> ScopeId {
        self.private.scope
    }
}

impl PersistentSession {
    /// Freeze the execution's final current writes once. Historical binding
    /// writes shadowed within the same execution do not become final winners.
    pub fn freeze_execution_intent(
        &mut self,
        admission: &super::PrivateExecutionAdmission,
        write_ids: Vec<SessionVarId>,
        source_keys: Vec<SourceLeaseKey>,
    ) -> Result<Arc<FinalExecutionIntent>, SessionError> {
        let completed_values = admission.completed_values.lock();
        self.freeze_execution_intent_locked(admission, write_ids, source_keys, &completed_values)
    }

    /// The caller holds this admission's ledger lock from write selection
    /// through final sealing, so effect acceptance cannot enter between them.
    pub(super) fn freeze_execution_intent_locked(
        &mut self,
        admission: &super::PrivateExecutionAdmission,
        write_ids: Vec<SessionVarId>,
        source_keys: Vec<SourceLeaseKey>,
        completed_values: &parking_lot::MutexGuard<
            '_,
            std::collections::HashMap<SessionVarId, super::admission::CertifiedPrivateValueWrite>,
        >,
    ) -> Result<Arc<FinalExecutionIntent>, SessionError> {
        if !Arc::ptr_eq(&admission.owner, self.admission_owner())
            || admission.owner_epoch != self.admission_owner().epoch()
            || admission.view().session() != self.lib().session_id()
            || self.binding_tip_id(admission.private_scope()) != Some(admission.binding_tip())
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        if let Some(intent) = admission.final_intent.get() {
            if self
                .public_visibility_snapshot_in(admission.private_scope())
                .as_ref()
                != Some(&intent.private)
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
            if write_ids.iter().any(|id| {
                self.bindings()
                    .get(*id)
                    .is_none_or(|entry| entry.scope != admission.private_scope())
            }) {
                return Err(SessionError::InvalidPublicBindingPromotion(
                    tidepool_codegen::binding_table::BindingPromotionError::MissingOrForeignBinding,
                ));
            }
            let mut writes = write_ids
                .into_iter()
                .filter(|id| {
                    intent
                        .private
                        .bindings
                        .iter()
                        .any(|(_, current)| current == id)
                })
                .collect::<Vec<_>>();
            writes.sort_by_key(|id| id.raw());
            writes.dedup();
            let keys = source_keys
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if writes != intent.write_ids || keys != intent.source_keys {
                return Err(invalid_at(
                    &self.lib().root,
                    "execution final intent is already frozen with different writes",
                ));
            }
            return Ok(intent.clone());
        }
        let intent = self.freeze_execution_intent_from_snapshot(
            admission.admitted_public(),
            admission.private_scope(),
            write_ids,
            source_keys,
            Some(admission.scope_lease.clone()),
            admission.durable_owner.clone(),
            completed_values,
        )?;
        assert!(
            admission.final_intent.set(intent.clone()).is_ok(),
            "exclusive session checkout finalizes an admission once"
        );
        Ok(intent)
    }

    fn freeze_execution_intent_from_snapshot(
        &mut self,
        admitted: &PublicVisibilitySnapshot,
        private_scope: ScopeId,
        write_ids: Vec<SessionVarId>,
        source_keys: Vec<SourceLeaseKey>,
        private_scope_lease: Option<Arc<super::RuntimeLexicalScopeLease>>,
        durable_owner: Option<RecoveryPublicOwner>,
        completed_values: &std::collections::HashMap<
            SessionVarId,
            super::admission::CertifiedPrivateValueWrite,
        >,
    ) -> Result<Arc<FinalExecutionIntent>, SessionError> {
        if private_scope == admitted.scope || !self.scope_tree().is_live(private_scope) {
            return Err(SessionError::DeadScope(private_scope));
        }
        let private = self
            .public_visibility_snapshot_in(private_scope)
            .ok_or(SessionError::MissingDeclarationLibrary)?;
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
            return Err(invalid_at(&lib.root, "a final value write overlaps a retained declaration without certified private Value ownership or an exact retraction"));
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
            .prepare_exact_publication_in(
                self.scope_tree(),
                private_scope,
                admitted.scope,
                &write_ids,
                &source_keys,
            )
            .map_err(SessionError::InvalidPublicBindingPromotion)?;
        let reserved = if self.lib().durable_graph.is_some() {
            self.lib_mut().reserve_join_generation_durable()?
        } else {
            self.lib_mut().log.reserve()
        };
        Ok(Arc::new(FinalExecutionIntent {
            owner: self.admission_owner().clone(),
            owner_epoch: self.admission_owner().epoch(),
            durable_owner,
            _private_scope_lease: private_scope_lease,
            admitted: admitted.clone(),
            private,
            private_base,
            writes,
            _completed_values: write_ids
                .iter()
                .filter_map(|id| completed_values.get(id).map(|proof| proof.proof.clone()))
                .collect(),
            write_ids,
            source_keys,
            head_replacements,
            reserved,
        }))
    }

    /// Capture only the latest public merge baseline for a fixed execution.
    pub fn restage_execution_publication(
        &mut self,
        owner: RecoveryPublicOwner,
        intent: Arc<FinalExecutionIntent>,
    ) -> Result<ExecutionPublication, SessionError> {
        self.restage_execution_target(Some(owner), intent)
    }

    pub fn restage_ephemeral_execution_publication(
        &mut self,
        intent: Arc<FinalExecutionIntent>,
    ) -> Result<ExecutionPublication, SessionError> {
        self.restage_execution_target(None, intent)
    }

    fn restage_execution_target(
        &mut self,
        owner: Option<RecoveryPublicOwner>,
        intent: Arc<FinalExecutionIntent>,
    ) -> Result<ExecutionPublication, SessionError> {
        let base = self.restage_declaration_target(owner, intent)?;
        let declaration_conflict = base.current_public.as_ref().is_some_and(|tip| {
            tip.exports.iter().any(|export| {
                base.intent.head_replacements.iter().any(|replacement| {
                    replacement.namespace == export.head.namespace
                        && replacement.occurrence == export.head.occurrence
                })
            })
        });
        if base.intent.writes.is_empty() && !declaration_conflict {
            Ok(ExecutionPublication::Bindings(base.public))
        } else {
            Ok(ExecutionPublication::Declarations(base))
        }
    }

    /// Capture a declaration join baseline when the fixed intent requires one.
    pub fn restage_declaration_publication(
        &mut self,
        owner: RecoveryPublicOwner,
        intent: Arc<FinalExecutionIntent>,
    ) -> Result<DeclarationPublicationBase, SessionError> {
        self.restage_declaration_target(Some(owner), intent)
    }

    fn restage_declaration_target(
        &mut self,
        owner: Option<RecoveryPublicOwner>,
        intent: Arc<FinalExecutionIntent>,
    ) -> Result<DeclarationPublicationBase, SessionError> {
        if !Arc::ptr_eq(&intent.owner, self.admission_owner())
            || intent.owner_epoch != self.admission_owner().epoch()
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        if intent.durable_owner != owner {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        let public = self.snapshot_publication_target(
            owner,
            intent.admitted.scope,
            intent.private.scope,
            intent.write_ids.clone(),
            intent.source_keys.clone(),
        )?;
        if public.expected_private != intent.private || !self.lib().log.is_reserved(intent.reserved)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let current_public = tip(self.lib(), public.expected_public.declaration_tip)?;
        let mut surface = current_public
            .as_ref()
            .map(|tip| tip.surface.clone())
            .unwrap_or_default();
        for write in &intent.writes {
            surface = extend_admitted_surface(&write.evidence, surface)?;
        }
        let expected_public_version = paired_version(&public);
        Ok(DeclarationPublicationBase {
            reserved: intent.reserved,
            public,
            intent,
            current_public,
            surface,
            includes: self.lib().extra_include.clone(),
            session_root: self.lib().root.clone(),
            expected_public_version,
        })
    }

    #[cfg(test)]
    pub(crate) fn snapshot_declaration_publication(
        &mut self,
        owner: RecoveryPublicOwner,
        admitted: &PublicVisibilitySnapshot,
        private_scope: ScopeId,
        write_ids: Vec<SessionVarId>,
        source_keys: Vec<SourceLeaseKey>,
    ) -> Result<DeclarationPublicationBase, SessionError> {
        let intent = self.freeze_execution_intent_from_snapshot(
            admitted,
            private_scope,
            write_ids,
            source_keys,
            None,
            Some(owner.clone()),
            &std::collections::HashMap::new(),
        )?;
        self.restage_declaration_publication(owner, intent)
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
        if base
            .admission_owner
            .as_ref()
            .is_none_or(|owner| !Arc::ptr_eq(owner, self.admission_owner()))
            || base.admission_owner_epoch != Some(self.admission_owner().epoch())
        {
            return Ok(false);
        }
        if !self.has_lib() {
            return Err(SessionError::MissingDeclarationLibrary);
        }
        Ok(self.lib().publication_baseline_is_current(base)?
            && self.publication_views_are_current(&base.expected_public, &base.expected_private))
    }
}

impl DeclarationPublicationBase {
    pub fn reserved_generation(&self) -> Generation {
        self.reserved
    }
    pub fn intent(&self) -> &Arc<FinalExecutionIntent> {
        &self.intent
    }

    pub fn certify(self) -> Result<CertifiedDeclarationPublication, SessionError> {
        let mut lexical = self.surface.lexical.clone();
        let mut selected_tips = BTreeSet::new();
        for generation in self
            .current_public
            .iter()
            .map(|tip| tip.generation)
            .chain(self.intent.private_base.iter().map(|tip| tip.generation))
            .chain(self.intent.writes.iter().map(|write| write.generation))
        {
            selected_tips.insert(generation);
        }
        let authored = self
            .intent
            .writes
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
        for generation in selected_tips {
            lexical.push(ExactLexicalNode {
                owner: ExactModuleIdentity {
                    unit: owner.unit.clone(),
                    module: SessionModule::lib(generation).module_name(),
                },
                imports: Vec::new(),
            });
        }
        let context = if let Some(public) = &self.current_public {
            (*public.context).clone().extend(&authored, &[], lexical)?
        } else {
            ExactDeclarationContext::new(&authored, &[], lexical)?
        };
        let scratch = tempfile::tempdir()?;
        let materialized = context.materialize_scratch(&scratch)?;
        let anchor = |generation: Generation| -> Result<ModuleSnapshot, SessionError> {
            let module = SessionModule::lib(generation).module_name();
            let artifact = materialized
                .artifacts
                .iter()
                .find(|artifact| {
                    artifact.interface.unit == owner.unit && artifact.interface.module == module
                })
                .ok_or_else(|| {
                    invalid(
                        &self.public,
                        "owned context lacks a selected exact interface",
                    )
                })?;
            Ok(ModuleSnapshot {
                module,
                path: artifact.interface.path.clone(),
                sha256: artifact.interface.sha256.clone(),
            })
        };
        let mut expected_exports = self
            .current_public
            .as_ref()
            .map(|tip| tip.exports.clone())
            .unwrap_or_default();
        let mut expected_instances = self
            .current_public
            .as_ref()
            .map(|tip| tip.instances.clone())
            .unwrap_or_default();
        let mut family_closure = self
            .current_public
            .as_ref()
            .map(|tip| tip.families.clone())
            .unwrap_or_default();
        let mut writes = Vec::new();
        for write in &self.intent.writes {
            expected_exports.retain(|export| !write.retractions.contains(&export.head));
            for introduced in write.evidence.introduced_exports() {
                expected_exports.retain(|export| {
                    export.head.namespace != introduced.head.namespace
                        || export.head.occurrence != introduced.head.occurrence
                });
                expected_exports.push(introduced.clone());
            }
            // Selection is exact and additive. A missing private head or a
            // retracted spelling never removes a latest-public dfun/axiom.
            expected_instances = merge_instances(
                &self.public.path,
                expected_instances,
                write.evidence.instances(),
            )?;
            family_closure.extend_from_slice(write.evidence.family_closure());
            writes.push(DeclarationWrite {
                generation: write.generation.0,
                module: anchor(write.generation)?,
                exports: write.evidence.introduced_exports().to_vec(),
                retractions: write.retractions.clone(),
            });
        }
        expected_exports.retain(|export| {
            !self.intent.head_replacements.iter().any(|replacement| {
                replacement.namespace == export.head.namespace
                    && replacement.occurrence == export.head.occurrence
            })
        });
        family_closure.sort();
        family_closure.dedup();
        let private_tip = self.intent.private.declaration_tip;
        let input = DeclarationJoinInput {
            expected_public_version: self.expected_public_version.clone(),
            public_module: self
                .current_public
                .as_ref()
                .map(|tip| anchor(tip.generation))
                .transpose()?,
            private_base: self
                .intent
                .private_base
                .as_ref()
                .map(|tip| anchor(tip.generation))
                .transpose()?,
            private_tip: (private_tip != Generation(0))
                .then(|| anchor(private_tip))
                .transpose()?,
            writes,
            reserved: ReservedJoin {
                unit: owner.unit.clone(),
                module: SessionModule::lib(self.reserved).module_name(),
                path: scratch.path().join("joined.hi"),
            },
            artifacts: materialized.artifacts,
            family_closure,
            expected_exports,
            expected_instances,
        };
        match tidepool_toolchain::declaration_join::certify_declaration_join(
            input,
            &context,
            &self.includes,
            &self.session_root,
        )? {
            CertifiedDeclarationJoin::Accepted(receipt) => Ok(
                CertifiedDeclarationPublication::Accepted(AcceptedDeclarationPublication {
                    base: self,
                    receipt: Arc::new(receipt),
                }),
            ),
            CertifiedDeclarationJoin::Rejected(receipt) => Ok(
                CertifiedDeclarationPublication::Rejected(RejectedDeclarationPublication {
                    base: self,
                    receipt,
                }),
            ),
        }
    }
}

impl AcceptedDeclarationPublication {
    pub fn intent(&self) -> &Arc<FinalExecutionIntent> {
        &self.base.intent
    }
    pub fn stage(self) -> Result<StagedPublicManifest, SessionError> {
        let Self { mut base, receipt } = self;
        let mut work = super::RecoveryPublicationWork::default();
        let context = Arc::new(ExactDeclarationContext::new(
            &[],
            std::slice::from_ref(&receipt),
            std::iter::once(ExactLexicalNode {
                owner: ExactModuleIdentity {
                    unit: receipt.reserved().unit.clone(),
                    module: receipt.reserved().module.clone(),
                },
                imports: base.surface.roots.clone(),
            })
            .chain(base.surface.lexical.iter().cloned())
            .collect(),
        )?);
        let binding_custody = receipt.native_binding_custody_requirements()?;
        let baseline = match &base.public.target {
            super::PublicPublicationBaseline::Durable { graph, .. } => {
                Some((graph.checksum().to_owned(), graph.high_water()))
            }
            super::PublicPublicationBaseline::Ephemeral => None,
        };
        let error_path = base.public.path.clone();
        if let super::PublicPublicationBaseline::Durable { owner, graph } = &mut base.public.target
        {
            let mut candidate = graph.candidate();
            let root = base
                .public
                .path
                .parent()
                .ok_or_else(|| invalid_at(&error_path, "manifest has no parent"))?;
            let materialized = receipt
                .materialize(root)
                .map_err(|error| invalid_at(&error_path, error.to_string()))?;
            work.recovery_materialization_hash_bytes += materialized.materialization_hash_bytes;
            let exports = receipt
                .exports()
                .iter()
                .map(super::certified_recovery_export)
                .collect();
            let mut implementation_refs = base
                .intent
                .writes
                .iter()
                .map(|write| write.generation)
                .collect::<Vec<_>>();
            if let Some(public) = &base.current_public {
                implementation_refs.push(public.generation);
            }
            let artifacts = materialized
                .products
                .into_iter()
                .map(recovery::RecoveryArtifactClosure::Home)
                .chain(
                    materialized
                        .module_interfaces
                        .into_iter()
                        .map(recovery::RecoveryArtifactClosure::ModuleInterface),
                )
                .chain(
                    materialized
                        .anchors
                        .into_iter()
                        .map(recovery::RecoveryArtifactClosure::Join),
                )
                .chain(
                    materialized
                        .value_interfaces
                        .into_iter()
                        .map(recovery::RecoveryArtifactClosure::ValueInterface),
                )
                .chain(std::iter::once(recovery::RecoveryArtifactClosure::Join(
                    materialized.join,
                )))
                .collect::<Vec<_>>();
            let artifact_refs = artifacts
                .iter()
                .map(recovery::RecoveryArtifactClosure::artifact_id)
                .collect();
            let live_dependencies =
                super::native_binding_dependencies(binding_custody.iter().cloned());
            let mut workbench_imports = base
                .current_public
                .as_ref()
                .map(|tip| tip.turn.workbench_imports.clone())
                .unwrap_or_default();
            for write in &base.intent.writes {
                workbench_imports.extend(&write.turn.workbench_imports);
            }
            candidate
                .insert_node(recovery::RecoveryNode {
                    id: base.reserved,
                    parent: None,
                    kind: recovery::RecoveryNodeKind::Join,
                    implementation_refs,
                    artifact_refs,
                    native_groups: context
                        .artifact_view()
                        .selected_native_groups()
                        .into_iter()
                        .collect(),
                    exports,
                    lexical_roots: vec![ExactModuleIdentity {
                        unit: receipt.reserved().unit.clone(),
                        module: receipt.reserved().module.clone(),
                    }],
                    lexical: context.lexical_graph().to_vec(),
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
                .map_err(|error| invalid_at(&error_path, error.to_string()))?;
            for artifact in artifacts {
                candidate
                    .insert_artifact(artifact)
                    .map_err(|error| invalid_at(&error_path, error.to_string()))?;
            }
            for (source, target, dependency) in materialized.artifact_dependencies {
                candidate
                    .insert_interface_edge(recovery::RecoveryArtifactDependency {
                        source,
                        target,
                        dependency,
                    })
                    .map_err(|error| invalid_at(&error_path, error.to_string()))?;
            }
            let mut surface =
                graph
                    .surface(owner)
                    .cloned()
                    .unwrap_or_else(|| recovery::RecoveryPublicSurface {
                        owner: owner.clone(),
                        declaration_root: None,
                        epoch: 0,
                        bindings: Vec::new(),
                        source_instances: Vec::new(),
                    });
            surface.declaration_root = Some(base.reserved);
            candidate.replace_surface(surface);
            let (sealed, encoded_bytes) = candidate
                .seal_with_encoded_bytes()
                .map_err(|error| invalid_at(&error_path, error.to_string()))?;
            work.checksum_encode_bytes += encoded_bytes;
            *graph = sealed;
        }
        let mut turn = base
            .current_public
            .as_ref()
            .map(|tip| tip.turn.clone())
            .unwrap_or_else(|| base.intent.writes[0].turn.clone());
        if base.current_public.is_none() {
            turn.items.clear();
            turn.value_types.clear();
            turn.workbench_imports = super::SourceImports::new();
        }
        for write in &base.intent.writes {
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
            .map(|export| match export.kind {
                tidepool_toolchain::declaration_join::DeclarationKind::Value => {
                    super::ExportItem::Value {
                        name: export.head.occurrence.clone(),
                    }
                }
                tidepool_toolchain::declaration_join::DeclarationKind::Type => {
                    super::ExportItem::Type {
                        name: export.head.occurrence.clone(),
                        cons: export
                            .children
                            .iter()
                            .map(|child| child.occurrence.clone())
                            .collect(),
                    }
                }
                tidepool_toolchain::declaration_join::DeclarationKind::Class => {
                    super::ExportItem::Class {
                        name: export.head.occurrence.clone(),
                        methods: export
                            .children
                            .iter()
                            .map(|child| child.occurrence.clone())
                            .collect(),
                    }
                }
            })
            .collect();
        let private_owners = base
            .intent
            .writes
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
            .writes
            .iter()
            .map(|write| write.evidence.product().owner().clone())
            .collect();
        let declaration = PreparedDeclarationPublication {
            source_domain_owners,
            binding_custody,
            generation: base.reserved,
            declared_names,
            joined: JoinedDeclaration {
                turn,
                evidence: receipt,
                context,
                surface: base.surface,
            },
        };
        base.public
            .final_bindings
            .retain(|(name, _)| !declaration.declared_names.contains(name));
        let mut ticket = base.public.stage()?;
        if let (
            Some((checksum, high_water)),
            super::StagedPublicationTarget::Durable {
                base_checksum,
                base_high_water,
                ..
            },
        ) = (baseline, &mut ticket.target)
        {
            *base_checksum = checksum;
            *base_high_water = high_water;
        }
        if let super::StagedPublicationTarget::Durable { staged, .. } = &mut ticket.target {
            staged.work.checksum_encode_bytes += work.checksum_encode_bytes;
            staged.work.recovery_materialization_hash_bytes +=
                work.recovery_materialization_hash_bytes;
        }
        ticket.declaration = Some(declaration);
        Ok(ticket)
    }
}

impl RejectedDeclarationPublication {
    pub fn intent(&self) -> &Arc<FinalExecutionIntent> {
        &self.base.intent
    }
}

impl SessionLib {
    pub(super) fn declaration_publication_is_ready(&self, ticket: &StagedPublicManifest) -> bool {
        ticket.declaration.as_ref().is_none_or(|declaration| {
            self.log.is_reserved(declaration.generation)
                && declaration.joined.turn.parent.is_none()
                && self.scope_tip(ticket.public_scope) == ticket.expected_public.declaration_tip
                && self.tips.contains_key(&ticket.public_scope)
                && declaration.joined.evidence.reserved().module
                    == SessionModule::lib(declaration.generation).module_name()
        })
    }
    pub(super) fn commit_prepared_declaration(
        &mut self,
        scope: ScopeId,
        declaration: PreparedDeclarationPublication,
    ) {
        let generation = declaration.generation;
        assert!(self.log.commit_reserved(generation, declaration.joined));
        *self
            .tips
            .get_mut(&scope)
            .expect("declaration tip preflight") = generation;
    }
}
#[cfg(test)]
mod linearization_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{
        ModuleEnv, PublicManifestCommit, PublicationDecision, PublicationPhase, SessionId,
        SourceImports,
    };

    fn accepted(base: DeclarationPublicationBase) -> AcceptedDeclarationPublication {
        match base.certify().unwrap() {
            CertifiedDeclarationPublication::Accepted(accepted) => accepted,
            CertifiedDeclarationPublication::Rejected(rejected) => {
                panic!("unexpected rejection: {:?}", rejected.receipt.outcome())
            }
        }
    }

    fn ephemeral_binding_stage(
        session: &mut PersistentSession,
        intent: Arc<FinalExecutionIntent>,
    ) -> StagedPublicManifest {
        let ExecutionPublication::Bindings(base) = session
            .restage_ephemeral_execution_publication(intent)
            .unwrap()
        else {
            panic!("fixture has no declaration writes");
        };
        base.stage().unwrap()
    }

    #[test]
    fn paired_ephemeral_completion_order_rebases_without_replaying_native_writes() {
        let root = tempfile::tempdir().unwrap();
        let lib = SessionLib::open(
            SessionId(4480),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_isolated_scope();
        let a = session.begin_ephemeral_private_execution(public).unwrap();
        let b = session.begin_ephemeral_private_execution(public).unwrap();
        let a_value =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "x", 701);
        let a_id = a_value.id;
        session.bind_in(a.private_scope(), a_value).unwrap();
        let b_value =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "x", 702);
        let b_id = b_value.id;
        session.bind_in(b.private_scope(), b_value).unwrap();
        let a_intent = session
            .freeze_execution_intent(&a, vec![a_id], vec![])
            .unwrap();
        let b_intent = session
            .freeze_execution_intent(&b, vec![b_id], vec![])
            .unwrap();
        let stale_a = ephemeral_binding_stage(&mut session, a_intent.clone());
        let b_stage = ephemeral_binding_stage(&mut session, b_intent);
        assert_eq!(
            session
                .publish_staged_public_manifest(b_stage, &PublicationDecision::new())
                .unwrap(),
            super::super::PublicManifestCommit::Ephemeral
        );
        assert_eq!(
            session
                .bindings()
                .resolve_in(session.scope_tree(), public, "x")
                .map(|entry| entry.id),
            Some(b_id)
        );
        let decision = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(stale_a, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), PublicationPhase::Running);
        let current_a = ephemeral_binding_stage(&mut session, a_intent.clone());
        assert_eq!(
            session
                .publish_staged_public_manifest(current_a, &decision)
                .unwrap(),
            PublicManifestCommit::Ephemeral
        );
        assert_eq!(decision.phase(), PublicationPhase::Published);
        assert_eq!(
            session
                .bindings()
                .resolve_in(session.scope_tree(), public, "x")
                .map(|entry| entry.id),
            Some(a_id)
        );
        assert!(session.bindings().get(b_id).is_some());
        assert!(session.lib().durable_graph.is_none());
        assert!(Arc::ptr_eq(
            &session
                .freeze_execution_intent(&a, vec![a_id], vec![])
                .unwrap(),
            &a_intent
        ));
    }

    #[test]
    fn paired_ephemeral_ticket_keeps_attached_manifest_and_admission_mode() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declarations.json");
        let mut lib = SessionLib::open(
            SessionId(4481),
            root.path().join("source"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        lib.attach_recovery_graph_v2(&path).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_isolated_scope();
        let admission = session.begin_ephemeral_private_execution(public).unwrap();
        let value =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "local", 703);
        let id = value.id;
        session.bind_in(admission.private_scope(), value).unwrap();
        let intent = session
            .freeze_execution_intent(&admission, vec![id], vec![])
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let cancelled = PublicationDecision::new();
        cancelled.request_cancellation();
        let stage = ephemeral_binding_stage(&mut session, intent.clone());
        assert_eq!(
            session
                .publish_staged_public_manifest(stage, &cancelled)
                .unwrap(),
            PublicManifestCommit::Cancelled
        );
        assert!(session
            .bindings()
            .resolve_in(session.scope_tree(), public, "local")
            .is_none());
        let unrelated = session.lib_mut().log.reserve();
        let stale = ephemeral_binding_stage(&mut session, intent.clone());
        // A reserved node can settle without changing the generation counter.
        let generation = session.lib().generation();
        let revision = session.lib().log.publication_revision();
        assert!(session.lib_mut().log.commit_reserved_authored(
            unrelated,
            DeclTurn {
                normalized: DeclarationSource {
                    prologue: Default::default(),
                    body: String::new()
                },
                external_imports: SourceImports::new(),
                sources: Vec::new(),
                workbench_imports: SourceImports::new(),
                items: Vec::new(),
                value_types: BTreeMap::new(),
                retracts: Vec::new(),
                parent: None,
            }
        ));
        assert_eq!(session.lib().generation(), generation);
        assert_ne!(session.lib().log.publication_revision(), revision);
        let decision = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(stale, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), PublicationPhase::Running);
        let current = ephemeral_binding_stage(&mut session, intent.clone());
        assert_eq!(
            session
                .publish_staged_public_manifest(current, &decision)
                .unwrap(),
            PublicManifestCommit::Ephemeral
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let owner = RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/ephemeral-mode").unwrap(),
            1,
        )
        .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        assert!(session
            .restage_execution_publication(owner, intent)
            .is_err());
        assert!(session.begin_ephemeral_private_execution(public).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn paired_ephemeral_foreign_runtime_ticket_never_claims() {
        let root = tempfile::tempdir().unwrap();
        let make = || {
            PersistentSession::new(
                Some(
                    SessionLib::open(
                        SessionId(4482),
                        root.path(),
                        ModuleEnv::standalone_default(),
                    )
                    .unwrap(),
                ),
                1024,
            )
        };
        let mut first = make();
        let public = first.mint_isolated_scope();
        let admission = first.begin_ephemeral_private_execution(public).unwrap();
        let intent = first
            .freeze_execution_intent(&admission, vec![], vec![])
            .unwrap();
        let ticket = ephemeral_binding_stage(&mut first, intent.clone());
        let mut second = make();
        let second_public = second.mint_isolated_scope();
        let second_admission = second
            .begin_ephemeral_private_execution(second_public)
            .unwrap();
        second
            .freeze_execution_intent(&second_admission, vec![], vec![])
            .unwrap();
        assert_eq!(
            first.public_visibility_snapshot_in(public),
            second.public_visibility_snapshot_in(second_public)
        );
        assert_eq!(
            first.lib().log.publication_revision(),
            second.lib().log.publication_revision()
        );
        let decision = PublicationDecision::new();
        assert_eq!(
            second
                .publish_staged_public_manifest(ticket, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), PublicationPhase::Running);
        assert!(second
            .restage_ephemeral_execution_publication(intent)
            .is_err());
    }

    #[test]
    fn paired_foreign_staged_ticket_never_claims_publication() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declarations.json");
        let mut lib =
            SessionLib::open(SessionId(986), root.path(), ModuleEnv::standalone_default()).unwrap();
        lib.attach_recovery_graph_v2(&path).unwrap();
        let mut first = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = first.mint_scope(ScopeId::ROOT).unwrap();
        let owner = RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/foreign-stage").unwrap(),
            1,
        )
        .unwrap();
        first
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let private = first.begin_private_execution(public).unwrap();
        let intent = first
            .freeze_execution_intent(&private, vec![], vec![])
            .unwrap();
        let ExecutionPublication::Bindings(base) = first
            .restage_execution_publication(owner.clone(), intent.clone())
            .unwrap()
        else {
            panic!("empty intent publishes bindings");
        };
        let ticket = base.stage().unwrap();
        let mut second_lib =
            SessionLib::open(SessionId(986), root.path(), ModuleEnv::standalone_default()).unwrap();
        second_lib.attach_recovery_graph_v2(&path).unwrap();
        let mut second = PersistentSession::new(Some(second_lib), 1024 * 1024);
        let second_public = second.mint_scope(ScopeId::ROOT).unwrap();
        second
            .bind_durable_public_scope(owner.clone(), second_public)
            .unwrap();
        let second_private = second.begin_private_execution(second_public).unwrap();
        assert_eq!(private.admitted_public(), second_private.admitted_public());
        assert_eq!(
            first.public_visibility_snapshot_in(private.private_scope()),
            second.public_visibility_snapshot_in(second_private.private_scope())
        );
        let decision = PublicationDecision::new();
        assert_eq!(
            second
                .publish_staged_public_manifest(ticket, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), PublicationPhase::Running);
        let ExecutionPublication::Bindings(base) =
            first.restage_execution_publication(owner, intent).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            first
                .publish_staged_public_manifest(base.stage().unwrap(), &decision)
                .unwrap(),
            PublicManifestCommit::Durable
        );
    }

    #[test]
    fn paired_materialized_value_retracts_only_admitted_value_namespace() {
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declarations.json");
        let mut lib =
            SessionLib::open(SessionId(987), root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
        lib.attach_recovery_graph_v2(&path).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let owner = RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/admitted-namespace").unwrap(),
            1,
        )
        .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        commit_source(
            &mut session,
            public,
            include_str!("fixtures/paired-late-operator.hs"),
        );
        let a = session.begin_private_execution(public).unwrap();
        let withdrawal = session.begin_private_execution(public).unwrap();
        let value =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "%%", 601);
        let value_id = value.id;
        session
            .bind_replacing_decl_in(a.private_scope(), value)
            .unwrap();
        let private_tip = tip(session.lib(), session.lib().scope_tip(a.private_scope()))
            .unwrap()
            .unwrap();
        assert!(private_tip
            .exports
            .iter()
            .any(|export| export.head.occurrence == "%%"
                && export.head.namespace == ExportNamespace::Type));
        assert!(!private_tip
            .exports
            .iter()
            .any(|export| export.head.occurrence == "%%"
                && export.head.namespace == ExportNamespace::Value));
        session
            .retract_many_in(withdrawal.private_scope(), &["%%".into()])
            .unwrap();
        let withdrawn = session
            .freeze_execution_intent(&withdrawal, vec![], vec![])
            .unwrap();
        assert_eq!(withdrawn.writes[0].retractions.len(), 2);
        let withdrawn_proof = accepted(
            session
                .restage_declaration_publication(owner.clone(), withdrawn)
                .unwrap(),
        );
        assert!(!withdrawn_proof
            .receipt
            .exports()
            .iter()
            .any(|export| export.head.occurrence == "%%"));
        let intent = session
            .freeze_execution_intent(&a, vec![value_id], vec![])
            .unwrap();
        assert_eq!(intent.writes[0].retractions.len(), 1);
        assert_eq!(
            intent.writes[0].retractions[0].namespace,
            ExportNamespace::Value
        );
        let publication = accepted(
            session
                .restage_declaration_publication(owner, intent)
                .unwrap(),
        );
        assert!(publication
            .receipt
            .exports()
            .iter()
            .any(|export| export.head.occurrence == "%%"
                && export.head.namespace == ExportNamespace::Type));
        assert_eq!(
            session
                .publish_staged_public_manifest(
                    publication.stage().unwrap(),
                    &PublicationDecision::new()
                )
                .unwrap(),
            PublicManifestCommit::Durable
        );
        assert!(session
            .public_visibility_snapshot_in(public)
            .unwrap()
            .bindings
            .contains(&("%%".into(), value_id)));
    }

    #[test]
    fn paired_binding_only_intent_removes_later_value_head_and_preserves_type_namespace() {
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declarations.json");
        let mut lib =
            SessionLib::open(SessionId(984), root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
        lib.attach_recovery_graph_v2(&path).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let owner = RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/binding-late").unwrap(),
            1,
        )
        .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let a = session.begin_private_execution(public).unwrap();
        let value =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "%%", 501);
        let value_id = value.id;
        session.bind_in(a.private_scope(), value).unwrap();
        let intent = session
            .freeze_execution_intent(&a, vec![value_id], vec![])
            .unwrap();
        assert!(intent.writes.is_empty());
        assert!(matches!(
            session
                .restage_execution_publication(owner.clone(), intent.clone())
                .unwrap(),
            ExecutionPublication::Bindings(_)
        ));
        let b = session.begin_private_execution(public).unwrap();
        let original = commit_source(
            &mut session,
            b.private_scope(),
            include_str!("fixtures/paired-late-operator.hs"),
        );
        let b_intent = session.freeze_execution_intent(&b, vec![], vec![]).unwrap();
        let b_stage = accepted(
            session
                .restage_declaration_publication(owner.clone(), b_intent)
                .unwrap(),
        )
        .stage()
        .unwrap();
        assert_eq!(
            session
                .publish_staged_public_manifest(b_stage, &PublicationDecision::new())
                .unwrap(),
            PublicManifestCommit::Durable
        );
        let ExecutionPublication::Declarations(base) = session
            .restage_execution_publication(owner.clone(), intent.clone())
            .unwrap()
        else {
            panic!("later value head requires a compiler-certified join");
        };
        let publication = accepted(base);
        let proof = publication.receipt.clone();
        assert!(
            proof
                .exports()
                .iter()
                .any(|export| export.head.occurrence == "%%"
                    && export.head.namespace == ExportNamespace::Type),
            "certified exports: {:?}",
            proof.exports()
        );
        assert!(!proof
            .exports()
            .iter()
            .any(|export| export.head.occurrence == "%%"
                && export.head.namespace == ExportNamespace::Value));
        assert!(proof
            .exports()
            .iter()
            .any(|export| export.head.occurrence == "laterUnrelated"
                && export.head.module == SessionModule::lib(original).module_name()));
        assert_eq!(
            session
                .publish_staged_public_manifest(
                    publication.stage().unwrap(),
                    &PublicationDecision::new()
                )
                .unwrap(),
            PublicManifestCommit::Durable
        );
        assert_eq!(
            session.lib().scope_tip(public),
            intent.reserved_generation()
        );
        assert!(session
            .public_visibility_snapshot_in(public)
            .unwrap()
            .bindings
            .contains(&("%%".into(), value_id)));
        session.retire_scope(a.private_scope());
        assert!(session.bindings().get(value_id).is_some());
    }

    #[test]
    fn paired_authored_parent_inventory_survives_value_only_tip_and_instance_only_suffix() {
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declarations.json");
        let mut lib =
            SessionLib::open(SessionId(985), root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
        lib.attach_recovery_graph_v2(&path).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let owner = RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/authored-inventory").unwrap(),
            1,
        )
        .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let initial = commit_source(
            &mut session,
            public,
            include_str!("fixtures/paired-rich-base.hs"),
        );
        let value_only = commit_source(
            &mut session,
            public,
            include_str!("fixtures/paired-public-value-only.hs"),
        );
        assert!(session
            .lib()
            .log
            .certified_authored_at(value_only)
            .unwrap()
            .instances()
            .classes
            .is_empty());
        let public_tip = tip(session.lib(), value_only).unwrap().unwrap();
        assert_eq!(public_tip.instances.classes.len(), 1);
        assert_eq!(public_tip.instances.families.len(), 1);
        let a = session.begin_private_execution(public).unwrap();
        let b = session.begin_private_execution(public).unwrap();
        let private = commit_source(
            &mut session,
            a.private_scope(),
            include_str!("fixtures/paired-instance-only.hs"),
        );
        assert!(session
            .lib()
            .log
            .certified_authored_at(private)
            .unwrap()
            .introduced_exports()
            .is_empty());
        let intent = session.freeze_execution_intent(&a, vec![], vec![]).unwrap();
        let accepted_a = accepted(
            session
                .restage_declaration_publication(owner.clone(), intent.clone())
                .unwrap(),
        );
        assert_eq!(accepted_a.receipt.instances().classes.len(), 2);
        assert_eq!(accepted_a.receipt.instances().families.len(), 2);
        assert!(accepted_a
            .receipt
            .exports()
            .iter()
            .any(|export| export.head.module == SessionModule::lib(initial).module_name()));
        let conflicting = commit_source(
            &mut session,
            b.private_scope(),
            include_str!("fixtures/paired-instance-only.hs"),
        );
        assert_ne!(private, conflicting);
        let b_intent = session.freeze_execution_intent(&b, vec![], vec![]).unwrap();
        let b_publication = accepted(
            session
                .restage_declaration_publication(owner.clone(), b_intent)
                .unwrap(),
        );
        assert_eq!(
            session
                .publish_staged_public_manifest(
                    b_publication.stage().unwrap(),
                    &PublicationDecision::new()
                )
                .unwrap(),
            PublicManifestCommit::Durable
        );
        assert!(matches!(
            session
                .restage_declaration_publication(owner, intent)
                .unwrap()
                .certify()
                .unwrap(),
            CertifiedDeclarationPublication::Rejected(_)
        ));
    }

    #[test]
    fn paired_independent_authored_rebase_preserves_originals_and_current_winners() {
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declarations.json");
        let mut lib =
            SessionLib::open(SessionId(994), root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
        lib.attach_recovery_graph_v2(&path).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let private_a = session.mint_detached_scope(public).unwrap();
        let private_b = session.mint_detached_scope(public).unwrap();
        let owner =
            RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/rebase").unwrap(), 1)
                .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let admitted = session.public_visibility_snapshot_in(public).unwrap();
        for (scope, source) in [
            (private_a, include_str!("fixtures/paired-rebase-A.hs")),
            (private_b, include_str!("fixtures/paired-rebase-B.hs")),
        ] {
            let receipt = session
                .lib()
                .declaration_receipt(&[source])
                .unwrap()
                .unwrap();
            let (candidate, values) = session
                .render_declaration_candidate_in(scope, &receipt, &SourceImports::new())
                .unwrap();
            let staged = crate::session::validate_declaration_candidate(
                candidate,
                session.lib().include_dir(),
            )
            .unwrap()
            .with_visible_values(values);
            session.adopt_staged_declaration_in(staged).unwrap();
        }
        let original_a = session.lib().scope_tip(private_a);
        let original_b = session.lib().scope_tip(private_b);
        let authored_b = session
            .lib()
            .log
            .certified_authored_arc_at(original_b)
            .unwrap();
        let value_a =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "fromA", 73);
        let value_a_id = value_a.id;
        session.bind_in(private_a, value_a).unwrap();
        let value_b =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "fromB", 74);
        let value_b_id = value_b.id;
        session.bind_in(private_b, value_b).unwrap();

        let old_b = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private_b,
                    vec![value_b_id],
                    vec![],
                )
                .unwrap(),
        )
        .stage()
        .unwrap();
        let first = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private_a,
                    vec![value_a_id],
                    vec![],
                )
                .unwrap(),
        );
        let first_generation = first.base.reserved;
        assert_eq!(session.publish_staged_public_manifest(
            first.stage().unwrap(), &PublicationDecision::new(),
        ).unwrap(), PublicManifestCommit::Durable);
        let first_public = session.public_visibility_snapshot_in(public).unwrap();
        let decision = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(old_b, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), PublicationPhase::Running);
        assert_eq!(
            session.public_visibility_snapshot_in(public).unwrap(),
            first_public
        );

        let cancelled = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private_b,
                    vec![value_b_id],
                    vec![],
                )
                .unwrap(),
        );
        assert_eq!(
            cancelled
                .receipt
                .input()
                .public_module
                .as_ref()
                .unwrap()
                .module,
            SessionModule::lib(first_generation).module_name()
        );
        let cancellation = PublicationDecision::new();
        cancellation.request_cancellation();
        assert_eq!(
            session
                .publish_staged_public_manifest(cancelled.stage().unwrap(), &cancellation,)
                .unwrap(),
            PublicManifestCommit::Cancelled
        );
        assert_eq!(
            session.public_visibility_snapshot_in(public).unwrap(),
            first_public
        );

        let retry = accepted(
            session
                .snapshot_declaration_publication(
                    owner,
                    &admitted,
                    private_b,
                    vec![value_b_id],
                    vec![],
                )
                .unwrap(),
        );
        assert!(Arc::ptr_eq(
            &retry.base.intent.writes.last().unwrap().evidence,
            &authored_b
        ));
        let generation = retry.base.reserved;
        let proof = retry.receipt.clone();
        for (name, original) in [
            ("keepA", original_a),
            ("keepB", original_b),
            ("answer", original_b),
            ("PublicShape", original_b),
        ] {
            let export = proof
                .exports()
                .iter()
                .find(|export| export.head.occurrence == name)
                .unwrap();
            assert_eq!(
                export.head.module,
                SessionModule::lib(original).module_name()
            );
        }
        let shape = proof
            .exports()
            .iter()
            .find(|export| export.head.occurrence == "PublicShape")
            .unwrap();
        assert_eq!(
            shape
                .children
                .iter()
                .map(|child| child.occurrence.as_str())
                .collect::<Vec<_>>(),
            vec!["NewShape"]
        );
        assert_eq!(
            session
                .publish_staged_public_manifest(retry.stage().unwrap(), &decision,)
                .unwrap(),
            PublicManifestCommit::Durable
        );
        assert_eq!(decision.phase(), PublicationPhase::Published);
        let published = session.public_visibility_snapshot_in(public).unwrap();
        assert_eq!(published.declaration_tip, generation);
        assert_eq!(published.epoch, first_public.epoch + 1);
        assert!(published.bindings.contains(&("fromA".into(), value_a_id)));
        assert!(published.bindings.contains(&("fromB".into(), value_b_id)));
        let turn = session.lib().log.turn(generation).unwrap();
        assert_eq!(turn.items.len(), 4);
        assert!(turn.sources.is_empty());
        assert_eq!(
            turn.value_types.get("answer"),
            session
                .lib()
                .log
                .turn(original_b)
                .unwrap()
                .value_types
                .get("answer")
        );
        let graph = recovery::read_v2(&path, root.path())
            .unwrap()
            .unwrap()
            .graph;
        let joined = graph.nodes().find(|node| node.id == generation).unwrap();
        let mut expected_refs = vec![original_b, first_generation];
        expected_refs.sort();
        assert_eq!(joined.implementation_refs, expected_refs);
        assert_eq!(joined.exports.len(), proof.exports().len());
        assert!(joined.exports.iter().all(|export| export
            .children
            .iter()
            .all(|child| child.occurrence != "OldShape")));
        assert_eq!(
            graph.public_surfaces().next().unwrap().declaration_root,
            Some(generation)
        );
        session.retire_scope(private_a);
        session.retire_scope(private_b);
        assert!(session.bindings().get(value_a_id).is_some());
        assert!(session.bindings().get(value_b_id).is_some());
        let view = session.compile_view_in(public).unwrap();
        let context = view.exact_declaration_context().unwrap();
        assert_eq!(
            context.lexical_graph().len(),
            1 + session
                .lib()
                .log
                .joined_at(generation)
                .unwrap()
                .surface
                .lexical
                .len()
        );
        assert!(context
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == SessionModule::lib(generation).module_name()));
        assert!(context
            .lexical_graph()
            .iter()
            .all(|node| ![original_a, original_b]
                .iter()
                .any(|original| node.owner.module == SessionModule::lib(*original).module_name())));
        for original in [original_a, original_b] {
            assert!(context.recovery_products().iter().any(|product| {
                product.owner().module == SessionModule::lib(original).module_name()
            }));
        }
        let materialized = proof.materialize(root.path()).unwrap();
        assert!(materialized
            .anchors
            .iter()
            .any(|anchor| anchor.module == SessionModule::lib(first_generation).module_name()));
        tidepool_toolchain::recovery_artifacts::verify_materialized_join(
            root.path(),
            &materialized.join,
        )
        .unwrap();
    }

    #[test]
    fn paired_first_authored_publication_fences_outcomes_and_finishes_uncertain_swap() {
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declarations.json");
        let mut lib =
            SessionLib::open(SessionId(993), root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
        lib.attach_recovery_graph_v2(&path).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let private = session.mint_detached_scope(public).unwrap();
        let owner =
            RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/paired").unwrap(), 1)
                .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let admitted = session.public_visibility_snapshot_in(public).unwrap();
        let receipt = session
            .lib()
            .declaration_receipt(&["data PrivateFlag = PrivateFlag\nanswer :: Int\nanswer = 42"])
            .unwrap()
            .unwrap();
        let (candidate, values) = session
            .render_declaration_candidate_in(private, &receipt, &SourceImports::new())
            .unwrap();
        let staged =
            crate::session::validate_declaration_candidate(candidate, session.lib().include_dir())
                .unwrap()
                .with_visible_values(values);
        session.adopt_staged_declaration_in(staged).unwrap();
        assert_eq!(session.lib().scope_tip(private), Generation(1));
        let authored = session
            .lib()
            .log
            .certified_authored_arc_at(Generation(1))
            .unwrap();

        // The original declaration is compiled once. These value writes use
        // genuine registered native roots, and are staged independently.
        let old_answer =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "answer", 71);
        let old_answer_id = old_answer.id;
        session.bind_in(public, old_answer).unwrap();
        let other =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "other", 72);
        let other_id = other.id;
        session.bind_in(private, other).unwrap();
        let incarnation = session
            .public_visibility_snapshot_in(public)
            .unwrap()
            .machine_incarnation;

        let first = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private,
                    vec![other_id],
                    vec![],
                )
                .unwrap(),
        );
        // Obtain a real protected rejection from the same owned closure. Only
        // the expected inventory is wrong; no runtime validation is bypassed.
        let context = ExactDeclarationContext::new(
            &[authored.clone()],
            &[],
            vec![ExactLexicalNode {
                owner: ExactModuleIdentity {
                    unit: authored.product().owner().unit.clone(),
                    module: authored.product().owner().module.clone(),
                },
                imports: Vec::new(),
            }],
        )
        .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let artifacts = context.materialize(scratch.path()).unwrap().artifacts;
        let original = artifacts
            .iter()
            .find(|artifact| artifact.interface.module == authored.product().owner().module)
            .unwrap();
        let anchor = ModuleSnapshot {
            module: original.interface.module.clone(),
            path: original.interface.path.clone(),
            sha256: original.interface.sha256.clone(),
        };
        let mut input = first.receipt.input().clone();
        input.artifacts = artifacts;
        input.private_tip = Some(anchor.clone());
        input.writes[0].module = anchor;
        input.reserved.path = scratch.path().join("rejected.hi");
        input.expected_exports[0]
            .head
            .occurrence
            .push_str("Missing");
        let CertifiedDeclarationJoin::Rejected(receipt) =
            tidepool_toolchain::declaration_join::certify_declaration_join(
                input,
                &context,
                &first.base.includes,
                &first.base.session_root,
            )
            .unwrap()
        else {
            panic!("expected protected inventory rejection");
        };
        let rejected = RejectedDeclarationPublication {
            base: first.base,
            receipt,
        };
        assert!(matches!(
            session.revalidate_declaration_rejection(&rejected).unwrap(),
            DeclarationPublicationRejection::Rejected { .. }
        ));
        let binding_stage = session
            .snapshot_binding_publication(owner.clone(), public, private, vec![])
            .unwrap()
            .stage()
            .unwrap();
        assert_eq!(
            session
                .publish_staged_public_manifest(binding_stage, &PublicationDecision::new())
                .unwrap(),
            PublicManifestCommit::Durable
        );
        assert_eq!(
            session.revalidate_declaration_rejection(&rejected).unwrap(),
            DeclarationPublicationRejection::Stale
        );

        let cancelled_stage = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private,
                    vec![other_id],
                    vec![],
                )
                .unwrap(),
        )
        .stage()
        .unwrap();
        let cancelled = PublicationDecision::new();
        cancelled.request_cancellation();
        assert_eq!(
            session
                .publish_staged_public_manifest(cancelled_stage, &cancelled)
                .unwrap(),
            PublicManifestCommit::Cancelled
        );
        assert_eq!(session.lib().scope_tip(public), Generation(0));
        assert!(session.bindings().get(old_answer_id).is_some());

        let stale_stage = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private,
                    vec![other_id],
                    vec![],
                )
                .unwrap(),
        )
        .stage()
        .unwrap();
        session.lib_mut().reserve_join_generation_durable().unwrap();
        let decision = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(stale_stage, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), PublicationPhase::Running);
        assert_eq!(session.lib().scope_tip(public), Generation(0));

        let retry = accepted(
            session
                .snapshot_declaration_publication(owner, &admitted, private, vec![other_id], vec![])
                .unwrap(),
        );
        let generation = retry.base.reserved;
        let proof = retry.receipt.clone();
        let ticket = retry.stage().unwrap();
        let before_epoch = session.public_visibility_snapshot_in(public).unwrap().epoch;
        session.lib_mut().fail_recovery_durability_once = true;
        assert!(matches!(
            session
                .publish_staged_public_manifest(ticket, &decision)
                .unwrap(),
            PublicManifestCommit::PublishedDurabilityUnconfirmed { .. }
        ));
        assert_eq!(decision.phase(), PublicationPhase::Published);
        let published = session.public_visibility_snapshot_in(public).unwrap();
        assert_eq!(published.epoch, before_epoch + 1);
        assert_eq!(published.declaration_tip, generation);
        assert_eq!(published.machine_incarnation, incarnation);
        assert!(!published.bindings.iter().any(|(name, _)| name == "answer"));
        assert!(published
            .bindings
            .iter()
            .any(|(name, id)| name == "other" && *id == other_id));
        let compiled_against = session.compile_view_in(public).unwrap();
        let context = compiled_against
            .exact_declaration_context()
            .unwrap()
            .clone();
        assert_eq!(
            context.lexical_graph().len(),
            1 + session
                .lib()
                .log
                .joined_at(generation)
                .unwrap()
                .surface
                .lexical
                .len()
        );
        assert!(context
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == SessionModule::lib(generation).module_name()));
        assert!(context
            .lexical_graph()
            .iter()
            .all(|node| node.owner.module != authored.product().owner().module));
        assert!(context
            .recovery_products()
            .iter()
            .any(|product| product.owner() == authored.product().owner()));
        assert!(Arc::ptr_eq(
            compiled_against.exact_declaration_context().unwrap(),
            session
                .compile_view_in(public)
                .unwrap()
                .exact_declaration_context()
                .unwrap(),
        ));
        let graph = recovery::read_v2(&path, root.path())
            .unwrap()
            .unwrap()
            .graph;
        let joined = graph.nodes().find(|node| node.id == generation).unwrap();
        assert_eq!(joined.kind, recovery::RecoveryNodeKind::Join);
        assert_eq!(joined.implementation_refs, vec![Generation(1)]);
        assert_eq!(
            graph.public_surfaces().next().unwrap().declaration_root,
            Some(generation)
        );
        assert!(!graph
            .public_surfaces()
            .next()
            .unwrap()
            .bindings
            .iter()
            .any(|binding| binding.name == "answer"));
        assert!(
            matches!(session.lib().log.turn(generation), Some(turn) if turn.sources.is_empty())
        );
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::super::render::render_module(
                &session.lib().log,
                generation,
                &ModuleEnv::standalone_default(),
            )
        }))
        .is_err());
        let stored = proof.materialize(root.path()).unwrap();
        assert_eq!(
            tidepool_toolchain::recovery_artifacts::verify_materialized_join(
                root.path(),
                &stored.join
            )
            .unwrap()
            .interface_bytes,
            proof.interface_bytes()
        );
        let bytes = std::fs::read(&path).unwrap();
        session.lib_mut().confirm_recovery_durability().unwrap();
        session.lib_mut().confirm_recovery_durability().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            session.public_visibility_snapshot_in(public).unwrap(),
            published
        );
        session.retire_scope(private);
        assert!(session.bindings().get(other_id).is_some());
        assert_eq!(session.lib().scope_tip(public), generation);
        let retained = session.compile_view_in(public).unwrap();
        assert!(retained.is_current_for(&compiled_against));
        assert!(Arc::ptr_eq(
            retained.exact_declaration_context().unwrap(),
            &context
        ));
    }
    fn commit_source(session: &mut PersistentSession, scope: ScopeId, source: &str) -> Generation {
        session.define_scoped_in(scope, &[source]).unwrap()
    }

    #[test]
    fn paired_rich_fixed_suffix_rebases_nonempty_base_and_final_value_winners() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declarations.json");
        let mut lib =
            SessionLib::open(SessionId(996), root.path(), ModuleEnv::standalone_default()).unwrap();
        lib.attach_recovery_graph_v2(&path).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let owner = RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/rich-fixed").unwrap(),
            1,
        )
        .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let base_admission = session.begin_private_execution(public).unwrap();
        let base_original = commit_source(
            &mut session,
            base_admission.private_scope(),
            include_str!("fixtures/paired-rich-base.hs"),
        );
        let base = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    base_admission.admitted_public(),
                    base_admission.private_scope(),
                    vec![],
                    vec![],
                )
                .unwrap(),
        );
        assert_eq!(base.receipt.instances().classes.len(), 1);
        assert_eq!(
            session
                .publish_staged_public_manifest(base.stage().unwrap(), &PublicationDecision::new())
                .unwrap(),
            PublicManifestCommit::Durable
        );

        let a = session.begin_private_execution(public).unwrap();
        let b = session.begin_private_execution(public).unwrap();
        assert_ne!(a.admitted_public().declaration_tip, Generation(0));
        let a_first = commit_source(
            &mut session,
            a.private_scope(),
            include_str!("fixtures/paired-rich-A1.hs"),
        );
        let a_second = commit_source(
            &mut session,
            a.private_scope(),
            include_str!("fixtures/paired-rich-A2.hs"),
        );
        session
            .retract_many_in(
                a.private_scope(),
                &["retractValue".into(), "baseValue".into()],
            )
            .unwrap();
        let value = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session,
            "retractValue",
            401,
        );
        let value_id = value.id;
        session.bind_in(a.private_scope(), value).unwrap();
        let intent = session
            .freeze_execution_intent(&a, vec![value_id], vec![])
            .unwrap();
        assert_eq!(intent.writes.len(), 3);
        assert_eq!(intent.writes[2].retractions.len(), 2);
        let high_water = session.lib().log.generation();
        let repeated = session
            .freeze_execution_intent(&a, vec![value_id], vec![])
            .unwrap();
        assert!(Arc::ptr_eq(&intent, &repeated));
        assert_eq!(session.lib().log.generation(), high_water);
        assert!(session.freeze_execution_intent(&a, vec![], vec![]).is_err());
        let reserved = intent.reserved_generation();
        let old_a = accepted(
            session
                .restage_declaration_publication(owner.clone(), intent.clone())
                .unwrap(),
        )
        .stage()
        .unwrap();

        let b_original = commit_source(
            &mut session,
            b.private_scope(),
            include_str!("fixtures/paired-rich-B.hs"),
        );
        let b_publication = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    b.admitted_public(),
                    b.private_scope(),
                    vec![],
                    vec![],
                )
                .unwrap(),
        );
        assert!(b_publication.base.reserved > reserved);
        assert_eq!(
            session
                .publish_staged_public_manifest(
                    b_publication.stage().unwrap(),
                    &PublicationDecision::new()
                )
                .unwrap(),
            PublicManifestCommit::Durable
        );
        let decision = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(old_a, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), PublicationPhase::Running);
        let publication = accepted(
            session
                .restage_declaration_publication(owner.clone(), intent.clone())
                .unwrap(),
        );
        assert_eq!(publication.base.reserved, reserved);
        assert!(Arc::ptr_eq(publication.intent(), &intent));
        let proof = publication.receipt.clone();
        assert_eq!(proof.instances().classes.len(), 2);
        assert_eq!(proof.instances().families.len(), 2);
        assert!(proof.family_closure().len() >= 2);
        for (name, original) in [
            ("firstPrivate", a_first),
            ("privateWinner", a_second),
            ("fromLaterPublic", b_original),
            ("baseValue", b_original),
        ] {
            let export = proof
                .exports()
                .iter()
                .find(|export| export.head.occurrence == name)
                .unwrap();
            assert_eq!(
                export.head.module,
                SessionModule::lib(original).module_name()
            );
        }
        assert!(!proof
            .exports()
            .iter()
            .any(|export| export.head.occurrence == "retractValue"));
        let record = proof
            .exports()
            .iter()
            .find(|export| export.head.occurrence == "PublicRecord")
            .unwrap();
        assert_eq!(
            record.head.module,
            SessionModule::lib(base_original).module_name()
        );
        assert!(record
            .children
            .iter()
            .any(|child| child.occurrence == "publicField"
                && child.record_parent.as_deref() == Some("PublicRecord")));
        assert_eq!(
            session
                .publish_staged_public_manifest(publication.stage().unwrap(), &decision)
                .unwrap(),
            PublicManifestCommit::Durable
        );
        assert_eq!(session.lib().scope_tip(public), reserved);
        assert!(session
            .public_visibility_snapshot_in(public)
            .unwrap()
            .bindings
            .iter()
            .any(|(name, id)| name == "retractValue" && *id == value_id));
        let graph = recovery::read_v2(&path, root.path())
            .unwrap()
            .unwrap()
            .graph;
        let node = graph.nodes().find(|node| node.id == reserved).unwrap();
        assert!(node
            .implementation_refs
            .iter()
            .any(|reference| *reference > reserved));
        assert_eq!(node.instances.classes.len(), 2);
        assert_eq!(
            node.lexical_roots[0].module,
            SessionModule::lib(reserved).module_name()
        );
        session.retire_scope(a.private_scope());
        assert!(session.bindings().get(value_id).is_some());
    }
}
