//! Resident Haskell behavior owned directly by one local actor.
//!
//! Ractor serializes logical turns and owns the mailbox. The shared resident
//! machine registry owns only short-lived machine checkout. This module is
//! the single driver between those boundaries; it does not recreate registry
//! turn leases, parked-obligation maps, or a host-side scheduler.

use std::sync::Arc;

mod commands;
mod replacement;

use parking_lot::Mutex;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_runtime::session::{
    OutputSink, ParsedBlock, ResidentHole, ResidentOutcome, ResidentSession, RootCustody,
    WorkbenchExecutionId, WorkbenchItemReceipt, WorkbenchItemStatus, WorkbenchOperationDisposition,
    WorkbenchOperationId, WorkbenchOperationReceipt, WorkbenchRequest, WorkbenchResponse,
    WorkbenchRunStatus, WorkbenchTerminalTransfer,
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

    pub fn into_parts(self) -> (ActorDescriptor, ResidentSession<H, O>, ResidentOutcome) {
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
    pub worktree_custody: Option<Arc<dyn crate::ForkWorkspaceCustody>>,
    pub effective_role: crate::EffectiveRole,
    pub fork_effort: Option<crate::ForkEffort>,
    pub model: Option<String>,
    pub instructions: Option<String>,
    pub creator: Option<crate::ActorRef>,
    pub fork_boundary: Option<tidepool_runtime::session::WorkbenchForkBoundary>,
    pub supervisor_parent: Option<crate::ActorRef>,
    pub context_parent: Option<crate::ActorRef>,
    pub fork_group: Option<crate::ForkGroupId>,
    pub fork_gate: Option<crate::ForkGroupGate>,
    pub runtime_observation: crate::ActorRuntimeObservationHandle,
}

#[derive(Clone)]
pub enum LocalResidentDeployment {
    CommandBackend(Arc<crate::command_jobs::CommandBackendRequest>),
    NotificationSend(Arc<crate::NotificationSend>),
    NotificationPoll(Arc<crate::NotificationPoll>),
    PolicyInstalled(LocalResidentInstallation),
    /// A resident program opened another typed session in an already-running
    /// interactive application. The message is an ordinary User activation;
    /// the live value itself is mounted as `sessionInput` in Haskell.
    SessionReady {
        activation: crate::ResidentActivation,
    },
    RequestUpdate {
        delivery: crate::RequestUpdateDelivery,
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
    commands: crate::command_jobs::CommandJobs,
    fork_groups: crate::ForkGroupRegistry,
    actors: Arc<Mutex<std::collections::HashMap<ActorRef, ResidentActorRecord>>>,
    fork_workspaces: Option<crate::fork_workspace::SharedForkWorkspaceAdmission>,
    root_admission_closed: Arc<tokio::sync::RwLock<bool>>,
    launch_resolver: Option<crate::WorkerLaunchResolver>,
}

#[derive(Clone)]
struct ResidentActorRecord {
    forest_control: bool,
    interactive_policy_installed: bool,
    observation_roots: std::collections::HashSet<ActorRef>,
    descriptor: ActorDescriptor,
    bound_worktree: Option<String>,
    terminal: Option<ActorTerminal>,
    runtime_observation: crate::ActorRuntimeObservationHandle,
}

#[derive(tidepool_bridge_derive::ToCore)]
enum ObservationShareResult {
    #[core(module = "Tidepool.Effects.Core", name = "ObservationShared")]
    Shared,
    #[core(
        module = "Tidepool.Effects.Core",
        name = "ObservationRecipientUnavailable"
    )]
    RecipientUnavailable,
    #[core(module = "Tidepool.Effects.Core", name = "ObservationScopeUnavailable")]
    ScopeUnavailable,
    #[core(module = "Tidepool.Effects.Core", name = "ObservationUnauthorized")]
    Unauthorized,
}

fn actor_can_control(
    owner: ActorRef,
    candidate: ActorRef,
    records: &std::collections::HashMap<ActorRef, ResidentActorRecord>,
) -> bool {
    if records
        .get(&owner)
        .is_some_and(|record| record.forest_control && record.terminal.is_none())
    {
        return true;
    }
    actor_in_tree(owner, candidate, records, |descriptor| {
        descriptor.supervisor_parent().or(descriptor.creator())
    })
}

fn actor_can_observe(
    owner: ActorRef,
    candidate: ActorRef,
    records: &std::collections::HashMap<ActorRef, ResidentActorRecord>,
) -> bool {
    actor_can_control(owner, candidate, records)
        || records.get(&owner).is_some_and(|record| {
            record
                .observation_roots
                .iter()
                .any(|root| actor_in_creation_tree(*root, candidate, records))
        })
}

fn actor_in_creation_tree(
    owner: ActorRef,
    candidate: ActorRef,
    records: &std::collections::HashMap<ActorRef, ResidentActorRecord>,
) -> bool {
    actor_in_tree(owner, candidate, records, |descriptor| {
        descriptor.creator().or(descriptor.supervisor_parent())
    })
}

fn actor_in_tree(
    owner: ActorRef,
    candidate: ActorRef,
    records: &std::collections::HashMap<ActorRef, ResidentActorRecord>,
    parent: impl Fn(&ActorDescriptor) -> Option<ActorRef>,
) -> bool {
    let mut cursor = candidate;
    for _ in 0..=records.len() {
        if cursor == owner {
            return true;
        }
        let Some(parent) = records
            .get(&cursor)
            .and_then(|record| parent(&record.descriptor))
        else {
            return false;
        };
        cursor = parent;
    }
    false
}

impl<H, O> Clone for ResidentEnvironment<H, O> {
    fn clone(&self) -> Self {
        Self {
            runner: self.runner.clone(),
            deployments: self.deployments.clone(),
            retired: Arc::clone(&self.retired),
            requests: Arc::clone(&self.requests),
            commands: self.commands.clone(),
            fork_groups: self.fork_groups.clone(),
            actors: Arc::clone(&self.actors),
            fork_workspaces: self.fork_workspaces.clone(),
            root_admission_closed: self.root_admission_closed.clone(),
            launch_resolver: self.launch_resolver.clone(),
        }
    }
}

enum ResidentBoot {
    Workbench,
    Prepared(Box<ResidentOutcome>),
    Entry(RootCustody),
    Replacement(Box<replacement::PreparedSuccessor>),
}

enum ResidentStanding {
    Workbench,
    Boot,
    Receiving(InstalledReceiver),
    Tools(crate::resident_tools::ResidentToolAwait),
    Interactive(crate::interactive_session::ResidentInteractiveAwait),
    Terminal,
    Paused(PausedHandler),
}

struct PausedHandler {
    checkpoint: StateCheckpoint,
    input: RetainedActorInput,
    detail: String,
}

struct StateCheckpoint {
    site: u64,
    value: Arc<RootCustody>,
}

enum RetainedActorInput {
    Mailbox(Arc<RootCustody>),
    Source(crate::SourceDelivery),
}

#[derive(Clone, tidepool_bridge_derive::ToCore)]
enum ActorInputOrigin {
    ActorStartup,
    ActorMessageFrom((i64, i64)),
    ActorProgressFrom(i64),
    ActorSettlementFrom(i64),
    ActorLifecycleFrom((i64, i64)),
    ActorCommandFrom(String),
}

fn actor_address(actor: ActorRef) -> (i64, i64) {
    (actor.id.0 as i64, actor.incarnation.0 as i64)
}

impl std::fmt::Debug for RetainedActorInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mailbox(value) => formatter.debug_tuple("Mailbox").field(value).finish(),
            Self::Source(delivery) => formatter.debug_tuple("Source").field(delivery).finish(),
        }
    }
}

struct WorkbenchExecutionFailure {
    receipts: Vec<WorkbenchItemReceipt>,
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
        receipts: completed.to_vec(),
        failed_index,
        total,
        source,
    }
}

fn workbench_failure_after_operations(
    completed: &[WorkbenchItemReceipt],
    failed_index: usize,
    total: usize,
    source: ResidentActorWorkbenchError,
    mut operations: Vec<WorkbenchOperationReceipt>,
) -> WorkbenchExecutionFailure {
    settle_prepared_operations(&mut operations, WorkbenchOperationDisposition::Unknown);
    let mut receipts = completed.to_vec();
    if !operations.is_empty() {
        receipts.push(WorkbenchItemReceipt {
            index: failed_index,
            status: WorkbenchItemStatus::Rejected,
            output: source.to_string(),
            warnings: Vec::new(),
            installed_bindings: Vec::new(),
            operations,
            terminal_transfer: None,
        });
    }
    WorkbenchExecutionFailure {
        receipts,
        failed_index,
        total,
        source,
    }
}

fn record_workbench_operation(
    operations: &mut Vec<WorkbenchOperationReceipt>,
    execution: Option<&WorkbenchExecutionId>,
    input_unit_index: usize,
    effect_ordinal: usize,
    effect: &str,
    disposition: WorkbenchOperationDisposition,
) {
    let Some(execution) = execution else {
        return;
    };
    operations.push(WorkbenchOperationReceipt {
        id: WorkbenchOperationId {
            execution: execution.clone(),
            input_unit_index,
            effect_ordinal,
        },
        effect: effect.to_owned(),
        disposition,
    });
}

fn settle_prepared_operations(
    operations: &mut [WorkbenchOperationReceipt],
    disposition: WorkbenchOperationDisposition,
) {
    for operation in operations {
        if operation.disposition == WorkbenchOperationDisposition::Prepared {
            operation.disposition = disposition;
        }
    }
}

struct WorkbenchUnitExecution<'a> {
    execution: Option<&'a WorkbenchExecutionId>,
    input_unit_index: usize,
    total: usize,
    operations: &'a mut Vec<WorkbenchOperationReceipt>,
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
    replacement_transfer: Option<replacement::ReplacementTransfer>,
    retained_replacements: Vec<replacement::RetainedHandler>,
    descriptor: ActorDescriptor,
    environment: ResidentEnvironment<H, O>,
    boot: Option<ResidentBoot>,
    standing: ResidentStanding,
    shutdown_hook: Option<RootCustody>,
    checkpoint: Option<StateCheckpoint>,
    pending_checkpoint: Option<StateCheckpoint>,
    active_input: Option<RetainedActorInput>,
    input_origin: ActorInputOrigin,
    sources: Vec<crate::request::sources::SourceBinding>,
    source_connections: Option<crate::request::sources::ActorSourceConnections>,
    launch_worktrees: Vec<String>,
    prepared_workspace: Option<crate::PreparedForkWorkspace>,
    worktree_custody: Option<Arc<dyn crate::ForkWorkspaceCustody>>,
    policy_installed: bool,
    forest_control: bool,
    pending_program: Option<ResidentOutcome>,
    pending_reply: Option<crate::RequestId>,
    pending_cancellation: Option<crate::RequestId>,
    suspended_cast: Option<SuspendedCast>,
    child_exit_observations: ChildExitObservations,
    deferred_child_failures: Vec<ChildExitNotice>,
    next_activation_sequence: u64,
    runtime_observation: crate::ActorRuntimeObservationHandle,
    completed_workbenches: CompletedWorkbenchExecutions,
    active_route: Option<(crate::WatchId, Vec<crate::ForkGroupId>)>,
    active_fork_boundary: Option<tidepool_runtime::session::WorkbenchForkBoundary>,
}

#[derive(Clone)]
struct CompletedWorkbenchExecution {
    request: WorkbenchRequest,
    reply: crate::KernelWorkbenchReply,
}

#[derive(Default)]
struct CompletedWorkbenchExecutions(
    std::collections::HashMap<WorkbenchExecutionId, CompletedWorkbenchExecution>,
);

impl CompletedWorkbenchExecutions {
    fn lookup(
        &self,
        execution: &WorkbenchExecutionId,
        request: &WorkbenchRequest,
    ) -> Result<Option<crate::KernelWorkbenchReply>, ()> {
        let Some(completed) = self.0.get(execution) else {
            return Ok(None);
        };
        if completed.request != *request {
            return Err(());
        }
        Ok(Some(completed.reply.clone()))
    }

    fn record(
        &mut self,
        execution: WorkbenchExecutionId,
        request: WorkbenchRequest,
        reply: crate::KernelWorkbenchReply,
    ) {
        self.0
            .insert(execution, CompletedWorkbenchExecution { request, reply });
    }
}

