//! Captured group readiness, publication and original release custody.

use super::*;

mod deferred;
pub(super) use deferred::{
    apply_ready_deferred, apply_scopes, await_ready_deferred, await_scopes, prepare_deferred,
    FinalizedDeferredCommit, PreparedDeferredCommit, PreparedDeferredScopes, ReadyDeferredCommit,
};

struct Continuation {
    context: ActorSessionContext,
    parent_descriptor: ActorDescriptor,
    publication: ForkPublication,
    control: Arc<crate::WorkbenchExecutionControl>,
    continuation: ForkContinuation,
    group: crate::ForkGroupId,
    owns_group: bool,
    committed_authority: Option<Arc<crate::lineage::CommittedForkGroups>>,
}

pub(super) struct PreparedCapturedCommit {
    frame: Continuation,
    readiness: Result<tokio::sync::watch::Receiver<crate::ForkGroupPhase>, String>,
}

enum Readiness {
    Interrupted,
    Rejected(String),
    Ready(Vec<(ActorRef, ActorDescriptor)>),
}

pub(super) struct ReadyCapturedCommit {
    frame: Continuation,
    readiness: Readiness,
}

enum Release {
    Interrupted,
    Rejected(String),
    Published(PendingForkPublication),
}

pub(super) struct CapturedCommitRelease {
    frame: Continuation,
    release: Release,
}

pub(super) struct CompletedCapturedCommit {
    frame: Continuation,
    completed: Completion,
}

enum Completion {
    Interrupted,
    Rejected(String),
    Released,
    Unconfirmed(PendingForkPublication, KernelBehaviorError),
}

pub(super) struct CapturedCommitResume {
    frame: Continuation,
    outcome: Resume,
}
enum Resume {
    Interrupted,
    Rejected(String),
    Released,
    Unconfirmed(KernelBehaviorError),
}

