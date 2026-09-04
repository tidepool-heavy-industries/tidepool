//! Resident Haskell behavior owned directly by one local actor.
//!
//! Ractor serializes logical turns and owns the mailbox. The shared resident
//! machine registry owns only short-lived machine checkout. This module is
//! the single driver between those boundaries; it does not recreate registry
//! turn leases, parked-obligation maps, or a host-side scheduler.

use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_runtime::session::{
    OutputSink, ParsedBlock, ResidentHole, ResidentOutcome, ResidentSession, RootCustody,
    WorkbenchItemReceipt, WorkbenchItemStatus, WorkbenchRequest, WorkbenchResponse,
    WorkbenchRunStatus,
};
use tokio::sync::mpsc;

use crate::mailbox::{InstalledReceiver, ResidentOutbound};
use crate::request::RequestRegistry;
use crate::resident_workbench::{
    ForkGroupBoundary, ResidentActorBoundary, ResidentActorStartupStep, ResidentKernelBoundary,
    ResidentWorkbenchFragment, ResidentWorkbenchStep,
};
use crate::{
    ActorDescriptor, ActorExitKind, ActorMachineRegistry, ActorRef, ActorSessionContext,
    ActorTerminal, ActorWorkbenchSource, ChildExitNotice, ExternalApplicationFailure,
    ExternalFailureDisposition, KernelBehavior, KernelBehaviorError, KernelCallFailure,
    KernelContext, KernelInvocationFailure, KernelMessage, KernelStep, LocalActorRef, MailboxValue,
    ResidentActorRunner, ResidentActorWorkbenchError, ResidentToolEndpoint,
};

/// A compiled root at the point where ownership moves into its local actor.
pub struct ResidentActorRoot<H, O> {
    descriptor: ActorDescriptor,
    machine: ResidentSession<H, O>,
    outcome: ResidentOutcome,
}

impl<H, O> ResidentActorRoot<H, O> {
    #[must_use]
    pub fn new(
        descriptor: ActorDescriptor,
        machine: ResidentSession<H, O>,
        outcome: ResidentOutcome,
    ) -> Self {
        Self {
            descriptor,
            machine,
            outcome,
        }
    }

    fn into_parts(self) -> (ActorDescriptor, ResidentSession<H, O>, ResidentOutcome) {
        (self.descriptor, self.machine, self.outcome)
    }
}

#[derive(Clone)]
pub struct LocalResidentInstallation {
    pub actor: LocalActorRef,
    pub label: String,
    pub policy: Arc<dyn ResidentToolEndpoint>,
    pub initial_user_message: Option<String>,
    pub launch_worktrees: Vec<String>,
    pub effective_role: crate::EffectiveRole,
    pub supervisor_parent: Option<crate::ActorRef>,
    pub context_parent: Option<crate::ActorRef>,
    pub fork_group: Option<crate::ForkGroupId>,
    pub fork_gate: Option<crate::ForkGroupGate>,
    pub runtime_observation: crate::ActorRuntimeObservationHandle,
}

#[derive(Clone)]
pub enum LocalResidentDeployment {
    PolicyInstalled(LocalResidentInstallation),
    /// A resident program opened another typed session in an already-running
    /// interactive application. The message is an ordinary User activation;
    /// the live value itself is mounted as `sessionInput` in Haskell.
    SessionReady {
        activation: crate::ResidentActivation,
    },
    ChildExited {
        notice: ChildExitNotice,
    },
    WatchChanged {
        notification: crate::request::WatchNotification,
    },
    RequestCancellation {
        notification: crate::RequestCancellationNotification,
    },
    Retired {
        actor: ActorRef,
        terminal: ActorTerminal,
    },
}

struct ResidentEnvironment<H, O> {
    runner: ResidentActorRunner<H, O>,
    deployments: mpsc::UnboundedSender<LocalResidentDeployment>,
    retired: Arc<Mutex<std::collections::HashSet<ActorRef>>>,
    requests: Arc<RequestRegistry>,
    fork_groups: crate::ForkGroupRegistry,
    actors: Arc<Mutex<std::collections::HashMap<ActorRef, ResidentActorRecord>>>,
    fork_workspaces: Option<crate::fork_workspace::SharedForkWorkspaceAdmission>,
}

#[derive(Clone)]
struct ResidentActorRecord {
    descriptor: ActorDescriptor,
    bound_worktree: Option<String>,
    terminal: Option<ActorTerminal>,
    runtime_observation: crate::ActorRuntimeObservationHandle,
}

fn actor_is_self_or_descendant(
    owner: ActorRef,
    candidate: ActorRef,
    records: &std::collections::HashMap<ActorRef, ResidentActorRecord>,
) -> bool {
    let mut cursor = candidate;
    loop {
        if cursor == owner {
            return true;
        }
        let Some(parent) = records
            .get(&cursor)
            .and_then(|record| record.descriptor.supervisor_parent())
        else {
            return false;
        };
        cursor = parent;
    }
}

impl<H, O> Clone for ResidentEnvironment<H, O> {
    fn clone(&self) -> Self {
        Self {
            runner: self.runner.clone(),
            deployments: self.deployments.clone(),
            retired: Arc::clone(&self.retired),
            requests: Arc::clone(&self.requests),
            fork_groups: self.fork_groups.clone(),
            actors: Arc::clone(&self.actors),
            fork_workspaces: self.fork_workspaces.clone(),
        }
    }
}

enum ResidentBoot {
    Prepared(Box<ResidentOutcome>),
    Entry(RootCustody),
}

enum ResidentStanding {
    Boot,
    Receiving(InstalledReceiver),
    Tools(crate::resident_tools::ResidentToolAwait),
    Interactive(crate::interactive_session::ResidentInteractiveAwait),
    Terminal,
}

struct WorkbenchExecutionFailure {
    completed: Vec<WorkbenchItemReceipt>,
    failed_index: usize,
    total: usize,
    source: ResidentActorWorkbenchError,
}

struct SuspendedCast {
    site: u64,
    receiver_continuation: ResidentHole,
    handler_realm: RealmId,
}

enum InteractivePark {
    Parked,
    Cancelled(crate::RequestId),
}

struct ReceiverSettlement<'a> {
    caller: Option<ActorRef>,
    ancestry: &'a crate::CallAncestry,
    suspended: SuspendedCast,
    outcome: ResidentOutcome,
}

enum ResidentCallError {
    Call(KernelCallFailure),
    Runtime(ResidentActorWorkbenchError),
}

fn workbench_failure(
    completed: &[WorkbenchItemReceipt],
    failed_index: usize,
    total: usize,
    source: ResidentActorWorkbenchError,
) -> WorkbenchExecutionFailure {
    WorkbenchExecutionFailure {
        completed: completed.to_vec(),
        failed_index,
        total,
        source,
    }
}

#[derive(Clone, Copy)]
enum ChildExitDisposition {
    Observed,
    Processed,
}

/// Actor-local disposition journal for exact child incarnations. Processed
/// entries remain for the owner's lifetime so repeated late polls cannot
/// recreate a pending observation after the supervisor notice has passed.
#[derive(Default)]
struct ChildExitObservations(std::collections::HashMap<ActorRef, ChildExitDisposition>);

impl ChildExitObservations {
    /// Record a typed observation. Returns whether the supervisor notice was
    /// already processed, in which case a deferred failure can be discarded.
    fn observe(&mut self, child: ActorRef) -> bool {
        match self.0.entry(child) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(ChildExitDisposition::Observed);
                false
            }
            std::collections::hash_map::Entry::Occupied(entry) => {
                matches!(entry.get(), ChildExitDisposition::Processed)
            }
        }
    }

    /// Record supervisor-notice processing. Returns whether the typed program
    /// had already observed the exit and therefore owns its disposition.
    fn process(&mut self, child: ActorRef) -> bool {
        match self.0.insert(child, ChildExitDisposition::Processed) {
            Some(ChildExitDisposition::Observed) => true,
            Some(ChildExitDisposition::Processed) | None => false,
        }
    }
}

/// All actor-local resident state. No field mirrors runnable/parked lifecycle;
/// `standing` is the actual Haskell continuation currently owned by the actor.
pub struct ResidentKernelBehavior<H, O> {
    descriptor: ActorDescriptor,
    environment: ResidentEnvironment<H, O>,
    boot: Option<ResidentBoot>,
    standing: ResidentStanding,
    shutdown_hook: Option<RootCustody>,
    launch_worktrees: Vec<String>,
    policy_installed: bool,
    pending_program: Option<ResidentOutcome>,
    pending_reply: Option<crate::RequestId>,
    pending_cancellation: Option<crate::RequestId>,
    suspended_cast: Option<SuspendedCast>,
    child_exit_observations: ChildExitObservations,
    deferred_child_failures: Vec<ChildExitNotice>,
    next_activation_sequence: u64,
    runtime_observation: crate::ActorRuntimeObservationHandle,
}

impl<H, O> ResidentKernelBehavior<H, O> {
    fn record_child_observation(&mut self, child: ActorRef) {
        if self.child_exit_observations.observe(child) {
            self.deferred_child_failures
                .retain(|notice| notice.child.identity() != child);
        }
    }

    fn prepared(
        descriptor: ActorDescriptor,
        environment: ResidentEnvironment<H, O>,
        outcome: ResidentOutcome,
    ) -> Self {
        Self {
            descriptor,
            environment,
            boot: Some(ResidentBoot::Prepared(Box::new(outcome))),
            standing: ResidentStanding::Boot,
            shutdown_hook: None,
            launch_worktrees: Vec::new(),
            policy_installed: false,
            pending_program: None,
            pending_reply: None,
            pending_cancellation: None,
            suspended_cast: None,
            child_exit_observations: ChildExitObservations::default(),
            deferred_child_failures: Vec::new(),
            next_activation_sequence: 1,
            runtime_observation: crate::ActorRuntimeObservationHandle::default(),
        }
    }

    fn child(
        descriptor: ActorDescriptor,
        environment: ResidentEnvironment<H, O>,
        entry: RootCustody,
        launch_worktrees: Vec<String>,
    ) -> Self {
        Self {
            descriptor,
            environment,
            boot: Some(ResidentBoot::Entry(entry)),
            standing: ResidentStanding::Boot,
            shutdown_hook: None,
            launch_worktrees,
            policy_installed: false,
            pending_program: None,
            pending_reply: None,
            pending_cancellation: None,
            suspended_cast: None,
            child_exit_observations: ChildExitObservations::default(),
            deferred_child_failures: Vec::new(),
            next_activation_sequence: 1,
            runtime_observation: crate::ActorRuntimeObservationHandle::default(),
        }
    }