impl<H, O> ResidentKernelBehavior<H, O> {
    fn pending_in_tool_block(&self, target: ActorRef) -> bool {
        self.active_fork_boundary.is_some() && self.environment.fork_groups.is_pending_child(target)
    }

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
        Self::with_boot(
            descriptor,
            environment,
            ResidentBoot::Prepared(Box::new(outcome)),
            Vec::new(),
        )
    }

    fn child(
        descriptor: ActorDescriptor,
        environment: ResidentEnvironment<H, O>,
        entry: RootCustody,
        launch_worktrees: Vec<String>,
    ) -> Self {
        Self::with_boot(
            descriptor,
            environment,
            ResidentBoot::Entry(entry),
            launch_worktrees,
        )
    }

    fn with_boot(
        descriptor: ActorDescriptor,
        environment: ResidentEnvironment<H, O>,
        boot: ResidentBoot,
        launch_worktrees: Vec<String>,
    ) -> Self {
        Self {
            replacement_transfer: None,
            retained_replacements: Vec::new(),
            descriptor,
            environment,
            boot: Some(boot),
            standing: ResidentStanding::Boot,
            shutdown_hook: None,
            checkpoint: None,
            pending_checkpoint: None,
            active_input: None,
            input_origin: ActorInputOrigin::ActorStartup,
            sources: Vec::new(),
            source_connections: None,
            launch_worktrees,
            prepared_workspace: None,
            worktree_custody: None,
            policy_installed: false,
            forest_control: false,
            pending_program: None,
            pending_reply: None,
            pending_cancellation: None,
            suspended_cast: None,
            child_exit_observations: ChildExitObservations::default(),
            deferred_child_failures: Vec::new(),
            next_activation_sequence: 1,
            runtime_observation: crate::ActorRuntimeObservationHandle::default(),
            completed_workbenches: CompletedWorkbenchExecutions::default(),
            active_route: None,
            active_fork_boundary: None,
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
        if let Some(record) = self
            .environment
            .actors
            .lock()
            .get_mut(&installation.actor.identity())
        {
            record.interactive_policy_installed = true;
        }
        let _ = self
            .environment
            .deployments
            .send(LocalResidentDeployment::PolicyInstalled(installation));
    }

    fn notification_supervisor(&self, mut next: Option<ActorRef>) -> Option<ActorRef> {
        let records = self.environment.actors.lock();
        let mut visited = std::collections::HashSet::new();
        while let Some(actor) = next {
            if !visited.insert(actor) {
                return None;
            }
            let record = records.get(&actor)?;
            if record.interactive_policy_installed && record.terminal.is_none() {
                return Some(actor);
            }
            next = record.descriptor.supervisor_parent();
        }
        None
    }

    fn notify_supervisor(&self, source: ActorRef, parent: Option<ActorRef>, message: String) {
        let Some(target) = self.notification_supervisor(parent) else {
            tracing::error!(actor = ?source, "actor failure has no live interactive supervisor");
            return;
        };
        let (command, admission) = crate::NotificationSend::new(source, target, message);
        // System notices have no model-owned receipt. Losing this waiter does
        // not retract durable admission or authorize another send.
        drop(admission);
        if self
            .environment
            .deployments
            .send(LocalResidentDeployment::NotificationSend(Arc::new(command)))
            .is_err()
        {
            tracing::error!(actor = ?source, "actor failure notice could not reach inbox owner");
        }
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

    fn status_text(&self, kernel: &KernelContext, actor: ActorRef, view: StatusView) -> String {
        let (standing, current_request) = match &self.standing {
            ResidentStanding::Workbench => ("operator-workbench", None),
            ResidentStanding::Boot => ("booting", None),
            ResidentStanding::Receiving(_) => ("receiving", None),
            ResidentStanding::Tools(_) => ("awaiting-tool", None),
            ResidentStanding::Interactive(awaiting) => {
                ("request-active", Some(awaiting.request.request))
            }
            ResidentStanding::Terminal => ("terminal", None),
            ResidentStanding::Paused(_) => ("handler-paused", None),
        };
        let requests = self.environment.requests.status_for(actor);
        let records = self.environment.actors.lock();
        let hidden_terminal_actors = records
            .iter()
            .filter(|(identity, _)| actor_can_observe(actor, **identity, &records))
            .filter(|(identity, record)| {
                record
                    .terminal
                    .clone()
                    .or_else(|| {
                        kernel
                            .resolve(**identity)
                            .and_then(|actor| actor.terminal().get())
                    })
                    .is_some_and(|terminal| terminal.kind != ActorExitKind::Failed)
            })
            .count();
        let mut roster = records
            .iter()
            .filter(|(identity, _)| actor_can_observe(actor, **identity, &records))
            .filter_map(|(identity, record)| {
                let terminal = record.terminal.clone().or_else(|| {
                    kernel
                        .resolve(*identity)
                        .and_then(|actor| actor.terminal().get())
                });
                if terminal.as_ref().is_some_and(|terminal| terminal.kind != ActorExitKind::Failed)
                    && view == StatusView::Concise {
                    return None;
                }
                let active = self.environment.requests.active_for_target(*identity);
                let state = match terminal {
                    Some(ref terminal) => format!("terminal:{:?} {:?}", terminal.kind, terminal.summary),
                    None if !active.is_empty() => format!("handling:{active:?}"),
                    None => "running".into(),
                };
                let runtime = record.runtime_observation.snapshot();
                let requests = self.environment.requests.work_for_target(*identity);
                let state = if terminal.is_none() {
                    format!("{state} disposition={:?} current={:?} queued={:?}",
                        runtime.disposition(!requests.0.is_empty() || !requests.1.is_empty()), requests.0, requests.1)
                } else { state };
                let state = match &runtime.provider_turn {
                    Some(turn) => format!("{state} provider={:?} turn={:?}", turn.state, turn.turn),
                    None => format!("{state} provider=unknown"),
                };
                let usage = runtime.latest_provider_usage();
                if view == StatusView::Concise {
                    return Some(format!(
                        "  - {:?} ({}@{}) supervisor={} role={:?} bound_worktree={:?} state={state} {}",
                        record.descriptor.label(), identity.id.0, identity.incarnation.0,
                        record.descriptor.supervisor_parent().map_or_else(
                            || "none".to_owned(),
                            |parent| format!("{}@{}", parent.id.0, parent.incarnation.0),
                        ),
                        record.descriptor.effective_role().role(),
                        record.bound_worktree,
                        runtime.usage_summary_display(),
                    ));
                }
                if view == StatusView::Lineage {
                    return Some(format!(
                        "  - {:?} ({}@{}) creator={:?} supervisor={:?} context_parent={:?} fork_group={:?}\n    haskell_scope={} provider_thread={:?} provider_parent_thread={:?} first_usage={:?} cache_boundary={:?} cached_input={:?} uncached_input={:?} bound_worktree={:?} {}",
                        record.descriptor.label(), identity.id.0, identity.incarnation.0,
                        record.descriptor.creator(), record.descriptor.supervisor_parent(), record.descriptor.context_parent(),
                        record.descriptor.fork_group(), record.descriptor.placement().lexical_scope.0,
                        runtime.provider_thread, runtime.provider_parent_thread,
                        runtime.first_provider_usage.as_ref().map(|sample| (&sample.observation_id, sample.cached_input_tokens, sample.uncached_input_tokens)),
                        usage.map(|sample| sample.cache_boundary),
                        usage.map(|sample| sample.cached_input_tokens),
                        usage.map(|sample| sample.uncached_input_tokens),
                        record.bound_worktree,
                        runtime.usage_summary_display(),
                    ));
                }
                Some(format!(
                    "  - {}@{} label={:?} supervisor={:?} context_parent={:?} fork_group={:?} role={:?} bound_worktree={:?} provider_thread={:?} provider_parent_thread={:?} cache_input={:?}/{:?} workbench={:?} state={} {}",
                    identity.id.0,
                    identity.incarnation.0,
                    record.descriptor.label(),
                    record.descriptor.supervisor_parent(),
                    record.descriptor.context_parent(),
                    record.descriptor.fork_group(),
                    record.descriptor.effective_role().role(),
                    record.bound_worktree,
                    runtime.provider_thread,
                    runtime.provider_parent_thread,
                    usage.map(|sample| sample.cached_input_tokens),
                    usage.map(|sample| sample.uncached_input_tokens),
                    runtime.workbench_posture,
                    state,
                    runtime.usage_summary_display(),
                ))
            })
            .collect::<Vec<_>>();
        drop(records);
        roster.sort();
        let runtime = self.runtime_observation.snapshot();
        let usage = runtime.latest_provider_usage();
        let unavailable_responses = format!("{:?}", requests.unavailable_responses);
        let unavailable_watches = format!("{:?}", requests.unavailable_watches);
        let roster_summary = if view != StatusView::Concise || hidden_terminal_actors == 0 {
            String::new()
        } else {
            format!("\n  completed/stopped actors hidden={hidden_terminal_actors} (use :status!)")
        };
        let sample_history = if view == StatusView::Lineage {
            format!(
                "\n  first_usage={:?}\n  latest_usage={:?}",
                runtime.first_provider_usage.as_ref().map(|sample| (
                    &sample.observation_id,
                    sample.cached_input_tokens,
                    sample.uncached_input_tokens
                )),
                usage.map(|sample| (
                    &sample.observation_id,
                    sample.cached_input_tokens,
                    sample.uncached_input_tokens
                ))
            )
        } else if view == StatusView::Trace {
            format!(
                "\n  provider_usage_history={:?}\n  usage_summary={:?}\n  latest_turn_usage={:?}",
                runtime.provider_usage,
                runtime.provider_usage_summary,
                runtime.latest_turn_usage_summary
            )
        } else {
            String::new()
        };
        let prompt_identity = if view == StatusView::Trace {
            format!(
                " prompt_catalog={:?} prompt_fingerprint={:?}\n  backend: executable={:?} version={:?} requested_model={:?} requested_effort={:?}\n  provider: turn={:?} stale={} confirmed_model={:?} confirmed_effort={:?}",
                runtime.prompt_catalog_version, runtime.prompt_fingerprint,
                runtime.backend_executable, runtime.backend_version,
                runtime.requested_model, runtime.requested_effort,
                runtime.provider_turn, runtime.provider_observation_stale,
                runtime.confirmed_model, runtime.confirmed_effort,
            )
        } else {
            String::new()
        };
        let current = if view == StatusView::Concise {
            format!(
                "actor {:?} ({}@{})\n  activation={:?} application={} program={standing} current_request={current_request:?}\n  responses: ready={:?} unavailable={} pending={:?}\n  watches: ready={:?} unavailable={} pending={:?}\n  role={:?} workspace={:?} descendant_depth={} active_children={:?} bound_worktree={:?} workbench={:?}{}",
                self.descriptor.label(), actor.id.0, actor.incarnation.0,
                runtime.activation_kind, if self.policy_installed { "attached" } else { "detached" }, requests.ready_responses, unavailable_responses,
                requests.pending_responses, requests.ready_watches, unavailable_watches,
                requests.pending_watches, self.descriptor.effective_role().role(),
                self.descriptor.effective_role().workspace(),
                self.descriptor.effective_role().descendants().maximum_depth,
                self.descriptor.effective_role().descendants().maximum_active_children,
                self.launch_worktrees.first(),
                runtime.workbench_posture, roster_summary,
            )
        } else {
            format!(
            "actor {}@{} label={:?}\n  lineage: creator={:?} supervisor={:?} context_parent={:?} fork_group={:?}\n  context: haskell_scope={} provider_thread={:?} provider_parent_thread={:?} cache_input={:?}/{:?} cache_boundary={:?}\n  activation: kind={:?} event_watermark={}\n  authority: role={:?} effects={} native_tools={:?} workspace={:?} descendants={:?} prompt_profile={:?}{}\n  runtime: application={} program={standing} workbench={:?} current_request={current_request:?} bound_worktree={:?}\n  responses: pending={:?} ready={:?} unavailable={}\n  watches: pending={:?} ready={:?} unavailable={}{}{}",
            actor.id.0,
            actor.incarnation.0,
            self.descriptor.label(),
            self.descriptor.creator(),
            self.descriptor.supervisor_parent(),
            self.descriptor.context_parent(),
            self.descriptor.fork_group(),
            self.descriptor.placement().lexical_scope.0,
            runtime.provider_thread,
            runtime.provider_parent_thread,
            usage.map(|sample| sample.cached_input_tokens),
            usage.map(|sample| sample.uncached_input_tokens),
            usage.map(|sample| sample.cache_boundary),
            runtime.activation_kind,
            runtime.event_watermark,
            self.descriptor.effective_role().role(),
            self.descriptor.effective_role().haskell_effects_type(),
            self.descriptor.effective_role().native_tools(),
            self.descriptor.effective_role().workspace(),
            self.descriptor.effective_role().descendants(),
            runtime
                .prompt_profile
                .as_deref()
                .unwrap_or(self.descriptor.effective_role().prompt_profile()),
            prompt_identity,
            if self.policy_installed {
                "attached"
            } else {
                "detached"
            },
            runtime.workbench_posture,
            self.launch_worktrees.first(),
            requests.pending_responses,
            requests.ready_responses,
            unavailable_responses,
            requests.pending_watches,
            requests.ready_watches,
            unavailable_watches,
            roster_summary,
            sample_history,
        )
        };
        let workspace = runtime.workspace.as_ref().map_or_else(
            || "workspace mapping: unavailable (no hosted launch observation)".to_owned(),
            crate::ActorWorkspaceObservation::orientation,
        );
        let failure = match &self.standing {
            ResidentStanding::Paused(failure) => {
                let input = match &failure.input {
                    RetainedActorInput::Mailbox(_) => "mailbox",
                    RetainedActorInput::Source(_) => "source",
                };
                format!(
                    "\n  handler failure: {}; retained state site={} input={input}",
                    failure.detail, failure.checkpoint.site,
                )
            }
            _ => String::new(),
        };
        let status = format!(
            "{current}{failure}\n  {workspace}\n  deadlines: [{}]\nactors:\n{}",
            requests
                .deadlines
                .iter()
                .map(|(_, deadline)| deadline.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            roster.join("\n")
        );
        if view == StatusView::Lineage {
            let lineage = roster.join("\n");
            format!(
                "actor {}@{} lineage\n  supervisor={:?}\n  context_parent={:?}\n  fork_group={:?}\nactors:\n{lineage}",
                actor.id.0,
                actor.incarnation.0,
                self.descriptor.supervisor_parent(),
                self.descriptor.context_parent(),
                self.descriptor.fork_group(),
            )
        } else {
            status
        }
    }

    fn idle_for_cleanup(&self, actor: ActorRef) -> bool {
        let requests = self.environment.requests.work_for_target(actor);
        self.environment
            .actors
            .lock()
            .get(&actor)
            .is_some_and(|record| {
                record
                    .runtime_observation
                    .snapshot()
                    .disposition(!requests.0.is_empty() || !requests.1.is_empty())
                    == crate::runtime_observation::AgentDisposition::IdleRetained
            })
    }

    fn cleanup_plan(
        &self,
        kernel: &KernelContext,
        owner: ActorRef,
        group: crate::ForkGroupId,
    ) -> crate::resident_workbench::CleanupPlanProjection {
        let members = match self.environment.fork_groups.members(group, owner) {
            Ok(members) => members,
            Err(error) => {
                return crate::resident_workbench::CleanupPlanProjection {
                    group,
                    actors: Vec::new(),
                    pending_responses: Vec::new(),
                    pending_watches: Vec::new(),
                    refusal: Some(error.to_string()),
                };
            }
        };
        let records = self.environment.actors.lock();
        let actors = members
            .iter()
            .map(|actor| {
                let record = records.get(actor);
                crate::resident_workbench::CleanupActorProjection {
                    actor: *actor,
                    revision: self.environment.requests.cleanup_revision(*actor),
                    label: record
                        .map(|record| record.descriptor.label().to_owned())
                        .unwrap_or_else(|| "<unavailable>".into()),
                    terminal: (record.is_none() && kernel.resolve(*actor).is_none())
                        || record.and_then(|record| record.terminal.as_ref()).is_some()
                        || kernel
                            .resolve(*actor)
                            .and_then(|actor| actor.terminal().get())
                            .is_some(),
                }
            })
            .collect::<Vec<_>>();
        drop(records);
        let targets = members
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let mut owners = targets.clone();
        owners.insert(owner);
        let (pending_responses, pending_watches) = self
            .environment
            .requests
            .campaign_cleanup_blockers(&owners, &targets);
        let refusal = actors.iter().find(|entry| !entry.terminal && !self.idle_for_cleanup(entry.actor))
            .map(|entry| format!("actor {}@{} has no confirmed idle provider turn; inspect its health before cleanup", entry.actor.id.0, entry.actor.incarnation.0));
        crate::resident_workbench::CleanupPlanProjection {
            group,
            actors,
            pending_responses,
            pending_watches,
            refusal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusView {
    Concise,
    Expanded,
    Lineage,
    Trace,
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
        deadline: crate::request::ActiveRequestDeadline,
    ) {
        let requests = Arc::clone(&self.environment.requests);
        let deployments = self.environment.deployments.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline.due_monotonic()).await;
            let (cancellation, notifications) = requests.deadline_request(owner, request);
            for notification in notifications {
                let _ = deployments.send(LocalResidentDeployment::WatchChanged { notification });
            }
            if let Some(notification) = cancellation {
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
        if self.pending_in_tool_block(target) {
            return Err(ResidentCallError::Call(KernelCallFailure::Handler {
                actor: target,
                detail: "child starts after this tool block completes; register a watch or call it in a later tool invocation".into(),
            }));
        }
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
            .call(context.actor, ancestry.clone(), request)
            .await
            .map_err(ResidentCallError::Call)
    }

    async fn start_child(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        start: crate::ResidentActorStart,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let crate::ResidentActorStart { parent_hole, child } = start;
        let fork_group = child.descriptor.fork_group();
        let started = self.try_start_child(kernel, context, child).await;
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

    fn validate_worker_context(
        &self,
        lifetime: crate::WorkerLifetime,
        context: crate::ForkContext,
    ) -> Result<(), String> {
        if lifetime == crate::WorkerLifetime::SwarmOwned {
            if context == crate::ForkContext::InheritedContext {
                return Err("a swarm-owned worker requires a selected context".into());
            }
            if self.descriptor.supervisor_parent().is_some() && !self.forest_control {
                return Err("only a top-level actor can admit a swarm-owned worker".into());
            }
        }
        if self.active_route.is_some() && context == crate::ForkContext::InheritedContext {
            return Err("automatic routes have no provider transcript boundary; select a task context for spawned workers".into());
        }
        Ok(())
    }

    async fn try_start_child(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        child: crate::start::CapturedChildLaunch,
    ) -> Result<
        (
            LocalActorRef,
            String,
            Option<tidepool_bridge_effects::WtWorktreeHandle>,
        ),
        ResidentActorWorkbenchError,
    > {
        let crate::start::CapturedChildLaunch {
            mut descriptor,
            entry,
            mut launch_worktrees,
            fork_workspace,
        } = child;
        let fork_group = descriptor.fork_group();
        let root_admission = self.environment.root_admission_closed.clone();
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
        let lifetime = if descriptor.supervisor_parent().is_none() {
            crate::WorkerLifetime::SwarmOwned
        } else {
            crate::WorkerLifetime::ParentOwned
        };
        let fork_context = if descriptor.context_parent().is_some() {
            crate::ForkContext::InheritedContext
        } else {
            crate::ForkContext::SelectedContext
        };
        self.validate_worker_context(lifetime, fork_context)
            .map_err(ResidentActorWorkbenchError::ActorProtocol)?;
        if descriptor.fork_group().is_some() {
            if self.policy_installed
                && self.active_fork_boundary.as_ref().is_none_or(|boundary| {
                    boundary.thread_id.is_empty() || boundary.call_id.is_empty()
                })
            {
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "context fork requires recorded invocation provenance from the hosted transport; use a Codex build that supplies contextCallId".into(),
                ));
            }
            descriptor = descriptor.with_fork_boundary(self.active_fork_boundary.clone());
        }
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
        let role = if descriptor.fork_group().is_some() {
            self.descriptor
                .effective_role()
                .preview_child(
                    descriptor.effective_role().clone(),
                    descriptor.fork_budget(),
                )
                .map_err(ResidentActorWorkbenchError::ActorProtocol)?
        } else {
            if !descriptor.effective_role().respects_role_ceiling() {
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "requested effect row exceeds or duplicates the role ceiling".into(),
                ));
            }
            self.descriptor
                .effective_role()
                .attenuate_child(descriptor.effective_role().clone())
        };
        descriptor = descriptor.with_effective_role(role);
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
        let prepared_workspace = if let Some(seed) = fork_workspace {
            let admission = self.environment.fork_workspaces.clone().ok_or_else(|| {
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
        if descriptor.placement().session != context.placement.session {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "child actor entry crossed a resident machine boundary".into(),
            ));
        }
        let allocated_label = descriptor.label().to_string();
        let admitted_worktree = prepared_workspace
            .as_ref()
            .map(|prepared| prepared.handle().clone());
        let mut behavior = Self::child(
            descriptor,
            self.environment.clone(),
            entry,
            launch_worktrees,
        );
        behavior.prepared_workspace = prepared_workspace;
        let child = kernel
            .spawn_worker(None, behavior, lifetime)
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
                target_ref.cast(context.actor, request).map_err(|error| {
                    ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                })?;
                self.environment
                    .runner
                    .resume_unit(context.clone(), continuation)
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

    /// Both authored tool replies and route callbacks resume the one active
    /// request continuation, then hand it back to the ordinary actor scheduler.
    async fn stage_request_reply(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        request: crate::RequestId,
        result: RootCustody,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let settled = async {
            if self.environment.fork_groups.has_incomplete(context.actor) {
                self.abort_incomplete_groups(
                    kernel,
                    context.actor,
                    "request reply interrupted unfold admission",
                )
                .await;
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "an actor cannot reply while an unfold group is unpublished".into(),
                ));
            }
            if self.pending_program.is_some() || self.pending_reply.is_some() {
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "actor settled a second reply before resuming the first".into(),
                ));
            }
            let awaiting = match std::mem::replace(&mut self.standing, ResidentStanding::Boot) {
                ResidentStanding::Interactive(awaiting) if awaiting.request.request == request => {
                    awaiting
                }
                standing => {
                    self.standing = standing;
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "reply did not match an active request continuation".into(),
                    ));
                }
            };
            let outcome = match self
                .environment
                .runner
                .resume_live(context.clone(), awaiting.hole.clone(), result)
                .await
            {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.standing = ResidentStanding::Interactive(awaiting);
                    return Err(error);
                }
            };
            self.pending_program = Some(outcome);
            self.pending_reply = Some(request);
            Ok(())
        }
        .await;
        if let Err(error) = &settled {
            let notifications = self
                .environment
                .requests
                .fail_reply_settlement(request, error.to_string());
            self.publish_watch_notifications(notifications);
        }
        settled
    }

    async fn resolve_effect(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        ancestry: &crate::CallAncestry,
        boundary: ResidentActorBoundary,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        // Keep each interpreter branch in its own future. A child can execute
        // an effect during Ractor startup on the caller's poll stack; embedding
        // every branch here makes that ordinary nesting exhaust a debug stack.
        let operation: futures_util::future::BoxFuture<
            '_,
            Result<ResidentOutcome, ResidentActorWorkbenchError>,
        > = match boundary {
            ResidentActorBoundary::Command {
                continuation,
                request,
            } => Box::pin(self.resolve_command(kernel, context, continuation, request)),
            ResidentActorBoundary::ActorLocalContext(continuation) => Box::pin(async move {
                self.environment
                    .runner
                    .resume_value(
                        context.clone(),
                        continuation,
                        (actor_address(context.actor), self.input_origin.clone()),
                    )
                    .await
            }),
            ResidentActorBoundary::ActorContext(continuation) => Box::pin(async move {
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
            }),
            ResidentActorBoundary::AgentInspect(inspection) => Box::pin(async move {
                let records = self.environment.actors.lock().clone();
                let observation = records.get(&inspection.target).and_then(|record| {
                    actor_can_observe(context.actor, inspection.target, &records).then(|| {
                        crate::resident_workbench::AgentRosterProjection {
                            received: self.environment.requests.received_counts(inspection.target),
                            requests: self.environment.requests.work_for_target(inspection.target),
                            actor: inspection.target,
                            descriptor: record.descriptor.clone(),
                            bound_worktree: record.bound_worktree.clone(),
                            terminal: record.terminal.clone().or_else(|| {
                                kernel
                                    .resolve(inspection.target)
                                    .and_then(|actor| actor.terminal().get())
                            }),
                            runtime: record.runtime_observation.snapshot(),
                        }
                    })
                });
                self.environment
                    .runner
                    .resume_agent_observation(context.clone(), inspection.continuation, observation)
                    .await
            }),
            ResidentActorBoundary::AgentList(continuation) => Box::pin(async move {
                let records = self.environment.actors.lock().clone();
                let mut roster = records
                    .iter()
                    .filter(|(actor, _)| actor_can_observe(context.actor, **actor, &records))
                    .map(
                        |(actor, record)| crate::resident_workbench::AgentRosterProjection {
                            received: self.environment.requests.received_counts(*actor),
                            requests: self.environment.requests.work_for_target(*actor),
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
            }),
            ResidentActorBoundary::AgentShareObservation {
                continuation,
                recipient,
                scope,
            } => Box::pin(async move {
                let outcome = {
                    let mut records = self.environment.actors.lock();
                    if records
                        .get(&recipient)
                        .is_none_or(|record| record.terminal.is_some())
                        || kernel
                            .resolve(recipient)
                            .is_none_or(|actor| actor.terminal().get().is_some())
                    {
                        ObservationShareResult::RecipientUnavailable
                    } else if !records.contains_key(&scope) {
                        ObservationShareResult::ScopeUnavailable
                    } else if !actor_can_observe(context.actor, scope, &records) {
                        ObservationShareResult::Unauthorized
                    } else {
                        records
                            .get_mut(&recipient)
                            .expect("checked exact recipient")
                            .observation_roots
                            .insert(scope);
                        ObservationShareResult::Shared
                    }
                };
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::AgentGroupList {
                continuation,
                group,
            } => Box::pin(async move {
                // Membership comes from exact admission ancestry, never a
                // display label or Git branch prefix. Unavailable/unauthorized
                // groups follow the existing optional inspection convention.
                let members = self
                    .environment
                    .fork_groups
                    .members(group, context.actor)
                    .ok();
                let records = self.environment.actors.lock().clone();
                let roster = members.and_then(|members| {
                    members
                        .into_iter()
                        .map(|actor| {
                            let record = records.get(&actor)?;
                            Some(crate::resident_workbench::AgentRosterProjection {
                                received: self.environment.requests.received_counts(actor),
                                requests: self.environment.requests.work_for_target(actor),
                                actor,
                                descriptor: record.descriptor.clone(),
                                bound_worktree: record.bound_worktree.clone(),
                                terminal: record.terminal.clone().or_else(|| {
                                    kernel
                                        .resolve(actor)
                                        .and_then(|actor| actor.terminal().get())
                                }),
                                runtime: record.runtime_observation.snapshot(),
                            })
                        })
                        .collect()
                });
                self.environment
                    .runner
                    .resume_group_roster(context.clone(), continuation, roster)
                    .await
            }),
            ResidentActorBoundary::AgentForget(forget) => Box::pin(async move {
                let authorized_terminal = {
                    let records = self.environment.actors.lock();
                    records.get(&forget.target).and_then(|record| {
                        actor_can_control(context.actor, forget.target, &records)
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
                        && actor_can_control(context.actor, forget.target, &records)
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
            }),
            ResidentActorBoundary::AgentStop(stop) => Box::pin(async move {
                let (known, authorized) = {
                    let records = self.environment.actors.lock();
                    (
                        records.contains_key(&stop.target),
                        stop.target != context.actor
                            && actor_can_control(context.actor, stop.target, &records),
                    )
                };
                let outcome = if stop.target == context.actor {
                    crate::resident_workbench::AgentStopProjection::Unauthorized
                } else if !known {
                    crate::resident_workbench::AgentStopProjection::Unavailable
                } else if !authorized {
                    crate::resident_workbench::AgentStopProjection::Unauthorized
                } else if kernel
                    .resolve(stop.target)
                    .and_then(|actor| actor.terminal().get())
                    .is_some()
                {
                    crate::resident_workbench::AgentStopProjection::AlreadyStopped
                } else if let Some(target) = kernel.resolve(stop.target) {
                    match target
                        .retire_by(
                            context.actor,
                            ActorTerminal {
                                kind: ActorExitKind::Cancelled,
                                summary: format!(
                                    "supervisor {}@{} requested retirement",
                                    context.actor.id.0, context.actor.incarnation.0
                                ),
                            },
                        )
                        .await
                    {
                        Ok(terminal) => {
                            self.publish_retired(stop.target, terminal);
                            crate::resident_workbench::AgentStopProjection::StoppedNow
                        }
                        Err(error) => crate::resident_workbench::AgentStopProjection::Failed(
                            error.to_string(),
                        ),
                    }
                } else {
                    crate::resident_workbench::AgentStopProjection::Unavailable
                };
                self.environment
                    .runner
                    .resume_agent_stop(context.clone(), stop.continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::CleanupPlan {
                continuation,
                group,
            } => Box::pin(async move {
                let plan = self.cleanup_plan(kernel, context.actor, group);
                self.environment
                    .runner
                    .resume_cleanup_plan(context.clone(), continuation, plan)
                    .await
            }),
            ResidentActorBoundary::CleanupExecute {
                continuation,
                group,
                inspected,
            } => Box::pin(async move {
                use crate::resident_workbench::{
                    AgentStopProjection, CleanupReceiptProjection, CleanupStepProjection,
                };

                let admission = (|| {
                    let actors = inspected
                        .iter()
                        .map(|(actor, _)| *actor)
                        .collect::<Vec<_>>();
                    let fork_guard = self
                        .environment
                        .fork_groups
                        .begin_cleanup(group, context.actor, &actors)
                        .map_err(|error| match error {
                            crate::ForkGroupError::CleanupScopeChanged(_) => {
                                CleanupStepProjection::StalePlan
                            }
                            other => CleanupStepProjection::Blocked(other.to_string()),
                        })?;
                    let revisions = inspected
                        .iter()
                        .filter(|(actor, _)| fork_guard.contains(actor))
                        .copied()
                        .collect::<Vec<_>>();
                    let request_guard = self
                        .environment
                        .requests
                        .begin_cleanup(context.actor, &revisions)
                        .map_err(|error| match error {
                            crate::request::CleanupAdmissionError::Stale => {
                                CleanupStepProjection::StalePlan
                            }
                            crate::request::CleanupAdmissionError::Busy => {
                                CleanupStepProjection::Blocked("cleanup already in progress".into())
                            }
                            crate::request::CleanupAdmissionError::Pending => {
                                CleanupStepProjection::Blocked(
                                    "subtree still has pending requests or watches".into(),
                                )
                            }
                        })?;
                    Ok::<_, CleanupStepProjection>((fork_guard, request_guard))
                })();
                let plan = self.cleanup_plan(kernel, context.actor, group);
                let (_fork_guard, _request_guard) = match admission {
                    Ok(guards) => guards,
                    Err(refusal) => {
                        return self
                            .environment
                            .runner
                            .resume_cleanup_receipt(
                                context.clone(),
                                continuation,
                                CleanupReceiptProjection {
                                    plan,
                                    steps: vec![refusal],
                                    complete: false,
                                },
                            )
                            .await;
                    }
                };
                let mut steps = Vec::new();

                if let Some(refusal) = &plan.refusal {
                    return self
                        .environment
                        .runner
                        .resume_cleanup_receipt(
                            context.clone(),
                            continuation,
                            CleanupReceiptProjection {
                                steps: vec![CleanupStepProjection::Blocked(refusal.clone())],
                                plan,
                                complete: false,
                            },
                        )
                        .await;
                }

                let group_order = match self
                    .environment
                    .fork_groups
                    .cleanup_group_order(group, context.actor)
                {
                    Ok(order) => order,
                    Err(error) => {
                        steps.push(CleanupStepProjection::Blocked(error.to_string()));
                        return self
                            .environment
                            .runner
                            .resume_cleanup_receipt(
                                context.clone(),
                                continuation,
                                CleanupReceiptProjection {
                                    plan,
                                    steps,
                                    complete: false,
                                },
                            )
                            .await;
                    }
                };

                let targets = plan
                    .actors
                    .iter()
                    .map(|actor| actor.actor)
                    .collect::<std::collections::HashSet<_>>();
                let mut owners = targets.clone();
                owners.insert(context.actor);
                let mut forgotten_responses = Vec::new();
                let mut forgotten_watches = Vec::new();
                for owner in owners {
                    let forgotten = self
                        .environment
                        .requests
                        .cleanup_campaign_metadata(owner, &targets);
                    forgotten_responses.extend(forgotten.forgotten_responses);
                    forgotten_watches.extend(forgotten.forgotten_watches);
                }
                forgotten_responses.sort_unstable();
                forgotten_watches.sort_unstable();
                if !forgotten_watches.is_empty() {
                    steps.push(CleanupStepProjection::ForgotWatches(forgotten_watches));
                }
                if !forgotten_responses.is_empty() {
                    steps.push(CleanupStepProjection::ForgotResponses(forgotten_responses));
                }

                let mut stop_failed = false;
                for actor_plan in &plan.actors {
                    let actor = actor_plan.actor;
                    let outcome = if actor_plan.terminal {
                        AgentStopProjection::AlreadyStopped
                    } else if !self.idle_for_cleanup(actor) {
                        stop_failed = true;
                        AgentStopProjection::Failed("provider is not confirmed idle; cleanup observation is stale or needs attention".into())
                    } else if let Some(target) = kernel.resolve(actor) {
                        match target
                            .retire_by(
                                context.actor,
                                ActorTerminal {
                                    kind: ActorExitKind::Cancelled,
                                    summary: format!(
                                        "campaign {} cleanup requested by {}@{}",
                                        group.0, context.actor.id.0, context.actor.incarnation.0
                                    ),
                                },
                            )
                            .await
                        {
                            Ok(terminal) => {
                                self.publish_retired(actor, terminal);
                                AgentStopProjection::StoppedNow
                            }
                            Err(error) => {
                                stop_failed = true;
                                AgentStopProjection::Failed(error.to_string())
                            }
                        }
                    } else {
                        stop_failed = true;
                        AgentStopProjection::Unavailable
                    };
                    steps.push(CleanupStepProjection::StoppedActor(actor, outcome));
                }

                if !stop_failed {
                    for actor in plan.actors.iter().map(|actor| actor.actor) {
                        match self
                            .environment
                            .requests
                            .forget_terminal_actor_metadata(actor)
                        {
                            Ok(()) => {
                                self.environment.actors.lock().remove(&actor);
                                self.environment.retired.lock().remove(&actor);
                                let _ = kernel.forget_terminal_actor(actor);
                                steps.push(CleanupStepProjection::ForgotActor(actor));
                            }
                            Err((requests, watches)) => {
                                stop_failed = true;
                                steps.push(CleanupStepProjection::ActorRetained {
                                    actor,
                                    requests,
                                    watches,
                                });
                            }
                        }
                    }
                }

                let complete = if stop_failed {
                    false
                } else {
                    let mut groups_complete = true;
                    for (cleanup_group, group_owner) in group_order {
                        match self
                            .environment
                            .fork_groups
                            .cleanup_committed(cleanup_group, group_owner)
                        {
                            Ok(crate::ForkGroupCleanupOutcome::Cleaned) => {
                                steps.push(CleanupStepProjection::GroupRetired(cleanup_group));
                            }
                            Ok(crate::ForkGroupCleanupOutcome::Active(active)) => {
                                groups_complete = false;
                                steps.push(CleanupStepProjection::Blocked(format!(
                                    "fork group {} still has active descendants: {active:?}",
                                    cleanup_group.0
                                )));
                            }
                            Err(error) => {
                                groups_complete = false;
                                steps.push(CleanupStepProjection::Blocked(error.to_string()));
                            }
                        }
                    }
                    groups_complete
                };
                self.environment
                    .runner
                    .resume_cleanup_receipt(
                        context.clone(),
                        continuation,
                        CleanupReceiptProjection {
                            plan,
                            steps,
                            complete,
                        },
                    )
                    .await
            }),
            ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Preview {
                continuation,
                role,
                effect_keys,
                budget,
                model,
                effort,
                context: fork_context,
                instructions,
                lifetime,
            }) => Box::pin(async move {
                let preview = self
                    .validate_worker_context(lifetime, fork_context)
                    .and_then(|()| {
                        let child = role
                            .effective_role(true)
                            .with_effect_keys(effect_keys.into_iter().map(Into::into).collect());
                        self.descriptor
                            .effective_role()
                            .preview_child(child, budget)
                            .map(|role| {
                                let budget = role.descendants();
                                let row = role.haskell_effects_type();
                                let launch =
                                    self.environment.launch_resolver.as_ref().map(|resolve| {
                                        resolve(&crate::WorkerLaunchRequest {
                                            role,
                                            model,
                                            effort,
                                            context: fork_context,
                                            instructions,
                                        })
                                    });
                                (
                                    (
                                        row,
                                        i64::from(budget.maximum_depth),
                                        budget.maximum_active_children.map(i64::from),
                                    ),
                                    launch,
                                )
                            })
                    });
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, preview)
                    .await
            }),
            ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Begin {
                continuation,
                relative,
                group,
                branches,
            }) => Box::pin(async move {
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
                        return Err(
                            "cannot unfold context: descendant depth budget is exhausted".into(),
                        );
                    }
                    let group_path = group.to_string();
                    let (group_id, reservations) = self
                        .environment
                        .fork_groups
                        .begin(
                            context.actor,
                            group,
                            branches,
                            budget.maximum_active_children.map(usize::from),
                        )
                        .map_err(|error| error.to_string())?;
                    Ok::<_, String>((group_id, group_path, reservations))
                })();
                match admitted {
                    Ok((group_id, group_path, reservations)) => {
                        if let Some((_, groups)) = &mut self.active_route {
                            groups.push(group_id);
                        }
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
            }),
            ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Commit {
                continuation,
                group,
            }) => Box::pin(async move {
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
            }),
            ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Abort {
                continuation,
                group,
            }) => Box::pin(async move {
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
            }),
            ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Cleanup {
                continuation,
                group,
            }) => Box::pin(async move {
                let outcome = self
                    .environment
                    .fork_groups
                    .cleanup_committed(group, context.actor);
                self.environment
                    .runner
                    .resume_fork_cleanup(context.clone(), continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::Start(start) => {
                Box::pin(self.start_child(kernel, context, start))
            }
            ResidentActorBoundary::Outbound(outbound) => Box::pin(async move {
                self.resolve_outbound(kernel, context, ancestry, outbound)
                    .await
            }),
            ResidentActorBoundary::Replace { target, candidate } => Box::pin(async move {
                let authorized = target != context.actor
                    && actor_can_control(context.actor, target, &self.environment.actors.lock());
                let actor = authorized
                    .then(|| kernel.resolve(target))
                    .flatten()
                    .filter(|actor| actor.terminal().get().is_none());
                let Some(actor) = actor else {
                    let placement = candidate.child.descriptor.placement();
                    drop(candidate);
                    self.environment
                        .runner
                        .retire_root_placement(placement)
                        .await?;
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        if authorized {
                            "replacement target is unavailable"
                        } else {
                            "actor replacement is not authorized"
                        }
                        .into(),
                    ));
                };
                let crate::ResidentActorStart { parent_hole, child } = candidate;
                let placement = child.descriptor.placement();
                let successor = match actor
                    .replace(crate::ActorReplacementDefinition { child })
                    .await
                {
                    Ok(successor) => successor,
                    Err(error) => {
                        // A failed send never transferred the candidate. A lost
                        // reply may follow cutover and must retain its custody.
                        if matches!(error, crate::KernelInvocationFailure::ActorExited(_)) {
                            if let Err(cleanup) = self
                                .environment
                                .runner
                                .retire_root_placement(placement)
                                .await
                            {
                                return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "{error}; replacement candidate cleanup failed: {cleanup}"
                                )));
                            }
                        }
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            error.to_string(),
                        ));
                    }
                };
                let identity = successor.identity();
                self.environment
                    .runner
                    .resume_value(
                        context.clone(),
                        parent_hole,
                        (identity.id.0 as i64, identity.incarnation.0 as i64),
                    )
                    .await
            }),
            ResidentActorBoundary::Drain {
                continuation,
                target,
            } => Box::pin(async move {
                let authorized = target != context.actor
                    && actor_can_control(context.actor, target, &self.environment.actors.lock());
                if !authorized {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "actor drain is not authorized".into(),
                    ));
                }
                let actor = kernel.resolve(target).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol("drain target is unavailable".into())
                })?;
                actor.drain().await.map_err(|error| {
                    ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                })?;
                self.environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
            }),
            ResidentActorBoundary::Wait(wait) => Box::pin(async move {
                if self.pending_in_tool_block(wait.target) {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "child starts after this tool block completes; register a watch or wait in a later tool invocation".into(),
                    ));
                }
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
            }),
            ResidentActorBoundary::Poll(poll) => Box::pin(async move {
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
            }),
            ResidentActorBoundary::NotificationSend {
                continuation,
                target,
                message,
            } => Box::pin(async move {
                let permitted = self
                    .descriptor
                    .effective_role()
                    .effect_keys()
                    .contains(&crate::ActorEffectKey::Notifications);
                let destination = kernel.resolve(target).zip(kernel.session_context(target));
                let outcome = if !permitted {
                    Err(crate::NotificationError::Unauthorized)
                } else if destination.as_ref().is_none_or(|(actor, target_context)| {
                    actor.terminal().get().is_some()
                        || target_context.placement.session != context.placement.session
                }) {
                    Err(crate::NotificationError::Unavailable)
                } else {
                    let (command, receive) =
                        crate::NotificationSend::new(context.actor, target, message);
                    if self
                        .environment
                        .deployments
                        .send(LocalResidentDeployment::NotificationSend(Arc::new(command)))
                        .is_err()
                    {
                        Err(crate::NotificationError::Unavailable)
                    } else {
                        crate::notification::receive_admission(receive)
                            .await
                            .and_then(crate::NotificationReceipt::into_wire)
                    }
                };
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::NotificationPoll {
                continuation,
                receipt,
            } => Box::pin(async move {
                let permitted = self
                    .descriptor
                    .effective_role()
                    .effect_keys()
                    .contains(&crate::ActorEffectKey::Notifications);
                let prepared = if permitted {
                    crate::NotificationReceipt::from_wire(receipt)
                        .and_then(|receipt| crate::NotificationPoll::new(context.actor, receipt))
                } else {
                    Err(crate::NotificationError::Unauthorized)
                };
                let outcome = match prepared {
                    Err(error) => Err(error),
                    Ok((command, receive)) => {
                        if self
                            .environment
                            .deployments
                            .send(LocalResidentDeployment::NotificationPoll(Arc::new(command)))
                            .is_err()
                        {
                            Err(crate::NotificationError::Unavailable)
                        } else {
                            crate::notification::receive_observation(receive).await
                        }
                    }
                };
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::RequestReservation(reservation) => Box::pin(async move {
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
            }),
            ResidentActorBoundary::RequestSubmission(submission) => Box::pin(async move {
                let request_deadline = submission
                    .deadline
                    .map(crate::request::ActiveRequestDeadline::start)
                    .transpose()
                    .map_err(ResidentActorWorkbenchError::ActorProtocol)?;
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
                        .mark_queued_with_deadline(
                            context.actor,
                            submission.target,
                            submission.request,
                            request_deadline.clone(),
                        )
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
            }),
            ResidentActorBoundary::ResponsePoll(poll) => Box::pin(async move {
                let observation = self
                    .environment
                    .requests
                    .observe_response(context.actor, poll.request);
                self.environment
                    .runner
                    .resume_response_observation(context.clone(), poll.continuation, observation)
                    .await
            }),
            ResidentActorBoundary::ProgressPublication {
                continuation,
                request,
                value,
            } => Box::pin(async move {
                let outcome = self
                    .environment
                    .requests
                    .publish_progress(context.actor, request, value)
                    .map(|(revision, notifications)| {
                        self.publish_watch_notifications(notifications);
                        revision
                    });
                self.environment
                    .runner
                    .resume_progress_publication(context.clone(), continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::ProgressPoll(poll) => Box::pin(async move {
                let observation = self
                    .environment
                    .requests
                    .observe_progress(context.actor, poll.request);
                self.environment
                    .runner
                    .resume_progress_observation(context.clone(), poll.continuation, observation)
                    .await
            }),
            ResidentActorBoundary::RequestUpdate {
                continuation,
                request,
                message,
            } => Box::pin(async move {
                let outcome = self
                    .environment
                    .requests
                    .update_request(context.actor, request, message)
                    .map(|(update, delivery)| {
                        if let Some(delivery) = delivery {
                            if let Err(error) = self
                                .environment
                                .deployments
                                .send(LocalResidentDeployment::RequestUpdate { delivery })
                            {
                                if let LocalResidentDeployment::RequestUpdate { delivery } = error.0
                                {
                                    if let Some(presentation) = delivery.begin() {
                                        presentation
                                            .not_presented("deployment owner unavailable".into());
                                    }
                                }
                            }
                        }
                        update.sequence as i64
                    });
                self.environment
                    .runner
                    .resume_request_update(context.clone(), continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::RequestUpdatePoll {
                continuation,
                update,
            } => Box::pin(async move {
                let outcome = self
                    .environment
                    .requests
                    .observe_update(context.actor, update);
                self.environment
                    .runner
                    .resume_request_update(context.clone(), continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::RequestCancellation(cancellation) => Box::pin(async move {
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
            }),
            ResidentActorBoundary::ResponseAbandonment(abandonment) => Box::pin(async move {
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
            }),
            ResidentActorBoundary::ResponseForget(forget) => Box::pin(async move {
                let outcome = self
                    .environment
                    .requests
                    .forget_response(context.actor, forget.request);
                self.environment
                    .runner
                    .resume_response_forget(context.clone(), forget.continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::ReplyPoll(poll) => Box::pin(async move {
                let observation = self
                    .environment
                    .requests
                    .observe_reply(context.actor, poll.request);
                self.environment
                    .runner
                    .resume_reply_observation(context.clone(), poll.continuation, observation)
                    .await
            }),
            ResidentActorBoundary::RouteRegistration {
                registration,
                entry,
            } => Box::pin(async move {
                let owner = kernel.resolve(context.actor).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol("route owner is unavailable".into())
                })?;
                let (watch, notifications) = self
                    .environment
                    .requests
                    .register_watch_groups_with_route(
                        context.actor,
                        registration.label,
                        registration.dependencies,
                        Some(crate::request::routes::WatchRoute::new(owner, entry)),
                    )
                    .map_err(|error| {
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "route registration rejected: {error:?}"
                        ))
                    })?;
                self.publish_watch_notifications(notifications);
                self.environment
                    .runner
                    .resume_int(context.clone(), registration.continuation, watch.0)
                    .await
            }),
            ResidentActorBoundary::RouteList(continuation) => Box::pin(async move {
                let routes = self
                    .environment
                    .requests
                    .list_routes(context.actor)
                    .into_iter()
                    .map(|id| {
                        i64::try_from(id.0).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "runtime route identity exceeds Haskell Int".into(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, routes)
                    .await
            }),
            ResidentActorBoundary::RoutePoll(poll) => Box::pin(async move {
                let state = self
                    .environment
                    .requests
                    .observe_route(context.actor, poll.watch);
                self.environment
                    .runner
                    .resume_route_state(context.clone(), poll.continuation, state)
                    .await
            }),
            ResidentActorBoundary::WatchRegistration(registration) => Box::pin(async move {
                crate::ActorPathSegment::new(&registration.label).map_err(|error| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "invalid watch label: {error}"
                    ))
                })?;
                let (watch, notifications) = self
                    .environment
                    .requests
                    .register_watch_requirement_groups(
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
            }),
            ResidentActorBoundary::WatchPoll(poll) => Box::pin(async move {
                let observation = self
                    .environment
                    .requests
                    .observe_watch(context.actor, poll.watch);
                self.environment
                    .runner
                    .resume_watch_observation(context.clone(), poll.continuation, observation)
                    .await
            }),
            ResidentActorBoundary::WatchProgressPoll {
                continuation,
                watch,
                request,
                after,
            } => Box::pin(async move {
                let observation = self.environment.requests.observe_watch_progress(
                    context.actor,
                    watch,
                    request,
                    after,
                );
                self.environment
                    .runner
                    .resume_progress_observation(context.clone(), continuation, observation)
                    .await
            }),
            ResidentActorBoundary::WatchForget(forget) => Box::pin(async move {
                let outcome = self
                    .environment
                    .requests
                    .forget_watch(context.actor, forget.watch);
                self.environment
                    .runner
                    .resume_watch_forget(context.clone(), forget.continuation, outcome)
                    .await
            }),
            other => Box::pin(async move {
                Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                    "`{}` is not an active actor effect",
                    other.operation()
                )))
            }),
        };
        operation.await
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
                    self.active_input = None;
                    self.standing = ResidentStanding::Terminal;
                    return Ok(KernelStep::Stop {
                        output: (),
                        terminal: completed_terminal(),
                    });
                }
                ResidentActorBoundary::Receive(receiver) => {
                    if let Some(checkpoint) = self.pending_checkpoint.take() {
                        if checkpoint.site != receiver.site {
                            return Err(ResidentActorWorkbenchError::ActorProtocol(
                                "checkpoint and receiver sites differ".into(),
                            ));
                        }
                        self.checkpoint = Some(checkpoint);
                    }
                    self.active_input = None;
                    self.input_origin = ActorInputOrigin::ActorStartup;
                    self.standing = ResidentStanding::Receiving(receiver);
                    return Ok(KernelStep::Continue(()));
                }
                ResidentActorBoundary::Checkpoint {
                    continuation,
                    site,
                    value,
                } => {
                    if self.pending_checkpoint.is_some() {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "actor staged two states before installing its receiver".into(),
                        ));
                    }
                    self.pending_checkpoint = Some(StateCheckpoint {
                        site,
                        value: Arc::new(value),
                    });
                    outcome = self
                        .environment
                        .runner
                        .resume_unit(context.clone(), continuation)
                        .await?;
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
                            worktree_custody: self.worktree_custody.clone(),
                            effective_role: self.descriptor.effective_role().clone(),
                            fork_effort: self.descriptor.fork_effort(),
                            model: self.descriptor.model().map(str::to_owned),
                            instructions: self.descriptor.instructions().map(str::to_owned),
                            creator: self.descriptor.creator(),
                            fork_boundary: self.descriptor.fork_boundary().cloned(),
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
        let already_installed = self.policy_installed;
        let workbench = self.environment.runner.workbench(
            request.response.clone(),
            request.request,
            request.type_modules(),
        );
        workbench
            .mount_named_input(
                context.clone(),
                "sessionInput",
                request.input_type.clone(),
                input,
            )
            .await?;
        let (input_preview, reply_preview) = workbench
            .activation_preview(
                context.clone(),
                request.response.expected_type().to_owned(),
                request.response.declaration.clone(),
            )
            .await;
        let contract = crate::interactive_session::ActivationContract {
            input_type: request.input_type.clone(),
            response: request.response.clone(),
            input_preview,
            reply_preview,
        };
        let request_message =
            contract.message(request.request, request.initial_user_message.as_deref());
        self.install_interactive_policy(kernel, context, Some(request_message.clone()))?;
        self.standing =
            ResidentStanding::Interactive(crate::interactive_session::ResidentInteractiveAwait {
                request,
                hole,
            });
        let active_request = match &self.standing {
            ResidentStanding::Interactive(awaiting) => awaiting.request.request,
            _ => unreachable!(),
        };
        self.runtime_observation.publish_request_activation(
            active_request,
            if already_installed {
                self.next_activation_sequence
            } else {
                0
            },
        );
        if already_installed {
            let activation = crate::ResidentActivation::mounted(
                context.actor,
                self.next_activation_sequence,
                match &self.standing {
                    ResidentStanding::Interactive(awaiting) => awaiting.request.request,
                    _ => unreachable!(),
                },
                contract,
                match &self.standing {
                    ResidentStanding::Interactive(awaiting) => {
                        awaiting.request.initial_user_message.as_deref()
                    }
                    _ => unreachable!(),
                },
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
            worktree_custody: self.worktree_custody.clone(),
            effective_role: self.descriptor.effective_role().clone(),
            fork_effort: self.descriptor.fork_effort(),
            model: self.descriptor.model().map(str::to_owned),
            instructions: self.descriptor.instructions().map(str::to_owned),
            creator: self.descriptor.creator(),
            fork_boundary: self.descriptor.fork_boundary().cloned(),
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
        let boot = match boot {
            ResidentBoot::Replacement(prepared) => {
                self.boot = Some(ResidentBoot::Replacement(prepared));
                return Ok(KernelStep::Continue(()));
            }
            boot => boot,
        };
        if let Some(terminal) = kernel.requested_shutdown() {
            return Ok(KernelStep::Stop {
                output: (),
                terminal,
            });
        }
        if self.worktree_custody.is_none() {
            if let Some(prepared) = self.prepared_workspace.take() {
                let actor = context.actor;
                self.worktree_custody = Some(
                    tokio::task::spawn_blocking(move || prepared.install(actor))
                        .await
                        .map_err(ResidentActorWorkbenchError::Join)?
                        .map_err(|error| {
                            ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                        })?,
                );
            }
        }
        if self.worktree_custody.is_none() {
            match self.launch_worktrees.as_slice() {
                [] => {}
                [worktree] => {
                    let admission = self.environment.fork_workspaces.as_ref().ok_or_else(|| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            "pre-bootstrap worktree custody is unavailable".into(),
                        )
                    })?;
                    let admission = admission.clone();
                    let actor = context.actor;
                    let worktree = worktree.clone();
                    self.worktree_custody = Some(
                        tokio::task::spawn_blocking(move || {
                            admission.install_custody(actor, &worktree)
                        })
                        .await
                        .map_err(ResidentActorWorkbenchError::Join)?
                        .map_err(|error| {
                            ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                        })?,
                    );
                }
                _ => {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "actor bootstrap requires at most one worktree".into(),
                    ))
                }
            }
        }
        if let Some(terminal) = kernel.requested_shutdown() {
            return Ok(KernelStep::Stop {
                output: (),
                terminal,
            });
        }
        let outcome = match boot {
            ResidentBoot::Replacement(_) => {
                unreachable!("replacement bootstrap parks before initialization")
            }
            ResidentBoot::Workbench => {
                self.standing = ResidentStanding::Workbench;
                self.policy_installed = true;
                return Ok(KernelStep::Continue(()));
            }
            ResidentBoot::Prepared(outcome) => *outcome,
            ResidentBoot::Entry(entry) => {
                let mut outcome = self
                    .environment
                    .runner
                    .run_rooted_entry(context.clone(), entry, context.placement.resource_scope)
                    .await?;
                loop {
                    let startup_step = self
                        .environment
                        .runner
                        .capture_startup_step(
                            context.clone(),
                            outcome,
                            context.placement.resource_scope,
                        )
                        .await?;
                    if let Some(terminal) = kernel.requested_shutdown() {
                        return Ok(KernelStep::Stop {
                            output: (),
                            terminal,
                        });
                    }
                    match startup_step {
                        ResidentActorStartupStep::InstallSource {
                            continuation,
                            source,
                        } => {
                            self.sources.push(source);
                            outcome = self
                                .environment
                                .runner
                                .resume_unit(context.clone(), continuation)
                                .await?;
                        }
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
                            if !self.sources.is_empty() {
                                let owner = self.descriptor.creator().ok_or_else(|| {
                                    ResidentActorWorkbenchError::ActorProtocol(
                                        "source actor has no creator".into(),
                                    )
                                })?;
                                let recipient = kernel.resolve(context.actor).ok_or_else(|| {
                                    ResidentActorWorkbenchError::ActorProtocol(
                                        "source actor is absent from its directory".into(),
                                    )
                                })?;
                                let sources = self
                                    .sources
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(slot, source)| match source.target {
                                        crate::request::sources::SourceTarget::Request(
                                            request,
                                            kind,
                                        ) => Some((slot, request, kind)),
                                        crate::request::sources::SourceTarget::Lifecycle(_)
                                        | crate::request::sources::SourceTarget::Command(_) => None,
                                    })
                                    .collect::<Vec<_>>();
                                let mut lifecycle = Vec::new();
                                for (slot, source) in self.sources.iter().enumerate() {
                                    if let crate::request::sources::SourceTarget::Lifecycle(
                                        target,
                                    ) = source.target
                                    {
                                        if !actor_can_observe(
                                            owner,
                                            target,
                                            &self.environment.actors.lock(),
                                        ) {
                                            return Err(
                                                ResidentActorWorkbenchError::ActorProtocol(
                                                    "lifecycle source is not authorized".into(),
                                                ),
                                            );
                                        }
                                        let actor = kernel.resolve(target).ok_or_else(|| {
                                            ResidentActorWorkbenchError::ActorProtocol(
                                                "lifecycle source target is unavailable".into(),
                                            )
                                        })?;
                                        lifecycle.push((slot, actor));
                                    }
                                }
                                self.source_connections = Some(
                                    self.environment
                                        .requests
                                        .attach_sources(owner, recipient, &sources)
                                        .map_err(|error| {
                                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                                "source attachment rejected: {error:?}"
                                            ))
                                        })?,
                                );
                                for (slot, source) in self.sources.iter().enumerate() {
                                    if let crate::request::sources::SourceTarget::Command(key) =
                                        source.target
                                    {
                                        self.source_connections
                                            .as_mut()
                                            .expect("attached sources")
                                            .attach_command(
                                                slot,
                                                key,
                                                owner,
                                                &self.environment.commands,
                                            )
                                            .map_err(|error| {
                                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                                    "command source: {error:?}"
                                                ))
                                            })?;
                                    }
                                }
                                for (slot, actor) in lifecycle {
                                    self.source_connections
                                        .as_mut()
                                        .expect("attached source set")
                                        .attach_lifecycle(slot, &actor);
                                }
                            }
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
        let request = Arc::new(request.into_custody());
        if self.checkpoint.is_some() && self.active_input.is_none() {
            self.active_input = Some(RetainedActorInput::Mailbox(Arc::clone(&request)));
        }
        let handler_realm = RealmId::fresh();
        let mut outcome = self
            .environment
            .runner
            .run_rooted_application(context.clone(), handler, request, handler_realm)
            .await?;
        if caller.is_some() {
            // The synchronous RPC remains owned while the handler performs
            // effects. Its first suspension need not be the eventual reply.
            while self
                .environment
                .runner
                .kernel_boundary(context.clone(), &outcome)
                .await?
                .is_none()
            {
                let boundary = self
                    .environment
                    .runner
                    .capture_boundary(context.clone(), outcome, handler_realm)
                    .await?;
                outcome = self
                    .resolve_effect(kernel, context, ancestry, boundary)
                    .await?;
            }
        }
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
        unit: WorkbenchUnitExecution<'_>,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        let mut effect_ordinal = 0;
        loop {
            self.runtime_observation.publish_workbench_posture(
                crate::ActorWorkbenchPosture::RunningUnit {
                    input_unit_index: unit.input_unit_index,
                    total: unit.total,
                },
            );
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
                    let effect = boundary.operation().to_owned();
                    self.runtime_observation.publish_workbench_posture(
                        crate::ActorWorkbenchPosture::AwaitingEffect {
                            input_unit_index: unit.input_unit_index,
                            total: unit.total,
                            effect: effect.clone(),
                        },
                    );
                    let ordinal = effect_ordinal;
                    effect_ordinal += 1;
                    match boundary {
                        ResidentActorBoundary::ReplyAttempt(attempt) => match self
                            .environment
                            .requests
                            .begin_reply(context.actor, attempt.request)
                        {
                            Ok(()) => {
                                record_workbench_operation(
                                    unit.operations,
                                    unit.execution,
                                    unit.input_unit_index,
                                    ordinal,
                                    &effect,
                                    WorkbenchOperationDisposition::Committed,
                                );
                                return Ok(ResidentWorkbenchStep::Replied {
                                    request: attempt.request,
                                    result: attempt.result,
                                });
                            }
                            Err(error) if attempt.recoverable => {
                                record_workbench_operation(
                                    unit.operations,
                                    unit.execution,
                                    unit.input_unit_index,
                                    ordinal,
                                    &effect,
                                    WorkbenchOperationDisposition::Rejected,
                                );
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
                                record_workbench_operation(
                                    unit.operations,
                                    unit.execution,
                                    unit.input_unit_index,
                                    ordinal,
                                    &effect,
                                    WorkbenchOperationDisposition::Rejected,
                                );
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
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        WorkbenchOperationDisposition::Committed,
                                    );
                                    return Ok(ResidentWorkbenchStep::CancellationAcknowledged {
                                        request: acknowledgement.request,
                                    });
                                }
                                Err(error) if acknowledgement.recoverable => {
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        WorkbenchOperationDisposition::Rejected,
                                    );
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
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        WorkbenchOperationDisposition::Rejected,
                                    );
                                    return Ok(ResidentWorkbenchStep::Rejected(format!(
                                        "cancellation acknowledgement rejected: {error:?}"
                                    )));
                                }
                            }
                        }
                        boundary => {
                            outcome = match self
                                .resolve_effect(
                                    kernel,
                                    context,
                                    &crate::CallAncestry::begin(context.actor),
                                    boundary,
                                )
                                .await
                            {
                                Ok(outcome) => {
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        WorkbenchOperationDisposition::Committed,
                                    );
                                    outcome
                                }
                                Err(error) => {
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        WorkbenchOperationDisposition::Unknown,
                                    );
                                    return Err(error);
                                }
                            };
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
        let execution = request.execution_id().cloned();
        let workbench = match &self.standing {
            ResidentStanding::Interactive(awaiting) => self.environment.runner.workbench(
                awaiting.request.response.clone(),
                awaiting.request.request,
                awaiting.request.type_modules(),
            ),
            ResidentStanding::Workbench | ResidentStanding::Receiving(_)
                if self.policy_installed =>
            {
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
            let command = request.items[index].trim();
            let status_view = match command {
                ":status" => Some(StatusView::Concise),
                ":status!" => Some(StatusView::Expanded),
                ":lineage" => Some(StatusView::Lineage),
                ":trace" => Some(StatusView::Trace),
                _ => None,
            };
            if let Some(status_view) = status_view {
                receipts.push(WorkbenchItemReceipt {
                    index,
                    status: WorkbenchItemStatus::Committed,
                    output: self.status_text(kernel, context.actor, status_view),
                    warnings: Vec::new(),
                    installed_bindings: Vec::new(),
                    operations: Vec::new(),
                    terminal_transfer: None,
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
                        installed_bindings: Vec::new(),
                        operations: Vec::new(),
                        terminal_transfer: None,
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
                            installed_bindings: Vec::new(),
                            operations: Vec::new(),
                            terminal_transfer: None,
                        }),
                        Err(output) => {
                            receipts.push(WorkbenchItemReceipt {
                                index: receipt_index,
                                status: WorkbenchItemStatus::Diagnostic,
                                output,
                                warnings: Vec::new(),
                                installed_bindings: Vec::new(),
                                operations: Vec::new(),
                                terminal_transfer: None,
                            });
                        }
                    }
                }
                index += batch_len;
                continue;
            }

            let source = request.items[index].clone();
            let mut unit_operations = Vec::new();
            let block = ParsedBlock {
                ordinal: index + 1,
                total: request.items.len(),
                source,
            };
            self.runtime_observation.publish_workbench_posture(
                crate::ActorWorkbenchPosture::RunningUnit {
                    input_unit_index: index,
                    total: request.items.len(),
                },
            );
            let mut step = match workbench
                .begin_item(context.clone(), block, request.input_kind(index))
                .await
            {
                Ok(step) => step,
                Err(source) => {
                    self.abort_incomplete_groups(
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
                    .settle_fragment_effects(
                        kernel,
                        context,
                        &workbench,
                        fragment,
                        *outcome,
                        WorkbenchUnitExecution {
                            execution: execution.as_ref(),
                            input_unit_index: index,
                            total: request.items.len(),
                            operations: &mut unit_operations,
                        },
                    )
                    .await
                {
                    Ok(step) => step,
                    Err(source) => {
                        self.abort_incomplete_groups(
                            kernel,
                            context.actor,
                            "Haskell workbench failed during unfold admission",
                        )
                        .await;
                        return Err(workbench_failure_after_operations(
                            &receipts,
                            index,
                            request.items.len(),
                            source,
                            unit_operations,
                        ));
                    }
                };
            }
            match step {
                ResidentWorkbenchStep::Committed {
                    output,
                    warnings,
                    installed_bindings,
                } => {
                    if self.environment.fork_groups.has_ready(context.actor) {
                        let publication = if self.active_fork_boundary.is_none() {
                            self.environment.fork_groups.publish_ready(context.actor)
                        } else {
                            Ok(Vec::new())
                        };
                        if let Err(source) = publication {
                            settle_prepared_operations(
                                &mut unit_operations,
                                WorkbenchOperationDisposition::Unknown,
                            );
                            return Err(workbench_failure_after_operations(
                                &receipts,
                                index,
                                request.items.len(),
                                ResidentActorWorkbenchError::ActorProtocol(source.to_string()),
                                unit_operations,
                            ));
                        }
                        settle_prepared_operations(
                            &mut unit_operations,
                            WorkbenchOperationDisposition::Committed,
                        );
                    } else if self.environment.fork_groups.has_incomplete(context.actor) {
                        self.abort_incomplete_groups(
                            kernel,
                            context.actor,
                            "Haskell input ended before unfold admission committed",
                        )
                        .await;
                        settle_prepared_operations(
                            &mut unit_operations,
                            WorkbenchOperationDisposition::Rejected,
                        );
                        receipts.push(WorkbenchItemReceipt {
                            index,
                            status: WorkbenchItemStatus::Rejected,
                            output: "unfold admission ended without committing every fork group"
                                .into(),
                            warnings: Vec::new(),
                            installed_bindings: Vec::new(),
                            operations: unit_operations,
                            terminal_transfer: None,
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
                        installed_bindings,
                        operations: unit_operations,
                        terminal_transfer: None,
                    });
                }
                ResidentWorkbenchStep::Rejected(output) => {
                    settle_prepared_operations(
                        &mut unit_operations,
                        WorkbenchOperationDisposition::Rejected,
                    );
                    if request.item_is_observational(index) {
                        receipts.push(WorkbenchItemReceipt {
                            index,
                            status: WorkbenchItemStatus::Diagnostic,
                            output,
                            warnings: Vec::new(),
                            installed_bindings: Vec::new(),
                            operations: unit_operations,
                            terminal_transfer: None,
                        });
                        index += 1;
                        continue;
                    }
                    self.abort_incomplete_groups(
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
                        installed_bindings: Vec::new(),
                        operations: unit_operations,
                        terminal_transfer: None,
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
                    self.stage_request_reply(kernel, context, request_id, result)
                        .await
                        .map_err(|error| {
                            workbench_failure_after_operations(
                                &receipts,
                                index,
                                request.items.len(),
                                error,
                                unit_operations.clone(),
                            )
                        })?;
                    receipts.push(WorkbenchItemReceipt {
                        index,
                        status: WorkbenchItemStatus::Committed,
                        output: "Reply submitted.".to_owned(),
                        warnings: Vec::new(),
                        installed_bindings: Vec::new(),
                        operations: unit_operations,
                        terminal_transfer: Some(WorkbenchTerminalTransfer::ReplyAccepted),
                    });
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
                    if self.environment.fork_groups.has_incomplete(context.actor) {
                        self.abort_incomplete_groups(
                            kernel,
                            context.actor,
                            "request cancellation interrupted unfold admission",
                        )
                        .await;
                        self.environment
                            .requests
                            .rollback_cancellation_acknowledgement(request_id);
                        return Err(workbench_failure_after_operations(
                            &receipts,
                            index,
                            request.items.len(),
                            ResidentActorWorkbenchError::ActorProtocol(
                                "an actor cannot acknowledge cancellation while an unfold group is unpublished"
                                    .into(),
                            ),
                            unit_operations,
                        ));
                    }
                    if self.pending_program.is_some()
                        || self.pending_reply.is_some()
                        || self.pending_cancellation.is_some()
                    {
                        self.environment
                            .requests
                            .rollback_cancellation_acknowledgement(request_id);
                        return Err(workbench_failure_after_operations(
                            &receipts,
                            index,
                            request.items.len(),
                            ResidentActorWorkbenchError::ActorProtocol(
                                "actor settled a second request before resuming the first".into(),
                            ),
                            unit_operations,
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
                                return Err(workbench_failure_after_operations(
                                    &receipts,
                                    index,
                                    request.items.len(),
                                    ResidentActorWorkbenchError::ActorProtocol(
                                        "cancellation did not match the active request".into(),
                                    ),
                                    unit_operations,
                                ));
                            }
                            standing => {
                                self.standing = standing;
                                self.environment
                                    .requests
                                    .rollback_cancellation_acknowledgement(request_id);
                                return Err(workbench_failure_after_operations(
                                    &receipts,
                                    index,
                                    request.items.len(),
                                    ResidentActorWorkbenchError::ActorProtocol(
                                        "cancellation lost its active request".into(),
                                    ),
                                    unit_operations,
                                ));
                            }
                        };
                    let Some(suspended) = self.suspended_cast.take() else {
                        self.standing = ResidentStanding::Interactive(awaiting);
                        self.environment
                            .requests
                            .rollback_cancellation_acknowledgement(request_id);
                        return Err(workbench_failure_after_operations(
                            &receipts,
                            index,
                            request.items.len(),
                            ResidentActorWorkbenchError::ActorProtocol(
                                "cancellation lost its mailbox continuation".into(),
                            ),
                            unit_operations,
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
                            return Err(workbench_failure_after_operations(
                                &receipts,
                                index,
                                request.items.len(),
                                error,
                                unit_operations,
                            ));
                        }
                    };
                    drop(awaiting);
                    self.pending_program = Some(outcome);
                    self.pending_cancellation = Some(request_id);
                    receipts.push(WorkbenchItemReceipt {
                        index,
                        status: WorkbenchItemStatus::Committed,
                        output: String::new(),
                        warnings: Vec::new(),
                        installed_bindings: Vec::new(),
                        operations: unit_operations,
                        terminal_transfer: Some(
                            WorkbenchTerminalTransfer::CancellationAcknowledged,
                        ),
                    });
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
    async fn abort_incomplete_groups(
        &self,
        kernel: &KernelContext,
        owner: ActorRef,
        summary: &str,
    ) {
        let selected = self
            .active_route
            .as_ref()
            .map(|(_, groups)| groups.as_slice());
        for child in self
            .environment
            .fork_groups
            .abort_incomplete(owner, selected)
        {
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
    fn replacement_staged(&self) -> bool {
        matches!(self.boot, Some(ResidentBoot::Replacement(_)))
    }
    fn discard_replacement<'a>(
        &'a mut self,
        definition: crate::ActorReplacementDefinition,
    ) -> futures_util::future::BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async move {
            let placement = definition.child.descriptor.placement();
            drop(definition);
            self.environment
                .runner
                .retire_root_placement(placement)
                .await
                .map_err(Self::failure)
        })
    }
    fn replace<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        definition: crate::ActorReplacementDefinition,
    ) -> futures_util::future::BoxFuture<'a, Result<LocalActorRef, KernelBehaviorError>> {
        Box::pin(async move {
            self.prepare_successor(kernel, definition)
                .await
                .map_err(Self::failure)
        })
    }

    fn commit_replacement(
        &mut self,
        predecessor: &KernelContext,
        successor: &KernelContext,
    ) -> Result<(), KernelBehaviorError> {
        self.transfer_replacement(predecessor, successor)
            .map_err(Self::failure)
    }

    fn replacement_retired(&mut self, context: &KernelContext, terminal: &ActorTerminal) {
        self.publish_retired(context.identity(), terminal.clone());
    }

    fn activate_replacement<'a>(
        &'a mut self,
        _kernel: &'a KernelContext,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move { self.activate_successor().map_err(Self::failure) })
    }

    fn source<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        delivery: crate::SourceDelivery,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            use crate::request::sources::{RequestSourceKind, SourceTarget};
            self.input_origin = match delivery.target {
                SourceTarget::Request(request, RequestSourceKind::Progress) => {
                    ActorInputOrigin::ActorProgressFrom(request.0 as i64)
                }
                SourceTarget::Request(request, RequestSourceKind::Settlement) => {
                    ActorInputOrigin::ActorSettlementFrom(request.0 as i64)
                }
                SourceTarget::Command(key) => {
                    ActorInputOrigin::ActorCommandFrom(uuid::Uuid::from_u128(key).to_string())
                }
                SourceTarget::Lifecycle(actor) => {
                    ActorInputOrigin::ActorLifecycleFrom(actor_address(actor))
                }
            };
            let source = self
                .sources
                .get(delivery.slot)
                .filter(|source| source.target == delivery.target)
                .ok_or_else(|| KernelBehaviorError {
                    detail: "source delivery does not match an installed connection".into(),
                })?;
            if self.checkpoint.is_some() {
                self.active_input = Some(RetainedActorInput::Source(delivery.clone()));
            }
            let message = self
                .environment
                .runner
                .map_source(context.clone(), Arc::clone(&source.entry), delivery.event)
                .await
                .map_err(Self::failure)?;
            let (_, step) = self
                .run_receiver(
                    kernel,
                    &context,
                    None,
                    &crate::CallAncestry::begin(context.actor),
                    message,
                )
                .await
                .map_err(Self::failure)?;
            Ok(step)
        })
    }

    fn accepts_mailbox(&self) -> bool {
        matches!(self.standing, ResidentStanding::Receiving(_))
    }

    fn begin_drain(&mut self) -> Result<(), KernelBehaviorError> {
        if self.checkpoint.is_none()
            || matches!(
                self.standing,
                ResidentStanding::Paused(_) | ResidentStanding::Terminal
            )
        {
            return Err(KernelBehaviorError {
                detail: "drain requires a live stateful actor; replace a failed handler first"
                    .into(),
            });
        }
        Ok(())
    }

    fn close_sources(&mut self) {
        self.source_connections.take();
    }

    fn drain<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            let ResidentStanding::Receiving(receiver) =
                std::mem::replace(&mut self.standing, ResidentStanding::Boot)
            else {
                return Err(KernelBehaviorError {
                    detail: "drain reached an actor without its receiver".into(),
                });
            };
            let context = self.context(kernel.identity());
            let outcome = self
                .environment
                .runner
                .resume_value(context.clone(), receiver.continuation, None::<()>)
                .await
                .map_err(Self::failure)?;
            self.stabilize_program(
                kernel,
                &context,
                &crate::CallAncestry::begin(context.actor),
                outcome,
            )
            .await
            .map_err(Self::failure)
        })
    }

    fn pause_failed_handler(&mut self, kernel: &KernelContext, detail: &str) -> bool {
        if matches!(self.standing, ResidentStanding::Paused(_)) {
            return true;
        }
        let (checkpoint, input) = match (self.checkpoint.take(), self.active_input.take()) {
            (Some(checkpoint), Some(input)) => (checkpoint, input),
            (checkpoint, input) => {
                self.checkpoint = checkpoint;
                self.active_input = input;
                return false;
            }
        };
        self.pending_checkpoint = None;
        let failure = PausedHandler {
            checkpoint,
            input,
            detail: detail.to_owned(),
        };
        tracing::debug!(
            actor = ?kernel.identity(),
            committed_state = ?failure.checkpoint.value,
            failed_input = ?failure.input,
            detail = %failure.detail,
            "retaining paused handler custody"
        );
        self.standing = ResidentStanding::Paused(failure);
        if let Some(actor) = kernel.resolve(kernel.identity()) {
            actor.terminal().publish_paused(detail.to_owned());
        }
        self.notify_supervisor(
            kernel.identity(),
            kernel.supervisor_identity(),
            format!("{:?} handler paused: {detail}. State/input/queue retained; no replay. Replace actor or stop.", kernel.identity()),
        );
        true
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
                    forest_control: self.forest_control,
                    interactive_policy_installed: false,
                    observation_roots: Default::default(),
                    descriptor: self.descriptor.clone(),
                    bound_worktree: self.launch_worktrees.first().cloned(),
                    terminal: None,
                    runtime_observation: self.runtime_observation.clone(),
                },
            );
            if self.descriptor.fork_boundary().is_some() {
                let group = self
                    .descriptor
                    .fork_group()
                    .ok_or_else(|| KernelBehaviorError {
                        detail: "deferred fork has no group".into(),
                    })?;
                self.environment
                    .fork_groups
                    .gate(group, context.actor)
                    .and_then(|gate| gate.mark_ready())
                    .map_err(|error| KernelBehaviorError {
                        detail: error.to_string(),
                    })?;
                return Ok(KernelStep::Continue(()));
            }
            let boot = self.boot.take().ok_or_else(|| KernelBehaviorError {
                detail: "resident actor boot was consumed twice".into(),
            })?;
            self.initialize(kernel, &context, boot)
                .await
                .map_err(Self::failure)
        })
    }

    fn abort_pending_forks<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
    ) -> futures_util::future::BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async move {
            self.abort_unpublished_groups(
                kernel,
                kernel.identity(),
                "host reattached without acknowledging tool completion; queued fork cancelled",
            )
            .await;
            Ok(())
        })
    }

    fn tool_completed<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> futures_util::future::BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            self.abort_incomplete_groups(
                kernel,
                context.actor,
                "fork admission stopped before tool completion",
            )
            .await;
            let groups = self.environment.fork_groups.ready_groups(context.actor);
            let groups: Vec<_> = groups
                .into_iter()
                .filter(|(_, children)| {
                    let actors = self.environment.actors.lock();
                    children.iter().all(|child| {
                        actors.get(child).is_some_and(|record| {
                            record.descriptor.fork_boundary() == Some(&boundary)
                        })
                    })
                })
                .collect();
            if groups.is_empty() {
                return Ok(());
            }
            let children: Vec<_> = {
                let actors = self.environment.actors.lock();
                groups
                    .iter()
                    .flat_map(|(_, children)| children.iter().copied())
                    .filter(|child| {
                        actors
                            .get(child)
                            .is_some_and(|record| record.terminal.is_none())
                            && kernel
                                .resolve(*child)
                                .is_some_and(|child| child.terminal().get().is_none())
                    })
                    .collect()
            };
            let previous = {
                let actors = self.environment.actors.lock();
                children
                    .iter()
                    .map(|child| {
                        (
                            actors[child].descriptor.placement().lexical_scope,
                            if actors[child].descriptor.context_parent().is_some() {
                                crate::ForkContext::InheritedContext
                            } else {
                                crate::ForkContext::SelectedContext
                            },
                        )
                    })
                    .collect()
            };
            let scopes = self
                .environment
                .runner
                .finalize_fork_scopes(context.clone(), previous)
                .await
                .map_err(Self::failure)?;
            for (group, _) in groups {
                self.environment
                    .fork_groups
                    .publish_group(group, context.actor)
                    .map_err(|error| KernelBehaviorError {
                        detail: error.to_string(),
                    })?;
            }
            let mut unused_scopes = Vec::new();
            for (child, scope) in children.into_iter().zip(scopes) {
                let sent = kernel.resolve(child).is_some_and(|child| {
                    child.terminal().get().is_none()
                        && child
                            .address()
                            .send_message(crate::KernelMessage::ReleaseFork { scope })
                            .is_ok()
                });
                if !sent {
                    unused_scopes.push(scope);
                }
            }
            if !unused_scopes.is_empty() {
                self.environment
                    .runner
                    .retire_fork_scopes(context, unused_scopes)
                    .await
                    .map_err(Self::failure)?;
            }
            Ok(())
        })
    }

    fn release_fork<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        scope: tidepool_codegen::scope::ScopeId,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            let boot = self.boot.take().ok_or_else(|| KernelBehaviorError {
                detail: "deferred fork was already released".into(),
            })?;
            self.descriptor.set_lexical_scope(scope);
            let context = self.context(kernel.identity());
            kernel.install_session_context(context.clone())?;
            if let Some(record) = self.environment.actors.lock().get_mut(&context.actor) {
                record.descriptor = self.descriptor.clone();
            }
            self.initialize(kernel, &context, boot)
                .await
                .map_err(Self::failure)
        })
    }

    fn cast<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        sender: ActorRef,
        request: MailboxValue,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            self.input_origin = ActorInputOrigin::ActorMessageFrom(actor_address(sender));
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
            self.input_origin = ActorInputOrigin::ActorMessageFrom(actor_address(caller));
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
                    ResidentStanding::Interactive(_)
                        | ResidentStanding::Receiving(_)
                        | ResidentStanding::Workbench
                )
            {
                return Err(KernelInvocationFailure::Rejected {
                    actor: context.actor,
                    detail: "actor has no active Haskell application workbench".into(),
                });
            }
            let execution = request.execution_id().cloned();
            if let Some(execution) = &execution {
                match self.completed_workbenches.lookup(execution, &request) {
                    Err(()) => {
                        return Err(KernelInvocationFailure::Rejected {
                            actor: context.actor,
                            detail:
                                "one hosted call identity was retried with different Haskell input"
                                    .into(),
                        });
                    }
                    Ok(Some(reply)) => return reply.map(KernelStep::Continue),
                    Ok(None) => {}
                }
            }
            let retained_request = execution.as_ref().map(|_| request.clone());
            self.active_fork_boundary = request.fork_boundary().cloned();
            let result = self.execute_workbench(kernel, &context, request).await;
            self.active_fork_boundary = None;
            match &result {
                Ok(KernelStep::Continue(_)) => self
                    .runtime_observation
                    .publish_workbench_posture(crate::ActorWorkbenchPosture::Idle),
                Ok(
                    KernelStep::ContinueLater(response)
                    | KernelStep::Stop {
                        output: response, ..
                    },
                ) => {
                    let transfer = match response.status {
                        WorkbenchRunStatus::Replied => Some(crate::ActorWorkbenchTransfer::Reply),
                        WorkbenchRunStatus::RequestCancelled => {
                            Some(crate::ActorWorkbenchTransfer::CancellationAcknowledgement)
                        }
                        WorkbenchRunStatus::Committed
                        | WorkbenchRunStatus::Rejected
                        | WorkbenchRunStatus::Completed => None,
                    };
                    self.runtime_observation.publish_workbench_posture(
                        transfer.map_or(crate::ActorWorkbenchPosture::Idle, |transfer| {
                            crate::ActorWorkbenchPosture::TerminalTransfer { transfer }
                        }),
                    );
                }
                Err(_) => self
                    .runtime_observation
                    .publish_workbench_posture(crate::ActorWorkbenchPosture::Failed),
            }
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
            let result = result.map_err(|failure| {
                KernelInvocationFailure::Workbench(crate::KernelWorkbenchFailure {
                    actor: context.actor,
                    receipts: failure.receipts,
                    failed_index: failure.failed_index,
                    total: failure.total,
                    detail: failure.source.to_string(),
                })
            });
            if let (Some(execution), Some(request)) = (execution, retained_request) {
                let reply = match &result {
                    Ok(
                        KernelStep::Continue(response)
                        | KernelStep::ContinueLater(response)
                        | KernelStep::Stop {
                            output: response, ..
                        },
                    ) => Ok(response.clone()),
                    Err(error) => Err(error.clone()),
                };
                self.completed_workbenches.record(execution, request, reply);
            }
            result
        })
    }

    fn route<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        watch: crate::WatchId,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let Some(entry) = self.environment.requests.take_route(context.actor, watch) else {
                return Ok(KernelStep::Continue(()));
            };
            self.active_route = Some((watch, Vec::new()));
            // This internal completion identity gates only selected-context children.
            // It never authorizes or describes a provider-context fork.
            let completion = tidepool_runtime::session::WorkbenchForkBoundary {
                thread_id: format!(
                    "route-owner:{}@{}",
                    context.actor.id.0, context.actor.incarnation.0
                ),
                call_id: format!("route:{}", watch.0),
            };
            self.active_fork_boundary = Some(completion.clone());
            let result = async {
                let mut outcome = self
                    .environment
                    .runner
                    .run_route_entry(context.clone(), entry, watch)
                    .await?;
                loop {
                    let boundary = self
                        .environment
                        .runner
                        .capture_boundary(
                            context.clone(),
                            outcome,
                            context.placement.resource_scope,
                        )
                        .await?;
                    match boundary {
                        ResidentActorBoundary::Completed => {
                            return Ok::<_, ResidentActorWorkbenchError>(false)
                        }
                        ResidentActorBoundary::ReplyAttempt(attempt) => {
                            match self
                                .environment
                                .requests
                                .begin_reply(context.actor, attempt.request)
                            {
                                Ok(()) => {
                                    self.stage_request_reply(
                                        kernel,
                                        &context,
                                        attempt.request,
                                        attempt.result,
                                    )
                                    .await?;
                                    return Ok(true);
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
                                    continue;
                                }
                                Err(error) => {
                                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                                        format!("reply rejected: {error:?}"),
                                    ))
                                }
                            }
                        }
                        _ => {}
                    }
                    outcome = self
                        .resolve_effect(
                            kernel,
                            &context,
                            &crate::CallAncestry::begin(context.actor),
                            boundary,
                        )
                        .await?;
                }
            }
            .await;
            let resume_reply = matches!(result, Ok(true));
            let mut result = result.map(|_| ()).map_err(|error| error.to_string());
            if result.is_ok() {
                result = self
                    .tool_completed(kernel, completion)
                    .await
                    .map_err(|error| error.to_string());
            }
            if result.is_err() {
                let groups = self
                    .active_route
                    .as_ref()
                    .map(|(_, groups)| groups.as_slice())
                    .unwrap_or_default();
                for child in self
                    .environment
                    .fork_groups
                    .abort_selected_unpublished(context.actor, groups)
                {
                    if let Some(child) = kernel.resolve(child) {
                        let _ = child
                            .shutdown(ActorTerminal {
                                kind: ActorExitKind::Cancelled,
                                summary: "route callback failed before publication".into(),
                            })
                            .await;
                    }
                }
            }
            self.environment.requests.abort_unsubmitted(context.actor);
            self.active_route = None;
            self.active_fork_boundary = None;
            let notification = self
                .environment
                .requests
                .finish_route(context.actor, watch, result);
            self.publish_watch_notifications(notification);
            Ok(if resume_reply {
                KernelStep::ContinueLater(())
            } else {
                KernelStep::Continue(())
            })
        })
    }

    fn resume<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            if matches!(self.standing, ResidentStanding::Paused(_)) {
                return Ok(KernelStep::Continue(()));
            }
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
                    self.runtime_observation
                        .publish_workbench_posture(crate::ActorWorkbenchPosture::Idle);
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
                    self.runtime_observation
                        .publish_workbench_posture(crate::ActorWorkbenchPosture::Failed);
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
            let (hook, realm) = self.shutdown_components(kernel, terminal).await;
            for component in [hook, realm] {
                if let crate::CleanupComponentOutcome::Unconfirmed(detail) = component {
                    return Err(KernelBehaviorError { detail });
                }
            }
            Ok(())
        })
    }

    fn shutdown_components<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> futures_util::future::BoxFuture<
        'a,
        (
            crate::CleanupComponentOutcome,
            crate::CleanupComponentOutcome,
        ),
    > {
        Box::pin(async move {
            use crate::CleanupComponentOutcome::{Confirmed, Unconfirmed};
            let staged_replacement = self.replacement_staged();
            self.source_connections.take();
            self.sources.clear();
            let context = self.context(kernel.identity());
            let notifications = self
                .environment
                .requests
                .actor_stopped(context.actor, terminal);
            self.publish_watch_notifications(notifications);
            self.abort_unpublished_groups(
                kernel,
                context.actor,
                "fork owner stopped before tool completion",
            )
            .await;
            let hook = if let Some(hook) = self.shutdown_hook.take() {
                match self
                    .environment
                    .runner
                    .run_shutdown(
                        context.clone(),
                        hook,
                        context.placement.resource_scope,
                        terminal.kind,
                        std::time::Duration::from_secs(30),
                    )
                    .await
                {
                    Ok(()) => Confirmed,
                    Err(error) => Unconfirmed(error.to_string()),
                }
            } else {
                Confirmed
            };
            self.active_input = None;
            self.pending_checkpoint = None;
            self.checkpoint = None;
            self.standing = ResidentStanding::Terminal;
            self.boot = None;
            self.sources.clear();
            let mut retained_errors = Vec::new();
            for retained in std::mem::take(&mut self.retained_replacements) {
                let placement = retained.placement;
                drop(retained);
                if let Err(error) = self
                    .environment
                    .runner
                    .retire_root_placement(placement)
                    .await
                {
                    retained_errors.push(error.to_string());
                }
            }
            // Realm retirement obtains its own exclusive checkout. A failed hook
            // does not skip this safe cleanup; it also never becomes success.
            let realm_result =
                if staged_replacement || self.descriptor.supervisor_parent().is_none() {
                    self.environment
                        .runner
                        .retire_root_placement(self.descriptor.placement())
                        .await
                } else {
                    self.environment
                        .runner
                        .close_realm(context, self.descriptor.placement().resource_scope)
                        .await
                };
            if let Err(error) = realm_result {
                retained_errors.push(error.to_string());
            }
            let realm = if retained_errors.is_empty() {
                Confirmed
            } else {
                Unconfirmed(retained_errors.join("; "))
            };
            (hook, realm)
        })
    }

    fn stopped<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> futures_util::future::BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(custody) = &self.worktree_custody {
                custody.actor_stopped(terminal);
            }
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
            } else if !self.policy_installed {
                self.notify_supervisor(
                    child,
                    self.descriptor.supervisor_parent(),
                    format!(
                        "{child:?} exited {:?}: {}",
                        notice.terminal.kind, notice.terminal.summary
                    ),
                );
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
    spawn_resident_root_in_incarnation(source, root, fork_workspaces, crate::Incarnation::FIRST)
        .await
}

