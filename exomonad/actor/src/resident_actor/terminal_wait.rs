//! Exact actor terminal observation retains the original native continuation.

use super::*;

enum ExitWaitEvent {
    Observed(ActorTerminal),
    Cancelled,
    Retired(ActorTerminal),
}

async fn wait_exit_event(
    target: &crate::RetainedActorExit,
    control: &Arc<crate::WorkbenchExecutionControl>,
    retirement: &crate::RetainedActorExit,
) -> ExitWaitEvent {
    tokio::select! {
        biased;
        terminal = retirement.wait_requested_shutdown() => ExitWaitEvent::Retired(terminal),
        () = control.wait_for_cancellation() => ExitWaitEvent::Cancelled,
        terminal = target.wait() => {
            match retirement.claim_before_shutdown(|| control.claim_expiry()) {
                Ok(true) => ExitWaitEvent::Observed(terminal),
                Ok(false) => ExitWaitEvent::Cancelled,
                Err(terminal) => ExitWaitEvent::Retired(terminal),
            }
        }
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
    let reason = match wait_exit_event(&terminal, &control, &kernel.retained_exit()).await {
        ExitWaitEvent::Observed(_) => {
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
