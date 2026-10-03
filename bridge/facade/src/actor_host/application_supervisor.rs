use super::*;

pub(super) async fn run_interactive_applications(
    mut lifecycle: mpsc::Receiver<LocalResidentDeployment>,
    application_owners: InteractiveOwners,
    fleet: InteractiveFleet,
    shutdown: watch::Receiver<Option<NativeRetirement>>,
    mut root_config: watch::Receiver<ActorHostConfig>,
    mut embedded_service: Option<embedded_service::EmbeddedService>,
) -> Result<(), String> {
    let InteractiveFleet {
        provider_forest,
        root,
        config,
        run_root,
        output_store,
        #[cfg(feature = "codex-compat")]
        tmux,
        #[cfg(feature = "codex-compat")]
        backend,
        worktrees,
        #[cfg(feature = "codex-compat")]
        bindings,
        readiness,
        worktree_authority,
        #[cfg(feature = "codex-compat")]
        watch_retention,
        #[cfg(feature = "codex-compat")]
        watch_observation,
        #[cfg(feature = "codex-compat")]
        open_request,
        #[cfg(feature = "codex-compat")]
        source_layers,
        #[cfg(feature = "codex-compat")]
        actor_recovery,
        #[cfg(feature = "codex-compat")]
        recovered_threads,
        #[cfg(feature = "codex-compat")]
        recovered_root_predecessor,
        host_graph,
    } = fleet;
    let base_prompt = FrozenBasePrompt::materialize_selected(
        &run_root,
        config
            .workspace_inputs
            .as_ref()
            .and_then(|inputs| inputs.prompts.get("core"))
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
        #[cfg(feature = "codex-compat")]
        tmux: tmux.clone(),
        #[cfg(feature = "codex-compat")]
        backend: backend.clone(),
        worktrees: worktrees.clone(),
        #[cfg(feature = "codex-compat")]
        source_layers: source_layers.clone(),
        #[cfg(feature = "codex-compat")]
        bindings: Arc::clone(&bindings),
        #[cfg(feature = "codex-compat")]
        actor_recovery,
        #[cfg(feature = "codex-compat")]
        recovered_threads: Arc::clone(&recovered_threads),
    };
    #[cfg(feature = "codex-compat")]
    let mut deployments: Vec<InteractiveDeployment> = Vec::new();
    #[cfg(feature = "codex-compat")]
    let mut launches = JoinSet::new();
    #[cfg(not(feature = "codex-compat"))]
    let mut launches: JoinSet<()> = JoinSet::new();
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
    if let Some(service) = embedded_service.as_ref() {
        let forest = Arc::clone(&provider_forest);
        let run = embedded_run.clone();
        service
            .control
            .install_actor_display_expander(Arc::new(move |input| {
                let forest = Arc::clone(&forest);
                let run = run.clone();
                Box::pin(async move {
                    if input.origin.run != run {
                        return Err("display belongs to a different run".into());
                    }
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
    #[cfg(feature = "codex-compat")]
    let mut binding_discoveries = JoinSet::new();
    #[cfg(not(feature = "codex-compat"))]
    let mut binding_discoveries: JoinSet<()> = JoinSet::new();
    #[cfg(feature = "codex-compat")]
    let mut retirements = JoinSet::new();
    #[cfg(not(feature = "codex-compat"))]
    let mut retirements: JoinSet<()> = JoinSet::new();
    // Supervisors waiting for a stopped actor's release receipt. Served from
    // the receipt slot when it already exists, else when retirement joins.
    let mut release_waiters: HashMap<ActorRef, Vec<Arc<exomonad_actor::ReleaseAwait>>> =
        HashMap::new();
    let mut notifications: JoinSet<(ActorRef, Result<(), String>)> = JoinSet::new();
    #[cfg(feature = "codex-compat")]
    let mut publication_retries = JoinSet::new();
    #[cfg(not(feature = "codex-compat"))]
    let mut publication_retries: JoinSet<()> = JoinSet::new();
    #[cfg(feature = "codex-compat")]
    let mut process_observations = JoinSet::new();
    #[cfg(not(feature = "codex-compat"))]
    let mut process_observations: JoinSet<()> = JoinSet::new();
    let mut health = tokio::time::interval(Duration::from_secs(1));
    if let Some(service) = embedded_service.as_ref() {
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
                    if let Some(service) = embedded_service.as_ref() {
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
                if let Some(service) = embedded_service.as_mut() {
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
                #[cfg(feature = "codex-compat")]
                if process_observations.is_empty() {
                    let rows = application_owners.lock();
                    for deployment in deployments.iter().filter(|deployment| {
                        !deployment.failure_reported
                            && deployment.local_actor.terminal().get().is_none()
                    }) {
                        let Some(slot) = rows
                            .get(&deployment.actor)
                            .and_then(|row| row.scoped_retention.as_ref())
                            .map(|retention| retention.slot.clone())
                        else {
                            continue;
                        };
                        let actor = deployment.actor;
                        process_observations.spawn_blocking(move || {
                            let observed = scoped_custody::observe_slot(
                                &slot,
                                std::time::Instant::now() + Duration::from_millis(250),
                            );
                            (actor, observed)
                        });
                    }
                }
                #[cfg(feature = "codex-compat")]
                if let Some(backend) = backend.codex_backend() {
                    for deployment in &deployments {
                        if let Some(thread) = &deployment.thread {
                            let workspace = deployment.active_workspace.clone();
                            if let Ok(mut publication) = workspace.publication.clone().try_lock_owned() {
                                if publication.is_pending() {
                                    let backend = Arc::clone(backend);
                                    let owner = BoundWorkspace { workspace, thread: thread.clone() };
                                    let actor = deployment.actor;
                                    publication_retries.spawn(async move {
                                        if let Err(error) = owner.settle_publication(&mut publication, backend.as_ref()).await {
                                            tracing::debug!(?actor, %error, "workspace publication recovery remains pending");
                                        }
                                    });
                                }
                            }
                        }
                    }
                }
                #[cfg(feature = "codex-compat")]
                for index in 0..deployments.len() {
                    let deployment = &deployments[index];
                    let snapshot = deployment.runtime_observation.snapshot();
                    if snapshot.provider_observation_stale { continue; }
                    for turn in snapshot.provider_failures {
                    let deployment = &deployments[index];
                    let exomonad_agent::ProviderTurnState::Failed(failure) = turn.state else { continue; };
                    let key = (turn.thread.clone(), turn.turn.clone());
                    if deployment.notified_provider_failures.contains(&key) { continue; }
                    let actor = deployment.actor;
                    if let Some(supervisor) = deployment.supervisor {
                        let Some(owner) = deployments.iter().find(|app| app.actor == supervisor) else { continue; };
                        let event = DurableActorEvent::Typed(TypedActorEvent::ProviderTurnFailed {
                            revision: turn.revision as u64,
                            actor, thread: turn.thread, turn: turn.turn, failure,
                        });
                        if let Err(error) = publish_inbox_event(Arc::clone(&owner.inbox), event).await {
                            tracing::warn!(?actor, %error, "provider failure notice pending publication");
                            // Preserve source order: a later watermark must not suppress
                            // this failed publication on retry.
                            break;
                        }
                    } else {
                        // Root failures are operator-visible, never self-injected retries.
                        tracing::error!(?actor, ?failure, turn = %turn.turn, "root provider turn needs attention");
                    }
                    deployments[index].notified_provider_failures.insert(key);
                    }
                }
                #[cfg(feature = "codex-compat")]
                if let Some(index) = deployments.iter().position(|deployment| {
                    !deployment.failure_reported
                        && deployment.local_actor.terminal().get().is_none()
                        && (hosted_retirement::service_finished(&deployment.service)
                            || matches!(
                                &deployment.connection,
                                InteractiveConnection::Bound { delivery, .. }
                                    if delivery.is_finished()
                            ))
                }) {
                    let local_actor = deployments[index].local_actor.clone();
                    deployments[index].failure_reported = true;
                    let detail = "interactive application exited before actor settlement".to_string();
                    if let Err(error) = apply_application_failure(
                        local_actor,
                        ExternalApplicationFailure {
                            class: ExternalApplicationFailureClass::UnexpectedExit,
                            detail,
                        }
                    ).await {
                        break Some(error);
                    }
                }
            }
            Some(observed) = process_observations.join_next(), if !process_observations.is_empty() => {
                #[cfg(feature = "codex-compat")]
                {
                let Ok((actor, Ok(scoped_custody::ScopedProcessObservation::ProcessStopped))) = observed else {
                    continue;
                };
                let Some(index) = deployments.iter().position(|deployment| {
                    deployment.actor == actor
                        && !deployment.failure_reported
                        && deployment.local_actor.terminal().get().is_none()
                }) else {
                    continue;
                };
                let local_actor = deployments[index].local_actor.clone();
                deployments[index].failure_reported = true;
                if let Err(error) = apply_application_failure(
                    local_actor,
                    ExternalApplicationFailure {
                        class: ExternalApplicationFailureClass::UnexpectedExit,
                        detail: "supervised native process exited before actor settlement".into(),
                    },
                )
                .await
                {
                    break Some(error);
                }
                }
            }
            command = async {
                match embedded_service.as_mut() {
                    Some(service) => service.commands.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                let Some(command) = command else {
                    break Some("embedded browser command channel closed".into());
                };
                let service = embedded_service.as_ref().expect("branch requires service");
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
                            embedded_service.as_ref().map(|service| &service.control),
                            &request,
                        );
                    }
                    LocalResidentDeployment::PolicyInstalled(installation) => {
                        let provider_attachment = match provider_attachment::ProviderAttachment::admit(
                            Arc::clone(&provider_forest), installation.actor.identity(),
                        ) {
                            Ok(admission) => admission,
                            Err(error) => break Some(format!(
                                "actor {:?} provider attachment is unavailable: {error}",
                                installation.actor.identity(),
                            )),
                        };
                        let embedded_policy = Arc::new(
                            embedded_policy::EmbeddedPolicyInstallation::from_installation(&installation),
                        );
                        if installation.creator.is_none() {
                            launch_context.config = root_config.borrow_and_update().clone();
                            root_identity = installation.actor.identity();
                            launch_context.root = root_identity;
                        }
                        worktree_authority.install_grant(
                            installation.actor.identity().into(),
                            worktree_grant(installation.effective_role.role()),
                        );
                        if launch_context.config.backend.kind() == crate::exomonad::ExomonadBackend::Embedded {
                            let Some(service) = embedded_service.as_ref() else {
                                break Some("embedded backend has no prepared service".into());
                            };
                            let Some(settings) = launch_context.config.embedded.clone() else {
                                break Some("embedded backend has no launch settings".into());
                            };
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
                                    &launch_context.run_root,
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
                            let mode = InteractiveLaunchMode::Fresh;
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
                            let effort = match launch_effort(
                                &mode,
                                launch_context.config.effort,
                                installation.fork_effort,
                            ) {
                                ReasoningEffort::Low => harness::model::Effort::Low,
                                ReasoningEffort::Medium => harness::model::Effort::Medium,
                                ReasoningEffort::High => harness::model::Effort::High,
                            };
                            let mut instructions = developer_instructions_selected(
                                &installation.effective_role,
                                &mode,
                                launch_context.config.workspace_inputs.as_ref(),
                                installation.instructions.as_deref(),
                            );
                            append_inheritance_authority(&mut instructions);
                            instructions = format!("{}\n\n{instructions}", launch_context.base_prompt.body());
                            let instructions = orient_launch_instructions(
                                &instructions,
                                &installation.runtime_observation.snapshot(),
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
                                        tracing::warn!(?actor, %error, "embedded child queue admission refused");
                                        continue;
                                    }
                                };
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
                        #[cfg(feature = "codex-compat")]
                        {
                        let fork_parent_thread = match (
                            recovered_threads.contains_key(&installation.actor.identity()),
                            installation.checkpoint.as_ref(),
                            installation.context_parent,
                        ) {
                            (true, _, _) => None,
                            (false, Some(checkpoint), _) => {
                                let Some(thread) = checkpoint.boundary.hosted().and_then(exomonad_tool::OriginalOperation::external_thread) else {
                                    break Some("Codex checkpoint lacks its external conversation origin".into());
                                };
                                Some(BackendThreadId(thread.to_owned()))
                            },
                            (false, None, None) => None,
                            (false, None, Some(parent)) => {
                                let Some(thread) = deployments
                                    .iter()
                                    .find(|deployment| deployment.actor == parent)
                                    .and_then(|deployment| deployment.thread.clone())
                                else {
                                    break Some(format!("context-fork parent {parent:?} has no queue-ready conversation"));
                                };
                                Some(thread.id().clone())
                            }
                        };
                        let workspace_prepared = installation.worktree_custody.as_ref()
                            .and_then(|custody| (custody.as_ref() as &dyn std::any::Any)
                                .downcast_ref::<ActorWorkspaceCustody>())
                            .is_some_and(|custody| custody.workspace.is_some());
                        let Some(native_backend) = launch_context.backend.codex_backend().cloned() else {
                            break Some("Codex actor launch has no native backend adapter".into());
                        };
                        let native_admission = NativeForkAdmission {
                            owners: application_owners.clone(), backend: native_backend.clone(), layout: None,
                        };
                        let context = launch_context.clone();
                        let actor = installation.actor.identity();
                        let (cancel, cancelled) = oneshot::channel();
                        let mut owners = application_owners.lock();
                        if owners.contains_key(&actor) {
                            break Some(format!("duplicate application owner for {actor:?}"));
                        }
                        let hosted_slot = Arc::new(Mutex::new(None));
                        let pane_slot = Arc::new(Mutex::new(None));
                        let mut owner = InteractiveApplicationOwner {
                            #[cfg(feature = "codex-compat")]
                            supervisor: installation.supervisor_parent,
                            creator_workspace: None,
                            cancel: Some(cancel),
                            native_retirement: NativeRetirement::Preserve,
                            pane: pane_slot.clone(),
                            fork_gate: installation.fork_gate.clone(),
                            custody: installation.worktree_custody.clone(),
                            scoped_retention: None,
                            #[cfg(feature = "codex-compat")]
                            hosted: hosted_slot.clone(),
                            embedded_policy: Some(embedded_policy),
                            #[cfg(feature = "codex-compat")]
                            launch: HostLaunchState::Pending,
                            #[cfg(feature = "codex-compat")]
                            pending_activations: Vec::new(),
                            embedded: None,
                            terminal: None,
                            retirement: Arc::new(Mutex::new(None)),
                        };
                        let workspace = match actor_workspace_request(
                            actor == root_identity, &installation.launch_worktrees,
                        ) {
                            Ok(workspace) => workspace,
                            Err(error) => break Some(error),
                        };
                        let scope_slot = match owner.reserve_scope(workspace, actor) {
                            Ok(slot) => slot,
                            Err(error) => break Some(format!(
                                "actor {actor:?} process-scope reservation failed: {error}"
                            )),
                        };
                        owners.insert(actor, owner);
                        drop(owners);
                        let retention = InteractiveLaunchRetention {
                            hosted: hosted_slot,
                            pane: pane_slot,
                            process: scope_slot,
                        };
                        tracing::info!(
                            actor = ?actor,
                            worktree = matches!(workspace, ActorWorkspaceRequest::Worktree(_)),
                            "actor launch started"
                        );
                        installation
                            .runtime_observation
                            .publish_launch_pending("preparing the workspace");
                        launches.spawn(async move {
                            let local_actor = installation.actor.clone();
                            let launch_observation = installation.runtime_observation.clone();
                            let result = AssertUnwindSafe(async {
                                let build_snapshot = if workspace_prepared { None } else {
                                    match installation.creator {
                                        Some(creator) => native_admission.build_snapshot(creator, installation.effective_role.native_tools()).await,
                                        None => None,
                                    }
                                };
                                launch_interactive_application(
                                    *installation,
                                    context,
                                    cancelled,
                                    InteractiveInheritance { thread: fork_parent_thread, build_snapshot },
                                    retention,
                                    provider_attachment.clone(),
                                ).await
                            })
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|_| {
                                Err(application_error(
                                    actor,
                                    InteractiveOperation::PrepareRuntime,
                                    "interactive launch task panicked",
                                ))
                            });
                            if let Err(error) = &result {
                                let state = match error.disposition {
                                    LaunchDisposition::Failed => "launch failed",
                                    LaunchDisposition::Cancelled => "launch cancelled",
                                };
                                launch_observation.publish_launch_pending(format!("{state}: {}", error.detail));
                            }
                            ((local_actor, provider_attachment), result)
                        });
                        }
                        #[cfg(not(feature = "codex-compat"))]
                        unreachable!("featureless actor host only admits embedded launches");
                    }
                    LocalResidentDeployment::SessionReady { activation } => {
                        let actor = activation.id.actor();
                        if launch_context.config.backend.kind() == crate::exomonad::ExomonadBackend::Embedded {
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
                        #[cfg(feature = "codex-compat")]
                        {
                        let Some(application) = deployments.iter_mut().find(|app| app.actor == actor) else {
                            let mut owners = application_owners.lock();
                            if let Some(owner) = owners.get_mut(&actor) {
                                if owner.terminal.is_some() { continue; }
                                match owner.launch {
                                    HostLaunchState::Pending => {
                                        owner.pending_activations.push(activation);
                                        continue;
                                    }
                                    HostLaunchState::Failed(_) | HostLaunchState::Abandoned => continue,
                                    HostLaunchState::Published => {}
                                }
                            }
                            break Some(format!("resident actor {actor:?} requested a session activation without a deployed application"));
                        };
                        if let Err(error) = deliver_session_activation(application, activation).await {
                            break Some(error);
                        }
                        }
                        #[cfg(not(feature = "codex-compat"))]
                        unreachable!("featureless actor host only admits embedded activations");
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
                        #[cfg(feature = "codex-compat")]
                        let retire_undeployed_native = application_owners
                            .lock()
                            .get(&actor)
                            .map_or(true, |owner| owner.should_retire_undeployed_native(false));
                        #[cfg(feature = "codex-compat")]
                        if let Some(index) = deployments.iter().position(|app| app.actor == actor) {
                            let deployment = deployments.swap_remove(index);
                            spawn_owned_retirement(&mut retirements, deployment, tmux.clone(), &application_owners);
                        } else if retire_undeployed_native {
                            spawn_undeployed_hosted_retirement(
                                &mut retirements,
                                actor,
                                &application_owners,
                                &tmux,
                            );
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
                        #[cfg(feature = "codex-compat")]
                        supply_resident_command_backend(
                            request,
                            &launch_context.config,
                            &worktree_authority,
                            &launch_context.worktrees,
                            owner,
                            resource_client,
                            host_resources,
                            deployments
                                .iter()
                                .find(|app| app.actor == owner)
                                .and_then(|app| app.thread.clone()),
                            launch_context.backend.codex_backend().cloned(),
                        );
                        #[cfg(not(feature = "codex-compat"))]
                        supply_resident_command_backend(
                            request,
                            &launch_context.config,
                            &worktree_authority,
                            &launch_context.worktrees,
                            owner,
                            host_resources,
                        );
                    }
                    #[cfg(feature = "codex-compat")]
                    LocalResidentDeployment::NotificationSend(command) => {
                        let target = command.target();
                        if launch_context.config.backend.kind()
                            == crate::exomonad::ExomonadBackend::Embedded
                        {
                            if let Some(binding) = embedded_binding(&application_owners, target)
                                .filter(|binding| binding.is_live()) {
                                schedule_embedded_notification_send(
                                    command,
                                    &binding,
                                    &mut notifications,
                                );
                            } else {
                                command.rejected(
                                    exomonad_actor::NotificationError::Unavailable,
                                );
                            }
                            continue;
                        }
                        let Some(application) = deployments.iter().find(|app| app.actor == target) else {
                            command.rejected(exomonad_actor::NotificationError::Unavailable);
                            continue;
                        };
                        if !application.thread.as_ref().is_some_and(QueueReadyThread::supports_active_input) {
                            command.rejected(exomonad_actor::NotificationError::Unavailable);
                            continue;
                        }
                        let inbox = Arc::clone(&application.inbox);
                        let key = application.notification_inbox_key.clone();
                        notifications.spawn(async move {
                            let result = tidepool_runtime::spawn_blocking_in_span(move || {
                                admit_notification(&command, key, &inbox);
                            }).await.map_err(|error| error.to_string());
                            (target, result)
                        });
                    }
                    #[cfg(not(feature = "codex-compat"))]
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
                    #[cfg(feature = "codex-compat")]
                    LocalResidentDeployment::NotificationPoll(command) => {
                        if launch_context.config.backend.kind()
                            == crate::exomonad::ExomonadBackend::Embedded
                        {
                            let target = command.receipt().target();
                            let result = match embedded_binding(&application_owners, target) {
                                Some(binding) => {
                                    observe_embedded_notification(&command, &binding, target).await
                                }
                                None => Err(exomonad_actor::NotificationError::Unavailable),
                            };
                            command.observed(result);
                            continue;
                        }
                        let result = deployments.iter()
                            .find(|application| application.actor == command.receipt().target())
                            .ok_or(exomonad_actor::NotificationError::Unavailable)
                            .and_then(|application| observe_notification_receipt(
                                &command, application.actor,
                                &application.notification_inbox_key, &application.inbox,
                            ));
                        command.observed(result);
                    }
                    #[cfg(not(feature = "codex-compat"))]
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
                    #[cfg(feature = "codex-compat")]
                    LocalResidentDeployment::RequestUpdate { delivery } => {
                        let target = delivery.target();
                        let Some(application) = deployments.iter().find(|app| app.actor == target) else {
                            if let Some(presentation) = delivery.begin() {
                                let detail = match application_owners.lock().get(&target) {
                                    // The host admitted this actor (it holds a
                                    // launch-lifecycle row) but has not published
                                    // an application for it yet, if ever.
                                    Some(owner) => format!(
                                        "actor {}@{} admitted; provider not started ({})",
                                        target.id.0,
                                        target.incarnation.0,
                                        owner.launch.provider_not_started_phase()
                                    ),
                                    None => "target application unavailable".into(),
                                };
                                presentation.not_presented(detail);
                            }
                            continue;
                        };
                        if application.thread.is_none() {
                            if let Some(presentation) = delivery.begin() {
                                presentation.not_presented("target conversation is not bound".into());
                            }
                            continue;
                        }
                        let update = delivery.id();
                        let context = DeliveryProvenance::RequestUpdate {
                            owner: delivery.owner(),
                            target,
                            request: update.request,
                            update: update.sequence,
                        };
                        let message = delivery.message().to_owned();
                        let published = application.inbox.publish_tracked(
                            DurableActorEvent::Typed(TypedActorEvent::RequestUpdate {
                                request: update.request,
                                update: update.sequence,
                                message: message.clone(),
                            }),
                            context.clone(),
                        );
                        let envelope = match published {
                            Ok(envelope) => envelope,
                            Err(error) => {
                                if let Some(presentation) = delivery.begin() {
                                    presentation.not_presented(format!("durable update publication failed: {error}"));
                                }
                                continue;
                            }
                        };
                        let Some(sequence) = std::num::NonZeroU64::new(envelope.sequence) else {
                            if let Some(presentation) = delivery.begin() {
                                presentation.not_presented("durable inbox allocated zero sequence".into());
                            }
                            continue;
                        };
                        let operation_id = InputOperationId {
                            producer: application.input_producer.clone(),
                            sequence,
                        };
                        let correlation = exomonad_actor::RequestUpdateCorrelation {
                            producer: operation_id.producer.as_str().to_owned(),
                            sequence,
                        };
                        let reconciler = match delivery.bind_correlation(correlation) {
                            Ok(reconciler) => reconciler,
                            Err(error) => {
                                if let Err(reject_error) = application
                                    .inbox
                                    .confirm_rejected(envelope.sequence, &context)
                                {
                                    tracing::warn!(
                                        sequence = envelope.sequence,
                                        error = %reject_error,
                                        "cannot confirm rejection of an unreconcilable update"
                                    );
                                }
                                if let Some(presentation) = delivery.begin() {
                                    presentation.not_presented(format!(
                                        "update correlation failed: {error}"
                                    ));
                                }
                                continue;
                            }
                        };
                        let Some(presentation) = delivery.begin() else {
                            if let Err(reject_error) = application
                                .inbox
                                .confirm_rejected(envelope.sequence, &context)
                            {
                                tracing::warn!(
                                    sequence = envelope.sequence,
                                    error = %reject_error,
                                    "cannot confirm rejection of an unpresented update"
                                );
                            }
                            continue;
                        };
                        application.update_reconciliations.lock().insert(
                            operation_id.native_key(),
                            PendingUpdateReconciliation {
                                inbox: Arc::clone(&application.inbox),
                                sequence: envelope.sequence,
                                context,
                                reconciler,
                            },
                        );
                        presentation.unconfirmed(
                            "request update is durably queued for ordered native delivery".into(),
                        );
                    }
                    #[cfg(not(feature = "codex-compat"))]
                    LocalResidentDeployment::RequestUpdate { delivery } => {
                        if let Some(presentation) = delivery.begin() {
                            presentation.not_presented(
                                "request updates require a native interactive conversation".into(),
                            );
                        }
                    }
                    #[cfg(feature = "codex-compat")]
                    LocalResidentDeployment::ChildExited { notice } => {
                        if let Some(notification) = prepare_owner_notification(&notice, &deployments) {
                            notifications.spawn(publish_owner_notification(notification));
                        }
                    }
                    #[cfg(not(feature = "codex-compat"))]
                    LocalResidentDeployment::ChildExited { .. } => {}
                    #[cfg(feature = "codex-compat")]
                    LocalResidentDeployment::WatchChanged { notification } => {
                        let Some(application) = deployments
                            .iter()
                            .find(|app| app.actor == notification.owner)
                        else {
                            continue;
                        };
                        notifications.spawn(publish_inbox_event_for(
                            notification.owner,
                            Arc::clone(&application.inbox),
                            DurableActorEvent::Typed(TypedActorEvent::WatchChanged {
                                notification,
                            }),
                        ));
                    }
                    #[cfg(not(feature = "codex-compat"))]
                    LocalResidentDeployment::WatchChanged { .. } => {}
                    #[cfg(feature = "codex-compat")]
                    LocalResidentDeployment::SettlementChanged { notification } => {
                        let Some(application) = deployments
                            .iter()
                            .find(|app| app.actor == notification.owner)
                        else {
                            continue;
                        };
                        notifications.spawn(publish_inbox_event_for(
                            notification.owner,
                            Arc::clone(&application.inbox),
                            DurableActorEvent::Typed(TypedActorEvent::SettlementChanged {
                                notification,
                            }),
                        ));
                    }
                    #[cfg(not(feature = "codex-compat"))]
                    LocalResidentDeployment::SettlementChanged { .. } => {}
                    #[cfg(feature = "codex-compat")]
                    LocalResidentDeployment::RequestCancellation { notification } => {
                        let Some(application) = deployments
                            .iter()
                            .find(|app| app.actor == notification.target)
                        else {
                            continue;
                        };
                        notifications.spawn(publish_inbox_event_for(
                            notification.target,
                            Arc::clone(&application.inbox),
                            DurableActorEvent::Typed(TypedActorEvent::RequestCancellation {
                                notification,
                            }),
                        ));
                    }
                    #[cfg(not(feature = "codex-compat"))]
                    LocalResidentDeployment::RequestCancellation { .. } => {}
                }
            }
            recovered = publication_retries.join_next(), if !publication_retries.is_empty() => {
                if let Some(Err(error)) = recovered {
                    tracing::warn!(%error, "build publication recovery task interrupted; durable state retained");
                }
            }
            launched = launches.join_next(), if !launches.is_empty() => {
                #[cfg(feature = "codex-compat")]
                {
                match launched {
                    Some(Ok(((local_actor, provider_attachment), Ok(Some(launched))))) => {
                        let actor = local_actor.identity();
                        let (already_retired, pending_activations) = {
                            let mut owners = application_owners.lock();
                            #[allow(clippy::expect_used, reason = "registered launch owner")]
                            let owner = owners.get_mut(&actor).expect("registered launch owner");
                            owner.launch = HostLaunchState::Published;
                            (owner.terminal.is_some(), std::mem::take(&mut owner.pending_activations))
                        };
                        let mut deployment = launched.deployment;
                        if let Err(error) = provider_attachment.validate() {
                            spawn_owned_retirement(&mut retirements, deployment, tmux.clone(), &application_owners);
                            break Some(format!("actor {actor:?} provider attachment became unavailable: {error}"));
                        }
                        if already_retired {
                            spawn_owned_retirement(&mut retirements, deployment, tmux.clone(), &application_owners);
                            continue;
                        }
                        if let Some((predecessor, _)) = recovered_threads.get(&actor) {
                            readiness.send(ActorHostReadiness::ActorRecovered {
                                predecessor: *predecessor,
                                actor,
                            }).ok();
                        }
                        if actor == root_identity {
                            if let Some(predecessor) = recovered_root_predecessor {
                                readiness.send(ActorHostReadiness::ActorRecovered {
                                    predecessor,
                                    actor,
                                }).ok();
                            }
                            readiness.send(ActorHostReadiness::AwaitingBinding { root: root_identity }).ok();
                        }
                        let pane = deployment.pane.clone();
                        let fork_gate = deployment.fork_gate.clone();
                        let checkpoint = deployment.checkpoint.clone();
                        let runtime_observation = deployment.runtime_observation.clone();
                        let fork_parent_thread = deployment.fork_parent_thread.clone();
                        let tmux = tmux.clone();
                        binding_discoveries.spawn(async move {
                            let result = AssertUnwindSafe(discover_interactive_binding(
                                actor,
                                launched.binding,
                                &tmux,
                                &pane,
                            ))
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|_| {
                                Err(application_error(
                                    actor,
                                    InteractiveOperation::DiscoverBinding,
                                    "interactive binding task panicked",
                                ))
                            });
                            if let Ok(thread) = &result {
                                runtime_observation.publish_provider_binding(
                                    fork_parent_thread.map(|thread| thread.0),
                                    thread.id().0.clone(),
                                );
                            }
                            let result = match (result, fork_gate) {
                                (Ok(thread), Some(gate)) => {
                                    match gate.mark_ready() {
                                        Err(error) => Err(application_error(
                                            actor,
                                            InteractiveOperation::DiscoverBinding,
                                            error,
                                        )),
                                        Ok(()) => match gate.wait_committed().await {
                                            Ok(()) => Ok(thread),
                                            Err(error) => Err(application_error(
                                                actor,
                                                InteractiveOperation::DiscoverBinding,
                                                error,
                                            )),
                                        },
                                    }
                                }
                                (result, None) => result,
                                (Err(error), Some(_)) => Err(error),
                            };
                            let result = match (result, checkpoint) {
                                (Ok(thread), Some(checkpoint)) => match checkpoint.wait_published().await {
                                    Ok(()) => Ok(thread),
                                    Err(refusal) => Err(application_error(actor, InteractiveOperation::DiscoverBinding,
                                        format!("checkpoint publication refused: {refusal:?}"))),
                                },
                                (result, _) => result,
                            };
                            ((actor, provider_attachment), result)
                        });
                        let mut activation_error = None;
                        for activation in pending_activations {
                            if let Err(error) = deliver_session_activation(&mut deployment, activation).await {
                                activation_error = Some(error);
                                break;
                            }
                        }
                        deployments.push(deployment);
                        if let Some(error) = activation_error { break Some(error); }
                    }
                    Some(Ok(((local_actor, _provider_attachment), Ok(None)))) => {
                        let actor = local_actor.identity();
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.launch = HostLaunchState::Abandoned;
                            owner.cancel();
                        }
                    }
                    Some(Ok(((local_actor, _provider_attachment), Err(error)))) => {
                        let actor = local_actor.identity();
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.launch = match error.disposition {
                                LaunchDisposition::Failed => HostLaunchState::Failed(error.detail.clone()),
                                LaunchDisposition::Cancelled => HostLaunchState::Abandoned,
                            };
                            owner.cancel();
                        }
                        if error.disposition == LaunchDisposition::Failed {
                            if let Err(error) = apply_application_failure(
                                local_actor,
                                ExternalApplicationFailure {
                                    class: error.operation.failure_class(),
                                    detail: error.detail,
                                }
                            ).await {
                                break Some(error);
                            }
                        }
                    }
                    Some(Err(error)) => break Some(format!("interactive launch task: {error}")),
                    None => {}
                }
                }
            }
            discovered = binding_discoveries.join_next(), if !binding_discoveries.is_empty() => {
                #[cfg(feature = "codex-compat")]
                {
                match discovered {
                    Some(Ok(((actor, provider_attachment), Ok(thread)))) => {
                        let Some(deployment) = deployments.iter_mut().find(|app| app.actor == actor) else {
                            continue;
                        };
                        if !matches!(deployment.connection, InteractiveConnection::AwaitingBinding) {
                            break Some(format!("interactive application {actor:?} published more than one conversation binding"));
                        }
                        if let Err(error) = provider_attachment.validate() {
                            break Some(format!("actor {actor:?} provider binding is unavailable: {error}"));
                        }
                        if let Err(error) = launch_context.actor_recovery.bind_application(
                            actor,
                            thread.id().0.clone(),
                        ) {
                            break Some(format!(
                                "interactive application {actor:?} binding could not be journalled: {error}"
                            ));
                        }
                        let Some(native_backend) = backend.codex_backend().cloned() else {
                            break Some(format!("native conversation binding for {actor:?} has no Codex backend"));
                        };
                        if let Err(error) = retain_input_custody_and_bind(
                            &deployment.service,
                            native_backend.clone(),
                            &thread,
                            &deployment.input_producer,
                            &provider_attachment,
                        )
                        .await
                        {
                            break Some(format!(
                                "interactive application {actor:?} {error}"
                            ));
                        }
                        if let Err(error) = provider_attachment.validate() {
                            break Some(format!("actor {actor:?} provider binding became unavailable: {error}"));
                        }
                        let (delivery_shutdown, stop_delivery) = oneshot::channel();
                        let delivery = tokio::spawn(run_delivery_pump(
                            actor,
                            Arc::clone(&deployment.inbox),
                            thread.clone(),
                            native_backend.clone(),
                            deployment.input_producer.clone(),
                            Arc::clone(&deployment.update_reconciliations),
                            deployment.workspace.clone(),
                            deployment.runtime_observation.clone(),
                            deployment.local_actor.clone(),
                            Arc::clone(&watch_retention),
                            Arc::clone(&watch_observation),
                            (actor != root_identity).then(|| Arc::clone(&open_request)),
                            source_layers.clone(),
                            worktrees.clone(),
                            stop_delivery,
                        ));
                        tracing::info!(
                            ?actor,
                            input_producer = deployment.input_producer.as_str(),
                            "installed run-scoped native input producer"
                        );
                        deployment.connection = InteractiveConnection::Bound {
                            delivery_shutdown,
                            delivery,
                        };
                        deployment.thread = Some(thread.clone());
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.creator_workspace = Some(BoundWorkspace {
                                workspace: deployment.active_workspace.clone(), thread: thread.clone(),
                            });
                        }
                        if actor == root_identity {
                            readiness.send(ActorHostReadiness::Ready {
                                root: root_identity,
                                thread,
                            }).ok();
                        }
                    }
                    Some(Ok(((actor, _provider_attachment), Err(error)))) => {
                        let Some(deployment) = deployments.iter_mut().find(|app| app.actor == actor) else {
                            continue;
                        };
                        if let Some(gate) = &deployment.fork_gate {
                            // best-effort: the fork group may already be resolved
                            // by a concurrent path; nothing more to do here.
                            gate.mark_failed().ok();
                        }
                        deployment.failure_reported = true;
                        let Some(local_actor) = deployments
                            .iter()
                            .find(|application| application.actor == actor)
                            .map(|application| application.local_actor.clone())
                        else {
                            break Some(format!("lost exact local actor for failed application {actor:?}"));
                        };
                        if let Err(error) = apply_application_failure(
                            local_actor,
                            ExternalApplicationFailure {
                                class: error.operation.failure_class(),
                                detail: error.detail,
                            }
                        ).await {
                            break Some(error);
                        }
                    }
                    Some(Err(error)) => break Some(format!("interactive binding task: {error}")),
                    None => {}
                }
                }
            }
            retired = retirements.join_next(), if !retirements.is_empty() => {
                #[cfg(feature = "codex-compat")]
                {
                match retired {
                    Some(Ok(receipt)) => {
                        let supervisor = application_owners.lock().get_mut(&receipt.actor).and_then(|owner| {
                            owner.retirement.lock().get_or_insert_with(|| receipt.clone());
                            owner.supervisor
                        });
                        let degraded = receipt.degraded();
                        if degraded {
                            tracing::warn!(actor = ?receipt.actor, components = ?receipt.components, "interactive application cleanup degraded");
                        } else {
                            tracing::info!(actor = ?receipt.actor, "interactive application retired");
                        }
                        // A supervisor that received the release in its stop
                        // receipt needs no notice. One that stopped waiting
                        // (its reply dropped) is told either way, so a
                        // `StoppedReleasing` receipt always gets its ending.
                        let waiters = release_waiters.remove(&receipt.actor).unwrap_or_default();
                        let waited = !waiters.is_empty();
                        let mut answered = false;
                        for waiter in waiters {
                            answered |= waiter.answer(receipt.release());
                        }
                        let notify = if waited { !answered } else { degraded };
                        if notify {
                            if let Some(supervisor) = supervisor {
                                if let Some(application) = deployments.iter().find(|app| app.actor == supervisor) {
                                    notifications.spawn(publish_inbox_event_for(
                                        supervisor,
                                        Arc::clone(&application.inbox),
                                        DurableActorEvent::Typed(TypedActorEvent::CleanupFinished { receipt }),
                                    ));
                                }
                            }
                        }
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "interactive retirement task join failed after cleanup isolation");
                    }
                    None => {}
                }
                }
            }
            notified = notifications.join_next(), if !notifications.is_empty() => {
                #[cfg(feature = "codex-compat")]
                {
                match notified {
                    Some(Ok((_actor, Ok(())))) => {}
                    Some(Ok((actor, Err(error)))) => {
                        tracing::warn!(?actor, %error, "actor notification delivery degraded");
                        let Some(local_actor) = deployments
                            .iter()
                            .find(|application| application.actor == actor)
                            .map(|application| application.local_actor.clone())
                        else {
                            continue;
                        };
                        if let Err(error) = apply_application_failure(
                            local_actor,
                            ExternalApplicationFailure {
                                class: ExternalApplicationFailureClass::ToolHostStartup,
                                detail: error,
                            },
                        )
                        .await
                        {
                            break Some(error);
                        }
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "actor notification task join failed");
                    }
                    None => {}
                }
                }
                #[cfg(not(feature = "codex-compat"))]
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
        if let Some(cancellation) = owner
            .embedded
            .as_ref()
            .and_then(|embedded| embedded.cancellation.as_ref())
        {
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
    let embedded_service_cleanup = match embedded_service.as_mut() {
        Some(service) => service
            .shutdown()
            .await
            .err()
            .map(|error| format!("embedded browser shutdown: {error}")),
        None => None,
    };
    #[cfg(feature = "codex-compat")]
    let launch_cleanup =
        drain_launches_for_shutdown(&mut launches, APPLICATION_SHUTDOWN_TIMEOUT).await;
    #[cfg(feature = "codex-compat")]
    deployments.extend(
        launch_cleanup
            .completed
            .into_iter()
            .map(|launched| launched.deployment),
    );
    #[cfg(feature = "codex-compat")]
    let launch_failure = if launch_cleanup.failures.is_empty() {
        None
    } else {
        Some(
            launch_cleanup
                .failures
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    };
    #[cfg(not(feature = "codex-compat"))]
    let launch_failure: Option<String> = None;
    let publication_cleanup = tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        let mut failure = None;
        while let Some(result) = publication_retries.join_next().await {
            if let Err(error) = result {
                failure.get_or_insert_with(|| format!("build publication recovery task: {error}"));
            }
        }
        failure
    })
    .await
    .unwrap_or_else(|_| {
        publication_retries.abort_all();
        Some("build publication recovery timed out; durable state and storage retained".into())
    });
    #[cfg(feature = "codex-compat")]
    {
        binding_discoveries.abort_all();
        while binding_discoveries.join_next().await.is_some() {}
        let undeployed = application_owners
            .lock()
            .iter()
            .filter(|(actor, owner)| {
                owner.should_retire_undeployed_native(
                    deployments
                        .iter()
                        .any(|deployment| deployment.actor == **actor),
                )
            })
            .map(|(actor, _)| *actor)
            .collect::<Vec<_>>();
        for actor in undeployed {
            spawn_undeployed_hosted_retirement(&mut retirements, actor, &application_owners, &tmux);
        }
        for deployment in deployments {
            spawn_owned_retirement(
                &mut retirements,
                deployment,
                tmux.clone(),
                &application_owners,
            );
        }
    }
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
    #[cfg(feature = "codex-compat")]
    let cleanup_failure = tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        let mut failure = None;
        while let Some(result) = retirements.join_next().await {
            if let Ok(receipt) = &result {
                answer_release_waiters(&mut release_waiters, receipt.actor, receipt.release());
                if let Some(owner) = application_owners.lock().get_mut(&receipt.actor) {
                    owner.retirement.lock().get_or_insert_with(|| receipt.clone());
                }
            }
            match result {
                Ok(receipt) if receipt.degraded() && receipt.actor == root_identity => {
                    failure.get_or_insert_with(|| receipt.render());
                }
                Ok(receipt) if receipt.degraded() => {
                    tracing::warn!(actor = ?receipt.actor, components = ?receipt.components, "child cleanup degraded during host shutdown");
                }
                Ok(_) => {}
                Err(error) => {
                    failure.get_or_insert_with(|| format!("interactive retirement task: {error}"));
                }
            }
        }
        failure
    })
    .await
    .unwrap_or_else(|_| Some("interactive application cleanup timed out".into()));
    #[cfg(not(feature = "codex-compat"))]
    let cleanup_failure: Option<String> = None;
    retain_unsettled_release_waiters(&mut release_waiters);
    let earlier_embedded_cleanup = {
        let failures = application_owners
            .lock()
            .iter()
            .filter_map(|(actor, owner)| {
                owner
                    .embedded
                    .as_ref()
                    .and_then(|embedded| embedded.cleanup_failure.as_ref())
                    .map(|error| format!("embedded Engine {actor:?}: {error}"))
            })
            .collect::<Vec<_>>();
        (!failures.is_empty()).then(|| failures.join("; "))
    };
    let cleanup_failures = [
        earlier_embedded_cleanup,
        launch_failure,
        embedded_cleanup,
        embedded_service_cleanup,
        publication_cleanup,
        notification_cleanup,
        cleanup_failure,
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
