use super::*;

fn registered_shutdown_owners(
    registrations: impl IntoIterator<Item = (ActorRef, tokio::task::Id)>,
) -> InteractiveOwners {
    Arc::new(Mutex::new(
        registrations
            .into_iter()
            .map(|(actor, task_id)| {
                let mut owner = InteractiveApplicationOwner::embedded();
                owner.embedded.live = true;
                owner.embedded.task_id = Some(task_id);
                (actor, owner)
            })
            .collect(),
    ))
}

#[tokio::test]
async fn embedded_shutdown_reports_driver_failure_and_accepts_confirmed_cancellation() {
    use embedded_service::EmbeddedDriverError;
    let actor = ActorRef::first(exomonad_actor::ActorId(42));
    let mut tasks = JoinSet::new();
    let task = tasks.spawn(async move { (actor, (), Ok(())) });
    let owners = registered_shutdown_owners([(actor, task.id())]);
    let mut waiters = HashMap::new();
    assert_eq!(
        drain_embedded_shutdown(&mut tasks, Duration::from_secs(1), &owners, &mut waiters).await,
        EmbeddedShutdownOutcome::default()
    );

    let task = tasks.spawn(async move {
        (
            actor,
            (),
            Err(EmbeddedDriverError::Engine(
                harness::engine::EngineError::InvalidFunctionCall,
            )),
        )
    });
    let owners = registered_shutdown_owners([(actor, task.id())]);
    let failure =
        drain_embedded_shutdown(&mut tasks, Duration::from_secs(1), &owners, &mut waiters)
            .await
            .driver_failure
            .unwrap();
    assert!(
        failure.contains("malformed Responses tool call item"),
        "{failure}"
    );
    assert!(failure.contains("42"), "{failure}");

    let task = tasks.spawn(async move {
        (
            actor,
            (),
            Err(EmbeddedDriverError::Engine(
                harness::engine::EngineError::Cleanup {
                    primary: Box::new(harness::engine::EngineError::Cancelled {
                        head_request: None,
                    }),
                    cleanup: "claim settlement failed".into(),
                },
            )),
        )
    });
    let owners = registered_shutdown_owners([(actor, task.id())]);
    let failure =
        drain_embedded_shutdown(&mut tasks, Duration::from_secs(1), &owners, &mut waiters)
            .await
            .cleanup_failure
            .unwrap();
    assert!(failure.contains("claim settlement failed"), "{failure}");
}

#[tokio::test(start_paused = true)]
async fn embedded_shutdown_timeout_does_not_wait_for_unconfirmed_abort() {
    let actor = ActorRef::first(exomonad_actor::ActorId(42));
    let pending = ActorRef::first(exomonad_actor::ActorId(43));
    let mut tasks: JoinSet<(
        ActorRef,
        (),
        Result<(), embedded_service::EmbeddedDriverError>,
    )> = JoinSet::new();
    let task = tasks.spawn(async move {
        (
            actor,
            (),
            Err(embedded_service::EmbeddedDriverError::Host(
                "prior driver failure".into(),
            )),
        )
    });
    let pending_task = tasks.spawn(std::future::pending());
    let owners = registered_shutdown_owners([(actor, task.id()), (pending, pending_task.id())]);
    let outcome = drain_embedded_shutdown(
        &mut tasks,
        Duration::from_secs(1),
        &owners,
        &mut HashMap::new(),
    )
    .await;
    let failure = outcome.cleanup_failure.unwrap();
    assert!(failure.contains("cleanup unconfirmed"), "{failure}");
    assert!(outcome
        .driver_failure
        .unwrap()
        .contains("prior driver failure"));
    while let Some(result) = tasks.join_next().await {
        assert!(result.unwrap_err().is_cancelled());
    }
}

#[test]
fn embedded_resource_release_preserves_cleanup_uncertainty() {
    use embedded_service::EmbeddedDriverError;
    use harness::engine::EngineError;
    assert_eq!(
        embedded_resource_release(None),
        exomonad_actor::ResourceRelease::Released
    );
    let cleanup = EmbeddedDriverError::Engine(EngineError::Cleanup {
        primary: Box::new(EngineError::Cancelled { head_request: None }),
        cleanup: "claim settlement failed".into(),
    });
    assert!(
        matches!(embedded_resource_release(Some(&cleanup)), exomonad_actor::ResourceRelease::Retained(detail)
        if detail.contains("claim settlement failed"))
    );
}

