//! The active World request retains its exact input and selected demand owners.
//! Framing and cancellation remain with the existing compiler transaction.

use super::*;
use crate::certified_products::native_demand::{self, NativeDemandRefusalReason};
use crate::declaration_context::{
    DemandCertifiedGroups, RequestCompilerInputs, ResolvedNativeDemand,
};

pub(crate) struct WorldNativeDemandCompletion {
    pub(super) reservation: Arc<crate::cell_plan::CompilerWorldCellReservation>,
    pub(super) operation: Arc<tidepool_repr::execution_schema::InventoryOperation>,
    pub(super) primary: crate::declaration_context::WorldPrimaryInputs,
    acquisitions: Vec<Arc<ResolvedNativeDemand>>,
    acquired_uses: BTreeMap<crate::certified_products::ReceiptImportOwner, usize>,
    stages: BTreeMap<u64, WorldNativeDemandStage>,
}

pub(crate) struct WorldNativeDemandStage {
    batches: Vec<Arc<ResolvedNativeDemand>>,
    selected_groups: Arc<DemandCertifiedGroups>,
}

/// Initial native permission for one actually emitted stage. The complete
/// request retains each resolved demand owner; the advancing source prefix is
/// deliberately not a replacement acquisition for these historical choices.
#[derive(Clone)]
pub(crate) struct CheckedWorldStageNativeInputs {
    request: Arc<WorldNativeDemandCompletion>,
    stage: Arc<CheckedWorldCellStage>,
    offset: u64,
}

impl std::fmt::Debug for CheckedWorldStageNativeInputs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CheckedWorldStageNativeInputs")
            .field("stage", &self.stage.index())
            .field("first_ordinal", &self.offset)
            .field("has_initial_selection", &self.selection().is_some())
            .finish_non_exhaustive()
    }
}

impl CheckedWorldStageNativeInputs {
    pub(crate) fn initial_inputs(&self) -> &Arc<RequestCompilerInputs> {
        self.request.initial_inputs()
    }

    pub(crate) fn operation(&self) -> &Arc<tidepool_repr::execution_schema::InventoryOperation> {
        self.request.operation()
    }

    pub(crate) fn stage(&self) -> &Arc<CheckedWorldCellStage> {
        &self.stage
    }

    pub(crate) fn selection(&self) -> Option<&WorldNativeDemandStage> {
        // Absence is known empty only because checked_stage proved that this
        // exact stage was emitted by the same completed request.
        self.request.stages.get(&self.offset)
    }
}

impl WorldNativeDemandStage {
    pub(crate) fn batches(&self) -> &[Arc<ResolvedNativeDemand>] {
        &self.batches
    }
    pub(crate) fn selected_groups(&self) -> &Arc<DemandCertifiedGroups> {
        &self.selected_groups
    }
}

impl WorldNativeDemandCompletion {
    pub(crate) fn reservation(&self) -> &Arc<crate::cell_plan::CompilerWorldCellReservation> {
        &self.reservation
    }
    pub(crate) fn operation(&self) -> &Arc<tidepool_repr::execution_schema::InventoryOperation> {
        &self.operation
    }
    pub(crate) fn initial_context(&self) -> &Arc<ExactDeclarationContext> {
        self.primary.context()
    }
    pub(crate) fn initial_inputs(&self) -> &Arc<RequestCompilerInputs> {
        self.primary.inputs()
    }

