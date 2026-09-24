//! Resident Haskell behavior owned directly by one local actor.
//!
//! Ractor serializes logical turns and owns the mailbox. The shared resident
//! machine registry owns only short-lived machine checkout. This module is
//! the single driver between those boundaries; it does not recreate registry
//! turn leases, parked-obligation maps, or a host-side scheduler.

use std::sync::Arc;

mod commands;
mod replacement;
mod status_rendering;
mod workbench_ledger;

use status_rendering::{render_bindings_section, render_job_line, render_source_drift_section};
use workbench_ledger::{WorkbenchBoundaryRecord, WorkbenchExecutions, WorkbenchReplayFailure};

use parking_lot::Mutex;
use tidepool_bridge_effects::CommandPresentation;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_runtime::session::{
    truncate_preview_at_line, CellSourceSpan, OutputSink, ParsedBlock, ResidentHole,
    ResidentOutcome, ResidentSession, RootCustody, TurnKind, WorkbenchCellItemKind,
    WorkbenchCellSourceItem, WorkbenchExecutionId, WorkbenchFailureLayer, WorkbenchItemReceipt,
    WorkbenchItemStatus, WorkbenchOperationDisposition, WorkbenchOperationId,
    WorkbenchOperationReceipt, WorkbenchRequest, WorkbenchResponse, WorkbenchRunStatus,
    WorkbenchTerminalTransfer,
};
use tokio::sync::mpsc;
use tracing::Instrument;

use crate::mailbox::{InstalledReceiver, ResidentOutbound};
use crate::request::RequestRegistry;
use crate::resident_workbench::{
    ForkGroupBoundary, PreparedCell, ResidentActorBoundary, ResidentActorStartupStep,
    ResidentKernelBoundary, ResidentWorkbenchFragment, ResidentWorkbenchStep,
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
    pub model: Option<crate::Model>,
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
    PolicyInstalled(Box<LocalResidentInstallation>),
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
    SettlementChanged {
        notification: crate::request::SettlementNotification,
    },
    RequestCancellation {
        notification: crate::RequestCancellationNotification,
    },
    Retired {
        actor: ActorRef,
        terminal: ActorTerminal,
    },
    /// A supervisor stopped `actor` and waits for its interactive resources
    /// (process, pane, tool service, socket, workspace view) to be released.
    /// The host answers once its cleanup receipt exists; a dropped reply means
    /// no host tracks resources for this actor.
    ReleaseAwait(Arc<ReleaseAwait>),
}

impl LocalResidentDeployment {
    /// The variant name, for diagnostics that must say what arrived.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::CommandBackend(_) => "CommandBackend",
            Self::NotificationSend(_) => "NotificationSend",
            Self::NotificationPoll(_) => "NotificationPoll",
            Self::PolicyInstalled(_) => "PolicyInstalled",
            Self::SessionReady { .. } => "SessionReady",
            Self::RequestUpdate { .. } => "RequestUpdate",
            Self::ChildExited { .. } => "ChildExited",
            Self::WatchChanged { .. } => "WatchChanged",
            Self::SettlementChanged { .. } => "SettlementChanged",
            Self::RequestCancellation { .. } => "RequestCancellation",
            Self::Retired { .. } => "Retired",
            Self::ReleaseAwait(_) => "ReleaseAwait",
        }
    }
}

/// One supervisor's wait for a stopped actor's release receipt. The host
/// answers at most once; a dropped request answers nobody.
pub struct ReleaseAwait {
    pub actor: ActorRef,
    reply: Mutex<Option<tokio::sync::oneshot::Sender<ResourceRelease>>>,
}

impl ReleaseAwait {
    /// Deliver the host's answer. Returns false when the waiter is gone.
    pub fn answer(&self, release: ResourceRelease) -> bool {
        self.reply
            .lock()
            .take()
            .is_some_and(|reply| reply.send(release).is_ok())
    }
}

impl std::fmt::Debug for ReleaseAwait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReleaseAwait")
            .field("actor", &self.actor)
            .finish_non_exhaustive()
    }
}

/// Host-side outcome of releasing one stopped actor's interactive resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceRelease {
    /// Every cleanup component settled.
    Released,
    /// The actor is stopped but some resources stay retained; the text names
    /// each component and why.
    Retained(String),
}

/// How long a stop waits for the host's release receipt before answering
/// `StoppedReleasing`. Retirement usually settles in a few seconds; the
/// workspace step may wait longer for a running host Git command.
const RELEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// Bound on the deployment observer channel. A deployment's events come from
/// per-actor lifecycle transitions (install, session-ready, retire) plus
/// request-update/watch/settlement traffic proportional to concurrently
/// in-flight requests; observed deployments run at most a few dozen actors
/// with a handful of requests each in flight at once, so this is roughly an
/// order of magnitude of headroom over a realistic burst, not a routine
/// operating point. It exists to cap memory under a stalled or absent
/// observer, not to apply steady-state backpressure. Most producers below use
/// `try_send` and treat a full channel exactly like a closed one, because
/// each of those sites already has a defined fallback for "no observer" and
/// nothing is silently and invisibly lost. `WatchChanged`/`SettlementChanged`
/// (`publish_watch_notifications`) are the exception: the consumer is the
/// only path a settled reply reaches the owning actor's inbox through, so
/// those two use the channel's real backpressure (`send(...).await`) instead
/// of `try_send`, and only a closed channel drops them.
const DEPLOYMENT_CHANNEL_CAPACITY: usize = 256;

struct ResidentEnvironment<H, O> {
    runner: ResidentActorRunner<H, O>,
    deployments: mpsc::Sender<LocalResidentDeployment>,
    retired: Arc<Mutex<std::collections::HashSet<ActorRef>>>,
    requests: Arc<RequestRegistry>,
    commands: crate::command_jobs::CommandJobs,
    fork_groups: crate::ForkGroupRegistry,
    actors: Arc<Mutex<std::collections::HashMap<ActorRef, ResidentActorRecord>>>,
    fork_workspaces: Option<crate::fork_workspace::SharedForkWorkspaceAdmission>,
    root_admission_closed: Arc<tokio::sync::RwLock<bool>>,
    launch_resolver: Option<crate::WorkerLaunchResolver>,
    /// Installed by a host that keeps a source layer per checkout. Without
    /// one every actor compiles against exactly the deployment-wide roots.
    source_layers: Option<crate::ActorSourceLayerResolver>,
    jev: crate::JevBackendHandle,
    /// Set by an actor host that answers `ReleaseAwait`; without one a stop
    /// has no interactive resources to wait for.
    release_tracked: Arc<std::sync::atomic::AtomicBool>,
    conversation_reader: Option<crate::ConversationReader>,
    usage_pointers: crate::UsagePointerTable,
    recovery: Option<Arc<crate::ActorRecoveryJournal>>,
}

#[derive(Clone)]
struct ResidentActorRecord {
    recovery_claimed: bool,
    workbench_executions: Arc<Mutex<WorkbenchExecutions>>,
    forest_control: bool,
    interactive_policy_installed: bool,
    observation_roots: std::collections::HashSet<ActorRef>,
    descriptor: ActorDescriptor,
    bound_worktree: Option<String>,
    terminal: Option<ActorTerminal>,
    runtime_observation: crate::ActorRuntimeObservationHandle,
    /// Scheduler ownership can differ from retained logical parentage after recovery.
    scheduler_root: bool,
}

#[derive(tidepool_bridge_derive::ToHaskell)]
enum ObservationShareResult {
    #[haskell(module = "Tidepool.Effects.Core", name = "ObservationShared")]
    Shared,
    #[haskell(
        module = "Tidepool.Effects.Core",
        name = "ObservationRecipientUnavailable"
    )]
    RecipientUnavailable,
    #[haskell(module = "Tidepool.Effects.Core", name = "ObservationScopeUnavailable")]
    ScopeUnavailable,
    #[haskell(module = "Tidepool.Effects.Core", name = "ObservationUnauthorized")]
    Unauthorized,
}

/// Why a watch was refused, said in terms of the resource state.
fn watch_registration_refusal(error: crate::request::ReplyError) -> String {
    use crate::request::ReplyError;
    let detail = match error {
        ReplyError::Unauthorized | ReplyError::WrongIncarnation => {
            "a request this watch names is not available to this actor"
        }
        ReplyError::Stale => {
            "a request this watch names is gone: it has already settled and been forgotten, \
             or it was never registered here"
        }
        ReplyError::CancellationRequested => {
            "this actor or the actor it waits on is being cleaned up, so no new watch is \
             admitted"
        }
        ReplyError::AlreadySettled => "a request this watch names has already settled",
        ReplyError::UpdatePending => "a request this watch names has an update in flight",
    };
    format!("watch registration was rejected: {detail}")
}

/// Phrase a unix-ms timestamp relative to this actor's own session start,
/// the same "+Xm Ys into your session" idiom the facade uses for queued
/// watch/settlement notices, so a model reads one consistent clock.
fn elapsed_into_session(launched_at_unix_ms: Option<i64>, occurred_at_unix_ms: u64) -> String {
    match launched_at_unix_ms
        .and_then(|start| i64::try_from(occurred_at_unix_ms).ok()?.checked_sub(start))
        .filter(|elapsed| *elapsed >= 0)
    {
        Some(ms) => format!(
            "+{}m{:02}s into your session",
            ms / 60_000,
            (ms / 1000) % 60
        ),
        None => "elapsed time unavailable".to_owned(),
    }
}

/// Pure rendering for the `status` tool's `watches` view: one line per
/// retained watch (id, label, state, registration/transition times) plus
/// every response still pending (id, label, registration time), so a model
/// can see everything it is waiting on without compiling a `pollWatch`
/// cell. Kept as a free function over an already-fetched
/// [`crate::request::WatchesOverview`] so it is testable without a full
/// actor/kernel fixture, the same way this module tests other rendering and
/// channel mechanics by reducing them to what a production call site
/// already does.
fn render_watches_view(
    launched_at_unix_ms: Option<i64>,
    overview: &crate::request::WatchesOverview,
) -> String {
    let mut lines = vec![format!("watches ({} total):", overview.watches.len())];
    if overview.watches.is_empty() {
        lines.push("  (none registered)".to_owned());
    }
    for watch in &overview.watches {
        let transitioned = watch.transitioned_at_unix_ms.map_or_else(
            || "not yet transitioned".to_owned(),
            |transitioned_at| elapsed_into_session(launched_at_unix_ms, transitioned_at),
        );
        lines.push(format!(
            "  - watch {} {:?}: {} registered {} transitioned {}",
            watch.id.0,
            watch.label,
            watch.state,
            elapsed_into_session(launched_at_unix_ms, watch.registered_at_unix_ms),
            transitioned,
        ));
    }
    lines.push(format!(
        "pending responses ({} total):",
        overview.pending_responses.len()
    ));
    if overview.pending_responses.is_empty() {
        lines.push("  (none pending)".to_owned());
    }
    for response in &overview.pending_responses {
        lines.push(format!(
            "  - request {} {:?}: registered {}",
            response.id.0,
            response.label,
            elapsed_into_session(launched_at_unix_ms, response.registered_at_unix_ms),
        ));
    }
    lines.push(
        "Reading a settled VALUE still needs pollWatch (watches) or pollResponse \
         (plain requests); this view only reports status, not values."
            .to_owned(),
    );
    lines.join("\n")
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

/// Refines a bare `Pending` observation into `Starting` while the host is
/// still launching the request target's provider application (the host's
/// `launch_pending` phase on the target's runtime observation); otherwise
/// fills its `PendingProgress` with that same target's current lifecycle,
/// provider health, and last observed activity, so a caller holding the
/// result has nothing a re-poll would add.
fn starting_observation<H, O>(
    environment: &ResidentEnvironment<H, O>,
    request: crate::RequestId,
    observation: crate::ResponseObservation,
) -> crate::ResponseObservation {
    let crate::ResponseObservation::Pending(base) = observation else {
        return observation;
    };
    let Some(target) = environment.requests.target_for(request) else {
        return crate::ResponseObservation::Pending(base);
    };
    enrich_pending_progress(environment, target, base)
}

/// Fills in the actor-runtime half of `PendingProgress` (lifecycle, provider
/// health, last activity) for the given target, leaving the registry-owned
/// half (`progress_revision`, `watched`) from `base` untouched. Refines to
/// `Starting` instead while the host is still launching the target's
/// provider application.
fn enrich_pending_progress<H, O>(
    environment: &ResidentEnvironment<H, O>,
    target: crate::ActorRef,
    base: crate::PendingProgress,
) -> crate::ResponseObservation {
    let records = environment.actors.lock();
    let Some(record) = records.get(&target) else {
        return crate::ResponseObservation::Pending(base);
    };
    let runtime = record.runtime_observation.snapshot();
    if let Some(phase) = &runtime.launch_pending {
        return crate::ResponseObservation::Starting(format!(
            "actor {}@{} admitted; provider not started ({phase})",
            target.id.0, target.incarnation.0
        ));
    }
    crate::ResponseObservation::Pending(crate::PendingProgress {
        actor_terminal: record.terminal.clone(),
        provider_turn: runtime.provider_turn.clone(),
        last_activity_unix_ms: runtime
            .provider_usage
            .last()
            .map(|sample| sample.observed_at_unix_ms),
        ..base
    })
}

/// The watch analogue of `starting_observation`: fills a pending watch
/// observation's `PendingProgress` with the runtime state of its first
/// unsettled dependency's target actor. A watch has no launch-phase
/// refinement of its own; a dependency still launching simply reports that
/// actor's current lifecycle like any other pending target.
fn watch_pending_observation<H, O>(
    environment: &ResidentEnvironment<H, O>,
    watch: crate::WatchId,
    observation: crate::WatchObservation,
) -> crate::WatchObservation {
    let crate::WatchObservation::Pending(base) = observation else {
        return observation;
    };
    let Some(target) = environment.requests.watch_pending_target(watch) else {
        return crate::WatchObservation::Pending(base);
    };
    let records = environment.actors.lock();
    let Some(record) = records.get(&target) else {
        return crate::WatchObservation::Pending(base);
    };
    let runtime = record.runtime_observation.snapshot();
    crate::WatchObservation::Pending(crate::PendingProgress {
        actor_terminal: record.terminal.clone(),
        provider_turn: runtime.provider_turn.clone(),
        last_activity_unix_ms: runtime
            .provider_usage
            .last()
            .map(|sample| sample.observed_at_unix_ms),
        ..base
    })
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
            source_layers: self.source_layers.clone(),
            jev: Arc::clone(&self.jev),
            release_tracked: Arc::clone(&self.release_tracked),
            conversation_reader: self.conversation_reader.clone(),
            usage_pointers: self.usage_pointers.clone(),
            recovery: self.recovery.clone(),
        }
    }
}

enum ResidentBoot {
    Workbench,
    Prepared(Box<ResidentOutcome>),
    Entry(RootCustody),
    Replacement(Box<replacement::PreparedSuccessor>),
}

/// A lightweight, cloneable record of a typed request presented to this
/// actor and not yet settled by a reply or cancellation acknowledgement.
/// Tracked independently of `standing` (which owns the actual suspended
/// continuation, in `ResidentStanding::Interactive`) so workbench and lookup
/// selection can keep serving `respond`/`sessionReply`/`sessionInput`
/// bindings for the request even if `standing` itself has moved on to
/// `Receiving` — the receive loop's own next-message wait does not by
/// itself mean the earlier request was answered.
#[derive(Clone)]
struct OutstandingInteractive {
    response: crate::ResponseExpectation,
    request: crate::RequestId,
    type_modules: Vec<String>,
}

impl OutstandingInteractive {
    fn new(request: &crate::interactive_session::InteractiveSessionRequest) -> Self {
        Self {
            response: request.response.clone(),
            request: request.request,
            type_modules: request.type_modules(),
        }
    }
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

impl ResidentStanding {
    /// A short label and the request this standing itself is tracking, if
    /// any (an `Interactive` standing only — `Receiving` and the rest never
    /// embed a request). Used only for status text and standing-transition
    /// logging; `outstanding_interactive` on the actor is the source of
    /// truth for whether a reply is owed.
    fn describe(&self) -> (&'static str, Option<crate::RequestId>) {
        match self {
            Self::Workbench => ("workbench", None),
            Self::Boot => ("boot", None),
            Self::Receiving(_) => ("receiving", None),
            Self::Tools(_) => ("tools", None),
            Self::Interactive(awaiting) => ("interactive", Some(awaiting.request.request)),
            Self::Terminal => ("terminal", None),
            Self::Paused(_) => ("paused", None),
        }
    }
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

#[derive(Clone, tidepool_bridge_derive::ToHaskell)]
#[allow(
    clippy::enum_variant_names,
    reason = "variant names are the wire truth: ToHaskell encodes them verbatim \
              as the matching Haskell constructor names, so the shared \
              `Actor` prefix must stay exactly as spelled, not be trimmed"
)]
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
    let failure_layer = resident_actor_failure_layer(&source);
    let mut receipts = completed.to_vec();
    if !operations.is_empty() {
        receipts.push(WorkbenchItemReceipt {
            diagnostics: Vec::new(),
            index: failed_index,
            kind: None,
            span: None,
            source_items: Vec::new(),
            status: WorkbenchItemStatus::Rejected,
            // The failure's own diagnostic lives on `source`/`detail`
            // instead of here; this field otherwise stays empty. The one
            // exception is a short, human-facing framing sentence for the
            // failure layer — the one piece of the failure this item alone
            // can say plainly, since a reader sees this receipt without
            // necessarily reading `detail`.
            output: failure_layer_output_hint(failure_layer),
            warnings: Vec::new(),
            installed_bindings: Vec::new(),
            operations,
            terminal_transfer: None,
            failure_layer,
        });
    }
    WorkbenchExecutionFailure {
        receipts,
        failed_index,
        total,
        source,
    }
}

/// A short, human-facing framing sentence for a failure layer — the tool
/// text this item's own (otherwise empty) `output` can say plainly, matching
/// what the layer means on [`WorkbenchFailureLayer`]. `Compile`/`None` add
/// nothing: a compile rejection already carries its own text, and an
/// unclassified failure has nothing this function can say honestly.
fn failure_layer_output_hint(layer: Option<WorkbenchFailureLayer>) -> String {
    match layer {
        Some(WorkbenchFailureLayer::Observation) => {
            "effects committed; observing the result failed".to_owned()
        }
        Some(WorkbenchFailureLayer::Effect) => {
            "an effect failed, or did not finish committing before the unit ended".to_owned()
        }
        Some(WorkbenchFailureLayer::Compile) | None => String::new(),
    }
}

/// Which failure layer produced `error`, for a receipt built from it.
/// `Compile`/`CellCheck`/`CompileInfrastructure` never ran an effect at all;
/// `Resident`/`Delivered` wrap a [`tidepool_runtime::session::ResidentError`],
/// which already distinguishes an effect failure from an observation one —
/// see [`tidepool_runtime::session::ResidentError::failure_layer`]. A
/// `Delivered` error whose inner error that classification does not cover is
/// still known to be post-commit (its doc: the response was already handed
/// to the machine before this failed), so it defaults to `Effect` rather
/// than staying unclassified.
/// A reload receipt leads with where it ended and how long it took. The
/// lines beneath say how far it got; a reader deciding what to do next should
/// not have to read them to learn the outcome.
fn reload_receipt(outcome: &str, started: std::time::Instant, lines: Vec<String>) -> String {
    format!(
        "outcome: {outcome} ({:.1}s)\n{}",
        started.elapsed().as_secs_f64(),
        lines.join("\n")
    )
}

fn resident_actor_failure_layer(
    error: &ResidentActorWorkbenchError,
) -> Option<WorkbenchFailureLayer> {
    match error {
        ResidentActorWorkbenchError::Compile(_)
        | ResidentActorWorkbenchError::CellCheck(_)
        | ResidentActorWorkbenchError::CompileInfrastructure(_) => {
            Some(WorkbenchFailureLayer::Compile)
        }
        ResidentActorWorkbenchError::Resident(inner) => inner.failure_layer(),
        ResidentActorWorkbenchError::Delivered(inner) => Some(
            inner
                .failure_layer()
                .unwrap_or(WorkbenchFailureLayer::Effect),
        ),
        _ => None,
    }
}

