//! Captured child admission, external startup and fenced parent application.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChildPlacementPhase {
    Reserved(crate::ActorPlacement),
    Prepared(crate::ActorPlacement),
    ActorOwned(ActorRef),
    ActorCleanup(crate::ResidentCleanupOutcome),
    Reclaiming(crate::ActorPlacement),
    CleanupRetained(crate::ActorPlacement),
    Released,
}

/// Exactly one owner may reclaim a launch placement. Startup transfers it
/// before native actor initialization; terminal observation cannot take it back.
#[derive(Clone)]
pub(crate) struct ChildPlacementCustody(
    Arc<Mutex<ChildPlacementPhase>>,
    Arc<std::sync::atomic::AtomicBool>,
);

impl ChildPlacementCustody {
    pub(super) fn reserved(placement: crate::ActorPlacement) -> Self {
        Self(
            Arc::new(Mutex::new(ChildPlacementPhase::Reserved(placement))),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
    }

    pub(super) fn new(placement: crate::ActorPlacement) -> Self {
        Self(
            Arc::new(Mutex::new(ChildPlacementPhase::Prepared(placement))),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
    }

    pub(super) fn startup_guard(&self) -> crate::WorkbenchAbandonGuard {
        let active = self.1.clone();
        crate::WorkbenchAbandonGuard::new(move || {
            active.store(false, std::sync::atomic::Ordering::Release);
        })
    }

    pub(super) async fn cleanup<H, O>(
        &self,
        runner: &ResidentActorRunner<H, O>,
        parent_session: tidepool_repr::SessionId,
    ) -> Result<(), ResidentActorWorkbenchError>
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        {
            let phase = self.0.lock();
            match &*phase {
                ChildPlacementPhase::ActorCleanup(cleanup) => {
                    return if cleanup.is_confirmed() {
                        Ok(())
                    } else {
                        Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                            "child startup cleanup remains retained: {cleanup:?}"
                        )))
                    };
                }
                ChildPlacementPhase::ActorOwned(_) | ChildPlacementPhase::Released => {
                    return Ok(());
                }
                _ => {}
            }
        }
        if self.1.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "launch placement remains with startup in flight".into(),
            ));
        }
        release_failed_launch_placement(runner, parent_session, self).await?;
        if matches!(
            *self.0.lock(),
            ChildPlacementPhase::ActorOwned(_) | ChildPlacementPhase::Released
        ) {
            Ok(())
        } else {
            Err(ResidentActorWorkbenchError::ActorProtocol(
                "launch placement reclamation remains in flight".into(),
            ))
        }
    }

    pub(super) fn provisioned(&self, placement: crate::ActorPlacement) {
        let mut phase = self.0.lock();
        assert!(
            matches!(*phase, ChildPlacementPhase::Reserved(_)),
            "only a reserved placement may acquire native startup custody"
        );
        *phase = ChildPlacementPhase::Prepared(placement);
    }

    pub(crate) fn provision_fallback_with(
        &self,
        expected: crate::ActorPlacement,
        session: tidepool_repr::SessionId,
        mint: impl FnOnce() -> tidepool_codegen::scope::ScopeId,
    ) -> Result<crate::ActorPlacement, String> {
        let mut phase = self.0.lock();
        let matches_placeholder = matches!(
            *phase,
            ChildPlacementPhase::Reserved(placement) | ChildPlacementPhase::Prepared(placement)
                if placement == expected
        ) && expected.session != session
            && expected.lexical_scope == tidepool_codegen::scope::ScopeId::ROOT;
        if !matches_placeholder || !self.1.load(std::sync::atomic::Ordering::Acquire) {
            return Err("fallback placement is no longer owned by startup admission".into());
        }
        // Cancellation may close admission while minting. Reclamation takes
        // this same lock and therefore observes the actual scope before it can
        // release the reservation. Dropping the native result cannot lose it.
        let placement = crate::ActorPlacement {
            session,
            lexical_scope: mint(),
            ..expected
        };
        *phase = ChildPlacementPhase::Prepared(placement);
        Ok(placement)
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

    pub(super) fn record_startup_cleanup(&self, cleanup: crate::ResidentCleanupOutcome) {
        let mut phase = self.0.lock();
        if matches!(*phase, ChildPlacementPhase::Prepared(_))
            || matches!(*phase, ChildPlacementPhase::ActorOwned(actor) if actor == cleanup.actor())
        {
            // Admission refusal can run actor cleanup before `start` transfers
            // placement custody. Its exact observation still owns settlement.
            *phase = ChildPlacementPhase::ActorCleanup(cleanup);
        }
    }

    fn take_unadmitted(&self) -> Option<crate::ActorPlacement> {
        let mut phase = self.0.lock();
        match *phase {
            ChildPlacementPhase::Reserved(_) => {
                *phase = ChildPlacementPhase::Released;
                None
            }
            ChildPlacementPhase::Prepared(placement)
            | ChildPlacementPhase::CleanupRetained(placement) => {
                *phase = ChildPlacementPhase::Reclaiming(placement);
                Some(placement)
            }
            ChildPlacementPhase::ActorOwned(_)
            | ChildPlacementPhase::ActorCleanup(_)
            | ChildPlacementPhase::Reclaiming(_)
            | ChildPlacementPhase::Released => None,
        }
    }
    fn finish_reclaim(&self, placement: crate::ActorPlacement, confirmed: bool) {
        let mut phase = self.0.lock();
        if *phase == ChildPlacementPhase::Reclaiming(placement) {
            *phase = if confirmed {
                ChildPlacementPhase::Released
            } else {
                ChildPlacementPhase::CleanupRetained(placement)
            };
        }
    }
}

