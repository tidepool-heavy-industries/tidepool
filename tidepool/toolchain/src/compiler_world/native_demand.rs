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
    pub(super) context: Arc<ExactDeclarationContext>,
    pub(super) inputs: Arc<RequestCompilerInputs>,
    stages: BTreeMap<u64, WorldNativeDemandStage>,
}

pub(crate) struct WorldNativeDemandStage {
    batches: Vec<Arc<ResolvedNativeDemand>>,
    selected_groups: Arc<DemandCertifiedGroups>,
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
        &self.context
    }
    pub(crate) fn initial_inputs(&self) -> &Arc<RequestCompilerInputs> {
        &self.inputs
    }

    pub(crate) fn stage(&self, stage: u64) -> Option<&WorldNativeDemandStage> {
        self.stages.get(&stage)
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
        let mut context = Arc::clone(&world.restoration.inputs.primary_context);
        let mut inputs = Arc::clone(&world.restoration.primary_compiler_inputs);
        if let Some(operand) = reservation.plan().operand() {
            let types = Arc::new(ExactDeclarationContext::checked_value_interface_context(
                operand.checked_row(),
            )?);
            let advance = inputs.with_type_interfaces(&[types])?;
            context = advance.context;
            inputs = advance.inputs;
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
                context,
                inputs,
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
        let resolved = self
            .completion
            .inputs
            .resolve_native_demand(&fresh, self.validation)?;
        if !resolved.belongs_to(&self.completion.inputs) {
            return Err(source_failure(
                "native demand resolved against another input owner",
            ));
        }
        let groups = resolved.groups();
        let reply =
            native_demand::encode_reply(groups.package_obligations(), &self.validation.inventory)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        let selected_groups = match prior {
            Some(prior) => Arc::new(prior.selected_groups.merge(groups)?),
            None => Arc::clone(resolved.shared_groups()),
        };
        self.validation
            .inventory
            .reserve::<Arc<ResolvedNativeDemand>>(1)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        if prior.is_none() {
            self.validation
                .inventory
                .reserve::<(u64, WorldNativeDemandStage)>(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        }
        let resolved = Arc::new(resolved);
        // No request or Original fact is changed until this whole batch succeeds.
        match self.completion.stages.entry(demand.stage) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let stage = entry.get_mut();
                stage.batches.push(resolved);
                stage.selected_groups = selected_groups;
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(WorldNativeDemandStage {
                    batches: vec![resolved],
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
