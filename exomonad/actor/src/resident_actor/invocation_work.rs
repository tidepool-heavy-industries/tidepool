//! Invocation membership borrows resources from their original lifecycle owners.

use super::*;
use crate::command_jobs::{CommandControl, CommandJobs};
use tidepool_bridge_effects::{CommandCleanup, CommandError, CommandResult, CommandStatus};

pub(crate) struct InvocationWork {
    owner: ActorRef,
    reservation: RequestReservationOwner,
    state: Mutex<InvocationWorkState>,
    cleanup_lock: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct InvocationWorkState {
    closed: bool,
    commands: Vec<String>,
    detached_commands: std::collections::HashSet<String>,
    workers: Vec<LocalActorRef>,
    groups: Vec<crate::ForkGroupId>,
    watches: Vec<crate::WatchId>,
    cleanup: Option<InvocationCleanup>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct InvocationCleanup {
    commands: Vec<InvocationCommandCleanup>,
    workers: Vec<InvocationWorkerCleanup>,
    failures: Vec<String>,
}

#[derive(Clone, Debug)]
struct InvocationCommandCleanup {
    job: String,
    result: Option<CommandResult>,
    failure: Option<CommandError>,
}

#[derive(Clone, Debug)]
struct InvocationWorkerCleanup {
    actor: ActorRef,
    kernel: Result<crate::ResidentCleanupOutcome, String>,
    host: Result<ResourceRelease, String>,
}

impl InvocationCleanup {
    pub(super) fn uncertainty(&self) -> Option<String> {
        let mut details = self.failures.clone();
        for command in &self.commands {
            if let Some(error) = &command.failure {
                details.push(format!("command {} cancellation: {error:?}", command.job));
            }
            match &command.result {
                Some(result) if result.cleanup == CommandCleanup::CommandClean => {}
                Some(result) => details.push(format!(
                    "command {} cleanup: {:?}",
                    command.job, result.cleanup
                )),
                None => details.push(format!(
                    "command {} has no terminal cleanup result",
                    command.job
                )),
            }
        }
        for worker in &self.workers {
            match &worker.kernel {
                Ok(cleanup) if cleanup.is_confirmed() => {}
                outcome => details.push(format!(
                    "worker {:?} kernel cleanup: {outcome:?}",
                    worker.actor
                )),
            }
            match &worker.host {
                Ok(ResourceRelease::Released) => {}
                outcome => details.push(format!(
                    "worker {:?} host cleanup: {outcome:?}",
                    worker.actor
                )),
            }
        }
        (!details.is_empty()).then(|| details.join("; "))
    }
}

impl InvocationWork {
    pub(super) fn new(owner: ActorRef, reservation: RequestReservationOwner) -> Arc<Self> {
        Arc::new(Self {
            owner,
            reservation,
            state: Mutex::new(Default::default()),
            cleanup_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub(super) fn matches(&self, owner: ActorRef, reservation: &RequestReservationOwner) -> bool {
        self.owner == owner && &self.reservation == reservation
    }

    pub(crate) fn register_command(&self, id: String) -> Result<(), CommandError> {
        let mut state = self.state.lock();
        if state.closed {
            return Err(CommandError::CommandUnavailable(
                "invocation ownership is closed".into(),
            ));
        }
        if !state.commands.contains(&id) {
            state.commands.push(id);
        }
        Ok(())
    }

    pub(super) fn detach_command(
        &self,
        jobs: &CommandJobs,
        caller: ActorRef,
        id: &str,
    ) -> Result<(), CommandError> {
        if caller != self.owner || jobs.owner(id)? != caller {
            return Err(CommandError::CommandUnauthorized);
        }
        let probe = jobs.source_probe(id)?;
        let mut state = self.state.lock();
        if state.detached_commands.contains(id) {
            return Ok(());
        }
        let Some(index) = state.commands.iter().position(|job| job == id) else {
            return Err(CommandError::CommandUnauthorized);
        };
        if state.closed {
            return Err(CommandError::CommandUnavailable(
                "invocation cleanup has begun".into(),
            ));
        }
        state.commands.remove(index);
        state.detached_commands.insert(id.into());
        if let Some(probe) = probe {
            if let Some(index) = state.commands.iter().position(|job| job == &probe) {
                state.commands.remove(index);
                state.detached_commands.insert(probe);
            }
        }
        Ok(())
    }

    pub(super) fn register_transient_watch(
        &self,
        watch: crate::WatchId,
    ) -> Result<(), crate::ReplyError> {
        let mut state = self.state.lock();
        if state.closed {
            return Err(crate::ReplyError::CancellationRequested);
        }
        if !state.watches.contains(&watch) {
            state.watches.push(watch);
        }
        Ok(())
    }

    pub(super) fn register_worker(&self, child: LocalActorRef) -> Result<(), String> {
        let mut state = self.state.lock();
        if state.closed {
            return Err("invocation ownership is closed".into());
        }
        if !state
            .workers
            .iter()
            .any(|worker| worker.identity() == child.identity())
        {
            state.workers.push(child);
        }
        Ok(())
    }

    pub(super) fn owns_worker(&self, actor: ActorRef) -> bool {
        self.state
            .lock()
            .workers
            .iter()
            .any(|worker| worker.identity() == actor)
    }

    pub(super) fn register_group(&self, group: crate::ForkGroupId) -> Result<(), String> {
        let mut state = self.state.lock();
        if state.closed {
            return Err("invocation ownership is closed".into());
        }
        if !state.groups.contains(&group) {
            state.groups.push(group);
        }
        Ok(())
    }

    pub(super) fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    pub(super) fn close(&self) {
        self.state.lock().closed = true;
    }

    pub(super) fn cleanup_observation(&self) -> Option<InvocationCleanup> {
        self.state.lock().cleanup.clone()
    }

    pub(super) async fn cleanup<H, O>(
        &self,
        environment: &ResidentEnvironment<H, O>,
        kernel: &KernelContext,
    ) -> InvocationCleanup
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        let _cleanup = self.cleanup_lock.lock().await;
        let (commands, mut workers, groups, watches) = {
            let mut state = self.state.lock();
            state.closed = true;
            if let Some(cleanup) = &state.cleanup {
                if cleanup.uncertainty().is_none() {
                    return cleanup.clone();
                }
            }
            (
                state.commands.clone(),
                state.workers.clone(),
                state.groups.clone(),
                state.watches.clone(),
            )
        };
        let mut cleanup = InvocationCleanup::default();
        for watch in watches {
            if let Err(error) = environment
                .requests
                .release_transient_watch(self.owner, watch)
            {
                if error != crate::ReplyError::Stale {
                    cleanup
                        .failures
                        .push(format!("transient watch {} release: {error:?}", watch.0));
                }
            }
        }
        publish_request_notifications(&environment.requests, &environment.deployments, Vec::new())
            .await;
        for group in groups {
            match environment.fork_groups.abort(group, self.owner) {
                Ok(children) => {
                    for actor in children {
                        if let Some(child) = kernel.resolve(actor) {
                            if !workers.iter().any(|worker| worker.identity() == actor) {
                                workers.push(child);
                            }
                        }
                    }
                }
                Err(
                    crate::ForkGroupError::Unknown(_) | crate::ForkGroupError::AlreadyCommitted(_),
                ) => {}
                Err(error) => cleanup
                    .failures
                    .push(format!("fork group {} release: {error}", group.0)),
            }
        }
        let command_cleanup =
            futures_util::future::join_all(commands.into_iter().map(|job| async move {
                let fallback_job = job.clone();
                let finish = async move {
                    let initial = environment.commands.status(self.owner, &job).await;
                    if let Ok(CommandStatus::CommandFinished(result)) = &initial {
                        if result.cleanup == CommandCleanup::CommandClean {
                            return InvocationCommandCleanup {
                                job,
                                result: Some(result.clone()),
                                failure: None,
                            };
                        }
                    }
                    let failure = environment
                        .commands
                        .control(self.owner, &job, CommandControl::Cancel)
                        .await
                        .err();
                    let status = environment
                        .commands
                        .wait(self.owner, &job, RELEASE_WAIT.as_millis() as i64)
                        .await;
                    let result = match status {
                        Ok(CommandStatus::CommandFinished(result)) => Some(result),
                        _ => None,
                    };
                    InvocationCommandCleanup {
                        job: job.clone(),
                        result,
                        failure,
                    }
                };
                tokio::time::timeout(RELEASE_WAIT + RELEASE_WAIT, finish)
                    .await
                    .unwrap_or_else(|_| InvocationCommandCleanup {
                        job: fallback_job,
                        result: None,
                        failure: Some(CommandError::CommandUnavailable(
                            "invocation cleanup observation timed out".into(),
                        )),
                    })
            }));
        let worker_cleanup =
            futures_util::future::join_all(workers.into_iter().map(|child| async move {
                let actor = child.identity();
                let terminal = ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "owning tool invocation ended".into(),
                };
                let kernel = match tokio::time::timeout(
                    crate::local_actor::SHUTDOWN_BUDGET,
                    child.shutdown_with_cleanup(terminal),
                )
                .await
                {
                    Ok(Ok(shutdown)) => Ok(shutdown.cleanup),
                    Ok(Err(error)) => Err(error.to_string()),
                    Err(_) => Err("worker retirement remains unconfirmed".into()),
                };
                let host = if !environment
                    .release_tracked
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    Ok(ResourceRelease::Released)
                } else {
                    let (reply, release) = tokio::sync::oneshot::channel();
                    let request = Arc::new(ReleaseAwait {
                        actor,
                        reply: Mutex::new(Some(reply)),
                    });
                    match environment
                        .deployments
                        .try_send(LocalResidentDeployment::ReleaseAwait(request))
                    {
                        Err(error) => Err(format!("host cleanup observation unavailable: {error}")),
                        Ok(()) => match tokio::time::timeout(RELEASE_WAIT, release).await {
                            Ok(Ok(outcome)) => Ok(outcome),
                            Ok(Err(_)) => Err("host cleanup reply was lost".into()),
                            Err(_) => Err("host cleanup remains pending".into()),
                        },
                    }
                };
                InvocationWorkerCleanup {
                    actor,
                    kernel,
                    host,
                }
            }));
        let (commands, workers) = tokio::join!(command_cleanup, worker_cleanup);
        cleanup.commands = commands;
        cleanup.workers = workers;
        self.state.lock().cleanup = Some(cleanup.clone());
        cleanup
    }
}

pub(super) fn ensure_workbench_execution_id(request: WorkbenchRequest) -> WorkbenchRequest {
    let execution = request
        .execution_id()
        .cloned()
        .unwrap_or_else(|| WorkbenchExecutionId::from_digest(*uuid::Uuid::new_v4().as_bytes()));
    request.with_execution_id(execution)
}

pub(super) fn retain_invocation_cleanup_summary(
    mut result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    uncertainty: Option<String>,
) -> Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure> {
    if let Some(detail) = uncertainty {
        if let Ok(
            KernelStep::Continue(response)
            | KernelStep::ContinueLater(response)
            | KernelStep::Stop {
                output: response, ..
            },
        ) = &mut result
        {
            let cleanup = format!("Invocation cleanup remains unconfirmed: {detail}");
            response.summary = Some(match response.summary.take() {
                Some(summary) => format!("{summary}\n{cleanup}"),
                None => cleanup,
            });
        }
    }
    result
}

#[cfg(test)]
#[path = "invocation_work_tests.rs"]
mod tests;
