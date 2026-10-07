//! Captured child admission, external startup and fenced parent application.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildPlacementPhase {
    Prepared(crate::ActorPlacement),
    ActorOwned(ActorRef),
    Released,
}

/// Exactly one owner may reclaim a launch placement. Startup transfers it
/// before native actor initialization; terminal observation cannot take it back.
#[derive(Clone)]
pub(super) struct ChildPlacementCustody(Arc<Mutex<ChildPlacementPhase>>);

impl ChildPlacementCustody {
    pub(super) fn new(placement: crate::ActorPlacement) -> Self {
        Self(Arc::new(Mutex::new(ChildPlacementPhase::Prepared(
            placement,
        ))))
    }

    fn update(&self, placement: crate::ActorPlacement) {
        let mut phase = self.0.lock();
        assert!(
            matches!(*phase, ChildPlacementPhase::Prepared(_)),
            "placement changes only before actor startup"
        );
        *phase = ChildPlacementPhase::Prepared(placement);
    }

    pub(super) fn transfer_to_actor(
        &self,
        actor: ActorRef,
        placement: crate::ActorPlacement,
    ) -> Result<(), String> {
        let mut phase = self.0.lock();
        match *phase {
            ChildPlacementPhase::Prepared(expected) if expected == placement => {
                *phase = ChildPlacementPhase::ActorOwned(actor);
                Ok(())
            }
            ChildPlacementPhase::ActorOwned(expected) if expected == actor => Ok(()),
            _ => Err("child placement is no longer owned by startup admission".into()),
        }
    }

    fn take_unadmitted(&self) -> Option<crate::ActorPlacement> {
        let mut phase = self.0.lock();
        match *phase {
            ChildPlacementPhase::Prepared(placement) => {
                *phase = ChildPlacementPhase::Released;
                Some(placement)
            }
            ChildPlacementPhase::ActorOwned(_) | ChildPlacementPhase::Released => None,
        }
    }
}

async fn release_failed_launch_placement<H, O>(
    runner: &ResidentActorRunner<H, O>,
    parent_session: tidepool_repr::SessionId,
    custody: &ChildPlacementCustody,
) -> Result<(), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    if let Some(placement) = custody.take_unadmitted() {
        // A dedicated machine belongs to its original startup lease until
        // admission. Its drop removes that machine; shared machines need only
        // this actual lexical scope/realm retired.
        if placement.session == parent_session {
            runner.retire_root_placement(placement).await?;
        }
    }
    Ok(())
}

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
    pub spawn_reply: bool,
    pub spawn_admission: Option<crate::SpawnAdmission>,
    pub placement_custody: ChildPlacementCustody,
}

pub(super) struct ChildLaunchAdmission {
    pub child: crate::start::CapturedChildLaunch,
    pub checkpoint_admission: Option<(crate::CheckpointLease, Option<HostedCheckpointAttachment>)>,
    pub spawn_admission: Option<crate::SpawnAdmission>,
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
    checkpoint_admission: Option<(crate::CheckpointLease, Option<HostedCheckpointAttachment>)>,
    inherited_source: Option<crate::CheckpointSourceLayer>,
    source_layers: Option<crate::ActorSourceLayerResolver>,
    helper_branch: Option<String>,
}

