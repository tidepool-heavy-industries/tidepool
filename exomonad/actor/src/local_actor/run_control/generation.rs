//! Generation provenance travels through the existing run-directory owners.
use super::*;
use crate::resident_workbench::generation_admission::{GenerationAdmissionError, GenerationFence};
use crate::ResidentActorWorkbenchError;

use crate::resident_workbench::generation_admission::{
    GenerationRetirementFence, GenerationRetirementFenceError,
};
use crate::resident_workbench::{
    ActorGeneration, ActorGenerationCandidateCustody, ActorGenerationCommit,
    ActorGenerationPublicationDiscard, ActorGenerationPublicationOperation,
    ActorGenerationPublicationRefusal, ActorGenerationPublicationRefusalObservation,
    ActorGenerationPublicationSettlement, ActorGenerationRetirementCutoff,
    ActorGenerationRetirementPreparation, ActorGenerationRetirementPreparationOutcome,
    ActorPublicationAttemptObserver, PreparedActorGeneration, ReservedActorGeneration,
};

pub(super) enum GenerationRetirementRecord {
    Idle,
    Active(GenerationRetirementAttempt),
    Completed(GenerationRetirementCompletion),
}

#[derive(Clone, Debug)]
pub(super) enum GenerationRetirementCompletion {
    UnreservedRefused,
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
    pub(super) fn is_active(&self) -> bool {
        matches!(self, Self::Active(_))
    }
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

/// The existing reservation record owns this product throughout native retirement.
/// The candidate and expected selection survive a panic inside consuming reserve.
pub(super) struct GenerationRetirementAttempt {
    actor: ActorRef,
    candidate: ActorGenerationCandidateCustody,
    expected: Arc<ActorGeneration>,
    diagnostic: GenerationRetirementDiagnostic,
    phase: GenerationRetirementPhase,
    inventory: GenerationRetirementInventory,
}

enum GenerationRetirementPhase {
    Preparation(ActorGenerationRetirementPreparation),
    Reserving,
    ReservationFailed(Arc<ResidentActorWorkbenchError>),
    Reserved {
        operation: ActorGenerationPublicationOperation,
        observer: ActorPublicationAttemptObserver,
        cutoff: GenerationRetirementCutoffPhase,
    },
    Publication {
        observer: ActorPublicationAttemptObserver,
        cutoff: GenerationRetirementCutoffPhase,
        custody: GenerationPublicationCustody,
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
    _publisher: Option<ActorGenerationPublicationDiscard>,
    _inventory: GenerationRetirementInventory,
}

pub(super) enum GenerationRetirementBeginFailure {
    Directory(DirectoryError),
    Unregistered {
        terminal: Option<ActorTerminal>,
        cleanup: Option<crate::ResidentCleanupOutcome>,
    },
    ExpectedGenerationMissing,
    Refused(String),
    Publication(Arc<ResidentActorWorkbenchError>),
}

pub(super) enum RetainedGenerationRetirementRecord {
    Active(RetainedGenerationRetirement),
    SourcesSettled {
        actor: ActorRef,
        completion: GenerationRetirementCompletion,
        cutoff: GenerationRetirementCutoffPhase,
    },
    Completed {
        actor: ActorRef,
        completion: GenerationRetirementCompletion,
    },
}

pub(super) struct RetainedGenerationRetirement {
    actor: ActorRef,
    candidate: ActorGenerationCandidateCustody,
    expected: Arc<ActorGeneration>,
    diagnostic: GenerationRetirementDiagnostic,
    phase: RetainedGenerationRetirementPhase,
}

enum RetainedGenerationRetirementPhase {
    Preparation(PreparedActorGeneration),
    Reserving,
    ReservationFailed(Arc<ResidentActorWorkbenchError>),
    Reserved {
        reserved: ReservedActorGeneration,
        observer: ActorPublicationAttemptObserver,
        cutoff: GenerationRetirementCutoffPhase,
    },
    Publication {
        observer: ActorPublicationAttemptObserver,
        cutoff: GenerationRetirementCutoffPhase,
        custody: GenerationPublicationCustody,
    },
}

impl GenerationRetirementAttempt {
    fn into_retained(self) -> (RetainedGenerationRetirement, GenerationRetirementDiscard) {
        let Self {
            actor,
            candidate,
            expected,
            diagnostic,
            phase,
            inventory,
        } = self;
        let (phase, discard) = match phase {
            GenerationRetirementPhase::Publication {
                observer,
                cutoff,
                custody,
            } => (
                RetainedGenerationRetirementPhase::Publication {
                    observer,
                    cutoff,
                    custody,
                },
                None,
            ),
            GenerationRetirementPhase::Preparation(preparation) => {
                let (prepared, discard) = preparation.into_retained_parts();
                (
                    RetainedGenerationRetirementPhase::Preparation(prepared),
                    Some(discard),
                )
            }
            GenerationRetirementPhase::Reserving => {
                (RetainedGenerationRetirementPhase::Reserving, None)
            }
            GenerationRetirementPhase::ReservationFailed(error) => (
                RetainedGenerationRetirementPhase::ReservationFailed(error),
                None,
            ),
            GenerationRetirementPhase::Reserved {
                operation,
                observer,
                cutoff,
            } => {
                let (reserved, discard) = operation.into_retained_parts();
                (
                    RetainedGenerationRetirementPhase::Reserved {
                        reserved,
                        observer,
                        cutoff,
                    },
                    Some(discard),
                )
            }
        };
        (
            RetainedGenerationRetirement {
                actor,
                candidate,
                expected,
                diagnostic,
                phase,
            },
            GenerationRetirementDiscard {
                _publisher: discard,
                _inventory: inventory,
            },
        )
    }
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
                    Ok(())
                        if matches!(reason, preparation::PublicationNonStart::SpawnFailed(_))
                            || !diagnostic.is_cancelled_component() =>
                    {
                        let old = std::mem::replace(self, Self::ProducerLostAfterExecute);
                        let Self::NotStarted {
                            reserved, reason, ..
                        } = old
                        else {
                            return None;
                        };
                        *self = Self::SourceRestoredPending(
                            PublicationSourceRestoredPending::NotStarted { reserved, reason },
                        );
                        None
                    }
                    Ok(()) => Some(GenerationRetirementCompletion::AReopened {
                        outcome: ActorGenerationPublicationRefusalObservation::Refused,
                        nonstart: Some(reason.kind()),
                        diagnostic: None,
                        diagnostic_refusals: Vec::new(),
                    }),
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

impl RunControlNativeRecovery<'_> {
    fn reconcile_publications(&mut self) {
        let Some(custody) = self.custody.as_mut() else {
            return;
        };
        for record in &mut custody.attempts {
            let RetainedGenerationRetirementRecord::Active(attempt) = record else {
                continue;
            };
            let RetainedGenerationRetirementPhase::Publication {
                cutoff, custody, ..
            } = &mut attempt.phase
            else {
                continue;
            };
            if let Some(completion) = custody.reconcile(cutoff, &mut attempt.diagnostic) {
                let actor = attempt.actor;
                // Unlike live RunState, this terminal parcel is now the
                // retained owner of its capture's actual cleanup callbacks.
                // Move that capture into SourcesSettled before releasing B.
                let displaced = std::mem::replace(
                    record,
                    RetainedGenerationRetirementRecord::Completed {
                        actor,
                        completion: GenerationRetirementCompletion::UnreservedRefused,
                    },
                );
                match displaced {
                    RetainedGenerationRetirementRecord::Active(RetainedGenerationRetirement {
                        actor,
                        candidate,
                        expected,
                        diagnostic,
                        phase:
                            RetainedGenerationRetirementPhase::Publication {
                                observer,
                                cutoff,
                                custody,
                            },
                    }) => {
                        *record = RetainedGenerationRetirementRecord::SourcesSettled {
                            actor,
                            completion,
                            cutoff,
                        };
                        drop((candidate, expected, diagnostic, observer, custody));
                    }
                    other => *record = other,
                }
            }
        }
    }
}

#[cfg(test)]
impl RetainedGenerationRetirementRecord {
    fn active_for_test(&self) -> &RetainedGenerationRetirement {
        match self {
            Self::Active(attempt) => attempt,
            _ => panic!("expected retained active attempt"),
        }
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
                GenerationRetirementPhase::Publication {
                    custody: GenerationPublicationCustody::SourceRestoredPending(_),
                    ..
                }
            ) {
                // A is already reopened. This phase owns only the independent
                // recovery obligation and cannot freeze/stop its prior closure.
                return None;
            }
            let (GenerationRetirementPhase::Reserved {
                cutoff: GenerationRetirementCutoffPhase::Captured { capture, .. },
                ..
            }
            | GenerationRetirementPhase::Publication {
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
        record.retirement = GenerationRetirementRecord::Active(GenerationRetirementAttempt {
            actor,
            candidate,
            expected,
            inventory,
            diagnostic,
            phase: GenerationRetirementPhase::Preparation(preparation),
        });
        let attempt = record
            .retirement
            .active_mut()
            .expect("retirement was just installed");
        let GenerationRetirementPhase::Preparation(preparation) =
            std::mem::replace(&mut attempt.phase, GenerationRetirementPhase::Reserving)
        else {
            unreachable!("new retirement owns one preparation")
        };
        let operation = match preparation.reserve() {
            Ok(ActorGenerationRetirementPreparationOutcome::Reserved(operation)) => operation,
            Ok(ActorGenerationRetirementPreparationOutcome::Refused(reason)) => {
                record.retirement = GenerationRetirementRecord::Completed(
                    GenerationRetirementCompletion::UnreservedRefused,
                );
                return Err(GenerationRetirementBeginFailure::Refused(reason));
            }
            Err(error) => {
                let error = Arc::new(error);
                attempt.phase = GenerationRetirementPhase::ReservationFailed(error.clone());
                return Err(GenerationRetirementBeginFailure::Publication(error));
            }
        };
        let observer = operation.attempt_observer();
        attempt.phase = GenerationRetirementPhase::Reserved {
            operation,
            observer: observer.clone(),
            cutoff: GenerationRetirementCutoffPhase::NotStarted,
        };
        let GenerationRetirementPhase::Reserved {
            operation, cutoff, ..
        } = &mut attempt.phase
        else {
            unreachable!("reservation was just retained")
        };
        match operation.reserved_mut().fence_original() {
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
        let attempt = self
            .reservations
            .get_mut(&actor)
            .and_then(|record| record.retirement.active_mut())
            .ok_or(GenerationPublicationDispatchFailure::AttemptChanged)?;
        let GenerationRetirementPhase::Reserved {
            operation,
            observer: issued,
            cutoff,
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
        if operation.reserved_mut().publication_phase()
            != tidepool_runtime::session::PublicationPhase::CancellationRequested
        {
            return Err(GenerationPublicationDispatchFailure::NotCancelled);
        }
        let (writer, receiver, terminal) =
            preparation::PublicationTransfer::channel(actor, observer.clone(), myself.clone());
        let old = std::mem::replace(&mut attempt.phase, GenerationRetirementPhase::Reserving);
        let GenerationRetirementPhase::Reserved {
            operation,
            observer,
            cutoff,
        } = old
        else {
            attempt.phase = old;
            return Err(GenerationPublicationDispatchFailure::AttemptChanged);
        };
        attempt.phase = GenerationRetirementPhase::Publication {
            observer,
            cutoff,
            custody: GenerationPublicationCustody::Waiting {
                receiver,
                terminal: terminal.clone(),
            },
        };
        let transfer = preparation::PublicationTransfer::new(operation, writer);
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
        let Some(attempt) = record.retirement.active_mut() else {
            return;
        };
        let GenerationRetirementPhase::Publication {
            observer: issued,
            cutoff,
            custody,
        } = &mut attempt.phase
        else {
            return;
        };
        if !issued.same_attempt(&observer) {
            return;
        }
        if let Some(completion) = custody.reconcile(cutoff, &mut attempt.diagnostic) {
            // No source/terminal locks are held. Publish the compact completed
            // fact before releasing the displaced candidate and full causes.
            let displaced = std::mem::replace(
                &mut record.retirement,
                GenerationRetirementRecord::Completed(completion),
            );
            drop(displaced);
        }
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
        let GenerationRetirementPhase::Reserved {
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
            GenerationRetirementPhase::Reserved {
                cutoff: GenerationRetirementCutoffPhase::AwaitCapture { fence, .. },
                ..
            } => fence.pending_claims(),
            _ => return,
        };
        let population = self.generation_population(attempt.expected.dependency().clone());
        let work = match &attempt.inventory {
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
        let GenerationRetirementPhase::Reserved { cutoff, .. } = &mut attempt.phase else {
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
        for (actor, record) in &mut self.reservations {
            match std::mem::replace(&mut record.retirement, GenerationRetirementRecord::Idle) {
                GenerationRetirementRecord::Active(attempt) => {
                    let (retained, discard) = attempt.into_retained();
                    // One slot per actual record was reserved before admission.
                    self.lost_retirements
                        .push(RetainedGenerationRetirementRecord::Active(retained));
                    self.retirement_discards.push(discard);
                }
                GenerationRetirementRecord::Completed(completion) => {
                    self.lost_retirements
                        .push(RetainedGenerationRetirementRecord::Completed {
                            actor: *actor,
                            completion,
                        })
                }
                GenerationRetirementRecord::Idle => {}
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
