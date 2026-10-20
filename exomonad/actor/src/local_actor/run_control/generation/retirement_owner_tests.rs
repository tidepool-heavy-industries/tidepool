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
        let (reply, answer) = oneshot::channel();
        self.owner
            .send_message(RunControlMessage::BeginGenerationRetirement {
                preparation,
                inventory: GenerationRetirementInventory::Unavailable,
                reply,
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
            Some(preparation::PublicationTerminalObservation::Completed(
                preparation::PublicationOutcomeObservation::Refused
            ))
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
                let RetainedGenerationRetirementPhase::Publication {
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
                [RetainedGenerationRetirementRecord::SourcesSettled { actor: completed, completion: GenerationRetirementCompletion::AReopened { .. }, .. }]
                    if *completed == actor)
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
        }
        drop(candidate);
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
                    reply,
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
            let RetainedGenerationRetirementPhase::Reserved {
                reserved,
                observer,
                cutoff,
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
                    let GenerationCapturePhysicalClosure::Guarded { actors, cells } =
                        &capture.physical_closure;
                    assert!(actors.contains(&actor));
                    assert!(capture.actors.iter().any(|captured| {
                        captured.identity() == actor
                            && cells.contains(&captured.address().get_id())
                    }));
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