pub(super) fn prepare<H, O>(
    environment: &ResidentEnvironment<H, O>,
    context: ActorSessionContext,
    parent_descriptor: ActorDescriptor,
    publication: ForkPublication,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    continuation: ForkContinuation,
    group: crate::ForkGroupId,
) -> PreparedCapturedCommit
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
        if boundary.as_ref() != publication.boundary() {
            return Err("captured fork group belongs to another execution boundary".into());
        }
        owns_group = true;
        if control
            .as_ref()
            .is_some_and(|control| control.has_context_binding())
        {
            return Err("captured fork commit requires settled context; use unfoldDeferred".into());
        }
        group_descriptors(environment, context.actor, group)?;
        environment
            .fork_groups
            .request_commit(group, context.actor)
            .map_err(|error| error.to_string())
    })();
    let control = control.unwrap_or_else(crate::WorkbenchExecutionControl::untracked);
    if readiness.is_ok() {
        control.arm_sleep();
    }
    PreparedCapturedCommit {
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

pub(super) async fn await_ready<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    prepared: PreparedCapturedCommit,
) -> ReadyCapturedCommit
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let PreparedCapturedCommit { frame, readiness } = prepared;
    let readiness = match readiness {
        Err(detail) => Readiness::Rejected(detail),
        Ok(mut phase) => {
            let waiting = async {
                loop {
                    let current = *phase.borrow();
                    match current {
                        crate::ForkGroupPhase::Ready => {
                            return group_descriptors(
                                &environment,
                                frame.context.actor,
                                frame.group,
                            );
                        }
                        crate::ForkGroupPhase::Committed => {
                            return Err("captured fork group was already published".into());
                        }
                        crate::ForkGroupPhase::Aborted => {
                            return Err(
                                "captured fork group was aborted while awaiting readiness".into()
                            );
                        }
                        crate::ForkGroupPhase::Staging => {}
                    }
                    phase
                        .changed()
                        .await
                        .map_err(|_| "captured fork group readiness channel closed".to_owned())?;
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
    ReadyCapturedCommit { frame, readiness }
}

pub(super) fn apply_ready<H, O>(
    environment: &ResidentEnvironment<H, O>,
    kernel: &KernelContext,
    parent: &ActorDescriptor,
    ready: ReadyCapturedCommit,
) -> CapturedCommitRelease
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let ReadyCapturedCommit {
        mut frame,
        readiness,
    } = ready;
    let release = match readiness {
        Readiness::Interrupted => Release::Interrupted,
        Readiness::Rejected(detail) => Release::Rejected(detail),
        Readiness::Ready(descriptors) => {
            if frame.control.cancellation_requested() {
                Release::Interrupted
            } else if !child_launch::matches_parent(
                kernel,
                parent,
                &frame.parent_descriptor,
                frame.context.actor,
            ) {
                Release::Rejected(
                    "captured fork parent admission changed while awaiting readiness".into(),
                )
            } else {
                match environment.fork_groups.publish_captured_group(
                    frame.group,
                    frame.context.actor,
                    frame.publication.boundary(),
                    &descriptors,
                ) {
                    Err(error) => Release::Rejected(error.to_string()),
                    Ok(authority) => {
                        let authority = Arc::new(authority);
                        frame.committed_authority = Some(Arc::clone(&authority));
                        Release::Published(PendingForkPublication {
                            boundary: frame.publication.boundary().cloned(),
                            phase: PendingForkPublicationPhase::Committed(authority),
                            releases: descriptors
                                .into_iter()
                                .map(|(child, descriptor)| PendingForkChildRelease {
                                    child,
                                    session: descriptor.placement().session,
                                    scope: descriptor.placement().lexical_scope,
                                    lexical: Arc::new(OnceLock::new()),
                                })
                                .collect(),
                            unused_scopes: Vec::new(),
                            inherited: Arc::new(OnceLock::new()),
                        })
                    }
                }
            }
        }
    };
    CapturedCommitRelease { frame, release }
}

/// Retain original committed custody before any external release can be abandoned.
pub(super) fn retain_release(
    pending: &mut Vec<PendingForkPublication>,
    release: &CapturedCommitRelease,
) {
    if let Release::Published(publication) = &release.release {
        pending.push(publication.clone());
    }
}

pub(super) async fn await_release<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    release: CapturedCommitRelease,
) -> CompletedCapturedCommit
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let CapturedCommitRelease { frame, release } = release;
    let completed = match release {
        Release::Interrupted => Completion::Interrupted,
        Release::Rejected(detail) => Completion::Rejected(detail),
        Release::Published(pending) => {
            let (pending, result) =
                release_pending(environment, kernel, frame.context.clone(), pending).await;
            match result {
                Ok(()) => Completion::Released,
                Err(error) => Completion::Unconfirmed(pending, error),
            }
        }
    };
    CompletedCapturedCommit { frame, completed }
}

pub(super) fn retain_completed(
    pending: &mut Vec<PendingForkPublication>,
    completed: CompletedCapturedCommit,
) -> CapturedCommitResume {
    let CompletedCapturedCommit { frame, completed } = completed;
    if let Some(original) = &frame.committed_authority {
        pending.retain(|publication| {
            !matches!(&publication.phase, PendingForkPublicationPhase::Committed(authority) if Arc::ptr_eq(authority, original))
        });
    }
    let outcome = match completed {
        Completion::Interrupted => Resume::Interrupted,
        Completion::Rejected(detail) => Resume::Rejected(detail),
        Completion::Released => Resume::Released,
        Completion::Unconfirmed(publication, error) => {
            pending.push(publication);
            Resume::Unconfirmed(error)
        }
    };
    CapturedCommitResume { frame, outcome }
}

