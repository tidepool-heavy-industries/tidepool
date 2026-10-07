use super::*;
use crate::command_jobs::CommandControl;
use crate::generated::commands::CommandsReq;
use tidepool_bridge_effects::CommandError;

/// The owner settles the command operation before evaluating its Haskell continuation.
pub(super) struct CommandResolution {
    pub disposition: WorkbenchOperationDisposition,
    pub outcome: Result<ResidentOutcome, ResidentActorWorkbenchError>,
    /// The job id `Cmd.start` minted, when this resolution answered a
    /// `CommandStartWith` request. The caller records it against the item's
    /// fragment so a sole command-job-typed binder this item installs can be
    /// tagged with the exact job it names — see
    /// `resident_workbench::settle_fragment`.
    pub started_job: Option<String>,
    /// Binding installed by the host for this exact notebook item.
    pub retained_job_binding: Option<crate::resident_workbench::RetainedHostBinding>,
}

pub(super) fn disposition<T>(result: &Result<T, CommandError>) -> WorkbenchOperationDisposition {
    match result {
        Ok(_) | Err(CommandError::CommandOutputPending) => WorkbenchOperationDisposition::Committed,
        Err(
            CommandError::CommandInvalid(_)
            | CommandError::CommandUnauthorized
            | CommandError::CommandInputRejected(_),
        ) => WorkbenchOperationDisposition::Rejected,
        Err(_) => WorkbenchOperationDisposition::Unknown,
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) async fn resolve_command(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        continuation: ResidentHole,
        request: CommandsReq,
        invocation: Option<&super::invocation_work::InvocationWork>,
    ) -> CommandResolution {
        let permitted = self
            .descriptor
            .capabilities()
            .effect_keys()
            .contains(&crate::ActorEffectKey::Commands);
        resolve_command(
            &self.environment,
            kernel,
            context,
            continuation,
            request,
            permitted,
            None,
            invocation,
        )
        .await
    }
}

