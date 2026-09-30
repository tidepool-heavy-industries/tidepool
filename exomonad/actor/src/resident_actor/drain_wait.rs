//! Drain acknowledgement belongs to the exact target after RPC admission.

use super::*;
use std::future::Future;

pub(crate) enum DrainWaitEvent {
    Settled {
        result: Result<(), KernelInvocationFailure>,
        deliver: bool,
    },
    Cancelled,
    Retired(ActorTerminal),
}

pub(crate) async fn wait_drain_event(
    target: &LocalActorRef,
    control: &Arc<crate::WorkbenchExecutionControl>,
    retirement: impl Future<Output = ActorTerminal>,
) -> DrainWaitEvent {
    if control.cancellation_requested() {
        return DrainWaitEvent::Cancelled;
    }
    tokio::select! {
        result = target.drain() => DrainWaitEvent::Settled {
            result,
            deliver: control.claim_expiry(),
        },
        () = control.wait_for_cancellation() => DrainWaitEvent::Cancelled,
        terminal = retirement => DrainWaitEvent::Retired(terminal),
    }
}

pub(super) async fn await_drain<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    control: Arc<crate::WorkbenchExecutionControl>,
    continuation: ResidentHole,
    target: LocalActorRef,
) -> commands::CommandResolution
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let mut disposition = WorkbenchOperationDisposition::Unknown;
    let reason = match wait_drain_event(&target, &control, kernel.wait_requested_shutdown()).await {
        DrainWaitEvent::Settled { result, deliver } => {
            disposition = match &result {
                Ok(()) => WorkbenchOperationDisposition::Committed,
                Err(KernelInvocationFailure::Rejected { .. }) => {
                    WorkbenchOperationDisposition::Rejected
                }
                Err(_) => WorkbenchOperationDisposition::Unknown,
            };
            if deliver {
                let outcome = match result {
                    Ok(()) => environment.runner.resume_unit(context, continuation).await,
                    Err(error) => Err(ResidentActorWorkbenchError::ActorProtocol(
                        error.to_string(),
                    )),
                };
                control.finish_sleep();
                return commands::CommandResolution {
                    disposition,
                    outcome,
                    started_job: None,
                };
            }
            "drainActor interrupted by delivered input".into()
        }
        DrainWaitEvent::Cancelled => "drainActor interrupted by delivered input".into(),
        DrainWaitEvent::Retired(terminal) => {
            control.request_cancellation();
            format!(
                "drainActor interrupted by actor retirement: {}",
                terminal.summary
            )
        }
    };
    // Dropping an accepted drain waiter does not withdraw its target fence.
    // Only an observed acknowledgement settles that operation's receipt.
    let (outcome, consumed) = environment
        .runner
        .abort_live(context, continuation, reason)
        .await;
    if consumed {
        control.acknowledge_cancellation();
    }
    commands::CommandResolution {
        disposition,
        outcome,
        started_job: None,
    }
}
