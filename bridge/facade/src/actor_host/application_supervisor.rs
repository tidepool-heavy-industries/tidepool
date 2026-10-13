use super::*;

#[derive(Debug, thiserror::Error)]
enum RunStoreAdmissionFailure {
    #[error("native run store admission is unavailable; authenticated recovery is required: {0}")]
    Native(#[source] tidepool_runtime::session::StoreError),
    #[error(
        "embedded run store admission is unavailable; authenticated recovery is required: {0}"
    )]
    Embedded(#[source] harness::store::StoreError),
}

/// External provider adapters can render native failures before returning them.
/// The original store owners retain the authoritative admission state.
fn ensure_run_store_admission(
    native: &tidepool_runtime::session::RunRecoveryStore,
    embedded: &harness::store::Store,
) -> Result<(), RunStoreAdmissionFailure> {
    native
        .ensure_ready()
        .map_err(RunStoreAdmissionFailure::Native)?;
    embedded
        .ensure_ready()
        .map_err(RunStoreAdmissionFailure::Embedded)
}

/// Run-store unavailability takes precedence over child-local attachment refusal.
/// Otherwise the actor failure owner performs retirement and records cleanup.
fn refuse_child_attachment(
    installation: &LocalResidentInstallation,
    detail: String,
    native: &tidepool_runtime::session::RunRecoveryStore,
    embedded: &harness::store::Store,
) -> impl std::future::Future<Output = Result<(), RunStoreAdmissionFailure>> + Send + 'static {
    let run_admission = ensure_run_store_admission(native, embedded);
    let actor = installation.actor.clone();
    let admission = installation.spawn_admission.clone();
    async move {
        run_admission?;
        let identity = actor.identity();
        if let Some(admission) = admission {
            admission.fail(detail.clone());
        }
        if let Err(failure) = apply_application_failure(
            actor,
            ExternalApplicationFailure {
                class: ExternalApplicationFailureClass::ToolHostStartup,
                detail,
            },
        )
        .await
        {
            tracing::warn!(actor = ?identity, %failure, "child attachment refusal cleanup remains unconfirmed");
        }
        Ok(())
    }
}