/// Spawn a resident root under one durable local-host incarnation.
pub async fn spawn_resident_root_in_incarnation<H, O>(
    source: ActorWorkbenchSource,
    root: ResidentActorRoot<H, O>,
    fork_workspaces: Option<crate::fork_workspace::SharedForkWorkspaceAdmission>,
    incarnation: crate::Incarnation,
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
    let (forest, receiver) = ResidentForest::new(
        source,
        descriptor.placement().session,
        machine,
        fork_workspaces,
        incarnation,
    );
    let (actor, task) = forest.admit_root(descriptor, outcome).await?;
    Ok((actor, task, receiver))
}

/// A point-in-time graph projection. Parent links are exact incarnation IDs;
/// callers can build a forest without parsing Haskell or display text.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActorGraphNode {
    pub actor: ActorRef,
    pub label: String,
    pub creator: Option<ActorRef>,
    pub supervisor_parent: Option<ActorRef>,
    pub context_parent: Option<ActorRef>,
    pub terminal: Option<ActorTerminal>,
    pub workbench: crate::ActorWorkbenchPosture,
    pub provider_thread: Option<String>,
    pub provider_turn: Option<tidepool_model::ProviderTurnObservation>,
    pub provider_observation_stale: bool,
    pub bound_worktree: Option<String>,
    pub active_requests: Vec<crate::RequestId>,
    pub queued_requests: Vec<crate::RequestId>,
}

