//! Generation provenance travels through the existing run-directory owners.
use super::*;
use crate::resident_workbench::generation_admission::{GenerationAdmissionError, GenerationFence};
use crate::ResidentActorWorkbenchError;

use crate::resident_workbench::generation_admission::{
    GenerationRetirementFence, GenerationRetirementFenceError,
};
use crate::resident_workbench::{
    ActorGeneration, ActorGenerationCandidateCustody, ActorGenerationCandidateObserver,
    ActorGenerationCommit, ActorGenerationPublicationDriver, ActorGenerationPublicationRefusal,
    ActorGenerationPublicationRefusalObservation, ActorGenerationPublicationSettlement,
    ActorGenerationReservationDriver, ActorGenerationRetirementCutoff,
    ActorGenerationRetirementPreparation, ActorGenerationRetirementPreparationOutcome,
    ActorPublicationAttemptObserver, PreparedActorGeneration, ReservedActorGeneration,
    UnstartedRetirementSourceSettlement,
};

/// The canonical reservation owns one affine core and its transient driver.
/// Native owner loss moves the core unchanged before disposing the driver.
pub(super) struct GenerationRetirementRecord {
    core: GenerationRetirementCore,
    driver: GenerationRetirementDriver,
}

pub(super) enum GenerationRetirementCore {
    Idle,
    Active(GenerationRetirementAttempt),
    SourcesSettled {
        actor: ActorRef,
        candidate: ActorGenerationCandidateObserver,
        completion: GenerationRetirementCompletion,
        cutoff: GenerationRetirementCutoffPhase,
    },
    Completed {
        actor: ActorRef,
        candidate: ActorGenerationCandidateObserver,
        completion: GenerationRetirementCompletion,
    },
}

struct GenerationRetirementDriver {
    action: GenerationRetirementAction,
    inventory: GenerationRetirementInventory,
}

enum GenerationRetirementAction {
    None,
    Reservation(ActorGenerationReservationDriver),
    Publication(ActorGenerationPublicationDriver),
}