pub(super) async fn resume<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    resume: CapturedCommitResume,
) -> Result<ResidentOutcome, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let CapturedCommitResume { frame, outcome } = resume;
    match outcome {
        Resume::Released => {
            environment
                .runner
                .resume_fork_unit(frame.context, frame.continuation)
                .await
        }
        Resume::Unconfirmed(error) => Err(error.into()),
        outcome => {
            let rejected_children = if frame.owns_group {
                environment
                    .fork_groups
                    .abort_selected_unpublished(frame.context.actor, &[frame.group])
            } else {
                Vec::new()
            };
            let rejected_children = rejected_children
                .into_iter()
                .filter_map(|child| {
                    kernel.resolve(child).map(|child| {
                        (
                            child,
                            ActorTerminal {
                                kind: ActorExitKind::Cancelled,
                                summary: "captured fork group admission rejected".into(),
                                diagnostic: None,
                            },
                        )
                    })
                })
                .collect();
            let rejected_children = crate::kernel::RetirementBatch::issue(rejected_children);
            for (child, terminal) in rejected_children.into_actors() {
                if let Err(error) = child.shutdown(terminal).await {
                    tracing::warn!(child = ?child.identity(), %error, "captured fork child cleanup retained");
                }
            }
            match outcome {
                Resume::Rejected(detail) => {
                    environment
                        .runner
                        .resume_fork_failure(frame.context, frame.continuation, detail)
                        .await
                }
                Resume::Interrupted => {
                    let (outcome, consumed) = environment
                        .runner
                        .abort_live(
                            frame.context,
                            frame.continuation.hole,
                            "captured fork admission interrupted".into(),
                        )
                        .await;
                    if consumed {
                        frame.control.acknowledge_cancellation();
                    }
                    outcome
                }
                _ => unreachable!("published release settled above"),
            }
        }
    }
}