pub(super) struct ChildLaunchResume {
    context: ActorSessionContext,
    parent_hole: ResidentHole,
    spawn_reply: bool,
    spawn_admission: Option<crate::SpawnAdmission>,
    invocation_work: Option<Arc<InvocationWork>>,
    placement_custody: ChildPlacementCustody,
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
            inherited_source,
            retained_checkpoint_scope,
            child_session_startup,
            invocation_work,
        } = admission?;
        let crate::start::CapturedChildLaunch {
            lifetime,
            mut descriptor,
            spawn,
            entry,
            mut launch_worktrees,
            record_workspace,
            seed,
        } = child;
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
        if !launch_worktrees.is_empty() {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "raw workspace identities cannot authorize actor startup".into(),
            ));
        }
        let selection = match &spawn {
            Some(definition) => definition.workspace.clone(),
            None => match record_workspace {
                Some(workspace) => {
                    crate::fork_workspace::SpawnWorkspaceWire::ExistingDirectory(workspace)
                }
                None => crate::fork_workspace::SpawnWorkspaceWire::SameDirectory,
            },
        };
        let prepared_workspace = match &environment.fork_workspaces {
            Some(admission) => {
                let prepared = admission
                    .prepare(context.actor, selection, None)
                    .await
                    .map_err(|error| {
                        ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                    })?;
                launch_worktrees = vec![prepared.handle().handle_receipt.tree_id.raw.clone()];
                if let Some(authority) = &spawn_admission {
                    authority.retain_workspace(prepared.handle().clone());
                }
                Some(prepared)
            }
            None if spawn.is_none() => None,
            None => {
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "workspace admission is unavailable".into(),
                ))
            }
        };
        // The child may carry declarations that import a helper published by
        // its parent. Fix its own snapshot and include roots before a fresh
        // machine bootstraps those declarations.
        let source_layers = environment.source_layers.clone();
        let helper_branch = if let Some(layers) = &source_layers {
            Some(
                layers
                    .retain_helpers(context.actor.into())
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
                    None => layers.layer_include_for(helper_branch.as_deref().unwrap_or_default()),
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
            continuation
                .placement_custody
                .update(descriptor.placement());
        }
        // Transfer the captured closure after releasing the parent checkout.
        // A session carrying RepoEvent stays on its existing machine.
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
            continuation
                .placement_custody
                .update(descriptor.placement());
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
        continuation
            .placement_custody
            .update(descriptor.placement());
        let mut behavior = if let Some(definition) = spawn {
            let mut behavior = ResidentKernelBehavior::with_boot(
                descriptor,
                environment.clone(),
                ResidentBoot::Workbench,
                launch_worktrees,
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
        behavior.child_placement_custody = Some(continuation.placement_custody.clone());
        behavior.admitted_checkpoint = checkpoint_admission.clone();
        behavior.prepared_workspace = prepared_workspace;
        behavior.child_session_startup = child_session_startup;
        let startup_admission = match lifetime {
            crate::WorkerLifetime::InvocationOwned | crate::WorkerLifetime::InScope(_) => {
                Some(invocation_work.clone().ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "invocation-owned worker has no request owner".into(),
                    )
                })?)
            }
            crate::WorkerLifetime::ActorOwned | crate::WorkerLifetime::RunOwned => None,
        };
        let startup_admission: Option<Arc<dyn crate::local_actor::WorkerStartupAdmission>> =
            match &spawn_admission {
                Some(authority) => Some(Arc::new(ChildStartupAdmission {
                    creator: context.actor,
                    authority: authority.clone(),
                    work: startup_admission,
                })),
                None => startup_admission
                    .map(|owner| owner as Arc<dyn crate::local_actor::WorkerStartupAdmission>),
            };
        let child_result = match startup_admission {
            Some(admission) => {
                kernel
                    .spawn_worker_scoped(None, behavior, lifetime, admission)
                    .await
            }
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
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    error.to_string(),
                ));
            }
        };
        if let Some(admission) = &spawn_admission {
            if let Err(detail) = admission.wait_ready().await {
                let cleanup = child
                    .shutdown_with_cleanup(ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: "spawn attachment failed".into(),
                        diagnostic: None,
                    })
                    .await;
                admission.retain_cleanup(match cleanup {
                    Ok(outcome) if outcome.cleanup.is_confirmed() => {
                        crate::lineage::SpawnCleanupOutcome::Confirmed
                    }
                    Ok(outcome) => crate::lineage::SpawnCleanupOutcome::Unconfirmed(format!(
                        "{:?}",
                        outcome.cleanup
                    )),
                    Err(error) => {
                        crate::lineage::SpawnCleanupOutcome::Unconfirmed(error.to_string())
                    }
                });
                return Err(ResidentActorWorkbenchError::ActorProtocol(detail));
            }
        }
        Ok(LaunchedChild {
            actor: child,
            allocated_label,
            admitted_worktree,
            checkpoint_admission,
            inherited_source,
            source_layers,
            helper_branch,
        })
    })
    .await;
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
            checkpoint_admission,
            inherited_source,
            source_layers,
            helper_branch,
        } = started;
        let checkpoint_lease = checkpoint_admission
            .as_ref()
            .map(|(lease, _)| lease.clone());
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
        spawn_reply: continuation.spawn_reply,
        spawn_admission: continuation.spawn_admission,
        invocation_work: continuation.invocation_work,
        placement_custody: continuation.placement_custody,
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
        spawn_reply,
        spawn_admission,
        invocation_work,
        placement_custody,
        failed_child,
        result,
    } = resume;
    if let Some(child) = failed_child {
        if let Some(invocation) = &invocation_work {
            invocation.retain_aborted_children(&kernel, &[child.identity()]);
        }
        let cleanup = child
            .shutdown_with_cleanup(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "child launch parent admission unavailable".into(),
                diagnostic: None,
            })
            .await;
        if let Some(authority) = &spawn_admission {
            authority.retain_cleanup(match &cleanup {
                Ok(outcome) if outcome.cleanup.is_confirmed() => {
                    crate::lineage::SpawnCleanupOutcome::Confirmed
                }
                Ok(outcome) => crate::lineage::SpawnCleanupOutcome::Unconfirmed(format!(
                    "{:?}",
                    outcome.cleanup
                )),
                Err(error) => crate::lineage::SpawnCleanupOutcome::Unconfirmed(error.to_string()),
            });
        }
        if let Err(error) = cleanup {
            tracing::warn!(child = ?child.identity(), %error, "failed child launch cleanup retained");
        }
    }
    if result.is_err() {
        if let Err(cleanup) = release_failed_launch_placement(
            &environment.runner,
            context.placement.session,
            &placement_custody,
        )
        .await
        {
            if let Some(authority) = &spawn_admission {
                authority.retain_cleanup(crate::lineage::SpawnCleanupOutcome::Unconfirmed(
                    cleanup.to_string(),
                ));
            }
            tracing::warn!(%cleanup, "unadmitted launch placement cleanup was retained");
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
    let (child, allocated_label, _) = match result {
        Ok(started) => started,

        Err(error) => return Err(error),
    };
    environment
        .runner
        .resume_starting_parent(context, parent_hole, child.identity(), allocated_label)
        .await
}