    pub(crate) fn checked_stage(
        self: &Arc<Self>,
        program: &CheckedWorldCellProgram,
        stage: &Arc<CheckedWorldCellStage>,
        validation: &crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<CheckedWorldStageNativeInputs, CompileError> {
        crate::host_work::checkpoint()?;
        if !Arc::ptr_eq(self, program.native_demand_completion())
            || !Arc::ptr_eq(&self.reservation, program.reservation())
            || !Arc::ptr_eq(&self.operation, &program.validation().inventory)
            || !Arc::ptr_eq(&self.operation, &validation.inventory)
        {
            return Err(source_failure(
                "World stage native inputs belong to another request",
            ));
        }
        // index is the emitted-stage vector position. The first lexical
        // ordinal is the independently checked key used by native exchange.
        if program
            .stages()
            .get(stage.index())
            .is_none_or(|emitted| !Arc::ptr_eq(emitted, stage))
        {
            return Err(source_failure(
                "World stage native inputs lack an emitted stage",
            ));
        }
        let first = *stage
            .ordinals()
            .first()
            .ok_or_else(|| source_failure("World stage native inputs have no ordinal"))?;
        if first >= self.reservation.slots().len()
            || program
                .stages()
                .binary_search_by_key(&first, |emitted| emitted.ordinals()[0])
                != Ok(stage.index())
        {
            return Err(source_failure(
                "World stage native inputs have another ordinal",
            ));
        }
        let offset = u64::try_from(first)
            .map_err(|_| source_failure("World native stage ordinal is out of range"))?;
        Ok(CheckedWorldStageNativeInputs {
            request: Arc::clone(self),
            stage: Arc::clone(stage),
            offset,
        })
    }

    pub(super) fn require_emitted_stages(
        &self,
        stages: &[Arc<CheckedWorldCellStage>],
    ) -> Result<(), CompileError> {
        for offset in self.stages.keys() {
            crate::host_work::checkpoint()?;
            let offset = usize::try_from(*offset)
                .map_err(|_| source_failure("native demand stage offset is out of range"))?;
            if stages
                .binary_search_by(|stage| stage.ordinals().first().copied().cmp(&Some(offset)))
                .is_err()
            {
                return Err(source_failure(
                    "native demand has no actual completed World stage",
                ));
            }
        }
        Ok(())
    }
}

/// Exercise the issuer with an actual completed compiler program. Altered
/// observations only test refusal; they never return accepted stage authority.
#[cfg(test)]
pub(crate) fn assert_captured_stage_input_refusals(program: &CheckedWorldCellProgram) {
    let request = program.native_demand_completion();
    let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(
        Arc::clone(&program.validation().inventory),
    );
    let mut selected_uses = 0;
    let mut unselected_uses = 0;
    let mut foreign_stage_contributions = 0;
    for stage in program.stages() {
        let selected = request
            .checked_stage(program, stage, &validation)
            .expect("actual emitted stage retains its initial native inputs");
        assert!(Arc::ptr_eq(selected.stage(), stage));
        assert!(Arc::ptr_eq(
            selected.initial_inputs(),
            request.initial_inputs()
        ));
        assert!(Arc::ptr_eq(selected.operation(), request.operation()));
        let recorded = request.stages.get(&selected.offset);
        assert_eq!(selected.selection().is_some(), recorded.is_some());
        if let Some(selection) = selected.selection() {
            assert!(std::ptr::eq(selection, recorded.unwrap()));
            for batch in selection.batches() {
                assert!(batch.belongs_to(request.initial_inputs()));
            }
        }

        let scoped = crate::certified_products::OriginalBindingSelection::from_world_stage(
            Arc::new(selected.clone()),
            request.acquired_uses.keys(),
        )
        .expect("actual request acquisitions authenticate metadata for each emitted stage");
        assert!(scoped.belongs_to_world_stage(&selected, selected.operation()));
        let other_stage = program
            .stages()
            .iter()
            .find(|other| !Arc::ptr_eq(stage, other))
            .expect("mixed producer emits another actual World stage");
        let other = request
            .checked_stage(program, other_stage, &validation)
            .expect("the same request authenticates its other actual emitted stage");
        assert!(!Arc::ptr_eq(selected.stage(), other.stage()));
        assert!(Arc::ptr_eq(
            selected.initial_inputs(),
            other.initial_inputs()
        ));
        assert!(Arc::ptr_eq(selected.operation(), other.operation()));
        let other_scoped = crate::certified_products::OriginalBindingSelection::from_world_stage(
            Arc::new(other.clone()),
            request.acquired_uses.keys(),
        )
        .expect("actual request acquisitions authenticate metadata for the other emitted stage");
        assert!(
            !other_scoped.belongs_to_world_stage(&selected, selected.operation()),
            "same request, operation and initial basis cannot substitute another emitted stage"
        );
        for (reference, acquisition) in &request.acquired_uses {
            let acquired = &request.acquisitions[*acquisition];
            let (acquired_binding, _) = acquired
                .groups()
                .original_use_row(reference)
                .expect("actual request use retains its issued exact group");
            let expected =
                recorded.and_then(|stage| stage.selected_groups.original_use_row(reference));
            let executable = scoped.executable(reference, selected.operation()).unwrap();
            match expected {
                Some((binding, _)) => {
                    selected_uses += 1;
                    assert_eq!(
                        binding, acquired_binding,
                        "stage choice reuses the actual request's exact issued group"
                    );
                    let support = scoped
                        .world_stage_group_support(&selected, reference, selected.operation())
                        .expect("selected use lends its already acquired contribution");
                    assert_eq!(support.binding(), binding);
                    assert_eq!(
                        executable,
                        acquired
                            .groups()
                            .resolved_original_bindings()
                            .unwrap()
                            .get(reference)
                    );
                }
                None => {
                    unselected_uses += 1;
                    assert!(
                        executable.is_none(),
                        "a request-wide acquisition cannot become another stage's executable use"
                    );
                    assert!(scoped.metadata(reference).is_some());
                    let error = scoped
                        .world_stage_group_support(&selected, reference, selected.operation())
                        .err()
                        .expect("acquired availability cannot lend an unselected stage occurrence");
                    assert!(matches!(&error, CompileError::CompilerEvidence(error)
                        if matches!(error.as_ref(), crate::certified_products::CertificationError::Mismatch(
                            "target Original lost its actual World occurrence"))),
                        "unselected contribution must refuse at the actual stage ceiling: {error:?}");
                }
            }
            let error = scoped
                .world_stage_group_support(&other, reference, selected.operation())
                .err()
                .expect("shared request and operation cannot substitute another actual stage");
            assert!(
                matches!(&error, CompileError::CompilerEvidence(error)
                if matches!(error.as_ref(), crate::certified_products::CertificationError::Mismatch(
                    "Original contribution belongs to another World stage"))),
                "cross-stage contribution must refuse its mismatched owner: {error:?}"
            );
            foreign_stage_contributions += 1;
            assert_eq!(
                scoped
                    .world_stage_group_support(&selected, reference, selected.operation())
                    .map(|support| support.binding())
                    .ok(),
                expected.map(|(binding, _)| binding),
                "cross-stage refusal preserves the actual stage's exact contribution decision"
            );
            let foreign = crate::recovery_artifacts::PackageInterfaceValidation::default();
            assert!(
                scoped.executable(reference, &foreign.inventory).is_err(),
                "foreign operation refuses before observing an empty stage"
            );
        }

        // Equal observations cannot issue another emitted-stage identity.
        let unknown = Arc::new(stage.as_ref().clone());
        assert!(request
            .checked_stage(program, &unknown, &validation)
            .is_err());
        let foreign_validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        assert!(request
            .checked_stage(program, stage, &foreign_validation)
            .is_err());
        assert!(
            request.checked_stage(program, stage, &validation).is_ok(),
            "refusal preserves the actual stage owner"
        );
    }
    assert!(
        selected_uses > 0 && unselected_uses > 0,
        "mixed producer must exercise both selected and unselected actual stage uses"
    );
    assert!(foreign_stage_contributions > 0);
    eprintln!("actual Original stage ceiling: {selected_uses} selected uses, {unselected_uses} unselected uses");
    eprintln!("actual Original stage contributions: {selected_uses} selected, {unselected_uses} unselected refusals, {foreign_stage_contributions} foreign-stage refusals");
    let (foreign, refusal) = WorldNativeDemandResolver::new(program.reservation(), &mut validation)
        .expect("same reservation has a distinct initial request acquisition")
        .finish();
    assert!(refusal.is_none());
    assert!(foreign
        .checked_stage(program, &program.stages()[0], &validation)
        .is_err());
    assert!(request
        .checked_stage(program, &program.stages()[0], &validation)
        .is_ok());
}

pub(super) struct WorldNativeDemandResolver<'a> {
    owner: tidepool_extract_cmd::NativeDemandOwner,
    slots: usize,
    completion: WorldNativeDemandCompletion,
    validation: &'a mut crate::recovery_artifacts::PackageInterfaceValidation,
    refusals: [Vec<u8>; 6],
    refusal: Option<CompileError>,
}

