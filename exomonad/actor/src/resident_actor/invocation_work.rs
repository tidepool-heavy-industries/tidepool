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

/// User work stops before publication; finalizer compiler work has the same
/// cleanup owner until closing fences every admission.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum InvocationWorkPhase {
    #[default]
    Active,
    Publishing,
    Closing,
}

#[derive(Default)]
struct InvocationWorkState {
    phase: InvocationWorkPhase,
    compilers: Vec<crate::termination::CompilerWorkReceipt>,
    commands: Vec<String>,
    detached_commands: std::collections::HashSet<String>,
    workers: Vec<LocalActorRef>,
    unresolved_workers: Vec<ActorRef>,
    pending_workers: Vec<ActorRef>,
    pending_cancellations: Vec<crate::RequestCancellationNotification>,
    pending_watch_notifications: Vec<crate::request::WatchNotification>,
    groups: Vec<crate::ForkGroupId>,
    watches: Vec<crate::WatchId>,
    cleanup: Option<InvocationCleanup>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct InvocationCleanup {
    compilers: Vec<crate::termination::CompilerWorkReceipt>,
    commands: Vec<InvocationCommandCleanup>,
    workers: Vec<InvocationWorkerCleanup>,
    requests: Vec<InvocationRequestCleanup>,
    failures: Vec<String>,
    settlement_notifications_pending: bool,
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

#[derive(Clone, Debug)]
struct InvocationRequestCleanup {
    request: crate::RequestId,
    cancellation: Result<crate::CancelRequestOutcome, crate::ReplyError>,
    target: Result<crate::request::RequestCleanupState, crate::ReplyError>,
}

impl InvocationCleanup {
    pub(super) fn uncertainty(&self) -> Option<String> {
        let mut details = self.failures.clone();
        for receipt in &self.compilers {
            let close = receipt.observation();
            if !close.is_confirmed() {
                details.push(format!("compiler close: {close:?}"));
            }
        }
        if self.settlement_notifications_pending {
            details.push("settlement notices remain queued for publication".into());
        }
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
        for request in &self.requests {
            if let Err(error) = &request.cancellation {
                details.push(format!(
                    "request {} cancellation: {error:?}",
                    request.request.0
                ));
            }
            if !matches!(
                request.target,
                Ok(crate::request::RequestCleanupState::TargetClosed)
            ) {
                details.push(format!(
                    "request {} target cleanup: {:?}",
                    request.request.0, request.target
                ));
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

    pub(crate) fn register_compiler_work(
        &self,
        receipt: crate::termination::CompilerWorkReceipt,
    ) -> bool {
        let mut state = self.state.lock();
        if state.phase != InvocationWorkPhase::Active {
            return false;
        }
        state.compilers.push(receipt);
        true
    }

    pub(super) fn begin_publication(&self) -> bool {
        let mut state = self.state.lock();
        if state.phase != InvocationWorkPhase::Active {
            return false;
        }
        state.phase = InvocationWorkPhase::Publishing;
        true
    }

    pub(crate) fn register_publication_compiler_work(
        &self,
        receipt: crate::termination::CompilerWorkReceipt,
    ) -> bool {
        let mut state = self.state.lock();
        if state.phase != InvocationWorkPhase::Publishing {
            return false;
        }
        state.compilers.push(receipt);
        true
    }

    pub(crate) fn register_command(&self, id: String) -> Result<(), CommandError> {
        let mut state = self.state.lock();
        if state.phase != InvocationWorkPhase::Active {
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
        if state.phase != InvocationWorkPhase::Active {
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

    pub(super) fn detach_request(
        &self,
        requests: &RequestRegistry,
        caller: ActorRef,
        request: crate::RequestId,
    ) -> Result<(), crate::ReplyError> {
        if caller != self.owner {
            return Err(crate::ReplyError::Unauthorized);
        }
        let state = self.state.lock();
        if state.phase != InvocationWorkPhase::Active {
            return Err(crate::ReplyError::CancellationRequested);
        }
        requests.detach_invocation_request(caller, request, Some(&self.reservation))
    }

    pub(super) fn register_transient_watch(
        &self,
        watch: crate::WatchId,
    ) -> Result<(), crate::ReplyError> {
        let mut state = self.state.lock();
        if state.phase != InvocationWorkPhase::Active {
            return Err(crate::ReplyError::CancellationRequested);
        }
        if !state.watches.contains(&watch) {
            state.watches.push(watch);
        }
        Ok(())
    }

    #[cfg(test)]
    fn register_worker(&self, child: LocalActorRef) -> Result<(), String> {
        let mut state = self.state.lock();
        if state.phase != InvocationWorkPhase::Active {
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

    pub(super) fn retain_aborted_children(&self, kernel: &KernelContext, children: &[ActorRef]) {
        let mut state = self.state.lock();
        for &actor in children {
            if let Some(child) = kernel.resolve(actor) {
                if !state
                    .workers
                    .iter()
                    .any(|worker| worker.identity() == actor)
                {
                    state.workers.push(child);
                }
            } else if !state.unresolved_workers.contains(&actor) {
                state.unresolved_workers.push(actor);
            }
        }
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
        if state.phase != InvocationWorkPhase::Active {
            return Err("invocation ownership is closed".into());
        }
        if !state.groups.contains(&group) {
            state.groups.push(group);
        }
        Ok(())
    }

    pub(super) fn is_closed(&self) -> bool {
        self.state.lock().phase == InvocationWorkPhase::Closing
    }

    pub(super) fn close(&self) {
        self.state.lock().phase = InvocationWorkPhase::Closing;
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
            state.phase = InvocationWorkPhase::Closing;
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
        let mut cleanup = InvocationCleanup {
            compilers: self.state.lock().compilers.clone(),
            ..InvocationCleanup::default()
        };
        {
            let mut state = self.state.lock();
            let pending = std::mem::take(&mut state.pending_workers);
            for actor in pending {
                if let Some(child) = kernel.resolve(actor) {
                    if !state
                        .workers
                        .iter()
                        .any(|worker| worker.identity() == actor)
                    {
                        state.workers.push(child);
                    }
                } else {
                    state.pending_workers.push(actor);
                    cleanup.failures.push(format!(
                        "worker {actor:?} admission cleanup remains unconfirmed"
                    ));
                }
            }
        }

        for group in groups {
            match environment.fork_groups.abort(group, self.owner) {
                Ok(children) => self.retain_aborted_children(kernel, &children),
                Err(
                    crate::ForkGroupError::Unknown(_) | crate::ForkGroupError::AlreadyCommitted(_),
                ) => {}
                Err(error) => cleanup
                    .failures
                    .push(format!("fork group {} release: {error}", group.0)),
            }
        }
        {
            let mut state = self.state.lock();
            for child in &state.workers {
                if !workers
                    .iter()
                    .any(|worker| worker.identity() == child.identity())
                {
                    workers.push(child.clone());
                }
            }
            let unresolved = std::mem::take(&mut state.unresolved_workers);
            for actor in unresolved {
                if let Some(child) = kernel.resolve(actor) {
                    if !state
                        .workers
                        .iter()
                        .any(|worker| worker.identity() == actor)
                    {
                        state.workers.push(child.clone());
                    }
                    if !workers.iter().any(|worker| worker.identity() == actor) {
                        workers.push(child);
                    }
                } else {
                    state.unresolved_workers.push(actor);
                    cleanup.failures.push(format!("worker {actor:?} has no retained lifecycle owner; cleanup remains unconfirmed"));
                }
            }
        }
        let workers = crate::kernel::RetirementBatch::issue(
            workers
                .into_iter()
                .map(|child| {
                    (
                        child,
                        ActorTerminal {
                            kind: ActorExitKind::Cancelled,
                            summary: "owning tool invocation ended".into(),
                            diagnostic: None,
                        },
                    )
                })
                .collect(),
        );
        // Roll back unsubmitted reservations, including detached branches,
        // before dispatching request cancellation notifications. Worker intent
        // is already fenced; it does not settle these request reservations.
        let (_, notifications) = environment
            .requests
            .abort_unsubmitted(self.owner, &self.reservation);
        {
            let mut state = self.state.lock();
            for notification in notifications {
                if !state.pending_watch_notifications.contains(&notification) {
                    state.pending_watch_notifications.push(notification);
                }
            }
        }
        let pending_watch_notifications = self.state.lock().pending_watch_notifications.clone();
        for notification in pending_watch_notifications {
            let owner = notification.owner;
            let watch = notification.watch;
            let delivered = if !environment.requests.retains_watch(owner, watch) {
                true
            } else {
                match tokio::time::timeout(
                    RELEASE_WAIT,
                    environment
                        .deployments
                        .send(LocalResidentDeployment::WatchChanged {
                            notification: notification.clone(),
                        }),
                )
                .await
                {
                    Ok(Ok(())) => true,
                    outcome => {
                        cleanup.failures.push(format!(
                            "watch {} rollback notice delivery remains unconfirmed: {outcome:?}",
                            watch.0
                        ));
                        false
                    }
                }
            };
            if delivered {
                self.state
                    .lock()
                    .pending_watch_notifications
                    .retain(|pending| pending != &notification);
            }
        }
        let mut request_cancellations = Vec::new();
        for request in environment
            .requests
            .invocation_requests(self.owner, &self.reservation)
        {
            let cancellation = environment
                .requests
                .cancel_request(
                    self.owner,
                    request,
                    crate::CancellationReason::RequesterCancelled,
                )
                .map(|(outcome, notification)| {
                    if let Some(notification) = notification {
                        let mut state = self.state.lock();
                        if !state
                            .pending_cancellations
                            .iter()
                            .any(|pending| pending.request == notification.request)
                        {
                            state.pending_cancellations.push(notification);
                        }
                    }
                    outcome
                });
            request_cancellations.push((request, cancellation));
        }

        let pending_cancellations = self.state.lock().pending_cancellations.clone();
        for notification in pending_cancellations {
            let request = notification.request;
            match tokio::time::timeout(
                RELEASE_WAIT,
                environment
                    .deployments
                    .send(LocalResidentDeployment::RequestCancellation { notification }),
            )
            .await
            {
                Ok(Ok(())) => self
                    .state
                    .lock()
                    .pending_cancellations
                    .retain(|pending| pending.request != request),
                outcome => cleanup.failures.push(format!(
                    "request {} cancellation delivery remains unconfirmed: {outcome:?}",
                    request.0
                )),
            }
        }
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
        if tokio::time::timeout(
            RELEASE_WAIT,
            publish_request_notifications(
                &environment.requests,
                &environment.deployments,
                Vec::new(),
            ),
        )
        .await
        .is_err()
        {
            cleanup.settlement_notifications_pending =
                environment.requests.has_settlement_notifications();
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
        let worker_cleanup = futures_util::future::join_all(workers.into_actors().into_iter().map(
            |(child, terminal)| async move {
                let actor = child.identity();
                let (kernel, retained_terminal) = match tokio::time::timeout(
                    crate::local_actor::SHUTDOWN_BUDGET,
                    child.shutdown_with_cleanup(terminal),
                )
                .await
                {
                    Ok(Ok(shutdown)) => (Ok(shutdown.cleanup), Some(shutdown.terminal)),
                    Ok(Err(error)) => (Err(error.to_string()), child.terminal().get()),
                    Err(_) => (
                        Err("worker retirement remains unconfirmed".into()),
                        child.terminal().get(),
                    ),
                };
                let host = if !environment
                    .release_tracked
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    if let Some(terminal) = retained_terminal {
                        publish_retired(environment, actor, terminal);
                    }
                    Ok(ResourceRelease::Released)
                } else if let Some(terminal) = retained_terminal {
                    match publish_retired_confirmed(environment, actor, terminal).await {
                        Err(error) => Err(error),
                        Ok(()) => {
                            let (request, release) = ReleaseAwait::channel(actor);
                            match environment
                                .deployments
                                .try_send(LocalResidentDeployment::ReleaseAwait(request))
                            {
                                Err(error) => {
                                    Err(format!("host cleanup observation unavailable: {error}"))
                                }
                                Ok(()) => match tokio::time::timeout(RELEASE_WAIT, release).await {
                                    Ok(Ok(outcome)) => Ok(outcome),
                                    Ok(Err(_)) => Err("host cleanup reply was lost".into()),
                                    Err(_) => Err("host cleanup remains pending".into()),
                                },
                            }
                        }
                    }
                } else {
                    Err("worker has no published terminal; host cleanup remains unconfirmed".into())
                };
                InvocationWorkerCleanup {
                    actor,
                    kernel,
                    host,
                }
            },
        ));
        let (commands, workers) = tokio::join!(command_cleanup, worker_cleanup);
        cleanup.commands = commands;
        cleanup.workers = workers;
        // Delivery and owned-worker retirement may close request targets after
        // cancellation admission. Retain their state at the cleanup boundary.
        cleanup.requests = request_cancellations
            .into_iter()
            .map(|(request, cancellation)| InvocationRequestCleanup {
                request,
                cancellation,
                target: environment
                    .requests
                    .request_cleanup_state(self.owner, request),
            })
            .collect();
        self.state.lock().cleanup = Some(cleanup.clone());
        cleanup
    }
}

impl crate::local_actor::WorkerStartupAdmission for InvocationWork {
    fn reserve(&self, actor: ActorRef) -> Result<(), String> {
        let mut state = self.state.lock();
        if state.phase != InvocationWorkPhase::Active {
            return Err("invocation closed before worker admission".into());
        }
        state.pending_workers.push(actor);
        Ok(())
    }

    fn admit(&self, actor: LocalActorRef) -> Result<(), String> {
        let mut state = self.state.lock();
        state
            .pending_workers
            .retain(|pending| *pending != actor.identity());
        if !state
            .workers
            .iter()
            .any(|worker| worker.identity() == actor.identity())
        {
            state.workers.push(actor);
        }
        if state.phase != InvocationWorkPhase::Active {
            return Err("invocation closed before worker initialization".into());
        }
        Ok(())
    }
}

pub(super) fn ensure_workbench_execution_id(
    request: WorkbenchRequest,
) -> (WorkbenchRequest, WorkbenchExecutionId) {
    let execution = request
        .execution_id()
        .cloned()
        .unwrap_or_else(|| WorkbenchExecutionId::from_digest(*uuid::Uuid::new_v4().as_bytes()));
    (request.with_execution_id(execution.clone()), execution)
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
pub(in crate::resident_actor) mod tests;
