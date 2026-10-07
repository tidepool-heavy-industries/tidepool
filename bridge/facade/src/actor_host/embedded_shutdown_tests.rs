use super::*;

#[tokio::test]
async fn embedded_shutdown_reports_driver_failure_and_accepts_confirmed_cancellation() {
    use embedded_service::EmbeddedDriverError;
    let actor = ActorRef::first(exomonad_actor::ActorId(42));
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { (actor, (), Ok(())) });
    assert_eq!(
        drain_embedded_shutdown(&mut tasks, Duration::from_secs(1), |_| None, |_, _| {}).await,
        None
    );

    tasks.spawn(async move {
        (
            actor,
            (),
            Err(EmbeddedDriverError::Engine(
                harness::engine::EngineError::InvalidFunctionCall,
            )),
        )
    });
    let failure = drain_embedded_shutdown(&mut tasks, Duration::from_secs(1), |_| None, |_, _| {})
        .await
        .unwrap();
    assert!(
        failure.contains("malformed Responses tool call item"),
        "{failure}"
    );
    assert!(failure.contains("42"), "{failure}");

    tasks.spawn(async move {
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
    let failure = drain_embedded_shutdown(&mut tasks, Duration::from_secs(1), |_| None, |_, _| {})
        .await
        .unwrap();
    assert!(failure.contains("claim settlement failed"), "{failure}");
}

#[tokio::test(start_paused = true)]
async fn embedded_shutdown_timeout_does_not_wait_for_unconfirmed_abort() {
    let mut tasks: JoinSet<((), (), Result<(), embedded_service::EmbeddedDriverError>)> =
        JoinSet::new();
    tasks.spawn(async {
        (
            (),
            (),
            Err(embedded_service::EmbeddedDriverError::Host(
                "prior driver failure".into(),
            )),
        )
    });
    tasks.spawn(std::future::pending());
    let failure = drain_embedded_shutdown(&mut tasks, Duration::from_secs(1), |_| None, |_, _| {})
        .await
        .unwrap();
    assert!(failure.contains("cleanup unconfirmed"), "{failure}");
    assert!(failure.contains("prior driver failure"), "{failure}");
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
    tasks.spawn(async move { (actors[0], (), Ok(())) });
    tasks.spawn(async move {
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
    tasks.spawn(std::future::pending());
    let failure = drain_embedded_shutdown(
        &mut tasks,
        Duration::from_secs(1),
        |_| None,
        |actor, release| {
            answer_release_waiters(&mut waiters, actor, release);
        },
    )
    .await
    .unwrap();
    assert!(failure.contains("claim settlement failed"));
    assert!(failure.contains("cleanup unconfirmed"));
    assert_eq!(waiters.len(), 1);
    assert!(waiters.contains_key(&actors[2]));
    retain_unsettled_release_waiters(&mut waiters);
    assert!(waiters.is_empty());
    let mut replies = replies.into_iter();
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
    let mut task_actors = HashMap::from([(task.id(), actor)]);
    let failure = drain_embedded_shutdown(
        &mut tasks,
        Duration::from_secs(1),
        |task_id| task_actors.remove(&task_id),
        |actor, release| {
            answer_release_waiters(&mut waiters, actor, release);
        },
    )
    .await
    .unwrap();
    assert!(failure.contains("driver lost before reporting cleanup"));
    assert!(failure.contains("42"), "{failure}");
    assert_eq!(waiters.len(), 1);
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
    assert!(application_supervisor::settle_embedded_shutdown(
        actor,
        wrong_task.id(),
        Ok(()),
        &owners,
        &mut waiters,
    )
    .is_some());
    assert!(application_supervisor::settle_embedded_shutdown(
        other,
        task.id(),
        Ok(()),
        &owners,
        &mut waiters,
    )
    .is_some());
    assert_eq!(observed_resource_release(actor, &owners), None);
    assert_eq!(waiters.len(), 2);
    assert_eq!(
        application_supervisor::settle_embedded_shutdown(
            actor,
            task.id(),
            Ok(()),
            &owners,
            &mut waiters,
        ),
        None
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
    assert!(application_supervisor::settle_embedded_shutdown(
        actor,
        task.id(),
        Err(failure),
        &owners,
        &mut waiters,
    )
    .is_some());
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