pub(super) fn group_descriptors<H, O>(
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
        let actors = environment.actors.lock();
        children
            .into_iter()
            .map(|child| {
                actors
                    .get(&child)
                    .map(|record| (child, record.descriptor.clone()))
                    .ok_or_else(|| {
                        format!("captured fork child {child:?} has no admitted descriptor")
                    })
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    descriptors
        .into_iter()
        .map(|(child, descriptor)| {
            if descriptor.fork_group() != Some(group) {
                return Err(format!(
                    "captured fork child {child:?} belongs to a different group"
                ));
            }
            if descriptor.checkpoint_token().is_some() {
                let admission = environment
                    .fork_groups
                    .checkpoint_admission(group, owner, child)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| {
                        format!("captured fork child {child:?} has no admitted checkpoint")
                    })?;
                if !admission.matches(&descriptor)
                    || admission.context != crate::HostedCheckpointContext::Captured
                {
                    return Err(format!(
                        "captured fork child {child:?} has no independently usable captured context"
                    ));
                }
            } else if descriptor.context_parent().is_some() {
                return Err(format!(
                    "captured fork child {child:?} requires a checkpoint or selected context"
                ));
            }
            Ok((child, descriptor))
        })
        .collect()
}

pub(super) async fn release_pending<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    mut pending: PendingForkPublication,
) -> (PendingForkPublication, Result<(), KernelBehaviorError>)
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let result = async {
        if let PendingForkPublicationPhase::Prepared(groups) = &pending.phase {
            match environment
                .fork_groups
                .publish_groups(groups, context.actor)
            {
                Ok(authority) => {
                    pending.phase = PendingForkPublicationPhase::Committed(Arc::new(authority))
                }
                Err(error) => {
                    return Err(KernelBehaviorError {
                        detail: error.to_string(),
                        diagnostic: None,
                    });
                }
            }
        }
        let PendingForkPublicationPhase::Committed(authority) = &pending.phase else {
            unreachable!("publication minted committed authority");
        };
        if authority.owner() != context.actor {
            return Err(KernelBehaviorError {
                detail: "fork publication authority belongs to another actor".into(),
                diagnostic: None,
            });
        }
        let valid = {
            let actors = environment.actors.lock();
            pending.releases.iter().all(|release| {
                actors.get(&release.child).is_none_or(|record| {
                    record.descriptor.placement().session == release.session
                        && record
                            .descriptor
                            .fork_group()
                            .is_some_and(|group| authority.groups().contains(&group))
                })
            })
        };
        if !valid {
            return Err(KernelBehaviorError {
                detail: "fork release differs from committed admission".into(),
                diagnostic: None,
            });
        }
        let authority = Arc::clone(authority);
        while let Some(prepared) = pending.releases.front().cloned() {
            let PendingForkChildRelease {
                child,
                session,
                scope,
                lexical: prepared_lexical,
            } = prepared;
            let admitted = environment
                .actors
                .lock()
                .get(&child)
                .filter(|record| record.terminal.is_none())
                .map(|record| record.descriptor.clone());
            if let (Some(admitted), Some(target)) = (admitted, kernel.resolve(child)) {
                if target.terminal().get().is_none() {
                    let inherited = if admitted.checkpoint_token().is_none()
                        && admitted
                            .source_imports()
                            .inherited_scope()
                            .map_err(|error| {
                                ResidentKernelBehavior::<H, O>::failure(error.to_string())
                            })?
                            .is_some()
                    {
                        if session != context.placement.session
                            || admitted.creator() != Some(context.actor)
                            || admitted.fork_boundary() != pending.boundary.as_ref()
                            || pending.boundary.is_none()
                        {
                            return Err(ResidentKernelBehavior::<H, O>::failure(
                                "deferred inherited child differs from its committed parent",
                            ));
                        }
                        if pending.inherited.get().is_none() {
                            return Err(ResidentKernelBehavior::<H, O>::failure(
                                "deferred inherited child has no source capture from its publication",
                            ));
                        }
                        pending.inherited.get().cloned()
                    } else {
                        None
                    };
                    let lexical = match prepared_lexical.get() {
                        Some(lexical) => Arc::clone(lexical),
                        None => match match &inherited {
                            Some(capture) => {
                                environment
                                    .runner
                                    .retain_deferred_child_scope(
                                        session,
                                        scope,
                                        Arc::clone(capture),
                                    )
                                    .await
                            }
                            None => {
                                environment
                                    .runner
                                    .retain_fork_release_scope(session, scope)
                                    .await
                            }
                        } {
                            Ok(lexical) => {
                                prepared_lexical.set(lexical).ok();
                                Arc::clone(
                                    prepared_lexical
                                        .get()
                                        .expect("release retains first lexical grant"),
                                )
                            }
                            Err(error) => {
                                return Err(ResidentKernelBehavior::<H, O>::workbench_failure(error));
                            }
                        },
                    };
                    let release = match ForkChildRelease::issue(
                        child,
                        admitted,
                        pending.boundary.clone(),
                        Arc::clone(&authority),
                        lexical,
                        inherited,
                    ) {
                        Ok(release) => release,
                        Err(error) => {
                            return Err(error);
                        }
                    };
                    target
                        .address()
                        .send_message(crate::KernelMessage::ReleaseFork { release })
                        .ok();
                }
            }
            pending.releases.pop_front();
            // The release owns an independent detached root. The original
            // prepared root is retired by this publication owner in either
            // delivery outcome; an undelivered capsule queues its own root.
            pending.unused_scopes.push((session, scope));
        }
        while let Some((session, _)) = pending.unused_scopes.first().copied() {
            let scopes = pending
                .unused_scopes
                .iter()
                .filter_map(|(owner, scope)| (*owner == session).then_some(*scope))
                .collect();
            if let Err(error) = environment
                .runner
                .retire_checkpoint_scopes(session, scopes)
                .await
            {
                return Err(ResidentKernelBehavior::<H, O>::workbench_failure(error));
            }
            pending.unused_scopes.retain(|(owner, _)| *owner != session);
        }
        Ok(())
    }
    .await;
    (pending, result)
}
