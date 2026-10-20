use super::*;
use crate::resident_workbench::{
    ActorGenerationBodyOutcome, ActorGenerationPublicationBodyCompletion, InstalledToolsState,
};

fn reserve(
    preparation: ActorGenerationRetirementPreparation,
) -> (
    ActorGenerationPublicationOwner,
    ActorGenerationPublicationDriver,
    GenerationRetirementFence,
    ActorPublicationAttemptObserver,
) {
    let (prepared, driver) = preparation.into_parts();
    let ActorGenerationRetirementPreparationOutcome::Reserved {
        mut reserved,
        publisher,
    } = driver
        .reserve(prepared)
        .unwrap_or_else(|_| panic!("the actual issuer accepts its same prepared candidate"))
    else {
        panic!("the actual replacement reserves against restored A")
    };
    let observer = reserved.attempt_observer();
    let ActorGenerationRetirementCutoff::Fenced(fence) = reserved.fence_original() else {
        panic!("the actual replacement owns its A cutoff")
    };
    (
        reserved.into_publication_owner(),
        publisher,
        fence,
        observer,
    )
}

fn return_through_stopped_endpoint(
    endpoint: &RactorRef<PreparationMessage>,
    actor: ActorRef,
    observer: ActorPublicationAttemptObserver,
    control: RactorRef<RunControlMessage>,
    owner: &mut ActorGenerationPublicationOwner,
    publisher: ActorGenerationPublicationDriver,
) -> GenerationPublicationCustody {
    let body = owner
        .take_body()
        .unwrap_or_else(|_| panic!("same retained owner issues its single body"));
    let (writer, receiver, terminal) =
        preparation::PublicationTransfer::channel(actor, observer, control);
    let transfer = preparation::PublicationTransfer::new(body, publisher, writer);
    let Err(ractor::MessagingErr::SendErr(PreparationMessage::PublishGeneration(transfer))) =
        endpoint.send_message(PreparationMessage::PublishGeneration(transfer))
    else {
        panic!("the stopped actual supervisor must return the entire unexecuted transfer")
    };
    transfer.return_unstarted(preparation::PublicationNonStart::SupervisorSendFailed);
    assert!(matches!(
        terminal.get(),
        Some(preparation::PublicationTerminalObservation::NotStarted(
            preparation::PublicationNonStartKind::SupervisorSendFailed
        ))
    ));
    GenerationPublicationCustody::Waiting { receiver, terminal }
}