    fn context(&self, actor: ActorRef) -> ActorSessionContext {
        self.descriptor.session_context(actor)
    }

    fn failure(error: impl std::fmt::Display) -> KernelBehaviorError {
        KernelBehaviorError {
            detail: error.to_string(),
        }
    }

    fn invocation_failure(
        actor: ActorRef,
        error: impl std::fmt::Display,
    ) -> KernelInvocationFailure {
        KernelInvocationFailure::Failed {
            actor,
            detail: error.to_string(),
        }
    }

    /// Publish an installed actor application exactly once readiness has made
    /// its reference usable.
    fn publish_installation(&self, installation: LocalResidentInstallation) {
        let _ = self
            .environment
            .deployments
            .send(LocalResidentDeployment::PolicyInstalled(installation));
    }

    fn publish_retired(&self, actor: ActorRef, terminal: ActorTerminal) {
        self.environment.fork_groups.retire_actor(actor);
        if let Some(record) = self.environment.actors.lock().get_mut(&actor) {
            record.terminal = Some(terminal.clone());
        }
        if self.environment.retired.lock().insert(actor) {
            let _ = self
                .environment
                .deployments
                .send(LocalResidentDeployment::Retired { actor, terminal });
        }
    }

    fn publish_watch_notifications(
        &self,
        notifications: impl IntoIterator<Item = crate::request::WatchNotification>,
    ) {
        for notification in notifications {
            let _ = self
                .environment
                .deployments
                .send(LocalResidentDeployment::WatchChanged { notification });
        }
    }

    fn publish_request_cancellation(
        &self,
        notification: Option<crate::RequestCancellationNotification>,
    ) {
        if let Some(notification) = notification {
            let _ = self
                .environment
                .deployments
                .send(LocalResidentDeployment::RequestCancellation { notification });
        }
    }