struct PlacementReclaimAttempt {
    custody: ChildPlacementCustody,
    placement: crate::ActorPlacement,
    confirmed: bool,
}

impl Drop for PlacementReclaimAttempt {
    fn drop(&mut self) {
        self.custody.finish_reclaim(self.placement, self.confirmed);
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
        let mut attempt = PlacementReclaimAttempt {
            custody: custody.clone(),
            placement,
            confirmed: false,
        };
        // A dedicated machine belongs to its original startup lease until
        // admission. Its drop removes that machine; shared machines need only
        // this actual lexical scope/realm retired.
        if placement.session == parent_session {
            runner.retire_root_placement(placement).await?;
        }
        attempt.confirmed = true;
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
    pub placement_startup: crate::WorkbenchAbandonGuard,
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
    placement_startup: crate::WorkbenchAbandonGuard,
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

#[tracing::instrument(
    target = "exomonad_actor::workbench_phase",
    name = "child_launch",
    skip_all,
    fields(parent_actor = %prepared.continuation.context.actor)
)]
pub(super) async fn await_launch<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    prepared: Box<PreparedChildLaunch>,
) -> Box<CompletedChildLaunch>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    #[cfg(test)]
    if std::env::var_os("TIDEPOOL_ASYNC_LAYOUT_DIAGNOSTICS").is_some() {
        eprintln!(
            "actor child launch layout prepared_bytes={} completed_bytes={} behavior_bytes={} error_bytes={} boundary_bytes={} wait_bytes={}",
            std::mem::size_of::<PreparedChildLaunch>(),
            std::mem::size_of::<CompletedChildLaunch>(),
            std::mem::size_of::<ResidentKernelBehavior<H, O>>(),
            std::mem::size_of::<ResidentActorWorkbenchError>(),
            std::mem::size_of::<ResidentActorBoundary>(),
            std::mem::size_of::<OwnedWorkbenchWait>(),
        );
    }
    let PreparedChildLaunch {
        continuation,
        admission,
    } = *prepared;
    let _startup_guard = SpawnStartupGuard(continuation.spawn_admission.clone());
    // A failed spawn can drop its behavior before refusal cleanup finishes.
    // Keep the original machine preparation lease outside that await.
    let _session_startup_custody = admission
        .as_ref()
        .ok()
        .and_then(|admission| admission.child_session_startup.clone());
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
            exit_destination,
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
                ));
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
        // Resolve placement before transferring the source-paired entry.
        // A session carrying RepoEvent stays on its existing machine.
        if descriptor.placement().session != context.placement.session
            && !environment.runner.supports_child_sessions()
        {
            let placement = environment
                .runner
                .provision_fallback_scope(
                    descriptor.placement(),
                    context.placement.session,
                    continuation.placement_custody.clone(),
                )
                .await?;
            descriptor = descriptor
                .with_session(placement.session)
                .with_lexical_scope(placement.lexical_scope);
        } else if descriptor.placement().session != context.placement.session {
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
            continuation
                .placement_custody
                .update(descriptor.placement());
        }
        let entry = environment
            .runner
            .transfer_mailbox_value(
                descriptor.session_context(context.actor),
                entry,
                descriptor.placement().session,
                descriptor.placement().resource_scope,
            )
            .await?
            .into_custody();
        // The checkout-derived include roots were fixed before any fresh
        // child machine bootstrapped inherited declarations.
        let allocated_label = descriptor.display_label().to_string();
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
        behavior.exit_destination = exit_destination;
        behavior.child_placement_custody = Some(continuation.placement_custody.clone());
        behavior.admitted_checkpoint = checkpoint_admission.clone();
        behavior.prepared_workspace = prepared_workspace;
        behavior.child_session_startup = child_session_startup.clone();
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
                let cleanup = settle_startup_refusal(
                    &environment.runner,
                    &kernel,
                    &continuation.placement_custody,
                    &error,
                )
                .await;
                if let Some(admission) = &spawn_admission {
                    admission.fail(error.to_string());
                    admission.retain_cleanup(match cleanup {
                        crate::CleanupComponentOutcome::Confirmed => {
                            crate::lineage::SpawnCleanupOutcome::Confirmed
                        }
                        crate::CleanupComponentOutcome::Unconfirmed(detail) => {
                            crate::lineage::SpawnCleanupOutcome::Unconfirmed(detail)
                        }
                        crate::CleanupComponentOutcome::Unsupported => {
                            crate::lineage::SpawnCleanupOutcome::Unconfirmed(
                                "startup owner does not support complete cleanup".into(),
                            )
                        }
                    });
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
    Box::new(CompletedChildLaunch {
        continuation,
        result,
    })
}

