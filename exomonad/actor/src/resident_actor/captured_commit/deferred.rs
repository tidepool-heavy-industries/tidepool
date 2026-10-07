//! Direct execution publication keeps checkpoint scope custody off the actor turn.

use super::*;

pub(in crate::resident_actor) struct PreparedDeferredCommit {
    frame: Continuation,
    readiness: Result<tokio::sync::watch::Receiver<crate::ForkGroupPhase>, String>,
}

pub(in crate::resident_actor) struct ReadyDeferredCommit {
    frame: Continuation,
    readiness: Readiness,
}

pub(in crate::resident_actor) struct PreparedDeferredScopes {
    frame: Continuation,
    readiness: Readiness,
}

pub(in crate::resident_actor) struct FinalizedDeferredCommit {
    frame: Continuation,
    readiness: Readiness,
    releases: Vec<PendingForkChildRelease>,
    inherited: Option<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
}

pub(in crate::resident_actor) fn prepare_deferred<H, O>(
    environment: &ResidentEnvironment<H, O>,
    context: ActorSessionContext,
    parent_descriptor: ActorDescriptor,
    publication: ForkPublication,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    continuation: ForkContinuation,
    group: crate::ForkGroupId,
) -> PreparedDeferredCommit
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let mut owns_group = false;
    let readiness = (|| {
        let boundary = environment
            .fork_groups
            .completion_boundary(group, context.actor)
            .map_err(|error| error.to_string())?;
        if boundary.as_ref() != publication.boundary()
            || !matches!(
                publication.boundary(),
                Some(tidepool_runtime::session::WorkbenchForkBoundary::Execution { .. })
            )
        {
            return Err("direct fork group belongs to another execution boundary".into());
        }
        owns_group = true;
        descriptors(environment, context.actor, group)?;
        environment
            .fork_groups
            .request_commit(group, context.actor)
            .map_err(|error| error.to_string())
    })();
    let control = control.unwrap_or_else(crate::WorkbenchExecutionControl::untracked);
    if readiness.is_ok() {
        control.arm_sleep();
    }
    PreparedDeferredCommit {
        frame: Continuation {
            context,
            parent_descriptor,
            publication,
            control,
            continuation,
            group,
            owns_group,
            committed_authority: None,
        },
        readiness,
    }
}

pub(in crate::resident_actor) async fn await_ready_deferred<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    prepared: PreparedDeferredCommit,
) -> ReadyDeferredCommit
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let PreparedDeferredCommit { frame, readiness } = prepared;
    let readiness = match readiness {
        Err(detail) => Readiness::Rejected(detail),
        Ok(mut phase) => {
            let waiting = async {
                loop {
                    match *phase.borrow() {
                        crate::ForkGroupPhase::Ready => {
                            return descriptors(&environment, frame.context.actor, frame.group);
                        }
                        crate::ForkGroupPhase::Committed => {
                            return Err("direct fork group was already published".into());
                        }
                        crate::ForkGroupPhase::Aborted => {
                            return Err(
                                "direct fork group was aborted while awaiting readiness".into()
                            );
                        }
                        crate::ForkGroupPhase::Staging => {}
                    }
                    phase
                        .changed()
                        .await
                        .map_err(|_| "direct fork group readiness channel closed".to_owned())?;
                }
            };
            tokio::pin!(waiting);
            tokio::select! {
                ready = &mut waiting => {
                    if frame.control.claim_expiry() {
                        frame.control.finish_sleep();
                        match ready { Ok(descriptors) => Readiness::Ready(descriptors), Err(detail) => Readiness::Rejected(detail) }
                    } else { Readiness::Interrupted }
                },
                () = frame.control.wait_for_cancellation() => Readiness::Interrupted,
                _ = kernel.wait_requested_shutdown() => { frame.control.request_cancellation(); Readiness::Interrupted },
            }
        }
    };
    ReadyDeferredCommit { frame, readiness }
}

pub(in crate::resident_actor) fn apply_ready_deferred<H, O>(
    environment: &ResidentEnvironment<H, O>,
    kernel: &KernelContext,
    parent: &ActorDescriptor,
    ready: ReadyDeferredCommit,
) -> PreparedDeferredScopes
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let ReadyDeferredCommit { frame, readiness } = ready;
    let readiness = fence(environment, kernel, parent, &frame, readiness);
    PreparedDeferredScopes { frame, readiness }
}

pub(in crate::resident_actor) async fn await_scopes<H, O>(
    environment: ResidentEnvironment<H, O>,
    prepared: PreparedDeferredScopes,
) -> FinalizedDeferredCommit
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let PreparedDeferredScopes { frame, readiness } = prepared;
    let mut releases = Vec::new();
    let mut inherited = None;
    let readiness = match readiness {
        Readiness::Ready(descriptors) => {
            let retained = async {
                for (child, descriptor) in &descriptors {
                    // Plain direct children already ran their original boot. A
                    // checkpoint child waits at its gate and keeps the issuer's
                    // exact scope; it must never be reminted from the launcher.
                    if descriptor.fork_boundary().is_none() {
                        continue;
                    }
                    if descriptor.checkpoint_token().is_none()
                        && descriptor
                            .source_imports()
                            .inherited_scope()
                            .map_err(|error| error.to_string())?
                            .is_some()
                    {
                        if inherited.is_none() {
                            // Direct Commit resumes this execution only after release;
                            // retain its exact admitted context before publishing the group.
                            inherited = Some(
                                environment
                                    .runner
                                    .retain_fork_release_scope(
                                        frame.context.placement.session,
                                        frame.context.placement.lexical_scope,
                                    )
                                    .await
                                    .map_err(|error| error.to_string())?,
                            );
                        }
                        releases.push(PendingForkChildRelease {
                            child: *child,
                            session: descriptor.placement().session,
                            scope: descriptor.placement().lexical_scope,
                            lexical: Arc::new(OnceLock::new()),
                        });
                        continue;
                    }
                    if frame.control.cancellation_requested() {
                        return Ok(false);
                    }
                    let placement = descriptor.placement();
                    let lexical = environment
                        .runner
                        .retain_fork_release_scope(placement.session, placement.lexical_scope)
                        .await
                        .map_err(|error| error.to_string())?;
                    releases.push(PendingForkChildRelease {
                        child: *child,
                        session: placement.session,
                        scope: placement.lexical_scope,
                        lexical: Arc::new(OnceLock::from(lexical)),
                    });
                }
                Ok::<_, String>(true)
            }
            .await;
            match retained {
                Ok(true) => Readiness::Ready(descriptors),
                Ok(false) => Readiness::Interrupted,
                Err(detail) => Readiness::Rejected(detail),
            }
        }
        readiness => readiness,
    };
    // Every prepared detached scope remains owned by the runtime lease even
    // if this external completion is discarded by the original task fence.
    FinalizedDeferredCommit {
        frame,
        readiness,
        releases,
        inherited,
    }
}