    fn status_text(&self, kernel: &KernelContext, actor: ActorRef) -> String {
        let (standing, current_request) = match &self.standing {
            ResidentStanding::Boot => ("booting", None),
            ResidentStanding::Receiving(_) => ("receiving", None),
            ResidentStanding::Tools(_) => ("awaiting-tool", None),
            ResidentStanding::Interactive(awaiting) => {
                ("request-active", Some(awaiting.request.request))
            }
            ResidentStanding::Terminal => ("terminal", None),
        };
        let requests = self.environment.requests.status_for(actor);
        let mut roster = self
            .environment
            .actors
            .lock()
            .iter()
            .map(|(identity, record)| {
                let terminal = record.terminal.clone().or_else(|| {
                    kernel
                        .resolve(*identity)
                        .and_then(|actor| actor.terminal().get())
                });
                let active = self.environment.requests.active_for_target(*identity);
                let state = match terminal {
                    Some(ref terminal) => format!("terminal:{:?}", terminal.kind),
                    None if !active.is_empty() => format!("handling:{active:?}"),
                    None => "running".into(),
                };
                let runtime = record.runtime_observation.snapshot();
                format!(
                    "  - {}@{} label={:?} supervisor={:?} context_parent={:?} role={:?} bound_worktree={:?} provider_thread={:?} cache_input={:?}/{:?} state={}",
                    identity.id.0,
                    identity.incarnation.0,
                    record.descriptor.label(),
                    record.descriptor.supervisor_parent(),
                    record.descriptor.context_parent(),
                    record.descriptor.effective_role().role(),
                    record.bound_worktree,
                    runtime.provider_thread,
                    runtime.cached_input_tokens,
                    runtime.uncached_input_tokens,
                    state,
                )
            })
            .collect::<Vec<_>>();
        roster.sort();
        let runtime = self.runtime_observation.snapshot();
        let current = format!(
            "actor {}@{} label={:?}\n  lineage: supervisor={:?} context_parent={:?} fork_group={:?}\n  context: haskell_snapshot={} provider_thread={:?} provider_parent_thread={:?} cache_input={:?}/{:?}\n  authority: role={:?} effects={} native_tools={:?} workspace={:?} descendants={:?} prompt_profile={:?}\n  runtime: application={} program={standing} current_request={current_request:?} bound_worktree={:?}\n  responses: pending={:?} ready={:?} unavailable={:?}\n  watches: pending={:?} ready={:?} unavailable={:?}",
            actor.id.0,
            actor.incarnation.0,
            self.descriptor.label(),
            self.descriptor.supervisor_parent(),
            self.descriptor.context_parent(),
            self.descriptor.fork_group(),
            self.descriptor.placement().lexical_scope.0,
            runtime.provider_thread,
            runtime.provider_parent_thread,
            runtime.cached_input_tokens,
            runtime.uncached_input_tokens,
            self.descriptor.effective_role().role(),
            self.descriptor.effective_role().haskell_effects_type(),
            self.descriptor.effective_role().native_tools(),
            self.descriptor.effective_role().workspace(),
            self.descriptor.effective_role().descendants(),
            self.descriptor.effective_role().prompt_profile(),
            if self.policy_installed { "attached" } else { "detached" },
            self.launch_worktrees.first(),
            requests.pending_responses,
            requests.ready_responses,
            requests.unavailable_responses,
            requests.pending_watches,
            requests.ready_watches,
            requests.unavailable_watches,
        );
        format!("{current}\nactors:\n{}", roster.join("\n"))
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    fn schedule_request_deadline(
        &self,
        owner: ActorRef,
        request: crate::RequestId,
        deadline: std::time::Duration,
    ) {
        let requests = Arc::clone(&self.environment.requests);
        let deployments = self.environment.deployments.clone();
        tokio::spawn(async move {
            tokio::time::sleep(deadline).await;
            if let Some(notification) = requests.deadline_request(owner, request) {
                let _ =
                    deployments.send(LocalResidentDeployment::RequestCancellation { notification });
            }
        });
    }

    async fn perform_call(
        &self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        ancestry: &crate::CallAncestry,
        target: ActorRef,
        request: MailboxValue,
    ) -> Result<MailboxValue, ResidentCallError> {
        ancestry.enter(target).map_err(ResidentCallError::Call)?;
        let target_ref = kernel
            .resolve(target)
            .ok_or_else(|| ResidentCallError::Call(KernelCallFailure::TargetUnavailable(target)))?;
        if target_ref.terminal().get().is_some() {
            return Err(ResidentCallError::Call(KernelCallFailure::TargetExited(
                target,
            )));
        }
        let target_context = kernel
            .session_context(target)
            .ok_or_else(|| ResidentCallError::Call(KernelCallFailure::TargetUnavailable(target)))?;
        if target_context.placement.session != context.placement.session {
            return Err(ResidentCallError::Call(
                KernelCallFailure::MachineBoundary {
                    caller: context.actor,
                    caller_session: context.placement.session,
                    target,
                    target_session: target_context.placement.session,
                },
            ));
        }
        let request = self
            .environment
            .runner
            .rehome_mailbox_value(
                context.clone(),
                request,
                target_context.placement.resource_scope,
            )
            .await
            .map_err(ResidentCallError::Runtime)?;
        target_ref
            .address()
            .call(
                |reply| KernelMessage::Call {
                    caller: context.actor,
                    ancestry: ancestry.clone(),
                    request,
                    reply,
                },
                None,
            )
            .await
            .map_err(|_| ResidentCallError::Call(KernelCallFailure::TargetExited(target)))?
            .success_or_else(|| KernelCallFailure::TargetExited(target))
            .map_err(ResidentCallError::Call)?
            .map_err(ResidentCallError::Call)
    }

    async fn start_child(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        start: crate::ResidentActorStart,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let (descriptor, parent_hole, entry, launch_worktrees, fork_group, fork_workspace) =
            start.into_parts();
        let started = self
            .try_start_child(
                kernel,
                context,
                descriptor,
                entry,
                launch_worktrees,
                fork_group,
                fork_workspace,
            )
            .await;
        let (child, allocated_label, admitted_worktree) = match started {
            Ok(started) => started,
            Err(error) if fork_group.is_some() => {
                if let Some(group) = fork_group {
                    if let Ok(children) = self.environment.fork_groups.abort(group, context.actor) {
                        for child in children {
                            if let Some(child) = kernel.resolve(child) {
                                let _ = child
                                    .shutdown(ActorTerminal {
                                        kind: ActorExitKind::Cancelled,
                                        summary: "fork group admission failed".into(),
                                    })
                                    .await;
                            }
                        }
                    }
                }
                return self
                    .environment
                    .runner
                    .resume_fork_failure(context.clone(), parent_hole, error.to_string())
                    .await;
            }
            Err(error) => return Err(error),
        };
        match admitted_worktree {
            Some(worktree) => {
                self.environment
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
                self.environment
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

    #[allow(clippy::too_many_arguments)]
    async fn try_start_child(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        mut descriptor: ActorDescriptor,
        entry: RootCustody,
        mut launch_worktrees: Vec<String>,
        fork_group: Option<crate::ForkGroupId>,
        fork_workspace: Option<crate::ForkWorkspaceSeed>,
    ) -> Result<
        (
            LocalActorRef,
            String,
            Option<tidepool_bridge_effects::WtWorktreeHandle>,
        ),
        ResidentActorWorkbenchError,
    > {
        if !self
            .descriptor
            .profile()
            .permits_child(descriptor.profile())
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "actor profile {:?} cannot start child profile {:?}",
                self.descriptor.profile(),
                descriptor.profile()
            )));
        }
        if !descriptor.effective_role().respects_role_ceiling() {
            return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "requested effect row exceeds or duplicates the {:?} role ceiling",
                descriptor.effective_role().role()
            )));
        }
        if descriptor.effective_role().role() == crate::ActorRole::Scaffolding {
            let parent_budget = self.descriptor.effective_role().descendants();
            let role = descriptor.effective_role().clone().with_descendant_budget(
                crate::DescendantBudget {
                    maximum_depth: parent_budget.maximum_depth.saturating_sub(1),
                    maximum_active_children: parent_budget.maximum_active_children,
                },
            );
            descriptor = descriptor.with_effective_role(role);
        }
        if descriptor.context_parent().is_some()
            && !self
                .descriptor
                .effective_role()
                .permits_child(descriptor.effective_role())
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "actor role {:?} cannot context-fork child role {:?}",
                self.descriptor.effective_role().role(),
                descriptor.effective_role().role()
            )));
        }
        if let Some(group) = fork_group {
            let requested = crate::ActorPath::parse(descriptor.label())
                .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
            let allocated = self
                .environment
                .fork_groups
                .claim(group, context.actor, &requested)
                .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
            descriptor = descriptor.with_actor_path(allocated);
        } else if descriptor.context_parent().is_some() {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "context fork did not name an admission group".into(),
            ));
        }
        let admitted_worktree = if let Some(seed) = fork_workspace {
            let admission = self.environment.fork_workspaces.clone().ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    "context-fork workspace admission is not installed".into(),
                )
            })?;
            let owner = context.actor;
            let actor_path = descriptor.label().to_owned();
            let admitted =
                tokio::task::spawn_blocking(move || admission.admit(owner, &actor_path, seed))
                    .await
                    .map_err(ResidentActorWorkbenchError::Join)?
                    .map_err(|error| {
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "worktree admission for `{}` failed: {}",
                            descriptor.label(),
                            error
                        ))
                    })?;
            launch_worktrees = vec![admitted.handle_receipt.tree_id.raw.clone()];
            Some(admitted)
        } else {
            None
        };
        if descriptor.placement().session != context.placement.session {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "child actor entry crossed a resident machine boundary".into(),
            ));
        }
        let allocated_label = descriptor.label().to_string();
        let child = kernel
            .spawn_child(
                None,
                Self::child(
                    descriptor,
                    self.environment.clone(),
                    entry,
                    launch_worktrees,
                ),
            )
            .await
            .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
        Ok((child, allocated_label, admitted_worktree))
    }

    async fn resolve_outbound(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        ancestry: &crate::CallAncestry,
        outbound: ResidentOutbound,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        match outbound {
            ResidentOutbound::Cast {
                target,
                continuation,
                request,
            } => {
                let target_ref = kernel.resolve(target).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::TargetUnavailable(target).to_string(),
                    )
                })?;
                let target_context = kernel.session_context(target).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::TargetUnavailable(target).to_string(),
                    )
                })?;
                if target_context.placement.session != context.placement.session {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::MachineBoundary {
                            caller: context.actor,
                            caller_session: context.placement.session,
                            target,
                            target_session: target_context.placement.session,
                        }
                        .to_string(),
                    ));
                }
                let request = self
                    .environment
                    .runner
                    .rehome_mailbox_value(
                        context.clone(),
                        request,
                        target_context.placement.resource_scope,
                    )
                    .await?;
                target_ref
                    .address()
                    .send_message(KernelMessage::Cast {
                        sender: context.actor,
                        request,
                    })
                    .map_err(|_| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            KernelCallFailure::TargetExited(target).to_string(),
                        )
                    })?;
                self.environment
                    .runner
                    .resume_fork_unit(context.clone(), continuation)
                    .await
            }
            ResidentOutbound::Call {
                target,
                continuation,
                request,
            } => {
                let reply = self
                    .perform_call(kernel, context, ancestry, target, request)
                    .await
                    .map_err(|error| match error {
                        ResidentCallError::Call(error) => {
                            ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                        }
                        ResidentCallError::Runtime(error) => error,
                    })?;
                self.environment
                    .runner
                    .resume_live(context.clone(), continuation, reply.into_custody())
                    .await
            }
            ResidentOutbound::TryCall {
                target,
                continuation,
                request,
            } => {
                let failure = match self
                    .perform_call(kernel, context, ancestry, target, request)
                    .await
                {
                    Ok(reply) => {
                        drop(reply);
                        None
                    }
                    Err(ResidentCallError::Call(error)) => Some(error.to_string()),
                    Err(ResidentCallError::Runtime(error)) => return Err(error),
                };
                self.environment
                    .runner
                    .resume_call_status(context.clone(), continuation, failure)
                    .await
            }
        }
    }

    async fn resolve_effect(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        ancestry: &crate::CallAncestry,
        boundary: ResidentActorBoundary,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        match boundary {
            ResidentActorBoundary::ActorContext(continuation) => {
                self.environment
                    .runner
                    .resume_actor_context(
                        context.clone(),
                        continuation,
                        self.descriptor.clone(),
                        self.launch_worktrees.first().cloned(),
                        self.runtime_observation.snapshot(),
                    )
                    .await
            }
            ResidentActorBoundary::AgentInspect(inspection) => {
                let records = self.environment.actors.lock().clone();
                let observation = records.get(&inspection.target).and_then(|record| {
                    actor_is_self_or_descendant(context.actor, inspection.target, &records).then(
                        || crate::resident_workbench::AgentRosterProjection {
                            actor: inspection.target,
                            descriptor: record.descriptor.clone(),
                            bound_worktree: record.bound_worktree.clone(),
                            terminal: record.terminal.clone().or_else(|| {
                                kernel
                                    .resolve(inspection.target)
                                    .and_then(|actor| actor.terminal().get())
                            }),
                            runtime: record.runtime_observation.snapshot(),
                        },
                    )
                });
                self.environment
                    .runner
                    .resume_agent_observation(context.clone(), inspection.continuation, observation)
                    .await
            }
            ResidentActorBoundary::AgentList(continuation) => {
                let records = self.environment.actors.lock().clone();
                let mut roster = records
                    .iter()
                    .filter(|(actor, _)| {
                        actor_is_self_or_descendant(context.actor, **actor, &records)
                    })
                    .map(
                        |(actor, record)| crate::resident_workbench::AgentRosterProjection {
                            actor: *actor,
                            descriptor: record.descriptor.clone(),
                            bound_worktree: record.bound_worktree.clone(),
                            terminal: record.terminal.clone().or_else(|| {
                                kernel
                                    .resolve(*actor)
                                    .and_then(|actor| actor.terminal().get())
                            }),
                            runtime: record.runtime_observation.snapshot(),
                        },
                    )
                    .collect::<Vec<_>>();
                roster.sort_by_key(|entry| (entry.actor.id, entry.actor.incarnation));
                self.environment
                    .runner
                    .resume_agent_roster(context.clone(), continuation, roster)
                    .await
            }
            ResidentActorBoundary::AgentForget(forget) => {
                let authorized_terminal = {
                    let records = self.environment.actors.lock();
                    records.get(&forget.target).and_then(|record| {
                        actor_is_self_or_descendant(context.actor, forget.target, &records)
                            .then(|| {
                                record.terminal.clone().or_else(|| {
                                    kernel
                                        .resolve(forget.target)
                                        .and_then(|actor| actor.terminal().get())
                                })
                            })
                            .flatten()
                    })
                };
                let outcome = if authorized_terminal.is_none() {
                    let records = self.environment.actors.lock();
                    if records.contains_key(&forget.target)
                        && actor_is_self_or_descendant(context.actor, forget.target, &records)
                    {
                        crate::resident_workbench::AgentForgetProjection::Running
                    } else {
                        crate::resident_workbench::AgentForgetProjection::Unavailable
                    }
                } else {
                    match self
                        .environment
                        .requests
                        .forget_terminal_actor_metadata(forget.target)
                    {
                        Ok(()) => {
                            self.environment.actors.lock().remove(&forget.target);
                            self.environment.retired.lock().remove(&forget.target);
                            let _ = kernel.forget_terminal_actor(forget.target);
                            crate::resident_workbench::AgentForgetProjection::Forgotten
                        }
                        Err((requests, watches)) => {
                            crate::resident_workbench::AgentForgetProjection::Retained {
                                requests,
                                watches,
                            }
                        }
                    }
                };
                self.environment
                    .runner
                    .resume_agent_forget(context.clone(), forget.continuation, outcome)
                    .await
            }
            ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Begin {
                continuation,
                relative,
                group,
                branches,
            }) => {
                let admitted = (|| {
                    let group = if relative {
                        let parent = self.descriptor.actor_path().ok_or_else(|| {
                            "relative subgroup requires an allocated parent actor path".to_string()
                        })?;
                        let segment = crate::ActorPathSegment::new(group)
                            .map_err(|error| error.to_string())?;
                        parent.child(segment).map_err(|error| error.to_string())?
                    } else {
                        crate::ActorPath::parse(&group).map_err(|error| error.to_string())?
                    };
                    let branches = branches
                        .into_iter()
                        .map(crate::ActorPathSegment::new)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|error| error.to_string())?;
                    let budget = self.descriptor.effective_role().descendants();
                    if budget.maximum_depth == 0 {
                        return Err("this actor role cannot recursively unfold context".into());
                    }
                    let group_path = group.to_string();
                    let (group_id, reservations) = self
                        .environment
                        .fork_groups
                        .begin(
                            context.actor,
                            group,
                            branches,
                            usize::from(budget.maximum_active_children),
                        )
                        .map_err(|error| error.to_string())?;
                    Ok::<_, String>((group_id, group_path, reservations))
                })();
                match admitted {
                    Ok((group_id, group_path, reservations)) => {
                        self.environment
                            .runner
                            .resume_fork_group(
                                context.clone(),
                                continuation,
                                group_id,
                                group_path,
                                reservations
                                    .into_iter()
                                    .map(|reservation| reservation.allocated.to_string())
                                    .collect(),
                            )
                            .await
                    }
                    Err(detail) => {
                        self.environment
                            .runner
                            .resume_fork_failure(context.clone(), continuation, detail)
                            .await
                    }
                }
            }
            ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Commit {
                continuation,
                group,
            }) => {
                let mut phase = match self
                    .environment
                    .fork_groups
                    .request_commit(group, context.actor)
                {
                    Ok(phase) => phase,
                    Err(error) => {
                        return self
                            .environment
                            .runner
                            .resume_fork_failure(context.clone(), continuation, error.to_string())
                            .await;
                    }
                };
                loop {
                    let current = *phase.borrow();
                    match current {
                        crate::ForkGroupPhase::Ready | crate::ForkGroupPhase::Committed => break,
                        crate::ForkGroupPhase::Aborted => {
                            let children = self
                                .environment
                                .fork_groups
                                .cleanup_failed(group, context.actor)
                                .map_err(|error| {
                                    ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                                })?;
                            for child in children {
                                if let Some(child) = kernel.resolve(child) {
                                    let _ = child
                                        .shutdown(ActorTerminal {
                                            kind: ActorExitKind::Cancelled,
                                            summary: "fork group admission failed".into(),
                                        })
                                        .await;
                                }
                            }
                            return self
                                .environment
                                .runner
                                .resume_fork_failure(
                                    context.clone(),
                                    continuation,
                                    format!(
                                        "fork group {} was aborted while awaiting readiness",
                                        group.0
                                    ),
                                )
                                .await;
                        }
                        crate::ForkGroupPhase::Staging => {}
                    }
                    if phase.changed().await.is_err() {
                        return self
                            .environment
                            .runner
                            .resume_fork_failure(
                                context.clone(),
                                continuation,
                                format!("fork group {} readiness channel closed", group.0),
                            )
                            .await;
                    }
                }
                self.environment
                    .runner
                    .resume_fork_unit(context.clone(), continuation)
                    .await
            }
            ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Abort {
                continuation,
                group,
            }) => {
                let children = match self.environment.fork_groups.abort(group, context.actor) {
                    Ok(children) => children,
                    Err(error) => {
                        return self
                            .environment
                            .runner
                            .resume_fork_failure(context.clone(), continuation, error.to_string())
                            .await;
                    }
                };
                for child in children {
                    if let Some(child) = kernel.resolve(child) {
                        let _ = child
                            .shutdown(ActorTerminal {
                                kind: ActorExitKind::Cancelled,
                                summary: "fork group admission aborted".into(),
                            })
                            .await;
                    }
                }
                self.environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
            }
            ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Cleanup {
                continuation,
                group,
            }) => {
                let outcome = self
                    .environment
                    .fork_groups
                    .cleanup_committed(group, context.actor);
                self.environment
                    .runner
                    .resume_fork_cleanup(context.clone(), continuation, outcome)
                    .await
            }
            ResidentActorBoundary::Start(start) => self.start_child(kernel, context, start).await,
            ResidentActorBoundary::Outbound(outbound) => {
                self.resolve_outbound(kernel, context, ancestry, outbound)
                    .await
            }
            ResidentActorBoundary::Wait(wait) => {
                let target = kernel.resolve(wait.target).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::TargetUnavailable(wait.target).to_string(),
                    )
                })?;
                let target_context = kernel.session_context(wait.target).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::TargetUnavailable(wait.target).to_string(),
                    )
                })?;
                if target_context.placement.session != context.placement.session {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "awaitExit crossed a resident machine boundary".into(),
                    ));
                }
                self.record_child_observation(wait.target);
                let terminal = target.terminal().wait().await;
                self.environment
                    .runner
                    .resume_terminal(context.clone(), wait.continuation, terminal)
                    .await
            }
            ResidentActorBoundary::Poll(poll) => {
                let terminal = kernel
                    .resolve(poll.target)
                    .and_then(|target| target.terminal().get());
                let observed = terminal.is_some();
                let outcome = self
                    .environment
                    .runner
                    .resume_optional_terminal(context.clone(), poll.continuation, terminal)
                    .await?;
                if observed {
                    self.record_child_observation(poll.target);
                }
                Ok(outcome)
            }
            ResidentActorBoundary::RequestReservation(reservation) => {
                crate::ActorPathSegment::new(&reservation.label).map_err(|error| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "invalid request label: {error}"
                    ))
                })?;
                let request = self.environment.requests.reserve_labeled(
                    context.actor,
                    reservation.target,
                    reservation.label,
                );
                self.environment
                    .runner
                    .resume_int(context.clone(), reservation.continuation, request.0)
                    .await
            }
            ResidentActorBoundary::RequestSubmission(submission) => {
                let request_deadline = submission.deadline;
                let target = kernel.resolve(submission.target);
                let target_context = kernel.session_context(submission.target);
                let deliverable = target.as_ref().zip(target_context.as_ref()).filter(
                    |(target, target_context)| {
                        target.terminal().get().is_none()
                            && target_context.placement.session == context.placement.session
                    },
                );
                if let Some((target, target_context)) = deliverable {
                    let message = self
                        .environment
                        .runner
                        .rehome_mailbox_value(
                            context.clone(),
                            submission.message,
                            target_context.placement.resource_scope,
                        )
                        .await?;
                    self.environment
                        .requests
                        .mark_queued(context.actor, submission.target, submission.request)
                        .map_err(|error| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "request submission was rejected: {error:?}"
                            ))
                        })?;
                    if target
                        .address()
                        .send_message(KernelMessage::Cast {
                            sender: context.actor,
                            request: message,
                        })
                        .is_err()
                    {
                        let notifications = self
                            .environment
                            .requests
                            .mark_target_unavailable(context.actor, submission.request);
                        self.publish_watch_notifications(notifications);
                    }
                    let outcome = self
                        .environment
                        .runner
                        .resume_unit(context.clone(), submission.continuation)
                        .await;
                    if let Some(deadline) = request_deadline {
                        self.schedule_request_deadline(context.actor, submission.request, deadline);
                    }
                    outcome
                } else {
                    let notifications = self
                        .environment
                        .requests
                        .mark_target_unavailable(context.actor, submission.request);
                    self.publish_watch_notifications(notifications);
                    let outcome = self
                        .environment
                        .runner
                        .resume_unit(context.clone(), submission.continuation)
                        .await;
                    if let Some(deadline) = request_deadline {
                        self.schedule_request_deadline(context.actor, submission.request, deadline);
                    }
                    outcome
                }
            }
            ResidentActorBoundary::ResponsePoll(poll) => {
                let observation = self
                    .environment
                    .requests
                    .observe_response(context.actor, poll.request);
                self.environment
                    .runner
                    .resume_response_observation(context.clone(), poll.continuation, observation)
                    .await
            }
            ResidentActorBoundary::RequestCancellation(cancellation) => {
                let projected = self
                    .environment
                    .requests
                    .cancel_request(
                        context.actor,
                        cancellation.request,
                        crate::CancellationReason::RequesterCancelled,
                    )
                    .map(|(outcome, notification)| {
                        self.publish_request_cancellation(notification);
                        outcome
                    });
                self.environment
                    .runner
                    .resume_cancel_request(context.clone(), cancellation.continuation, projected)
                    .await
            }
            ResidentActorBoundary::ResponseAbandonment(abandonment) => {
                let projected = self
                    .environment
                    .requests
                    .abandon_response(context.actor, abandonment.request)
                    .map(|(outcome, notifications)| {
                        self.publish_watch_notifications(notifications);
                        outcome
                    });
                self.environment
                    .runner
                    .resume_abandonment(context.clone(), abandonment.continuation, projected)
                    .await
            }
            ResidentActorBoundary::ResponseForget(forget) => {
                let outcome = self
                    .environment
                    .requests
                    .forget_response(context.actor, forget.request);
                self.environment
                    .runner
                    .resume_response_forget(context.clone(), forget.continuation, outcome)
                    .await
            }
            ResidentActorBoundary::ReplyPoll(poll) => {
                let observation = self
                    .environment
                    .requests
                    .observe_reply(context.actor, poll.request);
                self.environment
                    .runner
                    .resume_reply_observation(context.clone(), poll.continuation, observation)
                    .await
            }
            ResidentActorBoundary::WatchRegistration(registration) => {
                crate::ActorPathSegment::new(&registration.label).map_err(|error| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "invalid watch label: {error}"
                    ))
                })?;
                let (watch, notifications) = self
                    .environment
                    .requests
                    .register_watch_labeled(
                        context.actor,
                        registration.label,
                        registration.dependencies,
                    )
                    .map_err(|error| {
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "watch registration was rejected: {error:?}"
                        ))
                    })?;
                self.publish_watch_notifications(notifications);
                self.environment
                    .runner
                    .resume_int(context.clone(), registration.continuation, watch.0)
                    .await
            }
            ResidentActorBoundary::WatchPoll(poll) => {
                let observation = self
                    .environment
                    .requests
                    .observe_watch(context.actor, poll.watch);
                self.environment
                    .runner
                    .resume_watch_observation(context.clone(), poll.continuation, observation)
                    .await
            }
            ResidentActorBoundary::WatchForget(forget) => {
                let outcome = self
                    .environment
                    .requests
                    .forget_watch(context.actor, forget.watch);
                self.environment
                    .runner
                    .resume_watch_forget(context.clone(), forget.continuation, outcome)
                    .await
            }
            other => Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "`{}` is not an active actor effect",
                other.operation()
            ))),
        }
    }

    async fn stabilize_program(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        ancestry: &crate::CallAncestry,
        mut outcome: ResidentOutcome,
    ) -> Result<KernelStep<()>, ResidentActorWorkbenchError> {
        loop {
            match self
                .environment
                .runner
                .capture_boundary(context.clone(), outcome, context.placement.resource_scope)
                .await?
            {
                ResidentActorBoundary::Completed => {
                    self.standing = ResidentStanding::Terminal;
                    return Ok(KernelStep::Stop {
                        output: (),
                        terminal: completed_terminal(),
                    });
                }
                ResidentActorBoundary::Receive(receiver) => {
                    self.standing = ResidentStanding::Receiving(receiver);
                    return Ok(KernelStep::Continue(()));
                }
                ResidentActorBoundary::ToolAwait(awaiting) => {
                    if !self.policy_installed {
                        let actor = kernel.resolve(context.actor).ok_or_else(|| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "local actor was absent from its routing directory".into(),
                            )
                        })?;
                        let policy: Arc<dyn ResidentToolEndpoint> =
                            Arc::new(crate::resident_tools::install_local_resident_tools(
                                actor.clone(),
                                &awaiting,
                            ));
                        let fork_gate = self
                            .descriptor
                            .fork_group()
                            .map(|group| self.environment.fork_groups.gate(group, context.actor))
                            .transpose()
                            .map_err(|error| {
                                ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                            })?;
                        let installation = LocalResidentInstallation {
                            actor,
                            label: self.descriptor.label().to_owned(),
                            policy,
                            initial_user_message: awaiting.initial_user_message.clone(),
                            launch_worktrees: self.launch_worktrees.clone(),
                            effective_role: self.descriptor.effective_role().clone(),
                            supervisor_parent: self.descriptor.supervisor_parent(),
                            context_parent: self.descriptor.context_parent(),
                            fork_group: self.descriptor.fork_group(),
                            fork_gate,
                            runtime_observation: self.runtime_observation.clone(),
                        };
                        self.publish_installation(installation);
                        self.policy_installed = true;
                    }
                    self.standing = ResidentStanding::Tools(awaiting);
                    return Ok(KernelStep::Continue(()));
                }
                ResidentActorBoundary::AgentSession(session) => {
                    if let InteractivePark::Cancelled(request) =
                        self.park_interactive(kernel, context, session).await?
                    {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                            "request {request:?} was cancelled outside a mailbox handler"
                        )));
                    }
                    return Ok(KernelStep::Continue(()));
                }
                ResidentActorBoundary::AgentAttachment(attachment) => {
                    if self.policy_installed {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "actor installed its Codex application more than once".into(),
                        ));
                    }
                    self.install_interactive_policy(
                        kernel,
                        context,
                        attachment.initial_user_message,
                    )?;
                    outcome = self
                        .environment
                        .runner
                        .resume_unit(context.clone(), attachment.continuation)
                        .await?;
                }
                boundary => {
                    outcome = self
                        .resolve_effect(kernel, context, ancestry, boundary)
                        .await?;
                }
            }
        }
    }

    async fn park_interactive(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        session: crate::ResidentInteractiveSession,
    ) -> Result<InteractivePark, ResidentActorWorkbenchError> {
        let (request, hole, input) = session.into_parts();
        let cancellation = self
            .environment
            .requests
            .present(context.actor, request.request)
            .map_err(|error| {
                ResidentActorWorkbenchError::ActorProtocol(format!(
                    "request presentation was rejected: {error:?}"
                ))
            })?;
        if cancellation.is_some() {
            drop(hole);
            drop(input);
            return Ok(InteractivePark::Cancelled(request.request));
        }
        let request_message = request.initial_user_message.clone();
        let already_installed = self.policy_installed;
        let workbench = self.environment.runner.workbench(
            request.response.clone(),
            request.request,
            request.output_modules.clone(),
        );
        workbench
            .mount_named_input(
                context.clone(),
                "sessionInput",
                request.input_type.clone(),
                input,
            )
            .await?;
        self.install_interactive_policy(kernel, context, request.initial_user_message.clone())?;
        self.standing =
            ResidentStanding::Interactive(crate::interactive_session::ResidentInteractiveAwait {
                request,
                hole,
            });
        if already_installed {
            let activation = crate::ResidentActivation::mounted(
                context.actor,
                self.next_activation_sequence,
                match &self.standing {
                    ResidentStanding::Interactive(awaiting) => awaiting.request.request,
                    _ => unreachable!(),
                },
                match &self.standing {
                    ResidentStanding::Interactive(awaiting) => awaiting.request.input_type.clone(),
                    _ => unreachable!(),
                },
                request_message.as_deref(),
            );
            self.next_activation_sequence += 1;
            let _ = self
                .environment
                .deployments
                .send(LocalResidentDeployment::SessionReady { activation });
        }
        Ok(InteractivePark::Parked)
    }

    fn install_interactive_policy(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        initial_user_message: Option<String>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        if self.policy_installed {
            return Ok(());
        }
        let actor = kernel.resolve(context.actor).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "local actor was absent from its routing directory".into(),
            )
        })?;
        let policy: Arc<dyn ResidentToolEndpoint> =
            Arc::new(crate::ResidentInteractivePolicy::local(actor.clone()));
        let fork_gate = self
            .descriptor
            .fork_group()
            .map(|group| self.environment.fork_groups.gate(group, context.actor))
            .transpose()
            .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
        self.publish_installation(LocalResidentInstallation {
            actor,
            label: self.descriptor.label().to_owned(),
            policy,
            initial_user_message,
            launch_worktrees: self.launch_worktrees.clone(),
            effective_role: self.descriptor.effective_role().clone(),
            supervisor_parent: self.descriptor.supervisor_parent(),
            context_parent: self.descriptor.context_parent(),
            fork_group: self.descriptor.fork_group(),
            fork_gate,
            runtime_observation: self.runtime_observation.clone(),
        });
        self.policy_installed = true;
        for notice in self.deferred_child_failures.drain(..) {
            let _ = self
                .environment
                .deployments
                .send(LocalResidentDeployment::ChildExited { notice });
        }
        Ok(())
    }

    async fn initialize(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        boot: ResidentBoot,
    ) -> Result<KernelStep<()>, ResidentActorWorkbenchError> {
        let outcome = match boot {
            ResidentBoot::Prepared(outcome) => *outcome,
            ResidentBoot::Entry(entry) => {
                let mut outcome = self
                    .environment
                    .runner
                    .run_rooted_entry(context.clone(), entry, context.placement.resource_scope)
                    .await?;
                loop {
                    match self
                        .environment
                        .runner
                        .capture_startup_step(
                            context.clone(),
                            outcome,
                            context.placement.resource_scope,
                        )
                        .await?
                    {
                        ResidentActorStartupStep::InstallShutdown(shutdown) => {
                            if self.shutdown_hook.is_some() {
                                return Err(ResidentActorWorkbenchError::ActorProtocol(
                                    "actor installed its shutdown hook more than once".into(),
                                ));
                            }
                            let (continuation, hook) = shutdown.into_parts();
                            self.shutdown_hook = Some(hook);
                            outcome = self
                                .environment
                                .runner
                                .resume_unit(context.clone(), continuation)
                                .await?;
                        }
                        ResidentActorStartupStep::Attach(attachment) => {
                            if self.policy_installed {
                                return Err(ResidentActorWorkbenchError::ActorProtocol(
                                    "actor installed its Codex application more than once".into(),
                                ));
                            }
                            self.install_interactive_policy(
                                kernel,
                                context,
                                attachment.initial_user_message,
                            )?;
                            outcome = self
                                .environment
                                .runner
                                .resume_unit(context.clone(), attachment.continuation)
                                .await?;
                        }
                        ResidentActorStartupStep::Ready(readiness) => {
                            break self
                                .environment
                                .runner
                                .resume_readiness(context.clone(), readiness)
                                .await?;
                        }
                    }
                }
            }
        };
        self.stabilize_program(
            kernel,
            context,
            &crate::CallAncestry::begin(context.actor),
            outcome,
        )
        .await
    }

    async fn run_receiver(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        caller: Option<ActorRef>,
        ancestry: &crate::CallAncestry,
        request: MailboxValue,
    ) -> Result<(Option<MailboxValue>, KernelStep<()>), ResidentActorWorkbenchError> {
        let receiver = match std::mem::replace(&mut self.standing, ResidentStanding::Boot) {
            ResidentStanding::Receiving(receiver) => receiver,
            standing => {
                self.standing = standing;
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "actor has no installed mailbox receiver".into(),
                ));
            }
        };
        if request.session() != context.placement.session {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "mailbox value crossed a resident machine boundary".into(),
            ));
        }
        let request = self
            .environment
            .runner
            .rehome_mailbox_value(context.clone(), request, context.placement.resource_scope)
            .await?;
        let InstalledReceiver {
            site,
            continuation: receiver_continuation,
            handler,
        } = receiver;
        let handler_realm = RealmId::fresh();
        let outcome = self
            .environment
            .runner
            .run_mailbox_handler(
                context.clone(),
                handler,
                request.into_custody(),
                handler_realm,
            )
            .await?;
        if caller.is_none()
            && self
                .environment
                .runner
                .kernel_boundary(context.clone(), &outcome)
                .await?
                .is_none()
        {
            let step = self
                .advance_cast_handler(
                    kernel,
                    context,
                    ancestry,
                    SuspendedCast {
                        site,
                        receiver_continuation,
                        handler_realm,
                    },
                    outcome,
                )
                .await?;
            return Ok((None, step));
        }
        self.finish_receiver(
            kernel,
            context,
            ReceiverSettlement {
                caller,
                ancestry,
                suspended: SuspendedCast {
                    site,
                    receiver_continuation,
                    handler_realm,
                },
                outcome,
            },
        )
        .await
    }

    async fn finish_receiver(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        settlement: ReceiverSettlement<'_>,
    ) -> Result<(Option<MailboxValue>, KernelStep<()>), ResidentActorWorkbenchError> {
        let ReceiverSettlement {
            caller,
            ancestry,
            suspended:
                SuspendedCast {
                    site,
                    receiver_continuation,
                    handler_realm,
                },
            outcome,
        } = settlement;
        let reply = self
            .environment
            .runner
            .capture_kernel_value(
                context.clone(),
                outcome,
                ResidentKernelBoundary::Reply,
                site,
                handler_realm,
                context.placement.resource_scope,
            )
            .await?;
        let reply_continuation = reply.continuation;
        let reply = if let Some(caller) = caller {
            let caller_context = kernel.session_context(caller).ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    KernelCallFailure::TargetUnavailable(caller).to_string(),
                )
            })?;
            let value = MailboxValue::new(context.placement.session, reply.value);
            Some(
                self.environment
                    .runner
                    .rehome_mailbox_value(
                        context.clone(),
                        value,
                        caller_context.placement.resource_scope,
                    )
                    .await?,
            )
        } else {
            drop(reply.value);
            None
        };
        let outcome = self
            .environment
            .runner
            .resume_unit(context.clone(), reply_continuation)
            .await?;
        let next = self
            .environment
            .runner
            .capture_kernel_value(
                context.clone(),
                outcome,
                ResidentKernelBoundary::Continue,
                site,
                handler_realm,
                context.placement.resource_scope,
            )
            .await?;
        let handler_done = self
            .environment
            .runner
            .resume_unit(context.clone(), next.continuation)
            .await?;
        if !matches!(handler_done, ResidentOutcome::Completed { .. }) {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "mailbox handler continued after its private settlement protocol".into(),
            ));
        }
        self.environment
            .runner
            .close_realm(context.clone(), handler_realm)
            .await?;
        let program = self
            .environment
            .runner
            .resume_live(context.clone(), receiver_continuation, next.value)
            .await?;
        let step = self
            .stabilize_program(kernel, context, ancestry, program)
            .await?;
        Ok((reply, step))
    }

    async fn advance_cast_handler(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        ancestry: &crate::CallAncestry,
        suspended: SuspendedCast,
        mut outcome: ResidentOutcome,
    ) -> Result<KernelStep<()>, ResidentActorWorkbenchError> {
        loop {
            if self
                .environment
                .runner
                .kernel_boundary(context.clone(), &outcome)
                .await?
                .is_some()
            {
                let (_, step) = self
                    .finish_receiver(
                        kernel,
                        context,
                        ReceiverSettlement {
                            caller: None,
                            ancestry,
                            suspended,
                            outcome,
                        },
                    )
                    .await?;
                return Ok(step);
            }
            let boundary = self
                .environment
                .runner
                .capture_boundary(context.clone(), outcome, suspended.handler_realm)
                .await?;
            match boundary {
                ResidentActorBoundary::AgentSession(session) => {
                    let parked = self.park_interactive(kernel, context, session).await?;
                    if let InteractivePark::Cancelled(request) = parked {
                        self.environment
                            .requests
                            .begin_cancellation_acknowledgement(context.actor, request)
                            .map_err(|error| {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "queued cancellation acknowledgement failed: {error:?}"
                                ))
                            })?;
                        let outcome = self
                            .environment
                            .runner
                            .abandon_cast_handler(
                                context.clone(),
                                suspended.receiver_continuation,
                                suspended.handler_realm,
                            )
                            .await;
                        match outcome {
                            Ok(outcome) => {
                                let notifications = self
                                    .environment
                                    .requests
                                    .finish_cancellation_acknowledgement(request);
                                self.publish_watch_notifications(notifications);
                                return self
                                    .stabilize_program(kernel, context, ancestry, outcome)
                                    .await;
                            }
                            Err(error) => {
                                self.environment
                                    .requests
                                    .rollback_cancellation_acknowledgement(request);
                                return Err(error);
                            }
                        }
                    }
                    if self.suspended_cast.replace(suspended).is_some() {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "actor parked a second mailbox handler before resuming the first"
                                .into(),
                        ));
                    }
                    return Ok(KernelStep::Continue(()));
                }
                boundary => {
                    outcome = self
                        .resolve_effect(kernel, context, ancestry, boundary)
                        .await?;
                }
            }
        }
    }

    async fn settle_fragment_effects(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        workbench: &crate::ResidentActorWorkbench<H, O>,
        mut fragment: ResidentWorkbenchFragment,
        mut outcome: ResidentOutcome,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        loop {
            match workbench
                .settle_item(context.clone(), fragment, outcome)
                .await?
            {
                ResidentWorkbenchStep::Running {
                    fragment: next_fragment,
                    outcome: next,
                } => {
                    let boundary = self
                        .environment
                        .runner
                        .capture_boundary(context.clone(), *next, context.placement.resource_scope)
                        .await?;
                    if self.environment.fork_groups.has_ready(context.actor) {
                        self.abort_unpublished_groups(
                            kernel,
                            context.actor,
                            "effectful work followed a committed unfold",
                        )
                        .await;
                        return Ok(ResidentWorkbenchStep::Rejected(
                            "unfold must commit the final effect boundary; no effect may follow its admission commit"
                                .into(),
                        ));
                    }
                    match boundary {
                        ResidentActorBoundary::ReplyAttempt(attempt) => match self
                            .environment
                            .requests
                            .begin_reply(context.actor, attempt.request)
                        {
                            Ok(()) => {
                                return Ok(ResidentWorkbenchStep::Replied {
                                    request: attempt.request,
                                    result: attempt.result,
                                });
                            }
                            Err(error) if attempt.recoverable => {
                                drop(attempt.result);
                                outcome = self
                                    .environment
                                    .runner
                                    .resume_reply_rejection(
                                        context.clone(),
                                        attempt.continuation,
                                        error,
                                    )
                                    .await?;
                                fragment = next_fragment;
                                continue;
                            }
                            Err(error) => {
                                drop(attempt.result);
                                return Ok(ResidentWorkbenchStep::Rejected(format!(
                                    "reply rejected: {error:?}"
                                )));
                            }
                        },
                        ResidentActorBoundary::CancellationAcknowledgement(acknowledgement) => {
                            match self
                                .environment
                                .requests
                                .begin_cancellation_acknowledgement(
                                    context.actor,
                                    acknowledgement.request,
                                ) {
                                Ok(_) => {
                                    return Ok(ResidentWorkbenchStep::CancellationAcknowledged {
                                        request: acknowledgement.request,
                                    });
                                }
                                Err(error) if acknowledgement.recoverable => {
                                    outcome = self
                                        .environment
                                        .runner
                                        .resume_reply_rejection(
                                            context.clone(),
                                            acknowledgement.continuation,
                                            error,
                                        )
                                        .await?;
                                    fragment = next_fragment;
                                    continue;
                                }
                                Err(error) => {
                                    return Ok(ResidentWorkbenchStep::Rejected(format!(
                                        "cancellation acknowledgement rejected: {error:?}"
                                    )));
                                }
                            }
                        }
                        boundary => {
                            outcome = self
                                .resolve_effect(
                                    kernel,
                                    context,
                                    &crate::CallAncestry::begin(context.actor),
                                    boundary,
                                )
                                .await?;
                        }
                    }
                    fragment = next_fragment;
                }
                settled => return Ok(settled),
            }
        }
    }

    async fn execute_workbench(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        request: WorkbenchRequest,
    ) -> Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure> {
        let workbench = match &self.standing {
            ResidentStanding::Interactive(awaiting) => self.environment.runner.workbench(
                awaiting.request.response.clone(),
                awaiting.request.request,
                awaiting.request.output_modules.clone(),
            ),
            ResidentStanding::Receiving(_) if self.policy_installed => {
                self.environment.runner.application_workbench()
            }
            _ => {
                return Err(workbench_failure(
                    &[],
                    0,
                    request.items.len(),
                    ResidentActorWorkbenchError::ActorProtocol(
                        "actor application has no active Haskell workbench".into(),
                    ),
                ));
            }
        }
        .with_json_input(
            request
                .input
                .as_ref()
                .map(tidepool_runtime::session::normalize_workbench_input),
        );
        let mut receipts = Vec::new();
        let mut index = 0;
        while index < request.items.len() {
            if request.items[index].trim() == ":status" {
                receipts.push(WorkbenchItemReceipt {
                    index,
                    status: WorkbenchItemStatus::Committed,
                    output: self.status_text(kernel, context.actor),
                    warnings: Vec::new(),
                });
                index += 1;
                continue;
            }
            let inspection =
                workbench.inspection_query(&request.items[index], request.input_kind(index));
            if let Err(output) = &inspection {
                if request.item_is_observational(index) {
                    receipts.push(WorkbenchItemReceipt {
                        index,
                        status: WorkbenchItemStatus::Diagnostic,
                        output: output.clone(),
                        warnings: Vec::new(),
                    });
                    index += 1;
                    continue;
                }
            }
            if let Ok(Some(first)) = inspection {
                let mut queries = vec![first];
                while index + queries.len() < request.items.len() {
                    let candidate = index + queries.len();
                    match workbench
                        .inspection_query(&request.items[candidate], request.input_kind(candidate))
                    {
                        Ok(Some(query)) => queries.push(query),
                        Ok(None) | Err(_) => break,
                    }
                }
                let batch_len = queries.len();
                let outputs = workbench
                    .inspect_items(context.clone(), queries)
                    .await
                    .map_err(|source| {
                        workbench_failure(&receipts, index, request.items.len(), source)
                    })?;
                for (offset, output) in outputs.into_iter().enumerate() {
                    let receipt_index = index + offset;
                    match output {
                        Ok(output) => receipts.push(WorkbenchItemReceipt {
                            index: receipt_index,
                            status: WorkbenchItemStatus::Committed,
                            output,
                            warnings: Vec::new(),
                        }),
                        Err(output) => {
                            receipts.push(WorkbenchItemReceipt {
                                index: receipt_index,
                                status: WorkbenchItemStatus::Diagnostic,
                                output,
                                warnings: Vec::new(),
                            });
                        }
                    }
                }
                index += batch_len;
                continue;
            }

            let source = request.items[index].clone();
            let block = ParsedBlock {
                ordinal: index + 1,
                total: request.items.len(),
                source,
            };
            let mut step = match workbench
                .begin_item(context.clone(), block, request.input_kind(index))
                .await
            {
                Ok(step) => step,
                Err(source) => {
                    self.abort_unpublished_groups(
                        kernel,
                        context.actor,
                        "Haskell workbench failed during unfold admission",
                    )
                    .await;
                    return Err(workbench_failure(
                        &receipts,
                        index,
                        request.items.len(),
                        source,
                    ));
                }
            };
            if let ResidentWorkbenchStep::Running { fragment, outcome } = step {
                step = match self
                    .settle_fragment_effects(kernel, context, &workbench, fragment, *outcome)
                    .await
                {
                    Ok(step) => step,
                    Err(source) => {
                        self.abort_unpublished_groups(
                            kernel,
                            context.actor,
                            "Haskell workbench failed during unfold admission",
                        )
                        .await;
                        return Err(workbench_failure(
                            &receipts,
                            index,
                            request.items.len(),
                            source,
                        ));
                    }
                };
            }
            match step {
                ResidentWorkbenchStep::Committed { output, warnings } => {
                    if self.environment.fork_groups.has_ready(context.actor) {
                        if index + 1 != request.items.len() {
                            self.abort_unpublished_groups(
                                kernel,
                                context.actor,
                                "unfold was not the final Haskell input unit",
                            )
                            .await;
                            receipts.push(WorkbenchItemReceipt {
                                index,
                                status: WorkbenchItemStatus::Rejected,
                                output: "unfold must be the final executable input unit in its hosted Haskell call".into(),
                                warnings: Vec::new(),
                            });
                            return Ok(KernelStep::Continue(workbench_response(
                                WorkbenchRunStatus::Rejected,
                                receipts,
                                index,
                                request.items.len(),
                            )));
                        }
                        self.environment
                            .fork_groups
                            .publish_ready(context.actor)
                            .map_err(|source| {
                                workbench_failure(
                                    &receipts,
                                    index,
                                    request.items.len(),
                                    ResidentActorWorkbenchError::ActorProtocol(source.to_string()),
                                )
                            })?;
                    } else if self.environment.fork_groups.has_unpublished(context.actor) {
                        self.abort_unpublished_groups(
                            kernel,
                            context.actor,
                            "Haskell input ended before unfold admission committed",
                        )
                        .await;
                        receipts.push(WorkbenchItemReceipt {
                            index,
                            status: WorkbenchItemStatus::Rejected,
                            output: "unfold admission ended without committing every fork group"
                                .into(),
                            warnings: Vec::new(),
                        });
                        return Ok(KernelStep::Continue(workbench_response(
                            WorkbenchRunStatus::Rejected,
                            receipts,
                            index,
                            request.items.len(),
                        )));
                    }
                    receipts.push(WorkbenchItemReceipt {
                        index,
                        status: WorkbenchItemStatus::Committed,
                        output,
                        warnings,
                    });
                }
                ResidentWorkbenchStep::Rejected(output) => {
                    if request.item_is_observational(index) {
                        receipts.push(WorkbenchItemReceipt {
                            index,
                            status: WorkbenchItemStatus::Diagnostic,
                            output,
                            warnings: Vec::new(),
                        });
                        index += 1;
                        continue;
                    }
                    self.abort_unpublished_groups(
                        kernel,
                        context.actor,
                        "Haskell input rejected during unfold admission",
                    )
                    .await;
                    receipts.push(WorkbenchItemReceipt {
                        index,
                        status: WorkbenchItemStatus::Rejected,
                        output,
                        warnings: Vec::new(),
                    });
                    return Ok(KernelStep::Continue(workbench_response(
                        WorkbenchRunStatus::Rejected,
                        receipts,
                        index,
                        request.items.len(),
                    )));
                }
                ResidentWorkbenchStep::Replied {
                    request: request_id,
                    result,
                } => {
                    if self.environment.fork_groups.has_unpublished(context.actor) {
                        self.abort_unpublished_groups(
                            kernel,
                            context.actor,
                            "request reply interrupted unfold admission",
                        )
                        .await;
                        return Err(workbench_failure(
                            &receipts,
                            index,
                            request.items.len(),
                            ResidentActorWorkbenchError::ActorProtocol(
                                "an actor cannot reply while an unfold group is unpublished".into(),
                            ),
                        ));
                    }
                    let awaiting =
                        match std::mem::replace(&mut self.standing, ResidentStanding::Boot) {
                            ResidentStanding::Interactive(awaiting)
                                if awaiting.request.request == request_id =>
                            {
                                awaiting
                            }
                            ResidentStanding::Interactive(awaiting) => {
                                self.standing = ResidentStanding::Interactive(awaiting);
                                let notifications =
                                    self.environment.requests.fail_reply_settlement(
                                        request_id,
                                        "reply did not match the active request",
                                    );
                                self.publish_watch_notifications(notifications);
                                return Err(workbench_failure(
                                    &receipts,
                                    index,
                                    request.items.len(),
                                    ResidentActorWorkbenchError::ActorProtocol(
                                        "reply did not match the active request".into(),
                                    ),
                                ));
                            }
                            standing => {
                                self.standing = standing;
                                let notifications =
                                    self.environment.requests.fail_reply_settlement(
                                        request_id,
                                        "reply lost its request continuation",
                                    );
                                self.publish_watch_notifications(notifications);
                                return Err(workbench_failure(
                                    &receipts,
                                    index,
                                    request.items.len(),
                                    ResidentActorWorkbenchError::ActorProtocol(
                                        "reply lost its request continuation".into(),
                                    ),
                                ));
                            }
                        };
                    let outcome = match workbench
                        .resume_request(context.clone(), awaiting.hole.clone(), result)
                        .await
                    {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            self.standing = ResidentStanding::Interactive(awaiting);
                            let notifications = self.environment.requests.fail_reply_settlement(
                                request_id,
                                format!("request continuation failed: {error}"),
                            );
                            self.publish_watch_notifications(notifications);
                            return Err(workbench_failure(
                                &receipts,
                                index,
                                request.items.len(),
                                error,
                            ));
                        }
                    };
                    if self.pending_program.is_some() || self.pending_reply.is_some() {
                        let notifications = self.environment.requests.fail_reply_settlement(
                            request_id,
                            "actor settled a second reply before resuming the first",
                        );
                        self.publish_watch_notifications(notifications);
                        return Err(workbench_failure(
                            &receipts,
                            index,
                            request.items.len(),
                            ResidentActorWorkbenchError::ActorProtocol(
                                "actor settled a second reply before resuming the first".into(),
                            ),
                        ));
                    }
                    self.pending_program = Some(outcome);
                    self.pending_reply = Some(request_id);
                    return Ok(KernelStep::ContinueLater(workbench_response(
                        WorkbenchRunStatus::Replied,
                        receipts,
                        index + 1,
                        request.items.len(),
                    )));
                }
                ResidentWorkbenchStep::CancellationAcknowledged {
                    request: request_id,
                } => {
                    if self.environment.fork_groups.has_unpublished(context.actor) {
                        self.abort_unpublished_groups(
                            kernel,
                            context.actor,
                            "request cancellation interrupted unfold admission",
                        )
                        .await;
                        self.environment
                            .requests
                            .rollback_cancellation_acknowledgement(request_id);
                        return Err(workbench_failure(
                            &receipts,
                            index,
                            request.items.len(),
                            ResidentActorWorkbenchError::ActorProtocol(
                                "an actor cannot acknowledge cancellation while an unfold group is unpublished"
                                    .into(),
                            ),
                        ));
                    }
                    if self.pending_program.is_some()
                        || self.pending_reply.is_some()
                        || self.pending_cancellation.is_some()
                    {
                        self.environment
                            .requests
                            .rollback_cancellation_acknowledgement(request_id);
                        return Err(workbench_failure(
                            &receipts,
                            index,
                            request.items.len(),
                            ResidentActorWorkbenchError::ActorProtocol(
                                "actor settled a second request before resuming the first".into(),
                            ),
                        ));
                    }
                    let awaiting =
                        match std::mem::replace(&mut self.standing, ResidentStanding::Boot) {
                            ResidentStanding::Interactive(awaiting)
                                if awaiting.request.request == request_id =>
                            {
                                awaiting
                            }
                            ResidentStanding::Interactive(awaiting) => {
                                self.standing = ResidentStanding::Interactive(awaiting);
                                self.environment
                                    .requests
                                    .rollback_cancellation_acknowledgement(request_id);
                                return Err(workbench_failure(
                                    &receipts,
                                    index,
                                    request.items.len(),
                                    ResidentActorWorkbenchError::ActorProtocol(
                                        "cancellation did not match the active request".into(),
                                    ),
                                ));
                            }
                            standing => {
                                self.standing = standing;
                                self.environment
                                    .requests
                                    .rollback_cancellation_acknowledgement(request_id);
                                return Err(workbench_failure(
                                    &receipts,
                                    index,
                                    request.items.len(),
                                    ResidentActorWorkbenchError::ActorProtocol(
                                        "cancellation lost its active request".into(),
                                    ),
                                ));
                            }
                        };
                    let Some(suspended) = self.suspended_cast.take() else {
                        self.standing = ResidentStanding::Interactive(awaiting);
                        self.environment
                            .requests
                            .rollback_cancellation_acknowledgement(request_id);
                        return Err(workbench_failure(
                            &receipts,
                            index,
                            request.items.len(),
                            ResidentActorWorkbenchError::ActorProtocol(
                                "cancellation lost its mailbox continuation".into(),
                            ),
                        ));
                    };
                    let outcome = match self
                        .environment
                        .runner
                        .abandon_cast_handler(
                            context.clone(),
                            suspended.receiver_continuation.clone(),
                            suspended.handler_realm,
                        )
                        .await
                    {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            self.suspended_cast = Some(suspended);
                            self.standing = ResidentStanding::Interactive(awaiting);
                            self.environment
                                .requests
                                .rollback_cancellation_acknowledgement(request_id);
                            return Err(workbench_failure(
                                &receipts,
                                index,
                                request.items.len(),
                                error,
                            ));
                        }
                    };
                    drop(awaiting);
                    self.pending_program = Some(outcome);
                    self.pending_cancellation = Some(request_id);
                    return Ok(KernelStep::ContinueLater(workbench_response(
                        WorkbenchRunStatus::RequestCancelled,
                        receipts,
                        index + 1,
                        request.items.len(),
                    )));
                }
                ResidentWorkbenchStep::Running { .. } => {
                    unreachable!("running workbench steps are settled above")
                }
            }
            index += 1;
        }
        Ok(KernelStep::Continue(workbench_response(
            WorkbenchRunStatus::Committed,
            receipts,
            request.items.len(),
            request.items.len(),
        )))
    }

    async fn abort_unpublished_groups(
        &self,
        kernel: &KernelContext,
        owner: ActorRef,
        summary: &str,
    ) {
        for child in self.environment.fork_groups.abort_unpublished(owner) {
            if let Some(child) = kernel.resolve(child) {
                let _ = child
                    .shutdown(ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: summary.into(),
                    })
                    .await;
            }
        }
    }
}