impl RunControlClient {
    pub(crate) fn assert_cancelled_initial_publication(
        &self,
        preparation: ActorGenerationRetirementPreparation,
        state: Arc<InstalledToolsState>,
        decision: Arc<tidepool_runtime::session::PublicationDecision>,
        retained: bool,
    ) -> futures_util::future::BoxFuture<'_, ()> {
        Box::pin(async move {
            assert!(preparation.expected_generation().is_none());
            assert!(preparation.reload_report().is_none());
            let actor = preparation.actor();
            let bootstrap = state.current().unwrap();
            let (prepared, driver) = preparation.into_parts();
            let ActorGenerationRetirementPreparationOutcome::Reserved {
                reserved,
                publisher,
            } = driver
                .reserve(prepared)
                .unwrap_or_else(|_| panic!("actual initial preparation matches its driver"))
            else {
                panic!("the actual bootstrap must reserve its prepared initial generation")
            };
            let observer = reserved.attempt_observer();
            let mut owner = reserved.into_publication_owner();
            let body = owner
                .take_body()
                .unwrap_or_else(|_| panic!("actual initial owner issues its own body"));
            assert_eq!(
                decision.request_cancellation(),
                tidepool_runtime::session::PublicationCancellation::RequestedBeforeCommit
            );
            let (endpoint, task) = Actor::spawn(None, PreparationSupervisor, self.owner.clone())
                .await
                .unwrap();
            let (writer, receiver, terminal) =
                preparation::PublicationTransfer::channel(actor, observer, self.owner.clone());
            let transfer = preparation::PublicationTransfer::new(body, publisher, writer);
            assert!(endpoint
                .send_message(PreparationMessage::PublishGeneration(transfer))
                .is_ok());
            let mut custody = GenerationPublicationCustody::Waiting { receiver, terminal };
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    custody.observe(&mut owner);
                    if !matches!(custody, GenerationPublicationCustody::Waiting { .. }) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(matches!(custody, GenerationPublicationCustody::Received));
            assert!(matches!(
                owner.failure_outcome().unwrap().as_ref(),
                Ok(ActorGenerationCommit::Refused(_))
            ));
            assert!(matches!(
                owner.refuse_initial(),
                PublicationOwnerSettlement::Restored
            ));
            let (discharge, error) = initial_publication_failure(&owner, &custody);
            assert_eq!(discharge, !retained);
            match (&error, retained) {
                (InitialGenerationPublicationError::Body(actual), false)
                | (InitialGenerationPublicationError::UnconfirmedBody(actual), true) => {
                    assert!(Arc::ptr_eq(actual, owner.failure_outcome().unwrap()));
                }
                _ => panic!("actual body obligations must control the shared Initial classifier"),
            }
            let error = ResidentActorWorkbenchError::InitialPublication {
                source: Box::new(error),
            };
            assert_eq!(error.requires_recovery(), retained);
            assert_eq!(owner.settled_obligations(), !retained);
            assert!(owner.confirmed_generation().is_none());
            assert!(state.current_generation().is_none());
            let restored = state.current().unwrap();
            assert_eq!(restored.actor(), bootstrap.actor());
            assert!(restored.source().same_revision(bootstrap.source()));
            assert!(Arc::ptr_eq(
                &restored.tools_arc().unwrap(),
                &bootstrap.tools_arc().unwrap()
            ));
            endpoint.stop(None);
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .unwrap()
                .unwrap();
        })
    }

    pub(crate) fn assert_publication_transport_protocol(
        &self,
        preparations: Vec<ActorGenerationRetirementPreparation>,
        state: Arc<InstalledToolsState>,
        diagnostics: Vec<crate::resident_actor::WorkbenchOperationDiagnosticOwner>,
    ) -> futures_util::future::BoxFuture<'_, ()> {
        Box::pin(async move {
            assert_eq!(preparations.len(), 3);
            assert_eq!(diagnostics.len(), 3);
            let actor = preparations[0].actor();
            let expected = preparations[0].expected_generation().unwrap().clone();
            for preparation in &preparations {
                assert_eq!(preparation.actor(), actor);
                assert!(Arc::ptr_eq(
                    preparation.expected_generation().unwrap(),
                    &expected
                ));
                assert!(preparation.reload_report().is_none());
            }
            let compiler_calls = tidepool_extract_cmd::extract_spawn_count();
            let (endpoint, task) = Actor::spawn(None, PreparationSupervisor, self.owner.clone())
                .await
                .unwrap();
            endpoint.stop(None);
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .unwrap()
                .unwrap();
            let mut preparations = preparations.into_iter();
            let mut diagnostics = diagnostics
                .into_iter()
                .map(GenerationRetirementDiagnostic::operation);

            // Each candidate was separately issued before the protocol check. Each
            // reservation below gets a new fence only after its predecessor restores A.
            let (mut first, publisher, mut first_fence, observer) =
                reserve(preparations.next().unwrap());
            let first_attempt = observer.clone();
            assert!(expected.scope().admit_source().is_err());
            let mut first_delivery = return_through_stopped_endpoint(
                &endpoint,
                actor,
                observer,
                self.owner.clone(),
                &mut first,
                publisher,
            );
            let mut first_diagnostic = diagnostics.next().unwrap();
            let completion = first_delivery.reconcile_publication_source(
                &mut first,
                &mut first_fence,
                &mut first_diagnostic,
            );
            assert!(matches!(
                completion,
                Some(GenerationRetirementCompletion::AReopened {
                    nonstart: Some(preparation::PublicationNonStartKind::SupervisorSendFailed),
                    diagnostic: Some(_),
                    diagnostic_refusals,
                    ..
                }) if diagnostic_refusals.is_empty()
            ));
            assert!(first.source_restored());
            assert!(first.settled_obligations());
            assert!(first.confirmed_generation().is_none());
            assert!(Arc::ptr_eq(&state.current_generation().unwrap(), &expected));
            drop(expected.scope().admit_source().unwrap());

            let (mut second, publisher, mut second_fence, observer) =
                reserve(preparations.next().unwrap());
            assert!(!observer.same_attempt(&first_attempt));
            let second_attempt = observer.clone();
            let body = second
                .take_body()
                .unwrap_or_else(|_| panic!("second actual owner issues its own body"));
            let body = first
                .return_unstarted(body)
                .err()
                .expect("the previously settled owner cannot adopt another attempt's body");
            assert!(second.return_unstarted(body).is_ok());
            let mut second_delivery = return_through_stopped_endpoint(
                &endpoint,
                actor,
                observer,
                self.owner.clone(),
                &mut second,
                publisher,
            );
            let mut second_diagnostic = diagnostics.next().unwrap();
            for _ in 0..2 {
                assert!(second_delivery
                    .reconcile_publication_source(
                        &mut second,
                        &mut second_fence,
                        &mut second_diagnostic,
                    )
                    .is_none());
                assert!(matches!(
                    second_delivery,
                    GenerationPublicationCustody::SourceRestoredPending { .. }
                ));
                assert!(second.source_restored());
                assert!(second_diagnostic.recorded().is_none());
                assert!(Arc::ptr_eq(&state.current_generation().unwrap(), &expected));
                drop(expected.scope().admit_source().unwrap());
            }
            // The missing real journal cannot acknowledge a diagnostic. Its source
            // result says nothing about physical cleanup, STOP or a Reload receipt.
            assert!(matches!(
                &second_diagnostic,
                GenerationRetirementDiagnostic::Operation {
                    handoff: RetirementDiagnosticHandoff::Projected { refusals, .. },
                    ..
                } if refusals.contains(&crate::WorkbenchDiagnosticRefusal::ExecutionMissing)
            ));

            let (mut third, publisher, mut third_fence, observer) =
                reserve(preparations.next().unwrap());
            assert!(!observer.same_attempt(&first_attempt));
            assert!(!observer.same_attempt(&second_attempt));
            let body = third
                .take_body()
                .unwrap_or_else(|_| panic!("third actual owner issues its own body"));
            let (writer, receiver, terminal) =
                preparation::PublicationTransfer::channel(actor, observer, self.owner.clone());
            let result = publisher
                .run(body)
                .await
                .unwrap_or_else(|_| panic!("same driver must execute its actual body"));
            let completed = match result.completion() {
                ActorGenerationPublicationBodyCompletion::Observed {
                    outcome: Ok(ActorGenerationBodyOutcome::Visible { .. }),
                    ..
                } => "observed visibility",
                ActorGenerationPublicationBodyCompletion::Observed {
                    outcome: Ok(ActorGenerationBodyOutcome::Refused(_)),
                    ..
                } => "observed refusal",
                ActorGenerationPublicationBodyCompletion::Observed {
                    outcome: Ok(ActorGenerationBodyOutcome::Unconfirmed(_)),
                    ..
                } => "observed unconfirmed outcome",
                ActorGenerationPublicationBodyCompletion::Observed {
                    outcome: Err(_), ..
                } => "observed failure",
                ActorGenerationPublicationBodyCompletion::Unobserved { .. } => {
                    "unobserved body outcome"
                }
            };
            eprintln!("same accepted publication body completed: {completed}; discarding its exact result");
            drop(result);
            drop(writer);
            assert!(matches!(
                terminal.get(),
                Some(preparation::PublicationTerminalObservation::ProducerLostAfterExecute)
            ));
            let mut third_delivery = GenerationPublicationCustody::Waiting { receiver, terminal };
            let mut third_diagnostic = diagnostics.next().unwrap();
            for _ in 0..2 {
                assert!(third_delivery
                    .reconcile_publication_source(
                        &mut third,
                        &mut third_fence,
                        &mut third_diagnostic,
                    )
                    .is_none());
                assert!(matches!(
                    third_delivery,
                    GenerationPublicationCustody::DeliveryLost { .. }
                ));
                assert!(!third.source_restored());
                assert!(!third.settled_obligations());
                assert!(third.confirmed_generation().is_none());
                assert!(state.snapshot_for_call().is_err());
                assert!(expected.scope().admit_source().is_err());
                assert!(third_diagnostic.recorded().is_none());
            }
            assert_eq!(tidepool_extract_cmd::extract_spawn_count(), compiler_calls);
        })
    }
}
