use super::*;

pub(super) async fn run_interactive_applications(
    mut lifecycle: mpsc::Receiver<LocalResidentDeployment>,
    application_owners: InteractiveOwners,
    fleet: InteractiveFleet,
    shutdown: watch::Receiver<Option<NativeRetirement>>,
    mut root_config: watch::Receiver<ActorHostConfig>,
    mut embedded_service: embedded_service::EmbeddedService,
) -> Result<(), String> {
    let InteractiveFleet {
        provider_forest,
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
        Arc<harness::embedding::Conversation>,
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
                    let response = forest
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
                    serde_json::to_value(response).map_err(|error| error.to_string())
                })
            }))
            .map_err(str::to_owned)?;
    }

    // Answer a stopped actor's release wait from its settled Engine state,
    // or when its native driver joins and confirms cleanup.
    let mut release_waiters: HashMap<ActorRef, Vec<Arc<exomonad_actor::ReleaseAwait>>> =
        HashMap::new();
    let mut notifications: JoinSet<(ActorRef, Result<(), String>)> = JoinSet::new();

    let mut health = tokio::time::interval(Duration::from_secs(1));
    {
        let service = &embedded_service;
        let live_actors = embedded_live_actors(&application_owners);
        embedded_projection
            .publish(
                &service.control,
                &embedded_run,
                &(host_graph)(),
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
                |actor| !live_actors.contains(&actor),
            )
            .map_err(|error| format!("embedded actor history projection failed: {error}"))?;
    }
    let failure = AssertUnwindSafe(async {
        let failure = loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown.clone()) => break None,
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
                        if let Err(error) = embedded_projection.publish(
                            &service.control,
                            &embedded_run,
                            &(host_graph)(),
                            &states,
                            |actor| conversations.get(&actor).and_then(|binding| binding.conversation()).and_then(|conversation| conversation.active_round()),
                            |identity| service.runtime.store().embedded_round_frontier(identity).map(|frontier| frontier.pending_head.or(frontier.settled_head).map(|request| request.0)),
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
                    if let Err(error) = drain_embedded_browser_commands(
                        &service.runtime.store(), &service.control, &embedded_run,
                        &(host_graph)(), &live_actors, &embedded_projection,
                        &conversations, &embedded_lifecycle_tx,
                    ).await {
                        break Some(format!("embedded browser command drain failed: {error}"));
                    }
                    if let Err(error) = embedded_projection.publish(
                        &service.control,
                        &embedded_run,
                        &(host_graph)(),
                        &(*embedded_lifecycle_rx.borrow()).clone(),
                        |actor| conversations.get(&actor).and_then(|binding| binding.conversation()).and_then(|conversation| conversation.active_round()),
                        |identity| service.runtime.store().embedded_round_frontier(identity).map(|frontier| frontier.pending_head.or(frontier.settled_head).map(|request| request.0)),
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
                if let Err(error) = drain_embedded_browser_commands(
                    &store, &service.control, &embedded_run, &(host_graph)(),
                    &live_actors, &embedded_projection, &conversations,
                    &embedded_lifecycle_tx,
                ).await {
                    break Some(format!("embedded browser command drain failed: {error}"));
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
                update_embedded_state(&application_owners, actor, |state| {
                    if state.task_id == Some(task_id) {
                        state.task_id = None;
                    }
                    state.live = false;
                    state.pending_activations = None;
                    state.cancellation = None;
                });
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
                if let Some(waiters) = release_waiters.remove(&actor) {
                    let release = embedded_resource_release(outcome.as_ref().err());
                    for waiter in waiters {
                        waiter.answer(release.clone());
                    }
                }
                match outcome {
                    Ok(()) if local_actor.terminal().get().is_none() => {
                        if let Err(error) = apply_application_failure(
                            local_actor,
                            ExternalApplicationFailure {
                                class: ExternalApplicationFailureClass::UnexpectedExit,
                                detail: "embedded Engine exited before actor settlement".into(),
                            },
                        ).await {
                            break Some(error);
                        }
                    }
                    Ok(()) => {}
                    Err(error) => {
                        let cleanup_failed = error.cleanup_failed();
                        let detail = error.to_string();
                        if cleanup_failed {
                            update_embedded_state(&application_owners, actor, |state| {
                                state.cleanup_failure = Some(error);
                            });
                        }
                        tracing::error!(?actor, %detail, "embedded Engine failed");
                        if local_actor.terminal().get().is_none() {
                            if let Err(error) = apply_application_failure(
                                local_actor,
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
            }
            Some(((actor, provider_attachment), local_actor, conversation, ready_ack)) = embedded_ready_rx.recv() => {
                if let Err(error) = provider_attachment.validate() {
                    tracing::warn!(?actor, %error, "embedded provider attachment became unavailable");
                    if let Some(cancel) = with_embedded_state(&application_owners, actor, |state| state.cancellation.clone()).flatten() {
                        cancel.send_replace(true);
                    }
                    continue;
                }
                let Some(activations) = with_embedded_state(&application_owners, actor, |state| state.pending_activations.take()).flatten() else {
                    continue;
                };
                if !embedded_is_live(&application_owners, actor)
                    || with_embedded_state(&application_owners, actor, |state| state.cancellation.is_some()) != Some(true)
                {
                    continue;
                }
                let Ok(_admission) = local_actor.admit_transaction() else {
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
                if let Err(error) = binding.set_conversation(Arc::clone(&conversation)) {
                    tracing::warn!(?actor, ?error, "embedded child attachment was not accepted");
                    continue;
                }
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
                if let Err(error) = provider_attachment.validate() {
                    tracing::warn!(?actor, %error, "embedded provider readiness became unavailable");
                    if let Some(cancel) = with_embedded_state(&application_owners, actor, |state| state.cancellation.clone()).flatten() {
                        cancel.send_replace(true);
                    }
                    continue;
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
                        ) {
                            Ok(admission) => admission,
                            Err(error) => break Some(format!(
                                "actor {:?} provider attachment is unavailable: {error}",
                                installation.actor.identity(),
                            )),
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
                                match embedded_context::selected_provider_parent(
                                    &embedded_run, installation.creator.or(installation.supervisor_parent), &(host_graph)(), &identities,
                                ) {
                                    Ok(parent) => Some(parent),
                                    Err(error) => {
                                        if let Some(gate) = installation.fork_gate.as_ref() {
                                            gate.mark_failed().ok();
                                        }
                                        if let Some(admission) = &installation.spawn_admission { admission.fail(error.clone()); }
                                        tracing::warn!(?actor, %error, "selected provider ancestry refused");
                                        if let Err(failure) = apply_application_failure(installation.actor.clone(), ExternalApplicationFailure {
                                            class: ExternalApplicationFailureClass::ToolHostStartup,
                                            detail: error,
                                        }).await {
                                            tracing::warn!(?actor, %failure, "selected child startup failure was not delivered");
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
                                    break Some(format!("embedded child {actor:?} has no admitted checkpoint identity"));
                                };
                                captured.child_path(actor)
                            };
                            if embedded_binding(&application_owners, actor).is_none() {
                                let binding = match open_embedded_actor_binding(
                                    &launch_context.config.run_directory,
                                    actor,
                                    path.clone(),
                                    None,
                                ) {
                                    Ok(binding) => binding,
                                    Err(error) => break Some(format!(
                                        "embedded actor {actor:?} notification owner could not open: {error}"
                                    )),
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
                                Err(error) => break Some(error),
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
                                break Some(format!(
                                    "embedded child actor {actor:?} requires a hosted checkpoint"
                                ));
                            }
                            let initial_input = installation.initial_user_message.clone();
                            if !is_root {
                                let queue_admission = match local_actor.admit_transaction() {
                                    Ok(admission) => admission,
                                    Err(error) => {
                                        if let Some(admission) = &installation.spawn_admission { admission.fail(error.to_string()); }
                                        tracing::warn!(?actor, %error, "embedded child queue admission refused");
                                        continue;
                                    }
                                };
                                let spawn_admission = installation.spawn_admission.clone();
                                let fork_gate = installation.fork_gate.clone();
                                let committed_gate = fork_gate.clone();
                                let runtime = Arc::clone(&service.runtime);
                                let run_root = launch_context.run_root.clone();
                                let ready = embedded_ready_tx.clone();
                                let embedded_lifecycle = embedded_lifecycle_tx.clone();
                                #[cfg(test)]
                                let test_transport = service.test_transport();
                                let (cancel, mut cancellation_rx) = watch::channel(false);
                                update_embedded_state(&application_owners, actor, |state| {
                                    state.cancellation = Some(cancel.clone());
                                    state.pending_activations = Some(Vec::new());
                                    state.live = true;
                                });
                                let task = embedded_tasks.spawn(async move {
                                    let result = async {
                                        if let Some(gate) = committed_gate {
                                            tokio::select! {
                                                committed = gate.wait_committed() => {
                                                    committed.map_err(|error| format!("fork publication refused: {error}"))?;
                                                }
                                                _ = cancellation_rx.changed() => return Ok(()),
                                            }
                                        }
                                        if *cancellation_rx.borrow() || local_actor.terminal().get().is_some() {
                                            return Ok(());
                                        }
                                        provider_attachment.validate().map_err(|error| error.to_string())?;
                                        let attachment = async {
                                            match selected_parent {
                                                Some(parent) => embedded_service::attach_selected_actor(runtime.as_ref(), &run_root, parent, *installation, initial_input).await,
                                                None => embedded_service::attach_checkpoint_actor(runtime.as_ref(), &run_root, *installation, initial_input).await,
                                            }
                                        };
                                        let mut embedded = tokio::select! {
                                            attached = attachment => attached?,
                                            _ = cancellation_rx.changed() => return Ok(()),
                                        };
                                        if *cancellation_rx.borrow() || local_actor.terminal().get().is_some() {
                                            return Ok(());
                                        }
                                        if let Err(error) = provider_attachment.validate() {
                                            embedded.cancellation.send_replace(true);
                                            return Err(error.to_string().into());
                                        }
                                        embedded.cancellation = cancel;
                                        embedded.cancellation_rx = cancellation_rx;
                                        let (ready_ack, acknowledged) = oneshot::channel();
                                        ready.send(((actor, provider_attachment.clone()), local_actor.clone(), Arc::clone(&embedded.conversation), ready_ack))
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
                                                harness::transport::auth::CodexFileAuth, _
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
                                // The child attachment may wait for checkpoint publication.
                                // Its actor input queue is already owned here, so advertise
                                // readiness before that wait: the issuing cell can be waiting
                                // for this group to become ready before it delivers the capture.
                                if let Some(gate) = fork_gate {
                                    if let Err(error) = gate.mark_ready() {
                                        if let Some(cancel) = with_embedded_state(&application_owners, actor, |state| state.cancellation.clone()).flatten() {
                                            cancel.send_replace(true);
                                        }
                                        tracing::warn!(?actor, %error, "embedded child fork admission refused");
                                    }
                                }
                                drop(queue_admission);
                                continue;
                            }
                            if let Err(error) = provider_attachment.validate() {
                                break Some(format!("actor {actor:?} provider attachment became unavailable: {error}"));
                            }
                            let attachment = embedded_service::attach_actor(
                                service,
                                &launch_context.run_root,
                                harness::model::AgentPath("/root".into()),
                                None,
                                *installation,
                                initial_input,
                            ).await;
                            let embedded = match attachment {
                                Ok(embedded) => embedded,
                                Err(error) => break Some(format!(
                                    "embedded actor {actor:?} could not attach: {error}"
                                )),
                            };
                            if let Err(error) = provider_attachment.validate() {
                                embedded.cancellation.send_replace(true);
                                break Some(format!("actor {actor:?} provider attachment became unavailable: {error}"));
                            }
                            let root_conversation = Arc::clone(&embedded.conversation);
                            let Some(binding) = embedded_binding(&application_owners, actor) else {
                                break Some(format!("embedded notification owner was not retained for {actor:?}"));
                            };
                            let Ok(attachment_admission) = local_actor.admit_transaction() else {
                                binding.mark_retired();
                                update_embedded_state(&application_owners, actor, |state| {
                                    state.conversation = Some(binding.clone());
                                });
                                embedded.cancellation.send_replace(true);
                                continue;
                            };
                            match binding.set_conversation(Arc::clone(&root_conversation)) {
                                Ok(()) => {}
                                Err(embedded_harness::ConversationAttachError::Retired) => {
                                    embedded.cancellation.send_replace(true);
                                    continue;
                                }
                                Err(embedded_harness::ConversationAttachError::IdentityMismatch) => {
                                    break Some(format!(
                                        "embedded conversation identity does not match actor {actor:?}"
                                    ));
                                }
                            }
                            update_embedded_state(&application_owners, actor, |state| {
                                state.conversation = Some(binding.clone());
                            });
                            embedded_projection
                                .attached(actor, root_conversation.identity());
                            if let Some(binding) = embedded_binding(&application_owners, actor) {
                                schedule_embedded_notification_drain(
                                    actor,
                                    binding,
                                    &mut notifications,
                                );
                            }
                            update_embedded_state(&application_owners, actor, |state| {
                                state.cancellation = Some(embedded.cancellation.clone());
                                state.live = true;
                            });
                            embedded_lifecycle_tx.publish(
                                actor,
                                harness::server::HostActorLifecycle::Waiting,
                            );
                            let lifecycle_states =
                                (*embedded_lifecycle_rx.borrow()).clone();
                            let conversations = embedded_bindings(&application_owners);
                            let live_actors = embedded_live_actors(&application_owners);
                            if let Err(error) = embedded_projection.publish(
                                &service.control,
                                &embedded_run,
                                &(host_graph)(),
                                &lifecycle_states,
                                |actor| conversations.get(&actor).and_then(|binding| binding.conversation()).and_then(|conversation| conversation.active_round()),
                                |identity| service.runtime.store().embedded_round_frontier(identity).map(|frontier| frontier.pending_head.or(frontier.settled_head).map(|request| request.0)),
                                |actor| !live_actors.contains(&actor),
                            ) {
                                break Some(format!("embedded actor history projection failed: {error}"));
                            }
                            let runtime = Arc::clone(&service.runtime);
                            #[cfg(test)]
                            let test_transport = service.test_transport();
                            let embedded_lifecycle = embedded_lifecycle_tx.clone();
                            let embedded_actor = actor;
                            let provider_owner = provider_attachment.clone();
                            let task = embedded_tasks.spawn(async move {
                                let _provider_attachment = provider_owner;
                                #[cfg(test)]
                                let result = if let Some(transport) = test_transport {
                                    embedded_service::drive_conversation_with_transport::<
                                        harness::transport::auth::CodexFileAuth,
                                        _,
                                    >(
                                        embedded.driver,
                                        runtime,
                                        &settings,
                                        model,
                                        effort,
                                        instructions,
                                        embedded.cancellation_rx,
                                        embedded_lifecycle,
                                        embedded_actor,
                                        transport,
                                    )
                                    .await
                                } else {
                                    embedded_service::drive_conversation(
                                        embedded.driver,
                                        runtime,
                                        &settings,
                                        model,
                                        effort,
                                        instructions,
                                        embedded.cancellation_rx,
                                        embedded_lifecycle,
                                        embedded_actor,
                                    )
                                    .await
                                };
                                #[cfg(not(test))]
                                let result = embedded_service::drive_conversation(
                                    embedded.driver,
                                    runtime,
                                    &settings,
                                    model,
                                    effort,
                                    instructions,
                                    embedded.cancellation_rx,
                                    embedded_lifecycle,
                                    embedded_actor,
                                )
                                .await;
                                (actor, local_actor, result)
                            });
                            update_embedded_state(&application_owners, actor, |state| {
                                state.task_id = Some(task.id());
                            });
                            // The permanent root has an installed workbench but
                            // no typed request activation. PolicyInstalled is its
                            // actor-owned readiness boundary; SessionReady below
                            // carries subsequent typed request input only.
                            if is_root {
                                if let Err(error) = provider_attachment.validate() {
                                    break Some(format!("actor {actor:?} provider readiness is unavailable: {error}"));
                                }
                                tracing::info!(target: "tidepool::actor_host::startup", actor = %root_identity, "embedded root ready");
                                readiness
                                    .send(ActorHostReadiness::EmbeddedReady {
                                        root: root_identity,
                                        address: service.address,
                                    })
                                    .ok();
                            }
                            drop(attachment_admission);
                            continue;
                        }



                    }
                    LocalResidentDeployment::SessionReady { activation } => {
                        let actor = activation.id.actor();
                        {
                            if with_embedded_state(&application_owners, actor, |state| {
                                state.pending_activations.is_some()
                            }) == Some(true)
                            {
                                update_embedded_state(&application_owners, actor, |state| {
                                    if let Some(pending) = state.pending_activations.as_mut() {
                                        pending.push(activation);
                                    }
                                });
                                continue;
                            }
                            let Some(conversation) = embedded_binding(&application_owners, actor)
                                .filter(|binding| binding.is_live())
                                .and_then(|binding| binding.conversation()) else {
                                if actor != root_identity {
                                    if let Some(cancel) = with_embedded_state(&application_owners, actor, |state| state.cancellation.clone()).flatten() {
                                        cancel.send_replace(true);
                                    }
                                    tracing::warn!(?actor, "embedded child activation arrived without an attached conversation");
                                    continue;
                                }
                                break Some(format!("embedded actor {actor:?} received an activation before conversation attachment"));
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
                        worktree_authority.remove_grant(actor.into());
                        let binding = with_embedded_state(&application_owners, actor, |state| {
                            if let Some(cancel) = state.cancellation.take() {
                                cancel.send_replace(true);
                            }
                            state.pending_activations = None;
                            if let Some(binding) = state.conversation.as_ref() {
                                binding.mark_retired();
                            }
                            state.conversation.clone()
                        }).flatten();
                        if let Some(binding) = binding {
                            schedule_embedded_notification_drain(
                                actor,
                                binding,
                                &mut notifications,
                            );
                        }
                        let lifecycle = if terminal.kind == ActorExitKind::Failed {
                            harness::server::HostActorLifecycle::Lost
                        } else {
                            harness::server::HostActorLifecycle::Retired
                        };
                        embedded_lifecycle_tx.publish(actor, lifecycle);
                        if let Some(resources) = &launch_context.config.command_resources {
                            let producer = format!("{}-{}", actor.id.0, actor.incarnation.0);
                            if let Err(error) = resources.seal_producer(&producer).await {
                                tracing::warn!(?actor, %error, "command resource producer retirement remains unconfirmed");
                            }
                        }
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.retired(terminal);
                        }


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

    close_release_observations(&mut lifecycle, &mut release_waiters, |actor| {
        observed_resource_release(actor, &application_owners)
    });

    let native_retirement = if failure.is_some() {
        NativeRetirement::Preserve
    } else {
        shutdown.borrow().unwrap_or_default()
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
        |task_id| embedded_actor_for_task(&application_owners, task_id),
        |actor, release| {
            answer_release_waiters(&mut release_waiters, actor, release);
        },
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
        embedded_cleanup,
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
    match (failure, cleanup_failure) {
        (Some(error), Some(cleanup)) => Err(format!("{error}; cleanup: {cleanup}")),
        (Some(error), None) | (None, Some(error)) => Err(error),
        (None, None) => Ok(()),
    }
}