pub(super) async fn run_interactive_applications(
    mut lifecycle: mpsc::Receiver<LocalResidentDeployment>,
    application_owners: InteractiveOwners,
    fleet: InteractiveFleet,
    shutdown: watch::Receiver<ApplicationShutdown>,
    coordination_failed: oneshot::Sender<String>,
    mut root_config: watch::Receiver<ActorHostConfig>,
    mut embedded_service: embedded_service::EmbeddedService,
) -> Result<(), String> {
    let InteractiveFleet {
        provider_forest,
        recovery_store,
        root,
        config,
        run_root,
        worktrees,

        readiness,
        worktree_authority,

        host_graph,
        #[cfg(test)]
        test_observer,
    } = fleet;
    let output_store = embedded_service.runtime.store();
    let base_prompt = FrozenBasePrompt::materialize_selected(
        &run_root,
        config
            .workspace_inputs
            .as_ref()
            .and_then(|inputs| inputs.prompts.get("base"))
            .map(String::as_str),
        config
            .workspace_inputs
            .as_ref()
            .and_then(|inputs| inputs.prompts.get("api-guide"))
            .map(String::as_str),
        config.jev_surface(),
    )
    .map_err(|error| format!("cannot prepare Exomonad base prompt: {error}"))?;
    let mut root_identity = root.identity();
    let mut launch_context = InteractiveLaunchContext {
        base_prompt,
        root: root_identity,
        config,
        run_root,

        worktrees: worktrees.clone(),
    };

    let mut embedded_tasks: JoinSet<(
        ActorRef,
        LocalActorRef,
        Result<(), embedded_service::EmbeddedDriverError>,
    )> = JoinSet::new();
    let (embedded_ready_tx, mut embedded_ready_rx) = mpsc::unbounded_channel::<(
        (ActorRef, provider_attachment::ProviderAttachment),
        LocalActorRef,
        embedded_service::InitialContextReady,
        oneshot::Sender<()>,
    )>();
    let (embedded_lifecycle_tx, mut embedded_lifecycle_rx) =
        embedded_projection::LifecycleSender::channel();
    let mut embedded_projection = embedded_projection::EmbeddedProjection::default();
    let embedded_run = runtime_namespace(&launch_context.run_root);
    {
        let service = &embedded_service;
        let forest = Arc::downgrade(&provider_forest);
        let run = embedded_run.clone();
        service
            .control
            .install_actor_display_expander(Arc::new(move |input| {
                let forest = forest.clone();
                let run = run.clone();
                Box::pin(async move {
                    if input.origin.run != run {
                        return Err("display belongs to a different run".into());
                    }
                    let forest = forest
                        .upgrade()
                        .ok_or_else(|| "native display owner is unavailable".to_owned())?;
                    let actor = ActorRef {
                        id: exomonad_actor::ActorId(input.origin.native_actor),
                        incarnation: exomonad_actor::Incarnation(input.origin.incarnation),
                    };
                    forest
                        .request_expansion(
                            actor,
                            (
                                input.origin.native_actor as i64,
                                input.origin.incarnation as i64,
                                input.display_slot as i64,
                            ),
                            input.key as i64,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(())
                })
            }))
            .map_err(str::to_owned)?;
    }

    // Answer a stopped actor's release wait from its settled Engine state,
    // or when its native driver joins and confirms cleanup.
    let mut release_waiters: HashMap<ActorRef, Vec<Arc<exomonad_actor::ReleaseAwait>>> =
        HashMap::new();
    let mut notifications: JoinSet<(ActorRef, Result<(), String>)> = JoinSet::new();
    let mut browser_commands = BrowserCommandTasks::new();

    let mut health = tokio::time::interval(Duration::from_secs(1));
    {
        let service = &embedded_service;
        let graph = (host_graph)()
            .await
            .map_err(|error| format!("embedded actor graph observation failed: {error}"))?;
        let live_actors = embedded_live_actors(&application_owners);
        embedded_projection
            .publish(
                &service.control,
                &readiness,
                &embedded_run,
                &graph,
                &embedded_projection::LifecycleState::default(),
                |_| None,
                |identity| {
                    service
                        .runtime
                        .store()
                        .embedded_round_frontier(identity)
                        .map(|frontier| {
                            frontier
                                .pending_head
                                .or(frontier.settled_head)
                                .map(|request| request.0)
                        })
                },
                |identity| service.runtime.store().embedded_inference_state(identity),
                |actor| !live_actors.contains(&actor),
            )
            .map_err(|error| format!("embedded actor history projection failed: {error}"))?;
    }
    let test_failure = async {
        #[cfg(test)]
        if let Some(observer) = &test_observer {
            return observer.wait_coordination_failure().await;
        }
        std::future::pending::<String>().await
    };
    tokio::pin!(test_failure);
    let failure = AssertUnwindSafe(async {
        let failure = loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown.clone()) => break None,
            failure = &mut test_failure => break Some(failure),
            changed = root_config.changed() => {
                if changed.is_ok() { launch_context.config = root_config.borrow_and_update().clone(); }
            }
            changed = embedded_lifecycle_rx.changed() => {
                if changed.is_ok() {
                    let states = (*embedded_lifecycle_rx.borrow_and_update()).clone();
                    {
                        let service = &embedded_service;
                        let conversations = embedded_bindings(&application_owners);
                        let live_actors = embedded_live_actors(&application_owners);
                        if let Err(error) = service.control.refresh_completed_model_requests(&service.runtime.store()) {
                            break Some(format!("embedded request projection failed: {error}"));
                        }
                        let graph = match (host_graph)().await {
                            Ok(graph) => graph,
                            Err(error) => break Some(format!("embedded actor graph observation failed: {error}")),
                        };
                        if let Err(error) = embedded_projection.publish(
                            &service.control,
                            &readiness,
                            &embedded_run,
                            &graph,
                            &states,
                            |actor| conversations.get(&actor).and_then(|binding| binding.conversation()).and_then(|conversation| conversation.active_round()),
                            |identity| service.runtime.store().embedded_round_frontier(identity).map(|frontier| frontier.pending_head.or(frontier.settled_head).map(|request| request.0)),
                            |identity| service.runtime.store().embedded_inference_state(identity),
                |actor| !live_actors.contains(&actor),
                        ) {
                            break Some(format!("embedded actor history projection failed: {error}"));
                        }
                    }
                    for actor in states.keys() {
                        if let Some(binding) = embedded_binding(&application_owners, *actor) {
                            schedule_embedded_notification_drain(*actor, binding, &mut notifications);
                        }
                    }
                }
            }
            _ = health.tick() => {
                for (actor, binding) in embedded_bindings(&application_owners) {
                    schedule_embedded_notification_drain(
                        actor,
                        binding,
                        &mut notifications,
                    );
                }
                {
                    let service = &mut embedded_service;
                    if let Err(error) = service.control.refresh_completed_model_requests(&service.runtime.store()) {
                        break Some(format!("embedded request projection failed: {error}"));
                    }
                    if service.server_finished() {
                        let detail = match service.shutdown().await {
                            Ok(()) => "stopped unexpectedly".to_owned(),
                            Err(error) => error,
                        };
                        break Some(format!("embedded browser server failed: {detail}"));
                    }
                    let live_actors = embedded_live_actors(&application_owners);
                    let conversations = embedded_bindings(&application_owners);
                    let graph = match (host_graph)().await {
                        Ok(graph) => graph,
                        Err(error) => break Some(format!("embedded actor graph observation failed: {error}")),
                    };
                    if let Err(error) = drain_embedded_browser_commands(
                        &service.runtime.store(), &service.control, &embedded_run,
                        &graph, &live_actors, &embedded_projection,
                        &conversations, &embedded_lifecycle_tx,
                        &mut browser_commands,
                    ) {
                        break Some(format!("embedded browser command drain failed: {error}"));
                    }
                    if let Err(error) = embedded_projection.publish(
                        &service.control,
                        &readiness,
                        &embedded_run,
                        &graph,
                        &(*embedded_lifecycle_rx.borrow()).clone(),
                        |actor| conversations.get(&actor).and_then(|binding| binding.conversation()).and_then(|conversation| conversation.active_round()),
                        |identity| service.runtime.store().embedded_round_frontier(identity).map(|frontier| frontier.pending_head.or(frontier.settled_head).map(|request| request.0)),
                        |identity| service.runtime.store().embedded_inference_state(identity),
                |actor| !live_actors.contains(&actor),
                    ) {
                        break Some(format!("embedded actor history projection failed: {error}"));
                    }
                }




            }

            command = embedded_service.commands.recv() => {
                let Some(command) = command else {
                    break Some("embedded browser command channel closed".into());
                };
                let service = &embedded_service;
                let store = service.runtime.store();
                match command.command {
                    harness::server::ClientCommand::Host { operation_id, command } => {
                        // A settled retry is readable even when there are no
                        // queued rows left and its actor has already retired.
                        match store.embedded_command(&embedded_run, operation_id) {
                            Ok(Some(record)) if record.command == command => {
                                if let Some(receipt) = record.receipt {
                                    service.control.publish_command_receipt(receipt);
                                }
                            }
                            Ok(_) => {},
                            Err(error) => break Some(format!("embedded command lookup failed: {error}")),
                        }
                    }
                    harness::server::ClientCommand::Submit { .. } => {
                        service.control.publish_command_receipt(harness::server::CommandReceipt {
                            command_id: command.command_id,
                            outcome: harness::server::CommandReceiptOutcome::Refused {
                                target: None,
                                reason: "standalone submit is unavailable while the browser is attached to an embedded host".into(),
                            },
                        });
                    }
                }
                let live_actors = embedded_live_actors(&application_owners);
                let conversations = embedded_bindings(&application_owners);
                let graph = match (host_graph)().await {
                    Ok(graph) => graph,
                    Err(error) => break Some(format!("embedded actor graph observation failed: {error}")),
                };
                if let Err(error) = drain_embedded_browser_commands(
                    &store, &service.control, &embedded_run, &graph,
                    &live_actors, &embedded_projection, &conversations,
                    &embedded_lifecycle_tx,
                    &mut browser_commands,
                ) {
                    break Some(format!("embedded browser command drain failed: {error}"));
                }
            }
            Some(result) = browser_commands.join_next(), if !browser_commands.is_empty() => {
                if let Err(error) = observe_browser_command_join(result, &embedded_service.control) {
                    break Some(error);
                }
            }
            Some(result) = embedded_tasks.join_next_with_id(), if !embedded_tasks.is_empty() => {
                let (task_id, (actor, local_actor, outcome)) = match result {
                    Ok(joined) => joined,
                    Err(error) => {
                        let task_id = error.id();
                        let actor = embedded_actor_for_task(&application_owners, task_id);
                        if let Some(actor) = actor {
                            update_embedded_state(&application_owners, actor, |state| {
                                if state.task_id == Some(task_id) {
                                    state.task_id = None;
                                }
                            });
                        }
                        break Some(match actor {
                            Some(actor) => format!("embedded Engine task for {actor:?} failed: {error}"),
                            None => format!("unattributed embedded Engine task failed: {error}"),
                        });
                    }
                };
                let completion = match settle_embedded_completion(
                    actor, task_id, outcome, &application_owners, &mut release_waiters,
                ) {
                    Ok(completion) => completion,
                    Err(error) => break Some(error),
                };
                if let Err(error) = ensure_run_store_admission(&recovery_store, &output_store) {
                    break Some(error.to_string());
                }
                match completion {
                    EmbeddedTaskCompletion::RecoveryRequired(detail) => {
                        break Some(detail);
                    }
                    EmbeddedTaskCompletion::Completed if local_actor.terminal().get().is_none() => {
                        if let Err(error) = apply_application_failure(
                            local_actor.clone(),
                            ExternalApplicationFailure {
                                class: ExternalApplicationFailureClass::UnexpectedExit,
                                detail: "embedded Engine exited before actor settlement".into(),
                            },
                        ).await {
                            break Some(error);
                        }
                    }
                    EmbeddedTaskCompletion::Completed => {}
                    EmbeddedTaskCompletion::ExecutionFailed(detail)
                    | EmbeddedTaskCompletion::CleanupFailed(detail) => {
                        tracing::error!(?actor, %detail, "embedded Engine failed");
                        if local_actor.terminal().get().is_none() {
                            if let Err(error) = apply_application_failure(
                                local_actor.clone(),
                                ExternalApplicationFailure {
                                    class: ExternalApplicationFailureClass::UnexpectedExit,
                                    detail,
                                },
                            ).await {
                                break Some(error);
                            }
                        }
                    }
                }
                let lifecycle = match local_actor.terminal().get().map(|terminal| terminal.kind) {
                    Some(ActorExitKind::Completed | ActorExitKind::Cancelled) => {
                        harness::server::HostActorLifecycle::Retired
                    }
                    Some(ActorExitKind::Failed) | None => harness::server::HostActorLifecycle::Lost,
                };
                embedded_lifecycle_tx.publish(actor, lifecycle);
                if let Some(binding) = embedded_binding(&application_owners, actor) {
                    update_embedded_state(&application_owners, actor, |state| {
                        if let Some(binding) = state.conversation.as_ref() {
                            binding.mark_retired();
                        }
                    });
                    schedule_embedded_notification_drain(actor, binding, &mut notifications);
                }
            }
            Some(((actor, provider_attachment), local_actor, initial_context_ready, ready_ack)) = embedded_ready_rx.recv() => {
                if let Err(error) = provider_attachment.validate().await {
                    tracing::warn!(?actor, %error, "embedded provider attachment became unavailable");
                    if let Some(cancel) = with_embedded_state(&application_owners, actor, |state| state.cancellation.clone()).flatten() {
                        cancel.send_replace(true);
                    }
                    continue;
                }
                if !embedded_is_live(&application_owners, actor)
                    || with_embedded_state(&application_owners, actor, |state| state.cancellation.is_some()) != Some(true)
                {
                    continue;
                }
                let Ok(_admission) = local_actor.admit_transaction().await else {
                    continue;
                };
                let Some(binding) = embedded_binding(&application_owners, actor) else {
                    tracing::warn!(?actor, "embedded child attachment has no notification owner");
                    continue;
                };
                if !binding.is_live() {
                    tracing::warn!(?actor, "embedded child attachment identity is no longer admitted");
                    continue;
                }
                let activations = match binding.release_initial_context(initial_context_ready) {
                    Ok(activations) => activations,
                    Err(error) => {
                        tracing::warn!(?actor, ?error, "embedded attachment was not accepted");
                        continue;
                    }
                };
                let conversation = binding.conversation().expect("released initial-context gate");
                embedded_projection.attached(actor, conversation.identity());
                embedded_lifecycle_tx.publish(actor, harness::server::HostActorLifecycle::Waiting);
                update_embedded_state(&application_owners, actor, |state| {
                    state.conversation = Some(binding.clone());
                });
                schedule_embedded_notification_drain(actor, binding.clone(), &mut notifications);
                let mut activation_error = None;
                for activation in activations {
                    let sequence = activation.id.sequence();
                    if let Err(error) = conversation
                        .input(
                            &format!("session:{}:{sequence}", activation.request.0),
                            "resident",
                            &activation.message,
                        )
                        .await
                    {
                        activation_error = Some(format!("embedded activation for {actor:?} was not admitted: {error}"));
                        break;
                    }
                }
                if let Some(error) = activation_error {
                    update_embedded_state(&application_owners, actor, |state| {
                        if let Some(binding) = state.conversation.as_ref() {
                            binding.mark_retired();
                        }
                    });
                    tracing::warn!(?actor, %error, "embedded child attachment refused an activation");
                    continue;
                }
                if let Err(error) = provider_attachment.validate().await {
                    tracing::warn!(?actor, %error, "embedded provider readiness became unavailable");
                    if let Some(cancel) = with_embedded_state(&application_owners, actor, |state| state.cancellation.clone()).flatten() {
                        cancel.send_replace(true);
                    }
                    continue;
                }
                if actor == root_identity {
                    readiness.send(ActorHostReadiness::EmbeddedReady { root: actor, address: embedded_service.address }).ok();
                }
                ready_ack.send(()).ok();
            }
            event = lifecycle.recv() => {
                let Some(event) = event else { break None };
                match event {
                    LocalResidentDeployment::DisplayPublished(request) => {
                        let conversation = embedded_binding(&application_owners, request.actor)
                            .map(|binding| binding.identity().clone());
                        display_output::publish(
                            &provider_forest,
                            &output_store,
                            &embedded_run,
                            conversation,
                            Some(&embedded_service.control),
                            &request,
                        );
                    }
                    LocalResidentDeployment::PolicyInstalled(installation) => {
                        tracing::info!(target: "tidepool::actor_host::startup", actor = %installation.actor.identity(), "actor policy installed");
                        #[cfg(test)]
                        if let Some(observer) = &test_observer { observer.installed(&installation); }
                        let provider_attachment = match provider_attachment::ProviderAttachment::admit(
                            Arc::clone(&provider_forest), installation.actor.identity(),
                        ).await {
                            Ok(admission) => admission,
                            Err(error) => {
                                let detail = format!(
                                    "actor {:?} provider attachment is unavailable: {error}",
                                    installation.actor.identity(),
                                );
                                if installation.creator.is_none() { break Some(detail); }
                                if let Err(error) = refuse_child_attachment(&installation, detail, &recovery_store, &output_store).await {
                                    break Some(error.to_string());
                                }
                                continue;
                            },
                        };
                        if installation.creator.is_none() {
                            launch_context.config = root_config.borrow_and_update().clone();
                            root_identity = installation.actor.identity();
                            launch_context.root = root_identity;
                        }
                        if installation.creator.is_none() {
                            worktree_authority.install_grant(
                                installation.actor.identity().into(),
                                ActorWorktreeGrant::Repository,
                            );
                        }
                        {
                            let service = &embedded_service;
                            let settings = service.settings.clone();
                            let actor = installation.actor.identity();
                            application_owners
                                .lock()
                                .entry(actor)
                                .or_insert_with(InteractiveApplicationOwner::embedded);
                            let selected_parent = if actor != root_identity && installation.checkpoint.is_none() && installation.context_parent.is_none() && installation.checkpoint_attachment.is_none() {
                                let identities = embedded_bindings(&application_owners).iter().map(|(actor, conversation)| (*actor, conversation.identity().clone())).collect();
                                let graph = match (host_graph)().await {
                                    Ok(graph) => graph,
                                    Err(error) => break Some(format!("embedded actor graph observation failed: {error}")),
                                };
                                match embedded_context::selected_provider_parent(
                                    &embedded_run, installation.creator.or(installation.supervisor_parent), &graph, &identities,
                                ) {
                                    Ok(parent) => Some(parent),
                                    Err(error) => {
                                        if let Err(error) = refuse_child_attachment(&installation, error, &recovery_store, &output_store).await {
                                            break Some(error.to_string());
                                        }
                                        continue;
                                    },
                                }
                            } else { None };
                            let path = if actor == root_identity {
                                harness::model::AgentPath("/root".into())
                            } else if let Some(parent) = &selected_parent {
                                parent.child_path(actor)
                            } else {
                                let Some(captured) = installation.checkpoint_attachment.as_ref()
                                    .and_then(|attachment| attachment.downcast::<embedded_harness::EmbeddedHostedCheckpoint>()) else {
                                    if let Err(error) = refuse_child_attachment(&installation, format!(
                                        "embedded child {actor:?} has no admitted checkpoint identity"
                                    ), &recovery_store, &output_store).await {
                                        break Some(error.to_string());
                                    }
                                    continue;
                                };
                                captured.child_path(actor)
                            };
                            if embedded_binding(&application_owners, actor).is_none() {
                                let binding = match open_embedded_actor_binding(
                                    &launch_context.config.run_directory,
                                    actor,
                                    path.clone(),
                                ) {
                                    Ok(binding) => binding,
                                    Err(error) => {
                                        let detail = format!(
                                            "embedded actor {actor:?} notification owner could not open: {error}"
                                        );
                                        if installation.creator.is_none() { break Some(detail); }
                                        if let Err(error) = refuse_child_attachment(&installation, detail, &recovery_store, &output_store).await {
                                            break Some(error.to_string());
                                        }
                                        continue;
                                    },
                                };
                                update_embedded_state(&application_owners, actor, |state| {
                                    state.conversation = Some(binding);
                                });
                            }
                            let is_root = actor == root_identity;

                            let model = match installation
                                .model
                                .as_ref()
                                .map(|model| resolve_model(&launch_context.config, model))
                                .transpose()
                            {
                                Ok(Some(model)) => model,
                                Ok(None) => launch_context.config.model.clone(),
                                Err(error) => {
                                    if is_root { break Some(error); }
                                    if let Err(error) = refuse_child_attachment(&installation, error, &recovery_store, &output_store).await {
                                        break Some(error.to_string());
                                    }
                                    continue;
                                },
                            };
                            let effort = match installation.fork_effort.unwrap_or(launch_context.config.effort) {
                                ForkEffort::Low => harness::model::Effort::Low,
                                ForkEffort::Medium => harness::model::Effort::Medium,
                                ForkEffort::High => harness::model::Effort::High,
                            };
                            let mut instructions = developer_instructions_selected(
                                &installation.capabilities,
                                launch_context.config.workspace_inputs.as_ref(),
                                installation.instructions.as_deref(),
                            );
                            append_inheritance_authority(&mut instructions);
                            instructions = format!("{}\n\n{instructions}", launch_context.base_prompt.body());
                            let instructions = orient_launch_instructions(
                                &instructions,
                                &installation.runtime_observation.snapshot(),
                                installation.policy.tools(),
                            );
                            installation.runtime_observation.publish_launch_pending(
                                "attaching the embedded conversation",
                            );
                            installation.runtime_observation.publish_backend_provenance(
                                "embedded-engine".into(),
                                "harness".into(),
                                Some(model.clone()),
                                Some(format!("{effort:?}")),
                            );
                            let local_actor = installation.actor.clone();
                            if is_root {
                                if let Some(error) = embedded_root_attachment_error(
                                    actor,
                                    true,
                                    installation.checkpoint.is_some(),
                                    installation.context_parent,
                                ) {
                                    break Some(error);
                                }
                            } else if installation.checkpoint_attachment.is_none() && installation.context_parent.is_some() {
                                if let Err(error) = refuse_child_attachment(&installation, format!(
                                    "embedded child actor {actor:?} requires a hosted checkpoint"
                                ), &recovery_store, &output_store).await {
                                    break Some(error.to_string());
                                }
                                continue;
                            }
                            let initial_input = installation.initial_user_message.clone();
                            {
                                let queue_admission = match local_actor.admit_transaction().await {
                                    Ok(admission) => admission,
                                    Err(error) => {
                                        if let Err(error) = refuse_child_attachment(&installation, format!(
                                            "embedded child queue admission refused: {error}"
                                        ), &recovery_store, &output_store).await {
                                            break Some(error.to_string());
                                        }
                                        continue;
                                    }
                                };
                                let spawn_admission = installation.spawn_admission.clone();
                                let projection_target = if !is_root && selected_parent.is_none() { Some(match service.projection_target(model.clone()) {
                                    Ok(target) => target,
                                    Err(error) => {
                                        if let Err(error) = refuse_child_attachment(&installation, error, &recovery_store, &output_store).await {
                                            break Some(error.to_string());
                                        }
                                        continue;
                                    }
                                }) } else { None };
                                let binding = embedded_binding(&application_owners, actor).expect("opened startup binding");
                                let runtime = Arc::clone(&service.runtime);
                                let run_root = launch_context.run_root.clone();
                                let ready = embedded_ready_tx.clone();
                                let embedded_lifecycle = embedded_lifecycle_tx.clone();
                                #[cfg(test)]
                                let test_transport = service.test_transport();
                                let (cancel, mut cancellation_rx) = watch::channel(false);
                                update_embedded_state(&application_owners, actor, |state| {
                                    state.cancellation = Some(cancel.clone());
                                    state.live = true;
                                });
                                let task = embedded_tasks.spawn(async move {
                                    let result = async {
                                        if *cancellation_rx.borrow() || local_actor.terminal().get().is_some() {
                                            return Ok(());
                                        }
                                        provider_attachment.validate().await.map_err(|error| error.to_string())?;
                                        let attachment = async {
                                            if is_root {
                                                embedded_service::attach_root_actor(runtime.as_ref(), provider_attachment.clone(), &run_root, path, None, *installation, initial_input, Some(&binding)).await
                                            } else {
                                                match selected_parent {
                                                    Some(parent) => embedded_service::attach_selected_actor(runtime.as_ref(), provider_attachment.clone(), &run_root, parent, *installation, initial_input, &binding).await,
                                                    None => embedded_service::attach_checkpoint_actor(runtime.as_ref(), projection_target.expect("captured projection target"), provider_attachment.clone(), &run_root, *installation, initial_input, &binding).await,
                                                }
                                            }
                                        };
                                        let mut embedded = tokio::select! {
                                            attached = attachment => attached?,
                                            _ = cancellation_rx.changed() => return Ok(()),
                                        };
                                        if *cancellation_rx.borrow() || local_actor.terminal().get().is_some() {
                                            return Ok(());
                                        }
                                        if let Err(error) = provider_attachment.validate().await {
                                            embedded.cancellation.send_replace(true);
                                            return Err(error.to_string().into());
                                        }
                                        embedded.cancellation = cancel;
                                        embedded.cancellation_rx = cancellation_rx;
                                        let (ready_ack, acknowledged) = oneshot::channel();
                                        ready.send(((actor, provider_attachment.clone()), local_actor.clone(), embedded.initial_context_ready.clone(), ready_ack))
                                            .map_err(|_| "embedded attachment owner stopped".to_owned())?;
                                        tokio::select! {
                                            accepted = acknowledged => {
                                                accepted.map_err(|_| "embedded attachment was not accepted".to_owned())?;
                                            }
                                            _ = embedded.cancellation_rx.changed() => return Ok(()),
                                        }
                                        if let Some(admission) = &spawn_admission {
                                            admission.acknowledge(actor)?;
                                        }
                                        let _provider_attachment = provider_attachment;
                                        #[cfg(test)]
                                        let result = if let Some(transport) = test_transport {
                                            embedded_service::drive_conversation_with_transport::<
                                                harness::transport::auth::ChatGptPlanAuth, _
                                            >(
                                                embedded.driver, runtime, &settings, model, effort,
                                                instructions, embedded.cancellation_rx,
                                                embedded_lifecycle, actor, transport,
                                            ).await
                                        } else {
                                            embedded_service::drive_conversation(
                                                embedded.driver, runtime, &settings, model, effort,
                                                instructions, embedded.cancellation_rx,
                                                embedded_lifecycle, actor,
                                            ).await
                                        };
                                        #[cfg(not(test))]
                                        let result = embedded_service::drive_conversation(
                                            embedded.driver, runtime, &settings, model, effort,
                                            instructions, embedded.cancellation_rx,
                                            embedded_lifecycle, actor,
                                        ).await;
                                        result
                                    }.await;
                                    if let Some(admission) = &spawn_admission {
                                        match &result {
                                            Err(error) => admission.fail(error.to_string()),
                                            Ok(()) => admission.fail("actor attachment cancelled before readiness".into()),
                                        }
                                    }
                                    (actor, local_actor, result)
                                });
                                update_embedded_state(&application_owners, actor, |state| {
                                    state.task_id = Some(task.id());
                                });
                                drop(queue_admission);
                                continue;
                            }
                        }


                    }
                    LocalResidentDeployment::SessionReady { activation } => {
                        let actor = activation.id.actor();
                        {
                            let Some(binding) = embedded_binding(&application_owners, actor) else {
                                break Some(format!("embedded actor {actor:?} received activation before its host owner existed"));
                            };
                            let (conversation, activation) = match binding.admit_activation(activation) {
                                embedded_harness::ActivationAdmission::Pending => continue,
                                embedded_harness::ActivationAdmission::Ready { conversation, activation } => (conversation, activation),
                                embedded_harness::ActivationAdmission::Retired => continue,
                            };
                            let sequence = activation.id.sequence();
                            if let Err(error) = conversation
                                .input(
                                    &format!("session:{}:{sequence}", activation.request.0),
                                    "resident",
                                    &activation.message,
                                )
                            .await
                            {
                                if actor != root_identity {
                                    if let Some(cancel) = with_embedded_state(&application_owners, actor, |state| state.cancellation.clone()).flatten() {
                                        cancel.send_replace(true);
                                    }
                                    tracing::warn!(?actor, %error, "embedded child activation was refused");
                                    continue;
                                }
                                break Some(format!("embedded activation for {actor:?} was not admitted: {error}"));
                            }
                            continue;
                        }

                    }

                    LocalResidentDeployment::Retired { actor, terminal } => {
                        retire_interactive_application(
                            actor,
                            terminal,
                            &worktree_authority,
                            &application_owners,
                            &mut notifications,
                            &embedded_lifecycle_tx,
                            launch_context.config.command_resources.as_ref(),
                        ).await;


                    }
                    LocalResidentDeployment::ReleaseAwait(request) => {
                        // `Retired` precedes this on the same channel, so an
                        // owner row either already holds its receipt or has a
                        // retirement in flight. An actor without a row never
                        // held interactive resources.
                        let actor = request.actor;
                        let settled = observed_resource_release(actor, &application_owners);
                        match settled {
                            Some(release) => { request.answer(release); }
                            None => release_waiters.entry(actor).or_default().push(request),
                        }
                    }
                    LocalResidentDeployment::CommandBackend(request) => {
                        // An actor with an agent process of its own runs its
                        // commands inside that process's sandbox. One without
                        // — a record actor started from a notebook, or an
                        // operator workbench — has no sandbox to run in, so it
                        // runs here instead, in whatever worktree it holds
                        // custody of. Borrowing an ancestor's sandbox, which
                        // is what this did before, put the command somewhere
                        // the actor's own worktree is mounted read-only.
                        let owner = request.owner;
                        let resource_client = launch_context.config.command_resources.clone();
                        let host_resources = resource_client.clone();


                        supply_resident_command_backend(
                            request,
                            &launch_context.config,
                            &worktree_authority,
                            &launch_context.worktrees,
                            owner,
                            host_resources,
                        );
                    }


                    LocalResidentDeployment::NotificationSend(command) => {
                        let target = command.target();
                        if let Some(binding) = embedded_binding(&application_owners, target)
                            .filter(|binding| binding.is_live()) {
                            schedule_embedded_notification_send(
                                command,
                                &binding,
                                &mut notifications,
                            );
                        } else {
                            command.rejected(exomonad_actor::NotificationError::Unavailable);
                        }
                    }


                    LocalResidentDeployment::NotificationPoll(command) => {
                        let target = command.receipt().target();
                        let result = match embedded_binding(&application_owners, target) {
                            Some(binding) => {
                                observe_embedded_notification(&command, &binding, target).await
                            }
                            None => Err(exomonad_actor::NotificationError::Unavailable),
                        };
                        command.observed(result);
                    }


                    LocalResidentDeployment::RequestUpdate { delivery } => {
                        if let Some(presentation) = delivery.begin() {
                            presentation.not_presented(
                                "request updates require a native interactive conversation".into(),
                            );
                        }
                    }


                    LocalResidentDeployment::ChildExited { .. } => {}


                    LocalResidentDeployment::WatchChanged { .. } => {}


                    LocalResidentDeployment::SettlementChanged { .. } => {}


                    LocalResidentDeployment::RequestCancellation { .. } => {}
                }
            }

            notified = notifications.join_next(), if !notifications.is_empty() => {


                if let Some(Ok((actor, Err(error)))) = notified {
                    tracing::warn!(?actor, %error, "embedded notification delivery remains pending");
                }
            }
        }
        };
        failure
    })
    .catch_unwind()
    .await
    .unwrap_or_else(|_| Some("interactive application supervisor panicked".into()));

    if let Some(error) = &failure {
        readiness
            .send(ActorHostReadiness::CoordinationFailed {
                root: root_identity,
                error: error.clone(),
            })
            .ok();
    }

    if let Some(error) = &failure {
        let _ = coordination_failed.send(error.clone());
    }
    // Forest retirement needs this consumer to observe exact resource release.
    // Its settled outcomes, rather than the stop request, fence channel closure.
    let resident_cleanup = drain_resident_shutdown(
        &mut lifecycle,
        &mut embedded_tasks,
        &application_owners,
        &worktree_authority,
        &mut notifications,
        &embedded_lifecycle_tx,
        launch_context.config.command_resources.as_ref(),
        &mut release_waiters,
        shutdown.clone(),
    )
    .await;

    close_release_observations(&mut lifecycle, &mut release_waiters, |actor| {
        observed_resource_release(actor, &application_owners)
    });

    let native_retirement = if failure.is_some() {
        NativeRetirement::Preserve
    } else {
        match *shutdown.borrow() {
            ApplicationShutdown::Draining(retirement)
            | ApplicationShutdown::ForestSettled(retirement) => retirement,
            ApplicationShutdown::Running => NativeRetirement::Preserve,
        }
    };
    for owner in application_owners.lock().values_mut() {
        owner.native_retirement = native_retirement;
        owner.cancel();
        if let Some(cancellation) = owner.embedded.cancellation.as_ref() {
            cancellation.send_replace(true);
        }
    }
    let embedded_cleanup = drain_embedded_shutdown(
        &mut embedded_tasks,
        APPLICATION_SHUTDOWN_TIMEOUT,
        &application_owners,
        &mut release_waiters,
    )
    .await;
    let browser_command_cleanup = drain_browser_command_tasks(
        &mut browser_commands,
        &embedded_service.control,
        APPLICATION_SHUTDOWN_TIMEOUT,
    )
    .await;
    let embedded_service_cleanup = embedded_service
        .shutdown()
        .await
        .err()
        .map(|error| format!("embedded browser shutdown: {error}"));

    let notification_cleanup = tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        let mut failure = None;
        while let Some(result) = notifications.join_next().await {
            let result = result
                .map_err(|error| format!("owner notification task: {error}"))
                .and_then(|(_actor, result)| result);
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        failure
    })
    .await
    .unwrap_or_else(|_| Some("owner notification cleanup timed out".into()));

    retain_unsettled_release_waiters(&mut release_waiters);
    let earlier_embedded_cleanup = {
        let failures = application_owners
            .lock()
            .iter()
            .filter_map(|(actor, owner)| {
                owner
                    .embedded
                    .cleanup_failure
                    .as_ref()
                    .map(|error| format!("embedded Engine {actor:?}: {error}"))
            })
            .collect::<Vec<_>>();
        (!failures.is_empty()).then(|| failures.join("; "))
    };
    let cleanup_failures = [
        earlier_embedded_cleanup,
        resident_cleanup.cleanup_failure,
        embedded_cleanup.cleanup_failure,
        browser_command_cleanup,
        embedded_service_cleanup,
        notification_cleanup,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let cleanup_failure = if cleanup_failures.is_empty() {
        None
    } else {
        Some(cleanup_failures.join("; "))
    };
    let driver_failures = [
        resident_cleanup.driver_failure,
        embedded_cleanup.driver_failure,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let failure =
        failure.or_else(|| (!driver_failures.is_empty()).then(|| driver_failures.join("; ")));
    #[cfg(test)]
    if let Some(observer) = &test_observer {
        observer.application_shutdown(match &cleanup_failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        });
    }
    match (failure, cleanup_failure) {
        (Some(error), Some(cleanup)) => Err(format!("{error}; cleanup: {cleanup}")),
        (Some(error), None) | (None, Some(error)) => Err(error),
        (None, None) => Ok(()),
    }
}