/// Await refusal cleanup while the original preparation lease still retains
/// the native machine. An admitted actor's typed cleanup owns its placement.
async fn settle_startup_refusal<H, O>(
    runner: &ResidentActorRunner<H, O>,
    kernel: &KernelContext,
    custody: &ChildPlacementCustody,
    error: &ractor::SpawnErr,
) -> crate::CleanupComponentOutcome
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    use crate::CleanupComponentOutcome::{Confirmed, Unconfirmed};
    if let Some(cleanup) = crate::local_actor::startup_cleanup(error) {
        custody.record_startup_cleanup(cleanup.clone());
        return crate::local_actor::combine_cleanup(
            crate::local_actor::combine_cleanup(cleanup.hook().clone(), cleanup.realm().clone()),
            cleanup.children().clone(),
        );
    }
    let cleanup = if let Some(placement) = custody.take_unadmitted() {
        let mut attempt = PlacementReclaimAttempt {
            custody: custody.clone(),
            placement,
            confirmed: false,
        };
        match runner
            .retire_root_placement_wait(placement, crate::local_actor::SHUTDOWN_BUDGET)
            .await
        {
            Ok(()) => {
                attempt.confirmed = true;
                Confirmed
            }
            Err(cleanup) => Unconfirmed(format!(
                "child startup failed: {error}; placement cleanup: {cleanup}"
            )),
        }
    } else {
        Unconfirmed(format!(
            "child startup failed without available preparation custody: {error}"
        ))
    };
    kernel.retain_child_startup_cleanup(cleanup.clone());
    cleanup
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