impl<H, O> KernelBehavior for ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    fn accepts_mailbox(&self) -> bool {
        matches!(self.standing, ResidentStanding::Receiving(_))
    }

    fn start<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            kernel.install_session_context(context.clone())?;
            self.environment.actors.lock().insert(
                context.actor,
                ResidentActorRecord {
                    descriptor: self.descriptor.clone(),
                    bound_worktree: self.launch_worktrees.first().cloned(),
                    terminal: None,
                    runtime_observation: self.runtime_observation.clone(),
                },
            );
            let boot = self.boot.take().ok_or_else(|| KernelBehaviorError {
                detail: "resident actor boot was consumed twice".into(),
            })?;
            self.initialize(kernel, &context, boot)
                .await
                .map_err(Self::failure)
        })
    }

    fn cast<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        _sender: ActorRef,
        request: MailboxValue,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let (_, step) = self
                .run_receiver(
                    kernel,
                    &context,
                    None,
                    &crate::CallAncestry::begin(context.actor),
                    request,
                )
                .await
                .map_err(Self::failure)?;
            Ok(step)
        })
    }

    fn call<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        caller: ActorRef,
        ancestry: crate::CallAncestry,
        request: MailboxValue,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<MailboxValue>, KernelBehaviorError>>
    {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let (reply, step) = self
                .run_receiver(kernel, &context, Some(caller), &ancestry, request)
                .await
                .map_err(Self::failure)?;
            let reply = reply.ok_or_else(|| KernelBehaviorError {
                detail: "synchronous mailbox handler produced no reply".into(),
            })?;
            Ok(match step {
                KernelStep::Continue(()) => KernelStep::Continue(reply),
                KernelStep::ContinueLater(()) => KernelStep::ContinueLater(reply),
                KernelStep::Stop { terminal, .. } => KernelStep::Stop {
                    output: reply,
                    terminal,
                },
            })
        })
    }

    fn tool<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        invocation: tidepool_tool::ToolInvocation,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<KernelStep<serde_json::Value>, KernelInvocationFailure>,
    > {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let awaiting = match std::mem::replace(&mut self.standing, ResidentStanding::Boot) {
                ResidentStanding::Tools(awaiting) => awaiting,
                standing => {
                    self.standing = standing;
                    return Err(KernelInvocationFailure::Rejected {
                        actor: context.actor,
                        detail: "actor has no installed tool policy".into(),
                    });
                }
            };
            let tidepool_tool::ToolArguments::Structured(arguments) = invocation.arguments else {
                self.standing = ResidentStanding::Tools(awaiting);
                return Err(KernelInvocationFailure::Rejected {
                    actor: context.actor,
                    detail: "actor function tool received raw arguments".into(),
                });
            };
            let mut outcome = self
                .environment
                .runner
                .resume_tool_invocation(
                    context.clone(),
                    awaiting.continuation,
                    invocation.name,
                    arguments,
                )
                .await
                .map_err(|error| Self::invocation_failure(context.actor, error))?;
            let mut result = None;
            loop {
                let boundary = self
                    .environment
                    .runner
                    .capture_boundary(context.clone(), outcome, context.placement.resource_scope)
                    .await
                    .map_err(|error| Self::invocation_failure(context.actor, error))?;
                match boundary {
                    ResidentActorBoundary::ToolReply(reply) => {
                        if result.replace(reply.result).is_some() {
                            return Err(KernelInvocationFailure::Failed {
                                actor: context.actor,
                                detail: "actor tool invocation replied more than once".into(),
                            });
                        }
                        outcome = self
                            .environment
                            .runner
                            .resume_unit(context.clone(), reply.continuation)
                            .await
                            .map_err(|error| Self::invocation_failure(context.actor, error))?;
                    }
                    ResidentActorBoundary::ToolAwait(next) => {
                        let result = result.ok_or_else(|| KernelInvocationFailure::Failed {
                            actor: context.actor,
                            detail: "actor awaited another tool invocation without replying".into(),
                        })?;
                        self.standing = ResidentStanding::Tools(next);
                        return Ok(KernelStep::Continue(result));
                    }
                    ResidentActorBoundary::Completed => {
                        let result = result.ok_or_else(|| KernelInvocationFailure::Failed {
                            actor: context.actor,
                            detail: "actor completed a tool invocation without replying".into(),
                        })?;
                        self.standing = ResidentStanding::Terminal;
                        return Ok(KernelStep::Stop {
                            output: result,
                            terminal: completed_terminal(),
                        });
                    }
                    boundary => {
                        outcome = self
                            .resolve_effect(
                                kernel,
                                &context,
                                &crate::CallAncestry::begin(context.actor),
                                boundary,
                            )
                            .await
                            .map_err(|error| Self::invocation_failure(context.actor, error))?;
                    }
                }
            }
        })
    }

    fn workbench<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        request: WorkbenchRequest,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>,
    > {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            if !self.policy_installed
                || !matches!(
                    self.standing,
                    ResidentStanding::Interactive(_) | ResidentStanding::Receiving(_)
                )
            {
                return Err(KernelInvocationFailure::Rejected {
                    actor: context.actor,
                    detail: "actor has no active Haskell application workbench".into(),
                });
            }
            let result = self.execute_workbench(kernel, &context, request).await;
            let rejected = match &result {
                Err(_) => true,
                Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response)) => {
                    response.status == WorkbenchRunStatus::Rejected
                }
                Ok(KernelStep::Stop { output, .. }) => {
                    output.status == WorkbenchRunStatus::Rejected
                }
            };
            if rejected {
                let aborted = self.environment.requests.abort_unsubmitted(context.actor);
                if !aborted.is_empty() {
                    tracing::debug!(actor = ?context.actor, requests = ?aborted, "aborted unpublished request reservations after rejected workbench input");
                }
            }
            result.map_err(|failure| {
                KernelInvocationFailure::Workbench(crate::KernelWorkbenchFailure {
                    actor: context.actor,
                    completed: failure.completed,
                    failed_index: failure.failed_index,
                    total: failure.total,
                    detail: failure.source.to_string(),
                })
            })
        })
    }

    fn resume<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let outcome = self
                .pending_program
                .take()
                .ok_or_else(|| KernelBehaviorError {
                    detail: "resident actor resumed without a pending Haskell action".into(),
                })?;
            let step = if let Some(suspended) = self.suspended_cast.take() {
                self.advance_cast_handler(
                    kernel,
                    &context,
                    &crate::CallAncestry::begin(context.actor),
                    suspended,
                    outcome,
                )
                .await
                .map_err(Self::failure)
            } else {
                self.stabilize_program(
                    kernel,
                    &context,
                    &crate::CallAncestry::begin(context.actor),
                    outcome,
                )
                .await
                .map_err(Self::failure)
            };
            match step {
                Ok(step) => {
                    if let Some(request) = self.pending_reply.take() {
                        let notifications = self.environment.requests.finish_reply(request);
                        self.publish_watch_notifications(notifications);
                    }
                    if let Some(request) = self.pending_cancellation.take() {
                        let notifications = self
                            .environment
                            .requests
                            .finish_cancellation_acknowledgement(request);
                        self.publish_watch_notifications(notifications);
                    }
                    Ok(step)
                }
                Err(error) => {
                    if let Some(request) = self.pending_reply.take() {
                        let notifications = self.environment.requests.fail_reply_settlement(
                            request,
                            format!("reply continuation failed after acceptance: {error}"),
                        );
                        self.publish_watch_notifications(notifications);
                    }
                    if let Some(request) = self.pending_cancellation.take() {
                        self.environment
                            .requests
                            .rollback_cancellation_acknowledgement(request);
                    }
                    Err(error)
                }
            }
        })
    }

    fn external_application_failed(
        &mut self,
        _context: &KernelContext,
        _failure: ExternalApplicationFailure,
    ) -> futures_util::future::BoxFuture<'_, ExternalFailureDisposition> {
        Box::pin(async { ExternalFailureDisposition::Applied })
    }

    fn shutdown<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> futures_util::future::BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let notifications = self
                .environment
                .requests
                .actor_stopped(context.actor, terminal);
            self.publish_watch_notifications(notifications);
            if let Some(hook) = self.shutdown_hook.take() {
                self.environment
                    .runner
                    .run_shutdown(
                        context.clone(),
                        hook,
                        context.placement.resource_scope,
                        terminal.kind,
                        std::time::Duration::from_secs(30),
                    )
                    .await
                    .map_err(Self::failure)?;
            }
            self.environment
                .runner
                .close_realm(context, self.descriptor.placement().resource_scope)
                .await
                .map_err(Self::failure)?;
            self.standing = ResidentStanding::Terminal;
            Ok(())
        })
    }

    fn stopped<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> futures_util::future::BoxFuture<'a, ()> {
        Box::pin(async move {
            let notifications = self
                .environment
                .requests
                .actor_stopped(kernel.identity(), terminal);
            self.publish_watch_notifications(notifications);
            self.publish_retired(kernel.identity(), terminal.clone());
        })
    }

    fn child_exited(&mut self, notice: ChildExitNotice) -> futures_util::future::BoxFuture<'_, ()> {
        Box::pin(async move {
            let child = notice.child.identity();
            self.publish_retired(child, notice.terminal.clone());
            if self.child_exit_observations.process(child)
                || notice.terminal.kind == ActorExitKind::Completed
            {
                return;
            }
            if matches!(self.standing, ResidentStanding::Boot) {
                self.deferred_child_failures.push(notice);
            } else {
                let _ = self
                    .environment
                    .deployments
                    .send(LocalResidentDeployment::ChildExited { notice });
            }
        })
    }
}

