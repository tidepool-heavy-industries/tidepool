//! Exact actor terminal observation retains the original native continuation.

use super::*;
use std::future::Future;

enum ExitWaitEvent {
    Observed(ActorTerminal),
    Cancelled,
    Retired(ActorTerminal),
}

async fn wait_exit_event(
    target: &crate::RetainedActorExit,
    control: &Arc<crate::WorkbenchExecutionControl>,
    retirement: impl Future<Output = ActorTerminal>,
) -> ExitWaitEvent {
    tokio::select! {
        terminal = target.wait() => {
            if control.claim_expiry() {
                ExitWaitEvent::Observed(terminal)
            } else {
                ExitWaitEvent::Cancelled
            }
        }
        () = control.wait_for_cancellation() => ExitWaitEvent::Cancelled,
        terminal = retirement => ExitWaitEvent::Retired(terminal),
    }
}

pub(super) async fn await_exit<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    control: Arc<crate::WorkbenchExecutionControl>,
    continuation: ResidentHole,
    terminal: crate::RetainedActorExit,
) -> Result<ResidentOutcome, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let reason = match wait_exit_event(&terminal, &control, kernel.wait_requested_shutdown()).await
    {
        ExitWaitEvent::Observed(terminal) => {
            let outcome = environment
                .runner
                .resume_terminal(context, continuation, terminal)
                .await;
            control.finish_sleep();
            return outcome;
        }
        ExitWaitEvent::Cancelled => "awaitExit interrupted by delivered input".into(),
        ExitWaitEvent::Retired(terminal) => {
            control.request_cancellation();
            format!(
                "awaitExit interrupted by actor retirement: {}",
                terminal.summary
            )
        }
    };
    let (outcome, consumed) = environment
        .runner
        .abort_live(context, continuation, reason)
        .await;
    if consumed {
        control.acknowledge_cancellation();
    }
    outcome
}

#[cfg(test)]
#[path = "terminal_wait_tests.rs"]
mod tests;