fn observe_browser_command_join(
    result: Result<Result<Option<harness::server::CommandReceipt>, String>, tokio::task::JoinError>,
    control: &harness::server::ServerControl,
) -> Result<(), String> {
    let receipt =
        result.map_err(|error| format!("embedded browser command task failed: {error}"))??;
    if let Some(receipt) = receipt {
        control.publish_command_receipt(receipt);
    }
    Ok(())
}

pub(super) async fn drain_browser_command_tasks(
    tasks: &mut BrowserCommandTasks,
    control: &harness::server::ServerControl,
    timeout: Duration,
) -> Option<String> {
    let mut failures = Vec::new();
    if tokio::time::timeout(timeout, async {
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = observe_browser_command_join(result, control) {
                failures.push(error);
            }
        }
    })
    .await
    .is_err()
    {
        // Forest and Engine cleanup already ran with their original event
        // consumer. Only these command observers remain; their durable
        // claimed rows cannot be replayed or reported as confirmed.
        failures.push("embedded browser command observation cleanup timed out".to_owned());
        tasks.abort_all();
        while let Some(result) = tasks.join_next().await {
            match result {
                Err(error) if error.is_cancelled() => {}
                result => {
                    if let Err(error) = observe_browser_command_join(result, control) {
                        failures.push(error);
                    }
                }
            }
        }
    }
    (!failures.is_empty()).then(|| failures.join("; "))
}

