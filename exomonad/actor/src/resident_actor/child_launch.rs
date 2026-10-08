//! Captured child admission, external startup and fenced parent application.

use super::*;

pub(super) struct PreparedChildLaunch {
    pub continuation: ChildLaunchContinuation,
    pub admission: Result<ChildLaunchAdmission, ResidentActorWorkbenchError>,
}

pub(super) struct ChildLaunchContinuation {
    pub context: ActorSessionContext,
    pub parent_descriptor: ActorDescriptor,
    pub control: Option<Arc<crate::WorkbenchExecutionControl>>,
    pub invocation_work: Option<Arc<InvocationWork>>,
    pub parent_hole: ResidentHole,
    pub fork_reply: ForkReply,
    pub fork_group: Option<crate::ForkGroupId>,
    pub original_placement: crate::ActorPlacement,
}

pub(super) struct ChildLaunchAdmission {
    pub child: crate::start::CapturedChildLaunch,
    pub checkpoint_admission: Option<(crate::CheckpointLease, Option<HostedCheckpointAttachment>)>,
    pub inherited_host_attachment: Option<HostedCheckpointAttachment>,
    pub inherited_source: Option<crate::CheckpointSourceLayer>,
    pub retained_checkpoint_scope: Option<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
    pub child_session_startup: Option<crate::resident_workbench::ChildSessionStartupLease>,
    pub invocation_work: Option<Arc<InvocationWork>>,
}

pub(super) struct CompletedChildLaunch {
    continuation: ChildLaunchContinuation,
    result: Result<LaunchedChild, ResidentActorWorkbenchError>,
}

struct LaunchedChild {
    actor: LocalActorRef,
    allocated_label: String,
    admitted_worktree: Option<tidepool_bridge_effects::WtWorktreeHandle>,
    checkpoint_descriptor: Option<ActorDescriptor>,
    checkpoint_admission: Option<(crate::CheckpointLease, Option<HostedCheckpointAttachment>)>,
    inherited_source: Option<crate::CheckpointSourceLayer>,
    source_layers: Option<crate::ActorSourceLayerResolver>,
    helper_branch: Option<String>,
    bound_worktrees: Vec<String>,
}

pub(super) struct ChildLaunchResume {
    context: ActorSessionContext,
    parent_hole: ResidentHole,
    fork_reply: ForkReply,
    fork_group: Option<crate::ForkGroupId>,
    invocation_work: Option<Arc<InvocationWork>>,
    original_placement: crate::ActorPlacement,
    failed_child: Option<LocalActorRef>,
    result: Result<
        (
            LocalActorRef,
            String,
            Option<tidepool_bridge_effects::WtWorktreeHandle>,
        ),
        ResidentActorWorkbenchError,
    >,
}