#[test]
fn embedded_owner_keeps_release_pending_until_task_settles_and_retains_failed_cleanup() {
    use exomonad_actor::{ActorId, ResourceRelease};
    use harness::engine::EngineError;
    let actor = ActorRef::first(ActorId(42));
    let owner = InteractiveApplicationOwner::embedded();
    let owners = Arc::new(Mutex::new(HashMap::from([(actor, owner)])));

    update_embedded_state(&owners, actor, |state| state.live = true);
    assert_eq!(observed_resource_release(actor, &owners), None);
    update_embedded_state(&owners, actor, |state| state.live = false);
    assert_eq!(
        observed_resource_release(actor, &owners),
        Some(ResourceRelease::Released)
    );
    update_embedded_state(&owners, actor, |state| {
        state.cleanup_failure = Some(embedded_service::EmbeddedDriverError::Engine(
            EngineError::Cleanup {
                primary: Box::new(EngineError::Cancelled { head_request: None }),
                cleanup: "claim settlement failed".into(),
            },
        ));
    });
    assert!(matches!(
        observed_resource_release(actor, &owners),
        Some(ResourceRelease::Retained(detail)) if detail.contains("claim settlement failed")
    ));
}

#[tokio::test(start_paused = true)]
async fn embedded_shutdown_settles_exact_waiters_and_preserves_timeout_uncertainty() {
    use exomonad_actor::{ActorId, ReleaseAwait, ResourceRelease};
    let actors = [
        ActorRef::first(ActorId(42)),
        ActorRef::first(ActorId(43)),
        ActorRef::first(ActorId(44)),
    ];
    let mut waiters = HashMap::new();
    let mut replies = Vec::new();
    for actor in actors {
        let (request, reply) = ReleaseAwait::channel(actor);
        waiters.insert(actor, vec![request]);
        replies.push(reply);
    }
    let mut tasks = JoinSet::new();
    let first = tasks.spawn(async move { (actors[0], (), Ok(())) });
    let second = tasks.spawn(async move {
        (
            actors[1],
            (),
            Err(embedded_service::EmbeddedDriverError::Engine(
                harness::engine::EngineError::Cleanup {
                    primary: Box::new(harness::engine::EngineError::Cancelled {
                        head_request: None,
                    }),
                    cleanup: "claim settlement failed".into(),
                },
            )),
        )
    });
    let pending = tasks.spawn(std::future::pending());
    let owners = registered_shutdown_owners([
        (actors[0], first.id()),
        (actors[1], second.id()),
        (actors[2], pending.id()),
    ]);
    let failure =
        drain_embedded_shutdown(&mut tasks, Duration::from_secs(1), &owners, &mut waiters)
            .await
            .cleanup_failure
            .unwrap();
    assert!(failure.contains("claim settlement failed"));
    assert!(failure.contains("cleanup unconfirmed"));
    assert_eq!(waiters.len(), 1);
    assert!(waiters.contains_key(&actors[2]));
    retain_unsettled_release_waiters(&mut waiters);
    assert!(waiters.is_empty());
    let mut replies = replies.into_iter();
    assert_eq!(
        observed_resource_release(actors[0], &owners),
        Some(ResourceRelease::Released)
    );
    assert!(matches!(
        observed_resource_release(actors[1], &owners),
        Some(ResourceRelease::Retained(_))
    ));
    assert_eq!(observed_resource_release(actors[2], &owners), None);
    assert_eq!(
        replies.next().unwrap().await.unwrap(),
        ResourceRelease::Released
    );
    assert!(
        matches!(replies.next().unwrap().await.unwrap(), ResourceRelease::Retained(detail) if detail.contains("claim settlement failed"))
    );
    assert!(matches!(
        replies.next().unwrap().await.unwrap(),
        ResourceRelease::Retained(_)
    ));
}

