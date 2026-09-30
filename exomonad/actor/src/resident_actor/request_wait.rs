//! Request watches park with the original execution control and native hole.

use super::*;
use crate::request::{ReplyError, RequestRegistry, WatchId, WatchObservation};
use std::future::Future;

enum WatchWaitEvent {
    Resume(Result<WatchObservation, ReplyError>),
    Cancelled,
    Retired(crate::ActorTerminal),
}

/// The request registry owns subscription and exact-incarnation validation.
/// Selecting cancellation never acknowledges native continuation cleanup.
async fn wait_watch_event(
    requests: &Arc<RequestRegistry>,
    actor: crate::ActorRef,
    watch: WatchId,
    control: &Arc<crate::WorkbenchExecutionControl>,
    retirement: impl Future<Output = crate::ActorTerminal>,
) -> WatchWaitEvent {
    let waiting = requests.await_watch(actor, watch);
    tokio::pin!(waiting);
    tokio::select! {
        observation = &mut waiting => {
            tracing::debug!(?actor, ?watch, ?observation, "owned watch received settlement");
            if control.claim_expiry() {
                WatchWaitEvent::Resume(observation)
            } else {
                WatchWaitEvent::Cancelled
            }
        }
        () = control.wait_for_cancellation() => WatchWaitEvent::Cancelled,
        terminal = retirement => WatchWaitEvent::Retired(terminal),
    }
}

pub(super) async fn await_watch<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    control: Arc<crate::WorkbenchExecutionControl>,
    poll: crate::request_effect::WatchPoll,
) -> Result<ResidentOutcome, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    tracing::debug!(actor = ?context.actor, watch = ?poll.watch, "owned watch awaiting settlement");
    match wait_watch_event(
        &environment.requests,
        context.actor,
        poll.watch,
        &control,
        kernel.wait_requested_shutdown(),
    )
    .await
    {
        WatchWaitEvent::Resume(observation) => {
            let observation = observation.map(|observation| {
                watch_pending_observation(&environment, poll.watch, observation)
            });
            let outcome = environment
                .runner
                .resume_watch_observation(context, poll.continuation, observation)
                .await;
            control.finish_sleep();
            outcome
        }
        WatchWaitEvent::Cancelled => {
            let (outcome, consumed) = environment
                .runner
                .abort_live(
                    context,
                    poll.continuation,
                    "awaitWatch interrupted by delivered input".into(),
                )
                .await;
            if consumed {
                control.acknowledge_cancellation();
            }
            outcome
        }
        WatchWaitEvent::Retired(terminal) => {
            control.request_cancellation();
            let (outcome, consumed) = environment
                .runner
                .abort_live(
                    context,
                    poll.continuation,
                    format!(
                        "awaitWatch interrupted by actor retirement: {}",
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
#[path = "request_wait_tests.rs"]
mod tests;
