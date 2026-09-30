//! Timed waits retain the original execution control and native continuation.

use super::*;
use std::future::Future;
use std::time::Duration;

enum SleepWaitEvent {
    Expired,
    Cancelled,
    Retired(crate::ActorTerminal),
}

/// Selecting a wake claims its original control boundary, never native cleanup.
async fn wait_sleep_event(
    control: &Arc<crate::WorkbenchExecutionControl>,
    duration: Duration,
    retirement: impl Future<Output = crate::ActorTerminal>,
) -> SleepWaitEvent {
    let timer = tokio::time::sleep(duration);
    tokio::pin!(timer);
    tokio::select! {
        () = &mut timer => {
            if control.claim_expiry() {
                SleepWaitEvent::Expired
            } else {
                SleepWaitEvent::Cancelled
            }
        }
        () = control.wait_for_cancellation() => SleepWaitEvent::Cancelled,
        terminal = retirement => {
            if control.request_cancellation() || control.cancellation_requested() {
                SleepWaitEvent::Retired(terminal)
            } else {
                SleepWaitEvent::Expired
            }
        }
    }
}

pub(super) async fn await_sleep<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    control: Arc<crate::WorkbenchExecutionControl>,
    continuation: ResidentHole,
    duration: Duration,
) -> Result<ResidentOutcome, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    match wait_sleep_event(&control, duration, kernel.wait_requested_shutdown()).await {
        SleepWaitEvent::Expired => {
            let outcome = environment.runner.resume_unit(context, continuation).await;
            control.finish_sleep();
            outcome
        }
        SleepWaitEvent::Cancelled => {
            let (outcome, consumed) = environment
                .runner
                .abort_live(
                    context,
                    continuation,
                    "sleep interrupted by delivered input".into(),
                )
                .await;
            if consumed {
                control.acknowledge_cancellation();
            }
            outcome
        }
        SleepWaitEvent::Retired(terminal) => {
            let (outcome, consumed) = environment
                .runner
                .abort_live(
                    context,
                    continuation,
                    format!(
                        "sleep interrupted by actor retirement: {}",
                        terminal.summary
                    ),
                )
                .await;
            if consumed {
                control.acknowledge_cancellation();
            }
            outcome
        }
    }
}

#[cfg(test)]
#[path = "clock_wait_tests.rs"]
mod tests;
