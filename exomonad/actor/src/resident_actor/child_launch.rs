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
    pub spawn_reply: bool,
    pub spawn_admission: Option<crate::SpawnAdmission>,
    pub fork_group: Option<crate::ForkGroupId>,
    pub original_placement: crate::ActorPlacement,
}

pub(super) struct ChildLaunchAdmission {
    pub child: crate::start::CapturedChildLaunch,
    pub checkpoint_admission: Option<(crate::CheckpointLease, Option<HostedCheckpointAttachment>)>,
    pub spawn_admission: Option<crate::SpawnAdmission>,
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
    spawn_reply: bool,
    spawn_admission: Option<crate::SpawnAdmission>,
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

/// An interrupted startup wait cannot leave an unbound capacity reservation.
/// Once readiness wins, the retained terminal decision makes this drop inert.
struct SpawnStartupGuard(Option<crate::SpawnAdmission>);

impl Drop for SpawnStartupGuard {
    fn drop(&mut self) {
        if let Some(authority) = &self.0 {
            authority.fail("spawn startup was interrupted before readiness".into());
        }
    }
}

struct ChildStartupAdmission {
    creator: ActorRef,
    authority: crate::SpawnAdmission,
    work: Option<Arc<InvocationWork>>,
}

impl crate::local_actor::WorkerStartupAdmission for ChildStartupAdmission {
    fn reserve(&self, child: ActorRef) -> Result<(), String> {
        if let Some(work) = &self.work {
            crate::local_actor::WorkerStartupAdmission::reserve(work.as_ref(), child)?;
        }
        self.authority.reserve_child(self.creator, child)
    }