impl GenerationRetirementDriver {
    fn empty() -> Self {
        Self {
            action: GenerationRetirementAction::None,
            inventory: GenerationRetirementInventory::Unavailable,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum GenerationRetirementCompletion {
    UnreservedRefused,
    DefaultRefused {
        receipt: Arc<tidepool_bridge_effects::RlRetirementReceipt>,
        diagnostic: crate::resident_actor::RecordedRetirementDiagnosticObservation,
    },
    AReopened {
        outcome: ActorGenerationPublicationRefusalObservation,
        nonstart: Option<preparation::PublicationNonStartKind>,
        diagnostic: Option<crate::resident_actor::RecordedRetirementDiagnosticObservation>,
        diagnostic_refusals: Vec<crate::resident_actor::WorkbenchDiagnosticRefusal>,
    },
}

pub(super) enum GenerationRetirementDiagnostic {
    #[cfg(test)]
    CancelledComponent,
    Operation {
        owner: crate::resident_actor::WorkbenchOperationDiagnosticOwner,
        handoff: RetirementDiagnosticHandoff,
    },
}

pub(super) enum RetirementDiagnosticHandoff {
    Unprojected,
    Projected {
        evidence: Arc<crate::resident_actor::RetirementDiagnosticEvidence>,
        refusals: Vec<crate::resident_actor::WorkbenchDiagnosticRefusal>,
    },
    Recorded {
        observation: crate::resident_actor::RecordedRetirementDiagnosticObservation,
        refusals: Vec<crate::resident_actor::WorkbenchDiagnosticRefusal>,
    },
    ReceiptRefused {
        evidence: Arc<crate::resident_actor::RetirementDiagnosticEvidence>,
        receipt: crate::resident_actor::RecordedRetirementDiagnostic,
        refusals: Vec<crate::resident_actor::WorkbenchDiagnosticRefusal>,
    },
}

impl GenerationRetirementDiagnostic {
    pub(super) fn operation(
        owner: crate::resident_actor::WorkbenchOperationDiagnosticOwner,
    ) -> Self {
        Self::Operation {
            owner,
            handoff: RetirementDiagnosticHandoff::Unprojected,
        }
    }

    fn is_cancelled_component(&self) -> bool {
        match self {
            #[cfg(test)]
            Self::CancelledComponent => true,
            Self::Operation { .. } => false,
        }
    }

    fn record_refusal(
        &mut self,
        refusal: &ActorGenerationPublicationRefusal,
    ) -> Option<(
        crate::resident_actor::RecordedRetirementDiagnosticObservation,
        Vec<crate::resident_actor::WorkbenchDiagnosticRefusal>,
    )> {
        let (owner, handoff) = match self {
            Self::Operation { owner, handoff } => (owner, handoff),
            #[cfg(test)]
            Self::CancelledComponent => return None,
        };
        if matches!(handoff, RetirementDiagnosticHandoff::Unprojected) {
            // Retain the SAME immutable projection before the first write: a
            // lost receipt must never force reconstruction or text matching.
            *handoff = RetirementDiagnosticHandoff::Projected {
                evidence: Arc::new(
                    crate::resident_actor::RetirementDiagnosticEvidence::from_refusal(refusal),
                ),
                refusals: Vec::new(),
            };
        }
        Self::write_projected(owner, handoff)
    }

    fn operation_id(&self) -> Option<&tidepool_runtime::session::WorkbenchOperationId> {
        match self {
            Self::Operation { owner, .. } => Some(owner.operation()),
            #[cfg(test)]
            Self::CancelledComponent => None,
        }
    }

    fn record_default(
        &mut self,
        evidence: &Arc<crate::resident_actor::RetirementDiagnosticEvidence>,
    ) -> Option<crate::resident_actor::RecordedRetirementDiagnosticObservation> {
        let Self::Operation { owner, handoff } = self else {
            return None;
        };
        if matches!(handoff, RetirementDiagnosticHandoff::Unprojected) {
            *handoff = RetirementDiagnosticHandoff::Projected {
                evidence: evidence.clone(),
                refusals: Vec::new(),
            };
        }
        Self::write_projected(owner, handoff).map(|(observation, _)| observation)
    }

    fn write_projected(
        owner: &crate::resident_actor::WorkbenchOperationDiagnosticOwner,
        handoff: &mut RetirementDiagnosticHandoff,
    ) -> Option<(
        crate::resident_actor::RecordedRetirementDiagnosticObservation,
        Vec<crate::resident_actor::WorkbenchDiagnosticRefusal>,
    )> {
        if let RetirementDiagnosticHandoff::Projected { evidence, refusals } = handoff {
            match owner.record(evidence.clone()) {
                Ok(receipt) => match owner.acknowledge(receipt) {
                    Ok(observation) => {
                        let prior = std::mem::take(refusals);
                        *handoff = RetirementDiagnosticHandoff::Recorded {
                            observation,
                            refusals: prior,
                        };
                    }
                    Err((refusal, receipt)) => {
                        if !refusals.contains(&refusal) {
                            refusals.push(refusal);
                        }
                        let refusals = std::mem::take(refusals);
                        *handoff = RetirementDiagnosticHandoff::ReceiptRefused {
                            evidence: evidence.clone(),
                            receipt,
                            refusals,
                        };
                    }
                },
                Err(refusal) => {
                    if !refusals.contains(&refusal) {
                        refusals.push(refusal);
                    }
                }
            }
        }
        match handoff {
            RetirementDiagnosticHandoff::Recorded {
                observation,
                refusals,
            } => Some((observation.clone(), refusals.clone())),
            _ => None,
        }
    }
}

impl GenerationRetirementRecord {
    pub(super) fn idle() -> Self {
        Self {
            core: GenerationRetirementCore::Idle,
            driver: GenerationRetirementDriver::empty(),
        }
    }

    pub(super) fn is_active(&self) -> bool {
        matches!(
            self.core,
            GenerationRetirementCore::Active(_) | GenerationRetirementCore::SourcesSettled { .. }
        )
    }

    fn active(&self) -> Option<&GenerationRetirementAttempt> {
        self.core.active()
    }

    fn active_mut(&mut self) -> Option<&mut GenerationRetirementAttempt> {
        self.core.active_mut()
    }
}

impl GenerationRetirementCore {
    fn active(&self) -> Option<&GenerationRetirementAttempt> {
        match self {
            Self::Active(attempt) => Some(attempt),
            _ => None,
        }
    }

    fn active_mut(&mut self) -> Option<&mut GenerationRetirementAttempt> {
        match self {
            Self::Active(attempt) => Some(attempt),
            _ => None,
        }
    }
}

/// This same product remains in the reservation record or the terminal loss
/// parcel. Publisher closures and inventory access never enter this core.
pub(super) struct GenerationRetirementAttempt {
    actor: ActorRef,
    candidate: ActorGenerationCandidateCustody,
    expected: Arc<ActorGeneration>,
    diagnostic: GenerationRetirementDiagnostic,
    phase: GenerationRetirementPhase,
}

enum GenerationRetirementPhase {
    Preparation(PreparedActorGeneration),
    Reserving,
    ReservationFailed(Arc<ResidentActorWorkbenchError>),
    ReservationMismatch(PreparedActorGeneration),
    Unstarted {
        publication: GenerationUnstartedPublication,
        observer: ActorPublicationAttemptObserver,
        cutoff: GenerationRetirementCutoffPhase,
        progress: GenerationUnstartedProgress,
    },
    Entered {
        observer: ActorPublicationAttemptObserver,
        cutoff: GenerationRetirementCutoffPhase,
        custody: GenerationPublicationCustody,
    },
}

/// Consuming settlement retains the candidate, cutoff and capture in the core.
/// An interrupted consumption never recreates the actual reservation.
enum GenerationUnstartedPublication {
    Available(ReservedActorGeneration),
    Consuming,
}

impl GenerationUnstartedPublication {
    fn reserved_mut(&mut self) -> Option<&mut ReservedActorGeneration> {
        match self {
            Self::Available(reserved) => Some(reserved),
            Self::Consuming => None,
        }
    }
}

enum GenerationUnstartedProgress {
    AwaitingDecision,
    ProjectionRefused(GenerationRetirementProjectionRefusal),
    PublicationNotQualified(Arc<tidepool_bridge_effects::RlRetirementReceipt>),
    Refusing {
        receipt: tidepool_bridge_effects::RlRetirementReceipt,
    },
    SourceSettlementInProgress,
    SourcesRestored {
        receipt: Arc<tidepool_bridge_effects::RlRetirementReceipt>,
        evidence: Arc<crate::resident_actor::RetirementDiagnosticEvidence>,
        source_settlement: UnstartedRetirementSourceSettlement,
    },
    SettlingUnstarted {
        receipt: Arc<tidepool_bridge_effects::RlRetirementReceipt>,
        evidence: Arc<crate::resident_actor::RetirementDiagnosticEvidence>,
    },
    SettlementFailed {
        receipt: Arc<tidepool_bridge_effects::RlRetirementReceipt>,
        error: Arc<ResidentActorWorkbenchError>,
    },
}

enum GenerationPublicationCustody {
    Waiting {
        receiver: oneshot::Receiver<preparation::PublicationDelivery>,
        terminal: Arc<std::sync::OnceLock<preparation::PublicationTerminalObservation>>,
    },
    NotStarted {
        reserved: ReservedActorGeneration,
        reason: preparation::PublicationNonStart,
        settlement_error: Option<Arc<ResidentActorWorkbenchError>>,
    },
    Unpublished {
        refusal: ActorGenerationPublicationRefusal,
        settlement_error: Option<Arc<ResidentActorWorkbenchError>>,
    },
    SourceRestoredPending(PublicationSourceRestoredPending),
    Settled(Arc<Result<ActorGenerationCommit, ResidentActorWorkbenchError>>),
    ProducerLostAfterExecute,
}

enum PublicationSourceRestoredPending {
    Unpublished {
        refusal: ActorGenerationPublicationRefusal,
        observation: ActorGenerationPublicationRefusalObservation,
    },
    NotStarted {
        reserved: ReservedActorGeneration,
        reason: preparation::PublicationNonStart,
        source_settlement: UnstartedRetirementSourceSettlement,
    },
}

#[derive(Clone, Copy, Debug)]
pub(super) enum GenerationPublicationDispatchFailure {
    Unavailable,
    AttemptChanged,
    NotCaptured,
    NotCancelled,
}

enum GenerationRetirementCutoffPhase {
    NotStarted,
    AwaitCapture {
        fence: GenerationRetirementFence,
        delivery: GenerationCaptureDelivery,
    },
    Captured {
        fence: GenerationRetirementFence,
        capture: GenerationRetirementCapture,
    },
    Unconfirmed {
        fence: GenerationRetirementFence,
        error: Arc<ResidentActorWorkbenchError>,
    },
    Refused(Arc<ResidentActorWorkbenchError>),
}

enum GenerationCaptureDelivery {
    Pending,
    Enqueued,
    Unconfirmed,
}

/// These are the actual existing cleanup handles, retained separately from the
/// census. Observation identities alone cannot preserve physical cleanup custody.
struct GenerationRetirementCapture {
    population: GenerationPopulation,
    actors: Vec<LocalActorRef>,
    resources: Vec<ResourceChild>,
    reservations: Vec<RetainedActorExit>,
    preparations: Vec<RetainedActorExit>,
    work: GenerationRetirementWork,
    pending_claims: Result<usize, GenerationRetirementFenceError>,
    physical_closure: GenerationCapturePhysicalClosure,
}

/// A wire projection may refuse a coordinate; the owning capture remains intact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GenerationRetirementProjectionRefusal {
    ActorIdentity,
    PreparationIdentity,
    CompilerRegistration,
    WatchIdentity,
    OperationCoordinate,
    PendingClaims,
    FrozenReloadReportMissing,
    OperationMissing,
}

impl GenerationRetirementCapture {
    fn blockers(
        &self,
    ) -> Result<Vec<tidepool_bridge_effects::RlBlocker>, GenerationRetirementProjectionRefusal>
    {
        use crate::resident_actor::invocation_work::GenerationInvocationState as Invocation;
        use tidepool_bridge_effects::{
            RlBlocker as Blocker, RlBlockerReason as Reason, RlResource as Resource,
        };
        let mut blockers = Vec::new();
        for observed in &self.population.resources {
            blockers.push(Blocker::ReloadResourceBlocker(
                retirement_resource(&observed.identity)?,
                match &observed.state {
                    GenerationResourceState::Starting => Reason::ReloadStartingResource,
                    GenerationResourceState::Running => Reason::ReloadRunningResource,
                    GenerationResourceState::Transferring => Reason::ReloadTransferringResource,
                    GenerationResourceState::CleanupUnsupported => Reason::ReloadCleanupUnsupported,
                    GenerationResourceState::CleanupUnconfirmed(detail) => {
                        Reason::ReloadCleanupUnconfirmed(detail.clone())
                    }
                },
            ));
        }
        for observed in &self.population.unclassified {
            blockers.push(Blocker::ReloadResourceBlocker(
                retirement_resource(&observed.identity)?,
                Reason::ReloadUnclassifiedWork,
            ));
        }
        for edge in &self.population.collateral {
            use tidepool_bridge_effects::RlLifetimeEdge as Edge;
            blockers.push(Blocker::ReloadCollateralBlocker(
                retirement_resource(&edge.stopping)?,
                match &edge.affected {
                    GenerationCollateralIdentity::Classified(resource) => {
                        retirement_resource(resource)?
                    }
                    GenerationCollateralIdentity::UnclassifiedPhysical(id) => {
                        Resource::ReloadUnclassifiedPhysicalResource(retirement_native_identity(
                            *id,
                        ))
                    }
                },
                match edge.edge {
                    GenerationLifetimeEdge::PhysicalSupervision => Edge::ReloadPhysicalSupervision,
                    GenerationLifetimeEdge::PendingSupervision => Edge::ReloadPendingSupervision,
                    GenerationLifetimeEdge::PendingResourcePreparation => {
                        Edge::ReloadPendingResourcePreparation
                    }
                    GenerationLifetimeEdge::ResourceOwnership => Edge::ReloadResourceOwnership,
                    GenerationLifetimeEdge::OwnershipTransfer => Edge::ReloadOwnershipTransfer,
                    GenerationLifetimeEdge::OwnershipTransferUnconfirmed => {
                        Edge::ReloadOwnershipTransferUnconfirmed
                    }
                },
            ));
        }
        for observed in &self.population.compilers {
            let resource = match observed.owner {
                GenerationCompilerOwner::Actor(actor) => Resource::ReloadActorCompilerResource(
                    retirement_actor_identity(actor)?,
                    observed.compiler.registration.to_string(),
                ),
                GenerationCompilerOwner::Preparation(id) => {
                    Resource::ReloadPreparationCompilerResource(
                        id.to_string(),
                        observed.compiler.registration.to_string(),
                    )
                }
            };
            blockers.push(Blocker::ReloadResourceBlocker(
                resource,
                retirement_compiler_reason(&observed.compiler.close, &observed.compiler.source),
            ));
        }
        match &self.work {
            GenerationRetirementWork::Unavailable => {
                blockers.push(Blocker::ReloadUnclassifiedSourceWork(1))
            }
            GenerationRetirementWork::Observed(work) => {
                for observed in &work.invocations {
                    let actor = retirement_actor_identity(observed.actor)?;
                    let reservation = retirement_reservation(&observed.reservation)?;
                    let (resource, reason) = match &observed.state {
                        Invocation::Active => (Resource::ReloadInvocationResource(actor, reservation), Reason::ReloadActiveDependentWork),
                        Invocation::SourceUnclassified => (Resource::ReloadInvocationResource(actor, reservation), Reason::ReloadUnclassifiedWork),
                        Invocation::CleanupPending => (Resource::ReloadInvocationResource(actor, reservation), Reason::ReloadCleanupUnconfirmed("invocation cleanup is pending".into())),
                        Invocation::CleanupUnconfirmed(detail) => (Resource::ReloadInvocationResource(actor, reservation), Reason::ReloadCleanupUnconfirmed(detail.clone())),
                        Invocation::Compiler { registration, close, source } => (
                            Resource::ReloadCompilerResource(actor, reservation, i64::try_from(*registration).map_err(|_| GenerationRetirementProjectionRefusal::CompilerRegistration)?),
                            retirement_compiler_reason(close, source),
                        ),
                        Invocation::Form(mount) => (Resource::ReloadFormResource(actor, reservation, mount.clone()), Reason::ReloadActiveDependentWork),
                        Invocation::DisplayPendingProducer(operation) => (Resource::ReloadDisplayResource(retirement_operation(operation)?), Reason::ReloadPendingProducer),
                        Invocation::DisplayPublication(operation, publication) => (
                            Resource::ReloadDisplayResource(retirement_operation(operation)?),
                            match publication {
                                tidepool_runtime::session::WorkbenchDisplayPublication::Pending { .. } => Reason::ReloadDisplayAcknowledgementPending,
                                tidepool_runtime::session::WorkbenchDisplayPublication::Unconfirmed { detail, .. }
                                | tidepool_runtime::session::WorkbenchDisplayPublication::Refused { detail, .. } => Reason::ReloadDisplayAcknowledgementUnconfirmed(detail.clone()),
                                tidepool_runtime::session::WorkbenchDisplayPublication::Published { .. } => continue,
                            },
                        ),
                    };
                    blockers.push(Blocker::ReloadResourceBlocker(resource, reason));
                }
                for route in &work.routes {
                    use crate::request::routes::GenerationRouteDependency;
                    blockers.push(Blocker::ReloadResourceBlocker(
                        Resource::ReloadWatchResource(
                            retirement_actor_identity(route.owner)?,
                            i64::try_from(route.watch.0).map_err(|_| {
                                GenerationRetirementProjectionRefusal::WatchIdentity
                            })?,
                        ),
                        match route.dependency {
                            GenerationRouteDependency::Dependent => {
                                Reason::ReloadActiveDependentWork
                            }
                            GenerationRouteDependency::Unclassified => {
                                Reason::ReloadUnclassifiedWork
                            }
                        },
                    ));
                }
            }
        }
        match self.pending_claims {
            Ok(0) => {}
            Ok(count) => blockers.push(Blocker::ReloadUnclassifiedSourceWork(
                i64::try_from(count)
                    .map_err(|_| GenerationRetirementProjectionRefusal::PendingClaims)?,
            )),
            Err(_) => blockers.push(Blocker::ReloadUnclassifiedSourceWork(1)),
        }
        Ok(blockers)
    }
}

fn retirement_compiler_reason(
    close: &crate::CompilerWorkClose,
    source: &crate::termination::GenerationCompilerSource,
) -> tidepool_bridge_effects::RlBlockerReason {
    use tidepool_bridge_effects::RlBlockerReason as Reason;
    if matches!(
        source,
        crate::termination::GenerationCompilerSource::Unclassified
    ) {
        return Reason::ReloadUnclassifiedWork;
    }
    match close {
        crate::CompilerWorkClose::Pending => Reason::ReloadCompilerPending,
        crate::CompilerWorkClose::Abandoned => Reason::ReloadCompilerUnconfirmed(
            "compiler producer abandoned without physical close".into(),
        ),
        crate::CompilerWorkClose::Settled(close) => {
            Reason::ReloadCompilerUnconfirmed(format!("{close:?}"))
        }
    }
}

fn retirement_actor_identity(
    actor: ActorRef,
) -> Result<(i64, i64), GenerationRetirementProjectionRefusal> {
    Ok((
        i64::try_from(actor.id.0)
            .map_err(|_| GenerationRetirementProjectionRefusal::ActorIdentity)?,
        i64::try_from(actor.incarnation.0)
            .map_err(|_| GenerationRetirementProjectionRefusal::ActorIdentity)?,
    ))
}

fn retirement_native_identity(id: ractor::ActorId) -> tidepool_bridge_effects::RlNativeIdentity {
    use tidepool_bridge_effects::RlNativeIdentity as Native;
    match id {
        ractor::ActorId::Local(pid) => Native::ReloadNativeLocal(pid.to_string()),
        ractor::ActorId::Remote { node_id, pid } => {
            Native::ReloadNativeRemote(node_id.to_string(), pid.to_string())
        }
    }
}

fn retirement_resource(
    identity: &GenerationResourceIdentity,
) -> Result<tidepool_bridge_effects::RlResource, GenerationRetirementProjectionRefusal> {
    use tidepool_bridge_effects::RlResource as Resource;
    Ok(match identity {
        GenerationResourceIdentity::Actor(actor) => {
            Resource::ReloadActorResource(retirement_actor_identity(*actor)?)
        }
        GenerationResourceIdentity::Native { issued_to, id } => Resource::ReloadNativeResource(
            retirement_actor_identity(*issued_to)?,
            retirement_native_identity(*id),
        ),
        GenerationResourceIdentity::Preparation(id) => Resource::ReloadPreparationResource(
            i64::try_from(*id)
                .map_err(|_| GenerationRetirementProjectionRefusal::PreparationIdentity)?,
        ),
    })
}

fn retirement_reservation(
    owner: &crate::request::RequestReservationOwner,
) -> Result<tidepool_bridge_effects::RlReservation, GenerationRetirementProjectionRefusal> {
    use tidepool_bridge_effects::RlReservation as Reservation;
    Ok(match owner {
        crate::request::RequestReservationOwner::Workbench { execution, attempt } => {
            Reservation::ReloadWorkbenchReservation(execution.to_string(), attempt.to_string())
        }
        crate::request::RequestReservationOwner::Route(watch) => {
            Reservation::ReloadRouteReservation(
                i64::try_from(watch.0)
                    .map_err(|_| GenerationRetirementProjectionRefusal::WatchIdentity)?,
            )
        }
        crate::request::RequestReservationOwner::Scope(scope) => {
            Reservation::ReloadScopeReservation(*scope)
        }
    })
}

fn retirement_operation(
    operation: &tidepool_runtime::session::WorkbenchOperationId,
) -> Result<tidepool_bridge_effects::RlOperation, GenerationRetirementProjectionRefusal> {
    Ok(tidepool_bridge_effects::RlOperation {
        execution: operation.execution.to_string(),
        input_unit_index: i64::try_from(operation.input_unit_index)
            .map_err(|_| GenerationRetirementProjectionRefusal::OperationCoordinate)?,
        effect_ordinal: i64::try_from(operation.effect_ordinal)
            .map_err(|_| GenerationRetirementProjectionRefusal::OperationCoordinate)?,
    })
}

enum GenerationCapturePhysicalClosure {
    Guarded {
        actors: HashSet<ActorRef>,
        cells: HashSet<ractor::ActorId>,
    },
}

enum GenerationRetirementWork {
    Observed(crate::resident_actor::GenerationWorkObservations),
    Unavailable,
}

pub(super) enum GenerationRetirementInventory {
    Resident(crate::resident_actor::ResidentGenerationInventory),
    Unavailable,
}

pub(super) struct GenerationRetirementDiscard {
    _driver: GenerationRetirementDriver,
}

pub(super) enum GenerationRetirementBeginFailure {
    Directory(DirectoryError),
    Unregistered {
        terminal: Option<ActorTerminal>,
        cleanup: Option<crate::ResidentCleanupOutcome>,
    },
    ExpectedGenerationMissing,
    CandidateChanged,
    Refused(String),
    Publication(Arc<ResidentActorWorkbenchError>),
}

impl GenerationPublicationCustody {
    fn observe(&mut self) {
        let Self::Waiting { receiver, .. } = self else {
            return;
        };
        let delivery = match receiver.try_recv() {
            Ok(delivery) => delivery,
            Err(oneshot::error::TryRecvError::Empty) => return,
            Err(oneshot::error::TryRecvError::Closed) => {
                *self = Self::ProducerLostAfterExecute;
                return;
            }
        };
        // Install the actual guard before any source transition. A subsequent
        // panic/owner loss therefore evacuates this same affine continuation.
        *self = match delivery {
            preparation::PublicationDelivery::NotStarted { reserved, reason } => Self::NotStarted {
                reserved,
                reason,
                settlement_error: None,
            },
            preparation::PublicationDelivery::Settled(
                ActorGenerationPublicationSettlement::Unpublished(refusal),
            ) => Self::Unpublished {
                refusal,
                settlement_error: None,
            },
            preparation::PublicationDelivery::Settled(
                ActorGenerationPublicationSettlement::Settled(outcome),
            ) => Self::Settled(outcome),
        };
    }

    fn reconcile(
        &mut self,
        cutoff: &mut GenerationRetirementCutoffPhase,
        diagnostic: &mut GenerationRetirementDiagnostic,
    ) -> Option<GenerationRetirementCompletion> {
        self.observe();
        let fence = match cutoff {
            GenerationRetirementCutoffPhase::Captured { fence, .. } => fence,
            _ => return None,
        };
        let completion = match self {
            Self::NotStarted {
                reserved,
                reason,
                settlement_error,
            } if settlement_error.is_none() => {
                match reserved.refuse_retirement(fence, "publication worker did not execute") {
                    Ok(source_settlement) => {
                        let old = std::mem::replace(self, Self::ProducerLostAfterExecute);
                        let Self::NotStarted {
                            reserved, reason, ..
                        } = old
                        else {
                            return None;
                        };
                        *self = Self::SourceRestoredPending(
                            PublicationSourceRestoredPending::NotStarted {
                                reserved,
                                reason,
                                source_settlement,
                            },
                        );
                        None
                    }
                    Err(error) => {
                        *settlement_error = Some(Arc::new(error));
                        None
                    }
                }
            }
            Self::Unpublished {
                refusal,
                settlement_error,
            } if settlement_error.is_none() => {
                let observation = refusal.observation();
                match refusal.refuse_retirement(fence) {
                    Ok(()) => {
                        // Preserve the body-issued assessment through the real
                        // diagnostic handoff and consuming settled-owner check.
                        let old = std::mem::replace(self, Self::ProducerLostAfterExecute);
                        let Self::Unpublished { refusal, .. } = old else {
                            return None;
                        };
                        *self = Self::SourceRestoredPending(
                            PublicationSourceRestoredPending::Unpublished {
                                refusal,
                                observation,
                            },
                        );
                        None
                    }
                    Err(error) => {
                        *settlement_error = Some(Arc::new(error));
                        None
                    }
                }
            }
            _ => None,
        };
        completion.or_else(|| self.record_source_restored(diagnostic))
    }

    fn record_source_restored(
        &mut self,
        diagnostic: &mut GenerationRetirementDiagnostic,
    ) -> Option<GenerationRetirementCompletion> {
        if diagnostic.is_cancelled_component()
            && matches!(self, Self::SourceRestoredPending(
                PublicationSourceRestoredPending::NotStarted { reason, .. }
            ) if !matches!(reason, preparation::PublicationNonStart::SpawnFailed(_)))
        {
            let old = std::mem::replace(self, Self::ProducerLostAfterExecute);
            let Self::SourceRestoredPending(PublicationSourceRestoredPending::NotStarted {
                reserved,
                reason,
                source_settlement,
            }) = old
            else {
                *self = old;
                return None;
            };
            return match reserved.into_settled_unstarted(source_settlement) {
                Ok(()) => Some(GenerationRetirementCompletion::AReopened {
                    outcome: ActorGenerationPublicationRefusalObservation::Refused,
                    nonstart: Some(reason.kind()),
                    diagnostic: None,
                    diagnostic_refusals: Vec::new(),
                }),
                Err((reserved, source_settlement)) => {
                    *self =
                        Self::SourceRestoredPending(PublicationSourceRestoredPending::NotStarted {
                            reserved,
                            reason,
                            source_settlement,
                        });
                    None
                }
            };
        }
        let Self::SourceRestoredPending(PublicationSourceRestoredPending::Unpublished {
            refusal,
            ..
        }) = self
        else {
            return None;
        };
        let (recorded, refusals) = if diagnostic.is_cancelled_component() {
            // The private cancellation component has a fixed known cause. It
            // still requires the actual body's settled-obligation assessment.
            (None, Vec::new())
        } else {
            let (recorded, refusals) = diagnostic.record_refusal(refusal)?;
            (Some(recorded), refusals)
        };
        // The producer's consuming check has no await or source transition. It
        // returns the SAME refusal unless both its own obligations and guard
        // are settled; diagnostic acknowledgement alone grants no release.
        let old = std::mem::replace(self, Self::ProducerLostAfterExecute);
        let Self::SourceRestoredPending(PublicationSourceRestoredPending::Unpublished {
            refusal,
            observation,
        }) = old
        else {
            *self = old;
            return None;
        };
        match refusal.into_settled_diagnostic() {
            Ok(outcome) => Some(GenerationRetirementCompletion::AReopened {
                outcome,
                nonstart: None,
                diagnostic: recorded,
                diagnostic_refusals: refusals,
            }),
            Err(refusal) => {
                *self =
                    Self::SourceRestoredPending(PublicationSourceRestoredPending::Unpublished {
                        refusal,
                        observation,
                    });
                None
            }
        }
    }
}

fn consume_default_unstarted(
    reserved: ReservedActorGeneration,
    progress: &mut GenerationUnstartedProgress,
    diagnostic: crate::resident_actor::RecordedRetirementDiagnosticObservation,
) -> Result<GenerationRetirementCompletion, ReservedActorGeneration> {
    let GenerationUnstartedProgress::SourcesRestored {
        receipt, evidence, ..
    } = progress
    else {
        return Err(reserved);
    };
    let placeholder = GenerationUnstartedProgress::SettlingUnstarted {
        receipt: receipt.clone(),
        evidence: evidence.clone(),
    };
    let old = std::mem::replace(progress, placeholder);
    let GenerationUnstartedProgress::SourcesRestored {
        receipt,
        evidence,
        source_settlement,
    } = old
    else {
        *progress = old;
        return Err(reserved);
    };
    match reserved.into_settled_unstarted(source_settlement) {
        Ok(()) => Ok(GenerationRetirementCompletion::DefaultRefused {
            receipt,
            diagnostic,
        }),
        Err((reserved, source_settlement)) => {
            *progress = GenerationUnstartedProgress::SourcesRestored {
                receipt,
                evidence,
                source_settlement,
            };
            Err(reserved)
        }
    }
}

fn reconcile_unstarted(
    publication: &mut GenerationUnstartedPublication,
    cutoff: &mut GenerationRetirementCutoffPhase,
    progress: &mut GenerationUnstartedProgress,
    diagnostic: &mut GenerationRetirementDiagnostic,
) -> Option<GenerationRetirementCompletion> {
    if matches!(progress, GenerationUnstartedProgress::Refusing { .. }) {
        let GenerationRetirementCutoffPhase::Captured { fence, .. } = cutoff else {
            return None;
        };
        let reserved = publication.reserved_mut()?;
        let old = std::mem::replace(
            progress,
            GenerationUnstartedProgress::SourceSettlementInProgress,
        );
        let GenerationUnstartedProgress::Refusing { mut receipt } = old else {
            unreachable!()
        };
        match reserved.refuse_retirement(
            fence,
            "generation retirement refused because admitted dependent work remains",
        ) {
            Ok(source_settlement) => {
                receipt.state = tidepool_bridge_effects::RlRetirementState::ReloadAReopened;
                let receipt = Arc::new(receipt);
                let evidence = Arc::new(
                    crate::resident_actor::RetirementDiagnosticEvidence::default_refusal(&receipt),
                );
                *progress = GenerationUnstartedProgress::SourcesRestored {
                    receipt,
                    evidence,
                    source_settlement,
                };
            }
            Err(error) => {
                *progress = GenerationUnstartedProgress::SettlementFailed {
                    receipt: Arc::new(receipt),
                    error: Arc::new(error),
                };
            }
        }
    }
    let GenerationUnstartedProgress::SourcesRestored { evidence, .. } = progress else {
        return None;
    };
    let recorded = diagnostic.record_default(evidence)?;
    let old = std::mem::replace(publication, GenerationUnstartedPublication::Consuming);
    let GenerationUnstartedPublication::Available(reserved) = old else {
        return None;
    };
    match consume_default_unstarted(reserved, progress, recorded) {
        Ok(completion) => Some(completion),
        Err(reserved) => {
            *publication = GenerationUnstartedPublication::Available(reserved);
            None
        }
    }
}

impl GenerationRetirementCore {
    /// Live and terminal owners drive this SAME affine state. The terminal
    /// checkout never holds its slot lock while invoking source or ledger owners.
    fn reconcile(&mut self) {
        let Self::Active(attempt) = self else {
            return;
        };
        let completion = match &mut attempt.phase {
            GenerationRetirementPhase::Entered {
                cutoff, custody, ..
            } => custody.reconcile(cutoff, &mut attempt.diagnostic),
            GenerationRetirementPhase::Unstarted {
                publication,
                cutoff,
                progress,
                ..
            } => reconcile_unstarted(publication, cutoff, progress, &mut attempt.diagnostic),
            _ => None,
        };
        let Some(completion) = completion else {
            return;
        };
        let candidate_identity = attempt.candidate.identity_observer();
        let old = std::mem::replace(self, Self::Idle);
        let Self::Active(GenerationRetirementAttempt {
            actor,
            candidate,
            expected,
            diagnostic,
            phase,
        }) = old
        else {
            unreachable!()
        };
        match phase {
            GenerationRetirementPhase::Entered {
                observer,
                cutoff,
                custody,
            } => {
                *self = Self::SourcesSettled {
                    actor,
                    candidate: candidate_identity,
                    completion,
                    cutoff,
                };
                drop((candidate, expected, diagnostic, observer, custody));
            }
            GenerationRetirementPhase::Unstarted {
                publication,
                observer,
                cutoff,
                progress,
            } => {
                *self = Self::SourcesSettled {
                    actor,
                    candidate: candidate_identity,
                    completion,
                    cutoff,
                };
                drop((
                    candidate,
                    expected,
                    diagnostic,
                    publication,
                    observer,
                    progress,
                ));
            }
            _ => unreachable!("only an owned publication can settle retirement sources"),
        }
    }
}

impl RunControlNativeRecovery<'_> {
    fn reconcile_publications(&mut self) {
        let Some(custody) = self.custody.as_mut() else {
            return;
        };
        for core in &mut custody.attempts {
            core.reconcile();
        }
    }
}

#[cfg(test)]
impl GenerationRetirementCore {
    fn active_for_test(&self) -> &GenerationRetirementAttempt {
        self.active().expect("expected retained active attempt")
    }
}

impl RunState {
    pub(super) fn retirement_blocks_link(
        &self,
        actors: &[ActorRef],
        cells: &[ractor::ActorId],
    ) -> Option<ActorRef> {
        self.reservations.iter().find_map(|(actor, record)| {
            let attempt = record.retirement.active()?;
            if matches!(
                &attempt.phase,
                GenerationRetirementPhase::Entered {
                    custody: GenerationPublicationCustody::SourceRestoredPending(_),
                    ..
                } | GenerationRetirementPhase::Unstarted {
                    progress: GenerationUnstartedProgress::SourcesRestored { .. },
                    ..
                }
            ) {
                // A is already reopened. This phase owns only the independent
                // recovery obligation and cannot freeze/stop its prior closure.
                return None;
            }
            let (GenerationRetirementPhase::Unstarted {
                cutoff: GenerationRetirementCutoffPhase::Captured { capture, .. },
                ..
            }
            | GenerationRetirementPhase::Entered {
                cutoff: GenerationRetirementCutoffPhase::Captured { capture, .. },
                ..
            }) = &attempt.phase
            else {
                return None;
            };
            let GenerationCapturePhysicalClosure::Guarded {
                actors: protected_actors,
                cells: protected_cells,
            } = &capture.physical_closure;
            (actors.iter().any(|actor| protected_actors.contains(actor))
                || cells.iter().any(|cell| protected_cells.contains(cell)))
            .then_some(*actor)
        })
    }
    pub(super) fn begin_generation_retirement(
        &mut self,
        myself: &RactorRef<RunControlMessage>,
        preparation: ActorGenerationRetirementPreparation,
        inventory: GenerationRetirementInventory,
        diagnostic: GenerationRetirementDiagnostic,
    ) -> Result<ActorPublicationAttemptObserver, GenerationRetirementBeginFailure> {
        let actor = preparation.actor();
        let failure = |error| GenerationRetirementBeginFailure::Directory(error);
        if self.phase != RunPhase::Running {
            return Err(failure(DirectoryError::Closed));
        }
        let Some(record) = self.reservations.get_mut(&actor) else {
            return Err(failure(DirectoryError::Unauthorized { actor }));
        };
        if record.retirement.is_active() {
            return Err(failure(DirectoryError::LifetimeTransitionPending { actor }));
        }
        if !self.entries.contains_key(&actor)
            || !matches!(record.phase, ReservationPhase::Registered)
        {
            return Err(GenerationRetirementBeginFailure::Unregistered {
                terminal: record.terminal.get(),
                cleanup: record.terminal.cleanup(),
            });
        }
        let Some(expected) = preparation.expected_generation().cloned() else {
            return Err(GenerationRetirementBeginFailure::ExpectedGenerationMissing);
        };
        let candidate = preparation.candidate_custody();
        let (prepared, reserve) = preparation.into_parts();
        record.retirement = GenerationRetirementRecord {
            core: GenerationRetirementCore::Active(GenerationRetirementAttempt {
                actor,
                candidate,
                expected,
                diagnostic,
                phase: GenerationRetirementPhase::Preparation(prepared),
            }),
            driver: GenerationRetirementDriver {
                action: GenerationRetirementAction::Reservation(reserve),
                inventory,
            },
        };
        let GenerationRetirementDriver { action, .. } = &mut record.retirement.driver;
        let GenerationRetirementAction::Reservation(reserve) =
            std::mem::replace(action, GenerationRetirementAction::None)
        else {
            unreachable!()
        };
        let attempt = record
            .retirement
            .core
            .active_mut()
            .expect("retirement was just installed");
        let GenerationRetirementPhase::Preparation(prepared) =
            std::mem::replace(&mut attempt.phase, GenerationRetirementPhase::Reserving)
        else {
            unreachable!()
        };
        let outcome = match reserve.reserve(prepared) {
            Ok(outcome) => outcome,
            Err((prepared, reserve)) => {
                attempt.phase = GenerationRetirementPhase::ReservationMismatch(prepared);
                record.retirement.driver.action = GenerationRetirementAction::Reservation(reserve);
                return Err(GenerationRetirementBeginFailure::CandidateChanged);
            }
        };
        let (reserved, publisher) = match outcome {
            ActorGenerationRetirementPreparationOutcome::Reserved {
                reserved,
                publisher,
            } => (reserved, publisher),
            ActorGenerationRetirementPreparationOutcome::Refused(reason) => {
                let identity = attempt.candidate.identity_observer();
                record.retirement.core = GenerationRetirementCore::Completed {
                    actor,
                    candidate: identity,
                    completion: GenerationRetirementCompletion::UnreservedRefused,
                };
                return Err(GenerationRetirementBeginFailure::Refused(reason));
            }
            ActorGenerationRetirementPreparationOutcome::Failed(error) => {
                let error = Arc::new(error);
                attempt.phase = GenerationRetirementPhase::ReservationFailed(error.clone());
                return Err(GenerationRetirementBeginFailure::Publication(error));
            }
        };
        record.retirement.driver.action = GenerationRetirementAction::Publication(publisher);
        let observer = reserved.attempt_observer();
        attempt.phase = GenerationRetirementPhase::Unstarted {
            publication: GenerationUnstartedPublication::Available(reserved),
            observer: observer.clone(),
            cutoff: GenerationRetirementCutoffPhase::NotStarted,
            progress: GenerationUnstartedProgress::AwaitingDecision,
        };
        let GenerationRetirementPhase::Unstarted {
            publication,
            cutoff,
            ..
        } = &mut attempt.phase
        else {
            unreachable!()
        };
        let reserved = publication
            .reserved_mut()
            .expect("new reservation remains available");
        match reserved.fence_original() {
            ActorGenerationRetirementCutoff::Fenced(fence) => {
                *cutoff = GenerationRetirementCutoffPhase::AwaitCapture {
                    fence,
                    delivery: GenerationCaptureDelivery::Pending,
                };
                // source_enqueue holds the source gate through synchronous
                // mailbox insertion. Their pre-cutoff messages precede this
                // FIFO barrier even when they arrived behind Begin.
                let inserted = myself
                    .send_message(RunControlMessage::CaptureGenerationRetirement {
                        actor,
                        observer: observer.clone(),
                    })
                    .is_ok();
                if let GenerationRetirementCutoffPhase::AwaitCapture { delivery, .. } = cutoff {
                    *delivery = if inserted {
                        GenerationCaptureDelivery::Enqueued
                    } else {
                        GenerationCaptureDelivery::Unconfirmed
                    };
                }
                if !inserted {
                    // The same operation and fence stay retained. No capture or
                    // empty-population decision can be inferred from send loss.
                    return Err(failure(DirectoryError::Unavailable));
                }
                Ok(observer)
            }
            ActorGenerationRetirementCutoff::Refused(error) => {
                let error = Arc::new(error);
                *cutoff = GenerationRetirementCutoffPhase::Refused(error.clone());
                Err(GenerationRetirementBeginFailure::Publication(error))
            }
            ActorGenerationRetirementCutoff::Unconfirmed { fence, error } => {
                let error = Arc::new(error);
                *cutoff = GenerationRetirementCutoffPhase::Unconfirmed {
                    fence,
                    error: error.clone(),
                };
                Err(GenerationRetirementBeginFailure::Publication(error))
            }
        }
    }

