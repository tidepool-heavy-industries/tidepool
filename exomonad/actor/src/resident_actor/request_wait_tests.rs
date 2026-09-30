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
        let registry = Arc::new(RequestRegistry::default());
        let owner = ActorRef::first(ActorId(41));
        let target = ActorRef::first(ActorId(42));
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, notices) = registry.register_watch(owner, vec![request]).unwrap();
        assert!(notices.is_empty());
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
