use super::*;
use crate::request::{RequestId, ResponseFailure};
use crate::{ActorExitKind, ActorId, ActorRef, ActorTerminal, Incarnation, RetainedActorExit};
use std::task::Poll;
use std::time::Duration;

struct Fixture {
    registry: Arc<RequestRegistry>,
    owner: ActorRef,
    target: ActorRef,
    request: RequestId,
    watch: WatchId,
    control: Arc<crate::WorkbenchExecutionControl>,
    retirement: RetainedActorExit,
    groups: crate::ForkGroupRegistry,
}

impl Fixture {
    fn new() -> Self {
        Self::with_watch(false, true)
    }

    fn with_watch(transient: bool, notify_owner: bool) -> Self {
        let registry = Arc::new(RequestRegistry::default());
        let owner = ActorRef::first(ActorId(41));
        let target = ActorRef::first(ActorId(42));
        let request =
            registry.reserve_labeled_with_reporting(owner, target, "request".into(), notify_owner);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let watch = if transient {
            registry
                .register_transient_watch(
                    owner,
                    vec![vec![(
                        request,
                        crate::request::WatchRequirement::Response {
                            allow_failure: false,
                        },
                    )]],
                )
                .unwrap()
        } else {
            let (watch, notices) = registry.register_watch(owner, vec![request]).unwrap();
            assert!(notices.is_empty());
            watch
        };
        let control = crate::WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        Self {
            registry,
            owner,
            target,
            request,
            watch,
            control,
            retirement: RetainedActorExit::new(),
            groups: crate::ForkGroupRegistry::new(crate::ActorLineageRegistry::default()),
        }
    }

    fn complete(&self) {
        self.registry
            .begin_reply(self.target, self.request)
            .unwrap();
        self.registry.finish_reply(self.request, None);
    }

    async fn wait(&self) -> WatchWaitEvent {
        tokio::time::timeout(
            Duration::from_secs(2),
            wait_watch_event(
                &self.registry,
                self.owner,
                self.watch,
                &self.control,
                &self.retirement,
                &self.groups,
                None,
            ),
        )
        .await
        .expect("watch selector must observe the retained transition")
    }
}

fn deferred_child(
    groups: &crate::ForkGroupRegistry,
    owner: ActorRef,
    target: ActorRef,
    boundary: tidepool_runtime::session::WorkbenchForkBoundary,
) -> crate::ForkGroupId {
    let (group, paths) = groups
        .begin_at_boundary(
            owner,
            crate::ActorPath::parse("root/deferred").unwrap(),
            vec![crate::ActorPathSegment::new("child").unwrap()],
            None,
            boundary,
        )
        .unwrap();
    groups.claim(group, owner, &paths[0].allocated).unwrap();
    groups.attach_child(group, owner, target).unwrap();
    group
}

#[tokio::test]
async fn own_deferred_readiness_is_refused_without_waiting_or_cancelling_work() {
    let fixture = Fixture::with_watch(true, true);
    let lease = TransientWatchLease::new(&fixture.registry, fixture.owner, fixture.watch);
    let boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
        "thread".into(),
        "request".into(),
        "call".into(),
    );
    deferred_child(
        &fixture.groups,
        fixture.owner,
        fixture.target,
        boundary.clone(),
    );
    assert_eq!(
        guard_deferred_target(
            &fixture.groups,
            fixture.owner,
            Some(&boundary),
            fixture.target
        ),
        Err(DeferredWaitRefusal {
            target: fixture.target
        })
    );
    assert!(matches!(wait_watch_event(
        &fixture.registry, fixture.owner, fixture.watch, &fixture.control,
        &fixture.retirement, &fixture.groups, Some(&boundary),
    ).await, WatchWaitEvent::Refused(DeferredWaitRefusal { target }) if target == fixture.target));
    assert!(!fixture.control.cancellation_requested());
    assert!(matches!(
        fixture.registry.observe_watch(fixture.owner, fixture.watch),
        Ok(WatchObservation::Pending(_))
    ));
    drop(lease);
    assert!(!fixture.registry.retains_watch(fixture.owner, fixture.watch));
    assert!(matches!(
        fixture
            .registry
            .observe_response(fixture.owner, fixture.request),
        Ok(crate::request::ResponseObservation::Pending(_))
    ));
}