pub(super) async fn await_launch<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    prepared: PreparedChildLaunch,
) -> CompletedChildLaunch
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let PreparedChildLaunch {
        continuation,
        admission,
    } = prepared;
    let context = &continuation.context;
    let result = Box::pin(async {
        let ChildLaunchAdmission {
            child,
            checkpoint_admission,
            inherited_host_attachment,
            inherited_source,
            retained_checkpoint_scope,
            child_session_startup,
            invocation_work,
        } = admission?;
        let crate::start::CapturedChildLaunch {
            lifetime,
            mut descriptor,
            entry,
            mut launch_worktrees,
            fork_workspace,
            seed,
        } = child;
        let fork_group = descriptor.fork_group();
        let checkpoint_lease = checkpoint_admission
            .as_ref()
            .map(|(lease, _)| lease.clone());
        let root_admission = environment.root_admission_closed.clone();
        let _root_admission = if descriptor.supervisor_parent().is_none() {
            let admission = root_admission.read().await;
            if *admission {
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "swarm root admission is closed".into(),
                ));
            }
            Some(admission)
        } else {
            None
        };
        let prepared_workspace = if let Some(seed) = fork_workspace {
            let admission = environment.fork_workspaces.clone().ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    "context-fork workspace admission is not installed".into(),
                )
            })?;
            let owner = context.actor;
            let actor_path = descriptor.label().to_owned();
            let admitted = admission
                .admit(
                    owner,
                    actor_path,
                    seed,
                    crate::ForkWorkspacePolicy {
                        native_tools: descriptor.effective_role().native_tools(),
                        workspace: descriptor.effective_role().workspace(),
                    },
                )
                .await
                .map_err(|error| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "worktree admission for `{}` failed: {}",
                        descriptor.label(),
                        error
                    ))
                })?;
            launch_worktrees = vec![admitted.handle().handle_receipt.tree_id.raw.clone()];
            Some(admitted)
        } else {
            None
        };
        // The child may carry declarations that import a helper published by
        // its parent. Fix its own snapshot and include roots before a fresh
        // machine bootstraps those declarations.
        let source_layers = environment.source_layers.clone();
        let helper_branch = if let Some(layers) = &source_layers {
            Some(
                layers
                    .prepare_helpers(
                        context.actor.into(),
                        &launch_worktrees,
                        prepared_workspace.is_some(),
                    )
                    .map_err(ResidentActorWorkbenchError::ActorProtocol)?,
            )
        } else {
            None
        };
        if let Some(layers) = &source_layers {
            let layer = match &checkpoint_lease {
                Some(lease) => layers
                    .admit_checkpoint_layer(
                        &lease.issuer_source_layer,
                        context.actor.into(),
                        helper_branch.as_deref().unwrap_or_default(),
                        &launch_worktrees,
                    )
                    .map_err(ResidentActorWorkbenchError::ActorProtocol)?,
                None => match &inherited_source {
                    Some(source) => layers.admit_retained_layer(source),
                    None => layers.layer_include_for(
                        helper_branch.as_deref().unwrap_or_default(),
                        &launch_worktrees,
                    ),
                }
                .map_err(ResidentActorWorkbenchError::ActorProtocol)?,
            };
            descriptor = descriptor.with_source_layer(layer);
        }
        if let Some(retained_scope) = &retained_checkpoint_scope {
            let scope = environment
                .runner
                .remint_checkpoint_child_scope_from_lease(
                    context.clone(),
                    retained_scope.clone(),
                    descriptor.placement().lexical_scope,
                )
                .await?;
            descriptor = descriptor.with_lexical_scope(scope);
        }
        // A launch whose descriptor still names the launching session (the
        // common case: ineligible, or `InheritedContext`) needs nothing
        // further — the entry is already resident there. An eligible
        // `SelectedContext` launch's descriptor names a freshly minted
        // session instead (`child_session_eligibility`/`capture_decoded`):
        // provision that session's own dedicated machine now (build,
        // bootstrap with the run's shared program, install the shared
        // image registry, mint its own lexical scope), then cross the
        // entry into it with the same transfer primitive every other
        // resident-machine-boundary site already uses
        // (`ResidentActorRunner::transfer_custody`, parcel 6). Failure at
        // any step here leaves the launching session untouched and starts
        // nothing; the caller (`start_child`) discards a provisioned but
        // now-orphaned child session on any error this whole admission
        // sequence returns from here on, by comparing the ORIGINAL
        // captured launch's session against the one it actually admits on.
        let entry = if descriptor.placement().session == context.placement.session {
            entry
        } else if !environment.runner.supports_child_sessions() {
            // Eligible, but this host never installed a child-session
            // factory/bootstrap program (`ResidentActorRunner::supports_child_sessions`)
            // — fall back to the launching session, rather than failing an
            // otherwise-ordinary fork over a capability nothing asked for.
            // `capture_decoded` minted no real lexical scope for this
            // (eligible) launch, only a placeholder; mint the actual one
            // here, on the session this actor is actually falling back to.
            let lexical_scope = environment
                .runner
                .mint_lexical_scope(context.placement.session)
                .await?;
            descriptor = descriptor
                .with_session(context.placement.session)
                .with_lexical_scope(lexical_scope);
            entry
        } else {
            let child_session = descriptor.placement().session;
            let lexical_scope = environment
                .runner
                .provision_child_session(
                    child_session,
                    descriptor.placement().resource_scope,
                    seed.as_ref(),
                    descriptor.source_layer(),
                )
                .await
                .map_err(ResidentActorWorkbenchError::ActorProtocol)?;
            descriptor = descriptor.with_lexical_scope(lexical_scope);
            environment
                .runner
                .transfer_custody(
                    entry,
                    context.placement.session,
                    child_session,
                    descriptor.placement().resource_scope,
                )
                .await?
        };
        // The checkout-derived include roots were fixed before any fresh
        // child machine bootstrapped inherited declarations.
        let allocated_label = descriptor.label().to_string();
        let admitted_worktree = prepared_workspace
            .as_ref()
            .map(|prepared| prepared.handle().clone());
        let bound_worktrees = launch_worktrees.clone();
        let descriptor_placement = descriptor.placement();
        let checkpoint_descriptor = fork_group.map(|_| descriptor.clone());
        let mut behavior =
            ResidentKernelBehavior::child(descriptor, environment.clone(), entry, launch_worktrees);
        behavior.admitted_checkpoint = checkpoint_admission.clone();
        behavior.inherited_host_attachment = inherited_host_attachment;
        behavior.prepared_workspace = prepared_workspace;
        // Keep preparation custody until a failed spawn's native cleanup
        // has finished, even if Ractor already dropped the behavior.
        behavior.child_session_startup = child_session_startup.clone();
        let startup_admission = match lifetime {
            crate::WorkerLifetime::InvocationOwned => {
                Some(invocation_work.clone().ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "invocation-owned worker has no request owner".into(),
                    )
                })?)
            }
            crate::WorkerLifetime::ActorOwned | crate::WorkerLifetime::SwarmOwned => None,
        };
        let child_result = match startup_admission {
            Some(admission) => {
                let admission: Arc<dyn crate::local_actor::WorkerStartupAdmission> = admission;
                kernel
                    .spawn_worker_scoped(None, behavior, lifetime, admission)
                    .await
            }
            None => kernel.spawn_worker(None, behavior, lifetime).await,
        };
        let child = match child_result {
            Ok(child) => child,
            Err(error) => {
                settle_startup_refusal(&environment.runner, &kernel, descriptor_placement, &error)
                    .await;
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    error.to_string(),
                ));
            }
        };
        Ok(LaunchedChild {
            actor: child,
            allocated_label,
            admitted_worktree,
            checkpoint_descriptor,
            checkpoint_admission,
            inherited_source,
            source_layers,
            helper_branch,
            bound_worktrees,
        })
    })
    .await;
    CompletedChildLaunch {
        continuation,
        result,
    }
}

