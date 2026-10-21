use super::*;
use crate::resident_workbench::InstalledToolsState;

fn expect_begin(
    outcome: Result<ActorPublicationAttemptObserver, GenerationRetirementBeginFailure>,
) -> ActorPublicationAttemptObserver {
    match outcome {
        Ok(observer) => observer,
        Err(GenerationRetirementBeginFailure::Publication(error)) => {
            panic!("actual retirement failed: {error}")
        }
        Err(GenerationRetirementBeginFailure::Directory(error)) => {
            panic!("actual directory refused retirement: {error}")
        }
        Err(_) => panic!("the actual registered replacement must enter retirement"),
    }
}

// The source relationship is issued by the real reservation, while this
// native-only behavior merely exposes its actual context to resource admission.
struct NativeResourceParent {
    started: Option<oneshot::Sender<crate::KernelContext>>,
}

impl crate::KernelBehavior for NativeResourceParent {
    fn start<'a>(
        &'a mut self,
        context: &'a crate::KernelContext,
    ) -> BoxFuture<'a, Result<crate::KernelStep<()>, crate::KernelBehaviorError>> {
        let started = self.started.take().unwrap();
        let context = context.clone();
        Box::pin(async move {
            assert!(started.send(context).is_ok());
            Ok(crate::KernelStep::Continue(()))
        })
    }
    fn cast<'a>(
        &'a mut self,
        _: &'a crate::KernelContext,
        _: ActorRef,
        _: crate::MailboxValue,
    ) -> BoxFuture<'a, Result<crate::KernelStep<()>, crate::KernelBehaviorError>> {
        unreachable!("native resource fixture accepts no authored messages")
    }
    fn call<'a>(
        &'a mut self,
        _: &'a crate::KernelContext,
        _: ActorRef,
        _: crate::CallAncestry,
        _: crate::MailboxValue,
    ) -> BoxFuture<'a, Result<crate::KernelStep<crate::MailboxValue>, crate::KernelBehaviorError>>
    {
        unreachable!("native resource fixture accepts no authored calls")
    }
    fn tool<'a>(
        &'a mut self,
        _: &'a crate::KernelContext,
        _: exomonad_tool::ToolInvocation,
        _: Option<Arc<dyn crate::HostedCheckpointCapture>>,
    ) -> BoxFuture<'a, Result<crate::KernelStep<serde_json::Value>, crate::KernelInvocationFailure>>
    {
        unreachable!("native resource fixture has no tools")
    }
    fn dispatch_workbench(
        &mut self,
        _: &crate::KernelContext,
        _: crate::ActorWorkbenchInvocation,
        _: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> crate::WorkbenchDispatch<Self> {
        unreachable!("native resource fixture has no workbench")
    }
    fn external_application_failed<'a>(
        &'a mut self,
        _: &'a crate::KernelContext,
        _: crate::ExternalApplicationFailure,
    ) -> BoxFuture<'a, crate::ExternalFailureDisposition> {
        unreachable!("native resource fixture has no external application")
    }
    fn shutdown<'a>(
        &'a mut self,
        _: &'a crate::KernelContext,
        _: &'a crate::ActorTerminal,
    ) -> BoxFuture<'a, Result<(), crate::KernelBehaviorError>> {
        Box::pin(async { Ok(()) })
    }
    fn stopped<'a>(
        &'a mut self,
        _: &'a crate::KernelContext,
        _: &'a crate::ActorTerminal,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    fn child_exited(&mut self, _: crate::ChildExitNotice) {}
}