#[tokio::test]
async fn deferred_refusal_preserves_cancellation_and_retirement_priority() {
    for retired in [false, true] {
        let fixture = Fixture::new();
        let boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
            "thread".into(),
            "request".into(),
            "call".into(),
        );
        deferred_child(
            &fixture.groups,
            fixture.owner,
            fixture.target,
            boundary.clone(),
        );
        fixture.control.request_cancellation();
        if retired {
            fixture.retirement.request_shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "retirement before deferred refusal".into(),
                diagnostic: None,
            });
        }
        let event = wait_watch_event(
            &fixture.registry,
            fixture.owner,
            fixture.watch,
            &fixture.control,
            &fixture.retirement,
            &fixture.groups,
            Some(&boundary),
        )
        .await;
        assert!(if retired {
            matches!(event, WatchWaitEvent::Retired(_))
        } else {
            matches!(event, WatchWaitEvent::Cancelled)
        });
        assert!(fixture.control.cancellation_requested());
    }
}

#[tokio::test]
async fn unrelated_publication_and_committed_children_remain_awaitable() {
    for committed in [false, true] {
        let fixture = Fixture::new();
        let boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
            "thread".into(),
            "request".into(),
            "call".into(),
        );
        let child_boundary = if committed {
            boundary.clone()
        } else {
            tidepool_runtime::session::WorkbenchForkBoundary::external(
                "thread".into(),
                "request".into(),
                "other-call".into(),
            )
        };
        let group = deferred_child(
            &fixture.groups,
            fixture.owner,
            fixture.target,
            child_boundary,
        );
        if committed {
            fixture.groups.request_commit(group, fixture.owner).unwrap();
            fixture
                .groups
                .gate(group, fixture.target)
                .unwrap()
                .mark_ready()
                .unwrap();
            fixture
                .groups
                .publish_groups(&[group], fixture.owner)
                .unwrap();
        }
        assert_eq!(
            guard_deferred_target(
                &fixture.groups,
                fixture.owner,
                Some(&boundary),
                fixture.target
            ),
            Ok(())
        );
        let mut waiting = Box::pin(wait_watch_event(
            &fixture.registry,
            fixture.owner,
            fixture.watch,
            &fixture.control,
            &fixture.retirement,
            &fixture.groups,
            Some(&boundary),
        ));
        assert!(matches!(futures_util::poll!(&mut waiting), Poll::Pending));
        fixture.complete();
        assert!(matches!(
            waiting.await,
            WatchWaitEvent::Resume(Ok(WatchObservation::Ready(_)))
        ));
    }
}

#[tokio::test]
async fn any_of_can_complete_elsewhere_but_all_of_retains_own_publication_dependency() {
    for any_of in [false, true] {
        let fixture = Fixture::new();
        let unrelated = ActorRef::first(ActorId(43));
        let request = fixture.registry.reserve(fixture.owner, unrelated);
        fixture
            .registry
            .mark_queued(fixture.owner, unrelated, request)
            .unwrap();
        fixture.registry.present(unrelated, request).unwrap();
        let requirement = crate::request::WatchRequirement::Response {
            allow_failure: false,
        };
        let dependencies = if any_of {
            vec![vec![(fixture.request, requirement), (request, requirement)]]
        } else {
            vec![
                vec![(fixture.request, requirement)],
                vec![(request, requirement)],
            ]
        };
        let watch = fixture
            .registry
            .register_transient_watch(fixture.owner, dependencies)
            .unwrap();
        let boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
            "thread".into(),
            "request".into(),
            "call".into(),
        );
        deferred_child(
            &fixture.groups,
            fixture.owner,
            fixture.target,
            boundary.clone(),
        );
        let mut waiting = Box::pin(wait_watch_event(
            &fixture.registry,
            fixture.owner,
            watch,
            &fixture.control,
            &fixture.retirement,
            &fixture.groups,
            Some(&boundary),
        ));
        if any_of {
            assert!(matches!(futures_util::poll!(&mut waiting), Poll::Pending));
            fixture.registry.begin_reply(unrelated, request).unwrap();
            fixture.registry.finish_reply(request, None);
            assert!(matches!(
                waiting.await,
                WatchWaitEvent::Resume(Ok(WatchObservation::Ready(_)))
            ));
        } else {
            assert!(matches!(waiting.await, WatchWaitEvent::Refused(_)));
        }
    }
}