    #[cfg(test)]
    pub(super) fn dispatch_cancelled_generation_publication(
        &mut self,
        myself: &RactorRef<RunControlMessage>,
        actor: ActorRef,
        observer: ActorPublicationAttemptObserver,
    ) -> Result<
        Arc<std::sync::OnceLock<preparation::PublicationTerminalObservation>>,
        GenerationPublicationDispatchFailure,
    > {
        let supervisor = self
            .preparation_supervisor
            .clone()
            .ok_or(GenerationPublicationDispatchFailure::Unavailable)?;
        let record = self
            .reservations
            .get_mut(&actor)
            .ok_or(GenerationPublicationDispatchFailure::AttemptChanged)?;
        let attempt = record
            .retirement
            .core
            .active_mut()
            .ok_or(GenerationPublicationDispatchFailure::AttemptChanged)?;
        let GenerationRetirementPhase::Unstarted {
            publication,
            observer: issued,
            cutoff,
            progress,
        } = &mut attempt.phase
        else {
            return Err(GenerationPublicationDispatchFailure::AttemptChanged);
        };
        if !issued.same_attempt(&observer) {
            return Err(GenerationPublicationDispatchFailure::AttemptChanged);
        }
        if !matches!(cutoff, GenerationRetirementCutoffPhase::Captured { .. }) {
            return Err(GenerationPublicationDispatchFailure::NotCaptured);
        }
        if !matches!(progress, GenerationUnstartedProgress::AwaitingDecision) {
            return Err(GenerationPublicationDispatchFailure::AttemptChanged);
        }
        let Some(reserved) = publication.reserved_mut() else {
            return Err(GenerationPublicationDispatchFailure::AttemptChanged);
        };
        if reserved.publication_phase()
            != tidepool_runtime::session::PublicationPhase::CancellationRequested
        {
            return Err(GenerationPublicationDispatchFailure::NotCancelled);
        }
        if !matches!(
            record.retirement.driver.action,
            GenerationRetirementAction::Publication(_)
        ) {
            return Err(GenerationPublicationDispatchFailure::AttemptChanged);
        }
        // Allocate delivery while all actual authority is still installed.
        let (writer, receiver, terminal) =
            preparation::PublicationTransfer::channel(actor, observer.clone(), myself.clone());
        let old = std::mem::replace(&mut attempt.phase, GenerationRetirementPhase::Reserving);
        let GenerationRetirementPhase::Unstarted {
            publication: GenerationUnstartedPublication::Available(reserved),
            observer,
            cutoff,
            ..
        } = old
        else {
            unreachable!("validated unstarted reservation remains available")
        };
        attempt.phase = GenerationRetirementPhase::Entered {
            observer,
            cutoff,
            custody: GenerationPublicationCustody::Waiting {
                receiver,
                terminal: terminal.clone(),
            },
        };
        let GenerationRetirementAction::Publication(publisher) = std::mem::replace(
            &mut record.retirement.driver.action,
            GenerationRetirementAction::None,
        ) else {
            unreachable!()
        };
        let transfer = preparation::PublicationTransfer::new(reserved, publisher, writer);
        if let Err(ractor::MessagingErr::SendErr(PreparationMessage::PublishGeneration(transfer))) =
            supervisor.send_message(PreparationMessage::PublishGeneration(transfer))
        {
            transfer.return_unstarted(preparation::PublicationNonStart::SupervisorSendFailed);
        }
        Ok(terminal)
    }

