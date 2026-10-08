use super::*;
use crate::command_jobs::CommandControl;
use crate::generated::commands::CommandsReq;
use crate::request::ResourceCleanupOwner;
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

struct CommandRetention {
    kernel: KernelContext,
    jobs: crate::command_jobs::CommandJobs,
    caller: ActorRef,
    id: String,
    marker: ResourceCleanupOwner,
    source: Option<Arc<InvocationWork>>,
    destination: Option<Arc<InvocationWork>>,
    destination_run: bool,
}

impl CommandRetention {
    async fn commit(self) -> Result<(), CommandError> {
        let Self {
            kernel,
            jobs,
            caller,
            id,
            marker,
            source,
            destination,
            destination_run,
        } = self;
        if destination_run {
            // Backend supply captures the original principal and filesystem
            // grants. Until then, retirement still belongs to the old owner.
            jobs.supplied(&id).await?;
            return match source {
                Some(source) => source.transfer_command_to_run(&jobs, &kernel, caller, &id),
                None => jobs.transfer_cleanup_owner_in_context(
                    &kernel,
                    caller,
                    &id,
                    &marker,
                    ResourceCleanupOwner::Run,
                    |_| (),
                ),
            };
        }
        match (source, destination) {
            (Some(source), Some(destination)) => {
                source.transfer_command_to_owner(&destination, &jobs, caller, &id)
            }
            (Some(source), None) => source.transfer_command_to_actor(&jobs, caller, &id),
            (None, Some(destination)) if marker == ResourceCleanupOwner::Run => {
                destination.adopt_run_command(&jobs, &kernel, caller, &id)
            }
            (None, Some(destination)) => destination.adopt_actor_command(&jobs, caller, &id),
            (None, None) => jobs.transfer_cleanup_owner_in_context(
                &kernel,
                caller,
                &id,
                &marker,
                ResourceCleanupOwner::Actor,
                |_| (),
            ),
        }
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) fn prepare_command_ownership(
        &self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        effect_owner: &CurrentEffectOwner<'_>,
        continuation: ResidentHole,
        request: CommandsReq,
    ) -> futures_util::future::BoxFuture<
        'static,
        Result<ResidentOutcome, ResidentActorWorkbenchError>,
    > {
        let permitted = self
            .descriptor
            .capabilities()
            .effect_keys()
            .contains(&crate::ActorEffectKey::Commands);
        let environment = self.environment.clone();
        let context = context.clone();
        let kernel = kernel.clone();
        match request {
            CommandsReq::CommandRetainWith(id, lifetime) => {
                let result = if permitted {
                    self.retain_command(
                        &kernel,
                        context.actor,
                        &context,
                        effect_owner,
                        &id,
                        lifetime,
                    )
                } else {
                    Err(CommandError::CommandUnauthorized)
                };
                Box::pin(async move {
                    let result = match result {
                        Ok(prepared) => prepared.commit().await,
                        Err(error) => Err(error),
                    };
                    environment
                        .runner
                        .resume_value(context, continuation, result)
                        .await
                })
            }
            CommandsReq::CommandStartOwnedWith(spec, lifetime) => {
                let selected = if permitted {
                    self.command_resource_owner(&context, effect_owner, lifetime)
                } else {
                    Err(CommandError::CommandUnauthorized)
                };
                Box::pin(async move {
                    let owner = match selected {
                        Ok(owner) => owner,
                        Err(error) => {
                            return environment
                                .runner
                                .resume_value(context, continuation, Err::<String, _>(error))
                                .await
                        }
                    };
                    if lifetime == crate::WorkerLifetime::RunOwned {
                        let mut result =
                            super::command_settlement::CommandSettlements::new(&environment)
                                .start(&kernel, spec, false, None)
                                .await;
                        if let Ok(id) = &result {
                            let transfer = match environment.commands.supplied(id).await {
                                Ok(()) => environment.commands.transfer_cleanup_owner_in_context(
                                    &kernel,
                                    context.actor,
                                    id,
                                    &ResourceCleanupOwner::Actor,
                                    ResourceCleanupOwner::Run,
                                    |_| (),
                                ),
                                Err(error) => Err(error),
                            };
                            if let Err(error) = transfer {
                                drop(
                                    environment
                                        .commands
                                        .control(context.actor, id, CommandControl::Cancel)
                                        .await,
                                );
                                result = Err(error);
                            }
                        }
                        return environment
                            .runner
                            .resume_value(context, continuation, result)
                            .await;
                    }
                    resolve_command(
                        &environment,
                        &kernel,
                        &context,
                        continuation,
                        CommandsReq::CommandStartWith(spec),
                        permitted,
                        None,
                        owner.as_deref(),
                    )
                    .await
                    .outcome
                })
            }
            _ => unreachable!("command ownership operation"),
        }
    }