#[tokio::test]
async fn settlement_before_or_after_subscription_claims_the_original_control() {
    for subscribe_first in [false, true] {
        let fixture = Fixture::new();
        if !subscribe_first {
            fixture.complete();
        }
        let mut wait = Box::pin(fixture.wait());
        if subscribe_first {
            assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
            fixture.complete();
        }
        assert!(matches!(
            wait.await,
            WatchWaitEvent::Resume(Ok(WatchObservation::Ready(values))) if values.is_empty()
        ));
        assert!(
            !fixture.control.claim_expiry(),
            "the original control is claimed only once"
        );
        assert!(
            fixture.control.request_cancellation(),
            "readiness does not settle the still-active invocation"
        );
        assert!(fixture.registry.retains_watch(fixture.owner, fixture.watch));
    }
}

#[tokio::test]
async fn unavailable_observation_keeps_its_exact_request_and_failure() {
    let fixture = Fixture::new();
    let mut wait = Box::pin(fixture.wait());
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    fixture
        .registry
        .abandon_response(fixture.owner, fixture.request)
        .unwrap();
    assert!(matches!(
        wait.await,
        WatchWaitEvent::Resume(Ok(WatchObservation::Unavailable { request, failure: ResponseFailure::Abandoned }))
            if request == fixture.request
    ));
}

#[tokio::test]
async fn cancellation_does_not_acknowledge_cleanup_or_cancel_the_target_request() {
    let fixture = Fixture::new();
    let mut wait = Box::pin(fixture.wait());
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    assert!(fixture.control.request_cancellation());
    assert!(matches!(wait.await, WatchWaitEvent::Cancelled));
    assert!(
        fixture.control.cancellation_requested(),
        "only consumed native abort may acknowledge cancellation"
    );
    assert!(matches!(
        fixture.registry.observe_watch(fixture.owner, fixture.watch),
        Ok(WatchObservation::Pending(_))
    ));
    fixture.complete();
    assert_eq!(
        fixture.registry.observe_watch(fixture.owner, fixture.watch),
        Ok(WatchObservation::Ready(Vec::new()))
    );
}

#[tokio::test]
async fn cancellation_before_ready_never_resumes_even_when_both_wakes_are_ready() {
    let fixture = Fixture::new();
    assert!(fixture.control.request_cancellation());
    fixture.complete();
    assert!(matches!(fixture.wait().await, WatchWaitEvent::Cancelled));
    assert!(fixture.control.cancellation_requested());
    assert_eq!(
        fixture.registry.observe_watch(fixture.owner, fixture.watch),
        Ok(WatchObservation::Ready(Vec::new()))
    );
}

#[tokio::test]
async fn prior_retirement_wins_when_watch_settlement_is_also_ready() {
    let fixture = Fixture::new();
    let terminal = ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "owner retirement before ready watch".into(),
        diagnostic: None,
    };
    fixture.retirement.request_shutdown(terminal.clone());
    fixture.complete();
    assert!(
        matches!(fixture.wait().await, WatchWaitEvent::Retired(observed) if observed == terminal)
    );
    assert!(!fixture.control.cancellation_requested());
    assert!(matches!(
        fixture.registry.observe_watch(fixture.owner, fixture.watch),
        Ok(WatchObservation::Ready(_))
    ));
}