    pub(super) fn generation_publication_ready(
        &mut self,
        actor: ActorRef,
        observer: ActorPublicationAttemptObserver,
    ) {
        let Some(record) = self.reservations.get_mut(&actor) else {
            return;
        };
        let Some(attempt) = record.retirement.core.active() else {
            return;
        };
        let GenerationRetirementPhase::Entered {
            observer: issued, ..
        } = &attempt.phase
        else {
            return;
        };
        if !issued.same_attempt(&observer) {
            return;
        }
        record.retirement.core.reconcile();
        self.release_live_source_observations(actor);
    }

    pub(super) fn capture_generation_retirement(
        &mut self,
        actor: ActorRef,
        observer: ActorPublicationAttemptObserver,
    ) {
        let Some(attempt) = self
            .reservations
            .get(&actor)
            .and_then(|record| record.retirement.active())
        else {
            return;
        };
        let GenerationRetirementPhase::Unstarted {
            observer: current,
            cutoff:
                GenerationRetirementCutoffPhase::AwaitCapture {
                    delivery: GenerationCaptureDelivery::Enqueued,
                    ..
                },
            ..
        } = &attempt.phase
        else {
            return;
        };
        if !current.same_attempt(&observer) {
            return;
        }
        let pending_claims = match &attempt.phase {
            GenerationRetirementPhase::Unstarted {
                cutoff: GenerationRetirementCutoffPhase::AwaitCapture { fence, .. },
                ..
            } => fence.pending_claims(),
            _ => return,
        };
        let population = self.generation_population(attempt.expected.dependency().clone());
        let work = match &self
            .reservations
            .get(&actor)
            .expect("the selected record remains present")
            .retirement
            .driver
            .inventory
        {
            GenerationRetirementInventory::Resident(inventory) => {
                GenerationRetirementWork::Observed(inventory.observe(attempt.expected.dependency()))
            }
            GenerationRetirementInventory::Unavailable => GenerationRetirementWork::Unavailable,
        };
        let mut identities: HashSet<_> = population
            .resources
            .iter()
            .chain(&population.unclassified)
            .map(|resource| resource.identity.clone())
            .collect();
        for edge in &population.collateral {
            identities.insert(edge.stopping.clone());
            if let GenerationCollateralIdentity::Classified(affected) = &edge.affected {
                identities.insert(affected.clone());
            }
        }
        let mut actors = Vec::new();
        let mut resources = Vec::new();
        let mut reservations = Vec::new();
        let mut preparations = Vec::new();
        for identity in identities {
            match identity {
                GenerationResourceIdentity::Actor(actor) => {
                    if let Some(entry) = self.entries.get(&actor) {
                        actors.push(entry.actor.clone());
                    }
                    if let Some(record) = self.reservations.get(&actor) {
                        reservations.push(record.terminal.clone());
                    }
                }
                GenerationResourceIdentity::Native { id, .. } => {
                    if let Some(resource) = self.resources.get(&id) {
                        resources.push(resource.resource.clone());
                    }
                }
                GenerationResourceIdentity::Preparation(id) => {
                    if let Some(preparation) = self.preparations.get(&id) {
                        preparations.push(preparation.compiler.clone());
                    }
                }
            }
        }
        let mut protected_actors = HashSet::new();
        let mut protected_cells = HashSet::new();
        for resource in &population.resources {
            match resource.identity {
                GenerationResourceIdentity::Actor(actor) => {
                    protected_actors.insert(actor);
                    if let Some(entry) = self.entries.get(&actor) {
                        protected_cells.insert(entry.actor.address().get_id());
                    } else if let Some(executor) = self
                        .reservations
                        .get(&actor)
                        .and_then(|record| record.executor.as_ref())
                    {
                        protected_cells.insert(executor.get_id());
                    }
                }
                GenerationResourceIdentity::Native { id, .. } => {
                    protected_cells.insert(id);
                }
                GenerationResourceIdentity::Preparation(_) => {}
            }
        }
        let capture = GenerationRetirementCapture {
            population,
            work,
            actors,
            resources,
            reservations,
            preparations,
            pending_claims,
            physical_closure: GenerationCapturePhysicalClosure::Guarded {
                actors: protected_actors,
                cells: protected_cells,
            },
        };
        let Some(attempt) = self
            .reservations
            .get_mut(&actor)
            .and_then(|record| record.retirement.active_mut())
        else {
            return;
        };
        let GenerationRetirementPhase::Unstarted { cutoff, .. } = &mut attempt.phase else {
            return;
        };
        match std::mem::replace(cutoff, GenerationRetirementCutoffPhase::NotStarted) {
            GenerationRetirementCutoffPhase::AwaitCapture {
                fence,
                delivery: GenerationCaptureDelivery::Enqueued,
            } => {
                *cutoff = GenerationRetirementCutoffPhase::Captured { fence, capture };
            }
            previous => *cutoff = previous,
        }
        // Physical closure revalidation and publication settlement are separate
        // owner transitions. Captured grants no authority to initiate a stop.
        self.prepare_default_generation_refusal(actor);
    }