pub async fn spawn_resident_root<H, O>(
    source: ActorWorkbenchSource,
    root: ResidentActorRoot<H, O>,
) -> Result<
    (
        LocalActorRef,
        ractor::concurrency::JoinHandle<()>,
        mpsc::UnboundedReceiver<LocalResidentDeployment>,
    ),
    ractor::SpawnErr,
>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    spawn_resident_root_with_fork_admission(source, root, None).await
}

pub async fn spawn_resident_root_with_fork_admission<H, O>(
    source: ActorWorkbenchSource,
    root: ResidentActorRoot<H, O>,
    fork_workspaces: Option<crate::fork_workspace::SharedForkWorkspaceAdmission>,
) -> Result<
    (
        LocalActorRef,
        ractor::concurrency::JoinHandle<()>,
        mpsc::UnboundedReceiver<LocalResidentDeployment>,
    ),
    ractor::SpawnErr,
>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let (descriptor, machine, outcome) = root.into_parts();
    let machines = Arc::new(ActorMachineRegistry::<H, O>::new());
    let session = descriptor.placement().session;
    debug_assert!(machines.insert_idle(session, machine).is_none());
    let runner = ResidentActorRunner::new(machines, source);
    let (deployments, receiver) = mpsc::unbounded_channel();
    let lineage = crate::ActorLineageRegistry::default();
    let environment = ResidentEnvironment {
        runner,
        deployments,
        retired: Arc::new(Mutex::new(std::collections::HashSet::new())),
        requests: Arc::new(RequestRegistry::default()),
        fork_groups: crate::ForkGroupRegistry::new(lineage),
        actors: Arc::new(Mutex::new(std::collections::HashMap::new())),
        fork_workspaces,
    };
    let behavior = ResidentKernelBehavior::prepared(descriptor, environment, outcome);
    let (actor, task) = crate::spawn_local_actor(None, behavior).await?;
    Ok((actor, task, receiver))
}