pub(in crate::resident_actor) fn apply_scopes<H, O>(
    environment: &ResidentEnvironment<H, O>,
    kernel: &KernelContext,
    parent: &ActorDescriptor,
    finalized: FinalizedDeferredCommit,
) -> CapturedCommitRelease
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let FinalizedDeferredCommit {
        mut frame,
        readiness,
        releases,
        inherited,
    } = finalized;
    let release = match fence(environment, kernel, parent, &frame, readiness) {
        Readiness::Interrupted => Release::Interrupted,
        Readiness::Rejected(detail) => Release::Rejected(detail),
        Readiness::Ready(_) => match environment
            .fork_groups
            .publish_groups(&[frame.group], frame.context.actor)
        {
            Err(error) => Release::Rejected(error.to_string()),
            Ok(authority) => {
                let authority = Arc::new(authority);
                frame.committed_authority = Some(Arc::clone(&authority));
                Release::Published(PendingForkPublication {
                    boundary: frame.publication.boundary().cloned(),
                    phase: PendingForkPublicationPhase::Committed(authority),
                    releases: releases.into(),
                    unused_scopes: Vec::new(),
                    inherited: Arc::new(inherited.map(OnceLock::from).unwrap_or_default()),
                })
            }
        },
    };
    CapturedCommitRelease { frame, release }
}

fn descriptors<H, O>(
    environment: &ResidentEnvironment<H, O>,
    owner: ActorRef,
    group: crate::ForkGroupId,
) -> Result<Vec<(ActorRef, ActorDescriptor)>, String>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let children = environment
        .fork_groups
        .children_for_owner(group, owner)
        .map_err(|error| error.to_string())?;
    let descriptors = {
        let records = environment.actors.lock();
        children
            .into_iter()
            .map(|child| {
                let record = records
                    .get(&child)
                    .filter(|record| {
                        record.terminal.is_none() || record.descriptor.fork_boundary().is_none()
                    })
                    .ok_or_else(|| {
                        format!("direct deferred fork child {child:?} is unavailable")
                    })?;
                Ok((child, record.descriptor.clone()))
            })
            .collect::<Result<Vec<_>, String>>()?
    };
    for (child, descriptor) in &descriptors {
        if descriptor.fork_group() != Some(group) || descriptor.creator() != Some(owner) {
            return Err(format!(
                "direct fork child {child:?} differs from original group admission"
            ));
        }
        if descriptor.checkpoint_token().is_some() {
            let admission = environment
                .fork_groups
                .checkpoint_admission(group, owner, *child)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    format!("direct fork child {child:?} has no retained checkpoint admission")
                })?;
            if !admission.matches(descriptor) {
                return Err(format!(
                    "direct fork child {child:?} changed checkpoint admission"
                ));
            }
        }
    }
    Ok(descriptors)
}

fn fence<H, O>(
    environment: &ResidentEnvironment<H, O>,
    kernel: &KernelContext,
    parent: &ActorDescriptor,
    frame: &Continuation,
    readiness: Readiness,
) -> Readiness
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let Readiness::Ready(original) = readiness else {
        return readiness;
    };
    if frame.control.cancellation_requested() {
        return Readiness::Interrupted;
    }
    if !child_launch::matches_parent(
        kernel,
        parent,
        &frame.parent_descriptor,
        frame.context.actor,
    ) {
        return Readiness::Rejected(
            "direct fork parent admission changed while awaiting readiness".into(),
        );
    }
    let current = match descriptors(environment, frame.context.actor, frame.group) {
        Ok(current) => current,
        Err(detail) => return Readiness::Rejected(detail),
    };
    if current.len() != original.len()
        || current
            .iter()
            .zip(&original)
            .any(|((actor, descriptor), (previous_actor, previous))| {
                actor != previous_actor
                    || descriptor.placement() != previous.placement()
                    || descriptor.actor_path() != previous.actor_path()
                    || descriptor.creator() != previous.creator()
                    || descriptor.supervisor_parent() != previous.supervisor_parent()
                    || descriptor.profile() != previous.profile()
                    || descriptor.capabilities() != previous.capabilities()
                    || descriptor.context_parent() != previous.context_parent()
                    || descriptor.fork_boundary() != previous.fork_boundary()
                    || descriptor.checkpoint_token() != previous.checkpoint_token()
                    || descriptor.persistence_policy() != previous.persistence_policy()
                    || descriptor.source_layer() != previous.source_layer()
            })
    {
        return Readiness::Rejected("direct fork child changed during scope preparation".into());
    }
    Readiness::Ready(original)
}