#[tokio::test]
async fn issued_sibling_retirement_beats_ready_watch_in_both_cleanup_orders() {
    for target_first in [true, false] {
        let fixture = crate::resident_actor::invocation_work::tests::Fixture::start().await;
        let target = fixture.spawn_request_child().await;
        let waiting = fixture.spawn_request_child().await;
        let registry = &fixture.environment.requests;
        let owner = waiting.identity();
        let request = registry.reserve_labeled_with_reporting(
            owner,
            target.identity(),
            "watched sibling".into(),
            false,
        );
        registry
            .mark_queued(owner, target.identity(), request)
            .unwrap();
        registry.present(target.identity(), request).unwrap();
        let (watch, notices) = registry
            .register_watch_labeled(owner, "sibling settlement".into(), vec![(request, true)])
            .unwrap();
        assert!(notices.is_empty());
        let control = crate::WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        let mut wait = Box::pin(wait_watch_event(
            registry,
            owner,
            watch,
            &control,
            waiting.terminal(),
            &fixture.environment.fork_groups,
            None,
        ));
        assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
        let terminal = ActorTerminal::new(ActorExitKind::Cancelled, "selected sibling retirement");
        let selected = if target_first {
            vec![target.clone(), waiting.clone()]
        } else {
            vec![waiting.clone(), target.clone()]
        };
        let batch = crate::kernel::RetirementBatch::issue(
            selected
                .into_iter()
                .map(|child| (child, terminal.clone()))
                .collect(),
        );
        assert_eq!(
            target.terminal().requested_shutdown(),
            Some(terminal.clone())
        );
        assert_eq!(
            waiting.terminal().requested_shutdown(),
            Some(terminal.clone())
        );
        assert!(target.terminal().get().is_none());
        assert!(waiting.terminal().get().is_none());
        assert!(target.terminal().cleanup().is_none());
        assert!(waiting.terminal().cleanup().is_none());
        let mut children = batch.into_actors().into_iter();
        let (first, intent) = children.next().unwrap();
        let shutdown =
            tokio::time::timeout(Duration::from_secs(2), first.shutdown_with_cleanup(intent))
                .await
                .expect("responsive sibling must run its actual shutdown hook")
                .unwrap();
        assert!(shutdown.cleanup.is_confirmed());
        assert_eq!(shutdown.terminal, terminal);
        // The actual actor_stopped hook, rather than an injected reply or exit,
        // made the parked watch ready before it is polled again.
        assert!(matches!(
            registry.observe_watch(owner, watch),
            Ok(WatchObservation::Ready(_))
        ));
        let event = tokio::time::timeout(Duration::from_secs(2), wait)
            .await
            .unwrap();
        assert!(matches!(event, WatchWaitEvent::Retired(observed) if observed == terminal));
        assert!(!control.cancellation_requested());
        assert!(
            control.claim_expiry(),
            "retirement must not claim ready native work"
        );
        for (child, intent) in children {
            let shutdown =
                tokio::time::timeout(Duration::from_secs(2), child.shutdown_with_cleanup(intent))
                    .await
                    .unwrap()
                    .unwrap();
            assert!(shutdown.cleanup.is_confirmed());
            assert_eq!(shutdown.terminal, terminal);
        }
        assert!(target.terminal().cleanup().unwrap().is_confirmed());
        assert!(waiting.terminal().cleanup().unwrap().is_confirmed());
        fixture.finish().await;
    }
}

#[tokio::test]
async fn retirement_preserves_the_owner_terminal_without_claiming_or_acknowledging() {
    let fixture = Fixture::new();
    let retirement = RetainedActorExit::new();
    let mut wait = Box::pin(wait_watch_event(
        &fixture.registry,
        fixture.owner,
        fixture.watch,
        &fixture.control,
        &retirement,
        &fixture.groups,
        None,
    ));
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    let terminal = ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "exact owning retirement".into(),
        diagnostic: None,
    };
    retirement.request_shutdown(terminal.clone());
    let event = tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .unwrap();
    assert!(matches!(event, WatchWaitEvent::Retired(observed) if observed == terminal));
    assert!(!fixture.control.cancellation_requested());
    assert!(fixture.registry.retains_watch(fixture.owner, fixture.watch));
}