    /// This private branch consumes only the actual capture and unexecuted
    /// reservation. It never requests resource stop, cancellation or publication.
    fn prepare_default_generation_refusal(&mut self, actor: ActorRef) {
        use tidepool_bridge_effects::{
            RlCause, RlCauseKind, RlRetirementReceipt, RlRetirementStage, RlRetirementState,
        };
        let Some(attempt) = self
            .reservations
            .get_mut(&actor)
            .and_then(|record| record.retirement.active_mut())
        else {
            return;
        };
        if attempt.diagnostic.is_cancelled_component() {
            return;
        }
        let GenerationRetirementPhase::Unstarted {
            publication,
            cutoff: GenerationRetirementCutoffPhase::Captured { capture, .. },
            progress,
            ..
        } = &mut attempt.phase
        else {
            return;
        };
        if !matches!(progress, GenerationUnstartedProgress::AwaitingDecision) {
            return;
        }
        let Some(reserved) = publication.reserved_mut() else {
            return;
        };
        let projected = (|| {
            let report = reserved
                .reload_report()
                .ok_or(GenerationRetirementProjectionRefusal::FrozenReloadReportMissing)?;
            let operation_id = attempt
                .diagnostic
                .operation_id()
                .ok_or(GenerationRetirementProjectionRefusal::OperationMissing)?;
            Ok(RlRetirementReceipt {
                previous: report.previous.clone(),
                candidate: report.candidate.clone(),
                operation: Some(retirement_operation(operation_id)?),
                state: RlRetirementState::ReloadRetirementFenced(
                    RlRetirementStage::ReloadCandidateSettlement,
                ),
                blockers: capture.blockers()?,
                outcomes: Vec::new(),
                surface: report.surface.clone(),
                primary: Some(RlCause {
                    kind: RlCauseKind::ReloadRetirementBlocked,
                    detail: "generation retirement refused because admitted dependent work remains"
                        .into(),
                }),
                secondary: match &capture.pending_claims {
                    Err(error) => vec![RlCause {
                        kind: RlCauseKind::ReloadAcknowledgementUnconfirmed,
                        detail: error.to_string(),
                    }],
                    Ok(_) => Vec::new(),
                },
            })
        })();
        *progress = match projected {
            Err(refusal) => GenerationUnstartedProgress::ProjectionRefused(refusal),
            Ok(mut receipt) if receipt.blockers.is_empty() => {
                // An empty census does not enable the unqualified publication
                // consumer. Retain the same reservation/fence for reconciliation.
                receipt.state = RlRetirementState::ReloadRetirementFenced(
                    RlRetirementStage::ReloadSourcePublication,
                );
                receipt.primary = Some(RlCause {
                    kind: RlCauseKind::ReloadPublicationFailed,
                    detail: "generation publication is unavailable".into(),
                });
                GenerationUnstartedProgress::PublicationNotQualified(Arc::new(receipt))
            }
            Ok(receipt) => GenerationUnstartedProgress::Refusing { receipt },
        };
        let record = self
            .reservations
            .get_mut(&actor)
            .expect("the exclusive owner retains its reservation");
        record.retirement.core.reconcile();
        self.release_live_source_observations(actor);
    }