async fn retire_interactive_application(
    actor: ActorRef,
    terminal: ActorTerminal,
    worktree_authority: &ActorWorktreeAuthority,
    application_owners: &InteractiveOwners,
    notifications: &mut JoinSet<(ActorRef, Result<(), String>)>,
    embedded_lifecycle_tx: &embedded_projection::LifecycleSender,
    command_resources: Option<&Arc<exomonad_node::command_resources::CommandResourceClient>>,
) {
    worktree_authority.remove_grant(actor.into());
    let binding = with_embedded_state(application_owners, actor, |state| {
        if let Some(cancel) = state.cancellation.take() {
            cancel.send_replace(true);
        }
        if let Some(binding) = state.conversation.as_ref() {
            binding.mark_retired();
        }
        state.conversation.clone()
    })
    .flatten();
    if let Some(binding) = binding {
        schedule_embedded_notification_drain(actor, binding, notifications);
    }
    let lifecycle = if terminal.kind == ActorExitKind::Failed {
        harness::server::HostActorLifecycle::Lost
    } else {
        harness::server::HostActorLifecycle::Retired
    };
    embedded_lifecycle_tx.publish(actor, lifecycle);
    if let Some(resources) = command_resources {
        let producer = format!("{}-{}", actor.id.0, actor.incarnation.0);
        if let Err(error) = resources.seal_producer(&producer).await {
            tracing::warn!(?actor, %error, "command resource producer retirement remains unconfirmed");
        }
    }
    if let Some(owner) = application_owners.lock().get_mut(&actor) {
        owner.retired(terminal);
    }
}