#[tokio::test]
async fn shutdown_lost_driver_and_dropped_waiter_do_not_confirm_release() {
    use exomonad_actor::{ActorId, ReleaseAwait, ResourceRelease};
    let actor = ActorRef::first(ActorId(42));
    let (request, reply) = ReleaseAwait::channel(actor);
    let (abandoned, abandoned_reply) = ReleaseAwait::channel(actor);
    drop(abandoned_reply);
    let mut waiters = HashMap::from([(actor, vec![abandoned, request])]);
    let mut tasks: JoinSet<(
        ActorRef,
        (),
        Result<(), embedded_service::EmbeddedDriverError>,
    )> = JoinSet::new();
    let task = tasks.spawn(async { panic!("driver lost before reporting cleanup") });
    let owners = registered_shutdown_owners([(actor, task.id())]);
    let failure =
        drain_embedded_shutdown(&mut tasks, Duration::from_secs(1), &owners, &mut waiters)
            .await
            .cleanup_failure
            .unwrap();
    assert!(failure.contains("driver lost before reporting cleanup"));
    assert!(failure.contains("42"), "{failure}");
    assert_eq!(waiters.len(), 1);
    assert_eq!(observed_resource_release(actor, &owners), None);
    retain_unsettled_release_waiters(&mut waiters);
    assert!(matches!(reply.await.unwrap(), ResourceRelease::Retained(_)));
}

#[tokio::test]
async fn shutdown_answer_preserves_multiple_waiters_and_exact_incarnations() {
    use exomonad_actor::{ActorId, ReleaseAwait, ResourceRelease};
    let actor = ActorRef::first(ActorId(42));
    let other = ActorRef {
        id: actor.id,
        incarnation: exomonad_actor::Incarnation(2),
    };
    let (first, first_reply) = ReleaseAwait::channel(actor);
    let (second, second_reply) = ReleaseAwait::channel(actor);
    let (pending, pending_reply) = ReleaseAwait::channel(other);
    let mut waiters = HashMap::from([(actor, vec![first, second]), (other, vec![pending])]);
    answer_release_waiters(&mut waiters, actor, ResourceRelease::Released);
    assert_eq!(first_reply.await.unwrap(), ResourceRelease::Released);
    assert_eq!(second_reply.await.unwrap(), ResourceRelease::Released);
    assert_eq!(waiters.len(), 1);
    assert!(waiters.contains_key(&other));
    retain_unsettled_release_waiters(&mut waiters);
    assert!(matches!(
        pending_reply.await.unwrap(),
        ResourceRelease::Retained(_)
    ));
}

#[tokio::test]
async fn shutdown_closes_release_admission_and_preserves_queued_exact_actor_waits() {
    use exomonad_actor::{ActorId, ReleaseAwait, ResourceRelease};
    let actors = [41, 42, 43].map(|id| ActorRef::first(ActorId(id)));
    let (sender, mut lifecycle) = mpsc::channel(3);
    let mut replies = Vec::new();
    for actor in actors {
        let (request, reply) = ReleaseAwait::channel(actor);
        sender
            .try_send(LocalResidentDeployment::ReleaseAwait(request))
            .unwrap();
        replies.push(reply);
    }
    let mut waiters = HashMap::new();
    close_release_observations(&mut lifecycle, &mut waiters, |actor| {
        if actor == actors[0] {
            Some(ResourceRelease::Released)
        } else if actor == actors[1] {
            Some(ResourceRelease::Retained("retained cleanup receipt".into()))
        } else {
            None
        }
    });
    assert!(sender.is_closed());
    assert_eq!(waiters.len(), 1);
    assert!(waiters.contains_key(&actors[2]));
    let (request, _) = ReleaseAwait::channel(actors[2]);
    assert!(matches!(
        sender.try_send(LocalResidentDeployment::ReleaseAwait(request)),
        Err(mpsc::error::TrySendError::Closed(_))
    ));
    retain_unsettled_release_waiters(&mut waiters);
    let mut replies = replies.into_iter();
    assert_eq!(
        replies.next().unwrap().await.unwrap(),
        ResourceRelease::Released
    );
    assert_eq!(
        replies.next().unwrap().await.unwrap(),
        ResourceRelease::Retained("retained cleanup receipt".into())
    );
    assert!(matches!(
        replies.next().unwrap().await.unwrap(),
        ResourceRelease::Retained(_)
    ));
}