    /// A zero-stop refusal never moved resources out of these canonical maps.
    /// The live owner may release only its extra capture references while the
    /// actual cleanup owners are still present. Native recovery has no maps and
    /// retains the SAME SourcesSettled capture instead.
    fn release_live_source_observations(&mut self, actor: ActorRef) {
        let Some(record) = self.reservations.get(&actor) else {
            return;
        };
        let GenerationRetirementCore::SourcesSettled { cutoff, .. } = &record.retirement.core
        else {
            return;
        };
        let GenerationRetirementCutoffPhase::Captured { capture, .. } = cutoff else {
            return;
        };
        if !capture.resources.iter().all(|captured| {
            self.resources
                .get(&captured.cell.get_id())
                .is_some_and(|canonical| {
                    Arc::ptr_eq(&captured.cleanup, &canonical.resource.cleanup)
                })
        }) {
            return;
        }
        if !capture.actors.iter().all(|captured| {
            self.entries
                .values()
                .any(|entry| entry.actor.address().get_id() == captured.address().get_id())
        }) {
            return;
        }
        // These exact keys issued the retained-exit clones during capture.
        // A missing canonical owner is never inferred to have cleaned up.
        let retained_owner_present = |identity: &GenerationResourceIdentity| match identity {
            GenerationResourceIdentity::Actor(actor) => self.reservations.contains_key(actor),
            GenerationResourceIdentity::Preparation(id) => self.preparations.contains_key(id),
            GenerationResourceIdentity::Native { id, .. } => self.resources.contains_key(id),
        };
        if !capture
            .population
            .resources
            .iter()
            .chain(&capture.population.unclassified)
            .all(|resource| retained_owner_present(&resource.identity))
        {
            return;
        }
        if !capture.population.collateral.iter().all(|edge| {
            retained_owner_present(&edge.stopping)
                && match &edge.affected {
                    GenerationCollateralIdentity::Classified(identity) => {
                        retained_owner_present(identity)
                    }
                    GenerationCollateralIdentity::UnclassifiedPhysical(_) => true,
                }
        }) {
            return;
        }
        let record = self
            .reservations
            .get_mut(&actor)
            .expect("the canonical record remains present");
        let old = std::mem::replace(&mut record.retirement.core, GenerationRetirementCore::Idle);
        let GenerationRetirementCore::SourcesSettled {
            actor,
            candidate,
            completion,
            cutoff,
        } = old
        else {
            unreachable!()
        };
        record.retirement.core = GenerationRetirementCore::Completed {
            actor,
            candidate,
            completion,
        };
        let driver = std::mem::replace(
            &mut record.retirement.driver,
            GenerationRetirementDriver::empty(),
        );
        drop((cutoff, driver));
    }

    /// Both vectors remain empty while the owner is live. Each retained record
    /// has at most one outstanding attempt, including records without an entry.
    pub(super) fn reserve_retirement_loss_capacity(
        &mut self,
        records: usize,
    ) -> Result<(), DirectoryError> {
        self.lost_retirements
            .try_reserve(records)
            .map_err(|_| DirectoryError::RetentionCapacityUnavailable)?;
        self.retirement_discards
            .try_reserve(records)
            .map_err(|_| DirectoryError::RetentionCapacityUnavailable)?;
        Ok(())
    }

