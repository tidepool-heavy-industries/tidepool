use super::*;
use std::{task::Poll, time::Duration};

fn completed(summary: &str) -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: summary.into(),
    }
}

fn control() -> Arc<crate::WorkbenchExecutionControl> {
    let control = crate::WorkbenchExecutionControl::untracked();
    control.arm_sleep();
    control
}

#[tokio::test]
async fn exact_terminal_is_observed_before_or_after_subscription() {
    for subscribe_first in [false, true] {
        let retirement = crate::RetainedActorExit::new();
        let target = crate::RetainedActorExit::new();
        let control = control();
        let terminal = completed("original child");
        if !subscribe_first {
            target.publish(terminal.clone()).unwrap();
        }
        let mut wait = Box::pin(wait_exit_event(&target, &control, &retirement));
        if subscribe_first {
            assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
            target.publish(terminal.clone()).unwrap();
        }
        let event = tokio::time::timeout(Duration::from_secs(2), wait)
            .await
            .unwrap();
        assert!(matches!(event, ExitWaitEvent::Observed(observed) if observed == terminal));
        assert!(!control.request_cancellation());
        assert_eq!(target.get(), Some(terminal));
    }
}

#[tokio::test]
async fn prior_cancellation_wins_without_changing_or_acknowledging_the_target() {
    let retirement = crate::RetainedActorExit::new();
    let target = crate::RetainedActorExit::new();
    let control = control();
    let mut wait = Box::pin(wait_exit_event(&target, &control, &retirement));
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    assert!(control.request_cancellation());
    let terminal = completed("child completed independently");
    target.publish(terminal.clone()).unwrap();
    assert!(matches!(wait.await, ExitWaitEvent::Cancelled));
    assert!(control.cancellation_requested());
    assert_eq!(target.get(), Some(terminal));
    assert!(target.cleanup().is_none());
}

#[tokio::test]
async fn prior_owner_retirement_wins_when_target_exit_is_also_ready() {
    let target = crate::RetainedActorExit::new();
    let retirement = crate::RetainedActorExit::new();
    let control = control();
    let owner_terminal = ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "owner retired before terminal observation".into(),
    };
    retirement.request_shutdown(owner_terminal.clone());
    let target_terminal = completed("independent child");
    target.publish(target_terminal.clone()).unwrap();
    assert!(matches!(
        wait_exit_event(&target, &control, &retirement).await,
        ExitWaitEvent::Retired(observed) if observed == owner_terminal
    ));
    assert!(!control.cancellation_requested());
    assert_eq!(target.get(), Some(target_terminal));
}

#[tokio::test]
async fn original_owner_retirement_does_not_establish_native_cleanup() {
    let target = crate::RetainedActorExit::new();
    let retirement = crate::RetainedActorExit::new();
    let control = control();
    let mut wait = Box::pin(wait_exit_event(&target, &control, &retirement));
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    let terminal = ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "original owner retires".into(),
    };
    retirement.request_shutdown(terminal.clone());
    let event = tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .unwrap();
    assert!(matches!(event, ExitWaitEvent::Retired(observed) if observed == terminal));
    assert!(!control.cancellation_requested());
    assert!(target.get().is_none());
}

#[tokio::test]
async fn dropping_the_wait_preserves_later_observation() {
    let retirement = crate::RetainedActorExit::new();
    let target = crate::RetainedActorExit::new();
    let control = control();
    let mut wait = Box::pin(wait_exit_event(&target, &control, &retirement));
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    drop(wait);
    assert!(!control.cancellation_requested());
    let terminal = completed("later terminal");
    target.publish(terminal.clone()).unwrap();
    assert!(matches!(
        wait_exit_event(&target, &control, &retirement).await,
        ExitWaitEvent::Observed(observed) if observed == terminal
    ));
}

#[tokio::test]
async fn a_successor_terminal_cannot_satisfy_the_original_observation() {
    let retirement = crate::RetainedActorExit::new();
    let original = crate::RetainedActorExit::new();
    let successor = crate::RetainedActorExit::new();
    original.retain_successor(ActorRef {
        id: crate::ActorId(41),
        incarnation: crate::Incarnation(2),
    });
    let control = control();
    let mut wait = Box::pin(wait_exit_event(&original, &control, &retirement));
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    successor.publish(completed("successor")).unwrap();
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    let terminal = completed("original");
    original.publish(terminal.clone()).unwrap();
    assert!(matches!(wait.await, ExitWaitEvent::Observed(observed) if observed == terminal));
    assert_eq!(successor.get(), Some(completed("successor")));
}
