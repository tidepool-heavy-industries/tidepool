use super::*;
use crate::{ActorExitKind, ActorTerminal, RetainedActorExit};
use std::task::Poll;

fn control() -> Arc<crate::WorkbenchExecutionControl> {
    let control = crate::WorkbenchExecutionControl::untracked();
    control.arm_sleep();
    control
}

fn terminal() -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "exact clock owner retirement".into(),
    }
}

#[tokio::test(start_paused = true)]
async fn timer_expiry_claims_the_original_control_once() {
    let idle_owner = crate::RetainedActorExit::new();
    let control = control();
    let mut wait = Box::pin(wait_sleep_event(
        &control,
        Duration::from_secs(5),
        &idle_owner,
    ));
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(matches!(wait.await, SleepWaitEvent::Expired));
    assert!(!control.claim_expiry());
    assert!(!control.request_cancellation());
    assert!(!control.cancellation_requested());
}

#[tokio::test(start_paused = true)]
async fn cancellation_before_expiry_wins_when_both_wakes_are_ready() {
    let idle_owner = crate::RetainedActorExit::new();
    let control = control();
    let mut wait = Box::pin(wait_sleep_event(
        &control,
        Duration::from_secs(5),
        &idle_owner,
    ));
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    assert!(control.request_cancellation());
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(matches!(wait.await, SleepWaitEvent::Cancelled));
    assert!(control.cancellation_requested());
    assert!(!control.claim_expiry());
}

#[tokio::test(start_paused = true)]
async fn expiry_before_late_cancellation_never_becomes_abort_cleanup() {
    let idle_owner = crate::RetainedActorExit::new();
    let control = control();
    assert!(matches!(
        wait_sleep_event(&control, Duration::ZERO, &idle_owner).await,
        SleepWaitEvent::Expired
    ));
    assert!(!control.request_cancellation());
    assert!(matches!(
        wait_sleep_event(&control, Duration::from_secs(300), &retiring_owner()).await,
        SleepWaitEvent::Expired
    ));
    assert!(!control.cancellation_requested());
    // A failed late abort must not acknowledge cancellation: the owning timer
    // already selected native resume. Native resume still owns finish_sleep.
    control.finish_sleep();
    control.arm_sleep();
    assert!(control.request_cancellation());
}

#[tokio::test(start_paused = true)]
async fn cancellation_wake_preserves_unacknowledged_native_cleanup() {
    let idle_owner = crate::RetainedActorExit::new();
    let control = control();
    assert!(control.request_cancellation());
    assert!(matches!(
        wait_sleep_event(&control, Duration::from_secs(300), &idle_owner).await,
        SleepWaitEvent::Cancelled
    ));
    assert!(control.cancellation_requested());
    // The pure selector cannot claim that the actual native hole was consumed.
    assert!(matches!(
        wait_sleep_event(&control, Duration::from_secs(300), &idle_owner).await,
        SleepWaitEvent::Cancelled
    ));
    assert!(control.cancellation_requested());
}

#[tokio::test(start_paused = true)]
async fn retirement_claims_cancellation_and_keeps_the_exact_terminal() {
    let control = control();
    let retirement = RetainedActorExit::new();
    let mut wait = Box::pin(wait_sleep_event(
        &control,
        Duration::from_secs(300),
        &retirement,
    ));
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    let requested = terminal();
    retirement.request_shutdown(requested.clone());
    assert!(matches!(wait.await, SleepWaitEvent::Retired(observed) if observed == requested));
    assert!(control.cancellation_requested());
    assert!(!control.claim_expiry());
}

#[tokio::test(start_paused = true)]
async fn dropped_timer_retains_no_independent_control_owner() {
    let idle_owner = crate::RetainedActorExit::new();
    let original = control();
    let weak = Arc::downgrade(&original);
    let mut wait = Box::pin(wait_sleep_event(
        &original,
        Duration::from_secs(300),
        &idle_owner,
    ));
    assert!(matches!(futures_util::poll!(&mut wait), Poll::Pending));
    drop(wait);
    assert!(!original.cancellation_requested());
    assert!(original.request_cancellation());
    drop(original);
    assert!(weak.upgrade().is_none());
}

#[tokio::test(start_paused = true)]
async fn one_cancelled_execution_never_claims_another_timer() {
    let idle_owner = crate::RetainedActorExit::new();
    let first = control();
    let second = control();
    assert!(first.request_cancellation());
    assert!(matches!(
        wait_sleep_event(&first, Duration::from_secs(300), &idle_owner).await,
        SleepWaitEvent::Cancelled
    ));
    assert!(matches!(
        wait_sleep_event(&second, Duration::ZERO, &idle_owner).await,
        SleepWaitEvent::Expired
    ));
    assert!(first.cancellation_requested());
    assert!(!second.cancellation_requested());
    assert!(!second.request_cancellation());
}

fn retiring_owner() -> crate::RetainedActorExit {
    let owner = crate::RetainedActorExit::new();
    owner.request_shutdown(terminal());
    owner
}

#[tokio::test(start_paused = true)]
async fn prior_retirement_wins_when_timer_expiry_is_also_ready() {
    let control = control();
    let owner = retiring_owner();
    assert!(matches!(
        wait_sleep_event(&control, Duration::ZERO, &owner).await,
        SleepWaitEvent::Retired(observed) if observed == terminal()
    ));
    assert!(control.cancellation_requested());
    assert!(!control.claim_expiry());
}
