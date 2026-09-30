//! Drain acknowledgement belongs to the exact target after RPC admission.

use super::*;

pub(crate) enum DrainWaitEvent {
    Settled {
        result: Result<(), KernelInvocationFailure>,
        deliver: bool,
    },
    Cancelled,
    Retired(ActorTerminal),
    RetiredAfterSettlement {
        result: Result<(), KernelInvocationFailure>,
        terminal: ActorTerminal,
    },
}

pub(crate) async fn wait_drain_event(
    target: &LocalActorRef,
    control: &Arc<crate::WorkbenchExecutionControl>,
    retirement: &crate::RetainedActorExit,
) -> DrainWaitEvent {
    tokio::select! {
        biased;
        terminal = retirement.wait_requested_shutdown() => DrainWaitEvent::Retired(terminal),
        () = control.wait_for_cancellation() => DrainWaitEvent::Cancelled,
        result = target.drain() => {
            match retirement.claim_before_shutdown(|| control.claim_expiry()) {
                Ok(deliver) => DrainWaitEvent::Settled { result, deliver },
                Err(terminal) => DrainWaitEvent::RetiredAfterSettlement { result, terminal },
            }
        }
    }
}

fn drain_disposition(
    result: &Result<(), KernelInvocationFailure>,
) -> WorkbenchOperationDisposition {
    match result {
        Ok(()) => WorkbenchOperationDisposition::Committed,
        Err(KernelInvocationFailure::Rejected { .. }) => WorkbenchOperationDisposition::Rejected,
        Err(_) => WorkbenchOperationDisposition::Unknown,
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
    let reason = match wait_drain_event(&target, &control, &kernel.retained_exit()).await {
        DrainWaitEvent::Settled { result, deliver } => {
            disposition = drain_disposition(&result);
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
        DrainWaitEvent::RetiredAfterSettlement { result, terminal } => {
            disposition = drain_disposition(&result);
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
