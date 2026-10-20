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
    pub(crate) async fn assert_generation_retirement_owner_control(
        &self,
        owner_task: ractor::concurrency::JoinHandle<()>,
        preparation: ActorGenerationRetirementPreparation,
        state: Arc<InstalledToolsState>,
        expected: Arc<ActorGeneration>,
        decision: Arc<tidepool_runtime::session::PublicationDecision>,
        control: GenerationRetirementNativeControl,
        disposed: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) {
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
            let Some(RunControlNativeExit::OwnerLost { attempts, .. }) = retained.terminal.get()
            else {
                panic!("actual products must be retained before publisher disposal");
            };
            assert_eq!(attempts.len(), 1);
            assert!(candidate.same_candidate(&attempts[0].candidate));
            assert!(Arc::ptr_eq(&attempts[0].expected, &expected));
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
        let Some(RunControlNativeExit::OwnerLost { attempts, .. }) = retained.terminal.get() else {
            panic!("lost native owner must retain its terminal product")
        };
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
            let attempt = &attempts[0];
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
                        captured.identity() == actor && cells.contains(&captured.address().get_id())
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
        assert!(matches!(
            self.settle_run_preparations(Duration::from_millis(10))
                .await,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
    }
}
