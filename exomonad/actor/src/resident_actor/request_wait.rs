//! Request watches park with the original execution control and native hole.

use super::*;
use crate::request::{ReplyError, RequestRegistry, WatchId, WatchObservation};

/// A dropped or cancelled direct wait releases its subscription, never the
/// producing request. Ready capture transfers release to the invocation and
/// Haskell's forget operation because progress still reads this snapshot.
struct TransientWatchLease {
    requests: Arc<RequestRegistry>,
    actor: crate::ActorRef,
    watch: WatchId,
    armed: bool,
    deployments: Option<mpsc::Sender<LocalResidentDeployment>>,
}

impl TransientWatchLease {
    fn new(requests: &Arc<RequestRegistry>, actor: crate::ActorRef, watch: WatchId) -> Self {
        Self {
            requests: Arc::clone(requests),
            actor,
            watch,
            armed: requests.is_transient_watch(actor, watch),
            deployments: None,
        }
    }
}

impl Drop for TransientWatchLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(notifications) = self
            .requests
            .release_transient_watch(self.actor, self.watch)
        {
            if let Some(deployments) = self.deployments.clone() {
                let requests = Arc::clone(&self.requests);
                tokio::spawn(async move {
                    publish_request_notifications(&requests, &deployments, notifications).await;
                });
            }
        }
    }
}

enum WatchWaitEvent {
    Resume(Result<WatchObservation, ReplyError>),
    Cancelled,
    Retired(crate::ActorTerminal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("child {target} starts after this invocation settles; await it in a later invocation")]
pub(super) struct DeferredWaitRefusal {
    target: ActorRef,
}

pub(super) fn guard_deferred_target(
    groups: &crate::ForkGroupRegistry,
    owner: ActorRef,
    boundary: Option<&tidepool_runtime::session::WorkbenchForkBoundary>,
    target: ActorRef,
) -> Result<(), DeferredWaitRefusal> {
    if boundary.is_some_and(|boundary| {
        groups
            .pending_children_at_boundary(owner, boundary)
            .contains(&target)
    }) {
        Err(DeferredWaitRefusal { target })
    } else {
        Ok(())
    }
}

/// The request registry owns subscription and exact-incarnation validation.
/// Selecting cancellation never acknowledges native continuation cleanup.
async fn wait_watch_event(
    requests: &Arc<RequestRegistry>,
    actor: crate::ActorRef,
    watch: WatchId,
    control: &Arc<crate::WorkbenchExecutionControl>,
    retirement: &crate::RetainedActorExit,
    _groups: &crate::ForkGroupRegistry,
    _boundary: Option<&tidepool_runtime::session::WorkbenchForkBoundary>,
) -> WatchWaitEvent {
    let waiting = requests.await_watch(actor, watch);
    tokio::pin!(waiting);
    tokio::select! {
        biased;
        terminal = retirement.wait_requested_shutdown() => WatchWaitEvent::Retired(terminal),
        () = control.wait_for_cancellation() => WatchWaitEvent::Cancelled,
        observation = &mut waiting => {
            tracing::debug!(?actor, ?watch, ?observation, "owned watch received settlement");
            match retirement.claim_before_shutdown(|| control.claim_expiry()) {
                Ok(true) => {
                    let observation = observation.and_then(|observation| {
                        if requests.is_transient_watch(actor, watch) {
                            requests.claim_transient_watch_wake(actor, watch)?;
                        }
                        Ok(observation)
                    });
                    WatchWaitEvent::Resume(observation)
                }
                Ok(false) => WatchWaitEvent::Cancelled,
                Err(terminal) => WatchWaitEvent::Retired(terminal),
            }
        }
    }
}

pub(super) async fn await_watch<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    control: Arc<crate::WorkbenchExecutionControl>,
    poll: crate::request_effect::WatchPoll,
    boundary: Option<tidepool_runtime::session::WorkbenchForkBoundary>,
) -> Result<ResidentOutcome, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    tracing::debug!(actor = ?context.actor, watch = ?poll.watch, "owned watch awaiting settlement");
    let mut transient = TransientWatchLease::new(&environment.requests, context.actor, poll.watch);
    transient.deployments = Some(environment.deployments.clone());
    match wait_watch_event(
        &environment.requests,
        context.actor,
        poll.watch,
        &control,
        &kernel.retained_exit(),
        &environment.fork_groups,
        boundary.as_ref(),
    )
    .await
    {
        WatchWaitEvent::Resume(observation) => {
            transient.armed = false;
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
