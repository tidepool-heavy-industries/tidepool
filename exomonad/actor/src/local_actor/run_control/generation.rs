//! Generation provenance travels through the existing run-directory owners.
#[cfg(test)]
mod publication_protocol_tests;

use super::*;
use crate::resident_workbench::generation_admission::{GenerationAdmissionError, GenerationFence};
use crate::ResidentActorWorkbenchError;

use crate::resident_workbench::generation_admission::{
    GenerationRetirementFence, GenerationRetirementFenceError,
};
use crate::resident_workbench::{
    ActorGeneration, ActorGenerationCandidateCustody, ActorGenerationCandidateObserver,
    ActorGenerationCommit, ActorGenerationPublicationBody, ActorGenerationPublicationBodyResult,
    ActorGenerationPublicationDriver, ActorGenerationPublicationOwner,
    ActorGenerationPublicationRefusalObservation, ActorGenerationReservationDriver,
    ActorGenerationRetirementCutoff, ActorGenerationRetirementPreparation,
    ActorGenerationRetirementPreparationOutcome, ActorPublicationAttemptObserver,
    PreparedActorGeneration, PublicationOwnerSettlement, ReservedActorGeneration,
    UnstartedRetirementSourceSettlement,
};

pub(crate) type GenerationRetirementResult = Arc<tidepool_bridge_effects::RlRetirementReceipt>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum GenerationRetirementRequestError {
    #[error("retirement preparation belongs to a different actor")]
    ActorChanged,
    #[error("retirement preparation and execution control differ")]
    ExecutionChanged,
    #[error("reload preparation has no frozen reload report")]
    FrozenReloadReportMissing,
    #[error("generation retirement was not admitted: {0}")]
    Admission(GenerationRetirementBeginFailure),
    #[error("the run owner did not accept generation retirement")]
    NotEnqueued,
    #[error("the admitted operation cannot be represented in the reload receipt")]
    OperationCoordinate,
}

pub(super) enum GenerationRetirementBeginReply {
    #[cfg(test)]
    Observe(
        oneshot::Sender<Result<ActorPublicationAttemptObserver, GenerationRetirementBeginFailure>>,
    ),
    Reload(GenerationRetirementReloadReply),
}

#[derive(Clone)]
struct GenerationRetirementReplyMetadata {
    report: Arc<crate::resident_workbench::PreparedReloadReport>,
    candidate: ActorGenerationCandidateObserver,
    operation: tidepool_bridge_effects::RlOperation,
}

pub(super) struct GenerationRetirementReloadReply {
    sender: oneshot::Sender<Result<GenerationRetirementResult, GenerationRetirementRequestError>>,
    metadata: GenerationRetirementReplyMetadata,
    force: bool,
    publication_owner: GenerationRetirementPublicationOwner,
}

/// Existing notification handles move from the transient driver to the linked
/// worker. They never enter native core custody or confer observer authority.
pub(super) enum GenerationRetirementPublicationOwner {
    Pending {
        lifecycle: crate::LifecycleHandle,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    },
    Transferred,
    Observed(preparation::PublicationNotificationObservation),
}

impl GenerationRetirementPublicationOwner {
    pub(super) fn observe(
        &mut self,
        outcome: &Result<ActorGenerationCommit, ResidentActorWorkbenchError>,
    ) -> preparation::PublicationNotificationObservation {
        if let Self::Observed(observation) = self {
            return *observation;
        }
        let Ok(ActorGenerationCommit::Published {
            generation,
            cancellation_pending,
        }) = outcome
        else {
            return preparation::PublicationNotificationObservation::NotRequired;
        };
        let Self::Pending { lifecycle, control } = std::mem::replace(self, Self::Transferred)
        else {
            return preparation::PublicationNotificationObservation::Unobserved;
        };
        // These are the existing owning notifications, before the settlement
        // receiver or any user waiter can disappear. No unknown outcome issues
        // a successful publication fact.
        if let Some(control) = control {
            control.complete_continuing_publication(*cancellation_pending);
        }
        let observation = match lifecycle.generation_published(generation.clone()) {
            crate::kernel::GenerationPublicationNotification::Accepted => {
                preparation::PublicationNotificationObservation::Accepted
            }
            crate::kernel::GenerationPublicationNotification::Refused => {
                preparation::PublicationNotificationObservation::Refused
            }
        };
        *self = Self::Observed(observation);
        observation
    }
}

#[cfg(test)]
pub(crate) async fn assert_confirmed_publication_notifies_once(
    outcome: &Result<ActorGenerationCommit, ResidentActorWorkbenchError>,
) {
    use preparation::PublicationNotificationObservation as Observation;
    let Ok(ActorGenerationCommit::Published { generation, .. }) = outcome else {
        panic!("notification control requires the actual confirmed publisher result");
    };
    let lifecycle =
        crate::LifecycleHandle::start(generation.context().actor, crate::RetainedActorExit::new())
            .await
            .unwrap();
    let mut owner = GenerationRetirementPublicationOwner::Pending {
        lifecycle: lifecycle.clone(),
        control: None,
    };
    assert_eq!(owner.observe(outcome), Observation::Accepted);
    lifecycle.assert_publication_notification_attempts(1);
    lifecycle.stop_owner_for_test().await;
    // A new send to this same, confirmed-stopped recipient refuses. The
    // original observation must retain its accepted transfer without replay.
    let mut fresh = GenerationRetirementPublicationOwner::Pending {
        lifecycle: lifecycle.clone(),
        control: None,
    };
    assert_eq!(fresh.observe(outcome), Observation::Refused);
    lifecycle.assert_publication_notification_attempts(2);
    assert_eq!(owner.observe(outcome), Observation::Accepted);
    assert_eq!(owner.observe(outcome), Observation::Accepted);
    lifecycle.assert_publication_notification_attempts(2);
}

/// The canonical reservation owns one affine core and its transient driver.
/// Native owner loss moves the core unchanged before disposing the driver.
pub(super) struct GenerationRetirementRecord {
    core: GenerationRetirementCore,
    driver: GenerationRetirementDriver,
}

pub(super) enum GenerationRetirementCore {
    Idle,
    Initial {
        actor: ActorRef,
        candidate: ActorGenerationCandidateCustody,
        phase: InitialPublicationPhase,
    },
    InitialCompleted {
        actor: ActorRef,
        generation: std::sync::Weak<ActorGeneration>,
    },
    InitialRefused {
        actor: ActorRef,
        candidate: ActorGenerationCandidateObserver,
    },
    Replacement(GenerationRetirementAttempt),
    SourcesSettled {
        actor: ActorRef,
        candidate: ActorGenerationCandidateObserver,
        completion: GenerationRetirementCompletion,
        report: Option<Arc<crate::resident_workbench::PreparedReloadReport>>,
        cutoff: GenerationRetirementCutoffPhase,
    },
    Completed {
        actor: ActorRef,
        candidate: ActorGenerationCandidateObserver,
        completion: GenerationRetirementCompletion,
        report: Option<Arc<crate::resident_workbench::PreparedReloadReport>>,
    },
}

enum InitialPublicationPhase {
    Preparing(PreparedActorGeneration),
    Reserving,
    ReservationFailed(ActorGenerationRetirementPreparationOutcome),
    CandidateChanged(PreparedActorGeneration),
    Publishing {
        observer: ActorPublicationAttemptObserver,
        owner: ActorGenerationPublicationOwner,
        custody: GenerationPublicationCustody,
    },
}

#[derive(thiserror::Error)]
pub(crate) enum InitialGenerationPublicationError {
    #[error("initial publication belongs to another actor")]
    ActorChanged,
    #[error("initial publication has a replacement generation")]
    ExpectedBootstrap,
    #[error("initial publication was not admitted: {0}")]
    Admission(GenerationRetirementBeginFailure),
    #[error("initial publication was not enqueued")]
    NotEnqueued,
    #[error("initial publication owner was lost; actual custody remains retained")]
    OwnerLost,
    #[error("initial publication remains unconfirmed in its actual owner")]
    Unconfirmed,
    #[error("initial source settlement failed: {0}")]
    Settlement(Arc<crate::resident_workbench::PublicationSettlementError>),
    #[error("initial generation notification was refused")]
    NotificationRefused,
    #[error("initial publication body was returned without execution: {0:?}")]
    NotStarted(preparation::PublicationNonStartKind),
    #[error("initial publication did not commit: {}", initial_body_failure(.0))]
    Body(Arc<Result<ActorGenerationCommit, ResidentActorWorkbenchError>>),
    #[error("initial publication retains unresolved obligations: {}", initial_body_failure(.0))]
    UnconfirmedBody(Arc<Result<ActorGenerationCommit, ResidentActorWorkbenchError>>),
}

fn initial_body_failure(
    outcome: &Result<ActorGenerationCommit, ResidentActorWorkbenchError>,
) -> String {
    match outcome {
        Ok(ActorGenerationCommit::Refused(issue) | ActorGenerationCommit::Unconfirmed(issue)) => {
            issue.to_string()
        }
        Err(error) => error.to_string(),
        Ok(ActorGenerationCommit::Published { .. }) => {
            "publication outcome cannot be treated as a refusal".into()
        }
    }
}

impl InitialGenerationPublicationError {
    pub(crate) fn requires_recovery(&self) -> bool {
        match self {
            Self::OwnerLost
            | Self::Unconfirmed
            | Self::UnconfirmedBody(_)
            | Self::Settlement(_)
            | Self::NotificationRefused => true,
            Self::NotStarted(preparation::PublicationNonStartKind::SpawnFailed) => true,
            Self::Body(outcome) => match outcome.as_ref() {
                Err(error) => error.requires_recovery(),
                Ok(ActorGenerationCommit::Unconfirmed(_)) => true,
                Ok(ActorGenerationCommit::Refused(_) | ActorGenerationCommit::Published { .. }) => {
                    false
                }
            },
            _ => false,
        }
    }
}

impl std::fmt::Debug for InitialGenerationPublicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

pub(super) struct InitialGenerationPublicationReply {
    sender: oneshot::Sender<Result<Arc<ActorGeneration>, InitialGenerationPublicationError>>,
    lifecycle: crate::LifecycleHandle,
}