#[tokio::test]
async fn draining_join_retains_exact_owner_receipt_and_rejects_wrong_task() {
    use exomonad_actor::{ActorId, ReleaseAwait, ResourceRelease};
    let actor = ActorRef::first(ActorId(42));
    let other = ActorRef {
        incarnation: exomonad_actor::Incarnation(2),
        ..actor
    };
    let mut tasks = JoinSet::new();
    let task = tasks.spawn(async {});
    let wrong_task = tasks.spawn(async {});
    let owners = Arc::new(Mutex::new(HashMap::from([(
        actor,
        InteractiveApplicationOwner::embedded(),
    )])));
    update_embedded_state(&owners, actor, |state| {
        state.live = true;
        state.task_id = Some(task.id());
    });
    let (exact, exact_reply) = ReleaseAwait::channel(actor);
    let (foreign, foreign_reply) = ReleaseAwait::channel(other);
    let mut waiters = HashMap::from([(actor, vec![exact]), (other, vec![foreign])]);
    assert!(application_supervisor::settle_embedded_completion(
        actor,
        wrong_task.id(),
        Ok(()),
        &owners,
        &mut waiters,
    )
    .is_err());
    assert!(application_supervisor::settle_embedded_completion(
        other,
        task.id(),
        Ok(()),
        &owners,
        &mut waiters,
    )
    .is_err());
    assert_eq!(observed_resource_release(actor, &owners), None);
    assert_eq!(waiters.len(), 2);
    assert_eq!(
        application_supervisor::settle_embedded_completion(
            actor,
            task.id(),
            Ok(()),
            &owners,
            &mut waiters,
        ),
        Ok(application_supervisor::EmbeddedTaskCompletion::Completed)
    );
    assert_eq!(exact_reply.await.unwrap(), ResourceRelease::Released);
    assert_eq!(
        observed_resource_release(actor, &owners),
        Some(ResourceRelease::Released)
    );
    assert!(waiters.contains_key(&other));
    retain_unsettled_release_waiters(&mut waiters);
    assert!(matches!(
        foreign_reply.await.unwrap(),
        ResourceRelease::Retained(_)
    ));
    while tasks.join_next().await.is_some() {}
}

#[tokio::test]
async fn draining_join_retains_failed_cancellation_receipt_for_later_release_waits() {
    use exomonad_actor::{ActorId, ReleaseAwait, ResourceRelease};
    let actor = ActorRef::first(ActorId(42));
    let mut tasks = JoinSet::new();
    let task = tasks.spawn(async {});
    let owners = Arc::new(Mutex::new(HashMap::from([(
        actor,
        InteractiveApplicationOwner::embedded(),
    )])));
    update_embedded_state(&owners, actor, |state| {
        state.live = true;
        state.task_id = Some(task.id());
    });
    let (request, reply) = ReleaseAwait::channel(actor);
    let mut waiters = HashMap::from([(actor, vec![request])]);
    let failure =
        embedded_service::EmbeddedDriverError::Engine(harness::engine::EngineError::Cleanup {
            primary: Box::new(harness::engine::EngineError::Cancelled { head_request: None }),
            cleanup: "exact native owner did not acknowledge cancellation".into(),
        });
    assert!(matches!(
        application_supervisor::settle_embedded_completion(
            actor,
            task.id(),
            Err(failure),
            &owners,
            &mut waiters,
        ),
        Ok(application_supervisor::EmbeddedTaskCompletion::CleanupFailed(_))
    ));
    let release = reply.await.unwrap();
    assert!(
        matches!(&release, ResourceRelease::Retained(detail) if detail.contains("did not acknowledge"))
    );
    assert_eq!(observed_resource_release(actor, &owners), Some(release));
    assert_eq!(
        with_embedded_state(&owners, actor, |state| state.task_id),
        Some(None)
    );
    while tasks.join_next().await.is_some() {}
}

#[tokio::test]
async fn final_driver_drain_records_completion_for_later_release_observations() {
    use exomonad_actor::{ActorId, ReleaseAwait, ResourceRelease};
    let actor = ActorRef::first(ActorId(42));
    let mut tasks = JoinSet::new();
    let task = tasks.spawn(async move { (actor, (), Ok(())) });
    let owners = registered_shutdown_owners([(actor, task.id())]);
    let (request, reply) = ReleaseAwait::channel(actor);
    let mut waiters = HashMap::from([(actor, vec![request])]);

    assert_eq!(observed_resource_release(actor, &owners), None);
    assert_eq!(
        drain_embedded_shutdown(&mut tasks, Duration::from_secs(1), &owners, &mut waiters).await,
        EmbeddedShutdownOutcome::default()
    );
    assert_eq!(reply.await.unwrap(), ResourceRelease::Released);
    assert!(waiters.is_empty());
    assert_eq!(
        observed_resource_release(actor, &owners),
        Some(ResourceRelease::Released)
    );
    assert_eq!(
        with_embedded_state(&owners, actor, |state| state.task_id),
        Some(None)
    );
}