    pub(super) fn retain_retirements_after_owner_loss(&mut self) {
        for record in self.reservations.values_mut() {
            let core =
                std::mem::replace(&mut record.retirement.core, GenerationRetirementCore::Idle);
            let driver = std::mem::replace(
                &mut record.retirement.driver,
                GenerationRetirementDriver::empty(),
            );
            if !matches!(core, GenerationRetirementCore::Idle) {
                // Capacity was reserved on canonical reservation admission.
                self.lost_retirements.push(core);
                self.retirement_discards
                    .push(GenerationRetirementDiscard { _driver: driver });
            }
        }
        let attempts = std::mem::take(&mut self.lost_retirements);
        let worker_transfers = std::mem::take(&mut self.worker_transfers);
        let resource_transfers = std::mem::take(&mut self.resource_transfers);
        let installed = self
            .native_cleanup
            .terminal
            .set(RunControlNativeExit::OwnerLost {
                custody: parking_lot::Mutex::new(Some(RunControlNativeLoss {
                    attempts,
                    worker_transfers,
                    resource_transfers,
                })),
            })
            .is_ok();
        if installed {
            // The actual affine products are installed before any waiter is
            // notified, including a waiter whose queued reply was lost.
            self.native_cleanup.ready.notify_waiters();
        }
        // Publisher closures can lead back to this client. Release them only
        // after the actual affine products are visible in the terminal owner.
        drop(std::mem::take(&mut self.retirement_discards));
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum GenerationDirectoryError {
    #[error(transparent)]
    Admission(#[from] GenerationAdmissionError),
    #[error(transparent)]
    Directory(#[from] DirectoryError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GenerationResourceState {
    Starting,
    Running,
    Transferring,
    CleanupUnsupported,
    CleanupUnconfirmed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum GenerationResourceIdentity {
    Actor(ActorRef),
    Native {
        issued_to: ActorRef,
        id: ractor::ActorId,
    },
    Preparation(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GenerationResourceObservation {
    pub(crate) identity: GenerationResourceIdentity,
    pub(crate) state: GenerationResourceState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GenerationCollateralIdentity {
    Classified(GenerationResourceIdentity),
    UnclassifiedPhysical(ractor::ActorId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GenerationLifetimeEdge {
    PhysicalSupervision,
    PendingSupervision,
    PendingResourcePreparation,
    ResourceOwnership,
    OwnershipTransfer,
    OwnershipTransferUnconfirmed,
}

/// An actual lifetime edge requiring exclusion or transfer settlement before
/// stopping. Source independence does not authorize stopping the affected owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GenerationCollateral {
    pub(crate) stopping: GenerationResourceIdentity,
    pub(crate) affected: GenerationCollateralIdentity,
    pub(crate) edge: GenerationLifetimeEdge,
}

/// Issued by the directory after all pre-fence reservation messages. This is
/// observation of the actual population, never a caller-authored stop list.
pub(crate) struct GenerationPopulation {
    pub(crate) dependency: GenerationDependency,
    pub(crate) resources: Vec<GenerationResourceObservation>,
    pub(crate) unclassified: Vec<GenerationResourceObservation>,
    pub(crate) collateral: Vec<GenerationCollateral>,
    pub(crate) compilers: Vec<GenerationDirectoryCompilerObservation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GenerationCompilerOwner {
    Actor(ActorRef),
    Preparation(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GenerationDirectoryCompilerObservation {
    pub(crate) owner: GenerationCompilerOwner,
    pub(crate) compiler: crate::termination::GenerationCompilerObservation,
}

impl RunState {
    pub(super) fn generation_population(
        &self,
        dependency: GenerationDependency,
    ) -> GenerationPopulation {
        let mut resources = Vec::new();
        let mut unclassified = Vec::new();
        let mut compilers = Vec::new();
        for (actor, record) in &self.reservations {
            compilers.extend(
                record
                    .terminal
                    .generation_compiler_observations(&dependency)
                    .into_iter()
                    .map(|compiler| GenerationDirectoryCompilerObservation {
                        owner: GenerationCompilerOwner::Actor(*actor),
                        compiler,
                    }),
            );
            let resources = if matches!(record.dependency, Some(SourceDependency::Unqualified)) {
                &mut unclassified
            } else if record
                .dependency
                .as_ref()
                .is_some_and(|issued| issued.depends_on(&dependency))
            {
                &mut resources
            } else {
                continue;
            };
            if matches!(record.phase, ReservationPhase::NotStarted) {
                continue;
            }
            if let ReservationPhase::StartUnconfirmed(detail) = &record.phase {
                resources.push(GenerationResourceObservation {
                    identity: GenerationResourceIdentity::Actor(*actor),
                    state: GenerationResourceState::CleanupUnconfirmed(detail.clone()),
                });
                continue;
            }
            if let Some(entry) = self.entries.get(actor) {
                if entry.actor.terminal().get().is_none() {
                    let link_state = match record.executor_link {
                        ExecutorLink::Confirmed => None,
                        ExecutorLink::Pending => Some(GenerationResourceState::Starting),
                        ExecutorLink::Unconfirmed => {
                            Some(GenerationResourceState::CleanupUnconfirmed(
                                "executor physical supervision differs from its issued reservation"
                                    .into(),
                            ))
                        }
                    };
                    if let Some(state) = link_state {
                        resources.push(GenerationResourceObservation {
                            identity: GenerationResourceIdentity::Actor(*actor),
                            state,
                        });
                        continue;
                    }
                    let state = if self
                        .worker_transfers
                        .values()
                        .any(|transfer| transfer.child == *actor)
                    {
                        GenerationResourceState::Transferring
                    } else {
                        GenerationResourceState::Running
                    };
                    resources.push(GenerationResourceObservation {
                        identity: GenerationResourceIdentity::Actor(*actor),
                        state,
                    });
                } else if !entry
                    .actor
                    .terminal()
                    .cleanup()
                    .is_some_and(|cleanup| cleanup.is_confirmed())
                {
                    resources.push(GenerationResourceObservation {
                        identity: GenerationResourceIdentity::Actor(*actor),
                        state: GenerationResourceState::CleanupUnconfirmed(
                            "actor terminal cleanup is not confirmed".into(),
                        ),
                    });
                }
            } else if record
                .terminal
                .cleanup()
                .is_some_and(|cleanup| cleanup.is_confirmed())
            {
                continue;
            } else if record.preparation_pending()
                || matches!(record.phase, ReservationPhase::Ready)
            {
                resources.push(GenerationResourceObservation {
                    identity: GenerationResourceIdentity::Actor(*actor),
                    state: GenerationResourceState::Starting,
                });
            } else {
                resources.push(GenerationResourceObservation {
                    identity: GenerationResourceIdentity::Actor(*actor),
                    state: GenerationResourceState::CleanupUnconfirmed(
                        "reservation has no confirmed terminal cleanup".into(),
                    ),
                });
            }
        }
        for (id, record) in &self.resources {
            let resources = if matches!(record.dependency, Some(SourceDependency::Unqualified)) {
                &mut unclassified
            } else if record
                .dependency
                .as_ref()
                .is_some_and(|issued| issued.depends_on(&dependency))
            {
                &mut resources
            } else {
                continue;
            };
            let state = match record.owner {
                ResourceOwner::Retired { .. } => continue,
                ResourceOwner::Transfer { .. } => GenerationResourceState::Transferring,
                ResourceOwner::Owned(_) => GenerationResourceState::Running,
            };
            resources.push(GenerationResourceObservation {
                identity: GenerationResourceIdentity::Native {
                    issued_to: record.issued_to,
                    id: *id,
                },
                state,
            });
        }
        for (id, record) in &self.preparations {
            compilers.extend(
                record
                    .compiler
                    .generation_compiler_observations(&dependency)
                    .into_iter()
                    .map(|compiler| GenerationDirectoryCompilerObservation {
                        owner: GenerationCompilerOwner::Preparation(*id),
                        compiler,
                    }),
            );
            let resources = if matches!(record.dependency, Some(SourceDependency::Unqualified)) {
                &mut unclassified
            } else if record
                .dependency
                .as_ref()
                .is_some_and(|issued| issued.depends_on(&dependency))
            {
                &mut resources
            } else {
                continue;
            };
            let state = match &record.phase {
                PreparationPhase::Running => GenerationResourceState::Starting,
                PreparationPhase::Accepted
                | PreparationPhase::Settled(crate::CleanupComponentOutcome::Confirmed) => continue,
                PreparationPhase::Settled(crate::CleanupComponentOutcome::Unsupported) => {
                    GenerationResourceState::CleanupUnsupported
                }
                PreparationPhase::Settled(crate::CleanupComponentOutcome::Unconfirmed(detail)) => {
                    GenerationResourceState::CleanupUnconfirmed(detail.clone())
                }
            };
            resources.push(GenerationResourceObservation {
                identity: GenerationResourceIdentity::Preparation(*id),
                state,
            });
        }
        let collateral = self.generation_collateral(&resources);
        GenerationPopulation {
            dependency,
            resources,
            unclassified,
            collateral,
            compilers,
        }
    }

    fn generation_collateral(
        &self,
        population: &[GenerationResourceObservation],
    ) -> Vec<GenerationCollateral> {
        let captured: HashSet<_> = population
            .iter()
            .map(|resource| resource.identity.clone())
            .collect();
        let mut collateral = Vec::new();
        for resource in population {
            let cell = match resource.identity {
                GenerationResourceIdentity::Actor(actor) => self
                    .entries
                    .get(&actor)
                    .map(|entry| entry.actor.address().get_cell())
                    .or_else(|| self.reservations.get(&actor)?.executor.clone()),
                GenerationResourceIdentity::Native { id, .. } => self
                    .resources
                    .get(&id)
                    .map(|record| record.resource.cell.clone()),
                GenerationResourceIdentity::Preparation(_) => None,
            };
            if let Some(cell) = cell {
                let mut pending = cell.get_children();
                let mut visited = HashSet::new();
                while let Some(child) = pending.pop() {
                    let id = child.get_id();
                    if !visited.insert(id) {
                        continue;
                    }
                    pending.extend(child.get_children());
                    let identity = self
                        .identities
                        .runtime
                        .get(&id)
                        .copied()
                        .map(GenerationResourceIdentity::Actor)
                        .or_else(|| {
                            self.resources.get(&id).map(|record| {
                                GenerationResourceIdentity::Native {
                                    issued_to: record.issued_to,
                                    id,
                                }
                            })
                        });
                    let affected = match identity {
                        Some(identity) if captured.contains(&identity) => continue,
                        Some(identity) => GenerationCollateralIdentity::Classified(identity),
                        None => GenerationCollateralIdentity::UnclassifiedPhysical(id),
                    };
                    collateral.push(GenerationCollateral {
                        stopping: resource.identity.clone(),
                        affected,
                        edge: GenerationLifetimeEdge::PhysicalSupervision,
                    });
                }
            }
            if let GenerationResourceIdentity::Native { id, .. } = resource.identity {
                if self.resources.get(&id).is_some_and(|record| {
                    matches!(record.owner, ResourceOwner::Transfer { id, .. } if !self.resource_transfers.contains_key(&id))
                }) {
                    collateral.push(GenerationCollateral {
                        stopping: resource.identity.clone(),
                        affected: GenerationCollateralIdentity::Classified(resource.identity.clone()),
                        edge: GenerationLifetimeEdge::OwnershipTransferUnconfirmed,
                    });
                }
            }
            let GenerationResourceIdentity::Actor(parent) = resource.identity else {
                continue;
            };
            for (id, record) in &self.preparations {
                if record.owner == RootPreparationOwner::NativeResource(parent)
                    && matches!(record.phase, PreparationPhase::Running)
                {
                    let identity = GenerationResourceIdentity::Preparation(*id);
                    if !captured.contains(&identity) {
                        collateral.push(GenerationCollateral {
                            stopping: resource.identity.clone(),
                            affected: GenerationCollateralIdentity::Classified(identity),
                            edge: GenerationLifetimeEdge::PendingResourcePreparation,
                        });
                    }
                }
            }
            for transfer in self.worker_transfers.values() {
                if transfer.parent == parent {
                    collateral.push(GenerationCollateral {
                        stopping: resource.identity.clone(),
                        affected: GenerationCollateralIdentity::Classified(
                            GenerationResourceIdentity::Actor(transfer.child),
                        ),
                        edge: GenerationLifetimeEdge::OwnershipTransfer,
                    });
                }
            }
            for (child, record) in &self.reservations {
                let ownership = self
                    .entries
                    .get(child)
                    .map_or(record.planned_ownership, |entry| entry.spawn_ownership);
                if ownership != SpawnOwnership::Supervised(parent)
                    || matches!(record.phase, ReservationPhase::NotStarted)
                    || record
                        .terminal
                        .cleanup()
                        .is_some_and(|cleanup| cleanup.is_confirmed())
                {
                    continue;
                }
                let identity = GenerationResourceIdentity::Actor(*child);
                if !captured.contains(&identity) {
                    collateral.push(GenerationCollateral {
                        stopping: resource.identity.clone(),
                        affected: GenerationCollateralIdentity::Classified(identity),
                        edge: GenerationLifetimeEdge::PendingSupervision,
                    });
                }
            }
            for (id, record) in &self.resources {
                let edge = match record.owner {
                    ResourceOwner::Owned(ResourceLifetime::Actor(owner)) if owner == parent => {
                        GenerationLifetimeEdge::ResourceOwnership
                    }
                    ResourceOwner::Transfer { id, source }
                        if source == ResourceLifetime::Actor(parent)
                            || self.resource_transfers.get(&id).is_some_and(|transfer| {
                                transfer.destination == ResourceLifetime::Actor(parent)
                            }) =>
                    {
                        if self.resource_transfers.contains_key(&id) {
                            GenerationLifetimeEdge::OwnershipTransfer
                        } else {
                            GenerationLifetimeEdge::OwnershipTransferUnconfirmed
                        }
                    }
                    _ => continue,
                };
                let identity = GenerationResourceIdentity::Native {
                    issued_to: record.issued_to,
                    id: *id,
                };
                if matches!(
                    edge,
                    GenerationLifetimeEdge::OwnershipTransfer
                        | GenerationLifetimeEdge::OwnershipTransferUnconfirmed
                ) || !captured.contains(&identity)
                {
                    collateral.push(GenerationCollateral {
                        stopping: resource.identity.clone(),
                        affected: GenerationCollateralIdentity::Classified(identity),
                        edge,
                    });
                }
            }
        }
        collateral
    }
}

impl RunControlClient {
    /// Send under the gate; waiting for the directory and startup happens after
    /// releasing it. A subsequent fence observation cannot overtake this send.
    fn source_enqueue<T: Send + 'static>(
        &self,
        dependency: &SourceDependency,
        message: impl FnOnce(oneshot::Sender<T>) -> RunControlMessage,
    ) -> Result<oneshot::Receiver<T>, GenerationDirectoryError> {
        let (reply, receiver) = oneshot::channel();
        let message = message(reply);
        dependency
            .with_open_admission(|| self.owner.send_message(message))?
            .map_err(|_| DirectoryError::Unavailable)?;
        Ok(receiver)
    }

    pub(crate) async fn reserve_source_worker(
        &self,
        dependency: &SourceDependency,
        parent: ActorRef,
        incarnation: crate::Incarnation,
    ) -> Result<ActorReservation, GenerationDirectoryError> {
        let receiver =
            self.source_enqueue(dependency, |reply| RunControlMessage::ReserveWorker {
                parent,
                incarnation,
                dependency: Some(dependency.clone()),
                reply,
            })?;
        Ok(receiver.await.map_err(|_| DirectoryError::Unavailable)??)
    }

    pub(crate) async fn reserve_source_successor(
        &self,
        dependency: &SourceDependency,
        predecessor: ActorRef,
        incarnation: crate::Incarnation,
    ) -> Result<ActorReservation, GenerationDirectoryError> {
        let receiver =
            self.source_enqueue(dependency, |reply| RunControlMessage::ReserveSuccessor {
                predecessor,
                incarnation,
                dependency: Some(dependency.clone()),
                reply,
            })?;
        Ok(receiver.await.map_err(|_| DirectoryError::Unavailable)??)
    }

    pub(crate) async fn reserve_source_actor(
        &self,
        dependency: &SourceDependency,
        incarnation: crate::Incarnation,
    ) -> Result<ActorReservation, GenerationDirectoryError> {
        let receiver =
            self.source_enqueue(dependency, |reply| RunControlMessage::ReserveSourceActor {
                dependency: dependency.clone(),
                incarnation,
                reply,
            })?;
        Ok(receiver.await.map_err(|_| DirectoryError::Unavailable)??)
    }

    /// The reservation owner supplies provenance; callers cannot substitute a
    /// generation or omit it when preparing an already reserved worker.
    pub(crate) async fn prepare_worker_root(
        &self,
        reservation: ActorRef,
        operation: RootOperation,
    ) -> Result<RootOperationResult, DirectoryError> {
        self.prepare_owned_root(
            RootPreparationOwner::Worker(reservation),
            PreparationIntent::Creation,
            operation,
        )
        .await
    }

    pub(crate) async fn generation_population(
        &self,
        fence: &GenerationFence,
    ) -> Result<GenerationPopulation, DirectoryError> {
        self.ask(|reply| RunControlMessage::GenerationPopulation {
            dependency: fence.dependency().clone(),
            reply,
        })
        .await
    }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Probe;
    impl Actor for Probe {
        type Msg = ();
        type State = Arc<AtomicUsize>;
        type Arguments = Arc<AtomicUsize>;

        async fn pre_start(
            &self,
            _myself: RactorRef<()>,
            stopped: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(stopped)
        }

        async fn post_stop(
            &self,
            _myself: RactorRef<()>,
            stopped: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            stopped.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn handle_supervisor_evt(
            &self,
            _myself: RactorRef<()>,
            _event: ractor::SupervisionEvent,
            _stopped: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            // Like LocalActor, this parent handles child exit independently of
            // its own lifetime. Ractor's default instead stops the supervisor.
            Ok(())
        }
    }

    fn retain_native(state: &mut RunState, cell: ractor::ActorCell, issued_to: ActorRef) {
        state.resources.insert(
            cell.get_id(),
            OwnedResource {
                resource: ResourceChild {
                    cell,
                    cleanup: Arc::new(|_| {
                        Box::pin(async { crate::CleanupComponentOutcome::Confirmed })
                    }),
                },
                issued_to,
                owner: ResourceOwner::Owned(ResourceLifetime::Run),
                dependency: None,
            },
        );
    }

    fn observation(cell: &ractor::ActorCell, issued_to: ActorRef) -> GenerationResourceObservation {
        GenerationResourceObservation {
            identity: GenerationResourceIdentity::Native {
                issued_to,
                id: cell.get_id(),
            },
            state: GenerationResourceState::Running,
        }
    }

    #[test]
    fn pending_neutral_resource_preparation_is_a_physical_collateral_obligation() {
        let parent = ActorRef::first(crate::ActorId(1));
        let mut state = RunState::default();
        state.preparations.insert(
            7,
            RootPreparationRecord {
                owner: RootPreparationOwner::NativeResource(parent),
                phase: PreparationPhase::Running,
                compiler: RetainedActorExit::new(),
                placement: None,
                dependency: None,
            },
        );
        let captured = [GenerationResourceObservation {
            identity: GenerationResourceIdentity::Actor(parent),
            state: GenerationResourceState::Running,
        }];
        assert_eq!(
            state.generation_collateral(&captured),
            [GenerationCollateral {
                stopping: captured[0].identity.clone(),
                affected: GenerationCollateralIdentity::Classified(
                    GenerationResourceIdentity::Preparation(7)
                ),
                edge: GenerationLifetimeEdge::PendingResourcePreparation,
            }]
        );
        state.preparations.get_mut(&7).unwrap().phase =
            PreparationPhase::Settled(crate::CleanupComponentOutcome::Confirmed);
        assert!(state.generation_collateral(&captured).is_empty());
    }

    #[tokio::test]
    async fn missing_transfer_observation_remains_typed_uncertainty() {
        let stopped = Arc::new(AtomicUsize::new(0));
        let (actor, task) = Actor::spawn(None, Probe, stopped.clone()).await.unwrap();
        let issued_to = ActorRef::first(crate::ActorId(1));
        let mut state = RunState::default();
        retain_native(&mut state, actor.get_cell(), issued_to);
        state.resources.get_mut(&actor.get_id()).unwrap().owner = ResourceOwner::Transfer {
            id: 9,
            source: ResourceLifetime::Run,
        };
        let captured = [observation(&actor.get_cell(), issued_to)];
        assert_eq!(
            state.generation_collateral(&captured),
            [GenerationCollateral {
                stopping: captured[0].identity.clone(),
                affected: GenerationCollateralIdentity::Classified(captured[0].identity.clone()),
                edge: GenerationLifetimeEdge::OwnershipTransferUnconfirmed,
            }]
        );
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
        actor
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn physical_capture_reports_independent_child_and_leaves_sibling_live() {
        let stopped = Arc::new(AtomicUsize::new(0));
        let (parent, parent_task) = Actor::spawn(None, Probe, stopped.clone()).await.unwrap();
        let (child, child_task) =
            Actor::spawn_linked(None, Probe, stopped.clone(), parent.get_cell())
                .await
                .unwrap();
        let (sibling, sibling_task) = Actor::spawn(None, Probe, stopped.clone()).await.unwrap();
        let actor = ActorRef::first(crate::ActorId(1));
        let independent = ActorRef {
            id: actor.id,
            incarnation: crate::Incarnation(2),
        };
        let mut state = RunState::default();
        retain_native(&mut state, parent.get_cell(), actor);
        retain_native(&mut state, child.get_cell(), independent);
        retain_native(&mut state, sibling.get_cell(), independent);
        let captured = [observation(&parent.get_cell(), actor)];
        let report = state.generation_collateral(&captured);
        assert_eq!(
            report,
            [GenerationCollateral {
                stopping: captured[0].identity.clone(),
                affected: GenerationCollateralIdentity::Classified(
                    observation(&child.get_cell(), independent).identity
                ),
                edge: GenerationLifetimeEdge::PhysicalSupervision,
            }]
        );
        assert_eq!(
            stopped.load(Ordering::SeqCst),
            0,
            "capture dispatches no stops"
        );
        assert_ne!(sibling.get_status(), ActorStatus::Stopped);
        child
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        parent
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        sibling
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        child_task.await.unwrap();
        parent_task.await.unwrap();
        sibling_task.await.unwrap();
        assert_eq!(stopped.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn physical_capture_traverses_captured_child_and_preserves_unclassified_grandchild() {
        let stopped = Arc::new(AtomicUsize::new(0));
        let (parent, parent_task) = Actor::spawn(None, Probe, stopped.clone()).await.unwrap();
        let (child, child_task) =
            Actor::spawn_linked(None, Probe, stopped.clone(), parent.get_cell())
                .await
                .unwrap();
        let (grandchild, grandchild_task) =
            Actor::spawn_linked(None, Probe, stopped.clone(), child.get_cell())
                .await
                .unwrap();
        let actor = ActorRef::first(crate::ActorId(1));
        let mut state = RunState::default();
        retain_native(&mut state, parent.get_cell(), actor);
        retain_native(&mut state, child.get_cell(), actor);
        let captured = [
            observation(&parent.get_cell(), actor),
            observation(&child.get_cell(), actor),
        ];
        let report = state.generation_collateral(&captured);
        assert_eq!(report.len(), 2);
        for (edge, stopping) in report.iter().zip(&captured) {
            assert_eq!(edge.stopping, stopping.identity);
            assert_eq!(
                edge.affected,
                GenerationCollateralIdentity::UnclassifiedPhysical(grandchild.get_id())
            );
        }
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
        grandchild
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        child
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        parent
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        grandchild_task.await.unwrap();
        child_task.await.unwrap();
        parent_task.await.unwrap();
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(16))]
        #[test]
        fn physical_capture_matches_independent_parent_index_oracle(
            nodes in proptest::collection::vec((proptest::bool::ANY, proptest::bool::ANY, 0usize..16), 1..7)
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async {
                let stopped = Arc::new(AtomicUsize::new(0));
                let issued_to = ActorRef::first(crate::ActorId(1));
                let mut state = RunState::default();
                let mut actors: Vec<RactorRef<()>> = Vec::new();
                let mut tasks = Vec::new();
                let mut parents = Vec::new();
                let mut selected = Vec::new();
                let mut captured = Vec::new();
                for (index, (capture, known, parent_seed)) in nodes.iter().enumerate() {
                    let parent = if index == 0 || parent_seed % 2 == 0 {
                        None
                    } else {
                        Some(parent_seed % index)
                    };
                    let (actor, task) = if let Some(parent) = parent {
                        Actor::spawn_linked(None, Probe, stopped.clone(), actors[parent].get_cell()).await.unwrap()
                    } else {
                        Actor::spawn(None, Probe, stopped.clone()).await.unwrap()
                    };
                    let capture = index == 0 || *capture;
                    if capture || *known {
                        retain_native(&mut state, actor.get_cell(), issued_to);
                    }
                    if capture {
                        captured.push(observation(&actor.get_cell(), issued_to));
                    }
                    selected.push(capture);
                    parents.push(parent);
                    actors.push(actor);
                    tasks.push(task);
                }
                let mut expected = HashSet::new();
                for (stopping, capture) in selected.iter().enumerate() {
                    if !capture { continue; }
                    for affected in 0..nodes.len() {
                        if selected[affected] { continue; }
                        let mut cursor = parents[affected];
                        while let Some(parent) = cursor {
                            if parent == stopping {
                                expected.insert((stopping, affected));
                                break;
                            }
                            cursor = parents[parent];
                        }
                    }
                }
                let report = state.generation_collateral(&captured);
                let observed: HashSet<_> = report.iter().map(|edge| {
                    assert_eq!(edge.edge, GenerationLifetimeEdge::PhysicalSupervision);
                    let GenerationResourceIdentity::Native { id, .. } = edge.stopping else { unreachable!() };
                    let stopping = actors.iter().position(|actor| actor.get_id() == id).unwrap();
                    let id = match &edge.affected {
                        GenerationCollateralIdentity::Classified(GenerationResourceIdentity::Native { id, .. }) => *id,
                        GenerationCollateralIdentity::UnclassifiedPhysical(id) => *id,
                        _ => unreachable!(),
                    };
                    let affected = actors.iter().position(|actor| actor.get_id() == id).unwrap();
                    assert_eq!(matches!(&edge.affected, GenerationCollateralIdentity::UnclassifiedPhysical(_)), !nodes[affected].1);
                    (stopping, affected)
                }).collect();
                assert_eq!(observed, expected);
                assert_eq!(stopped.load(Ordering::SeqCst), 0);
                for actor in actors.iter().rev() {
                    actor.stop_and_wait(None, Some(Duration::from_secs(2))).await.unwrap();
                }
                for task in tasks { task.await.unwrap(); }
                assert_eq!(stopped.load(Ordering::SeqCst), nodes.len());
            });
        }
    }
}

#[cfg(test)]
mod retirement_retention_tests {
    use super::*;

    #[test]
    fn loss_capacity_refuses_overflow_without_issuing_an_identity() {
        let mut state = RunState::default();
        assert_eq!(
            state.reserve_retirement_loss_capacity(usize::MAX),
            Err(DirectoryError::RetentionCapacityUnavailable)
        );
        assert!(state.identities.claimed.is_empty());
        assert!(state.reservations.is_empty());
        assert!(state.lost_retirements.is_empty());
        assert!(state.retirement_discards.is_empty());
    }

    #[tokio::test]
    async fn record_issuer_reserves_both_empty_loss_vectors_before_admission() {
        let mut state = RunState::default();
        for id in 1..=5 {
            let actor = ActorRef::first(crate::ActorId(id));
            let (reply, received) = oneshot::channel();
            begin_reservation(&mut state, actor, ReservationPurpose::Root, reply);
            assert!(matches!(
                received.await.unwrap(),
                Err(DirectoryError::Unavailable)
            ));
            assert_eq!(state.identities.claimed.get(&actor.id), Some(&actor));
            assert_eq!(state.reservations.len(), id as usize);
            assert!(state.lost_retirements.capacity() >= state.reservations.len());
            assert!(state.retirement_discards.capacity() >= state.reservations.len());
            assert!(state.lost_retirements.is_empty());
            assert!(state.retirement_discards.is_empty());
        }
        let capacities = (
            state.lost_retirements.capacity(),
            state.retirement_discards.capacity(),
        );
        let actor = ActorRef::first(crate::ActorId(1));
        let (reply, received) = oneshot::channel();
        begin_reservation(&mut state, actor, ReservationPurpose::Root, reply);
        assert!(matches!(
            received.await.unwrap(),
            Err(DirectoryError::IdentityConflict { .. })
        ));
        assert_eq!(
            (
                state.lost_retirements.capacity(),
                state.retirement_discards.capacity()
            ),
            capacities
        );
        assert_eq!(state.reservations.len(), 5);
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenerationRetirementNativeControl {
    PublicationBodyRefusal,
    WorkerLiveRefusal,
    WorkerNativeLossAfterBody,
    BeforeSend,
    QueuedBeforeReceive,
    KillAfterReserve,
    KillAfterReserveDisposalPanic,
    PanicAfterReserve,
    LostWaiterAfterReserve,
    AfterCapture,
}

#[cfg(test)]
mod retirement_owner_tests;