struct GenerationRetirementDriver {
    action: GenerationRetirementAction,
    inventory: GenerationRetirementInventory,
    reply: Option<GenerationRetirementReloadReply>,
    publication_owner: Option<GenerationRetirementPublicationOwner>,
    initial_reply: Option<InitialGenerationPublicationReply>,
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
            reply: None,
            publication_owner: None,
            initial_reply: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum GenerationRetirementCompletion {
    UnreservedRefused,
    BPublished {
        generation: tidepool_runtime::session::NotebookGenerationId,
    },
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

    fn project_publication(&mut self, publication: &ActorGenerationPublicationOwner) {
        let handoff = match self {
            Self::Operation { handoff, .. } => handoff,
            #[cfg(test)]
            Self::CancelledComponent => return,
        };
        if !matches!(handoff, RetirementDiagnosticHandoff::Unprojected) {
            return;
        }
        let evidence = if let Some((outcome, observation)) = publication.diagnostic_outcome() {
            crate::resident_actor::RetirementDiagnosticEvidence::from_outcome(outcome, observation)
        } else if let Some(primary) = publication.unobserved_primary() {
            crate::resident_actor::RetirementDiagnosticEvidence::unobserved_publication(primary)
        } else {
            return;
        };
        *handoff = RetirementDiagnosticHandoff::Projected {
            evidence: Arc::new(evidence),
            refusals: Vec::new(),
        };
    }

    fn project_nonstart(&mut self, reason: &preparation::PublicationNonStart) {
        let Self::Operation { handoff, .. } = self else {
            return;
        };
        if matches!(handoff, RetirementDiagnosticHandoff::Unprojected) {
            let detail = match reason {
                preparation::PublicationNonStart::SpawnFailed(error) => {
                    format!("publication worker could not start: {error}")
                }
                _ => format!(
                    "publication body was returned without execution: {:?}",
                    reason.kind()
                ),
            };
            *handoff = RetirementDiagnosticHandoff::Projected {
                evidence: Arc::new(
                    crate::resident_actor::RetirementDiagnosticEvidence::nonstart(detail),
                ),
                refusals: Vec::new(),
            };
        }
    }

    fn retry_projected(&mut self) {
        match self {
            Self::Operation { owner, handoff } => {
                Self::write_projected(owner, handoff);
            }
            #[cfg(test)]
            Self::CancelledComponent => {}
        }
    }

    fn recorded(
        &self,
    ) -> Option<(
        crate::resident_actor::RecordedRetirementDiagnosticObservation,
        Vec<crate::resident_actor::WorkbenchDiagnosticRefusal>,
    )> {
        match self {
            Self::Operation {
                handoff:
                    RetirementDiagnosticHandoff::Recorded {
                        observation,
                        refusals,
                    },
                ..
            } => Some((observation.clone(), refusals.clone())),
            _ => None,
        }
    }

    fn outstanding(
        &self,
        evidence_required: bool,
    ) -> Option<tidepool_bridge_effects::RlObligationObservation> {
        use tidepool_bridge_effects::RlObligationObservation as Observation;
        match self {
            #[cfg(test)]
            Self::CancelledComponent => None,
            Self::Operation { handoff, .. } => match handoff {
                RetirementDiagnosticHandoff::Recorded { .. } => None,
                RetirementDiagnosticHandoff::Unprojected => {
                    evidence_required.then_some(Observation::ReloadObligationPending)
                }
                RetirementDiagnosticHandoff::Projected { refusals, .. } => {
                    Some(if refusals.is_empty() {
                        Observation::ReloadObligationPending
                    } else {
                        Observation::ReloadObligationRefused
                    })
                }
                RetirementDiagnosticHandoff::ReceiptRefused { .. } => {
                    Some(Observation::ReloadObligationRefused)
                }
            },
        }
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
        let (owner, handoff) = match self {
            Self::Operation { owner, handoff } => (owner, handoff),
            #[cfg(test)]
            Self::CancelledComponent => return None,
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

impl KernelContext {
    pub(crate) async fn retire_actor_generation(
        &self,
        preparation: ActorGenerationRetirementPreparation,
        inventory: crate::resident_actor::ResidentGenerationInventory,
        diagnostic: crate::resident_actor::WorkbenchOperationDiagnosticOwner,
        force: bool,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> Result<GenerationRetirementResult, GenerationRetirementRequestError> {
        if preparation.actor() != self.identity {
            return Err(GenerationRetirementRequestError::ActorChanged);
        }
        if !preparation.matches_execution_control(control.as_ref()) {
            return Err(GenerationRetirementRequestError::ExecutionChanged);
        }
        let report = Arc::new(
            preparation
                .reload_report()
                .ok_or(GenerationRetirementRequestError::FrozenReloadReportMissing)?
                .clone(),
        );
        let operation = retirement_operation(diagnostic.operation())
            .map_err(|_| GenerationRetirementRequestError::OperationCoordinate)?;
        let metadata = GenerationRetirementReplyMetadata {
            report,
            candidate: preparation.candidate_custody().identity_observer(),
            operation,
        };
        self.directory
            .request_generation_retirement(
                preparation,
                inventory,
                diagnostic,
                force,
                control,
                self.lifecycle.clone(),
                metadata,
            )
            .await
    }
}

impl RunControlClient {
    pub(crate) async fn publish_initial_generation(
        &self,
        preparation: ActorGenerationRetirementPreparation,
        lifecycle: crate::LifecycleHandle,
    ) -> Result<Arc<ActorGeneration>, InitialGenerationPublicationError> {
        if preparation.actor() != lifecycle.identity() {
            return Err(InitialGenerationPublicationError::ActorChanged);
        }
        if !preparation.is_initial() {
            return Err(InitialGenerationPublicationError::ExpectedBootstrap);
        }
        let (sender, receiver) = oneshot::channel();
        let message = RunControlMessage::BeginInitialGenerationPublication {
            preparation,
            reply: InitialGenerationPublicationReply { sender, lifecycle },
        };
        match self.owner.send_message(message) {
            Ok(()) => {}
            Err(ractor::MessagingErr::SendErr(message)) => {
                // Returned unreserved preparation retains its own provisional
                // cleanup; accepted messages are owned only by RunState.
                drop(message);
                return Err(InitialGenerationPublicationError::NotEnqueued);
            }
            Err(_) => return Err(InitialGenerationPublicationError::OwnerLost),
        }
        receiver
            .await
            .unwrap_or(Err(InitialGenerationPublicationError::OwnerLost))
    }
}

impl RunControlClient {
    async fn request_generation_retirement(
        &self,
        preparation: ActorGenerationRetirementPreparation,
        inventory: crate::resident_actor::ResidentGenerationInventory,
        diagnostic: crate::resident_actor::WorkbenchOperationDiagnosticOwner,
        force: bool,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
        lifecycle: crate::LifecycleHandle,
        metadata: GenerationRetirementReplyMetadata,
    ) -> Result<GenerationRetirementResult, GenerationRetirementRequestError> {
        let (sender, mut receiver) = oneshot::channel();
        let message = RunControlMessage::BeginGenerationRetirement {
            preparation,
            inventory: GenerationRetirementInventory::Resident(inventory),
            diagnostic: GenerationRetirementDiagnostic::operation(diagnostic),
            reply: GenerationRetirementBeginReply::Reload(GenerationRetirementReloadReply {
                sender,
                metadata: metadata.clone(),
                force,
                publication_owner: GenerationRetirementPublicationOwner::Pending {
                    lifecycle,
                    control,
                },
            }),
        };
        match self.owner.send_message(message) {
            Ok(()) => {}
            Err(ractor::MessagingErr::SendErr(message)) => {
                // This returns the SAME entire unreserved message. Releasing it
                // cannot reserve, publish, fence, or cancel either generation.
                drop(message);
                return Err(GenerationRetirementRequestError::NotEnqueued);
            }
            Err(_) => {
                return Ok(metadata.unavailable(
                    "the transport returned no actual unreserved payload; admission is unobserved",
                ));
            }
        }
        tokio::select! {
            biased;
            reply = &mut receiver => match reply {
                Ok(reply) => reply,
                Err(_) => Ok(self.native_cleanup.retirement_reply(&metadata).unwrap_or_else(|| metadata.unavailable("the accepted retirement reply was lost without a matching terminal observation"))),
            },
            _ = self.native_cleanup.wait_for_terminal() => {
                // The normal final reply may race with successful finalization.
                // Terminal observation never substitutes a fabricated success.
                if let Ok(reply) = receiver.try_recv() { return reply; }
                Ok(self.native_cleanup.retirement_reply(&metadata).unwrap_or_else(|| metadata.unavailable("the terminal owner has no available final receipt for the accepted retirement")))
            }
        }
    }
}

impl RunControlNativeCleanup {
    fn retirement_reply(
        &self,
        metadata: &GenerationRetirementReplyMetadata,
    ) -> Option<GenerationRetirementResult> {
        let recovery = match self.take_recovery() {
            Ok(recovery) => recovery,
            Err(NativeRecoveryUnavailable::Busy) => {
                return Some(metadata.unavailable(
                    "the terminal retirement parcel has one active recovery consumer",
                ));
            }
            Err(
                NativeRecoveryUnavailable::OwnerNotObserved | NativeRecoveryUnavailable::Finalized,
            ) => return None,
        };
        // The existing affine checkout releases the terminal lock before any
        // projection enters source owners or formats a retained cause. Its Drop
        // returns the SAME parcel even if projection panics; no recovery rights
        // or resource authority escape through this data-only method.
        let result = recovery
            .custody
            .as_ref()
            .and_then(|custody| {
                custody
                    .attempts
                    .iter()
                    .find(|core| core.matches_candidate(&metadata.candidate))
            })
            .map(|core| core.reply(metadata, &GenerationRetirementReplyOwner::Native(&recovery)))
            .unwrap_or_else(|| {
                metadata.unavailable("the terminal owner has no matching admitted retirement core")
            });
        drop(recovery);
        Some(result)
    }
}

impl GenerationRetirementReplyMetadata {
    fn receipt(
        &self,
        state: Option<tidepool_bridge_effects::RlRetirementState>,
        blockers: Vec<tidepool_bridge_effects::RlBlocker>,
        primary: Option<tidepool_bridge_effects::RlCause>,
        secondary: Vec<tidepool_bridge_effects::RlCause>,
    ) -> Arc<tidepool_bridge_effects::RlRetirementReceipt> {
        Arc::new(tidepool_bridge_effects::RlRetirementReceipt {
            previous: self.report.previous.clone(),
            candidate: self.report.candidate.clone(),
            operation: Some(self.operation.clone()),
            state,
            blockers,
            outcomes: Vec::new(),
            surface: self.report.surface.clone(),
            primary,
            secondary,
            settlement: tidepool_bridge_effects::RlOperationalSettlement::ReloadOperationalUnsettled(
                tidepool_bridge_effects::RlRetirementObligation::ReloadCandidateSettlementObligation(
                    tidepool_bridge_effects::RlObligationObservation::ReloadObligationUnconfirmed,
                ),
                Vec::new(),
            ),
        })
    }

    fn unavailable(&self, detail: &str) -> GenerationRetirementResult {
        use tidepool_bridge_effects::{RlCause, RlCauseKind};
        self.receipt(
            None,
            Vec::new(),
            Some(RlCause {
                kind: RlCauseKind::ReloadAcknowledgementUnconfirmed,
                detail: detail.into(),
            }),
            Vec::new(),
        )
    }
}

enum GenerationRetirementReplyOwner<'a, 'terminal> {
    Live(&'a RunState),
    Native(&'a RunControlNativeRecovery<'terminal>),
}

impl GenerationRetirementReplyOwner<'_, '_> {
    fn physical_obligation(
        &self,
        capture: &GenerationRetirementCapture,
    ) -> Option<tidepool_bridge_effects::RlObligationObservation> {
        use tidepool_bridge_effects::RlObligationObservation as Observation;
        match self {
            Self::Live(state) => (!state.capture_has_canonical_physical_owners(capture))
                .then_some(Observation::ReloadObligationUnconfirmed),
            Self::Native(recovery) => {
                if recovery.custody.is_none() {
                    return Some(Observation::ReloadObligationUnconfirmed);
                }
                // Callback and native-preparation outcomes currently live in
                // their canonical owner, not these capture references. After
                // owner loss this product retains the route but cannot infer
                // whether cleanup or transfer already settled.
                if !capture.resources.is_empty() || !capture.preparations.is_empty() {
                    return Some(Observation::ReloadObligationUnconfirmed);
                }
                if capture.reservations.iter().any(|terminal| {
                    !terminal
                        .cleanup()
                        .is_some_and(|cleanup| cleanup.is_confirmed())
                }) || capture.actors.iter().any(|actor| {
                    actor.address().get_status() != ractor::ActorStatus::Stopped
                        || !actor.terminal().cleanup().is_some_and(|cleanup| {
                            cleanup.actor() == actor.identity() && cleanup.is_confirmed()
                        })
                }) {
                    return Some(Observation::ReloadObligationUnconfirmed);
                }
                // An unavailable census never certifies absent physical work.
                // The complete owner-issued capture, rather than an empty
                // callback vector, is the scope of this observation.
                if matches!(capture.work, GenerationRetirementWork::Unavailable)
                    || !capture.population.unclassified.is_empty()
                    || capture.population.collateral.iter().any(|edge| {
                        matches!(
                            edge.affected,
                            GenerationCollateralIdentity::UnclassifiedPhysical(_)
                        )
                    })
                {
                    Some(Observation::ReloadObligationUnconfirmed)
                } else {
                    None
                }
            }
        }
    }
}

impl GenerationRetirementCore {
    fn matches_candidate(&self, candidate: &ActorGenerationCandidateObserver) -> bool {
        match self {
            Self::Idle | Self::InitialCompleted { .. } => false,
            Self::InitialRefused { .. } => false,
            Self::Initial {
                candidate: actual, ..
            } => candidate.same_candidate(actual),
            Self::Replacement(attempt) => candidate.same_candidate(&attempt.candidate),
            Self::SourcesSettled {
                candidate: actual, ..
            }
            | Self::Completed {
                candidate: actual, ..
            } => candidate.same_observer(actual),
        }
    }

    /// Data projection only; it never settles a source, takes a worker result,
    /// resumes a callback, or supplies authority through an observer.
    fn reply(
        &self,
        metadata: &GenerationRetirementReplyMetadata,
        owner: &GenerationRetirementReplyOwner<'_, '_>,
    ) -> GenerationRetirementResult {
        let mut receipt = self.source_reply(metadata);
        let (obligations, secondary) = self.operational_obligations(owner);
        let mut obligations = obligations.into_iter();
        let settlement = match obligations.next() {
            None => tidepool_bridge_effects::RlOperationalSettlement::ReloadOperationalSettled,
            Some(first) => {
                tidepool_bridge_effects::RlOperationalSettlement::ReloadOperationalUnsettled(
                    first,
                    obligations.collect(),
                )
            }
        };
        if receipt.settlement != settlement || !secondary.is_empty() {
            // Data-only projection: shared progress/journal evidence remains
            // immutable. Reference counts never decide lifecycle authority.
            let receipt = Arc::make_mut(&mut receipt);
            receipt.settlement = settlement;
            receipt.secondary.extend(secondary);
        }
        receipt
    }

    fn operational_obligations(
        &self,
        owner: &GenerationRetirementReplyOwner<'_, '_>,
    ) -> (
        Vec<tidepool_bridge_effects::RlRetirementObligation>,
        Vec<tidepool_bridge_effects::RlCause>,
    ) {
        use tidepool_bridge_effects::{
            RlCause, RlCauseKind, RlObligationObservation as Observation,
            RlRetirementObligation as Obligation,
        };
        let mut obligations = Vec::new();
        let mut secondary = Vec::new();
        let mut capture = None;
        match self {
            Self::Initial { .. }
            | Self::InitialCompleted { .. }
            | Self::InitialRefused { .. }
            | Self::Idle => obligations.push(Obligation::ReloadCandidateSettlementObligation(
                Observation::ReloadObligationUnconfirmed,
            )),
            Self::Completed {
                completion: GenerationRetirementCompletion::UnreservedRefused,
                ..
            } => obligations.push(Obligation::ReloadCandidateSettlementObligation(
                Observation::ReloadObligationUnconfirmed,
            )),
            Self::Completed { .. } => {}
            Self::SourcesSettled { cutoff, .. } => {
                capture = cutoff.capture();
            }
            Self::Replacement(attempt) => {
                let (cutoff, sources_restored, evidence_required) = match &attempt.phase {
                    GenerationRetirementPhase::Preparation(_) => {
                        obligations.push(Obligation::ReloadCandidateSettlementObligation(
                            Observation::ReloadObligationPending,
                        ));
                        (None, false, false)
                    }
                    GenerationRetirementPhase::Reserving => {
                        obligations.push(Obligation::ReloadCandidateSettlementObligation(
                            Observation::ReloadObligationUnconfirmed,
                        ));
                        (None, false, true)
                    }
                    GenerationRetirementPhase::ReservationFailed(_) => {
                        obligations.push(Obligation::ReloadCandidateSettlementObligation(
                            Observation::ReloadObligationUnconfirmed,
                        ));
                        (None, false, true)
                    }
                    GenerationRetirementPhase::ReservationMismatch(_) => {
                        obligations.push(Obligation::ReloadCandidateSettlementObligation(
                            Observation::ReloadObligationRefused,
                        ));
                        (None, false, true)
                    }
                    GenerationRetirementPhase::Unstarted {
                        publication,
                        cutoff,
                        progress,
                        ..
                    } => {
                        obligations.push(Obligation::ReloadCandidateSettlementObligation(
                            if matches!(publication, GenerationUnstartedPublication::Consuming)
                                || matches!(
                                    progress,
                                    GenerationUnstartedProgress::SourceSettlementInProgress
                                        | GenerationUnstartedProgress::SettlementFailed { .. }
                                )
                            {
                                Observation::ReloadObligationUnconfirmed
                            } else {
                                Observation::ReloadObligationPending
                            },
                        ));
                        let restored = matches!(
                            progress,
                            GenerationUnstartedProgress::SourcesRestored { .. }
                                | GenerationUnstartedProgress::SettlingUnstarted { .. }
                        );
                        let evidence_required = !matches!(
                            progress,
                            GenerationUnstartedProgress::AwaitingDecision
                                | GenerationUnstartedProgress::ReadyForPublication
                        );
                        (Some(cutoff), restored, evidence_required)
                    }
                    GenerationRetirementPhase::Entered {
                        cutoff,
                        custody,
                        owner,
                        ..
                    } => {
                        if let Some(error) = owner.settlement_error() {
                            secondary.push(RlCause {
                                kind: RlCauseKind::ReloadCandidateSettlementFailed,
                                detail: error.to_string(),
                            });
                        }
                        let candidate = match custody {
                            GenerationPublicationCustody::Settled { outcome, .. }
                                if matches!(
                                    outcome.as_ref(),
                                    Ok(ActorGenerationCommit::Published { .. })
                                ) =>
                            {
                                None
                            }
                            GenerationPublicationCustody::Waiting { .. }
                            | GenerationPublicationCustody::NotStarted { .. }
                            | GenerationPublicationCustody::Received => {
                                Some(match owner.settlement() {
                                    PublicationOwnerSettlement::Failed(_) => {
                                        Observation::ReloadObligationRefused
                                    }
                                    PublicationOwnerSettlement::Confirming
                                    | PublicationOwnerSettlement::Restoring => {
                                        Observation::ReloadObligationUnconfirmed
                                    }
                                    _ => Observation::ReloadObligationPending,
                                })
                            }
                            // The same full body/refusal remains retained until
                            // its positive owning settlement gate consumes it.
                            _ => Some(Observation::ReloadObligationUnconfirmed),
                        };
                        if let Some(candidate) = candidate {
                            obligations
                                .push(Obligation::ReloadCandidateSettlementObligation(candidate));
                        }
                        if custody.published()
                            || matches!(
                                custody,
                                GenerationPublicationCustody::ProducerLostAfterExecute
                            )
                            || matches!(custody, GenerationPublicationCustody::DeliveryLost { .. })
                        {
                            let notification = match custody.notification() {
                                preparation::PublicationNotificationObservation::Accepted => None,
                                preparation::PublicationNotificationObservation::Refused => {
                                    secondary.push(RlCause {
                                        kind: RlCauseKind::ReloadAcknowledgementUnconfirmed,
                                        detail: "the existing lifecycle owner refused the published generation notification".into(),
                                    });
                                    Some(Observation::ReloadObligationRefused)
                                }
                                preparation::PublicationNotificationObservation::NotRequired
                                | preparation::PublicationNotificationObservation::Unobserved => {
                                    Some(Observation::ReloadObligationUnconfirmed)
                                }
                            };
                            if let Some(notification) = notification {
                                obligations.push(
                                    Obligation::ReloadPublicationNotificationObligation(
                                        notification,
                                    ),
                                );
                            }
                        }
                        let evidence_required = match custody {
                            GenerationPublicationCustody::Waiting { .. } => false,
                            GenerationPublicationCustody::Settled { outcome, .. } => !matches!(
                                outcome.as_ref(),
                                Ok(ActorGenerationCommit::Published { .. })
                            ),
                            _ => true,
                        };
                        (
                            Some(cutoff),
                            matches!(
                                custody,
                                GenerationPublicationCustody::SourceRestoredPending { .. }
                            ),
                            evidence_required,
                        )
                    }
                };
                if let Some(cutoff) = cutoff {
                    capture = cutoff.capture();
                    if !sources_restored {
                        let original = match cutoff {
                            GenerationRetirementCutoffPhase::NotStarted => None,
                            GenerationRetirementCutoffPhase::Retired { .. } => None,
                            GenerationRetirementCutoffPhase::AwaitCapture { .. }
                            | GenerationRetirementCutoffPhase::Captured { .. }
                            | GenerationRetirementCutoffPhase::RetirementInProgress { .. } => {
                                Some(Observation::ReloadObligationPending)
                            }
                            GenerationRetirementCutoffPhase::Unconfirmed { .. }
                            | GenerationRetirementCutoffPhase::Refused(_)
                            | GenerationRetirementCutoffPhase::RetirementFailed { .. } => {
                                Some(Observation::ReloadObligationUnconfirmed)
                            }
                        };
                        if let Some(original) = original {
                            obligations
                                .push(Obligation::ReloadOriginalRetirementObligation(original));
                        }
                    }
                }
                if let Some(observation) = attempt.diagnostic.outstanding(evidence_required) {
                    obligations.push(Obligation::ReloadDiagnosticDeliveryObligation(observation));
                }
            }
        }
        if let Some(capture) = capture {
            if let Some(observation) = owner.physical_obligation(capture) {
                obligations.push(Obligation::ReloadPhysicalCleanupObligation(observation));
            }
            if let GenerationSourceRestoration::Started(redispatches) = &capture.source_restoration
            {
                use crate::resident_actor::source_slots::SourceRedispatchOutcome;
                for redispatch in redispatches {
                    let (identity, observation, detail) = match redispatch {
                        GenerationSourceRedispatch::InProgress(identity) => {
                            (identity, Observation::ReloadObligationUnconfirmed, None)
                        }
                        GenerationSourceRedispatch::Observed(
                            SourceRedispatchOutcome::Refused {
                                identity, failure, ..
                            },
                        ) => (
                            identity,
                            Observation::ReloadObligationRefused,
                            Some(failure.to_string()),
                        ),
                        GenerationSourceRedispatch::Observed(
                            SourceRedispatchOutcome::NotAttempted
                            | SourceRedispatchOutcome::Accepted { .. },
                        ) => continue,
                    };
                    match retirement_actor_identity(identity.actor) {
                        Ok(actor) => obligations.push(Obligation::ReloadSourceDeliveryObligation(
                            tidepool_bridge_effects::RlResource::ReloadSourceBindingResource(
                                actor,
                                identity.slot.to_string(),
                            ),
                            observation,
                        )),
                        Err(_) => obligations.push(Obligation::ReloadDiagnosticDeliveryObligation(
                            Observation::ReloadObligationUnconfirmed,
                        )),
                    }
                    if let Some(detail) = detail {
                        secondary.push(RlCause {
                            kind: RlCauseKind::ReloadAcknowledgementUnconfirmed,
                            detail,
                        });
                    }
                }
            }
        }
        (obligations, secondary)
    }

    fn source_reply(
        &self,
        metadata: &GenerationRetirementReplyMetadata,
    ) -> GenerationRetirementResult {
        use tidepool_bridge_effects::{
            RlCause, RlCauseKind as Cause, RlRetirementStage as Stage, RlRetirementState as State,
        };
        let cause = |kind, detail: String| RlCause { kind, detail };
        let completion = match self {
            Self::Completed { completion, .. } | Self::SourcesSettled { completion, .. } => {
                Some(completion)
            }
            _ => None,
        };
        if let Some(completion) = completion {
            let receipt = match completion {
                GenerationRetirementCompletion::DefaultRefused { receipt, .. } => receipt.clone(),
                GenerationRetirementCompletion::BPublished { .. } => metadata.receipt(
                    Some(State::ReloadBPublished(metadata.report.changed_modules.clone(), Vec::new())),
                    Vec::new(),
                    None,
                    Vec::new(),
                ),
                GenerationRetirementCompletion::AReopened {
                    outcome,
                    nonstart,
                    diagnostic_refusals,
                    diagnostic,
                } => metadata.receipt(
                    Some(State::ReloadAReopened),
                    Vec::new(),
                    Some(cause(
                        Cause::ReloadPublicationFailed,
                        diagnostic.as_ref().map_or_else(
                            || format!("candidate publication was refused: {outcome:?}; nonstart={nonstart:?}"),
                            |recorded| recorded.detail().to_owned(),
                        ),
                    )),
                    diagnostic_refusals
                        .iter()
                        .map(|refusal| {
                            cause(Cause::ReloadAcknowledgementUnconfirmed, refusal.to_string())
                        })
                        .collect(),
                ),
                GenerationRetirementCompletion::UnreservedRefused => {
                    return metadata.unavailable(
                        "reservation was refused without an original-source settlement proof",
                    )
                }
            };
            return receipt;
        }
        let Self::Replacement(attempt) = self else {
            return metadata.unavailable("the exact admitted attempt is unavailable");
        };
        let (cutoff, custody) = match &attempt.phase {
            GenerationRetirementPhase::Unstarted {
                cutoff, progress, ..
            } => {
                match progress {
                    GenerationUnstartedProgress::SourcesRestored { receipt, .. }
                    | GenerationUnstartedProgress::SettlingUnstarted { receipt, .. } => {
                        let mut pending = receipt.as_ref().clone();
                        pending.secondary.push(cause(Cause::ReloadAcknowledgementUnconfirmed, "original source is restored; operation evidence or reservation settlement remains pending".into()));
                        return Arc::new(pending);
                    }
                    GenerationUnstartedProgress::SettlementFailed { receipt, error } => {
                        let mut failed = receipt.as_ref().clone();
                        failed
                            .secondary
                            .push(cause(Cause::ReloadOriginalReopenFailed, error.to_string()));
                        return Arc::new(failed);
                    }
                    _ => {}
                }
                (Some(cutoff), None)
            }
            GenerationRetirementPhase::Entered {
                cutoff, custody, ..
            } => (Some(cutoff), Some(custody)),
            GenerationRetirementPhase::ReservationFailed(error) => {
                return metadata.receipt(
                    None,
                    Vec::new(),
                    Some(cause(
                        Cause::ReloadCandidateSettlementFailed,
                        error.to_string(),
                    )),
                    Vec::new(),
                );
            }
            _ => (None, None),
        };
        if let Some(GenerationPublicationCustody::SourceRestoredPending { .. }) = custody {
            return metadata.receipt(
                Some(State::ReloadAReopened), Vec::new(), None,
                vec![cause(Cause::ReloadAcknowledgementUnconfirmed, "original source is restored; actual publication obligations and diagnostic evidence remain retained".into())],
            );
        }
        let capture = match cutoff {
            Some(
                GenerationRetirementCutoffPhase::Captured { capture, .. }
                | GenerationRetirementCutoffPhase::RetirementInProgress { capture }
                | GenerationRetirementCutoffPhase::RetirementFailed { capture, .. }
                | GenerationRetirementCutoffPhase::Retired { capture },
            ) => Some(capture),
            _ => None,
        };
        let mut secondary = Vec::new();
        let blockers = match capture {
            Some(capture) => {
                if let Err(error) = &capture.pending_claims {
                    secondary.push(cause(
                        Cause::ReloadAcknowledgementUnconfirmed,
                        error.to_string(),
                    ));
                }
                let mut blockers = Vec::new();
                if let Err(refusal) = capture.append_blockers(&mut blockers) {
                    secondary.push(cause(
                        Cause::ReloadAcknowledgementUnconfirmed,
                        format!("retirement coordinate projection refused: {refusal:?}"),
                    ));
                }
                blockers
            }
            None => Vec::new(),
        };
        if let Some(GenerationRetirementCutoffPhase::RetirementFailed { error, .. }) = cutoff {
            secondary.push(cause(
                Cause::ReloadAcknowledgementUnconfirmed,
                error.to_string(),
            ));
        }
        let primary = match custody {
            Some(GenerationPublicationCustody::Settled { outcome, .. }) => match outcome.as_ref() {
                Ok(ActorGenerationCommit::Published { .. }) => cause(
                    Cause::ReloadAcknowledgementUnconfirmed,
                    "candidate B is published; original-source retirement remains unresolved"
                        .into(),
                ),
                Ok(
                    ActorGenerationCommit::Refused(issue)
                    | ActorGenerationCommit::Unconfirmed(issue),
                ) => cause(Cause::ReloadPublicationFailed, issue.to_string()),
                Err(error) => cause(Cause::ReloadPublicationFailed, error.to_string()),
            },
            _ => cause(
                Cause::ReloadAcknowledgementUnconfirmed,
                "the actual retirement owner retains unresolved source or publication settlement"
                    .into(),
            ),
        };
        let state = if custody.is_some_and(GenerationPublicationCustody::published) {
            Some(State::ReloadBPublished(
                metadata.report.changed_modules.clone(),
                Vec::new(),
            ))
        } else {
            cutoff.and_then(|cutoff| {
                let fence = match cutoff {
                    GenerationRetirementCutoffPhase::AwaitCapture { fence, .. }
                    | GenerationRetirementCutoffPhase::Captured { fence, .. }
                    | GenerationRetirementCutoffPhase::Unconfirmed { fence, .. }
                    | GenerationRetirementCutoffPhase::RetirementFailed { fence, .. } => fence,
                    GenerationRetirementCutoffPhase::NotStarted
                    | GenerationRetirementCutoffPhase::Refused(_)
                    | GenerationRetirementCutoffPhase::RetirementInProgress { .. }
                    | GenerationRetirementCutoffPhase::Retired { .. } => return None,
                };
                fence
                    .pending_claims()
                    .ok()
                    .map(|_| State::ReloadRetirementFenced(Stage::ReloadSourcePublication))
            })
        };
        metadata.receipt(state, blockers, Some(primary), secondary)
    }
}

impl GenerationRetirementRecord {
    pub(super) fn idle() -> Self {
        Self {
            core: GenerationRetirementCore::Idle,
            driver: GenerationRetirementDriver::empty(),
        }
    }

    pub(super) fn completed(&self) -> Option<&GenerationRetirementCompletion> {
        match &self.core {
            GenerationRetirementCore::Completed { completion, .. } => Some(completion),
            _ => None,
        }
    }

    pub(super) fn is_active(&self) -> bool {
        matches!(
            self.core,
            GenerationRetirementCore::Initial { .. }
                | GenerationRetirementCore::Replacement(_)
                | GenerationRetirementCore::SourcesSettled { .. }
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
            Self::Replacement(attempt) => Some(attempt),
            _ => None,
        }
    }

    fn active_mut(&mut self) -> Option<&mut GenerationRetirementAttempt> {
        match self {
            Self::Replacement(attempt) => Some(attempt),
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
    report: Option<Arc<crate::resident_workbench::PreparedReloadReport>>,
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
        owner: ActorGenerationPublicationOwner,
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
    ReadyForPublication,
    DispatchRefused(GenerationPublicationDispatchFailure),
    ProjectionRefused(GenerationRetirementProjectionRefusal),
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
    DeliveryLost {
        terminal: Arc<std::sync::OnceLock<preparation::PublicationTerminalObservation>>,
    },
    RejectedDelivery(preparation::PublicationDelivery),
    Received,
    NotStarted {
        reason: preparation::PublicationNonStart,
    },
    SourceRestoredPending {
        nonstart: Option<preparation::PublicationNonStart>,
    },
    Settled {
        outcome: Arc<Result<ActorGenerationCommit, ResidentActorWorkbenchError>>,
        notification: preparation::PublicationNotificationObservation,
    },
    ProducerLostAfterExecute,
}

enum GenerationPublicationAuthorization {
    EmptyCapture,
    #[cfg(test)]
    CancelledComponent,
    #[cfg(test)]
    DeliveryLossComponent {
        lifecycle: crate::LifecycleHandle,
    },
}

#[derive(Debug)]
pub(super) enum GenerationPublicationDispatchFailure {
    Unavailable,
    AttemptChanged,
    NotCaptured,
    NotCancelled,
    #[cfg(test)]
    NotRunning,
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
    RetirementInProgress {
        capture: GenerationRetirementCapture,
    },
    RetirementFailed {
        fence: GenerationRetirementFence,
        capture: GenerationRetirementCapture,
        error: Arc<GenerationRetirementFenceError>,
    },
    Retired {
        capture: GenerationRetirementCapture,
    },
}

impl GenerationRetirementCutoffPhase {
    fn capture(&self) -> Option<&GenerationRetirementCapture> {
        match self {
            Self::Captured { capture, .. }
            | Self::RetirementInProgress { capture }
            | Self::RetirementFailed { capture, .. }
            | Self::Retired { capture } => Some(capture),
            _ => None,
        }
    }
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
    source_restoration: GenerationSourceRestoration,
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
    WorkUnavailable,
    FrozenReloadReportMissing,
    OperationMissing,
}

/// Only redispatch initiated by this retirement is its delivery obligation.
/// Canonical service queues and their historical failures remain independent.
enum GenerationSourceRestoration {
    NotStarted,
    Started(Vec<GenerationSourceRedispatch>),
}

enum GenerationSourceRedispatch {
    InProgress(crate::resident_actor::source_slots::SourceRegistrationIdentity),
    Observed(crate::resident_actor::source_slots::SourceRedispatchOutcome),
}

impl GenerationRetirementCapture {
    /// Called only after this attempt has restored A. Record each selected
    /// slot before dispatch so loss of the synchronous result stays unknown.
    /// Neither uncertain nor refused dispatches are retried by reconciliation.
    fn resume_deferred_sources(&mut self) {
        use crate::resident_actor::source_slots::SourceRegistrationDependency;
        if !matches!(
            self.source_restoration,
            GenerationSourceRestoration::NotStarted
        ) {
            return;
        }
        self.source_restoration = GenerationSourceRestoration::Started(Vec::new());
        let GenerationSourceRestoration::Started(redispatches) = &mut self.source_restoration
        else {
            return;
        };
        let GenerationRetirementWork::Observed(work) = &self.work else {
            return;
        };
        for registration in &work.source_registrations {
            let observed = registration.observation();
            if observed.dependency != SourceRegistrationDependency::Dependent {
                continue;
            }
            redispatches.push(GenerationSourceRedispatch::InProgress(observed.identity));
            let outcome = registration.resume_deferred();
            if let Some(redispatch) = redispatches.last_mut() {
                *redispatch = GenerationSourceRedispatch::Observed(outcome);
            }
        }
    }

    fn has_unsettled_source_redispatch(&self) -> bool {
        use crate::resident_actor::source_slots::SourceRedispatchOutcome;
        match &self.source_restoration {
            GenerationSourceRestoration::NotStarted => false,
            GenerationSourceRestoration::Started(redispatches) => redispatches.iter().any(|item| {
                matches!(
                    item,
                    GenerationSourceRedispatch::InProgress(_)
                        | GenerationSourceRedispatch::Observed(
                            SourceRedispatchOutcome::Refused { .. }
                        )
                )
            }),
        }
    }

    fn blockers(
        &self,
    ) -> Result<Vec<tidepool_bridge_effects::RlBlocker>, GenerationRetirementProjectionRefusal>
    {
        let mut blockers = Vec::new();
        self.append_blockers(&mut blockers)?;
        Ok(blockers)
    }

    fn append_blockers(
        &self,
        blockers: &mut Vec<tidepool_bridge_effects::RlBlocker>,
    ) -> Result<(), GenerationRetirementProjectionRefusal> {
        use tidepool_bridge_effects::{
            RlBlocker as Blocker, RlBlockerReason as Reason, RlResource as Resource,
        };
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
        self.work.append_blockers(blockers)?;
        match self.pending_claims {
            Ok(0) => {}
            Ok(count) => blockers.push(Blocker::ReloadUnclassifiedSourceWork(
                i64::try_from(count)
                    .map_err(|_| GenerationRetirementProjectionRefusal::PendingClaims)?,
            )),
            Err(_) => return Err(GenerationRetirementProjectionRefusal::PendingClaims),
        }
        Ok(())
    }
}

impl GenerationRetirementWork {
    fn append_blockers(
        &self,
        blockers: &mut Vec<tidepool_bridge_effects::RlBlocker>,
    ) -> Result<(), GenerationRetirementProjectionRefusal> {
        use crate::resident_actor::invocation_work::GenerationInvocationState as Invocation;
        use tidepool_bridge_effects::{
            RlBlocker as Blocker, RlBlockerReason as Reason, RlResource as Resource,
        };
        match self {
            Self::Unavailable => {
                return Err(GenerationRetirementProjectionRefusal::WorkUnavailable)
            }
            Self::Observed(work) => {
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
                for registration in &work.source_registrations {
                    use crate::resident_actor::source_slots::{
                        SourceRegistrationDependency as Dependency,
                        SourceRegistrationState as State,
                    };
                    let observed = registration.observation();
                    if matches!(observed.state, State::Closed { .. }) {
                        continue;
                    }
                    let reason = match observed.dependency {
                        Dependency::Independent => continue,
                        Dependency::Dependent => Reason::ReloadActiveDependentWork,
                        Dependency::Unclassified => Reason::ReloadUnclassifiedWork,
                    };
                    blockers.push(Blocker::ReloadResourceBlocker(
                        Resource::ReloadSourceBindingResource(
                            retirement_actor_identity(observed.identity.actor)?,
                            observed.identity.slot.to_string(),
                        ),
                        reason,
                    ));
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
        Ok(())
    }
}

/// Assertion-only receiving control: all source registrations come from their
/// actual issuing owners. It neither constructs a population nor grants access
/// to any registration, source, or cleanup authority.
#[cfg(test)]
pub(crate) fn assert_source_registration_retirement_projection(
    registrations: Vec<crate::resident_actor::source_slots::CapturedSourceRegistration>,
    expected: &[(ActorRef, usize)],
) {
    use tidepool_bridge_effects::{RlBlocker, RlBlockerReason, RlResource};
    let work =
        GenerationRetirementWork::Observed(crate::resident_actor::GenerationWorkObservations {
            actors: Vec::new(),
            invocations: Vec::new(),
            routes: Vec::new(),
            source_registrations: registrations,
        });
    let mut actual = Vec::new();
    work.append_blockers(&mut actual).unwrap();
    let expected: Vec<_> = expected
        .iter()
        .map(|(actor, slot)| {
            RlBlocker::ReloadResourceBlocker(
                RlResource::ReloadSourceBindingResource(
                    (
                        i64::try_from(actor.id.0).unwrap(),
                        i64::try_from(actor.incarnation.0).unwrap(),
                    ),
                    slot.to_string(),
                ),
                RlBlockerReason::ReloadActiveDependentWork,
            )
        })
        .collect();
    assert_eq!(actual, expected);
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

#[derive(Debug, thiserror::Error)]
pub(crate) enum GenerationRetirementBeginFailure {
    #[error(transparent)]
    Directory(DirectoryError),
    #[error("the exact actor has no registered live owner")]
    Unregistered {
        terminal: Option<ActorTerminal>,
        cleanup: Option<crate::ResidentCleanupOutcome>,
    },
    #[error("the preparation has no original generation")]
    ExpectedGenerationMissing,
    #[error("the reservation driver belongs to a different candidate")]
    CandidateChanged,
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Publication(Arc<ResidentActorWorkbenchError>),
}

impl GenerationPublicationCustody {
    fn published(&self) -> bool {
        matches!(self, Self::Settled { outcome, .. }
            if matches!(outcome.as_ref(), Ok(ActorGenerationCommit::Published { .. })))
    }

    fn notification(&self) -> preparation::PublicationNotificationObservation {
        match self {
            Self::Settled { notification, .. } => *notification,
            _ => preparation::PublicationNotificationObservation::Unobserved,
        }
    }

    fn observe(&mut self, owner: &mut ActorGenerationPublicationOwner) {
        let Self::Waiting { receiver, terminal } = self else {
            return;
        };
        let delivery = match receiver.try_recv() {
            Ok(delivery) => delivery,
            Err(oneshot::error::TryRecvError::Empty) => return,
            Err(oneshot::error::TryRecvError::Closed) => {
                *self = Self::DeliveryLost {
                    terminal: terminal.clone(),
                };
                return;
            }
        };
        // The guard was installed before dispatch. Receipt installation never
        // moves it, and failure retains the exact rejected payload.
        *self = match delivery {
            preparation::PublicationDelivery::NotStarted { body, reason } => {
                match owner.return_unstarted(body) {
                    Ok(()) => Self::NotStarted { reason },
                    Err(body) => {
                        Self::RejectedDelivery(preparation::PublicationDelivery::NotStarted {
                            body,
                            reason,
                        })
                    }
                }
            }
            preparation::PublicationDelivery::Observed { body } => match owner.record_body(body) {
                Ok(()) => Self::Received,
                Err(body) => {
                    Self::RejectedDelivery(preparation::PublicationDelivery::Observed { body })
                }
            },
        };
    }

    fn reconcile(
        &mut self,
        owner: &mut ActorGenerationPublicationOwner,
        cutoff: &mut GenerationRetirementCutoffPhase,
        diagnostic: &mut GenerationRetirementDiagnostic,
    ) -> Option<GenerationRetirementCompletion> {
        let GenerationRetirementCutoffPhase::Captured { fence, capture } = cutoff else {
            return None;
        };
        let completion = self.reconcile_publication_source(owner, fence, diagnostic);
        if owner.source_restored() {
            capture.resume_deferred_sources();
        }
        completion
    }

    /// Reconcile the actual body delivery, source transition and operation
    /// evidence. Physical capture/STOP obligations remain with the outer owner.
    fn reconcile_publication_source(
        &mut self,
        owner: &mut ActorGenerationPublicationOwner,
        fence: &mut GenerationRetirementFence,
        diagnostic: &mut GenerationRetirementDiagnostic,
    ) -> Option<GenerationRetirementCompletion> {
        self.observe(owner);
        if matches!(self, Self::Received) {
            diagnostic.project_publication(owner);
            if matches!(owner.confirm(), PublicationOwnerSettlement::Confirmed) {
                let (generation, cancellation_pending) = owner.confirmed_generation()?;
                *self = Self::Settled {
                    outcome: Arc::new(Ok(ActorGenerationCommit::Published {
                        generation: generation.clone(),
                        cancellation_pending,
                    })),
                    notification: preparation::PublicationNotificationObservation::Unobserved,
                };
            }
        }
        if let Self::NotStarted { reason } = self {
            diagnostic.project_nonstart(reason);
        }
        diagnostic.retry_projected();
        if matches!(self, Self::NotStarted { .. } | Self::Received) {
            if matches!(
                owner.refuse_retirement(fence),
                PublicationOwnerSettlement::Restored
            ) {
                let nonstart = match std::mem::replace(self, Self::ProducerLostAfterExecute) {
                    Self::NotStarted { reason } => Some(reason),
                    Self::Received => None,
                    _ => {
                        unreachable!("the serial source settlement keeps transport state unchanged")
                    }
                };
                *self = Self::SourceRestoredPending { nonstart };
            }
        }
        if !matches!(self, Self::SourceRestoredPending { .. }) {
            return None;
        }
        if !owner.settled_obligations() {
            return None;
        }
        let Self::SourceRestoredPending { nonstart } = self else {
            return None;
        };
        // Spawn failure can retain independent native obligations; source
        // restoration or an acknowledged diagnostic cannot discharge them.
        if matches!(
            nonstart,
            Some(preparation::PublicationNonStart::SpawnFailed(_))
        ) {
            return None;
        }
        let (recorded, refusals) = if diagnostic.is_cancelled_component() {
            (None, Vec::new())
        } else {
            let (recorded, refusals) = diagnostic.recorded()?;
            (Some(recorded), refusals)
        };
        let observation = owner
            .diagnostic_outcome()
            .map(|(_, observation)| observation)
            .unwrap_or(ActorGenerationPublicationRefusalObservation::Refused);
        Some(GenerationRetirementCompletion::AReopened {
            outcome: observation,
            nonstart: nonstart
                .as_ref()
                .map(preparation::PublicationNonStart::kind),
            diagnostic: recorded,
            diagnostic_refusals: refusals,
        })
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
                receipt.state = Some(tidepool_bridge_effects::RlRetirementState::ReloadAReopened);
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
    if let GenerationRetirementCutoffPhase::Captured { capture, .. } = cutoff {
        capture.resume_deferred_sources();
    }
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

fn retire_published_source(
    cutoff: &mut GenerationRetirementCutoffPhase,
    successor: tidepool_runtime::session::NotebookGenerationId,
) -> bool {
    if matches!(cutoff, GenerationRetirementCutoffPhase::Retired { .. }) {
        return true;
    }
    if !matches!(cutoff, GenerationRetirementCutoffPhase::Captured { .. }) {
        return false;
    }
    let GenerationRetirementCutoffPhase::Captured { fence, capture } =
        std::mem::replace(cutoff, GenerationRetirementCutoffPhase::NotStarted)
    else {
        unreachable!()
    };
    *cutoff = GenerationRetirementCutoffPhase::RetirementInProgress { capture };
    let result = fence.retire(successor);
    let GenerationRetirementCutoffPhase::RetirementInProgress { capture } =
        std::mem::replace(cutoff, GenerationRetirementCutoffPhase::NotStarted)
    else {
        unreachable!()
    };
    match result {
        Ok(()) => {
            *cutoff = GenerationRetirementCutoffPhase::Retired { capture };
            true
        }
        Err((fence, error)) => {
            *cutoff = GenerationRetirementCutoffPhase::RetirementFailed {
                fence,
                capture,
                error: Arc::new(error),
            };
            false
        }
    }
}

fn initial_publication_failure(
    owner: &ActorGenerationPublicationOwner,
    custody: &GenerationPublicationCustody,
) -> (bool, InitialGenerationPublicationError) {
    let independent_nonstart = matches!(
        custody,
        GenerationPublicationCustody::NotStarted {
            reason: preparation::PublicationNonStart::SpawnFailed(_)
        }
    );
    let discharge = owner.settled_obligations() && !independent_nonstart;
    let error = if let Some(error) = owner.settlement_error() {
        InitialGenerationPublicationError::Settlement(error.clone())
    } else if let Some(outcome) = owner.failure_outcome() {
        if discharge {
            InitialGenerationPublicationError::Body(outcome.clone())
        } else {
            InitialGenerationPublicationError::UnconfirmedBody(outcome.clone())
        }
    } else if let GenerationPublicationCustody::NotStarted { reason } = custody {
        InitialGenerationPublicationError::NotStarted(reason.kind())
    } else {
        InitialGenerationPublicationError::Unconfirmed
    };
    (discharge, error)
}

impl GenerationRetirementRecord {
    fn reconcile_live(&mut self) {
        self.core.reconcile();
        if let GenerationRetirementCore::Initial {
            actor,
            candidate,
            phase: InitialPublicationPhase::Publishing { owner, custody, .. },
            ..
        } = &mut self.core
        {
            let Some((generation, _)) = owner.confirmed_generation() else {
                if matches!(custody, GenerationPublicationCustody::Waiting { .. }) {
                    return;
                }
                let Some(reply) = self.driver.initial_reply.take() else {
                    return;
                };
                let (discharge, error) = initial_publication_failure(owner, custody);
                if discharge {
                    let actor = *actor;
                    let candidate = candidate.identity_observer();
                    // Actual source restoration and the body's positive
                    // obligation assessment precede release, never reply loss.
                    self.core = GenerationRetirementCore::InitialRefused { actor, candidate };
                }
                let _ = reply.sender.send(Err(error));
                return;
            };
            let Some(reply) = self.driver.initial_reply.take() else {
                return;
            };
            let generation = generation.clone();
            let notification = reply.lifecycle.generation_published(generation.clone());
            if matches!(
                notification,
                crate::kernel::GenerationPublicationNotification::Accepted
            ) {
                let actor = *actor;
                let weak = Arc::downgrade(&generation);
                // Installed generation owns published resources. The record
                // preserves only identity before reply delivery can be lost.
                self.core = GenerationRetirementCore::InitialCompleted {
                    actor,
                    generation: weak,
                };
                let _ = reply.sender.send(Ok(generation));
            } else {
                if let GenerationPublicationCustody::Settled { notification, .. } = custody {
                    *notification = preparation::PublicationNotificationObservation::Refused;
                }
                let _ = reply
                    .sender
                    .send(Err(InitialGenerationPublicationError::NotificationRefused));
            }
            return;
        }
        let GenerationRetirementCore::Replacement(attempt) = &mut self.core else {
            return;
        };
        let GenerationRetirementPhase::Entered {
            custody:
                GenerationPublicationCustody::Settled {
                    outcome,
                    notification,
                },
            ..
        } = &mut attempt.phase
        else {
            return;
        };
        let owner = self.driver.publication_owner.as_mut().or_else(|| {
            self.driver
                .reply
                .as_mut()
                .map(|reply| &mut reply.publication_owner)
        });
        if let Some(owner) = owner {
            *notification = owner.observe(outcome);
        }
        self.core.reconcile();
    }
}

impl GenerationRetirementCore {
    /// Live and terminal owners drive this SAME affine state. The terminal
    /// checkout never holds its slot lock while invoking source or ledger owners.
    fn reconcile(&mut self) {
        if let Self::Initial {
            phase: InitialPublicationPhase::Publishing { owner, custody, .. },
            ..
        } = self
        {
            custody.observe(owner);
            if matches!(custody, GenerationPublicationCustody::Received) {
                if matches!(owner.confirm(), PublicationOwnerSettlement::Confirmed) {
                    if let Some((generation, cancellation_pending)) = owner.confirmed_generation() {
                        *custody = GenerationPublicationCustody::Settled {
                            outcome: Arc::new(Ok(ActorGenerationCommit::Published {
                                generation: generation.clone(),
                                cancellation_pending,
                            })),
                            notification:
                                preparation::PublicationNotificationObservation::Unobserved,
                        };
                    }
                }
            }
            if matches!(
                custody,
                GenerationPublicationCustody::Received
                    | GenerationPublicationCustody::NotStarted { .. }
            ) {
                owner.refuse_initial();
            }
            return;
        }
        let Self::Replacement(attempt) = self else {
            return;
        };
        let completion = match &mut attempt.phase {
            GenerationRetirementPhase::Entered {
                cutoff,
                custody,
                owner,
                ..
            } => custody.reconcile(owner, cutoff, &mut attempt.diagnostic),
            GenerationRetirementPhase::Unstarted {
                publication,
                cutoff,
                progress,
                ..
            } => reconcile_unstarted(publication, cutoff, progress, &mut attempt.diagnostic),
            _ => None,
        };
        let completion = completion.or_else(|| {
            let GenerationRetirementPhase::Entered {
                cutoff,
                custody:
                    GenerationPublicationCustody::Settled {
                        outcome,
                        notification,
                    },
                ..
            } = &mut attempt.phase
            else {
                return None;
            };
            let Ok(ActorGenerationCommit::Published { generation, .. }) = outcome.as_ref() else {
                return None;
            };
            let original_retired = retire_published_source(cutoff, generation.id());
            (original_retired
                && *notification == preparation::PublicationNotificationObservation::Accepted)
                .then(|| GenerationRetirementCompletion::BPublished {
                    generation: generation.id(),
                })
        });
        let Some(completion) = completion else {
            return;
        };
        let candidate_identity = attempt.candidate.identity_observer();
        let old = std::mem::replace(self, Self::Idle);
        let Self::Replacement(GenerationRetirementAttempt {
            actor,
            candidate,
            expected,
            diagnostic,
            report,
            phase,
        }) = old
        else {
            unreachable!()
        };
        match phase {
            GenerationRetirementPhase::Entered {
                observer,
                cutoff,
                owner,
                custody,
            } => {
                *self = Self::SourcesSettled {
                    actor,
                    candidate: candidate_identity,
                    completion,
                    report,
                    cutoff,
                };
                drop((candidate, expected, diagnostic, observer, owner, custody));
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
                    report,
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
                    custody: GenerationPublicationCustody::SourceRestoredPending { .. },
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
    fn notify_generation_retirement(&mut self, actor: ActorRef) {
        let Some(record) = self.reservations.get_mut(&actor) else {
            return;
        };
        let Some(reply) = record.retirement.driver.reply.take() else {
            return;
        };
        let record = self
            .reservations
            .get(&actor)
            .expect("the canonical record remains present");
        let result = record
            .retirement
            .core
            .reply(&reply.metadata, &GenerationRetirementReplyOwner::Live(self));
        let _ = reply.sender.send(Ok(result));
    }

    pub(super) fn receive_initial_generation_publication(
        &mut self,
        myself: &RactorRef<RunControlMessage>,
        preparation: ActorGenerationRetirementPreparation,
        reply: InitialGenerationPublicationReply,
    ) {
        let actor = preparation.actor();
        let refuse = |reply: InitialGenerationPublicationReply, error| {
            let _ = reply.sender.send(Err(error));
        };
        if !preparation.is_initial() {
            refuse(reply, InitialGenerationPublicationError::ExpectedBootstrap);
            return;
        }
        let Some(supervisor) = self.preparation_supervisor.clone() else {
            refuse(reply, InitialGenerationPublicationError::NotEnqueued);
            return;
        };
        if self.phase != RunPhase::Running || reply.lifecycle.identity() != actor {
            refuse(reply, InitialGenerationPublicationError::ActorChanged);
            return;
        }
        let Some(record) = self.reservations.get_mut(&actor) else {
            refuse(
                reply,
                InitialGenerationPublicationError::Admission(
                    GenerationRetirementBeginFailure::Directory(DirectoryError::Unauthorized {
                        actor,
                    }),
                ),
            );
            return;
        };
        if record.retirement.is_active() {
            refuse(
                reply,
                InitialGenerationPublicationError::Admission(
                    GenerationRetirementBeginFailure::Directory(
                        DirectoryError::LifetimeTransitionPending { actor },
                    ),
                ),
            );
            return;
        }
        let candidate = preparation.candidate_custody();
        let (prepared, reserve) = preparation.into_parts();
        record.retirement = GenerationRetirementRecord {
            core: GenerationRetirementCore::Initial {
                actor,
                candidate,
                phase: InitialPublicationPhase::Preparing(prepared),
            },
            driver: GenerationRetirementDriver {
                action: GenerationRetirementAction::Reservation(reserve),
                inventory: GenerationRetirementInventory::Unavailable,
                reply: None,
                publication_owner: None,
                initial_reply: Some(reply),
            },
        };
        let GenerationRetirementCore::Initial { phase, .. } = &mut record.retirement.core else {
            unreachable!()
        };
        let InitialPublicationPhase::Preparing(prepared) =
            std::mem::replace(phase, InitialPublicationPhase::Reserving)
        else {
            unreachable!()
        };
        let GenerationRetirementAction::Reservation(reserve) = std::mem::replace(
            &mut record.retirement.driver.action,
            GenerationRetirementAction::None,
        ) else {
            unreachable!()
        };
        let outcome = match reserve.reserve(prepared) {
            Ok(outcome) => outcome,
            Err((prepared, reserve)) => {
                *phase = InitialPublicationPhase::CandidateChanged(prepared);
                record.retirement.driver.action = GenerationRetirementAction::Reservation(reserve);
                if let Some(reply) = record.retirement.driver.initial_reply.take() {
                    refuse(reply, InitialGenerationPublicationError::Unconfirmed);
                }
                return;
            }
        };
        let ActorGenerationRetirementPreparationOutcome::Reserved {
            reserved,
            publisher,
        } = outcome
        else {
            *phase = InitialPublicationPhase::ReservationFailed(outcome);
            if let Some(reply) = record.retirement.driver.initial_reply.take() {
                refuse(reply, InitialGenerationPublicationError::Unconfirmed);
            }
            return;
        };
        let observer = reserved.attempt_observer();
        let (writer, receiver, terminal) =
            preparation::PublicationTransfer::channel(actor, observer.clone(), myself.clone());
        *phase = InitialPublicationPhase::Publishing {
            observer,
            owner: reserved.into_publication_owner(),
            custody: GenerationPublicationCustody::Waiting { receiver, terminal },
        };
        let InitialPublicationPhase::Publishing { owner, .. } = phase else {
            unreachable!()
        };
        let body = match owner.take_body() {
            Ok(body) => body,
            Err(error) => {
                record.retirement.driver.action =
                    GenerationRetirementAction::Publication(publisher);
                if let Some(reply) = record.retirement.driver.initial_reply.take() {
                    refuse(reply, InitialGenerationPublicationError::Settlement(error));
                }
                return;
            }
        };
        let transfer = preparation::PublicationTransfer::new(body, publisher, writer);
        if let Err(ractor::MessagingErr::SendErr(PreparationMessage::PublishGeneration(transfer))) =
            supervisor.send_message(PreparationMessage::PublishGeneration(transfer))
        {
            transfer.return_unstarted(preparation::PublicationNonStart::SupervisorSendFailed);
        }
    }

    pub(super) fn receive_generation_retirement(
        &mut self,
        myself: &RactorRef<RunControlMessage>,
        preparation: ActorGenerationRetirementPreparation,
        inventory: GenerationRetirementInventory,
        diagnostic: GenerationRetirementDiagnostic,
        reply: GenerationRetirementBeginReply,
    ) {
        match reply {
            #[cfg(test)]
            GenerationRetirementBeginReply::Observe(reply) => {
                let outcome = self.begin_generation_retirement(
                    myself,
                    preparation,
                    inventory,
                    diagnostic,
                    &mut None,
                );
                let _ = reply.send(outcome);
            }
            GenerationRetirementBeginReply::Reload(reply) => {
                let actor = preparation.actor();
                let mut reply = Some(reply);
                let outcome = self.begin_generation_retirement(
                    myself,
                    preparation,
                    inventory,
                    diagnostic,
                    &mut reply,
                );
                if let Some(reply) = reply {
                    let result = match outcome {
                        Err(error) => Err(GenerationRetirementRequestError::Admission(error)),
                        Ok(_) => Ok(reply
                            .metadata
                            .unavailable("the admitted retirement has no final reply owner")),
                    };
                    let _ = reply.sender.send(result);
                } else if outcome.is_err() {
                    self.notify_generation_retirement(actor);
                }
            }
        }
    }

    pub(super) fn begin_generation_retirement(
        &mut self,
        myself: &RactorRef<RunControlMessage>,
        preparation: ActorGenerationRetirementPreparation,
        inventory: GenerationRetirementInventory,
        diagnostic: GenerationRetirementDiagnostic,
        reply: &mut Option<GenerationRetirementReloadReply>,
    ) -> Result<ActorPublicationAttemptObserver, GenerationRetirementBeginFailure> {
        let actor = preparation.actor();
        self.release_live_source_observations(actor);
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
            core: GenerationRetirementCore::Replacement(GenerationRetirementAttempt {
                actor,
                candidate,
                expected,
                diagnostic,
                report: reply.as_ref().map(|reply| reply.metadata.report.clone()),
                phase: GenerationRetirementPhase::Preparation(prepared),
            }),
            driver: GenerationRetirementDriver {
                action: GenerationRetirementAction::Reservation(reserve),
                inventory,
                reply: reply.take(),
                publication_owner: None,
                initial_reply: None,
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
                let frozen_report = attempt.report.clone();
                *reply = record.retirement.driver.reply.take();
                let displaced = std::mem::replace(
                    &mut record.retirement.core,
                    GenerationRetirementCore::Completed {
                        actor,
                        candidate: identity,
                        completion: GenerationRetirementCompletion::UnreservedRefused,
                        report: frozen_report,
                    },
                );
                let driver = std::mem::replace(
                    &mut record.retirement.driver,
                    GenerationRetirementDriver::empty(),
                );
                drop((displaced, driver));
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
        self.dispatch_generation_publication(
            myself,
            actor,
            observer,
            GenerationPublicationAuthorization::CancelledComponent,
        )
    }

    #[cfg(test)]
    pub(super) fn dispatch_delivery_lost_generation_publication(
        &mut self,
        myself: &RactorRef<RunControlMessage>,
        actor: ActorRef,
        observer: ActorPublicationAttemptObserver,
        lifecycle: crate::LifecycleHandle,
    ) -> Result<
        Arc<std::sync::OnceLock<preparation::PublicationTerminalObservation>>,
        GenerationPublicationDispatchFailure,
    > {
        self.dispatch_generation_publication(
            myself,
            actor,
            observer,
            GenerationPublicationAuthorization::DeliveryLossComponent { lifecycle },
        )
    }

    fn dispatch_generation_publication(
        &mut self,
        myself: &RactorRef<RunControlMessage>,
        actor: ActorRef,
        observer: ActorPublicationAttemptObserver,
        authorization: GenerationPublicationAuthorization,
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
        let Some(reserved) = publication.reserved_mut() else {
            return Err(GenerationPublicationDispatchFailure::AttemptChanged);
        };
        #[cfg(test)]
        let lose_delivery = matches!(
            &authorization,
            GenerationPublicationAuthorization::DeliveryLossComponent { .. }
        );
        let publication_owner = match authorization {
            GenerationPublicationAuthorization::EmptyCapture => {
                if !matches!(progress, GenerationUnstartedProgress::ReadyForPublication) {
                    return Err(GenerationPublicationDispatchFailure::NotCaptured);
                }
                let GenerationRetirementCutoffPhase::Captured { capture, fence } = cutoff else {
                    return Err(GenerationPublicationDispatchFailure::NotCaptured);
                };
                if !matches!(capture.blockers(), Ok(blockers) if blockers.is_empty())
                    || !matches!(fence.pending_claims(), Ok(0))
                {
                    return Err(GenerationPublicationDispatchFailure::NotCaptured);
                }
                None
            }
            #[cfg(test)]
            GenerationPublicationAuthorization::CancelledComponent => {
                if !matches!(progress, GenerationUnstartedProgress::AwaitingDecision) {
                    return Err(GenerationPublicationDispatchFailure::AttemptChanged);
                }
                if reserved.publication_phase()
                    != tidepool_runtime::session::PublicationPhase::CancellationRequested
                {
                    return Err(GenerationPublicationDispatchFailure::NotCancelled);
                }
                None
            }
            #[cfg(test)]
            GenerationPublicationAuthorization::DeliveryLossComponent { lifecycle } => {
                if lifecycle.identity() != actor {
                    return Err(GenerationPublicationDispatchFailure::AttemptChanged);
                }
                if !matches!(progress, GenerationUnstartedProgress::AwaitingDecision) {
                    return Err(GenerationPublicationDispatchFailure::AttemptChanged);
                }
                if reserved.publication_phase()
                    != tidepool_runtime::session::PublicationPhase::Running
                {
                    return Err(GenerationPublicationDispatchFailure::NotRunning);
                }
                Some(GenerationRetirementPublicationOwner::Pending {
                    lifecycle,
                    control: None,
                })
            }
        };
        if !matches!(
            record.retirement.driver.action,
            GenerationRetirementAction::Publication(_)
        ) {
            return Err(GenerationPublicationDispatchFailure::AttemptChanged);
        }
        // Allocate delivery while all actual authority is still installed.
        let (writer, receiver, terminal) =
            preparation::PublicationTransfer::channel(actor, observer.clone(), myself.clone());
        #[cfg(test)]
        let writer = if lose_delivery {
            let expected = reserved
                .expected_generation()
                .ok_or(GenerationPublicationDispatchFailure::AttemptChanged)?;
            writer.lose_delivery_after_completion_for_test(expected.scope().scope())
        } else {
            writer
        };
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
            owner: reserved.into_publication_owner(),
            custody: GenerationPublicationCustody::Waiting {
                receiver,
                terminal: terminal.clone(),
            },
        };
        let GenerationRetirementPhase::Entered { owner, .. } = &mut attempt.phase else {
            unreachable!()
        };
        let body = owner
            .take_body()
            .map_err(|_| GenerationPublicationDispatchFailure::AttemptChanged)?;
        let GenerationRetirementAction::Publication(publisher) = std::mem::replace(
            &mut record.retirement.driver.action,
            GenerationRetirementAction::None,
        ) else {
            unreachable!()
        };
        if let Some(owner) = publication_owner {
            record.retirement.driver.publication_owner = Some(owner);
        }
        let transfer = preparation::PublicationTransfer::new(body, publisher, writer);
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
        if let GenerationRetirementCore::Initial {
            phase:
                InitialPublicationPhase::Publishing {
                    observer: issued, ..
                },
            ..
        } = &record.retirement.core
        {
            if !issued.same_attempt(&observer) {
                return;
            }
            record.retirement.reconcile_live();
            return;
        }
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
        record.retirement.reconcile_live();
        self.release_live_source_observations(actor);
        self.notify_generation_retirement(actor);
    }

    pub(super) fn capture_generation_retirement(
        &mut self,
        myself: &RactorRef<RunControlMessage>,
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
            source_restoration: GenerationSourceRestoration::NotStarted,
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
        let observer = self
            .reservations
            .get(&actor)
            .and_then(|record| record.retirement.active())
            .and_then(|attempt| match &attempt.phase {
                GenerationRetirementPhase::Unstarted {
                    observer,
                    progress: GenerationUnstartedProgress::ReadyForPublication,
                    ..
                } => Some(observer.clone()),
                _ => None,
            });
        if let Some(observer) = observer {
            if let Err(error) = self.dispatch_generation_publication(
                myself,
                actor,
                observer,
                GenerationPublicationAuthorization::EmptyCapture,
            ) {
                if let Some(attempt) = self
                    .reservations
                    .get_mut(&actor)
                    .and_then(|record| record.retirement.active_mut())
                {
                    if let GenerationRetirementPhase::Unstarted { progress, .. } =
                        &mut attempt.phase
                    {
                        *progress = GenerationUnstartedProgress::DispatchRefused(error);
                    }
                }
                self.notify_generation_retirement(actor);
            }
        }
    }

    /// This private branch consumes only the actual capture and unexecuted
    /// reservation. It never requests resource stop, cancellation or publication.
    fn prepare_default_generation_refusal(&mut self, actor: ActorRef) {
        use tidepool_bridge_effects::{
            RlCause, RlCauseKind, RlRetirementReceipt, RlRetirementStage, RlRetirementState,
        };
        let force = self
            .reservations
            .get(&actor)
            .and_then(|record| record.retirement.driver.reply.as_ref())
            .is_some_and(|reply| reply.force);
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
                state: Some(RlRetirementState::ReloadRetirementFenced(
                    RlRetirementStage::ReloadCandidateSettlement,
                )),
                blockers: capture.blockers()?,
                outcomes: Vec::new(),
                surface: report.surface.clone(),
                primary: Some(RlCause {
                    kind: if force { RlCauseKind::ReloadStopFailed } else { RlCauseKind::ReloadRetirementBlocked },
                    detail: if force { "forced retirement is not qualified for this captured physical set; no stop was requested" } else { "generation retirement refused because admitted dependent work remains" }.into(),
                }),
                settlement: tidepool_bridge_effects::RlOperationalSettlement::ReloadOperationalUnsettled(
                    tidepool_bridge_effects::RlRetirementObligation::ReloadCandidateSettlementObligation(
                        tidepool_bridge_effects::RlObligationObservation::ReloadObligationPending,
                    ), Vec::new(),
                ),
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
            Ok(receipt) if receipt.blockers.is_empty() => {
                GenerationUnstartedProgress::ReadyForPublication
            }
            Ok(receipt) => GenerationUnstartedProgress::Refusing { receipt },
        };
        if matches!(
            attempt.phase,
            GenerationRetirementPhase::Unstarted {
                progress: GenerationUnstartedProgress::ReadyForPublication,
                ..
            }
        ) {
            return;
        }
        let record = self
            .reservations
            .get_mut(&actor)
            .expect("the exclusive owner retains its reservation");
        record.retirement.reconcile_live();
        self.release_live_source_observations(actor);
        self.notify_generation_retirement(actor);
    }

    /// A zero-stop refusal never moved resources out of these canonical maps.
    /// The live owner may release only its extra capture references while the
    /// actual cleanup owners are still present. Native recovery has no maps and
    /// retains the SAME SourcesSettled capture instead.
    fn release_live_source_observations(&mut self, actor: ActorRef) {
        let Some(record) = self.reservations.get_mut(&actor) else {
            return;
        };
        if !matches!(
            record.retirement.core,
            GenerationRetirementCore::SourcesSettled { .. }
        ) {
            return;
        }
        let mut driver = std::mem::replace(
            &mut record.retirement.driver,
            GenerationRetirementDriver::empty(),
        );
        record.retirement.driver.reply = driver.reply.take();
        // The core and pending final reply remain installed if disposal panics.
        drop(driver);
        let record = self
            .reservations
            .get(&actor)
            .expect("the canonical record remains present");
        let GenerationRetirementCore::SourcesSettled { cutoff, .. } = &record.retirement.core
        else {
            return;
        };
        let (GenerationRetirementCutoffPhase::Captured { capture, .. }
        | GenerationRetirementCutoffPhase::Retired { capture }) = cutoff
        else {
            return;
        };
        if capture.has_unsettled_source_redispatch()
            || !self.capture_has_canonical_physical_owners(capture)
        {
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
            report,
            cutoff,
        } = old
        else {
            unreachable!()
        };
        record.retirement.core = GenerationRetirementCore::Completed {
            actor,
            candidate,
            completion,
            report,
        };
        drop(cutoff);
    }

    fn capture_has_canonical_physical_owners(&self, capture: &GenerationRetirementCapture) -> bool {
        if !capture.resources.iter().all(|captured| {
            self.resources
                .get(&captured.cell.get_id())
                .is_some_and(|canonical| {
                    Arc::ptr_eq(&captured.cleanup, &canonical.resource.cleanup)
                })
        }) {
            return false;
        }
        if !capture.actors.iter().all(|captured| {
            match self.entries.get(&captured.identity()) {
                Some(entry) => entry.actor.address().get_id() == captured.address().get_id(),
                // Forget removes a terminal actor even if its cleanup was
                // unconfirmed. Only this SAME physical owner's complete
                // cleanup proof can replace its canonical live custody.
                None => {
                    captured.address().get_status() == ractor::ActorStatus::Stopped
                        && captured.terminal().cleanup().is_some_and(|cleanup| {
                            cleanup.actor() == captured.identity() && cleanup.is_confirmed()
                        })
                }
            }
        }) {
            return false;
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
            return false;
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
            return false;
        }
        true
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
    ReservationTargetClosed,
    PublicationBodyRefusal,
    WorkerLiveRefusal,
    WorkerNativeLossAfterBody,
    WorkerCapturedSourceRefusalDeliveryLost,
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