async fn settle_startup_refusal<H, O>(
    runner: &ResidentActorRunner<H, O>,
    kernel: &KernelContext,
    placement: crate::ActorPlacement,
    error: &ractor::SpawnErr,
) where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    use crate::CleanupComponentOutcome::{Confirmed, Unconfirmed};
    // The kernel already retained every component of this actual cleanup.
    if crate::local_actor::startup_cleanup(error).is_some() {
        return;
    }
    let cleanup = match runner
        .retire_root_placement_wait(placement, crate::local_actor::SHUTDOWN_BUDGET)
        .await
    {
        Ok(()) => Confirmed,
        Err(cleanup) => Unconfirmed(format!(
            "child startup failed: {error}; placement cleanup: {cleanup}"
        )),
    };
    kernel.retain_child_startup_cleanup(cleanup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::BoxFuture;

    struct CaptureKernel(Option<tokio::sync::oneshot::Sender<KernelContext>>);

    impl KernelBehavior for CaptureKernel {
        fn start(
            &mut self,
            kernel: &KernelContext,
        ) -> BoxFuture<'_, Result<KernelStep<()>, KernelBehaviorError>> {
            self.0.take().unwrap().send(kernel.clone()).ok().unwrap();
            Box::pin(async { Ok(KernelStep::Continue(())) })
        }

        fn cast(
            &mut self,
            _: &KernelContext,
            _: ActorRef,
            _: MailboxValue,
        ) -> BoxFuture<'_, Result<KernelStep<()>, KernelBehaviorError>> {
            Box::pin(async { panic!("fixture has no casts") })
        }

        fn call(
            &mut self,
            _: &KernelContext,
            _: ActorRef,
            _: crate::CallAncestry,
            _: MailboxValue,
        ) -> BoxFuture<'_, Result<KernelStep<MailboxValue>, KernelBehaviorError>> {
            Box::pin(async { panic!("fixture has no calls") })
        }

        fn tool<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: exomonad_tool::ToolInvocation,
            _: Option<Arc<dyn crate::HostedCheckpointCapture>>,
        ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
            Box::pin(async { panic!("fixture has no tools") })
        }

        fn workbench<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: crate::ActorWorkbenchInvocation,
            _: Option<Arc<crate::WorkbenchExecutionControl>>,
        ) -> BoxFuture<
            'a,
            Result<
                KernelStep<tidepool_runtime::session::WorkbenchResponse>,
                KernelInvocationFailure,
            >,
        > {
            Box::pin(async { panic!("fixture has no workbench requests") })
        }

        fn external_application_failed(
            &mut self,
            _: &KernelContext,
            _: ExternalApplicationFailure,
        ) -> BoxFuture<'_, ExternalFailureDisposition> {
            Box::pin(async { panic!("fixture has no external applications") })
        }

        fn shutdown(
            &mut self,
            _: &KernelContext,
            _: &ActorTerminal,
        ) -> BoxFuture<'_, Result<(), KernelBehaviorError>> {
            Box::pin(async { Ok(()) })
        }

        fn stopped(&mut self, _: &KernelContext, _: &ActorTerminal) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }

        fn child_exited(&mut self, _: ChildExitNotice) {}
    }

    struct RefuseStartup;

    impl crate::local_actor::WorkerStartupAdmission for RefuseStartup {
        fn reserve(&self, _: ActorRef) -> Result<(), String> {
            Err("invocation startup reservation refused".into())
        }

        fn admit(&self, _: LocalActorRef) -> Result<(), String> {
            panic!("refused reservation cannot reach actor admission")
        }
    }

    type TestEnvironment = ResidentEnvironment<frunk::HNil, tidepool_mcp::CapturedOutput>;

    async fn fixture() -> (
        TestEnvironment,
        KernelContext,
        LocalActorRef,
        tempfile::TempDir,
    ) {
        use tidepool_runtime::session::{ModuleEnv, SessionLib};
        let root = tempfile::tempdir().unwrap();
        let session_id = tidepool_repr::SessionId(0x57_A6_E);
        let lib =
            SessionLib::open(session_id, root.path(), ModuleEnv::standalone_default()).unwrap();
        let machine = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let (forest, _deployments) = ResidentForest::new(
            ActorWorkbenchSource::new("", Vec::new()),
            session_id,
            machine,
            None,
            crate::Incarnation(1),
        );
        let (context_tx, context_rx) = tokio::sync::oneshot::channel();
        let (parent, _task) = crate::local_actor::spawn_local_actor_in_directory(
            None,
            CaptureKernel(Some(context_tx)),
            crate::Incarnation(1),
            forest.directory.clone(),
        )
        .await
        .unwrap();
        (forest.environment, context_rx.await.unwrap(), parent, root)
    }

    fn terminal() -> ActorTerminal {
        ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "fixture cleanup".into(),
            diagnostic: None,
        }
    }

    async fn prepared_child(
        environment: &TestEnvironment,
        kernel: &KernelContext,
    ) -> (
        crate::ActorPlacement,
        ResidentKernelBehavior<frunk::HNil, tidepool_mcp::CapturedOutput>,
    ) {
        let placement = environment
            .runner
            .provision_root_scope(tidepool_repr::SessionId(0x57_A6_E))
            .await
            .unwrap();
        let descriptor = ActorDescriptor::new("prepared child", placement)
            .with_supervisor_parent(kernel.identity());
        let behavior = ResidentKernelBehavior::with_boot(
            descriptor,
            environment.clone(),
            ResidentBoot::Workbench,
            Vec::new(),
        );
        (placement, behavior)
    }

    async fn assert_scope_retired(environment: &TestEnvironment, placement: crate::ActorPlacement) {
        assert!(
            environment
                .runner
                .retain_fork_release_scope(placement.session, placement.lexical_scope)
                .await
                .is_err(),
            "real scope owner must refuse the retired lexical scope"
        );
        assert!(
            environment
                .runner
                .machines_for_test()
                .kind(placement.session)
                .is_some(),
            "shared native session stays available"
        );
    }

    #[tokio::test]
    async fn closed_parent_refusal_retires_prepared_shared_scope() {
        let (environment, kernel, parent, _root) = fixture().await;
        let (placement, behavior) = prepared_child(&environment, &kernel).await;
        parent.shutdown(terminal()).await.unwrap();
        let error = kernel
            .spawn_worker(None, behavior, crate::WorkerLifetime::ActorOwned)
            .await
            .unwrap_err();
        assert!(
            crate::local_actor::startup_cleanup(&error)
                .unwrap()
                .is_confirmed(),
            "parent refusal runs real cleanup before member insertion"
        );
        assert!(error
            .to_string()
            .contains("actor child admission is closed"));
        settle_startup_refusal(&environment.runner, &kernel, placement, &error).await;
        assert_scope_retired(&environment, placement).await;
    }

    #[tokio::test]
    async fn startup_reservation_refusal_awaits_prepared_shared_scope_cleanup() {
        let (environment, kernel, parent, _root) = fixture().await;
        let (placement, behavior) = prepared_child(&environment, &kernel).await;
        let checkout = environment
            .runner
            .machines_for_test()
            .checkout_run(placement.session)
            .unwrap();
        let mut startup = Box::pin(kernel.spawn_worker_scoped(
            None,
            behavior,
            crate::WorkerLifetime::InvocationOwned,
            Arc::new(RefuseStartup),
        ));
        use std::future::Future;
        let progress =
            std::future::poll_fn(|cx| std::task::Poll::Ready(startup.as_mut().poll(cx))).await;
        assert!(
            matches!(progress, std::task::Poll::Pending),
            "refusal waits for actual native cleanup checkout"
        );
        drop(checkout);
        let error = startup.await.unwrap_err();
        assert!(crate::local_actor::startup_cleanup(&error)
            .unwrap()
            .is_confirmed());
        assert!(error
            .to_string()
            .contains("invocation startup reservation refused"));
        settle_startup_refusal(&environment.runner, &kernel, placement, &error).await;
        assert_scope_retired(&environment, placement).await;
        let stopped = parent.shutdown_with_cleanup(terminal()).await.unwrap();
        assert_eq!(
            stopped.cleanup.children(),
            &crate::CleanupComponentOutcome::Confirmed
        );
    }

    #[tokio::test]
    async fn scheduler_refusal_awaits_scope_cleanup_without_upgrading_uncertainty() {
        let (environment, kernel, parent, _root) = fixture().await;
        let name = format!("staged-startup-refusal-{}", parent.identity());
        let (context_tx, context_rx) = tokio::sync::oneshot::channel();
        kernel
            .spawn_child(Some(name.clone()), CaptureKernel(Some(context_tx)))
            .await
            .unwrap();
        let _child_context = context_rx.await.unwrap();
        let (placement, behavior) = prepared_child(&environment, &kernel).await;
        let error = kernel
            .spawn_worker(Some(name), behavior, crate::WorkerLifetime::ActorOwned)
            .await
            .unwrap_err();
        assert!(crate::local_actor::startup_cleanup(&error).is_none());
        let checkout = environment
            .runner
            .machines_for_test()
            .checkout_run(placement.session)
            .unwrap();
        let mut cleanup = Box::pin(settle_startup_refusal(
            &environment.runner,
            &kernel,
            placement,
            &error,
        ));
        use std::future::Future;
        let progress =
            std::future::poll_fn(|cx| std::task::Poll::Ready(cleanup.as_mut().poll(cx))).await;
        assert!(matches!(progress, std::task::Poll::Pending));
        drop(checkout);
        cleanup.await;
        assert_scope_retired(&environment, placement).await;
        let stopped = parent.shutdown_with_cleanup(terminal()).await.unwrap();
        assert!(matches!(
            stopped.cleanup.children(),
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
    }

    #[tokio::test]
    async fn unavailable_refusal_cleanup_remains_unconfirmed() {
        let (environment, kernel, parent, _root) = fixture().await;
        let (placement, behavior) = prepared_child(&environment, &kernel).await;
        assert!(environment
            .runner
            .machines_for_test()
            .remove(
                placement.session,
                "native machine lost before startup cleanup"
            )
            .is_some());
        let error = kernel
            .spawn_worker_scoped(
                None,
                behavior,
                crate::WorkerLifetime::InvocationOwned,
                Arc::new(RefuseStartup),
            )
            .await
            .unwrap_err();
        assert!(!crate::local_actor::startup_cleanup(&error)
            .unwrap()
            .is_confirmed());
        settle_startup_refusal(&environment.runner, &kernel, placement, &error).await;
        let stopped = parent.shutdown_with_cleanup(terminal()).await.unwrap();
        assert!(matches!(
            stopped.cleanup.children(),
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
    }
}

pub(super) fn matches_parent(
    kernel: &KernelContext,
    current: &ActorDescriptor,
    original: &ActorDescriptor,
    actor: ActorRef,
) -> bool {
    !(actor != kernel.identity()
        || kernel.requested_shutdown().is_some()
        || current.placement() != original.placement()
        || current.actor_path() != original.actor_path()
        || current.creator() != original.creator()
        || current.supervisor_parent() != original.supervisor_parent()
        || current.context_parent() != original.context_parent()
        || current.profile() != original.profile()
        || current.effective_role() != original.effective_role()
        || current.persistence_policy() != original.persistence_policy()
        || current.source_layer() != original.source_layer())
}

pub(super) fn apply_launch<H, O>(
    environment: &ResidentEnvironment<H, O>,
    kernel: &KernelContext,
    descriptor: &ActorDescriptor,
    completed: CompletedChildLaunch,
) -> ChildLaunchResume
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let CompletedChildLaunch {
        continuation,
        result,
    } = completed;
    let context = &continuation.context;
    let original = &continuation.parent_descriptor;
    let failed_child = result.as_ref().ok().map(|started| started.actor.clone());
    let result = result.and_then(|started| {
        if continuation
            .control
            .as_ref()
            .is_some_and(|control| control.cancellation_requested())
            || !matches_parent(kernel, descriptor, original, context.actor)
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "child launch parent admission changed during startup".into(),
            ));
        }
        let LaunchedChild {
            actor: child,
            allocated_label,
            admitted_worktree,
            checkpoint_descriptor,
            checkpoint_admission,
            inherited_source,
            source_layers,
            helper_branch,
            bound_worktrees,
        } = started;
        let fork_group = continuation.fork_group;
        let checkpoint_lease = checkpoint_admission
            .as_ref()
            .map(|(lease, _)| lease.clone());
        let checkpoint_attachment = checkpoint_admission
            .as_ref()
            .and_then(|(_, attachment)| attachment.clone());
        if let (Some(group), Some(lease), Some(descriptor)) = (
            fork_group,
            checkpoint_lease.as_ref(),
            checkpoint_descriptor.as_ref(),
        ) {
            environment
                .fork_groups
                .retain_checkpoint_admission(
                    group,
                    context.actor,
                    child.identity(),
                    descriptor,
                    lease,
                    checkpoint_attachment.as_ref(),
                )
                .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
        } else if let (Some(group), Some(descriptor)) = (fork_group, checkpoint_descriptor.as_ref())
        {
            if descriptor.context_parent().is_none() {
                environment
                    .fork_groups
                    .retain_selected_admission(group, context.actor, child.identity(), descriptor)
                    .map_err(|error| {
                        ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                    })?;
            }
        }
        // The child now has a principal, so the layer its descriptor carries
        // can be named as its own. This happens before the child runs, so its
        // first cell already reaches its own layer and no other.
        if let Some(layers) = &source_layers {
            if let Some(lease) = &checkpoint_lease {
                layers
                    .bind_checkpoint_for(
                        child.identity().into(),
                        helper_branch.as_deref().unwrap_or_default(),
                        &lease.issuer_source_layer,
                    )
                    .map_err(ResidentActorWorkbenchError::ActorProtocol)?;
            } else if let Some(source) = &inherited_source {
                layers
                    .bind_checkpoint_for(
                        child.identity().into(),
                        helper_branch.as_deref().unwrap_or_default(),
                        source,
                    )
                    .map_err(ResidentActorWorkbenchError::ActorProtocol)?;
            } else {
                layers.bind_for(
                    child.identity().into(),
                    helper_branch.as_deref().unwrap_or_default(),
                    &bound_worktrees,
                );
            }
        }
        Ok((child, allocated_label, admitted_worktree))
    });
    ChildLaunchResume {
        context: continuation.context,
        parent_hole: continuation.parent_hole,
        fork_reply: continuation.fork_reply,
        fork_group: continuation.fork_group,
        invocation_work: continuation.invocation_work,
        original_placement: continuation.original_placement,
        failed_child: result.as_ref().err().and(failed_child),
        result,
    }
}