pub(super) async fn resolve_command<H, O>(
    environment: &ResidentEnvironment<H, O>,
    kernel: &KernelContext,
    context: &ActorSessionContext,
    continuation: ResidentHole,
    request: CommandsReq,
    permitted: bool,
    control: Option<&crate::WorkbenchExecutionControl>,
    invocation: Option<&super::invocation_work::InvocationWork>,
) -> CommandResolution
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let jobs = environment.commands.clone();
    let owner = context.actor;
    let mut settled = WorkbenchOperationDisposition::Unknown;
    let mut started_job = None;
    // The job remains owned by CommandJobs. Cancelling this read-only wait
    // abandons only the cell's observation, before its native input is used.
    macro_rules! observe {
        ($waiting:expr) => {{
            match wait_for_observation(kernel, control, $waiting).await {
                Ok(observation) => observation,
                Err(reason) => {
                    let (outcome, consumed) = environment
                        .runner
                        .abort_live(context.clone(), continuation, reason)
                        .await;
                    if consumed {
                        if let Some(control) = control {
                            control.acknowledge_cancellation();
                        }
                    }
                    settled = match &outcome {
                        Ok(_) => WorkbenchOperationDisposition::Committed,
                        Err(error) => disposition_for_non_command_failure(error),
                    };
                    return outcome;
                }
            }
        }};
    }
    macro_rules! answer {
        ($action:expr) => {{
            let result = if permitted {
                $action
            } else {
                Err(CommandError::CommandUnauthorized)
            };
            settled = disposition(&result);
            environment
                .runner
                .resume_value(context.clone(), continuation, result)
                .await
        }};
    }
    let notify_owner = matches!(&request, CommandsReq::CommandBackgroundWith(..));
    let outcome = async {
        match request {
            // An inspection-only actor runs commands in its read-only
            // project view; the mount, not this handler, prevents writes.
            CommandsReq::CommandStartWith(spec) | CommandsReq::CommandBackgroundWith(spec) => {
                answer!({
                    let started = super::command_settlement::CommandSettlements::new(environment)
                        .start(
                            kernel,
                            spec,
                            notify_owner,
                            if notify_owner { None } else { invocation },
                        )
                        .await;
                    if let Ok(id) = &started {
                        started_job = Some(id.clone());
                    }
                    started
                })
            }
            CommandsReq::CommandStatusWith(id) => answer!(jobs.status(owner, &id).await),
            CommandsReq::CommandAwaitWith(id, milliseconds) => {
                answer!(observe!(jobs.wait(owner, &id, milliseconds)))
            }
            CommandsReq::CommandAwaitAndNotifyWith(id, milliseconds) => answer!({
                let settlements = super::command_settlement::CommandSettlements::new(environment);
                let observed = observe!(async {
                    if jobs.owner(&id)? != owner {
                        return Err(CommandError::CommandUnauthorized);
                    }
                    jobs.wait(owner, &id, milliseconds).await
                });
                match observed {
                    Ok(observed) => {
                        settlements
                            .notify_after_observation(owner, &id, observed)
                            .await
                    }
                    Err(error) => Err(error),
                }
            }),
            CommandsReq::CommandWaitWith(id) => answer!({
                let result = observe!(jobs.finished(owner, &id));
                match result {
                    Ok(result) => Ok(tidepool_bridge_effects::CommandObservation {
                        result,
                        output: jobs.output(owner, &id, 1024 * 1024).await,
                    }),
                    Err(error) => Err(error),
                }
            }),
            CommandsReq::CommandPresentWith(id, _) => {
                if !permitted {
                    settled = WorkbenchOperationDisposition::Rejected;
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "command presentation is not authorized".into(),
                    ));
                }
                jobs.status(owner, &id).await.map_err(|error| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "command presentation rejected: {error:?}"
                    ))
                })?;
                settled = WorkbenchOperationDisposition::Committed;
                environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
            }
            // Only the owned workbench can install a command job in its
            // active notebook scope. The ordinary actor interpreter has no
            // such notebook binding owner.
            CommandsReq::CommandRetainJobWith(_) => {
                answer!(Err::<String, _>(CommandError::CommandUnavailable(
                    "command job bindings require an owned workbench".into(),
                ),))
            }
            CommandsReq::CommandOutputWith(id, bytes) => answer!({
                match usize::try_from(bytes) {
                    Ok(bytes) => jobs.output(owner, &id, bytes).await,
                    Err(_) => Err(CommandError::CommandInvalid(
                        "output byte count must be nonnegative".into(),
                    )),
                }
            }),
            CommandsReq::CommandReadWith(id, stream, position) => {
                answer!(jobs.read(owner, &id, stream, position).await)
            }
            CommandsReq::CommandInputWith(id, text) => {
                answer!(jobs.control(owner, &id, CommandControl::Input(text)).await)
            }
            CommandsReq::CommandFinishInputWith(id, text) => {
                answer!(
                    jobs.control(owner, &id, CommandControl::InputAndClose(text))
                        .await
                )
            }
            CommandsReq::CommandCloseInputWith(id) => {
                answer!(jobs.control(owner, &id, CommandControl::CloseInput).await)
            }
            CommandsReq::CommandDetachWith(id) => answer!({
                match invocation {
                    Some(invocation) => invocation.detach_command(&jobs, owner, &id),
                    None => jobs.owner(&id).and_then(|actual| {
                        if actual == owner {
                            Ok(())
                        } else {
                            Err(CommandError::CommandUnauthorized)
                        }
                    }),
                }
            }),
            CommandsReq::CommandCancelWith(id) => {
                answer!(jobs.control(owner, &id, CommandControl::Cancel).await)
            }
            CommandsReq::CommandResizeWith(id, rows, columns) => answer!({
                match (u16::try_from(rows), u16::try_from(columns)) {
                    (Ok(rows), Ok(columns)) if rows > 0 && columns > 0 => {
                        jobs.control(owner, &id, CommandControl::Resize { rows, columns })
                            .await
                    }
                    _ => Err(CommandError::CommandInvalid(
                        "terminal dimensions must be 1..65535".into(),
                    )),
                }
            }),
        }
    }
    .await;
    if let Some(control) = control {
        control.finish_sleep();
    }
    CommandResolution {
        disposition: settled,
        outcome,
        started_job,
        retained_job_binding: None,
    }
}

pub(super) fn waits_for_completion(request: &CommandsReq) -> bool {
    matches!(
        request,
        CommandsReq::CommandAwaitWith(..)
            | CommandsReq::CommandAwaitAndNotifyWith(..)
            | CommandsReq::CommandWaitWith(..)
    )
}

async fn wait_for_observation<F: std::future::Future>(
    kernel: &KernelContext,
    control: Option<&crate::WorkbenchExecutionControl>,
    waiting: F,
) -> Result<F::Output, String> {
    let Some(control) = control else {
        return Ok(waiting.await);
    };
    tokio::pin!(waiting);
    tokio::select! {
        observation = &mut waiting => {
            if control.claim_expiry() {
                Ok(observation)
            } else {
                Err("command observation interrupted by delivered input".into())
            }
        }
        () = control.wait_for_cancellation() => {
            Err("command observation interrupted by delivered input".into())
        }
        terminal = kernel.wait_requested_shutdown() => {
            control.request_cancellation();
            Err(format!("command observation interrupted by actor retirement: {}", terminal.summary))
        }
    }
}