    fn command_resource_owner(
        &self,
        context: &ActorSessionContext,
        effect_owner: &CurrentEffectOwner<'_>,
        lifetime: crate::WorkerLifetime,
    ) -> Result<Option<Arc<InvocationWork>>, CommandError> {
        self.resolve_resource_owner(context, effect_owner, lifetime)
            .map_err(|error| CommandError::CommandUnavailable(error.to_string()))
    }

    fn retain_command(
        &self,
        kernel: &KernelContext,
        caller: ActorRef,
        context: &ActorSessionContext,
        effect_owner: &CurrentEffectOwner<'_>,
        id: &str,
        lifetime: crate::WorkerLifetime,
    ) -> Result<CommandRetention, CommandError> {
        let jobs = &self.environment.commands;
        let marker = jobs.cleanup_owner(caller, id)?;
        let destination = self.command_resource_owner(context, effect_owner, lifetime)?;
        let source = match &marker {
            ResourceCleanupOwner::Actor | ResourceCleanupOwner::Run => None,
            marker => Some(
                self.retained_scope_roots()
                    .into_iter()
                    .find_map(|root| root.find_command_owner(marker))
                    .ok_or_else(|| {
                        CommandError::CommandUnavailable(
                            "command cleanup owner is no longer retained".into(),
                        )
                    })?,
            ),
        };
        Ok(CommandRetention {
            kernel: kernel.clone(),
            jobs: jobs.clone(),
            caller,
            id: id.into(),
            marker,
            source,
            destination,
            destination_run: lifetime == crate::WorkerLifetime::RunOwned,
        })
    }

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
            CommandsReq::CommandStartOwnedWith(..) | CommandsReq::CommandRetainWith(..) => {
                answer!(Err::<String, _>(CommandError::CommandUnavailable(
                    "command ownership requires its admitted resource owner".into(),
                )))
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

pub(super) fn ownership_operation(request: &CommandsReq) -> bool {
    matches!(
        request,
        CommandsReq::CommandStartOwnedWith(..) | CommandsReq::CommandRetainWith(..)
    )
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

#[cfg(test)]
mod retention_tests {
    use super::*;
    use crate::resident_actor::invocation_work::tests::Fixture;
    use std::time::Duration;
    use tidepool_bridge_effects::{CommandInput, CommandSourceCapture, CommandSpec};

    #[tokio::test]
    async fn run_retention_waits_for_original_backend_admission_before_publication() {
        let fixture = Fixture::start().await;
        let caller = fixture.actor.identity();
        let jobs = fixture.environment.commands.clone();
        let (id, backend) = jobs
            .start(
                &fixture.kernel,
                CommandSpec {
                    argv: vec!["admission-fixture".into()],
                    directory: None,
                    environment: vec![],
                    memory: 64 * 1024 * 1024,
                    input: CommandInput::ClosedInput,
                    source_capture: CommandSourceCapture::NoCapture,
                },
                None,
            )
            .await
            .unwrap();
        let prepared = CommandRetention {
            kernel: fixture.kernel.clone(),
            jobs: jobs.clone(),
            caller,
            id: id.clone(),
            marker: ResourceCleanupOwner::Actor,
            source: None,
            destination: None,
            destination_run: true,
        };
        let mut transfer = tokio::spawn(prepared.commit());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut transfer)
                .await
                .is_err()
        );
        assert_eq!(
            jobs.cleanup_owner(caller, &id).unwrap(),
            ResourceCleanupOwner::Actor
        );
        backend.supply(Err(CommandError::CommandUnavailable(
            "backend admission refused".into(),
        )));
        tokio::time::timeout(Duration::from_secs(1), transfer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(jobs.owner(&id).unwrap(), caller);
        assert_eq!(
            jobs.cleanup_owner(caller, &id).unwrap(),
            ResourceCleanupOwner::Run
        );
        assert!(matches!(
            jobs.status(caller, &id).await.unwrap(),
            tidepool_bridge_effects::CommandStatus::CommandFinished(_)
        ));
        fixture.finish().await;
    }
}
