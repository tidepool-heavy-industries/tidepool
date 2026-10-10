//! Resident Haskell behavior owned directly by one local actor.
//!
//! Ractor serializes logical turns and owns the mailbox. The shared resident
//! machine registry owns only short-lived machine checkout. This module is
//! the single driver between those boundaries; it does not recreate registry
//! turn leases, parked-obligation maps, or a host-side scheduler.

use std::sync::{Arc, OnceLock};

#[cfg(test)]
mod activation_publication_tests;
mod after_tool_wait;
mod agent_retention;
#[cfg(test)]
mod capture_workspace_tests;
pub(crate) mod child_launch;
mod clock_wait;
mod command_presentation;
mod command_settlement;
mod commands;
mod display_settlement;
mod drain_wait;
#[cfg(test)]
mod forest_shutdown_tests;
pub(crate) mod forms;
pub(crate) mod green;
mod green_notebook;
#[cfg(test)]
mod green_runtime_tests;
mod green_tool;
mod inspection_wait;
mod invocation_effects;
pub(crate) mod invocation_work;
#[cfg(test)]
mod jev_form_runtime_tests;
mod owned_workbench;
#[cfg(test)]
mod provider_owner_tests;
mod replacement;
mod request_wait;
#[cfg(test)]
mod scope_runtime_tests;
mod scopes;
mod status_rendering;
#[cfg(test)]
mod terminal_transfer_tests;
mod terminal_wait;
mod tool_support;
mod workbench_ledger;

#[cfg(test)]
pub(crate) use drain_wait::{wait_drain_event, DrainWaitEvent};
pub(crate) use owned_workbench::{
    ExecutionResourceOwners, WorkbenchCompilationAuthority, WorkbenchPublicOwner,
};

use display_settlement::{
    DisplayExecutionSettlement, DisplayOperationSettlement, DisplayPacketSettlement,
    DisplayReceiptSubmission,
};
use invocation_work::{
    ensure_workbench_execution_id, retain_invocation_cleanup_summary, InvocationWork,
};
use status_rendering::{
    render_bindings_section, render_job_line, render_revisions_section, render_roster_changes,
    render_source_drift_section, RevisionIdentities, RosterSnapshot,
};
use workbench_ledger::{WorkbenchBoundaryRecord, WorkbenchExecutions, WorkbenchReplayFailure};

use parking_lot::Mutex;
use tidepool_bridge_effects::CommandPresentation;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_runtime::session::{
    truncate_preview_at_line, CellSourceSpan, OutputSink, ParsedBlock, ResidentHole,
    ResidentOutcome, ResidentSession, RootCustody, TurnKind, WorkbenchCellItemKind,
    WorkbenchCellSourceItem, WorkbenchDisplayOutput, WorkbenchDisplayPage, WorkbenchExecutionId,
    WorkbenchFailureLayer, WorkbenchFailurePoint, WorkbenchItemReceipt, WorkbenchItemStatus,
    WorkbenchOperationDisposition, WorkbenchOperationId, WorkbenchOperationReceipt,
    WorkbenchPublicationOutcome, WorkbenchRequest, WorkbenchResponse, WorkbenchRunStatus,
    WorkbenchTerminalTransfer,
};
use tokio::sync::mpsc;
use tracing::Instrument;

use crate::mailbox::{InstalledReceiver, ResidentOutbound};
use crate::request::{RequestRegistry, RequestReservationOwner};
use crate::resident_workbench::{
    AgentStopProjection, ContextCheckpointBoundary, PreparedCell, ResidentActorBoundary,
    ResidentActorStartupStep, ResidentKernelBoundary, ResidentWorkbenchFragment,
    ResidentWorkbenchStep, ResidentWorkbenchSuspension,
};
use crate::{
    ActorDescriptor, ActorExitKind, ActorMachineRegistry, ActorRef, ActorSessionContext,
    ActorTerminal, ActorWorkbenchSource, ChildExitNotice, ExternalApplicationFailure,
    ExternalFailureDisposition, HostedCheckpointAttachment, KernelBehavior, KernelBehaviorError,
    KernelCallFailure, KernelContext, KernelInvocationFailure, KernelMessage, KernelStep,
    LocalActorRef, MailboxValue, ResidentActorRunner, ResidentActorWorkbenchError,
    ResidentToolEndpoint,
};

/// The original executable retained by the actor's existing boot owner.
pub enum ResidentRootEntry {
    Prepared(ResidentOutcome),
    Startup(tidepool_runtime::session::PreparedStartupEntry),
}

/// A compiled root at the point where ownership moves into its local actor.
pub struct ResidentActorRoot<H, O> {
    descriptor: ActorDescriptor,
    machine: ResidentSession<H, O>,
    outcome: ResidentRootEntry,
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
            outcome: ResidentRootEntry::Prepared(outcome),
        }
    }

    /// Native installation has completed, but no authored entry has executed.
    pub fn pending(
        descriptor: ActorDescriptor,
        machine: ResidentSession<H, O>,
        entry: tidepool_runtime::session::PreparedStartupEntry,
    ) -> Self {
        Self {
            descriptor,
            machine,
            outcome: ResidentRootEntry::Startup(entry),
        }
    }

    pub fn into_parts(self) -> (ActorDescriptor, ResidentSession<H, O>, ResidentRootEntry) {
        (self.descriptor, self.machine, self.outcome)
    }
}

#[derive(Clone)]
pub struct LocalResidentInstallation {
    /// Newly prepared handler custody transfers only with application publication.
    pub(crate) prepared_tools: Option<crate::InstalledToolLease>,
    /// Exact installation observation survives handler transfer without retaining its custody.
    toolset_acquisition: Option<crate::ToolsetAcquisition>,
    pub actor: LocalActorRef,
    pub label: String,
    pub policy: Arc<dyn ResidentToolEndpoint>,
    pub initial_user_message: Option<String>,
    pub fresh_context_seed: Option<String>,
    pub spawn_admission: Option<crate::SpawnAdmission>,
    pub launch_worktrees: Vec<String>,
    pub worktree_custody: Option<Arc<dyn crate::WorkspaceCustody>>,
    pub capabilities: crate::ActorCapabilities,
    pub fork_effort: Option<crate::ForkEffort>,
    pub model: Option<crate::Model>,
    pub instructions: Option<String>,
    pub creator: Option<crate::ActorRef>,
    pub checkpoint_boundary: Option<tidepool_runtime::session::ContextCheckpointBoundary>,
    pub checkpoint: Option<crate::CheckpointLease>,
    /// Captured when the child is admitted, before release can revoke new users.
    pub checkpoint_attachment: Option<HostedCheckpointAttachment>,
    pub supervisor_parent: Option<crate::ActorRef>,
    pub context_parent: Option<crate::ActorRef>,
    pub runtime_observation: crate::ActorRuntimeObservationHandle,
}

impl LocalResidentInstallation {
    #[must_use]
    pub fn toolset_acquisition(&self) -> Option<&crate::ToolsetAcquisition> {
        self.toolset_acquisition.as_ref()
    }
}

#[derive(Clone)]
pub enum LocalResidentDeployment {
    DisplayPublished(Arc<DisplayPublication>),
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
            Self::DisplayPublished(_) => "DisplayPublished",
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

/// Publication uses the actor's issued slot and page counter, independent of
/// expansion keys, which may recur across pages.
pub struct DisplayPublication {
    pub actor: ActorRef,
    pub page_ordinal: u64,
    pub operation: Option<WorkbenchOperationId>,
    pub page: tidepool_runtime::session::WorkbenchDisplayPage,
    host_context: OnceLock<DisplayPublicationHostContext>,
    receipt: OnceLock<Arc<DisplayPacketSettlement>>,
    was_unconfirmed: std::sync::atomic::AtomicBool,
    outcome: Mutex<Option<DisplayPublicationOutcome>>,
    reply: Mutex<Option<tokio::sync::oneshot::Sender<DisplayPublicationOutcome>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayPublicationOutcome {
    Published(tidepool_runtime::session::ActorOutputReference),
    /// The host confirms no output row was committed.
    Refused(String),
    /// The exact emission remains retained until its durability is resolved.
    Unconfirmed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayPublicationHostContext {
    pub run: String,
    pub conversation: Option<DisplayConversationIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayConversationIdentity {
    pub run: String,
    pub actor: String,
    pub incarnation: String,
}

impl DisplayPublication {
    fn channel(
        actor: ActorRef,
        page_ordinal: u64,
        operation: Option<WorkbenchOperationId>,
        page: tidepool_runtime::session::WorkbenchDisplayPage,
    ) -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<DisplayPublicationOutcome>,
    ) {
        let (reply, answer) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                actor,
                page_ordinal,
                operation,
                page,
                host_context: OnceLock::new(),
                receipt: OnceLock::new(),
                was_unconfirmed: std::sync::atomic::AtomicBool::new(false),
                outcome: Mutex::new(None),
                reply: Mutex::new(Some(reply)),
            }),
            answer,
        )
    }

    /// Retain the answer before waking its original execution. Losing that
    /// waiter cannot lose a committed page's callback authority.
    pub fn answer(&self, answer: DisplayPublicationOutcome) -> bool {
        let mut outcome = self.outcome.lock();
        // A refusal from a later attempt cannot establish that an earlier
        // uncertain attempt did not commit. Serialize this fence with timeout.
        let answer = match answer {
            DisplayPublicationOutcome::Published(reference)
                if !valid_display_reference(self, &reference) =>
            {
                DisplayPublicationOutcome::Unconfirmed(
                    "host returned an invalid display output reference".into(),
                )
            }
            DisplayPublicationOutcome::Refused(detail) if self.was_unconfirmed() => {
                DisplayPublicationOutcome::Unconfirmed(detail)
            }
            answer => answer,
        };
        let answer = match answer {
            DisplayPublicationOutcome::Refused(detail) => DisplayPublicationOutcome::Refused(
                crate::workbench_display::bounded_output(&detail, 2048),
            ),
            DisplayPublicationOutcome::Unconfirmed(detail) => {
                DisplayPublicationOutcome::Unconfirmed(crate::workbench_display::bounded_output(
                    &detail, 2048,
                ))
            }
            answer => answer,
        };
        if matches!(
            &*outcome,
            Some(DisplayPublicationOutcome::Published(_) | DisplayPublicationOutcome::Refused(_))
        ) || matches!(
            (&*outcome, &answer),
            (
                Some(DisplayPublicationOutcome::Unconfirmed(_)),
                DisplayPublicationOutcome::Unconfirmed(_)
            )
        ) {
            return false;
        }
        if matches!(&answer, DisplayPublicationOutcome::Unconfirmed(_)) {
            self.was_unconfirmed
                .store(true, std::sync::atomic::Ordering::Release);
        }
        if let Some(receipt) = self.receipt.get() {
            receipt.answer(&answer);
        }
        *outcome = Some(answer.clone());
        if let Some(reply) = self.reply.lock().take() {
            let _ = reply.send(answer);
        }
        true
    }

    pub fn outcome(&self) -> Option<DisplayPublicationOutcome> {
        self.outcome.lock().clone()
    }

    /// Retrying never erases the possibility of an earlier durable commit.
    pub fn was_unconfirmed(&self) -> bool {
        self.was_unconfirmed
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// The first host admission freezes provenance for exact Store retries.
    pub fn admit_host_context(
        &self,
        context: DisplayPublicationHostContext,
    ) -> &DisplayPublicationHostContext {
        self.host_context.get_or_init(|| context)
    }

    pub fn host_context(&self) -> Option<&DisplayPublicationHostContext> {
        self.host_context.get()
    }

    fn retry_channel(&self) -> Option<tokio::sync::oneshot::Receiver<DisplayPublicationOutcome>> {
        let mut outcome = self.outcome.lock();
        if !matches!(&*outcome, Some(DisplayPublicationOutcome::Unconfirmed(_))) {
            return None;
        }
        let (reply, answer) = tokio::sync::oneshot::channel();
        *self.reply.lock() = Some(reply);
        *outcome = None;
        Some(answer)
    }
}

/// One supervisor's wait for a stopped actor's release receipt. The host
/// answers at most once; a dropped request answers nobody.
pub struct ReleaseAwait {
    pub actor: ActorRef,
    reply: Mutex<Option<tokio::sync::oneshot::Sender<ResourceRelease>>>,
}

impl ReleaseAwait {
    /// Create an exact-actor release observation and its acknowledgement.
    pub fn channel(
        actor: ActorRef,
    ) -> (Arc<Self>, tokio::sync::oneshot::Receiver<ResourceRelease>) {
        let (reply, release) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                actor,
                reply: Mutex::new(Some(reply)),
            }),
            release,
        )
    }

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
    actor_admissions: crate::ActorAdmissionRegistry,
    actors: Arc<Mutex<std::collections::HashMap<ActorRef, ResidentActorRecord>>>,
    fork_workspaces: Option<crate::fork_workspace::SharedWorkspaceAdmission>,
    root_admission_closed: Arc<tokio::sync::RwLock<bool>>,
    /// Installed by a host that keeps a source layer per checkout. Without
    /// one every actor compiles against exactly the deployment-wide roots.
    source_layers: Option<crate::ActorSourceLayerResolver>,
    jev: crate::JevBackendHandle,
    cell_model_factory: Option<Arc<dyn crate::CellModelFactory>>,
    form_host: Option<Arc<dyn crate::FormHost>>,
    form_registry:
        Arc<Mutex<std::collections::HashMap<String, std::sync::Weak<crate::forms::MountedForm>>>>,
    /// Set by an actor host that answers `ReleaseAwait`; without one a stop
    /// has no interactive resources to wait for.
    release_tracked: Arc<std::sync::atomic::AtomicBool>,
    conversation_reader: Option<crate::ConversationReader>,
    usage_pointers: crate::UsagePointerTable,
    recovery: Option<Arc<crate::ActorRecoveryJournal>>,
}

fn retain_retired_metadata<H, O>(
    environment: &ResidentEnvironment<H, O>,
    actor: ActorRef,
    terminal: &ActorTerminal,
) {
    environment.actor_admissions.fail_issuer_checkpoints(actor);
    if let Some(recovery) = &environment.recovery {
        if let Err(error) = recovery.retire(actor, terminal.clone()) {
            tracing::error!(?actor, %error, "actor terminal evidence remains uncertain");
        }
    }
    environment.actor_admissions.retire_actor(actor);
    if let Some(record) = environment.actors.lock().get_mut(&actor) {
        record.descriptor.source_imports().release_capture();
        record.terminal = Some(terminal.clone());
        record.displays.lock().retire();
    }
}

fn publish_retired<H, O>(
    environment: &ResidentEnvironment<H, O>,
    actor: ActorRef,
    terminal: ActorTerminal,
) {
    retain_retired_metadata(environment, actor, &terminal);
    let mut retired = environment.retired.lock();
    if !retired.contains(&actor)
        && environment
            .deployments
            .try_send(LocalResidentDeployment::Retired { actor, terminal })
            .is_ok()
    {
        retired.insert(actor);
    }
}

/// Confirm admission through the original retirement publisher. The exact
/// incarnation enters `retired` only after channel admission, so retry cannot
/// duplicate a delivered host cleanup or hide a failed send.
async fn publish_retired_confirmed<H, O>(
    environment: &ResidentEnvironment<H, O>,
    actor: ActorRef,
    terminal: ActorTerminal,
) -> Result<(), String> {
    retain_retired_metadata(environment, actor, &terminal);
    if environment.retired.lock().contains(&actor) {
        return Ok(());
    }
    let permit = tokio::time::timeout(RELEASE_WAIT, environment.deployments.reserve())
        .await
        .map_err(|_| "host retirement admission remains pending".to_owned())?
        .map_err(|error| format!("host retirement admission unavailable: {error}"))?;
    let mut retired = environment.retired.lock();
    if !retired.contains(&actor) {
        permit.send(LocalResidentDeployment::Retired { actor, terminal });
        retired.insert(actor);
    }
    Ok(())
}

#[derive(Clone)]
struct ResidentActorRecord {
    root_startup: Option<Arc<Mutex<RootStartupState>>>,
    public_owner: ActorPublicOwnerPlane,
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
    displays: Arc<Mutex<ActorDisplays>>,
}

impl ResidentActorRecord {
    fn owns_display_resources(&self, context: &ActorSessionContext) -> bool {
        let placement = self.descriptor.placement();
        self.terminal.is_none()
            && placement.session == context.placement.session
            && placement.resource_scope == context.placement.resource_scope
    }
}

const DEFAULT_DISPLAY_CHARACTER_ALLOWANCE: i64 = 8192;
const DISPLAY_METADATA_BYTE_LIMIT: usize = 8192;

/// Reserve the output separator and four UTF-8 bytes per character so an
/// accepted page fits the existing transport allowance without truncation.
fn display_character_allowance(remaining_bytes: usize) -> i64 {
    (remaining_bytes.saturating_sub(1) / 4).min(DEFAULT_DISPLAY_CHARACTER_ALLOWANCE as usize) as i64
}

pub(crate) fn validate_display_page(
    text: &str,
    allowance: i64,
) -> Result<(), ResidentActorWorkbenchError> {
    let allowance = allowance.clamp(0, DEFAULT_DISPLAY_CHARACTER_ALLOWANCE) as usize;
    if text.len() > allowance * 4 || text.chars().take(allowance + 1).count() > allowance {
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "display page exceeds its current output allowance".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_display_metadata(
    output: &WorkbenchDisplayPage,
) -> Result<(), ResidentActorWorkbenchError> {
    #[derive(serde::Serialize)]
    struct Metadata<'a> {
        identity: (i64, i64, i64),
        expansions: &'a [(i64, String)],
        unavailable: bool,
    }

    // Reject an oversized lower bound before allocating the escaped encoding.
    let raw_bytes = output
        .expansions
        .iter()
        .try_fold(0usize, |bytes, (_, label)| {
            bytes.checked_add(label.len())?.checked_add(1)
        });
    if !raw_bytes.is_some_and(|bytes| bytes <= DISPLAY_METADATA_BYTE_LIMIT) {
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "display metadata exceeds its encoded output budget".into(),
        ));
    }
    let encoded = serde_json::to_vec(&Metadata {
        identity: output.identity,
        expansions: &output.expansions,
        unavailable: output.unavailable,
    })
    .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
    if encoded.len() > DISPLAY_METADATA_BYTE_LIMIT {
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "display metadata exceeds its encoded output budget".into(),
        ));
    }
    Ok(())
}

#[derive(Default)]
struct ActorDisplays {
    next_slot: i64,
    slots: std::collections::HashMap<i64, DisplaySlot>,
    retired: bool,
}

struct DisplaySlot {
    callback: Option<Arc<RootCustody>>,
    keys: Vec<(i64, String)>,
    expanding: Option<u64>,
    next_expansion: u64,
    page_ordinal: u64,
    pending: Option<StagedDisplayPublication>,
}

struct StagedDisplayPublication {
    request: Arc<DisplayPublication>,
    callback: Option<Arc<RootCustody>>,
}

fn valid_display_reference(
    request: &DisplayPublication,
    reference: &tidepool_runtime::session::ActorOutputReference,
) -> bool {
    !reference.run.is_empty()
        && reference.run.len() <= 1024
        && reference.sequence > 0
        && request
            .host_context()
            .is_none_or(|context| context.run == reference.run)
}

impl ActorDisplays {
    fn reserve_rich_slot(&mut self) -> Result<u64, ResidentActorWorkbenchError> {
        if self.retired {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "display actor is retired".into(),
            ));
        }
        let slot = self.next_slot.checked_add(1).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol("display slot space exhausted".into())
        })?;
        self.next_slot = slot;
        Ok(slot as u64)
    }
    fn pending_count(&mut self) -> usize {
        for slot in self.slots.keys().copied().collect::<Vec<_>>() {
            let _ = self.reconcile(slot);
        }
        self.slots
            .values()
            .filter(|slot| slot.pending.is_some())
            .count()
    }
    fn retire(&mut self) {
        self.retired = true;
        for slot in self.slots.values_mut() {
            slot.callback = None;
            slot.keys.clear();
            slot.expanding = None;
            if let Some(pending) = &mut slot.pending {
                pending.callback = None;
            }
        }
        // Only accepted, unsettled emission metadata survives resource release.
        self.slots.retain(|_, slot| slot.pending.is_some());
    }

    fn validate_identity(
        actor: ActorRef,
        identity: (i64, i64, i64),
    ) -> Result<i64, ResidentActorWorkbenchError> {
        let owner = actor_address(actor);
        if (identity.0, identity.1) != owner || identity.2 <= 0 {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "display belongs to a different actor incarnation or has an invalid slot".into(),
            ));
        }
        Ok(identity.2)
    }

    fn stage(
        &mut self,
        actor: ActorRef,
        mut output: WorkbenchDisplayPage,
        callback: Option<RootCustody>,
        update: bool,
        allowance: i64,
        operation: Option<WorkbenchOperationId>,
    ) -> Result<
        (
            Arc<DisplayPublication>,
            tokio::sync::oneshot::Receiver<DisplayPublicationOutcome>,
        ),
        ResidentActorWorkbenchError,
    > {
        if self.retired {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "display actor is retired".into(),
            ));
        }
        validate_display_page(&output.text, allowance)?;
        if update == (output.identity == (0, 0, 0)) {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "display publication does not match its invocation authority".into(),
            ));
        }
        let slot = if output.identity == (0, 0, 0) {
            let next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol("display slot space exhausted".into())
            })?;
            let owner = actor_address(actor);
            output.identity = (owner.0, owner.1, next_slot);
            next_slot
        } else {
            let slot = Self::validate_identity(actor, output.identity)?;
            if !self
                .slots
                .get(&slot)
                .is_some_and(|slot| slot.expanding.is_some() && slot.pending.is_none())
            {
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "display slot is unavailable".into(),
                ));
            }
            slot
        };
        validate_display_metadata(&output)?;
        let mut seen = std::collections::HashSet::new();
        if output
            .expansions
            .iter()
            .any(|(key, _)| *key <= 0 || !seen.insert(*key))
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "display contains invalid or duplicate expansion keys".into(),
            ));
        }
        let ordinal = self
            .slots
            .get(&slot)
            .map_or(0, |slot| slot.page_ordinal)
            .checked_add(1)
            .filter(|ordinal| *ordinal <= i64::MAX as u64)
            .ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol("display page ordinal exhausted".into())
            })?;
        let callback = if output.expansions.is_empty() {
            None
        } else {
            Some(Arc::new(callback.ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    "expandable display has no retained callback".into(),
                )
            })?))
        };
        if !update {
            self.next_slot = slot;
            self.slots.insert(
                slot,
                DisplaySlot {
                    callback: None,
                    keys: Vec::new(),
                    expanding: None,
                    next_expansion: 0,
                    page_ordinal: 0,
                    pending: None,
                },
            );
        }
        let owned = self.slots.get_mut(&slot).expect("issued display slot");
        let (request, answer) = DisplayPublication::channel(actor, ordinal, operation, output);
        owned.pending = Some(StagedDisplayPublication {
            request: request.clone(),
            callback,
        });
        Ok((request, answer))
    }

    fn reconcile(&mut self, slot: i64) -> Result<(), ResidentActorWorkbenchError> {
        let Some(owned) = self.slots.get_mut(&slot) else {
            return Ok(());
        };
        let Some(pending) = &owned.pending else {
            return Ok(());
        };
        match pending.request.outcome() {
            Some(DisplayPublicationOutcome::Published(reference))
                if valid_display_reference(&pending.request, &reference) =>
            {
                let pending = owned.pending.take().expect("acknowledged publication");
                if !self.retired {
                    owned.callback = pending.callback;
                    owned.keys = pending.request.page.expansions.clone();
                }
                owned.page_ordinal = pending.request.page_ordinal;
                owned.expanding = None;
                Ok(())
            }
            Some(DisplayPublicationOutcome::Refused(detail)) => {
                owned.pending = None;
                owned.expanding = None;
                Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                    "display publication refused: {detail}"
                )))
            }
            Some(DisplayPublicationOutcome::Unconfirmed(detail)) => {
                Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                    "display publication remains unconfirmed: {detail}"
                )))
            }
            Some(DisplayPublicationOutcome::Published(_)) => {
                Err(ResidentActorWorkbenchError::ActorProtocol(
                    "display publication returned an invalid durable reference".into(),
                ))
            }
            None => Ok(()),
        }
    }

    fn select(
        &mut self,
        actor: ActorRef,
        identity: (i64, i64, i64),
        key: i64,
    ) -> Result<(Arc<RootCustody>, u64), ResidentActorWorkbenchError> {
        if self.retired {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "display actor is retired".into(),
            ));
        }
        let slot = Self::validate_identity(actor, identity)?;
        let _ = self.reconcile(slot);
        let selected = self
            .slots
            .get_mut(&slot)
            .filter(|slot| {
                slot.pending.is_none()
                    && slot.expanding.is_none()
                    && slot.keys.iter().any(|(available, _)| *available == key)
            })
            .ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    "display expansion key is unavailable".into(),
                )
            })?;
        let callback = selected.callback.clone().ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "display expansion callback is unavailable".into(),
            )
        })?;
        let generation = selected.next_expansion.checked_add(1).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "display expansion generation exhausted".into(),
            )
        })?;
        selected.next_expansion = generation;
        selected.expanding = Some(generation);
        Ok((callback, generation))
    }
}

struct DisplayPublicationLease {
    displays: Arc<Mutex<ActorDisplays>>,
    slot: i64,
    request: Arc<DisplayPublication>,
}

impl Drop for DisplayPublicationLease {
    fn drop(&mut self) {
        let mut displays = self.displays.lock();
        if !displays
            .slots
            .get(&self.slot)
            .and_then(|slot| slot.pending.as_ref())
            .is_some_and(|pending| Arc::ptr_eq(&pending.request, &self.request))
        {
            return;
        }
        // Cancellation can drop the send or acknowledgment wait before its
        // timeout runs. Retain retryable uncertainty on this exact emission.
        self.request.answer(DisplayPublicationOutcome::Unconfirmed(
            "display execution stopped before host settlement".into(),
        ));
        let _ = displays.reconcile(self.slot);
    }
}

struct DisplayPublicationSubmission {
    displays: Arc<Mutex<ActorDisplays>>,
    request: Arc<DisplayPublication>,
    answer: tokio::sync::oneshot::Receiver<DisplayPublicationOutcome>,
    _lease: DisplayPublicationLease,
}

impl DisplayPublicationSubmission {
    fn new(
        displays: Arc<Mutex<ActorDisplays>>,
        request: Arc<DisplayPublication>,
        answer: tokio::sync::oneshot::Receiver<DisplayPublicationOutcome>,
    ) -> Self {
        let lease = DisplayPublicationLease {
            displays: displays.clone(),
            slot: request.page.identity.2,
            request: request.clone(),
        };
        Self {
            displays,
            request,
            answer,
            _lease: lease,
        }
    }
}

async fn complete_display_publication<H, O>(
    environment: &ResidentEnvironment<H, O>,
    submission: DisplayPublicationSubmission,
) -> Result<WorkbenchDisplayOutput, ResidentActorWorkbenchError> {
    let DisplayPublicationSubmission {
        displays,
        request,
        answer,
        _lease,
    } = submission;
    let admitted = async {
        if environment
            .deployments
            .send(LocalResidentDeployment::DisplayPublished(request.clone()))
            .await
            .is_err()
        {
            request.answer(DisplayPublicationOutcome::Unconfirmed(
                "host output service is unavailable".into(),
            ));
        }
        let _ = answer.await;
    };
    if tokio::time::timeout(RELEASE_WAIT, admitted).await.is_err() {
        request.answer(DisplayPublicationOutcome::Unconfirmed(
            "host output acknowledgement timed out".into(),
        ));
    }
    // The retained answer is authoritative even when its wakeup was lost.
    displays.lock().reconcile(request.page.identity.2)?;
    match request.outcome() {
        Some(DisplayPublicationOutcome::Published(output))
            if valid_display_reference(&request, &output) =>
        {
            Ok(WorkbenchDisplayOutput {
                page: request.page.clone(),
                output,
            })
        }
        Some(DisplayPublicationOutcome::Refused(detail)) => {
            Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "display publication refused: {detail}"
            )))
        }
        Some(DisplayPublicationOutcome::Unconfirmed(detail)) => {
            Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "display publication remains unconfirmed: {detail}"
            )))
        }
        _ => Err(ResidentActorWorkbenchError::ActorProtocol(
            "display publication acknowledgement is unavailable".into(),
        )),
    }
}

/// Dropping a cancelled or failed expansion releases its reservation. Retirement
/// removes the slot first; the guard never recreates an actor-owned resource.
struct DisplayExpansionLease {
    displays: Arc<Mutex<ActorDisplays>>,
    slot: i64,
    generation: u64,
}

impl Drop for DisplayExpansionLease {
    fn drop(&mut self) {
        if let Some(slot) = self.displays.lock().slots.get_mut(&self.slot) {
            if slot.expanding == Some(self.generation) {
                slot.expanding = None;
            }
        }
    }
}

#[cfg(test)]
mod display_tests {
    use std::time::Duration;

    use super::*;

    fn terminal_page() -> WorkbenchDisplayPage {
        WorkbenchDisplayPage {
            identity: (0, 0, 0),
            text: "visible".into(),
            expansions: Vec::new(),
            unavailable: false,
        }
    }

    fn durable_reference() -> tidepool_runtime::session::ActorOutputReference {
        tidepool_runtime::session::ActorOutputReference {
            run: "run-1".into(),
            sequence: 7,
        }
    }

    #[tokio::test]
    async fn display_timeout_fences_stale_host_refusal_until_confirmed_commit() {
        for retry_before_refusal in [false, true] {
            let actor = ActorRef::first(crate::ActorId(37));
            let displays = Arc::new(Mutex::new(ActorDisplays::default()));
            let (request, answer) = displays
                .lock()
                .stage(actor, terminal_page(), None, false, 8192, None)
                .unwrap();
            let slot = request.page.identity.2;
            let entered = Arc::new(std::sync::Barrier::new(2));
            let release = Arc::new(std::sync::Barrier::new(2));
            let host = std::thread::spawn({
                let request = request.clone();
                let entered = entered.clone();
                let release = release.clone();
                move || {
                    // Freeze the adapter's stale observation before the wait expires.
                    assert!(!request.was_unconfirmed());
                    entered.wait();
                    release.wait();
                    request.answer(DisplayPublicationOutcome::Refused(
                        "late host authority refusal".into(),
                    ))
                }
            });
            entered.wait();
            assert!(tokio::time::timeout(Duration::ZERO, answer).await.is_err());
            assert!(request.answer(DisplayPublicationOutcome::Unconfirmed(
                "host output acknowledgement timed out".into(),
            )));
            let retry = retry_before_refusal.then(|| request.retry_channel().unwrap());
            release.wait();
            assert_eq!(host.join().unwrap(), retry_before_refusal);
            if let Some(retry) = retry {
                assert!(matches!(
                    retry.await.unwrap(),
                    DisplayPublicationOutcome::Unconfirmed(_)
                ));
            }
            assert!(request.was_unconfirmed());
            assert!(matches!(
                request.outcome(),
                Some(DisplayPublicationOutcome::Unconfirmed(_))
            ));
            {
                let mut owner = displays.lock();
                assert_eq!(owner.pending_count(), 1);
                assert_eq!(owner.slots[&slot].page_ordinal, 0);
                assert!(Arc::ptr_eq(
                    &owner.slots[&slot].pending.as_ref().unwrap().request,
                    &request,
                ));
            }
            assert!(request.answer(DisplayPublicationOutcome::Published(durable_reference())));
            drop(DisplayPublicationLease {
                displays: displays.clone(),
                slot,
                request: request.clone(),
            });
            assert_eq!(displays.lock().pending_count(), 0);
            assert_eq!(displays.lock().slots[&slot].page_ordinal, 1);
            assert!(!request.answer(DisplayPublicationOutcome::Published(durable_reference())));
            displays.lock().reconcile(slot).unwrap();
            assert_eq!(displays.lock().slots[&slot].page_ordinal, 1);
        }
    }

    #[tokio::test]
    async fn cancelled_display_send_and_ack_wait_retain_same_retry_packet() {
        for sent_before_cancel in [false, true] {
            let actor = ActorRef::first(crate::ActorId(37));
            let displays = Arc::new(Mutex::new(ActorDisplays::default()));
            let (request, answer) = displays
                .lock()
                .stage(actor, terminal_page(), None, false, 8192, None)
                .unwrap();
            let slot = request.page.identity.2;
            let lease = DisplayPublicationLease {
                displays: displays.clone(),
                slot,
                request: request.clone(),
            };
            let (sender, mut receiver) = mpsc::channel(1);
            if sent_before_cancel {
                sender
                    .send(LocalResidentDeployment::DisplayPublished(request.clone()))
                    .await
                    .unwrap();
            }
            drop(lease);
            assert!(matches!(
                answer.await.unwrap(),
                DisplayPublicationOutcome::Unconfirmed(_)
            ));
            assert!(request.was_unconfirmed());
            assert_eq!(displays.lock().pending_count(), 1);
            if sent_before_cancel {
                let LocalResidentDeployment::DisplayPublished(first) =
                    receiver.recv().await.unwrap()
                else {
                    panic!("only the original publication was queued");
                };
                assert!(Arc::ptr_eq(&first, &request));
            } else {
                assert!(receiver.try_recv().is_err());
            }
            let retry = request.retry_channel().unwrap();
            let pending = displays.lock().slots[&slot]
                .pending
                .as_ref()
                .unwrap()
                .request
                .clone();
            assert!(Arc::ptr_eq(&pending, &request));
            sender
                .send(LocalResidentDeployment::DisplayPublished(pending))
                .await
                .unwrap();
            let LocalResidentDeployment::DisplayPublished(retried) = receiver.recv().await.unwrap()
            else {
                panic!("retry must submit the retained publication");
            };
            assert!(Arc::ptr_eq(&retried, &request));
            assert_eq!(retried.page_ordinal, 1);
            assert_eq!(
                displays.lock().next_slot,
                1,
                "retry never restages the callback result"
            );
            assert!(retried.answer(DisplayPublicationOutcome::Published(durable_reference())));
            assert!(matches!(
                retry.await.unwrap(),
                DisplayPublicationOutcome::Published(_)
            ));
            drop(DisplayPublicationLease {
                displays: displays.clone(),
                slot,
                request: retried,
            });
            assert_eq!(displays.lock().pending_count(), 0);
            assert_eq!(displays.lock().slots[&slot].page_ordinal, 1);
            assert!(!request.answer(DisplayPublicationOutcome::Published(durable_reference())));
        }
    }

    #[test]
    fn display_commit_survives_lost_ack_wakeup_and_installs_once() {
        let actor = ActorRef::first(crate::ActorId(37));
        let displays = Arc::new(Mutex::new(ActorDisplays::default()));
        let (request, answer) = displays
            .lock()
            .stage(actor, terminal_page(), None, false, 8192, None)
            .unwrap();
        let slot = request.page.identity.2;
        assert_eq!(request.page_ordinal, 1);
        assert_eq!(displays.lock().slots[&slot].page_ordinal, 0);
        drop(answer);
        let lease = DisplayPublicationLease {
            displays: displays.clone(),
            slot,
            request: request.clone(),
        };
        assert!(request.answer(DisplayPublicationOutcome::Published(durable_reference())));
        drop(lease);
        let mut displays = displays.lock();
        let committed = &displays.slots[&slot];
        assert_eq!(committed.page_ordinal, 1);
        assert!(committed.pending.is_none());
        assert!(committed.callback.is_none());
        assert!(!request.answer(DisplayPublicationOutcome::Published(durable_reference())));
        displays.reconcile(slot).unwrap();
        assert_eq!(displays.slots[&slot].page_ordinal, 1);
        assert!(displays.select(actor, request.page.identity, 1).is_err());
    }

    #[test]
    fn uncertain_display_retry_keeps_exact_emission_and_frozen_host_context() {
        let actor = ActorRef::first(crate::ActorId(37));
        let displays = Arc::new(Mutex::new(ActorDisplays::default()));
        let (request, answer) = displays
            .lock()
            .stage(actor, terminal_page(), None, false, 8192, None)
            .unwrap();
        let slot = request.page.identity.2;
        let frozen = DisplayPublicationHostContext {
            run: "run-1".into(),
            conversation: None,
        };
        assert_eq!(request.admit_host_context(frozen.clone()), &frozen);
        assert_eq!(
            request.admit_host_context(DisplayPublicationHostContext {
                run: "changed".into(),
                conversation: Some(DisplayConversationIdentity {
                    run: "changed".into(),
                    actor: "new conversation".into(),
                    incarnation: "1".into(),
                }),
            }),
            &frozen
        );
        assert!(request.answer(DisplayPublicationOutcome::Unconfirmed(
            "lost commit acknowledgement".into()
        )));
        drop(answer);
        drop(DisplayPublicationLease {
            displays: displays.clone(),
            slot,
            request: request.clone(),
        });
        {
            let mut owner = displays.lock();
            assert_eq!(owner.slots[&slot].page_ordinal, 0);
            assert!(Arc::ptr_eq(
                &owner.slots[&slot].pending.as_ref().unwrap().request,
                &request
            ));
            assert!(owner.select(actor, request.page.identity, 1).is_err());
            assert_eq!(owner.pending_count(), 1);
        }
        let retry = request.retry_channel().unwrap();
        assert!(request.was_unconfirmed());
        drop(retry);
        assert!(request.answer(DisplayPublicationOutcome::Published(durable_reference())));
        drop(DisplayPublicationLease {
            displays: displays.clone(),
            slot,
            request: request.clone(),
        });
        assert_eq!(displays.lock().slots[&slot].page_ordinal, 1);
        assert_eq!(displays.lock().pending_count(), 0);
        assert_eq!(request.page_ordinal, 1);
        assert_eq!(request.host_context(), Some(&frozen));
        assert!(request.was_unconfirmed());
    }

    #[test]
    fn display_refusal_preserves_issued_tombstone_without_committing_page() {
        let actor = ActorRef::first(crate::ActorId(37));
        let displays = Arc::new(Mutex::new(ActorDisplays::default()));
        let (request, answer) = displays
            .lock()
            .stage(actor, terminal_page(), None, false, 8192, None)
            .unwrap();
        drop(answer);
        let slot = request.page.identity.2;
        request.answer(DisplayPublicationOutcome::Refused(
            "authority refused".into(),
        ));
        drop(DisplayPublicationLease {
            displays: displays.clone(),
            slot,
            request: request.clone(),
        });
        let mut owner = displays.lock();
        assert!(owner.slots[&slot].pending.is_none());
        assert_eq!(owner.slots[&slot].page_ordinal, 0);
        let (next, _) = owner
            .stage(actor, terminal_page(), None, false, 8192, None)
            .unwrap();
        assert_ne!(next.page.identity, request.page.identity);
    }

    #[test]
    fn accepted_display_can_settle_after_retirement_without_new_authority() {
        let actor = ActorRef::first(crate::ActorId(37));
        let displays = Arc::new(Mutex::new(ActorDisplays::default()));
        let (request, answer) = displays
            .lock()
            .stage(actor, terminal_page(), None, false, 8192, None)
            .unwrap();
        let slot = request.page.identity.2;
        drop(answer);
        displays.lock().retire();
        assert!(Arc::ptr_eq(
            &displays.lock().slots[&slot]
                .pending
                .as_ref()
                .unwrap()
                .request,
            &request
        ));
        request.answer(DisplayPublicationOutcome::Published(durable_reference()));
        drop(DisplayPublicationLease {
            displays: displays.clone(),
            slot,
            request: request.clone(),
        });
        let mut owner = displays.lock();
        assert_eq!(owner.slots[&slot].page_ordinal, 1);
        assert!(owner.slots[&slot].pending.is_none());
        assert!(owner.slots[&slot].callback.is_none());
        assert!(owner
            .stage(actor, terminal_page(), None, false, 8192, None)
            .is_err());
        assert!(owner.select(actor, request.page.identity, 1).is_err());
    }

    #[test]
    fn display_allowance_preserves_rendered_utf8_page_in_transport_budget() {
        for remaining in [0, 1, 3, 4, 127, 8192, 32768, 65536, usize::MAX] {
            let allowance = display_character_allowance(remaining) as usize;
            assert!(allowance <= 8192);
            for scalar in ["a", "λ", "界", "🙂"] {
                let page = scalar.repeat(allowance);
                assert!(page.len() <= remaining);
                if !page.is_empty() {
                    assert!(page.len() + 1 <= remaining);
                }
                assert_eq!(
                    crate::workbench_display::bounded_output(&page, remaining),
                    page
                );
            }
        }
    }

    #[test]
    fn display_page_rejects_stale_or_ignored_character_grants() {
        let current = display_character_allowance(9);
        assert_eq!(current, 2);
        assert!(validate_display_page("🙂🙂", current).is_ok());
        assert!(validate_display_page("abc", current).is_err());
        assert!(validate_display_page("🙂🙂🙂", current).is_err());
        assert!(validate_display_page("", 0).is_ok());
        assert!(validate_display_page("a", 0).is_err());
        assert!(validate_display_page(&"a".repeat(8193), i64::MAX).is_err());
    }

    #[test]
    fn display_metadata_budget_counts_encoded_labels_and_envelope() {
        let mut output = WorkbenchDisplayPage {
            identity: (i64::MAX, i64::MAX, i64::MAX),
            text: String::new(),
            expansions: vec![(i64::MAX, String::new())],
            unavailable: false,
        };
        let envelope = serde_json::to_vec(&serde_json::json!({
            "identity": output.identity,
            "expansions": output.expansions,
            "unavailable": output.unavailable,
        }))
        .unwrap()
        .len();
        output.expansions[0].1 = "a".repeat(DISPLAY_METADATA_BYTE_LIMIT - envelope);
        assert!(validate_display_metadata(&output).is_ok());
        output.expansions[0].1.push('a');
        assert!(validate_display_metadata(&output).is_err());

        // JSON control escapes expand raw bytes; raw label length is not a cap.
        for label in ["\0".repeat(1400), "\n\t\"\\\r".repeat(900)] {
            assert!(label.len() < DISPLAY_METADATA_BYTE_LIMIT);
            output.expansions[0].1 = label;
            assert!(validate_display_metadata(&output).is_err());
        }
        output.expansions[0].1 = "🙂".repeat(1900);
        assert!(validate_display_metadata(&output).is_ok());
        output.expansions = (1..=300).map(|key| (key, String::new())).collect();
        assert!(validate_display_metadata(&output).is_ok());
    }

    #[test]
    fn display_expansion_validates_exact_incarnation_and_issued_slot() {
        let actor = ActorRef::first(crate::ActorId(37));
        let owner = actor_address(actor);
        let mut displays = ActorDisplays::default();
        assert_eq!(
            ActorDisplays::validate_identity(actor, (owner.0, owner.1, 1)).unwrap(),
            1
        );
        for identity in [
            (owner.0 + 1, owner.1, 1),
            (owner.0, owner.1 + 1, 1),
            (owner.0, owner.1, 0),
            (owner.0, owner.1, -1),
        ] {
            assert!(ActorDisplays::validate_identity(actor, identity).is_err());
        }
        assert!(
            displays.select(actor, (owner.0, owner.1, 1), 1).is_err(),
            "a guessed slot must not authorize expansion"
        );
    }
}

fn stage_actor_display<H, O>(
    environment: &ResidentEnvironment<H, O>,
    context: &ActorSessionContext,
    page: WorkbenchDisplayPage,
    callback: RootCustody,
    update: bool,
    allowance: i64,
    operation: Option<WorkbenchOperationId>,
) -> Result<DisplayPublicationSubmission, ResidentActorWorkbenchError> {
    let records = environment.actors.lock();
    let record = records
        .get(&context.actor)
        .filter(|record| record.owns_display_resources(context))
        .ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol("display actor is unavailable".into())
        })?;
    let displays = record.displays.clone();
    let (request, answer) = displays.lock().stage(
        context.actor,
        page,
        Some(callback),
        update,
        allowance,
        operation,
    )?;
    Ok(DisplayPublicationSubmission::new(displays, request, answer))
}

async fn publish_actor_display<H, O>(
    environment: &ResidentEnvironment<H, O>,
    context: &ActorSessionContext,
    page: WorkbenchDisplayPage,
    callback: RootCustody,
    update: bool,
    allowance: i64,
    operation: Option<WorkbenchOperationId>,
    mut receipt: Option<DisplayReceiptSubmission<'_>>,
) -> Result<WorkbenchDisplayOutput, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let stage = || {
        stage_actor_display(
            environment,
            context,
            page,
            callback,
            update,
            allowance,
            operation,
        )
    };
    let submission = match &mut receipt {
        Some(receipt) => receipt.stage_and_prepare(stage)?,
        None => stage()?,
    };
    complete_display_publication(&environment, submission).await
}

async fn expand_actor_display<H, O>(
    environment: &ResidentEnvironment<H, O>,
    context: &ActorSessionContext,
    identity: (i64, i64, i64),
    key: i64,
    allowance: i64,
    operation: Option<WorkbenchOperationId>,
    mut receipt: Option<DisplayReceiptSubmission<'_>>,
) -> Result<WorkbenchDisplayOutput, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let displays = {
        let records = environment.actors.lock();
        records
            .get(&context.actor)
            .filter(|record| record.owns_display_resources(context))
            .ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol("display actor is unavailable".into())
            })?
            .displays
            .clone()
    };
    let pending = {
        let mut displays = displays.lock();
        let slot = ActorDisplays::validate_identity(context.actor, identity)?;
        let pending = displays
            .slots
            .get(&slot)
            .and_then(|slot| slot.pending.as_ref())
            .map(|pending| pending.request.clone());
        if pending.as_ref().is_some_and(|pending| {
            matches!(
                pending.outcome(),
                Some(DisplayPublicationOutcome::Refused(_))
            )
        }) {
            let _ = displays.reconcile(slot);
            None
        } else {
            pending
        }
    };
    if let Some(request) = pending {
        // Resolve the same pending emission before interpreting another key.
        // Repeating a legal key must never remint its earlier publication.
        validate_display_page(&request.page.text, allowance)?;
        if let Some(answer) = request.retry_channel() {
            let submission = DisplayPublicationSubmission::new(displays, request, answer);
            if let Some(receipt) = &mut receipt {
                receipt.prepare(&submission.request)?;
            }
            let output = complete_display_publication(&environment, submission).await?;
            validate_display_page(&output.text, allowance)?;
            return Ok(output);
        }
        if let Some(DisplayPublicationOutcome::Published(output)) = request.outcome() {
            displays.lock().reconcile(identity.2)?;
            validate_display_page(&request.page.text, allowance)?;
            if let Some(receipt) = &mut receipt {
                receipt.prepare(&request)?;
            }
            return Ok(WorkbenchDisplayOutput {
                page: request.page.clone(),
                output,
            });
        }
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "display publication is still pending".into(),
        ));
    }
    let (callback, generation) = displays.lock().select(context.actor, identity, key)?;
    let lease = DisplayExpansionLease {
        displays,
        slot: identity.2,
        generation,
    };
    let (page, callback) = environment
        .runner
        .expand_display(context.clone(), callback, identity, key, allowance)
        .await?;
    let output = publish_actor_display(
        environment,
        context,
        page,
        callback,
        true,
        allowance,
        operation,
        receipt,
    )
    .await;
    drop(lease);
    output
}

async fn prepare_actor_display_boundary<H, O>(
    environment: &ResidentEnvironment<H, O>,
    context: &ActorSessionContext,
    boundary: ResidentActorBoundary,
    allowance: i64,
    operation: Option<WorkbenchOperationId>,
    receipt: Option<DisplayReceiptSubmission<'_>>,
) -> Result<(ResidentActorBoundary, Option<WorkbenchDisplayOutput>), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    match boundary {
        ResidentActorBoundary::DisplayAllowance { continuation } => Ok((
            ResidentActorBoundary::DisplayAllowanceGranted {
                continuation,
                allowance,
            },
            None,
        )),
        ResidentActorBoundary::DisplayPublish {
            continuation,
            output,
            callback,
        } => {
            let output = publish_actor_display(
                environment,
                context,
                output,
                callback,
                false,
                allowance,
                operation,
                receipt,
            )
            .await?;
            Ok((
                ResidentActorBoundary::DisplayPublished {
                    continuation,
                    identity: output.identity,
                },
                Some(output),
            ))
        }
        ResidentActorBoundary::DisplayExpand {
            continuation,
            identity,
            key,
        } => {
            let output = expand_actor_display(
                environment,
                context,
                identity,
                key,
                allowance,
                operation,
                receipt,
            )
            .await?;
            Ok((
                ResidentActorBoundary::DisplayExpanded {
                    continuation,
                    keys: output.expansions.clone(),
                },
                Some(output),
            ))
        }
        boundary => Ok((boundary, None)),
    }
}

fn settle_public_owner_record(
    record: &mut ResidentActorRecord,
    context: &ActorSessionContext,
    expected_owner: &tidepool_runtime::session::RecoveryPublicOwner,
    outcome: &tidepool_runtime::session::PublicManifestCommit,
    readiness: Option<Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>>,
) -> Result<(), ResidentActorWorkbenchError> {
    if record.terminal.is_some()
        || record.descriptor.placement() != context.placement
        || !(matches!(&record.public_owner,
                ActorPublicOwnerPlane::DurablePending(owner)
                | ActorPublicOwnerPlane::DurablePublishedUnconfirmed { owner, .. }
                if owner == expected_owner)
            || matches!(&record.public_owner, ActorPublicOwnerPlane::DurableReady(owner)
                if owner.durable() == Some(expected_owner)))
    {
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "public settlement requires its original pending owner".into(),
        ));
    }
    match outcome {
        tidepool_runtime::session::PublicManifestCommit::Durable => {
            if let ActorPublicOwnerPlane::DurableReady(owner) = &record.public_owner {
                return if owner.is_current() {
                    Ok(())
                } else {
                    Err(ResidentActorWorkbenchError::ActorProtocol(
                        "original native owner was revoked before confirmation settlement".into(),
                    ))
                };
            }
            record.public_owner = ActorPublicOwnerPlane::DurableReady(
                WorkbenchPublicOwner::issue(context, &record.descriptor, readiness).map_err(
                    |error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()),
                )?,
            );
        }
        tidepool_runtime::session::PublicManifestCommit::PublishedDurabilityUnconfirmed {
            detail,
        } => {
            if matches!(record.public_owner, ActorPublicOwnerPlane::DurableReady(_)) {
                return Ok(());
            }
            record.public_owner = ActorPublicOwnerPlane::DurablePublishedUnconfirmed {
                owner: expected_owner.clone(),
                detail: detail.clone(),
            };
        }
        _ => {}
    }
    Ok(())
}

#[derive(Clone)]
enum RootPublicOwnerPosture {
    Pending,
    PublishedUnconfirmed,
    Ready,
}

#[derive(Clone)]
enum ActorPublicOwnerPlane {
    Ephemeral(Arc<WorkbenchPublicOwner>),
    DurablePending(tidepool_runtime::session::RecoveryPublicOwner),
    DurablePublishedUnconfirmed {
        owner: tidepool_runtime::session::RecoveryPublicOwner,
        detail: String,
    },
    DurableReady(Arc<WorkbenchPublicOwner>),
}

impl ActorPublicOwnerPlane {
    fn ready(&self) -> Option<&Arc<WorkbenchPublicOwner>> {
        match self {
            Self::Ephemeral(owner) | Self::DurableReady(owner) if owner.is_ready() => Some(owner),
            Self::Ephemeral(_) | Self::DurableReady(_) => None,
            Self::DurablePending(_) | Self::DurablePublishedUnconfirmed { .. } => None,
        }
    }
}

/// Actual actor-owner readiness for the provider composition boundary. The
/// frontend retains this capsule across waits and revalidates it before bind.
pub struct ActorProviderAdmission {
    owner: Arc<WorkbenchPublicOwner>,
}

/// Forest-issued custody for one already-admitted output. Completion may cross
/// actor retirement; the capsule grants no new display or expansion authority.
pub struct ActorDisplayAdmission {
    actor: ActorRef,
    placement: crate::ActorPlacement,
    displays: Arc<Mutex<ActorDisplays>>,
    request: Arc<DisplayPublication>,
}

impl ActorDisplayAdmission {
    pub fn actor(&self) -> ActorRef {
        self.actor
    }
    pub fn placement(&self) -> crate::ActorPlacement {
        self.placement
    }
}

impl ActorProviderAdmission {
    pub fn actor(&self) -> ActorRef {
        self.owner.actor()
    }
    pub fn placement(&self) -> crate::ActorPlacement {
        self.owner.placement()
    }
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
        ReplyError::UpdatePending => {
            "a request this watch names has request input publication in flight"
        }
        ReplyError::ProgressTypeMismatch => {
            "a request this watch names has an incompatible progress type"
        }
        ReplyError::InvalidReadiness => "this watch's readiness expression is invalid",
        ReplyError::ReplyResultTypeMismatch => "a request has an incompatible result type",
        ReplyError::ReplyResultUnavailable => "a request has no admitted result destination",
    };
    format!("watch registration was rejected: {detail}")
}

/// What an actor sees when its own reply or cancellation acknowledgement for
/// `request` cannot settle.
///
/// Input publication in flight is refused, not held. The requester's update reaches
/// the target's model only as provider input between model rounds, so a hold
/// inside the cell would hold the very tool call that has to end before the
/// update can be delivered. Settling once presentation is confirmed would
/// instead publish an answer written before the model read the update, which
/// is what the fence exists to prevent. The refusal therefore tells the model
/// where the update will appear and that retrying earlier fails the same way.
///
/// The workspace template's `Project.Watchdog.hostSettlementRefusal` reads the
/// `<settlement> not settled: ` prefix to tell request state from a failed
/// approach; keep the two in step.
fn settlement_refusal(
    settlement: &str,
    request: crate::RequestId,
    error: crate::request::ReplyError,
) -> String {
    use crate::request::ReplyError;
    let request = request.0;
    let detail = match error {
        ReplyError::UpdatePending => format!(
            "request {request} has input publication in flight or awaiting confirmation. \
             End your turn so queued input can be shown, then retry once publication is confirmed"
        ),
        ReplyError::CancellationRequested => {
            format!("request {request} is being cancelled; acknowledge the cancellation instead")
        }
        ReplyError::ProgressTypeMismatch => {
            format!("request {request} expects a different progress type")
        }
        ReplyError::InvalidReadiness => {
            format!("request {request} has an invalid readiness expression")
        }
        ReplyError::ReplyResultTypeMismatch => {
            format!("request {request} expects a different result type")
        }
        ReplyError::ReplyResultUnavailable => {
            format!("request {request} has no admitted result destination")
        }
        ReplyError::AlreadySettled => format!("request {request} is already settled"),
        ReplyError::Stale => format!("request {request} is not active for this actor"),
        ReplyError::Unauthorized | ReplyError::WrongIncarnation => {
            format!("request {request} was not assigned to this actor")
        }
    };
    format!("{settlement} not settled: {detail}. Nothing was sent.")
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
    if !overview.running_jobs.is_empty() {
        lines.push(format!(
            "running jobs ({} total):",
            overview.running_jobs.len()
        ));
    }
    for job in &overview.running_jobs {
        lines.push(format!(
            "  - job {}: started {}",
            job.label,
            elapsed_into_session(launched_at_unix_ms, job.registered_at_unix_ms),
        ));
    }
    lines.push(
        "Reading a settled VALUE still needs pollWatch (watches) or pollResponse \
         (plain requests); this view only reports status, not values."
            .to_owned(),
    );
    lines.join("\n")
}

async fn install_explicit_replacement<H, O>(
    environment: ResidentEnvironment<H, O>,
    context: ActorSessionContext,
    state: Arc<crate::resident_workbench::InstalledToolsState>,
    allowed: Vec<crate::ActorEffectKey>,
    definition: crate::SpecReplacementDefinition,
) -> Result<(), crate::SpecReplacementError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let expected = state
        .current()
        .filter(|lease| lease.tools().is_some())
        .ok_or(crate::SpecReplacementError::Unavailable)?;
    if definition
        .effects
        .iter()
        .any(|effect| !allowed.contains(effect))
    {
        return Err(crate::SpecReplacementError::Unauthorized);
    }
    let workbench = environment
        .runner
        .application_workbench()
        .with_intrinsic_effect_support(environment.intrinsic_effect_support());
    let installer = Arc::new(
        environment
            .runner
            .transfer_custody(
                definition.installer,
                definition.session,
                context.placement.session,
                context.placement.resource_scope,
            )
            .await
            .map_err(|error| crate::SpecReplacementError::Failed(error.to_string()))?,
    );
    let install = expected
        .tools()
        .expect("admitted installation")
        .install
        .checked_add(1)
        .ok_or_else(|| {
            crate::SpecReplacementError::Failed("installation identity exhausted".into())
        })?;
    let tools = workbench
        .prepare_explicit_tools(context, install, definition.effects, installer)
        .await
        .map_err(|error| crate::SpecReplacementError::Failed(error.to_string()))?;
    state.replace_explicit(&expected, Arc::new(tools))
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
            actor_admissions: self.actor_admissions.clone(),
            actors: Arc::clone(&self.actors),
            fork_workspaces: self.fork_workspaces.clone(),
            root_admission_closed: self.root_admission_closed.clone(),
            source_layers: self.source_layers.clone(),
            jev: Arc::clone(&self.jev),
            cell_model_factory: self.cell_model_factory.clone(),
            form_host: self.form_host.clone(),
            form_registry: self.form_registry.clone(),
            release_tracked: Arc::clone(&self.release_tracked),
            conversation_reader: self.conversation_reader.clone(),
            usage_pointers: self.usage_pointers.clone(),
            recovery: self.recovery.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootStartupState {
    Pending,
    Released,
    Activated,
}

/// Original, process-local release authority for a registered pending root.
/// Keeping this receipt permits acknowledgment retries without rebuilding boot.
pub struct RootStartupRelease {
    actor: ActorRef,
    placement: crate::ActorPlacement,
    latch: Arc<Mutex<RootStartupState>>,
    intent: crate::RootStartupIntent,
}

impl RootStartupRelease {
    pub fn actor(&self) -> ActorRef {
        self.actor
    }
}

impl std::fmt::Debug for RootStartupRelease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RootStartupRelease")
            .field("actor", &self.actor)
            .field("placement", &self.placement)
            .finish_non_exhaustive()
    }
}

enum ResidentBoot {
    Workbench,
    Prepared(Box<ResidentOutcome>),
    Startup(tidepool_runtime::session::PreparedStartupEntry),
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
    type_evidence: Arc<tidepool_runtime::session::SiteTypeEvidence>,
    input_binding: tidepool_repr::SessionVarId,
    input_scope: tidepool_codegen::scope::ScopeId,
}

impl OutstandingInteractive {
    fn new(
        request: &crate::interactive_session::InteractiveSessionRequest,
        input_binding: tidepool_repr::SessionVarId,
        input_scope: tidepool_codegen::scope::ScopeId,
    ) -> Self {
        Self {
            response: request.response.clone(),
            request: request.request,
            type_evidence: request.type_evidence.clone(),
            input_binding,
            input_scope,
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
    point: WorkbenchFailurePoint,
    publication: Option<WorkbenchPublicationOutcome>,
    total: usize,
    source: ResidentActorWorkbenchError,
}

struct SuspendedCast {
    site: u64,
    receiver_continuation: ResidentHole,
    handler_realm: RealmId,
    cleanup: Option<crate::resident_workbench::ParkedHoleAbortGuard>,
}

#[derive(Debug, Clone, Copy)]
enum AcceptedTerminalKind {
    Reply,
    Cancellation,
}

#[derive(Debug)]
struct AcceptedTerminalTransfer {
    request: crate::RequestId,
    kind: AcceptedTerminalKind,
    owner: RequestReservationOwner,
}

struct PendingActorProgram {
    transfer: Option<Arc<AcceptedTerminalTransfer>>,
    outcome: ResidentOutcome,
    cleanup: Option<crate::resident_workbench::ParkedHoleAbortGuard>,
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
        point: WorkbenchFailurePoint::InputUnit {
            index: failed_index,
        },
        publication: None,
        total,
        source,
    }
}

fn failed_checkpoint_cleanup_response(
    response: WorkbenchResponse,
    cleanup: String,
) -> WorkbenchExecutionFailure {
    let status = response.status;
    let next_index = response.next_index;
    WorkbenchExecutionFailure {
        receipts: response.items,
        point: WorkbenchFailurePoint::Finalization {
            completed_input_units: next_index,
        },
        publication: response.publication,
        total: response.total,
        source: ResidentActorWorkbenchError::ActorProtocol(format!(
            "{cleanup}; original workbench status {status:?}, next index {next_index}"
        )),
    }
}

fn checkpoint_capture_delivered<T>(resumed: &Result<T, ResidentActorWorkbenchError>) -> bool {
    resumed.is_ok() || matches!(resumed, Err(ResidentActorWorkbenchError::Delivered(_)))
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
    let recovered_bindings = source.recovered_bindings().to_vec();
    let mut receipts = completed.to_vec();
    if !operations.is_empty() || failure_layer.is_some() || !recovered_bindings.is_empty() {
        receipts.push(WorkbenchItemReceipt {
            diagnostics: Vec::new(),
            index: failed_index,
            kind: None,
            span: None,
            source_items: Vec::new(),
            status: if failure_layer == Some(WorkbenchFailureLayer::Observation) {
                WorkbenchItemStatus::Diagnostic
            } else {
                WorkbenchItemStatus::Rejected
            },
            // The failure's own diagnostic lives on `source`/`detail`
            // instead of here; this field otherwise stays empty. The one
            // exception is a short, human-facing framing sentence for the
            // failure layer — the one piece of the failure this item alone
            // can say plainly, since a reader sees this receipt without
            // necessarily reading `detail`.
            output: failure_layer_output_hint(failure_layer),
            value: None,
            warnings: Vec::new(),
            installed_bindings: recovered_bindings,
            operations,
            terminal_transfer: None,
            failure_layer,
        });
    }
    WorkbenchExecutionFailure {
        receipts,
        point: WorkbenchFailurePoint::InputUnit {
            index: failed_index,
        },
        publication: None,
        total,
        source,
    }
}

fn workbench_failure_after_unit(
    completed: &[WorkbenchItemReceipt],
    failed_index: usize,
    total: usize,
    source: ResidentActorWorkbenchError,
    operations: Vec<WorkbenchOperationReceipt>,
    unit_bindings: &[String],
) -> WorkbenchExecutionFailure {
    let mut failure =
        workbench_failure_after_operations(completed, failed_index, total, source, operations);
    let has_bindings = !unit_bindings.is_empty()
        || failure
            .receipts
            .iter()
            .any(|receipt| receipt.index == failed_index && !receipt.installed_bindings.is_empty());
    if !has_bindings {
        return failure;
    }
    let layer = resident_actor_failure_layer(&failure.source);
    let receipt_index = match failure
        .receipts
        .iter()
        .rposition(|receipt| receipt.index == failed_index)
    {
        Some(index) => index,
        None => {
            failure.receipts.push(WorkbenchItemReceipt {
                diagnostics: Vec::new(),
                index: failed_index,
                kind: None,
                span: None,
                source_items: Vec::new(),
                status: WorkbenchItemStatus::Stopped,
                output: failure_layer_output_hint(layer),
                value: None,
                warnings: Vec::new(),
                installed_bindings: Vec::new(),
                operations: Vec::new(),
                terminal_transfer: None,
                failure_layer: layer,
            });
            failure.receipts.len() - 1
        }
    };
    let receipt = &mut failure.receipts[receipt_index];
    let mut names = receipt.installed_bindings.clone();
    names.extend_from_slice(unit_bindings);
    merge_retained_bindings(receipt, &names);
    if layer == Some(WorkbenchFailureLayer::Effect) {
        receipt.status = WorkbenchItemStatus::Stopped;
    }
    failure
}

fn merge_retained_bindings(receipt: &mut WorkbenchItemReceipt, bindings: &[String]) {
    for binding in bindings {
        if !receipt.installed_bindings.contains(binding) {
            receipt.installed_bindings.push(binding.clone());
        }
    }
    if bindings.is_empty()
        || receipt
            .output
            .contains("private bindings (discarded unless this cell publishes):")
    {
        return;
    }
    let retained = format!(
        "private bindings (discarded unless this cell publishes): {}",
        receipt.installed_bindings.join(", ")
    );
    if receipt.output.is_empty() {
        receipt.output = retained;
    } else {
        receipt.output.push('\n');
        receipt.output.push_str(&retained);
    }
}

/// A short, human-facing framing sentence for a failure layer — the tool
/// text this item's own (otherwise empty) `output` can say plainly, matching
/// what the layer means on [`WorkbenchFailureLayer`]. `Compile`/`None` add
/// nothing: a compile rejection already carries its own text, and an
/// unclassified failure has nothing this function can say honestly.
fn failure_layer_output_hint(layer: Option<WorkbenchFailureLayer>) -> String {
    match layer {
        Some(WorkbenchFailureLayer::Install) => {
            "the program failed before running; earlier effects may have committed".to_owned()
        }
        Some(WorkbenchFailureLayer::Observation) => {
            "effects committed; observing the result failed".to_owned()
        }
        Some(WorkbenchFailureLayer::Effect) => {
            "an effect failed, or did not finish committing before the unit ended".to_owned()
        }
        Some(WorkbenchFailureLayer::Compile) | None => String::new(),
    }
}

/// A reload receipt leads with where it ended and how long it took. The
/// lines beneath retain the selected source and publication details.
fn reload_receipt(outcome: &str, started: std::time::Instant, lines: Vec<String>) -> String {
    format!(
        "{outcome} ({:.1}s)\n{}",
        started.elapsed().as_secs_f64(),
        lines.join("\n")
    )
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
fn resident_actor_failure_layer(
    error: &ResidentActorWorkbenchError,
) -> Option<WorkbenchFailureLayer> {
    match error {
        ResidentActorWorkbenchError::Compile(_)
        | ResidentActorWorkbenchError::CellCheck(_)
        | ResidentActorWorkbenchError::InputCompilation { .. }
        | ResidentActorWorkbenchError::CompileInfrastructure(_) => {
            Some(WorkbenchFailureLayer::Compile)
        }
        ResidentActorWorkbenchError::Resident(inner) => inner.failure_layer(),
        ResidentActorWorkbenchError::CompletedResultObservation { .. } => {
            Some(WorkbenchFailureLayer::Observation)
        }
        ResidentActorWorkbenchError::Delivered(inner) => Some(
            inner
                .failure_layer()
                .unwrap_or(WorkbenchFailureLayer::Effect),
        ),
        ResidentActorWorkbenchError::InvocationCancelled => None,
        _ => None,
    }
}

fn record_workbench_operation(
    operations: &mut Vec<WorkbenchOperationReceipt>,
    execution: Option<&WorkbenchExecutionId>,
    input_unit_index: usize,
    effect_ordinal: usize,
    effect: &str,
    display: Option<WorkbenchDisplayOutput>,
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
        display_publication: None,
        display,
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

fn scoped_operation_disposition(
    success: WorkbenchOperationDisposition,
    actual: WorkbenchOperationDisposition,
) -> WorkbenchOperationDisposition {
    if actual == WorkbenchOperationDisposition::Committed {
        success
    } else {
        actual
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
    effect_ordinal: &'a mut usize,
    display_remaining: &'a mut usize,
    command_output: &'a mut Vec<String>,
    recovered_bindings: &'a mut Vec<String>,
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
struct PreparedInteractivePublication {
    owner: Arc<WorkbenchPublicOwner>,
    bootstrap: Option<tidepool_runtime::session::DurablePublicBootstrap>,
    installation: LocalResidentInstallation,
}

pub struct ResidentKernelBehavior<H, O> {
    replacement_transfer: Option<replacement::ReplacementTransfer>,
    retained_replacements: Vec<replacement::RetainedHandler>,
    descriptor: ActorDescriptor,
    environment: ResidentEnvironment<H, O>,
    boot: Option<ResidentBoot>,
    explicit_installer: Option<Arc<RootCustody>>,
    prepared_request_receiver: Option<crate::resident_workbench::PreparedRequestReceiver>,
    request_receiver_scope: Option<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
    fresh_context_seed: Option<String>,
    spawn_source: Option<crate::CheckpointSourceLayer>,
    spawn_helper_branch: Option<String>,
    spawn_admission: Option<crate::SpawnAdmission>,
    root_startup: Option<(crate::RootStartupIntent, Arc<Mutex<RootStartupState>>)>,
    standing: ResidentStanding,
    shutdown_hook: Option<RootCustody>,
    checkpoint: Option<StateCheckpoint>,
    admitted_checkpoint: Option<(crate::CheckpointLease, Option<HostedCheckpointAttachment>)>,
    child_session_startup: Option<crate::resident_workbench::ChildSessionStartupLease>,
    child_placement_custody: Option<child_launch::ChildPlacementCustody>,
    pending_checkpoint: Option<StateCheckpoint>,
    active_input: Option<RetainedActorInput>,
    input_origin: ActorInputOrigin,
    sources: Vec<crate::request::sources::SourceBinding>,
    static_source_count: usize,
    source_connections: Option<crate::request::sources::ActorSourceConnections>,
    launch_worktrees: Vec<String>,
    prepared_workspace: Option<crate::PreparedWorkspaceAttachment>,
    worktree_custody: Option<Arc<dyn crate::WorkspaceCustody>>,
    policy_installed: bool,
    installed_tools: Arc<crate::resident_workbench::InstalledToolsState>,
    /// How many specs this incarnation has installed. `policy_installed` stays
    /// the first-install latch; a reload is a second, explicit path that
    /// replaces the current installed record and advances this.
    spec_installs: u64,
    /// Every after-tool invocation this actor has made, and what became of it.
    /// An abstention's reason lives here and nowhere else.
    after_tool: crate::after_tool::AfterToolLog,
    forest_control: bool,
    pending_program: Option<PendingActorProgram>,
    pending_reply: Option<crate::request::RequestReplyClaim>,
    pending_response: Option<Arc<crate::owned_result::OwnedResultSnapshot>>,
    exit_destination: Option<Arc<crate::owned_result::RequestResultDestination>>,
    pending_exit: Option<Arc<crate::owned_result::OwnedResultSnapshot>>,
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
    checkpoint_publication: CheckpointPublication,
    active_route_reservation_owner: Option<RequestReservationOwner>,
    settled_checkpoint_boundaries: Vec<tidepool_runtime::session::ContextCheckpointBoundary>,
    /// This actor's last summary-family status roster, for the `changed`
    /// view. One per actor, replaced on each such call.
    roster_snapshot: Mutex<Option<status_rendering::RosterSnapshot>>,
    /// `taskSource` of the current request's session input, read from its
    /// rendered preview when the request was presented.
    assignment_base: Option<String>,
    #[cfg(test)]
    activation_preview_observer: Option<crate::resident_workbench::ActivationPublicationObserver>,
}

/// One admitted call owns its authority and cursor until final settlement.
/// The actor prepares each fenced step; private execution state is never cloned.
struct WorkbenchExecutionState {
    cell_span: tracing::Span,
    effects: WorkbenchEffectState,
    request: WorkbenchRequest,
    replay_request: Option<WorkbenchRequest>,
    invocation: Option<crate::resident_tools::WorkbenchCallKey>,
    cursor: WorkbenchCursor,
}

/// Admission owns the cell span; preparation and every resumed step retain it.
fn cell_execution_span(context: &ActorSessionContext, request: &WorkbenchRequest) -> tracing::Span {
    tracing::info_span!(
        "cell",
        actor = %context.actor,
        execution = request.execution_id().map(|id| id.as_str()),
        tool = request.tool_call().map(|call| call.name.as_str()),
        items = request.items.len(),
    )
}

fn install_cell_preparation(
    request: &mut WorkbenchRequest,
    cursor: &mut WorkbenchCursor,
    prepared_result: Result<
        (tidepool_runtime::session::CellCheck, PreparedCell),
        ResidentActorWorkbenchError,
    >,
) -> Result<Option<KernelStep<WorkbenchResponse>>, WorkbenchExecutionFailure> {
    let cell_source = request
        .cell_source()
        .expect("authored cell preparation")
        .to_owned();
    let (checked, prepared) = match prepared_result {
        Ok(checked) => checked,
        Err(ResidentActorWorkbenchError::CellCheck(failure)) => {
            return Ok(Some(KernelStep::Continue(cell_check_rejection(
                failure,
                &cell_source,
            ))));
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
    let PreparedCell {
        items,
        dependencies,
    } = prepared;
    cursor.dependencies = Some(dependencies.into());
    cursor.prepared_cell = Some(items.into_iter().map(Some).collect::<Vec<_>>());
    cursor.cell_check = Some(checked);
    cursor.preparation_done = true;
    Ok(None)
}

struct WorkbenchFinalization {
    context: ActorSessionContext,
    reservation_owner: RequestReservationOwner,
    invocation_work: Arc<InvocationWork>,
    kernel: KernelContext,
    result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    rejected: bool,
    retire_scopes: Option<Vec<tidepool_codegen::scope::ScopeId>>,
    context_boundary: Option<tidepool_runtime::session::ContextCheckpointBoundary>,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
}

struct WorkbenchFinalizationResult {
    result: Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>,
    cleanup_confirmed: bool,
}

async fn settle_workbench_finalization<H, O>(
    environment: ResidentEnvironment<H, O>,
    finalization: WorkbenchFinalization,
) -> WorkbenchFinalizationResult
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let WorkbenchFinalization {
        context,
        reservation_owner,
        invocation_work,
        kernel,
        result,
        rejected,
        mut retire_scopes,
        context_boundary,
        control,
    } = finalization;
    let invocation_cleanup = invocation_work.cleanup(&environment, &kernel).await;
    let cleanup_uncertainty = invocation_cleanup.uncertainty();
    let cleanup_confirmed = cleanup_uncertainty.is_none();
    let result = retain_invocation_cleanup_summary(result, cleanup_uncertainty);
    let context_cancelled = context_boundary.is_some()
        && control
            .as_ref()
            .is_some_and(|control| control.context_cancellation_requested());
    let context_failed = context_boundary.is_some() && (!cleanup_confirmed || context_cancelled);
    if context_failed {
        let scopes = environment
            .actor_admissions
            .settle_checkpoints(
                context.actor,
                context_boundary.as_ref().expect("context boundary"),
                false,
            )
            .into_iter()
            .filter_map(|(session, scope)| (session == context.placement.session).then_some(scope));
        retire_scopes.get_or_insert_with(Vec::new).extend(scopes);
    }
    let checkpoint_cleanup_failure = match retire_scopes {
        Some(scopes) => environment
            .runner
            .retire_context_scopes(context.clone(), scopes)
            .await
            .err()
            .map(|failure| format!("failed checkpoint cleanup: {failure}")),
        None => None,
    };
    let cleanup_confirmed = cleanup_confirmed && checkpoint_cleanup_failure.is_none();
    if rejected || context_failed || checkpoint_cleanup_failure.is_some() {
        let (aborted, notifications) = environment
            .requests
            .abort_unsubmitted(context.actor, &reservation_owner);
        publish_request_notifications(
            &environment.requests,
            &environment.deployments,
            notifications,
        )
        .await;
        if !aborted.is_empty() {
            tracing::debug!(actor = ?context.actor, requests = ?aborted, "aborted unpublished request reservations after rejected workbench input");
        }
    }
    let result = match (result, checkpoint_cleanup_failure) {
        (result, None) => result,
        (Err(failure), Some(cleanup)) => Err(WorkbenchExecutionFailure {
            receipts: failure.receipts,
            point: failure.point,
            publication: failure.publication,
            total: failure.total,
            source: ResidentActorWorkbenchError::ActorProtocol(format!(
                "{}; {cleanup}",
                failure.source
            )),
        }),
        (
            Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response)),
            Some(cleanup),
        )
        | (
            Ok(KernelStep::Stop {
                output: response, ..
            }),
            Some(cleanup),
        ) => Err(failed_checkpoint_cleanup_response(response, cleanup)),
    };
    let result = result.map_err(|failure| {
        let source = failure.source;
        if matches!(&source, ResidentActorWorkbenchError::ToolDispatch(_))
            && failure.publication.is_none()
        {
            let detail = match &source {
                ResidentActorWorkbenchError::ToolDispatch(error) => error.to_string(),
                _ => source.to_string(),
            };
            return KernelInvocationFailure::Rejected {
                receipts: failure.receipts,
                actor: context.actor,
                detail,
                diagnostic: None,
            };
        }
        KernelInvocationFailure::Workbench(crate::KernelWorkbenchFailure {
            actor: context.actor,
            receipts: failure.receipts,
            point: failure.point,
            publication: failure.publication,
            total: failure.total,
            diagnostic: source.failure_diagnostic(),
            detail: source.to_string(),
        })
    });
    WorkbenchFinalizationResult {
        result,
        cleanup_confirmed,
    }
}

struct WorkbenchEffectState {
    display_receipt_owner: Option<Arc<DisplayExecutionSettlement>>,
    park_effects: bool,
    context: ActorSessionContext,
    public_visibility: Option<tidepool_runtime::session::PublicVisibilitySnapshot>,
    control: Option<Arc<crate::resident_tools::WorkbenchExecutionControl>>,
    model: Option<Arc<dyn crate::CellModelBinding>>,
    context_binding: Option<Arc<dyn crate::HostedContextBinding>>,
    installed_tools: Option<crate::InstalledToolLease>,
    admitted_source: crate::CheckpointSourceLayer,
    reservation_owner: RequestReservationOwner,
    invocation_work: Arc<InvocationWork>,
    publication: CheckpointPublication,
    /// The nested slot borrows this execution and cannot recursively invoke itself.
    after_tool_active: bool,
    terminal_transfer: Option<Arc<AcceptedTerminalTransfer>>,
}

struct WorkbenchCursor {
    preparation_done: bool,
    dispatch_initialized: bool,
    prepared_cell: Option<Vec<Option<crate::resident_workbench::PreparedCellItem>>>,
    dependencies: Option<crate::resident_workbench::CellPreparationLease>,
    cell_check: Option<tidepool_runtime::session::CellCheck>,
    receipts: Vec<WorkbenchItemReceipt>,
    cell_display_remaining: usize,
    index: usize,
    running: Option<WorkbenchFragmentExecution>,
    starting: Option<owned_workbench::WorkbenchUnitStart>,
    started: Option<Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>>,
    unit: WorkbenchUnitState,
    tool_dispatch: Option<Arc<RootCustody>>,
    completed: Option<WorkbenchItemReceipt>,
    after_tool: Option<WorkbenchAfterToolExecution>,
}

enum WorkbenchAfterToolAnswer {
    Settled(Result<crate::after_tool::Annotation, ResidentActorWorkbenchError>),
    TimedOut,
}

struct WorkbenchAfterToolExecution {
    frame: after_tool_wait::AfterToolFrame,
    receipt: WorkbenchItemReceipt,
    prepared: Option<Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>>,
    answer: Option<WorkbenchAfterToolAnswer>,
    enforce_deadline: bool,
    span: tracing::Span,
}

struct WorkbenchFragmentExecution {
    native_start: Option<owned_workbench::WorkbenchFragmentRequest>,
    native_result:
        Option<Result<owned_workbench::WorkbenchFragmentAdvance, ResidentActorWorkbenchError>>,
    parked_effect: Option<ParkedWorkbenchEffect>,
    inflight_effect: Option<WorkbenchEffectStamp>,
    resume_failure: Option<ResidentActorWorkbenchError>,
    fragment: Option<ResidentWorkbenchFragment>,
    outcome: Option<ResidentOutcome>,
    scopes: Vec<scopes::ScopeFrame>,
    green: Option<green::GreenInvocation<green_notebook::EffectCompletion>>,
}

impl WorkbenchFragmentExecution {
    fn new(fragment: ResidentWorkbenchFragment, outcome: ResidentWorkbenchSuspension) -> Self {
        Self {
            native_start: None,
            native_result: None,
            parked_effect: None,
            inflight_effect: None,
            resume_failure: None,
            fragment: Some(fragment),
            outcome: Some(outcome.into()),
            scopes: Vec::new(),
            green: None,
        }
    }
}

#[derive(Default)]
struct WorkbenchUnitState {
    effect_ordinal: usize,
    operations: Vec<WorkbenchOperationReceipt>,
    command_output: Vec<String>,
    recovered_bindings: Vec<String>,
    display_remaining: usize,
}

impl Default for WorkbenchCursor {
    fn default() -> Self {
        Self {
            preparation_done: false,
            dispatch_initialized: false,
            prepared_cell: None,
            dependencies: None,
            cell_check: None,
            receipts: Vec::new(),
            cell_display_remaining: 8192,
            index: 0,
            running: None,
            starting: None,
            started: None,
            unit: WorkbenchUnitState::default(),
            tool_dispatch: None,
            completed: None,
            after_tool: None,
        }
    }
}

struct WorkbenchEffectStamp {
    display: Option<WorkbenchDisplayOutput>,
    display_settlement: Option<Arc<DisplayOperationSettlement>>,
    success_disposition: WorkbenchOperationDisposition,
    ordinal: usize,
    effect: String,
    started: std::time::Instant,
}

struct ParkedWorkbenchEffect {
    display: Option<WorkbenchDisplayOutput>,
    success_disposition: WorkbenchOperationDisposition,
    wait: OwnedWorkbenchWait,
    ordinal: usize,
    effect: String,
    started: std::time::Instant,
}

enum OwnedWorkbenchWait {
    Display {
        boundary: ResidentActorBoundary,
        allowance: i64,
        operation: Option<WorkbenchOperationId>,
    },
    Launch(Box<child_launch::PreparedChildLaunch>),
    Prepared(
        futures_util::future::BoxFuture<
            'static,
            Result<ResidentOutcome, ResidentActorWorkbenchError>,
        >,
    ),
    RetainCommandBinding {
        continuation: ResidentHole,
        operation: futures_util::future::BoxFuture<
            'static,
            (
                Result<String, tidepool_bridge_effects::CommandError>,
                Option<crate::resident_workbench::RetainedHostBinding>,
            ),
        >,
    },
    Watch(crate::request_effect::WatchPoll),
    Drain {
        continuation: ResidentHole,
        target: LocalActorRef,
    },
    Exit {
        continuation: ResidentHole,
        target: ActorRef,
        terminal: crate::RetainedActorExit,
    },
    PollExit {
        continuation: ResidentHole,
        target: ActorRef,
        terminal: Option<crate::RetainedActorExit>,
    },
    Sleep {
        continuation: ResidentHole,
        duration: std::time::Duration,
    },
    External {
        continuation: ResidentHole,
        work: tidepool_effect::DeferredEffect,
    },
    Jev {
        continuation: ResidentHole,
        request: String,
    },
    Command {
        continuation: ResidentHole,
        request: crate::generated::commands::CommandsReq,
    },
}

impl OwnedWorkbenchWait {
    fn observe_after_resume(&self) -> Option<ActorRef> {
        match self {
            Self::Exit { target, .. } => Some(*target),
            Self::PollExit {
                target,
                terminal: Some(_),
                ..
            } => Some(*target),
            _ => None,
        }
    }

    fn capture(boundary: ResidentActorBoundary) -> Result<Self, ResidentActorBoundary> {
        match boundary {
            ResidentActorBoundary::WatchAwait(poll) => Ok(Self::Watch(poll)),
            ResidentActorBoundary::Sleep {
                continuation,
                duration,
            } => Ok(Self::Sleep {
                continuation,
                duration,
            }),
            ResidentActorBoundary::External { continuation, work } => {
                Ok(Self::External { continuation, work })
            }
            ResidentActorBoundary::Jev {
                continuation,
                request,
            } => Ok(Self::Jev {
                continuation,
                request,
            }),
            ResidentActorBoundary::Command {
                continuation,
                request,
            } => Ok(Self::Command {
                continuation,
                request,
            }),
            other => Err(other),
        }
    }
}

enum WorkbenchRunAdvance {
    Complete(KernelStep<WorkbenchResponse>),
    ParkUnit,
    ParkNative,
    ParkEffect,
    ParkGreen,
    ParkAfterToolStart,
    ParkAfterToolFinish,
}

enum FragmentAdvance {
    Settled(ResidentWorkbenchStep),
    ParkEffect,
    ParkNative,
    ParkGreen,
}

/// Structured actor turns retain actor publication authority. Workbench effects
/// borrow the exact admitted call instead of consulting an actor singleton.
#[derive(Clone)]
enum CurrentEffectOwner<'a> {
    Workbench(&'a WorkbenchEffectState),
    Scoped {
        base: Box<CurrentEffectOwner<'a>>,
        scope: Arc<InvocationWork>,
        wait_control: Option<Arc<crate::WorkbenchExecutionControl>>,
    },
    Tool {
        work: Arc<InvocationWork>,
        publication: CheckpointPublication,
        control: Arc<crate::WorkbenchExecutionControl>,
    },
    Actor {
        ephemeral_work: Arc<InvocationWork>,
        publication: CheckpointPublication,
        reservation_owner: Option<RequestReservationOwner>,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    },
}

impl CurrentEffectOwner<'_> {
    fn context_binding(&self) -> Option<Arc<dyn crate::HostedContextBinding>> {
        match self {
            Self::Scoped { base, .. } => base.context_binding(),

            Self::Workbench(execution) if !execution.after_tool_active => {
                execution.context_binding.clone()
            }
            _ => None,
        }
    }
    fn model(&self) -> Option<Arc<dyn crate::CellModelBinding>> {
        match self {
            Self::Scoped { base, .. } => base.model(),

            Self::Workbench(execution) => execution.model.clone(),
            Self::Actor { .. } | Self::Tool { .. } => None,
        }
    }

    fn invocation_work(&self) -> Option<Arc<InvocationWork>> {
        match self {
            Self::Scoped { base, .. } => base.invocation_work(),

            Self::Workbench(execution) => {
                assert!(execution
                    .invocation_work
                    .matches(execution.context.actor, &execution.reservation_owner));
                Some(execution.invocation_work.clone())
            }
            Self::Tool { work, .. } => Some(work.clone()),
            Self::Actor { .. } => None,
        }
    }

    fn ephemeral_work(&self) -> Option<Arc<InvocationWork>> {
        match self {
            Self::Scoped { scope, .. } => Some(scope.clone()),
            Self::Actor { ephemeral_work, .. } => Some(ephemeral_work.clone()),
            Self::Tool { work, .. } => Some(work.clone()),
            _ => self.invocation_work(),
        }
    }

    fn publication(&self) -> &CheckpointPublication {
        match self {
            Self::Scoped { base, .. } => base.publication(),

            Self::Workbench(execution) => &execution.publication,
            Self::Actor { publication, .. } | Self::Tool { publication, .. } => publication,
        }
    }

    fn interaction_control(&self) -> Option<Arc<crate::WorkbenchExecutionControl>> {
        match self {
            Self::Scoped { base, .. } => base.interaction_control(),
            Self::Workbench(execution) => execution.control.clone(),
            Self::Actor { control, .. } => control.clone(),
            Self::Tool { control, .. } => Some(control.clone()),
        }
    }

    fn control(&self) -> Option<Arc<crate::WorkbenchExecutionControl>> {
        match self {
            Self::Scoped {
                base, wait_control, ..
            } => wait_control.clone().or_else(|| base.control()),

            Self::Workbench(execution) => execution.control.clone(),
            Self::Actor { control, .. } => control.clone(),
            Self::Tool { control, .. } => Some(control.clone()),
        }
    }

    fn admitted_source(&self) -> Option<&crate::CheckpointSourceLayer> {
        match self {
            Self::Scoped { base, .. } => base.admitted_source(),

            Self::Workbench(execution) => Some(&execution.admitted_source),
            Self::Actor { .. } | Self::Tool { .. } => None,
        }
    }

    fn reservation_owner(&self) -> Option<RequestReservationOwner> {
        match self {
            Self::Scoped { scope, .. } => Some(scope.reservation_owner()),

            Self::Workbench(execution) => Some(execution.reservation_owner.clone()),
            Self::Actor {
                reservation_owner, ..
            } => reservation_owner.clone(),
            Self::Tool { work, .. } => Some(work.reservation_owner()),
        }
    }

    fn submission_owner(&self) -> Option<RequestReservationOwner> {
        match self {
            Self::Scoped { base, .. } => base.submission_owner(),
            _ => self.reservation_owner(),
        }
    }

    fn after_tool_active(&self) -> bool {
        match self {
            Self::Scoped { base, .. } => base.after_tool_active(),
            Self::Workbench(execution) => execution.after_tool_active,
            Self::Actor { .. } | Self::Tool { .. } => false,
        }
    }
}

fn prepare_execution_effect(
    context: &ActorSessionContext,
    owner: &CurrentEffectOwner<'_>,
    boundary: ResidentActorBoundary,
) -> ResidentActorBoundary {
    match boundary {
        ResidentActorBoundary::Context {
            continuation,
            request,
            table,
        } => {
            let work = match owner.context_binding() {
                Some(binding) => binding.prepare(
                    request,
                    tidepool_repr::PrincipalId::from(context.actor),
                    table,
                ),
                None => tidepool_effect::DeferredEffect::blocking(|| {
                    Err(tidepool_effect::error::EffectError::Handler(
                        "context mutation requires the exact synchronous invocation".into(),
                    ))
                }),
            };
            ResidentActorBoundary::External { continuation, work }
        }
        ResidentActorBoundary::Model {
            continuation,
            request,
            table,
        } => {
            let work = match owner.model() {
                Some(model) => model.prepare(
                    request,
                    tidepool_repr::PrincipalId::from(context.actor),
                    table,
                ),
                None => crate::cell_model::unavailable_model_work(request),
            };
            ResidentActorBoundary::External { continuation, work }
        }
        boundary => boundary,
    }
}

struct WorkbenchAdmission {
    context: ActorSessionContext,
    request: WorkbenchRequest,
    installed_tools: Option<crate::InstalledToolLease>,
    admitted_source: crate::CheckpointSourceLayer,
    compilation_authority: Option<Arc<WorkbenchCompilationAuthority>>,
    public_owner: Arc<WorkbenchPublicOwner>,
    current_builtin: bool,
    capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
    context_binding: Option<Arc<dyn crate::HostedContextBinding>>,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    invocation: Option<crate::resident_tools::WorkbenchCallKey>,
}

enum WorkbenchPreflight {
    Retained(crate::KernelWorkbenchReply),
    Admitted(WorkbenchAdmission),
}

fn admit_context_authority(
    actor: ActorRef,
    selected: Option<&exomonad_tool::HostedTool>,
    invocation: Option<&exomonad_tool::ToolInvocationContext>,
) -> Result<(), KernelInvocationFailure> {
    if selected.is_none_or(|tool| {
        tool.scheduling() != exomonad_tool::ToolScheduling::BeforeNextInference
            || !tool
                .effect_keys()
                .contains(&exomonad_tool::ToolEffectKey::ContextReadWrite)
    }) || invocation.is_none_or(|invocation| invocation.model_operation().is_none())
    {
        return Err(KernelInvocationFailure::Rejected {
            receipts: Vec::new(),
            actor,
            detail: "context authority requires an exact synchronous ContextReadWrite invocation"
                .into(),
            diagnostic: None,
        });
    }
    Ok(())
}

// Checkpoints retain the exact invocation provenance and hosted capture owner.
#[derive(Clone)]
enum CheckpointPublication {
    Resident,
    Workbench {
        boundary: Option<tidepool_runtime::session::ContextCheckpointBoundary>,
        capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
    },
    Route(tidepool_runtime::session::ContextCheckpointBoundary),
}

impl CheckpointPublication {
    fn boundary(&self) -> Option<&tidepool_runtime::session::ContextCheckpointBoundary> {
        match self {
            Self::Resident => None,
            Self::Workbench { boundary, .. } => boundary.as_ref(),
            Self::Route(boundary) => Some(boundary),
        }
    }

    fn hosted_boundary(&self) -> Option<&tidepool_runtime::session::ContextCheckpointBoundary> {
        match self {
            Self::Workbench {
                boundary: Some(boundary),
                ..
            } => boundary.hosted().map(|_| boundary),
            Self::Resident | Self::Workbench { .. } | Self::Route(_) => None,
        }
    }

    fn capture(&self) -> Option<&Arc<dyn crate::HostedCheckpointCapture>> {
        match self {
            Self::Workbench { capture, .. } => capture.as_ref(),
            Self::Resident | Self::Route(_) => None,
        }
    }
}

impl<H, O> ResidentKernelBehavior<H, O> {
    fn capture_exit_target(
        &self,
        kernel: &KernelContext,
        _effect_owner: &CurrentEffectOwner<'_>,
        target: ActorRef,
    ) -> Result<crate::RetainedActorExit, ResidentActorWorkbenchError> {
        let actor = kernel.resolve(target).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                KernelCallFailure::TargetUnavailable(target).to_string(),
            )
        })?;
        kernel.session_context(target).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                KernelCallFailure::TargetUnavailable(target).to_string(),
            )
        })?;
        Ok(actor.terminal().clone())
    }

    fn capture_drain_target(
        &self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        target: ActorRef,
    ) -> Result<LocalActorRef, ResidentActorWorkbenchError> {
        let authorized = target != context.actor
            && actor_can_control(context.actor, target, &self.environment.actors.lock());
        if !authorized {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "actor drain is not authorized".into(),
            ));
        }
        kernel.resolve(target).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol("drain target is unavailable".into())
        })
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
            explicit_installer: None,
            prepared_request_receiver: None,
            request_receiver_scope: None,
            fresh_context_seed: None,
            spawn_source: None,
            spawn_helper_branch: None,
            spawn_admission: None,
            root_startup: None,
            standing: ResidentStanding::Boot,
            shutdown_hook: None,
            checkpoint: None,
            admitted_checkpoint: None,
            child_session_startup: None,
            child_placement_custody: None,
            pending_checkpoint: None,
            active_input: None,
            input_origin: ActorInputOrigin::ActorStartup,
            sources: Vec::new(),
            source_connections: None,
            static_source_count: 0,
            launch_worktrees,
            prepared_workspace: None,
            worktree_custody: None,
            policy_installed: false,
            installed_tools: Arc::default(),
            spec_installs: 0,
            after_tool: crate::after_tool::AfterToolLog::default(),
            forest_control: false,
            pending_program: None,
            pending_reply: None,
            pending_response: None,
            exit_destination: None,
            pending_exit: None,
            pending_reply_preview: None,
            pending_cancellation: None,
            suspended_cast: None,
            outstanding_interactive: None,
            child_exit_observations: ChildExitObservations::default(),
            deferred_child_failures: Vec::new(),
            next_activation_sequence: 1,
            runtime_observation: crate::ActorRuntimeObservationHandle::default(),
            workbench_executions: Arc::default(),
            checkpoint_publication: CheckpointPublication::Resident,
            active_route_reservation_owner: None,
            settled_checkpoint_boundaries: Vec::new(),
            roster_snapshot: Mutex::new(None),
            assignment_base: None,
            #[cfg(test)]
            activation_preview_observer: None,
        }
    }
    fn context(&self, actor: ActorRef) -> ActorSessionContext {
        self.descriptor.session_context(actor)
    }

    fn preflight_workbench(
        &self,
        actor: ActorRef,
        invocation: crate::ActorWorkbenchInvocation,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> Result<WorkbenchPreflight, KernelInvocationFailure>
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        let mut context = self.context(actor);
        let (mut request, execution) = ensure_workbench_execution_id(invocation.request);
        if request.checkpoint_boundary().is_none() {
            request = request.with_checkpoint_boundary(
                tidepool_runtime::session::ContextCheckpointBoundary::Execution {
                    actor_id: actor.id.0,
                    incarnation: actor.incarnation.0,
                    execution_id: execution,
                },
            );
        }
        let installed_tools = match invocation.installed_tools {
            Some(lease) if lease.actor() != context.actor => {
                return Err(KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor: context.actor,
                    detail: "issued tool installation belongs to another actor".into(),
                    diagnostic: None,
                });
            }
            Some(lease) => Some(lease),
            None => self.installed_tools.current(),
        };
        let current_builtin = request.tool_call().is_some_and(|call| {
            matches!(
                call.name.as_str(),
                crate::status_tool::STATUS_TOOL
                    | crate::reload_spec_tool::RELOAD_SPEC_TOOL
                    | crate::reload_helpers_tool::RELOAD_HELPERS_TOOL
            )
        });
        if !self.policy_installed
            || !matches!(
                self.standing,
                ResidentStanding::Interactive(_)
                    | ResidentStanding::Receiving(_)
                    | ResidentStanding::Workbench
            )
        {
            return Err(KernelInvocationFailure::Rejected {
                receipts: Vec::new(),
                actor: context.actor,
                detail: "actor has no active Haskell application workbench".into(),
                diagnostic: None,
            });
        }
        let invocation_key = control
            .as_ref()
            .and_then(|control| control.invocation.clone());
        let selected_tool = invocation.selected_tool.or_else(|| {
            request.tool_call().and_then(|call| {
                installed_tools
                    .as_ref()
                    .and_then(|lease| lease.tools())
                    .and_then(|tools| {
                        tools
                            .declarations
                            .iter()
                            .find(|tool| tool.name() == call.name)
                    })
                    .cloned()
            })
        });
        if let Some(selected) = &selected_tool {
            let builtin = if current_builtin {
                request
                    .tool_call()
                    .and_then(|call| match call.name.as_str() {
                        crate::status_tool::STATUS_TOOL => Some(crate::status_tool::declaration()),
                        crate::reload_spec_tool::RELOAD_SPEC_TOOL => {
                            Some(crate::reload_spec_tool::declaration())
                        }
                        crate::reload_helpers_tool::RELOAD_HELPERS_TOOL => {
                            Some(crate::reload_helpers_tool::declaration())
                        }
                        _ => None,
                    })
            } else {
                None
            };
            let retained = builtin.as_ref().or_else(|| {
                installed_tools
                    .as_ref()
                    .and_then(|lease| lease.tools())
                    .and_then(|tools| {
                        tools
                            .declarations
                            .iter()
                            .find(|tool| tool.name() == selected.name())
                    })
            });
            if retained != Some(selected)
                || request
                    .tool_call()
                    .is_some_and(|call| call.name != selected.name())
            {
                return Err(KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor,
                    detail: "selected notebook contract differs from the issued installation"
                        .into(),
                    diagnostic: None,
                });
            }
            let keys = selected.effect_keys();
            if keys.iter().any(|key| match key {
                exomonad_tool::ToolEffectKey::Actor(key) => {
                    !self.descriptor.capabilities().effect_keys().contains(key)
                }
                exomonad_tool::ToolEffectKey::ContextReadWrite => {
                    selected.scheduling() != exomonad_tool::ToolScheduling::BeforeNextInference
                }
            }) {
                return Err(KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor,
                    detail: "selected notebook effects exceed its scheduling or actor authority"
                        .into(),
                    diagnostic: None,
                });
            }
            if !current_builtin {
                context.haskell_effects_alias = match selected.implementation() {
                    exomonad_tool::ToolImplementation::HaskellCell => format!(
                        "'[{}]",
                        keys.iter()
                            .map(|key| key.haskell_name())
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                    .into(),
                    exomonad_tool::ToolImplementation::ResidentHandler => installed_tools
                        .as_ref()
                        .and_then(|lease| lease.tools())
                        .expect("selected retained handler checked")
                        .dispatcher_effects
                        .clone(),
                };
            }
        }
        if invocation.context_binding.is_some() {
            admit_context_authority(
                actor,
                selected_tool.as_ref(),
                invocation_key.as_ref().map(|key| key.invocation()),
            )?;
        }
        if let Some(execution) = request.execution_id() {
            match self
                .workbench_executions
                .lock()
                .lookup_for_provider_control(
                    execution,
                    &request,
                    invocation_key.as_ref(),
                    control.as_ref(),
                ) {
                Err(failure) => {
                    return Err(KernelInvocationFailure::Rejected {
                        receipts: Vec::new(),
                        actor: context.actor,
                        detail: match failure {
                            WorkbenchReplayFailure::DifferentInput => "one hosted call identity was retried with different Haskell input",
                            WorkbenchReplayFailure::Unconfirmed => "the original hosted call outcome is unconfirmed; replay cannot repeat its effects",
                        }
                        .into(),
                        diagnostic: None,
                    });
                }
                Ok(Some(reply)) => {
                    return Ok(WorkbenchPreflight::Retained(reply));
                }
                Ok(None) => {}
            }
        }
        let admitted_source = if current_builtin {
            // Builtins inspect current state or prepare a fresh reload source;
            // they acquire no compilation authority from this admission.
            installed_tools
                .as_ref()
                .map_or_else(crate::CheckpointSourceLayer::default, |lease| {
                    lease.source().clone()
                })
        } else {
            installed_tools.as_ref().ok_or_else(|| KernelInvocationFailure::Rejected {
                receipts: Vec::new(),
                actor: context.actor,
                detail: "cannot admit exact source layer: the source installation is unavailable; repair it with reload_helpers".into(),
                diagnostic: None,
            })?.source().clone()
        };
        let compilation_authority = if current_builtin {
            None
        } else {
            let (selected, authority) = WorkbenchCompilationAuthority::admit(
                context,
                admitted_source.clone(),
                installed_tools.clone(),
                self.environment.source_layers.as_ref(),
            )?;
            context = selected;
            Some(authority)
        };
        if self
            .root_startup
            .as_ref()
            .is_some_and(|(_, latch)| *latch.lock() != RootStartupState::Activated)
        {
            return Err(KernelInvocationFailure::Rejected {
                receipts: Vec::new(),
                actor,
                detail: "root startup is pending durable release".into(),
                diagnostic: None,
            });
        }
        let public_owner = {
            let records = self.environment.actors.lock();
            let record = records
                .get(&actor)
                .ok_or_else(|| KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor,
                    detail: "publication owner has no registered actor".into(),
                    diagnostic: None,
                })?;
            let owner =
                record
                    .public_owner
                    .ready()
                    .ok_or_else(|| KernelInvocationFailure::Rejected {
                        receipts: Vec::new(),
                        actor,
                        detail: "durable actor public surface is not initialized".into(),
                        diagnostic: None,
                    })?;
            if record.terminal.is_some()
                || record.descriptor.placement() != self.descriptor.placement()
                || record.descriptor.persistence_policy() != self.descriptor.persistence_policy()
                || record.descriptor.actor_path() != self.descriptor.actor_path()
                || !owner.matches_context(&context)
            {
                return Err(KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor,
                    detail: "publication owner differs from current actor admission".into(),
                    diagnostic: None,
                });
            }
            owner.clone()
        };
        if let Some(binding) = &invocation.context_binding {
            let control = control
                .as_ref()
                .expect("context admission checked exact invocation");
            binding
                .admit(
                    request.execution_id().expect("workbench execution issued"),
                    invocation_key
                        .as_ref()
                        .expect("exact invocation checked")
                        .invocation(),
                    tidepool_repr::PrincipalId::from(actor),
                )
                .map_err(|error| KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor,
                    detail: error.to_string(),
                    diagnostic: None,
                })?;
            control.bind_context(binding.clone());
        }
        Ok(WorkbenchPreflight::Admitted(WorkbenchAdmission {
            context,
            public_owner,
            request,
            installed_tools,
            admitted_source,
            compilation_authority,
            current_builtin,
            capture: invocation.hosted_checkpoint_capture,
            context_binding: invocation.context_binding,
            control,
            invocation: invocation_key,
        }))
    }

    fn actor_effect_owner(&self, actor: ActorRef) -> CurrentEffectOwner<'static> {
        let control = crate::resident_workbench::execution_control();
        let ephemeral_work = match &self.active_route_reservation_owner {
            Some(reservation) => self
                .workbench_executions
                .lock()
                .callback_root(actor, reservation.clone()),
            None => self.workbench_executions.lock().actor_scope_root(actor),
        };
        CurrentEffectOwner::Actor {
            ephemeral_work,
            publication: self.checkpoint_publication.clone(),
            reservation_owner: control
                .as_ref()
                .and_then(|control| control.reservation_owner(actor))
                .or_else(|| self.active_route_reservation_owner.clone()),
            control,
        }
    }

    fn failure(detail: impl Into<String>) -> KernelBehaviorError {
        KernelBehaviorError::new(detail)
    }

    fn tool_invocation_failure(
        actor: ActorRef,
        error: ResidentActorWorkbenchError,
    ) -> KernelInvocationFailure {
        match error {
            ResidentActorWorkbenchError::InvocationCancelled => {
                KernelInvocationFailure::Cancelled { actor }
            }
            error => Self::invocation_failure(actor, error),
        }
    }

    fn workbench_failure(error: ResidentActorWorkbenchError) -> KernelBehaviorError {
        error.into_kernel_behavior_error()
    }

    fn invocation_failure(
        actor: ActorRef,
        error: impl Into<KernelBehaviorError>,
    ) -> KernelInvocationFailure {
        let error = error.into();
        KernelInvocationFailure::Failed {
            receipts: Vec::new(),
            actor,
            detail: error.detail,
            diagnostic: error.diagnostic,
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
        let admission = installation.spawn_admission.clone();
        if let Err(error) =
            self.environment
                .deployments
                .try_send(LocalResidentDeployment::PolicyInstalled(Box::new(
                    installation,
                )))
        {
            if let Some(authority) = admission {
                authority.fail(format!(
                    "provider attachment deployment unavailable: {error}"
                ));
            }
        }
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
        if let Some(admission) = &self.spawn_admission {
            admission.fail(terminal.summary.clone());
        }
        publish_retired(&self.environment, actor, terminal);
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
        tracked_stopped_projection(actor, &self.environment.deployments, RELEASE_WAIT).await
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
        publish_request_notifications(
            &self.environment.requests,
            &self.environment.deployments,
            notifications,
        )
        .await;
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

    /// `changes_only` applies to the concise view: once this actor has a
    /// previous summary-family roster, render only rows whose text changed.
    fn status_text(
        &self,
        kernel: &KernelContext,
        actor: ActorRef,
        view: StatusView,
        changes_only: bool,
    ) -> String {
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
        let records = self.environment.actors.lock().clone();
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
                let key = format!(
                    "{:?} ({}@{})",
                    record.descriptor.display_label(),
                    identity.id.0,
                    identity.incarnation.0
                );
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
                // Delivery line: one per live child, from the host pump's
                // published inbound-delivery state.
                let delivery = if terminal.is_none() {
                    format!(
                        "\n    {}",
                        runtime.delivery_status_line(
                            &record.descriptor.display_label(),
                            requests.0.len() + requests.1.len(),
                            crate::runtime_observation::unix_time_ms(),
                        )
                    )
                } else {
                    String::new()
                };
                if view == StatusView::Concise {
                    return Some((key, format!(
                        "  - {:?} ({}@{}) supervisor={} bound_worktree={:?} state={state} {}{delivery}",
                        record.descriptor.display_label(), identity.id.0, identity.incarnation.0,
                        record.descriptor.supervisor_parent().map_or_else(
                            || "none".to_owned(),
                            |parent| format!("{}@{}", parent.id.0, parent.incarnation.0),
                        ),
                        record.bound_worktree,
                        runtime.usage_summary_display(),
                    )));
                }
                if view == StatusView::Lineage {
                    return Some((key, format!(
                        "  - {:?} ({}@{}) creator={:?} supervisor={:?} context_parent={:?}\n    haskell_scope={} provider_thread={:?} provider_parent_thread={:?} first_usage={:?} cache_boundary={:?} cached_input={:?} uncached_input={:?} bound_worktree={:?} {}",
                        record.descriptor.display_label(), identity.id.0, identity.incarnation.0,
                        record.descriptor.creator(), record.descriptor.supervisor_parent(), record.descriptor.context_parent(),
                        record.descriptor.placement().lexical_scope.0,
                        runtime.provider_thread, runtime.provider_parent_thread,
                        runtime.first_provider_usage.as_ref().map(|sample| (&sample.observation_id, sample.cached_input_tokens, sample.uncached_input_tokens)),
                        usage.map(|sample| sample.cache_boundary),
                        usage.map(|sample| sample.cached_input_tokens),
                        usage.map(|sample| sample.uncached_input_tokens),
                        record.bound_worktree,
                        runtime.usage_summary_display(),
                    )));
                }
                Some((key, format!(
                    "  - {}@{} label={:?} supervisor={:?} context_parent={:?} bound_worktree={:?} provider_thread={:?} provider_parent_thread={:?} cache_input={:?}/{:?} workbench={:?} state={} {}{delivery}",
                    identity.id.0,
                    identity.incarnation.0,
                    record.descriptor.display_label(),
                    record.descriptor.supervisor_parent(),
                    record.descriptor.context_parent(),
                    record.bound_worktree,
                    runtime.provider_thread,
                    runtime.provider_parent_thread,
                    usage.map(|sample| sample.cached_input_tokens),
                    usage.map(|sample| sample.uncached_input_tokens),
                    runtime.workbench_posture,
                    state,
                    runtime.usage_summary_display(),
                )))
            })
            .collect::<Vec<_>>();
        drop(records);
        roster.sort_by(|left, right| left.1.cmp(&right.1));
        let roster_text = if view == StatusView::Concise {
            let mut snapshot = self.roster_snapshot.lock();
            let text = match snapshot.as_ref().filter(|_| changes_only) {
                Some(previous) => render_roster_changes(previous, &roster),
                None => join_roster_rows(&roster),
            };
            *snapshot = Some(RosterSnapshot::new(
                crate::runtime_observation::unix_time_ms(),
                &roster,
            ));
            text
        } else {
            join_roster_rows(&roster)
        };
        let runtime = self.runtime_observation.snapshot();
        let usage = runtime.latest_provider_usage();
        let unavailable_responses = format!("{:?}", requests.unavailable_responses);
        let unavailable_watches = format!(
            "{:?}; rejected: {:?}",
            requests.unavailable_watches, requests.rejected_watches
        );
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
                "actor {:?} ({}@{})\n  activation={:?} application={} program={standing} current_request={current_request:?}\n  responses: ready={:?} unavailable={} pending={:?}\n  watches: ready={:?} unavailable={} pending={:?}\n  jobs: running={:?}\n  descendant_depth={} active_children={} bound_worktree={:?} workbench={:?}{}",
                self.descriptor.display_label(),
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
                requests.running_jobs,
                self.descriptor.capabilities().descendants().maximum_depth,
                crate::render_child_budget(
                    self.descriptor
                        .capabilities()
                        .descendants()
                        .maximum_active_children,
                ),
                self.launch_worktrees.first(),
                runtime.workbench_posture,
                roster_summary,
            )
        } else {
            format!(
                "actor {}@{} label={:?}\n  lineage: creator={:?} supervisor={:?} context_parent={:?}\n  context: haskell_scope={} provider_thread={:?} provider_parent_thread={:?} cache_input={:?}/{:?} cache_boundary={:?}\n  activation: kind={:?} event_watermark={}\n  available effects: {} descendants={:?}{}\n  runtime: application={} program={standing} workbench={:?} current_request={current_request:?} bound_worktree={:?}\n  responses: pending={:?} ready={:?} unavailable={}\n  watches: pending={:?} ready={:?} unavailable={}\n  jobs: running={:?}{}{}",
                actor.id.0,
                actor.incarnation.0,
                self.descriptor.display_label(),
                self.descriptor.creator(),
                self.descriptor.supervisor_parent(),
                self.descriptor.context_parent(),
                self.descriptor.placement().lexical_scope.0,
                runtime.provider_thread,
                runtime.provider_parent_thread,
                usage.map(|sample| sample.cached_input_tokens),
                usage.map(|sample| sample.uncached_input_tokens),
                usage.map(|sample| sample.cache_boundary),
                runtime.activation_kind,
                runtime.event_watermark,
                self.descriptor.capabilities().haskell_effects_type(),
                self.descriptor.capabilities().descendants(),
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
                requests.running_jobs,
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
        let spec = match (view, self.installed_tools.observe()) {
            (StatusView::Concise, _) => String::new(),
            (_, crate::resident_workbench::InstalledToolsObservation::Available(Some(tools))) => {
                format!(
                    "\n  spec: {} slots=[{}]",
                    tools.provenance(),
                    tools.slots.join(", ")
                )
            }
            (
                _,
                crate::resident_workbench::InstalledToolsObservation::SourceUnavailable(Some(
                    tools,
                )),
            ) => format!(
                "\n  spec: {} slots=[{}]; source unavailable",
                tools.provenance(),
                tools.slots.join(", ")
            ),
            (_, crate::resident_workbench::InstalledToolsObservation::SourceUnavailable(None)) => {
                "\n  spec: none installed; source unavailable".to_string()
            }
            (
                _,
                crate::resident_workbench::InstalledToolsObservation::Empty
                | crate::resident_workbench::InstalledToolsObservation::Available(None),
            ) => "\n  spec: none installed".to_string(),
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
        let revisions = match view {
            StatusView::Expanded => format!(
                "\n{}",
                render_revisions_section(&self.revision_identities(kernel, actor))
            ),
            _ => String::new(),
        };
        let status = format!(
            "{current}{failure}{spec}{after_tool}\n  {workspace}\n  deadlines: [{}]\nactors:\n{roster_text}{revisions}",
            requests
                .deadlines
                .iter()
                .map(|(_, deadline)| deadline.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
        if view == StatusView::Lineage {
            let lineage = roster_text;
            format!(
                "actor {}@{} lineage\n  supervisor={:?}\n  context_parent={:?}\n \nactors:\n{lineage}",
                actor.id.0,
                actor.incarnation.0,
                self.descriptor.supervisor_parent(),
                self.descriptor.context_parent(),
            )
        } else {
            status
        }
    }

    /// The revision identities this actor works against, all read from
    /// observations the host already publishes (no Git here): the root's
    /// checkout head for the operator checkout, this actor's own checked
    /// head and source layer drift, its current assignment base, and each
    /// live descendant's checked head.
    fn revision_identities(&self, kernel: &KernelContext, actor: ActorRef) -> RevisionIdentities {
        let own = self.runtime_observation.snapshot();
        let records = self.environment.actors.lock();
        let live = |identity: &ActorRef, record: &ResidentActorRecord| {
            record.terminal.is_none()
                && kernel
                    .resolve(*identity)
                    .and_then(|actor| actor.terminal().get())
                    .is_none()
        };
        let operator_checkout = if self.descriptor.is_root() {
            own.source_drift.checkout.clone()
        } else {
            records
                .iter()
                .filter(|(identity, record)| record.descriptor.is_root() && live(identity, record))
                .map(|(_, record)| record.runtime_observation.snapshot().source_drift.checkout)
                .next()
                .unwrap_or_default()
        };
        let mut children = records
            .iter()
            .filter(|(identity, record)| {
                **identity != actor
                    && actor_in_creation_tree(actor, **identity, &records)
                    && live(identity, record)
            })
            .map(|(_, record)| {
                (
                    record.descriptor.display_label().into_owned(),
                    record.runtime_observation.snapshot().source_drift.checkout,
                )
            })
            .collect::<Vec<_>>();
        drop(records);
        children.sort_by(|left, right| left.0.cmp(&right.0));
        RevisionIdentities {
            operator_checkout,
            checked: own.source_drift.checkout,
            assignment_base: self.assignment_base.clone(),
            children,
            layer: own.source_drift.layer,
        }
    }

    /// The `status` tool's `revisions` view.
    fn revisions_status_text(&self, kernel: &KernelContext, actor: ActorRef) -> String {
        format!(
            "actor {}@{} revisions\n{}",
            actor.id.0,
            actor.incarnation.0,
            render_revisions_section(&self.revision_identities(kernel, actor))
        )
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
        let display_observations = journal
            .display_observations()
            .into_iter()
            .map(|(id, publication)| {
                format!(
                    "  {}:{}:{} {}",
                    id.execution,
                    id.input_unit_index + 1,
                    id.effect_ordinal + 1,
                    serde_json::to_string(&publication)
                        .expect("portable display publication metadata")
                )
            })
            .collect::<Vec<_>>();
        let display_observations = if display_observations.is_empty() {
            String::new()
        } else {
            format!(
                "\ndisplay publication settlement (current observation):\n{}",
                display_observations.join("\n")
            )
        };
        let warnings = journal.cleanup_warnings();
        let cleanup_warnings = if warnings.is_empty() {
            String::new()
        } else {
            format!("\ninvocation cleanup unconfirmed:\n{}", warnings.join("\n"))
        };
        let source_drift =
            render_source_drift_section(&self.runtime_observation.snapshot().source_drift);
        let attachments = self
            .sources
            .iter()
            .enumerate()
            .skip(self.static_source_count)
            .map(|(slot, source)| {
                use crate::request::sources::{RequestSourceKind, SourceTarget};
                let target = match source.target {
                    SourceTarget::Request(request, RequestSourceKind::Progress) => {
                        format!("progress request {}", request.0)
                    }
                    SourceTarget::Request(request, RequestSourceKind::Settlement) => {
                        format!("settlement request {}", request.0)
                    }
                    SourceTarget::Command(key) => format!("command {}", uuid::Uuid::from_u128(key)),
                    SourceTarget::Lifecycle(actor) => {
                        format!("lifecycle {}@{}", actor.id.0, actor.incarnation.0)
                    }
                };
                format!("  {slot}: {target}")
            })
            .collect::<Vec<_>>();
        let attachments = if attachments.is_empty() {
            "  (none)".to_owned()
        } else {
            attachments.join("\n")
        };

        format!(
            "actor {}@{} what-is-live\ncollectors:\n{jobs}\nbindings:\n{binding_lines}\nattachments:\n{attachments}\nsource drift:\n{source_drift}{cleanup_warnings}{display_observations}",
            actor.id.0, actor.incarnation.0,
        )
    }
}

fn join_roster_rows(roster: &[(String, String)]) -> String {
    roster
        .iter()
        .map(|(_, row)| row.as_str())
        .collect::<Vec<_>>()
        .join("\n")
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
    /// Release every `RootCustody`/`Arc<RootCustody>` this behavior itself
    /// owns on its own resident session, ahead of the outstanding-custody
    /// check `stopped` performs right after calling this
    /// (`ResidentActorRunner::retire_child_session`): every field here that
    /// can hold one MUST be released, or a dedicated child session never
    /// reaches zero outstanding custody and its teardown defers forever
    /// (nothing else ever checks it out again to retry). Called only from
    /// `stopped`, the terminal kernel hook — `self` is dropped once it
    /// returns and nothing else calls a method on a stopped behavior
    /// first, so nothing reads any of these fields again afterwards.
    fn release_session_state(&mut self) {
        self.descriptor.source_imports().release_capture();
        self.explicit_installer.take();
        self.prepared_request_receiver.take();
        self.request_receiver_scope.take();
        self.shutdown_hook.take();
        self.checkpoint.take();
        self.admitted_checkpoint.take();
        self.pending_checkpoint.take();
        self.active_input.take();
        self.boot.take();
        std::mem::take(&mut self.retained_replacements);
        // Issued requests may retain their installation past actor retirement.
        // Their RootCustody keeps the compiled handler live until the last
        // request releases its lease.
        self.installed_tools.clear();
    }

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
                outstanding.type_evidence.clone(),
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
        _effect_owner: CurrentEffectOwner<'_>,
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
        let request = self
            .environment
            .runner
            .transfer_mailbox_value(
                context.clone(),
                request,
                target_context.placement.session,
                target_context.placement.resource_scope,
            )
            .await
            .map_err(ResidentCallError::Runtime)?;
        target_ref
            .call(context.actor, ancestry.clone(), request)
            .await
            .map_err(ResidentCallError::Call)
    }

    fn start_child<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        context: &'a ActorSessionContext,
        effect_owner: CurrentEffectOwner<'a>,
        start: crate::ResidentActorStart,
    ) -> futures_util::future::BoxFuture<'a, Result<ResidentOutcome, ResidentActorWorkbenchError>>
    {
        let prepared = self.prepare_child_launch(context, effect_owner, start);
        Box::pin(async move {
            let completed =
                child_launch::await_launch(self.environment.clone(), kernel.clone(), prepared)
                    .await;
            let resume = self.apply_child_launch(kernel, completed);
            child_launch::resume_launch(self.environment.clone(), kernel.clone(), resume).await
        })
    }

    fn prepare_child_launch(
        &mut self,
        context: &ActorSessionContext,
        effect_owner: CurrentEffectOwner<'_>,
        start: crate::ResidentActorStart,
    ) -> Box<child_launch::PreparedChildLaunch> {
        tracing::info!(target: "exomonad_actor::workbench_phase", parent = %context.actor, label = %start.child.descriptor.display_label(), phase = "child_launch_requested", "actor phase");
        let crate::ResidentActorStart { parent_hole, child } = start;
        let spawn_reply = child.spawn.is_some();
        let placement_custody =
            child_launch::ChildPlacementCustody::new(child.descriptor.placement());
        let placement_startup = placement_custody.startup_guard();
        let placement_registration = effect_owner
            .ephemeral_work()
            .expect("actor effects retain their construction cleanup owner")
            .retain_launch_placement(context, placement_custody.clone());
        let resolved_owner = self.resolve_resource_owner(context, &effect_owner, child.lifetime);
        let invocation_work = resolved_owner.as_ref().ok().cloned().flatten();
        let mut retained_spawn_admission = None;
        let admission = (|| {
            placement_registration.map_err(ResidentActorWorkbenchError::ActorProtocol)?;
            let invocation_work = resolved_owner?;
            let crate::start::CapturedChildLaunch {
                lifetime,
                mut descriptor,
                spawn,
                entry,
                launch_worktrees,
                record_workspace,
                seed,
                exit_destination,
            } = child;
            let child_session_startup = (descriptor.placement().session
                != context.placement.session
                && self.environment.runner.supports_child_sessions())
            .then(|| {
                self.environment
                    .runner
                    .child_session_startup_lease(descriptor.placement().session)
            });
            let checkpoint_preview = descriptor
                .checkpoint_token()
                .map(|token| {
                    self.environment
                        .actor_admissions
                        .preview_checkpoint(token, context.placement.session)
                })
                .transpose()
                .map_err(|refusal| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "checkpoint refusal: {refusal:?}"
                    ))
                })?;
            let checkpoint_lease = checkpoint_preview.as_ref().map(|preview| preview.lease());
            if let Some(lease) = checkpoint_lease {
                if let Some(layers) = &self.environment.source_layers {
                    layers
                        .validate_source_authority(&lease.issuer_source_layer)
                        .map_err(ResidentActorWorkbenchError::ActorProtocol)?;
                }
                lease.retained_scope().map_err(|refusal| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "checkpoint retained-scope refusal: {refusal:?}"
                    ))
                })?;
            }
            if descriptor.model().is_none() {
                let checkpoint_model = checkpoint_lease
                    .as_ref()
                    .and_then(|lease| lease.issuer_model.clone());
                descriptor = descriptor
                    .with_model(checkpoint_model.or_else(|| self.descriptor.model().cloned()));
            }
            if descriptor.fork_effort().is_none() {
                let checkpoint_effort = checkpoint_lease
                    .as_ref()
                    .and_then(|lease| lease.issuer_effort);
                descriptor = descriptor
                    .with_fork_effort(checkpoint_effort.or(self.descriptor.fork_effort()));
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
            let capabilities = self
                .descriptor
                .capabilities()
                .preview_child(descriptor.capabilities().clone(), descriptor.fork_budget())
                .map_err(ResidentActorWorkbenchError::ActorProtocol)?;
            descriptor = descriptor.with_capabilities(capabilities);
            let mut checkpoint_admission = None;
            let mut spawn_admission = None;
            if spawn.is_some() {
                descriptor =
                    descriptor.with_persistence_policy(self.descriptor.persistence_policy());
                let claimed = self
                    .environment
                    .actor_admissions
                    .claim_spawn(
                        context.actor,
                        context.placement.session,
                        descriptor.checkpoint_token(),
                        self.descriptor
                            .capabilities()
                            .descendants()
                            .maximum_active_children
                            .map(usize::from),
                    )
                    .map_err(|refusal| {
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "checkpoint refusal: {refusal:?}"
                        ))
                    })?;
                descriptor = descriptor.with_actor_path(claimed.path);
                checkpoint_admission = claimed.checkpoint;
                retained_spawn_admission = Some(claimed.authority.clone());
                spawn_admission = Some(claimed.authority);
                if let Some((lease, _)) = &checkpoint_admission {
                    descriptor = descriptor
                        .with_context_parent(lease.issuer)
                        .with_checkpoint_boundary(Some(lease.boundary.clone()));
                }
            }

            drop(checkpoint_preview);
            let retained_checkpoint_scope = checkpoint_admission
                .as_ref()
                .map(|(lease, _)| lease.retained_scope().cloned())
                .transpose()
                .map_err(|refusal| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "checkpoint retained-scope refusal: {refusal:?}"
                    ))
                })?;
            let inherited_source =
                if spawn.is_some() || descriptor.source_imports().inherited_scope()?.is_some() {
                    let source = match effect_owner.admitted_source() {
                        Some(source) => source.clone(),
                        None => self.freeze_installed_source(context.actor)?,
                    };
                    if let Some(layers) = &self.environment.source_layers {
                        layers
                            .validate_source_authority(&source)
                            .map_err(ResidentActorWorkbenchError::ActorProtocol)?;
                    }
                    Some(source)
                } else {
                    None
                };
            Ok(child_launch::ChildLaunchAdmission {
                child: crate::start::CapturedChildLaunch {
                    lifetime,
                    descriptor,
                    spawn,
                    entry,
                    launch_worktrees,
                    record_workspace,
                    seed,
                    exit_destination,
                },
                checkpoint_admission,
                spawn_admission,
                inherited_source,
                retained_checkpoint_scope,
                child_session_startup,
                invocation_work: invocation_work.clone(),
            })
        })();
        if let (Err(error), Some(authority)) = (&admission, &retained_spawn_admission) {
            authority.fail(error.to_string());
        }
        let spawn_admission = retained_spawn_admission;
        Box::new(child_launch::PreparedChildLaunch {
            continuation: child_launch::ChildLaunchContinuation {
                context: context.clone(),
                parent_descriptor: self.descriptor.clone(),
                control: effect_owner.control(),
                invocation_work,
                parent_hole,
                spawn_reply,
                spawn_admission,
                placement_custody,
                placement_startup,
            },
            admission,
        })
    }

    fn apply_child_launch(
        &mut self,
        kernel: &KernelContext,
        completed: Box<child_launch::CompletedChildLaunch>,
    ) -> child_launch::ChildLaunchResume {
        child_launch::apply_launch(kernel, &self.descriptor, completed)
    }

    async fn resolve_outbound(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        effect_owner: CurrentEffectOwner<'_>,
        ancestry: &crate::CallAncestry,
        outbound: ResidentOutbound,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        match outbound {
            ResidentOutbound::Cast {
                target,
                continuation,
                request,
            } => {
                self.enqueue_cast(kernel, context, target, request).await?;
                self.environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
            }
            ResidentOutbound::TryCast {
                target,
                continuation,
                request,
            } => {
                let result = self
                    .enqueue_cast(kernel, context, target, request)
                    .await
                    .map_err(|error| error.to_string());
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, result)
                    .await
            }
            ResidentOutbound::Call {
                target,
                continuation,
                request,
            } => {
                let reply = self
                    .perform_call(
                        kernel,
                        context,
                        effect_owner.clone(),
                        ancestry,
                        target,
                        request,
                    )
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
                    .perform_call(
                        kernel,
                        context,
                        effect_owner.clone(),
                        ancestry,
                        target,
                        request,
                    )
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

    async fn enqueue_cast(
        &self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        target: ActorRef,
        request: crate::MailboxValue,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let unavailable = || {
            ResidentActorWorkbenchError::ActorProtocol(
                KernelCallFailure::TargetUnavailable(target).to_string(),
            )
        };
        let target_ref = kernel.resolve(target).ok_or_else(&unavailable)?;
        let target_context = kernel.session_context(target).ok_or_else(&unavailable)?;
        let request = self
            .environment
            .runner
            .transfer_mailbox_value(
                context.clone(),
                request,
                target_context.placement.session,
                target_context.placement.resource_scope,
            )
            .await?;
        target_ref
            .cast(context.actor, request)
            .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))
    }

    /// Byte budget for the reply a settlement notice carries. A reply within
    /// it is shown whole, so the owner reads the child's result in the notice
    /// rather than asking for it again; only a larger reply is cut, and the
    /// cut names this budget.
    const SETTLEMENT_REPLY_PREVIEW_BYTE_BUDGET: usize = 8192;

    /// Both authored tool replies and route callbacks resume the one active
    /// request continuation, then hand it back to the ordinary actor scheduler.
    async fn stage_request_reply(
        &mut self,
        _kernel: &KernelContext,
        context: &ActorSessionContext,
        claim: crate::request::RequestReplyClaim,
        result: RootCustody,
        carried_preview: Option<String>,
        _boundary: Option<&tidepool_runtime::session::ContextCheckpointBoundary>,
        _invocation: Option<&InvocationWork>,
        effects: Option<&mut WorkbenchEffectState>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let request = claim.request();
        let settled = async {

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
                        Self::SETTLEMENT_REPLY_PREVIEW_BYTE_BUDGET,
                    )),
                ),
                _ => match self
                    .environment
                    .runner
                    .preview_retained(
                        context.clone(),
                        result,
                        Self::SETTLEMENT_REPLY_PREVIEW_BYTE_BUDGET,
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
            self.pending_program = Some(PendingActorProgram { transfer: None, outcome, cleanup: None });
            self.pending_reply = Some(claim);
            self.pending_reply_preview = reply_preview;
            if let Some(effects) = effects {
                self.record_terminal_transfer(effects, request, AcceptedTerminalKind::Reply);
            }
            let cleanup = self.environment.runner.handoff_actor_continuation(
                context.clone(), &self.pending_program.as_ref().expect("native reply pending").outcome,
            )?;
            self.pending_program.as_mut().expect("native reply pending").cleanup = cleanup;
            if let Some(suspended) = &mut self.suspended_cast {
                // The previous Interactive hole was consumed by resume_live;
                // its successor now has the pending program's exact guard.
                suspended.cleanup.take();
            }
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

    fn attach_source(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        owner: crate::ActorRef,
        source: crate::request::sources::SourceBinding,
    ) -> Result<(), String> {
        use crate::request::sources::SourceTarget;
        if owner != context.actor {
            return Err("event sink belongs to another actor incarnation".into());
        }
        if self.source_connections.is_none() {
            let recipient = kernel
                .resolve(context.actor)
                .ok_or_else(|| "current actor is absent from its directory".to_owned())?;
            self.source_connections = Some(
                self.environment
                    .requests
                    .attach_sources(context.actor, recipient, &[])
                    .map_err(|error| format!("source attachment rejected: {error:?}"))?,
            );
        }
        let slot = self.sources.len();
        let connections = self
            .source_connections
            .as_mut()
            .ok_or_else(|| "source connections are unavailable".to_owned())?;
        match source.target {
            SourceTarget::Request(request, kind) => connections
                .attach_request(slot, request, kind, context.actor)
                .map_err(|error| format!("request source refused: {error:?}"))?,
            SourceTarget::Command(key) => connections
                .attach_command(slot, key, context.actor, &self.environment.commands)
                .map_err(|error| format!("command source refused: {error:?}"))?,
            SourceTarget::Lifecycle(target) => {
                if !actor_can_observe(context.actor, target, &self.environment.actors.lock()) {
                    return Err("lifecycle source is not authorized".into());
                }
                let actor = kernel
                    .resolve(target)
                    .ok_or_else(|| "lifecycle source target is unavailable".to_owned())?;
                connections.attach_lifecycle(slot, &actor);
            }
        }
        self.sources.push(source);
        Ok(())
    }

    /// Admit actor-owned decisions before transferring the native or service wait.
    fn prepare_independent_effect(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        effect_owner: &CurrentEffectOwner<'_>,
        boundary: ResidentActorBoundary,
    ) -> Result<
        futures_util::future::BoxFuture<
            'static,
            Result<ResidentOutcome, ResidentActorWorkbenchError>,
        >,
        ResidentActorBoundary,
    > {
        let environment = self.environment.clone();
        let kernel = kernel.clone();
        let context = context.clone();
        let input_origin = self.input_origin.clone();
        let descriptor = self.descriptor.clone();
        let bound_worktree = self.launch_worktrees.first().cloned();
        let observation = self.runtime_observation.snapshot();
        let workbench = self
            .active_workbench()
            .unwrap_or_else(|| environment.runner.application_workbench());
        let control = effect_owner.control();
        let ephemeral_work = effect_owner.ephemeral_work();
        let interaction_control = effect_owner.interaction_control();
        let operation: futures_util::future::BoxFuture<
            'static,
            Result<ResidentOutcome, ResidentActorWorkbenchError>,
        > = match boundary {
            ResidentActorBoundary::Form {
                continuation,
                operation,
                publication,
            } => forms::service(
                environment,
                kernel,
                context,
                ephemeral_work,
                control,
                interaction_control,
                continuation,
                operation,
                publication,
            ),
            ResidentActorBoundary::RichView {
                continuation,
                view,
                publication,
            } => {
                let slot = environment
                    .actors
                    .lock()
                    .get(&context.actor)
                    .filter(|record| record.owns_display_resources(&context))
                    .ok_or_else(|| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            "rich display actor unavailable".into(),
                        )
                    })
                    .and_then(|record| record.displays.lock().reserve_rich_slot());
                Box::pin(async move {
                    let slot = slot?;
                    let host = environment.form_host.as_ref().ok_or_else(|| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            "rich display has no host".into(),
                        )
                    })?;
                    let publish = || host.display(&publication, slot, &view);
                    let result = match interaction_control.as_ref() {
                        Some(control) => control.admit_interaction(publish).ok_or_else(|| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "rich display cancelled before publication".into(),
                            )
                        })?,
                        None => publish(),
                    };
                    result.map_err(|cause| {
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "rich display: {cause:?}"
                        ))
                    })?;
                    environment.runner.resume_unit(context, continuation).await
                })
            }
            ResidentActorBoundary::Command {
                continuation,
                request,
            } if commands::ownership_operation(&request) => self.prepare_command_ownership(
                &kernel,
                &context,
                effect_owner,
                continuation,
                request,
            ),
            ResidentActorBoundary::ReplaceSpec {
                continuation,
                target,
                definition,
            } => {
                let authorized =
                    actor_can_control(context.actor, target, &environment.actors.lock());
                let installed = self.installed_tools.clone();
                let installation_context = self.context(context.actor);
                let allowed = descriptor.capabilities().effect_keys().to_vec();
                Box::pin(async move {
                    let result = if !authorized {
                        Err(crate::SpecReplacementError::Unauthorized)
                    } else if target == context.actor {
                        install_explicit_replacement(
                            environment.clone(),
                            installation_context,
                            installed,
                            allowed,
                            definition,
                        )
                        .await
                    } else if let Some(actor) = kernel
                        .resolve(target)
                        .filter(|actor| actor.terminal().get().is_none())
                    {
                        actor.replace_spec(definition).await
                    } else {
                        Err(crate::SpecReplacementError::Unavailable)
                    };
                    environment
                        .runner
                        .resume_spec_replacement(context, continuation, result)
                        .await
                })
            }

            ResidentActorBoundary::DisplayAllowanceGranted {
                continuation,
                allowance,
            } => Box::pin(async move {
                environment
                    .runner
                    .resume_value(context.clone(), continuation, allowance)
                    .await
            }),
            ResidentActorBoundary::DisplayPublished {
                continuation,
                identity,
            } => Box::pin(async move {
                environment
                    .runner
                    .resume_value(context.clone(), continuation, identity)
                    .await
            }),
            ResidentActorBoundary::DisplayExpanded { continuation, keys } => Box::pin(async move {
                environment
                    .runner
                    .resume_value(context.clone(), continuation, keys)
                    .await
            }),
            ResidentActorBoundary::Console { continuation, text } => Box::pin(async move {
                tracing::debug!(actor = ?context.actor, output = %crate::workbench_display::bounded_output(&text, 8192), "actor console");
                environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
            }),
            ResidentActorBoundary::ActorLocalContext(continuation) => Box::pin(async move {
                environment
                    .runner
                    .resume_value(
                        context.clone(),
                        continuation,
                        (actor_address(context.actor), input_origin),
                    )
                    .await
            }),
            ResidentActorBoundary::AttachSource {
                continuation,
                owner,
                source,
            } => {
                let result = self.attach_source(&kernel, &context, owner, source);
                Box::pin(async move {
                    environment
                        .runner
                        .resume_value(context, continuation, result)
                        .await
                })
            }
            ResidentActorBoundary::ActorContext(continuation) => Box::pin(async move {
                environment
                    .runner
                    .resume_actor_context(
                        context.clone(),
                        continuation,
                        descriptor,
                        bound_worktree,
                        observation,
                    )
                    .await
            }),
            ResidentActorBoundary::AgentInspect(inspection) => Box::pin(async move {
                let records = environment.actors.lock().clone();
                let observation = records.get(&inspection.target).and_then(|record| {
                    actor_can_observe(context.actor, inspection.target, &records).then(|| {
                        crate::resident_workbench::AgentRosterProjection {
                            received: environment.requests.received_counts(inspection.target),
                            requests: environment.requests.work_for_target(inspection.target),
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
                environment
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
                workbench
                    .resume_lookup(
                        context.clone(),
                        continuation,
                        request,
                        environment.usage_pointers.clone(),
                        control.clone(),
                    )
                    .await
            }),
            ResidentActorBoundary::Introspection {
                continuation,
                query,
                kind,
            } => Box::pin(async move {
                environment
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
                let outcome = match environment.conversation_reader.clone() {
                    Some(reader) => {
                        reader(context.actor, crate::conversation::requested(count)).await
                    }
                    None => Err(crate::ConversationUnavailable::Unbound),
                };
                environment
                    .runner
                    .resume_value(
                        context.clone(),
                        continuation,
                        crate::conversation::reflection(outcome),
                    )
                    .await
            }),
            ResidentActorBoundary::AgentList(continuation) => Box::pin(async move {
                let records = environment.actors.lock().clone();
                let mut roster = records
                    .iter()
                    .filter(|(actor, _)| actor_can_observe(context.actor, **actor, &records))
                    .map(
                        |(actor, record)| crate::resident_workbench::AgentRosterProjection {
                            received: environment.requests.received_counts(*actor),
                            requests: environment.requests.work_for_target(*actor),
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
                environment
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
                    let mut records = environment.actors.lock();
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
                environment
                    .runner
                    .resume_value(context.clone(), continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::ResponsePoll(poll) => Box::pin(async move {
                let observation = environment
                    .requests
                    .observe_response(context.actor, poll.request)
                    .map(|observation| {
                        starting_observation(&environment, poll.request, observation)
                    });
                let (observation, snapshot) = match observation {
                    Ok(crate::ResponseObservation::Ready) => match environment
                        .requests
                        .observe_response_result(context.actor, poll.request)
                    {
                        Ok(snapshot) => (Ok(crate::ResponseObservation::Ready), Some(snapshot)),
                        Err(error) => (Err(error), None),
                    },
                    observation => (observation, None),
                };
                environment
                    .runner
                    .resume_response_observation(
                        context.clone(),
                        poll.continuation,
                        observation,
                        snapshot,
                    )
                    .await
            }),
            ResidentActorBoundary::WatchResponsePoll {
                continuation,
                watch,
                path,
                node,
            } => Box::pin(async move {
                let observation = environment
                    .requests
                    .observe_watch_snapshot_response(watch, &path, node);
                environment
                    .runner
                    .resume_watch_response(context.clone(), continuation, observation)
                    .await
            }),
            ResidentActorBoundary::ProgressPoll(poll) => Box::pin(async move {
                let observation = environment
                    .requests
                    .observe_progress(context.actor, poll.request);
                environment
                    .runner
                    .resume_progress_observation(context.clone(), poll.continuation, observation)
                    .await
            }),
            ResidentActorBoundary::RequestUpdate {
                continuation,
                request,
                message,
            } => Box::pin(async move {
                let outcome = environment
                    .requests
                    .update_request(context.actor, request, message)
                    .map(|(update, delivery)| {
                        if let Err(error) = environment
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
                environment
                    .runner
                    .resume_request_update(context.clone(), continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::RequestUpdatePoll {
                continuation,
                update,
            } => Box::pin(async move {
                let outcome = environment.requests.observe_update(context.actor, update);
                environment
                    .runner
                    .resume_request_update(context.clone(), continuation, outcome)
                    .await
            }),
            ResidentActorBoundary::ReplyPoll(poll) => Box::pin(async move {
                let observation = environment
                    .requests
                    .observe_reply(context.actor, poll.request);
                environment
                    .runner
                    .resume_reply_observation(context.clone(), poll.continuation, observation)
                    .await
            }),
            ResidentActorBoundary::RouteList(continuation) => Box::pin(async move {
                let routes = environment
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
                environment
                    .runner
                    .resume_value(context.clone(), continuation, routes)
                    .await
            }),
            ResidentActorBoundary::RoutePoll(poll) => Box::pin(async move {
                let state = environment
                    .requests
                    .observe_route(context.actor, poll.watch);
                environment
                    .runner
                    .resume_route_state(context.clone(), poll.continuation, state)
                    .await
            }),
            ResidentActorBoundary::WatchPoll(poll) => Box::pin(async move {
                let observation = environment
                    .requests
                    .observe_watch(context.actor, poll.watch)
                    .map(|observation| {
                        watch_pending_observation(&environment, poll.watch, observation)
                    });
                environment
                    .runner
                    .resume_watch_observation(context.clone(), poll.continuation, observation)
                    .await
            }),
            ResidentActorBoundary::WatchProgressPoll {
                continuation,
                watch,
                path,
                request,
                after,
            } => Box::pin(async move {
                let observation = environment.requests.observe_watch_snapshot_progress(
                    context.actor,
                    watch,
                    &path,
                    request,
                    after,
                );
                environment
                    .runner
                    .resume_progress_observation(context.clone(), continuation, observation)
                    .await
            }),
            ResidentActorBoundary::WatchDecisionPoll {
                continuation,
                watch,
                path,
            } => Box::pin(async move {
                let decision = environment
                    .requests
                    .observe_watch_snapshot_decision(watch, &path)
                    .map_err(|error| {
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "retained watch decision unavailable: {error:?}"
                        ))
                    })?;
                environment
                    .runner
                    .resume_value(context.clone(), continuation, decision)
                    .await
            }),
            ResidentActorBoundary::CommandReportPoll {
                continuation,
                watch,
                path,
                job,
            } => Box::pin(async move {
                let report = environment
                    .requests
                    .observe_watch_snapshot_command(watch, &path, &job)
                    .map_err(|error| {
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "watch command snapshot unavailable: {error:?}"
                        ))
                    })?;
                environment
                    .runner
                    .resume_value(context.clone(), continuation, report)
                    .await
            }),
            ResidentActorBoundary::WatchRelease(release) => Box::pin(async move {
                let released = environment
                    .requests
                    .release_transient_watch(context.actor, release.watch);
                if released.is_ok() || matches!(&released, Err(crate::ReplyError::Stale)) {
                    if let Some(work) = ephemeral_work {
                        work.release_transient_watch_membership(release.watch);
                    }
                }
                let outcome = match released {
                    Ok(notifications) => {
                        publish_request_notifications(
                            &environment.requests,
                            &environment.deployments,
                            notifications,
                        )
                        .await;
                        Ok(())
                    }
                    Err(error) => Err(error),
                };
                environment
                    .runner
                    .resume_value(
                        context.clone(),
                        release.continuation,
                        crate::request_effect::ReplyResult(outcome),
                    )
                    .await
            }),
            ResidentActorBoundary::WatchForget(forget) => Box::pin(async move {
                let outcome = environment
                    .requests
                    .forget_watch(context.actor, forget.watch);
                environment
                    .runner
                    .resume_watch_forget(context.clone(), forget.continuation, outcome)
                    .await
            }),
            other => return Err(other),
        };
        Ok(operation)
    }

    async fn expand_display(
        &self,
        context: &ActorSessionContext,
        identity: (i64, i64, i64),
        key: i64,
        allowance: i64,
        operation: Option<WorkbenchOperationId>,
    ) -> Result<WorkbenchDisplayOutput, ResidentActorWorkbenchError> {
        expand_actor_display(
            &self.environment,
            context,
            identity,
            key,
            allowance,
            operation,
            None,
        )
        .await
    }

    async fn prepare_display_boundary(
        &self,
        context: &ActorSessionContext,
        boundary: ResidentActorBoundary,
        allowance: i64,
        operation: Option<WorkbenchOperationId>,
        receipt: Option<DisplayReceiptSubmission<'_>>,
    ) -> Result<(ResidentActorBoundary, Option<WorkbenchDisplayOutput>), ResidentActorWorkbenchError>
    {
        prepare_actor_display_boundary(
            &self.environment,
            context,
            boundary,
            allowance,
            operation,
            receipt,
        )
        .await
    }

    async fn resolve_effect(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        effect_owner: CurrentEffectOwner<'_>,
        ancestry: &crate::CallAncestry,
        boundary: ResidentActorBoundary,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let boundary = prepare_execution_effect(context, &effect_owner, boundary);
        let (boundary, display) = self
            .prepare_display_boundary(
                context,
                boundary,
                DEFAULT_DISPLAY_CHARACTER_ALLOWANCE,
                None,
                None,
            )
            .await?;
        let _ = display;
        let boundary =
            match self.prepare_independent_effect(kernel, context, &effect_owner, boundary) {
                Ok(operation) => return operation.await,
                Err(boundary) => boundary,
            };
        // Keep each interpreter branch in its own future. A child can execute
        // an effect during Ractor startup on the caller's poll stack; embedding
        // every branch here makes that ordinary nesting exhaust a debug stack.
        let operation: futures_util::future::BoxFuture<
            '_,
            Result<ResidentOutcome, ResidentActorWorkbenchError>,
        > = match boundary {
            ResidentActorBoundary::ScopeResumed { outcome } => Box::pin(async move { Ok(outcome) }),
            ResidentActorBoundary::ScopeRun {
                continuation,
                callback,
            } => Box::pin(self.run_resource_scope(
                kernel,
                context,
                effect_owner,
                ancestry,
                continuation,
                callback,
            )),
            ResidentActorBoundary::ScopeDone { .. } => Box::pin(async {
                Err(ResidentActorWorkbenchError::ActorProtocol(
                    "scope completion outside its owning delimiter".into(),
                ))
            }),
            ResidentActorBoundary::External { continuation, work } => Box::pin(async move {
                owned_workbench::await_external(
                    self.environment.clone(),
                    kernel.clone(),
                    context.clone(),
                    effect_owner.control(),
                    continuation,
                    work,
                    effect_owner.model(),
                )
                .await
            }),
            ResidentActorBoundary::Jev {
                continuation,
                request,
            } => Box::pin(async move {
                owned_workbench::ask_jev(
                    self.environment.clone(),
                    kernel.clone(),
                    context.clone(),
                    effect_owner.control(),
                    continuation,
                    request,
                )
                .await
            }),
            ResidentActorBoundary::Sleep {
                continuation,
                duration,
            } => Box::pin(async move {
                let control = effect_owner
                    .control()
                    .unwrap_or_else(crate::WorkbenchExecutionControl::untracked);
                control.arm_sleep();
                clock_wait::await_sleep(
                    self.environment.clone(),
                    kernel.clone(),
                    context.clone(),
                    control,
                    continuation,
                    duration,
                )
                .await
            }),
            ResidentActorBoundary::Command {
                continuation,
                request,
            } => Box::pin(async move {
                let started = std::time::Instant::now();
                let resolution = self
                    .resolve_command(
                        kernel,
                        context,
                        continuation,
                        request,
                        effect_owner.invocation_work().as_deref(),
                    )
                    .await;
                crate::call_timing::add_exec_ms(started.elapsed().as_millis());
                resolution.outcome
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
                let pending_displays = if authorized_terminal.is_some() {
                    self.environment
                        .actors
                        .lock()
                        .get(&forget.target)
                        .map_or(0, |record| record.displays.lock().pending_count())
                } else {
                    0
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
                } else if pending_displays > 0 {
                    crate::resident_workbench::AgentForgetProjection::OutputPending {
                        displays: pending_displays,
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
            ResidentActorBoundary::AgentRetention {
                continuation,
                target,
                lifetime,
            } => Box::pin(async move {
                let outcome =
                    self.retain_agent_resource(kernel, context, &effect_owner, target, lifetime);
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, outcome)
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
                                diagnostic: None,
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

            ResidentActorBoundary::ContextCheckpoint(ContextCheckpointBoundary::Checkpoint {
                continuation,
                name,
            }) => Box::pin(async move {
                let result: Result<String, crate::CheckpointRefusal> = async {
                    let boundary = effect_owner
                        .publication()
                        .hosted_boundary()
                        .filter(|boundary| {
                            boundary
                                .hosted()
                                .is_some_and(exomonad_tool::OriginalOperation::is_complete)
                                && !name.is_empty()
                        })
                        .cloned()
                        .ok_or(crate::CheckpointRefusal::NoHostedBoundary)?;
                    let freeze_source = || {
                        self.environment
                            .source_layers
                            .as_ref()
                            .map(|layers| layers.freeze_checkpoint_layer(context.actor.into()))
                            .transpose()
                            .map(|layer| layer.unwrap_or_default())
                    };
                    // Workbench admission already froze the revision paths.
                    // The run source owner retains immutable revisions for the
                    // run; a later reload cannot change this capture's source.
                    let admitted_source = effect_owner.admitted_source().cloned();
                    let before = admitted_source.clone().map_or_else(
                        || freeze_source().map_err(|_| crate::CheckpointRefusal::CaptureFailed),
                        Ok,
                    )?;
                    let (scope, retained_scope) = self
                        .environment
                        .runner
                        .capture_retained_context_scope(context.clone())
                        .await
                        .map_err(|_| crate::CheckpointRefusal::CaptureFailed)?;
                    let after = if admitted_source.is_some() {
                        Ok(before.clone())
                    } else {
                        freeze_source()
                    };
                    if !matches!(after, Ok(ref layer) if layer.same_revision(&before)) {
                        self.environment
                            .runner
                            .retire_context_scopes(context.clone(), vec![scope])
                            .await
                            .map_err(|_| crate::CheckpointRefusal::CaptureFailed)?;
                        return Err(crate::CheckpointRefusal::CaptureFailed);
                    }
                    let attachment = match effect_owner.publication().capture() {
                        Some(capture) => match capture.capture(&name, &boundary) {
                            Ok(attachment) => Some(attachment),
                            Err(_) => {
                                self.environment
                                    .runner
                                    .retire_context_scopes(context.clone(), vec![scope])
                                    .await
                                    .map_err(|_| crate::CheckpointRefusal::CaptureFailed)?;
                                return Err(crate::CheckpointRefusal::CaptureFailed);
                            }
                        },
                        None => None,
                    };
                    Ok(self
                        .environment
                        .actor_admissions
                        .capture_checkpoint_with_retained_scope(
                            name,
                            context.actor,
                            self.descriptor.capabilities().clone(),
                            self.descriptor.model().cloned(),
                            self.descriptor.fork_effort(),
                            before,
                            context.placement.session,
                            scope,
                            boundary,
                            attachment,
                            retained_scope,
                            self.descriptor.persistence_policy(),
                        ))
                }
                .await;
                let token = result.as_ref().ok().cloned();
                let resumed = self
                    .environment
                    .runner
                    .resume_value(context.clone(), continuation, result)
                    .await;
                if let Some(token) = token {
                    let delivered = checkpoint_capture_delivered(&resumed);
                    let retired = self
                        .environment
                        .actor_admissions
                        .settle_checkpoint(&token, context.placement.session, delivered)
                        .map_err(|error| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "checkpoint settlement failed: {error:?}"
                            ))
                        })?;
                    if let Some(scope) = retired {
                        self.environment
                            .runner
                            .retire_context_scopes(context.clone(), vec![scope])
                            .await?;
                    }
                }
                resumed
            }),
            ResidentActorBoundary::ContextCheckpoint(
                ContextCheckpointBoundary::CheckCheckpoint {
                    continuation,
                    token,
                },
            ) => Box::pin(async move {
                let result: Result<(), crate::CheckpointRefusal> = self
                    .environment
                    .actor_admissions
                    .checkpoint(&token, context.placement.session)
                    .map(|_| ());
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, result)
                    .await
            }),
            ResidentActorBoundary::ContextCheckpoint(
                ContextCheckpointBoundary::ReleaseCheckpoint {
                    continuation,
                    token,
                },
            ) => Box::pin(async move {
                let result = self
                    .environment
                    .actor_admissions
                    .release_checkpoint(&token, context.placement.session);
                let result: Result<(), crate::CheckpointRefusal> = match result {
                    Ok(Some(scope)) => {
                        let cleanup = self
                            .environment
                            .runner
                            .retire_checkpoint_scopes(context.placement.session, vec![scope])
                            .await;
                        match cleanup {
                            Ok(()) => self
                                .environment
                                .actor_admissions
                                .confirm_checkpoint_release(
                                    &token,
                                    context.placement.session,
                                    scope,
                                ),
                            Err(_) => Err(crate::CheckpointRefusal::CaptureFailed),
                        }
                    }
                    Ok(None) => Ok(()),
                    Err(refusal) => Err(refusal),
                };
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, result)
                    .await
            }),

            ResidentActorBoundary::ReplaceSpec { .. } => {
                unreachable!("spec replacement was prepared as an independent effect")
            }
            ResidentActorBoundary::Start(start) => {
                Box::pin(self.start_child(kernel, context, effect_owner, start))
            }
            ResidentActorBoundary::Outbound(outbound) => Box::pin(async move {
                self.resolve_outbound(kernel, context, effect_owner, ancestry, outbound)
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
                let actor = self.capture_drain_target(kernel, context, target)?;
                actor.drain().await.map_err(|error| {
                    ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                })?;
                self.environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
            }),
            ResidentActorBoundary::Wait(wait) => Box::pin(async move {
                let target = self.capture_exit_target(kernel, &effect_owner, wait.target)?;
                self.record_child_observation(wait.target);
                target.wait().await;
                self.environment
                    .runner
                    .resume_terminal(context.clone(), wait.continuation, target)
                    .await
            }),
            ResidentActorBoundary::Poll(poll) => Box::pin(async move {
                let terminal = kernel
                    .resolve(poll.target)
                    .map(|target| target.terminal().clone())
                    .filter(|terminal| terminal.get().is_some());
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
                    from_slot = effect_owner.after_tool_active(),
                    reason = %crate::workbench_display::bounded_output(&message, 1024),
                    "actor notification sent"
                );
                let permitted = self
                    .descriptor
                    .capabilities()
                    .effect_keys()
                    .contains(&crate::ActorEffectKey::Notifications);
                // `message` here is a decoded `String` (see
                // `resident_workbench.rs`'s `decode_address`/`message`
                // params), not a resident value -- there is nothing rooted
                // in either machine for a session-equality gate to protect,
                // so liveness is the only thing checked below.
                let destination = kernel.resolve(target);
                let outcome = if !permitted {
                    Err(crate::NotificationError::Unauthorized)
                } else if destination
                    .as_ref()
                    .is_none_or(|actor| actor.terminal().get().is_some())
                {
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
                    .capabilities()
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
            ResidentActorBoundary::CurrentRequest { continuation, site } => {
                let current = self.outstanding_interactive.as_ref().map(|active| {
                    (
                        active.request,
                        active.type_evidence.clone(),
                        active.input_scope,
                        active.input_binding,
                    )
                });
                Box::pin(async move {
                    self.environment
                        .runner
                        .resume_current_request(context.clone(), continuation, site, current)
                        .await
                })
            }
            ResidentActorBoundary::RequestAdmissionRejected {
                continuation,
                error,
            } => Box::pin(async move {
                self.environment
                    .runner
                    .resume_value(context.clone(), continuation, Err::<(), _>(error))
                    .await
            }),
            ResidentActorBoundary::RequestReservation(reservation) => Box::pin(async move {
                use crate::request_effect::RequestError;
                let owner =
                    match self.request_resource_owner(context, &effect_owner, reservation.lifetime)
                    {
                        Ok(owner) => owner,
                        Err(error) => {
                            return self
                                .environment
                                .runner
                                .resume_value(
                                    context.clone(),
                                    reservation.continuation,
                                    Err::<crate::RequestId, _>(
                                        RequestError::RequestReservationRejected(error),
                                    ),
                                )
                                .await;
                        }
                    };
                if kernel.resolve(reservation.target).is_none() {
                    return self
                        .environment
                        .runner
                        .resume_value(
                            context.clone(),
                            reservation.continuation,
                            Err::<crate::RequestId, _>(RequestError::RequestReservationRejected(
                                crate::ReplyError::Stale,
                            )),
                        )
                        .await;
                }
                let cleanup_owner = match reservation.lifetime {
                    crate::WorkerLifetime::ActorOwned => {
                        crate::request::ResourceCleanupOwner::Actor
                    }
                    crate::WorkerLifetime::RunOwned => crate::request::ResourceCleanupOwner::Run,
                    _ => owner
                        .as_ref()
                        .expect("bounded lifetime resolved an owner")
                        .resource_cleanup_owner(),
                };
                let reserve = || {
                    self.environment.requests.reserve_for_cleanup_owner(
                        context.actor,
                        reservation.target,
                        reservation.label.unwrap_or_default(),
                        reservation.notify_owner,
                        effect_owner.reservation_owner(),
                        cleanup_owner,
                    )
                };
                let admitted: Result<crate::RequestId, RequestError> = match owner {
                    Some(owner) => owner.with_admission(reserve).map_err(|_| {
                        RequestError::RequestReservationRejected(
                            crate::ReplyError::CancellationRequested,
                        )
                    }),
                    None => Ok(reserve()),
                };
                self.environment
                    .runner
                    .resume_value(context.clone(), reservation.continuation, admitted)
                    .await
            }),
            ResidentActorBoundary::RequestSubmission(submission) => Box::pin(async move {
                use crate::request_effect::RequestError;
                if let Err(error) = self
                    .environment
                    .requests
                    .request_cleanup_owner(context.actor, submission.request)
                {
                    return self
                        .environment
                        .runner
                        .resume_value(
                            context.clone(),
                            submission.continuation,
                            Err::<(), _>(RequestError::RequestSubmissionRejected(error)),
                        )
                        .await;
                }
                if let Err(error) = self.environment.requests.admit_result_destination(
                    context.actor,
                    submission.request,
                    submission.destination,
                ) {
                    return self
                        .environment
                        .runner
                        .resume_value(
                            context.clone(),
                            submission.continuation,
                            Err::<(), _>(RequestError::RequestSubmissionRejected(error)),
                        )
                        .await;
                }
                let request_deadline = submission
                    .deadline
                    .map(crate::request::ActiveRequestDeadline::start)
                    .transpose()
                    .map_err(ResidentActorWorkbenchError::ActorProtocol)?;
                let target = kernel.resolve(submission.target);
                let target_context = kernel.session_context(submission.target);
                // Session equality is no longer part of deliverability: a
                // target on another resident session still receives the
                // message, via `transfer_mailbox_value` below, rather than
                // being excluded here.
                let deliverable = target
                    .as_ref()
                    .zip(target_context.as_ref())
                    .filter(|(target, _target_context)| target.terminal().get().is_none());
                if let Some((target, target_context)) = deliverable {
                    let message = self
                        .environment
                        .runner
                        .transfer_mailbox_value(
                            context.clone(),
                            submission.message,
                            target_context.placement.session,
                            target_context.placement.resource_scope,
                        )
                        .await?;
                    if let Err(error) = self.environment.requests.mark_queued_with_deadline(
                        context.actor,
                        submission.target,
                        submission.request,
                        request_deadline.clone(),
                        effect_owner.submission_owner(),
                    ) {
                        return self
                            .environment
                            .runner
                            .resume_value(
                                context.clone(),
                                submission.continuation,
                                Err::<(), _>(RequestError::RequestSubmissionRejected(error)),
                            )
                            .await;
                    }
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
                        .resume_value(
                            context.clone(),
                            submission.continuation,
                            Ok::<(), RequestError>(()),
                        )
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
                        .resume_value(
                            context.clone(),
                            submission.continuation,
                            Err::<(), _>(RequestError::RequestSubmissionRejected(
                                crate::ReplyError::Stale,
                            )),
                        )
                        .await;
                    outcome
                }
            }),
            ResidentActorBoundary::ExitPublication {
                continuation,
                value,
            } => Box::pin(async move {
                let destination = self.exit_destination.clone().ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "typed exit publication has no admitted destination".into(),
                    )
                })?;
                if self.pending_exit.is_some() {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "actor published its exit twice".into(),
                    ));
                }
                self.pending_exit = Some(
                    self.environment
                        .runner
                        .incorporate_result(context.placement.session, value, destination)
                        .await?,
                );
                self.environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
            }),
            ResidentActorBoundary::ResponsePublication {
                continuation,
                request,
                value,
            } => Box::pin(async move {
                let destination = self
                    .pending_reply
                    .as_ref()
                    .filter(|claim| claim.request() == request)
                    .map(crate::request::RequestReplyClaim::destination)
                    .ok_or_else(|| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            "response publication does not belong to the accepted reply".into(),
                        )
                    })?;
                if self.pending_response.is_some() {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "reply wrapper published its response twice".into(),
                    ));
                }
                self.pending_response = Some(
                    self.environment
                        .runner
                        .incorporate_result(context.placement.session, value, destination)
                        .await?,
                );
                self.environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
            }),
            ResidentActorBoundary::ProgressPublication {
                continuation,
                request,
                value,
            } => Box::pin(async move {
                let published = self.environment.requests.publish_progress(
                    context.actor,
                    request,
                    value,
                    context.placement.session,
                );
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
            ResidentActorBoundary::RequestRetention {
                continuation,
                request,
                lifetime,
            } => Box::pin(async move {
                let retained =
                    self.retain_request_resource(context, &effect_owner, request, lifetime);
                self.environment
                    .runner
                    .resume_request_update(context.clone(), continuation, retained)
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
            ResidentActorBoundary::RouteRegistration {
                registration,
                entry,
            } => Box::pin(async move {
                let owner = kernel.resolve(context.actor).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol("route owner is unavailable".into())
                })?;
                let settlements = command_settlement::CommandSettlements::new(&self.environment);
                let dependencies =
                    settlements
                        .resolve(registration.dependencies)
                        .map_err(|error| {
                            ResidentActorWorkbenchError::ActorProtocol(watch_registration_refusal(
                                error,
                            ))
                        })?;
                let (watch, notifications) = self
                    .environment
                    .requests
                    .register_watch_plan_with_route(
                        context.actor,
                        registration.label,
                        dependencies.clone(),
                        Some(crate::request::routes::WatchRoute::new(owner, entry)),
                    )
                    .map_err(|error| {
                        settlements.release(&dependencies);
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
            ResidentActorBoundary::WatchRegistration(registration) => Box::pin(async move {
                let settlements = command_settlement::CommandSettlements::new(&self.environment);
                if registration.transient {
                    let registered = match settlements.resolve(registration.dependencies) {
                        Err(error) => Err(error),
                        Ok(dependencies) => {
                            let registered = self
                                .environment
                                .requests
                                .register_transient_watch(context.actor, dependencies.clone());
                            if registered.is_err() {
                                settlements.release(&dependencies);
                            }
                            registered.and_then(|watch| {
                                if let Some(invocation) = effect_owner.ephemeral_work() {
                                    if let Err(error) = invocation.register_transient_watch(watch) {
                                        let _ = self
                                            .environment
                                            .requests
                                            .release_transient_watch(context.actor, watch);
                                        return Err(error);
                                    }
                                }
                                i64::try_from(watch.0)
                                    .map_err(|_| crate::request::ReplyError::Stale)
                            })
                        }
                    };
                    return self
                        .environment
                        .runner
                        .resume_value(
                            context.clone(),
                            registration.continuation,
                            crate::request_effect::ReplyResult(registered),
                        )
                        .await;
                }
                let dependencies =
                    settlements
                        .resolve(registration.dependencies)
                        .map_err(|error| {
                            ResidentActorWorkbenchError::ActorProtocol(watch_registration_refusal(
                                error,
                            ))
                        })?;
                let (watch, notifications) = self
                    .environment
                    .requests
                    .register_watch_plan(context.actor, registration.label, dependencies.clone())
                    .map_err(|error| {
                        settlements.release(&dependencies);
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
            ResidentActorBoundary::WatchAwait(poll) => {
                let control = effect_owner
                    .control()
                    .unwrap_or_else(crate::WorkbenchExecutionControl::untracked);
                control.arm_sleep();
                Box::pin(request_wait::await_watch(
                    self.environment.clone(),
                    kernel.clone(),
                    context.clone(),
                    control,
                    poll,
                ))
            }
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
        mut prepared_installation: Option<PreparedInteractivePublication>,
    ) -> Result<KernelStep<()>, ResidentActorWorkbenchError> {
        loop {
            match self
                .environment
                .runner
                .capture_boundary(context.clone(), outcome, context.placement.resource_scope)
                .await?
            {
                ResidentActorBoundary::Completed => {
                    if self.exit_destination.is_some() {
                        let result = self.pending_exit.take().ok_or_else(|| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "typed actor completed without publishing its exit".into(),
                            )
                        })?;
                        let retained = kernel.retained_exit();
                        match retained
                            .claim_before_shutdown(|| retained.retain_result(result).is_ok())
                        {
                            Ok(true) => {}
                            Ok(false) => {
                                return Err(ResidentActorWorkbenchError::ActorProtocol(
                                    "typed exit already settled".into(),
                                ));
                            }
                            Err(terminal) => {
                                return Ok(KernelStep::Stop {
                                    output: (),
                                    terminal,
                                });
                            }
                        }
                    }
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
                    if let Some(prepared) = prepared_installation.take() {
                        self.settle_interactive_publication(kernel, context, prepared)
                            .await?;
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
                    if let Some(prepared) = prepared_installation.take() {
                        self.settle_interactive_publication(kernel, context, prepared)
                            .await?;
                    }
                    if !self.policy_installed {
                        let actor = kernel.resolve(context.actor).ok_or_else(|| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "local actor was absent from its routing directory".into(),
                            )
                        })?;
                        let local_policy = crate::resident_tools::install_local_resident_tools(
                            actor.clone(),
                            &awaiting,
                        )?;
                        let policy: Arc<dyn ResidentToolEndpoint> = Arc::new(local_policy);

                        let (checkpoint, checkpoint_attachment) = self
                            .admitted_checkpoint
                            .take()
                            .map_or((None, None), |(lease, attachment)| {
                                (Some(lease), attachment)
                            });
                        let checkpoint_attachment = checkpoint_attachment;
                        let installation = LocalResidentInstallation {
                            prepared_tools: None,
                            toolset_acquisition: None,
                            actor,
                            label: self.descriptor.display_label().into_owned(),
                            policy,
                            initial_user_message: awaiting.initial_user_message.clone(),
                            fresh_context_seed: self.fresh_context_seed.clone(),
                            spawn_admission: self.spawn_admission.clone(),
                            launch_worktrees: self.launch_worktrees.clone(),
                            worktree_custody: self.worktree_custody.clone(),
                            capabilities: self.descriptor.capabilities().clone(),
                            fork_effort: self.descriptor.fork_effort(),
                            model: self.descriptor.model().cloned(),
                            instructions: self.descriptor.instructions().map(str::to_owned),
                            creator: self.descriptor.creator(),
                            checkpoint_boundary: self.descriptor.checkpoint_boundary().cloned(),
                            checkpoint,
                            checkpoint_attachment,
                            supervisor_parent: self.descriptor.supervisor_parent(),
                            context_parent: self.descriptor.context_parent(),
                            runtime_observation: self.runtime_observation.clone(),
                        };
                        self.commit_interactive_installation(kernel, context, installation)?;
                    }
                    self.set_standing(context.actor, ResidentStanding::Tools(awaiting));
                    return Ok(KernelStep::Continue(()));
                }
                ResidentActorBoundary::AgentSession(session) => {
                    if let InteractivePark::Cancelled(request) = self
                        .park_interactive(kernel, context, session, prepared_installation.take())
                        .await?
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
                    if prepared_installation.is_some() {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "actor prepared its Codex application more than once".into(),
                        ));
                    }
                    let owner = self.ready_public_owner(context)?;
                    let bootstrap = self
                        .environment
                        .runner
                        .begin_public_bootstrap(context.clone(), Arc::clone(&owner))
                        .await?;
                    let installation = self
                        .prepare_interactive_policy(
                            kernel,
                            context,
                            attachment.initial_user_message,
                        )
                        .await?;
                    prepared_installation = Some(PreparedInteractivePublication {
                        owner,
                        bootstrap,
                        installation,
                    });
                    outcome = self
                        .environment
                        .runner
                        .resume_unit(context.clone(), attachment.continuation)
                        .await?;
                }
                boundary => {
                    outcome = self
                        .resolve_effect(
                            kernel,
                            context,
                            self.actor_effect_owner(context.actor),
                            ancestry,
                            boundary,
                        )
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
        prepared: Option<PreparedInteractivePublication>,
    ) -> Result<InteractivePark, ResidentActorWorkbenchError> {
        let (request, hole, input) = session.into_parts();
        let presentation = self
            .environment
            .requests
            .present_with_progress_type(
                context.actor,
                request.request,
                input.progress_type_witness(),
            )
            .map_err(|error| {
                ResidentActorWorkbenchError::ActorProtocol(format!(
                    "request presentation was rejected: {error:?}"
                ))
            })?;
        let already_installed = self.policy_installed;
        let (owner, bootstrap, prepared_installation) = match prepared {
            Some(prepared) => (
                prepared.owner,
                prepared.bootstrap,
                Some(prepared.installation),
            ),
            None => {
                let owner = self.ready_public_owner(context)?;
                let bootstrap = self
                    .environment
                    .runner
                    .begin_public_bootstrap(context.clone(), Arc::clone(&owner))
                    .await?;
                (owner, bootstrap, None)
            }
        };
        // Application-native bootstrap has its own completed surface. Request
        // preparation begins only after that exact durable owner is admissible.
        self.publish_application_surface(kernel, context, owner.clone(), bootstrap)
            .await?;
        if presentation.cancellation.is_some() {
            drop(hole);
            drop(input);
            return Ok(InteractivePark::Cancelled(request.request));
        }
        let private = Arc::new(
            self.environment
                .runner
                .begin_private_execution(
                    context.clone(),
                    owner.clone(),
                    tidepool_runtime::session::PublicationDecision::new(),
                )
                .await?,
        );
        let mut private_context = context.clone();
        private_context.placement.lexical_scope = private.private_scope;
        let installed_tools = prepared_installation
            .as_ref()
            .and_then(|installation| installation.prepared_tools.clone())
            .or_else(|| self.installed_tools.current());
        let activation_source = match &installed_tools {
            Some(lease) => lease.source().clone(),
            None => self.freeze_installed_source(context.actor)?,
        };
        let (compile_context, authority) = WorkbenchCompilationAuthority::admit(
            private_context.clone(),
            activation_source.clone(),
            installed_tools,
            self.environment.source_layers.as_ref(),
        )
        .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
        let workbench = self
            .environment
            .runner
            .workbench(
                request.response.clone(),
                request.request,
                request.type_evidence.clone(),
            )
            .with_compilation_authority(authority)
            .with_private_execution(private.clone());
        #[cfg(test)]
        let workbench = {
            let mut workbench = workbench;
            workbench.activation_preview_observer = self.activation_preview_observer.clone();
            workbench
        };
        // Input preparation precedes the request's invocation owner. Retain
        // its compiler transaction under the actor's initialization lifetime.
        let mounted =
            crate::resident_workbench::CompilerCloseOwner::Initialization(kernel.retained_exit())
                .scope(workbench.mount_activation_input(
                    compile_context,
                    input,
                    presentation.origin.as_ref(),
                    request.response.expected_type().to_owned(),
                    request.response.declaration.clone(),
                    request.response.declaration_modules.clone(),
                ))
                .await;
        if let Some(terminal) = kernel.requested_shutdown() {
            return Err(ResidentActorWorkbenchError::RetiredBeforeAdmission(
                terminal,
            ));
        }
        let mut input = mounted?;
        let input_binding = input.binding;
        let input_preview = std::mem::take(&mut input.input_preview);
        let reply_preview = std::mem::take(&mut input.reply_preview);
        let result = async {
            let assignment_base = status_rendering::assignment_base_from_input(&input_preview);
            let contract = crate::interactive_session::ActivationContract {
                input_type: request.input_type.clone(),
                response: request.response.clone(),
                input_preview,
                reply_preview,
                siblings: request.siblings.clone(),
            };
            let request_message =
                contract.message(request.request, request.initial_user_message.as_deref());
            let installation = match prepared_installation {
                Some(mut installation) => {
                    installation.initial_user_message =
                        Some(match installation.initial_user_message.take() {
                            Some(attachment) => format!("{attachment}\n\n{request_message}"),
                            None => request_message.clone(),
                        });
                    Some(installation)
                }
                None if !self.policy_installed => Some(
                    self.prepare_interactive_policy_from_source(
                        kernel,
                        &private_context,
                        Some(request_message.clone()),
                        activation_source,
                    )
                    .await?,
                ),
                None => None,
            };
            let request_id = request.request;
            let native = self
                .environment
                .runner
                .publish_activation_input(
                    private_context.clone(),
                    &input,
                    self.environment.requests.clone(),
                    request_id,
                    kernel.retained_exit(),
                    #[cfg(test)]
                    self.activation_preview_observer.clone(),
                )
                .await?;
            let completion = match native {
                crate::resident_workbench::ActivationInputPublication::Published(completion) => {
                    completion
                }
                crate::resident_workbench::ActivationInputPublication::Cancelled => {
                    return Ok(InteractivePark::Cancelled(request_id));
                }
                crate::resident_workbench::ActivationInputPublication::PublishedUnconfirmed {
                    detail,
                } => {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "activation native publication durability remains unconfirmed: {detail}"
                    )));
                }
                crate::resident_workbench::ActivationInputPublication::BeforeRename { detail } => {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "activation native publication did not commit: {detail}"
                    )));
                }
            };
            self.check_application_readiness(kernel, context)?;
            let publication = completion.publish_if_current(|| {
                kernel.retained_exit().claim_before_shutdown(|| {
                    if let Some(installation) = installation {
                        self.publish_interactive_installation(installation);
                    }
                    self.assignment_base = assignment_base;
                    self.outstanding_interactive = Some(OutstandingInteractive::new(
                        &request,
                        input_binding,
                        context.placement.lexical_scope,
                    ));
                    self.set_standing(
                        context.actor,
                        ResidentStanding::Interactive(
                            crate::interactive_session::ResidentInteractiveAwait { request, hole },
                        ),
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
                    true
                })
            });
            match publication {
                Ok(claim) => {
                    claim.map_err(ResidentActorWorkbenchError::RetiredBeforeAdmission)?;
                    Ok(InteractivePark::Parked)
                }
                Err(crate::ReplyError::CancellationRequested) => {
                    Ok(InteractivePark::Cancelled(request_id))
                }
                Err(error) => Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                    "request publication was rejected: {error:?}"
                ))),
            }
        }
        .await;
        // Native claim wins. A late cancellation/retirement suppresses provider
        // activation but cannot withdraw its already promoted exact input.
        result
    }

    fn freeze_installed_source(
        &self,
        actor: ActorRef,
    ) -> Result<crate::CheckpointSourceLayer, ResidentActorWorkbenchError> {
        self.environment.source_layers.as_ref().map_or_else(
            || Ok(crate::CheckpointSourceLayer::default()),
            |layers| {
                layers
                    .freeze_checkpoint_layer(tidepool_repr::PrincipalId::from(actor))
                    .map_err(ResidentActorWorkbenchError::ActorProtocol)
            },
        )
    }

    fn ready_public_owner(
        &self,
        context: &ActorSessionContext,
    ) -> Result<Arc<WorkbenchPublicOwner>, ResidentActorWorkbenchError> {
        self.environment
            .actors
            .lock()
            .get(&context.actor)
            .and_then(|record| record.public_owner.ready())
            .cloned()
            .ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    "application readiness requires its confirmed public owner".into(),
                )
            })
    }

    fn commit_interactive_installation(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        installation: LocalResidentInstallation,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.check_application_readiness(kernel, context)?;
        kernel
            .retained_exit()
            .claim_before_shutdown(|| {
                self.publish_interactive_installation(installation);
                true
            })
            .map_err(ResidentActorWorkbenchError::RetiredBeforeAdmission)?;
        Ok(())
    }

    fn publish_interactive_installation(&mut self, mut installation: LocalResidentInstallation) {
        if let Some(tools) = installation.prepared_tools.take() {
            self.installed_tools.publish(tools);
        }
        self.publish_installation(installation);
        self.policy_installed = true;
        for notice in self.deferred_child_failures.drain(..) {
            self.environment
                .deployments
                .try_send(LocalResidentDeployment::ChildExited { notice })
                .ok();
        }
    }

    async fn settle_interactive_publication(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        prepared: PreparedInteractivePublication,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.publish_application_surface(kernel, context, prepared.owner, prepared.bootstrap)
            .await?;
        self.commit_interactive_installation(kernel, context, prepared.installation)
    }

    fn check_application_readiness(
        &self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
    ) -> Result<(), ResidentActorWorkbenchError> {
        if let Some(terminal) = kernel.requested_shutdown() {
            return Err(ResidentActorWorkbenchError::RetiredBeforeAdmission(
                terminal,
            ));
        }
        let records = self.environment.actors.lock();
        let record = records.get(&context.actor).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "application readiness lost its original actor".into(),
            )
        })?;
        if record.terminal.is_some()
            || record.descriptor.placement() != context.placement
            || record
                .public_owner
                .ready()
                .is_none_or(|owner| !owner.matches_context(context))
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "application readiness lost its live confirmed public placement".into(),
            ));
        }
        Ok(())
    }

    async fn publish_application_surface(
        &self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        owner: Arc<WorkbenchPublicOwner>,
        bootstrap: Option<tidepool_runtime::session::DurablePublicBootstrap>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.check_application_readiness(kernel, context)?;
        if !self
            .ready_public_owner(context)
            .is_ok_and(|current| Arc::ptr_eq(&current, &owner))
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "bootstrap publication lost its original public owner".into(),
            ));
        }
        let commit = self
            .environment
            .runner
            .publish_public_bootstrap(context.clone(), Arc::clone(&owner), bootstrap)
            .await?;
        if let tidepool_runtime::session::PublicManifestCommit::PublishedDurabilityUnconfirmed {
            detail,
        } = commit
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "native bootstrap public surface remains durably unconfirmed: {detail}",
            )));
        }
        if commit != tidepool_runtime::session::PublicManifestCommit::Durable {
            return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "native bootstrap public surface was not published: {commit:?}",
            )));
        }
        self.check_application_readiness(kernel, context)?;
        if !self
            .ready_public_owner(context)
            .is_ok_and(|current| Arc::ptr_eq(&current, &owner))
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "bootstrap publication lost its original public owner".into(),
            ));
        }
        Ok(())
    }

    async fn prepare_interactive_policy(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        initial_user_message: Option<String>,
    ) -> Result<LocalResidentInstallation, ResidentActorWorkbenchError> {
        let source = self.freeze_installed_source(context.actor)?;
        self.prepare_interactive_policy_from_source(kernel, context, initial_user_message, source)
            .await
    }

    #[tracing::instrument(
        target = "exomonad_actor::workbench_phase",
        name = "actor_application_prepare",
        skip_all,
        fields(
            actor = %context.actor,
            actor_path = %self.descriptor.actor_path().map(ToString::to_string).unwrap_or_default(),
        )
    )]
    async fn prepare_interactive_policy_from_source(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        initial_user_message: Option<String>,
        source: crate::CheckpointSourceLayer,
    ) -> Result<LocalResidentInstallation, ResidentActorWorkbenchError> {
        let actor = kernel.resolve(context.actor).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "local actor was absent from its routing directory".into(),
            )
        })?;
        self.spec_installs = 1;
        let prepare_started = std::time::Instant::now();
        let (compile_context, authority) = WorkbenchCompilationAuthority::admit(
            context.clone(),
            source.clone(),
            None,
            self.environment.source_layers.as_ref(),
        )
        .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
        let application_workbench = Arc::new(
            self.environment
                .runner
                .application_workbench()
                .with_intrinsic_effect_support(self.environment.intrinsic_effect_support())
                .with_compilation_authority(authority),
        );
        let compiler_owner =
            crate::resident_workbench::CompilerCloseOwner::Initialization(kernel.retained_exit());
        let compiled_tools = match self.explicit_installer.as_ref() {
            Some(installer) => compiler_owner
                .scope(application_workbench.prepare_explicit_application(
                    compile_context.clone(),
                    self.spec_installs,
                    self.descriptor.capabilities().effect_keys().to_vec(),
                    Arc::clone(installer),
                ))
                .await
                .map(|(tools, receiver)| {
                    self.prepared_request_receiver = Some(receiver);
                    tools
                }),
            None => {
                compiler_owner
                    .scope(application_workbench.prepare_tools(
                        compile_context.clone(),
                        self.spec_installs,
                        self.descriptor.capabilities().effect_keys().to_vec(),
                    ))
                    .await
            }
        };
        tracing::info!(
            actor = %context.actor,
            phase = "startup",
            install = self.spec_installs,
            elapsed_ms = prepare_started.elapsed().as_millis(),
            success = compiled_tools.is_ok(),
            "agent spec preparation"
        );
        let compiled_tools = Arc::new(compiled_tools?);
        self.explicit_installer.take();
        #[cfg(test)]
        if let Some(observer) = &self.activation_preview_observer {
            observer(
                crate::resident_workbench::ActivationPublicationObservation::prepared_tools(
                    &compiled_tools,
                ),
            )?;
        }
        let declarations = compiled_tools.declarations.clone();
        let prepared_tools =
            crate::InstalledToolLease::new(context.actor, source, Some(compiled_tools));
        let policy: Arc<dyn ResidentToolEndpoint> =
            Arc::new(crate::ResidentInteractivePolicy::local_with_installation(
                actor.clone(),
                declarations,
                self.installed_tools.clone(),
            ));

        let (checkpoint, checkpoint_attachment) = self
            .admitted_checkpoint
            .take()
            .map_or((None, None), |(lease, attachment)| {
                (Some(lease), attachment)
            });
        let checkpoint_attachment = checkpoint_attachment;
        Ok(LocalResidentInstallation {
            toolset_acquisition: prepared_tools.toolset_acquisition().cloned(),
            prepared_tools: Some(prepared_tools),
            actor,
            label: self.descriptor.display_label().into_owned(),
            policy,
            initial_user_message,
            fresh_context_seed: self.fresh_context_seed.clone(),
            spawn_admission: self.spawn_admission.clone(),
            launch_worktrees: self.launch_worktrees.clone(),
            worktree_custody: self.worktree_custody.clone(),
            capabilities: self.descriptor.capabilities().clone(),
            fork_effort: self.descriptor.fork_effort(),
            model: self.descriptor.model().cloned(),
            instructions: self.descriptor.instructions().map(str::to_owned),
            creator: self.descriptor.creator(),
            checkpoint_boundary: self.descriptor.checkpoint_boundary().cloned(),
            checkpoint,
            checkpoint_attachment,
            supervisor_parent: self.descriptor.supervisor_parent(),
            context_parent: self.descriptor.context_parent(),
            runtime_observation: self.runtime_observation.clone(),
        })
    }

    async fn initialize(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        boot: ResidentBoot,
    ) -> Result<KernelStep<()>, ResidentActorWorkbenchError> {
        let workbench = self.environment.runner.application_workbench();
        let guard = workbench
            .actor_initialization_cleanup(context.clone())
            .with_compiler_owner(
                crate::resident_workbench::CompilerCloseOwner::Initialization(
                    kernel.retained_exit(),
                ),
            );
        let registration = guard.registration();
        // Keep initialization state out of the enclosing task-local scope future.
        let result = registration
            .scope(Box::pin(self.initialize_inner(kernel, context, boot)))
            .await;
        if result.is_ok() {
            workbench
                .settle_initialization_custody(context.clone(), registration)
                .await?;
            guard.disarm();
        }
        result
    }

    async fn initialize_inner(
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
        if self.worktree_custody.is_none() && !self.launch_worktrees.is_empty() {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "workspace metadata requires an authorized prepared attachment".into(),
            ));
        }

        if let Some(terminal) = kernel.requested_shutdown() {
            return Ok(KernelStep::Stop {
                output: (),
                terminal,
            });
        }
        if matches!(boot, ResidentBoot::Workbench) && self.explicit_installer.is_some() {
            let installation = self
                .prepare_interactive_policy(kernel, context, None)
                .await?;
            let receiver = self.prepared_request_receiver.take().ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    "public spawn completed tools without its request receiver".into(),
                )
            })?;
            // Receiver source custody survives replacement of the independently installed tools.
            self.request_receiver_scope = Some(receiver.scope);
            let outcome = self
                .environment
                .runner
                .run_rooted_entry(
                    context.clone(),
                    receiver.entry,
                    context.placement.resource_scope,
                )
                .await?;
            let boundary = self
                .environment
                .runner
                .capture_boundary(context.clone(), outcome, context.placement.resource_scope)
                .await?;
            let ResidentActorBoundary::Receive(receiver) = boundary else {
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "public spawn request driver did not install its mailbox receiver".into(),
                ));
            };
            self.set_standing(context.actor, ResidentStanding::Receiving(receiver));
            self.commit_interactive_installation(kernel, context, installation)?;
            return Ok(KernelStep::Continue(()));
        }
        if matches!(boot, ResidentBoot::Workbench) {
            let source = self.freeze_installed_source(context.actor)?;
            self.installed_tools.publish_source(context.actor, source);
            self.set_standing(context.actor, ResidentStanding::Workbench);
            self.policy_installed = true;
            return Ok(KernelStep::Continue(()));
        }
        let public_owner = self.ready_public_owner(context)?;
        let bootstrap = self
            .environment
            .runner
            .begin_public_bootstrap(context.clone(), Arc::clone(&public_owner))
            .await?;
        let mut installation = None;
        let outcome = match boot {
            ResidentBoot::Replacement(_) => {
                unreachable!("replacement bootstrap parks before initialization")
            }
            ResidentBoot::Workbench => unreachable!("workbench returned before native bootstrap"),
            ResidentBoot::Prepared(outcome) => *outcome,
            ResidentBoot::Startup(entry) => {
                self.environment
                    .runner
                    .run_startup_entry(context.clone(), entry)
                    .await?
            }
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
                            if installation.is_some() {
                                return Err(ResidentActorWorkbenchError::ActorProtocol(
                                    "actor prepared its Codex application more than once".into(),
                                ));
                            }
                            installation = Some(
                                self.prepare_interactive_policy(
                                    kernel,
                                    context,
                                    attachment.initial_user_message,
                                )
                                .await?,
                            );
                            outcome = self
                                .environment
                                .runner
                                .resume_unit(context.clone(), attachment.continuation)
                                .await?;
                        }
                        ResidentActorStartupStep::Ready(readiness) => {
                            self.static_source_count = self.sources.len();
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
        let prepared = if let Some(installation) = installation {
            Some(PreparedInteractivePublication {
                owner: public_owner,
                bootstrap,
                installation,
            })
        } else {
            self.publish_application_surface(kernel, context, public_owner, bootstrap)
                .await?;
            None
        };
        Box::pin(self.stabilize_program(
            kernel,
            context,
            &crate::CallAncestry::begin(context.actor),
            outcome,
            prepared,
        ))
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
        let request = self
            .environment
            .runner
            .transfer_mailbox_value(
                context.clone(),
                request,
                context.placement.session,
                context.placement.resource_scope,
            )
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
                    .resolve_effect(
                        kernel,
                        context,
                        self.actor_effect_owner(context.actor),
                        ancestry,
                        boundary,
                    )
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
                        cleanup: None,
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
                    cleanup: None,
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
                    cleanup: _cleanup,
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
                    .transfer_mailbox_value(
                        context.clone(),
                        value,
                        caller_context.placement.session,
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
            .stabilize_program(kernel, context, ancestry, program, None)
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
                    let parked = self
                        .park_interactive(kernel, context, session, None)
                        .await?;
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
                                    .stabilize_program(kernel, context, ancestry, outcome, None)
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
                        .resolve_effect(
                            kernel,
                            context,
                            self.actor_effect_owner(context.actor),
                            ancestry,
                            boundary,
                        )
                        .await?;
                }
            }
        }
    }

    async fn settle_fragment_effects(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        execution_state: &mut WorkbenchEffectState,
        workbench: &crate::ResidentActorWorkbench<H, O>,
        current: &mut WorkbenchFragmentExecution,
        unit: WorkbenchUnitExecution<'_>,
    ) -> Result<FragmentAdvance, ResidentActorWorkbenchError> {
        loop {
            if !execution_state.park_effects {
                if let Some(request) = current.native_start.take() {
                    current.native_result = Some(
                        owned_workbench::advance_fragment(
                            workbench,
                            &self.environment.runner,
                            context.clone(),
                            request,
                        )
                        .await,
                    );
                }
            }
            self.runtime_observation.publish_workbench_posture(
                crate::ActorWorkbenchPosture::RunningUnit {
                    input_unit_index: unit.input_unit_index,
                    total: unit.total,
                },
            );
            let native = match current.native_result.take() {
                Some(Ok(result)) => result,
                Some(Err(error)) => {
                    if current.scopes.is_empty() {
                        return Err(error);
                    }
                    self.prepare_scope_finish(
                        kernel,
                        context,
                        current,
                        Err(
                            tidepool_bridge_effects::ScopeFailure::ScopeEvaluationFailed(
                                error.to_string(),
                            ),
                        ),
                    )?;
                    if execution_state.park_effects {
                        return Ok(FragmentAdvance::ParkNative);
                    }
                    continue;
                }
                None => {
                    let request = match current.resume_failure.take() {
                        Some(error) => {
                            if current.scopes.is_empty() {
                                return Err(error);
                            }
                            self.prepare_scope_finish(
                                kernel,
                                context,
                                current,
                                Err(
                                    tidepool_bridge_effects::ScopeFailure::ScopeEvaluationFailed(
                                        error.to_string(),
                                    ),
                                ),
                            )?;
                            if execution_state.park_effects {
                                return Ok(FragmentAdvance::ParkNative);
                            }
                            continue;
                        }
                        None if !current.scopes.is_empty()
                            || current
                                .green
                                .as_ref()
                                .and_then(green::GreenInvocation::active_realm)
                                .is_some() =>
                        {
                            owned_workbench::WorkbenchFragmentRequest::Scoped {
                                fragment: current
                                    .fragment
                                    .take()
                                    .expect("scope retains parent fragment"),
                                outcome: current.outcome.take().expect("scope owns body frontier"),
                                realm: current
                                    .scopes
                                    .last()
                                    .map(|scope| scope.realm)
                                    .or_else(|| {
                                        current
                                            .green
                                            .as_ref()
                                            .and_then(green::GreenInvocation::active_realm)
                                    })
                                    .expect("active native resource scope"),
                            }
                        }
                        None => owned_workbench::WorkbenchFragmentRequest::Settle {
                            fragment: current
                                .fragment
                                .take()
                                .expect("running unit owns its fragment"),
                            outcome: current
                                .outcome
                                .take()
                                .expect("running unit owns its outcome"),
                        },
                    };
                    if execution_state.park_effects {
                        assert!(
                            current.native_start.is_none(),
                            "one captured native request"
                        );
                        current.native_start = Some(request);
                        if execution_state.park_effects {
                            return Ok(FragmentAdvance::ParkNative);
                        }
                        continue;
                    }
                    owned_workbench::advance_fragment(
                        workbench,
                        &self.environment.runner,
                        context.clone(),
                        request,
                    )
                    .await?
                }
            };
            match native {
                owned_workbench::WorkbenchFragmentAdvance::ScopeFailed { fragment, error } => {
                    current.fragment = Some(fragment);
                    if current.scopes.is_empty() {
                        return Err(error);
                    }
                    self.prepare_scope_finish(
                        kernel,
                        context,
                        current,
                        Err(
                            tidepool_bridge_effects::ScopeFailure::ScopeEvaluationFailed(
                                error.to_string(),
                            ),
                        ),
                    )?;
                    if execution_state.park_effects {
                        return Ok(FragmentAdvance::ParkNative);
                    }
                    continue;
                }
                owned_workbench::WorkbenchFragmentAdvance::Resumed(outcome) => {
                    current.outcome = Some(outcome);
                    continue;
                }
                owned_workbench::WorkbenchFragmentAdvance::Captured { fragment, boundary } => {
                    current.fragment = Some(fragment);
                    let effect_owner = match current
                        .scopes
                        .last()
                        .map(|frame| frame.work.clone())
                        .or_else(|| {
                            current
                                .green
                                .as_ref()
                                .and_then(green::GreenInvocation::active_work)
                        }) {
                        Some(work) => CurrentEffectOwner::Scoped {
                            base: Box::new(CurrentEffectOwner::Workbench(execution_state)),
                            scope: work,
                            wait_control: current
                                .green
                                .as_ref()
                                .and_then(green::GreenInvocation::active_wait_control),
                        },
                        None => CurrentEffectOwner::Workbench(execution_state),
                    };
                    let boundary = match boundary {
                        ResidentActorBoundary::Green(boundary) => {
                            if !execution_state.park_effects {
                                return Err(ResidentActorWorkbenchError::ActorProtocol(
                                    "async requires an independently admitted notebook execution"
                                        .into(),
                                ));
                            }
                            return self.prepare_green_boundary(
                                kernel,
                                context,
                                &effect_owner,
                                current,
                                boundary,
                            );
                        }
                        ResidentActorBoundary::Completed
                            if current
                                .green
                                .as_ref()
                                .and_then(green::GreenInvocation::active_realm)
                                .is_some()
                                && current.scopes.is_empty() =>
                        {
                            return Err(ResidentActorWorkbenchError::ActorProtocol(
                                "async body completed without publishing its result marker".into(),
                            ));
                        }
                        ResidentActorBoundary::ScopeResumed { outcome } => {
                            current.outcome = Some(outcome);
                            continue;
                        }
                        ResidentActorBoundary::ScopeRun {
                            continuation,
                            callback,
                        } => {
                            let (work, realm) = match self
                                .register_resource_scope(context, &effect_owner)
                            {
                                Ok(registered) => registered,
                                Err(detail) => {
                                    let runner = self.environment.runner.clone();
                                    let context = context.clone();
                                    current.native_start = Some(
                                        owned_workbench::WorkbenchFragmentRequest::ScopeResume {
                                            fragment: current
                                                .fragment
                                                .take()
                                                .expect("scope retains parent fragment"),
                                            operation: Box::pin(async move {
                                                runner.resume_value(context, continuation,
                                            (Err::<(), _>(tidepool_bridge_effects::ScopeFailure::ScopeRejected(detail)), Ok::<(), tidepool_bridge_effects::CleanupError>(()))).await
                                            }),
                                        },
                                    );
                                    if execution_state.park_effects {
                                        return Ok(FragmentAdvance::ParkNative);
                                    }
                                    continue;
                                }
                            };
                            let frame = scopes::ScopeFrame {
                                continuation,
                                work,
                                realm,
                            };
                            let token =
                                frame.work.scope_token().expect("registered scope identity");
                            let realm = frame.realm;
                            let work = frame.work.clone();
                            current.scopes.push(frame);
                            current.native_start =
                                Some(owned_workbench::WorkbenchFragmentRequest::ScopeStart {
                                    fragment: current
                                        .fragment
                                        .take()
                                        .expect("scope retains parent fragment"),
                                    callback,
                                    realm,
                                    token,
                                    work,
                                });
                            if execution_state.park_effects {
                                return Ok(FragmentAdvance::ParkNative);
                            }
                            continue;
                        }
                        ResidentActorBoundary::ScopeDone { token, .. }
                            if !current.scopes.is_empty() =>
                        {
                            let body = if current
                                .scopes
                                .last()
                                .and_then(|frame| frame.work.scope_token())
                                == Some(token)
                            {
                                Ok(())
                            } else {
                                Err(
                                    tidepool_bridge_effects::ScopeFailure::ScopeEvaluationFailed(
                                        "scope completion belongs to a different delimiter".into(),
                                    ),
                                )
                            };
                            self.prepare_scope_finish(kernel, context, current, body)?;
                            if execution_state.park_effects {
                                return Ok(FragmentAdvance::ParkNative);
                            }
                            continue;
                        }
                        ResidentActorBoundary::Completed if !current.scopes.is_empty() => {
                            self.prepare_scope_finish(
                                kernel,
                                context,
                                current,
                                Err(
                                    tidepool_bridge_effects::ScopeFailure::ScopeEvaluationFailed(
                                        "scope body completed without its result marker".into(),
                                    ),
                                ),
                            )?;
                            if execution_state.park_effects {
                                return Ok(FragmentAdvance::ParkNative);
                            }
                            continue;
                        }
                        boundary => boundary,
                    };
                    let next_fragment = current
                        .fragment
                        .as_mut()
                        .expect("captured effect retains its fragment");
                    let success_disposition = boundary.success_disposition();
                    let effect = boundary.operation().to_owned();
                    self.runtime_observation.publish_workbench_posture(
                        crate::ActorWorkbenchPosture::AwaitingEffect {
                            input_unit_index: unit.input_unit_index,
                            total: unit.total,
                            effect: effect.clone(),
                        },
                    );
                    let ordinal = *unit.effect_ordinal;
                    *unit.effect_ordinal += 1;
                    // Timed from here, not from `capture_boundary` above:
                    // this brackets the boundary's own service work, which
                    // is what `record_workbench_operation` reports as
                    // `elapsed_ms` once the match below settles it.
                    let effect_started = std::time::Instant::now();
                    let display_settlement = if matches!(
                        &boundary,
                        ResidentActorBoundary::DisplayPublish { .. }
                            | ResidentActorBoundary::DisplayExpand { .. }
                    ) {
                        unit.execution.map(|execution| {
                            Arc::new(DisplayOperationSettlement::new(
                                WorkbenchOperationId {
                                    execution: execution.clone(),
                                    input_unit_index: unit.input_unit_index,
                                    effect_ordinal: ordinal,
                                },
                                effect.clone(),
                                success_disposition,
                            ))
                        })
                    } else {
                        None
                    };
                    if let Some(settlement) = &display_settlement {
                        let owner =
                            execution_state
                                .display_receipt_owner
                                .as_ref()
                                .ok_or_else(|| {
                                    ResidentActorWorkbenchError::ActorProtocol(
                                        "display has no original admitted receipt owner".into(),
                                    )
                                })?;
                        owner.retain(settlement.clone())?;
                    }
                    current.inflight_effect = Some(WorkbenchEffectStamp {
                        display: None,
                        display_settlement: display_settlement.clone(),
                        success_disposition,
                        ordinal,
                        effect: effect.clone(),
                        started: effect_started,
                    });
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
                    let mut boundary = prepare_execution_effect(context, &effect_owner, boundary);
                    match &mut boundary {
                        ResidentActorBoundary::Form { publication, .. }
                        | ResidentActorBoundary::RichView { publication, .. } => {
                            publication.operation =
                                unit.execution.map(|execution| WorkbenchOperationId {
                                    execution: execution.clone(),
                                    input_unit_index: unit.input_unit_index,
                                    effect_ordinal: ordinal,
                                });
                        }
                        _ => {}
                    }
                    if execution_state.park_effects
                        && matches!(
                            &boundary,
                            ResidentActorBoundary::DisplayPublish { .. }
                                | ResidentActorBoundary::DisplayExpand { .. }
                        )
                    {
                        current.parked_effect = Some(ParkedWorkbenchEffect {
                            display: None,
                            success_disposition,
                            wait: OwnedWorkbenchWait::Display {
                                boundary,
                                allowance: display_character_allowance(*unit.display_remaining),
                                operation: unit.execution.map(|execution| WorkbenchOperationId {
                                    execution: execution.clone(),
                                    input_unit_index: unit.input_unit_index,
                                    effect_ordinal: ordinal,
                                }),
                            },
                            ordinal,
                            effect,
                            started: effect_started,
                        });
                        return Ok(FragmentAdvance::ParkEffect);
                    }
                    let (boundary, mut display) = self
                        .prepare_display_boundary(
                            context,
                            boundary,
                            display_character_allowance(*unit.display_remaining),
                            unit.execution.map(|execution| WorkbenchOperationId {
                                execution: execution.clone(),
                                input_unit_index: unit.input_unit_index,
                                effect_ordinal: ordinal,
                            }),
                            Some(DisplayReceiptSubmission {
                                owner: execution_state.display_receipt_owner.clone(),
                                settlement: display_settlement,
                                fragment: &mut *next_fragment,
                                remaining: &mut *unit.display_remaining,
                            }),
                        )
                        .await?;
                    current
                        .inflight_effect
                        .as_mut()
                        .expect("captured display retains its effect stamp")
                        .display = display.clone();
                    if let Some(display) = &mut display {
                        let rendered = display.text.clone();
                        current
                            .inflight_effect
                            .as_mut()
                            .expect("display retains its effect stamp")
                            .display = Some(display.clone());
                        if !rendered.is_empty() {
                            unit.command_output.push(rendered);
                        }
                    }
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
                    let boundary = if execution_state.park_effects {
                        let captured = match boundary {
                            ResidentActorBoundary::Start(start) => Ok(OwnedWorkbenchWait::Launch(
                                self.prepare_child_launch(context, effect_owner.clone(), start),
                            )),
                            boundary => self.prepare_invocation_wait(
                                kernel,
                                context,
                                &effect_owner,
                                boundary,
                            )?,
                        };
                        match captured {
                            Ok(wait) => {
                                if matches!(
                                    &wait,
                                    OwnedWorkbenchWait::Watch(_)
                                        | OwnedWorkbenchWait::Sleep { .. }
                                        | OwnedWorkbenchWait::Exit { .. }
                                        | OwnedWorkbenchWait::Drain { .. }
                                ) || matches!(&wait, OwnedWorkbenchWait::Command { request, .. }
                                    if commands::waits_for_completion(request)
                                        && self.descriptor.capabilities().effect_keys().contains(&crate::ActorEffectKey::Commands))
                                {
                                    execution_state
                                        .control
                                        .get_or_insert_with(
                                            crate::WorkbenchExecutionControl::untracked,
                                        )
                                        .arm_sleep();
                                }
                                current.parked_effect = Some(ParkedWorkbenchEffect {
                                    display: display.clone(),
                                    success_disposition: current
                                        .inflight_effect
                                        .as_ref()
                                        .expect("parked effect retains its boundary stamp")
                                        .success_disposition,
                                    wait,
                                    ordinal,
                                    effect,
                                    started: effect_started,
                                });
                                return Ok(FragmentAdvance::ParkEffect);
                            }
                            Err(boundary) => boundary,
                        }
                    } else {
                        boundary
                    };
                    match boundary {
                        ResidentActorBoundary::ReplyAttempt(attempt) => match self
                            .environment
                            .requests
                            .begin_reply(context.actor, attempt.request)
                        {
                            Ok(claim) => {
                                current.inflight_effect = None;
                                record_workbench_operation(
                                    unit.operations,
                                    unit.execution,
                                    unit.input_unit_index,
                                    ordinal,
                                    &effect,
                                    display.clone(),
                                    effect_started.elapsed(),
                                    WorkbenchOperationDisposition::Committed,
                                );
                                return Ok(FragmentAdvance::Settled(
                                    ResidentWorkbenchStep::Replied {
                                        claim,
                                        request: attempt.request,
                                        result: attempt.result,
                                        preview: attempt.preview,
                                    },
                                ));
                            }
                            Err(error) if attempt.recoverable => {
                                tracing::info!(rejection = ?error, "reply rejected");
                                current.inflight_effect = None;
                                record_workbench_operation(
                                    unit.operations,
                                    unit.execution,
                                    unit.input_unit_index,
                                    ordinal,
                                    &effect,
                                    display.clone(),
                                    effect_started.elapsed(),
                                    WorkbenchOperationDisposition::Rejected,
                                );
                                drop(attempt.result);
                                if execution_state.park_effects
                                    && !execution_state.after_tool_active
                                {
                                    current.native_start = Some(
                                        owned_workbench::WorkbenchFragmentRequest::ReplyRejection {
                                            continuation: attempt.continuation,
                                            error,
                                        },
                                    );
                                    return Ok(FragmentAdvance::ParkNative);
                                }
                                current.outcome = Some(
                                    self.environment
                                        .runner
                                        .resume_reply_rejection(
                                            context.clone(),
                                            attempt.continuation,
                                            error,
                                        )
                                        .await?,
                                );
                                continue;
                            }
                            Err(error) => {
                                tracing::info!(rejection = ?error, "reply rejected");
                                current.inflight_effect = None;
                                record_workbench_operation(
                                    unit.operations,
                                    unit.execution,
                                    unit.input_unit_index,
                                    ordinal,
                                    &effect,
                                    display.clone(),
                                    effect_started.elapsed(),
                                    WorkbenchOperationDisposition::Rejected,
                                );
                                drop(attempt.result);
                                return Ok(FragmentAdvance::Settled(
                                    ResidentWorkbenchStep::Rejected(
                                        settlement_refusal("reply", attempt.request, error).into(),
                                    ),
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
                                    current.inflight_effect = None;
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        display.clone(),
                                        effect_started.elapsed(),
                                        WorkbenchOperationDisposition::Committed,
                                    );
                                    return Ok(FragmentAdvance::Settled(
                                        ResidentWorkbenchStep::CancellationAcknowledged {
                                            request: acknowledgement.request,
                                        },
                                    ));
                                }
                                Err(error) if acknowledgement.recoverable => {
                                    current.inflight_effect = None;
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        display.clone(),
                                        effect_started.elapsed(),
                                        WorkbenchOperationDisposition::Rejected,
                                    );
                                    if execution_state.park_effects
                                        && !execution_state.after_tool_active
                                    {
                                        current.native_start = Some(
                                            owned_workbench::WorkbenchFragmentRequest::ReplyRejection {
                                                continuation: acknowledgement.continuation,
                                                error,
                                            },
                                        );
                                        return Ok(FragmentAdvance::ParkNative);
                                    }
                                    current.outcome = Some(
                                        self.environment
                                            .runner
                                            .resume_reply_rejection(
                                                context.clone(),
                                                acknowledgement.continuation,
                                                error,
                                            )
                                            .await?,
                                    );
                                    continue;
                                }
                                Err(error) => {
                                    current.inflight_effect = None;
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        display.clone(),
                                        effect_started.elapsed(),
                                        WorkbenchOperationDisposition::Rejected,
                                    );
                                    return Ok(FragmentAdvance::Settled(
                                        ResidentWorkbenchStep::Rejected(
                                            settlement_refusal(
                                                "cancellation acknowledgement",
                                                acknowledgement.request,
                                                error,
                                            )
                                            .into(),
                                        ),
                                    ));
                                }
                            }
                        }
                        boundary => {
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
                            if let Some((job, presentation)) = presentation {
                                let prepared = command_presentation::prepare(
                                    &self.environment.commands,
                                    context,
                                    workbench,
                                    command_presentation::CommandPresentationRequest {
                                        job,
                                        presentation,
                                        summarize: !unit.named_tool
                                            && next_fragment.summarizes_bound_commands(),
                                        named_tool: unit.named_tool,
                                        display_remaining: *unit.display_remaining,
                                    },
                                    self.descriptor
                                        .capabilities()
                                        .effect_keys()
                                        .contains(&crate::ActorEffectKey::Commands),
                                )
                                .await?;
                                prepared.apply(
                                    &self.environment.commands,
                                    context.actor,
                                    next_fragment,
                                    unit.display_remaining,
                                    unit.command_output,
                                    unit.recovered_bindings,
                                )?;
                            }
                            let (resolved, command_disposition) = match boundary {
                                ResidentActorBoundary::Command {
                                    continuation,
                                    request,
                                } => {
                                    let exec_started = std::time::Instant::now();
                                    let resolved = self
                                        .resolve_command(
                                            kernel,
                                            context,
                                            continuation,
                                            request,
                                            Some(execution_state.invocation_work.as_ref()),
                                        )
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
                                        effect_owner.clone(),
                                        &crate::CallAncestry::begin(context.actor),
                                        boundary,
                                    )
                                    .await,
                                    None,
                                ),
                            };
                            current.outcome = Some(match resolved {
                                Ok(outcome) => {
                                    current.inflight_effect = None;
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        display.clone(),
                                        effect_started.elapsed(),
                                        scoped_operation_disposition(
                                            success_disposition,
                                            command_disposition.unwrap_or(
                                                WorkbenchOperationDisposition::Committed,
                                            ),
                                        ),
                                    );
                                    outcome
                                }
                                Err(error) => {
                                    current.inflight_effect = None;
                                    record_workbench_operation(
                                        unit.operations,
                                        unit.execution,
                                        unit.input_unit_index,
                                        ordinal,
                                        &effect,
                                        display.clone(),
                                        effect_started.elapsed(),
                                        scoped_operation_disposition(
                                            success_disposition,
                                            command_disposition.unwrap_or_else(|| {
                                                disposition_for_non_command_failure(&error)
                                            }),
                                        ),
                                    );
                                    if current.scopes.is_empty() {
                                        return Err(error);
                                    }
                                    self.prepare_scope_finish(kernel, context, current, Err(tidepool_bridge_effects::ScopeFailure::ScopeEvaluationFailed(error.to_string())))?;
                                    if execution_state.park_effects {
                                        return Ok(FragmentAdvance::ParkNative);
                                    }
                                    continue;
                                }
                            });
                        }
                    }
                }
                owned_workbench::WorkbenchFragmentAdvance::Settled(step) => {
                    return Ok(FragmentAdvance::Settled(step));
                }
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
        execution_state: &mut WorkbenchEffectState,
        workbench: &crate::ResidentActorWorkbench<H, O>,
        tools: &crate::resident_workbench::ResidentWorkbenchTools,
        call: &tidepool_runtime::session::workbench::WorkbenchToolCall,
        output: String,
        value: Option<serde_json::Value>,
    ) -> String {
        use crate::after_tool::{Annotation, Disposition, Invocation};

        if !tools
            .slots
            .iter()
            .any(|slot| slot == crate::after_tool::AFTER_TOOL_SLOT)
        {
            return output;
        }
        // A slot's own effects and tool use never trigger a slot.
        if execution_state.after_tool_active {
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
            let payload = after_tool_wait::result_payload(call, &handle, ordinal, &output, value);
            let started = std::time::Instant::now();
            let wait = crate::after_tool::wait();
            let observation = self.runtime_observation.clone();
            execution_state.after_tool_active = true;
            let answer = {
                let slot = workbench.with_exact_continuation_cleanup(
                    context.clone(),
                    "after-tool slot ran out of time or lost its caller".into(),
                    self.run_after_tool(
                        kernel,
                        context,
                        execution_state,
                        workbench,
                        Arc::clone(&tools.dispatch),
                        call.name.clone(),
                        payload,
                    ),
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
            execution_state.after_tool_active = false;
            let elapsed = started.elapsed();
            tracing::Span::current().record("elapsed_ms", elapsed.as_millis() as u64);
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
                        .and_then(|binding| binding.accept().map(|_| ()))
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
        execution_state: &mut WorkbenchEffectState,
        workbench: &crate::ResidentActorWorkbench<H, O>,
        dispatch: Arc<RootCustody>,
        tool: String,
        payload: serde_json::Value,
    ) -> Result<crate::after_tool::Annotation, ResidentActorWorkbenchError> {
        let mut operations = Vec::new();
        let mut effect_ordinal = 0;
        let mut display_remaining = 16usize * 1024;
        let mut command_output = Vec::new();
        let mut recovered_bindings = Vec::new();
        let step = workbench
            .begin_after_tool(context.clone(), dispatch, tool, payload)
            .await?;
        let step = match step {
            ResidentWorkbenchStep::Running { fragment, outcome } => {
                let mut current = WorkbenchFragmentExecution::new(*fragment, *outcome);
                match self
                    .settle_fragment_effects(
                        kernel,
                        context,
                        execution_state,
                        workbench,
                        &mut current,
                        WorkbenchUnitExecution {
                            execution: None,
                            input_unit_index: 0,
                            total: 1,
                            named_tool: true,
                            operations: &mut operations,
                            effect_ordinal: &mut effect_ordinal,
                            display_remaining: &mut display_remaining,
                            command_output: &mut command_output,
                            recovered_bindings: &mut recovered_bindings,
                        },
                    )
                    .await?
                {
                    FragmentAdvance::Settled(step) => step,
                    FragmentAdvance::ParkEffect
                    | FragmentAdvance::ParkNative
                    | FragmentAdvance::ParkGreen => {
                        unreachable!("nested after-tool remains serial")
                    }
                }
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
    fn execute_workbench<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        execution_state: &'a mut WorkbenchExecutionState,
        admitted_workbench: Option<&'a crate::ResidentActorWorkbench<H, O>>,
    ) -> futures_util::future::BoxFuture<'a, Result<WorkbenchRunAdvance, WorkbenchExecutionFailure>>
    {
        let cell_span = execution_state.cell_span.clone();
        let future = async move {
            let WorkbenchExecutionState {
                effects,
                request,
                cursor,
                ..
            } = execution_state;
            let context = effects.context.clone();
            let context = &context;
            let installed_tools = effects.installed_tools.clone();
            let execution = request.execution_id().cloned();
            let status_call = request
                .tool_call()
                .filter(|call| call.name == crate::status_tool::STATUS_TOOL)
                .cloned();
            let reload_spec_call = request
                .tool_call()
                .filter(|call| call.name == crate::reload_spec_tool::RELOAD_SPEC_TOOL)
                .cloned();
            let reload_helpers_call = request
                .tool_call()
                .filter(|call| call.name == crate::reload_helpers_tool::RELOAD_HELPERS_TOOL)
                .cloned();
            if !cursor.dispatch_initialized {
                cursor.tool_dispatch = if let Some(call) = request.tool_call().filter(|call| {
                    call.name != crate::status_tool::STATUS_TOOL
                        && call.name != crate::reload_spec_tool::RELOAD_SPEC_TOOL
                        && call.name != crate::reload_helpers_tool::RELOAD_HELPERS_TOOL
                }) {
                    let tools = installed_tools
                        .as_ref()
                        .and_then(crate::InstalledToolLease::tools)
                        .filter(|tools| {
                            tools.declarations.iter().any(|tool| {
                                tool.name() == call.name
                                    && match tool {
                                        exomonad_tool::HostedTool::Custom(_) => {
                                            call.arguments.is_string()
                                        }
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
                cursor.dispatch_initialized = true;
            }
            if reload_helpers_call.is_some() || reload_spec_call.is_some() {
                return Err(workbench_failure(
                    &[],
                    0,
                    1,
                    ResidentActorWorkbenchError::ActorProtocol(
                        "reload requires its owned preparation task".into(),
                    ),
                ));
            }
            let default_workbench = if admitted_workbench.is_none() {
                Some(
                    self.active_workbench()
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
                        ),
                )
            } else {
                None
            };
            let workbench = admitted_workbench
                .or(default_workbench.as_ref())
                .expect("selected workbench");
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
                    crate::status_tool::StatusView::Changed => {
                        self.status_text(kernel, context.actor, StatusView::Concise, true)
                    }
                    crate::status_tool::StatusView::Summary => {
                        self.status_text(kernel, context.actor, StatusView::Concise, false)
                    }
                    crate::status_tool::StatusView::Revisions => {
                        self.revisions_status_text(kernel, context.actor)
                    }
                    crate::status_tool::StatusView::Detailed => {
                        self.status_text(kernel, context.actor, StatusView::Expanded, false)
                    }
                    crate::status_tool::StatusView::Lineage => {
                        self.status_text(kernel, context.actor, StatusView::Lineage, false)
                    }
                    crate::status_tool::StatusView::Trace => {
                        self.status_text(kernel, context.actor, StatusView::Trace, false)
                    }
                    crate::status_tool::StatusView::Watches => {
                        self.status_text(kernel, context.actor, StatusView::Watches, false)
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
                return Ok(WorkbenchRunAdvance::Complete(KernelStep::Continue(
                    workbench_response(
                        WorkbenchRunStatus::Committed,
                        vec![WorkbenchItemReceipt {
                            diagnostics: Vec::new(),
                            index: 0,
                            kind: None,
                            span: None,
                            source_items: Vec::new(),
                            status: WorkbenchItemStatus::Committed,
                            output,
                            value: None,
                            warnings: Vec::new(),
                            installed_bindings: Vec::new(),
                            operations: Vec::new(),
                            terminal_transfer: None,
                            failure_layer: None,
                        }],
                        1,
                        1,
                        None,
                    ),
                )));
            }
            if !cursor.preparation_done {
                if let Some(cell_source) = request.cell_source() {
                    let prepared = workbench
                        .prepare_cell(context.clone(), cell_source.to_owned())
                        .await;
                    if let Some(step) = install_cell_preparation(request, cursor, prepared)? {
                        return Ok(WorkbenchRunAdvance::Complete(step));
                    }
                } else {
                    cursor.preparation_done = true;
                }
            }
            while cursor.index < request.items.len() {
                let source = request.items[cursor.index].clone();
                if let Some(mut receipt) = cursor.completed.take() {
                    let command_prefix = cursor.unit.command_output.join("\n");
                    if let Some(checked) = &cursor.cell_check {
                        let spent = if checked.items[cursor.index].verdict.kind
                            == tidepool_runtime::session::TurnKind::Expr
                        {
                            receipt.output.chars().count()
                        } else {
                            command_prefix.chars().count()
                        };
                        cursor.cell_display_remaining =
                            cursor.cell_display_remaining.saturating_sub(spent);
                    }

                    receipt.operations = std::mem::take(&mut cursor.unit.operations);
                    cursor.receipts.push(receipt);
                    cursor.index += 1;
                    continue;
                }
                if cursor.after_tool.is_some() {
                    let prepared = cursor
                        .after_tool
                        .as_mut()
                        .expect("same after-tool frame")
                        .prepared
                        .take();
                    let mut settled = match prepared {
                        Some(Ok(ResidentWorkbenchStep::Running { fragment, outcome })) => {
                            cursor.running =
                                Some(WorkbenchFragmentExecution::new(*fragment, *outcome));
                            None
                        }
                        Some(step) => Some(step),
                        None => None,
                    };
                    if cursor.running.is_some() {
                        match self
                            .settle_fragment_effects(
                                kernel,
                                context,
                                effects,
                                workbench,
                                cursor
                                    .running
                                    .as_mut()
                                    .expect("after-tool retains original fragment cursor"),
                                WorkbenchUnitExecution {
                                    execution: execution.as_ref(),
                                    input_unit_index: cursor.index,
                                    total: request.items.len(),
                                    named_tool: true,
                                    operations: &mut cursor.unit.operations,
                                    effect_ordinal: &mut cursor.unit.effect_ordinal,
                                    display_remaining: &mut cursor.unit.display_remaining,
                                    command_output: &mut cursor.unit.command_output,
                                    recovered_bindings: &mut cursor.unit.recovered_bindings,
                                },
                            )
                            .await
                        {
                            Ok(FragmentAdvance::ParkEffect) => {
                                return Ok(WorkbenchRunAdvance::ParkEffect);
                            }
                            Ok(FragmentAdvance::ParkNative) => {
                                return Ok(WorkbenchRunAdvance::ParkNative);
                            }
                            Ok(FragmentAdvance::ParkGreen) => {
                                return Ok(WorkbenchRunAdvance::ParkGreen);
                            }
                            Ok(FragmentAdvance::Settled(step)) => settled = Some(Ok(step)),
                            Err(error) => settled = Some(Err(error)),
                        }
                        cursor.running = None;
                    }
                    let answer = match settled.expect("after-tool settles or yields one owned task")
                    {
                        Ok(ResidentWorkbenchStep::Committed { output, .. }) => {
                            crate::after_tool::Annotation::decode(&output)
                                .map_err(ResidentActorWorkbenchError::ActorProtocol)
                        }
                        Ok(ResidentWorkbenchStep::Rejected(rejection)) => {
                            Err(ResidentActorWorkbenchError::ActorProtocol(rejection.output))
                        }
                        Ok(_) => Err(ResidentActorWorkbenchError::ActorProtocol(
                            "the after-tool slot ended in a transfer instead of an annotation"
                                .into(),
                        )),
                        Err(error) => Err(error),
                    };
                    let after_tool = cursor
                        .after_tool
                        .as_mut()
                        .expect("same completed slot frame");
                    after_tool.answer = Some(WorkbenchAfterToolAnswer::Settled(answer));
                    after_tool.enforce_deadline = false;
                    return Ok(WorkbenchRunAdvance::ParkAfterToolFinish);
                }
                let mut step = None;
                if cursor.running.is_none() {
                    let started = match cursor.started.take() {
                        Some(started) => started,
                        None => {
                            cursor.unit = WorkbenchUnitState::default();
                            // Leave room for a later stop/error receipt without hiding offered command output.
                            let display_budget = if request.tool_call().is_some() {
                                28usize * 1024
                            } else {
                                60usize * 1024
                            };
                            cursor.unit.display_remaining = display_budget.saturating_sub(
                                cursor
                                    .receipts
                                    .iter()
                                    .map(|item| item.output.len() + 1)
                                    .sum::<usize>(),
                            );
                            if request.tool_call().is_none() {
                                cursor.unit.display_remaining = cursor
                                    .unit
                                    .display_remaining
                                    .min(cursor.cell_display_remaining);
                            }
                            let block = ParsedBlock {
                                ordinal: cursor.index + 1,
                                total: request.items.len(),
                                source,
                            };
                            // The input-unit level of the span tree. Held across this
                            // iteration's two await points by instrumenting the futures
                            // themselves, never by a guard.
                            let unit_span = tracing::info_span!(
                                "unit",
                                index = cursor.index,
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
                                index = cursor.index,
                                source = %block.source,
                                "input unit source"
                            );
                            self.runtime_observation.publish_workbench_posture(
                                crate::ActorWorkbenchPosture::RunningUnit {
                                    input_unit_index: cursor.index,
                                    total: request.items.len(),
                                },
                            );
                            let request_start = if let (Some(call), Some(dispatch)) =
                                (request.tool_call(), &cursor.tool_dispatch)
                            {
                                owned_workbench::WorkbenchUnitStartRequest::Tool {
                                    dispatch: Arc::clone(dispatch),
                                    name: call.name.clone(),
                                    arguments: call.arguments.clone(),
                                }
                            } else {
                                let Some(items) = cursor.prepared_cell.as_mut() else {
                                    let source = ResidentActorWorkbenchError::CompileInfrastructure(
                                        "authored cell reached execution without compiler preparation".into(),
                                    );
                                    return Err(workbench_failure(
                                        &cursor.receipts,
                                        cursor.index,
                                        request.items.len(),
                                        source,
                                    ));
                                };
                                let prepared = items[cursor.index].take().ok_or_else(|| {
                                    workbench_failure(
                                        &cursor.receipts,
                                        cursor.index,
                                        request.items.len(),
                                        ResidentActorWorkbenchError::CompileInfrastructure(
                                            "prepared cell item was already consumed".into(),
                                        ),
                                    )
                                })?;
                                owned_workbench::WorkbenchUnitStartRequest::Prepared {
                                    block,
                                    item: prepared,
                                }
                            };
                            let start = owned_workbench::WorkbenchUnitStart {
                                request: request_start,
                                span: unit_span,
                            };
                            if effects.park_effects {
                                assert!(cursor.starting.is_none(), "one pending native unit");
                                cursor.starting = Some(start);
                                return Ok(WorkbenchRunAdvance::ParkUnit);
                            }
                            owned_workbench::begin_unit(workbench, context.clone(), start).await
                        }
                    };
                    let started = match started {
                        Ok(step) => step,
                        Err(source) => {
                            return Err(workbench_failure(
                                &cursor.receipts,
                                cursor.index,
                                request.items.len(),
                                source,
                            ));
                        }
                    };
                    match started {
                        ResidentWorkbenchStep::Running { fragment, outcome } => {
                            cursor.running =
                                Some(WorkbenchFragmentExecution::new(*fragment, *outcome));
                        }
                        settled => step = Some(settled),
                    }
                }
                if cursor.running.is_some() {
                    step = Some(
                        match self
                            .settle_fragment_effects(
                                kernel,
                                context,
                                effects,
                                &workbench,
                                cursor
                                    .running
                                    .as_mut()
                                    .expect("cell retained its running unit"),
                                WorkbenchUnitExecution {
                                    execution: execution.as_ref(),
                                    input_unit_index: cursor.index,
                                    total: request.items.len(),
                                    named_tool: request.tool_call().is_some(),
                                    operations: &mut cursor.unit.operations,
                                    effect_ordinal: &mut cursor.unit.effect_ordinal,
                                    display_remaining: &mut cursor.unit.display_remaining,
                                    command_output: &mut cursor.unit.command_output,
                                    recovered_bindings: &mut cursor.unit.recovered_bindings,
                                },
                            )
                            .await
                        {
                            Ok(FragmentAdvance::Settled(step)) => step,
                            Ok(FragmentAdvance::ParkEffect) => {
                                return Ok(WorkbenchRunAdvance::ParkEffect);
                            }
                            Ok(FragmentAdvance::ParkNative) => {
                                return Ok(WorkbenchRunAdvance::ParkNative);
                            }
                            Ok(FragmentAdvance::ParkGreen) => {
                                return Ok(WorkbenchRunAdvance::ParkGreen);
                            }
                            Err(source) => {
                                let mut failure = workbench_failure_after_unit(
                                    &cursor.receipts,
                                    cursor.index,
                                    request.items.len(),
                                    source,
                                    std::mem::take(&mut cursor.unit.operations),
                                    &cursor.unit.recovered_bindings,
                                );
                                if let Some(receipt) = failure
                                    .receipts
                                    .last_mut()
                                    .filter(|receipt| receipt.index == cursor.index)
                                {
                                    receipt.output = format!(
                                        "{}\n{}",
                                        cursor.unit.command_output.join("\n"),
                                        receipt.output
                                    );
                                }
                                return Err(failure);
                            }
                        },
                    );
                    cursor.running = None;
                }
                let step = step.expect("a unit settled or returned its owned watch");
                let command_prefix = cursor.unit.command_output.join("\n");
                match step {
                    ResidentWorkbenchStep::Committed {
                        output,
                        value,
                        warnings,
                        installed_bindings,
                    } => {
                        let mut warnings = warnings;
                        let (checked_warnings, diagnostics) = cursor
                            .cell_check
                            .as_ref()
                            .map(|checked| committed_declaration_warnings(checked, cursor.index))
                            .unwrap_or_default();
                        warnings.extend(checked_warnings);
                        let output = if request.tool_call().is_some() {
                            crate::bound_workbench_display(&output, cursor.unit.display_remaining)
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
                        // `reload_agent_spec` and `reload_helpers` are the only committed outcomes
                        // that never acquire one, so a broken slot can never
                        // block its own repair.
                        let hosted_call = request
                            .tool_call()
                            .filter(|_| cursor.tool_dispatch.is_some())
                            .cloned();
                        let cell_call = if hosted_call.is_none() && cursor.cell_check.is_some() {
                            installed_tools
                                .as_ref()
                                .and_then(crate::InstalledToolLease::tools)
                                .map(
                                    |_| tidepool_runtime::session::workbench::WorkbenchToolCall {
                                        name: crate::HASKELL_TOOL.to_string(),
                                        arguments: serde_json::Value::String(
                                            request.items[cursor.index].clone(),
                                        ),
                                    },
                                )
                        } else {
                            None
                        };
                        let call = hosted_call.or(cell_call);
                        let mut receipt = WorkbenchItemReceipt {
                            diagnostics,
                            index: cursor.index,
                            kind: None,
                            span: None,
                            source_items: Vec::new(),
                            status: WorkbenchItemStatus::Committed,
                            output,
                            value: value.clone(),
                            warnings,
                            installed_bindings,
                            operations: Vec::new(),
                            terminal_transfer: None,
                            failure_layer: None,
                        };
                        if let Some(call) = call {
                            if let Some(handler) = installed_tools
                                .as_ref()
                                .and_then(crate::InstalledToolLease::handler)
                                .filter(|handler| {
                                    handler
                                        .tools()
                                        .slots
                                        .iter()
                                        .any(|slot| slot == crate::after_tool::AFTER_TOOL_SLOT)
                                })
                            {
                                if effects.park_effects {
                                    let frame = after_tool_wait::capture(
                                        handler,
                                        call,
                                        std::mem::take(&mut receipt.output),
                                        receipt.value.clone(),
                                        self.after_tool.begin(),
                                        cursor.unit.display_remaining,
                                    );
                                    let span = tracing::info_span!("after_tool_slot",
                                        slot = %crate::after_tool::AFTER_TOOL_SLOT,
                                        tool = %frame.call().name,
                                        actor = %context.actor,
                                        revision = %frame.revision(), ordinal = frame.ordinal(),
                                    );
                                    cursor.after_tool = Some(WorkbenchAfterToolExecution {
                                        frame,
                                        receipt,
                                        prepared: None,
                                        answer: None,
                                        enforce_deadline: true,
                                        span,
                                    });
                                    effects.after_tool_active = true;
                                    return Ok(WorkbenchRunAdvance::ParkAfterToolStart);
                                }
                                receipt.output = self
                                    .annotate_tool_result(
                                        kernel,
                                        context,
                                        effects,
                                        workbench,
                                        handler.tools(),
                                        &call,
                                        receipt.output,
                                        receipt.value.clone(),
                                    )
                                    .await;
                            }
                        }
                        cursor.completed = Some(receipt);
                        continue;
                    }
                    ResidentWorkbenchStep::Rejected(rejection) => {
                        let tidepool_runtime::session::CompileRejection {
                            output,
                            diagnostics,
                        } = rejection;
                        let output = if request.tool_call().is_some() {
                            crate::bound_workbench_display(&output, cursor.unit.display_remaining)
                        } else {
                            output
                        };
                        let output = if command_prefix.is_empty() {
                            output
                        } else {
                            format!("{command_prefix}\n{output}")
                        };
                        settle_prepared_operations(
                            &mut cursor.unit.operations,
                            WorkbenchOperationDisposition::Rejected,
                        );

                        let mut failure_receipt = WorkbenchItemReceipt {
                            diagnostics,
                            index: cursor.index,
                            kind: None,
                            span: None,
                            source_items: Vec::new(),
                            status: if cursor.unit.recovered_bindings.is_empty() {
                                WorkbenchItemStatus::Rejected
                            } else {
                                WorkbenchItemStatus::Stopped
                            },
                            output,
                            value: None,
                            warnings: Vec::new(),
                            installed_bindings: Vec::new(),
                            operations: std::mem::take(&mut cursor.unit.operations),
                            terminal_transfer: None,
                            failure_layer: Some(if cursor.unit.recovered_bindings.is_empty() {
                                WorkbenchFailureLayer::Compile
                            } else {
                                WorkbenchFailureLayer::Effect
                            }),
                        };
                        merge_retained_bindings(
                            &mut failure_receipt,
                            &cursor.unit.recovered_bindings,
                        );
                        cursor.receipts.push(failure_receipt);
                        return Ok(WorkbenchRunAdvance::Complete(KernelStep::Continue(
                            workbench_response(
                                WorkbenchRunStatus::Rejected,
                                std::mem::take(&mut cursor.receipts),
                                cursor.index,
                                request.items.len(),
                                cursor
                                    .cell_check
                                    .as_ref()
                                    .map(|checked| checked.items.as_slice()),
                            ),
                        )));
                    }
                    ResidentWorkbenchStep::Replied {
                        claim,
                        request: _,
                        result,
                        preview,
                    } => {
                        let publication_boundary = effects.publication.boundary().cloned();
                        let invocation_work = effects.invocation_work.clone();
                        self.stage_request_reply(
                            kernel,
                            context,
                            claim,
                            result,
                            preview,
                            publication_boundary.as_ref(),
                            Some(invocation_work.as_ref()),
                            Some(&mut *effects),
                        )
                        .await
                        .map_err(|error| {
                            workbench_failure_after_unit(
                                &cursor.receipts,
                                cursor.index,
                                request.items.len(),
                                error,
                                cursor.unit.operations.clone(),
                                &cursor.unit.recovered_bindings,
                            )
                        })?;
                        let mut receipt = WorkbenchItemReceipt {
                            diagnostics: Vec::new(),
                            index: cursor.index,
                            kind: None,
                            span: None,
                            source_items: Vec::new(),
                            status: WorkbenchItemStatus::Committed,
                            output: "Reply submitted.".to_owned(),
                            value: None,
                            warnings: Vec::new(),
                            installed_bindings: Vec::new(),
                            operations: std::mem::take(&mut cursor.unit.operations),
                            terminal_transfer: Some(WorkbenchTerminalTransfer::ReplyAccepted),
                            failure_layer: None,
                        };
                        merge_retained_bindings(&mut receipt, &cursor.unit.recovered_bindings);
                        cursor.receipts.push(receipt);
                        return Ok(WorkbenchRunAdvance::Complete(KernelStep::ContinueLater(
                            workbench_response(
                                WorkbenchRunStatus::Replied,
                                std::mem::take(&mut cursor.receipts),
                                cursor.index + 1,
                                request.items.len(),
                                cursor
                                    .cell_check
                                    .as_ref()
                                    .map(|checked| checked.items.as_slice()),
                            ),
                        )));
                    }
                    ResidentWorkbenchStep::CancellationAcknowledged {
                        request: request_id,
                    } => {
                        if self.pending_program.is_some()
                            || self.pending_reply.is_some()
                            || self.pending_cancellation.is_some()
                        {
                            self.environment
                                .requests
                                .rollback_cancellation_acknowledgement(request_id);
                            return Err(workbench_failure_after_unit(
                                &cursor.receipts,
                                cursor.index,
                                request.items.len(),
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "actor settled a second request before resuming the first"
                                        .into(),
                                ),
                                std::mem::take(&mut cursor.unit.operations),
                                &cursor.unit.recovered_bindings,
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
                                    return Err(workbench_failure_after_unit(
                                        &cursor.receipts,
                                        cursor.index,
                                        request.items.len(),
                                        ResidentActorWorkbenchError::ActorProtocol(
                                            "cancellation did not match the active request".into(),
                                        ),
                                        std::mem::take(&mut cursor.unit.operations),
                                        &cursor.unit.recovered_bindings,
                                    ));
                                }
                                standing => {
                                    self.standing = standing;
                                    self.environment
                                        .requests
                                        .rollback_cancellation_acknowledgement(request_id);
                                    return Err(workbench_failure_after_unit(
                                        &cursor.receipts,
                                        cursor.index,
                                        request.items.len(),
                                        ResidentActorWorkbenchError::ActorProtocol(
                                            "cancellation lost its active request".into(),
                                        ),
                                        std::mem::take(&mut cursor.unit.operations),
                                        &cursor.unit.recovered_bindings,
                                    ));
                                }
                            };
                        let Some(suspended) = self.suspended_cast.take() else {
                            self.standing = ResidentStanding::Interactive(awaiting);
                            self.environment
                                .requests
                                .rollback_cancellation_acknowledgement(request_id);
                            return Err(workbench_failure_after_unit(
                                &cursor.receipts,
                                cursor.index,
                                request.items.len(),
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "cancellation lost its mailbox continuation".into(),
                                ),
                                std::mem::take(&mut cursor.unit.operations),
                                &cursor.unit.recovered_bindings,
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
                                return Err(workbench_failure_after_unit(
                                    &cursor.receipts,
                                    cursor.index,
                                    request.items.len(),
                                    error,
                                    std::mem::take(&mut cursor.unit.operations),
                                    &cursor.unit.recovered_bindings,
                                ));
                            }
                        };
                        drop(awaiting);
                        // The cancellation landed: this request no longer owes
                        // `respond` bindings.
                        self.outstanding_interactive = None;
                        self.pending_program = Some(PendingActorProgram {
                            transfer: None,
                            outcome,
                            cleanup: None,
                        });
                        self.pending_cancellation = Some(request_id);
                        self.record_terminal_transfer(
                            effects,
                            request_id,
                            AcceptedTerminalKind::Cancellation,
                        );
                        let cleanup = self
                            .environment
                            .runner
                            .handoff_actor_continuation(
                                context.clone(),
                                &self
                                    .pending_program
                                    .as_ref()
                                    .expect("native cancellation pending")
                                    .outcome,
                            )
                            .map_err(|error| {
                                workbench_failure_after_unit(
                                    &cursor.receipts,
                                    cursor.index,
                                    request.items.len(),
                                    error,
                                    cursor.unit.operations.clone(),
                                    &cursor.unit.recovered_bindings,
                                )
                            })?;
                        self.pending_program
                            .as_mut()
                            .expect("native cancellation pending")
                            .cleanup = cleanup;
                        let mut receipt = WorkbenchItemReceipt {
                            diagnostics: Vec::new(),
                            index: cursor.index,
                            kind: None,
                            span: None,
                            source_items: Vec::new(),
                            status: WorkbenchItemStatus::Committed,
                            output: String::new(),
                            value: None,
                            warnings: Vec::new(),
                            installed_bindings: Vec::new(),
                            operations: std::mem::take(&mut cursor.unit.operations),
                            terminal_transfer: Some(
                                WorkbenchTerminalTransfer::CancellationAcknowledged,
                            ),
                            failure_layer: None,
                        };
                        merge_retained_bindings(&mut receipt, &cursor.unit.recovered_bindings);
                        cursor.receipts.push(receipt);
                        return Ok(WorkbenchRunAdvance::Complete(KernelStep::ContinueLater(
                            workbench_response(
                                WorkbenchRunStatus::RequestCancelled,
                                std::mem::take(&mut cursor.receipts),
                                cursor.index + 1,
                                request.items.len(),
                                cursor
                                    .cell_check
                                    .as_ref()
                                    .map(|checked| checked.items.as_slice()),
                            ),
                        )));
                    }
                    ResidentWorkbenchStep::Running { .. } => {
                        unreachable!("running workbench steps are settled above")
                    }
                }
            }
            Ok(WorkbenchRunAdvance::Complete(KernelStep::Continue(
                workbench_response(
                    WorkbenchRunStatus::Committed,
                    std::mem::take(&mut cursor.receipts),
                    request.items.len(),
                    request.items.len(),
                    cursor
                        .cell_check
                        .as_ref()
                        .map(|checked| checked.items.as_slice()),
                ),
            )))
        };
        Box::pin(future.instrument(cell_span))
    }

    fn begin_workbench_finalization(
        &mut self,
        execution_state: &WorkbenchExecutionState,
        kernel: &KernelContext,
        result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    ) -> WorkbenchFinalization {
        let context = execution_state.effects.context.clone();
        let checkpoint_boundary = execution_state.request.checkpoint_boundary().cloned();
        execution_state.effects.invocation_work.close();
        self.checkpoint_publication = CheckpointPublication::Resident;
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
        let context_ineligible = execution_state.effects.context_binding.is_some()
            && match &result {
                Err(_) => true,
                Ok(
                    KernelStep::Continue(response)
                    | KernelStep::ContinueLater(response)
                    | KernelStep::Stop {
                        output: response, ..
                    },
                ) => {
                    !matches!(
                        response.status,
                        WorkbenchRunStatus::Committed | WorkbenchRunStatus::Completed
                    ) || response.next_index != response.total
                }
            };
        let rejected = context_ineligible
            || match &result {
                Err(_) => true,
                Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response)) => {
                    response.status == WorkbenchRunStatus::Rejected
                }
                Ok(KernelStep::Stop { output, .. }) => {
                    output.status == WorkbenchRunStatus::Rejected
                }
            };
        let checkpoint_failed = context_ineligible
            || match &result {
                Err(_) => true,
                Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response)) => {
                    matches!(
                        response.status,
                        WorkbenchRunStatus::Rejected | WorkbenchRunStatus::RequestCancelled
                    )
                }
                Ok(KernelStep::Stop { output, .. }) => {
                    matches!(
                        output.status,
                        WorkbenchRunStatus::Rejected | WorkbenchRunStatus::RequestCancelled
                    )
                }
            };
        let retire_scopes = if checkpoint_failed {
            checkpoint_boundary.as_ref().map(|boundary| {
                self.environment
                    .actor_admissions
                    .settle_checkpoints(context.actor, boundary, false)
                    .into_iter()
                    .filter_map(|(session, scope)| {
                        (session == context.placement.session).then_some(scope)
                    })
                    .collect()
            })
        } else {
            None
        };
        WorkbenchFinalization {
            context,
            reservation_owner: execution_state.effects.reservation_owner.clone(),
            invocation_work: execution_state.effects.invocation_work.clone(),
            kernel: kernel.clone(),
            result,
            rejected,
            retire_scopes,
            context_boundary: execution_state
                .effects
                .context_binding
                .as_ref()
                .and(checkpoint_boundary),
            control: execution_state.effects.control.clone(),
        }
    }

    fn record_terminal_transfer(
        &mut self,
        effects: &mut WorkbenchEffectState,
        request: crate::RequestId,
        kind: AcceptedTerminalKind,
    ) {
        let transfer = Arc::new(AcceptedTerminalTransfer {
            request,
            kind,
            owner: effects.reservation_owner.clone(),
        });
        self.pending_program
            .as_mut()
            .expect("accepted transfer owns its pending program")
            .transfer = Some(transfer.clone());
        effects.terminal_transfer = Some(transfer);
    }

    fn complete_workbench_finalization(
        &mut self,
        execution_state: &mut WorkbenchExecutionState,
        finalized: WorkbenchFinalizationResult,
    ) -> (
        Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>,
        Vec<crate::request::WatchNotification>,
    ) {
        let WorkbenchFinalizationResult {
            result,
            cleanup_confirmed,
        } = finalized;
        let mut notifications = Vec::new();
        let mut result = match (result, execution_state.effects.terminal_transfer.take()) {
            (Err(source), Some(transfer)) => {
                // Only this admitted attempt may settle its transferred program.
                if transfer.owner == execution_state.effects.reservation_owner
                    && self
                        .pending_program
                        .as_ref()
                        .and_then(|program| program.transfer.as_ref())
                        .is_some_and(|pending| Arc::ptr_eq(pending, &transfer))
                {
                    self.pending_program.take();
                    match transfer.kind {
                        AcceptedTerminalKind::Reply => {
                            if self
                                .pending_reply
                                .as_ref()
                                .is_some_and(|claim| claim.request() == transfer.request)
                            {
                                self.pending_reply.take();
                                self.pending_reply_preview.take();
                                self.pending_response.take();
                            }
                            notifications = self
                                .environment
                                .requests
                                .fail_reply_settlement(transfer.request, source.to_string());
                        }
                        AcceptedTerminalKind::Cancellation => {
                            if self.pending_cancellation == Some(transfer.request) {
                                self.pending_cancellation.take();
                            }
                            self.environment
                                .requests
                                .rollback_cancellation_acknowledgement(transfer.request);
                        }
                    }
                }
                // Native reply/cancellation consumed the previous standing;
                // an ordinary invocation error cannot leave this actor reusable.
                Err(KernelInvocationFailure::TerminalTransferFailed {
                    actor: execution_state.effects.context.actor,
                    request: transfer.request,
                    source: Box::new(source),
                })
            }
            (result, _) => result,
        };
        let execution = execution_state.request.execution_id().cloned();
        if let Some(execution) = execution.as_ref() {
            self.workbench_executions.lock().freeze_display_receipts(
                execution,
                execution_state.invocation.as_ref(),
                &mut result,
            );
            let exit = match execution_state.effects.control.as_ref() {
                Some(control) => control.finish_cell(execution.clone(), &result, cleanup_confirmed),
                None => crate::CellExit::from_reply(
                    execution.clone(),
                    &result,
                    cleanup_confirmed,
                    false,
                ),
            };
            self.workbench_executions.lock().retain_cell_terminal(
                execution,
                execution_state.invocation.as_ref(),
                exit.clone(),
            );
            if let Some(binding) = execution_state.effects.context_binding.take() {
                binding.finish(exit);
            }
        }
        if let (Some(execution), Some(request)) = (execution, execution_state.replay_request.take())
        {
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
            let cancellation = execution_state.effects.control.as_ref().map_or_else(
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
                execution_state.invocation.as_ref(),
            );
        }
        (result, notifications)
    }

    // Builtin and unconverted ingress retain their serial driver until their
    // owned-step conversion. Hosted authored executions use the sync advances.
    async fn finalize_serial_workbench_execution(
        &mut self,
        execution_state: &mut WorkbenchExecutionState,
        kernel: &KernelContext,
        result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    ) -> Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure> {
        let finalization = self.begin_workbench_finalization(execution_state, kernel, result);
        let result = settle_workbench_finalization(self.environment.clone(), finalization).await;
        let (result, notifications) = self.complete_workbench_finalization(execution_state, result);
        self.publish_watch_notifications(notifications).await;
        result
    }

    // The admitted journal retains the same scope tree used by Green waits and
    // child startup. Finalization observes its actual cleanup, including retries.
    async fn settle_provider_resources(
        &self,
        kernel: &KernelContext,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Result<(), KernelBehaviorError> {
        if boundary.hosted().is_none() {
            return Ok(());
        }
        let work = self
            .workbench_executions
            .lock()
            .provider_invocation_work(boundary)?;
        if let Some(work) = work {
            let cleanup = work.cleanup(&self.environment, kernel).await;
            if let Some(detail) = cleanup.uncertainty() {
                return Err(KernelBehaviorError::new(detail));
            }
        }
        Ok(())
    }

    async fn abort_provider_boundary(
        &mut self,
        kernel: &KernelContext,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Result<(), KernelBehaviorError> {
        if self.settled_checkpoint_boundaries.contains(&boundary) {
            return self.settle_provider_resources(kernel, &boundary).await;
        }
        self.settle_provider_resources(kernel, &boundary).await?;
        let context = self.context(kernel.identity());
        let owner = WorkbenchExecutions::boundary_abort_owner(
            &self.workbench_executions,
            &boundary,
            || true,
        )?;
        let Some(owner) = owner else {
            self.settled_checkpoint_boundaries.push(boundary);
            return Ok(());
        };
        let mut cleanup = owner.collect_cleanup(|| {
            let scopes = self
                .environment
                .actor_admissions
                .settle_checkpoints(context.actor, &boundary, false)
                .into_iter()
                .filter_map(|(session, scope)| {
                    (session == context.placement.session).then_some(scope)
                })
                .collect();
            workbench_ledger::BoundaryAbortCleanup { scopes }
        });
        self.environment
            .runner
            .retire_context_scopes(context, cleanup.scopes.clone())
            .await
            .map_err(Self::workbench_failure)?;
        cleanup.scopes.clear();
        owner.retain_cleanup(cleanup);
        self.settled_checkpoint_boundaries.push(boundary);
        Ok(())
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
                .map_err(Self::workbench_failure)
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
                .map_err(Self::workbench_failure)
        })
    }

    fn commit_replacement(
        &mut self,
        predecessor: &KernelContext,
        successor: &KernelContext,
    ) -> Result<(), KernelBehaviorError> {
        self.transfer_replacement(predecessor, successor)
            .map_err(Self::workbench_failure)
    }

    fn replacement_retired(&mut self, context: &KernelContext, terminal: &ActorTerminal) {
        self.publish_retired(context.identity(), terminal.clone());
    }

    fn activate_replacement<'a>(
        &'a mut self,
        _kernel: &'a KernelContext,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move { self.activate_successor().map_err(Self::workbench_failure) })
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
                    diagnostic: None,
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
                .map_err(Self::workbench_failure)?;
            let (_, step) = self
                .run_receiver(
                    kernel,
                    &context,
                    None,
                    &crate::CallAncestry::begin(context.actor),
                    message,
                )
                .await
                .map_err(Self::workbench_failure)?;
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
                diagnostic: None,
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
                    diagnostic: None,
                });
            };
            let context = self.context(kernel.identity());
            let outcome = self
                .environment
                .runner
                .resume_value(context.clone(), receiver.continuation, None::<()>)
                .await
                .map_err(Self::workbench_failure)?;
            self.stabilize_program(
                kernel,
                &context,
                &crate::CallAncestry::begin(context.actor),
                outcome,
                None,
            )
            .await
            .map_err(Self::workbench_failure)
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
        let placement_transfer = self
            .child_placement_custody
            .as_ref()
            .map(|custody| {
                custody.transfer_to_actor(kernel.identity(), self.descriptor.placement())
            })
            .transpose();
        if placement_transfer.is_ok() {
            if let Some(lease) = self.child_session_startup.take() {
                lease.admitted();
            }
        }
        Box::pin(async move {
            placement_transfer.map_err(Self::failure)?;
            let context = self.context(kernel.identity());
            let public_owner = match self.descriptor.persistence_policy() {
                crate::ActorPersistencePolicy::Ephemeral => ActorPublicOwnerPlane::Ephemeral(
                    WorkbenchPublicOwner::issue(&context, &self.descriptor, None)
                        .map_err(KernelInvocationFailure::into_behavior_error)?,
                ),
                crate::ActorPersistencePolicy::Durable => {
                    let owner = self
                        .descriptor
                        .actor_path()
                        .and_then(|path| {
                            tidepool_runtime::session::RecoveryPublicOwner::new(
                                path,
                                context.actor.incarnation.0,
                            )
                        })
                        .ok_or_else(|| {
                            Self::failure("durable actor requires its canonical admitted path")
                        })?;
                    ActorPublicOwnerPlane::DurablePending(owner)
                }
            };
            if let Some((intent, _)) = &self.root_startup {
                let Some(ResidentBoot::Startup(entry)) = &self.boot else {
                    return Err(Self::failure(
                        "pending root lost its original unexecuted startup entry",
                    ));
                };
                if entry.compile_input_identity() != Some(intent.bootstrap_identity.as_str()) {
                    return Err(Self::failure(
                        "startup intent differs from its original compiler-issued input identity",
                    ));
                }
            }
            if let Some(recovery) = &self.environment.recovery {
                recovery
                    .admit_with_startup(
                        context.actor,
                        &self.descriptor,
                        &self.launch_worktrees,
                        self.root_startup.as_ref().map(|(intent, _)| intent.clone()),
                    )
                    .map_err(|error| Self::failure(error.to_string()))?;
            }
            kernel.install_session_context(context.clone())?;
            self.environment.actors.lock().insert(
                context.actor,
                ResidentActorRecord {
                    root_startup: self
                        .root_startup
                        .as_ref()
                        .map(|(_, latch)| Arc::clone(latch)),
                    public_owner,
                    recovery_claimed: false,
                    workbench_executions: self.workbench_executions.clone(),
                    forest_control: self.forest_control,
                    interactive_policy_installed: false,
                    observation_roots: Default::default(),
                    descriptor: self.descriptor.clone(),
                    bound_worktree: self.launch_worktrees.first().cloned(),
                    terminal: None,
                    runtime_observation: self.runtime_observation.clone(),
                    scheduler_root: kernel.spawn_ownership().is_independent(),
                    displays: Default::default(),
                },
            );
            if let Some(admission) = &self.spawn_admission {
                admission
                    .bind(
                        self.descriptor
                            .creator()
                            .ok_or_else(|| Self::failure("spawn has no creating actor"))?,
                        context.actor,
                    )
                    .map_err(Self::failure)?;
                if let Some(layers) = &self.environment.source_layers {
                    let source = self
                        .admitted_checkpoint
                        .as_ref()
                        .map(|(lease, _)| &lease.issuer_source_layer)
                        .or(self.spawn_source.as_ref());
                    if let Some(source) = source {
                        layers
                            .bind_checkpoint_for(
                                context.actor.into(),
                                self.spawn_helper_branch.as_deref().unwrap_or_default(),
                                source,
                            )
                            .map_err(Self::failure)?;
                    } else {
                        layers.bind_for(context.actor.into(), "");
                    }
                }
            }
            if self.root_startup.is_some() {
                // The original boot stays resident and opaque until durable application binding.
                return Ok(KernelStep::Continue(()));
            }

            if self.descriptor.persistence_policy() == crate::ActorPersistencePolicy::Durable
                && (self.spawn_admission.is_some()
                    || (self.descriptor.creator().is_none()
                        && self.descriptor.supervisor_parent().is_none()
                        && self.descriptor.context_parent().is_none()
                        && matches!(self.boot.as_ref(), Some(ResidentBoot::Prepared(_)))))
            {
                let owner = {
                    let records = self.environment.actors.lock();
                    let record = records.get(&context.actor).ok_or_else(|| {
                        Self::failure("durable actor lost its original allocation")
                    })?;
                    match &record.public_owner {
                        ActorPublicOwnerPlane::DurablePending(owner) => owner.clone(),
                        _ => {
                            return Err(Self::failure("durable actor lost its pending owner"));
                        }
                    }
                };
                let (outcome, readiness) = self
                    .environment
                    .runner
                    .initialize_durable_public_owner(context.clone(), owner.clone())
                    .await
                    .map_err(Self::workbench_failure)?
                    .into_parts();
                {
                    let mut records = self.environment.actors.lock();
                    let record = records.get_mut(&context.actor).ok_or_else(|| {
                        Self::failure("durable actor allocation retired before publication")
                    })?;
                    settle_public_owner_record(record, &context, &owner, &outcome, readiness)
                        .map_err(Self::workbench_failure)?;
                }
                match outcome {
                    tidepool_runtime::session::PublicManifestCommit::Durable => {},
                    tidepool_runtime::session::PublicManifestCommit::PublishedDurabilityUnconfirmed { .. } => {
                        let readiness = self.environment.runner.confirm_initial_durable_public_owner(
                            context.clone(), owner.clone(),
                        ).await.map_err(Self::workbench_failure)?;
                        let mut records = self.environment.actors.lock();
                        let record = records.get_mut(&context.actor).ok_or_else(|| Self::failure(
                            "durable actor allocation retired before confirmation",
                        ))?;
                        settle_public_owner_record(record, &context, &owner,
                            &tidepool_runtime::session::PublicManifestCommit::Durable,
                            Some(readiness),
                        ).map_err(Self::workbench_failure)?;
                    }
                    other => return Err(Self::failure(format!(
                        "durable actor public owner was not published: {other:?}",
                    ))),
                }
            }
            if matches!(self.boot.as_ref(), Some(ResidentBoot::Prepared(_))) {
                // Admission must return before the host starts consuming deployment
                // events. Keep prepared effects in their original custody until Resume.
                return Ok(KernelStep::ContinueLater(()));
            }
            let boot = self.boot.take().ok_or_else(|| KernelBehaviorError {
                detail: "resident actor boot was consumed twice".into(),
                diagnostic: None,
            })?;
            self.initialize(kernel, &context, boot)
                .await
                .map_err(Self::workbench_failure)
        })
    }

    fn reconcile_workbench_boundary<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<crate::WorkbenchBoundaryReconciliation, KernelBehaviorError>,
    > {
        Box::pin(async move {
            if self.settled_checkpoint_boundaries.contains(&boundary) {
                return Ok(crate::WorkbenchBoundaryReconciliation::Settled);
            }
            let retained = self.workbench_executions.lock().at_boundary(&boundary);
            let reply = match retained {
                Some(WorkbenchBoundaryRecord::Terminal(reply)) => Some(reply),
                Some(WorkbenchBoundaryRecord::Unconfirmed) => {
                    return Ok(crate::WorkbenchBoundaryReconciliation::Pending);
                }
                None => None,
            };
            let context = self.context(kernel.identity());

            if let Some(reply) = reply {
                return Ok(crate::WorkbenchBoundaryReconciliation::Recovered { reply });
            }

            let retired = self.environment.actor_admissions.settle_checkpoints(
                context.actor,
                &boundary,
                false,
            );
            self.environment
                .runner
                .retire_context_scopes(
                    context.clone(),
                    retired
                        .into_iter()
                        .filter_map(|(session, scope)| {
                            (session == context.placement.session).then_some(scope)
                        })
                        .collect(),
                )
                .await
                .map_err(Self::workbench_failure)?;
            self.settled_checkpoint_boundaries.push(boundary);
            Ok(crate::WorkbenchBoundaryReconciliation::Settled)
        })
    }

    fn tool_aborted<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> futures_util::future::BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async move {
            let result = self.abort_provider_boundary(kernel, boundary.clone()).await;
            self.workbench_executions.lock().finalize_provider_boundary(
                &boundary,
                result
                    .as_ref()
                    .map(|()| crate::ProviderFinalizationKind::Aborted)
                    .map_err(ToString::to_string),
            );
            result
        })
    }

    fn tool_completed<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> futures_util::future::BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async move {
            if !self
                .workbench_executions
                .lock()
                .cell_allows_publication(&boundary)
            {
                return self.tool_aborted(kernel, boundary).await;
            }
            let result = async {
                self.settle_provider_resources(kernel, &boundary).await?;
                let context = self.context(kernel.identity());
                self.environment.actor_admissions.settle_checkpoints(
                    context.actor,
                    &boundary,
                    true,
                );
                if !self.settled_checkpoint_boundaries.contains(&boundary) {
                    self.settled_checkpoint_boundaries.push(boundary.clone());
                }
                Ok(())
            }
            .await;
            self.workbench_executions.lock().finalize_provider_boundary(
                &boundary,
                result
                    .as_ref()
                    .map(|()| crate::ProviderFinalizationKind::Completed)
                    .map_err(ToString::to_string),
            );
            result
        })
    }

    fn cast<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        sender: ActorRef,
        request: MailboxValue,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            self.checkpoint_publication = CheckpointPublication::Resident;
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
                .map_err(Self::workbench_failure)?;
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
            self.checkpoint_publication = CheckpointPublication::Resident;
            let context = self.context(kernel.identity());
            self.input_origin = ActorInputOrigin::ActorMessageFrom(actor_address(caller));
            let (reply, step) = self
                .run_receiver(kernel, &context, Some(caller), &ancestry, request)
                .await
                .map_err(Self::workbench_failure)?;
            let reply = reply.ok_or_else(|| KernelBehaviorError {
                detail: "synchronous mailbox handler produced no reply".into(),
                diagnostic: None,
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

    fn dispatch_tool(
        &mut self,
        kernel: &KernelContext,
        invocation: exomonad_tool::ToolInvocation,
        capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
        control: Arc<crate::WorkbenchExecutionControl>,
    ) -> crate::OwnedActorTask<Self, serde_json::Value> {
        let actor = kernel.identity();
        let key = control.invocation.clone();
        let execution = Some(control.execution_id(actor));
        let request = execution.as_ref().map(|execution| {
            let arguments = match &invocation.arguments {
                exomonad_tool::ToolArguments::Raw(text) => serde_json::Value::String(text.clone()),
                exomonad_tool::ToolArguments::Structured(value) => value.clone(),
            };
            let boundary = invocation
                .context
                .as_ref()
                .and_then(|context| context.model_operation())
                .map(|operation| {
                    tidepool_runtime::session::ContextCheckpointBoundary::Hosted(operation.clone())
                })
                .unwrap_or_else(|| {
                    tidepool_runtime::session::ContextCheckpointBoundary::Execution {
                        actor_id: actor.id.0,
                        incarnation: actor.incarnation.0,
                        execution_id: execution.clone(),
                    }
                });
            WorkbenchRequest::for_tool(invocation.name.clone(), arguments)
                .with_execution_id(execution.clone())
                .with_checkpoint_boundary(boundary)
        });
        if let (Some(execution), Some(request)) = (&execution, &request) {
            match self
                .workbench_executions
                .lock()
                .lookup_for_provider_control(execution, request, key.as_ref(), Some(&control))
            {
                Ok(Some(reply)) => {
                    let result =
                        reply.and_then(|reply| {
                            let output = reply.items.first().ok_or_else(|| {
                                KernelInvocationFailure::Failed {
                                    receipts: Vec::new(),
                                    actor,
                                    detail: "retained tool reply has no result".into(),
                                    diagnostic: None,
                                }
                            })?;
                            serde_json::from_str(&output.output)
                                .map(KernelStep::Continue)
                                .map_err(|error| KernelInvocationFailure::Failed {
                                    receipts: Vec::new(),
                                    actor,
                                    detail: format!("retained tool result is invalid: {error}"),
                                    diagnostic: None,
                                })
                        });
                    return crate::OwnedActorTask::new(Box::pin(async move {
                        crate::OwnedActorCompletion::new(move |_| result)
                    }));
                }
                Err(failure) => {
                    let detail = match failure {
                        WorkbenchReplayFailure::DifferentInput => {
                            "one hosted tool identity was retried with different input"
                        }
                        WorkbenchReplayFailure::Unconfirmed => {
                            "the original tool outcome is unconfirmed; replay cannot repeat its effects"
                        }
                    };
                    return crate::OwnedActorTask::new(Box::pin(async move {
                        crate::OwnedActorCompletion::new(move |_| {
                            Err(KernelInvocationFailure::Rejected {
                                receipts: Vec::new(),
                                actor,
                                detail: detail.into(),
                                diagnostic: None,
                            })
                        })
                    }));
                }
                Ok(None) => {}
            }
            self.workbench_executions
                .lock()
                .begin(execution, request.clone(), key.as_ref());
            self.workbench_executions.lock().bind_provider_finalization(
                execution,
                key.as_ref(),
                Some(&control),
            );
        }
        let work = InvocationWork::new(
            actor,
            control
                .reservation_owner(actor)
                .expect("admitted tool reservation"),
        );
        self.workbench_executions.lock().retain_invocation_work(
            work.clone(),
            execution.as_ref(),
            key.as_ref(),
        );
        crate::OwnedActorTask::serial(move |mut behavior: Self, kernel| {
            Box::pin(async move {
                let context = behavior.context(actor);
                let runner = behavior.environment.runner.clone();
                let workbench = runner.application_workbench();
                let guard = workbench.actor_invocation_cleanup(
                    context.clone(),
                    "tool invocation abandoned before standing custody".into(),
                );
                let registration = guard.registration();
                let mut result = registration
                    .scope(crate::resident_workbench::with_execution_control(
                        control.clone(),
                        behavior.tool(&kernel, invocation, capture),
                    ))
                    .await;
                work.close();
                let cleanup = registration
                    .scope(work.cleanup(&behavior.environment, &kernel))
                    .await;
                if let Some(detail) = cleanup.uncertainty() {
                    result = Err(KernelInvocationFailure::CleanupUnconfirmed {
                        actor,
                        publication: None,
                        receipts: Vec::new(),
                        detail: format!(
                            "tool resource cleanup unconfirmed: {detail}; original outcome: {result:?}"
                        ),
                    });
                }
                let standing_can_transfer = !control
                    .native_cancel()
                    .load(std::sync::atomic::Ordering::Acquire)
                    && cleanup.uncertainty().is_none()
                    && matches!(
                        &behavior.standing,
                        ResidentStanding::Tools(_) | ResidentStanding::Terminal
                    )
                    && matches!(
                        &result,
                        Ok(_) | Err(KernelInvocationFailure::Rejected { .. })
                    );
                let custody = if standing_can_transfer {
                    workbench
                        .settle_actor_invocation_custody(context.clone(), registration.clone())
                        .await
                } else {
                    workbench
                        .abort_owned_continuations(
                            context.clone(),
                            registration.clone(),
                            "tool invocation did not settle successfully".into(),
                        )
                        .await
                };
                let cleanup_confirmed = cleanup.uncertainty().is_none() && custody.is_ok();
                match custody {
                    Ok(()) => guard.disarm(),
                    Err(error) => {
                        result = Err(KernelInvocationFailure::CleanupUnconfirmed {
                            actor,
                            publication: None,
                            receipts: Vec::new(),
                            detail: format!(
                                "tool continuation cleanup unconfirmed: {error}; original outcome: {result:?}"
                            ),
                        });
                    }
                }
                if cleanup_confirmed
                    && control
                        .native_cancel()
                        .load(std::sync::atomic::Ordering::Acquire)
                {
                    if control.cancellation_requested() {
                        control.acknowledge_cancellation();
                    }
                    result = Err(KernelInvocationFailure::Cancelled { actor });
                }
                if result.is_err() {
                    if let Some(owner) = control.reservation_owner(actor) {
                        let (_, notifications) = behavior
                            .environment
                            .requests
                            .abort_unsubmitted(actor, &owner);
                        publish_request_notifications(
                            &behavior.environment.requests,
                            &behavior.environment.deployments,
                            notifications,
                        )
                        .await;
                    }
                }
                let completion = crate::OwnedActorCompletion::new(move |behavior: &mut Self| {
                    if let (Some(execution), Some(request)) = (execution, request) {
                        let reply = crate::local_actor::tool_control_reply(
                            &result
                                .as_ref()
                                .map(|step| match step {
                                    KernelStep::Continue(value)
                                    | KernelStep::ContinueLater(value)
                                    | KernelStep::Stop { output: value, .. } => value.clone(),
                                })
                                .map_err(Clone::clone),
                        );
                        control.finish_cell(
                            execution.clone(),
                            &reply.clone().map(KernelStep::Continue),
                            cleanup_confirmed,
                        );
                        let cancellation =
                            control.cancellation_outcome(execution.clone(), reply.clone());
                        behavior.workbench_executions.lock().record(
                            execution,
                            request,
                            reply,
                            cancellation,
                            key.as_ref(),
                        );
                    }
                    result
                });
                (behavior, completion)
            })
        })
    }

    fn tool<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        invocation: exomonad_tool::ToolInvocation,
        hosted_checkpoint_capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<KernelStep<serde_json::Value>, KernelInvocationFailure>,
    > {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let hosted_boundary = hosted_checkpoint_capture.as_ref().and_then(|_| {
                let invocation_context = invocation.context.as_ref()?;
                invocation_context
                    .model_operation()
                    .cloned()
                    .map(tidepool_runtime::session::ContextCheckpointBoundary::Hosted)
            });
            if hosted_checkpoint_capture.is_some()
                && hosted_boundary
                    .as_ref()
                    .is_none_or(|boundary| !boundary.is_complete())
            {
                return Err(KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor: context.actor,
                    detail: "hosted checkpoint capture requires an exact provider invocation"
                        .into(),
                    diagnostic: None,
                });
            }
            let awaiting = match std::mem::replace(&mut self.standing, ResidentStanding::Boot) {
                ResidentStanding::Tools(awaiting) => awaiting,
                standing => {
                    self.standing = standing;
                    return Err(KernelInvocationFailure::Rejected {
                        receipts: Vec::new(),
                        actor: context.actor,
                        detail: "actor has no installed tool policy".into(),
                        diagnostic: None,
                    });
                }
            };
            if !awaiting.declarations.iter().any(|tool| {
                tool.name == invocation.name && tool.accepts_arguments(&invocation.arguments)
            }) {
                self.set_standing(context.actor, ResidentStanding::Tools(awaiting));
                return Err(KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor: context.actor,
                    detail: "unknown tool or invalid argument kind".into(),
                    diagnostic: None,
                });
            }
            let arguments = match invocation.arguments {
                exomonad_tool::ToolArguments::Raw(text) => serde_json::Value::String(text),
                exomonad_tool::ToolArguments::Structured(value) => value,
            };
            self.checkpoint_publication = CheckpointPublication::Workbench {
                boundary: hosted_boundary.or_else(|| {
                    crate::resident_workbench::execution_control().map(|control| {
                        tidepool_runtime::session::ContextCheckpointBoundary::Execution {
                            actor_id: context.actor.id.0,
                            incarnation: context.actor.incarnation.0,
                            execution_id: control.execution_id(context.actor),
                        }
                    })
                }),
                capture: hosted_checkpoint_capture,
            };
            let control = crate::resident_workbench::execution_control().ok_or_else(|| {
                Self::invocation_failure(
                    context.actor,
                    "tool execution has no original admission control",
                )
            })?;
            let work = self
                .workbench_executions
                .lock()
                .admitted_invocation_work(
                    &control.execution_id(context.actor),
                    control.invocation.as_ref(),
                )
                .ok_or_else(|| {
                    Self::invocation_failure(
                        context.actor,
                        "tool execution lost its original resource owner",
                    )
                })?;
            let mut cursor =
                green_tool::ToolCursor::new(work, control, self.checkpoint_publication.clone());
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
                .map_err(|error| Self::tool_invocation_failure(context.actor, error))?;
            let mut result = None;
            loop {
                cursor
                    .check_admission(kernel)
                    .map_err(|error| Self::tool_invocation_failure(context.actor, error))?;
                let boundary = self
                    .environment
                    .runner
                    .capture_boundary(
                        context.clone(),
                        outcome,
                        cursor.realm(context.placement.resource_scope),
                    )
                    .await
                    .map_err(|error| Self::tool_invocation_failure(context.actor, error))?;
                if !cursor.is_main_terminal()
                    && matches!(
                        &boundary,
                        ResidentActorBoundary::ToolReply(_)
                            | ResidentActorBoundary::ToolAwait(_)
                            | ResidentActorBoundary::Completed
                    )
                {
                    return Err(Self::invocation_failure(
                        context.actor,
                        "async tool child completed outside its result delimiter",
                    ));
                }
                match boundary {
                    ResidentActorBoundary::ToolReply(reply) => {
                        if result.replace(reply.result).is_some() {
                            return Err(KernelInvocationFailure::Failed {
                                receipts: Vec::new(),
                                actor: context.actor,
                                detail: "actor tool invocation replied more than once".into(),
                                diagnostic: None,
                            });
                        }
                        outcome = self
                            .environment
                            .runner
                            .resume_unit(context.clone(), reply.continuation)
                            .await
                            .map_err(|error| Self::tool_invocation_failure(context.actor, error))?;
                    }
                    ResidentActorBoundary::ToolAwait(next) => {
                        let result = result.ok_or_else(|| KernelInvocationFailure::Failed {
                            receipts: Vec::new(),
                            actor: context.actor,
                            detail: "actor awaited another tool invocation without replying".into(),
                            diagnostic: None,
                        })?;
                        self.checkpoint_publication = CheckpointPublication::Resident;
                        self.set_standing(context.actor, ResidentStanding::Tools(next));
                        let output = result.into_output().map_err(|error| {
                            KernelInvocationFailure::Rejected {
                                receipts: Vec::new(),
                                actor: context.actor,
                                detail: error.to_string(),
                                diagnostic: None,
                            }
                        })?;
                        return Ok(KernelStep::Continue(output));
                    }
                    ResidentActorBoundary::Completed => {
                        let result = result.ok_or_else(|| KernelInvocationFailure::Failed {
                            receipts: Vec::new(),
                            actor: context.actor,
                            detail: "actor completed a tool invocation without replying".into(),
                            diagnostic: None,
                        })?;
                        self.checkpoint_publication = CheckpointPublication::Resident;
                        self.set_standing(context.actor, ResidentStanding::Terminal);
                        let output = result.into_output().map_err(|error| {
                            KernelInvocationFailure::Rejected {
                                receipts: Vec::new(),
                                actor: context.actor,
                                detail: error.to_string(),
                                diagnostic: None,
                            }
                        })?;
                        return Ok(KernelStep::Stop {
                            output,
                            terminal: completed_terminal(),
                        });
                    }
                    boundary => {
                        outcome = self
                            .advance_tool_frontier(kernel, &context, &mut cursor, boundary)
                            .await
                            .map_err(|error| Self::tool_invocation_failure(context.actor, error))?;
                    }
                }
            }
        })
    }

    fn replace_spec<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        definition: crate::SpecReplacementDefinition,
    ) -> futures_util::future::BoxFuture<'a, Result<(), crate::SpecReplacementError>> {
        Box::pin(async move {
            let target = kernel.identity();
            let authorized =
                actor_can_control(definition.caller, target, &self.environment.actors.lock());
            if !authorized {
                return Err(crate::SpecReplacementError::Unauthorized);
            }
            let outcome = install_explicit_replacement(
                self.environment.clone(),
                self.context(target),
                self.installed_tools.clone(),
                self.descriptor.capabilities().effect_keys().to_vec(),
                definition,
            )
            .await;
            if outcome.is_ok() {
                self.spec_installs = self
                    .installed_tools
                    .current_tools()
                    .map_or(self.spec_installs, |tools| tools.install);
                self.after_tool.forget_failures();
            }
            outcome
        })
    }

    fn allows_independent_workbench_admission(&self) -> bool {
        true
    }

    fn serializes_workbench_publication(&self, request: &WorkbenchRequest) -> bool {
        request.tool_call().is_some_and(|call| {
            matches!(
                call.name.as_str(),
                crate::reload_spec_tool::RELOAD_SPEC_TOOL
                    | crate::reload_helpers_tool::RELOAD_HELPERS_TOOL
            )
        })
    }

    fn dispatch_workbench(
        &mut self,
        kernel: &KernelContext,
        invocation: crate::ActorWorkbenchInvocation,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> crate::WorkbenchDispatch<Self> {
        if invocation.display_expansion.is_some() {
            return crate::WorkbenchDispatch::Sequential {
                invocation,
                control,
            };
        }
        self.dispatch_owned_workbench(kernel, invocation, control)
    }

    fn workbench<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        invocation: crate::ActorWorkbenchInvocation,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>,
    > {
        Box::pin(async move {
            if let Some((identity, key)) = invocation.display_expansion {
                let context = self.context(kernel.identity());
                if control
                    .as_ref()
                    .is_some_and(|control| control.cancellation_requested())
                {
                    return Err(KernelInvocationFailure::Rejected {
                        receipts: Vec::new(),
                        actor: context.actor,
                        detail: "display actor is unavailable or invocation was cancelled".into(),
                        diagnostic: None,
                    });
                }
                let expansion = self.expand_display(
                    &context,
                    identity,
                    key,
                    DEFAULT_DISPLAY_CHARACTER_ALLOWANCE,
                    None,
                );
                let output = match control {
                    Some(control) => {
                        crate::resident_workbench::with_execution_control(control, expansion).await
                    }
                    None => expansion.await,
                }
                .map_err(|error| KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor: context.actor,
                    detail: error.to_string(),
                    diagnostic: error.failure_diagnostic(),
                })?;
                let operation = WorkbenchOperationReceipt {
                    display_publication: None,
                    id: WorkbenchOperationId {
                        execution: WorkbenchExecutionId::from_digest(
                            *uuid::Uuid::new_v4().as_bytes(),
                        ),
                        input_unit_index: 0,
                        effect_ordinal: 0,
                    },
                    effect: "expand".into(),
                    disposition: WorkbenchOperationDisposition::Committed,
                    display: Some(output.clone()),
                };
                return Ok(KernelStep::Continue(workbench_response(
                    WorkbenchRunStatus::Committed,
                    vec![WorkbenchItemReceipt {
                        index: 0,
                        kind: None,
                        span: None,
                        source_items: Vec::new(),
                        status: WorkbenchItemStatus::Committed,
                        output: output.page.text,
                        value: None,
                        diagnostics: Vec::new(),
                        failure_layer: None,
                        warnings: Vec::new(),
                        installed_bindings: Vec::new(),
                        operations: vec![operation],
                        terminal_transfer: None,
                    }],
                    1,
                    1,
                    None,
                )));
            }
            let admitted = self.preflight_workbench(kernel.identity(), invocation, control)?;
            let WorkbenchAdmission {
                context,
                request,
                installed_tools,
                admitted_source,
                compilation_authority,
                public_owner: _public_owner,
                current_builtin,
                capture,
                context_binding,
                control,
                invocation,
            } = match admitted {
                WorkbenchPreflight::Retained(reply) => return reply.map(KernelStep::Continue),
                WorkbenchPreflight::Admitted(admitted) => admitted,
            };
            let admitted_workbench = compilation_authority.and_then(|authority| {
                self.active_workbench().map(|workbench| {
                    workbench
                        .with_compilation_authority(authority)
                        .with_json_input(
                            request
                                .input
                                .as_ref()
                                .map(tidepool_runtime::session::normalize_workbench_input),
                        )
                })
            });
            let execution = request.execution_id().cloned();
            let retained_request = execution.as_ref().map(|_| request.clone());
            let public_visibility = if current_builtin {
                None
            } else {
                Some(
                    self.environment
                        .runner
                        .public_visibility_snapshot(context.clone())
                        .await
                        .map_err(|error| KernelInvocationFailure::Rejected {
                            receipts: Vec::new(),
                            actor: context.actor,
                            detail: format!("cannot capture public workbench view: {error}"),
                            diagnostic: None,
                        })?,
                )
            };
            if let Some(execution) = &execution {
                // Persist the fence in the forest-retained journal before effects
                // can run; actor termination cannot turn uncertainty into replay.
                self.workbench_executions.lock().begin(
                    execution,
                    request.clone(),
                    invocation.as_ref(),
                );
                self.workbench_executions.lock().bind_provider_finalization(
                    execution,
                    invocation.as_ref(),
                    control.as_ref(),
                );
            }
            let local_execution_id = execution.clone().unwrap_or_else(|| {
                WorkbenchExecutionId::from_digest(*uuid::Uuid::new_v4().as_bytes())
            });
            let reservation_owner = RequestReservationOwner::Workbench {
                execution: local_execution_id.clone(),
                attempt: crate::request::WorkbenchReservationAttempt::fresh(),
            };
            let invocation_work = InvocationWork::new(context.actor, reservation_owner.clone());
            self.workbench_executions.lock().retain_invocation_work(
                invocation_work.clone(),
                execution.as_ref(),
                invocation.as_ref(),
            );
            let display_receipt_owner = execution.as_ref().and_then(|execution| {
                self.workbench_executions
                    .lock()
                    .display_receipt_owner(execution, invocation.as_ref())
            });
            if let (Some(owner), Some(control)) = (&display_receipt_owner, &control) {
                control.bind_receipt_owner(owner.clone());
            }
            let mut execution_state = WorkbenchExecutionState {
                cell_span: cell_execution_span(&context, &request),
                effects: WorkbenchEffectState {
                    display_receipt_owner,
                    park_effects: false,
                    context: context.clone(),
                    public_visibility,
                    control,
                    model: None,
                    context_binding,
                    installed_tools,
                    admitted_source,
                    reservation_owner,
                    invocation_work,
                    publication: CheckpointPublication::Workbench {
                        boundary: request.checkpoint_boundary().cloned(),
                        capture,
                    },
                    after_tool_active: false,
                    terminal_transfer: None,
                },
                request,
                replay_request: retained_request,
                invocation,
                cursor: WorkbenchCursor::default(),
            };
            // One INFO line per hosted tool call or cell, breaking down
            // where its wall time went (checkout wait/hold, compile, Jev,
            // exec) — see `crate::call_timing`. The scope wraps the whole
            // call so every nested site it awaits (workbench compiles,
            // resolved effects) can add to it as a task-local.
            let call_kind = execution_state
                .request
                .tool_call()
                .map(|call| call.name.clone())
                .unwrap_or_else(|| "cell".to_string());
            let (call_actor, call_incarnation) = actor_address(context.actor);
            let call_scope = crate::call_timing::CallScope::new(
                call_kind,
                call_actor as u64,
                call_incarnation as u64,
            );
            if let Some(public) = &execution_state.effects.public_visibility {
                tracing::debug!(
                    actor = ?context.actor,
                    scope = ?public.scope,
                    epoch = public.epoch,
                    declaration_tip = public.declaration_tip.0,
                    binding_count = public.bindings.len(),
                    "workbench public view admitted"
                );
            }
            let compiler_owner = crate::resident_workbench::CompilerCloseOwner::Invocation {
                work: execution_state.effects.invocation_work.clone(),
                control: execution_state.effects.control.clone(),
            };
            let result = compiler_owner
                .scope(call_scope.run(self.execute_workbench(
                    kernel,
                    &mut execution_state,
                    admitted_workbench.as_ref(),
                )))
                .await
                .and_then(|advance| {
                    match advance {
                    WorkbenchRunAdvance::Complete(step) => Ok(step),
                    WorkbenchRunAdvance::ParkGreen => Err(workbench_failure(
                        &execution_state.cursor.receipts,
                        execution_state.cursor.index,
                        execution_state.request.items.len(),
                        ResidentActorWorkbenchError::ActorProtocol(
                            "async frontier reached a serial workbench without owned task admission"
                                .into(),
                        ),
                    )),
                    WorkbenchRunAdvance::ParkEffect
                    | WorkbenchRunAdvance::ParkUnit
                    | WorkbenchRunAdvance::ParkNative
                    | WorkbenchRunAdvance::ParkAfterToolStart
                    | WorkbenchRunAdvance::ParkAfterToolFinish => {
                        unreachable!("legacy workbench remains serial")
                    }
                }
                });
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
            self.finalize_serial_workbench_execution(&mut execution_state, kernel, result)
                .await
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
            let reservation_owner = RequestReservationOwner::Route(watch);
            let route_work = self
                .workbench_executions
                .lock()
                .callback_root(context.actor, reservation_owner.clone());
            let previous_reservation_owner = self
                .active_route_reservation_owner
                .replace(reservation_owner.clone());
            // Route completion settles only checkpoints captured by this callback.
            let completion = tidepool_runtime::session::ContextCheckpointBoundary::Route {
                actor_id: context.actor.id.0,
                incarnation: context.actor.incarnation.0,
                watch_id: watch.0,
            };
            self.checkpoint_publication = CheckpointPublication::Route(completion.clone());
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
                                Ok(claim) => {
                                    let publication_boundary =
                                        self.checkpoint_publication.boundary().cloned();
                                    self.stage_request_reply(
                                        kernel,
                                        &context,
                                        claim,
                                        attempt.result,
                                        attempt.preview,
                                        publication_boundary.as_ref(),
                                        None,
                                        None,
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
                                        settlement_refusal("reply", attempt.request, error),
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
                            self.actor_effect_owner(context.actor),
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
            // Projection and nested scope resources belong to this callback.
            result = self
                .finish_callback_resources(kernel, &route_work, result)
                .await;
            self.active_route_reservation_owner = previous_reservation_owner;
            self.checkpoint_publication = CheckpointPublication::Resident;
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

    fn dispatch_resume(
        &mut self,
        kernel: &KernelContext,
        kind: crate::kernel::KernelResume,
    ) -> Result<crate::OwnedActorTask<Self, ()>, KernelBehaviorError> {
        if kind == crate::kernel::KernelResume::ReleaseRootStartup {
            let (_, latch) = self
                .root_startup
                .as_ref()
                .ok_or_else(|| Self::failure("actor has no pending root startup"))?;
            let mut state = latch.lock();
            match *state {
                RootStartupState::Pending => {
                    return Err(Self::failure("root startup has not been durably released"));
                }
                RootStartupState::Activated => {
                    return Ok(crate::OwnedActorTask::new(Box::pin(async {
                        crate::OwnedActorCompletion::new(|_| Ok(KernelStep::Continue(())))
                    })));
                }
                RootStartupState::Released => *state = RootStartupState::Activated,
            }
            drop(state);
            let boot = self
                .boot
                .take()
                .ok_or_else(|| Self::failure("original root boot was already consumed"))?;
            let context = self.context(kernel.identity());
            return Ok(crate::OwnedActorTask::serial(
                move |mut behavior: Self, kernel| {
                    Box::pin(async move {
                        let result = behavior
                            .initialize(&kernel, &context, boot)
                            .await
                            .map_err(|error| Self::invocation_failure(kernel.identity(), error));
                        (behavior, crate::OwnedActorCompletion::new(move |_| result))
                    })
                },
            ));
        }
        if kind == crate::kernel::KernelResume::ConfirmChildDurability {
            let context = self.context(kernel.identity());
            let owner = self
                .environment
                .actors
                .lock()
                .get(&context.actor)
                .and_then(|record| match &record.public_owner {
                    ActorPublicOwnerPlane::DurableReady(owner)
                        if owner.matches_context(&context) =>
                    {
                        owner.durable().cloned()
                    }
                    _ => None,
                });
            let runner = self.environment.runner.clone();
            // Confirmation of a post-initialization write cannot resume
            // the program or mint a different owner. Its shared native fact
            // becomes ready even if delivery of this completion is lost.
            return Ok(crate::OwnedActorTask::new(Box::pin(async move {
                if let Some(owner) = owner {
                    if let Err(error) = runner.confirm_durable_public_owner(context, owner).await {
                        tracing::warn!(%error, "child native publication remains unconfirmed");
                    }
                }
                crate::OwnedActorCompletion::new(|_| Ok(KernelStep::Continue(())))
            })));
        }
        // Remaining program stabilization is still the original serial driver.
        Ok(crate::OwnedActorTask::serial(
            move |mut behavior: Self, kernel| {
                Box::pin(async move {
                    let result = behavior
                        .resume(&kernel)
                        .await
                        .map_err(|error| Self::invocation_failure(kernel.identity(), error));
                    (behavior, crate::OwnedActorCompletion::new(move |_| result))
                })
            },
        ))
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
            if self.root_startup.is_none()
                && self.descriptor.checkpoint_boundary().is_none()
                && matches!(self.boot.as_ref(), Some(ResidentBoot::Prepared(_)))
            {
                let boot = self
                    .boot
                    .take()
                    .expect("prepared startup retained until resume");
                return self
                    .initialize(kernel, &context, boot)
                    .await
                    .map_err(Self::workbench_failure);
            }
            let PendingActorProgram {
                outcome, cleanup, ..
            } = self
                .pending_program
                .take()
                .ok_or_else(|| KernelBehaviorError {
                    detail: "resident actor resumed without a pending Haskell action".into(),
                    diagnostic: None,
                })?;
            let mut handler = self.suspended_cast.take();
            if let Some(handler) = &mut handler {
                handler.cleanup.take();
            }
            let resumed = async {
                if let Some(suspended) = &handler {
                    self.advance_cast_handler(
                        kernel,
                        &context,
                        &crate::CallAncestry::begin(context.actor),
                        SuspendedCast {
                            site: suspended.site,
                            receiver_continuation: suspended.receiver_continuation.clone(),
                            handler_realm: suspended.handler_realm,
                            cleanup: None,
                        },
                        outcome,
                    )
                    .await
                    .map_err(Self::workbench_failure)
                } else {
                    self.stabilize_program(
                        kernel,
                        &context,
                        &crate::CallAncestry::begin(context.actor),
                        outcome,
                        None,
                    )
                    .await
                    .map_err(Self::workbench_failure)
                }
            };
            let step = match &cleanup {
                Some(cleanup) => cleanup.registration().scope(resumed).await,
                None => resumed.await,
            };
            match step {
                Ok(step) => {
                    if let Some(cleanup) = cleanup {
                        if let Some(suspended) = &mut self.suspended_cast {
                            // A repeated AgentSession remains in the handler's
                            // distinct realm. Keep exact cleanup until it finishes.
                            suspended.cleanup = Some(cleanup);
                        } else {
                            // finish_receiver closed the handler realm; ordinary
                            // actor standing now owns any remaining receiver hole.
                            cleanup.disarm();
                        }
                    }
                    if let Some(claim) = self.pending_reply.take() {
                        let request = claim.request();
                        let reply_preview = self.pending_reply_preview.take();
                        // Only this actor can read its own descriptor and
                        // prepared workspace, so its path and seed revision
                        // are attached here, ahead of settlement, rather
                        // than looked up later from the bare request.
                        self.environment.requests.record_target_identity(
                            request,
                            self.descriptor.actor_path().map(ToString::to_string),
                            self.prepared_workspace.as_ref().map(|workspace| {
                                workspace.handle().handle_receipt.source_head.raw.clone()
                            }),
                        );
                        let notifications = self.environment.requests.finish_reply(
                            claim,
                            match self.pending_response.take() {
                                Some(response) => response,
                                None => {
                                    let detail =
                                        "reply wrapper completed without publishing its response";
                                    let notifications = self
                                        .environment
                                        .requests
                                        .fail_reply_settlement(request, detail);
                                    self.publish_watch_notifications(notifications).await;
                                    return Err(Self::workbench_failure(
                                        ResidentActorWorkbenchError::ActorProtocol(detail.into()),
                                    ));
                                }
                            },
                            reply_preview,
                        );
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
                    if let Some(mut handler) = handler {
                        // Failed stabilization still owes exact handler realm
                        // retirement; actor root realm cleanup cannot cover it.
                        handler.cleanup = cleanup;
                        self.suspended_cast = Some(handler);
                    }
                    self.pending_reply_preview = None;
                    self.pending_response.take();
                    if let Some(claim) = self.pending_reply.take() {
                        let request = claim.request();
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
                    return Err(KernelBehaviorError::new(detail));
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
            let invocations = self.workbench_executions.lock().invocation_work();
            for invocation in &invocations {
                invocation.close();
            }
            let environment = &self.environment;
            let invocation_cleanup = futures_util::future::join_all(invocations.into_iter().map(
                |invocation| async move {
                    tokio::time::timeout_at(deadline, invocation.cleanup(environment, kernel))
                        .await
                        .map(|cleanup| cleanup.uncertainty())
                        .unwrap_or_else(|_| {
                            Some("invocation cleanup remains pending at actor retirement".into())
                        })
                },
            ))
            .await;
            let mut provider_cleanup_errors = Vec::new();
            let provider_boundaries = self
                .workbench_executions
                .lock()
                .pending_provider_boundaries();
            for boundary in provider_boundaries {
                let result = tokio::time::timeout_at(
                    deadline,
                    self.abort_provider_boundary(kernel, boundary.clone()),
                )
                .await
                .unwrap_or_else(|_| {
                    Err(KernelBehaviorError::new(
                        "provider boundary cleanup remains pending at actor retirement",
                    ))
                });
                self.workbench_executions.lock().finalize_provider_boundary(
                    &boundary,
                    result
                        .as_ref()
                        .map(|()| crate::ProviderFinalizationKind::RetirementAborted)
                        .map_err(ToString::to_string),
                );
                if let Err(error) = result {
                    provider_cleanup_errors.push(error.to_string());
                }
            }
            let staged_replacement = self.replacement_staged();
            let staged_placement = self.boot.is_some();
            self.source_connections.take();
            self.sources.clear();
            let context = self.context(kernel.identity());
            let notifications = self
                .environment
                .requests
                .actor_stopped(context.actor, terminal);
            self.publish_watch_notifications(notifications).await;

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
            let mut retained_errors = invocation_cleanup.into_iter().flatten().collect::<Vec<_>>();
            retained_errors.extend(provider_cleanup_errors);
            self.pending_program.take();
            if let Some(suspended) = self.suspended_cast.take() {
                if let Err(error) = self
                    .environment
                    .runner
                    .close_realm_wait(
                        context.clone(),
                        suspended.handler_realm,
                        deadline.saturating_duration_since(tokio::time::Instant::now()),
                    )
                    .await
                {
                    retained_errors.push(error.to_string());
                }
                drop(suspended);
            }
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
            let realm_result = if staged_replacement
                || staged_placement
                || kernel.spawn_ownership().is_independent()
            {
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
            let session = self.descriptor.placement().session;
            let failed_scopes = self
                .environment
                .actor_admissions
                .failed_checkpoint_scopes(kernel.identity());
            if let Err(error) = self
                .environment
                .runner
                .retire_checkpoint_scopes(
                    session,
                    failed_scopes
                        .into_iter()
                        .filter_map(|(owner, scope)| (owner == session).then_some(scope))
                        .collect(),
                )
                .await
            {
                tracing::warn!(actor = ?kernel.identity(), %error, "failed checkpoint scope cleanup was retained");
            }
            let pending_releases = self
                .environment
                .actor_admissions
                .pending_release_scopes(session);
            if !pending_releases.is_empty() {
                match self
                    .environment
                    .runner
                    .retire_checkpoint_scopes(
                        session,
                        pending_releases.iter().map(|(_, scope)| *scope).collect(),
                    )
                    .await
                {
                    Ok(()) => {
                        for (token, scope) in pending_releases {
                            if let Err(error) = self
                                .environment
                                .actor_admissions
                                .confirm_checkpoint_release(&token, session, scope)
                            {
                                tracing::warn!(actor = ?kernel.identity(), ?error, "checkpoint release confirmation failed");
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(actor = ?kernel.identity(), %error, "checkpoint release cleanup was retained");
                    }
                }
            }
            self.release_session_state();
            // A live actor or published checkpoint keeps the issuing machine
            // available after this actor's terminal record is written.
            let session_retained = self.environment.actors.lock().values().any(|record| {
                record.descriptor.placement().session == session && record.terminal.is_none()
            }) || self.environment.actor_admissions.retains_session(session);
            // A no-op check for every actor that never got a dedicated
            // child session (the shared session is never a member). One
            // bounded checkout, no wait for whoever still needs this
            // machine's output — see
            // `ResidentActorRunner::retire_child_session`'s doc comment.
            if let Err(error) = self
                .environment
                .runner
                .retire_child_session(session, session_retained)
                .await
            {
                tracing::warn!(
                    actor = ?kernel.identity(), session = ?self.descriptor.placement().session,
                    %error, "dedicated child session teardown check failed"
                );
            }
        })
    }

    fn child_exited(&mut self, notice: ChildExitNotice) {
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
    spawn_resident_root_with_workspace_admission(source, root, None).await
}

pub async fn spawn_resident_root_with_workspace_admission<H, O>(
    source: ActorWorkbenchSource,
    root: ResidentActorRoot<H, O>,
    fork_workspaces: Option<crate::fork_workspace::SharedWorkspaceAdmission>,
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
    fork_workspaces: Option<crate::fork_workspace::SharedWorkspaceAdmission>,
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
    let (descriptor, machine, entry) = root.into_parts();
    let ResidentRootEntry::Prepared(outcome) = entry else {
        return Err(ractor::SpawnErr::StartupFailed(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "retained startup requires pending root admission with its exact intent",
            ),
        )));
    };
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
    pub model_actor: bool,
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
    worktree_custody: Option<Arc<dyn crate::WorkspaceCustody>>,
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

    /// Bind each subsequently admitted workbench execution to one model owner.
    /// Child admission supplies its actual descriptor to this same factory.
    #[must_use]
    pub fn with_cell_model_factory(mut self, factory: Arc<dyn crate::CellModelFactory>) -> Self {
        self.environment.cell_model_factory = Some(factory);
        self
    }

    /// Give every actor launched from now on its own source layer, resolved
    /// from the checkout it is launched with. Without this every actor
    /// compiles against the deployment-wide include roots and nothing else.
    pub fn set_source_layers(&mut self, layers: crate::ActorSourceLayerResolver) {
        self.environment.source_layers = Some(layers);
    }

    /// Install the durable human interaction owner before actor admission.
    pub fn with_form_host(mut self, host: Arc<dyn crate::FormHost>) -> Self {
        self.environment.form_host = Some(host);
        self
    }

    /// Install the immutable usage pointers supplied by the facade's shipped
    /// workspace. The actor kernel owns lookup behavior; the facade owns the
    /// source inventory and its generated table.
    pub fn with_usage_pointers(mut self, pointers: crate::UsagePointerTable) -> Self {
        self.environment.usage_pointers = pointers;
        self
    }

    /// Install the composition root's [`crate::ChildSessionFactory`]. Omitted,
    /// every launch keeps running on the session that admitted it — today's
    /// behavior, unchanged. Call this before any actor is admitted: it
    /// replaces `environment.runner` outright, so a clone taken beforehand
    /// (e.g. by an already-admitted actor's workbench) would not see it.
    #[must_use]
    pub fn with_child_session_factory(
        mut self,
        factory: crate::resident_workbench::ChildSessionFactory<H, O>,
    ) -> Self {
        self.environment.runner = self.environment.runner.with_child_session_factory(factory);
        self
    }

    /// Install this run's shared [`tidepool_runtime::session::ImageRegistry`],
    /// applied to every session's engine (root and any child alike) on its
    /// later checkouts — see [`ResidentActorRunner::with_image_registry`].
    /// Omitted, every session compiles its own images, unchanged.
    #[must_use]
    pub fn with_image_registry(
        mut self,
        registry: std::sync::Arc<tidepool_runtime::session::ImageRegistry>,
    ) -> Self {
        self.environment.runner = self.environment.runner.with_image_registry(registry);
        self
    }

    /// Install the compiled turn a fresh child session bootstraps with —
    /// see [`crate::resident_workbench::ResidentActorRunner::with_child_bootstrap_program`].
    /// Required, alongside [`Self::with_child_session_factory`], for an
    /// eligible `SelectedContext` launch's own machine to actually come up.
    #[must_use]
    pub fn with_child_bootstrap_program(
        mut self,
        program: Arc<tidepool_runtime::session::CompiledTurn>,
    ) -> Self {
        self.environment.runner = self
            .environment
            .runner
            .with_child_bootstrap_program(program);
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

    /// The session `actor` is currently placed on, if it is still in the
    /// directory — for a `SelectedContext` launch this may differ from
    /// [`Self::session`] (this forest's own root), see per-actor machines
    /// parcel 7. A thin, read-only observability primitive: callers decide
    /// what to do with the id, this never mutates anything.
    #[must_use]
    pub fn actor_session(&self, actor: ActorRef) -> Option<tidepool_repr::SessionId> {
        self.directory
            .session_context(actor)
            .map(|context| context.placement.session)
    }

    /// Queue one confirmation attempt for a visible child surface. Queue
    /// acceptance is not provider readiness; attachment still requires the
    /// original actor's durable-ready capsule.
    pub fn request_child_durability_confirmation(
        &self,
        actor: ActorRef,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let refuse = |detail: &str| ResidentActorWorkbenchError::ActorProtocol(detail.into());
        let target = self
            .directory
            .resolve(actor)
            .ok_or_else(|| refuse("child confirmation actor is unavailable"))?;
        let context = self
            .directory
            .session_context(actor)
            .ok_or_else(|| refuse("child confirmation has no installed placement"))?;
        if target.terminal().requested_shutdown().is_some() {
            return Err(refuse(
                "child confirmation requires its original live allocation",
            ));
        }
        let records = self.environment.actors.lock();
        let record = records
            .get(&actor)
            .ok_or_else(|| refuse("child confirmation has no registered allocation"))?;
        if record.terminal.is_some()
            || record.descriptor.placement() != context.placement
            || record.descriptor.creator().is_none()
        {
            return Err(refuse(
                "child confirmation requires its original live allocation",
            ));
        }
        match &record.public_owner {
            ActorPublicOwnerPlane::DurableReady(owner) if owner.is_ready() => return Ok(()),
            ActorPublicOwnerPlane::DurableReady(_) => {}
            ActorPublicOwnerPlane::DurablePublishedUnconfirmed { detail, .. } => {
                tracing::debug!(actor = %actor, %detail, "child durability confirmation requested");
            }
            _ => return Err(refuse("child has no visible unconfirmed public surface")),
        }
        drop(records);
        target
            .admit_mailbox(KernelMessage::Resume {
                kind: crate::kernel::KernelResume::ConfirmChildDurability,
            })
            .map_err(|error| refuse(&error.to_string()))
    }

    /// Bind recovery evidence to an actually admitted canonical root. The
    /// durable journal revalidates this placement before certifying transfer.
    pub fn root_recovery_placement(
        &self,
        actor: ActorRef,
    ) -> Result<crate::RootRecoveryPlacement, ResidentActorWorkbenchError> {
        let refuse = |detail: &str| ResidentActorWorkbenchError::ActorProtocol(detail.into());
        let context = self
            .directory
            .session_context(actor)
            .ok_or_else(|| refuse("root recovery actor has no admitted session context"))?;
        if self
            .directory
            .resolve(actor)
            .is_none_or(|actor| actor.terminal().get().is_some())
        {
            return Err(refuse("root recovery actor is unavailable or terminal"));
        }
        let records = self.environment.actors.lock();
        let record = records
            .get(&actor)
            .ok_or_else(|| refuse("root recovery actor has no registered descriptor"))?;
        if context.actor != actor
            || context.placement != record.descriptor.placement()
            || !record.scheduler_root
            || record.descriptor.persistence_policy() != crate::ActorPersistencePolicy::Durable
            || record.terminal.is_some()
            || record.descriptor.creator().is_some()
            || record.descriptor.supervisor_parent().is_some()
            || record.descriptor.context_parent().is_some()
        {
            return Err(refuse(
                "root recovery requires the exact live admitted root placement",
            ));
        }
        let path = record
            .descriptor
            .actor_path()
            .ok_or_else(|| refuse("root recovery requires a registered canonical actor path"))?;
        let owner = tidepool_runtime::session::RecoveryPublicOwner::new(path, actor.incarnation.0)
            .ok_or_else(|| refuse("root recovery incarnation is invalid"))?;
        Ok(crate::RootRecoveryPlacement::new(
            actor,
            owner,
            context.placement,
        ))
    }

    fn validate_root_startup_public_operation(
        &self,
        placement: &crate::RootRecoveryPlacement,
        predecessor: Option<&tidepool_runtime::session::RecoveryPublicOwner>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let records = self.environment.actors.lock();
        let pending_startup = records
            .get(&placement.actor())
            .is_some_and(|record| record.root_startup.is_some());
        drop(records);
        if !pending_startup {
            return Ok(());
        }
        let journal = self.environment.recovery.as_ref().ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol("root startup journal is absent".into())
        })?;
        let records = journal
            .validated_records()
            .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
        let intent = records
            .iter()
            .find(|record| record.admission.actor == placement.actor())
            .and_then(|record| record.startup.as_ref())
            .ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    "root original startup intent is absent".into(),
                )
            })?;
        if intent
            .manifest
            .as_ref()
            .map(|manifest| &manifest.public_owner)
            != predecessor
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "root public operation differs from its original startup manifest owner".into(),
            ));
        }
        Ok(())
    }

    fn root_public_owner_posture(
        &self,
        placement: &crate::RootRecoveryPlacement,
    ) -> Result<RootPublicOwnerPosture, ResidentActorWorkbenchError> {
        let records = self.environment.actors.lock();
        let record = records.get(&placement.actor()).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "root public owner allocation is absent".into(),
            )
        })?;
        match &record.public_owner {
            ActorPublicOwnerPlane::DurablePending(owner) if owner == placement.owner() => {
                Ok(RootPublicOwnerPosture::Pending)
            }
            ActorPublicOwnerPlane::DurablePublishedUnconfirmed { owner, .. }
                if owner == placement.owner() =>
            {
                Ok(RootPublicOwnerPosture::PublishedUnconfirmed)
            }
            ActorPublicOwnerPlane::DurableReady(owner)
                if owner.durable() == Some(placement.owner()) =>
            {
                Ok(if owner.is_ready() {
                    RootPublicOwnerPosture::Ready
                } else {
                    RootPublicOwnerPosture::PublishedUnconfirmed
                })
            }
            _ => Err(ResidentActorWorkbenchError::ActorProtocol(
                "root public plane differs from its original requested owner".into(),
            )),
        }
    }

    fn settle_root_public_owner(
        &self,
        placement: &crate::RootRecoveryPlacement,
        outcome: &tidepool_runtime::session::PublicManifestCommit,
        readiness: Option<Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let context = self.root_recovery_context(placement)?;
        let mut records = self.environment.actors.lock();
        let record = records.get_mut(&placement.actor()).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "root public allocation retired before confirmation".into(),
            )
        })?;
        settle_public_owner_record(record, &context, placement.owner(), outcome, readiness)
    }

    /// Confirm only directory durability of the original visible root owner.
    /// This never stages another manifest or consumes the retained startup boot.
    pub async fn confirm_durable_root_public_owner(
        &self,
        actor: ActorRef,
    ) -> Result<tidepool_runtime::session::PublicManifestCommit, ResidentActorWorkbenchError> {
        let placement = self.root_recovery_placement(actor)?;
        match self.root_public_owner_posture(&placement)? {
            RootPublicOwnerPosture::Ready => {
                return Ok(tidepool_runtime::session::PublicManifestCommit::Durable);
            }
            RootPublicOwnerPosture::Pending => {
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "root has no visible original manifest to confirm".into(),
                ));
            }
            RootPublicOwnerPosture::PublishedUnconfirmed => {}
        }
        let context = self.root_recovery_context(&placement)?;
        let established = self.environment.actors.lock().get(&actor)
            .is_some_and(|record| matches!(&record.public_owner,
                ActorPublicOwnerPlane::DurableReady(owner) if owner.durable() == Some(placement.owner())));
        let readiness = if established {
            self.environment
                .runner
                .confirm_durable_public_owner(context, placement.owner().clone())
                .await?;
            None
        } else {
            Some(
                self.environment
                    .runner
                    .confirm_initial_durable_public_owner(context, placement.owner().clone())
                    .await?,
            )
        };
        let outcome = tidepool_runtime::session::PublicManifestCommit::Durable;
        self.settle_root_public_owner(&placement, &outcome, readiness)?;
        Ok(outcome)
    }

    /// Persist the admitted root's initial empty surface. Visible uncertainty
    /// routes repeated calls through confirmation, never through initialization.
    pub async fn bind_durable_root_public_owner(
        &self,
        actor: ActorRef,
    ) -> Result<tidepool_runtime::session::PublicManifestCommit, ResidentActorWorkbenchError> {
        let placement = self.root_recovery_placement(actor)?;
        self.validate_root_startup_public_operation(&placement, None)?;
        match self.root_public_owner_posture(&placement)? {
            RootPublicOwnerPosture::Ready => {
                return Ok(tidepool_runtime::session::PublicManifestCommit::Durable);
            }
            RootPublicOwnerPosture::PublishedUnconfirmed => {
                return self.confirm_durable_root_public_owner(actor).await;
            }
            RootPublicOwnerPosture::Pending => {}
        }
        let context = self.root_recovery_context(&placement)?;
        let (outcome, readiness) = self
            .environment
            .runner
            .initialize_durable_public_owner(context, placement.owner().clone())
            .await?
            .into_parts();
        self.settle_root_public_owner(&placement, &outcome, readiness)?;
        Ok(outcome)
    }

    /// Transfer under the exact journal proof. After a visible publication,
    /// repeated calls confirm the original owner without restaging the transfer.
    pub async fn transfer_recovered_root_public_owner(
        &self,
        actor: ActorRef,
        predecessor: &tidepool_runtime::session::RecoveryPublicOwner,
        authority: Arc<dyn tidepool_runtime::session::RecoverySuccessorAuthority>,
    ) -> Result<tidepool_runtime::session::PublicManifestCommit, ResidentActorWorkbenchError> {
        let placement = self.root_recovery_placement(actor)?;
        self.validate_root_startup_public_operation(&placement, Some(predecessor))?;
        match self.root_public_owner_posture(&placement)? {
            RootPublicOwnerPosture::Ready => {
                return Ok(tidepool_runtime::session::PublicManifestCommit::Durable);
            }
            RootPublicOwnerPosture::PublishedUnconfirmed => {
                return self.confirm_durable_root_public_owner(actor).await;
            }
            RootPublicOwnerPosture::Pending => {}
        }
        let context = self.root_recovery_context(&placement)?;
        let (outcome, readiness) = self
            .environment
            .runner
            .transfer_recovered_root_public_owner(
                context,
                predecessor,
                placement.owner().clone(),
                authority,
            )
            .await?
            .into_parts();
        self.settle_root_public_owner(&placement, &outcome, readiness)?;
        Ok(outcome)
    }

    /// Request compiled display detail through its owning mailbox. Provider
    /// attachment and a resident tool surface are not prerequisites.
    pub async fn request_expansion(
        &self,
        actor: ActorRef,
        identity: (i64, i64, i64),
        key: i64,
    ) -> Result<WorkbenchResponse, crate::ResidentToolError> {
        ActorDisplays::validate_identity(actor, identity)
            .map_err(|error| crate::ResidentToolError::Unavailable(error.to_string()))?;
        let target = self.directory.resolve(actor).ok_or_else(|| {
            crate::ResidentToolError::Unavailable("display actor is unavailable".into())
        })?;
        crate::resident_tools::expand_display_response(&target, identity, key).await
    }

    /// Authorize an exact accepted emission independently of provider startup.
    pub fn authorize_display_publication(
        &self,
        request: &Arc<DisplayPublication>,
    ) -> Result<Arc<ActorDisplayAdmission>, ResidentActorWorkbenchError> {
        let records = self.environment.actors.lock();
        let record = records.get(&request.actor).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol("display actor owner is unavailable".into())
        })?;
        let admission = Arc::new(ActorDisplayAdmission {
            actor: request.actor,
            placement: record.descriptor.placement(),
            displays: record.displays.clone(),
            request: request.clone(),
        });
        drop(records);
        self.validate_display_publication(&admission, request)?;
        Ok(admission)
    }

    /// Recheck the same owner's pending Arc under the Store transaction. An
    /// actor's retirement releases roots but preserves accepted pending output.
    pub fn validate_display_publication(
        &self,
        admission: &ActorDisplayAdmission,
        request: &Arc<DisplayPublication>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let refuse = || {
            ResidentActorWorkbenchError::ActorProtocol(
                "display admission no longer matches its issued pending publication".into(),
            )
        };
        if request.actor != admission.actor || !Arc::ptr_eq(request, &admission.request) {
            return Err(refuse());
        }
        let slot = ActorDisplays::validate_identity(request.actor, request.page.identity)?;
        let records = self.environment.actors.lock();
        let record = records.get(&admission.actor).ok_or_else(refuse)?;
        if record.descriptor.placement() != admission.placement
            || !Arc::ptr_eq(&record.displays, &admission.displays)
        {
            return Err(refuse());
        }
        let displays = record.displays.lock();
        let owned = displays.slots.get(&slot).ok_or_else(refuse)?;
        if !owned.pending.as_ref().is_some_and(|pending| {
            Arc::ptr_eq(&pending.request, request)
                && owned.page_ordinal.checked_add(1) == Some(request.page_ordinal)
        }) || matches!(
            request.outcome(),
            Some(DisplayPublicationOutcome::Refused(_))
        ) {
            return Err(refuse());
        }
        Ok(())
    }

    /// Admit attachment only after the actual actor's requested plane is ready.
    pub fn authorize_provider_attachment(
        &self,
        actor: ActorRef,
    ) -> Result<Arc<ActorProviderAdmission>, ResidentActorWorkbenchError> {
        let records = self.environment.actors.lock();
        Self::validate_provider_lineage(actor, &records)?;
        if records.get(&actor).is_some_and(|record| {
            record
                .root_startup
                .as_ref()
                .is_some_and(|latch| *latch.lock() != RootStartupState::Activated)
        }) {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "root startup has not released provider attachment".into(),
            ));
        }
        let owner = records
            .get(&actor)
            .and_then(|record| record.public_owner.ready())
            .cloned()
            .ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    "actor publication plane is not ready for provider attachment".into(),
                )
            })?;
        drop(records);
        let admission = Arc::new(ActorProviderAdmission { owner });
        self.validate_provider_attachment(&admission)?;
        Ok(admission)
    }

    /// Recheck after external setup waits and before binding or readiness.
    pub fn validate_provider_attachment(
        &self,
        admission: &ActorProviderAdmission,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let actor = admission.actor();
        let refuse = || {
            ResidentActorWorkbenchError::ActorProtocol(
                "provider admission no longer matches the live actor owner".into(),
            )
        };
        let context = self.directory.session_context(actor).ok_or_else(refuse)?;
        if self
            .environment
            .actors
            .lock()
            .get(&actor)
            .is_some_and(|record| {
                record
                    .root_startup
                    .as_ref()
                    .is_some_and(|latch| *latch.lock() != RootStartupState::Activated)
            })
        {
            return Err(refuse());
        }
        if !admission.owner.matches_context(&context)
            || self.directory.resolve(actor).is_none_or(|actor| {
                actor.terminal().get().is_some() || actor.terminal().requested_shutdown().is_some()
            })
        {
            return Err(refuse());
        }
        let records = self.environment.actors.lock();
        Self::validate_provider_lineage(actor, &records)?;
        let record = records.get(&actor).ok_or_else(refuse)?;
        if record.terminal.is_some()
            || record.descriptor.placement() != context.placement
            || record
                .public_owner
                .ready()
                .is_none_or(|owner| !Arc::ptr_eq(owner, &admission.owner))
        {
            return Err(refuse());
        }
        Ok(())
    }

    fn validate_provider_lineage(
        actor: ActorRef,
        records: &std::collections::HashMap<ActorRef, ResidentActorRecord>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let record = records.get(&actor).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "provider actor has no registered lineage admission".into(),
            )
        })?;
        if record.descriptor.persistence_policy() == crate::ActorPersistencePolicy::Ephemeral
            && record.descriptor.creator().is_some_and(|parent| {
                records.get(&parent).is_none_or(|parent| {
                    parent.descriptor.persistence_policy() == crate::ActorPersistencePolicy::Durable
                })
            })
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "provider child requires explicit durable lineage admission before attachment"
                    .into(),
            ));
        }
        Ok(())
    }

    fn root_recovery_context(
        &self,
        expected: &crate::RootRecoveryPlacement,
    ) -> Result<ActorSessionContext, ResidentActorWorkbenchError> {
        let current = self.root_recovery_placement(expected.actor())?;
        let context = self
            .directory
            .session_context(expected.actor())
            .ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    "root recovery placement was retired".into(),
                )
            })?;
        if current.owner() != expected.owner()
            || current.placement() != expected.placement()
            || context.actor != expected.actor()
            || context.placement != expected.placement()
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "root recovery placement changed before readiness".into(),
            ));
        }
        Ok(context)
    }

    /// Whether `session` is still live in the shared machine registry — for
    /// asserting a dedicated child session's teardown
    /// (`ResidentActorRunner::retire_child_session`) actually happened,
    /// without reaching into any crate-private state.
    #[must_use]
    pub fn session_state_of(
        &self,
        session: tidepool_repr::SessionId,
    ) -> tidepool_runtime::session::ResidentSessionState {
        self.environment.runner.resident_session_state(session)
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

    /// Observe the machine actually mounted for this actor incarnation.
    /// Unknown placements and running or retired machines have no snapshot.
    #[must_use]
    pub fn measurement_snapshot_for(
        &self,
        actor: ActorRef,
    ) -> Option<crate::resident_workbench::ResidentMachineMeasurement> {
        self.actor_session(actor)
            .and_then(|session| self.environment.runner.measurement_snapshot(session))
    }

    pub fn new(
        source: ActorWorkbenchSource,
        session: tidepool_repr::SessionId,
        machine: ResidentSession<H, O>,
        fork_workspaces: Option<crate::fork_workspace::SharedWorkspaceAdmission>,
        incarnation: crate::Incarnation,
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
            actor_admissions: crate::ActorAdmissionRegistry::new(),
            actors: Arc::new(Mutex::new(std::collections::HashMap::new())),
            fork_workspaces,
            root_admission_closed: Arc::new(tokio::sync::RwLock::new(false)),
            source_layers: None,
            jev: Arc::new(crate::jev::UnconfiguredJev),
            cell_model_factory: None,
            form_host: None,
            form_registry: Arc::new(Mutex::new(Default::default())),
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

    /// The request presented to `actor` that it has not begun replying to,
    /// while it waits on nothing it owns. The facade reminds an actor whose
    /// provider turn went idle with this request still open.
    #[must_use]
    pub fn open_request_without_reply(&self, actor: ActorRef) -> Option<crate::RequestId> {
        self.environment.requests.open_without_reply(actor)
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
        self.actor_graph(Some(requester))
    }

    /// Host-owned observation of this forest, including surviving children and
    /// terminal actors after its original root has stopped.
    pub fn inspect_host_graph(&self) -> Vec<ActorGraphNode> {
        self.actor_graph(None).unwrap_or_default()
    }

    fn actor_graph(&self, requester: Option<ActorRef>) -> Option<Vec<ActorGraphNode>> {
        // Request publication may hold request state while updating actor metadata.
        // Observation retains a snapshot before consulting that request owner.
        let records = self.environment.actors.lock().clone();
        if requester.is_some_and(|requester| !records.contains_key(&requester)) {
            return None;
        }
        let mut nodes = records
            .iter()
            .filter(|(actor, _)| {
                requester.is_none_or(|requester| actor_can_observe(requester, **actor, &records))
            })
            .map(|(actor, record)| {
                let runtime = record.runtime_observation.snapshot();
                let (active_requests, queued_requests) =
                    self.environment.requests.work_for_target(*actor);
                ActorGraphNode {
                    actor: *actor,
                    label: record.descriptor.display_label().into_owned(),
                    model_actor: record.interactive_policy_installed,
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
        role: crate::ActorCapabilities,
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
        role: crate::ActorCapabilities,
        compiled: Arc<tidepool_runtime::session::CompiledTurn>,
        worktree_custody: Option<Arc<dyn crate::WorkspaceCustody>>,
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
        let mut descriptor = ActorDescriptor::new_optional(durable.label.clone(), placement)
            .with_capabilities(role)
            .with_model(durable.model.clone().map(crate::Model::Literal))
            .with_instructions(durable.instructions.clone())
            .with_fork_effort(durable.effort)
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
                if crate::local_actor::startup_cleanup(&error).is_none() {
                    self.environment
                        .runner
                        .retire_root_placement(placement)
                        .await?;
                }
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
        role: crate::ActorCapabilities,
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
        role: crate::ActorCapabilities,
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
                ActorDescriptor::new(label, placement).with_capabilities(role),
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
                if crate::local_actor::startup_cleanup(&error).is_none() {
                    self.environment
                        .runner
                        .retire_root_placement(placement)
                        .await?;
                }
                Err(Box::new(error))
            }
        }
    }

    /// Fence every admitted member before draining run and root-owned cleanup.
    /// A forced actor stop remains unconfirmed even after its scheduler task exits.
    pub async fn shutdown(&self) -> Vec<crate::ForestRootShutdown> {
        let retirement = self.directory.seal().cancel_all(ActorTerminal::new(
            ActorExitKind::Cancelled,
            "forest host shutdown",
        ));
        *self.environment.root_admission_closed.write().await = true;
        self.directory.close_run_admission().await;
        let roots = retirement.into_roots();
        let mut outcomes = Vec::with_capacity(roots.len() + 1);
        outcomes.push(crate::ForestRootShutdown::RunResources(
            self.directory.shutdown_run_resources().await,
        ));
        for root in roots {
            outcomes.push(shutdown_forest_root(&root, std::time::Duration::from_secs(30)).await);
        }
        outcomes
    }

    /// Provision a host-authorized workbench without a provider attachment.
    pub async fn new_workbench(
        &self,
        label: String,
        role: crate::ActorCapabilities,
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
        let descriptor = ActorDescriptor::new(label, placement).with_capabilities(role);
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
                if crate::local_actor::startup_cleanup(&error).is_none() {
                    self.environment
                        .runner
                        .retire_root_placement(placement)
                        .await?;
                }
                Err(Box::new(error))
            }
        }
    }

    /// Admit a prepared independent root. Its continuation and scopes must have
    /// been prepared in this forest's machine, just as for a child entry.
    ///
    /// A retained startup executable requires the pending transaction API.
    ///
    /// ```compile_fail,E0308
    /// use exomonad_actor::{ActorDescriptor, ResidentForest};
    /// use tidepool_effect::dispatch::DispatchEffect;
    /// use tidepool_runtime::session::{OutputSink, PreparedStartupEntry};
    /// async fn rejects_startup<H, O>(
    ///     forest: &ResidentForest<H, O>, descriptor: ActorDescriptor,
    ///     entry: PreparedStartupEntry,
    /// ) where H: DispatchEffect<O> + Send + 'static, O: OutputSink + Sync + 'static {
    ///     forest.admit_root(descriptor, entry).await;
    /// }
    /// ```
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

    /// Admit an initial durable root with the directory's original reserved identity.
    /// An already executed outcome cannot enter the pending startup transaction.
    ///
    /// ```compile_fail,E0308
    /// use exomonad_actor::{ActorDescriptor, ResidentForest, RootStartupIntent};
    /// use tidepool_effect::dispatch::DispatchEffect;
    /// use tidepool_runtime::session::{OutputSink, ResidentOutcome};
    /// async fn rejects_preexecuted<H, O>(
    ///     forest: &ResidentForest<H, O>, descriptor: ActorDescriptor,
    ///     outcome: ResidentOutcome, intent: RootStartupIntent,
    /// ) where H: DispatchEffect<O> + Send + 'static, O: OutputSink + Sync + 'static {
    ///     forest.admit_pending_root(descriptor, outcome, move |_| intent).await;
    /// }
    /// ```
    pub async fn admit_pending_root<F>(
        &self,
        descriptor: ActorDescriptor,
        entry: tidepool_runtime::session::PreparedStartupEntry,
        intent: F,
    ) -> Result<
        (
            LocalActorRef,
            ractor::concurrency::JoinHandle<()>,
            RootStartupRelease,
        ),
        ractor::SpawnErr,
    >
    where
        F: FnOnce(ActorRef) -> crate::RootStartupIntent + Send,
    {
        self.admit_pending_root_inner(descriptor, entry, None, intent)
            .await
    }

    /// Register a durable root and retain its original boot without executing it.
    /// The admission and exact startup intent are one fsynced journal row.
    pub async fn admit_pending_root_with_identity(
        &self,
        descriptor: ActorDescriptor,
        entry: tidepool_runtime::session::PreparedStartupEntry,
        identity: ActorRef,
        intent: crate::RootStartupIntent,
    ) -> Result<
        (
            LocalActorRef,
            ractor::concurrency::JoinHandle<()>,
            RootStartupRelease,
        ),
        ractor::SpawnErr,
    > {
        self.admit_pending_root_inner(descriptor, entry, Some(identity), move |_| intent)
            .await
    }

    async fn admit_pending_root_inner<F>(
        &self,
        descriptor: ActorDescriptor,
        entry: tidepool_runtime::session::PreparedStartupEntry,
        identity: Option<ActorRef>,
        intent: F,
    ) -> Result<
        (
            LocalActorRef,
            ractor::concurrency::JoinHandle<()>,
            RootStartupRelease,
        ),
        ractor::SpawnErr,
    >
    where
        F: FnOnce(ActorRef) -> crate::RootStartupIntent + Send,
    {
        let admission = self.environment.root_admission_closed.read().await;
        let refuse =
            |detail: &str| ractor::SpawnErr::StartupFailed(std::io::Error::other(detail).into());
        if *admission || self.environment.recovery.is_none() {
            return Err(refuse(
                "pending root requires an open forest and durable recovery journal",
            ));
        }
        if descriptor.placement().session != self.session
            || descriptor.persistence_policy() != crate::ActorPersistencePolicy::Durable
            || descriptor.actor_path().is_none()
            || descriptor.creator().is_some()
            || descriptor.supervisor_parent().is_some()
            || descriptor.context_parent().is_some()
            || descriptor.checkpoint_boundary().is_some()
        {
            return Err(refuse(
                "pending root requires its exact independent durable placement",
            ));
        }
        let placement = descriptor.placement();
        let latch = Arc::new(Mutex::new(RootStartupState::Pending));
        let mut original_intent = None;
        let build = |identity| {
            let intent = intent(identity);
            original_intent = Some(intent.clone());
            let mut behavior = ResidentKernelBehavior::with_boot(
                descriptor,
                self.environment.clone(),
                ResidentBoot::Startup(entry),
                Vec::new(),
            );
            behavior.root_startup = Some((intent, Arc::clone(&latch)));
            behavior
        };
        let (actor, task) = match identity {
            Some(identity) => {
                crate::local_actor::spawn_local_actor_in_directory_with_identity(
                    None,
                    build(identity),
                    identity,
                    self.directory.clone(),
                )
                .await?
            }
            None => {
                crate::local_actor::spawn_local_actor_in_directory_with_factory(
                    None,
                    self.incarnation,
                    self.directory.clone(),
                    build,
                )
                .await?
            }
        };
        let identity = actor.identity();
        Ok((
            actor,
            task,
            RootStartupRelease {
                actor: identity,
                placement,
                latch,
                intent: original_intent
                    .expect("reserved actor admission constructed startup intent"),
            },
        ))
    }

    /// Release only the original registered boot after durable manifest and
    /// exact backend binding. Repeated acknowledgments never rerun initialization.
    pub fn release_root_startup(
        &self,
        release: &RootStartupRelease,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let refuse = |detail: &str| ResidentActorWorkbenchError::ActorProtocol(detail.into());
        let placement = self.root_recovery_placement(release.actor)?;
        if placement.placement() != release.placement {
            return Err(refuse("root startup placement changed"));
        }
        let records = self.environment.actors.lock();
        if records.get(&release.actor).is_none_or(|record| {
            record.public_owner.ready().is_none()
                || record
                    .root_startup
                    .as_ref()
                    .is_none_or(|latch| !Arc::ptr_eq(latch, &release.latch))
        }) {
            return Err(refuse("root startup manifest durability is unconfirmed"));
        }
        drop(records);
        let journal = self
            .environment
            .recovery
            .as_ref()
            .ok_or_else(|| refuse("root startup journal is absent"))?;
        let durable = journal
            .validated_records()
            .map_err(|error| refuse(&error.to_string()))?;
        let record = durable
            .iter()
            .find(|record| record.admission.actor == release.actor)
            .ok_or_else(|| refuse("root startup durable admission is absent"))?;
        if record.terminal.is_some()
            || record.startup.as_ref() != Some(&release.intent)
            || record
                .application
                .as_ref()
                .and_then(|application| application.conversation.as_ref())
                != Some(&release.intent.conversation)
        {
            return Err(refuse(
                "root startup requires its exact durable ApplicationBound",
            ));
        }
        let actor = self
            .directory
            .resolve(release.actor)
            .ok_or_else(|| refuse("root startup actor is unavailable"))?;
        let mut state = release.latch.lock();
        if *state == RootStartupState::Pending {
            *state = RootStartupState::Released;
        }
        if *state != RootStartupState::Activated {
            actor
                .address()
                .send_message(crate::KernelMessage::Resume {
                    kind: crate::kernel::KernelResume::ReleaseRootStartup,
                })
                .map_err(|error| refuse(&error.to_string()))?;
        }
        Ok(())
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
        // Keep the composed startup future out of each forwarding caller's
        // inline future. It still runs in this task under the admission guard.
        Box::pin(async move {
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
        })
        .await
    }
}

fn completed_terminal() -> ActorTerminal {
    ActorTerminal::new(ActorExitKind::Completed, "completed")
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
            value: None,
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
                let index = checked
                    .map(|analysis| cell_diagnostic_item_index(diagnostic, analysis))
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

fn cell_diagnostic_item_index(
    diagnostic: &tidepool_toolchain::diag::ExtractDiag,
    items: &[tidepool_runtime::session::CellAnalysisItem],
) -> usize {
    diagnostic
        .span
        .as_ref()
        .filter(|span| span.file == "<cell>")
        .and_then(|span| {
            items.iter().position(|item| {
                item.source_items.iter().any(|source| {
                    let start = (source.span.start_line, source.span.start_column);
                    let end = (source.span.end_line, source.span.end_column);
                    let point = (span.start_line as usize, span.start_col as usize);
                    start <= point && point <= end
                })
            })
        })
        .unwrap_or(0)
}

fn committed_declaration_warnings(
    checked: &tidepool_runtime::session::CellCheck,
    index: usize,
) -> (
    Vec<String>,
    Vec<tidepool_toolchain::diag::StructuredDiagnostic>,
) {
    if checked.items[index].verdict.kind != TurnKind::Decl {
        return (Vec::new(), Vec::new());
    }
    let mut warnings = Vec::new();
    let mut diagnostics = Vec::new();
    for warning in &checked.warnings {
        if cell_diagnostic_item_index(warning, &checked.items) != index {
            continue;
        }
        let rendered = tidepool_runtime::session::render_cell_compile_rejection(
            &tidepool_runtime::CompileError::Diagnostics(vec![warning.clone()]),
            &checked.checked_cell_text,
        );
        warnings.push(rendered.output);
        diagnostics.extend(rendered.diagnostics);
    }
    (warnings, diagnostics)
}

fn receipt_kind(kind: TurnKind) -> WorkbenchCellItemKind {
    match kind {
        TurnKind::Decl => WorkbenchCellItemKind::Declaration,
        TurnKind::Bind => WorkbenchCellItemKind::Statement,
        TurnKind::Expr => WorkbenchCellItemKind::Expression,
    }
}

fn annotate_workbench_receipts(
    items: &mut [WorkbenchItemReceipt],
    cell_check: Option<&[tidepool_runtime::session::CellAnalysisItem]>,
) {
    if let Some(checked) = cell_check {
        for receipt in items {
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
}

fn workbench_response(
    status: WorkbenchRunStatus,
    mut items: Vec<WorkbenchItemReceipt>,
    next_index: usize,
    total: usize,
    cell_check: Option<&[tidepool_runtime::session::CellAnalysisItem]>,
) -> WorkbenchResponse {
    annotate_workbench_receipts(&mut items, cell_check);
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
            value: None,
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
        publication: None,
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

/// The one publication path for request-registry notices: watch transitions,
/// then every queued settlement. The resident actor calls it after each
/// registry transition it makes; a command completion task calls it after
/// settling a job outside any actor turn.
pub(crate) async fn publish_request_notifications(
    requests: &RequestRegistry,
    deployments: &mpsc::Sender<LocalResidentDeployment>,
    notifications: impl IntoIterator<Item = crate::request::WatchNotification>,
) {
    for notification in notifications {
        // A watch forgotten by campaign cleanup keeps no state to poll.
        // Delivering a transition for it would send its owner to
        // `pollWatch`, which can only answer `WatchUnavailable
        // (WatchRejected ReplyStale)` — a notice about nothing.
        if !requests.retains_watch(notification.owner, notification.watch) {
            continue;
        }
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
    if !requests.has_settlement_notifications() {
        return;
    }
    let _publication = requests.lock_settlement_publication().await;
    while requests.has_settlement_notifications() {
        // Keep the exact notice in the registry while capacity is unavailable.
        // The publication lock prevents another publisher from claiming it
        // between reservation and this synchronous send.
        let permit = match deployments.reserve().await {
            Ok(permit) => permit,
            Err(_closed) => {
                tracing::warn!(
                    kind = "settlement_changed",
                    outcome = "closed",
                    "publishing settlement notice: deployment observer channel has no consumer"
                );
                break;
            }
        };
        let Some(notification) = requests.take_next_settlement_notification() else {
            break;
        };
        let owner = notification.owner;
        let request = notification.request;
        let preview_len = notification.reply_preview.as_ref().map(String::len);
        permit.send(LocalResidentDeployment::SettlementChanged { notification });
        tracing::info!(
            actor = ?owner,
            request = ?request,
            kind = "settlement_changed",
            preview_len,
            outcome = "sent",
            "publishing settlement notice"
        );
    }
}

pub(crate) async fn shutdown_forest_root(
    root: &LocalActorRef,
    grace: std::time::Duration,
) -> crate::ForestRootShutdown {
    let result = tokio::time::timeout(
        grace,
        root.shutdown_with_cleanup(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "forest host shutdown".into(),
            diagnostic: None,
        }),
    )
    .await;
    match result {
        Ok(Ok(shutdown)) => crate::ForestRootShutdown::Settled(shutdown),
        result => {
            root.address().kill();
            match result {
                Ok(Err(cause)) => crate::ForestRootShutdown::Failed {
                    actor: root.identity(),
                    cause,
                },
                Err(_) => crate::ForestRootShutdown::TimedOut {
                    actor: root.identity(),
                },
                Ok(Ok(_)) => unreachable!(),
            }
        }
    }
}

async fn tracked_stopped_projection(
    actor: ActorRef,
    deployments: &mpsc::Sender<LocalResidentDeployment>,
    grace: std::time::Duration,
) -> AgentStopProjection {
    let (request, release) = ReleaseAwait::channel(actor);
    match deployments.try_send(LocalResidentDeployment::ReleaseAwait(request)) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            return AgentStopProjection::StoppedRetaining(
                "host release observation unavailable: lifecycle channel full".into(),
            );
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            return AgentStopProjection::StoppedRetaining(
                "host release observation unavailable: lifecycle channel closed".into(),
            );
        }
    }
    match tokio::time::timeout(grace, release).await {
        Ok(Ok(ResourceRelease::Released)) => AgentStopProjection::StoppedNow,
        Ok(Ok(ResourceRelease::Retained(detail))) => AgentStopProjection::StoppedRetaining(detail),
        Ok(Err(_)) => AgentStopProjection::StoppedRetaining(
            "host release acknowledgement lost; cleanup unconfirmed".into(),
        ),
        Err(_) => AgentStopProjection::StoppedReleasing,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        checkpoint_capture_delivered, disposition_for_non_command_failure,
        failed_checkpoint_cleanup_response, lookup_response, settlement_refusal,
        workbench_failure_after_operations, workbench_failure_after_unit, workbench_response,
        ChildExitObservations,
    };

    #[test]
    fn native_context_authority_requires_explicit_effect_and_exact_sync_invocation() {
        use exomonad_tool::{
            CustomToolDeclaration, HostedTool, ToolEffectKey, ToolImplementation,
            ToolInvocationContext, ToolScheduling,
        };
        let actor = crate::ActorRef::first(crate::ActorId(8));
        let mut declaration = CustomToolDeclaration {
            name: "ordinary-notebook".into(),
            description: "Selected notebook".into(),
            schedule: ToolScheduling::BeforeNextInference,
            implementation: ToolImplementation::HaskellCell,
            effect_keys: Vec::new(),
        };
        let exact = ToolInvocationContext::external(
            "thread".into(),
            "request".into(),
            "call".into(),
            Some("call".into()),
            None,
        );
        let direct = ToolInvocationContext::external(
            "thread".into(),
            "request".into(),
            "call".into(),
            None,
            None,
        );
        let rejected = |tool: Option<&HostedTool>, invocation: Option<&ToolInvocationContext>| {
            assert!(matches!(
                super::admit_context_authority(actor, tool, invocation),
                Err(crate::KernelInvocationFailure::Rejected { actor: rejected, .. })
                    if rejected == actor
            ));
        };
        rejected(None, Some(&exact));
        rejected(Some(&HostedTool::Custom(declaration.clone())), Some(&exact));
        declaration
            .effect_keys
            .push(ToolEffectKey::ContextReadWrite);
        declaration.name = "explicit-curation".into();
        let curation = HostedTool::Custom(declaration.clone());
        assert!(super::admit_context_authority(actor, Some(&curation), Some(&exact)).is_ok());
        rejected(Some(&curation), None);
        rejected(Some(&curation), Some(&direct));
        declaration.schedule = ToolScheduling::Async;
        rejected(Some(&HostedTool::Custom(declaration)), Some(&exact));
    }

    #[test]
    fn checkpoint_answer_remains_delivered_when_resumed_computation_fails() {
        let after_delivery: Result<(), crate::ResidentActorWorkbenchError> = Err(
            crate::ResidentActorWorkbenchError::Delivered(ResidentError::ForeignCustody),
        );
        let before_delivery: Result<(), crate::ResidentActorWorkbenchError> = Err(
            crate::ResidentActorWorkbenchError::Resident(ResidentError::ForeignCustody),
        );
        assert!(checkpoint_capture_delivered(&after_delivery));
        assert!(!checkpoint_capture_delivered(&before_delivery));
    }
    use crate::resident_workbench::AgentStopProjection;
    use crate::{ActorId, ActorRef, Incarnation, ResidentActorWorkbenchError};
    use tidepool_runtime::session::{
        CellAnalysisItem, CellAnalysisSourceItem, CellCheck, CellSourceSpan, InfoEntry,
        InspectionAvailability, InspectionResult, ResidentError, TurnClassification, TurnKind,
        TypeMatch, TypeMatchQuality, WorkbenchCellItemKind, WorkbenchExecutionId,
        WorkbenchFailureLayer, WorkbenchFailurePoint, WorkbenchItemReceipt, WorkbenchItemStatus,
        WorkbenchOperationDisposition, WorkbenchOperationId, WorkbenchOperationReceipt,
        WorkbenchResponse, WorkbenchRunStatus, WorkbenchTerminalTransfer,
    };

    #[test]
    fn failed_checkpoint_cleanup_keeps_original_receipts_and_cancellation() {
        let receipt = WorkbenchItemReceipt {
            diagnostics: Vec::new(),
            index: 2,
            kind: None,
            span: None,
            source_items: Vec::new(),
            status: WorkbenchItemStatus::Committed,
            output: "effect already completed".into(),
            value: None,
            warnings: Vec::new(),
            installed_bindings: Vec::new(),
            operations: Vec::new(),
            terminal_transfer: Some(WorkbenchTerminalTransfer::CancellationAcknowledged),
            failure_layer: None,
        };
        let failure = failed_checkpoint_cleanup_response(
            WorkbenchResponse {
                status: WorkbenchRunStatus::RequestCancelled,
                publication: None,
                summary: None,
                items: vec![receipt.clone()],
                next_index: 3,
                total: 5,
            },
            "injected scope checkout failure".into(),
        );
        assert_eq!(failure.receipts, vec![receipt]);
        assert_eq!(
            failure.point,
            WorkbenchFailurePoint::Finalization {
                completed_input_units: 3
            }
        );
        assert_eq!(failure.total, 5);
        let detail = failure.source.to_string();
        assert!(detail.contains("injected scope checkout failure"));
        assert!(detail.contains("RequestCancelled"));
        assert!(detail.contains("next index 3"));
    }

    #[test]
    fn synthetic_route_boundary_cannot_issue_provider_checkpoint() {
        let boundary = tidepool_runtime::session::ContextCheckpointBoundary::Route {
            actor_id: 1,
            incarnation: 1,
            watch_id: 9,
        };
        assert!(super::CheckpointPublication::Route(boundary.clone())
            .hosted_boundary()
            .is_none());
        assert!(super::CheckpointPublication::Workbench {
            boundary: Some(boundary),
            capture: None,
        }
        .hosted_boundary()
        .is_none());
        let direct = tidepool_runtime::session::ContextCheckpointBoundary::Execution {
            actor_id: 1,
            incarnation: 1,
            execution_id: WorkbenchExecutionId::from_digest([7; 16]),
        };
        assert!(super::CheckpointPublication::Workbench {
            boundary: Some(direct),
            capture: None
        }
        .hosted_boundary()
        .is_none());
        let hosted = tidepool_runtime::session::ContextCheckpointBoundary::external(
            "thread".into(),
            "request".into(),
            "call".into(),
        );
        assert!(super::CheckpointPublication::Workbench {
            boundary: Some(hosted),
            capture: None
        }
        .hosted_boundary()
        .is_some());
    }

    #[tokio::test]
    async fn tracked_release_observation_full_and_closed_channels_retain_uncertainty() {
        let actor = ActorRef::first(ActorId(2));
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let (request, _) = super::ReleaseAwait::channel(actor);
        sender
            .try_send(super::LocalResidentDeployment::ReleaseAwait(request))
            .unwrap();
        assert!(matches!(
            super::tracked_stopped_projection(actor, &sender, std::time::Duration::from_secs(1))
                .await,
            AgentStopProjection::StoppedRetaining(_)
        ));
        receiver.close();
        assert!(matches!(
            super::tracked_stopped_projection(actor, &sender, std::time::Duration::from_secs(1))
                .await,
            AgentStopProjection::StoppedRetaining(_)
        ));
    }

    #[tokio::test]
    async fn tracked_release_observation_requires_exact_acknowledgement() {
        let actor = ActorRef::first(ActorId(2));
        for release in [
            Some(super::ResourceRelease::Released),
            Some(super::ResourceRelease::Retained("cleanup failed".into())),
            None,
        ] {
            let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
            let observation = super::tracked_stopped_projection(
                actor,
                &sender,
                std::time::Duration::from_secs(1),
            );
            let host = async {
                let Some(super::LocalResidentDeployment::ReleaseAwait(request)) =
                    receiver.recv().await
                else {
                    panic!("missing wait");
                };
                assert_eq!(request.actor, actor);
                if let Some(release) = release.clone() {
                    assert!(request.answer(release));
                }
            };
            let (outcome, ()) = tokio::join!(observation, host);
            match release {
                Some(super::ResourceRelease::Released) => {
                    assert!(matches!(outcome, AgentStopProjection::StoppedNow))
                }
                Some(super::ResourceRelease::Retained(detail)) => assert!(
                    matches!(outcome, AgentStopProjection::StoppedRetaining(actual) if actual == detail)
                ),
                None => assert!(matches!(outcome, AgentStopProjection::StoppedRetaining(_))),
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn tracked_release_observation_timeout_remains_releasing() {
        let actor = ActorRef::first(ActorId(2));
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let observation =
            super::tracked_stopped_projection(actor, &sender, std::time::Duration::from_secs(1));
        let host = async {
            let Some(super::LocalResidentDeployment::ReleaseAwait(request)) = receiver.recv().await
            else {
                panic!("missing wait");
            };
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            assert!(
                !request.answer(super::ResourceRelease::Released),
                "timed-out waiter must not become confirmation"
            );
        };
        let (outcome, ()) = tokio::join!(observation, host);
        assert!(matches!(outcome, AgentStopProjection::StoppedReleasing));
    }

    /// A reply fenced by an update still in delivery tells the model which
    /// request is waiting, that queued messages appear only at a turn end, and that an earlier
    /// retry fails the same way; it never shows the bare error constructor.
    #[test]
    fn update_pending_refusal_says_where_the_update_arrives() {
        let text = settlement_refusal(
            "reply",
            crate::RequestId(2),
            crate::request::ReplyError::UpdatePending,
        );
        assert!(text.starts_with("reply not settled: "), "{text}");
        assert!(text.contains("update to request 2"), "{text}");
        assert!(text.contains("end your turn now"), "{text}");
        assert!(text.contains("refused the same way"), "{text}");
        assert!(text.ends_with("Nothing was sent."), "{text}");
        assert!(!text.contains("UpdatePending"), "{text}");
    }

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
            running_jobs: vec![crate::request::PendingResponseAge {
                id: crate::RequestId(8),
                label: "job-1".into(),
                registered_at_unix_ms: 1_040_000,
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
        assert!(rendered.contains("running jobs (1 total):"));
        assert!(rendered.contains("job job-1: started +0m40s into your session"));
        assert!(!rendered.contains("request 8"));
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
                running_jobs: Vec::new(),
            },
        );
        assert!(empty.contains("(none registered)"));
        assert!(empty.contains("(none pending)"));
        assert!(empty.contains("pollWatch"));
    }

    #[derive(Clone, Default)]
    pub(super) struct CapturedWriter(pub(super) std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    pub(super) struct CapturedGuard(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

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
                diagnostic: None,
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
                diagnostic: None,
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
                    diagnostic: None,
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
                            diagnostic: None,
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
                    value: None,
                    warnings: Vec::new(),
                    installed_bindings: Vec::new(),
                    operations: vec![WorkbenchOperationReceipt {
                        display_publication: None,
                        display: None,
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
    fn preparation_infrastructure_and_cancellation_do_not_become_source_rejections() {
        let expected =
            tidepool_toolchain::classify_compile(&tidepool_runtime::CompileError::MissingOutput(
                std::path::PathBuf::from("controlled-missing-output"),
            ));
        for error in [
            ResidentActorWorkbenchError::CompileInfrastructure(expected.clone()),
            ResidentActorWorkbenchError::InvocationCancelled,
        ] {
            let mut request = tidepool_runtime::session::WorkbenchRequest::from_cell_input(":}");
            let mut cursor = super::WorkbenchCursor::default();
            let failure = super::install_cell_preparation(&mut request, &mut cursor, Err(error))
                .expect_err("non-source failure must retain its failure boundary");
            assert!(failure.receipts.is_empty());
            assert!(failure.publication.is_none());
            assert!(request.items.is_empty());
            assert_eq!(request.cell_source(), Some(":}"));
            assert!(!cursor.preparation_done);
            assert!(cursor.prepared_cell.is_none());
            match failure.source {
                ResidentActorWorkbenchError::CompileInfrastructure(diagnostic) => {
                    assert_eq!(diagnostic, expected)
                }
                ResidentActorWorkbenchError::InvocationCancelled => {}
                _ => panic!("non-source failure changed kind"),
            }
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(96))]
        #[test]
        fn source_rejection_without_plan_preserves_diagnostics_and_output_budget(
            plan in 0u8..3,
            location in 0u8..3,
            diagnostic_count in 0usize..4,
            repetitions in 0usize..35000,
        ) {
            use proptest::prelude::*;
            use tidepool_toolchain::diag::{DiagSpan, DiagnosticLocation, DiagnosticSeverity, ExtractDiag};
            let message = format!("parse witness\n{}\nlast witness", "λ🙂\n".repeat(repetitions));
            let diagnostics = (0..diagnostic_count).map(|_| ExtractDiag {
                span: (location != 0).then(|| DiagSpan {
                    file: if location == 1 { "<cell>" } else { "Library.hs" }.into(),
                    start_line: 2, start_col: 3, end_line: 2, end_col: 4,
                }),
                severity: DiagnosticSeverity::Error,
                message: message.clone(),
            }).collect::<Vec<_>>();
            let items = match plan {
                0 => None,
                1 => Some(Vec::new()),
                _ => Some(vec![CellAnalysisItem {
                    span: CellSourceSpan { start_line: 1, start_column: 1, end_line: 2, end_column: 5 },
                    source: "-- λ\n:}".into(), prologue_only: false,
                    verdict: TurnClassification { kind: TurnKind::Expr, binders: Vec::new(), items: Vec::new() },
                    source_items: Vec::new(),
                }]),
            };
            let response = super::cell_check_rejection(tidepool_runtime::session::CellCheckFailure {
                error: tidepool_runtime::CompileError::Diagnostics(diagnostics), items,
            }, "-- λ\n:}");
            prop_assert_eq!(response.status, WorkbenchRunStatus::Rejected);
            prop_assert_eq!(response.items.len(), 1);
            prop_assert_eq!(response.next_index, 0);
            let receipt = &response.items[0];
            prop_assert_eq!(receipt.diagnostics.len(), diagnostic_count);
            prop_assert!(receipt.output.len() <= 65536);
            prop_assert!(receipt.installed_bindings.is_empty());
            prop_assert!(receipt.operations.is_empty());
            for diagnostic in &receipt.diagnostics {
                prop_assert!(diagnostic.message.contains("parse witness"));
                prop_assert!(diagnostic.message.contains("last witness"));
                match (&diagnostic.location, location) {
                    (DiagnosticLocation::Unlocated, 0) => {},
                    (DiagnosticLocation::Authored { label, start_line, start_col, .. }, 1) => {
                        prop_assert_eq!(label, "<cell>"); prop_assert_eq!(*start_line, 2); prop_assert_eq!(*start_col, 3);
                    },
                    (DiagnosticLocation::Foreign { file, start_line, .. }, 2) => {
                        prop_assert_eq!(file, "Library.hs"); prop_assert_eq!(*start_line, 2);
                    },
                    _ => prop_assert!(false, "diagnostic source identity changed"),
                }
            }
            if diagnostic_count != 0 {
                prop_assert_eq!(receipt.status, WorkbenchItemStatus::Rejected);
                prop_assert_eq!(receipt.failure_layer, Some(WorkbenchFailureLayer::Compile));
                prop_assert!(receipt.output.contains("parse witness"));
                prop_assert!(receipt.output.contains("last witness"));
            }
        }
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
        let checked: CellCheck = tidepool_runtime::session::turn::CellCheckObservations {
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
            warnings: Vec::new(),
        }
        .into();
        let committed = WorkbenchItemReceipt {
            diagnostics: Vec::new(),
            index: 0,
            kind: None,
            span: None,
            source_items: Vec::new(),
            status: WorkbenchItemStatus::Committed,
            output: "[bound prior]".into(),
            value: None,
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
                    value: None,
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
    fn committed_declaration_receipt_keeps_authored_warning_as_diagnostic() {
        use tidepool_toolchain::diag::{
            DiagSpan, DiagnosticLevel, DiagnosticLocation, DiagnosticSeverity, ExtractDiag,
        };

        let span = CellSourceSpan {
            start_line: 2,
            start_column: 1,
            end_line: 2,
            end_column: 45,
        };
        let checked: CellCheck = tidepool_runtime::session::turn::CellCheckObservations {
            prologue: Default::default(),
            items: vec![CellAnalysisItem {
                span,
                source: "request = MergeRequest { mergeSourceHead = 1 }".into(),
                prologue_only: false,
                verdict: TurnClassification {
                    kind: TurnKind::Decl,
                    binders: vec!["request".into()],
                    items: Vec::new(),
                },
                source_items: vec![CellAnalysisSourceItem {
                    ordinal: 0,
                    span,
                    kind: TurnKind::Decl,
                }],
            }],
            pins: Vec::new(),
            checked_source: String::new(),
            checked_cell_text: "data MergeRequest = MergeRequest { mergeSourceHead :: Int, mergeSourceWorktree :: Int }\nrequest = MergeRequest { mergeSourceHead = 1 }\n".into(),
            compile_generation: 0,
            compile_view_evidence: String::new(),
            expression_plans: Vec::new(),
            warnings: vec![ExtractDiag {
                span: Some(DiagSpan {
                    file: "<cell>".into(),
                    start_line: 2,
                    start_col: 11,
                    end_line: 2,
                    end_col: 23,
                }),
                severity: DiagnosticSeverity::Warning,
                message: "Fields of ‘MergeRequest’ not initialised: mergeSourceWorktree".into(),
            }],
        }.into();
        let (warnings, diagnostics) = super::committed_declaration_warnings(&checked, 0);
        let response = workbench_response(
            WorkbenchRunStatus::Committed,
            vec![WorkbenchItemReceipt {
                diagnostics,
                index: 0,
                kind: None,
                span: None,
                source_items: Vec::new(),
                status: WorkbenchItemStatus::Committed,
                output: "[bound request]".into(),
                value: None,
                warnings,
                installed_bindings: vec!["request".into()],
                operations: Vec::new(),
                terminal_transfer: None,
                failure_layer: None,
            }],
            1,
            1,
            Some(&checked.items),
        );
        assert_eq!(response.status, WorkbenchRunStatus::Committed);
        let receipt = &response.items[0];
        assert_eq!(receipt.kind, Some(WorkbenchCellItemKind::Declaration));
        assert_eq!(receipt.status, WorkbenchItemStatus::Committed);
        assert_eq!(receipt.output, "[bound request]");
        assert_eq!(receipt.installed_bindings, vec!["request".to_owned()]);
        assert_eq!(receipt.warnings.len(), 1);
        assert!(receipt.warnings[0].contains("mergeSourceWorktree"));
        assert_eq!(receipt.diagnostics.len(), 1);
        assert_eq!(receipt.diagnostics[0].severity, DiagnosticLevel::Warning);
        assert!(matches!(
            &receipt.diagnostics[0].location,
            DiagnosticLocation::Authored { label, start_line: 2, .. } if label == "<cell>"
        ));
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
                display_publication: None,
                display: None,
                id: WorkbenchOperationId {
                    execution,
                    input_unit_index: 0,
                    effect_ordinal: 0,
                },
                effect: "spawn child".into(),
                disposition: WorkbenchOperationDisposition::Prepared,
            }],
        );
        assert_eq!(
            failure.receipts[0].operations[0].disposition,
            WorkbenchOperationDisposition::Unknown
        );
    }

    #[test]
    fn completed_observation_failure_keeps_recovered_bindings_and_classification() {
        let recovered = vec!["commandJob".to_owned()];
        let source = ResidentActorWorkbenchError::CompletedResultObservation {
            detail: "presenter result exceeded observation budget".into(),
            recovered_bindings: Vec::new(),
        };
        let execution = WorkbenchExecutionId::from_digest([10; 16]);
        let operation = WorkbenchOperationReceipt {
            display_publication: None,
            display: None,
            id: WorkbenchOperationId {
                execution,
                input_unit_index: 0,
                effect_ordinal: 0,
            },
            effect: "retain command job binding".into(),
            disposition: WorkbenchOperationDisposition::Committed,
        };
        let failure = workbench_failure_after_unit(&[], 0, 1, source, vec![operation], &recovered);

        let receipt = failure
            .receipts
            .last()
            .expect("completed observation failure has a receipt");
        assert_eq!(receipt.status, WorkbenchItemStatus::Diagnostic);
        assert_eq!(
            receipt.failure_layer,
            Some(WorkbenchFailureLayer::Observation)
        );
        assert_eq!(receipt.installed_bindings, recovered);
        assert!(receipt.output.contains("effects committed"));
        assert!(receipt
            .output
            .contains("private bindings (discarded unless this cell publishes): commandJob"));
        assert_eq!(
            receipt.operations[0].disposition,
            WorkbenchOperationDisposition::Committed
        );
    }

    #[test]
    fn displayed_receipt_failure_keeps_committed_command_binding_recovery() {
        let recovered = vec!["commandJob".to_owned()];
        let source = ResidentActorWorkbenchError::CompletedResultObservation {
            detail: "command observation receipt: displayed-page bookkeeping failed".into(),
            recovered_bindings: recovered.clone(),
        };
        let execution = WorkbenchExecutionId::from_digest([12; 16]);
        let operation = WorkbenchOperationReceipt {
            display_publication: None,
            display: None,
            id: WorkbenchOperationId {
                execution,
                input_unit_index: 0,
                effect_ordinal: 0,
            },
            effect: "retain command job binding".into(),
            disposition: WorkbenchOperationDisposition::Committed,
        };
        let failure = workbench_failure_after_unit(&[], 0, 1, source, vec![operation], &recovered);
        let receipt = failure
            .receipts
            .last()
            .expect("display receipt failure retains its completed effect evidence");

        assert_eq!(receipt.status, WorkbenchItemStatus::Diagnostic);
        assert_eq!(
            receipt.failure_layer,
            Some(WorkbenchFailureLayer::Observation)
        );
        assert_eq!(receipt.installed_bindings, recovered);
        assert!(receipt.output.contains("effects committed"));
        assert!(receipt
            .output
            .contains("private bindings (discarded unless this cell publishes): commandJob"));
        assert_eq!(
            receipt.operations[0].disposition,
            WorkbenchOperationDisposition::Committed
        );
    }

    fn completed_context_operations() -> Vec<WorkbenchOperationReceipt> {
        let execution = WorkbenchExecutionId::from_digest([11; 16]);
        let requests = [
            crate::ContextReq::GetContextWith,
            crate::ContextReq::PutContextWith(tidepool_bridge_effects::ContextDocument {
                blocks: Vec::new(),
            }),
            crate::ContextReq::SetNextModelWith("next-model".into()),
            crate::ContextReq::SetNextEffortWith(crate::ForkEffort::High),
        ];
        let mut operations = Vec::new();
        for (ordinal, request) in requests.into_iter().enumerate() {
            let boundary = crate::resident_workbench::ResidentActorBoundary::Context {
                continuation: tidepool_runtime::session::ResidentHole::plain("context-answer"),
                request,
                table: tidepool_repr::DataConTable::new(),
            };
            super::record_workbench_operation(
                &mut operations,
                Some(&execution),
                0,
                ordinal,
                boundary.operation(),
                None,
                std::time::Duration::ZERO,
                super::scoped_operation_disposition(
                    boundary.success_disposition(),
                    WorkbenchOperationDisposition::Committed,
                ),
            );
        }
        operations
    }

    fn assert_context_receipt_states(operations: &[WorkbenchOperationReceipt]) {
        let encoded = serde_json::to_value(operations).unwrap();
        for (ordinal, state) in ["read", "staged", "staged"].into_iter().enumerate() {
            assert_eq!(encoded[ordinal]["effect"], "context transformation");
            assert_eq!(encoded[ordinal]["disposition"], state);
        }
    }

    #[test]
    fn context_operation_receipts_are_not_promoted_at_cell_completion() {
        let mut operations = completed_context_operations();
        // Ordinary per-item success settles provisional fork admissions, but
        // it is not the Store's enclosing context transaction acknowledgment.
        super::settle_prepared_operations(
            &mut operations,
            WorkbenchOperationDisposition::Committed,
        );
        assert_context_receipt_states(&operations);
    }

    #[test]
    fn context_operation_receipts_survive_failure_and_cancellation_without_commit() {
        let failure = workbench_failure_after_operations(
            &[],
            0,
            1,
            crate::ResidentActorWorkbenchError::ActorProtocol("authored cell failed".into()),
            completed_context_operations(),
        );
        assert_context_receipt_states(&failure.receipts[0].operations);
        let cancelled = workbench_response(
            WorkbenchRunStatus::RequestCancelled,
            failure.receipts,
            0,
            1,
            None,
        );
        assert_context_receipt_states(&cancelled.items[0].operations);
        let cleanup_failure =
            failed_checkpoint_cleanup_response(cancelled, "cleanup unavailable".into());
        assert_context_receipt_states(&cleanup_failure.receipts[0].operations);
    }

    #[test]
    fn delivered_context_response_does_not_claim_store_commit() {
        let error = crate::ResidentActorWorkbenchError::Delivered(ResidentError::ForeignCustody);
        let delivered = disposition_for_non_command_failure(&error);
        assert_eq!(delivered, WorkbenchOperationDisposition::Committed);
        for disposition in [
            WorkbenchOperationDisposition::Read,
            WorkbenchOperationDisposition::Staged,
        ] {
            assert_eq!(
                super::scoped_operation_disposition(disposition, delivered),
                disposition
            );
            assert_eq!(
                super::scoped_operation_disposition(
                    disposition,
                    WorkbenchOperationDisposition::Unknown
                ),
                WorkbenchOperationDisposition::Unknown
            );
        }
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