pub(super) async fn resume_launch<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    resume: ChildLaunchResume,
) -> Result<ResidentOutcome, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let ChildLaunchResume {
        context,
        parent_hole,
        fork_reply,
        fork_group,
        invocation_work,
        original_placement,
        failed_child,
        result,
    } = resume;
    if let Some(child) = failed_child {
        if let Some(invocation) = &invocation_work {
            invocation.retain_aborted_children(&kernel, &[child.identity()]);
        }
        if let Err(error) = child
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "child launch parent admission unavailable".into(),
                diagnostic: None,
            })
            .await
        {
            tracing::warn!(child = ?child.identity(), %error, "failed child launch cleanup retained");
        }
    }
    if result.is_err() && original_placement.session != context.placement.session {
        environment
            .runner
            .discard_child_session(original_placement.session);
    } else if result.is_err() {
        if let Err(cleanup) = environment
            .runner
            .retire_fork_scopes(context.clone(), vec![original_placement.lexical_scope])
            .await
        {
            tracing::warn!(%cleanup, "failed launch scope cleanup was retained");
        }
    }
    let (child, allocated_label, admitted_worktree) = match result {
        Ok(started) => started,
        Err(error) if fork_group.is_some() => {
            if let Some(group) = fork_group {
                if let Ok(children) = environment.fork_groups.abort(group, context.actor) {
                    if let Some(invocation) = &invocation_work {
                        invocation.retain_aborted_children(&kernel, &children);
                    }
                    let children = selected_child_retirement_batch(
                        &kernel,
                        children,
                        "fork group admission failed",
                    );
                    for (child, terminal) in children.into_actors() {
                        if let Err(error) = child.shutdown(terminal).await {
                            tracing::warn!(child = ?child.identity(), %error, "fork-group child did not shut down");
                        }
                    }
                }
            }
            return environment
                .runner
                .resume_fork_failure(
                    context.clone(),
                    ForkContinuation::for_reply(parent_hole, fork_reply),
                    error.to_string(),
                )
                .await;
        }
        Err(error) => return Err(error),
    };
    match admitted_worktree {
        Some(worktree) => {
            environment
                .runner
                .resume_fork_starting_parent(
                    context.clone(),
                    parent_hole,
                    child.identity(),
                    allocated_label,
                    worktree,
                )
                .await
        }
        None => {
            environment
                .runner
                .resume_starting_parent(
                    context.clone(),
                    parent_hole,
                    child.identity(),
                    allocated_label,
                )
                .await
        }
    }
}