#[tokio::test]
async fn dropping_the_wait_preserves_watch_and_later_target_settlement() {
    let fixture = Fixture::new();
    let mut wait = Box::pin(fixture.wait());
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    drop(wait);
    assert!(!fixture.control.cancellation_requested());
    assert!(fixture.registry.retains_watch(fixture.owner, fixture.watch));
    fixture.complete();
    assert!(matches!(
        fixture.wait().await,
        WatchWaitEvent::Resume(Ok(WatchObservation::Ready(_)))
    ));
    let weak = Arc::downgrade(&fixture.registry);
    drop(fixture);
    assert!(
        weak.upgrade().is_none(),
        "dropped subscription creates no independent registry owner"
    );
}

#[tokio::test]
async fn stale_watch_and_replacement_incarnation_remain_typed_refusals() {
    let fixture = Fixture::new();
    let replacement = ActorRef {
        id: fixture.owner.id,
        incarnation: Incarnation(fixture.owner.incarnation.0 + 1),
    };
    let replacement_control = crate::WorkbenchExecutionControl::untracked();
    replacement_control.arm_sleep();
    let refused = wait_watch_event(
        &fixture.registry,
        replacement,
        fixture.watch,
        &replacement_control,
        &fixture.retirement,
        &fixture.groups,
        None,
    )
    .await;
    assert!(matches!(
        refused,
        WatchWaitEvent::Resume(Err(ReplyError::WrongIncarnation))
    ));
    assert!(fixture.registry.retains_watch(fixture.owner, fixture.watch));

    let mut wait = Box::pin(fixture.wait());
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    fixture.complete();
    fixture
        .registry
        .forget_terminal_actor_metadata(fixture.owner)
        .unwrap();
    assert!(matches!(
        wait.await,
        WatchWaitEvent::Resume(Err(ReplyError::Stale))
    ));
}

#[tokio::test]
async fn dropping_a_direct_wait_releases_only_its_subscription() {
    let fixture = Fixture::new();
    let transient = fixture
        .registry
        .register_transient_watch(
            fixture.owner,
            vec![vec![(
                fixture.request,
                crate::request::WatchRequirement::Response {
                    allow_failure: false,
                },
            )]],
        )
        .unwrap();
    let lease = TransientWatchLease::new(&fixture.registry, fixture.owner, transient);
    let mut waiting = Box::pin(fixture.registry.await_watch(fixture.owner, transient));
    assert!(matches!(futures_util::poll!(&mut waiting), Poll::Pending));
    drop(lease);
    assert!(matches!(waiting.await, Err(ReplyError::Stale)));
    assert!(!fixture.registry.retains_watch(fixture.owner, transient));
    assert!(fixture.registry.retains_watch(fixture.owner, fixture.watch));
    fixture.complete();
    assert!(matches!(
        fixture.registry.observe_watch(fixture.owner, fixture.watch),
        Ok(WatchObservation::Ready(_))
    ));
}

#[tokio::test]
async fn direct_readiness_wakes_without_a_named_watch_notice_and_preserves_typed_failure() {
    let registry = Arc::new(RequestRegistry::default());
    let owner = ActorRef::first(ActorId(81));
    let target = ActorRef::first(ActorId(82));
    let request = registry.reserve(owner, target);
    registry.mark_queued(owner, target, request).unwrap();
    registry.present(target, request).unwrap();
    let watch = registry
        .register_transient_watch(
            owner,
            vec![vec![(
                request,
                crate::request::WatchRequirement::Response {
                    allow_failure: false,
                },
            )]],
        )
        .unwrap();
    let control = crate::WorkbenchExecutionControl::untracked();
    control.arm_sleep();
    let retirement = RetainedActorExit::new();
    let groups = crate::ForkGroupRegistry::new(crate::ActorLineageRegistry::default());
    let mut waiting = Box::pin(wait_watch_event(
        &registry,
        owner,
        watch,
        &control,
        &retirement,
        &groups,
        None,
    ));
    assert!(matches!(futures_util::poll!(&mut waiting), Poll::Pending));
    let (_, notices) = registry.abandon_response(owner, request).unwrap();
    assert!(notices.is_empty());
    assert!(matches!(waiting.await,
        WatchWaitEvent::Resume(Ok(WatchObservation::Unavailable {
            request: observed, failure: ResponseFailure::Abandoned,
        })) if observed == request));
    registry.release_transient_watch(owner, watch).unwrap();
    assert_eq!(
        registry.release_transient_watch(owner, watch),
        Err(ReplyError::Stale)
    );
}