/// Host-owned routing and resident execution domain. Every root uses the same
/// machine registry, heap, request registry, lineage and deployment channel.
/// Supervision and resource retirement remain local to each admitted tree.
pub struct ResidentForest<H, O> {
    environment: ResidentEnvironment<H, O>,
    directory: crate::LocalActorDirectory,
    session: tidepool_repr::SessionId,
    incarnation: crate::Incarnation,
}

impl<H, O> ResidentForest<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub fn new(
        source: ActorWorkbenchSource,
        session: tidepool_repr::SessionId,
        machine: ResidentSession<H, O>,
        fork_workspaces: Option<crate::fork_workspace::SharedForkWorkspaceAdmission>,
        incarnation: crate::Incarnation,
    ) -> (Self, mpsc::UnboundedReceiver<LocalResidentDeployment>) {
        Self::new_with_launch_resolver(source, session, machine, fork_workspaces, incarnation, None)
    }

    pub fn new_with_launch_resolver(
        source: ActorWorkbenchSource,
        session: tidepool_repr::SessionId,
        machine: ResidentSession<H, O>,
        fork_workspaces: Option<crate::fork_workspace::SharedForkWorkspaceAdmission>,
        incarnation: crate::Incarnation,
        launch_resolver: Option<crate::WorkerLaunchResolver>,
    ) -> (Self, mpsc::UnboundedReceiver<LocalResidentDeployment>) {
        let machines = Arc::new(ActorMachineRegistry::<H, O>::new());
        machines.insert_idle(session, machine);
        let runner = ResidentActorRunner::new(machines, source);
        let (deployments, receiver) = mpsc::unbounded_channel();
        let environment = ResidentEnvironment {
            commands: Default::default(),
            runner,
            deployments,
            retired: Arc::new(Mutex::new(std::collections::HashSet::new())),
            requests: Arc::new(RequestRegistry::default()),
            fork_groups: crate::ForkGroupRegistry::new(crate::ActorLineageRegistry::default()),
            actors: Arc::new(Mutex::new(std::collections::HashMap::new())),
            fork_workspaces,
            root_admission_closed: Arc::new(tokio::sync::RwLock::new(false)),
            launch_resolver,
        };
        (
            Self {
                environment,
                directory: crate::LocalActorDirectory::default(),
                session,
                incarnation,
            },
            receiver,
        )
    }

    /// Observe only actors the exact requester can inspect. This does not enter
    /// an actor turn or wait for its Haskell workbench, so running work is visible.
    pub fn inspect_graph(&self, requester: ActorRef) -> Option<Vec<ActorGraphNode>> {
        if self
            .directory
            .resolve(requester)?
            .terminal()
            .get()
            .is_some()
        {
            return None;
        }
        let records = self.environment.actors.lock();
        if !records.contains_key(&requester) {
            return None;
        }
        let mut nodes = records
            .iter()
            .filter(|(actor, _)| actor_can_observe(requester, **actor, &records))
            .map(|(actor, record)| {
                let runtime = record.runtime_observation.snapshot();
                let (active_requests, queued_requests) =
                    self.environment.requests.work_for_target(*actor);
                ActorGraphNode {
                    actor: *actor,
                    label: record.descriptor.label().to_owned(),
                    creator: record.descriptor.creator(),
                    supervisor_parent: record.descriptor.supervisor_parent(),
                    context_parent: record.descriptor.context_parent(),
                    terminal: record.terminal.clone().or_else(|| {
                        self.directory
                            .resolve(*actor)
                            .and_then(|a| a.terminal().get())
                    }),
                    workbench: runtime.workbench_posture,
                    provider_thread: runtime.provider_thread,
                    provider_turn: runtime.provider_turn,
                    provider_observation_stale: runtime.provider_observation_stale,
                    bound_worktree: record.bound_worktree.clone(),
                    active_requests,
                    queued_requests,
                }
            })
            .collect::<Vec<_>>();
        nodes.sort_by_key(|node| (node.actor.id, node.actor.incarnation));
        Some(nodes)
    }

    pub async fn new_program_root(
        &self,
        label: String,
        role: crate::EffectiveRole,
        compiled: Arc<tidepool_runtime::session::CompiledTurn>,
    ) -> Result<
        (LocalActorRef, ractor::concurrency::JoinHandle<()>),
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let admission = self.environment.root_admission_closed.read().await;
        if *admission {
            return Err(std::io::Error::other("swarm root admission is closed").into());
        }
        let (placement, outcome) = self
            .environment
            .runner
            .prepare_root_program(self.session, compiled)
            .await?;
        match self
            .admit_prepared_root(
                ActorDescriptor::new(label, placement).with_effective_role(role),
                outcome,
                &admission,
            )
            .await
        {
            Ok(root) => Ok(root),
            Err(error) => {
                self.environment
                    .runner
                    .retire_root_placement(placement)
                    .await?;
                Err(Box::new(error))
            }
        }
    }

    pub async fn shutdown(&self) {
        *self.environment.root_admission_closed.write().await = true;
        let roots = self
            .environment
            .actors
            .lock()
            .iter()
            .filter(|(_, record)| record.descriptor.supervisor_parent().is_none())
            .filter_map(|(actor, _)| self.directory.resolve(*actor))
            .collect::<Vec<_>>();
        for root in roots {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                root.shutdown(ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "forest host shutdown".into(),
                }),
            )
            .await;
            if !matches!(result, Ok(Ok(_))) {
                root.address().kill();
            }
        }
    }

    /// Provision a host-authorized workbench without a provider attachment.
    pub async fn new_workbench(
        &self,
        label: String,
        role: crate::EffectiveRole,
    ) -> Result<LocalActorRef, Box<dyn std::error::Error + Send + Sync>> {
        let admission = self.environment.root_admission_closed.read().await;
        if *admission {
            return Err(std::io::Error::other("swarm root admission is closed").into());
        }
        let placement = self
            .environment
            .runner
            .provision_root_scope(self.session)
            .await?;
        let descriptor = ActorDescriptor::new(label, placement).with_effective_role(role);
        let mut behavior = ResidentKernelBehavior::with_boot(
            descriptor,
            self.environment.clone(),
            ResidentBoot::Workbench,
            Vec::new(),
        );
        behavior.forest_control = true;
        match crate::local_actor::spawn_local_actor_in_directory(
            None,
            behavior,
            self.incarnation,
            self.directory.clone(),
        )
        .await
        {
            Ok((actor, _task)) => Ok(actor),
            Err(error) => {
                self.environment
                    .runner
                    .retire_root_placement(placement)
                    .await?;
                Err(Box::new(error))
            }
        }
    }

    /// Admit a prepared independent root. Its continuation and scopes must have
    /// been prepared in this forest's machine, just as for a child entry.
    pub async fn admit_root(
        &self,
        descriptor: ActorDescriptor,
        outcome: ResidentOutcome,
    ) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr> {
        let admission = self.environment.root_admission_closed.read().await;
        if *admission {
            return Err(ractor::SpawnErr::StartupFailed(
                std::io::Error::other("swarm root admission is closed").into(),
            ));
        }
        self.admit_prepared_root(descriptor, outcome, &admission)
            .await
    }

    async fn admit_prepared_root(
        &self,
        descriptor: ActorDescriptor,
        outcome: ResidentOutcome,
        _admission: &tokio::sync::RwLockReadGuard<'_, bool>,
    ) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr> {
        if descriptor.placement().session != self.session
            || descriptor.supervisor_parent().is_some()
            || descriptor.context_parent().is_some()
        {
            return Err(ractor::SpawnErr::StartupFailed(Box::new(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "forest root must be independent and belong to the forest machine",
                ),
            )));
        }
        let behavior =
            ResidentKernelBehavior::prepared(descriptor, self.environment.clone(), outcome);
        crate::local_actor::spawn_local_actor_in_directory(
            None,
            behavior,
            self.incarnation,
            self.directory.clone(),
        )
        .await
    }
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
        installed_bindings: Vec::new(),
        operations: Vec::new(),
        terminal_transfer: None,
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
    use super::{
        workbench_failure_after_operations, workbench_response, ChildExitObservations,
        CompletedWorkbenchExecutions,
    };
    use crate::{ActorId, ActorRef, Incarnation};
    use tidepool_runtime::session::{
        WorkbenchExecutionId, WorkbenchItemReceipt, WorkbenchItemStatus,
        WorkbenchOperationDisposition, WorkbenchOperationId, WorkbenchOperationReceipt,
        WorkbenchRequest, WorkbenchResponse, WorkbenchRunStatus,
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
        let committed = WorkbenchItemReceipt {
            index: 0,
            status: WorkbenchItemStatus::Committed,
            output: "[bound prior]".into(),
            warnings: Vec::new(),
            installed_bindings: vec!["prior".into()],
            operations: Vec::new(),
            terminal_transfer: None,
        };
        let response = workbench_response(
            WorkbenchRunStatus::Rejected,
            vec![
                committed.clone(),
                WorkbenchItemReceipt {
                    index: 1,
                    status: WorkbenchItemStatus::Rejected,
                    output: "<input unit 2>: runtime error: pattern match failure: Just x".into(),
                    warnings: Vec::new(),
                    installed_bindings: Vec::new(),
                    operations: Vec::new(),
                    terminal_transfer: None,
                },
            ],
            1,
            4,
        );
        assert_eq!(response.items.len(), 4);
        assert_eq!(response.items[0], committed);
        assert_eq!(response.items[1].status, WorkbenchItemStatus::Rejected);
        assert!(response.items[1].installed_bindings.is_empty());
        assert_eq!(response.items[2].status, WorkbenchItemStatus::NotRun);
        assert_eq!(response.items[3].status, WorkbenchItemStatus::NotRun);
        assert!(response.items[2..]
            .iter()
            .all(|item| item.installed_bindings.is_empty()));
    }

    #[test]
    fn actor_owned_workbench_retry_returns_only_the_exact_committed_call() {
        let execution = WorkbenchExecutionId::from_digest([7; 16]);
        let request = WorkbenchRequest::from_ghci_input("effectfulAction")
            .unwrap()
            .with_execution_id(execution.clone());
        let reply = Ok(WorkbenchResponse {
            status: WorkbenchRunStatus::Committed,
            items: Vec::new(),
            next_index: 1,
            total: 1,
        });
        let mut completed = CompletedWorkbenchExecutions::default();
        completed.record(execution.clone(), request.clone(), reply.clone());

        assert_eq!(completed.lookup(&execution, &request), Ok(Some(reply)));
        let different = WorkbenchRequest::from_ghci_input("differentAction")
            .unwrap()
            .with_execution_id(execution.clone());
        assert_eq!(completed.lookup(&execution, &different), Err(()));
        assert_eq!(
            completed.lookup(&WorkbenchExecutionId::from_digest([8; 16]), &request),
            Ok(None)
        );
    }

    #[test]
    fn prepared_operations_never_escape_a_failed_unit() {
        let execution = WorkbenchExecutionId::from_digest([9; 16]);
        let failure = workbench_failure_after_operations(
            &[],
            0,
            1,
            crate::ResidentActorWorkbenchError::ActorProtocol("publish failed".into()),
            vec![WorkbenchOperationReceipt {
                id: WorkbenchOperationId {
                    execution,
                    input_unit_index: 0,
                    effect_ordinal: 0,
                },
                effect: "commit context-fork group".into(),
                disposition: WorkbenchOperationDisposition::Prepared,
            }],
        );
        assert_eq!(
            failure.receipts[0].operations[0].disposition,
            WorkbenchOperationDisposition::Unknown
        );
    }
}