pub(super) fn apply_launch(
    kernel: &KernelContext,
    descriptor: &ActorDescriptor,
    completed: Box<CompletedChildLaunch>,
) -> ChildLaunchResume {
    let CompletedChildLaunch {
        continuation,
        result,
    } = *completed;
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
        placement_startup: continuation.placement_startup,
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
        placement_startup: _placement_startup,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_codegen::scope::ScopeId;
    use tidepool_repr::{DataConTable, SessionId};
    use tidepool_runtime::session::{ModuleEnv, RuntimeLexicalScopeLease, SessionLib, SlotKind};

    type Machines = ActorMachineRegistry<frunk::HNil, tidepool_mcp::CapturedOutput>;
    type Runner = ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>;

    #[tokio::test]
    async fn scheduler_refusal_waits_for_original_placement_cleanup() {
        let mut fixture = super::super::invocation_work::tests::Fixture::start().await;
        let (runner, machines, placement, captured, _root) = shared_fixture();
        fixture.environment.runner = runner;
        let custody = ChildPlacementCustody::new(placement);
        let error = ractor::SpawnErr::StartupFailed(
            std::io::Error::other("scheduler refused prepared child").into(),
        );
        let checkout = machines.checkout_run(placement.session).unwrap();
        let mut settling = Box::pin(settle_startup_refusal(
            &fixture.environment.runner,
            &fixture.kernel,
            &custody,
            &error,
        ));
        assert!(matches!(
            futures_util::poll!(&mut settling),
            std::task::Poll::Pending
        ));
        assert_eq!(
            *custody.0.lock(),
            ChildPlacementPhase::Reclaiming(placement)
        );
        drop(checkout);
        assert_eq!(settling.await, crate::CleanupComponentOutcome::Confirmed);
        assert_eq!(*custody.0.lock(), ChildPlacementPhase::Released);
        assert!(!capture_is_live(&machines, placement, &captured));
        fixture.finish().await;
    }

    fn session(
        id: SessionId,
        root: &std::path::Path,
    ) -> ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput> {
        let lib = SessionLib::open(id, root, ModuleEnv::standalone_default()).unwrap();
        ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        )
    }

    fn captured_placement(
        machines: &Machines,
        id: SessionId,
    ) -> (crate::ActorPlacement, Arc<RuntimeLexicalScopeLease>) {
        let mut checkout = machines.checkout_run(id).unwrap();
        let scope = checkout.machine().mint_isolated_scope();
        let captured = checkout.machine().retain_lexical_scope(scope).unwrap();
        checkout.machine().retire_scope(scope);
        (
            crate::ActorPlacement {
                session: id,
                resource_scope: RealmId::fresh(),
                lexical_scope: captured.scope(),
            },
            captured,
        )
    }

    fn capture_is_live(
        machines: &Machines,
        placement: crate::ActorPlacement,
        captured: &RuntimeLexicalScopeLease,
    ) -> bool {
        machines
            .checkout_run(placement.session)
            .unwrap()
            .machine()
            .validate_lexical_scope_lease(placement.lexical_scope, captured)
            .is_ok()
    }

    fn shared_fixture() -> (
        Runner,
        Arc<Machines>,
        crate::ActorPlacement,
        Arc<RuntimeLexicalScopeLease>,
        tempfile::TempDir,
    ) {
        let id = SessionId(970);
        let root = tempfile::tempdir().unwrap();
        let machines = Arc::new(Machines::new());
        machines.insert_idle(id, Box::new(session(id, root.path())));
        let runner = Runner::new(machines.clone(), ActorWorkbenchSource::new("", Vec::new()));
        let (placement, captured) = captured_placement(&machines, id);
        (runner, machines, placement, captured, root)
    }

    #[tokio::test]
    async fn admitted_machine_and_capture_survive_unconfirmed_launch_cleanup_until_actor_retirement(
    ) {
        crate::resident_workbench::CompilerCloseOwner::ActorLifecycle(
            crate::RetainedActorExit::new(),
        )
        .scope(async {
            let parent = SessionId(971);
            let id = SessionId(972);
            let root = tempfile::tempdir().unwrap();
            let root_path = root.path().to_path_buf();
            let machines = Arc::new(Machines::new());
            let runner = Runner::new(machines.clone(), ActorWorkbenchSource::new("", Vec::new()))
                .with_child_session_factory(Arc::new(move |id, _, _settlement| {
                    Ok(Box::new(session(id, &root_path)))
                }))
                .with_child_bootstrap_program(bootstrap_program());
            let startup = runner.child_session_startup_lease(id);
            runner
                .provision_child_session(id, RealmId::fresh(), None, &[])
                .await
                .unwrap();
            let (placement, captured) = captured_placement(&machines, id);
            let custody = ChildPlacementCustody::new(placement);
            let actor = ActorRef::first(crate::ActorId(973));
            custody.transfer_to_actor(actor, placement).unwrap();
            startup.admitted();
            let cleanup = crate::lineage::SpawnCleanupOutcome::Unconfirmed(
                "child cleanup still retained".into(),
            );
            assert!(matches!(
                cleanup,
                crate::lineage::SpawnCleanupOutcome::Unconfirmed(_)
            ));
            release_failed_launch_placement(&runner, parent, &custody)
                .await
                .unwrap();
            release_failed_launch_placement(&runner, parent, &custody)
                .await
                .unwrap();
            assert_eq!(machines.kind(id), Some(SlotKind::Idle));
            assert!(capture_is_live(&machines, placement, &captured));
            assert_eq!(*custody.0.lock(), ChildPlacementPhase::ActorOwned(actor));
            // The retained child actor, rather than parent launch failure, owns retirement.
            runner.retire_root_placement(placement).await.unwrap();
            assert!(!capture_is_live(&machines, placement, &captured));
            runner.retire_child_session(id, false).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while machines.kind(id).is_some() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        })
        .await;
    }

    #[tokio::test]
    async fn dropped_owned_launch_completion_is_reclaimed_by_retained_journal() {
        let mut fixture = super::super::invocation_work::tests::Fixture::start().await;
        let (runner, machines, placement, captured, _root) = shared_fixture();
        fixture.environment.runner = runner;
        let actor = fixture.actor.identity();
        let descriptor = ActorDescriptor::new("abandoned launch", placement);
        let context = descriptor.session_context(actor);
        let work = InvocationWork::new(actor, RequestReservationOwner::Scope(0));
        let custody = ChildPlacementCustody::new(placement);
        let startup = custody.startup_guard();
        work.retain_launch_placement(&context, custody.clone())
            .unwrap();
        let completed = await_launch(
            fixture.environment.clone(),
            fixture.kernel.clone(),
            Box::new(PreparedChildLaunch {
                continuation: ChildLaunchContinuation {
                    context,
                    parent_descriptor: descriptor,
                    control: None,
                    invocation_work: None,
                    parent_hole: ResidentHole::plain("abandoned-launch-parent"),
                    spawn_reply: false,
                    spawn_admission: None,
                    placement_custody: custody,
                    placement_startup: startup,
                },
                admission: Err(ResidentActorWorkbenchError::ActorProtocol(
                    "startup refused before actor admission".into(),
                )),
            }),
        )
        .await;
        let completion = crate::OwnedWorkbenchCompletion::<
            ResidentKernelBehavior<frunk::HNil, tidepool_mcp::CapturedOutput>,
        >::advance(move |behavior, kernel| {
            let _resume = behavior.apply_child_launch(kernel, completed);
            unreachable!("forced-stop completion must never be applied")
        });
        // Closing while a completed launch still awaits mailbox delivery
        // cannot reclaim custody that its continuation might still transfer.
        let first = work.cleanup(&fixture.environment, &fixture.kernel).await;
        assert!(first.uncertainty().is_some());
        assert!(capture_is_live(&machines, placement, &captured));
        drop(completion);
        let settled = work.cleanup(&fixture.environment, &fixture.kernel).await;
        assert_eq!(settled.uncertainty(), None);
        assert!(!capture_is_live(&machines, placement, &captured));
        assert_eq!(machines.kind(placement.session), Some(SlotKind::Idle));
        fixture.finish().await;
    }

    #[tokio::test]
    async fn construction_journal_leaves_transferred_actor_placement_live() {
        let mut fixture = super::super::invocation_work::tests::Fixture::start().await;
        let (runner, machines, placement, captured, _root) = shared_fixture();
        fixture.environment.runner = runner;
        let actor = fixture.actor.identity();
        let context = ActorDescriptor::new("transferred launch", placement).session_context(actor);
        let work = InvocationWork::new(actor, RequestReservationOwner::Scope(0));
        let custody = ChildPlacementCustody::new(placement);
        let startup = custody.startup_guard();
        work.retain_launch_placement(&context, custody.clone())
            .unwrap();
        custody
            .transfer_to_actor(ActorRef::first(crate::ActorId(978)), placement)
            .unwrap();
        drop(startup);
        let settled = work.cleanup(&fixture.environment, &fixture.kernel).await;
        assert_eq!(settled.uncertainty(), None);
        assert!(capture_is_live(&machines, placement, &captured));
        fixture
            .environment
            .runner
            .retire_root_placement(placement)
            .await
            .unwrap();
        fixture.finish().await;
    }

    #[tokio::test]
    async fn abandoned_launch_journal_retains_failed_cleanup_until_machine_returns() {
        let mut fixture = super::super::invocation_work::tests::Fixture::start().await;
        let actor = fixture.actor.identity();
        let id = SessionId(979);
        let root = tempfile::tempdir().unwrap();
        let mut machine = session(id, root.path());
        let scope = machine.mint_isolated_scope();
        let captured = machine.retain_lexical_scope(scope).unwrap();
        machine.retire_scope(scope);
        let placement = crate::ActorPlacement {
            session: id,
            resource_scope: RealmId::fresh(),
            lexical_scope: captured.scope(),
        };
        let machines = Arc::new(Machines::new());
        fixture.environment.runner =
            Runner::new(machines.clone(), ActorWorkbenchSource::new("", Vec::new()));
        let context = ActorDescriptor::new("retained launch", placement).session_context(actor);
        let work = InvocationWork::new(actor, RequestReservationOwner::Scope(0));
        let scope_owner = work.new_scope().unwrap();
        // A parent that already confirmed cleanup must revisit late custody
        // retained by its closed nested scope.
        assert_eq!(
            work.cleanup(&fixture.environment, &fixture.kernel)
                .await
                .uncertainty(),
            None
        );
        let custody = ChildPlacementCustody::new(placement);
        let startup = custody.startup_guard();
        assert!(scope_owner
            .retain_launch_placement(&context, custody)
            .is_err());
        drop(startup);
        let first = work.cleanup(&fixture.environment, &fixture.kernel).await;
        assert!(first.uncertainty().is_some());
        machines.insert_idle(id, Box::new(machine));
        assert!(capture_is_live(&machines, placement, &captured));
        let settled = work.cleanup(&fixture.environment, &fixture.kernel).await;
        assert_eq!(settled.uncertainty(), None);
        assert!(!capture_is_live(&machines, placement, &captured));
        fixture.finish().await;
    }

    #[tokio::test]
    async fn late_launch_retention_fences_concurrent_root_and_nested_cleanup_proofs() {
        for nested in [false, true] {
            let mut fixture = super::super::invocation_work::tests::Fixture::start().await;
            let (runner, machines, placement, captured, _root) = shared_fixture();
            fixture.environment.runner = runner;
            let actor = fixture.actor.identity();
            let context =
                ActorDescriptor::new("concurrent launch", placement).session_context(actor);
            let work = InvocationWork::new(actor, RequestReservationOwner::Scope(0));
            let owner = if nested {
                work.new_scope().unwrap()
            } else {
                work.clone()
            };
            let custody = ChildPlacementCustody::new(placement);
            let startup = custody.startup_guard();
            owner
                .retain_launch_placement(&context, custody.clone())
                .unwrap();
            drop(startup);

            let mut checkout = machines.checkout_run(placement.session).unwrap();
            let late_scope = checkout.machine().mint_isolated_scope();
            let late_capture = checkout.machine().retain_lexical_scope(late_scope).unwrap();
            checkout.machine().retire_scope(late_scope);
            checkout
                .machine()
                .validate_lexical_scope_lease(late_capture.scope(), &late_capture)
                .unwrap();
            let late_placement = crate::ActorPlacement {
                resource_scope: RealmId::fresh(),
                lexical_scope: late_capture.scope(),
                ..placement
            };
            let late_custody = ChildPlacementCustody::new(late_placement);
            let late_startup = late_custody.startup_guard();
            let mut cleanup = Box::pin(work.cleanup(&fixture.environment, &fixture.kernel));
            assert!(matches!(
                futures_util::poll!(&mut cleanup),
                std::task::Poll::Pending
            ));
            assert_eq!(
                *custody.0.lock(),
                ChildPlacementPhase::Reclaiming(placement)
            );
            assert!(owner
                .retain_launch_placement(&context, late_custody)
                .is_err());
            drop(late_startup);
            drop(checkout);
            assert!(cleanup.await.uncertainty().is_some());
            assert!(!capture_is_live(&machines, placement, &captured));
            assert!(capture_is_live(&machines, late_placement, &late_capture));
            assert_eq!(
                work.cleanup(&fixture.environment, &fixture.kernel)
                    .await
                    .uncertainty(),
                None
            );
            assert!(!capture_is_live(&machines, late_placement, &late_capture));
            fixture.finish().await;
        }
    }

    #[tokio::test]
    async fn failed_preactor_cleanup_keeps_exact_placement_for_confirmed_retry() {
        let id = SessionId(976);
        let root = tempfile::tempdir().unwrap();
        let mut machine = session(id, root.path());
        let scope = machine.mint_isolated_scope();
        let captured = machine.retain_lexical_scope(scope).unwrap();
        machine.retire_scope(scope);
        let placement = crate::ActorPlacement {
            session: id,
            resource_scope: RealmId::fresh(),
            lexical_scope: captured.scope(),
        };
        let machines = Arc::new(Machines::new());
        let runner = Runner::new(machines.clone(), ActorWorkbenchSource::new("", Vec::new()));
        let custody = ChildPlacementCustody::new(placement);
        assert!(release_failed_launch_placement(&runner, id, &custody)
            .await
            .is_err());
        assert_eq!(
            *custody.0.lock(),
            ChildPlacementPhase::CleanupRetained(placement)
        );
        assert!(custody
            .transfer_to_actor(ActorRef::first(crate::ActorId(977)), placement)
            .is_err());
        machines.insert_idle(id, Box::new(machine));
        assert!(capture_is_live(&machines, placement, &captured));
        release_failed_launch_placement(&runner, id, &custody)
            .await
            .unwrap();
        assert_eq!(*custody.0.lock(), ChildPlacementPhase::Released);
        assert!(!capture_is_live(&machines, placement, &captured));
        assert_eq!(machines.kind(id), Some(SlotKind::Idle));
    }

    #[tokio::test]
    async fn preactor_refusal_retires_shared_capture_and_refuses_late_transfer() {
        let (runner, machines, placement, captured, _root) = shared_fixture();
        let custody = ChildPlacementCustody::new(placement);
        assert!(capture_is_live(&machines, placement, &captured));
        release_failed_launch_placement(&runner, placement.session, &custody)
            .await
            .unwrap();
        assert!(!capture_is_live(&machines, placement, &captured));
        assert_eq!(machines.kind(placement.session), Some(SlotKind::Idle));
        assert_eq!(*custody.0.lock(), ChildPlacementPhase::Released);
        assert!(custody
            .transfer_to_actor(ActorRef::first(crate::ActorId(974)), placement)
            .is_err());
        release_failed_launch_placement(&runner, placement.session, &custody)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn fallback_refusal_retires_actual_shared_scope_instead_of_fresh_placeholder() {
        let (runner, machines, placement, captured, _root) = shared_fixture();
        let placeholder = crate::ActorPlacement {
            session: SessionId(975),
            lexical_scope: ScopeId::ROOT,
            ..placement
        };
        let custody = ChildPlacementCustody::new(placeholder);
        custody.update(placement);
        release_failed_launch_placement(&runner, placement.session, &custody)
            .await
            .unwrap();
        assert!(!capture_is_live(&machines, placement, &captured));
        assert_eq!(machines.kind(placement.session), Some(SlotKind::Idle));
        assert!(machines.kind(placeholder.session).is_none());
        assert!(custody
            .transfer_to_actor(ActorRef::first(crate::ActorId(976)), placement)
            .is_err());
    }
    fn bootstrap_program() -> Arc<tidepool_runtime::session::CompiledTurn> {
        use tidepool_repr::execution_schema::{
            testing, Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame, FieldLayout,
            Group, HeapBinding, HeapRhs, ResultContract, RuntimeRep, ValueId, ValueRef,
        };
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        for (index, (name, fields)) in [("Done", 1), ("Suspended", 2), ("Unit", 0)]
            .into_iter()
            .enumerate()
        {
            let module = if index < 2 {
                "Tidepool.Internal.Resume"
            } else {
                "Fixture"
            };
            let mut identity = testing::identity(module, name);
            identity.namespace = "constructor".into();
            let mut family = testing::identity(module, if index < 2 { "Settled" } else { "Unit" });
            family.namespace = "type".into();
            wire.constructors.push(ConstructorDecl {
                identity,
                family,
                host_id: tidepool_repr::DataConId(900 + index as u64),
                result_rep: RuntimeRep::LiftedRef,
                tag: if index == 1 { 2 } else { 1 },
                family_size: if index < 2 { 2 } else { 1 },
                field_reps: vec![RuntimeRep::LiftedRef; fields],
                strict_fields: vec![false; fields],
                layout: CheckedLayout {
                    fields: (0..fields)
                        .map(|field| FieldLayout {
                            rep: RuntimeRep::LiftedRef,
                            offset: field as u32 * 8,
                        })
                        .collect(),
                    alignment: if fields == 0 { 1 } else { 8 },
                    payload_size: fields as u32 * 8,
                    root_mask: vec![true; fields],
                },
            });
        }
        wire.expressions.nodes = vec![
            ExprFrame::Construct {
                constructor: ConstructorId(0),
                fields: vec![Atom::Ref(ValueRef::Local(ValueId(1)))],
            },
            ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(1),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(2),
                        fields: vec![],
                    },
                }),
                body: 0,
            },
        ];
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Function { body, .. } = &mut top.binding.rhs else {
            unreachable!()
        };
        *body = 1;
        let mut table = DataConTable::new();
        for constructor in &wire.constructors {
            table
                .insert_checked(tidepool_repr::DataCon {
                    identity: constructor.identity.clone(),
                    id: constructor.host_id,
                    name: constructor.identity.occurrence.clone(),
                    tag: constructor.tag,
                    rep_arity: constructor.field_reps.len() as u32,
                    field_bangs: vec![],
                    qualified_name: Some(format!(
                        "{}.{}",
                        constructor.identity.module, constructor.identity.occurrence
                    )),
                    type_name: constructor.family.occurrence.clone(),
                })
                .expect("valid fixture metadata");
        }
        Arc::new(
            tidepool_runtime::session::CompiledTurn::from_prepared(
                Arc::new(testing::prepare(wire).unwrap()),
                table,
                Default::default(),
                Vec::new(),
            )
            .unwrap(),
        )
    }
}
