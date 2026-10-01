//! Captured child admission, external startup and fenced parent application.

use super::*;

pub(super) struct PreparedChildLaunch {
    pub continuation: ChildLaunchContinuation,
    pub admission: Result<ChildLaunchAdmission, ResidentActorWorkbenchError>,
}

pub(super) struct ChildLaunchContinuation {
    pub context: ActorSessionContext,
    pub parent_descriptor: ActorDescriptor,
    pub parent_hole: ResidentHole,
    pub fork_group: Option<crate::ForkGroupId>,
    pub original_placement: crate::ActorPlacement,
}

pub(super) struct ChildLaunchAdmission {
    pub child: crate::start::CapturedChildLaunch,
    pub checkpoint_admission: Option<(crate::CheckpointLease, Option<HostedCheckpointAttachment>)>,
    pub retained_checkpoint_scope: Option<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
    pub lifetime: crate::WorkerLifetime,
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
    source_layers: Option<crate::ActorSourceLayerResolver>,
    helper_branch: Option<String>,
    bound_worktrees: Vec<String>,
}

pub(super) struct ChildLaunchResume {
    context: ActorSessionContext,
    parent_hole: ResidentHole,
    fork_group: Option<crate::ForkGroupId>,
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
    let result = async {
        let ChildLaunchAdmission { child, checkpoint_admission, retained_checkpoint_scope, lifetime } = admission?;
        let crate::start::CapturedChildLaunch { mut descriptor, entry, mut launch_worktrees, fork_workspace, seed } = child;
        let fork_group = descriptor.fork_group();
        let checkpoint_lease = checkpoint_admission.as_ref().map(|(lease, _)| lease.clone());
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
                    None => layers
                        .layer_include_for(
                            helper_branch.as_deref().unwrap_or_default(),
                            &launch_worktrees,
                        )
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
            let descriptor_scope = descriptor.placement().lexical_scope;
            let checkpoint_descriptor = fork_group.map(|_| descriptor.clone());
            let mut behavior = ResidentKernelBehavior::child(
                descriptor,
                environment.clone(),
                entry,
                launch_worktrees,
            );
            behavior.admitted_checkpoint = checkpoint_admission.clone();
            behavior.prepared_workspace = prepared_workspace;
            let child = match kernel.spawn_worker(None, behavior, lifetime).await {
                Ok(child) => child,
                Err(error) => {
                    if checkpoint_lease.is_some() {
                        if let Err(cleanup) = environment
                            .runner
                            .retire_checkpoint_scopes(
                                context.placement.session,
                                vec![descriptor_scope],
                            )
                            .await
                        {
                            tracing::warn!(%cleanup, "failed checkpoint child scope cleanup was retained");
                        }
                    }
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        error.to_string(),
                    ));
                }
            };
            Ok(LaunchedChild {
                actor: child, allocated_label, admitted_worktree, checkpoint_descriptor,
                checkpoint_admission, source_layers, helper_branch, bound_worktrees,
            })
    }.await;
    CompletedChildLaunch {
        continuation,
        result,
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
        if !matches_parent(kernel, descriptor, original, context.actor) {
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
        fork_group: continuation.fork_group,
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
        fork_group,
        original_placement,
        failed_child,
        result,
    } = resume;
    if let Some(child) = failed_child {
        if let Err(error) = child
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "child launch parent admission unavailable".into(),
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
                    for child in children {
                        if let Some(child) = kernel.resolve(child) {
                            // A child already gone from a failed fork-group admission is
                            // the common case here; log anything else so an actor that
                            // refused shutdown does not silently linger.
                            if let Err(error) = child
                                .shutdown(ActorTerminal {
                                    kind: ActorExitKind::Cancelled,
                                    summary: "fork group admission failed".into(),
                                })
                                .await
                            {
                                tracing::warn!(child = ?child.identity(), %error, "fork-group child did not shut down");
                            }
                        }
                    }
                }
            }
            return environment
                .runner
                .resume_fork_failure(context.clone(), parent_hole, error.to_string())
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