    fn admit(&self, child: LocalActorRef) -> Result<(), String> {
        if let Some(work) = &self.work {
            crate::local_actor::WorkerStartupAdmission::admit(work.as_ref(), child.clone())?;
        }
        self.authority.bind(self.creator, child.identity())
    }
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
    let _startup_guard = SpawnStartupGuard(continuation.spawn_admission.clone());
    let context = &continuation.context;
    let result = Box::pin(async {
        let ChildLaunchAdmission {
            child,
            checkpoint_admission,
            spawn_admission,
            inherited_host_attachment,
            inherited_source,
            retained_checkpoint_scope,
            child_session_startup,
            invocation_work,
        } = admission?;
        let crate::start::CapturedChildLaunch { lifetime, mut descriptor, spawn, entry, mut launch_worktrees, fork_workspace, seed } = child;
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
            let prepared_workspace = if let Some(definition) = &spawn {
                let admission = environment.fork_workspaces.as_ref().ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol("workspace admission is unavailable".into())
                })?;
                let prepared = admission.prepare(context.actor, definition.workspace.clone(), None)
                    .await.map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
                launch_worktrees = vec![prepared.handle().handle_receipt.tree_id.raw.clone()];
                if let Some(authority) = &spawn_admission { authority.retain_workspace(prepared.handle().clone()); }
                Some(prepared)
            } else if let Some(seed) = fork_workspace {
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
                            native_tools: descriptor.capabilities().native_tools(),
                            workspace: descriptor.capabilities().workspace(),
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
                    layers.retain_helpers(context.actor.into())
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
                        )
                        .map_err(ResidentActorWorkbenchError::ActorProtocol)?,
                    None => match &inherited_source {
                        Some(source) => layers.admit_retained_layer(source),
                        None => layers.layer_include_for(
                            helper_branch.as_deref().unwrap_or_default(),
                        ),
                    }.map_err(ResidentActorWorkbenchError::ActorProtocol)?,
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
            let mut behavior = if let Some(definition) = spawn {
                let mut behavior = ResidentKernelBehavior::with_boot(
                    descriptor, environment.clone(), ResidentBoot::Workbench, launch_worktrees,
                );
                behavior.explicit_installer = Some(Arc::new(entry));
                behavior.spawn_admission = spawn_admission.clone();
                behavior.spawn_source = inherited_source.clone();
                behavior.spawn_helper_branch = helper_branch.clone();
                behavior.fresh_context_seed = match definition.context {
                    crate::start::SpawnContextWire::FreshSpawn(prompt) => Some(prompt),
                    crate::start::SpawnContextWire::CapturedSpawn(_) => None,
                };
                behavior
            } else {
                ResidentKernelBehavior::child(descriptor, environment.clone(), entry, launch_worktrees)
            };
            behavior.admitted_checkpoint = checkpoint_admission.clone();
            behavior.inherited_host_attachment = inherited_host_attachment;
            behavior.prepared_workspace = prepared_workspace;
            behavior.child_session_startup = child_session_startup;
            let startup_admission = match lifetime {
                crate::WorkerLifetime::InvocationOwned | crate::WorkerLifetime::InScope(_) => Some(
                    invocation_work.clone().ok_or_else(|| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            "invocation-owned worker has no request owner".into(),
                        )
                    })?,
                ),
                crate::WorkerLifetime::ActorOwned | crate::WorkerLifetime::RunOwned => None,
            };
            let startup_admission: Option<Arc<dyn crate::local_actor::WorkerStartupAdmission>> =
                match &spawn_admission {
                    Some(authority) => Some(Arc::new(ChildStartupAdmission {
                        creator: context.actor, authority: authority.clone(), work: startup_admission,
                    })),
                    None => startup_admission.map(|owner| owner as Arc<dyn crate::local_actor::WorkerStartupAdmission>),
                };
            let child_result = match startup_admission {
                Some(admission) => kernel.spawn_worker_scoped(None, behavior, lifetime, admission).await,
                None => kernel.spawn_worker(None, behavior, lifetime).await,
            };
            let child = match child_result {
                Ok(child) => child,
                Err(error) => {
                    if let Some(admission) = &spawn_admission {
                        admission.fail(error.to_string());
                        admission.retain_cleanup(crate::lineage::SpawnCleanupOutcome::Unconfirmed(
                            "startup cleanup remains with the kernel owner".into(),
                        ));
                    }
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
            if let Some(admission) = &spawn_admission {
                if let Err(detail) = admission.wait_ready().await {
                    let cleanup = child.shutdown(ActorTerminal {
                        kind: ActorExitKind::Cancelled, summary: "spawn attachment failed".into(), diagnostic: None,
                    }).await;
                    admission.retain_cleanup(match cleanup {
                        Ok(_) => crate::lineage::SpawnCleanupOutcome::Confirmed,
                        Err(error) => crate::lineage::SpawnCleanupOutcome::Unconfirmed(error.to_string()),
                    });
                    return Err(ResidentActorWorkbenchError::ActorProtocol(detail));
                }
            }
            Ok(LaunchedChild {
                actor: child, allocated_label, admitted_worktree, checkpoint_descriptor,
                checkpoint_admission, inherited_source, source_layers, helper_branch, bound_worktrees,
            })
    }).await;
    if let (Err(error), Some(authority)) = (&result, &continuation.spawn_admission) {
        authority.fail(error.to_string());
    }
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
        || current.capabilities() != original.capabilities()
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
                );
            }
        }
        Ok((child, allocated_label, admitted_worktree))
    });
    ChildLaunchResume {
        context: continuation.context,
        parent_hole: continuation.parent_hole,
        fork_reply: continuation.fork_reply,
        spawn_reply: continuation.spawn_reply,
        spawn_admission: continuation.spawn_admission,
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
        spawn_reply,
        spawn_admission,
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
        let cleanup = child
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "child launch parent admission unavailable".into(),
                diagnostic: None,
            })
            .await;
        if let Some(authority) = &spawn_admission {
            authority.retain_cleanup(match &cleanup {
                Ok(_) => crate::lineage::SpawnCleanupOutcome::Confirmed,
                Err(error) => crate::lineage::SpawnCleanupOutcome::Unconfirmed(error.to_string()),
            });
        }
        if let Err(error) = cleanup {
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
    if spawn_reply {
        let result = result
            .map(|(child, _, workspace)| {
                (
                    child.identity().id.0 as i64,
                    child.identity().incarnation.0 as i64,
                    workspace,
                )
            })
            .map_err(|error| match &spawn_admission {
                Some(admission) => admission.error(error.to_string()),
                None => crate::start::SpawnError::SpawnRefused(error.to_string()),
            });
        return environment
            .runner
            .resume_spawn_parent(context, parent_hole, result)
            .await;
    }
    let (child, allocated_label, admitted_worktree) = match result {
        Ok(started) => started,
        Err(error) if fork_group.is_some() => {
            if let Some(group) = fork_group {
                if let Ok(children) = environment.fork_groups.abort(group, context.actor) {
                    if let Some(invocation) = &invocation_work {
                        invocation.retain_aborted_children(&kernel, &children);
                    }
                    for child in children {
                        if let Some(child) = kernel.resolve(child) {
                            // A child already gone from a failed fork-group admission is
                            // the common case here; log anything else so an actor that
                            // refused shutdown does not silently linger.
                            if let Err(error) = child
                                .shutdown(ActorTerminal {
                                    kind: ActorExitKind::Cancelled,
                                    summary: "fork group admission failed".into(),
                                    diagnostic: None,
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