fn record_workbench_operation(
    operations: &mut Vec<WorkbenchOperationReceipt>,
    execution: Option<&WorkbenchExecutionId>,
    input_unit_index: usize,
    effect_ordinal: usize,
    effect: &str,
    elapsed: std::time::Duration,
    disposition: WorkbenchOperationDisposition,
) {
    // Every effect boundary funnels through here to record its outcome —
    // this is the one place, not the many `resolve_effect`/`resolve_command`
    // arms above, that "no effect type can go untraced" is enforced. A
    // hosted tool call's effects (`execution` is `Some`) get their "effect
    // settled" line later, batched with the rest of the cell's receipt (see
    // the loop over `item.operations` in `execute_workbench`). An S1 slot's
    // own effects (`execution` is `None`: a slot invocation is not itself a
    // call the provider sees, so that later batching never runs for it)
    // would otherwise report nothing at all past "effect boundary
    // captured" — so they get their settlement, with timing, right here.
    if execution.is_none() {
        tracing::info!(
            input_unit_index,
            ordinal = effect_ordinal,
            effect = %effect,
            elapsed_ms = elapsed.as_millis(),
            disposition = ?disposition,
            "effect settled"
        );
    }
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

/// Classify a `resolve_effect` failure that is not a command boundary (a
/// command computes its own disposition in `resolve_command` and that value
/// always wins over this one via the `command_disposition.unwrap_or(..)`
/// callers below).
///
/// Every non-command arm of `resolve_effect` produces its response and then
/// hands it to the resident machine through exactly one of
/// `ResidentSession::resume`, `resume_handle`, or `resume_framed_custody`
/// (see the `resume_*`/`abort_live` helpers on `ResidentMachineAccess` in
/// `resident_workbench.rs`). Those three calls are fused with driving the
/// resumed fragment onward to its next suspension or completion, so a
/// failure returned from one of them does not mean delivery failed — it can
/// equally mean delivery succeeded and something LATER in that same
/// resumption failed. `ResidentActorWorkbenchError::Delivered` is raised
/// only at those three call sites (never before), so it proves the response
/// already crossed into the machine: the operation committed even though
/// the error propagates. Every other error variant here happened before or
/// during delivery, so it keeps the conservative `Unknown` disposition,
/// which documents (workbench.rs:265-268) "the effect owner failed after
/// dispatch without proving whether its mutation crossed the commit point".
fn disposition_for_non_command_failure(
    error: &ResidentActorWorkbenchError,
) -> WorkbenchOperationDisposition {
    match error {
        ResidentActorWorkbenchError::Delivered(_) => WorkbenchOperationDisposition::Committed,
        _ => WorkbenchOperationDisposition::Unknown,
    }
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
    named_tool: bool,
    operations: &'a mut Vec<WorkbenchOperationReceipt>,
    display_remaining: &'a mut usize,
    command_output: &'a mut Vec<String>,
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
    compiled_tools: Option<crate::resident_workbench::ResidentWorkbenchTools>,
    /// How many specs this incarnation has installed. `policy_installed` stays
    /// the first-install latch; a reload is a second, explicit path that
    /// replaces `compiled_tools` and advances this.
    spec_installs: u64,
    /// Every after-tool invocation this actor has made, and what became of it.
    /// An abstention's reason lives here and nowhere else.
    after_tool: crate::after_tool::AfterToolLog,
    /// Set while the after-tool slot is running. A slot's own effects and tool
    /// use never trigger a slot, so a broken slot can never block its own
    /// repair.
    after_tool_active: bool,
    forest_control: bool,
    pending_program: Option<ResidentOutcome>,
    pending_reply: Option<crate::RequestId>,
    /// The bounded reply-value preview `stage_request_reply` obtained for
    /// `pending_reply`, if any -- carried to the settlement notice minted
    /// once the resumed continuation settles (`resume`'s `finish_reply`
    /// call). `None` either because the reply could not be previewed or
    /// because no reply is pending.
    pending_reply_preview: Option<String>,
    pending_cancellation: Option<crate::RequestId>,
    suspended_cast: Option<SuspendedCast>,
    /// The request `standing == Interactive` is presenting, tracked
    /// independently of `standing` itself. See [`OutstandingInteractive`].
    outstanding_interactive: Option<OutstandingInteractive>,
    child_exit_observations: ChildExitObservations,
    deferred_child_failures: Vec<ChildExitNotice>,
    next_activation_sequence: u64,
    runtime_observation: crate::ActorRuntimeObservationHandle,
    workbench_executions: Arc<Mutex<WorkbenchExecutions>>,
    active_route: Option<(crate::WatchId, Vec<crate::ForkGroupId>)>,
    active_fork_boundary: Option<tidepool_runtime::session::WorkbenchForkBoundary>,
    active_workbench_control: Option<Arc<crate::resident_tools::WorkbenchExecutionControl>>,
    settled_fork_boundaries: Vec<tidepool_runtime::session::WorkbenchForkBoundary>,
    pending_fork_publications: Vec<PendingForkPublication>,
}

struct PendingForkPublication {
    boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    groups: Vec<crate::ForkGroupId>,
    releases: Vec<(ActorRef, tidepool_codegen::scope::ScopeId)>,
    unused_scopes: Vec<tidepool_codegen::scope::ScopeId>,
    published: bool,
}

impl<H, O> ResidentKernelBehavior<H, O> {
    /// A fork refusal, with the full coordinator named by its path.
    ///
    /// Every ancestor's ceiling counts its whole subtree, so the coordinator
    /// that is full is often not the one forking. It is carried as an
    /// incarnation, which an agent cannot act on: it reasons in paths.
    /// Resolve it here, where the actor records are in reach, and fall back to
    /// the short form when the record is gone.
    fn name_coordinator(&self, error: crate::lineage::ForkGroupError) -> String {
        use crate::lineage::ForkGroupError;
        let ForkGroupError::DescendantBudgetExceeded {
            coordinator,
            requested,
            active,
            maximum,
        } = error
        else {
            return error.to_string();
        };
        let named = self
            .environment
            .actors
            .lock()
            .get(&coordinator)
            .and_then(|record| record.descriptor.actor_path())
            .map_or_else(|| coordinator.to_string(), |path| path.to_string());
        format!(
            "fork group would exceed the active descendant ceiling of coordinator {named} \
             ({requested} requested, {active} already active or reserved in its subtree, \
             maximum {maximum})"
        )
    }

    fn pending_in_tool_block(&self, target: ActorRef) -> bool {
        self.active_fork_boundary.is_some() && self.environment.fork_groups.is_pending_child(target)
    }

    /// Replace `standing`, logging the transition. The sole place `standing`
    /// changes so every from/to pair, and the request either side is
    /// tracking, is visible without instrumenting each call site by hand.
    fn set_standing(&mut self, actor: ActorRef, next: ResidentStanding) -> ResidentStanding {
        let previous = std::mem::replace(&mut self.standing, next);
        let (from, from_request) = previous.describe();
        let (to, to_request) = self.standing.describe();
        tracing::info!(
            ?actor,
            from,
            ?from_request,
            to,
            ?to_request,
            "resident actor standing transition"
        );
        previous
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
            compiled_tools: None,
            spec_installs: 0,
            after_tool: crate::after_tool::AfterToolLog::default(),
            after_tool_active: false,
            forest_control: false,
            pending_program: None,
            pending_reply: None,
            pending_reply_preview: None,
            pending_cancellation: None,
            suspended_cast: None,
            outstanding_interactive: None,
            child_exit_observations: ChildExitObservations::default(),
            deferred_child_failures: Vec::new(),
            next_activation_sequence: 1,
            runtime_observation: crate::ActorRuntimeObservationHandle::default(),
            workbench_executions: Arc::default(),
            active_route: None,
            active_fork_boundary: None,
            active_workbench_control: None,
            settled_fork_boundaries: Vec::new(),
            pending_fork_publications: Vec::new(),
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
        // best-effort: the deployment observer channel may have no listener
        // (e.g. shut down); a missed PolicyInstalled event has no correctness
        // effect since the policy flag above is already committed to state.
        self.environment
            .deployments
            .try_send(LocalResidentDeployment::PolicyInstalled(Box::new(
                installation,
            )))
            .ok();
    }

    /// The actor whose native application services this actor's commands.
    /// A record actor has no native process of its own; its commands run as
    /// the nearest creator (or supervisor) that does, in that ancestor's
    /// checkout and namespace — the same place the ancestor's own `Cmd.run`
    /// would run. Falls back to the actor itself when no such ancestor is
    /// known, and the host then reports the command unavailable.
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
            .try_send(LocalResidentDeployment::NotificationSend(Arc::new(command)))
            .is_err()
        {
            tracing::error!(actor = ?source, "actor failure notice could not reach inbox owner");
        }
    }

    fn publish_retired(&self, actor: ActorRef, terminal: ActorTerminal) {
        if let Some(recovery) = &self.environment.recovery {
            if let Err(error) = recovery.retire(actor, terminal.kind, terminal.summary.clone()) {
                // The actor is already terminal. Retain the earlier admission
                // as active so restart reconciliation remains conservative.
                tracing::error!(?actor, %error, "actor terminal evidence remains uncertain");
            }
        }
        self.environment.fork_groups.retire_actor(actor);
        if let Some(record) = self.environment.actors.lock().get_mut(&actor) {
            record.terminal = Some(terminal.clone());
        }
        if self.environment.retired.lock().insert(actor) {
            // best-effort: deployment observer channel may have no listener.
            self.environment
                .deployments
                .try_send(LocalResidentDeployment::Retired { actor, terminal })
                .ok();
        }
    }

    /// Project a stop the supervisor just requested. The actor is already
    /// terminal; the projection reports whether the host has also released
    /// its interactive resources, so a receipt never reads as final while a
    /// workspace view or process is still retained.
    async fn stopped_projection(
        &self,
        actor: ActorRef,
    ) -> crate::resident_workbench::AgentStopProjection {
        use crate::resident_workbench::AgentStopProjection;
        if !self
            .environment
            .release_tracked
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return AgentStopProjection::StoppedNow;
        }
        let (reply, release) = tokio::sync::oneshot::channel();
        let request = Arc::new(ReleaseAwait {
            actor,
            reply: Mutex::new(Some(reply)),
        });
        if self
            .environment
            .deployments
            .try_send(LocalResidentDeployment::ReleaseAwait(request))
            .is_err()
        {
            return AgentStopProjection::StoppedNow;
        }
        match tokio::time::timeout(RELEASE_WAIT, release).await {
            Ok(Ok(ResourceRelease::Released)) | Ok(Err(_)) => AgentStopProjection::StoppedNow,
            Ok(Ok(ResourceRelease::Retained(detail))) => {
                AgentStopProjection::StoppedRetaining(detail)
            }
            Err(_) => AgentStopProjection::StoppedReleasing,
        }
    }

    /// Unlike the other `deployments` producers in this file, a `WatchChanged`
    /// or `SettlementChanged` notice is not merely observer traffic: the
    /// `deployments` consumer is the sole path that turns a durably-settled
    /// reply into a row in the owning actor's own inbox (see
    /// `publish_inbox_event_for` in `bridge/facade/src/actor_host.rs`).
    /// `try_send(...).ok()` on a full channel would silently discard a
    /// settled reply with nothing left to reconstruct it from but a manual
    /// `status` poll. Use the bounded channel's real backpressure
    /// (`send(...).await`) instead so a transient burst waits rather than
    /// drops; only a closed channel (no consumer left at all) is unrecoverable.
    async fn publish_watch_notifications(
        &self,
        notifications: impl IntoIterator<Item = crate::request::WatchNotification>,
    ) {
        for notification in notifications {
            // A watch forgotten by campaign cleanup keeps no state to poll.
            // Delivering a transition for it would send its owner to
            // `pollWatch`, which can only answer `WatchUnavailable
            // (WatchRejected ReplyStale)` — a notice about nothing.
            if !self
                .environment
                .requests
                .retains_watch(notification.owner, notification.watch)
            {
                continue;
            }
            let owner = notification.owner;
            let watch = notification.watch;
            match self
                .environment
                .deployments
                .send(LocalResidentDeployment::WatchChanged { notification })
                .await
            {
                Ok(()) => tracing::info!(
                    actor = ?owner,
                    watch = ?watch,
                    kind = "watch_changed",
                    outcome = "sent",
                    "publishing watch notice"
                ),
                Err(_closed) => tracing::warn!(
                    actor = ?owner,
                    watch = ?watch,
                    kind = "watch_changed",
                    outcome = "closed",
                    "publishing watch notice: deployment observer channel has no consumer"
                ),
            }
        }
        for notification in self.environment.requests.take_settlement_notifications() {
            let owner = notification.owner;
            let request = notification.request;
            let preview_len = notification.reply_preview.as_ref().map(String::len);
            match self
                .environment
                .deployments
                .send(LocalResidentDeployment::SettlementChanged { notification })
                .await
            {
                Ok(()) => tracing::info!(
                    actor = ?owner,
                    request = ?request,
                    kind = "settlement_changed",
                    preview_len,
                    outcome = "sent",
                    "publishing settlement notice"
                ),
                Err(_closed) => tracing::warn!(
                    actor = ?owner,
                    request = ?request,
                    kind = "settlement_changed",
                    preview_len,
                    outcome = "closed",
                    "publishing settlement notice: deployment observer channel has no consumer"
                ),
            }
        }
    }

    fn publish_request_cancellation(
        &self,
        notification: Option<crate::RequestCancellationNotification>,
    ) {
        if let Some(notification) = notification {
            // best-effort: deployment observer channel may have no listener.
            self.environment
                .deployments
                .try_send(LocalResidentDeployment::RequestCancellation { notification })
                .ok();
        }
    }

    fn status_text(&self, kernel: &KernelContext, actor: ActorRef, view: StatusView) -> String {
        if view == StatusView::Watches {
            return self.watches_status_text(actor);
        }
        let (standing, current_request) = match &self.standing {
            ResidentStanding::Workbench => ("operator-workbench", None),
            ResidentStanding::Boot => ("booting", None),
            ResidentStanding::Receiving(_) => (
                "receiving",
                self.outstanding_interactive
                    .as_ref()
                    .map(|outstanding| outstanding.request),
            ),
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
                runtime.prompt_catalog_version,
                runtime.prompt_fingerprint,
                runtime.backend_executable,
                runtime.backend_version,
                runtime.requested_model,
                runtime.requested_effort,
                runtime.provider_turn,
                runtime.provider_observation_stale,
                runtime.confirmed_model,
                runtime.confirmed_effort,
            )
        } else {
            String::new()
        };
        let current = if view == StatusView::Concise {
            format!(
                "actor {:?} ({}@{})\n  activation={:?} application={} program={standing} current_request={current_request:?}\n  responses: ready={:?} unavailable={} pending={:?}\n  watches: ready={:?} unavailable={} pending={:?}\n  role={:?} workspace={:?} descendant_depth={} active_children={} bound_worktree={:?} workbench={:?}{}",
                self.descriptor.label(),
                actor.id.0,
                actor.incarnation.0,
                runtime.activation_kind,
                if self.policy_installed {
                    "attached"
                } else {
                    "detached"
                },
                requests.ready_responses,
                unavailable_responses,
                requests.pending_responses,
                requests.ready_watches,
                unavailable_watches,
                requests.pending_watches,
                self.descriptor.effective_role().role(),
                self.descriptor.effective_role().workspace(),
                self.descriptor.effective_role().descendants().maximum_depth,
                crate::render_child_budget(
                    self.descriptor
                        .effective_role()
                        .descendants()
                        .maximum_active_children,
                ),
                self.launch_worktrees.first(),
                runtime.workbench_posture,
                roster_summary,
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
        // Discovery is implicit, so the resolved rule and file are reported
        // rather than left to be guessed. A concise view stays concise; every
        // wider view names the spec that is actually live.
        let spec = match (view, self.compiled_tools.as_ref()) {
            (StatusView::Concise, _) => String::new(),
            (_, Some(tools)) => format!(
                "\n  spec: {} slots=[{}]",
                tools.provenance(),
                tools.slots.join(", ")
            ),
            (_, None) => "\n  spec: none installed".to_string(),
        };
        // Where an abstention's reason lives, and where a failure line's
        // reference points. Never repeated into the results themselves.
        let after_tool = match view {
            StatusView::Concise => String::new(),
            _ => match self.after_tool.rows() {
                rows if rows.is_empty() => String::new(),
                rows => format!("\n  after-tool:\n    {}", rows.join("\n    ")),
            },
        };
        let status = format!(
            "{current}{failure}{spec}{after_tool}\n  {workspace}\n  deadlines: [{}]\nactors:\n{}",
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

    /// The `status` tool's `watches` view: one line per retained watch (id,
    /// label, state, and when it registered/last transitioned, phrased
    /// relative to this actor's own session start) followed by every
    /// response still pending, so a model can see everything it is waiting
    /// on without compiling a `pollWatch` cell. Settled *values* still
    /// require `pollWatch`/`pollResponse`; this view only reports status.
    fn watches_status_text(&self, actor: ActorRef) -> String {
        let launched_at = self.runtime_observation.snapshot().launched_at_unix_ms;
        let overview = self.environment.requests.watches_overview(actor);
        render_watches_view(launched_at, &overview)
    }

    /// The what-is-live status view: collectors and the command jobs they
    /// watch (finished or not), and persistent bindings with the session
    /// generation that defines them, the exact source of their defining
    /// cell, the execution id that submitted it, and any same-session name
    /// the source mentions that is no longer live.
    ///
    /// `bindings` is fetched by the caller through
    /// `ResidentActorWorkbench::live_bindings` — the only part of this view
    /// that needs the live machine; everything else here reads
    /// `self.environment.commands` (command jobs) and
    /// `self.workbench_executions` (this actor's own replay journal),
    /// neither of which this view creates. See `status_rendering` for the
    /// pure helpers this leans on.
    ///
    /// Source drift (is what is running still what is on disk) is published
    /// from `tidepool`'s composition root into
    /// `ActorRuntimeObservation::source_drift`, since the data — the source
    /// layer's active/disk revision, the checkout's Git state, and the
    /// frozen workspace's — lives in `tidepool::exomonad::source` and
    /// `tidepool` depends on `exomonad-actor` (see `tidepool/Cargo.toml`),
    /// never the reverse. See `status_rendering::render_source_drift_section`.
    /// One piece is left out rather than
    /// guessed at even there: no build script or embedded string anywhere in
    /// this system records the running binary's build revision, so the
    /// checkout row reports Git's own head and dirty files and says plainly
    /// that the binary-revision comparison is unavailable, rather than
    /// inventing one. Which agent-spec revision each actor activated is
    /// omitted for a similar reason: no such tracking exists in
    /// `exomonad-actor` today, and this view does not invent any.
    fn live_status_text(
        &self,
        actor: ActorRef,
        bindings: &[tidepool_runtime::session::WorkbenchBinding],
    ) -> String {
        let records = self.environment.actors.lock();
        let mut jobs = self
            .environment
            .commands
            .snapshot()
            .into_iter()
            .filter(|job| {
                actor_can_observe(actor, job.owner, &records)
                    || job
                        .observers
                        .iter()
                        .any(|observer| actor_can_observe(actor, *observer, &records))
            })
            .map(|job| render_job_line(&job))
            .collect::<Vec<_>>();
        drop(records);
        jobs.sort();
        let jobs = if jobs.is_empty() {
            "  (no retained command jobs)".to_owned()
        } else {
            jobs.join("\n")
        };

        let journal = self.workbench_executions.lock();
        let executions = journal.terminal_entries();
        let binding_lines = render_bindings_section(bindings, &executions);
        let source_drift =
            render_source_drift_section(&self.runtime_observation.snapshot().source_drift);

        format!(
            "actor {}@{} what-is-live\ncollectors:\n{jobs}\nbindings:\n{binding_lines}\nsource drift:\n{source_drift}",
            actor.id.0, actor.incarnation.0,
        )
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
    Watches,
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// The workbench a cell, tool call, or lookup should run against right
    /// now, or `None` when nothing is installed to run one. A pending typed
    /// request's `respond`/`sessionReply`/`sessionInput`/`reportProgress`
    /// bindings are owed for as long as `outstanding_interactive` is set,
    /// independent of `standing`: a native delivery or mailbox turn that
    /// advances `standing` past `Interactive` to `Receiving` does not by
    /// itself mean the request was answered. The sole selection point for
    /// both [`Self::execute_workbench`] and lookup resolution so the two
    /// cannot drift.
    fn active_workbench(&self) -> Option<crate::ResidentActorWorkbench<H, O>> {
        let live_standing = !matches!(
            self.standing,
            ResidentStanding::Terminal | ResidentStanding::Paused(_) | ResidentStanding::Boot
        );
        if let Some(outstanding) = self
            .outstanding_interactive
            .as_ref()
            .filter(|_| live_standing)
        {
            return Some(self.environment.runner.workbench(
                outstanding.response.clone(),
                outstanding.request,
                outstanding.type_modules.clone(),
            ));
        }
        match &self.standing {
            ResidentStanding::Workbench | ResidentStanding::Receiving(_)
                if self.policy_installed =>
            {
                Some(self.environment.runner.application_workbench())
            }
            _ => None,
        }
    }

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
                // Same discipline as `publish_watch_notifications`: this is
                // the sole path a deadline's watch transition reaches the
                // owning actor's inbox through, so a full channel must wait
                // rather than silently drop it.
                let owner = notification.owner;
                let watch = notification.watch;
                match deployments
                    .send(LocalResidentDeployment::WatchChanged { notification })
                    .await
                {
                    Ok(()) => tracing::info!(
                        actor = ?owner,
                        watch = ?watch,
                        kind = "watch_changed",
                        outcome = "sent",
                        "publishing deadline watch notice"
                    ),
                    Err(_closed) => tracing::warn!(
                        actor = ?owner,
                        watch = ?watch,
                        kind = "watch_changed",
                        outcome = "closed",
                        "publishing deadline watch notice: deployment observer channel has no consumer"
                    ),
                }
            }
            if let Some(notification) = cancellation {
                // best-effort: deployment observer channel may have no listener.
                deployments
                    .try_send(LocalResidentDeployment::RequestCancellation { notification })
                    .ok();
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
                                // A child already gone from a failed fork-group admission is
                                // the common case here; log anything else so an actor that
                                // refused shutdown does not silently linger.
                                if let Err(error) = child
                                    .shutdown(ActorTerminal {
                                        kind: ActorExitKind::Cancelled,
                                        summary: "fork group admission failed".into(),
                                    })
                                    .await
                                {
                                    tracing::warn!(child = ?child.identity(), %error, "fork-group child did not shut down");
                                }
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
        if descriptor.model().is_none() {
            descriptor = descriptor.with_model(self.descriptor.model().cloned());
        }
        if descriptor.fork_effort().is_none() {
            descriptor = descriptor.with_fork_effort(self.descriptor.fork_effort());
        }
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
        // The checkout this child is launched with is settled here — a
        // context-fork workspace has already been admitted above — so this is
        // the last moment before the child exists, and the only honest place
        // to fix its search path. `base_include` is deployment-wide and shared
        // by every actor in the forest; an actor's OWN layer travels on its
        // descriptor instead, and neither ever moves afterwards.
        let source_layers = self.environment.source_layers.clone();
        if let Some(layers) = &source_layers {
            descriptor = descriptor.with_source_layer(layers.layer_include(&launch_worktrees));
        }
        let allocated_label = descriptor.label().to_string();
        let admitted_worktree = prepared_workspace
            .as_ref()
            .map(|prepared| prepared.handle().clone());
        let bound_worktrees = launch_worktrees.clone();
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
        // The child now has a principal, so the layer its descriptor carries
        // can be named as its own. This happens before the child runs, so its
        // first cell already reaches its own layer and no other.
        if let Some(layers) = &source_layers {
            layers.bind(child.identity().into(), &bound_worktrees);
        }
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

    /// Upper bound, in characters, on the reply-value preview a settlement
    /// notice carries -- generous enough for an ordinary reply record, small
    /// enough that a notice never dwarfs the wake it accompanies.
    const SETTLEMENT_REPLY_PREVIEW_CHAR_BUDGET: usize = 2048;

    /// Both authored tool replies and route callbacks resume the one active
    /// request continuation, then hand it back to the ordinary actor scheduler.
    async fn stage_request_reply(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        request: crate::RequestId,
        result: RootCustody,
        carried_preview: Option<String>,
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
                    tracing::info!(
                        actor = ?context.actor,
                        from = "interactive",
                        ?request,
                        to = "boot",
                        "resident actor standing transition"
                    );
                    awaiting
                }
                standing => {
                    self.standing = standing;
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "reply did not match an active request continuation".into(),
                    ));
                }
            };
            // Best-effort: a settlement notice that carries the reply data
            // saves the owner a `pollResponse` compile just to read it. The
            // preview never blocks or fails the reply itself -- an unreadable
            // shape (a function, an exhausted budget) simply omits it. The
            // replying Haskell program already rendered a preview through
            // `WorkbenchDisplay` (readable even for a `Text` field, unlike
            // this session's own non-forcing heap walk); prefer that,
            // enforcing this session's own line-boundary budget on it, and
            // fall back to the retained-heap walk only when it is absent.
            let (result, reply_preview) = match carried_preview {
                Some(carried) if !carried.is_empty() => (
                    result,
                    Some(truncate_preview_at_line(
                        carried,
                        Self::SETTLEMENT_REPLY_PREVIEW_CHAR_BUDGET,
                    )),
                ),
                _ => match self
                    .environment
                    .runner
                    .preview_retained(
                        context.clone(),
                        result,
                        Self::SETTLEMENT_REPLY_PREVIEW_CHAR_BUDGET,
                    )
                    .await
                {
                    Ok(pair) => pair,
                    Err(error) => {
                        self.standing = ResidentStanding::Interactive(awaiting);
                        return Err(error);
                    }
                },
            };
            if reply_preview.is_none() {
                tracing::debug!(
                    request = request.0,
                    "reply preview unavailable; settlement notice will fall back to `pollResponse` guidance"
                );
            }
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
            // The reply landed: this request no longer owes `respond`
            // bindings, whatever `standing` now reads while the resumed
            // program is stabilized.
            self.outstanding_interactive = None;
            self.pending_program = Some(outcome);
            self.pending_reply = Some(request);
            self.pending_reply_preview = reply_preview;
            Ok(())
        }
        .await;
        if let Err(error) = &settled {
            let notifications = self
                .environment
                .requests
                .fail_reply_settlement(request, error.to_string());
            self.publish_watch_notifications(notifications).await;
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
            ResidentActorBoundary::Console { continuation, text } => Box::pin(async move {
                tracing::debug!(actor = ?context.actor, output = %crate::workbench_display::bounded_output(&text, 8192), "actor console");
                self.environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
            }),
            ResidentActorBoundary::Jev {
                continuation,
                request,
            } => Box::pin(async move {
                let backend = Arc::clone(&self.environment.jev);
                // The packet is opaque JSON from here: `Jev.Operators` on the
                // Haskell side already carries the questions, labels and
                // (on the way back) likelihoods. Full bodies are debug-only
                // and bounded; `info` stays one compact line either way.
                tracing::debug!(
                    actor = %context.actor,
                    packet = %crate::workbench_display::bounded_output(&request, 4096),
                    "jev call packet"
                );
                let started = std::time::Instant::now();
                let answer = backend.ask(request).await;
                let elapsed_ms = started.elapsed().as_millis();
                crate::call_timing::add_jev_ms(elapsed_ms);
                match &answer {
                    Ok(body) => {
                        tracing::debug!(
                            actor = %context.actor,
                            answer = %crate::workbench_display::bounded_output(body, 4096),
                            "jev call answer"
                        );
                        tracing::info!(actor = %context.actor, elapsed_ms, "jev call answered");
                    }
                    Err(failure) => {
                        tracing::info!(
                            actor = %context.actor,
                            ?failure,
                            elapsed_ms,
                            "jev call failed"
                        );
                    }
                }
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, answer)
                    .await
            }),
            ResidentActorBoundary::Sleep {
                continuation,
                duration,
            } => Box::pin(async move {
                // Mailbox handlers and direct local workbenches have no hosted
                // evaluation to cancel, but still share the actor-owned timer
                // and retirement path.
                let control = self
                    .active_workbench_control
                    .clone()
                    .unwrap_or_else(crate::resident_tools::WorkbenchExecutionControl::untracked);
                control.arm_sleep();
                let timer = tokio::time::sleep(duration);
                tokio::pin!(timer);
                tokio::select! {
                    () = &mut timer => {
                        if control.claim_expiry() {
                            let outcome = self.environment
                                .runner
                                .resume_unit(context.clone(), continuation)
                                .await;
                            control.finish_sleep();
                            outcome
                        } else {
                            let (outcome, consumed) = self.environment
                                .runner
                                .abort_live(
                                    context.clone(),
                                    continuation,
                                    "sleep interrupted by delivered input".into(),
                                )
                                .await;
                            if consumed {
                                control.acknowledge_cancellation();
                            }
                            outcome
                        }
                    }
                    () = control.wait_for_cancellation() => {
                        let (outcome, consumed) = self.environment
                            .runner
                            .abort_live(
                                context.clone(),
                                continuation,
                                "sleep interrupted by delivered input".into(),
                            )
                            .await;
                        if consumed {
                            control.acknowledge_cancellation();
                        }
                        outcome
                    }
                    terminal = kernel.wait_requested_shutdown() => {
                        if control.request_cancellation() || control.cancellation_requested() {
                            let (outcome, consumed) = self.environment
                                .runner
                                .abort_live(
                                    context.clone(),
                                    continuation,
                                    format!("sleep interrupted by actor retirement: {}", terminal.summary),
                                )
                                .await;
                            if consumed {
                                control.acknowledge_cancellation();
                            }
                            outcome
                        } else {
                            let outcome = self.environment
                                .runner
                                .resume_unit(context.clone(), continuation)
                                .await;
                            control.finish_sleep();
                            outcome
                        }
                    }
                }
            }),
            ResidentActorBoundary::Command {
                continuation,
                request,
            } => Box::pin(async move {
                let started = std::time::Instant::now();
                let resolution = self
                    .resolve_command(kernel, context, continuation, request)
                    .await;
                crate::call_timing::add_exec_ms(started.elapsed().as_millis());
                resolution.outcome
            }),
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
            ResidentActorBoundary::Lookup {
                continuation,
                request,
            } => Box::pin(async move {
                // Use the same selection as workbench cell execution: a
                // lookup issued while a typed request is outstanding must
                // see `respond`/`sessionReply`/`sessionInput`/
                // `reportProgress` as bound, not report them missing.
                self.active_workbench()
                    .unwrap_or_else(|| self.environment.runner.application_workbench())
                    .resume_lookup(
                        context.clone(),
                        continuation,
                        request,
                        self.environment.usage_pointers.clone(),
                    )
                    .await
            }),
            ResidentActorBoundary::Introspection {
                continuation,
                query,
                kind,
            } => Box::pin(async move {
                self.environment
                    .runner
                    .application_workbench()
                    .resume_structured_introspection(context.clone(), continuation, query, kind)
                    .await
            }),
            ResidentActorBoundary::ReflectConversation {
                continuation,
                count,
            } => Box::pin(async move {
                // The reader is called with the executing actor and nothing
                // else, so a caller cannot reach another conversation.
                let outcome = match self.environment.conversation_reader.clone() {
                    Some(reader) => {
                        reader(context.actor, crate::conversation::requested(count)).await
                    }
                    None => Err(crate::ConversationUnavailable::Unbound),
                };
                self.environment
                    .runner
                    .resume_value(
                        context.clone(),
                        continuation,
                        crate::conversation::reflection(outcome),
                    )
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
                        #[allow(
                            clippy::expect_used,
                            reason = "the RecipientUnavailable branch above already \
                                      returned unless records.get(&recipient) is Some, \
                                      and `records` is the same lock held throughout"
                        )]
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
                        Ok(notifications) => {
                            self.publish_watch_notifications(notifications).await;
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
                            self.stopped_projection(stop.target).await
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
                    self.publish_watch_notifications(forgotten.watch_notifications)
                        .await;
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
                                self.stopped_projection(actor).await
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
                            Ok(notifications) => {
                                self.publish_watch_notifications(notifications).await;
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
                                let launch = self
                                    .environment
                                    .launch_resolver
                                    .as_ref()
                                    .map(|resolve| {
                                        resolve(&crate::WorkerLaunchRequest {
                                            role,
                                            model: model
                                                .or_else(|| self.descriptor.model().cloned()),
                                            effort: effort.or(self.descriptor.fork_effort()),
                                            context: fork_context,
                                            instructions,
                                        })
                                    })
                                    .transpose()?;
                                Ok((
                                    (
                                        row,
                                        i64::from(budget.maximum_depth),
                                        budget.maximum_active_children.map(i64::from),
                                    ),
                                    launch,
                                ))
                            })
                            .and_then(|value| value)
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
                    let maximum = budget.maximum_active_children.map(usize::from);
                    let (group_id, reservations) = match self.active_fork_boundary.clone() {
                        Some(boundary) => self.environment.fork_groups.begin_at_boundary(
                            context.actor,
                            group,
                            branches,
                            maximum,
                            boundary,
                        ),
                        None => self.environment.fork_groups.begin(
                            context.actor,
                            group,
                            branches,
                            maximum,
                        ),
                    }
                    .map_err(|error| self.name_coordinator(error))?;
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
                                    // A child already gone from a failed fork-group admission is
                                    // the common case here; log anything else so an actor that
                                    // refused shutdown does not silently linger.
                                    if let Err(error) = child
                                        .shutdown(ActorTerminal {
                                            kind: ActorExitKind::Cancelled,
                                            summary: "fork group admission failed".into(),
                                        })
                                        .await
                                    {
                                        tracing::warn!(child = ?child.identity(), %error, "fork-group child did not shut down");
                                    }
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
                        // A child already gone from a failed fork-group admission is
                        // the common case here; log anything else so an actor that
                        // refused shutdown does not silently linger.
                        if let Err(error) = child
                            .shutdown(ActorTerminal {
                                kind: ActorExitKind::Cancelled,
                                summary: "fork group admission aborted".into(),
                            })
                            .await
                        {
                            tracing::warn!(child = ?child.identity(), %error, "fork-group child did not shut down");
                        }
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
                        // reply may follow cutover and must retain its owned handle.
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
                // A `Notifications` send otherwise leaves no trace beyond
                // the recipient's inbox — most pointedly a slot's own send
                // (a watchdog escalating to its parent, in the shipped
                // worked example: `after_tool_slot` is on the span stack, so
                // it need not be named again here), which is exactly the
                // judgment this actor made. Traced the same whether it came
                // from a slot body or ordinary tool-body code: `from_slot`
                // is the one thing that distinguishes them.
                tracing::info!(
                    actor = %context.actor,
                    target = %target,
                    from_slot = self.after_tool_active,
                    reason = %crate::workbench_display::bounded_output(&message, 1024),
                    "actor notification sent"
                );
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
                        .try_send(LocalResidentDeployment::NotificationSend(Arc::new(command)))
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
                            .try_send(LocalResidentDeployment::NotificationPoll(Arc::new(command)))
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
                let request = self.environment.requests.reserve_labeled_with_reporting(
                    context.actor,
                    reservation.target,
                    reservation.label,
                    reservation.notify_owner,
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
                        self.publish_watch_notifications(notifications).await;
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
                    self.publish_watch_notifications(notifications).await;
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
                    .observe_response(context.actor, poll.request)
                    .map(|observation| {
                        starting_observation(&self.environment, poll.request, observation)
                    });
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
                let published =
                    self.environment
                        .requests
                        .publish_progress(context.actor, request, value);
                let outcome = match published {
                    Ok((revision, notifications)) => {
                        self.publish_watch_notifications(notifications).await;
                        Ok(revision)
                    }
                    Err(error) => Err(error),
                };
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
                        if let Err(error) = self
                            .environment
                            .deployments
                            .try_send(LocalResidentDeployment::RequestUpdate { delivery })
                        {
                            // A full channel and a closed one both mean the
                            // deployment owner cannot observe this update
                            // right now; treat them alike rather than
                            // silently keeping the update queued unseen.
                            if let LocalResidentDeployment::RequestUpdate { delivery } =
                                error.into_inner()
                            {
                                if let Some(presentation) = delivery.begin() {
                                    presentation
                                        .not_presented("deployment owner unavailable".into());
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
                let abandoned = self
                    .environment
                    .requests
                    .abandon_response(context.actor, abandonment.request);
                let projected = match abandoned {
                    Ok((outcome, notifications)) => {
                        self.publish_watch_notifications(notifications).await;
                        Ok(outcome)
                    }
                    Err(error) => Err(error),
                };
                self.environment
                    .runner
                    .resume_abandonment(context.clone(), abandonment.continuation, projected)
                    .await
            }),
            ResidentActorBoundary::ResponseForget(forget) => Box::pin(async move {
                let forgotten = self
                    .environment
                    .requests
                    .forget_response(context.actor, forget.request);
                let outcome = match forgotten {
                    Ok((outcome, notifications)) => {
                        self.publish_watch_notifications(notifications).await;
                        Ok(outcome)
                    }
                    Err(error) => Err(error),
                };
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
                self.publish_watch_notifications(notifications).await;
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
                        ResidentActorWorkbenchError::ActorProtocol(watch_registration_refusal(
                            error,
                        ))
                    })?;
                self.publish_watch_notifications(notifications).await;
                self.environment
                    .runner
                    .resume_int(context.clone(), registration.continuation, watch.0)
                    .await
            }),
            ResidentActorBoundary::WatchPoll(poll) => Box::pin(async move {
                let observation = self
                    .environment
                    .requests
                    .observe_watch(context.actor, poll.watch)
                    .map(|observation| {
                        watch_pending_observation(&self.environment, poll.watch, observation)
                    });
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
                    self.set_standing(context.actor, ResidentStanding::Terminal);
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
                    if let Some(outstanding) = &self.outstanding_interactive {
                        // The receive loop is advancing to its next native
                        // message while a typed request presented earlier is
                        // still unsettled (no reply, no cancellation
                        // acknowledgement). `outstanding_interactive` is
                        // tracked independently of `standing` precisely so
                        // this is not a silent loss: workbench and lookup
                        // selection keep serving that request's
                        // `respond`/`sessionReply`/`sessionInput` bindings
                        // until it settles, whatever `standing` reads.
                        tracing::info!(
                            actor = ?context.actor,
                            request = ?outstanding.request,
                            "resident actor receive loop advanced with an interactive request still outstanding"
                        );
                    }
                    self.set_standing(context.actor, ResidentStanding::Receiving(receiver));
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
                            model: self.descriptor.model().cloned(),
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
                    self.set_standing(context.actor, ResidentStanding::Tools(awaiting));
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
                    )
                    .await?;
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
        let (input_preview, reply_preview) = workbench
            .mount_activation_input(
                context.clone(),
                request.input_type.clone(),
                input,
                request.response.expected_type().to_owned(),
                request.response.declaration.clone(),
                request.response.declaration_modules.clone(),
            )
            .await?;
        let contract = crate::interactive_session::ActivationContract {
            input_type: request.input_type.clone(),
            response: request.response.clone(),
            input_preview,
            reply_preview,
            siblings: request.siblings.clone(),
        };
        let request_message =
            contract.message(request.request, request.initial_user_message.as_deref());
        self.install_interactive_policy(kernel, context, Some(request_message.clone()))
            .await?;
        self.outstanding_interactive = Some(OutstandingInteractive::new(&request));
        self.set_standing(
            context.actor,
            ResidentStanding::Interactive(crate::interactive_session::ResidentInteractiveAwait {
                request,
                hole,
            }),
        );
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
            // best-effort: deployment observer channel may have no listener.
            self.environment
                .deployments
                .try_send(LocalResidentDeployment::SessionReady { activation })
                .ok();
        }
        Ok(InteractivePark::Parked)
    }

    /// Rebuild this actor's spec and swap the retained record between calls.
    ///
    /// Four steps, and the receipt says which one it ended at, because they
    /// really can end in different places. Publishing the layer while the spec
    /// itself fails to compile is a real outcome, not an error: the revision is
    /// live for this actor's later cells, so a model repairing its spec can
    /// import and exercise the new modules from a cell while the spec does not
    /// yet compile, and this actor keeps serving the record it already has.
    ///
    /// Step three refuses rather than asks. The tool bridge registers a tool
    /// list once and serves it read-only for the life of that registration, so
    /// no confirmation could make a changed surface work; a changed surface
    /// takes effect at the actor's next incarnation.
    async fn reload_agent_spec(
        &mut self,
        context: &ActorSessionContext,
        also_check: &[String],
    ) -> String {
        let Some(active) = self.compiled_tools.as_ref() else {
            return "this actor installed no agent spec, so there is nothing to reload."
                .to_string();
        };
        let resolved = active.resolved.clone();
        let started = std::time::Instant::now();
        let mut receipt = vec![format!("spec: {}", resolved.describe())];

        // The spec module was found by convention and is in no configured
        // module list, so the reload adds it: a spec that fails to compile
        // must fail its own reload rather than surface later at an unrelated
        // call.
        let mut checked: Vec<String> = also_check.to_vec();
        if let Some(module) = resolved.checked_module() {
            if !checked.contains(&module) {
                checked.push(module);
            }
        }

        let Some(layers) = self.environment.source_layers.clone() else {
            receipt.push(
                "layer: this host installs no source layers, so nothing was republished.".into(),
            );
            return reload_receipt("unavailable", started, receipt);
        };
        // The layer is the one the host bound to THIS principal when the actor
        // was admitted. A reload is scoped to the actor that asked and never
        // upgrades a child.
        match layers.reload(tidepool_repr::PrincipalId::from(context.actor), &checked) {
            crate::SourceLayerReload::Unavailable(detail) => {
                receipt.push(format!("layer: {detail}"));
                return reload_receipt("unavailable", started, receipt);
            }
            crate::SourceLayerReload::Rejected {
                active,
                rejected,
                diagnostics,
            } => {
                receipt.push(format!(
                    "layer: rejected. {active} is still active; {rejected} did not typecheck. \
                     Your edited files are on disk exactly as you wrote them, and the previous \
                     spec is still serving calls.\n{diagnostics}"
                ));
                return reload_receipt("rejected", started, receipt);
            }
            crate::SourceLayerReload::Unchanged { revision } => {
                receipt.push(format!("layer: unchanged at {revision}."));
            }
            crate::SourceLayerReload::Published {
                previous,
                revision,
                changed,
            } => {
                receipt.push(format!(
                    "layer: published {revision} over {previous}; changed {}.",
                    if changed.is_empty() {
                        "nothing".to_string()
                    } else {
                        changed.join(", ")
                    }
                ));
            }
        }

        let install = self.spec_installs + 1;
        let prepare_started = std::time::Instant::now();
        let candidate = self
            .environment
            .runner
            .application_workbench()
            .prepare_tools(context.clone(), install)
            .await;
        tracing::info!(
            actor = %context.actor,
            phase = "reload",
            install,
            elapsed_ms = prepare_started.elapsed().as_millis(),
            success = candidate.is_ok(),
            "agent spec preparation"
        );
        let candidate = match candidate {
            Ok(Some(candidate)) => candidate,
            Ok(None) => {
                receipt.push(
                    "spec: the resolved entry named nothing to install; the previous record is \
                     still active."
                        .into(),
                );
                return reload_receipt("nothing to install", started, receipt);
            }
            Err(error) => {
                receipt.push(format!(
                    "spec: the install fragment did not compile against the new revision, so the \
                     previous record is still active. The layer above WAS published, so your \
                     cells already see the edited modules.\n{error}"
                ));
                return reload_receipt("spec did not compile", started, receipt);
            }
        };

        let Some(active) = self.compiled_tools.as_ref() else {
            receipt
                .push("spec: the active record vanished mid-reload; nothing was swapped.".into());
            return reload_receipt("not swapped", started, receipt);
        };
        let changes =
            exomonad_tool::surface::compare_surfaces(&active.declarations, &candidate.declarations);
        if !changes.is_empty() {
            receipt.push(format!(
                "refused: the rebuilt spec declares a different surface, and the tool list was \
                 registered once for this session. The previous record is still serving calls; \
                 a changed surface takes effect at your next incarnation.\n{}",
                exomonad_tool::surface::describe_changes(&changes)
            ));
            return reload_receipt("refused", started, receipt);
        }

        receipt.push(format!(
            "swapped: install {install} now serves later calls ({}). A call already accepted \
             keeps the implementation it started with.",
            candidate.provenance()
        ));
        if !candidate.slots.is_empty() {
            receipt.push(format!("slots: {}", candidate.slots.join(", ")));
        }
        self.spec_installs = install;
        self.compiled_tools = Some(candidate);
        self.after_tool.forget_failures();
        reload_receipt("swapped", started, receipt)
    }

    async fn install_interactive_policy(
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
        self.spec_installs = 1;
        let prepare_started = std::time::Instant::now();
        let compiled_tools = self
            .environment
            .runner
            .application_workbench()
            .prepare_tools(context.clone(), self.spec_installs)
            .await;
        tracing::info!(
            actor = %context.actor,
            phase = "startup",
            install = self.spec_installs,
            elapsed_ms = prepare_started.elapsed().as_millis(),
            success = compiled_tools.is_ok(),
            "agent spec preparation"
        );
        self.compiled_tools = compiled_tools?;
        let declarations = self
            .compiled_tools
            .as_ref()
            .map(|tools| tools.declarations.clone())
            .unwrap_or_default();
        let policy: Arc<dyn ResidentToolEndpoint> = Arc::new(
            crate::ResidentInteractivePolicy::local_with_tools(actor.clone(), declarations),
        );
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
            model: self.descriptor.model().cloned(),
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
            // best-effort: deployment observer channel may have no listener.
            self.environment
                .deployments
                .try_send(LocalResidentDeployment::ChildExited { notice })
                .ok();
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
                    tidepool_runtime::spawn_blocking_in_span(move || prepared.install(actor))
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
                    let role = self.descriptor.effective_role().role();
                    self.worktree_custody = Some(
                        tidepool_runtime::spawn_blocking_in_span(move || {
                            admission.install_custody(actor, &worktree, role)
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
                    ));
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
                self.set_standing(context.actor, ResidentStanding::Workbench);
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
                            )
                            .await?;
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
                                        #[allow(
                                            clippy::expect_used,
                                            reason = "self.source_connections was set to \
                                                      Some(..) immediately above, with no \
                                                      intervening code that clears it"
                                        )]
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
                                    #[allow(
                                        clippy::expect_used,
                                        reason = "self.source_connections was set to \
                                                  Some(..) immediately above, with no \
                                                  intervening code that clears it"
                                    )]
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
                                self.publish_watch_notifications(notifications).await;
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
                    fragment: mut next_fragment,
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
                    // Timed from here, not from `capture_boundary` above:
                    // this brackets the boundary's own service work, which
                    // is what `record_workbench_operation` reports as
                    // `elapsed_ms` once the match below settles it.
                    let effect_started = std::time::Instant::now();
                    // The effect level. The boundary's own service work is
                    // spread across the match below, so this is an event on
                    // the input-unit span rather than a span of its own;
                    // its disposition arrives with the unit's receipt.
                    tracing::info!(
                        input_unit_index = unit.input_unit_index,
                        ordinal,
                        effect = %effect,
                        "effect boundary captured"
                    );
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
                                    effect_started.elapsed(),
                                    WorkbenchOperationDisposition::Committed,
                                );
                                return Ok(ResidentWorkbenchStep::Replied {
                                    request: attempt.request,
                                    result: attempt.result,
                                    preview: attempt.preview,
                                });
                            }
                            Err(error) if attempt.recoverable => {
                                record_workbench_operation(
                                    unit.operations,
                                    unit.execution,
                                    unit.input_unit_index,
                                    ordinal,
                                    &effect,
                                    effect_started.elapsed(),
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
                                fragment = *next_fragment;
                                continue;
                            }
                            Err(error) => {
                                record_workbench_operation(
                                    unit.operations,
                                    unit.execution,
                                    unit.input_unit_index,
                                    ordinal,
                                    &effect,
                                    effect_started.elapsed(),
                                    WorkbenchOperationDisposition::Rejected,
                                );
                                drop(attempt.result);
                                return Ok(ResidentWorkbenchStep::Rejected(
                                    format!("reply rejected: {error:?}").into(),
                                ));
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
                                        effect_started.elapsed(),
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
                                        effect_started.elapsed(),
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
                                    fragment = *next_fragment;
                                    continue;
                                }
                                Err(error) => {
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        effect_started.elapsed(),
                                        WorkbenchOperationDisposition::Rejected,
                                    );
                                    return Ok(ResidentWorkbenchStep::Rejected(
                                        format!("cancellation acknowledgement rejected: {error:?}")
                                            .into(),
                                    ));
                                }
                            }
                        }
                        boundary => {
                            if let ResidentActorBoundary::Console { text, .. } = &boundary {
                                let rendered = crate::workbench_display::bounded_output(
                                    text,
                                    (*unit.display_remaining).min(32768),
                                );
                                let rendered =
                                    next_fragment.present_output(&rendered, unit.display_remaining);
                                if !rendered.is_empty() {
                                    unit.command_output.push(rendered);
                                }
                            }
                            let presentation = match &boundary {
                                ResidentActorBoundary::Command {
                                    request:
                                        crate::generated::commands::CommandsReq::CommandPresentWith(
                                            job,
                                            presentation,
                                        ),
                                    ..
                                } => Some((job.clone(), presentation.clone())),
                                _ => None,
                            };
                            if let Some((job, mut presentation)) = presentation {
                                if !unit.named_tool
                                    && next_fragment.summarizes_bound_commands()
                                    && matches!(
                                        presentation,
                                        CommandPresentation::CommandVisible(_, _)
                                    )
                                {
                                    let status =
                                        self.environment.commands.status(context.actor, &job).await;
                                    let output = self
                                        .environment
                                        .commands
                                        .output(context.actor, &job, 0)
                                        .await;
                                    let exit = match status {
                                        Ok(tidepool_bridge_effects::CommandStatus::CommandFinished(result)) => match result.outcome {
                                            tidepool_bridge_effects::CommandOutcome::CommandExited(code) => format!("exit {code}"),
                                            other => format!("{:?}", other),
                                        },
                                        Ok(tidepool_bridge_effects::CommandStatus::CommandQueued) => "queued".into(),
                                        Ok(tidepool_bridge_effects::CommandStatus::CommandStarting) => "starting".into(),
                                        Ok(tidepool_bridge_effects::CommandStatus::CommandRunning) => "running".into(),
                                        Ok(tidepool_bridge_effects::CommandStatus::CommandStopping) => "stopping".into(),
                                        Err(error) => format!("status unavailable: {error:?}"),
                                    };
                                    let counts = match output {
                                        Ok(output) => format!(
                                            "stdout {} bytes · stderr {} bytes",
                                            output.stdout.available_end,
                                            output.stderr.available_end
                                        ),
                                        Err(error) => {
                                            format!(
                                                "stdout/stderr byte counts unavailable: {error:?}"
                                            )
                                        }
                                    };
                                    presentation = CommandPresentation::CommandVisible(
                                        format!("command {job}: {exit} · {counts}"),
                                        512,
                                    );
                                }
                                let limit = match &presentation {
                                    CommandPresentation::CommandVisible(_, bytes) => {
                                        usize::try_from(*bytes)
                                            .unwrap_or(0)
                                            .min(65536)
                                            .min(*unit.display_remaining)
                                    }
                                    CommandPresentation::CommandQuiet => 0,
                                };
                                let mut shortened = false;
                                let pages = if matches!(
                                    presentation,
                                    CommandPresentation::CommandVisible(_, _)
                                ) && limit >= 1024
                                {
                                    Some(
                                        self.environment
                                            .commands
                                            .observation(context.actor, &job)
                                            .await,
                                    )
                                } else {
                                    None
                                };
                                if let CommandPresentation::CommandVisible(text, _) =
                                    &mut presentation
                                {
                                    *text = crate::workbench_display::bounded_output(text, 512);
                                    match &pages {
                                                Some(Ok(pages)) => text.push_str(&crate::workbench_display::command_pages(pages)),
                                                Some(Err(tidepool_bridge_effects::CommandError::CommandOutputPending)) => text.push_str("\nNo output yet; streams are starting."),
                                                Some(Err(error)) => text.push_str(&format!("\nOutput observation unavailable: {error:?}. Inspect the same job; do not rerun for output.")),
                                                None => {}
                                            }
                                }
                                if unit.named_tool {
                                    if let CommandPresentation::CommandVisible(text, _) =
                                        &mut presentation
                                    {
                                        let incomplete =
                                            pages.as_ref().is_some_and(|pages| match pages {
                                                Ok(pages) => pages.iter().any(|(_, page)| {
                                                    page.lost_bytes > 0
                                                        || page.end < page.available_end
                                                }),
                                                Err(tidepool_bridge_effects::CommandError::CommandOutputPending) => false,
                                                Err(_) => true,
                                            });
                                        // Every direct command tool call retains a Haskell binding,
                                        // not only ones whose output would otherwise be truncated —
                                        // the cheap path must cross into a program for free.
                                        let oversized = text.len() > limit || incomplete;
                                        shortened = oversized;
                                        let binding = workbench
                                            .bind_command_job(context.clone(), job.clone())
                                            .await?;
                                        if oversized {
                                            shortened |= text.len()
                                                > (8 * 1024).min(limit).saturating_sub(512);
                                            *text = crate::workbench_display::bounded_output(
                                                text,
                                                (8 * 1024).min(limit).saturating_sub(512),
                                            );
                                            *text = format!("retained as {binding} :: Cmd.Job\nnext: read_output session_id={job}, stream=Stdout (or Stderr), offset=0. Do not rerun.\n{text}");
                                        } else {
                                            *text =
                                                format!("retained as {binding} :: Cmd.Job\n{text}");
                                        }
                                        next_fragment.retain_job_binding(binding);
                                    }
                                }
                                if let CommandPresentation::CommandVisible(text, _) =
                                    &mut presentation
                                {
                                    shortened |= text.len() > limit;
                                    *text = crate::workbench_display::bounded_output(text, limit);
                                }
                                let rendered = next_fragment.present_command(
                                    job.clone(),
                                    presentation,
                                    unit.display_remaining,
                                );
                                if !rendered.is_empty() {
                                    unit.command_output.push(rendered);
                                }
                                if let Some(Ok(pages)) = pages.filter(|_| !shortened) {
                                    self.environment
                                        .commands
                                        .mark_displayed(context.actor, &job, &pages)
                                        .map_err(|error| {
                                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                                "command observation receipt: {error:?}"
                                            ))
                                        })?;
                                }
                            }
                            let (resolved, command_disposition) = match boundary {
                                ResidentActorBoundary::Command {
                                    continuation,
                                    request,
                                } => {
                                    let exec_started = std::time::Instant::now();
                                    let resolved = self
                                        .resolve_command(kernel, context, continuation, request)
                                        .await;
                                    crate::call_timing::add_exec_ms(
                                        exec_started.elapsed().as_millis(),
                                    );
                                    if let Some(job) = resolved.started_job {
                                        // Record the job id against this
                                        // item's fragment: on commit, a sole
                                        // command-job-typed binder installed
                                        // by this item is discoverable by
                                        // this exact id, the same as a
                                        // host-mounted binding. See
                                        // `resident_workbench::settle_fragment`.
                                        next_fragment.record_started_job(job);
                                    }
                                    (resolved.outcome, Some(resolved.disposition))
                                }
                                boundary => (
                                    self.resolve_effect(
                                        kernel,
                                        context,
                                        &crate::CallAncestry::begin(context.actor),
                                        boundary,
                                    )
                                    .await,
                                    None,
                                ),
                            };
                            outcome = match resolved {
                                Ok(outcome) => {
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        effect_started.elapsed(),
                                        command_disposition
                                            .unwrap_or(WorkbenchOperationDisposition::Committed),
                                    );
                                    outcome
                                }
                                Err(ResidentActorWorkbenchError::CommandObservationStopped {
                                    job,
                                    reason,
                                }) => {
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        effect_started.elapsed(),
                                        command_disposition
                                            .unwrap_or(WorkbenchOperationDisposition::Committed),
                                    );
                                    return workbench
                                        .bind_background_job(context.clone(), job, reason)
                                        .await;
                                }
                                Err(error) => {
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        effect_started.elapsed(),
                                        command_disposition.unwrap_or_else(|| {
                                            disposition_for_non_command_failure(&error)
                                        }),
                                    );
                                    return Err(error);
                                }
                            };
                        }
                    }
                    fragment = *next_fragment;
                }
                settled => return Ok(settled),
            }
        }
    }

    /// Apply the retained after-tool slot at the tool-result boundary, and
    /// answer what the model is shown.
    ///
    /// The result waits for the slot, up to five minutes. Past thirty seconds
    /// the elapsed time is published as workbench posture, which is an
    /// observation channel: nothing here wakes the model or causes an
    /// inference. On timeout or failure the original result is delivered with
    /// one compact line and a reference — work the slot already completed is
    /// kept rather than replayed, because the blocking machine task settles
    /// its own checkout whether or not this caller is still polling it, and a
    /// diagnostic already shown becomes a reference to its first sighting
    /// instead of a second copy.
    async fn annotate_tool_result(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        workbench: &crate::ResidentActorWorkbench<H, O>,
        dispatch: Arc<RootCustody>,
        call: &tidepool_runtime::session::workbench::WorkbenchToolCall,
        output: String,
    ) -> String {
        use crate::after_tool::{Annotation, Disposition, Invocation};

        let Some(tools) = self.compiled_tools.as_ref() else {
            return output;
        };
        if !tools
            .slots
            .iter()
            .any(|slot| slot == crate::after_tool::AFTER_TOOL_SLOT)
        {
            return output;
        }
        // A slot's own effects and tool use never trigger a slot.
        if self.after_tool_active {
            return output;
        }
        let provenance = tools.provenance();
        let revision = tools.revision.clone().unwrap_or_else(|| "(run)".to_owned());
        let ordinal = self.after_tool.begin();
        // The span every Jev call and effect a slot makes is attributed to:
        // which slot, the tool call that triggered it, the actor it ran on,
        // and the spec revision it was compiled from. `elapsed_ms` is filled
        // in once the slot settles.
        let slot_span = tracing::info_span!(
            "after_tool_slot",
            slot = %crate::after_tool::AFTER_TOOL_SLOT,
            tool = %call.name,
            actor = %context.actor,
            revision = %revision,
            ordinal,
            elapsed_ms = tracing::field::Empty,
        );
        async {
            // The handle is chosen before the slot runs, because the slot is shown
            // it and names it back when it prunes. It is defined only if the slot
            // actually prunes.
            let handle = format!("toolResult{ordinal}");
            let payload = serde_json::json!({
                "call": { "name": call.name, "arguments": call.arguments },
                "result": {
                    "name": call.name,
                    "handle": handle,
                    "ordinal": ordinal,
                    "output": output,
                },
            });
            let started = std::time::Instant::now();
            let wait = crate::after_tool::wait();
            let observation = self.runtime_observation.clone();
            // What is parked before the slot runs, so a slot that is cut off can
            // have exactly its own suspended turn aborted and nothing else.
            let parked_before = workbench
                .parked_continuations(context.clone())
                .await
                .unwrap_or_default();
            self.after_tool_active = true;
            let answer = {
                let slot = self.run_after_tool(
                    kernel,
                    context,
                    workbench,
                    dispatch,
                    call.name.clone(),
                    payload,
                );
                tokio::pin!(slot);
                let expiry = tokio::time::sleep(wait);
                tokio::pin!(expiry);
                let mut progress = tokio::time::interval_at(
                    tokio::time::Instant::now() + crate::after_tool::AFTER_TOOL_PROGRESS,
                    crate::after_tool::AFTER_TOOL_PROGRESS,
                );
                loop {
                    tokio::select! {
                        biased;
                        settled = &mut slot => break Some(settled),
                        () = &mut expiry => break None,
                        _ = progress.tick() => observation.publish_workbench_posture(
                            crate::ActorWorkbenchPosture::AwaitingEffect {
                                input_unit_index: 0,
                                total: 1,
                                effect: format!(
                                    "after-tool slot, {}s elapsed",
                                    started.elapsed().as_secs()
                                ),
                            },
                        ),
                    }
                }
            };
            self.after_tool_active = false;
            let elapsed = started.elapsed();
            tracing::Span::current().record("elapsed_ms", elapsed.as_millis() as u64);
            if answer.is_none() {
                // Nobody is driving the slot any more, so an effect it is
                // suspended on would never be answered and its turn would hold
                // the machine against the next call.
                match workbench
                    .abort_parked_since(
                        context.clone(),
                        parked_before,
                        "after-tool slot ran out of time".into(),
                    )
                    .await
                {
                    Ok(aborted) => tracing::info!(
                        actor = %context.actor,
                        ordinal,
                        aborted,
                        "after-tool slot cut off; its suspended turn was aborted"
                    ),
                    Err(error) => tracing::warn!(
                        actor = %context.actor,
                        ordinal,
                        %error,
                        "after-tool slot cut off and its suspended turn could not be aborted"
                    ),
                }
            }
            // `outcome_detail` is the one bounded line of reason text a compact
            // `info` event can carry for whichever the outcome was: nothing said
            // (empty), a nudge written straight onto the child's own result, a
            // selection, or a failure. An escalation's own detail — the tripped
            // heuristics and the parent it went to — is traced separately where
            // the `Notifications` send happens, gated on `after_tool_active`.
            let (delivered, disposition, outcome_detail) = match answer {
                None => {
                    let reason = format!(
                        "no answer within {}",
                        crate::after_tool::describe_wait(wait)
                    );
                    let notice = self.after_tool.notice(ordinal, &reason);
                    (
                        crate::after_tool::failed(&output, &notice),
                        Disposition::TimedOut(wait),
                        reason,
                    )
                }
                // A request or a display value too large to materialize says
                // nothing about the tool result itself: the slot's own
                // relay of it (to Jev, to a binding) hit the same size limit
                // that display already tolerates elsewhere in a turn. The
                // model asked for a judgement it happens not to be able to
                // get, not a broken judgement, so this is a non-decision
                // like any other abstention rather than a failure line
                // repeated on every large tool result. Bounding what the
                // slot itself sends through an effect (`Watchdog.hs`) is the
                // real fix; this keeps the hook from failing regardless.
                Some(Err(error)) if error.is_observation_budget_exhausted() => (
                    output,
                    Disposition::Abstained(
                        "tool result too large for the slot to relay through its own effects"
                            .into(),
                    ),
                    String::new(),
                ),
                Some(Err(error)) => {
                    let reason = crate::after_tool::compact_reason(&error.to_string());
                    let notice = self.after_tool.notice(ordinal, &reason);
                    (
                        crate::after_tool::failed(&output, &notice),
                        Disposition::Failed(reason.clone()),
                        reason,
                    )
                }
                Some(Ok(Annotation::Nothing)) => (output, Disposition::Silent, String::new()),
                // Silent to the model. It never asked for a judgement on this
                // result, and a non-decision is not a refusal to announce.
                Some(Ok(Annotation::Abstained(reason))) => {
                    let detail = reason.clone();
                    (output, Disposition::Abstained(reason), detail)
                }
                Some(Ok(Annotation::Annotated(text))) => {
                    let detail = crate::after_tool::compact_reason(&text);
                    (
                        crate::after_tool::annotated(&output, &text, &revision),
                        Disposition::Annotated,
                        detail,
                    )
                }
                Some(Ok(Annotation::Pruned { text, .. })) => {
                    match workbench
                        .bind_tool_result(context.clone(), handle.clone(), output.clone())
                        .await
                    {
                        Ok(()) => {
                            let detail = crate::after_tool::compact_reason(&text);
                            (
                                crate::after_tool::pruned(&text, &handle, &revision),
                                Disposition::Pruned(handle),
                                detail,
                            )
                        }
                        // A selection whose whole is unreachable would be a
                        // rewrite, so the original is delivered instead.
                        Err(error) => {
                            let reason = crate::after_tool::compact_reason(&error.to_string());
                            let notice = self.after_tool.notice(ordinal, &reason);
                            (
                                crate::after_tool::failed(&output, &notice),
                                Disposition::Failed(reason.clone()),
                                reason,
                            )
                        }
                    }
                }
            };
            tracing::info!(
                actor = %context.actor,
                tool = %call.name,
                ordinal,
                elapsed_ms = elapsed.as_millis(),
                disposition = ?disposition,
                detail = %outcome_detail,
                "after-tool slot invoked"
            );
            self.after_tool.record(Invocation {
                ordinal,
                tool: call.name.clone(),
                elapsed,
                provenance,
                disposition,
            });
            delivered
        }
        .instrument(slot_span)
        .await
    }

    /// One slot invocation, in the actor's own resident machine: the retained
    /// dispatcher entered at the slot's index, and its effects settled exactly
    /// as a tool call's are.
    async fn run_after_tool(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        workbench: &crate::ResidentActorWorkbench<H, O>,
        dispatch: Arc<RootCustody>,
        tool: String,
        payload: serde_json::Value,
    ) -> Result<crate::after_tool::Annotation, ResidentActorWorkbenchError> {
        let mut operations = Vec::new();
        let mut display_remaining = 16usize * 1024;
        let mut command_output = Vec::new();
        let step = workbench
            .begin_after_tool(context.clone(), dispatch, tool, payload)
            .await?;
        let step = match step {
            ResidentWorkbenchStep::Running { fragment, outcome } => {
                self.settle_fragment_effects(
                    kernel,
                    context,
                    workbench,
                    *fragment,
                    *outcome,
                    WorkbenchUnitExecution {
                        execution: None,
                        input_unit_index: 0,
                        total: 1,
                        named_tool: true,
                        operations: &mut operations,
                        display_remaining: &mut display_remaining,
                        command_output: &mut command_output,
                    },
                )
                .await?
            }
            settled => settled,
        };
        match step {
            ResidentWorkbenchStep::Committed { output, .. } => {
                crate::after_tool::Annotation::decode(&output)
                    .map_err(ResidentActorWorkbenchError::ActorProtocol)
            }
            ResidentWorkbenchStep::Rejected(rejection) => {
                Err(ResidentActorWorkbenchError::ActorProtocol(rejection.output))
            }
            _ => Err(ResidentActorWorkbenchError::ActorProtocol(
                "the after-tool slot ended in a transfer instead of an annotation".into(),
            )),
        }
    }

    /// The cell level of the run's span tree. `execution` is the tool call's
    /// own identity carried into the actor task, and is how a reconstructed
    /// cell joins back to the provider call that asked for it.
    #[tracing::instrument(
        name = "cell",
        skip_all,
        fields(
            actor = %context.actor,
            execution = request.execution_id().map_or("", |id| id.as_str()),
            tool = request.tool_call().map_or("", |call| call.name.as_str()),
            items = request.items.len(),
        )
    )]
    async fn execute_workbench(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        mut request: WorkbenchRequest,
    ) -> Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure> {
        let execution = request.execution_id().cloned();
        let status_call = request
            .tool_call()
            .filter(|call| call.name == crate::status_tool::STATUS_TOOL)
            .cloned();
        let reload_spec_call = request
            .tool_call()
            .filter(|call| call.name == crate::reload_spec_tool::RELOAD_SPEC_TOOL)
            .cloned();
        let tool_dispatch = if let Some(call) = request.tool_call().filter(|call| {
            call.name != crate::status_tool::STATUS_TOOL
                && call.name != crate::reload_spec_tool::RELOAD_SPEC_TOOL
        }) {
            let tools = self
                .compiled_tools
                .as_ref()
                .filter(|tools| {
                    tools.declarations.iter().any(|tool| {
                        tool.name() == call.name
                            && match tool {
                                exomonad_tool::HostedTool::Custom(_) => call.arguments.is_string(),
                                exomonad_tool::HostedTool::Function(_) => {
                                    call.arguments.is_object()
                                }
                            }
                    })
                })
                .ok_or_else(|| {
                    workbench_failure(
                        &[],
                        0,
                        1,
                        ResidentActorWorkbenchError::ActorProtocol(
                            "unknown tool or invalid argument kind".into(),
                        ),
                    )
                })?;
            // Dispatch clones the retained record, so the identity of THAT
            // record is known at the moment of the call and costs nothing to
            // carry. It says which installed record served the call, and the
            // revision that record was built from — not that everything
            // reachable through the call belongs to one revision.
            tracing::info!(
                actor = %context.actor,
                tool = %call.name,
                spec = %tools.provenance(),
                "hosted tool call served by an installed spec"
            );
            Some(Arc::clone(&tools.dispatch))
        } else {
            None
        };
        if let Some(call) = reload_spec_call {
            let arguments = crate::reload_spec_tool::parse(call.arguments).map_err(|error| {
                workbench_failure(
                    &[],
                    0,
                    1,
                    ResidentActorWorkbenchError::ActorProtocol(error.to_string()),
                )
            })?;
            let output = self.reload_agent_spec(context, &arguments.also_check).await;
            return Ok(KernelStep::Continue(workbench_response(
                WorkbenchRunStatus::Committed,
                vec![WorkbenchItemReceipt {
                    diagnostics: Vec::new(),
                    index: 0,
                    kind: None,
                    span: None,
                    source_items: Vec::new(),
                    status: WorkbenchItemStatus::Committed,
                    output,
                    warnings: Vec::new(),
                    installed_bindings: Vec::new(),
                    operations: Vec::new(),
                    terminal_transfer: None,
                    failure_layer: None,
                }],
                1,
                1,
                None,
            )));
        }
        let workbench = self
            .active_workbench()
            .ok_or_else(|| {
                workbench_failure(
                    &[],
                    0,
                    request.items.len(),
                    ResidentActorWorkbenchError::ActorProtocol(
                        "actor application has no active Haskell workbench".into(),
                    ),
                )
            })?
            .with_json_input(
                request
                    .input
                    .as_ref()
                    .map(tidepool_runtime::session::normalize_workbench_input),
            );
        if let Some(call) = status_call {
            let view = crate::status_tool::parse(call.arguments).map_err(|error| {
                workbench_failure(
                    &[],
                    0,
                    1,
                    ResidentActorWorkbenchError::ActorProtocol(error.to_string()),
                )
            })?;
            let output = match view {
                crate::status_tool::StatusView::Summary => {
                    self.status_text(kernel, context.actor, StatusView::Concise)
                }
                crate::status_tool::StatusView::Detailed => {
                    self.status_text(kernel, context.actor, StatusView::Expanded)
                }
                crate::status_tool::StatusView::Lineage => {
                    self.status_text(kernel, context.actor, StatusView::Lineage)
                }
                crate::status_tool::StatusView::Trace => {
                    self.status_text(kernel, context.actor, StatusView::Trace)
                }
                crate::status_tool::StatusView::Watches => {
                    self.status_text(kernel, context.actor, StatusView::Watches)
                }
                crate::status_tool::StatusView::Recovery => workbench
                    .status_discovery(
                        context.clone(),
                        crate::status_tool::StatusDiscovery::Recovery,
                    )
                    .await
                    .map_err(|error| workbench_failure(&[], 0, 1, error))?,
                crate::status_tool::StatusView::Bindings => workbench
                    .status_discovery(
                        context.clone(),
                        crate::status_tool::StatusDiscovery::Bindings,
                    )
                    .await
                    .map_err(|error| workbench_failure(&[], 0, 1, error))?,
                crate::status_tool::StatusView::Live => {
                    let bindings = workbench
                        .live_bindings(context.clone())
                        .await
                        .map_err(|error| workbench_failure(&[], 0, 1, error))?;
                    self.live_status_text(context.actor, &bindings)
                }
            };
            return Ok(KernelStep::Continue(workbench_response(
                WorkbenchRunStatus::Committed,
                vec![WorkbenchItemReceipt {
                    diagnostics: Vec::new(),
                    index: 0,
                    kind: None,
                    span: None,
                    source_items: Vec::new(),
                    status: WorkbenchItemStatus::Committed,
                    output,
                    warnings: Vec::new(),
                    installed_bindings: Vec::new(),
                    operations: Vec::new(),
                    terminal_transfer: None,
                    failure_layer: None,
                }],
                1,
                1,
                None,
            )));
        }
        let mut prepared_cell = None;
        let mut _cell_dependencies = None;
        let cell_check = if let Some(cell_source) = request.cell_source() {
            let (checked, prepared) = match workbench
                .prepare_cell(context.clone(), cell_source.to_owned())
                .await
            {
                Ok(checked) => checked,
                Err(ResidentActorWorkbenchError::CellCheck(failure)) => {
                    return Ok(KernelStep::Continue(cell_check_rejection(
                        failure,
                        cell_source,
                    )));
                }
                Err(source) => return Err(workbench_failure(&[], 0, 1, source)),
            };
            request.install_cell_items(
                checked
                    .items
                    .iter()
                    .map(|item| item.source.clone())
                    .collect(),
            );
            match prepared {
                PreparedCell::Ready {
                    items,
                    dependencies,
                } => {
                    _cell_dependencies = Some(dependencies);
                    prepared_cell = Some(items.into_iter().map(Some).collect::<Vec<_>>());
                }
                PreparedCell::Rejected { index, diagnostic } => {
                    let items = (0..=index)
                        .map(|prior| WorkbenchItemReceipt {
                            diagnostics: if prior == index {
                                diagnostic.diagnostics.clone()
                            } else {
                                Vec::new()
                            },
                            index: prior,
                            kind: None,
                            span: None,
                            source_items: Vec::new(),
                            status: if prior == index {
                                WorkbenchItemStatus::Rejected
                            } else {
                                WorkbenchItemStatus::NotRun
                            },
                            output: if prior == index {
                                diagnostic.output.clone()
                            } else {
                                String::new()
                            },
                            warnings: Vec::new(),
                            installed_bindings: Vec::new(),
                            operations: Vec::new(),
                            terminal_transfer: None,
                            failure_layer: if prior == index {
                                Some(WorkbenchFailureLayer::Compile)
                            } else {
                                None
                            },
                        })
                        .collect();
                    return Ok(KernelStep::Continue(workbench_response(
                        WorkbenchRunStatus::Rejected,
                        items,
                        index,
                        request.items.len(),
                        Some(&checked.items),
                    )));
                }
            }
            Some(checked)
        } else {
            None
        };
        let mut receipts: Vec<WorkbenchItemReceipt> = Vec::new();
        let mut cell_display_remaining = 8192usize;
        let mut index = 0;
        while index < request.items.len() {
            let source = request.items[index].clone();
            let mut unit_operations = Vec::new();
            let mut command_output = Vec::new();
            // Leave room for a later stop/error receipt without hiding offered command output.
            let display_budget = if request.tool_call().is_some() {
                28usize * 1024
            } else {
                60usize * 1024
            };
            let mut display_remaining = display_budget.saturating_sub(
                receipts
                    .iter()
                    .map(|item| item.output.len() + 1)
                    .sum::<usize>(),
            );
            if request.tool_call().is_none() {
                display_remaining = display_remaining.min(cell_display_remaining);
            }
            let block = ParsedBlock {
                ordinal: index + 1,
                total: request.items.len(),
                source,
            };
            // The input-unit level of the span tree. Held across this
            // iteration's two await points by instrumenting the futures
            // themselves, never by a guard.
            let unit_span = tracing::info_span!(
                "unit",
                index,
                total = request.items.len(),
                kind = if request.tool_call().is_some() {
                    "tool"
                } else {
                    "cell"
                },
            );
            tracing::info!(
                target: "exomonad::content",
                parent: &unit_span,
                index,
                source = %block.source,
                "input unit source"
            );
            self.runtime_observation.publish_workbench_posture(
                crate::ActorWorkbenchPosture::RunningUnit {
                    input_unit_index: index,
                    total: request.items.len(),
                },
            );
            let started =
                if let (Some(call), Some(dispatch)) = (request.tool_call(), &tool_dispatch) {
                    workbench
                        .begin_tool(
                            context.clone(),
                            Arc::clone(dispatch),
                            call.name.clone(),
                            call.arguments.clone(),
                        )
                        .instrument(unit_span.clone())
                        .await
                } else {
                    match prepared_cell.as_mut() {
                        Some(items) => {
                            let prepared = items[index].take().ok_or_else(|| {
                                workbench_failure(
                                    &receipts,
                                    index,
                                    request.items.len(),
                                    ResidentActorWorkbenchError::CompileInfrastructure(
                                        "prepared cell item was already consumed".into(),
                                    ),
                                )
                            })?;
                            workbench
                                .begin_prepared_cell_item(
                                    context.clone(),
                                    block,
                                    prepared,
                                    cell_display_remaining,
                                )
                                .instrument(unit_span.clone())
                                .await
                        }
                        None => Err(ResidentActorWorkbenchError::CompileInfrastructure(
                            "authored cell reached execution without compiler preparation".into(),
                        )),
                    }
                };
            let mut step = match started {
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
                        *fragment,
                        *outcome,
                        WorkbenchUnitExecution {
                            execution: execution.as_ref(),
                            input_unit_index: index,
                            total: request.items.len(),
                            named_tool: request.tool_call().is_some(),
                            operations: &mut unit_operations,
                            display_remaining: &mut display_remaining,
                            command_output: &mut command_output,
                        },
                    )
                    .instrument(unit_span.clone())
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
                        let mut failure = workbench_failure_after_operations(
                            &receipts,
                            index,
                            request.items.len(),
                            source,
                            unit_operations,
                        );
                        if let Some(receipt) = failure
                            .receipts
                            .last_mut()
                            .filter(|receipt| receipt.index == index)
                        {
                            receipt.output =
                                format!("{}\n{}", command_output.join("\n"), receipt.output);
                        }
                        return Err(failure);
                    }
                };
            }
            let command_prefix = command_output.join("\n");
            match step {
                ResidentWorkbenchStep::Committed {
                    output,
                    warnings,
                    installed_bindings,
                } => {
                    let output = crate::workbench_display::resolve_job_binding_placeholder(
                        output,
                        &installed_bindings,
                    );
                    let output = if request.tool_call().is_some() {
                        crate::bound_workbench_display(&output, display_remaining)
                    } else {
                        output
                    };
                    let output = if command_prefix.is_empty() {
                        output
                    } else {
                        format!("{command_prefix}\n{output}")
                    };
                    // The tool-result boundary: the result exists and has not
                    // been returned. A hosted tool call reaches here with its
                    // own dispatcher; an authored Haskell cell reaches here
                    // too, under the `haskell` tool name and the same
                    // installed spec's dispatcher — `status` and
                    // `reload_agent_spec` are the only committed outcomes
                    // that never acquire one, so a broken slot can never
                    // block its own repair.
                    let hosted_call = request.tool_call().cloned().zip(tool_dispatch.clone());
                    let cell_call = if hosted_call.is_none() && cell_check.is_some() {
                        self.compiled_tools.as_ref().map(|tools| {
                            (
                                tidepool_runtime::session::workbench::WorkbenchToolCall {
                                    name: crate::HASKELL_TOOL.to_string(),
                                    arguments: serde_json::Value::String(
                                        request.items[index].clone(),
                                    ),
                                },
                                Arc::clone(&tools.dispatch),
                            )
                        })
                    } else {
                        None
                    };
                    let output = match hosted_call.or(cell_call) {
                        Some((call, dispatch)) => {
                            self.annotate_tool_result(
                                kernel, context, &workbench, dispatch, &call, output,
                            )
                            .await
                        }
                        None => output,
                    };
                    if let Some(checked) = &cell_check {
                        let spent = if checked.items[index].verdict.kind
                            == tidepool_runtime::session::TurnKind::Expr
                        {
                            output.chars().count()
                        } else {
                            command_prefix.chars().count()
                        };
                        cell_display_remaining = cell_display_remaining.saturating_sub(spent);
                    }
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
                            diagnostics: Vec::new(),
                            index,
                            kind: None,
                            span: None,
                            source_items: Vec::new(),
                            status: WorkbenchItemStatus::Rejected,
                            output: "unfold admission ended without committing every fork group"
                                .into(),
                            warnings: Vec::new(),
                            installed_bindings: Vec::new(),
                            operations: unit_operations,
                            terminal_transfer: None,
                            failure_layer: Some(WorkbenchFailureLayer::Effect),
                        });
                        return Ok(KernelStep::Continue(workbench_response(
                            WorkbenchRunStatus::Rejected,
                            receipts,
                            index,
                            request.items.len(),
                            cell_check.as_ref().map(|checked| checked.items.as_slice()),
                        )));
                    }
                    receipts.push(WorkbenchItemReceipt {
                        diagnostics: Vec::new(),
                        index,
                        kind: None,
                        span: None,
                        source_items: Vec::new(),
                        status: WorkbenchItemStatus::Committed,
                        output,
                        warnings,
                        installed_bindings,
                        operations: unit_operations,
                        terminal_transfer: None,
                        failure_layer: None,
                    });
                }
                ResidentWorkbenchStep::Rejected(rejection) => {
                    let tidepool_runtime::session::CompileRejection {
                        output,
                        diagnostics,
                    } = rejection;
                    let output = if request.tool_call().is_some() {
                        crate::bound_workbench_display(&output, display_remaining)
                    } else {
                        output
                    };
                    let output = if command_prefix.is_empty() {
                        output
                    } else {
                        format!("{command_prefix}\n{output}")
                    };
                    settle_prepared_operations(
                        &mut unit_operations,
                        WorkbenchOperationDisposition::Rejected,
                    );
                    self.abort_incomplete_groups(
                        kernel,
                        context.actor,
                        "Haskell input rejected during unfold admission",
                    )
                    .await;
                    receipts.push(WorkbenchItemReceipt {
                        diagnostics,
                        index,
                        kind: None,
                        span: None,
                        source_items: Vec::new(),
                        status: WorkbenchItemStatus::Rejected,
                        output,
                        warnings: Vec::new(),
                        installed_bindings: Vec::new(),
                        operations: unit_operations,
                        terminal_transfer: None,
                        failure_layer: Some(WorkbenchFailureLayer::Compile),
                    });
                    return Ok(KernelStep::Continue(workbench_response(
                        WorkbenchRunStatus::Rejected,
                        receipts,
                        index,
                        request.items.len(),
                        cell_check.as_ref().map(|checked| checked.items.as_slice()),
                    )));
                }
                ResidentWorkbenchStep::CommandBackgrounded {
                    job,
                    binding,
                    reason,
                } => {
                    let reason =
                        crate::workbench_display::bounded_output(&reason.to_string(), 1024);
                    let mut output = if request.tool_call().is_some() {
                        format!(
                            "session_id: {job}\n{reason}. Observation ended; the command remains retained. Poll/send input with write_stdin; read_output navigates output. Later handler effects did not run.\nHaskell binding: {binding} :: Cmd.Job"
                        )
                    } else {
                        format!(
                            "Retained command · session_id: {job}\n{reason}. Available binding:\n\n{binding} :: Cmd.Job\n\nThe enclosing result was not bound; subsequent statements did not run.\nContinue with: result <- Cmd.await {binding}"
                        )
                    };
                    if !command_prefix.is_empty() {
                        output.push_str(&format!("\n{command_prefix}"));
                    }
                    match self
                        .environment
                        .commands
                        .observation(context.actor, &job)
                        .await
                    {
                        Ok(pages) => {
                            let rendered = crate::workbench_display::bounded_output(
                                &crate::workbench_display::command_pages(&pages),
                                display_remaining.saturating_sub(output.len()),
                            );
                            if !rendered.is_empty() {
                                output.push_str(&rendered);
                                // Explicit pages remain available even when presentation is shortened.
                                if let Err(error) = self.environment.commands.mark_displayed(
                                    context.actor,
                                    &job,
                                    &pages,
                                ) {
                                    output.push_str(&format!("\nOutput cursor unavailable: {error:?}; explicit reads remain non-consuming."));
                                }
                            }
                        }
                        Err(tidepool_bridge_effects::CommandError::CommandOutputPending) => {
                            output.push_str("\nNo output yet; streams are starting.")
                        }
                        Err(error) => output.push_str(&format!(
                            "\nOutput unavailable: {error:?}; the same job remains retained."
                        )),
                    }
                    receipts.push(WorkbenchItemReceipt {
                        diagnostics: Vec::new(),
                        index,
                        kind: None,
                        span: None,
                        source_items: Vec::new(),
                        status: WorkbenchItemStatus::Stopped,
                        output,
                        warnings: Vec::new(),
                        installed_bindings: vec![binding],
                        operations: unit_operations,
                        terminal_transfer: Some(WorkbenchTerminalTransfer::CommandBackgrounded),
                        failure_layer: None,
                    });
                    return Ok(KernelStep::Continue(workbench_response(
                        WorkbenchRunStatus::Backgrounded,
                        receipts,
                        index,
                        request.items.len(),
                        cell_check.as_ref().map(|checked| checked.items.as_slice()),
                    )));
                }
                ResidentWorkbenchStep::Replied {
                    request: request_id,
                    result,
                    preview,
                } => {
                    self.stage_request_reply(kernel, context, request_id, result, preview)
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
                        diagnostics: Vec::new(),
                        index,
                        kind: None,
                        span: None,
                        source_items: Vec::new(),
                        status: WorkbenchItemStatus::Committed,
                        output: "Reply submitted.".to_owned(),
                        warnings: Vec::new(),
                        installed_bindings: Vec::new(),
                        operations: unit_operations,
                        terminal_transfer: Some(WorkbenchTerminalTransfer::ReplyAccepted),
                        failure_layer: None,
                    });
                    return Ok(KernelStep::ContinueLater(workbench_response(
                        WorkbenchRunStatus::Replied,
                        receipts,
                        index + 1,
                        request.items.len(),
                        cell_check.as_ref().map(|checked| checked.items.as_slice()),
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
                                tracing::info!(
                                    actor = ?context.actor,
                                    from = "interactive",
                                    request = ?request_id,
                                    to = "boot",
                                    "resident actor standing transition"
                                );
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
                    // The cancellation landed: this request no longer owes
                    // `respond` bindings.
                    self.outstanding_interactive = None;
                    self.pending_program = Some(outcome);
                    self.pending_cancellation = Some(request_id);
                    receipts.push(WorkbenchItemReceipt {
                        diagnostics: Vec::new(),
                        index,
                        kind: None,
                        span: None,
                        source_items: Vec::new(),
                        status: WorkbenchItemStatus::Committed,
                        output: String::new(),
                        warnings: Vec::new(),
                        installed_bindings: Vec::new(),
                        operations: unit_operations,
                        terminal_transfer: Some(
                            WorkbenchTerminalTransfer::CancellationAcknowledged,
                        ),
                        failure_layer: None,
                    });
                    return Ok(KernelStep::ContinueLater(workbench_response(
                        WorkbenchRunStatus::RequestCancelled,
                        receipts,
                        index + 1,
                        request.items.len(),
                        cell_check.as_ref().map(|checked| checked.items.as_slice()),
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
            cell_check.as_ref().map(|checked| checked.items.as_slice()),
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
                // A child already gone from a failed fork-group admission is
                // the common case here; log anything else so an actor that
                // refused shutdown does not silently linger.
                if let Err(error) = child
                    .shutdown(ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: summary.into(),
                    })
                    .await
                {
                    tracing::warn!(child = ?child.identity(), %error, "fork-group child did not shut down");
                }
            }
        }
    }
    async fn finish_pending_fork_publication(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> Result<bool, KernelBehaviorError> {
        let Some(index) = self
            .pending_fork_publications
            .iter()
            .position(|pending| &pending.boundary == boundary)
        else {
            return Ok(false);
        };
        let mut pending = self.pending_fork_publications.remove(index);
        if !pending.published {
            if let Err(error) = self
                .environment
                .fork_groups
                .publish_groups(&pending.groups, context.actor)
            {
                self.pending_fork_publications.push(pending);
                return Err(KernelBehaviorError {
                    detail: error.to_string(),
                });
            }
            pending.published = true;
        }
        for (child, scope) in pending.releases.drain(..) {
            let sent = kernel.resolve(child).is_some_and(|child| {
                child.terminal().get().is_none()
                    && child
                        .address()
                        .send_message(crate::KernelMessage::ReleaseFork { scope })
                        .is_ok()
            });
            if !sent {
                pending.unused_scopes.push(scope);
            }
        }
        if !pending.unused_scopes.is_empty() {
            if let Err(error) = self
                .environment
                .runner
                .retire_fork_scopes(context.clone(), pending.unused_scopes.clone())
                .await
            {
                self.pending_fork_publications.push(pending);
                return Err(Self::failure(error));
            }
        }
        Ok(true)
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
                // A child already gone from a failed fork-group admission is
                // the common case here; log anything else so an actor that
                // refused shutdown does not silently linger.
                if let Err(error) = child
                    .shutdown(ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: summary.into(),
                    })
                    .await
                {
                    tracing::warn!(child = ?child.identity(), %error, "fork-group child did not shut down");
                }
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
            let delivery = self.environment.requests.accept_source_delivery(delivery);
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
        self.set_standing(kernel.identity(), ResidentStanding::Paused(failure));
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
            if let Some(recovery) = &self.environment.recovery {
                recovery
                    .admit(context.actor, &self.descriptor, &self.launch_worktrees)
                    .map_err(Self::failure)?;
            }
            kernel.install_session_context(context.clone())?;
            self.environment.actors.lock().insert(
                context.actor,
                ResidentActorRecord {
                    recovery_claimed: false,
                    workbench_executions: self.workbench_executions.clone(),
                    forest_control: self.forest_control,
                    interactive_policy_installed: false,
                    observation_roots: Default::default(),
                    descriptor: self.descriptor.clone(),
                    bound_worktree: self.launch_worktrees.first().cloned(),
                    terminal: None,
                    runtime_observation: self.runtime_observation.clone(),
                    scheduler_root: kernel.supervisor_identity().is_none(),
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

    fn reconcile_workbench_boundary<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<crate::WorkbenchBoundaryReconciliation, KernelBehaviorError>,
    > {
        Box::pin(async move {
            if self.settled_fork_boundaries.contains(&boundary) {
                return Ok(crate::WorkbenchBoundaryReconciliation::Settled);
            }
            let retained = self.workbench_executions.lock().at_boundary(&boundary);
            let reply = match retained {
                Some(WorkbenchBoundaryRecord::Terminal(reply)) => Some(reply),
                Some(WorkbenchBoundaryRecord::Unconfirmed) => {
                    return Ok(crate::WorkbenchBoundaryReconciliation::Pending)
                }
                None => None,
            };
            let context = self.context(kernel.identity());
            for child in self
                .environment
                .fork_groups
                .abort_incomplete_at_boundary(context.actor, &boundary)
            {
                if let Some(child) = kernel.resolve(child) {
                    // A child already gone from a failed fork-group admission is
                    // the common case here; log anything else so an actor that
                    // refused shutdown does not silently linger.
                    if let Err(error) = child
                        .shutdown(ActorTerminal {
                            kind: ActorExitKind::Cancelled,
                            summary: "fork admission stopped before interrupted tool settlement"
                                .into(),
                        })
                        .await
                    {
                        tracing::warn!(child = ?child.identity(), %error, "fork-group child did not shut down");
                    }
                }
            }
            if let Some(reply) = reply {
                return Ok(crate::WorkbenchBoundaryReconciliation::Recovered { reply });
            }
            if !self
                .environment
                .fork_groups
                .ready_groups_at_boundary(context.actor, &boundary)
                .is_empty()
            {
                return Ok(crate::WorkbenchBoundaryReconciliation::Pending);
            }
            self.settled_fork_boundaries.push(boundary);
            Ok(crate::WorkbenchBoundaryReconciliation::Settled)
        })
    }

    fn tool_completed<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> futures_util::future::BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            if self
                .finish_pending_fork_publication(kernel, &context, &boundary)
                .await?
            {
                if !self.settled_fork_boundaries.contains(&boundary) {
                    self.settled_fork_boundaries.push(boundary);
                }
                return Ok(());
            }
            for child in self
                .environment
                .fork_groups
                .abort_incomplete_at_boundary(context.actor, &boundary)
            {
                if let Some(child) = kernel.resolve(child) {
                    // A child already gone from a failed fork-group admission is
                    // the common case here; log anything else so an actor that
                    // refused shutdown does not silently linger.
                    if let Err(error) = child
                        .shutdown(ActorTerminal {
                            kind: ActorExitKind::Cancelled,
                            summary: "fork admission stopped before tool completion".into(),
                        })
                        .await
                    {
                        tracing::warn!(child = ?child.identity(), %error, "fork-group child did not shut down");
                    }
                }
            }
            let groups = self
                .environment
                .fork_groups
                .ready_groups_at_boundary(context.actor, &boundary);
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
                if !self.settled_fork_boundaries.contains(&boundary) {
                    self.settled_fork_boundaries.push(boundary);
                }
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
            self.pending_fork_publications.push(PendingForkPublication {
                boundary: boundary.clone(),
                groups: groups.into_iter().map(|(group, _)| group).collect(),
                releases: children.into_iter().zip(scopes).collect(),
                unused_scopes: Vec::new(),
                published: false,
            });
            self.finish_pending_fork_publication(kernel, &context, &boundary)
                .await?;
            if !self.settled_fork_boundaries.contains(&boundary) {
                self.settled_fork_boundaries.push(boundary);
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
        invocation: exomonad_tool::ToolInvocation,
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
            if !awaiting.declarations.iter().any(|tool| {
                tool.name == invocation.name
                    && exomonad_tool::HostedTool::from(tool.clone()).accepts(&invocation.arguments)
            }) {
                self.set_standing(context.actor, ResidentStanding::Tools(awaiting));
                return Err(KernelInvocationFailure::Rejected {
                    actor: context.actor,
                    detail: "unknown tool or invalid argument kind".into(),
                });
            }
            let arguments = match invocation.arguments {
                exomonad_tool::ToolArguments::Raw(text) => serde_json::Value::String(text),
                exomonad_tool::ToolArguments::Structured(value) => value,
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
                        self.set_standing(context.actor, ResidentStanding::Tools(next));
                        return Ok(KernelStep::Continue(result));
                    }
                    ResidentActorBoundary::Completed => {
                        let result = result.ok_or_else(|| KernelInvocationFailure::Failed {
                            actor: context.actor,
                            detail: "actor completed a tool invocation without replying".into(),
                        })?;
                        self.set_standing(context.actor, ResidentStanding::Terminal);
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
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
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
            let invocation = control
                .as_ref()
                .and_then(|control| control.invocation.as_ref());
            if let Some(execution) = &execution {
                let retained = self
                    .workbench_executions
                    .lock()
                    .lookup(execution, &request, invocation);
                match retained {
                    Err(failure) => {
                        let result = Err(KernelInvocationFailure::Rejected {
                            actor: context.actor,
                            detail: match failure {
                                WorkbenchReplayFailure::DifferentInput => "one hosted call identity was retried with different Haskell input",
                                WorkbenchReplayFailure::Unconfirmed => "the original hosted call outcome is unconfirmed; replay cannot repeat its effects",
                            }.into(),
                        });
                        if let Some(control) = &control {
                            control.settle(result.clone());
                        }
                        return result.map(KernelStep::Continue);
                    }
                    Ok(Some(reply)) => {
                        if let Some(control) = &control {
                            control.settle(reply.clone());
                        }
                        return reply.map(KernelStep::Continue);
                    }
                    Ok(None) => {}
                }
            }
            let retained_request = execution.as_ref().map(|_| request.clone());
            if let Some(execution) = &execution {
                // Persist the fence in the forest-retained journal before effects
                // can run; actor termination cannot turn uncertainty into replay.
                self.workbench_executions
                    .lock()
                    .begin(execution, request.clone(), invocation);
            }
            self.active_workbench_control = control.clone();
            self.active_fork_boundary = request.fork_boundary().cloned();
            // One INFO line per hosted tool call or cell, breaking down
            // where its wall time went (checkout wait/hold, compile, Jev,
            // exec) — see `crate::call_timing`. The scope wraps the whole
            // call so every nested site it awaits (workbench compiles,
            // resolved effects) can add to it as a task-local.
            let call_kind = request
                .tool_call()
                .map(|call| call.name.clone())
                .unwrap_or_else(|| "cell".to_string());
            let (call_actor, call_incarnation) = actor_address(context.actor);
            let call_scope = crate::call_timing::CallScope::new(
                call_kind,
                call_actor as u64,
                call_incarnation as u64,
            );
            let result = call_scope
                .run(self.execute_workbench(kernel, &context, request))
                .await;
            let call_outcome = match &result {
                Ok(
                    KernelStep::Continue(response)
                    | KernelStep::ContinueLater(response)
                    | KernelStep::Stop {
                        output: response, ..
                    },
                ) => format!("{:?}", response.status),
                Err(_) => "error".to_string(),
            };
            call_scope.finish(&call_outcome);
            self.active_fork_boundary = None;
            self.active_workbench_control = None;
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
                        | WorkbenchRunStatus::Backgrounded
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
                let (aborted, notifications) =
                    self.environment.requests.abort_unsubmitted(context.actor);
                self.publish_watch_notifications(notifications).await;
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
                let cancellation = control.as_ref().map_or_else(
                    || crate::WorkbenchCancellationOutcome::NotSleeping {
                        execution: execution.clone(),
                    },
                    |control| control.cancellation_outcome(execution.clone(), reply.clone()),
                );
                self.workbench_executions.lock().record(
                    execution,
                    request,
                    reply,
                    cancellation,
                    invocation,
                );
            }
            let terminal_reply = match &result {
                Ok(
                    KernelStep::Continue(response)
                    | KernelStep::ContinueLater(response)
                    | KernelStep::Stop {
                        output: response, ..
                    },
                ) => Ok(response.clone()),
                Err(error) => Err(error.clone()),
            };
            if let Some(control) = control {
                control.settle(terminal_reply);
            }
            result
        })
    }

    fn reconcile_workbench_cancellation(
        &self,
        execution: WorkbenchExecutionId,
        invocation: Option<exomonad_tool::ToolInvocationContext>,
    ) -> crate::WorkbenchCancellationOutcome {
        let invocation = invocation.map(crate::resident_tools::WorkbenchCallKey::from);
        self.workbench_executions
            .lock()
            .cancellation(execution, invocation.as_ref())
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
                            return Ok::<_, ResidentActorWorkbenchError>(false);
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
                                        attempt.preview,
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
                                    ));
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
                        // A child already gone from a failed fork-group admission is
                        // the common case here; log anything else so an actor that
                        // refused shutdown does not silently linger.
                        if let Err(error) = child
                            .shutdown(ActorTerminal {
                                kind: ActorExitKind::Cancelled,
                                summary: "route callback failed before publication".into(),
                            })
                            .await
                        {
                            tracing::warn!(child = ?child.identity(), %error, "fork-group child did not shut down");
                        }
                    }
                }
            }
            let (_, notifications) = self.environment.requests.abort_unsubmitted(context.actor);
            self.publish_watch_notifications(notifications).await;
            self.active_route = None;
            self.active_fork_boundary = None;
            let notification = self
                .environment
                .requests
                .finish_route(context.actor, watch, result);
            self.publish_watch_notifications(notification).await;
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
                        let reply_preview = self.pending_reply_preview.take();
                        let notifications = self
                            .environment
                            .requests
                            .finish_reply(request, reply_preview);
                        self.publish_watch_notifications(notifications).await;
                    }
                    if let Some(request) = self.pending_cancellation.take() {
                        let notifications = self
                            .environment
                            .requests
                            .finish_cancellation_acknowledgement(request);
                        self.publish_watch_notifications(notifications).await;
                    }
                    self.runtime_observation
                        .publish_workbench_posture(crate::ActorWorkbenchPosture::Idle);
                    Ok(step)
                }
                Err(error) => {
                    self.pending_reply_preview = None;
                    if let Some(request) = self.pending_reply.take() {
                        let notifications = self.environment.requests.fail_reply_settlement(
                            request,
                            format!("reply continuation failed after acceptance: {error}"),
                        );
                        self.publish_watch_notifications(notifications).await;
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
            // Not on the `finish_actor` path (which always calls
            // `shutdown_components` directly with its own computed deadline);
            // this fallback keeps the same budget for any other caller.
            let deadline = tokio::time::Instant::now() + crate::local_actor::SHUTDOWN_BUDGET;
            let (hook, realm) = self.shutdown_components(kernel, terminal, deadline).await;
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
        deadline: tokio::time::Instant,
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
            self.publish_watch_notifications(notifications).await;
            self.abort_unpublished_groups(
                kernel,
                context.actor,
                "fork owner stopped before tool completion",
            )
            .await;
            // Hook admission and every realm/placement checkout below race the
            // same deadline `finish_actor` computed once: a busy machine can
            // no longer delay exit publication without bound, and a removed
            // or terminal machine still fails fast (checkout errors, not a
            // wait).
            let hook = if let Some(hook) = self.shutdown_hook.take() {
                match self
                    .environment
                    .runner
                    .run_shutdown(
                        context.clone(),
                        hook,
                        context.placement.resource_scope,
                        terminal.kind,
                        deadline.saturating_duration_since(tokio::time::Instant::now()),
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
            self.outstanding_interactive = None;
            self.set_standing(context.actor, ResidentStanding::Terminal);
            self.boot = None;
            self.sources.clear();
            let mut retained_errors = Vec::new();
            for retained in std::mem::take(&mut self.retained_replacements) {
                let placement = retained.placement;
                drop(retained);
                if let Err(error) = self
                    .environment
                    .runner
                    .retire_root_placement_wait(
                        placement,
                        deadline.saturating_duration_since(tokio::time::Instant::now()),
                    )
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
                        .retire_root_placement_wait(
                            self.descriptor.placement(),
                            deadline.saturating_duration_since(tokio::time::Instant::now()),
                        )
                        .await
                } else {
                    self.environment
                        .runner
                        .close_realm_wait(
                            context,
                            self.descriptor.placement().resource_scope,
                            deadline.saturating_duration_since(tokio::time::Instant::now()),
                        )
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
            self.publish_watch_notifications(notifications).await;
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
                // best-effort: deployment observer channel may have no listener.
                self.environment
                    .deployments
                    .try_send(LocalResidentDeployment::ChildExited { notice })
                    .ok();
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
        mpsc::Receiver<LocalResidentDeployment>,
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
        mpsc::Receiver<LocalResidentDeployment>,
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
        mpsc::Receiver<LocalResidentDeployment>,
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
    pub provider_turn: Option<exomonad_model::ProviderTurnObservation>,
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

#[derive(Default)]
struct PreparedRootAdmission {
    replay: Option<Arc<Mutex<WorkbenchExecutions>>>,
    identity: Option<ActorRef>,
    launch_worktrees: Vec<String>,
    worktree_custody: Option<Arc<dyn crate::ForkWorkspaceCustody>>,
}

impl<H, O> ResidentForest<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// Answer every actor's `Jev` requests with `backend` from now on.
    pub fn set_jev_backend(&mut self, backend: crate::JevBackendHandle) {
        self.environment.jev = backend;
    }

    /// Give every actor launched from now on its own source layer, resolved
    /// from the checkout it is launched with. Without this every actor
    /// compiles against the deployment-wide include roots and nothing else.
    pub fn set_source_layers(&mut self, layers: crate::ActorSourceLayerResolver) {
        self.environment.source_layers = Some(layers);
    }

    /// Install the immutable usage pointers supplied by the facade's shipped
    /// workspace. The actor kernel owns lookup behavior; the facade owns the
    /// source inventory and its generated table.
    pub fn with_usage_pointers(mut self, pointers: crate::UsagePointerTable) -> Self {
        self.environment.usage_pointers = pointers;
        self
    }

    /// Declare that an actor host answers `LocalResidentDeployment::ReleaseAwait`.
    /// From now on a stop reports `StoppedNow` only once that host has released
    /// the actor's interactive resources.
    pub fn track_resource_release(&self) {
        self.environment
            .release_tracked
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Observe whether this forest's one resident session can be considered
    /// for same-incarnation root reentry. The subsequent checkout remains the
    /// authoritative admission boundary.
    #[must_use]
    pub fn resident_session_state(&self) -> tidepool_runtime::session::ResidentSessionState {
        self.environment.runner.resident_session_state(self.session)
    }

    /// Read-only resident counters for matched measurement harnesses. A
    /// running or retired machine has no snapshot rather than fabricated
    /// zeroes.
    #[must_use]
    pub fn measurement_snapshot(
        &self,
    ) -> Option<crate::resident_workbench::ResidentMachineMeasurement> {
        self.environment.runner.measurement_snapshot(self.session)
    }

    pub fn new(
        source: ActorWorkbenchSource,
        session: tidepool_repr::SessionId,
        machine: ResidentSession<H, O>,
        fork_workspaces: Option<crate::fork_workspace::SharedForkWorkspaceAdmission>,
        incarnation: crate::Incarnation,
    ) -> (Self, mpsc::Receiver<LocalResidentDeployment>) {
        Self::new_with_launch_resolver(source, session, machine, fork_workspaces, incarnation, None)
    }

    pub fn new_with_launch_resolver(
        source: ActorWorkbenchSource,
        session: tidepool_repr::SessionId,
        machine: ResidentSession<H, O>,
        fork_workspaces: Option<crate::fork_workspace::SharedForkWorkspaceAdmission>,
        incarnation: crate::Incarnation,
        launch_resolver: Option<crate::WorkerLaunchResolver>,
    ) -> (Self, mpsc::Receiver<LocalResidentDeployment>) {
        let machines = Arc::new(ActorMachineRegistry::<H, O>::new());
        machines.insert_idle(session, Box::new(machine));
        let runner = ResidentActorRunner::new(machines, source);
        let (deployments, receiver) = mpsc::channel(DEPLOYMENT_CHANNEL_CAPACITY);
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
            source_layers: None,
            jev: Arc::new(crate::jev::UnconfiguredJev),
            release_tracked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            conversation_reader: None,
            usage_pointers: crate::UsagePointerTable::default(),
            recovery: None,
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

    /// Read whether an exact actor still owns a retained watch.
    ///
    /// The facade uses this nonblocking registry observation immediately
    /// before presenting a queued watch-transition notice. It does not grant
    /// authority to poll or transfer the watch.
    #[must_use]
    pub fn retains_watch(&self, owner: ActorRef, watch: crate::WatchId) -> bool {
        self.environment.requests.retains_watch(owner, watch)
    }

    /// Read whether `owner` has already observed `watch` (via `ObserveWatch`
    /// or `pollWatch`) settled Ready or Unavailable at or after
    /// `occurred_at_unix_ms`.
    ///
    /// The facade uses this nonblocking registry observation immediately
    /// before presenting a queued watch-transition notice: a notice queued
    /// while the owner was mid-turn can describe a transition the owner
    /// already picked up by polling the same watch in the meantime, and
    /// should be acknowledged without prompting rather than re-announced.
    #[must_use]
    pub fn watch_observed_since(
        &self,
        owner: ActorRef,
        watch: crate::WatchId,
        occurred_at_unix_ms: u64,
    ) -> bool {
        self.environment
            .requests
            .watch_observed_since(owner, watch, occurred_at_unix_ms)
    }

    /// Install the host's reader for an actor's own conversation. Without one,
    /// `reflect` reports every context unbound rather than reading anything.
    #[must_use]
    pub fn with_conversation_reader(mut self, reader: crate::ConversationReader) -> Self {
        self.environment.conversation_reader = Some(reader);
        self
    }

    /// Persist actor-owned identity and lifecycle transitions before they are
    /// published to the host or an authored program.
    #[must_use]
    pub fn with_recovery_journal(mut self, journal: Arc<crate::ActorRecoveryJournal>) -> Self {
        self.environment.recovery = Some(journal);
        self
    }

    /// Keep logical IDs from being consumed by unrelated new actors while
    /// restart reconciliation decides which durable actors can be restored.
    pub fn fence_recovery_identities(
        &self,
        actors: impl IntoIterator<Item = crate::ActorId>,
    ) -> Result<(), String> {
        self.directory.fence_logical_ids(actors)
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
        self.new_program_root_with_replay(label, role, compiled, None, None)
            .await
    }

    /// Rebuild a durable logical actor as an independent scheduler root in the
    /// new host incarnation. Haskell heap state is fresh; the caller supplies
    /// only replayable source and launch configuration.
    pub async fn recover_durable_program_root(
        &self,
        durable: &crate::DurableActorAdmission,
        role: crate::EffectiveRole,
        compiled: Arc<tidepool_runtime::session::CompiledTurn>,
        worktree_custody: Option<Arc<dyn crate::ForkWorkspaceCustody>>,
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
        let current = |actor: ActorRef| ActorRef {
            id: actor.id,
            incarnation: self.incarnation,
        };
        let mut descriptor = ActorDescriptor::new(&durable.label, placement)
            .with_effective_role(role)
            .with_model(durable.model.clone().map(crate::Model::Literal))
            .with_instructions(durable.instructions.clone())
            .with_fork_effort(durable.effort.as_deref().and_then(|effort| match effort {
                "low" => Some(crate::ForkEffort::Low),
                "medium" => Some(crate::ForkEffort::Medium),
                "high" => Some(crate::ForkEffort::High),
                _ => None,
            }))
            .with_source_layer(durable.source_layer.clone());
        if let Some(creator) = durable.creator {
            descriptor = descriptor.with_creator(current(creator));
        }
        descriptor = descriptor.with_supervisor_parent(durable.supervisor_parent.map(current));
        if let Some(parent) = durable.context_parent {
            descriptor = descriptor.with_context_parent(current(parent));
        }
        let identity =
            ActorRef {
                id: durable.actor.id,
                incarnation: crate::Incarnation(
                    durable.actor.incarnation.0.checked_add(1).ok_or_else(|| {
                        std::io::Error::other("actor incarnation space exhausted")
                    })?,
                ),
            };
        match self
            .admit_prepared_root(
                descriptor,
                outcome,
                &admission,
                PreparedRootAdmission {
                    identity: Some(identity),
                    launch_worktrees: durable.launch_worktrees.clone(),
                    worktree_custody,
                    ..Default::default()
                },
            )
            .await
        {
            Ok(actor) => Ok(actor),
            Err(error) => {
                self.environment
                    .runner
                    .retire_root_placement(placement)
                    .await?;
                Err(Box::new(error))
            }
        }
    }

    /// Recover a failed root in this resident forest without replaying its
    /// retained native invocations. Only replay evidence crosses this boundary;
    /// actor identity, placement and grants are freshly admitted.
    pub async fn recover_program_root(
        &self,
        predecessor: ActorRef,
        label: String,
        role: crate::EffectiveRole,
        compiled: Arc<tidepool_runtime::session::CompiledTurn>,
    ) -> Result<
        (LocalActorRef, ractor::concurrency::JoinHandle<()>),
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let prior = self.directory.resolve(predecessor).ok_or_else(|| {
            std::io::Error::other("root recovery predecessor is not in this forest")
        })?;
        if prior
            .terminal()
            .get()
            .is_none_or(|terminal| terminal.kind != ActorExitKind::Failed)
        {
            return Err(std::io::Error::other(
                "root recovery requires a failed, terminal predecessor",
            )
            .into());
        }
        if !matches!(
            self.resident_session_state(),
            tidepool_runtime::session::ResidentSessionState::Reusable
                | tidepool_runtime::session::ResidentSessionState::Uninitialized
        ) {
            return Err(
                std::io::Error::other("root recovery resident session is not reusable").into(),
            );
        }
        let journal = {
            let mut records = self.environment.actors.lock();
            let record = records
                .get_mut(&predecessor)
                .ok_or_else(|| std::io::Error::other("root recovery evidence is unavailable"))?;
            if record.descriptor.placement().session != self.session
                || record.descriptor.creator().is_some()
                || record.descriptor.supervisor_parent().is_some()
                || record.descriptor.context_parent().is_some()
                || record.recovery_claimed
            {
                return Err(std::io::Error::other(
                    "root recovery predecessor is ineligible or already recovered",
                )
                .into());
            }
            record.recovery_claimed = true;
            record.workbench_executions.clone()
        };
        let identity =
            ActorRef {
                id: predecessor.id,
                incarnation: crate::Incarnation(
                    predecessor.incarnation.0.checked_add(1).ok_or_else(|| {
                        std::io::Error::other("actor incarnation space exhausted")
                    })?,
                ),
            };
        let result = self
            .new_program_root_with_replay(label, role, compiled, Some(journal), Some(identity))
            .await;
        if result.is_err() {
            if let Some(record) = self.environment.actors.lock().get_mut(&predecessor) {
                record.recovery_claimed = false;
            }
        }
        result
    }

    async fn new_program_root_with_replay(
        &self,
        label: String,
        role: crate::EffectiveRole,
        compiled: Arc<tidepool_runtime::session::CompiledTurn>,
        replay: Option<Arc<Mutex<WorkbenchExecutions>>>,
        identity: Option<ActorRef>,
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
                PreparedRootAdmission {
                    replay,
                    identity,
                    ..Default::default()
                },
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
            .filter(|(_, record)| record.scheduler_root)
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
        self.admit_prepared_root(descriptor, outcome, &admission, Default::default())
            .await
    }

    /// Admit the run root with a durable logical identity selected by the
    /// host recovery owner. The identity must already have been fenced.
    pub async fn admit_root_with_identity(
        &self,
        descriptor: ActorDescriptor,
        outcome: ResidentOutcome,
        identity: ActorRef,
    ) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr> {
        let admission = self.environment.root_admission_closed.read().await;
        if *admission {
            return Err(ractor::SpawnErr::StartupFailed(
                std::io::Error::other("swarm root admission is closed").into(),
            ));
        }
        self.admit_prepared_root(
            descriptor,
            outcome,
            &admission,
            PreparedRootAdmission {
                identity: Some(identity),
                ..Default::default()
            },
        )
        .await
    }

    async fn admit_prepared_root(
        &self,
        descriptor: ActorDescriptor,
        outcome: ResidentOutcome,
        _admission: &tokio::sync::RwLockReadGuard<'_, bool>,
        recovery: PreparedRootAdmission,
    ) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr> {
        let PreparedRootAdmission {
            replay,
            identity,
            launch_worktrees,
            worktree_custody,
        } = recovery;
        if descriptor.placement().session != self.session
            || (identity.is_none()
                && (descriptor.supervisor_parent().is_some()
                    || descriptor.context_parent().is_some()))
        {
            return Err(ractor::SpawnErr::StartupFailed(Box::new(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "forest root must be independent and belong to the forest machine",
                ),
            )));
        }
        let mut behavior =
            ResidentKernelBehavior::prepared(descriptor, self.environment.clone(), outcome);
        behavior.launch_worktrees = launch_worktrees;
        behavior.worktree_custody = worktree_custody;
        if let Some(replay) = replay {
            behavior.workbench_executions = replay;
        }
        match identity {
            Some(identity) => {
                crate::local_actor::spawn_local_actor_in_directory_with_identity(
                    None,
                    behavior,
                    identity,
                    self.directory.clone(),
                )
                .await
            }
            None => {
                crate::local_actor::spawn_local_actor_in_directory(
                    None,
                    behavior,
                    self.incarnation,
                    self.directory.clone(),
                )
                .await
            }
        }
    }
}

fn completed_terminal() -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "completed".into(),
    }
}

fn cell_check_rejection(
    failure: tidepool_runtime::session::CellCheckFailure,
    source: &str,
) -> WorkbenchResponse {
    let checked = failure.items.as_deref();
    let total = checked.map_or(1, |analysis| analysis.len().max(1));
    let mut items = (0..total)
        .map(|index| WorkbenchItemReceipt {
            diagnostics: Vec::new(),
            index,
            kind: None,
            span: None,
            source_items: Vec::new(),
            status: WorkbenchItemStatus::NotRun,
            output: String::new(),
            warnings: Vec::new(),
            installed_bindings: Vec::new(),
            operations: Vec::new(),
            terminal_transfer: None,
            failure_layer: None,
        })
        .collect::<Vec<_>>();
    match &failure.error {
        tidepool_runtime::CompileError::Diagnostics(diagnostics) => {
            for diagnostic in diagnostics {
                let index = diagnostic
                    .span
                    .as_ref()
                    .filter(|span| span.file == "<cell>")
                    .and_then(|span| {
                        checked.and_then(|analysis| {
                            analysis.iter().position(|item| {
                                item.source_items.iter().any(|item| {
                                    let start = (item.span.start_line, item.span.start_column);
                                    let end = (item.span.end_line, item.span.end_column);
                                    let point = (span.start_line as usize, span.start_col as usize);
                                    start <= point && point <= end
                                })
                            })
                        })
                    })
                    .unwrap_or(0);
                let rejection = tidepool_runtime::session::render_cell_compile_rejection(
                    &tidepool_runtime::CompileError::Diagnostics(vec![diagnostic.clone()]),
                    source,
                );
                // The structured form follows the diagnostic to the item the
                // rendered text was just attributed to — the span walk above
                // already decided which item that is, and this is the same
                // decision, not a second one.
                items[index]
                    .diagnostics
                    .extend(rejection.diagnostics.iter().cloned());
                if diagnostic.severity == tidepool_toolchain::diag::DiagnosticSeverity::Warning {
                    items[index].warnings.push(rejection.output);
                } else {
                    items[index].status = WorkbenchItemStatus::Rejected;
                    items[index].failure_layer = Some(WorkbenchFailureLayer::Compile);
                    if !items[index].output.is_empty() {
                        items[index].output.push_str("\n\n");
                    }
                    items[index].output.push_str(&rejection.output);
                }
            }
        }
        error => {
            let rejection = tidepool_runtime::session::render_cell_compile_rejection(error, source);
            items[0].status = WorkbenchItemStatus::Rejected;
            items[0].failure_layer = Some(WorkbenchFailureLayer::Compile);
            items[0].output = rejection.output;
            items[0].diagnostics = rejection.diagnostics;
        }
    }
    workbench_response(WorkbenchRunStatus::Rejected, items, 0, total, checked)
}

fn workbench_response(
    status: WorkbenchRunStatus,
    mut items: Vec<WorkbenchItemReceipt>,
    next_index: usize,
    total: usize,
    cell_check: Option<&[tidepool_runtime::session::CellAnalysisItem]>,
) -> WorkbenchResponse {
    let receipt_kind = |kind| match kind {
        TurnKind::Decl => WorkbenchCellItemKind::Declaration,
        TurnKind::Bind => WorkbenchCellItemKind::Statement,
        TurnKind::Expr => WorkbenchCellItemKind::Expression,
    };
    if let Some(checked) = cell_check {
        for receipt in &mut items {
            if let Some(item) = checked.get(receipt.index) {
                receipt.kind = Some(receipt_kind(item.verdict.kind));
                receipt.span = Some(item.span);
                receipt.source_items = item
                    .source_items
                    .iter()
                    .map(|source| WorkbenchCellSourceItem {
                        ordinal: source.ordinal,
                        kind: receipt_kind(source.kind),
                        span: source.span,
                    })
                    .collect();
            }
        }
    }
    let essential = items.iter().rposition(|item| {
        matches!(
            item.status,
            WorkbenchItemStatus::Stopped | WorkbenchItemStatus::Rejected
        )
    });
    let reserved = essential.map_or(0, |index| items[index].output.len().min(4096));
    let mut remaining = 64 * 1024 - reserved;
    for (index, item) in items.iter_mut().enumerate() {
        let allowance = if Some(index) == essential {
            reserved + remaining
        } else {
            remaining
        };
        item.output = crate::workbench_display::bounded_output(&item.output, allowance);
        let spent = if Some(index) == essential {
            item.output.len().saturating_sub(reserved)
        } else {
            item.output.len()
        };
        remaining = remaining.saturating_sub(spent);
    }
    let first_not_run = match status {
        WorkbenchRunStatus::Rejected | WorkbenchRunStatus::Backgrounded => {
            next_index.saturating_add(1)
        }
        WorkbenchRunStatus::Replied
        | WorkbenchRunStatus::RequestCancelled
        | WorkbenchRunStatus::Completed => next_index,
        WorkbenchRunStatus::Committed => total,
    };
    let recorded_through = items.last().map_or(0, |item| item.index + 1);
    items.extend((first_not_run.max(recorded_through)..total).map(|index| {
        WorkbenchItemReceipt {
            diagnostics: Vec::new(),
            index,
            kind: cell_check.and_then(|checked| {
                checked
                    .get(index)
                    .map(|item| receipt_kind(item.verdict.kind))
            }),
            span: cell_check.and_then(|checked| checked.get(index).map(|item| item.span)),
            source_items: cell_check
                .and_then(|checked| checked.get(index))
                .map(|item| {
                    item.source_items
                        .iter()
                        .map(|source| WorkbenchCellSourceItem {
                            ordinal: source.ordinal,
                            kind: receipt_kind(source.kind),
                            span: source.span,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            status: WorkbenchItemStatus::NotRun,
            output: String::new(),
            warnings: Vec::new(),
            installed_bindings: Vec::new(),
            operations: Vec::new(),
            terminal_transfer: None,
            failure_layer: None,
        }
    }));
    // Every terminal path through `execute_workbench` renders its receipts
    // here exactly once, so this is the one place a reconstruction can read
    // what each input unit actually produced.
    for item in &items {
        tracing::info!(
            target: "exomonad::content",
            index = item.index,
            status = ?item.status,
            output_bytes = item.output.len(),
            operations = item.operations.len(),
            diagnostics = item.diagnostics.len(),
            output = %item.output,
            "input unit receipt"
        );
        for operation in &item.operations {
            tracing::info!(
                input_unit_index = item.index,
                ordinal = operation.id.effect_ordinal,
                effect = %operation.effect,
                disposition = ?operation.disposition,
                "effect settled"
            );
        }
        for diagnostic in &item.diagnostics {
            tracing::info!(
                target: "exomonad::content",
                index = item.index,
                diagnostic = ?diagnostic,
                "input unit diagnostic"
            );
        }
    }
    WorkbenchResponse {
        status,
        summary: cell_check.map(|checked| {
            let mut declarations = 0;
            let mut statements = 0;
            let mut expressions = 0;
            for item in checked.iter().flat_map(|item| &item.source_items) {
                match item.kind {
                    TurnKind::Decl => declarations += 1,
                    TurnKind::Bind => statements += 1,
                    TurnKind::Expr => expressions += 1,
                }
            }
            let label = |count, singular| {
                if count == 1 {
                    format!("1 {singular}")
                } else {
                    format!("{count} {singular}s")
                }
            };
            format!(
                "{}, {}, {}",
                label(declarations, "declaration"),
                label(statements, "statement"),
                label(expressions, "expression")
            )
        }),
        items,
        next_index,
        total,
    }
}

#[cfg(test)]
fn lookup_response(
    prepared: Vec<crate::lookup_tool::PreparedLookup>,
    inspected: Vec<tidepool_runtime::session::InspectionResult>,
    live_modules: &[String],
    workspace_modules: &[String],
) -> crate::lookup_tool::LookupResponse {
    crate::lookup_tool::resolve(
        prepared,
        inspected,
        live_modules,
        workspace_modules,
        crate::UsagePointerTable::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        disposition_for_non_command_failure, lookup_response, workbench_failure_after_operations,
        workbench_response, ChildExitObservations,
    };
    use crate::{ActorId, ActorRef, Incarnation};
    use tidepool_runtime::session::{
        CellAnalysisItem, CellAnalysisSourceItem, CellCheck, CellSourceSpan, InfoEntry,
        InspectionAvailability, InspectionResult, ResidentError, TurnClassification, TurnKind,
        TypeMatch, TypeMatchQuality, WorkbenchCellItemKind, WorkbenchExecutionId,
        WorkbenchItemReceipt, WorkbenchItemStatus, WorkbenchOperationDisposition,
        WorkbenchOperationId, WorkbenchOperationReceipt, WorkbenchRunStatus,
    };

    /// The `watches` status view names the settled state, when the watch
    /// registered and last transitioned (relative to the actor's own
    /// session, "+Xm Ys into your session"), every pending response's age,
    /// and says plainly that a settled VALUE still needs `pollWatch` or
    /// `pollResponse` — this view only reports status.
    #[test]
    fn watches_view_renders_registration_transition_and_pending_ages_with_pollwatch_reminder() {
        let launched_at_unix_ms = Some(1_000_000);
        let overview = crate::request::WatchesOverview {
            watches: vec![
                crate::request::WatchViewEntry {
                    id: crate::WatchId(3),
                    label: "child-a".into(),
                    state: "Ready".into(),
                    registered_at_unix_ms: 1_005_000,
                    transitioned_at_unix_ms: Some(1_012_340),
                },
                crate::request::WatchViewEntry {
                    id: crate::WatchId(4),
                    label: "child-b".into(),
                    state: "Pending".into(),
                    registered_at_unix_ms: 1_002_000,
                    transitioned_at_unix_ms: None,
                },
            ],
            pending_responses: vec![crate::request::PendingResponseAge {
                id: crate::RequestId(7),
                label: "compile".into(),
                registered_at_unix_ms: 1_030_000,
            }],
        };

        let rendered = super::render_watches_view(launched_at_unix_ms, &overview);

        assert!(rendered.contains("watches (2 total):"));
        assert!(rendered.contains(
            r#"watch 3 "child-a": Ready registered +0m05s into your session transitioned +0m12s into your session"#
        ));
        assert!(rendered.contains(
            r#"watch 4 "child-b": Pending registered +0m02s into your session transitioned not yet transitioned"#
        ));
        assert!(rendered.contains("pending responses (1 total):"));
        assert!(rendered.contains(r#"request 7 "compile": registered +0m30s into your session"#));
        assert!(rendered.contains("pollWatch"));
        assert!(rendered.contains("pollResponse"));
        assert!(rendered.contains("still needs"));

        // No launch time: neither timestamp can be phrased relative to the
        // session, so both fall back plainly instead of a bogus figure.
        let unavailable = super::render_watches_view(None, &overview);
        assert!(
            unavailable.contains("watch 3 \"child-a\": Ready registered elapsed time unavailable")
        );

        // Nothing retained and nothing pending still renders the reminder,
        // never an empty or missing section.
        let empty = super::render_watches_view(
            launched_at_unix_ms,
            &crate::request::WatchesOverview {
                watches: Vec::new(),
                pending_responses: Vec::new(),
            },
        );
        assert!(empty.contains("(none registered)"));
        assert!(empty.contains("(none pending)"));
        assert!(empty.contains("pollWatch"));
    }

    #[derive(Clone, Default)]
    struct CapturedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    struct CapturedGuard(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedGuard {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedWriter {
        type Writer = CapturedGuard;

        fn make_writer(&'writer self) -> Self::Writer {
            CapturedGuard(std::sync::Arc::clone(&self.0))
        }
    }

    /// The deployment observer channel is bounded with a `try_send`
    /// overflow policy: a slow or absent receiver never makes a producer
    /// block, and once the receiver drains below capacity, sends succeed
    /// again. This exercises the mechanism directly (bypassing the full
    /// `ResidentForest`) since every production call site already reduces
    /// to exactly this: bound the queue, `try_send`, and treat `Full`
    /// exactly like a closed channel (each site's own fallback already
    /// covers "no observer").
    #[test]
    fn deployment_channel_applies_backpressure_via_bounded_try_send_not_unbounded_growth() {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<crate::LocalResidentDeployment>(1);
        let event = |summary: &str| crate::LocalResidentDeployment::Retired {
            actor: ActorRef::first(ActorId(1)),
            terminal: crate::ActorTerminal {
                kind: crate::ActorExitKind::Cancelled,
                summary: summary.to_owned(),
            },
        };

        // One event fits in the bound.
        assert!(sender.try_send(event("first")).is_ok());
        // A slow/stalled receiver leaves the channel full; the overflow
        // policy is an explicit, immediate `Full` error, never an
        // unboundedly growing queue and never a blocking send.
        let overflow = sender.try_send(event("second"));
        assert!(matches!(
            overflow,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_))
        ));

        // Once the receiver catches up, the channel accepts new events
        // again; capacity, not the count of events ever sent, bounds it.
        let drained = receiver.try_recv().expect("first event was queued");
        assert_eq!(drained.kind(), "Retired");
        assert!(sender.try_send(event("third")).is_ok());

        // Dropping every sender closes the channel; a still-pending event
        // is delivered before the receiver observes the close.
        drop(sender);
        assert_eq!(
            receiver.try_recv().expect("third event was queued").kind(),
            "Retired"
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        ));
    }

    /// `publish_watch_notifications` sends `WatchChanged`/`SettlementChanged`
    /// with `send(...).await`, not `try_send`, precisely because those two
    /// are the sole path a settled reply reaches the owning actor's inbox
    /// through: a full channel must apply backpressure, never silently
    /// drop the notice the way the other `try_send(...).ok()` producers in
    /// this module may. This exercises that exact discipline on the channel
    /// itself: a `send` on a full channel does not resolve until the
    /// receiver drains it, and the value is delivered, never lost.
    #[tokio::test]
    async fn deployment_channel_send_on_full_channel_waits_and_never_drops_a_settlement() {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<crate::LocalResidentDeployment>(1);
        let filler = crate::LocalResidentDeployment::Retired {
            actor: ActorRef::first(ActorId(1)),
            terminal: crate::ActorTerminal {
                kind: crate::ActorExitKind::Cancelled,
                summary: "filler".into(),
            },
        };
        sender.try_send(filler).expect("one slot of capacity");
        // The channel is now full: a `try_send` would report `Full`, exactly
        // as the sibling test above confirms.
        assert!(matches!(
            sender.try_send(crate::LocalResidentDeployment::Retired {
                actor: ActorRef::first(ActorId(2)),
                terminal: crate::ActorTerminal {
                    kind: crate::ActorExitKind::Cancelled,
                    summary: "would overflow".into(),
                },
            }),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_))
        ));

        let settlement_owner = ActorRef::first(ActorId(9));
        let blocked_send = tokio::spawn({
            let sender = sender.clone();
            async move {
                sender
                    .send(crate::LocalResidentDeployment::Retired {
                        actor: settlement_owner,
                        terminal: crate::ActorTerminal {
                            kind: crate::ActorExitKind::Cancelled,
                            summary: "settlement".into(),
                        },
                    })
                    .await
            }
        });

        // The channel is still full; the blocking send has not resolved.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !blocked_send.is_finished(),
            "send on a full channel must wait, not drop"
        );

        // Draining the filler frees capacity; the pending send now
        // completes and its value is delivered, never dropped.
        let drained = receiver.recv().await.expect("filler was queued");
        assert_eq!(drained.kind(), "Retired");
        blocked_send
            .await
            .expect("task did not panic")
            .expect("receiver was retained");
        let delivered = receiver.recv().await.expect("settlement was queued");
        match delivered {
            crate::LocalResidentDeployment::Retired { actor, terminal } => {
                assert_eq!(actor, settlement_owner);
                assert_eq!(terminal.summary, "settlement");
            }
            other => panic!("expected the blocked settlement send, got {}", other.kind()),
        }
    }

    #[test]
    fn a_settled_cell_renders_its_receipts_and_effects_into_the_trace() {
        let trace = CapturedWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_current_span(true)
            .with_span_list(true)
            .with_ansi(false)
            .with_writer(trace.clone())
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            let cell = tracing::info_span!("cell", execution = "exec-3");
            let _cell = cell.enter();
            workbench_response(
                WorkbenchRunStatus::Committed,
                vec![WorkbenchItemReceipt {
                    diagnostics: Vec::new(),
                    failure_layer: None,
                    index: 0,
                    kind: None,
                    span: None,
                    source_items: Vec::new(),
                    status: WorkbenchItemStatus::Committed,
                    output: "42".into(),
                    warnings: Vec::new(),
                    installed_bindings: Vec::new(),
                    operations: vec![WorkbenchOperationReceipt {
                        id: WorkbenchOperationId {
                            execution: WorkbenchExecutionId::from_digest([3; 16]),
                            input_unit_index: 0,
                            effect_ordinal: 0,
                        },
                        effect: "commandRun".into(),
                        disposition: WorkbenchOperationDisposition::Committed,
                    }],
                    terminal_transfer: None,
                }],
                1,
                1,
                None,
            );
        });

        let lines: Vec<serde_json::Value> = String::from_utf8(trace.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let receipt = lines
            .iter()
            .find(|line| line["fields"]["message"] == "input unit receipt")
            .expect("every terminal path renders its receipts once");
        assert_eq!(receipt["target"], "exomonad::content");
        assert_eq!(receipt["spans"][0]["execution"], "exec-3");
        assert_eq!(receipt["fields"]["index"], 0);
        assert_eq!(receipt["fields"]["status"], "Committed");
        assert_eq!(receipt["fields"]["output"], "42");
        assert_eq!(receipt["fields"]["output_bytes"], 2);
        let effect = lines
            .iter()
            .find(|line| line["fields"]["message"] == "effect settled")
            .expect("each operation receipt names its effect and disposition");
        assert_eq!(effect["fields"]["ordinal"], 0);
        assert_eq!(effect["fields"]["effect"], "commandRun");
        assert_eq!(effect["fields"]["disposition"], "Committed");
    }

    #[test]
    fn lookup_response_preserves_mixed_batch_and_actual_signature() {
        let prepared = crate::lookup_tool::prepare(serde_json::json!({
            "queries": [
                "awaitSettled",
                ":: NotInScope -> Int",
                ":: Response result -> Await (Settlement result)"
            ]
        }))
        .unwrap();
        let response = lookup_response(
            prepared,
            vec![
                InspectionResult::Info {
                    query: "awaitSettled".into(),
                    entries: vec![InfoEntry {
                        references: vec![],
                        name: "awaitSettled".into(),
                        module: Some("Tidepool.Agent.Watch.Internal".into()),
                        kind: "value".into(),
                        display: "awaitSettled :: Response result -> Await (Settlement result)"
                            .into(),
                        availability: InspectionAvailability::Available,
                    }],
                },
                InspectionResult::Rejected {
                    diagnostic: "NotInScope is not in scope".into(),
                },
                InspectionResult::TypeMatches {
                    query: "Response result -> Await (Settlement result)".into(),
                    matches: vec![TypeMatch {
                        references: vec![],
                        name: "awaitSettled".into(),
                        module: Some("Tidepool.Agent.Watch.Internal".into()),
                        signature: "Response result -> Await (Settlement result)".into(),
                        quality: TypeMatchQuality::Exact,
                        availability: InspectionAvailability::Available,
                    }],
                },
            ],
            &[],
            &[],
        );
        assert_eq!(response.results.len(), 3);
        assert!(matches!(
            response.results[0].outcome,
            crate::lookup_tool::LookupOutcome::Found { .. }
        ));
        assert!(matches!(
            response.results[1].outcome,
            crate::lookup_tool::LookupOutcome::Rejected { .. }
        ));
        assert!(matches!(
            response.results[2].outcome,
            crate::lookup_tool::LookupOutcome::Found { .. }
        ));
        assert!(response
            .render_text()
            .contains("awaitSettled :: Response result -> Await (Settlement result)"));
        let structured = serde_json::to_value(&response).unwrap();
        assert_eq!(
            structured["results"][0]["outcome"]["matches"][0]["availability"],
            "available"
        );
        assert_eq!(
            structured["results"][2]["outcome"]["matches"][0]["availability"],
            "available"
        );
    }

    fn info_entry(name: &str, module: &str, kind: &str, display: &str) -> InfoEntry {
        InfoEntry {
            references: vec![],
            name: name.into(),
            module: Some(module.into()),
            kind: kind.into(),
            display: display.into(),
            availability: InspectionAvailability::Available,
        }
    }

    #[test]
    fn module_shaped_lookup_renders_a_browse_result() {
        let prepared = crate::lookup_tool::prepare(serde_json::json!({
            "queries": ["Project.Investigate"]
        }))
        .unwrap();
        assert_eq!(
            prepared[0].kind,
            crate::lookup_tool::PreparedLookupKind::Qualified("Project.Investigate".into())
        );
        let response = lookup_response(
            prepared,
            // The same batch carried both interpretations: no name, a module.
            vec![
                InspectionResult::NotFound {
                    query: "Project.Investigate".into(),
                },
                InspectionResult::Browse {
                    module: "Project.Investigate".into(),
                    expanded: false,
                    entries: vec![info_entry(
                        "investigate",
                        "Project.Investigate",
                        "value",
                        "investigate :: FilePath -> IO ()",
                    )],
                },
            ],
            &[],
            &[],
        );
        assert_eq!(response.results.len(), 1);
        assert!(matches!(
            response.results[0].outcome,
            crate::lookup_tool::LookupOutcome::Found { .. }
        ));
        assert!(response
            .render_text()
            .contains("investigate :: FilePath -> IO ()"));
    }

    /// The defect this path exists for: `Cmd.CommandResult` is a qualified type
    /// and was answered `no match` because its spelling was read as a module.
    #[test]
    fn qualified_lookup_answers_the_name_when_no_such_module_exists() {
        let prepared = crate::lookup_tool::prepare(serde_json::json!({
            "queries": ["Cmd.CommandResult"]
        }))
        .unwrap();
        let response = lookup_response(
            prepared,
            vec![
                InspectionResult::Info {
                    query: "Cmd.CommandResult".into(),
                    entries: vec![info_entry(
                        "CommandResult",
                        "Tidepool.Command",
                        "type",
                        "data CommandResult",
                    )],
                },
                InspectionResult::ModuleNotFound {
                    module: "Cmd.CommandResult".into(),
                },
            ],
            &[],
            &[],
        );
        assert_eq!(
            response.render_text(),
            "Cmd.CommandResult\n  data CommandResult"
        );
    }

    /// A name that is both a type and a value answers with both entries; the
    /// module interpretation of the same batch must not displace either.
    #[test]
    fn qualified_lookup_keeps_every_entry_of_an_ambiguous_name() {
        let prepared = crate::lookup_tool::prepare(serde_json::json!({
            "queries": ["Cmd.RunResult"]
        }))
        .unwrap();
        let response = lookup_response(
            prepared,
            vec![
                InspectionResult::Ambiguous {
                    query: "Cmd.RunResult".into(),
                    entries: vec![
                        info_entry(
                            "RunResult",
                            "Tidepool.Command",
                            "type",
                            "data RunResult = RunResult",
                        ),
                        info_entry(
                            "RunResult",
                            "Tidepool.Command",
                            "constructor",
                            "RunResult :: Int -> RunResult",
                        ),
                    ],
                },
                InspectionResult::ModuleNotFound {
                    module: "Cmd.RunResult".into(),
                },
            ],
            &[],
            &[],
        );
        let crate::lookup_tool::LookupOutcome::Ambiguous { matches, .. } =
            &response.results[0].outcome
        else {
            panic!("expected ambiguous, got {:?}", response.results[0].outcome);
        };
        assert_eq!(matches.len(), 2, "{matches:?}");
        let rendered = response.render_text();
        assert!(
            rendered.contains("data RunResult = RunResult"),
            "{rendered}"
        );
        assert!(
            rendered.contains("RunResult :: Int -> RunResult"),
            "{rendered}"
        );
    }

    #[test]
    fn unresolved_module_shaped_lookup_reports_both_interpretations_it_tried() {
        let prepared = crate::lookup_tool::prepare(serde_json::json!({
            "queries": ["No.Such.Module"]
        }))
        .unwrap();
        let response = lookup_response(
            prepared,
            vec![
                InspectionResult::NotFound {
                    query: "No.Such.Module".into(),
                },
                InspectionResult::ModuleNotFound {
                    module: "No.Such.Module".into(),
                },
            ],
            &[],
            &[],
        );
        assert_eq!(response.results.len(), 1);
        assert_eq!(
            response.results[0].outcome,
            crate::lookup_tool::LookupOutcome::NotFound {
                attempted: vec![
                    crate::lookup_tool::LookupInterpretation::Name,
                    crate::lookup_tool::LookupInterpretation::Module,
                ],
                suggestions: Vec::new(),
            }
        );
        assert_eq!(
            response.render_text(),
            "No.Such.Module\n  no match: not in scope as a name, and no module of that name"
        );
    }

    /// A dotted, lowercase-final `Name` miss (`Cmd.exitCode`) sent a paired
    /// qualifier `Browse` in the same batch; its entries seed near-match
    /// suggestions on the `NotFound` outcome, and the query right after it in
    /// the batch must still land on its own result — the pairing consumes
    /// exactly two results, same as the already-landed `Qualified` shape.
    #[test]
    fn qualifier_shaped_name_miss_suggests_close_exports_without_misaligning_the_batch() {
        let prepared = crate::lookup_tool::prepare(serde_json::json!({
            "queries": ["Cmd.exitCode", "awaitSettled"]
        }))
        .unwrap();
        assert_eq!(
            prepared[0].kind,
            crate::lookup_tool::PreparedLookupKind::Name("Cmd.exitCode".into())
        );
        let response = lookup_response(
            prepared,
            vec![
                InspectionResult::NotFound {
                    query: "Cmd.exitCode".into(),
                },
                InspectionResult::Browse {
                    module: "Tidepool.Command".into(),
                    expanded: false,
                    entries: vec![
                        info_entry(
                            "commandExitCode",
                            "Tidepool.Command",
                            "value",
                            "commandExitCode :: CommandResult -> Int",
                        ),
                        info_entry(
                            "readStdout",
                            "Tidepool.Command",
                            "value",
                            "readStdout :: Job -> IO Text",
                        ),
                    ],
                },
                InspectionResult::Info {
                    query: "awaitSettled".into(),
                    entries: vec![info_entry(
                        "awaitSettled",
                        "Tidepool.Agent.Watch",
                        "value",
                        "awaitSettled :: Int",
                    )],
                },
            ],
            &[],
            &[],
        );
        assert_eq!(response.results.len(), 2);
        assert_eq!(
            response.results[0].outcome,
            crate::lookup_tool::LookupOutcome::NotFound {
                attempted: vec![crate::lookup_tool::LookupInterpretation::Name],
                suggestions: vec!["commandExitCode".into()],
            }
        );
        assert!(matches!(
            response.results[1].outcome,
            crate::lookup_tool::LookupOutcome::Found { .. }
        ));
        let rendered = response.render_text();
        assert!(
            rendered.contains(
                "Cmd.exitCode\n  no match: not in scope as a name; close: commandExitCode"
            ),
            "{rendered}"
        );
        assert!(rendered.contains("awaitSettled :: Int"), "{rendered}");
    }

    /// A qualifier-shaped `Name` query that GHC does resolve still consumes
    /// both results in the batch (the paired browse is discarded, not left
    /// for the next query to accidentally consume).
    #[test]
    fn qualifier_shaped_name_hit_discards_its_paired_browse_result() {
        let prepared = crate::lookup_tool::prepare(serde_json::json!({
            "queries": ["Cmd.readOutput"]
        }))
        .unwrap();
        let response = lookup_response(
            prepared,
            vec![
                InspectionResult::Info {
                    query: "Cmd.readOutput".into(),
                    entries: vec![info_entry(
                        "readOutput",
                        "Tidepool.Command",
                        "value",
                        "readOutput :: Job -> IO Text",
                    )],
                },
                InspectionResult::Browse {
                    module: "Tidepool.Command".into(),
                    expanded: false,
                    entries: vec![info_entry(
                        "readOutput",
                        "Tidepool.Command",
                        "value",
                        "readOutput :: Job -> IO Text",
                    )],
                },
            ],
            &[],
            &[],
        );
        assert_eq!(response.results.len(), 1);
        assert!(matches!(
            response.results[0].outcome,
            crate::lookup_tool::LookupOutcome::Found { .. }
        ));
        assert!(response
            .render_text()
            .contains("readOutput :: Job -> IO Text"));
    }

    /// Queries answered locally consume no compiler result and a dotted
    /// capitalized query consumes exactly two, so a mixed batch stays aligned:
    /// every query must land on the results issued for it and no other.
    #[test]
    fn a_mixed_batch_keeps_each_query_on_its_own_results() {
        let prepared = crate::lookup_tool::prepare(serde_json::json!({
            "queries": [
                "doc topics", "", "Cmd.CommandResult", "Project.Investigate",
                "Cmd.CommandExited", "Unknown.Module"
            ]
        }))
        .unwrap();
        let response = lookup_response(
            prepared,
            vec![
                InspectionResult::Info {
                    query: "Cmd.CommandResult".into(),
                    entries: vec![info_entry(
                        "CommandResult",
                        "Tidepool.Command",
                        "type",
                        "data CommandResult",
                    )],
                },
                InspectionResult::ModuleNotFound {
                    module: "Cmd.CommandResult".into(),
                },
                InspectionResult::NotFound {
                    query: "Project.Investigate".into(),
                },
                InspectionResult::Browse {
                    module: "Project.Investigate".into(),
                    expanded: false,
                    entries: vec![info_entry(
                        "investigate",
                        "Project.Investigate",
                        "value",
                        "investigate :: FilePath -> IO ()",
                    )],
                },
                InspectionResult::Ambiguous {
                    query: "Cmd.CommandExited".into(),
                    entries: vec![info_entry(
                        "CommandExited",
                        "Tidepool.Command",
                        "constructor",
                        "CommandExited :: Int -> CommandOutcome",
                    )],
                },
                InspectionResult::ModuleNotFound {
                    module: "Cmd.CommandExited".into(),
                },
                InspectionResult::NotFound {
                    query: "Unknown.Module".into(),
                },
                InspectionResult::ModuleNotFound {
                    module: "Unknown.Module".into(),
                },
            ],
            &[],
            &[],
        );
        let queries: Vec<&str> = response
            .results
            .iter()
            .map(|result| result.query.as_str())
            .collect();
        assert_eq!(
            queries,
            [
                "doc topics",
                "",
                "Cmd.CommandResult",
                "Project.Investigate",
                "Cmd.CommandExited",
                "Unknown.Module"
            ]
        );
        assert!(matches!(
            response.results[1].outcome,
            crate::lookup_tool::LookupOutcome::Rejected { .. }
        ));
        assert!(matches!(
            response.results[4].outcome,
            crate::lookup_tool::LookupOutcome::Ambiguous { .. }
        ));
        assert_eq!(
            response.results[5].outcome,
            crate::lookup_tool::LookupOutcome::NotFound {
                attempted: vec![
                    crate::lookup_tool::LookupInterpretation::Name,
                    crate::lookup_tool::LookupInterpretation::Module,
                ],
                suggestions: Vec::new(),
            }
        );
        let rendered = response.render_text();
        assert!(
            rendered.contains("Cmd.CommandResult\n  data CommandResult"),
            "{rendered}"
        );
        assert!(
            rendered
                .contains("Project.Investigate\n  [available] investigate :: FilePath -> IO ()"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Cmd.CommandExited\n  CommandExited :: Int -> CommandOutcome"),
            "{rendered}"
        );
    }

    /// One query's inspection failing is not the batch failing: the results
    /// that resolved are still rendered, in their own places.
    #[test]
    fn a_failed_query_leaves_the_rest_of_the_batch_rendered() {
        let prepared = crate::lookup_tool::prepare(serde_json::json!({
            "queries": ["awaitSettled", "Broken.Query", "Project.Investigate"]
        }))
        .unwrap();
        let response = lookup_response(
            prepared,
            vec![
                InspectionResult::Info {
                    query: "awaitSettled".into(),
                    entries: vec![info_entry(
                        "awaitSettled",
                        "Tidepool.Agent.Watch",
                        "value",
                        "awaitSettled :: Int",
                    )],
                },
                InspectionResult::NotFound {
                    query: "Broken.Query".into(),
                },
                InspectionResult::Rejected {
                    diagnostic: "inspection unavailable".into(),
                },
                InspectionResult::NotFound {
                    query: "Project.Investigate".into(),
                },
                InspectionResult::Browse {
                    module: "Project.Investigate".into(),
                    expanded: false,
                    entries: vec![info_entry(
                        "investigate",
                        "Project.Investigate",
                        "value",
                        "investigate :: FilePath -> IO ()",
                    )],
                },
            ],
            &[],
            &[],
        );
        assert_eq!(response.results.len(), 3);
        assert!(matches!(
            response.results[1].outcome,
            crate::lookup_tool::LookupOutcome::Rejected { .. }
        ));
        let rendered = response.render_text();
        assert!(
            rendered.contains("[available] awaitSettled :: Int"),
            "{rendered}"
        );
        assert!(
            rendered.contains("error: inspection unavailable"),
            "{rendered}"
        );
        assert!(
            rendered.contains("investigate :: FilePath -> IO ()"),
            "{rendered}"
        );
    }

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
        let item = |kind, line| {
            let span = CellSourceSpan {
                start_line: line,
                start_column: 1,
                end_line: line,
                end_column: 8,
            };
            CellAnalysisItem {
                span,
                source: String::new(),
                prologue_only: false,
                verdict: TurnClassification {
                    kind,
                    binders: Vec::new(),
                    items: Vec::new(),
                },
                source_items: vec![CellAnalysisSourceItem {
                    ordinal: line - 1,
                    span,
                    kind,
                }],
            }
        };
        let checked = CellCheck {
            prologue: Default::default(),
            items: vec![
                item(TurnKind::Decl, 1),
                item(TurnKind::Bind, 2),
                item(TurnKind::Bind, 3),
                item(TurnKind::Expr, 4),
            ],
            pins: Vec::new(),
            checked_source: String::new(),
            checked_cell_text: String::new(),
            compile_generation: 0,
            compile_view_evidence: String::new(),
            expression_plans: Vec::new(),
        };
        let committed = WorkbenchItemReceipt {
            diagnostics: Vec::new(),
            index: 0,
            kind: None,
            span: None,
            source_items: Vec::new(),
            status: WorkbenchItemStatus::Committed,
            output: "[bound prior]".into(),
            warnings: Vec::new(),
            installed_bindings: vec!["prior".into()],
            operations: Vec::new(),
            terminal_transfer: None,
            failure_layer: None,
        };
        let response = workbench_response(
            WorkbenchRunStatus::Rejected,
            vec![
                committed.clone(),
                WorkbenchItemReceipt {
                    diagnostics: Vec::new(),
                    index: 1,
                    kind: None,
                    span: None,
                    source_items: Vec::new(),
                    status: WorkbenchItemStatus::Rejected,
                    output: "<cell item 2>: runtime error: pattern match failure: Just x".into(),
                    warnings: Vec::new(),
                    installed_bindings: Vec::new(),
                    operations: Vec::new(),
                    terminal_transfer: None,
                    failure_layer: None,
                },
            ],
            1,
            4,
            Some(&checked.items),
        );
        assert_eq!(
            response.summary.as_deref(),
            Some("1 declaration, 2 statements, 1 expression")
        );
        assert_eq!(response.items.len(), 4);
        assert_eq!(
            response.items[0].kind,
            Some(WorkbenchCellItemKind::Declaration)
        );
        assert_eq!(response.items[0].span.unwrap().start_line, 1);
        assert_eq!(response.items[1].status, WorkbenchItemStatus::Rejected);
        assert_eq!(
            response.items[1].kind,
            Some(WorkbenchCellItemKind::Statement)
        );
        assert!(response.items[1].installed_bindings.is_empty());
        assert_eq!(response.items[2].status, WorkbenchItemStatus::NotRun);
        assert_eq!(response.items[3].status, WorkbenchItemStatus::NotRun);
        assert_eq!(
            response.items[3].kind,
            Some(WorkbenchCellItemKind::Expression)
        );
        assert!(response.items[2..]
            .iter()
            .all(|item| item.installed_bindings.is_empty()));
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

    #[test]
    fn effect_response_delivered_then_downstream_failure_commits() {
        // `Delivered` is raised only at the `resume`/`resume_handle`/
        // `resume_framed_custody` call sites in `resident_workbench.rs`,
        // i.e. only once the boundary's response has already crossed into
        // the resident machine. A failure carrying it must not be reported
        // `Unknown` (workbench.rs:265-268): the mutation is known to have
        // crossed the commit point even though something downstream then
        // failed (this is the `reflect` conversation-reader bug: the value
        // was delivered and only a later step failed).
        let error = crate::ResidentActorWorkbenchError::Delivered(ResidentError::ForeignCustody);
        assert_eq!(
            disposition_for_non_command_failure(&error),
            WorkbenchOperationDisposition::Committed
        );
    }

    #[test]
    fn effect_failure_before_delivery_stays_unknown() {
        // A failure produced before any response reached the runner (e.g.
        // the arm's own answer computation) proves nothing about whether a
        // mutation crossed the commit point, so it keeps the conservative
        // `Unknown` disposition.
        let error = crate::ResidentActorWorkbenchError::ActorProtocol(
            "dispatch failed before delivery".into(),
        );
        assert_eq!(
            disposition_for_non_command_failure(&error),
            WorkbenchOperationDisposition::Unknown
        );
    }

    #[test]
    fn effect_failure_via_bare_resident_variant_stays_unknown() {
        // `Resident` (unlike `Delivered`) also covers failures BEFORE a
        // response reaches the machine, e.g. `set_actor_execution` in
        // `with_machine_wait`. It must not be reclassified as `Committed`
        // just because it wraps the same `ResidentError` payload as
        // `Delivered` can.
        let error = crate::ResidentActorWorkbenchError::Resident(ResidentError::ForeignCustody);
        assert_eq!(
            disposition_for_non_command_failure(&error),
            WorkbenchOperationDisposition::Unknown
        );
    }
}
