use super::*;

#[cfg(feature = "codex-compat")]
pub(super) async fn launch_prepared_interactive_application(
    installation: LocalResidentInstallation,
    context: InteractiveLaunchContext,
    worktree: Option<WorktreeHandle>,
    mut cancelled: oneshot::Receiver<NativeRetirement>,
    inherited: InteractiveInheritance,
    retention: InteractiveLaunchRetention,
    provider_attachment: provider_attachment::ProviderAttachment,
) -> Result<Option<LaunchedInteractiveApplication>, InteractiveApplicationError> {
    let InteractiveInheritance {
        thread: fork_parent_thread,
        build_snapshot,
    } = inherited;
    let InteractiveLaunchRetention {
        hosted: hosted_slot,
        pane: pane_slot,
        process: scope_slot,
    } = retention;
    let InteractiveLaunchContext {
        base_prompt,
        root,
        config,
        run_root,
        tmux,
        backend,
        worktrees,
        source_layers,
        bindings: _,
        actor_recovery,
        recovered_threads,
    } = context;
    let actor = installation.actor;
    let fork_gate = installation.fork_gate.clone();
    let runtime_observation = installation.runtime_observation.clone();
    let actor_identity = actor.identity();
    let Some(backend) = backend.codex_backend().cloned() else {
        return Err(application_error(
            actor_identity,
            InteractiveOperation::BuildCommand,
            "native interactive launch has no Codex backend adapter",
        ));
    };
    #[cfg(feature = "codex-compat")]
    let interactive_agent = config.backend.interactive_agent().ok_or_else(|| {
        application_error(
            actor_identity,
            InteractiveOperation::BuildCommand,
            "native interactive launch has no Codex installation",
        )
    })?;
    provider_attachment.validate().map_err(|error| {
        application_error(
            actor_identity,
            InteractiveOperation::PrepareRuntime,
            error.to_string(),
        )
    })?;
    let _resource_start = match &config.command_resources {
        Some(owner) => {
            tracing::info!(actor = ?actor_identity, "actor waiting for resource admission");
            runtime_observation.publish_launch_pending("waiting for memory admission");
            let admitted = Some(tokio::select! {
                result = owner.admit_actor() => result.map_err(|error| application_error(actor_identity, InteractiveOperation::LaunchProcess, error.to_string()))?,
                _ = &mut cancelled => return Ok(None),
            });
            tracing::info!(actor = ?actor_identity, "actor resource admission granted");
            runtime_observation.publish_launch_pending("starting the provider");
            admitted
        }
        None => None,
    };
    let workspace = worktree.as_ref().map_or_else(
        || config.workspace.clone(),
        |handle| handle.cwd().to_path_buf(),
    );
    let actor_root = run_root.join(format!(
        "{}-{}",
        actor_identity.id.0, actor_identity.incarnation.0
    ));
    std::fs::create_dir_all(&actor_root).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let agent_workspace = PathBuf::from(ACTOR_PROJECT_ROOT);
    if cancelled.try_recv().is_ok() {
        return Ok(None);
    }
    let prepared_workspace = installation
        .worktree_custody
        .as_ref()
        .and_then(|custody| {
            (custody.as_ref() as &dyn std::any::Any).downcast_ref::<ActorWorkspaceCustody>()
        })
        .and_then(|custody| custody.workspace.clone());
    let prepared_workspace = match prepared_workspace {
        Some(prepared) => prepared,
        None => {
            let layout = WorkspaceLayout {
                run_namespace: runtime_namespace(&run_root),
                source_root: config.workspace.clone(),
                source_exclude: config.source_exclude.clone(),
                source_import: config.source_import,
                root_imports: Arc::default(),
                worktrees: worktrees.clone(),
                base_prompt: base_prompt.clone(),
                backend: backend.clone(),
            };
            let host_path = workspace.clone();
            let id = worktree.as_ref().map(|tree| tree.id().clone());
            let key = id
                .as_ref()
                .map(|id| id.as_str().to_owned())
                .unwrap_or_else(|| {
                    format!(
                        "actor-{}-{}",
                        actor_identity.id.0, actor_identity.incarnation.0
                    )
                });
            let policy = exomonad_actor::ForkWorkspacePolicy {
                native_tools: installation.effective_role.native_tools(),
                workspace: installation.effective_role.workspace(),
            };
            let helper_branch = source_layers
                .as_ref()
                .and_then(|layers| layers.helper_branch_for(actor.identity().into()));
            tidepool_runtime::spawn_blocking_in_span(move || {
                layout.prepare(
                    host_path,
                    id,
                    &key,
                    actor_identity == root,
                    policy,
                    None,
                    build_snapshot,
                    helper_branch,
                )
            })
            .await
            .map_err(|error| {
                application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
            })?
            .map_err(|error| {
                application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
            })?
        }
    };
    let workspace_view = prepared_workspace.view.clone();
    let build_output = prepared_workspace
        .build
        .as_ref()
        .map(|_| agent_workspace.join(ACTOR_BUILD_TARGET));
    let run_socket_id = run_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("run");
    let socket_root = std::env::temp_dir().join(format!(
        "tidepool-{}-{}-{}",
        &run_socket_id[..run_socket_id.len().min(8)],
        actor_identity.id.0,
        actor_identity.incarnation.0
    ));
    let (mut socket_directory, listener, inbox) = prepare_socket_inbox(
        actor_identity,
        socket_root,
        actor_root.join("inbox.jsonl"),
        actor_root.join("inbox.cursor"),
    )?;
    let endpoint = socket_directory.path().join("host-tools.sock");
    let binding_path = if actor_identity == root {
        config.root_binding_path.clone()
    } else {
        actor_root.join("binding.json")
    };
    let accepted_source = active_source_identity(&run_root, config.workspace_inputs.is_some())
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?;
    actor_recovery
        .prepare_application(actor_identity, binding_path.clone(), accepted_source)
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?;
    let launch_mode = if let Some((_, thread)) = recovered_threads.get(&actor_identity) {
        InteractiveLaunchMode::Resume(thread.id().clone())
    } else if let Some(parent) = fork_parent_thread.clone() {
        let boundary = installation
            .fork_boundary
            .as_ref()
            .and_then(tidepool_runtime::session::WorkbenchForkBoundary::hosted)
            .filter(|operation| {
                operation.external_thread() == Some(parent.0.as_str()) && operation.is_complete()
            })
            .ok_or_else(|| {
                application_error(
                    actor_identity,
                    InteractiveOperation::BuildCommand,
                    "context fork has no matching parent hosted-call boundary",
                )
            })?;
        InteractiveLaunchMode::Fork {
            parent,
            after_call: boundary.call_id.clone(),
        }
    } else if actor_identity == root {
        config.root_launch_mode.clone()
    } else {
        InteractiveLaunchMode::Fresh
    };
    let expected_resume = match &launch_mode {
        InteractiveLaunchMode::Resume(thread) => Some(thread.clone()),
        InteractiveLaunchMode::Fresh | InteractiveLaunchMode::Fork { .. } => None,
    };
    runtime_observation.publish_cache_boundary(match &launch_mode {
        InteractiveLaunchMode::Fresh => exomonad_actor::CacheBoundaryReason::Fresh,
        InteractiveLaunchMode::Fork { .. } => exomonad_actor::CacheBoundaryReason::ForkedPrefix,
        InteractiveLaunchMode::Resume(_) => exomonad_actor::CacheBoundaryReason::ReattachedThread,
    });
    let workspace_observation = exomonad_actor::ActorWorkspaceObservation {
        workspace_path: agent_workspace.clone(),
        host_storage_path: workspace.clone(),
        worktree_id: installation.launch_worktrees.first().cloned(),
        expected_branch: worktree
            .as_ref()
            .map(|tree| tree.branch().as_str().to_owned()),
    };
    runtime_observation.publish_workspace(workspace_observation);
    runtime_observation.publish_launch_role(installation.effective_role.clone(), current_time_ms());
    let resolved_worker = installation
        .creator
        .map(|_| {
            resolve_worker_launch(
                &config,
                &exomonad_actor::WorkerLaunchRequest {
                    role: installation.effective_role.clone(),
                    model: installation.model.clone(),
                    effort: installation.fork_effort,
                    context: if matches!(launch_mode, InteractiveLaunchMode::Fork { .. }) {
                        exomonad_actor::ForkContext::InheritedContext
                    } else {
                        exomonad_actor::ForkContext::SelectedContext
                    },
                    instructions: installation.instructions.clone(),
                },
                blake3::hash(base_prompt.body().as_bytes())
                    .to_hex()
                    .as_ref(),
            )
        })
        .transpose()
        .map_err(|detail| {
            application_error(actor_identity, InteractiveOperation::BuildCommand, detail)
        })?;
    let developer_instructions = if let Some(resolved) = &resolved_worker {
        resolved.instructions.clone()
    } else {
        let mut instructions = developer_instructions_selected(
            &installation.effective_role,
            &launch_mode,
            config.workspace_inputs.as_ref(),
            installation.instructions.as_deref(),
        );
        append_inheritance_authority(&mut instructions);
        instructions
    };
    let mut developer_instructions = developer_instructions;
    if let Some(notice) = installation
        .worktree_custody
        .as_ref()
        .and_then(|custody| {
            (custody.as_ref() as &dyn std::any::Any).downcast_ref::<ActorWorkspaceCustody>()
        })
        .and_then(|custody| custody.inheritance_notice.as_ref())
    {
        developer_instructions.push('\n');
        developer_instructions.push_str(notice);
    }
    let developer_instructions =
        orient_launch_instructions(&developer_instructions, &runtime_observation.snapshot());
    runtime_observation.publish_prompt_profile(
        installation.effective_role.prompt_profile(),
        PromptId::CATALOG_VERSION,
        PromptId::composed_fingerprint(
            base_prompt.body(),
            &developer_instructions,
            &exomonad_actor::exomonad_hosted_prompt_fingerprint(),
        ),
    );
    let (model, effort) = if let Some(resolved) = resolved_worker {
        (
            resolved.model,
            launch_effort(&launch_mode, ReasoningEffort::Low, Some(resolved.effort)),
        )
    } else {
        (
            installation
                .model
                .as_ref()
                .map(|model| model.value().to_owned())
                .or_else(|| {
                    (!matches!(launch_mode, InteractiveLaunchMode::Fork { .. }))
                        .then(|| config.model.clone())
                }),
            launch_effort(&launch_mode, config.effort, installation.fork_effort),
        )
    };
    let recovery_notice = matches!(launch_mode, InteractiveLaunchMode::Resume(_))
        .then(|| std::fs::read_to_string(config.run_root.join("host-recovery-notice.txt")).ok())
        .flatten();
    let spec = InteractiveAgentSpec {
        shell_tools: exomonad_agent::InteractiveShellTools::Hosted,
        mode: launch_mode,
        // Exomonad owns continuation on every node. Keep the native tool surface
        // identical across roots and forks, without inheriting native goals.
        goal_policy: exomonad_agent::InteractiveGoalPolicy::Disabled,
        model,
        effort: Some(effort),
        developer_instructions,
        base_instructions_file: base_prompt.file().to_path_buf(),
        initial_prompt: recovery_notice.or_else(|| installation.initial_user_message.clone()),
        native_sandbox: InteractiveNativeSandbox::HostMountBoundary,
        host_tools_socket: endpoint.clone(),
    };
    let command = backend.render(&spec).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::BuildCommand, error)
    })?;
    #[cfg(feature = "codex-compat")]
    runtime_observation.publish_backend_provenance(
        interactive_agent
            .executable()
            .to_string_lossy()
            .into_owned(),
        interactive_agent.version().into(),
        spec.model.clone(),
        spec.effort.map(|effort| format!("{effort:?}")),
    );
    let command = ProcessInvocation {
        program: command.program,
        args: command.args,
    };
    // Accepted hosted work may outlive listener cancellation. Retention starts
    // before either hosted submission or native process submission can occur.
    socket_directory.work_may_exist();
    let service = hosted_retirement::start_with_resources(
        &hosted_slot,
        actor.clone(),
        installation.policy.tools().iter().cloned().collect(),
        binding_path.clone(),
        expected_resume.clone(),
        listener,
        config.command_resources.clone().map(|r| {
            (
                r,
                format!("{}-{}", actor_identity.id.0, actor_identity.incarnation.0),
            )
        }),
        hosted_operation_journal(&run_root, actor_identity.id),
    )
    .map_err(|error| {
        application_error(actor_identity, InteractiveOperation::ServeToolHost, error)
    })?;
    let retirement_service = service.clone();
    let launch_result = async {
    if cancelled.try_recv().is_ok() {
        return Err(socket_launch_cancelled(
            actor_identity,
            "launch cancelled after hosted work submission",
            socket_directory,
        ));
    }
    let mut launch_environment = actor_launch_environment(
        config.pane_environment.clone(),
        actor_identity == root,
        build_output.as_deref(),
    );
    // Shell commands must resolve the same verified installation as delivery.
    // An inherited PATH or override can name a different rollout protocol.
    #[cfg(feature = "codex-compat")]
    launch_environment.set.insert(
        "EXOMONAD_INTERACTIVE_CODEX_BIN".into(),
        interactive_agent
            .executable()
            .to_string_lossy()
            .into_owned(),
    );
    if let Some(owner) = &config.command_resources {
        let actor_key = format!("{}-{}", actor_identity.id.0, actor_identity.incarnation.0);
        let directory = owner.actor_directory(&actor_key).await.map_err(|error| {
            application_error(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                error.to_string(),
            )
        })?;
        launch_environment.set.insert(
            "CODEX_COMMAND_RESOURCE_SOCKET".into(),
            endpoint.to_string_lossy().into_owned(),
        );
        launch_environment.set.insert(
            "CODEX_COMMAND_WRITER_CGROUP".into(),
            directory.to_string_lossy().into_owned(),
        );
    }
    let supervisor_directory = socket_directory.path().join("process-supervisor");
    std::fs::create_dir(&supervisor_directory).map_err(|error| {
        application_error(
            actor_identity,
            InteractiveOperation::PrepareRuntime,
            format!("cannot reserve private process supervisor directory: {error}"),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &supervisor_directory,
            std::fs::Permissions::from_mode(0o700),
        )
        .map_err(|error| {
            application_error(
                actor_identity,
                InteractiveOperation::PrepareRuntime,
                format!("cannot protect process supervisor directory: {error}"),
            )
        })?;
    }
    let supervisor_directory = std::fs::canonicalize(&supervisor_directory).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let launch_id = format!(
        "actor-{}-{}-{}",
        actor_identity.id.0,
        actor_identity.incarnation.0,
        uuid::Uuid::new_v4().simple()
    );
    let pairing_secret = fresh_process_supervisor_secret();
    let recovery_secret = fresh_process_supervisor_secret();
    let bubblewrap = resolve_scope_bubblewrap(&launch_environment.set).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let boundary = ProcessMountBoundary::new(&workspace, [workspace.clone()], [workspace.clone()])
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?;
    let mut manifest = ProcessSupervisorManifest::new(
        launch_id.clone(),
        pairing_secret.clone(),
        recovery_secret.clone(),
        supervisor_directory,
        bubblewrap,
        boundary,
        command,
        ServiceEnvironment {
            set: launch_environment.set.clone(),
            unset: launch_environment.unset.clone(),
        },
    )
    .map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let supervisor_socket = manifest.socket_path();
    manifest.retained_view = Some(exomonad_node::RetainedProcessView {
        entry: workspace_view.entry().map_err(|error| {
            application_error(actor_identity, InteractiveOperation::BuildCommand, error)
        })?,
        directory: agent_workspace.clone(),
    });
    let manifest_path = manifest.write_new().map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    tidepool_atomic_write::write_durable(
        &actor_root.join(PROCESS_RECOVERY_RECORD),
        &serde_json::to_vec_pretty(&ProcessRecoveryRecord {
            version: 1,
            launch_id: launch_id.clone(),
            recovery_secret: recovery_secret.clone(),
            supervisor_socket: supervisor_socket.clone(),
            socket_root: socket_directory.path().to_path_buf(),
            retired: false,
        })
        .map_err(|error| {
            application_error(
                actor_identity,
                InteractiveOperation::PrepareRuntime,
                error,
            )
        })?,
    )
    .map_err(|error| {
        application_error(
            actor_identity,
            InteractiveOperation::PrepareRuntime,
            error,
        )
    })?;
    scoped_custody::stage_supervisor(
        &scope_slot,
        scoped_custody::SupervisorRecoveryKey::new(
            supervisor_socket.clone(),
            launch_id.clone(),
            recovery_secret,
        ),
    )
    .map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let supervisor_command = exomonad_node::ProcessInvocation {
        program: config.exomonad_executable.to_string_lossy().into_owned(),
        args: vec![
            "process-supervisor".into(),
            "--manifest".into(),
            manifest_path.to_string_lossy().into_owned(),
        ],
    };
    let supervisor_command = match &config.systemd_slice {
        Some(slice) => {
            slice.scope(slice.verified_command(&config.exomonad_executable, supervisor_command))
        }
        None => supervisor_command,
    };
    provider_attachment.validate().map_err(|error| {
        application_error(actor_identity, InteractiveOperation::LaunchProcess, error)
    })?;
    let pane = match tokio::time::timeout(
        PROCESS_OPERATION_TIMEOUT,
        tmux.spawn_window(&TmuxLaunch {
            // The full actor path, so a tmux window reads the same as the
            // path in logs, activation messages and sibling rosters.
            window_name: format!(
                "{} [{}@{}]",
                installation.label, actor_identity.id.0, actor_identity.incarnation.0
            ),
            cwd: workspace.clone(),
            program: supervisor_command.program,
            args: supervisor_command.args,
            environment: launch_environment.set,
            unset_environment: launch_environment.unset,
        }),
    )
    .await
    {
        Ok(Ok(pane)) => pane,
        Ok(Err(error)) => {
            return Err(socket_launch_failure(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                error,
                socket_directory,
            ));
        }
        Err(_) => {
            return Err(socket_launch_failure(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                format!("tmux launch exceeded {PROCESS_OPERATION_TIMEOUT:?}"),
                socket_directory,
            ));
        }
    };

    *pane_slot.lock() = Some(pane.clone());
    let active_workspace = match activate_process_supervisor(
        actor_identity,
        scope_slot.clone(),
        supervisor_socket,
        launch_id,
        pairing_secret,
        &mut cancelled,
        prepared_workspace.clone(),
        worktrees.clone(),
        provider_attachment.clone(),
    )
    .await?
    {
        ProcessActivation::Activated(workspace) => workspace,
        ProcessActivation::Cancelled(native_retirement) => {
            let process = retire_scoped_process(Some(scope_slot.clone()), native_retirement).await;
            if matches!(process, Some(CleanupComponentOutcome::Completed)) {
                if let CleanupComponentOutcome::Failed { detail } =
                    retire_pane_artifact(&tmux, &pane, native_retirement).await
                {
                    tracing::warn!(actor = ?actor_identity, %detail, "cannot retire actor pane after cancelled launch");
                }
            }
            return Err(socket_launch_cancelled(
                actor_identity,
                "launch cancelled during exact process activation",
                socket_directory,
            ));
        }
    };
    if let Err(error) = tmux.retain_pane_on_exit(&pane).await {
        tracing::warn!(actor = ?actor_identity, %error, "cannot retain actor pane for exit diagnosis; application remains active");
    }

    if actor_identity == root {
        if let Err(error) = tmux.select_window_for_pane(&pane).await {
            tracing::warn!(actor = ?actor_identity, %error, "cannot select root window; application remains active");
        }
    }
    if let Ok(native_retirement) = cancelled.try_recv() {
        let process = retire_scoped_process(Some(scope_slot.clone()), native_retirement).await;
        if matches!(process, Some(CleanupComponentOutcome::Completed)) {
            if let CleanupComponentOutcome::Failed { detail } =
                retire_pane_artifact(&tmux, &pane, native_retirement).await
            {
                tracing::warn!(actor = ?actor_identity, %detail, "cannot retire actor pane after cancelled launch");
            }
        }
        return Err(socket_launch_cancelled(
            actor_identity,
            "launch cancelled after native submission",
            socket_directory,
        ));
    }
    tracing::info!(
        actor = ?actor_identity,
        pane = pane.as_str(),
        worktree = worktree
            .as_ref()
            .map(|handle| handle.id().as_str())
            .unwrap_or("source"),
        "interactive application launched"
    );
    let notification_inbox_key = format!(
        "{}:{}:{}",
        runtime_namespace(&run_root),
        actor_identity.id.0,
        actor_identity.incarnation.0
    );
    let input_producer = input_producer_id(&run_root, actor_identity, &notification_inbox_key)
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?;
    let binding_control = service.lock().await.control.clone();
    Ok(Some(LaunchedInteractiveApplication {
        deployment: InteractiveDeployment {
            _provider_attachment: provider_attachment,
            active_workspace,
            supervisor: installation.supervisor_parent,
            notified_provider_failures: Default::default(),
            actor: actor_identity,
            local_actor: actor,
            pane,
            workspace,
            inbox,
            notification_inbox_key,
            input_producer,
            update_reconciliations: Arc::new(Mutex::new(BTreeMap::new())),
            connection: InteractiveConnection::AwaitingBinding,
            service,
            socket_directory,
            process_recovery_record: actor_root.join(PROCESS_RECOVERY_RECORD),
            worktree_custody: installation.worktree_custody.clone(),
            failure_reported: false,
            last_activation_sequence: 0,
            thread: None,
            fork_gate,
            checkpoint: installation.checkpoint.clone(),
            runtime_observation,
            fork_parent_thread,
        },
        binding: InteractiveBindingRequest {
            control: binding_control,
            path: binding_path,
            expected: expected_resume,
        },
    }))
    }
    .await;
    if let Err(error) = &launch_result {
        // The hosted service can retire this actor before the host consumes
        // `launch_result`. Carry the launch failure into that terminal so
        // request settlement sees the original cause after cleanup.
        hosted_retirement::confirm_no_input_producer(&retirement_service)
            .await
            .ok();
        let boundary = match error.disposition {
            LaunchDisposition::Failed => {
                hosted_retirement::CompletionBoundary::AbortForFailure(ActorTerminal {
                    kind: ActorExitKind::Failed,
                    summary: format!("native actor application failed: {}", error.detail),
                })
            }
            LaunchDisposition::Cancelled => hosted_retirement::CompletionBoundary::AbortForShutdown,
        };
        drop(
            hosted_retirement::observe(
                &retirement_service,
                boundary,
                APPLICATION_TASK_GRACE_TIMEOUT,
            )
            .await,
        );
    }
    launch_result
}

