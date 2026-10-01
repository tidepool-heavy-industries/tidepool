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
            ),
        )
        .await
        .expect("watch selector must observe the retained transition")
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
            !fixture.control.request_cancellation(),
            "ready won this execution's boundary"
        );
        assert!(
            !fixture.control.claim_expiry(),
            "the original control is claimed only once"
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
async fn retirement_preserves_the_owner_terminal_without_claiming_or_acknowledging() {
    let fixture = Fixture::new();
    let retirement = RetainedActorExit::new();
    let mut wait = Box::pin(wait_watch_event(
        &fixture.registry,
        fixture.owner,
        fixture.watch,
        &fixture.control,
        &retirement,
    ));
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    let terminal = ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "exact owning retirement".into(),
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
    let mut waiting = Box::pin(wait_watch_event(
        &registry,
        owner,
        watch,
        &control,
        &retirement,
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
