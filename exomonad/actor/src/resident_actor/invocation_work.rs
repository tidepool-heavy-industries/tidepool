//! Resource cleanup membership borrows resources from their lifecycle owners.

use super::*;
use crate::command_jobs::{CommandControl, CommandJobs};
use crate::request::ResourceCleanupOwner;
use std::sync::Weak;
use tidepool_bridge_effects::{CommandCleanup, CommandError, CommandResult, CommandStatus};

pub(crate) struct InvocationWork {
    owner: ActorRef,
    reservation: RequestReservationOwner,
    cleanup_owner: ResourceCleanupOwner,
    scope_id: Option<i64>,
    parent: Weak<InvocationWork>,
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
    scopes: Vec<Arc<InvocationWork>>,
    compilers: Vec<crate::termination::CompilerWorkReceipt>,
    commands: Vec<String>,
    detached_commands: std::collections::HashSet<String>,
    workers: Vec<LocalActorRef>,
    detached_workers: std::collections::HashSet<ActorRef>,
    realms: Vec<(ActorSessionContext, tidepool_codegen::suspension::RealmId)>,
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
    scopes: Vec<(i64, InvocationCleanup)>,
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
        for (scope, cleanup) in &self.scopes {
            if let Some(detail) = cleanup.uncertainty() {
                details.push(format!("scope {scope}: {detail}"));
            }
        }
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
            cleanup_owner: ResourceCleanupOwner::Invocation(reservation.clone()),
            reservation,
            scope_id: None,
            parent: Weak::new(),
            state: Mutex::new(Default::default()),
            cleanup_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub(super) fn new_scope(self: &Arc<Self>) -> Result<Arc<Self>, String> {
        static NEXT_SCOPE: OnceLock<std::sync::atomic::AtomicI64> = OnceLock::new();
        let issuer = NEXT_SCOPE.get_or_init(|| {
            let mut seed = [0; 8];
            seed.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..8]);
            std::sync::atomic::AtomicI64::new((i64::from_le_bytes(seed) & (i64::MAX >> 1)) + 1)
        });
        self.with_admission_state(|state| {
            let token = issuer
                .fetch_update(
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                    |next| next.checked_add(1),
                )
                .map_err(|_| "resource scope identity exhausted".to_string())?;
            let scope = Arc::new(Self {
                owner: self.owner,
                reservation: RequestReservationOwner::Scope(token),
                cleanup_owner: ResourceCleanupOwner::Scope(token),
                scope_id: Some(token),
                parent: Arc::downgrade(self),
                state: Mutex::new(Default::default()),
                cleanup_lock: tokio::sync::Mutex::new(()),
            });
            state.scopes.push(scope.clone());
            Ok(scope)
        })?
    }

    pub(super) fn is_owned_by(&self, actor: ActorRef) -> bool {
        self.owner == actor
    }

    pub(super) fn scope_token(&self) -> Option<i64> {
        self.scope_id
    }

    pub(super) fn reservation_owner(&self) -> RequestReservationOwner {
        self.reservation.clone()
    }

    pub(super) fn resource_cleanup_owner(&self) -> ResourceCleanupOwner {
        self.cleanup_owner.clone()
    }

    pub(super) fn scopes(&self) -> Vec<Arc<Self>> {
        self.state.lock().scopes.clone()
    }

    pub(super) fn find_scope(
        self: &Arc<Self>,
        caller: ActorRef,
        token: i64,
    ) -> Result<Arc<Self>, String> {
        if caller != self.owner {
            return Err("resource scope belongs to another actor incarnation".into());
        }
        let found = self
            .find_retained_scope(token)
            .ok_or_else(|| "resource scope is not retained by this owner".to_string())?;
        found.with_admission(|| ())?;
        Ok(found)
    }

    fn find_retained_scope(self: &Arc<Self>, token: i64) -> Option<Arc<Self>> {
        if self.scope_id == Some(token) {
            return Some(self.clone());
        }
        self.scopes()
            .into_iter()
            .find_map(|scope| scope.find_retained_scope(token))
    }

    /// Lock ancestry in one order so closing any ancestor fences child
    /// admission. The callback must neither await nor reenter this owner.
    pub(super) fn with_admission<T>(&self, operation: impl FnOnce() -> T) -> Result<T, String> {
        self.with_admission_state(|_| operation())
    }

    /// Lock shared ancestors once when moving membership between two owners.
    /// The callback may access the request registry, never these owner states.
    pub(super) fn with_transfer_admission<T>(
        self: &Arc<Self>,
        destination: &Arc<Self>,
        operation: impl FnOnce() -> T,
    ) -> Result<T, String> {
        self.with_transfer_states(destination, |_, _, _| operation())
    }

    fn with_transfer_states<T>(
        self: &Arc<Self>,
        destination: &Arc<Self>,
        operation: impl FnOnce(
            &mut [parking_lot::MutexGuard<'_, InvocationWorkState>],
            usize,
            usize,
        ) -> T,
    ) -> Result<T, String> {
        if self.owner != destination.owner {
            return Err("resource cleanup transfer crosses actor authority".into());
        }
        let mut owners = Vec::new();
        for leaf in [self, destination] {
            let mut chain = vec![leaf.clone()];
            let mut current = leaf.clone();
            while current.scope_id.is_some() {
                current = current
                    .parent
                    .upgrade()
                    .ok_or_else(|| "resource scope parent is no longer retained".to_string())?;
                chain.push(current.clone());
            }
            chain.reverse();
            for (depth, owner) in chain.into_iter().enumerate() {
                if !owners
                    .iter()
                    .any(|(_, existing)| Arc::ptr_eq(existing, &owner))
                {
                    owners.push((depth, owner));
                }
            }
        }
        owners.sort_by_key(|(depth, owner)| (*depth, Arc::as_ptr(owner) as usize));
        let source_index = owners
            .iter()
            .position(|(_, owner)| Arc::ptr_eq(owner, self))
            .ok_or_else(|| "source cleanup owner is not retained".to_string())?;
        let destination_index = owners
            .iter()
            .position(|(_, owner)| Arc::ptr_eq(owner, destination))
            .ok_or_else(|| "destination cleanup owner is not retained".to_string())?;
        let mut states = Vec::new();
        for (_, owner) in &owners {
            let state = owner.state.lock();
            if state.phase != InvocationWorkPhase::Active {
                return Err("resource scope ownership is closed".into());
            }
            states.push(state);
        }
        Ok(operation(&mut states, source_index, destination_index))
    }

    fn with_admission_state<T>(
        &self,
        operation: impl FnOnce(&mut InvocationWorkState) -> T,
    ) -> Result<T, String> {
        self.with_phase_state(InvocationWorkPhase::Active, operation)
    }

    fn with_phase_state<T>(
        &self,
        phase: InvocationWorkPhase,
        operation: impl FnOnce(&mut InvocationWorkState) -> T,
    ) -> Result<T, String> {
        let mut ancestors = Vec::new();
        let mut parent = self.parent.upgrade();
        if self.scope_id.is_some() && parent.is_none() {
            return Err("resource scope parent is no longer retained".into());
        }
        while let Some(owner) = parent {
            parent = owner.parent.upgrade();
            if owner.scope_id.is_some() && parent.is_none() {
                return Err("resource scope parent is no longer retained".into());
            }
            ancestors.push(owner);
        }
        ancestors.reverse();
        let mut guards = Vec::new();
        for ancestor in &ancestors {
            let guard = ancestor.state.lock();
            if guard.phase != InvocationWorkPhase::Active {
                return Err("resource scope ownership is closed".into());
            }
            guards.push(guard);
        }
        let mut state = self.state.lock();
        if state.phase != phase {
            return Err("resource scope ownership is closed".into());
        }
        Ok(operation(&mut state))
    }

    pub(super) fn matches(&self, owner: ActorRef, reservation: &RequestReservationOwner) -> bool {
        self.owner == owner && &self.reservation == reservation
    }

    pub(crate) fn register_compiler_work(
        &self,
        receipt: crate::termination::CompilerWorkReceipt,
    ) -> bool {
        self.with_admission_state(|state| state.compilers.push(receipt))
            .is_ok()
    }

    pub(super) fn begin_publication(&self) -> bool {
        self.with_admission_state(|state| state.phase = InvocationWorkPhase::Publishing)
            .is_ok()
    }

    pub(crate) fn register_publication_compiler_work(
        &self,
        receipt: crate::termination::CompilerWorkReceipt,
    ) -> bool {
        self.with_phase_state(InvocationWorkPhase::Publishing, |state| {
            state.compilers.push(receipt)
        })
        .is_ok()
    }

    #[cfg(test)]
    pub(crate) fn register_command(&self, id: String) -> Result<(), CommandError> {
        self.with_admission_state(|state| {
            if !state.commands.contains(&id) {
                state.commands.push(id);
            }
        })
        .map_err(CommandError::CommandUnavailable)
    }

    pub(super) fn find_command_owner(
        self: &Arc<Self>,
        marker: &crate::request::ResourceCleanupOwner,
    ) -> Option<Arc<Self>> {
        if &self.resource_cleanup_owner() == marker {
            return Some(self.clone());
        }
        self.scopes()
            .into_iter()
            .find_map(|scope| scope.find_command_owner(marker))
    }

    pub(crate) fn register_command_with_jobs(
        &self,
        jobs: &CommandJobs,
        id: &str,
    ) -> Result<(), CommandError> {
        self.with_admission_state(|state| {
            if state.commands.iter().any(|command| command == id) {
                return if jobs.cleanup_owner(self.owner, id)? == self.resource_cleanup_owner() {
                    Ok(())
                } else {
                    Err(CommandError::CommandUnauthorized)
                };
            }
            jobs.transfer_cleanup_owner(
                self.owner,
                id,
                &crate::request::ResourceCleanupOwner::Actor,
                self.resource_cleanup_owner(),
                |probe| {
                    state.commands.push(id.into());
                    if let Some(probe) = probe {
                        if !state.commands.iter().any(|job| job == probe) {
                            state.commands.push(probe.into());
                        }
                    }
                },
            )
        })
        .map_err(CommandError::CommandUnavailable)?
    }

    pub(super) fn adopt_actor_command(
        &self,
        jobs: &CommandJobs,
        caller: ActorRef,
        id: &str,
    ) -> Result<(), CommandError> {
        if caller != self.owner {
            return Err(CommandError::CommandUnauthorized);
        }
        self.with_admission_state(|state| {
            jobs.transfer_cleanup_owner(
                caller,
                id,
                &crate::request::ResourceCleanupOwner::Actor,
                self.resource_cleanup_owner(),
                |probe| {
                    if !state.commands.iter().any(|command| command == id) {
                        state.commands.push(id.into());
                    }
                    if let Some(probe) = probe {
                        if !state.commands.iter().any(|job| job == probe) {
                            state.commands.push(probe.into());
                        }
                    }
                    state.detached_commands.remove(id);
                    if let Some(probe) = probe {
                        state.detached_commands.remove(probe);
                    }
                },
            )
        })
        .map_err(CommandError::CommandUnavailable)?
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
        self.with_admission_state(|state| {
            if state.detached_commands.contains(id) {
                return if jobs.cleanup_owner(caller, id)?
                    == crate::request::ResourceCleanupOwner::Actor
                {
                    Ok(())
                } else {
                    Err(CommandError::CommandUnauthorized)
                };
            }
            if !state.commands.iter().any(|command| command == id) {
                return Err(CommandError::CommandUnauthorized);
            }
            jobs.transfer_cleanup_owner(
                caller,
                id,
                &self.resource_cleanup_owner(),
                crate::request::ResourceCleanupOwner::Actor,
                |probe| {
                    state
                        .commands
                        .retain(|job| job != id && probe != Some(job.as_str()));
                    state.detached_commands.insert(id.into());
                    if let Some(probe) = probe {
                        state.detached_commands.insert(probe.into());
                    }
                },
            )
        })
        .map_err(CommandError::CommandUnavailable)?
    }

    pub(super) fn transfer_command_to_run(
        &self,
        jobs: &CommandJobs,
        kernel: &KernelContext,
        caller: ActorRef,
        id: &str,
    ) -> Result<(), CommandError> {
        if caller != self.owner {
            return Err(CommandError::CommandUnauthorized);
        }
        self.with_admission_state(|state| {
            if !state.commands.iter().any(|command| command == id) {
                return Err(CommandError::CommandUnauthorized);
            }
            jobs.transfer_cleanup_owner_in_context(
                kernel,
                caller,
                id,
                &self.resource_cleanup_owner(),
                ResourceCleanupOwner::Run,
                |probe| {
                    state
                        .commands
                        .retain(|job| job != id && probe != Some(job.as_str()));
                    state.detached_commands.insert(id.into());
                    if let Some(probe) = probe {
                        state.detached_commands.insert(probe.into());
                    }
                },
            )
        })
        .map_err(CommandError::CommandUnavailable)?
    }

    pub(super) fn adopt_run_command(
        &self,
        jobs: &CommandJobs,
        kernel: &KernelContext,
        caller: ActorRef,
        id: &str,
    ) -> Result<(), CommandError> {
        if caller != self.owner {
            return Err(CommandError::CommandUnauthorized);
        }
        self.with_admission_state(|state| {
            jobs.transfer_cleanup_owner_in_context(
                kernel,
                caller,
                id,
                &ResourceCleanupOwner::Run,
                self.resource_cleanup_owner(),
                |probe| {
                    if !state.commands.iter().any(|job| job == id) {
                        state.commands.push(id.into());
                    }
                    if let Some(probe) = probe {
                        if !state.commands.iter().any(|job| job == probe) {
                            state.commands.push(probe.into());
                        }
                    }
                    state.detached_commands.remove(id);
                    if let Some(probe) = probe {
                        state.detached_commands.remove(probe);
                    }
                },
            )
        })
        .map_err(CommandError::CommandUnavailable)?
    }

    pub(super) fn transfer_command_to_actor(
        &self,
        jobs: &CommandJobs,
        caller: ActorRef,
        id: &str,
    ) -> Result<(), CommandError> {
        self.detach_command(jobs, caller, id)
    }

    pub(super) fn transfer_command_to_owner(
        self: &Arc<Self>,
        destination: &Arc<Self>,
        jobs: &CommandJobs,
        caller: ActorRef,
        id: &str,
    ) -> Result<(), CommandError> {
        if caller != self.owner || jobs.owner(id)? != caller || destination.owner != caller {
            return Err(CommandError::CommandUnauthorized);
        }
        self.with_transfer_states(destination, |states, source, target| {
            let Some(index) = states[source].commands.iter().position(|job| job == id) else {
                return Err(CommandError::CommandUnauthorized);
            };
            jobs.transfer_cleanup_owner(
                caller,
                id,
                &self.resource_cleanup_owner(),
                destination.resource_cleanup_owner(),
                |probe| {
                    if source != target {
                        let job = states[source].commands.remove(index);
                        if !states[target].commands.contains(&job) {
                            states[target].commands.push(job);
                        }
                        if let Some(probe) = probe {
                            states[source].commands.retain(|command| command != probe);
                            if !states[target].commands.iter().any(|job| job == probe) {
                                states[target].commands.push(probe.into());
                            }
                        }
                    }
                    states[target].detached_commands.remove(id);
                    if let Some(probe) = probe {
                        states[target].detached_commands.remove(probe);
                    }
                },
            )
        })
        .map_err(CommandError::CommandUnavailable)?
    }

    pub(super) fn transfer_worker_to_actor(
        &self,
        caller: ActorRef,
        actor: ActorRef,
    ) -> Result<(), String> {
        if caller != self.owner {
            return Err("worker cleanup transfer is unauthorized".into());
        }
        self.with_admission_state(|state| {
            if state.detached_workers.contains(&actor) {
                return Ok(());
            }
            let index = state
                .workers
                .iter()
                .position(|worker| worker.identity() == actor)
                .ok_or_else(|| "worker is not admitted to this cleanup owner".to_string())?;
            state.workers.remove(index);
            state.detached_workers.insert(actor);
            Ok(())
        })?
    }

    pub(super) fn register_scope_realm(
        &self,
        context: ActorSessionContext,
        realm: tidepool_codegen::suspension::RealmId,
    ) -> Result<(), String> {
        if context.actor != self.owner || self.scope_id.is_none() {
            return Err("resource scope realm is unauthorized".into());
        }
        self.with_admission_state(|state| {
            if !state
                .realms
                .iter()
                .any(|(existing, existing_realm)| existing == &context && *existing_realm == realm)
            {
                state.realms.push((context, realm));
            }
        })
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
        self.with_admission(|| {
            if self.scope_id.is_some() {
                requests.transfer_request_cleanup_owner(
                    caller,
                    request,
                    &self.cleanup_owner,
                    ResourceCleanupOwner::Actor,
                )
            } else {
                requests.detach_invocation_request(caller, request, Some(&self.reservation))
            }
        })
        .map_err(|_| crate::ReplyError::CancellationRequested)?
    }

    pub(super) fn transfer_request_to_owner(
        self: &Arc<Self>,
        destination: &Arc<Self>,
        requests: &RequestRegistry,
        caller: ActorRef,
        request: crate::RequestId,
    ) -> Result<(), crate::ReplyError> {
        if caller != self.owner || caller != destination.owner {
            return Err(crate::ReplyError::Unauthorized);
        }
        self.with_transfer_admission(destination, || {
            requests.transfer_request_cleanup_owner(
                caller,
                request,
                &self.cleanup_owner,
                destination.cleanup_owner.clone(),
            )
        })
        .map_err(|_| crate::ReplyError::CancellationRequested)?
    }

    pub(super) fn register_transient_watch(
        &self,
        watch: crate::WatchId,
    ) -> Result<(), crate::ReplyError> {
        self.with_admission_state(|state| {
            if !state.watches.contains(&watch) {
                state.watches.push(watch);
            }
        })
        .map_err(|_| crate::ReplyError::CancellationRequested)
    }

    #[cfg(test)]
    fn register_worker(&self, child: LocalActorRef) -> Result<(), String> {
        self.with_admission_state(|state| {
            if !state
                .workers
                .iter()
                .any(|worker| worker.identity() == child.identity())
            {
                state.workers.push(child);
            }
        })
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
        self.with_admission_state(|state| {
            if !state.groups.contains(&group) {
                state.groups.push(group);
            }
        })
    }

    pub(super) fn is_closed(&self) -> bool {
        self.state.lock().phase == InvocationWorkPhase::Closing
    }

    pub(super) fn close(&self) {
        let scopes = {
            let mut state = self.state.lock();
            state.phase = InvocationWorkPhase::Closing;
            state.scopes.clone()
        };
        for scope in scopes {
            scope.close();
        }
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
        for scope in self.scopes() {
            let child_cleanup = Box::pin(scope.cleanup(environment, kernel)).await;
            if let Some(token) = scope.scope_token() {
                cleanup.scopes.push((token, child_cleanup));
            }
        }
        // Reserved identities never reached a target. Preserve the original
        // rollback fence, including internally detached branch reservations,
        // before target cancellation changes any request state.
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

        let mut request_cancellations = Vec::new();
        for request in environment
            .requests
            .cleanup_owner_requests(self.owner, &self.cleanup_owner)
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
        let mut watch_notifications = Vec::new();
        for watch in watches {
            match environment
                .requests
                .release_transient_watch(self.owner, watch)
            {
                Ok(notifications) => watch_notifications.extend(notifications),
                Err(error) if error != crate::ReplyError::Stale => cleanup
                    .failures
                    .push(format!("transient watch {} release: {error:?}", watch.0)),
                Err(_) => {}
            }
        }
        if tokio::time::timeout(
            RELEASE_WAIT,
            publish_request_notifications(
                &environment.requests,
                &environment.deployments,
                watch_notifications,
            ),
        )
        .await
        .is_err()
        {
            cleanup.settlement_notifications_pending =
                environment.requests.has_settlement_notifications();
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
        let terminal_summary = if self.scope_id.is_some() {
            "owning resource scope ended"
        } else {
            "owning tool invocation ended"
        };
        let worker_cleanup =
            futures_util::future::join_all(workers.into_iter().map(|child| async move {
                let actor = child.identity();
                let terminal = ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: terminal_summary.into(),
                    diagnostic: None,
                };
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
            }));
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
        let realms = self.state.lock().realms.clone();
        for (context, realm) in realms {
            match tokio::time::timeout(
                RELEASE_WAIT,
                environment.runner.close_realm(context.clone(), realm),
            )
            .await
            {
                Ok(Ok(())) => {
                    self.state
                        .lock()
                        .realms
                        .retain(|(pending_context, pending_realm)| {
                            pending_context != &context || *pending_realm != realm
                        })
                }
                outcome => cleanup.failures.push(format!(
                    "resource scope realm {realm:?} cleanup remains unconfirmed: {outcome:?}"
                )),
            }
        }
        self.state.lock().cleanup = Some(cleanup.clone());
        cleanup
    }
}

impl crate::local_actor::WorkerStartupAdmission for InvocationWork {
    fn reserve(&self, actor: ActorRef) -> Result<(), String> {
        self.with_admission_state(|state| state.pending_workers.push(actor))
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
        drop(state);
        self.with_admission(|| ())
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