fn completed_terminal() -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "completed".into(),
    }
}

fn workbench_response(
    status: WorkbenchRunStatus,
    mut items: Vec<WorkbenchItemReceipt>,
    next_index: usize,
    total: usize,
) -> WorkbenchResponse {
    let first_not_run = match status {
        WorkbenchRunStatus::Rejected => next_index.saturating_add(1),
        WorkbenchRunStatus::Replied
        | WorkbenchRunStatus::RequestCancelled
        | WorkbenchRunStatus::Completed => next_index,
        WorkbenchRunStatus::Committed => total,
    };
    items.extend((first_not_run..total).map(|index| WorkbenchItemReceipt {
        index,
        status: WorkbenchItemStatus::NotRun,
        output: String::new(),
        warnings: Vec::new(),
    }));
    WorkbenchResponse {
        status,
        items,
        next_index,
        total,
    }
}

#[cfg(test)]
mod tests {
    use super::{workbench_response, ChildExitObservations};
    use crate::{ActorId, ActorRef, Incarnation};
    use tidepool_runtime::session::{
        WorkbenchItemReceipt, WorkbenchItemStatus, WorkbenchRunStatus,
    };

    #[test]
    fn child_exit_observation_tracks_exact_processing_order() {
        let observed = ActorRef {
            id: ActorId(7),
            incarnation: Incarnation(1),
        };
        let replacement = ActorRef {
            id: ActorId(7),
            incarnation: Incarnation(2),
        };
        let mut exits = ChildExitObservations::default();

        assert!(!exits.observe(observed));

        assert!(!exits.process(replacement));
        assert!(exits.process(observed));
        assert!(!exits.process(observed));
        assert!(exits.observe(observed));

        let processed_first = ActorRef {
            id: ActorId(8),
            incarnation: Incarnation(1),
        };
        assert!(!exits.process(processed_first));
        assert!(exits.observe(processed_first));
    }

    #[test]
    fn rejected_workbench_response_marks_the_unexecuted_suffix() {
        let response = workbench_response(
            WorkbenchRunStatus::Rejected,
            vec![WorkbenchItemReceipt {
                index: 0,
                status: WorkbenchItemStatus::Rejected,
                output: "bad effect".into(),
                warnings: Vec::new(),
            }],
            0,
            3,
        );
        assert_eq!(response.items.len(), 3);
        assert_eq!(response.items[1].status, WorkbenchItemStatus::NotRun);
        assert_eq!(response.items[2].status, WorkbenchItemStatus::NotRun);
    }
}
