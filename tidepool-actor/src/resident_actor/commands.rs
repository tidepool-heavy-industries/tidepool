use super::*;
use crate::command_jobs::CommandControl;
use crate::generated::commands::CommandsReq;
use crate::resident_workbench::CommandObservationStop;
use tidepool_bridge_effects::CommandError;

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
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let permitted = self
            .descriptor
            .effective_role()
            .effect_keys()
            .contains(&crate::ActorEffectKey::Commands);
        let jobs = self.environment.commands.clone();
        let owner = context.actor;
        macro_rules! answer {
            ($action:expr) => {{
                let result = if permitted {
                    $action
                } else {
                    Err(CommandError::CommandUnauthorized)
                };
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, result)
                    .await
            }};
        }
        match request {
            CommandsReq::CommandStartWith(spec) => answer!({
                if self.descriptor.effective_role().native_tools()
                    == crate::NativeToolClass::InspectionOnly
                {
                    Err(CommandError::CommandUnauthorized)
                } else {
                    match jobs.start(kernel, spec).await {
                        Ok((id, request)) => {
                            if self
                                .environment
                                .deployments
                                .send(LocalResidentDeployment::CommandBackend(request.clone()))
                                .is_err()
                            {
                                request.supply(Err(CommandError::CommandUnavailable(
                                    "native host unavailable".into(),
                                )));
                            }
                            Ok(id)
                        }
                        Err(error) => Err(error),
                    }
                }
            }),
            CommandsReq::CommandStatusWith(id) => answer!(jobs.status(owner, &id).await),
            CommandsReq::CommandAwaitWith(id, milliseconds) => {
                answer!(jobs.wait(owner, &id, milliseconds).await)
            }
            CommandsReq::CommandForegroundWith(id) => {
                let observed = if permitted {
                    jobs.wait(owner, &id, 30_000).await
                } else {
                    Err(CommandError::CommandUnauthorized)
                };
                match observed {
                    Ok(tidepool_bridge_effects::CommandStatus::CommandFinished(result)) => {
                        match jobs.output(owner, &id, 1024 * 1024).await {
                            Ok(output) => {
                                self.environment
                                    .runner
                                    .resume_value(
                                        context.clone(),
                                        continuation,
                                        Ok::<_, CommandError>(
                                            tidepool_bridge_effects::CommandObservation {
                                                result,
                                                output,
                                            },
                                        ),
                                    )
                                    .await
                            }
                            Err(error) => {
                                self.environment
                                    .runner
                                    .stop_command_observation(
                                        context.clone(),
                                        continuation,
                                        id,
                                        CommandObservationStop::OutputUnavailable(error),
                                    )
                                    .await
                            }
                        }
                    }
                    Ok(_) => {
                        self.environment
                            .runner
                            .stop_command_observation(
                                context.clone(),
                                continuation,
                                id,
                                CommandObservationStop::Deadline,
                            )
                            .await
                    }
                    Err(error) => {
                        self.environment
                            .runner
                            .resume_value(
                                context.clone(),
                                continuation,
                                Err::<tidepool_bridge_effects::CommandObservation, _>(error),
                            )
                            .await
                    }
                }
            }
            CommandsReq::CommandPresentWith(id, _) => {
                if !permitted {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "command presentation is not authorized".into(),
                    ));
                }
                jobs.status(owner, &id).await.map_err(|error| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "command presentation rejected: {error:?}"
                    ))
                })?;
                self.environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
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
}