impl<'a> WorldNativeDemandResolver<'a> {
    pub(super) fn new(
        reservation: &Arc<crate::cell_plan::CompilerWorldCellReservation>,
        validation: &'a mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Self, CompileError> {
        let world = reservation.plan().world();
        let mut primary = world.restoration.primary_inputs.clone();
        if let Some(operand) = reservation.plan().operand() {
            let types = Arc::new(ExactDeclarationContext::checked_value_interface_context(
                operand.checked_row(),
            )?);
            primary = primary.with_type_interfaces(&[types])?.primary;
        }
        // Reserve the one terminal semantic-refusal reply before traversing
        // untrusted demand. Exhausting admission work must still be reportable.
        let messages = [
            (
                NativeDemandRefusalReason::InvalidWitness,
                "invalid native input witness",
            ),
            (
                NativeDemandRefusalReason::OriginalUnavailable,
                "selected Original unavailable",
            ),
            (
                NativeDemandRefusalReason::NativeGroupUnavailable,
                "native group unavailable",
            ),
            (
                NativeDemandRefusalReason::PackageProductUnavailable,
                "package product unavailable",
            ),
            (
                NativeDemandRefusalReason::InputConflict,
                "native input conflict",
            ),
            (
                NativeDemandRefusalReason::AdmissionLimit,
                "native demand admission limit",
            ),
        ];
        let mut frames = Vec::with_capacity(messages.len());
        for (reason, message) in messages {
            let frame = native_demand::encode_refusal(reason, message, &validation.inventory)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
            validation
                .inventory
                .charge(frame.len())
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            frames.push(frame);
        }
        let refusals = frames
            .try_into()
            .map_err(|_| source_failure("native refusal frame count"))?;
        Ok(Self {
            owner: tidepool_extract_cmd::NativeDemandOwner {
                admission: *reservation.admission_digest(),
                world: *world.logical_id(),
                inputs: world.restoration.compiler_input_identity,
            },
            slots: reservation.slots().len(),
            completion: WorldNativeDemandCompletion {
                reservation: Arc::clone(reservation),
                operation: Arc::clone(&validation.inventory),
                primary,
                acquisitions: Vec::new(),
                acquired_uses: BTreeMap::new(),
                stages: BTreeMap::new(),
            },
            validation,
            refusals,
            refusal: None,
        })
    }