#[tokio::test]
async fn driver_join_after_forest_settlement_preserves_execution_and_cleanup_outcomes() {
    use exomonad_actor::{ActorId, ResourceRelease};
    for cleanup_failed in [false, true] {
        let actor = ActorRef::first(ActorId(42));
        let (complete, completion) = oneshot::channel();
        let mut tasks = JoinSet::new();
        let task = tasks.spawn(async move {
            completion.await.unwrap();
            let error = if cleanup_failed {
                embedded_service::EmbeddedDriverError::Engine(
                    harness::engine::EngineError::Cleanup {
                        primary: Box::new(harness::engine::EngineError::InvalidFunctionCall),
                        cleanup: "native release remains unconfirmed".into(),
                    },
                )
            } else {
                embedded_service::EmbeddedDriverError::Engine(
                    harness::engine::EngineError::InvalidFunctionCall,
                )
            };
            (actor, (), Err(error))
        });
        let owners = registered_shutdown_owners([(actor, task.id())]);
        let storage = tempfile::tempdir().unwrap();
        let anchor = tidepool_atomic_write::DirectoryAnchor::open_existing(storage.path()).unwrap();
        let bindings = Arc::new(Mutex::new(BindingTable::open(&anchor, "bindings").unwrap()));
        let authority = ActorWorktreeAuthority::new("shutdown-control", bindings);
        let (_sender, mut lifecycle) = mpsc::channel(1);
        let (lifecycle_projection, _projection) = embedded_projection::LifecycleSender::channel();
        let (_forest, shutdown) = watch::channel(ApplicationShutdown::ForestSettled(
            NativeRetirement::Preserve,
        ));
        let mut waiters = HashMap::new();
        let mut notifications = JoinSet::new();

        let resident = application_supervisor::drain_resident_shutdown(
            &mut lifecycle,
            &mut tasks,
            &owners,
            &authority,
            &mut notifications,
            &lifecycle_projection,
            None,
            &mut waiters,
            shutdown,
        )
        .await;
        assert_eq!(resident, EmbeddedShutdownOutcome::default());
        assert_eq!(observed_resource_release(actor, &owners), None);
        complete.send(()).unwrap();

        let final_drain =
            drain_embedded_shutdown(&mut tasks, Duration::from_secs(1), &owners, &mut waiters)
                .await;
        assert_eq!(final_drain.cleanup_failure.is_some(), cleanup_failed);
        assert_eq!(final_drain.driver_failure.is_some(), !cleanup_failed);
        assert!(
            matches!(
                observed_resource_release(actor, &owners),
                Some(ResourceRelease::Retained(_)) if cleanup_failed
            ) || observed_resource_release(actor, &owners) == Some(ResourceRelease::Released)
                && !cleanup_failed
        );
        assert_eq!(
            with_embedded_state(&owners, actor, |state| state.task_id),
            Some(None)
        );
    }
}