/// Keep the original lifecycle consumer alive while actor-owned retirement
/// obtains its exact host release receipts. Ordinary work is refused here.
pub(super) async fn drain_resident_shutdown<L: Send + 'static>(
    lifecycle: &mut mpsc::Receiver<LocalResidentDeployment>,
    embedded_tasks: &mut JoinSet<(
        ActorRef,
        L,
        Result<(), embedded_service::EmbeddedDriverError>,
    )>,
    application_owners: &InteractiveOwners,
    worktree_authority: &ActorWorktreeAuthority,
    notifications: &mut JoinSet<(ActorRef, Result<(), String>)>,
    embedded_lifecycle_tx: &embedded_projection::LifecycleSender,
    command_resources: Option<&Arc<exomonad_node::command_resources::CommandResourceClient>>,
    release_waiters: &mut HashMap<ActorRef, Vec<Arc<exomonad_actor::ReleaseAwait>>>,
    mut shutdown: watch::Receiver<ApplicationShutdown>,
) -> EmbeddedShutdownOutcome {
    let mut failures = Vec::new();
    let mut driver_failures = Vec::new();
    loop {
        let forest_settled = matches!(*shutdown.borrow(), ApplicationShutdown::ForestSettled(_));
        let event = if forest_settled {
            // No forest cleanup remains entitled to publish another release
            // request. Consume the already-admitted cleanup events before exit.
            match lifecycle.try_recv() {
                Ok(event) => Some(event),
                Err(_) => break,
            }
        } else {
            tokio::select! {
                biased;
                event = lifecycle.recv() => {
                    if event.is_none() {
                        failures.push("resident cleanup consumer closed before forest settlement".into());
                        break;
                    }
                    event
                }
                result = embedded_tasks.join_next_with_id(), if !embedded_tasks.is_empty() => {
                    match result {
                        Some(Ok((task_id, (actor, _, outcome)))) => {
                            let settled = settle_embedded_completion(
                                actor, task_id, outcome, application_owners, release_waiters,
                            );
                            match settled {
                                Ok(EmbeddedTaskCompletion::Completed) => {}
                                Ok(EmbeddedTaskCompletion::CleanupFailed(detail)) => {
                                    failures.push(format!("embedded Engine {actor:?}: {detail}"));
                                }
                                Ok(EmbeddedTaskCompletion::ExecutionFailed(detail))
                                | Ok(EmbeddedTaskCompletion::RecoveryRequired(detail)) => {
                                    driver_failures.push(format!("embedded Engine {actor:?}: {detail}"));
                                }
                                Err(failure) => failures.push(failure),
                            }
                        }
                        Some(Err(error)) => {
                            // A lost driver never certifies resource release.
                            let actor = embedded_actor_for_task(application_owners, error.id());
                            failures.push(format!("embedded Engine task for {actor:?}: {error}"));
                        }
                        None => {}
                    }
                    continue;
                }
                changed = shutdown.changed() => {
                    if changed.is_err() {
                        failures.push("forest shutdown owner disappeared before settlement".into());
                        break;
                    }
                    continue;
                }
            }
        };
        match event.expect("draining lifecycle event exists") {
            LocalResidentDeployment::Retired { actor, terminal } => {
                retire_interactive_application(
                    actor,
                    terminal,
                    worktree_authority,
                    application_owners,
                    notifications,
                    embedded_lifecycle_tx,
                    command_resources,
                )
                .await;
            }
            LocalResidentDeployment::ReleaseAwait(request) => {
                match observed_resource_release(request.actor, application_owners) {
                    Some(release) => {
                        request.answer(release);
                    }
                    None => release_waiters
                        .entry(request.actor)
                        .or_default()
                        .push(request),
                }
            }
            LocalResidentDeployment::CommandBackend(request) => {
                request.supply(Err(
                    tidepool_bridge_effects::CommandError::CommandUnavailable(
                        "host admission is closed for forest shutdown".into(),
                    ),
                ));
            }
            LocalResidentDeployment::NotificationSend(command) => {
                command.rejected(exomonad_actor::NotificationError::Unavailable);
            }
            LocalResidentDeployment::NotificationPoll(command) => {
                command.observed(Err(exomonad_actor::NotificationError::Unavailable));
            }
            LocalResidentDeployment::RequestUpdate { delivery } => {
                if let Some(presentation) = delivery.begin() {
                    presentation
                        .not_presented("host admission is closed for forest shutdown".into());
                }
            }
            LocalResidentDeployment::PolicyInstalled(installation) => {
                if let Some(admission) = installation.spawn_admission.as_ref() {
                    admission.fail("host admission is closed for forest shutdown".into());
                }
            }
            LocalResidentDeployment::RequestCancellation { .. } => {
                // As in the running embedded consumer, this notice does not
                // acknowledge a typed request. Invocation cleanup retires its
                // owned workers and then checks RequestCleanupState::TargetClosed;
                // an unclosed target keeps that cleanup unconfirmed.
            }
            LocalResidentDeployment::DisplayPublished(request) => {
                if request.outcome().is_none() {
                    // Channel receipt cannot prove that an earlier Store attempt
                    // committed or rolled back. Retain publication uncertainty.
                    request.answer(exomonad_actor::DisplayPublicationOutcome::Unconfirmed(
                        "host shutdown closed before display publication was confirmed".into(),
                    ));
                }
            }
            // These notices only wake ordinary input in the embedded host.
            // Target closure is established by the request/actor owners, never
            // by receipt of a notice or the fact that its channel closed.
            LocalResidentDeployment::SessionReady { .. }
            | LocalResidentDeployment::ChildExited { .. }
            | LocalResidentDeployment::WatchChanged { .. }
            | LocalResidentDeployment::SettlementChanged { .. } => {}
        }
    }
    EmbeddedShutdownOutcome {
        driver_failure: (!driver_failures.is_empty()).then(|| driver_failures.join("; ")),
        cleanup_failure: (!failures.is_empty()).then(|| failures.join("; ")),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum EmbeddedTaskCompletion {
    Completed,
    RecoveryRequired(String),
    ExecutionFailed(String),
    CleanupFailed(String),
}

/// Reconcile the exact driver's result before making resource release observable.
/// Every join consumer uses this transition, including the final shutdown drain.
pub(super) fn settle_embedded_completion(
    actor: ActorRef,
    task_id: tokio::task::Id,
    outcome: Result<(), embedded_service::EmbeddedDriverError>,
    application_owners: &InteractiveOwners,
    release_waiters: &mut HashMap<ActorRef, Vec<Arc<exomonad_actor::ReleaseAwait>>>,
) -> Result<EmbeddedTaskCompletion, String> {
    let completion = match &outcome {
        Ok(()) => EmbeddedTaskCompletion::Completed,
        Err(error) if error.requires_recovery() => {
            EmbeddedTaskCompletion::RecoveryRequired(error.to_string())
        }
        Err(error) if error.cleanup_failed() => {
            EmbeddedTaskCompletion::CleanupFailed(error.to_string())
        }
        Err(error) => EmbeddedTaskCompletion::ExecutionFailed(error.to_string()),
    };
    let release = {
        let mut owners = application_owners.lock();
        let state = owners
            .get_mut(&actor)
            .map(|owner| &mut owner.embedded)
            .filter(|state| state.task_id == Some(task_id))
            .ok_or_else(|| {
                format!(
                    "embedded Engine {actor:?} joined without its exact registered task; release remains unconfirmed"
                )
            })?;
        state.task_id = None;
        state.live = false;
        state.cancellation = None;
        if let Err(error) = outcome {
            if error.cleanup_failed() {
                state.cleanup_failure = Some(error);
            }
        }
        embedded_resource_release(state.cleanup_failure.as_ref())
    };
    answer_release_waiters(release_waiters, actor, release);
    Ok(completion)
}

#[cfg(test)]
mod tests {
    use super::{ensure_run_store_admission, RunStoreAdmissionFailure};
    use tidepool_runtime::session::{RunRecoveryStore, StoreErrorKind};

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_display_uncertainty_preserves_late_publication() {
        use super::super::*;
        use exomonad_actor::DisplayPublicationOutcome;
        use exomonad_tool::{ToolArguments, ToolInvocation};

        let campaign = test_campaign::TestCampaign::start().await;
        campaign.run_scenario(|campaign| Box::pin(async move {
            let policy = campaign.root_installation.policy.clone();
            let mut running = Some(tokio::spawn(async move {
                policy.dispatch_boxed(ToolInvocation {
                    context: None,
                    name: exomonad_actor::HASKELL_TOOL.into(),
                    arguments: ToolArguments::Raw("display True".into()),
                }).await
            }));
            let request = campaign.next_deployment(
                "display publication before shutdown drain",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::DisplayPublished(request) => Ok(request),
                    other => Err(other),
                },
            ).await;
            let store = display_output::open_run_store(campaign.session_root.path()).unwrap();
            let run = runtime_namespace(campaign.session_root.path());
            let owners = Arc::new(Mutex::new(HashMap::new()));
            let mut tasks: JoinSet<(ActorRef, (), Result<(), embedded_service::EmbeddedDriverError>)> = JoinSet::new();
            let mut notifications = JoinSet::new();
            let (projection, _) = embedded_projection::LifecycleSender::channel();
            let mut waiters = HashMap::new();
            for published in [false, true] {
                let (sender, mut lifecycle) = mpsc::channel(1);
                sender.send(LocalResidentDeployment::DisplayPublished(Arc::clone(&request)))
                    .await.unwrap();
                let (_forest, shutdown) = watch::channel(ApplicationShutdown::ForestSettled(
                    NativeRetirement::Preserve,
                ));
                let result = super::drain_resident_shutdown(
                    &mut lifecycle, &mut tasks, &owners, &campaign.authority,
                    &mut notifications, &projection, None, &mut waiters, shutdown,
                ).await;
                assert_eq!(result, EmbeddedShutdownOutcome::default());
                if published {
                    assert!(matches!(request.outcome(), Some(DisplayPublicationOutcome::Published(_))));
                } else {
                    assert!(matches!(request.outcome(), Some(DisplayPublicationOutcome::Unconfirmed(_))));
                    assert!(!request.answer(DisplayPublicationOutcome::Refused("closed admission".into())));
                    assert!(matches!(request.outcome(), Some(DisplayPublicationOutcome::Unconfirmed(_))));
                    let result = running.take().unwrap().await.unwrap();
                    let receipts = match &result {
                        Err(exomonad_actor::ResidentToolError::Invocation(failure)) => failure.receipts(),
                        Ok(exomonad_actor::ResidentToolResponse::Workbench(reply)) => &reply.items,
                        other => panic!("display uncertainty lost its typed receipt: {other:?}"),
                    };
                    assert!(receipts.iter().flat_map(|item| &item.operations).any(|operation| matches!(
                        operation.display_publication,
                        Some(tidepool_runtime::session::WorkbenchDisplayPublication::Unconfirmed { .. })
                    )));
                    display_output::publish(&campaign.forest, &store, &run, None, None, &request);
                    assert!(matches!(request.outcome(), Some(DisplayPublicationOutcome::Published(_))));
                }
            }
        })).await;
    }

    #[test]
    fn original_native_owner_refuses_replaced_database_while_successor_is_ready() {
        let directory = tempfile::tempdir().unwrap();
        let native_path = directory.path().join("native.sqlite");
        let native = RunRecoveryStore::create(&native_path).unwrap();
        let embedded =
            harness::store::Store::open(directory.path().join("embedded.sqlite")).unwrap();
        ensure_run_store_admission(&native, &embedded).unwrap();

        std::fs::rename(&native_path, directory.path().join("retained.sqlite")).unwrap();
        let replacement = RunRecoveryStore::create(&native_path).unwrap();
        ensure_run_store_admission(&replacement, &embedded).unwrap();
        assert!(matches!(
            ensure_run_store_admission(&native, &embedded),
            Err(RunStoreAdmissionFailure::Native(error)) if error.kind == StoreErrorKind::Integrity
        ));
        embedded.ensure_ready().unwrap();
    }

    #[test]
    fn original_embedded_commit_fence_survives_healthy_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let native = RunRecoveryStore::create(&directory.path().join("native.sqlite")).unwrap();
        let embedded_path = directory.path().join("embedded.sqlite");
        let embedded = harness::store::Store::open(&embedded_path).unwrap();
        ensure_run_store_admission(&native, &embedded).unwrap();

        rusqlite::Connection::open(&embedded_path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE commit_fault_parent(id INTEGER PRIMARY KEY);
                 CREATE TABLE commit_fault_child(id INTEGER REFERENCES commit_fault_parent(id) DEFERRABLE INITIALLY DEFERRED);
                 CREATE TRIGGER event_commit_fault AFTER INSERT ON events BEGIN INSERT INTO commit_fault_child VALUES(1); END;",
            )
            .unwrap();
        assert!(matches!(
            embedded.record_event(None, "diagnostic", &serde_json::json!({})),
            Err(harness::store::StoreError::RecoveryRequired(_))
        ));
        let reopened = harness::store::Store::open(&embedded_path).unwrap();
        ensure_run_store_admission(&native, &reopened).unwrap();
        assert!(matches!(
            ensure_run_store_admission(&native, &embedded),
            Err(RunStoreAdmissionFailure::Embedded(
                harness::store::StoreError::AdmissionFenced
            ))
        ));
        native.ensure_ready().unwrap();
    }
}