#[test]
fn transient_release_checks_exact_owner_and_never_removes_named_watch() {
    let fixture = Fixture::new();
    let transient = fixture
        .registry
        .register_transient_watch(fixture.owner, vec![])
        .unwrap();
    let replacement = ActorRef {
        id: fixture.owner.id,
        incarnation: Incarnation(fixture.owner.incarnation.0 + 1),
    };
    assert_eq!(
        fixture
            .registry
            .release_transient_watch(replacement, transient),
        Err(ReplyError::WrongIncarnation)
    );
    assert_eq!(
        fixture
            .registry
            .release_transient_watch(fixture.owner, fixture.watch),
        Err(ReplyError::Unauthorized)
    );
    assert!(fixture.registry.retains_watch(fixture.owner, fixture.watch));
    fixture
        .registry
        .release_transient_watch(fixture.owner, transient)
        .unwrap();
}

#[tokio::test]
async fn cancelled_direct_wait_restores_one_owner_notice_even_when_settlement_wins_the_registry_race(
) {
    for complete_first in [false, true] {
        let fixture = Fixture::with_watch(true, true);
        let lease = TransientWatchLease::new(&fixture.registry, fixture.owner, fixture.watch);
        if complete_first {
            fixture.complete();
        }
        assert!(fixture.control.request_cancellation());
        if !complete_first {
            fixture.complete();
        }
        assert!(fixture.registry.take_settlement_notifications().is_empty());
        assert!(matches!(fixture.wait().await, WatchWaitEvent::Cancelled));
        drop(lease);
        let notices = fixture.registry.take_settlement_notifications();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].request, fixture.request);
        assert!(fixture.registry.take_settlement_notifications().is_empty());
    }
}

#[tokio::test]
async fn captured_direct_wait_consumes_the_owner_wake_without_emitting_a_later_notice() {
    let fixture = Fixture::with_watch(true, true);
    fixture.complete();
    assert!(matches!(
        fixture.wait().await,
        WatchWaitEvent::Resume(Ok(WatchObservation::Ready(_)))
    ));
    fixture
        .registry
        .release_transient_watch(fixture.owner, fixture.watch)
        .unwrap();
    assert!(fixture.registry.take_settlement_notifications().is_empty());
}

#[tokio::test]
async fn overlapping_direct_waits_restore_the_notice_only_after_the_last_cancelled_subscription() {
    let fixture = Fixture::with_watch(true, true);
    let second = fixture
        .registry
        .register_transient_watch(
            fixture.owner,
            vec![vec![(
                fixture.request,
                crate::request::WatchRequirement::Response {
                    allow_failure: false,
                },
            )]],
        )
        .unwrap();
    fixture.complete();
    fixture
        .registry
        .release_transient_watch(fixture.owner, fixture.watch)
        .unwrap();
    assert!(fixture.registry.take_settlement_notifications().is_empty());
    fixture
        .registry
        .release_transient_watch(fixture.owner, second)
        .unwrap();
    assert_eq!(fixture.registry.take_settlement_notifications().len(), 1);
}

#[tokio::test]
async fn named_watch_and_silent_reporting_keep_their_policy_after_direct_wait_cancellation() {
    let fixture = Fixture::new();
    let direct = fixture
        .registry
        .register_transient_watch(
            fixture.owner,
            vec![vec![(
                fixture.request,
                crate::request::WatchRequirement::Response {
                    allow_failure: false,
                },
            )]],
        )
        .unwrap();
    fixture.complete();
    fixture
        .registry
        .release_transient_watch(fixture.owner, direct)
        .unwrap();
    assert!(fixture.registry.take_settlement_notifications().is_empty());
    assert!(matches!(
        fixture.registry.observe_watch(fixture.owner, fixture.watch),
        Ok(WatchObservation::Ready(_))
    ));

    let silent = Fixture::with_watch(true, false);
    silent.complete();
    silent
        .registry
        .release_transient_watch(silent.owner, silent.watch)
        .unwrap();
    assert!(silent.registry.take_settlement_notifications().is_empty());
}