#[tokio::test]
async fn generated_driver_receipt_histories_preserve_exact_owner_and_cleanup() {
    use exomonad_actor::{ActorId, Incarnation, ReleaseAwait, ResourceRelease};
    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestRunner};

    #[derive(Clone, Copy, Debug)]
    enum CompletionKind {
        Success,
        ExecutionFailure,
        CleanupFailure,
    }
    #[derive(Clone, Copy, Debug)]
    enum Registration {
        Exact,
        OtherIncarnation,
        Stale,
    }
    #[derive(Clone, Copy, Debug)]
    enum Operation {
        Observe(bool),
        Await(bool),
        Complete(bool, Registration, CompletionKind),
    }
    #[derive(Clone, Copy)]
    enum Model {
        Pending,
        Released,
        Retained,
    }
    fn matches_model(release: Option<&ResourceRelease>, model: Model) -> bool {
        matches!(
            (release, model),
            (None, Model::Pending)
                | (Some(ResourceRelease::Released), Model::Released)
                | (Some(ResourceRelease::Retained(_)), Model::Retained)
        )
    }

    let operation = prop_oneof![
        any::<bool>().prop_map(Operation::Observe),
        any::<bool>().prop_map(Operation::Await),
        (
            any::<bool>(),
            prop::sample::select(vec![
                Registration::Exact,
                Registration::OtherIncarnation,
                Registration::Stale
            ]),
            prop::sample::select(vec![
                CompletionKind::Success,
                CompletionKind::ExecutionFailure,
                CompletionKind::CleanupFailure
            ])
        )
            .prop_map(|(actor, registration, kind)| Operation::Complete(
                actor,
                registration,
                kind
            )),
    ];
    let mut config = Config::default();
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        ));
    }
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some("bridge/facade/src/actor_host/embedded_shutdown_tests.rs");
    config.test_name = Some(concat!(
        module_path!(),
        "::generated_driver_receipt_histories_preserve_exact_owner_and_cleanup"
    ));
    let mut runner = TestRunner::new(config);
    runner
        .run(&prop::collection::vec(operation, 0..64), |history| {
            let first = ActorRef::first(ActorId(42));
            let actors = [
                first,
                ActorRef {
                    incarnation: Incarnation(2),
                    ..first
                },
            ];
            let mut tasks = JoinSet::new();
            let ids = [tasks.spawn(async {}).id(), tasks.spawn(async {}).id()];
            let stale = tasks.spawn(async {}).id();
            let owners = registered_shutdown_owners([(actors[0], ids[0]), (actors[1], ids[1])]);
            let mut waiters = HashMap::new();
            let mut replies = Vec::new();
            let mut models = [Model::Pending; 2];
            for operation in history {
                match operation {
                    Operation::Observe(target) => {
                        let index = usize::from(target);
                        prop_assert!(matches_model(
                            observed_resource_release(actors[index], &owners).as_ref(),
                            models[index]
                        ));
                    }
                    Operation::Await(target) => {
                        let index = usize::from(target);
                        let (request, reply) = ReleaseAwait::channel(actors[index]);
                        match observed_resource_release(actors[index], &owners) {
                            Some(release) => {
                                request.answer(release);
                            }
                            None => waiters
                                .entry(actors[index])
                                .or_insert_with(Vec::new)
                                .push(request),
                        }
                        replies.push((index, reply));
                    }
                    Operation::Complete(target, registration, kind) => {
                        let index = usize::from(target);
                        let task_id = match registration {
                            Registration::Exact => ids[index],
                            Registration::OtherIncarnation => ids[1 - index],
                            Registration::Stale => stale,
                        };
                        let outcome = match kind {
                            CompletionKind::Success => Ok(()),
                            CompletionKind::ExecutionFailure => {
                                Err(embedded_service::EmbeddedDriverError::Host(
                                    "execution failed".into(),
                                ))
                            }
                            CompletionKind::CleanupFailure => {
                                Err(embedded_service::EmbeddedDriverError::Engine(
                                    harness::engine::EngineError::Cleanup {
                                        primary: Box::new(
                                            harness::engine::EngineError::Cancelled {
                                                head_request: None,
                                            },
                                        ),
                                        cleanup: "release unconfirmed".into(),
                                    },
                                ))
                            }
                        };
                        let expected_admission = matches!(models[index], Model::Pending)
                            && matches!(registration, Registration::Exact);
                        let completion = application_supervisor::settle_embedded_completion(
                            actors[index],
                            task_id,
                            outcome,
                            &owners,
                            &mut waiters,
                        );
                        prop_assert_eq!(completion.is_ok(), expected_admission);
                        if expected_admission {
                            models[index] = if matches!(kind, CompletionKind::CleanupFailure) {
                                Model::Retained
                            } else {
                                Model::Released
                            };
                        }
                    }
                }
                for index in 0..2 {
                    prop_assert!(matches_model(
                        observed_resource_release(actors[index], &owners).as_ref(),
                        models[index]
                    ));
                    prop_assert_eq!(
                        with_embedded_state(&owners, actors[index], |state| state.task_id),
                        Some(if matches!(models[index], Model::Pending) {
                            Some(ids[index])
                        } else {
                            None
                        })
                    );
                }
                for index in (0..replies.len()).rev() {
                    let (actor, reply) = &mut replies[index];
                    match reply.try_recv() {
                        Ok(release) => {
                            prop_assert!(matches_model(Some(&release), models[*actor]));
                            replies.swap_remove(index);
                        }
                        Err(oneshot::error::TryRecvError::Empty) => {
                            prop_assert!(matches!(models[*actor], Model::Pending))
                        }
                        Err(oneshot::error::TryRecvError::Closed) => {
                            prop_assert!(false, "live receipt observer was dropped")
                        }
                    }
                }
            }
            Ok(())
        })
        .unwrap();
}