    pub(super) fn owner(&self) -> tidepool_extract_cmd::NativeDemandOwner {
        self.owner.clone()
    }

    pub(super) fn finish(self) -> (Arc<WorldNativeDemandCompletion>, Option<CompileError>) {
        (Arc::new(self.completion), self.refusal)
    }

    fn resolve(
        &mut self,
        demand: &tidepool_extract_cmd::NativeDemand,
    ) -> Result<Vec<u8>, CompileError> {
        if demand.owner != self.owner
            || usize::try_from(demand.stage)
                .ok()
                .is_none_or(|stage| stage >= self.slots)
        {
            return Err(source_failure(
                "native demand belongs to another preparation stage",
            ));
        }
        let packet = crate::certified_products::NativeDemandPacket::decode(
            &demand.payload,
            &self.validation.inventory,
        )
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        let prior = self.completion.stages.get(&demand.stage);
        let fresh = packet
            .without_known(
                |reference| {
                    prior
                        .is_some_and(|stage| stage.selected_groups.contains_original_use(reference))
                },
                &self.validation.inventory,
            )
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        if fresh.originals().is_empty() {
            return Err(source_failure("native demand made no selection progress"));
        }
        let unacquired = fresh
            .without_known(
                |reference| self.completion.acquired_uses.contains_key(reference),
                &self.validation.inventory,
            )
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        let new_index = self.completion.acquisitions.len();
        let acquired = if unacquired.originals().is_empty() {
            None
        } else {
            let resolved = self
                .completion
                .primary
                .inputs()
                .resolve_native_demand(&unacquired, self.validation)?;
            if !resolved.belongs_to(self.completion.primary.inputs()) {
                return Err(source_failure(
                    "native demand resolved against another input owner",
                ));
            }
            Some(Arc::new(resolved))
        };
        self.validation
            .inventory
            .reserve::<(usize, Vec<crate::certified_products::ReceiptImportOwner>)>(
                fresh.originals().len(),
            )
            .and_then(|_| {
                self.validation
                    .inventory
                    .reserve::<crate::certified_products::ReceiptImportOwner>(
                        fresh.originals().len(),
                    )
            })
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut requested =
            BTreeMap::<usize, Vec<crate::certified_products::ReceiptImportOwner>>::new();
        for reference in fresh.into_originals() {
            crate::host_work::checkpoint()?;
            let index = self
                .completion
                .acquired_uses
                .get(&reference)
                .copied()
                .unwrap_or(new_index);
            requested.entry(index).or_default().push(reference);
        }
        self.validation
            .inventory
            .reserve::<Arc<ResolvedNativeDemand>>(requested.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut batches = Vec::with_capacity(requested.len());
        let mut groups: Option<Arc<DemandCertifiedGroups>> = None;
        for (index, references) in requested {
            let acquisition = self
                .completion
                .acquisitions
                .get(index)
                .or_else(|| {
                    if index == new_index {
                        acquired.as_ref()
                    } else {
                        None
                    }
                })
                .ok_or_else(|| source_failure("native use lost its request acquisition"))?;
            let selected = Arc::new(acquisition.restrict_uses(&references)?);
            groups = Some(match groups {
                Some(prior) => Arc::new(prior.merge(selected.groups())?),
                None => Arc::clone(selected.shared_groups()),
            });
            batches.push(selected);
        }
        let groups =
            groups.ok_or_else(|| source_failure("native demand lost its fresh selected uses"))?;
        let mut reply_groups = BTreeMap::new();
        for batch in &batches {
            self.validation
                .inventory
                .reserve::<(
                    &crate::certified_products::ReceiptImportOwner,
                    Vec<&crate::declaration_context::NativePackageObligation>,
                )>(batch.original_uses().len())
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            for reference in batch.original_uses() {
                let obligations = batch.groups().package_obligations_for_use(reference)?;
                reply_groups.insert(reference, obligations);
            }
        }
        self.validation
            .inventory
            .reserve::<native_demand::NativeDemandUseReply<'_>>(reply_groups.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let replies = reply_groups
            .iter()
            .map(
                |(reference, obligations)| native_demand::NativeDemandUseReply {
                    original: reference,
                    obligations,
                },
            )
            .collect::<Vec<_>>();
        let reply = native_demand::encode_reply(&replies, &self.validation.inventory)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        let selected_groups = match prior {
            Some(prior) => Arc::new(prior.selected_groups.merge(&groups)?),
            None => groups,
        };
        self.validation
            .inventory
            .reserve::<Arc<ResolvedNativeDemand>>(batches.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        if prior.is_none() {
            self.validation
                .inventory
                .reserve::<(u64, WorldNativeDemandStage)>(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        }
        if acquired.is_some() {
            self.validation
                .inventory
                .reserve::<Arc<ResolvedNativeDemand>>(1)
                .and_then(|_| {
                    self.validation
                        .inventory
                        .reserve::<(crate::certified_products::ReceiptImportOwner, usize)>(
                            unacquired.originals().len(),
                        )
                })
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        }
        // No request or Original fact is changed until this whole batch succeeds.
        if let Some(acquired) = acquired {
            self.completion.acquisitions.push(acquired);
            for reference in unacquired.into_originals() {
                self.completion.acquired_uses.insert(reference, new_index);
            }
        }
        match self.completion.stages.entry(demand.stage) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let stage = entry.get_mut();
                stage.batches.extend(batches);
                stage.selected_groups = selected_groups;
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(WorldNativeDemandStage {
                    batches,
                    selected_groups,
                });
            }
        }
        Ok(reply)
    }
}

fn interruption(error: &CompileError) -> Option<std::io::ErrorKind> {
    let kind = match error {
        CompileError::Io(error) => error.kind(),
        CompileError::CompilerEvidence(error) => match error.as_ref() {
            crate::certified_products::CertificationError::Interrupted(error) => error.kind(),
            crate::certified_products::CertificationError::OriginalClosure(error) => {
                return interruption(error)
            }
            _ => return None,
        },
        _ => return None,
    };
    matches!(
        kind,
        std::io::ErrorKind::Interrupted | std::io::ErrorKind::TimedOut
    )
    .then_some(kind)
}

fn refusal_reason(error: &CompileError) -> NativeDemandRefusalReason {
    use crate::certified_products::CertificationError as C;
    match error {
        CompileError::ArtifactInventory(_) => NativeDemandRefusalReason::NativeGroupUnavailable,
        CompileError::CompilerEvidence(error) => match error.as_ref() {
            C::OriginalAvailability(_) => NativeDemandRefusalReason::NativeGroupUnavailable,
            C::OriginalMembership(_) | C::Receipt(_) => NativeDemandRefusalReason::InvalidWitness,
            C::InputDecision {
                reason: crate::certified_products::InputDecisionRefusal::RoleMismatch,
                ..
            } => NativeDemandRefusalReason::OriginalUnavailable,
            C::InputDecision { .. } => NativeDemandRefusalReason::InputConflict,
            C::SizeLimit { .. }
            | C::Product(tidepool_repr::execution_schema::ParseError::LimitExceeded(_)) => {
                NativeDemandRefusalReason::AdmissionLimit
            }
            C::OriginalClosure(error) => refusal_reason(error),
            _ => NativeDemandRefusalReason::InputConflict,
        },
        _ => NativeDemandRefusalReason::InputConflict,
    }
}

impl tidepool_extract_cmd::NativeDemandResolver for WorldNativeDemandResolver<'_> {
    fn resolve_native_demand(
        &mut self,
        demand: &tidepool_extract_cmd::NativeDemand,
        control: &tidepool_extract_cmd::NativeDemandControl,
    ) -> std::io::Result<tidepool_extract_cmd::NativeReply> {
        control.checkpoint()?;
        if self.refusal.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compiler demanded native inputs after semantic refusal",
            ));
        }
        match self.resolve(demand) {
            Ok(reply) => {
                control.checkpoint()?;
                Ok(tidepool_extract_cmd::NativeReply::Resolved(reply))
            }
            Err(error) => {
                if let Some(kind) = interruption(&error) {
                    return Err(std::io::Error::new(
                        kind,
                        "native input resolution interrupted",
                    ));
                }
                control.checkpoint()?;
                let reply = self.refusals[refusal_reason(&error) as usize].clone();
                self.refusal = Some(error);
                Ok(tidepool_extract_cmd::NativeReply::Refused(reply))
            }
        }
    }
}