struct NativeCleanupResource;
impl ractor::Actor for NativeCleanupResource {
    type Msg = ();
    type State = ();
    type Arguments = ();
    async fn pre_start(&self, _: RactorRef<()>, (): ()) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

struct NativeResourceWitness {
    parent: LocalActorRef,
    parent_task: ractor::concurrency::JoinHandle<()>,
    child: RactorRef<()>,
    cleanup_probe: std::sync::Weak<()>,
    cleanup_calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl RunControlClient {
    pub(crate) async fn assert_retirement_owner_loss_without_attempt(
        &self,
        owner_task: ractor::concurrency::JoinHandle<()>,
    ) {
        let retained = self.native_cleanup.clone();
        self.owner.get_cell().kill();
        tokio::time::timeout(Duration::from_secs(10), owner_task)
            .await
            .unwrap()
            .unwrap();
        let Some(RunControlNativeExit::OwnerLost { custody }) = retained.terminal.get() else {
            panic!("lost native owner must retain its terminal observation");
        };
        let custody = custody.lock();
        assert!(custody.as_ref().unwrap().attempts.is_empty());
    }

    async fn admit_native_cleanup_resource(
        &self,
        dependency: &GenerationDependency,
    ) -> NativeResourceWitness {
        let reservation = self
            .reserve_source_actor(
                &SourceDependency::Generation(dependency.clone()),
                crate::Incarnation::FIRST,
            )
            .await
            .unwrap();
        let identity = reservation.identity();
        let (started, context) = oneshot::channel();
        let (parent, parent_task) = crate::local_actor::spawn_reserved_local_actor(
            None,
            NativeResourceParent {
                started: Some(started),
            },
            reservation,
            self.clone(),
        )
        .await
        .unwrap();
        let context = context.await.unwrap();
        assert_eq!(parent.identity(), identity);
        assert_eq!(context.identity(), identity);
        assert_eq!(
            self.resolve(identity).await.unwrap().unwrap().identity(),
            identity
        );
        let probe = Arc::new(());
        let cleanup_probe = Arc::downgrade(&probe);
        let cleanup_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let issued_cell = Arc::new(std::sync::OnceLock::<ractor::ActorCell>::new());
        let child = context
            .spawn_resource(None, NativeCleanupResource, (), {
                let issued_cell = issued_cell.clone();
                let cleanup_calls = cleanup_calls.clone();
                move |mode| {
                    let _probe = probe.clone();
                    let cell = issued_cell.get().cloned();
                    let cleanup_calls = cleanup_calls.clone();
                    Box::pin(async move {
                        let _probe = _probe;
                        let Some(cell) = cell else {
                            return crate::CleanupComponentOutcome::Unconfirmed(
                                "resource never returned its issued physical cell".into(),
                            );
                        };
                        match mode {
                            ResourceCleanup::Observe => {
                                crate::CleanupComponentOutcome::Unconfirmed(
                                    "native resource is still live".into(),
                                )
                            }
                            ResourceCleanup::Retire => {
                                cleanup_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                cell.stop(None);
                                match cell.wait(Some(Duration::from_secs(10))).await {
                                    Ok(()) => crate::CleanupComponentOutcome::Confirmed,
                                    Err(error) => crate::CleanupComponentOutcome::Unconfirmed(
                                        error.to_string(),
                                    ),
                                }
                            }
                        }
                    })
                }
            })
            .await
            .unwrap();
        assert!(issued_cell.set(child.get_cell()).is_ok());
        assert_eq!(
            child.get_cell().try_get_supervisor().unwrap().get_id(),
            parent.address().get_id()
        );
        drop(context);
        assert!(cleanup_probe.upgrade().is_some());
        NativeResourceWitness {
            parent,
            parent_task,
            child,
            cleanup_probe,
            cleanup_calls,
        }
    }

    async fn assert_generation_publication_worker_control(
        &self,
        owner_task: ractor::concurrency::JoinHandle<()>,
        preparation: ActorGenerationRetirementPreparation,
        state: Arc<InstalledToolsState>,
        expected: Arc<ActorGeneration>,
        decision: Arc<tidepool_runtime::session::PublicationDecision>,
        target: GenerationDependency,
        control: GenerationRetirementNativeControl,
    ) {
        let actor = preparation.actor();
        let candidate = preparation.candidate_custody();
        let candidate_retention = candidate.retention_observer();
        let retained = self.native_cleanup.clone();
        let compiler_calls = tidepool_extract_cmd::extract_spawn_count();
        let native_resource =
            if control == GenerationRetirementNativeControl::WorkerNativeLossAfterBody {
                Some(
                    self.admit_native_cleanup_resource(expected.dependency())
                        .await,
                )
            } else {
                None
            };
        let (reply, answer) = oneshot::channel();
        self.owner
            .send_message(RunControlMessage::BeginGenerationRetirement {
                preparation,
                inventory: GenerationRetirementInventory::Unavailable,
                diagnostic: GenerationRetirementDiagnostic::CancelledComponent,
                reply: GenerationRetirementBeginReply::Observe(reply),
            })
            .unwrap();
        let observer = expect_begin(answer.await.unwrap());
        assert_eq!(
            self.resolve(actor).await.unwrap().unwrap().identity(),
            actor
        );
        assert_eq!(
            decision.request_cancellation(),
            tidepool_runtime::session::PublicationCancellation::RequestedBeforeCommit
        );
        let (reply, answer) = oneshot::channel();
        self.owner
            .send_message(RunControlMessage::DispatchCancelledGenerationPublication {
                actor,
                observer: observer.clone(),
                reply,
            })
            .unwrap();
        let pause = if control == GenerationRetirementNativeControl::WorkerNativeLossAfterBody {
            let (entered, paused) = oneshot::channel();
            let (release, parked) = oneshot::channel();
            // The current-thread owning test queues this pause before yielding;
            // the worker's eventual ready notification must remain behind it.
            self.owner
                .send_message(RunControlMessage::PauseForNativeRetention {
                    entered,
                    release: parked,
                })
                .unwrap();
            Some((release, paused))
        } else {
            None
        };
        let terminal = answer.await.unwrap().unwrap();
        let release = if let Some((release, paused)) = pause {
            paused.await.unwrap();
            Some(release)
        } else {
            None
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            while terminal.get().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            terminal.get(),
            Some(preparation::PublicationTerminalObservation::Completed {
                outcome: preparation::PublicationOutcomeObservation::Refused,
            })
        ));
        if control == GenerationRetirementNativeControl::WorkerNativeLossAfterBody {
            assert!(state.current_generation().is_none());
            assert!(SourceDependency::Generation(expected.dependency().clone())
                .claim()
                .is_err());
            assert!(SourceDependency::Generation(target.clone())
                .claim()
                .is_err());
            self.owner.get_cell().kill();
            tokio::time::timeout(Duration::from_secs(10), owner_task)
                .await
                .unwrap()
                .unwrap();
            drop(release);
            let recovery = retained.take_recovery().unwrap();
            assert_eq!(
                retained.observation(),
                NativeRecoveryObservation::Recovering
            );
            assert!(matches!(
                retained.take_recovery(),
                Err(NativeRecoveryUnavailable::Busy)
            ));
            {
                let custody = recovery.custody.as_ref().unwrap();
                assert_eq!(custody.attempts.len(), 1);
                let attempt = custody.attempts[0].active_for_test();
                assert!(candidate.same_candidate(&attempt.candidate));
                assert!(Arc::ptr_eq(&attempt.expected, &expected));
                let GenerationRetirementPhase::Entered {
                    observer: issued, ..
                } = &attempt.phase
                else {
                    panic!("the same receiver must survive native owner loss");
                };
                assert!(issued.same_attempt(&observer));
            }
            // Cancellation of the first unique consumer returns the same
            // unsettled candidate, held fence, and sole result receiver.
            drop(recovery);
            let mut recovery = retained.take_recovery().unwrap();
            assert!(candidate.same_candidate(
                &recovery.custody.as_ref().unwrap().attempts[0]
                    .active_for_test()
                    .candidate
            ));
            recovery.reconcile_publications();
            assert!(
                matches!(recovery.custody.as_ref().unwrap().attempts.as_slice(),
                [GenerationRetirementCore::SourcesSettled { actor: completed, completion: GenerationRetirementCompletion::AReopened { .. }, .. }]
                    if *completed == actor)
            );
            let resource_witness = native_resource.as_ref().unwrap();
            let GenerationRetirementCore::SourcesSettled {
                cutoff: GenerationRetirementCutoffPhase::Captured { capture, .. },
                ..
            } = &recovery.custody.as_ref().unwrap().attempts[0]
            else {
                panic!("source settlement retains physical cleanup custody");
            };
            assert!(capture
                .actors
                .iter()
                .any(|parent| parent.identity() == resource_witness.parent.identity()));
            let resource = capture
                .resources
                .iter()
                .find(|resource| resource.cell.get_id() == resource_witness.child.get_id())
                .expect("the actual source-A resource is captured");
            assert!(resource_witness.cleanup_probe.upgrade().is_some());
            assert_eq!(
                resource_witness
                    .cleanup_calls
                    .load(std::sync::atomic::Ordering::SeqCst),
                0
            );
            assert_ne!(
                resource_witness.child.get_status(),
                ractor::ActorStatus::Stopped
            );
            assert!(resource_witness
                .parent
                .terminal()
                .requested_shutdown()
                .is_none());
            drop(candidate);
            assert!(
                !candidate_retention.is_retained(),
                "B releases while the same native cleanup callback remains retained"
            );
            assert!(matches!(
                (resource.cleanup)(ResourceCleanup::Retire).await,
                crate::CleanupComponentOutcome::Confirmed
            ));
            assert_eq!(
                resource_witness.child.get_status(),
                ractor::ActorStatus::Stopped
            );
            assert_eq!(
                resource_witness
                    .cleanup_calls
                    .load(std::sync::atomic::Ordering::SeqCst),
                1
            );
            drop(recovery);
        } else {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let (reply, answer) = oneshot::channel();
                    self.owner
                        .send_message(RunControlMessage::ObserveGenerationRetirementCompletion {
                            actor,
                            reply,
                        })
                        .unwrap();
                    if let Some(completion) = answer.await.unwrap() {
                        assert!(matches!(
                            completion,
                            GenerationRetirementCompletion::AReopened { .. }
                        ));
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            self.owner.get_cell().kill();
            tokio::time::timeout(Duration::from_secs(10), owner_task)
                .await
                .unwrap()
                .unwrap();
            drop(candidate);
        }
        assert!(
            !candidate_retention.is_retained(),
            "known settled records release the same frozen candidate product"
        );
        assert_eq!(
            retained.observation(),
            NativeRecoveryObservation::Retained {
                attempts: 0,
                completed: usize::from(
                    control == GenerationRetirementNativeControl::WorkerLiveRefusal
                ),
                sources_settled: usize::from(
                    control == GenerationRetirementNativeControl::WorkerNativeLossAfterBody
                ),
                worker_transfers: 0,
                resource_transfers: 0,
            }
        );
        assert!(Arc::ptr_eq(&state.current_generation().unwrap(), &expected));
        drop(
            SourceDependency::Generation(expected.dependency().clone())
                .claim()
                .unwrap(),
        );
        drop(SourceDependency::Generation(target).claim().unwrap());
        drop(
            SourceDependency::Generation(
                expected
                    .dependency()
                    .prerequisite()
                    .as_ref()
                    .unwrap()
                    .clone(),
            )
            .claim()
            .unwrap(),
        );
        assert_eq!(
            decision.phase(),
            tidepool_runtime::session::PublicationPhase::CancellationRequested
        );
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), compiler_calls);
        if let Some(witness) = native_resource {
            witness.parent.address().stop(None);
            tokio::time::timeout(Duration::from_secs(10), witness.parent_task)
                .await
                .unwrap()
                .unwrap();
        }
    }