#[tokio::test]
async fn dropped_direct_wait_publishes_restored_owner_notice_without_another_event() {
    let fixture = Fixture::with_watch(true, true);
    let (deployments, mut receive) = mpsc::channel(4);
    let mut lease = TransientWatchLease::new(&fixture.registry, fixture.owner, fixture.watch);
    lease.deployments = Some(deployments);
    fixture.complete();
    assert!(fixture.registry.take_settlement_notifications().is_empty());
    drop(lease);
    let event = tokio::time::timeout(Duration::from_secs(2), receive.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, LocalResidentDeployment::SettlementChanged { notification }
        if notification.owner == fixture.owner && notification.request == fixture.request)
    );
    assert!(receive.try_recv().is_err());
}

#[test]
fn direct_command_wait_cancellation_restores_notice_but_capture_consumes_it() {
    for captured in [false, true] {
        let registry = RequestRegistry::default();
        let owner = ActorRef::first(ActorId(91));
        let request = registry.reserve_command_settlement(owner, "job".into(), true);
        let watch = registry
            .register_transient_watch(
                owner,
                vec![vec![(
                    request,
                    crate::request::WatchRequirement::Response {
                        allow_failure: false,
                    },
                )]],
            )
            .unwrap();
        assert!(registry
            .settle_command(request, "exit 0".into(), Some("revision".into()))
            .is_empty());
        assert!(registry.take_settlement_notifications().is_empty());
        if captured {
            registry.claim_transient_watch_wake(owner, watch).unwrap();
        }
        registry.release_transient_watch(owner, watch).unwrap();
        let notices = registry.take_settlement_notifications();
        assert_eq!(notices.len(), usize::from(!captured));
        if let Some(notice) = notices.first() {
            assert_eq!(notice.command_job.as_deref(), Some("job"));
            assert_eq!(notice.target_revision.as_deref(), Some("revision"));
        }
    }
}

fn restore_command_notice(
    registry: &Arc<RequestRegistry>,
    owner: ActorRef,
    job: &str,
    notify_owner: bool,
) -> RequestId {
    let request = registry.reserve_command_settlement(owner, job.into(), notify_owner);
    let watch = registry
        .register_transient_watch(
            owner,
            vec![vec![(
                request,
                crate::request::WatchRequirement::Response {
                    allow_failure: false,
                },
            )]],
        )
        .unwrap();
    let lease = TransientWatchLease::new(registry, owner, watch);
    assert!(registry
        .settle_command(request, format!("{job}: exit 0"), Some("revision".into()))
        .is_empty());
    // Cancellation releases the observation without consuming its wake.
    drop(lease);
    request
}

fn channel_filler(owner: ActorRef) -> LocalResidentDeployment {
    LocalResidentDeployment::Retired {
        actor: owner,
        terminal: ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "fill deployment channel".into(),
            diagnostic: None,
        },
    }
}

#[tokio::test]
async fn cancelled_full_channel_publication_retains_restored_command_notice_for_exactly_one_retry()
{
    let registry = Arc::new(RequestRegistry::default());
    let owner = ActorRef::first(ActorId(92));
    let request = restore_command_notice(&registry, owner, "restored-job", true);
    let (deployments, mut receive) = mpsc::channel(1);
    assert!(deployments.try_send(channel_filler(owner)).is_ok());

    let mut publication = Box::pin(publish_request_notifications(
        &registry,
        &deployments,
        Vec::new(),
    ));
    assert!(matches!(
        futures_util::poll!(&mut publication),
        Poll::Pending
    ));
    drop(publication);
    assert!(registry.has_settlement_notifications());
    assert!(matches!(
        receive.recv().await,
        Some(LocalResidentDeployment::Retired { .. })
    ));

    tokio::time::timeout(
        Duration::from_secs(2),
        publish_request_notifications(&registry, &deployments, Vec::new()),
    )
    .await
    .expect("cancelled publication must release its publisher lock");
    let Some(LocalResidentDeployment::SettlementChanged { notification }) = receive.recv().await
    else {
        panic!("retry must deliver the retained settlement notice");
    };
    assert_eq!(notification.owner, owner);
    assert_eq!(notification.request, request);
    assert_eq!(notification.command_job.as_deref(), Some("restored-job"));
    assert_eq!(notification.target_revision.as_deref(), Some("revision"));
    assert_eq!(
        notification.reply_preview.as_deref(),
        Some("restored-job: exit 0")
    );
    assert_eq!(notification.sequence, notification.watermark);
    assert!(notification.occurred_at_unix_ms > 0);
    assert!(!registry.has_settlement_notifications());
    assert!(receive.try_recv().is_err());

    // A silent command retains its original reporting policy after cancellation.
    restore_command_notice(&registry, owner, "silent-job", false);
    assert!(deployments.try_send(channel_filler(owner)).is_ok());
    tokio::time::timeout(
        Duration::from_secs(2),
        publish_request_notifications(&registry, &deployments, Vec::new()),
    )
    .await
    .expect("an empty notice queue must not wait for channel capacity");
    assert!(matches!(
        receive.try_recv(),
        Ok(LocalResidentDeployment::Retired { .. })
    ));
    assert!(receive.try_recv().is_err());
}

