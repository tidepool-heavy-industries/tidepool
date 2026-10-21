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
    acquisitions: Vec<Arc<ResolvedNativeDemand>>,
    acquired_uses: BTreeMap<crate::certified_products::ReceiptImportOwner, usize>,
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
                .inputs
                .resolve_native_demand(&unacquired, self.validation)?;
            if !resolved.belongs_to(&self.completion.inputs) {
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
                    Arc<DemandCertifiedGroups>,
                )>(batch.original_uses().len())
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            for reference in batch.original_uses() {
                let projected = batch.restrict_uses(std::slice::from_ref(reference))?;
                reply_groups.insert(reference, Arc::clone(projected.shared_groups()));
            }
        }
        self.validation
            .inventory
            .reserve::<native_demand::NativeDemandUseReply<'_>>(reply_groups.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let replies = reply_groups
            .iter()
            .map(|(reference, groups)| native_demand::NativeDemandUseReply {
                original: reference,
                obligations: groups.package_obligations(),
            })
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