    pub(crate) async fn assert_generation_publication_delivery_lost(
        &self,
        owner_task: ractor::concurrency::JoinHandle<()>,
        preparation: ActorGenerationRetirementPreparation,
        state: Arc<InstalledToolsState>,
        expected: Arc<ActorGeneration>,
        decision: Arc<tidepool_runtime::session::PublicationDecision>,
        target: GenerationDependency,
        lifecycle: crate::LifecycleHandle,
    ) {
        let actor = preparation.actor();
        let candidate = preparation.candidate_custody();
        let retained = self.native_cleanup.clone();
        let compiler_calls = tidepool_extract_cmd::extract_spawn_count();
        assert_eq!(lifecycle.identity(), actor);
        assert!(Arc::ptr_eq(
            preparation.expected_generation().unwrap(),
            &expected
        ));
        let (reply, answer) = oneshot::channel();
        self.owner
            .send_message(RunControlMessage::BeginGenerationRetirement {
                preparation,
                inventory: GenerationRetirementInventory::Unavailable,
                diagnostic: GenerationRetirementDiagnostic::CancelledComponent,
                reply: GenerationRetirementBeginReply::Observe(reply),
            })
            .unwrap();
        let observer = expect_begin(answer.await.unwrap());
        let (reply, answer) = oneshot::channel();
        self.owner
            .send_message(
                RunControlMessage::DispatchDeliveryLostGenerationPublication {
                    actor,
                    observer: observer.clone(),
                    lifecycle: lifecycle.clone(),
                    reply,
                },
            )
            .unwrap();
        let terminal = answer.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while terminal.get().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            matches!(
                terminal.get(),
                Some(preparation::PublicationTerminalObservation::Completed {
                    outcome: preparation::PublicationOutcomeObservation::Visible,
                })
            ),
            "expected actual Visible body before delivery loss, observed {:?}",
            terminal.get()
        );
        // This round trip drains the worker completion without supplying its lost body result.
        assert_eq!(
            self.resolve(actor).await.unwrap().unwrap().identity(),
            actor
        );
        assert!(state.current_generation().is_none());
        assert!(state.snapshot_for_call().is_err());
        assert_eq!(
            decision.phase(),
            tidepool_runtime::session::PublicationPhase::Published
        );
        assert!(matches!(
            SourceDependency::Generation(expected.dependency().clone()).claim(),
            Err(
                crate::resident_workbench::generation_admission::GenerationAdmissionError::Closed {
                    phase: tidepool_runtime::session::SourceAdmissionPhase::Fenced { .. },
                    ..
                }
            )
        ));
        assert!(matches!(
            SourceDependency::Generation(target.clone()).claim(),
            Err(
                crate::resident_workbench::generation_admission::GenerationAdmissionError::Closed {
                    phase: tidepool_runtime::session::SourceAdmissionPhase::Fenced { .. },
                    ..
                }
            )
        ));
        self.owner.get_cell().kill();
        tokio::time::timeout(Duration::from_secs(10), owner_task)
            .await
            .unwrap()
            .unwrap();
        let mut recovery = retained.take_recovery().unwrap();
        for _ in 0..2 {
            recovery.reconcile_publications();
            let custody = recovery.custody.as_ref().unwrap();
            assert_eq!(custody.attempts.len(), 1);
            let attempt = custody.attempts[0].active_for_test();
            assert!(candidate.same_candidate(&attempt.candidate));
            assert!(Arc::ptr_eq(&attempt.expected, &expected));
            let GenerationRetirementPhase::Entered {
                observer: actual,
                cutoff: GenerationRetirementCutoffPhase::Captured { fence, .. },
                custody: publication,
                owner,
                ..
            } = &attempt.phase
            else {
                panic!("lost affine delivery must retain the actual A fence");
            };
            assert!(actual.same_attempt(&observer));
            assert!(fence.dependency().same_owner(expected.dependency()));
            let GenerationPublicationCustody::DeliveryLost { terminal: actual } = publication
            else {
                panic!("closed receiver must retain the SAME terminal publication metadata");
            };
            assert!(Arc::ptr_eq(actual, &terminal));
            assert!(!publication.published());
            assert!(owner.confirmed_generation().is_none());
            assert!(!owner.source_restored());
            assert!(!owner.settled_obligations());
            assert_eq!(
                publication.notification(),
                preparation::PublicationNotificationObservation::Unobserved
            );
            let (obligations, _) = custody.attempts[0]
                .operational_obligations(&GenerationRetirementReplyOwner::Native(&recovery));
            assert!(obligations.contains(&tidepool_bridge_effects::RlRetirementObligation::ReloadCandidateSettlementObligation(
                tidepool_bridge_effects::RlObligationObservation::ReloadObligationUnconfirmed,
            )));
            assert!(obligations.contains(&tidepool_bridge_effects::RlRetirementObligation::ReloadOriginalRetirementObligation(
                tidepool_bridge_effects::RlObligationObservation::ReloadObligationPending,
            )));
            assert!(obligations.contains(&tidepool_bridge_effects::RlRetirementObligation::ReloadPublicationNotificationObligation(
                tidepool_bridge_effects::RlObligationObservation::ReloadObligationUnconfirmed,
            )));

            assert!(state.current_generation().is_none());
            assert!(state.snapshot_for_call().is_err());
            assert!(matches!(SourceDependency::Generation(target.clone()).claim(),
                Err(crate::resident_workbench::generation_admission::GenerationAdmissionError::Closed {
                    phase: tidepool_runtime::session::SourceAdmissionPhase::Fenced { .. }, ..
                })));
            assert_eq!(
                decision.phase(),
                tidepool_runtime::session::PublicationPhase::Published
            );
            assert!(matches!(SourceDependency::Generation(expected.dependency().clone()).claim(),
                Err(crate::resident_workbench::generation_admission::GenerationAdmissionError::Closed {
                    phase: tidepool_runtime::session::SourceAdmissionPhase::Fenced { .. }, ..
                })));
            assert_eq!(tidepool_extract_cmd::extract_spawn_count(), compiler_calls);
        }
    }

    pub(crate) async fn assert_generation_retirement_reservation_failed(
        &self,
        owner_task: ractor::concurrency::JoinHandle<()>,
        preparation: ActorGenerationRetirementPreparation,
        state: Arc<InstalledToolsState>,
        expected: Arc<ActorGeneration>,
        decision: Arc<tidepool_runtime::session::PublicationDecision>,
        target_fence: GenerationFence,
    ) {
        let actor = preparation.actor();
        let candidate = preparation.candidate_custody();
        assert!(Arc::ptr_eq(
            preparation.expected_generation().unwrap(),
            &expected,
        ));
        assert_eq!(actor, expected.context().actor);
        let target = target_fence.dependency().clone();
        let retained = self.native_cleanup.clone();
        let compiler_calls = tidepool_extract_cmd::extract_spawn_count();
        let (reply, answer) = oneshot::channel();
        self.owner
            .send_message(RunControlMessage::BeginGenerationRetirement {
                preparation,
                inventory: GenerationRetirementInventory::Unavailable,
                diagnostic: GenerationRetirementDiagnostic::CancelledComponent,
                reply: GenerationRetirementBeginReply::Observe(reply),
            })
            .unwrap();
        let Err(GenerationRetirementBeginFailure::Publication(primary)) = answer.await.unwrap()
        else {
            panic!("the actual foreign target fence must refuse reservation before A cutoff");
        };
        assert!(Arc::ptr_eq(&state.current_generation().unwrap(), &expected));
        drop(
            SourceDependency::Generation(expected.dependency().clone())
                .claim()
                .unwrap(),
        );
        assert!(SourceDependency::Generation(target.clone())
            .claim()
            .is_err());
        assert_eq!(
            decision.phase(),
            tidepool_runtime::session::PublicationPhase::Running
        );
        self.owner.get_cell().kill();
        tokio::time::timeout(Duration::from_secs(10), owner_task)
            .await
            .unwrap()
            .unwrap();
        let recovery = retained.take_recovery().unwrap();
        let custody = recovery.custody.as_ref().unwrap();
        assert_eq!(custody.attempts.len(), 1);
        let attempt = custody.attempts[0].active_for_test();
        assert_eq!(attempt.actor, actor);
        assert!(candidate.same_candidate(&attempt.candidate));
        assert!(Arc::ptr_eq(&attempt.expected, &expected));
        let GenerationRetirementPhase::ReservationFailed(actual) = &attempt.phase else {
            panic!("native loss must retain the SAME failed reservation, not a completion");
        };
        assert!(Arc::ptr_eq(actual, &primary));
        // Releasing the foreign gate is not an instruction to retry the failed
        // reservation or to enter the body. Its original cause remains owned.
        target_fence.reopen().unwrap();
        drop(SourceDependency::Generation(target).claim().unwrap());
        assert!(Arc::ptr_eq(&state.current_generation().unwrap(), &expected));
        drop(
            SourceDependency::Generation(expected.dependency().clone())
                .claim()
                .unwrap(),
        );
        assert_eq!(
            decision.phase(),
            tidepool_runtime::session::PublicationPhase::Running
        );
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), compiler_calls);
        drop(recovery);
        let recovery = retained.take_recovery().unwrap();
        let attempt = recovery.custody.as_ref().unwrap().attempts[0].active_for_test();
        assert!(candidate.same_candidate(&attempt.candidate));
        assert!(
            matches!(&attempt.phase, GenerationRetirementPhase::ReservationFailed(actual)
            if Arc::ptr_eq(actual, &primary))
        );
    }

    pub(crate) async fn assert_generation_retirement_owner_control(
        &self,
        owner_task: ractor::concurrency::JoinHandle<()>,
        preparation: ActorGenerationRetirementPreparation,
        state: Arc<InstalledToolsState>,
        expected: Arc<ActorGeneration>,
        decision: Arc<tidepool_runtime::session::PublicationDecision>,
        target: GenerationDependency,
        control: GenerationRetirementNativeControl,
        disposed: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) {
        if matches!(
            control,
            GenerationRetirementNativeControl::WorkerLiveRefusal
                | GenerationRetirementNativeControl::WorkerNativeLossAfterBody
        ) {
            self.assert_generation_publication_worker_control(
                owner_task,
                preparation,
                state,
                expected,
                decision,
                target,
                control,
            )
            .await;
            return;
        }
        let actor = preparation.actor();
        let candidate = preparation.candidate_custody();
        assert_eq!(actor, expected.context().actor);
        assert!(Arc::ptr_eq(
            preparation.expected_generation().unwrap(),
            &expected
        ));
        assert!(Arc::ptr_eq(&state.current_generation().unwrap(), &expected));
        let compiler_calls = tidepool_extract_cmd::extract_spawn_count();
        let retained = self.native_cleanup.clone();
        // This member is admitted under A's real gate. Installing A does not
        // change the hosting actor's independent physical birth provenance.
        let captured_reservation = if control == GenerationRetirementNativeControl::AfterCapture {
            Some(
                self.reserve_source_actor(
                    &SourceDependency::Generation(expected.dependency().clone()),
                    crate::Incarnation::FIRST,
                )
                .await
                .unwrap(),
            )
        } else {
            None
        };
        let mut acknowledged = None;
        let mut waiter = None;
        let mut pause_release = None;
        if control == GenerationRetirementNativeControl::BeforeSend {
            drop(preparation);
        } else {
            if control == GenerationRetirementNativeControl::QueuedBeforeReceive {
                let (entered, paused) = oneshot::channel();
                let (release, parked) = oneshot::channel();
                self.owner
                    .send_message(RunControlMessage::PauseForNativeRetention {
                        entered,
                        release: parked,
                    })
                    .unwrap();
                paused.await.unwrap();
                pause_release = Some(release);
            }
            let (reply, answer) = oneshot::channel();
            self.owner
                .send_message(RunControlMessage::BeginGenerationRetirement {
                    preparation,
                    inventory: GenerationRetirementInventory::Unavailable,
                    diagnostic: GenerationRetirementDiagnostic::CancelledComponent,
                    reply: GenerationRetirementBeginReply::Observe(reply),
                })
                .unwrap();
            if control == GenerationRetirementNativeControl::PanicAfterReserve {
                self.owner
                    .send_message(RunControlMessage::PanicForNativeRetention)
                    .unwrap();
                acknowledged = Some(expect_begin(answer.await.unwrap()));
            } else if matches!(
                control,
                GenerationRetirementNativeControl::KillAfterReserve
                    | GenerationRetirementNativeControl::KillAfterReserveDisposalPanic
                    | GenerationRetirementNativeControl::LostWaiterAfterReserve
            ) {
                let (entered, paused) = oneshot::channel();
                let (release, parked) = oneshot::channel();
                // The owning named test uses a current-thread runtime. Both
                // insertions precede its next yield, so Begin puts Capture
                // behind this actual pause command on that scheduler.
                self.owner
                    .send_message(RunControlMessage::PauseForNativeRetention {
                        entered,
                        release: parked,
                    })
                    .unwrap();
                if control == GenerationRetirementNativeControl::LostWaiterAfterReserve {
                    drop(answer);
                } else {
                    acknowledged = Some(expect_begin(answer.await.unwrap()));
                }
                paused.await.unwrap();
                pause_release = Some(release);
            } else if control == GenerationRetirementNativeControl::AfterCapture {
                acknowledged = Some(expect_begin(answer.await.unwrap()));
                // Begin inserted Capture before acknowledging. The real
                // directory request is a FIFO observation barrier after it.
                assert_eq!(
                    self.resolve(actor).await.unwrap().unwrap().identity(),
                    actor
                );
            } else if control == GenerationRetirementNativeControl::QueuedBeforeReceive {
                waiter = Some(answer);
            } else {
                acknowledged = Some(expect_begin(answer.await.unwrap()));
            }
        }
        let before_reserve = matches!(
            control,
            GenerationRetirementNativeControl::BeforeSend
                | GenerationRetirementNativeControl::QueuedBeforeReceive
        );
        if !before_reserve {
            assert_eq!(
                decision.phase(),
                tidepool_runtime::session::PublicationPhase::Running,
                "losing an observer does not request publication cancellation"
            );
            assert_eq!(
                decision.request_cancellation(),
                tidepool_runtime::session::PublicationCancellation::RequestedBeforeCommit
            );
        }
        if control != GenerationRetirementNativeControl::PanicAfterReserve {
            self.owner.get_cell().kill();
        }
        if let Some(disposed) = &disposed {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !disposed.load(std::sync::atomic::Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let Some(RunControlNativeExit::OwnerLost { custody }) = retained.terminal.get() else {
                panic!("actual products must be retained before publisher disposal");
            };
            let custody = custody.lock();
            let attempts = &custody.as_ref().unwrap().attempts;
            assert_eq!(attempts.len(), 1);
            assert!(candidate.same_candidate(&attempts[0].active_for_test().candidate));
            assert!(Arc::ptr_eq(
                &attempts[0].active_for_test().expected,
                &expected
            ));
        }
        let joined = tokio::time::timeout(Duration::from_secs(10), owner_task)
            .await
            .unwrap();
        if control == GenerationRetirementNativeControl::KillAfterReserveDisposalPanic {
            assert!(joined.unwrap_err().is_panic());
        } else {
            joined.unwrap();
        }
        drop(pause_release);
        drop(waiter);
        let Some(RunControlNativeExit::OwnerLost { custody }) = retained.terminal.get() else {
            panic!("lost native owner must retain its terminal product")
        };
        let custody = custody.lock();
        let attempts = &custody.as_ref().unwrap().attempts;
        if before_reserve {
            assert!(attempts.is_empty());
            assert!(Arc::ptr_eq(&state.current_generation().unwrap(), &expected));
            drop(
                SourceDependency::Generation(expected.dependency().clone())
                    .claim()
                    .unwrap(),
            );
            assert_eq!(
                decision.phase(),
                tidepool_runtime::session::PublicationPhase::Running
            );
        } else {
            assert_eq!(attempts.len(), 1);
            let attempt = attempts[0].active_for_test();
            assert_eq!(attempt.actor, actor);
            assert!(candidate.same_candidate(&attempt.candidate));
            assert!(Arc::ptr_eq(&attempt.expected, &expected));
            let GenerationRetirementPhase::Unstarted {
                publication: GenerationUnstartedPublication::Available(reserved),
                observer,
                cutoff,
                ..
            } = &attempt.phase
            else {
                panic!("successful reserve must retain the same actual reservation")
            };
            assert!(reserved.attempt_observer().same_attempt(observer));
            if let Some(acknowledged) = acknowledged {
                assert!(acknowledged.same_attempt(observer));
            }
            assert!(reserved.candidate_custody().same_candidate(&candidate));
            assert!(Arc::ptr_eq(
                reserved.expected_generation().unwrap(),
                &expected
            ));
            match (control, cutoff) {
                (
                    GenerationRetirementNativeControl::AfterCapture,
                    GenerationRetirementCutoffPhase::Captured { fence, capture },
                ) => {
                    assert!(fence.dependency().same_owner(expected.dependency()));
                    assert!(matches!(
                        capture.work,
                        GenerationRetirementWork::Unavailable
                    ));
                    assert!(matches!(
                        capture.blockers(),
                        Err(GenerationRetirementProjectionRefusal::WorkUnavailable)
                    ));
                    let GenerationCapturePhysicalClosure::Guarded { actors, .. } =
                        &capture.physical_closure;
                    let member = captured_reservation.as_ref().unwrap().identity();
                    assert!(actors.contains(&member));
                    assert!(capture
                        .population
                        .resources
                        .iter()
                        .any(|resource| resource.identity
                            == GenerationResourceIdentity::Actor(member)));
                    assert_eq!(capture.reservations.len(), 1);
                    assert!(!actors.contains(&actor));
                    assert!(!capture
                        .actors
                        .iter()
                        .any(|captured| captured.identity() == actor));
                }
                (
                    _,
                    GenerationRetirementCutoffPhase::AwaitCapture {
                        fence,
                        delivery: GenerationCaptureDelivery::Enqueued,
                    },
                ) => {
                    assert!(fence.dependency().same_owner(expected.dependency()));
                }
                _ => panic!("native loss must preserve the exact issued fence phase"),
            }
            assert!(state.current_generation().is_none());
            assert!(SourceDependency::Generation(expected.dependency().clone())
                .claim()
                .is_err());
            assert_eq!(
                decision.phase(),
                tidepool_runtime::session::PublicationPhase::CancellationRequested
            );
        }
        let external = expected.dependency().prerequisite().as_ref().unwrap();
        drop(
            SourceDependency::Generation(external.clone())
                .claim()
                .unwrap(),
        );
        assert_eq!(
            tidepool_extract_cmd::extract_spawn_count(),
            compiler_calls,
            "native loss never replays preparation or publication"
        );
        drop(custody);
        assert!(matches!(
            self.settle_run_preparations(Duration::from_millis(10))
                .await,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
    }
}