enum ProcessActivation {
    Activated(ActiveWorkspace),
    Cancelled(NativeRetirement),
}

/// Cross the process-supervisor phase boundary only after its exact identity,
/// workspace, provider attachment, and release have all been verified.
async fn activate_process_supervisor(
    actor: ActorRef,
    slot: Arc<Mutex<scoped_custody::ScopedProcessSlot>>,
    socket: PathBuf,
    launch_id: String,
    pairing_secret: String,
    cancelled: &mut oneshot::Receiver<NativeRetirement>,
    workspace: PreparedWorkspace,
    worktrees: exomonad_worktree::WorktreeRegistry,
    provider: provider_attachment::ProviderAttachment,
) -> Result<ProcessActivation, InteractiveApplicationError> {
    let activation_cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let activation_cancelled_worker = activation_cancelled.clone();
    // Cancellation and release share one linearization point. If cancellation
    // acquires it first, the worker cannot submit release; if release acquires
    // it first, later cancellation retires an already committed launch.
    let release_gate = Arc::new(std::sync::Mutex::new(()));
    let release_gate_worker = release_gate.clone();
    let activation_slot = slot;
    let mut activation_task = tidepool_runtime::spawn_blocking_in_span(move || {
        let deadline = std::time::Instant::now() + PROCESS_OPERATION_TIMEOUT;
        while !socket.exists() {
            if std::time::Instant::now() >= deadline {
                return Err(scoped_custody::ScopedProcessError::WrongPhase);
            }
            #[allow(
                clippy::disallowed_methods,
                reason = "dedicated blocking-pool thread (spawn_blocking_in_span), not async context"
            )]
            std::thread::sleep(Duration::from_millis(10));
        }
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or(scoped_custody::ScopedProcessError::WrongPhase)?;
        let (client, observation) =
            ProcessSupervisorClient::pair(socket, launch_id, pairing_secret, remaining)?;
        scoped_custody::install_supervisor(&activation_slot, client, observation)?;
        if activation_cancelled_worker.load(std::sync::atomic::Ordering::Acquire) {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        if scoped_custody::prepare_supervisor_slot(&activation_slot, deadline)?
            != scoped_custody::ScopedProcessObservation::Blocked
        {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        if activation_cancelled_worker.load(std::sync::atomic::Ordering::Acquire) {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        if scoped_custody::pin_supervisor_slot(&activation_slot, deadline)?
            != scoped_custody::ScopedProcessObservation::Pinned
        {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        if activation_cancelled_worker.load(std::sync::atomic::Ordering::Acquire) {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        let _release = release_gate_worker
            .lock()
            .map_err(|_| scoped_custody::ScopedProcessError::WrongPhase)?;
        if activation_cancelled_worker.load(std::sync::atomic::Ordering::Acquire) {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        provider
            .validate()
            .map_err(|_| scoped_custody::ScopedProcessError::WrongPhase)?;
        let view = scoped_custody::supervisor_workspace(&activation_slot, deadline)?;
        let active = workspace.activate(&worktrees, view)?;
        provider
            .validate()
            .map_err(|_| scoped_custody::ScopedProcessError::WrongPhase)?;
        match scoped_custody::release_supervisor_slot(&activation_slot, deadline)? {
            scoped_custody::ScopedProcessObservation::Released => Ok(active),
            // A committed but unconfirmed release is never retried. Retain the
            // row for explicit recovery rather than publishing readiness.
            scoped_custody::ScopedProcessObservation::ReleaseUnconfirmed => {
                Err(scoped_custody::ScopedProcessError::WrongPhase)
            }
            _ => Err(scoped_custody::ScopedProcessError::WrongPhase),
        }
    });
    let activation = tokio::select! {
        result = &mut activation_task => result,
        retirement = &mut *cancelled => {
            let _release = release_gate.lock().map_err(|_| {
                application_error(
                    actor,
                    InteractiveOperation::LaunchProcess,
                    "process supervisor release gate poisoned",
                )
            })?;
            activation_cancelled.store(true, std::sync::atomic::Ordering::Release);
            let requested = retirement.unwrap_or(NativeRetirement::Preserve);
            drop(_release);
            let _activation = activation_task.await;
            return Ok(ProcessActivation::Cancelled(requested));
        }
    };
    let workspace = activation
        .map_err(|error| {
            application_error(
                actor,
                InteractiveOperation::LaunchProcess,
                format!("process supervisor activation task failed: {error}"),
            )
        })?
        .map_err(|error| application_error(actor, InteractiveOperation::LaunchProcess, error))?;
    Ok(ProcessActivation::Activated(workspace))
}