#[tokio::test]
async fn competing_settlement_publishers_preserve_fifo_without_duplicates_or_empty_queue_waits() {
    let registry = Arc::new(RequestRegistry::default());
    let owner = ActorRef::first(ActorId(93));
    let first_request = restore_command_notice(&registry, owner, "first-job", true);
    let second_request = restore_command_notice(&registry, owner, "second-job", true);
    let (deployments, mut receive) = mpsc::channel(1);
    assert!(deployments.try_send(channel_filler(owner)).is_ok());
    let mut first = Box::pin(publish_request_notifications(
        &registry,
        &deployments,
        Vec::new(),
    ));
    let mut second = Box::pin(publish_request_notifications(
        &registry,
        &deployments,
        Vec::new(),
    ));
    assert!(matches!(futures_util::poll!(&mut first), Poll::Pending));
    assert!(matches!(futures_util::poll!(&mut second), Poll::Pending));
    assert!(matches!(
        receive.recv().await,
        Some(LocalResidentDeployment::Retired { .. })
    ));
    assert!(matches!(futures_util::poll!(&mut first), Poll::Pending));
    assert!(matches!(futures_util::poll!(&mut second), Poll::Pending));
    let Some(LocalResidentDeployment::SettlementChanged {
        notification: first_notice,
    }) = receive.recv().await
    else {
        panic!("first queued settlement must publish first");
    };
    assert_eq!(first_notice.request, first_request);
    assert!(matches!(futures_util::poll!(&mut first), Poll::Ready(())));
    // The first publisher has filled the channel with the last notice. The
    // second must finish immediately because there is nothing left to claim.
    assert!(matches!(futures_util::poll!(&mut second), Poll::Ready(())));
    let Some(LocalResidentDeployment::SettlementChanged {
        notification: second_notice,
    }) = receive.recv().await
    else {
        panic!("second queued settlement must publish second");
    };
    assert_eq!(second_notice.request, second_request);
    assert!(first_notice.sequence < second_notice.sequence);
    assert!(!registry.has_settlement_notifications());
    assert!(receive.try_recv().is_err());
}

#[tokio::test]
async fn closed_settlement_channel_preserves_notice_for_a_new_publication_channel() {
    let registry = Arc::new(RequestRegistry::default());
    let owner = ActorRef::first(ActorId(94));
    let request = restore_command_notice(&registry, owner, "retained-job", true);
    let (closed, receive) = mpsc::channel(1);
    drop(receive);
    publish_request_notifications(&registry, &closed, Vec::new()).await;
    assert!(registry.has_settlement_notifications());

    let (deployments, mut receive) = mpsc::channel(1);
    publish_request_notifications(&registry, &deployments, Vec::new()).await;
    assert!(matches!(
        receive.recv().await,
        Some(LocalResidentDeployment::SettlementChanged { notification })
            if notification.request == request
    ));
    assert!(!registry.has_settlement_notifications());
    assert!(receive.try_recv().is_err());
}
