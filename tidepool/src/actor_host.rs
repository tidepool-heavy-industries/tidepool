//! Composition root for the first actor-native interactive swarm.
//!
//! The daemon owns resident Haskell scheduling and exact actor lifecycle. One
//! stock interactive agent is attached to each installed Haskell tool policy;
//! tmux is process ownership and observability, never message transport.

#[cfg(test)]
mod custody_tests;
#[cfg(test)]
mod documentation_tests;
mod host_incarnation;
#[allow(dead_code)] // Full retained domain evidence is richer than current UI rendering.
mod hosted_retirement;
mod overlay_resource;
pub(crate) use hosted_retirement::{CompletionBoundary, HostedObservation};
mod model_free;
mod prompt_catalog;
pub(crate) mod recipe_checks;
#[cfg(test)]
mod research_policy_tests;
#[allow(dead_code)] // Staged owner; no launch switch before native/quiescence integration.
mod scoped_custody;
mod socket_directory;
#[cfg(test)]
mod test_campaign;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use frunk::{hlist, HCons, HNil};
use futures_util::FutureExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tidepool_actor::{
    ActorDescriptor, ActorEffectProfile, ActorExitKind, ActorPlacement, ActorRef, ActorTerminal,
    ActorWorkbenchSource, ExternalApplicationFailure, ExternalApplicationFailureClass,
    ExternalFailureDisposition, ForkWorkspaceAdmission, ForkWorkspaceAdmissionError,
    ForkWorkspaceSeed, LocalActorRef, LocalResidentDeployment, LocalResidentInstallation,
    ResidentActorRoot, ResidentForest,
};
use tidepool_agent::{
    native_interactive_backend, read_interactive_binding, BackendThreadId, InteractiveAgentBackend,
    InteractiveAgentInstallation, InteractiveAgentSpec, InteractiveLaunchMode,
    InteractiveNativeSandbox, InteractiveNativeToolPolicy, InteractivePolicyMount,
    QueueReadyThread, ReasoningEffort,
};
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_handlers::{
    ActorBoundWorktreeHandler, ActorWorktreeAllocationHandler, ActorWorktreeAuthority,
    ActorWorktreeGrant, ActorWorktreeHandler, ActorWorktreeIntegrationHandler,
    ActorWorktreeRegistryHandler, WorktreeHandler,
};
use tidepool_mcp::CapturedOutput;
use tidepool_node::{
    DurableInbox, ProcessInvocation, ProcessMountBoundary, TmuxLaunch, TmuxPaneId, TmuxSession,
    BUBBLEWRAP_PROGRAM,
};
use tidepool_repr::SessionId;
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, ResidentSession, SessionLib,
    TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_worktree::{
    ActiveBinding, AgentRef as WorktreePrincipal, BindingTable, GitCli, WorktreeHandle, WorktreeId,
    WorktreeManager, WorktreeRegistry,
};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

use self::host_incarnation::HostIncarnationLease;
use self::overlay_resource::{
    NativePublication, OverlayResourceLease, OverlaySnapshot, PublicationSkip,
};
use self::prompt_catalog::{FrozenBasePrompt, PromptId};
use self::socket_directory::SocketDirectory;

/// Every interactive actor sees its own repository at this path. Bubblewrap
/// mount namespaces make the shared name safe across concurrent actors, while
/// Codex needs only one persisted project-trust decision.
pub(crate) const ACTOR_PROJECT_ROOT: &str = "/tmp/tidepool-actor-workspace";
const ACTOR_BUILD_TARGET: &str = ".shoal/build/cargo";

const DRIVER_MODULE: &str = "Tidepool.Actors.Internal.ShoalDriver";
const WORKBENCH_SURFACE_MODULE: &str = "Tidepool.Actors.Shoal";
const DRIVER_ENTRY: &str = "rootDriver";
const DRIVER_EFFECTS: &str = "RootEffects";
const SHOAL_REPLACED_EFFECT_NAMES: &[&str] = &[
    "boundWorktree",
    "createWorktree",
    "listWorktrees",
    "lookupWorktree",
    "observeSubmission",
    "tryMerge",
    "worktreeBranch",
    "worktreeHead",
    "worktreeId",
];
const CHILD_LIFECYCLE_NOTICE: &str = "A child actor changed lifecycle state.";
const APPLICATION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
const APPLICATION_TASK_GRACE_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

type ShoalHandlerStack = HCons<
    ActorBoundWorktreeHandler,
    HCons<
        ActorWorktreeRegistryHandler,
        HCons<
            ActorWorktreeAllocationHandler,
            HCons<ActorWorktreeIntegrationHandler, HCons<ActorWorktreeHandler, HNil>>,
        >,
    >,
>;
type ShoalRoot = ResidentActorRoot<ShoalHandlerStack, CapturedOutput>;

#[derive(Clone)]
struct ActorForkWorkspaceAdmission {
    worktrees: Arc<Mutex<ActorWorktreeHandler>>,
    manager: WorktreeManager,
    bindings: Arc<Mutex<BindingTable>>,
    runtime: String,
    native: Option<NativeForkAdmission>,
}

#[derive(Clone, Default)]
enum BuildInheritance {
    #[default]
    Unprepared,
    Prepared(Option<OverlaySnapshot>),
}

struct ActorWorkspaceCustody {
    bindings: Arc<Mutex<BindingTable>>,
    binding: Option<ActiveBinding>,
    actor: ActorRef,
    state: Mutex<scoped_custody::CustodyState>,
    build_inheritance: BuildInheritance,
}

impl tidepool_actor::ForkWorkspaceCustody for ActorWorkspaceCustody {
    fn actor_stopped(&self, terminal: &tidepool_actor::ActorTerminal) {
        // Observation is monotonic: duplicate notifications cannot replace the
        // first exact terminal or confuse "not completed" with "still active".
        self.state
            .lock()
            .terminal
            .get_or_insert_with(|| terminal.clone());
    }
    fn process_may_exist(&self) {
        self.state.lock().launch = scoped_custody::LaunchCustody::Legacy;
    }
}

impl Drop for ActorWorkspaceCustody {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        if matches!(
            state.launch,
            scoped_custody::LaunchCustody::Legacy | scoped_custody::LaunchCustody::ScopedClaimed
        ) {
            tracing::error!(actor = ?self.actor, "retaining worktree custody: process or host cleanup is unconfirmed");
            return;
        }
        if let Some(binding) = self.binding.take() {
            let result = if state
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.kind == ActorExitKind::Completed)
            {
                binding.complete(&mut self.bindings.lock())
            } else {
                binding.release(&mut self.bindings.lock())
            };
            if let Err(error) = result {
                tracing::error!(actor = ?self.actor, %error, "worktree custody release failed");
            }
        }
    }
}

impl ActorForkWorkspaceAdmission {
    fn bind_workspace(
        &self,
        actor: ActorRef,
        worktree: &str,
        build_inheritance: BuildInheritance,
    ) -> Result<Arc<dyn tidepool_actor::ForkWorkspaceCustody>, ForkWorkspaceAdmissionError> {
        if !WorktreeId::is_path_safe(worktree) {
            return Err(ForkWorkspaceAdmissionError {
                detail: "invalid custody worktree id".into(),
            });
        }
        if self
            .manager
            .lookup(&WorktreeId::from_raw(worktree))
            .map_err(|error| ForkWorkspaceAdmissionError {
                detail: error.to_string(),
            })?
            .is_none()
        {
            return Err(ForkWorkspaceAdmissionError {
                detail: "custody worktree is not registered".into(),
            });
        }
        let principal =
            WorktreePrincipal::exact_actor(&self.runtime, actor.id.0, actor.incarnation.0);
        let binding = self
            .bindings
            .lock()
            .bind(
                &WorktreeId::from_raw(worktree),
                &principal,
                current_time_ms(),
            )
            .map_err(|error| ForkWorkspaceAdmissionError {
                detail: error.to_string(),
            })?;
        Ok(Arc::new(ActorWorkspaceCustody {
            bindings: self.bindings.clone(),
            binding: Some(binding),
            actor,
            state: Mutex::new(scoped_custody::CustodyState::default()),
            build_inheritance,
        }))
    }
}

impl ForkWorkspaceAdmission for ActorForkWorkspaceAdmission {
    fn install_custody(
        &self,
        actor: ActorRef,
        worktree: &str,
    ) -> Result<Arc<dyn tidepool_actor::ForkWorkspaceCustody>, ForkWorkspaceAdmissionError> {
        self.bind_workspace(actor, worktree, BuildInheritance::Unprepared)
    }

    fn admit(
        &self,
        owner: ActorRef,
        actor_path: String,
        seed: ForkWorkspaceSeed,
        native_tools: tidepool_actor::NativeToolClass,
    ) -> tidepool_actor::ForkWorkspaceAdmissionFuture<'_> {
        let worktrees = self.worktrees.clone();
        let custody = self.clone();
        Box::pin(async move {
            let build_snapshot = match &custody.native {
                Some(native) => native.build_snapshot(owner, native_tools).await,
                None => None,
            };
            let handle = tokio::task::spawn_blocking(move || {
                let (spec, dirty_policy) = match seed {
                    ForkWorkspaceSeed::Explicit(spec) => {
                        let dirty_policy = spec.spec_dirty_policy;
                        (Some(spec), dirty_policy)
                    }
                    ForkWorkspaceSeed::BoundHead(dirty_policy) => (None, dirty_policy),
                };
                worktrees
                    .lock()
                    .admit_fork_workspace(owner.into(), actor_path, spec, dirty_policy)
                    .map_err(|error| ForkWorkspaceAdmissionError {
                        detail: format!("{error:?}"),
                    })
            })
            .await
            .map_err(|error| ForkWorkspaceAdmissionError {
                detail: format!("workspace preparation task failed: {error}"),
            })??;
            let worktree = handle.handle_receipt.tree_id.raw.clone();
            Ok(tidepool_actor::PreparedForkWorkspace::new(
                handle,
                move |actor| {
                    custody.bind_workspace(
                        actor,
                        &worktree,
                        BuildInheritance::Prepared(build_snapshot),
                    )
                },
            ))
        })
    }
}

fn fork_workspace_admission(
    worktrees: WorktreeManager,
    authority: ActorWorktreeAuthority,
    bindings: Arc<Mutex<BindingTable>>,
    runtime: String,
    native: Option<NativeForkAdmission>,
) -> Arc<ActorForkWorkspaceAdmission> {
    Arc::new(ActorForkWorkspaceAdmission {
        bindings,
        runtime,
        native,
        manager: worktrees.clone(),
        worktrees: Arc::new(Mutex::new(ActorWorktreeHandler::new(
            WorktreeHandler::from_manager(worktrees),
            authority,
        ))),
    })
}

#[derive(Clone)]
pub struct ActorHostConfig {
    /// This Shoal installation provides the internal namespace-entry executable.
    pub shoal_executable: PathBuf,
    pub workspace_inputs: Option<crate::shoal::workspace::FrozenWorkspace>,
    pub workspace: PathBuf,
    pub haskell_root: PathBuf,
    pub run_root: PathBuf,
    pub root_binding_path: PathBuf,
    pub interactive_agent: InteractiveAgentInstallation,
    pub tmux_session: String,
    pub model: String,
    pub effort: ReasoningEffort,
    pub research_policy: tidepool_actor::ResearchPolicy,
    pub root_launch_mode: InteractiveLaunchMode,
    pub pane_environment: std::collections::BTreeMap<String, String>,
}

fn worker_launch_resolver(config: &ActorHostConfig) -> tidepool_actor::WorkerLaunchResolver {
    let config = config.clone();
    let base = FrozenBasePrompt::selected_body(
        config
            .workspace_inputs
            .as_ref()
            .and_then(|inputs| inputs.prompts.get("core"))
            .map(String::as_str),
    );
    let fingerprint = blake3::hash(base.as_bytes()).to_hex().to_string();
    Arc::new(move |request| resolve_worker_launch(&config, request, &fingerprint))
}

fn resolve_worker_launch(
    config: &ActorHostConfig,
    request: &tidepool_actor::WorkerLaunchRequest,
    base_fingerprint: &str,
) -> tidepool_actor::WorkerLaunchPreview {
    let mut instructions = developer_instructions_selected(
        &request.role,
        &InteractiveLaunchMode::Fresh,
        config.workspace_inputs.as_ref(),
        request.instructions.as_deref(),
    );
    append_inheritance_authority(&mut instructions);
    tidepool_actor::WorkerLaunchPreview {
        model: request.model.clone().or_else(|| {
            (request.context == tidepool_actor::ForkContext::SelectedContext)
                .then(|| config.model.clone())
        }),
        effort: request.effort.unwrap_or(tidepool_actor::ForkEffort::Low),
        instructions,
        base_fingerprint: base_fingerprint.into(),
        workspace_identity: config
            .workspace_inputs
            .as_ref()
            .map(|inputs| inputs.identity().into()),
        modules: config
            .workspace_inputs
            .as_ref()
            .map(|inputs| inputs.import_modules().map(str::to_owned).collect())
            .unwrap_or_default(),
    }
}

fn append_inheritance_authority(instructions: &mut String) {
    instructions.push_str("\nInherited parent bindings do not grant parent authority; the runtime policy above governs this actor.\n");
}

fn launch_effort(
    mode: &InteractiveLaunchMode,
    default: ReasoningEffort,
    requested: Option<tidepool_actor::ForkEffort>,
) -> ReasoningEffort {
    match requested {
        Some(tidepool_actor::ForkEffort::Low) => ReasoningEffort::Low,
        Some(tidepool_actor::ForkEffort::Medium) => ReasoningEffort::Medium,
        Some(tidepool_actor::ForkEffort::High) => ReasoningEffort::High,
        None if matches!(mode, InteractiveLaunchMode::Fork { .. }) => ReasoningEffort::Low,
        None => default,
    }
}

#[derive(Debug, Clone)]
pub enum ActorHostReadiness {
    /// The root pane is selected, but its queue-ready session binding has not
    /// yet been published.
    AwaitingBinding { root: ActorRef },
    /// The v2 host-tools session callback proved that the exact surrounding
    /// thread is durably addressable by native lifecycle commands.
    Ready {
        root: ActorRef,
        thread: QueueReadyThread,
    },
}

struct InteractiveDeployment {
    /// Retain the view independently of the bootstrap and native process lifetimes.
    _workspace_view: tidepool_node::MountNamespace,
    supervisor: Option<ActorRef>,
    notified_provider_failures: std::collections::BTreeSet<(String, String)>,
    actor: ActorRef,
    local_actor: LocalActorRef,
    pane: TmuxPaneId,
    workspace: PathBuf,
    inbox: Arc<ActorInbox>,
    notification_inbox_key: String,
    connection: InteractiveConnection,
    service: hosted_retirement::HostedOwner,
    socket_directory: SocketDirectory,
    worktree_custody: Option<Arc<dyn tidepool_actor::ForkWorkspaceCustody>>,
    failure_reported: bool,
    last_activation_sequence: u64,
    thread: Option<QueueReadyThread>,
    fork_gate: Option<tidepool_actor::ForkGroupGate>,
    runtime_observation: tidepool_actor::ActorRuntimeObservationHandle,
    fork_parent_thread: Option<BackendThreadId>,
    build_resource: Option<Arc<tokio::sync::Mutex<OverlayResourceLease>>>,
}

enum InteractiveConnection {
    // Pane, inbox, tool listener, and cleanup are already owned in this state.
    AwaitingBinding,
    Bound {
        delivery_shutdown: oneshot::Sender<()>,
        delivery: tokio::task::JoinHandle<()>,
    },
}

struct InteractiveBindingRequest {
    control: crate::host_dynamic_tools::HostToolControl,
    path: PathBuf,
    expected: Option<BackendThreadId>,
}

struct LaunchedInteractiveApplication {
    deployment: InteractiveDeployment,
    binding: InteractiveBindingRequest,
}

struct OwnerNotification {
    owner: ActorRef,
    inbox: Arc<ActorInbox>,
    event: DurableActorEvent,
}

type ActorInbox = DurableInbox<DurableActorEvent, NotificationProvenance>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NotificationProvenance {
    sender: ActorRef,
    target: ActorRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum DurableActorEvent {
    Typed(TypedActorEvent),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum TypedActorEvent {
    ProviderTurnFailed {
        #[serde(default)]
        revision: u64,
        actor: ActorRef,
        thread: String,
        turn: String,
        failure: tidepool_agent::ProviderFailure,
    },
    SessionReady {
        sequence: u64,
        request: tidepool_actor::RequestId,
        input_type: String,
        message: String,
    },
    WatchChanged {
        #[serde(flatten)]
        notification: tidepool_actor::WatchNotification,
    },
    RequestCancellation {
        #[serde(flatten)]
        notification: tidepool_actor::RequestCancellationNotification,
    },
    CleanupFinished {
        receipt: InteractiveCleanupReceipt,
    },
    ChildExited,
}

impl DurableActorEvent {
    fn session(activation: &tidepool_actor::ResidentActivation) -> Self {
        Self::Typed(TypedActorEvent::SessionReady {
            sequence: activation.id.sequence(),
            request: activation.request,
            input_type: activation.input_type.clone(),
            message: activation.message.clone(),
        })
    }

    fn render(&self, launched_at: Option<i64>) -> String {
        let elapsed = |occurred: u64| match launched_at
            .and_then(|start| i64::try_from(occurred).ok()?.checked_sub(start))
            .filter(|elapsed| *elapsed >= 0)
        {
            Some(ms) => format!(
                "+{}m{:02}s since actor launch",
                ms / 60_000,
                (ms / 1000) % 60
            ),
            None => "elapsed time unavailable".to_owned(),
        };
        match self {
            Self::Typed(TypedActorEvent::ProviderTurnFailed { actor, thread, turn, failure, .. }) => format!(
                "actor {}@{} provider turn {turn:?} in thread {thread:?} failed: {failure:?}. The actor request remains pending. Inspect status before deciding whether to retire or recover it; further steering does not repair rejected history.",
                actor.id.0, actor.incarnation.0,
            ),
            Self::Typed(TypedActorEvent::SessionReady { message, .. }) => message.clone(),
            Self::Text(message) => message.clone(),
            Self::Typed(TypedActorEvent::WatchChanged { notification })
                if matches!(notification.transition, tidepool_actor::WatchTransition::RouteFailed { .. }) => format!(
                "route {} failed: {:?} ({}). Recover owned handles with `listRoutes`, then inspect with `pollRoute`. Earlier effects may have completed; do not replay the callback blindly.",
                notification.watch.0,
                notification.current,
                elapsed(notification.occurred_at_unix_ms),
            ),
            Self::Typed(TypedActorEvent::WatchChanged { notification }) => format!(
                "watch {} {:?}: {:?} → {:?} ({}). Poll its retained handle with `pollWatch`.",
                notification.watch.0,
                notification.label,
                notification.previous,
                notification.current,
                elapsed(notification.occurred_at_unix_ms),
            ),
            Self::Typed(TypedActorEvent::RequestCancellation { notification }) => format!(
                "request {} {:?} has cancellation pending ({:?}; {}). Inspect `sessionReply` with `pollReply`; acknowledge it with `acknowledgeCancellation sessionReply` when the active work is safely quiescent.",
                notification.request.0,
                notification.label,
                notification.reason,
                elapsed(notification.occurred_at_unix_ms),
            ),
            Self::Typed(TypedActorEvent::CleanupFinished { receipt }) => receipt.render(),
            Self::Typed(TypedActorEvent::ChildExited) => CHILD_LIFECYCLE_NOTICE.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum CleanupComponent {
    Process,
    ToolService,
    Delivery,
    Socket,
    WorktreeBinding,
    BuildResource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum CleanupComponentOutcome {
    Completed,
    Forced,
    Failed { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CleanupComponentReceipt {
    component: CleanupComponent,
    outcome: CleanupComponentOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct InteractiveCleanupReceipt {
    actor: ActorRef,
    components: Vec<CleanupComponentReceipt>,
}

impl InteractiveCleanupReceipt {
    fn degraded(&self) -> bool {
        self.components
            .iter()
            .any(|component| matches!(component.outcome, CleanupComponentOutcome::Failed { .. }))
    }

    fn render(&self) -> String {
        let failures = self
            .components
            .iter()
            .filter_map(|component| match &component.outcome {
                CleanupComponentOutcome::Failed { detail } => {
                    Some(format!("{:?}: {detail}", component.component))
                }
                CleanupComponentOutcome::Completed | CleanupComponentOutcome::Forced => None,
            })
            .collect::<Vec<_>>();
        if failures.is_empty() {
            format!(
                "Actor {:?} retired and all cleanup components settled.",
                self.actor
            )
        } else {
            format!(
                "Actor {:?} retired with degraded cleanup: {}. The permanent host and sibling actors remain available.",
                self.actor,
                failures.join("; ")
            )
        }
    }
}

struct InteractiveApplicationOwner {
    creator_build: Option<CreatorBuild>,
    cancel: Option<oneshot::Sender<NativeRetirement>>,
    native_retirement: NativeRetirement,
    pane: Arc<Mutex<Option<TmuxPaneId>>>,
    fork_gate: Option<tidepool_actor::ForkGroupGate>,
    custody: Option<Arc<dyn tidepool_actor::ForkWorkspaceCustody>>,
    scoped_retention: Option<scoped_custody::ScopedHostRetention>,
    hosted: hosted_retirement::HostedSlot,
    launch: HostLaunchState,
    terminal: Option<ActorTerminal>,
    retirement: Arc<Mutex<Option<InteractiveCleanupReceipt>>>,
}

/// Coordination failure does not authorize terminating the native conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum NativeRetirement {
    #[default]
    Preserve,
    Terminate,
}

#[derive(Clone, Copy)]
enum HostLaunchState {
    Pending,
    Published,
    Abandoned,
    Failed,
}

type InteractiveOwners = Arc<Mutex<HashMap<ActorRef, InteractiveApplicationOwner>>>;

#[derive(Clone)]
struct CreatorBuild {
    resource: Arc<tokio::sync::Mutex<OverlayResourceLease>>,
    thread: QueueReadyThread,
}

#[derive(Clone)]
struct NativeForkAdmission {
    owners: InteractiveOwners,
    backend: Arc<dyn InteractiveAgentBackend>,
}

fn native_tool_policy(
    native_tools: tidepool_actor::NativeToolClass,
) -> InteractiveNativeToolPolicy {
    match native_tools {
        tidepool_actor::NativeToolClass::InspectionOnly => {
            InteractiveNativeToolPolicy::InspectionOnly
        }
        tidepool_actor::NativeToolClass::Coding
        | tidepool_actor::NativeToolClass::Integration
        | tidepool_actor::NativeToolClass::Inherited => InteractiveNativeToolPolicy::Standard,
    }
}

impl NativeForkAdmission {
    async fn build_snapshot(
        &self,
        creator: ActorRef,
        native_tools: tidepool_actor::NativeToolClass,
    ) -> Option<OverlaySnapshot> {
        if native_tool_policy(native_tools) == InteractiveNativeToolPolicy::InspectionOnly {
            return None;
        }
        let source = self
            .owners
            .lock()
            .get(&creator)
            .filter(|owner| owner.terminal.is_none())
            .and_then(|owner| owner.creator_build.clone())?;
        let mut resource = source.resource.lock().await;
        // Complete native admission before dropping the resource lock. Retained
        // publication records own any uncertain transition across host failure.
        match resource
            .publish_native(
                self.backend.as_ref(),
                &source.thread,
                &PathBuf::from(ACTOR_PROJECT_ROOT).join(ACTOR_BUILD_TARGET),
                &[],
            )
            .await
        {
            Ok(NativePublication::Published { sequence, snapshot }) => {
                tracing::debug!(?creator, %sequence, "creator build snapshot published during admission");
                Some(snapshot)
            }
            Ok(NativePublication::Skipped(reason)) => {
                if let PublicationSkip::NativeUnavailable(reason) = reason {
                    tracing::debug!(?creator, %reason, "creator build publication unavailable");
                }
                resource.latest_snapshot()
            }
            Err(error) => {
                tracing::warn!(?creator, %error, "creator build publication retained for recovery");
                resource.latest_snapshot()
            }
        }
    }
}

impl InteractiveApplicationOwner {
    #[allow(dead_code)] // Staged slot admission; production selection remains disabled.
    fn reserve_scope(
        &mut self,
        custody: Arc<ActorWorkspaceCustody>,
        actor: ActorRef,
    ) -> Result<Arc<Mutex<scoped_custody::ScopedProcessSlot>>, scoped_custody::ScopedClaimError>
    {
        let erased: Arc<dyn tidepool_actor::ForkWorkspaceCustody> = custody.clone();
        if !self
            .custody
            .as_ref()
            .is_some_and(|installed| Arc::ptr_eq(installed, &erased))
        {
            return Err(scoped_custody::ScopedClaimError::WrongActor);
        }
        if self.scoped_retention.is_some() {
            return Err(scoped_custody::ScopedClaimError::AlreadyClaimed);
        }
        let retention = scoped_custody::reserve(custody, actor)?;
        let slot = retention.slot.clone();
        self.scoped_retention = Some(retention);
        Ok(slot)
    }

    fn cancel(&mut self) {
        self.creator_build = None;
        if let Some(gate) = &self.fork_gate {
            let _ = gate.mark_failed();
        }
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(self.native_retirement);
        }
    }

    fn retired(&mut self, terminal: ActorTerminal) {
        self.native_retirement = match terminal.kind {
            ActorExitKind::Failed => NativeRetirement::Preserve,
            ActorExitKind::Completed | ActorExitKind::Cancelled => NativeRetirement::Terminate,
        };
        if let Some(custody) = &self.custody {
            custody.actor_stopped(&terminal);
        }
        self.terminal.get_or_insert(terminal);
        self.cancel();
    }
}

/// Returned to run's real host caller, not formatted into a resource-free error.
/// The host retains this error through shutdown. Dropping it at process exit is
/// not settlement, nor continuity of process handles across host death.
pub(crate) struct RetainedInteractiveFleet {
    owners: InteractiveOwners,
    unfinished: Option<tokio::task::JoinHandle<Result<(), String>>>,
    failures: Vec<Box<dyn std::error::Error>>,
}
impl fmt::Debug for RetainedInteractiveFleet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedInteractiveFleet")
            .field("actors", &self.owners.lock().keys().collect::<Vec<_>>())
            .field("unfinished", &self.unfinished.is_some())
            .field("failures", &self.failures)
            .finish()
    }
}
impl fmt::Display for RetainedInteractiveFleet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for error in &self.failures {
            write!(f, "{error}; ")?;
        }
        write!(f, "addressable actor resources retained: host work/process settlement remains unsupported")
    }
}
impl std::error::Error for RetainedInteractiveFleet {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.failures.first().map(|error| error.as_ref())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RetainedHostedError {
    #[error("exact actor has no retained hosted service")]
    NoHostedActor,
}

/// Host-only operations. These never select a launch mode, release the command
/// gate, establish host-work quiescence, or settle workspace custody.
#[allow(dead_code)] // Available to the crate's host error consumer; not a model API.
pub(crate) enum RetainedProcessOperation {
    Observe,
    Pin,
    Stop,
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum RetainedProcessState {
    Reserved,
    Spawning,
    NotSpawned(String),
    Owned,
    Pinned,
    ProcessStopped(tidepool_node::ServiceScopeCleanup),
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct RetainedProcessObservation {
    pub(crate) actor: ActorRef,
    pub(crate) actor_terminal: Option<ActorTerminal>,
    pub(crate) process: RetainedProcessState,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RetainedProcessError {
    #[error("exact actor has no retained scoped process")]
    NoScopedActor,
    #[error("retained process observation deadline elapsed")]
    Deadline,
    #[error(transparent)]
    Scope(#[from] tidepool_node::ServiceScopeError),
}

#[allow(dead_code)]
impl RetainedInteractiveFleet {
    /// Recover the actual resource-bearing error after the host's ordinary
    /// Box<dyn Error> propagation. Crate visibility lets shoal own a subsequent
    /// recovery policy without exposing this mechanism to authored programs.
    pub(crate) fn from_error<'a>(
        error: &'a mut (dyn std::error::Error + 'static),
    ) -> Option<&'a mut Self> {
        error.downcast_mut::<Self>()
    }

    /// Continue the exact stored seal/shutdown/service operations after waiter
    /// loss. Abort is an explicit host decision; no native-success branch exists.
    pub(crate) async fn recover_hosted(
        &self,
        actor: ActorRef,
        boundary: CompletionBoundary,
        timeout: Duration,
    ) -> Result<HostedObservation, RetainedHostedError> {
        let owner = {
            let rows = self.owners.lock();
            rows.get(&actor)
                .and_then(|row| row.hosted.lock().clone())
                .ok_or(RetainedHostedError::NoHostedActor)?
        };
        Ok(hosted_retirement::observe(&owner, boundary, timeout).await)
    }

    /// Blocking, deadline-bounded host operation: call outside an actor turn.
    /// Exact identity includes incarnation. Even ProcessStopped is status only;
    /// the row remains owned by this carrier after every result or error.
    pub(crate) fn recover_process(
        &self,
        actor: ActorRef,
        operation: RetainedProcessOperation,
        deadline: std::time::Instant,
    ) -> Result<RetainedProcessObservation, RetainedProcessError> {
        if std::time::Instant::now() >= deadline {
            return Err(RetainedProcessError::Deadline);
        }
        let rows = self
            .owners
            .try_lock_until(deadline)
            .ok_or(RetainedProcessError::Deadline)?;
        let retention = rows
            .get(&actor)
            .and_then(|row| row.scoped_retention.as_ref())
            .ok_or(RetainedProcessError::NoScopedActor)?;
        let mut slot = retention
            .slot
            .try_lock_until(deadline)
            .ok_or(RetainedProcessError::Deadline)?;
        use scoped_custody::ScopedProcessSlot;
        let process = match (operation, &mut *slot) {
            (RetainedProcessOperation::Observe, ScopedProcessSlot::Reserved) => {
                RetainedProcessState::Reserved
            }
            (RetainedProcessOperation::Observe, ScopedProcessSlot::Spawning) => {
                RetainedProcessState::Spawning
            }
            (RetainedProcessOperation::Observe, ScopedProcessSlot::NotSpawned(error)) => {
                RetainedProcessState::NotSpawned(error.to_string())
            }
            (RetainedProcessOperation::Observe, ScopedProcessSlot::Owned(_)) => {
                RetainedProcessState::Owned
            }
            (RetainedProcessOperation::Pin, ScopedProcessSlot::Owned(scope)) => {
                scope.pin_init(deadline)?;
                RetainedProcessState::Pinned
            }
            (RetainedProcessOperation::Stop, ScopedProcessSlot::Owned(scope)) => {
                RetainedProcessState::ProcessStopped(scope.terminate_and_wait(deadline)?)
            }
            _ => return Err(tidepool_node::ServiceScopeError::WrongPhase.into()),
        };
        Ok(RetainedProcessObservation {
            actor,
            actor_terminal: retention
                .terminal_until(deadline)
                .ok_or(RetainedProcessError::Deadline)?,
            process,
        })
    }
}

fn handoff_application_owners(
    owners: InteractiveOwners,
    task: tokio::task::JoinHandle<Result<(), String>>,
    cleanup: Result<(), Box<dyn std::error::Error>>,
    run_result: Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let retained = owners.lock().values().any(|owner| {
        owner.scoped_retention.is_some() || owner.custody.is_some() || owner.hosted.lock().is_some()
    });
    let unfinished = !task.is_finished();
    if retained || unfinished {
        return Err(Box::new(RetainedInteractiveFleet {
            owners,
            unfinished: unfinished.then_some(task),
            // Preserve the errors themselves: they may own resources too.
            failures: cleanup.err().into_iter().chain(run_result.err()).collect(),
        }));
    }
    cleanup?;
    run_result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootRunDisposition {
    Complete,
    Recover,
}

#[derive(Debug, Clone, Copy)]
enum InteractiveOperation {
    BindWorktree,
    PrepareRuntime,
    BindToolHost,
    BuildCommand,
    ServeToolHost,
    LaunchProcess,
    DiscoverBinding,
}

impl fmt::Display for InteractiveOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::BindWorktree => "bind actor worktree",
            Self::PrepareRuntime => "prepare runtime",
            Self::BindToolHost => "bind tool host",
            Self::BuildCommand => "build agent command",
            Self::ServeToolHost => "serve host dynamic tools",
            Self::LaunchProcess => "launch agent process",
            Self::DiscoverBinding => "discover conversation binding",
        };
        formatter.write_str(name)
    }
}

impl InteractiveOperation {
    fn failure_class(self) -> ExternalApplicationFailureClass {
        match self {
            Self::BindWorktree => ExternalApplicationFailureClass::WorktreeBinding,
            Self::BuildCommand => ExternalApplicationFailureClass::CommandConstruction,
            Self::LaunchProcess => ExternalApplicationFailureClass::ProcessLaunch,
            Self::BindToolHost
            | Self::ServeToolHost
            | Self::DiscoverBinding
            | Self::PrepareRuntime => ExternalApplicationFailureClass::ToolHostStartup,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("actor {actor:?} failed to {operation}: {detail}")]
struct InteractiveApplicationError {
    actor: ActorRef,
    operation: InteractiveOperation,
    detail: String,
}

async fn apply_application_failure(
    actor: LocalActorRef,
    failure: ExternalApplicationFailure,
) -> Result<(), String> {
    let identity = actor.identity();
    match actor.report_external_failure(failure).await {
        Ok(ExternalFailureDisposition::Applied | ExternalFailureDisposition::AlreadyTerminal) => {
            Ok(())
        }
        Ok(ExternalFailureDisposition::UnknownOrStale) => Err(format!(
            "actor registry rejected exact deployed actor {identity:?} as unknown or stale"
        )),
        Err(error) => Err(format!(
            "actor registry could not record application failure for {identity:?}: {error}"
        )),
    }
}

fn application_error(
    actor: ActorRef,
    operation: InteractiveOperation,
    error: impl fmt::Display,
) -> InteractiveApplicationError {
    InteractiveApplicationError {
        actor,
        operation,
        detail: error.to_string(),
    }
}

struct InteractiveFleet {
    root: LocalActorRef,
    config: ActorHostConfig,
    run_root: PathBuf,
    tmux: TmuxSession,
    backend: Arc<dyn InteractiveAgentBackend>,
    worktrees: WorktreeManager,
    bindings: Arc<Mutex<BindingTable>>,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    worktree_authority: ActorWorktreeAuthority,
}

#[derive(Clone)]
struct InteractiveLaunchContext {
    base_prompt: FrozenBasePrompt,
    root: ActorRef,
    config: ActorHostConfig,
    run_root: PathBuf,
    tmux: TmuxSession,
    backend: Arc<dyn InteractiveAgentBackend>,
    worktrees: WorktreeManager,
    bindings: Arc<Mutex<BindingTable>>,
}

pub async fn run(
    mut config: ActorHostConfig,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
) -> Result<(), Box<dyn std::error::Error>> {
    let run_root = config.run_root.clone();
    std::fs::create_dir_all(&run_root)?;
    let host_incarnation = HostIncarnationLease::claim(&run_root)?;

    let workspace = config.workspace.clone();
    let (worktrees, bindings) =
        tokio::task::spawn_blocking(move || actor_worktree_resources(&workspace)).await??;
    let bindings = Arc::new(Mutex::new(bindings));
    let worktree_authority =
        ActorWorktreeAuthority::new(runtime_namespace(&run_root), Arc::clone(&bindings));
    let tmux = TmuxSession::new(&config.tmux_session)?;
    if !tmux.exists().await? {
        return Err(runtime_error(format!(
            "Shoal tmux session {:?} does not exist",
            config.tmux_session
        )));
    }
    let backend = native_interactive_backend(config.interactive_agent.clone());
    let application_owners: InteractiveOwners = Arc::new(Mutex::new(HashMap::new()));
    let (source, root, program) = compile_root(
        &config,
        &run_root,
        worktrees.clone(),
        worktree_authority.clone(),
    )?;
    let (descriptor, machine, outcome) = root.into_parts();
    let (forest, deployments) = ResidentForest::new_with_launch_resolver(
        source,
        descriptor.placement().session,
        machine,
        Some(fork_workspace_admission(
            worktrees.clone(),
            worktree_authority.clone(),
            bindings.clone(),
            runtime_namespace(&run_root),
            Some(NativeForkAdmission {
                owners: application_owners.clone(),
                backend: backend.clone(),
            }),
        )),
        host_incarnation.incarnation(),
        Some(worker_launch_resolver(&config)),
    );
    let forest = Arc::new(forest);
    let (mut root_actor, mut root_task) = forest.admit_root(descriptor, outcome).await?;
    worktree_authority.install_grant(root_actor.identity().into(), ActorWorktreeGrant::Repository);
    let provision_forest = forest.clone();
    let provision_authority = worktree_authority.clone();
    let operator_role =
        tidepool_actor::EffectiveRole::root().with_research_policy(config.research_policy);
    let operator_socket = run_root.join("operator").join("operator.sock");
    if operator_socket.exists() {
        std::fs::remove_file(&operator_socket)?;
    }
    let inspection_forest = forest.clone();
    let operator = crate::operator::OperatorService::bind(
        operator_socket.clone(),
        Arc::new(move || {
            let forest = provision_forest.clone();
            let role = operator_role.clone();
            let authority = provision_authority.clone();
            Box::pin(async move {
                let grant = worktree_grant(role.role());
                let actor = forest
                    .new_workbench("operator".into(), role)
                    .await
                    .map_err(|e| e.to_string())?;
                authority.install_grant(actor.identity().into(), grant);
                Ok(actor)
            })
        }),
        Arc::new(move |requester| inspection_forest.inspect_graph(requester)),
    )
    .await;
    let operator = match operator {
        Ok(operator) => operator,
        Err(error) => {
            forest.shutdown().await;
            return Err(Box::new(error));
        }
    };
    tracing::info!(socket = %operator_socket.display(), "operator control and attachment ready");
    let (shutdown, shutdown_rx) = watch::channel(None);
    let (root_config, root_config_rx) = watch::channel(config.clone());
    let mut applications_task = tokio::spawn(run_interactive_applications(
        deployments,
        application_owners.clone(),
        InteractiveFleet {
            root: root_actor.clone(),
            config: config.clone(),
            run_root: run_root.clone(),
            tmux: tmux.clone(),
            backend,
            worktrees,
            bindings,
            readiness,
            worktree_authority: worktree_authority.clone(),
        },
        shutdown_rx,
        root_config_rx,
    ));
    let mut recovery = 0_u64;
    let mut applications_finished = false;
    let mut root_active = true;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        loop {
            tokio::select! {
                signal = operator_shutdown() => { signal?; break Ok(()); }
                result = &mut applications_task => {
                    applications_finished = true;
                    break result.map_err(join_error)?.map_err(runtime_error);
                }
                result = &mut root_task, if root_active => {
                    result.map_err(join_error)?;
                    let terminal = root_actor.terminal().get().ok_or_else(|| runtime_error("root stopped without terminal"))?;
                    if terminal.kind == ActorExitKind::Failed {
                        let pane = application_owners.lock().get(&root_actor.identity())
                            .and_then(|owner| owner.pane.lock().clone());
                        if let Err(error) = confirm_native_exit(&tmux, pane.as_ref()).await {
                            tracing::error!(%error, "root coordination stopped; retaining original TUI without automatic conversation resume");
                            root_active = false;
                            continue;
                        }
                    }
                    match prepare_root_recovery(&mut config, root_actor.identity(), terminal, &mut recovery).await {
                        Ok(RootRunDisposition::Recover) => {}
                        Ok(RootRunDisposition::Complete) => {
                            root_active = false;
                            continue;
                        }
                        Err(error) => {
                            tracing::error!(%error, "model root recovery unavailable; operator forest remains attached");
                            root_active = false;
                            continue;
                        }
                    }
                    root_config.send_replace(config.clone());
                    (root_actor, root_task) = forest.new_program_root("shoal-root".into(),
                        tidepool_actor::EffectiveRole::root().with_research_policy(config.research_policy), program.clone())
                        .await.map_err(|e| runtime_error(e.to_string()))?;
                    worktree_authority.install_grant(root_actor.identity().into(), ActorWorktreeGrant::Repository);
                }
            }
        }
    }.await;
    shutdown.send_replace(Some(if result.is_ok() {
        NativeRetirement::Terminate
    } else {
        NativeRetirement::Preserve
    }));
    operator.shutdown().await;
    forest.shutdown().await;
    let cleanup = if !applications_finished {
        await_applications(&mut applications_task, APPLICATION_SHUTDOWN_TIMEOUT).await
    } else {
        Ok(())
    };
    handoff_application_owners(application_owners, applications_task, cleanup, result)
}

async fn confirm_native_exit(tmux: &TmuxSession, pane: Option<&TmuxPaneId>) -> Result<(), String> {
    let pane = pane.ok_or("native launch outcome is unknown")?;
    match tmux.pane_status(pane).await {
        Ok(Some(status)) if status.dead => Ok(()),
        Ok(_) => Err("native pane is live or its exit is unconfirmed".into()),
        Err(error) => Err(format!("cannot confirm native pane exit: {error}")),
    }
}

/// Classify an intentional completion or prepare an abnormal root for a fresh
/// incarnation attached to the retained conversation.
async fn prepare_root_recovery(
    config: &mut ActorHostConfig,
    actor: ActorRef,
    terminal: ActorTerminal,
    recovery: &mut u64,
) -> Result<RootRunDisposition, Box<dyn std::error::Error>> {
    let Some((launch_mode, thread)) =
        root_recovery_launch_mode(&config.root_binding_path, &terminal).await?
    else {
        return Ok(RootRunDisposition::Complete);
    };
    *recovery = (*recovery).saturating_add(1);
    tracing::warn!(
        ?actor,
        kind = ?terminal.kind,
        summary = %terminal.summary,
        recovery = *recovery,
        thread = %thread.id().0,
        "Shoal root stopped abnormally; recreating a fresh root incarnation"
    );
    config.root_launch_mode = launch_mode;
    Ok(RootRunDisposition::Recover)
}

async fn root_recovery_launch_mode(
    binding_path: &Path,
    terminal: &ActorTerminal,
) -> Result<Option<(InteractiveLaunchMode, QueueReadyThread)>, Box<dyn std::error::Error>> {
    if terminal.kind != ActorExitKind::Failed {
        return Ok(None);
    }
    let thread = read_interactive_binding(binding_path)
        .await
        .map_err(|error| {
            runtime_error(format!(
                "Shoal root {terminal:?} and its conversation cannot be resumed: {error}"
            ))
        })?;
    Ok(Some((
        InteractiveLaunchMode::Resume(thread.id().clone()),
        thread,
    )))
}

async fn await_applications(
    task: &mut tokio::task::JoinHandle<Result<(), String>>,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    match tokio::time::timeout(timeout, &mut *task).await {
        Ok(result) => result.map_err(join_error)?.map_err(runtime_error),
        Err(_) => {
            // The caller transfers the unfinished task AND its addressable
            // owner map into the host result; timeout is not cancellation proof.
            Err(runtime_error(format!(
                "interactive fleet did not stop within {timeout:?}"
            )))
        }
    }
}

fn actor_worktree_resources(
    workspace: &Path,
) -> Result<(WorktreeManager, BindingTable), tidepool_worktree::WorktreeError> {
    let project = blake3::hash(workspace.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string();
    let root = tidepool_runtime::paths::cache_dir()
        .join("shoal")
        .join("actor-worktrees")
        .join(project);
    actor_worktree_resources_at(&root, workspace)
}

fn actor_worktree_resources_at(
    root: &Path,
    workspace: &Path,
) -> Result<(WorktreeManager, BindingTable), tidepool_worktree::WorktreeError> {
    let registry = WorktreeRegistry::open(root.join("registry"))?;
    let worktree_root = root.join("worktrees");
    std::fs::create_dir_all(&worktree_root).map_err(|error| {
        tidepool_worktree::WorktreeError::StorageFailure {
            path: worktree_root.clone(),
            detail: error.to_string(),
        }
    })?;
    Ok((
        WorktreeManager::new(GitCli::new(), registry, worktree_root, workspace),
        BindingTable::open_with_timeout(root.join("bindings"), Duration::from_secs(10))?,
    ))
}

fn runtime_namespace(run_root: &Path) -> String {
    run_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-runtime")
        .to_owned()
}

pub(crate) fn shoal_effect_declarations() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::agent_session_decl(),
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_context_decl(),
        tidepool_mcp::agent_control_decl(),
        tidepool_mcp::notifications_decl(),
        tidepool_mcp::agent_inspection_decl(),
        tidepool_mcp::agent_launch_decl(),
        tidepool_mcp::forks_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::fs_read_decl(),
        tidepool_mcp::worktree_decl(),
        tidepool_mcp::bound_worktree_decl(),
        tidepool_mcp::worktree_registry_decl(),
        tidepool_mcp::worktree_allocation_decl(),
        tidepool_mcp::worktree_integration_decl(),
    ]
}

struct CompiledShoalDriver {
    preamble: String,
    include: Vec<PathBuf>,
    compiled: tidepool_runtime::session::CompiledTurn,
}

/// Compile the exact selected driver and imports without launching an actor.
/// Initialization uses this before replacing a live swarm; admission uses the
/// same compiler path and the toolchain owner's content-addressed cache.
pub(crate) fn validate_workspace_program(
    inputs: &crate::shoal::workspace::FrozenWorkspace,
    run_root: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    compile_driver(
        &crate::haskell_sources::ensure_shoal_haskell()?,
        Some(inputs),
        run_root,
    )?;
    Ok(())
}

fn compile_driver(
    haskell_root: &Path,
    inputs: Option<&crate::shoal::workspace::FrozenWorkspace>,
    run_root: &Path,
) -> Result<CompiledShoalDriver, Box<dyn std::error::Error>> {
    let declarations = shoal_effect_declarations();
    let effects = tidepool_mcp::ensure_effects_module(&declarations)?;
    let mut include = effects.include_paths().to_vec();
    include.push(haskell_root.to_path_buf());
    include.push(crate::haskell_sources::ensure_embedded_stdlib()?);
    let mut preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble_with_companions_hiding(
            &declarations,
            false,
            tidepool_mcp::CompanionImports::Omit,
            SHOAL_REPLACED_EFFECT_NAMES,
        ),
        DRIVER_MODULE,
    );
    if let Some(inputs) = inputs {
        include.extend(inputs.include.iter().cloned());
        for module in inputs.import_modules() {
            preamble = insert_preamble_imports(&preamble, module);
        }
    }
    let templates = resident_workbench_templates(&preamble, DRIVER_EFFECTS, "");
    let include_refs: Vec<_> = include.iter().map(PathBuf::as_path).collect();
    let session_root = run_root.join("haskell-session");
    std::fs::create_dir_all(&session_root)?;
    let compiled = match run_turn(HaskellTurnRequest {
        turn_text: DRIVER_ENTRY,
        templates: &templates,
        include: &include_refs,
        session_root: &session_root,
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .map_err(render_root_compile_failure)?
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => {
            return Err(runtime_error(format!(
                "root interactive driver is not an expression: {other:?}"
            )))
        }
    };

    Ok(CompiledShoalDriver {
        preamble,
        include,
        compiled,
    })
}

fn compile_root(
    config: &ActorHostConfig,
    run_root: &Path,
    worktrees: WorktreeManager,
    worktree_authority: ActorWorktreeAuthority,
) -> Result<
    (
        ActorWorkbenchSource,
        ShoalRoot,
        Arc<tidepool_runtime::session::CompiledTurn>,
    ),
    Box<dyn std::error::Error>,
> {
    let CompiledShoalDriver {
        preamble,
        include,
        compiled,
    } = compile_driver(
        &config.haskell_root,
        config.workspace_inputs.as_ref(),
        run_root,
    )?;
    let declarations = shoal_effect_declarations();
    let session_root = run_root.join("haskell-session");
    let session = fresh_session_id();
    let mut module_env = tidepool_mcp::session_decl_module_env_hiding(
        &declarations,
        false,
        tidepool_mcp::CompanionImports::Omit,
        SHOAL_REPLACED_EFFECT_NAMES,
    );
    if let Some(inputs) = &config.workspace_inputs {
        module_env.imports.extend(inputs.imports());
    }
    let mut library = SessionLib::open(session, &session_root, module_env)?
        .with_validation_include(include.clone());
    let recovery_report =
        library.attach_recovery_manifest(config.run_root.join("root-declarations.json"))?;
    tracing::info!(
        source_session = ?recovery_report.source_session,
        successor_session = recovery_report.successor_session,
        replayed = recovery_report.replayed.len(),
        lost = recovery_report.lost.len(),
        "attached Shoal root declaration recovery manifest"
    );
    let worktree_handler =
        ActorWorktreeHandler::new(WorktreeHandler::from_manager(worktrees), worktree_authority);
    let mut machine = ResidentSession::bootstrap(
        &compiled.expr,
        compiled.table.clone(),
        hlist![
            ActorBoundWorktreeHandler::new(worktree_handler.clone()),
            ActorWorktreeRegistryHandler::new(worktree_handler.clone()),
            ActorWorktreeAllocationHandler::new(worktree_handler.clone()),
            ActorWorktreeIntegrationHandler::new(worktree_handler.clone()),
            worktree_handler,
        ],
        CapturedOutput::new(),
        include.clone(),
        DEFAULT_NURSERY_SIZE,
        Some(library),
    )?;
    machine.set_effect_execution(
        EffectRunPolicy::HandleOrSuspend,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    );
    let lexical_scope = machine.mint_isolated_scope();
    machine.set_actor_execution(
        tidepool_runtime::session::SessionRunContext {
            lexical_scope,
            resource_scope: tidepool_codegen::suspension::RealmId::fresh(),
            ..tidepool_runtime::session::SessionRunContext::ROOT
        },
        EffectRunPolicy::HandleOrSuspend,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    )?;
    let outcome = machine.run_with_sites(
        "shoal_root_driver",
        &compiled.expr,
        &compiled.table,
        &compiled.asks,
    )?;
    let resource_scope = match &outcome {
        tidepool_runtime::session::ResidentOutcome::Suspended { hole, .. } => machine
            .parked_realm(hole)
            .ok_or_else(|| runtime_error("root driver suspension had no owning realm"))?,
        _ => {
            return Err(runtime_error(
                "root driver completed before attaching its permanent application",
            ))
        }
    };
    let descriptor = ActorDescriptor::new(
        "shoal-root",
        ActorPlacement {
            session,
            resource_scope,
            lexical_scope,
        },
    )
    // Profiles classify resident Haskell rows, not the native Codex sandbox.
    // The root allocates worktrees and may attenuate children to ReadOnly.
    .with_profile(ActorEffectProfile::ReadWrite)
    .with_effective_role(
        tidepool_actor::EffectiveRole::root().with_research_policy(config.research_policy),
    );
    Ok((
        ActorWorkbenchSource::new(preamble, include)
            .with_default_browse_module(WORKBENCH_SURFACE_MODULE)
            .with_default_quasiquoters(),
        ResidentActorRoot::new(descriptor, machine, outcome),
        Arc::new(compiled),
    ))
}

fn render_root_compile_failure(
    failure: tidepool_runtime::session::TurnFailure,
) -> Box<dyn std::error::Error> {
    let source = failure.attempted_source.as_deref().unwrap_or_default();
    let detail = match &failure.error {
        tidepool_runtime::CompileError::Diagnostics(diagnostics)
        | tidepool_runtime::CompileError::WorkerFailure(diagnostics) => {
            tidepool_runtime::diag::render_diagnostics(
                diagnostics,
                &tidepool_runtime::diag::RenderOpts {
                    anchor: "Expr.hs",
                    label: "<shoal-driver>",
                    user_lines: None,
                    line_offset: 0,
                    col_indent: 0,
                    drop_foreign_gen_warnings_except: None,
                    source,
                },
            )
        }
        _ => failure.to_string(),
    };
    runtime_error(format!(
        "root interactive driver compilation failed:\n{detail}"
    ))
}

fn spawn_undeployed_hosted_retirement(
    retirements: &mut JoinSet<InteractiveCleanupReceipt>,
    actor: ActorRef,
    owners: &InteractiveOwners,
) {
    let retained = {
        let rows = owners.lock();
        rows.get(&actor).and_then(|row| {
            row.hosted
                .lock()
                .clone()
                .map(|service| (service, row.retirement.clone()))
        })
    };
    let Some((mut service, receipt_slot)) = retained else {
        return;
    };
    retirements.spawn(async move {
        let http = stop_retired_tool_service(actor, &mut service).await;
        let receipt = InteractiveCleanupReceipt {
            actor,
            components: vec![
                CleanupComponentReceipt {
                    component: CleanupComponent::ToolService,
                    outcome: http,
                },
                CleanupComponentReceipt {
                    component: CleanupComponent::Process,
                    outcome: CleanupComponentOutcome::Failed {
                        detail: "hosted retirement does not establish native/external cleanup"
                            .into(),
                    },
                },
            ],
        };
        receipt_slot.lock().get_or_insert_with(|| receipt.clone());
        receipt
    });
}

fn spawn_owned_retirement(
    retirements: &mut JoinSet<InteractiveCleanupReceipt>,
    deployment: InteractiveDeployment,
    tmux: TmuxSession,
    owners: &InteractiveOwners,
) {
    let (scope, receipt_slot, native_retirement) = {
        let rows = owners.lock();
        let row = rows
            .get(&deployment.actor)
            .expect("exact deployment retention row");
        (
            row.scoped_retention
                .as_ref()
                .map(|retention| retention.slot.clone()),
            row.retirement.clone(),
            row.native_retirement,
        )
    };
    // Only slots cross the task boundary; neither owns a back-reference to its
    // map row. Namespace cleanup is status only and does not discharge host work.
    retirements.spawn(async move {
        if let Some(scope) = scope.filter(|_| native_retirement == NativeRetirement::Terminate) {
            let stopped = tokio::task::spawn_blocking(move || {
                scoped_custody::stop_slot(
                    &scope,
                    std::time::Instant::now() + APPLICATION_SHUTDOWN_TIMEOUT,
                )
            })
            .await;
            if !matches!(stopped, Ok(Ok(_))) {
                tracing::warn!("scoped process cleanup remains unconfirmed in retained owner");
            }
        }
        let receipt =
            retire_interactive_application_guarded(deployment, &tmux, native_retirement).await;
        receipt_slot.lock().get_or_insert_with(|| receipt.clone());
        receipt
    });
}

async fn run_interactive_applications(
    mut lifecycle: mpsc::UnboundedReceiver<LocalResidentDeployment>,
    application_owners: InteractiveOwners,
    fleet: InteractiveFleet,
    shutdown: watch::Receiver<Option<NativeRetirement>>,
    mut root_config: watch::Receiver<ActorHostConfig>,
) -> Result<(), String> {
    let InteractiveFleet {
        root,
        config,
        run_root,
        tmux,
        backend,
        worktrees,
        bindings,
        readiness,
        worktree_authority,
    } = fleet;
    let base_prompt = FrozenBasePrompt::materialize_selected(
        &run_root,
        config
            .workspace_inputs
            .as_ref()
            .and_then(|inputs| inputs.prompts.get("core"))
            .map(String::as_str),
    )
    .map_err(|error| format!("cannot prepare Shoal base prompt: {error}"))?;
    let mut root_identity = root.identity();
    let mut launch_context = InteractiveLaunchContext {
        base_prompt,
        root: root_identity,
        config,
        run_root,
        tmux: tmux.clone(),
        backend: Arc::clone(&backend),
        worktrees,
        bindings: Arc::clone(&bindings),
    };
    let mut deployments: Vec<InteractiveDeployment> = Vec::new();
    let mut launches = JoinSet::new();
    let mut binding_discoveries = JoinSet::new();
    let mut retirements = JoinSet::new();
    let mut notifications = JoinSet::new();
    let mut publication_retries = JoinSet::new();
    let mut health = tokio::time::interval(Duration::from_secs(1));
    let failure = loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown.clone()) => break None,
            changed = root_config.changed() => {
                if changed.is_ok() { launch_context.config = root_config.borrow_and_update().clone(); }
            }
            _ = health.tick() => {
                for deployment in &deployments {
                    if let (Some(resource), Some(thread)) = (&deployment.build_resource, &deployment.thread) {
                        if let Ok(mut resource) = resource.clone().try_lock_owned() {
                            if resource.native_publication_needs_retry() {
                                let backend = backend.clone();
                                let thread = thread.clone();
                                let actor = deployment.actor;
                                publication_retries.spawn(async move {
                                    if let Err(error) = resource.publish_native(
                                        backend.as_ref(), &thread,
                                        &PathBuf::from(ACTOR_PROJECT_ROOT).join(ACTOR_BUILD_TARGET), &[],
                                    ).await {
                                        tracing::debug!(?actor, %error, "build publication recovery remains pending");
                                    }
                                });
                            }
                        }
                    }
                }
                for index in 0..deployments.len() {
                    let deployment = &deployments[index];
                    let snapshot = deployment.runtime_observation.snapshot();
                    if snapshot.provider_observation_stale { continue; }
                    for turn in snapshot.provider_failures {
                    let deployment = &deployments[index];
                    let tidepool_agent::ProviderTurnState::Failed(failure) = turn.state else { continue; };
                    let key = (turn.thread.clone(), turn.turn.clone());
                    if deployment.notified_provider_failures.contains(&key) { continue; }
                    let actor = deployment.actor;
                    if let Some(supervisor) = deployment.supervisor {
                        let Some(owner) = deployments.iter().find(|app| app.actor == supervisor) else { continue; };
                        let event = DurableActorEvent::Typed(TypedActorEvent::ProviderTurnFailed {
                            revision: turn.revision as u64,
                            actor, thread: turn.thread, turn: turn.turn, failure,
                        });
                        if let Err(error) = publish_inbox_event(Arc::clone(&owner.inbox), event).await {
                            tracing::warn!(?actor, %error, "provider failure notice pending publication");
                            // Preserve source order: a later watermark must not suppress
                            // this failed publication on retry.
                            break;
                        }
                    } else {
                        // Root failures are operator-visible, never self-injected retries.
                        tracing::error!(?actor, ?failure, turn = %turn.turn, "root provider turn needs attention");
                    }
                    deployments[index].notified_provider_failures.insert(key);
                    }
                }
                if let Some(index) = deployments.iter().position(|deployment| {
                    !deployment.failure_reported
                        && deployment.local_actor.terminal().get().is_none()
                        && (hosted_retirement::service_finished(&deployment.service)
                            || matches!(
                                &deployment.connection,
                                InteractiveConnection::Bound { delivery, .. }
                                    if delivery.is_finished()
                            ))
                }) {
                    let local_actor = deployments[index].local_actor.clone();
                    deployments[index].failure_reported = true;
                    let detail = "interactive application exited before actor settlement".to_string();
                    if let Err(error) = apply_application_failure(
                        local_actor,
                        ExternalApplicationFailure {
                            class: ExternalApplicationFailureClass::UnexpectedExit,
                            detail,
                        }
                    ).await {
                        break Some(error);
                    }
                }
            }
            event = lifecycle.recv() => {
                let Some(event) = event else { break None };
                match event {
                    LocalResidentDeployment::PolicyInstalled(installation) => {
                        if installation.creator.is_none() {
                            launch_context.config = root_config.borrow_and_update().clone();
                            root_identity = installation.actor.identity();
                            launch_context.root = root_identity;
                        }
                        worktree_authority.install_grant(
                            installation.actor.identity().into(),
                            worktree_grant(installation.effective_role.role()),
                        );
                        let fork_parent_thread = match installation.context_parent {
                            None => None,
                            Some(parent) => {
                                let Some(thread) = deployments
                                    .iter()
                                    .find(|deployment| deployment.actor == parent)
                                    .and_then(|deployment| deployment.thread.clone())
                                else {
                                    break Some(format!("context-fork parent {parent:?} has no queue-ready conversation"));
                                };
                                Some(thread.id().clone())
                            }
                        };
                        let build_inheritance = installation.worktree_custody.as_ref()
                            .and_then(|custody| (custody.as_ref() as &dyn std::any::Any)
                                .downcast_ref::<ActorWorkspaceCustody>())
                            .map(|custody| custody.build_inheritance.clone())
                            .unwrap_or_default();
                        let native_admission = NativeForkAdmission {
                            owners: application_owners.clone(), backend: backend.clone(),
                        };
                        let context = launch_context.clone();
                        let actor = installation.actor.identity();
                        let (cancel, cancelled) = oneshot::channel();
                        let mut owners = application_owners.lock();
                        if owners.contains_key(&actor) {
                            break Some(format!("duplicate application owner for {actor:?}"));
                        }
                        let hosted_slot = Arc::new(Mutex::new(None));
                        let pane_slot = Arc::new(Mutex::new(None));
                        owners.insert(actor, InteractiveApplicationOwner {
                            creator_build: None,
                            cancel: Some(cancel),
                            native_retirement: NativeRetirement::Preserve,
                            pane: pane_slot.clone(),
                            fork_gate: installation.fork_gate.clone(),
                            custody: installation.worktree_custody.clone(),
                            scoped_retention: None, // No scope launch selection before native pin.
                            hosted: hosted_slot.clone(),
                            launch: HostLaunchState::Pending,
                            terminal: None,
                            retirement: Arc::new(Mutex::new(None)),
                        });
                        drop(owners);
                        launches.spawn(async move {
                            let local_actor = installation.actor.clone();
                            let result = AssertUnwindSafe(async {
                                let build_snapshot = match build_inheritance {
                                    BuildInheritance::Prepared(snapshot) => snapshot,
                                    BuildInheritance::Unprepared => match installation.creator {
                                        Some(creator) => native_admission.build_snapshot(creator, installation.effective_role.native_tools()).await,
                                        None => None,
                                    },
                                };
                                launch_interactive_application(
                                    installation,
                                    context,
                                    cancelled,
                                    InteractiveInheritance { thread: fork_parent_thread, build_snapshot },
                                    hosted_slot,
                                    pane_slot,
                                ).await
                            })
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|_| {
                                Err(application_error(
                                    actor,
                                    InteractiveOperation::PrepareRuntime,
                                    "interactive launch task panicked",
                                ))
                            });
                            (local_actor, result)
                        });
                    }
                    LocalResidentDeployment::SessionReady { activation } => {
                        let actor = activation.id.actor();
                        let Some(application) = deployments.iter_mut().find(|app| app.actor == actor) else {
                            break Some(format!("resident actor {actor:?} requested a session activation without a deployed application"));
                        };
                        if accepts_activation(application, &activation) {
                            let sequence = activation.id.sequence();
                            if let Err(error) = publish_inbox_event(
                                Arc::clone(&application.inbox),
                                DurableActorEvent::session(&activation),
                            ).await {
                                application.failure_reported = true;
                                let local_actor = application.local_actor.clone();
                                tracing::warn!(?actor, %error, "actor activation delivery degraded");
                                if let Err(error) = apply_application_failure(
                                    local_actor,
                                    ExternalApplicationFailure {
                                        class: ExternalApplicationFailureClass::ToolHostStartup,
                                        detail: error,
                                    },
                                )
                                .await
                                {
                                    break Some(error);
                                }
                                continue;
                            }
                            application
                                .runtime_observation
                                .publish_request_activation(activation.request, sequence);
                            application.last_activation_sequence = sequence;
                        }
                    }
                    LocalResidentDeployment::Retired { actor, terminal } => {
                        worktree_authority.remove_grant(actor.into());
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.retired(terminal);
                        }
                        if let Some(index) = deployments.iter().position(|app| app.actor == actor) {
                            let deployment = deployments.swap_remove(index);
                            spawn_owned_retirement(&mut retirements, deployment, tmux.clone(), &application_owners);
                        } else {
                            spawn_undeployed_hosted_retirement(&mut retirements, actor, &application_owners);
                        }
                    }
                    LocalResidentDeployment::NotificationSend(command) => {
                        // No correlated notification controller is installed yet.
                        // Reject before publication rather than route through legacy push.
                        command.rejected(tidepool_actor::NotificationError::Unavailable);
                    }
                    LocalResidentDeployment::NotificationPoll(command) => {
                        let result = deployments.iter()
                            .find(|application| application.actor == command.receipt().target())
                            .ok_or(tidepool_actor::NotificationError::Unavailable)
                            .and_then(|application| observe_notification_receipt(
                                &command, application.actor,
                                &application.notification_inbox_key, &application.inbox,
                            ));
                        command.observed(result);
                    }
                    LocalResidentDeployment::RequestUpdate { delivery } => {
                        let target = delivery.target();
                        let Some(presentation) = delivery.begin() else { continue; };
                        let Some(application) = deployments.iter().find(|app| app.actor == target) else {
                            presentation.not_presented("target application unavailable".into());
                            continue;
                        };
                        let Some(thread) = application.thread.clone() else {
                            presentation.not_presented("target conversation is not bound".into());
                            continue;
                        };
                        let workspace = application.workspace.clone();
                        let backend = Arc::clone(&backend);
                        notifications.spawn(async move {
                            match backend.present_update(&workspace.to_string_lossy(), &thread, presentation.key(), presentation.message()).await {
                                Ok(()) => presentation.presented(),
                                Err(tidepool_agent::UpdatePresentationError::NotSubmitted(error)) => presentation.not_presented(error.to_string()),
                                Err(tidepool_agent::UpdatePresentationError::Unconfirmed(error)) => presentation.unconfirmed(error.to_string()),
                            }
                            (target, Ok(()))
                        });
                    }
                    LocalResidentDeployment::ChildExited { notice } => {
                        if let Some(notification) = prepare_owner_notification(&notice, &deployments) {
                            notifications.spawn(publish_owner_notification(notification));
                        }
                    }
                    LocalResidentDeployment::WatchChanged { notification } => {
                        let Some(application) = deployments
                            .iter()
                            .find(|app| app.actor == notification.owner)
                        else {
                            continue;
                        };
                        notifications.spawn(publish_inbox_event_for(
                            notification.owner,
                            Arc::clone(&application.inbox),
                            DurableActorEvent::Typed(TypedActorEvent::WatchChanged {
                                notification,
                            }),
                        ));
                    }
                    LocalResidentDeployment::RequestCancellation { notification } => {
                        let Some(application) = deployments
                            .iter()
                            .find(|app| app.actor == notification.target)
                        else {
                            continue;
                        };
                        notifications.spawn(publish_inbox_event_for(
                            notification.target,
                            Arc::clone(&application.inbox),
                            DurableActorEvent::Typed(TypedActorEvent::RequestCancellation {
                                notification,
                            }),
                        ));
                    }
                }
            }
            recovered = publication_retries.join_next(), if !publication_retries.is_empty() => {
                if let Some(Err(error)) = recovered {
                    tracing::warn!(%error, "build publication recovery task interrupted; durable state retained");
                }
            }
            launched = launches.join_next(), if !launches.is_empty() => {
                match launched {
                    Some(Ok((local_actor, Ok(Some(launched))))) => {
                        let actor = local_actor.identity();
                        let already_retired = {
                            let mut owners = application_owners.lock();
                            let owner = owners.get_mut(&actor).expect("registered launch owner");
                            owner.launch = HostLaunchState::Published;
                            owner.terminal.is_some()
                        };
                        let deployment = launched.deployment;
                        if already_retired {
                            spawn_owned_retirement(&mut retirements, deployment, tmux.clone(), &application_owners);
                            continue;
                        }
                        if actor == root_identity {
                            let _ = readiness.send(ActorHostReadiness::AwaitingBinding { root: root_identity });
                        }
                        let pane = deployment.pane.clone();
                        let fork_gate = deployment.fork_gate.clone();
                        let runtime_observation = deployment.runtime_observation.clone();
                        let fork_parent_thread = deployment.fork_parent_thread.clone();
                        let tmux = tmux.clone();
                        binding_discoveries.spawn(async move {
                            let result = AssertUnwindSafe(discover_interactive_binding(
                                actor,
                                launched.binding,
                                &tmux,
                                &pane,
                            ))
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|_| {
                                Err(application_error(
                                    actor,
                                    InteractiveOperation::DiscoverBinding,
                                    "interactive binding task panicked",
                                ))
                            });
                            if let Ok(thread) = &result {
                                runtime_observation.publish_provider_binding(
                                    fork_parent_thread.map(|thread| thread.0),
                                    thread.id().0.clone(),
                                );
                            }
                            let result = match (result, fork_gate) {
                                (Ok(thread), Some(gate)) => {
                                    match gate.mark_ready() {
                                        Err(error) => Err(application_error(
                                            actor,
                                            InteractiveOperation::DiscoverBinding,
                                            error,
                                        )),
                                        Ok(()) => match gate.wait_committed().await {
                                            Ok(()) => Ok(thread),
                                            Err(error) => Err(application_error(
                                                actor,
                                                InteractiveOperation::DiscoverBinding,
                                                error,
                                            )),
                                        },
                                    }
                                }
                                (result, None) => result,
                                (Err(error), Some(_)) => Err(error),
                            };
                            (actor, result)
                        });
                        deployments.push(deployment);
                    }
                    Some(Ok((local_actor, Ok(None)))) => {
                        let actor = local_actor.identity();
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.launch = HostLaunchState::Abandoned;
                            owner.cancel();
                        }
                    }
                    Some(Ok((local_actor, Err(error)))) => {
                        let actor = local_actor.identity();
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.launch = HostLaunchState::Failed;
                            owner.cancel();
                        }
                        if let Err(error) = apply_application_failure(
                            local_actor,
                            ExternalApplicationFailure {
                                class: error.operation.failure_class(),
                                detail: error.detail,
                            }
                        ).await {
                            break Some(error);
                        }
                    }
                    Some(Err(error)) => break Some(format!("interactive launch task: {error}")),
                    None => {}
                }
            }
            discovered = binding_discoveries.join_next(), if !binding_discoveries.is_empty() => {
                match discovered {
                    Some(Ok((actor, Ok(thread)))) => {
                        let Some(deployment) = deployments.iter_mut().find(|app| app.actor == actor) else {
                            continue;
                        };
                        if !matches!(deployment.connection, InteractiveConnection::AwaitingBinding) {
                            break Some(format!("interactive application {actor:?} published more than one conversation binding"));
                        }
                        let (delivery_shutdown, stop_delivery) = oneshot::channel();
                        let delivery = tokio::spawn(run_delivery_pump(
                            actor,
                            Arc::clone(&deployment.inbox),
                            thread.clone(),
                            Arc::clone(&backend),
                            deployment.workspace.clone(),
                            deployment.runtime_observation.clone(),
                            stop_delivery,
                        ));
                        deployment.connection = InteractiveConnection::Bound {
                            delivery_shutdown,
                            delivery,
                        };
                        deployment.thread = Some(thread.clone());
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.creator_build = deployment.build_resource.as_ref().map(|resource| CreatorBuild {
                                resource: resource.clone(), thread: thread.clone(),
                            });
                        }
                        if actor == root_identity {
                            let _ = readiness.send(ActorHostReadiness::Ready {
                                root: root_identity,
                                thread,
                            });
                        }
                    }
                    Some(Ok((actor, Err(error)))) => {
                        let Some(deployment) = deployments.iter_mut().find(|app| app.actor == actor) else {
                            continue;
                        };
                        if let Some(gate) = &deployment.fork_gate {
                            let _ = gate.mark_failed();
                        }
                        deployment.failure_reported = true;
                        let Some(local_actor) = deployments
                            .iter()
                            .find(|application| application.actor == actor)
                            .map(|application| application.local_actor.clone())
                        else {
                            break Some(format!("lost exact local actor for failed application {actor:?}"));
                        };
                        if let Err(error) = apply_application_failure(
                            local_actor,
                            ExternalApplicationFailure {
                                class: error.operation.failure_class(),
                                detail: error.detail,
                            }
                        ).await {
                            break Some(error);
                        }
                    }
                    Some(Err(error)) => break Some(format!("interactive binding task: {error}")),
                    None => {}
                }
            }
            retired = retirements.join_next(), if !retirements.is_empty() => {
                match retired {
                    Some(Ok(receipt)) => {
                        if let Some(owner) = application_owners.lock().get_mut(&receipt.actor) {
                            owner.retirement.lock().get_or_insert_with(|| receipt.clone());
                        }
                        let degraded = receipt.degraded();
                        if degraded {
                            tracing::warn!(actor = ?receipt.actor, components = ?receipt.components, "interactive application cleanup degraded");
                            if receipt.actor != root_identity {
                                if let Some(root_application) = deployments.iter().find(|app| app.actor == root_identity) {
                                    notifications.spawn(publish_inbox_event_for(
                                        root_identity,
                                        Arc::clone(&root_application.inbox),
                                        DurableActorEvent::Typed(TypedActorEvent::CleanupFinished { receipt }),
                                    ));
                                }
                            }
                        } else {
                            tracing::info!(actor = ?receipt.actor, "interactive application retired");
                        }
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "interactive retirement task join failed after cleanup isolation");
                    }
                    None => {}
                }
            }
            notified = notifications.join_next(), if !notifications.is_empty() => {
                match notified {
                    Some(Ok((_actor, Ok(())))) => {}
                    Some(Ok((actor, Err(error)))) => {
                        tracing::warn!(?actor, %error, "actor notification delivery degraded");
                        let Some(local_actor) = deployments
                            .iter()
                            .find(|application| application.actor == actor)
                            .map(|application| application.local_actor.clone())
                        else {
                            continue;
                        };
                        if let Err(error) = apply_application_failure(
                            local_actor,
                            ExternalApplicationFailure {
                                class: ExternalApplicationFailureClass::ToolHostStartup,
                                detail: error,
                            },
                        )
                        .await
                        {
                            break Some(error);
                        }
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "actor notification task join failed");
                    }
                    None => {}
                }
            }
        }
    };

    let native_retirement = if failure.is_some() {
        NativeRetirement::Preserve
    } else {
        shutdown.borrow().unwrap_or_default()
    };
    for owner in application_owners.lock().values_mut() {
        owner.native_retirement = native_retirement;
        owner.cancel();
    }
    let launch_cleanup =
        drain_launches_for_shutdown(&mut launches, APPLICATION_SHUTDOWN_TIMEOUT).await;
    deployments.extend(
        launch_cleanup
            .completed
            .into_iter()
            .map(|launched| launched.deployment),
    );
    let launch_failure = if launch_cleanup.failures.is_empty() {
        None
    } else {
        Some(
            launch_cleanup
                .failures
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    };
    let publication_cleanup = tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        let mut failure = None;
        while let Some(result) = publication_retries.join_next().await {
            if let Err(error) = result {
                failure.get_or_insert_with(|| format!("build publication recovery task: {error}"));
            }
        }
        failure
    })
    .await
    .unwrap_or_else(|_| {
        publication_retries.abort_all();
        Some("build publication recovery timed out; durable state and storage retained".into())
    });
    binding_discoveries.abort_all();
    while binding_discoveries.join_next().await.is_some() {}
    let undeployed = application_owners
        .lock()
        .keys()
        .copied()
        .filter(|actor| {
            !deployments
                .iter()
                .any(|deployment| deployment.actor == *actor)
        })
        .collect::<Vec<_>>();
    for actor in undeployed {
        spawn_undeployed_hosted_retirement(&mut retirements, actor, &application_owners);
    }
    for deployment in deployments {
        spawn_owned_retirement(
            &mut retirements,
            deployment,
            tmux.clone(),
            &application_owners,
        );
    }
    let notification_cleanup = tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        let mut failure = None;
        while let Some(result) = notifications.join_next().await {
            let result = result
                .map_err(|error| format!("owner notification task: {error}"))
                .and_then(|(_actor, result)| result);
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        failure
    })
    .await
    .unwrap_or_else(|_| Some("owner notification cleanup timed out".into()));
    let cleanup_failure = tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        let mut failure = None;
        while let Some(result) = retirements.join_next().await {
            if let Ok(receipt) = &result {
                if let Some(owner) = application_owners.lock().get_mut(&receipt.actor) {
                    owner.retirement.lock().get_or_insert_with(|| receipt.clone());
                }
            }
            match result {
                Ok(receipt) if receipt.degraded() && receipt.actor == root_identity => {
                    failure.get_or_insert_with(|| receipt.render());
                }
                Ok(receipt) if receipt.degraded() => {
                    tracing::warn!(actor = ?receipt.actor, components = ?receipt.components, "child cleanup degraded during host shutdown");
                }
                Ok(_) => {}
                Err(error) => {
                    failure.get_or_insert_with(|| format!("interactive retirement task: {error}"));
                }
            }
        }
        failure
    })
    .await
    .unwrap_or_else(|_| Some("interactive application cleanup timed out".into()));
    let cleanup_failures = [
        launch_failure,
        publication_cleanup,
        notification_cleanup,
        cleanup_failure,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let cleanup_failure = if cleanup_failures.is_empty() {
        None
    } else {
        Some(cleanup_failures.join("; "))
    };
    match (failure, cleanup_failure) {
        (Some(error), Some(cleanup)) => Err(format!("{error}; cleanup: {cleanup}")),
        (Some(error), None) | (None, Some(error)) => Err(error),
        (None, None) => Ok(()),
    }
}

struct LaunchShutdown<T> {
    completed: Vec<T>,
    failures: Vec<LaunchShutdownFailure>,
}

#[derive(Debug, thiserror::Error)]
enum LaunchShutdownFailure {
    #[error("interactive launch cleanup failed: {0}")]
    Launch(InteractiveApplicationError),
    #[error("interactive launch join failed; cleanup unconfirmed: {0}")]
    Join(tokio::task::JoinError),
    #[error("interactive launch cleanup timed out with {pending} unsettled tasks; abort requested, cleanup unconfirmed")]
    TimedOut { pending: usize },
}

impl<T> LaunchShutdown<T> {
    fn record<A>(
        &mut self,
        result: Result<(A, Result<Option<T>, InteractiveApplicationError>), tokio::task::JoinError>,
    ) {
        match result {
            Ok((_, Ok(Some(completed)))) => self.completed.push(completed),
            Ok((_, Ok(None))) => {}
            Ok((_, Err(error))) => self.failures.push(LaunchShutdownFailure::Launch(error)),
            Err(error) => self.failures.push(LaunchShutdownFailure::Join(error)),
        }
    }
}

/// Keep observed completed deployments outside timeout-owned futures so they can
/// still be retired if a later launch fails or cannot finish. Aborting a task is
/// not proof that its process, hosted work or resource cleanup completed.
async fn drain_launches_for_shutdown<A: Send + 'static, T: Send + 'static>(
    launches: &mut JoinSet<(A, Result<Option<T>, InteractiveApplicationError>)>,
    grace: Duration,
) -> LaunchShutdown<T> {
    let mut outcome = LaunchShutdown {
        completed: Vec::new(),
        failures: Vec::new(),
    };
    let deadline = tokio::time::Instant::now() + grace;
    while !launches.is_empty() {
        match tokio::time::timeout_at(deadline, launches.join_next()).await {
            Ok(Some(result)) => outcome.record(result),
            Ok(None) => break,
            Err(_) => {
                outcome.failures.push(LaunchShutdownFailure::TimedOut {
                    pending: launches.len(),
                });
                launches.abort_all();
                // Preserve results already ready at the cutoff. Do not wait
                // indefinitely for cancellation or label it cleanup success.
                while let Some(result) = launches.try_join_next() {
                    outcome.record(result);
                }
                break;
            }
        }
    }
    outcome
}

struct InteractiveInheritance {
    thread: Option<BackendThreadId>,
    build_snapshot: Option<OverlaySnapshot>,
}

async fn launch_interactive_application(
    installation: LocalResidentInstallation,
    context: InteractiveLaunchContext,
    cancelled: oneshot::Receiver<NativeRetirement>,
    inherited: InteractiveInheritance,
    hosted_slot: hosted_retirement::HostedSlot,
    pane_slot: Arc<Mutex<Option<TmuxPaneId>>>,
) -> Result<Option<LaunchedInteractiveApplication>, InteractiveApplicationError> {
    let worktree = prepare_actor_worktree(&installation, &context)?;
    launch_prepared_interactive_application(
        installation,
        context,
        worktree,
        cancelled,
        inherited,
        hosted_slot,
        pane_slot,
    )
    .await
}

fn prepare_actor_worktree(
    installation: &LocalResidentInstallation,
    context: &InteractiveLaunchContext,
) -> Result<Option<WorktreeHandle>, InteractiveApplicationError> {
    let actor = installation.actor.identity();
    let raw_id =
        match actor_workspace_request(actor == context.root, &installation.launch_worktrees)
            .map_err(|detail| {
                application_error(actor, InteractiveOperation::BindWorktree, detail)
            })? {
            ActorWorkspaceRequest::SourceCheckout => return Ok(None),
            ActorWorkspaceRequest::Worktree(raw_id) => raw_id,
        };
    if !WorktreeId::is_path_safe(raw_id) {
        return Err(application_error(
            actor,
            InteractiveOperation::BindWorktree,
            "the worktree recipe carried an invalid durable id",
        ));
    }
    let id = WorktreeId::from_raw(raw_id);
    let handle = context
        .worktrees
        .lookup(&id)
        .map_err(|error| application_error(actor, InteractiveOperation::BindWorktree, error))?
        .ok_or_else(|| {
            application_error(
                actor,
                InteractiveOperation::BindWorktree,
                format!("worktree {id} is not registered"),
            )
        })?;
    let principal = WorktreePrincipal::exact_actor(
        &runtime_namespace(&context.run_root),
        actor.id.0,
        actor.incarnation.0,
    );
    if installation.worktree_custody.is_none()
        || !context
            .bindings
            .lock()
            .current(handle.id())
            .is_some_and(|binding| binding.agent() == &principal)
    {
        return Err(application_error(
            actor,
            InteractiveOperation::BindWorktree,
            "exact pre-bootstrap worktree custody is absent",
        ));
    }
    Ok(Some(handle))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActorWorkspaceRequest<'a> {
    /// The actor may inspect the source checkout. Only the root receives write
    /// authority for it; an ordinary actor without a worktree remains a useful
    /// orchestration or review actor rather than acquiring ambient code-write
    /// authority by accident.
    SourceCheckout,
    Worktree(&'a str),
}

fn actor_workspace_request<'a>(
    root: bool,
    launch_worktrees: &'a [String],
) -> Result<ActorWorkspaceRequest<'a>, String> {
    match (root, launch_worktrees) {
        (_, []) => Ok(ActorWorkspaceRequest::SourceCheckout),
        (false, [worktree]) => Ok(ActorWorkspaceRequest::Worktree(worktree)),
        (true, _) => Err("the root application may not carry a child worktree recipe".into()),
        (false, worktrees) => Err(format!(
            "an interactive actor may carry at most one worktree recipe, received {}",
            worktrees.len()
        )),
    }
}

fn current_time_ms() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn retained_workspace_command(
    executable: &Path,
    view: &tidepool_node::MountNamespace,
    cwd: &Path,
    command: ProcessInvocation,
) -> std::io::Result<ProcessInvocation> {
    if !executable.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Shoal entry executable must be absolute",
        ));
    }
    let mut args = vec![
        "enter-view".into(),
        "--view".into(),
        serde_json::to_string(&view.entry()?).map_err(std::io::Error::other)?,
        "--cwd".into(),
        cwd.to_string_lossy().into_owned(),
        "--".into(),
        command.program,
    ];
    args.extend(command.args);
    Ok(ProcessInvocation {
        program: executable.to_string_lossy().into_owned(),
        args,
    })
}

async fn launch_prepared_interactive_application(
    installation: LocalResidentInstallation,
    context: InteractiveLaunchContext,
    mut worktree: Option<WorktreeHandle>,
    mut cancelled: oneshot::Receiver<NativeRetirement>,
    inherited: InteractiveInheritance,
    hosted_slot: hosted_retirement::HostedSlot,
    pane_slot: Arc<Mutex<Option<TmuxPaneId>>>,
) -> Result<Option<LaunchedInteractiveApplication>, InteractiveApplicationError> {
    let InteractiveInheritance {
        thread: fork_parent_thread,
        build_snapshot,
    } = inherited;
    let InteractiveLaunchContext {
        base_prompt,
        root,
        config,
        run_root,
        tmux,
        backend,
        worktrees,
        bindings: _,
    } = context;
    let actor = installation.actor;
    let fork_gate = installation.fork_gate.clone();
    let runtime_observation = installation.runtime_observation.clone();
    let actor_identity = actor.identity();
    let workspace = worktree.as_ref().map_or_else(
        || config.workspace.clone(),
        |handle| handle.cwd().to_path_buf(),
    );
    let actor_root = run_root.join(format!(
        "{}-{}",
        actor_identity.id.0, actor_identity.incarnation.0
    ));
    std::fs::create_dir_all(&actor_root).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let git_common_dir =
        tidepool_worktree::git::inspect::git_common_dir(worktrees.git(), &workspace).map_err(
            |error| application_error(actor_identity, InteractiveOperation::PrepareRuntime, error),
        )?;
    let writable_roots = writable_repository_roots(
        actor_identity == root,
        installation.effective_role.workspace(),
        &config.workspace,
        worktree.as_ref().map(WorktreeHandle::cwd),
        &git_common_dir,
    );
    let agent_workspace = PathBuf::from(ACTOR_PROJECT_ROOT);
    let native_tool_policy = native_tool_policy(installation.effective_role.native_tools());
    let policy_mounts = backend
        .prepare_native_tool_policy(native_tool_policy, &actor_root.join("native-policy"))
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?;
    let mut process_boundary = ProcessMountBoundary::new(
        &workspace,
        [
            config.workspace.clone(),
            worktrees.managed_root().to_path_buf(),
            git_common_dir,
        ],
        writable_roots,
    )
    .and_then(|boundary| boundary.with_project_root(&agent_workspace))
    .and_then(|boundary| {
        boundary.with_read_only_overlay(base_prompt.directory(), base_prompt.directory())
    })
    .map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    for InteractivePolicyMount { source, target } in policy_mounts {
        process_boundary = process_boundary
            .with_read_only_overlay(source, target)
            .map_err(|error| {
                application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
            })?;
    }
    if cancelled.try_recv().is_ok() {
        return Ok(None);
    }
    let mut build_resource = if native_tool_policy == InteractiveNativeToolPolicy::InspectionOnly {
        None
    } else {
        let run_id = run_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("run");
        let lease = OverlayResourceLease::allocate_build(run_id, actor_identity, build_snapshot)
            .map_err(|error| {
                application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
            })?;
        let mountpoint = workspace.join(ACTOR_BUILD_TARGET);
        std::fs::create_dir_all(&mountpoint).map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?;
        process_boundary = lease
            .mount(process_boundary, &agent_workspace.join(ACTOR_BUILD_TARGET))
            .map_err(|error| {
                application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
            })?;
        tracing::info!(
            actor = ?actor_identity,
            resource = %lease.path().display(),
            target = ACTOR_BUILD_TARGET,
            "actor build resource allocated"
        );
        Some(lease)
    };
    let build_output = build_resource
        .as_ref()
        .map(|_| agent_workspace.join(ACTOR_BUILD_TARGET));
    let run_socket_id = run_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("run");
    let socket_root = std::env::temp_dir().join(format!(
        "tidepool-{}-{}-{}",
        &run_socket_id[..run_socket_id.len().min(8)],
        actor_identity.id.0,
        actor_identity.incarnation.0
    ));
    let (mut socket_directory, listener, inbox) = prepare_socket_inbox(
        actor_identity,
        socket_root,
        actor_root.join("inbox.jsonl"),
        actor_root.join("inbox.cursor"),
    )?;
    let endpoint = socket_directory.path().join("host-tools.sock");
    let binding_path = if actor_identity == root {
        config.root_binding_path.clone()
    } else {
        actor_root.join("binding.json")
    };
    let launch_mode = if let Some(parent) = fork_parent_thread.clone() {
        let boundary = installation
            .fork_boundary
            .as_ref()
            .filter(|boundary| boundary.thread_id == parent.0 && !boundary.call_id.is_empty())
            .ok_or_else(|| {
                application_error(
                    actor_identity,
                    InteractiveOperation::BuildCommand,
                    "context fork has no matching parent hosted-call boundary",
                )
            })?;
        InteractiveLaunchMode::Fork {
            parent,
            after_call: boundary.call_id.clone(),
        }
    } else if actor_identity == root {
        config.root_launch_mode.clone()
    } else {
        InteractiveLaunchMode::Fresh
    };
    let expected_resume = match &launch_mode {
        InteractiveLaunchMode::Resume(thread) => Some(thread.clone()),
        InteractiveLaunchMode::Fresh | InteractiveLaunchMode::Fork { .. } => None,
    };
    runtime_observation.publish_cache_boundary(match &launch_mode {
        InteractiveLaunchMode::Fresh => tidepool_actor::CacheBoundaryReason::Fresh,
        InteractiveLaunchMode::Fork { .. } => tidepool_actor::CacheBoundaryReason::ForkedPrefix,
        InteractiveLaunchMode::Resume(_) => tidepool_actor::CacheBoundaryReason::ReattachedThread,
    });
    let workspace_observation = tidepool_actor::ActorWorkspaceObservation {
        workspace_path: agent_workspace.clone(),
        host_storage_path: workspace.clone(),
        worktree_id: installation.launch_worktrees.first().cloned(),
        expected_branch: worktree
            .as_ref()
            .map(|tree| tree.branch().as_str().to_owned()),
    };
    runtime_observation.publish_workspace(workspace_observation);
    runtime_observation.publish_launch_role(installation.effective_role.clone(), current_time_ms());
    let resolved_worker = installation.creator.map(|_| {
        resolve_worker_launch(
            &config,
            &tidepool_actor::WorkerLaunchRequest {
                role: installation.effective_role.clone(),
                model: installation.model.clone(),
                effort: installation.fork_effort,
                context: if matches!(launch_mode, InteractiveLaunchMode::Fork { .. }) {
                    tidepool_actor::ForkContext::InheritedContext
                } else {
                    tidepool_actor::ForkContext::SelectedContext
                },
                instructions: installation.instructions.clone(),
            },
            &blake3::hash(base_prompt.body().as_bytes())
                .to_hex()
                .to_string(),
        )
    });
    let developer_instructions = if let Some(resolved) = &resolved_worker {
        resolved.instructions.clone()
    } else {
        let mut instructions = developer_instructions_selected(
            &installation.effective_role,
            &launch_mode,
            config.workspace_inputs.as_ref(),
            installation.instructions.as_deref(),
        );
        append_inheritance_authority(&mut instructions);
        instructions
    };
    let developer_instructions =
        orient_launch_instructions(&developer_instructions, &runtime_observation.snapshot());
    runtime_observation.publish_prompt_profile(
        installation.effective_role.prompt_profile(),
        PromptId::CATALOG_VERSION,
        PromptId::composed_fingerprint(
            base_prompt.body(),
            &developer_instructions,
            &tidepool_actor::shoal_hosted_prompt_fingerprint(),
        ),
    );
    let (model, effort) = if let Some(resolved) = resolved_worker {
        (
            resolved.model,
            launch_effort(&launch_mode, ReasoningEffort::Low, Some(resolved.effort)),
        )
    } else {
        (
            installation.model.clone().or_else(|| {
                (!matches!(launch_mode, InteractiveLaunchMode::Fork { .. }))
                    .then(|| config.model.clone())
            }),
            launch_effort(&launch_mode, config.effort, installation.fork_effort),
        )
    };
    let spec = InteractiveAgentSpec {
        mode: launch_mode,
        // Shoal owns continuation on every node. Keep the native tool surface
        // identical across roots and forks, without inheriting native goals.
        goal_policy: tidepool_agent::InteractiveGoalPolicy::Disabled,
        model,
        effort: Some(effort),
        developer_instructions,
        base_instructions_file: base_prompt.file().to_path_buf(),
        initial_prompt: installation.initial_user_message.clone(),
        native_sandbox: InteractiveNativeSandbox::HostMountBoundary,
        host_tools_socket: endpoint,
    };
    let command = backend.render(&spec).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::BuildCommand, error)
    })?;
    runtime_observation.publish_backend_provenance(
        config
            .interactive_agent
            .executable()
            .to_string_lossy()
            .into_owned(),
        config.interactive_agent.version().into(),
        spec.model.clone(),
        spec.effort.map(|effort| format!("{effort:?}")),
    );
    if let Some(resource) = &mut build_resource {
        resource.process_may_exist();
    }
    if let Some(custody) = &installation.worktree_custody {
        custody.process_may_exist();
    }
    let workspace_view = tokio::task::spawn_blocking(move || {
        process_boundary.prepare_view(
            BUBBLEWRAP_PROGRAM,
            std::time::Instant::now() + PROCESS_OPERATION_TIMEOUT,
        )
    })
    .await
    .map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?
    .map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    if let Some(handle) = &worktree {
        let manager = worktrees.clone();
        let id = handle.id().clone();
        let namespace = workspace_view.clone();
        let visible_root = agent_workspace.clone();
        worktree = Some(
            tokio::task::spawn_blocking(move || {
                manager.mount_worktree(&id, namespace, &visible_root)
            })
            .await
            .map_err(|error| {
                application_error(actor_identity, InteractiveOperation::BindWorktree, error)
            })?
            .map_err(|error| {
                application_error(actor_identity, InteractiveOperation::BindWorktree, error)
            })?,
        );
    }
    let command = retained_workspace_command(
        &config.shoal_executable,
        &workspace_view,
        &agent_workspace,
        ProcessInvocation {
            program: command.program,
            args: command.args,
        },
    )
    .map_err(|error| {
        application_error(actor_identity, InteractiveOperation::BuildCommand, error)
    })?;
    // Accepted hosted work may outlive listener cancellation. Retention starts
    // before either hosted submission or native process submission can occur.
    socket_directory.work_may_exist();
    let service = hosted_retirement::start(
        &hosted_slot,
        actor.clone(),
        binding_path.clone(),
        expected_resume.clone(),
        listener,
    )
    .map_err(|error| {
        application_error(actor_identity, InteractiveOperation::ServeToolHost, error)
    })?;
    if cancelled.try_recv().is_ok() {
        let _ = hosted_retirement::observe(
            &service,
            hosted_retirement::CompletionBoundary::AbortForShutdown,
            APPLICATION_TASK_GRACE_TIMEOUT,
        )
        .await;
        return Err(socket_launch_failure(
            actor_identity,
            InteractiveOperation::LaunchProcess,
            "launch cancelled after hosted work submission",
            socket_directory,
        ));
    }
    let mut launch_environment = actor_launch_environment(
        config.pane_environment.clone(),
        actor_identity == root,
        build_output.as_deref(),
    );
    // Shell commands must resolve the same verified installation as delivery.
    // An inherited PATH or override can name a different rollout protocol.
    launch_environment.set.insert(
        "TIDEPOOL_INTERACTIVE_CODEX_BIN".into(),
        config
            .interactive_agent
            .executable()
            .to_string_lossy()
            .into_owned(),
    );
    let pane = match tokio::time::timeout(
        PROCESS_OPERATION_TIMEOUT,
        tmux.spawn_window(&TmuxLaunch {
            window_name: format!(
                "{} [{}@{}]",
                installation
                    .label
                    .rsplit('/')
                    .next()
                    .unwrap_or(&installation.label),
                actor_identity.id.0,
                actor_identity.incarnation.0
            ),
            cwd: workspace.clone(),
            program: command.program,
            args: command.args,
            environment: launch_environment.set,
            unset_environment: launch_environment.unset,
        }),
    )
    .await
    {
        Ok(Ok(pane)) => pane,
        Ok(Err(error)) => {
            let _ = hosted_retirement::observe(
                &service,
                hosted_retirement::CompletionBoundary::AbortForShutdown,
                APPLICATION_TASK_GRACE_TIMEOUT,
            )
            .await;
            return Err(socket_launch_failure(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                error,
                socket_directory,
            ));
        }
        Err(_) => {
            let _ = hosted_retirement::observe(
                &service,
                hosted_retirement::CompletionBoundary::AbortForShutdown,
                APPLICATION_TASK_GRACE_TIMEOUT,
            )
            .await;
            return Err(socket_launch_failure(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                format!("tmux launch exceeded {PROCESS_OPERATION_TIMEOUT:?}"),
                socket_directory,
            ));
        }
    };

    *pane_slot.lock() = Some(pane.clone());
    if let Err(error) = tmux.retain_pane_on_exit(&pane).await {
        tracing::warn!(actor = ?actor_identity, %error, "cannot retain actor pane for exit diagnosis; application remains active");
    }

    if actor_identity == root {
        if let Err(error) = tmux.select_window_for_pane(&pane).await {
            tracing::warn!(actor = ?actor_identity, %error, "cannot select root window; application remains active");
        }
    }
    if let Ok(native_retirement) = cancelled.try_recv() {
        abandon_interactive_application(
            &tmux,
            &pane,
            service,
            socket_directory.path(),
            native_retirement,
        )
        .await;
        return Err(socket_launch_failure(
            actor_identity,
            InteractiveOperation::LaunchProcess,
            "launch cancelled after native submission",
            socket_directory,
        ));
    }
    tracing::info!(
        actor = ?actor_identity,
        pane = pane.as_str(),
        worktree = worktree
            .as_ref()
            .map(|handle| handle.id().as_str())
            .unwrap_or("source"),
        "interactive application launched"
    );
    let binding_control = service.lock().await.control.clone();
    Ok(Some(LaunchedInteractiveApplication {
        deployment: InteractiveDeployment {
            _workspace_view: workspace_view,
            supervisor: installation.supervisor_parent,
            notified_provider_failures: Default::default(),
            actor: actor_identity,
            local_actor: actor,
            pane,
            workspace,
            inbox,
            notification_inbox_key: format!(
                "{}:{}:{}",
                runtime_namespace(&run_root),
                actor_identity.id.0,
                actor_identity.incarnation.0
            ),
            connection: InteractiveConnection::AwaitingBinding,
            service,
            socket_directory,
            worktree_custody: installation.worktree_custody.clone(),
            failure_reported: false,
            last_activation_sequence: 0,
            thread: None,
            fork_gate,
            runtime_observation,
            fork_parent_thread,
            build_resource: build_resource
                .map(|resource| Arc::new(tokio::sync::Mutex::new(resource))),
        },
        binding: InteractiveBindingRequest {
            control: binding_control,
            path: binding_path,
            expected: expected_resume,
        },
    }))
}

/// Acquire exclusive path custody before the first fallible preparation step.
fn prepare_socket_inbox(
    actor: ActorRef,
    socket_root: PathBuf,
    rows: PathBuf,
    cursor: PathBuf,
) -> Result<(SocketDirectory, UnixListener, Arc<ActorInbox>), InteractiveApplicationError> {
    let socket = SocketDirectory::create(socket_root)
        .map_err(|error| application_error(actor, InteractiveOperation::PrepareRuntime, error))?;
    let prepared: Result<_, InteractiveApplicationError> = (|| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(socket.path(), std::fs::Permissions::from_mode(0o700))
                .map_err(|error| {
                    application_error(actor, InteractiveOperation::PrepareRuntime, error)
                })?;
        }
        let listener = UnixListener::bind(socket.path().join("host-tools.sock"))
            .map_err(|error| application_error(actor, InteractiveOperation::BindToolHost, error))?;
        let inbox = ActorInbox::open(rows, cursor).map_err(|error| {
            application_error(actor, InteractiveOperation::PrepareRuntime, error)
        })?;
        Ok((listener, Arc::new(inbox)))
    })();
    match prepared {
        Ok((listener, inbox)) => Ok((socket, listener, inbox)),
        Err(error) => Err(socket_launch_failure(
            actor,
            error.operation,
            error.detail,
            socket,
        )),
    }
}

fn socket_cleanup_outcome(socket: SocketDirectory) -> CleanupComponentOutcome {
    match socket.release() {
        Ok(()) => CleanupComponentOutcome::Completed,
        Err(error) => CleanupComponentOutcome::Failed {
            detail: error.to_string(),
        },
    }
}

fn socket_launch_failure(
    actor: ActorRef,
    operation: InteractiveOperation,
    cause: impl fmt::Display,
    socket: SocketDirectory,
) -> InteractiveApplicationError {
    let detail = match socket_cleanup_outcome(socket) {
        CleanupComponentOutcome::Failed { detail } => {
            format!("{cause}; socket cleanup failed: {detail}")
        }
        _ => cause.to_string(),
    };
    application_error(actor, operation, detail)
}

fn accepts_activation(
    deployment: &InteractiveDeployment,
    activation: &tidepool_actor::ResidentActivation,
) -> bool {
    accepts_activation_id(
        deployment.actor,
        deployment.last_activation_sequence,
        activation.id.actor(),
        activation.id.sequence(),
    )
}

fn accepts_activation_id(
    actor: ActorRef,
    last_sequence: u64,
    candidate_actor: ActorRef,
    candidate_sequence: u64,
) -> bool {
    candidate_actor == actor && candidate_sequence > last_sequence
}

/// Toolchain processes selected for the source checkout are not valid in a
/// child worktree. Each worker resolves tools from its own sources instead.
fn workspace_local_toolchain_pins(is_root: bool) -> BTreeSet<String> {
    if is_root {
        return BTreeSet::new();
    }

    [
        "TIDEPOOL_EXTRACT",
        "TIDEPOOL_EXTRACT_WORKER",
        "TIDEPOOL_EXTRACT_DAEMON_SOCKET",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

struct ActorLaunchEnvironment {
    set: BTreeMap<String, String>,
    unset: BTreeSet<String>,
}

/// Compose the host environment and actor-local credentials before handing
/// them to tmux. A worker's explicit unsets win over values captured from the
/// root process; handing the same name to both tmux channels is ambiguous and
/// rejected by the deployment adapter.
fn actor_launch_environment(
    mut inherited: BTreeMap<String, String>,
    is_root: bool,
    build_output: Option<&Path>,
) -> ActorLaunchEnvironment {
    if let Some(build_output) = build_output {
        inherited.insert(
            "CARGO_TARGET_DIR".into(),
            build_output.to_string_lossy().into_owned(),
        );
        // A host compiler-cache daemon resolves the shared visible pathname in
        // its own mount namespace. Empty overrides also disable Cargo config
        // wrappers, keeping compiler execution inside this actor's workspace.
        inherited.insert("RUSTC_WRAPPER".into(), String::new());
        inherited.insert("RUSTC_WORKSPACE_WRAPPER".into(), String::new());
    }
    let unset = workspace_local_toolchain_pins(is_root);
    inherited.retain(|name, _| !unset.contains(name));
    ActorLaunchEnvironment {
        set: inherited,
        unset,
    }
}

fn orient_launch_instructions(
    message: &str,
    observation: &tidepool_actor::ActorRuntimeObservation,
) -> String {
    match observation.launch_orientation() {
        Some(orientation) => format!("{message}\n\n{orientation}"),
        None => message.to_owned(),
    }
}

fn observe_notification_receipt(
    command: &tidepool_actor::NotificationPoll,
    target: ActorRef,
    inbox_key: &str,
    inbox: &ActorInbox,
) -> Result<tidepool_actor::NotificationState, tidepool_actor::NotificationError> {
    use tidepool_actor::{NotificationError, NotificationState};
    use tidepool_node::{DeliveryPhase, ReceiptLookup};
    let receipt = command.receipt();
    if receipt.owner() != command.owner() {
        return Err(NotificationError::Unauthorized);
    }
    if receipt.target() != target || receipt.inbox() != inbox_key {
        return Err(NotificationError::InvalidReceipt);
    }
    match inbox
        .observe_receipt(receipt.sequence())
        .map_err(|error| NotificationError::StorageFailure(error.to_string()))?
    {
        ReceiptLookup::Unavailable => Err(NotificationError::Unavailable),
        ReceiptLookup::Retained(evidence) => {
            if evidence.context
                != (NotificationProvenance {
                    sender: command.owner(),
                    target,
                })
            {
                return Err(NotificationError::Unauthorized);
            }
            Ok(match evidence.phase {
                DeliveryPhase::Accepted => NotificationState::Accepted,
                DeliveryPhase::Presented => NotificationState::Presented,
                DeliveryPhase::InFlight | DeliveryPhase::Submitted | DeliveryPhase::Unconfirmed => {
                    NotificationState::Unconfirmed
                }
            })
        }
    }
}

async fn deliver_pending(
    actor: ActorRef,
    inbox: &Arc<ActorInbox>,
    thread: &QueueReadyThread,
    backend: &dyn InteractiveAgentBackend,
    workspace: &Path,
    runtime_observation: &tidepool_actor::ActorRuntimeObservationHandle,
) -> Result<(), String> {
    let cwd = workspace.to_string_lossy();
    let pending_inbox = Arc::clone(inbox);
    let pending = tokio::task::spawn_blocking(move || pending_inbox.legacy_pending_prefix())
        .await
        .map_err(|error| format!("inbox reader task: {error}"))?
        .map_err(|error| error.to_string())?;
    let Some(last) = pending.last() else {
        return Ok(());
    };
    let inbox_watermark = inbox.watermark();
    let inbox_sequence = last.sequence;
    let activation = pending
        .iter()
        .rev()
        .find_map(|message| match &message.payload {
            DurableActorEvent::Typed(TypedActorEvent::SessionReady {
                sequence, request, ..
            }) => Some((*request, *sequence)),
            _ => None,
        });
    let event_sequences = pending
        .iter()
        .filter(|message| {
            !matches!(
                message.payload,
                DurableActorEvent::Typed(TypedActorEvent::SessionReady { .. })
            )
        })
        .map(|message| message.sequence)
        .collect::<Vec<_>>();
    let rendered = pending
        .iter()
        .map(|message| {
            message
                .payload
                .render(runtime_observation.snapshot().launched_at_unix_ms)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    backend
        .push(&cwd, thread, &rendered)
        .await
        .map_err(|error| error.to_string())?;
    if let Some((request, sequence)) = activation {
        runtime_observation.publish_request_activation(request, sequence);
    } else {
        runtime_observation.publish_event_activation(event_sequences, inbox_watermark);
    }
    let ack_inbox = Arc::clone(inbox);
    tokio::task::spawn_blocking(move || ack_inbox.acknowledge(inbox_sequence))
        .await
        .map_err(|error| format!("inbox acknowledgement task: {error}"))?
        .map_err(|error| error.to_string())?;
    tracing::info!(
        actor = ?actor,
        inbox_sequence,
        inbox_watermark,
        event_count = pending.len(),
        "actor activation batch delivered"
    );
    Ok(())
}

async fn run_delivery_pump(
    actor: ActorRef,
    inbox: Arc<ActorInbox>,
    thread: QueueReadyThread,
    backend: Arc<dyn InteractiveAgentBackend>,
    workspace: PathBuf,
    runtime_observation: tidepool_actor::ActorRuntimeObservationHandle,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut health = tokio::time::interval(Duration::from_secs(1));
    let mut usage_poll = tokio::time::interval(Duration::from_secs(10));
    let mut last_error = None;
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = health.tick() => {
                let result = deliver_pending(
                    actor,
                    &inbox,
                    &thread,
                    backend.as_ref(),
                    &workspace,
                    &runtime_observation,
                ).await;
                match result {
                    Ok(()) => {
                        if last_error.take().is_some() {
                            tracing::info!(actor = ?actor, "actor inbox delivery recovered");
                        }
                    }
                    Err(error) => {
                        if last_error.as_deref() != Some(error.as_str()) {
                            tracing::warn!(actor = ?actor, %error, "actor inbox delivery is pending retry");
                        }
                        last_error = Some(error);
                    }
                }
            }
            _ = usage_poll.tick() => {
                match backend.observe(&thread).await {
                    Ok(Some(observation)) => runtime_observation.publish_provider_observation(observation),
                    Ok(None) => runtime_observation.mark_provider_observation_stale(),
                    Err(error) => {
                        runtime_observation.mark_provider_observation_stale();
                        tracing::debug!(actor = ?actor, %error, "provider observation unavailable");
                    }
                }
            }
        }
    }
}

fn prepare_owner_notification(
    notice: &tidepool_actor::ChildExitNotice,
    deployments: &[InteractiveDeployment],
) -> Option<OwnerNotification> {
    if notice
        .child
        .terminal()
        .retirement_acknowledged_by(notice.owner)
    {
        return None;
    }
    let owner_application = deployments.iter().find(|app| app.actor == notice.owner)?;
    Some(OwnerNotification {
        owner: notice.owner,
        inbox: Arc::clone(&owner_application.inbox),
        event: DurableActorEvent::Typed(TypedActorEvent::ChildExited),
    })
}

async fn publish_owner_notification(
    notification: OwnerNotification,
) -> (ActorRef, Result<(), String>) {
    publish_inbox_event_for(notification.owner, notification.inbox, notification.event).await
}

async fn publish_inbox_event_for(
    actor: ActorRef,
    inbox: Arc<ActorInbox>,
    event: DurableActorEvent,
) -> (ActorRef, Result<(), String>) {
    (actor, publish_inbox_event(inbox, event).await)
}

async fn publish_inbox_event(
    inbox: Arc<ActorInbox>,
    event: DurableActorEvent,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        if let DurableActorEvent::Typed(TypedActorEvent::ProviderTurnFailed {
            actor,
            thread,
            revision,
            ..
        }) = &event
        {
            inbox
                .publish_latest(
                    format!(
                        "provider-failure:{}@{}:{thread}",
                        actor.id.0, actor.incarnation.0
                    ),
                    *revision,
                    event,
                )
                .map(|_| ())
        } else {
            inbox.publish(event).map(|_| ())
        }
    })
    .await
    .map_err(|error| format!("actor inbox publisher task: {error}"))?
    .map_err(|error| error.to_string())?;
    Ok(())
}

async fn retire_interactive_application_guarded(
    deployment: InteractiveDeployment,
    tmux: &TmuxSession,
    native_retirement: NativeRetirement,
) -> InteractiveCleanupReceipt {
    let actor = deployment.actor;
    match AssertUnwindSafe(retire_interactive_application(
        deployment,
        tmux,
        native_retirement,
    ))
    .catch_unwind()
    .await
    {
        Ok(receipt) => receipt,
        Err(_) => InteractiveCleanupReceipt {
            actor,
            components: vec![CleanupComponentReceipt {
                component: CleanupComponent::ToolService,
                outcome: CleanupComponentOutcome::Failed {
                    detail: "cleanup task panicked; component completion is unknown".into(),
                },
            }],
        },
    }
}

async fn retire_interactive_application(
    mut deployment: InteractiveDeployment,
    tmux: &TmuxSession,
    native_retirement: NativeRetirement,
) -> InteractiveCleanupReceipt {
    let actor = deployment.actor;
    let mut components = Vec::with_capacity(6);
    let mut delivery = match deployment.connection {
        InteractiveConnection::AwaitingBinding => None,
        InteractiveConnection::Bound {
            delivery_shutdown,
            delivery,
            ..
        } => {
            let _ = delivery_shutdown.send(());
            Some(delivery)
        }
    };
    components.push(CleanupComponentReceipt {
        component: CleanupComponent::Process,
        outcome: retire_native_pane(tmux, &deployment.pane, native_retirement).await,
    });
    let (service_outcome, delivery_outcome) = tokio::join!(
        stop_retired_tool_service(deployment.actor, &mut deployment.service),
        async {
            if let Some(delivery) = delivery.as_mut() {
                stop_retired_delivery(deployment.actor, delivery, APPLICATION_TASK_GRACE_TIMEOUT)
                    .await
            } else {
                CleanupComponentOutcome::Completed
            }
        },
    );
    components.push(CleanupComponentReceipt {
        component: CleanupComponent::ToolService,
        outcome: service_outcome,
    });
    components.push(CleanupComponentReceipt {
        component: CleanupComponent::Delivery,
        outcome: delivery_outcome,
    });
    components.push(CleanupComponentReceipt {
        component: CleanupComponent::Socket,
        outcome: socket_cleanup_outcome(deployment.socket_directory),
    });
    let build_outcome =
        deployment
            .build_resource
            .take()
            .map_or(CleanupComponentOutcome::Completed, |lease| {
                let released = Arc::try_unwrap(lease)
                    .map_err(|_| {
                        std::io::Error::other(
                            "build publication still owns the resource; cleanup is unconfirmed",
                        )
                    })
                    .and_then(|lease| lease.into_inner().release());
                match released {
                    Ok(()) => CleanupComponentOutcome::Completed,
                    Err(error) => CleanupComponentOutcome::Failed {
                        detail: error.to_string(),
                    },
                }
            });
    components.push(CleanupComponentReceipt {
        component: CleanupComponent::BuildResource,
        outcome: build_outcome,
    });
    let binding_outcome = if deployment.worktree_custody.take().is_some() {
        // Pane removal (including an absent/non-owned pane) is not a process reap.
        CleanupComponentOutcome::Failed {
            detail: "custody retained: tmux cannot prove exact process termination".into(),
        }
    } else {
        CleanupComponentOutcome::Completed
    };
    components.push(CleanupComponentReceipt {
        component: CleanupComponent::WorktreeBinding,
        outcome: binding_outcome,
    });
    InteractiveCleanupReceipt { actor, components }
}

/// Account for exact resident cleanup before draining the original HTTP task.
/// Namespace/native and external-handler domains remain independently unknown.
async fn stop_retired_tool_service(
    _actor: ActorRef,
    service: &mut hosted_retirement::HostedOwner,
) -> CleanupComponentOutcome {
    match hosted_retirement::observe(service,
        hosted_retirement::CompletionBoundary::AbortForShutdown,
        APPLICATION_TASK_GRACE_TIMEOUT).await {
        hosted_retirement::HostedObservation::Observed {
            http: hosted_retirement::HttpObservation::Drained, ..
        } => CleanupComponentOutcome::Completed,
        observation => CleanupComponentOutcome::Failed {
            detail: format!("resident/HTTP cleanup retained: {observation:?}; native/external cleanup is not established"),
        },
    }
}

async fn stop_retired_delivery(
    actor: ActorRef,
    delivery: &mut tokio::task::JoinHandle<()>,
    grace: Duration,
) -> CleanupComponentOutcome {
    match tokio::time::timeout(grace, &mut *delivery).await {
        Ok(Ok(())) => CleanupComponentOutcome::Completed,
        Ok(Err(error)) => {
            tracing::warn!(actor = ?actor, %error, "retired actor inbox task failed");
            CleanupComponentOutcome::Failed {
                detail: error.to_string(),
            }
        }
        Err(_) => {
            tracing::debug!(actor = ?actor, "forcing retired actor inbox task to stop");
            delivery.abort();
            let _ = delivery.await;
            CleanupComponentOutcome::Forced
        }
    }
}

async fn abandon_interactive_application(
    tmux: &TmuxSession,
    pane: &TmuxPaneId,
    service: hosted_retirement::HostedOwner,
    socket_root: &Path,
    native_retirement: NativeRetirement,
) {
    let outcome = retire_native_pane(tmux, pane, native_retirement).await;
    tracing::warn!(path = %socket_root.display(), ?outcome, "abandoned socket directory retained: exact process and accepted hosted work cleanup unconfirmed");
    let _ = hosted_retirement::observe(
        &service,
        hosted_retirement::CompletionBoundary::AbortForShutdown,
        APPLICATION_TASK_GRACE_TIMEOUT,
    )
    .await;
}

async fn retire_native_pane(
    tmux: &TmuxSession,
    pane: &TmuxPaneId,
    disposition: NativeRetirement,
) -> CleanupComponentOutcome {
    let detail = match disposition {
        NativeRetirement::Preserve => {
            "native TUI preserved; hosted coordination unavailable; process custody retained".into()
        }
        NativeRetirement::Terminate => match tmux.kill_pane(pane).await {
            Ok(()) => {
                "pane cleanup requested; exact process termination remains unconfirmed".into()
            }
            Err(error) => error.to_string(),
        },
    };
    CleanupComponentOutcome::Failed { detail }
}

async fn discover_interactive_binding(
    actor: ActorRef,
    request: InteractiveBindingRequest,
    tmux: &TmuxSession,
    pane: &TmuxPaneId,
) -> Result<QueueReadyThread, InteractiveApplicationError> {
    let mut binding_poll = tokio::time::interval(Duration::from_millis(100));
    let mut pane_health = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = binding_poll.tick() => {
                // A retained binding belongs to the previous launch until this
                // exact tool host has accepted the new TUI's session callback.
                if !request.control.session_attached() {
                    continue;
                }
                if let Ok(thread) = read_interactive_binding(&request.path).await {
                    if let Some(expected) = &request.expected {
                        if expected != thread.id() {
                            return Err(application_error(
                                actor,
                                InteractiveOperation::DiscoverBinding,
                                format!(
                                    "resume published thread {} instead of retained thread {}",
                                    thread.id().0, expected.0
                                ),
                            ));
                        }
                    }
                    return Ok(thread);
                }
            }
            _ = pane_health.tick() => {
                let status = tmux.pane_status(pane).await.map_err(|error| {
                    application_error(actor, InteractiveOperation::DiscoverBinding, error)
                })?;
                if status.as_ref().is_none_or(|status| status.dead) {
                    let exit_status = status.and_then(|status| status.exit_status);
                    let output = tmux
                        .capture_pane(pane, 120)
                        .await
                        .unwrap_or_else(|error| format!("<pane output unavailable: {error}>"));
                    let detail = if output.is_empty() {
                        format!(
                            "interactive application exited before conversation binding; exit_status={exit_status:?}"
                        )
                    } else {
                        format!(
                            "interactive application exited before conversation binding; exit_status={exit_status:?}; pane_output={output:?}"
                        )
                    };
                    tracing::error!(?actor, ?exit_status, pane_output = %output, "interactive application exited before binding");
                    return Err(application_error(
                        actor,
                        InteractiveOperation::DiscoverBinding,
                        detail,
                    ));
                }
            }
        }
    }
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<Option<NativeRetirement>>) {
    if shutdown.borrow().is_some() {
        return;
    }
    while shutdown.changed().await.is_ok() {
        if shutdown.borrow().is_some() {
            return;
        }
    }
}

async fn operator_shutdown() -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut terminate = signal(SignalKind::terminate())?;
        let mut hangup = signal(SignalKind::hangup())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
            _ = hangup.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

#[cfg(test)]
fn developer_instructions(
    effective_role: &tidepool_actor::EffectiveRole,
    mode: &InteractiveLaunchMode,
) -> String {
    developer_instructions_selected(effective_role, mode, None, None)
}

fn developer_instructions_selected(
    effective_role: &tidepool_actor::EffectiveRole,
    mode: &InteractiveLaunchMode,
    inputs: Option<&crate::shoal::workspace::FrozenWorkspace>,
    instructions: Option<&str>,
) -> String {
    if let Some(body) = instructions {
        return append_effective_role(body.to_owned(), effective_role);
    }
    let role = effective_role.role();
    let key = match role {
        tidepool_actor::ActorRole::Root => "root",
        tidepool_actor::ActorRole::Research => "research",
        tidepool_actor::ActorRole::Coding | tidepool_actor::ActorRole::Inherited => "coding",
        tidepool_actor::ActorRole::Scaffolding => "scaffolding",
        tidepool_actor::ActorRole::Integration => "integration",
    };
    if let Some(body) = inputs.and_then(|inputs| inputs.prompts.get(key)) {
        let mut body = body.clone();
        if role == tidepool_actor::ActorRole::Root
            && matches!(mode, InteractiveLaunchMode::Resume(_))
        {
            body.push_str(PromptId::RecreatedRoot.body());
        }
        return append_effective_role(body, effective_role);
    }
    if role == tidepool_actor::ActorRole::Root {
        let mut instructions = PromptId::ShoalRoot.body().to_string();
        if matches!(mode, InteractiveLaunchMode::Resume(_)) {
            instructions.push_str(PromptId::RecreatedRoot.body());
        }
        append_effective_role(instructions, effective_role)
    } else {
        let instructions = match role {
            tidepool_actor::ActorRole::Research => PromptId::ReadonlyAgent.body().into(),
            tidepool_actor::ActorRole::Coding | tidepool_actor::ActorRole::Inherited => {
                PromptId::WorktreeAgent.body().into()
            }
            tidepool_actor::ActorRole::Scaffolding => PromptId::ScaffoldingAgent.body().into(),
            tidepool_actor::ActorRole::Integration => PromptId::IntegrationAgent.body().into(),
            tidepool_actor::ActorRole::Root => unreachable!("root handled above"),
        };
        append_effective_role(instructions, effective_role)
    }
}

fn append_effective_role(mut instructions: String, role: &tidepool_actor::EffectiveRole) -> String {
    let descendants = role.descendants();
    instructions.push_str(&format!(
        "\n\nRuntime policy ({}): role={:?}; effects={}; native_tools={:?}; workspace={:?}; descendant_depth={}; active_children={}. These are the effective runtime facts; effect membership alone is not authority.\n",
        role.prompt_profile(),
        role.role(),
        role.haskell_effects_type(),
        role.native_tools(),
        role.workspace(),
        descendants.maximum_depth,
        descendants.maximum_active_children,
    ));
    instructions
}

fn worktree_grant(role: tidepool_actor::ActorRole) -> ActorWorktreeGrant {
    match role {
        tidepool_actor::ActorRole::Root => ActorWorktreeGrant::Repository,
        tidepool_actor::ActorRole::Coding | tidepool_actor::ActorRole::Scaffolding => {
            ActorWorktreeGrant::Bound {
                enumerate: false,
                allocate: true,
                integrate: true,
            }
        }
        tidepool_actor::ActorRole::Integration => ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: false,
            integrate: true,
        },
        tidepool_actor::ActorRole::Research | tidepool_actor::ActorRole::Inherited => {
            ActorWorktreeGrant::default()
        }
    }
}

fn writable_repository_roots(
    root: bool,
    workspace_access: tidepool_actor::WorkspaceAccess,
    source: &Path,
    worker_worktree: Option<&Path>,
    git_common_dir: &Path,
) -> Vec<PathBuf> {
    let mut writable = if root {
        // Integration advances the source HEAD; child coding happens only in
        // the exact linked worktree granted to that child.
        vec![source.to_path_buf()]
    } else if workspace_access == tidepool_actor::WorkspaceAccess::WritableBound {
        worker_worktree.map(Path::to_path_buf).into_iter().collect()
    } else {
        Vec::new()
    };
    if root || workspace_access == tidepool_actor::WorkspaceAccess::WritableBound {
        // Writable linked worktrees intentionally share objects, refs, config,
        // and per-worktree administrative state. Inspection-only actors must
        // observe the same metadata without being able to mutate it.
        writable.push(git_common_dir.to_path_buf());
    }
    writable
}

fn fresh_session_id() -> SessionId {
    let id = uuid::Uuid::new_v4();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&id.as_bytes()[..8]);
    SessionId(u64::from_le_bytes(bytes))
}

fn join_error(error: tokio::task::JoinError) -> Box<dyn std::error::Error> {
    runtime_error(format!("actor host task failed: {error}"))
}

fn runtime_error(message: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::other(message.into()))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn roster_observation_preserves_host_and_sibling_workbenches() {
        let mut campaign = test_campaign::TestCampaign::start().await;
        let root = campaign.root_installation.policy.clone();
        let setup =
            dispatch_haskell_script(root.as_ref(), include_str!("actor_host/roster_setup.hs"))
                .await;
        assert_eq!(setup["status"], "committed", "{setup:?}");
        let children = tokio::time::timeout(Duration::from_secs(30), async {
            let mut children = Vec::new();
            while children.len() < 2 {
                match campaign.deployments.recv().await {
                    Some(LocalResidentDeployment::PolicyInstalled(child)) => children.push(child),
                    Some(LocalResidentDeployment::Retired { actor, terminal }) => {
                        panic!("{actor:?}: {terminal:?}")
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed"),
                }
            }
            children
        })
        .await
        .unwrap();
        for policy in
            std::iter::once(root.as_ref()).chain(children.iter().map(|child| child.policy.as_ref()))
        {
            let observed =
                dispatch_haskell_script(policy, include_str!("actor_host/roster_observe.hs")).await;
            assert_eq!(observed["status"], "committed", "{observed:?}");
            assert!(
                observed["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["output"]
                        .as_str()
                        .is_some_and(|output| output.contains("rosterActorId"))),
                "{observed:?}"
            );
            let next = dispatch_haskell_script(policy, "40 + 2 :: Int").await;
            assert_eq!(next["status"], "committed", "{next:?}");
            assert_eq!(next["items"][0]["output"], "42", "{next:?}");
        }
        let status = dispatch_haskell_script(root.as_ref(), ":status!").await;
        assert_ne!(status["status"], "failed", "{status:?}");
        campaign.forest.shutdown().await;
        campaign.hosted.await.unwrap();
    }

    #[tokio::test]
    async fn forest_operator_survives_model_root_recovery() {
        let campaign = test_campaign::TestCampaign::start().await;
        let operator = campaign
            .forest
            .new_workbench("operator".into(), tidepool_actor::EffectiveRole::root())
            .await
            .unwrap();
        assert_eq!(
            campaign
                .forest
                .inspect_graph(campaign.actor.identity())
                .unwrap()
                .len(),
            1,
            "ordinary roots cannot inspect other trees"
        );
        assert_eq!(
            campaign
                .forest
                .inspect_graph(operator.identity())
                .unwrap()
                .len(),
            2
        );
        async fn submit(
            actor: &LocalActorRef,
            source: &str,
        ) -> tidepool_runtime::session::WorkbenchResponse {
            let (reply, receive) = tokio::sync::oneshot::channel();
            actor
                .address()
                .send_message(tidepool_actor::KernelMessage::Workbench {
                    request: tidepool_runtime::session::WorkbenchRequest::from_ghci_input(source)
                        .unwrap(),
                    reply: reply.into(),
                })
                .unwrap();
            receive.await.unwrap().unwrap()
        }
        let bound = submit(&operator, "let retainedOperatorValue = 123").await;
        assert_eq!(
            bound.status,
            tidepool_runtime::session::WorkbenchRunStatus::Committed
        );
        let requested = submit(&operator, include_str!("actor_host/operator_request.hs")).await;
        assert_eq!(
            requested.status,
            tidepool_runtime::session::WorkbenchRunStatus::Committed,
            "{requested:?}"
        );
        let mut deployments = campaign.deployments;
        let child = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(LocalResidentDeployment::PolicyInstalled(child)) =
                    deployments.recv().await
                {
                    break child;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(child.supervisor_parent, Some(operator.identity()));
        assert_eq!(child.context_parent, None);
        assert_eq!(
            campaign
                .forest
                .inspect_graph(child.actor.identity())
                .unwrap()
                .len(),
            1,
            "operator forest grant must not propagate to descendants"
        );
        let replied =
            dispatch_haskell_script(child.policy.as_ref(), "respond (sessionInput + 1 :: Int)")
                .await;
        assert_eq!(replied["status"], "replied", "{replied:?}");
        let response = submit(&operator, "inspectFull <$> pollResponse answer").await;
        assert!(
            response.items.iter().any(|item| item.output.contains("42")),
            "{response:?}"
        );
        campaign
            .actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Failed,
                summary: "recovery test".into(),
            })
            .await
            .unwrap();
        campaign.hosted.await.unwrap();
        let (replacement, task) = campaign
            .forest
            .new_program_root(
                "replacement".into(),
                tidepool_actor::EffectiveRole::root(),
                campaign.program.clone(),
            )
            .await
            .unwrap();
        assert_ne!(replacement.identity(), campaign.actor.identity());
        assert_eq!(
            submit(&operator, "retainedOperatorValue").await.items[0].output,
            "123"
        );
        assert_eq!(
            campaign
                .forest
                .inspect_graph(replacement.identity())
                .unwrap()
                .len(),
            1
        );
        assert!(campaign
            .forest
            .inspect_graph(operator.identity())
            .unwrap()
            .iter()
            .any(|node| node.actor == replacement.identity()));
        campaign.forest.shutdown().await;
        task.await.unwrap();
        assert!(operator.terminal().get().is_some());
    }

    #[tokio::test]
    async fn progress_retains_closures_and_watch_snapshots_across_calls() {
        let mut campaign = test_campaign::TestCampaign::start().await;
        let root = campaign.root_installation.policy.clone();
        let setup =
            dispatch_haskell_script(root.as_ref(), include_str!("actor_host/progress_setup.hs"))
                .await;
        assert_eq!(setup["status"], "committed", "{setup:?}");
        let child = tokio::time::timeout(Duration::from_secs(30), async {
            let mut child = None;
            loop {
                match campaign.deployments.recv().await {
                    Some(LocalResidentDeployment::PolicyInstalled(installation)) => {
                        child = Some(installation)
                    }
                    Some(LocalResidentDeployment::SessionReady { .. }) => {
                        return child.expect("child policy installed before request")
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed"),
                }
            }
        })
        .await
        .expect("progress request activation");
        let first = dispatch_haskell_script(
            child.policy.as_ref(),
            "reportProgress (ProgressNote 1 (+ sessionInput))",
        )
        .await;
        assert_eq!(first["status"], "committed", "{first:?}");
        campaign.await_watch_ready().await;
        let second = dispatch_haskell_script(
            child.policy.as_ref(),
            "reportProgress (ProgressNote 2 (* sessionInput))",
        )
        .await;
        assert_eq!(second["status"], "committed", "{second:?}");
        let captured = dispatch_haskell_script(
            root.as_ref(),
            include_str!("actor_host/progress_observe.hs"),
        )
        .await;
        assert_eq!(captured["status"], "committed", "{captured:?}");
        assert!(captured.to_string().contains("(13,30)"), "{captured:?}");
        assert_eq!(captured["items"][7]["output"], "40", "{captured:?}");
        let reply = dispatch_haskell_script(child.policy.as_ref(), "respond (42 :: Int)").await;
        assert_eq!(reply["status"], "replied", "{reply:?}");
        let stopped = dispatch_haskell_script(root.as_ref(), "stopAgent worker").await;
        assert_eq!(stopped["status"], "committed", "{stopped:?}");
        let retained = dispatch_haskell_script(
            root.as_ref(),
            include_str!("actor_host/progress_retained.hs"),
        )
        .await;
        assert_eq!(retained["status"], "committed", "{retained:?}");
        assert_eq!(retained["items"][1]["output"], "True", "{retained:?}");
        assert_eq!(retained["items"][2]["output"], "15", "{retained:?}");
        assert_eq!(retained["items"][4]["output"], "50", "{retained:?}");
        assert_eq!(retained["items"][6]["output"], "16", "{retained:?}");
        campaign
            .actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "progress test complete".into(),
            })
            .await
            .unwrap();
        campaign.hosted.await.unwrap();
    }

    #[tokio::test]
    async fn active_update_keeps_original_request_and_fences_terminal_delivery() {
        let mut campaign = test_campaign::TestCampaign::start().await;
        let root = campaign.root_installation.policy.clone();
        let setup = dispatch_haskell_script(
            root.as_ref(),
            include_str!("actor_host/active_update_setup.hs"),
        )
        .await;
        assert_eq!(setup["status"], "committed", "{setup:?}");
        let child = tokio::time::timeout(Duration::from_secs(30), async {
            let mut child = None;
            loop {
                match campaign.deployments.recv().await {
                    Some(LocalResidentDeployment::PolicyInstalled(installation)) => {
                        child = Some(installation)
                    }
                    Some(LocalResidentDeployment::SessionReady { .. }) => return child.unwrap(),
                    Some(_) => {}
                    None => panic!("deployment channel closed"),
                }
            }
        })
        .await
        .unwrap();
        let failed = dispatch_haskell_script(
            root.as_ref(),
            "Right failedClarification <- updateRequest answer \"Private baseline clarification\"",
        )
        .await;
        assert_eq!(failed["status"], "committed", "{failed:?}");
        let failed_delivery = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match campaign.deployments.recv().await {
                    Some(LocalResidentDeployment::RequestUpdate { delivery }) => return delivery,
                    Some(LocalResidentDeployment::SessionReady { .. }) => {
                        panic!("update queued another assignment")
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed"),
                }
            }
        })
        .await
        .unwrap();
        let presentation = failed_delivery.begin().unwrap();
        let key = presentation.key().to_owned();
        let log = tempfile::NamedTempFile::new().unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(log.reopen().unwrap()))
            .finish();
        let error = tidepool_agent::UpdatePresentationError::NotSubmitted(
            "connecting update proxy: controlled transport failure".into(),
        );
        tracing::subscriber::with_default(subscriber, || {
            presentation.not_presented(error.to_string())
        });
        let logged = std::fs::read_to_string(log.path()).unwrap();
        for expected in [
            "request update not presented",
            "actor=ActorRef",
            "request=RequestId",
            "update=1",
            &key,
            "connecting update proxy",
        ] {
            assert!(logged.contains(expected), "missing {expected}: {logged}");
        }
        assert!(!logged.contains("Private baseline clarification"));
        let failed_state =
            dispatch_haskell_script(root.as_ref(), "pollRequestUpdate failedClarification").await;
        assert!(
            failed_state.to_string().contains("UpdateNotPresented"),
            "{failed_state:?}"
        );
        assert!(
            !failed_state.to_string().contains("agent run failed"),
            "{failed_state:?}"
        );
        let pending = dispatch_haskell_script(root.as_ref(), "pollResponse answer").await;
        assert!(
            pending.to_string().contains("ResponsePending"),
            "{pending:?}"
        );
        let sent = dispatch_haskell_script(
            root.as_ref(),
            "Right clarification <- updateRequest answer \"Tabs must be clickable\"",
        )
        .await;
        assert_eq!(sent["status"], "committed", "{sent:?}");
        let queued =
            dispatch_haskell_script(root.as_ref(), "pollRequestUpdate clarification").await;
        assert!(
            queued.to_string().contains("Right UpdateQueued"),
            "{queued:?}"
        );
        let delivery = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match campaign.deployments.recv().await {
                    Some(LocalResidentDeployment::RequestUpdate { delivery }) => return delivery,
                    Some(LocalResidentDeployment::SessionReady { .. }) => {
                        panic!("update queued another assignment")
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed"),
                }
            }
        })
        .await
        .unwrap();
        let presentation = delivery.begin().unwrap();
        assert!(presentation.message().contains("Tabs must be clickable"));
        let rejected = dispatch_haskell_script(
            child.policy.as_ref(),
            "attemptReply sessionReply (sessionInput + 32)",
        )
        .await;
        assert!(
            rejected.to_string().contains("ReplyUpdatePending"),
            "{rejected:?}"
        );
        let pending = dispatch_haskell_script(root.as_ref(), "pollResponse answer").await;
        assert!(
            pending.to_string().contains("ResponsePending"),
            "{pending:?}"
        );
        // The backend seam owns the proof of input insertion. This test drives
        // that boundary explicitly, without sending input to a live model.
        presentation.presented();
        let observed =
            dispatch_haskell_script(root.as_ref(), "pollRequestUpdate clarification").await;
        assert!(
            observed.to_string().contains("Right UpdatePresented"),
            "{observed:?}"
        );
        let reply =
            dispatch_haskell_script(child.policy.as_ref(), "respond (sessionInput + 32)").await;
        assert_eq!(reply["status"], "replied", "{reply:?}");
        let ready = dispatch_haskell_script(root.as_ref(), "pollResponse answer >>= \\s -> pure (case s of { ResponseReady result -> responseValue result == 42; _ -> False })").await;
        assert_eq!(ready["items"][0]["output"], "True", "{ready:?}");
        let late = dispatch_haskell_script(
            root.as_ref(),
            "Right late <- updateRequest answer \"too late\"\npollRequestUpdate late",
        )
        .await;
        assert!(late.to_string().contains("Right UpdateTooLate"), "{late:?}");
        campaign
            .actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "active update test complete".into(),
            })
            .await
            .unwrap();
        campaign.hosted.await.unwrap();
    }

    #[test]
    fn provider_failure_notice_preserves_identity_and_old_inbox_payloads() {
        let notice = super::DurableActorEvent::Typed(super::TypedActorEvent::ProviderTurnFailed {
            revision: 10,
            actor: tidepool_actor::ActorRef {
                id: tidepool_actor::ActorId(7),
                incarnation: tidepool_actor::Incarnation(3),
            },
            thread: "provider-thread".into(),
            turn: "provider-turn".into(),
            failure: tidepool_agent::ProviderFailure::Other("unknown provider code".into()),
        });
        let encoded = serde_json::to_string(&notice).unwrap();
        assert_eq!(
            serde_json::from_str::<super::DurableActorEvent>(&encoded).unwrap(),
            notice
        );
        assert!(notice.render(None).contains("7@3"));
        let legacy = serde_json::from_str::<super::DurableActorEvent>("\"old event\"").unwrap();
        assert_eq!(legacy.render(None), "old event");
    }

    #[tokio::test]
    async fn provider_failure_publication_deduplicates_across_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let rows = directory.path().join("rows");
        let cursor = directory.path().join("cursor");
        let inbox = Arc::new(ActorInbox::open(rows.clone(), cursor.clone()).unwrap());
        let notice = |turn: &str, revision| {
            DurableActorEvent::Typed(TypedActorEvent::ProviderTurnFailed {
                actor: tidepool_actor::ActorRef::first(tidepool_actor::ActorId(7)),
                thread: "thread".into(),
                turn: turn.into(),
                revision,
                failure: tidepool_agent::ProviderFailure::RequestRejected,
            })
        };
        publish_inbox_event(inbox.clone(), notice("first", 10))
            .await
            .unwrap();
        publish_inbox_event(inbox.clone(), notice("first", 10))
            .await
            .unwrap();
        assert_eq!(inbox.pending().unwrap().len(), 1);
        inbox.acknowledge(1).unwrap();
        drop(inbox);
        let inbox = Arc::new(ActorInbox::open(rows, cursor).unwrap());
        publish_inbox_event(inbox.clone(), notice("first", 10))
            .await
            .unwrap();
        assert!(inbox.pending().unwrap().is_empty());
        publish_inbox_event(inbox.clone(), notice("second", 20))
            .await
            .unwrap();
        assert_eq!(inbox.pending().unwrap().len(), 1);
    }

    #[test]
    fn fork_effort_defaults_low_and_preserves_explicit_overrides() {
        use tidepool_actor::ForkEffort;
        use tidepool_agent::{BackendThreadId, InteractiveLaunchMode, ReasoningEffort};
        let fork = InteractiveLaunchMode::Fork {
            parent: BackendThreadId("parent".into()),
            after_call: "call".into(),
        };
        for default in [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ] {
            assert_eq!(
                super::launch_effort(&fork, default, None),
                ReasoningEffort::Low
            );
            for (requested, selected) in [
                (ForkEffort::Low, ReasoningEffort::Low),
                (ForkEffort::Medium, ReasoningEffort::Medium),
                (ForkEffort::High, ReasoningEffort::High),
            ] {
                assert_eq!(
                    super::launch_effort(&fork, default, Some(requested)),
                    selected
                );
            }
            assert_eq!(
                super::launch_effort(&InteractiveLaunchMode::Fresh, default, None),
                default
            );
            assert_eq!(
                super::launch_effort(
                    &InteractiveLaunchMode::Resume(BackendThreadId("retained".into())),
                    default,
                    None
                ),
                default
            );
        }
    }

    use super::*;
    use tidepool_agent::{
        AgentBackendError, InteractiveAgentCommand, InteractiveAgentSpec, InteractiveFuture,
    };
    use tidepool_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};
    use tidepool_worktree::WorktreeSpec;

    fn normalized_prompt(prompt: &str) -> String {
        prompt.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn activation_delivery_refuses_duplicates_and_stale_sequences() {
        let actor = ActorRef::first(tidepool_actor::ActorId(7));
        let other_actor = ActorRef::first(tidepool_actor::ActorId(8));
        assert!(accepts_activation_id(actor, 0, actor, 1));
        assert!(!accepts_activation_id(actor, 1, actor, 1));
        assert!(accepts_activation_id(actor, 1, actor, 3));
        assert!(!accepts_activation_id(actor, 3, actor, 2));
        assert!(!accepts_activation_id(actor, 3, other_actor, 4));
    }

    async fn dispatch_haskell(
        endpoint: &dyn tidepool_actor::ResidentToolEndpoint,
        items: impl IntoIterator<Item = &'static str>,
    ) -> serde_json::Value {
        let mut last = None;
        for item in items {
            let result = dispatch_haskell_script(endpoint, item).await;
            assert_ne!(
                result["status"], "rejected",
                "Haskell item rejected:\n{item}\n\n{result:?}\n\nprevious receipt: {last:?}"
            );
            last = Some(result);
        }
        last.expect("non-empty Haskell fixture")
    }

    pub(super) async fn dispatch_haskell_script(
        endpoint: &dyn tidepool_actor::ResidentToolEndpoint,
        script: &str,
    ) -> serde_json::Value {
        let call_id = uuid::Uuid::new_v4().simple().to_string();
        let result = endpoint
            .dispatch_boxed(ToolInvocation {
                context: Some(ToolInvocationContext {
                    context_call_id: Some(call_id.clone()),
                    thread_id: "actor-host-vertical".into(),
                    turn_id: call_id.clone(),
                    call_id: call_id.clone(),
                    namespace: Some("haskell".into()),
                }),
                name: tidepool_actor::HASKELL_TOOL.into(),
                arguments: ToolArguments::Raw(script.into()),
            })
            .await
            .unwrap_or_else(|error| panic!("Haskell script failed:\n{script}\n\n{error}"));
        endpoint
            .complete_boxed(tidepool_runtime::session::WorkbenchForkBoundary {
                thread_id: "actor-host-vertical".into(),
                call_id,
            })
            .await
            .expect("recorded tool completion");
        result
    }

    #[tokio::test]
    async fn idle_application_waits_for_current_host_attachment_despite_retained_binding() {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let session = TmuxSession::with_socket(
            format!("shoal_binding_{}", &suffix[..8]),
            format!("shoal-binding-{}", &suffix[..8]),
        )
        .unwrap();
        let pane = session
            .create(&TmuxLaunch {
                window_name: "Root".into(),
                cwd: std::env::temp_dir(),
                program: "sleep".into(),
                args: vec!["60".into()],
                environment: std::collections::BTreeMap::new(),
                unset_environment: BTreeSet::new(),
            })
            .await
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("binding.json");
        let thread = BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into());
        tidepool_agent::accept_interactive_session_binding(
            &path,
            tidepool_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
            thread.clone(),
            None,
        )
        .await
        .unwrap();
        let host = crate::host_dynamic_tools::HostDynamicToolService::new(
            crate::host_dynamic_tools::test_endpoint(),
            path.clone(),
            Some(thread.clone()),
        )
        .unwrap();
        let control = host.control();
        let socket = root.path().join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(host.serve(listener));
        let actor = ActorRef::first(tidepool_actor::ActorId(1));
        let binding = discover_interactive_binding(
            actor,
            InteractiveBindingRequest {
                control,
                path: path.clone(),
                expected: None,
            },
            &session,
            &pane,
        );
        tokio::pin!(binding);

        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut binding)
                .await
                .is_err()
        );
        let client = reqwest::Client::builder()
            .unix_socket(socket)
            .no_proxy()
            .build()
            .unwrap();
        let response = client
            .post("http://localhost/v1/dynamic-tools/session")
            .json(&serde_json::json!({"protocolVersion": 3, "threadId":thread.0}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), &mut binding)
                .await
                .unwrap()
                .unwrap()
                .id(),
            &thread
        );
        session.kill().await.unwrap();
        server.abort();
    }

    #[test]
    fn root_instructions_preserve_idle_and_resume_contracts() {
        let role = tidepool_actor::EffectiveRole::root();
        let fresh = developer_instructions(&role, &InteractiveLaunchMode::Fresh);
        let resumed = developer_instructions(
            &role,
            &InteractiveLaunchMode::Resume(BackendThreadId("retained-thread".into())),
        );
        assert!(fresh.starts_with(PromptId::ShoalRoot.body()));
        assert!(!fresh.contains(PromptId::ShoalBase.body()));
        assert!(!resumed.contains(PromptId::ShoalBase.body()));
        assert!(!fresh.contains(PromptId::RecreatedRoot.body()));
        assert!(resumed.starts_with(PromptId::ShoalRoot.body()));
        assert_eq!(resumed.matches(PromptId::RecreatedRoot.body()).count(), 1);
        assert!(normalized_prompt(&resumed).contains("Previous actor handles"));
        let root_effects = role.haskell_effects_type();
        for projection in [
            role.prompt_profile(),
            root_effects.as_str(),
            "native_tools=Coding",
            "workspace=WritableBound",
        ] {
            assert!(fresh.contains(projection), "missing {projection}: {fresh}");
        }
    }

    #[tokio::test]
    async fn abnormal_root_reuses_only_a_queue_ready_conversation() {
        let root = tempfile::tempdir().unwrap();
        let binding = root.path().join("root-binding.json");
        let thread = BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into());
        tidepool_agent::accept_interactive_session_binding(
            &binding,
            tidepool_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
            thread.clone(),
            None,
        )
        .await
        .unwrap();

        let completed = ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "done".into(),
        };
        assert!(root_recovery_launch_mode(&binding, &completed)
            .await
            .unwrap()
            .is_none());
        assert!(root_recovery_launch_mode(
            &binding,
            &ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "operator cancelled".into(),
            }
        )
        .await
        .unwrap()
        .is_none());

        let failed = ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "stale reply target".into(),
        };
        let (mode, retained) = root_recovery_launch_mode(&binding, &failed)
            .await
            .unwrap()
            .expect("failed roots are recreated");
        assert_eq!(mode, InteractiveLaunchMode::Resume(thread.clone()));
        assert_eq!(retained.id(), &thread);

        let missing = root.path().join("missing-binding.json");
        assert!(root_recovery_launch_mode(&missing, &failed)
            .await
            .unwrap_err()
            .to_string()
            .contains("cannot be resumed"));
    }

    #[test]
    fn child_lifecycle_notice_contains_no_copied_runtime_state() {
        assert_eq!(
            CHILD_LIFECYCLE_NOTICE,
            "A child actor changed lifecycle state."
        );
        for forbidden in ["ActorRef", "ActorId", "incarnation", "handle", "summary"] {
            assert!(!CHILD_LIFECYCLE_NOTICE.contains(forbidden));
        }
    }

    #[test]
    fn actor_workspace_recipes_distinguish_orchestrators_from_coding_workers() {
        let none = Vec::new();
        let one = vec!["worker-one".to_string()];
        let two = vec!["worker-one".to_string(), "worker-two".to_string()];

        assert_eq!(
            actor_workspace_request(true, &none),
            Ok(ActorWorkspaceRequest::SourceCheckout)
        );
        assert_eq!(
            actor_workspace_request(false, &none),
            Ok(ActorWorkspaceRequest::SourceCheckout)
        );
        assert_eq!(
            actor_workspace_request(false, &one),
            Ok(ActorWorkspaceRequest::Worktree("worker-one"))
        );
        assert!(actor_workspace_request(true, &one).is_err());
        assert!(actor_workspace_request(false, &two).is_err());

        let research = tidepool_actor::EffectiveRole::research();
        let instructions = developer_instructions(&research, &InteractiveLaunchMode::Fresh);
        assert!(instructions.starts_with(PromptId::ReadonlyAgent.body()));
        let normalized = normalized_prompt(&instructions);
        assert!(normalized.contains("Do not run builds, tests, formatters"));
        assert!(instructions.contains("native_tools=InspectionOnly"));
        assert!(instructions.contains(&research.haskell_effects_type()));

        let worker = developer_instructions(
            &tidepool_actor::EffectiveRole::coding(),
            &InteractiveLaunchMode::Fresh,
        );
        assert!(worker.starts_with(PromptId::WorktreeAgent.body()));
        let scaffold = developer_instructions(
            &tidepool_actor::EffectiveRole::scaffolding(tidepool_actor::DescendantBudget {
                maximum_depth: 2,
                maximum_active_children: 3,
            }),
            &InteractiveLaunchMode::Fork {
                parent: BackendThreadId("parent".into()),
                after_call: "call".into(),
            },
        );
        assert!(scaffold.starts_with(PromptId::ScaffoldingAgent.body()));
        assert!(scaffold.contains("descendant_depth=2; active_children=3"));
        let integration = developer_instructions(
            &tidepool_actor::EffectiveRole::integration(),
            &InteractiveLaunchMode::Fresh,
        );
        assert!(integration.starts_with(PromptId::IntegrationAgent.body()));
    }

    #[test]
    fn root_and_worker_share_git_metadata_but_not_working_tree_authority() {
        let source = Path::new("/source");
        let worker = Path::new("/workers/one");
        let common = Path::new("/source/.git");

        assert_eq!(
            writable_repository_roots(
                true,
                tidepool_actor::WorkspaceAccess::WritableBound,
                source,
                None,
                common,
            ),
            vec![source.to_path_buf(), common.to_path_buf()]
        );
        assert_eq!(
            writable_repository_roots(
                false,
                tidepool_actor::WorkspaceAccess::WritableBound,
                source,
                Some(worker),
                common,
            ),
            vec![worker.to_path_buf(), common.to_path_buf()]
        );
        assert_eq!(
            writable_repository_roots(
                false,
                tidepool_actor::WorkspaceAccess::InspectOnly,
                source,
                Some(worker),
                common,
            ),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn worker_launch_unsets_source_checkout_extractor_pins() {
        let launch = actor_launch_environment(
            BTreeMap::from([
                ("PATH".into(), "/bin".into()),
                ("RUSTC_WRAPPER".into(), "/host/sccache".into()),
                ("RUSTC_WORKSPACE_WRAPPER".into(), "/host/wrapper".into()),
                ("TIDEPOOL_EXTRACT".into(), "/source/tidepool-extract".into()),
                (
                    "TIDEPOOL_EXTRACT_WORKER".into(),
                    "/source/tidepool-extract-worker".into(),
                ),
            ]),
            false,
            Some(Path::new(
                "/tmp/tidepool-actor-workspace/.shoal/build/cargo",
            )),
        );
        assert_eq!(
            launch.unset,
            [
                "TIDEPOOL_EXTRACT".to_string(),
                "TIDEPOOL_EXTRACT_DAEMON_SOCKET".to_string(),
                "TIDEPOOL_EXTRACT_WORKER".to_string(),
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(
            launch.set.get("CARGO_TARGET_DIR").map(String::as_str),
            Some("/tmp/tidepool-actor-workspace/.shoal/build/cargo")
        );
        assert_eq!(launch.set.get("PATH").map(String::as_str), Some("/bin"));
        assert_eq!(
            launch.set.get("RUSTC_WRAPPER").map(String::as_str),
            Some("")
        );
        assert_eq!(
            launch
                .set
                .get("RUSTC_WORKSPACE_WRAPPER")
                .map(String::as_str),
            Some("")
        );
        assert!(launch
            .unset
            .iter()
            .all(|name| !launch.set.contains_key(name)));
    }

    #[test]
    fn root_launch_retains_its_source_checkout_toolchain() {
        let launch = actor_launch_environment(
            BTreeMap::from([("TIDEPOOL_EXTRACT".into(), "/source/extract".into())]),
            true,
            None,
        );
        assert!(launch.unset.is_empty());
        assert_eq!(
            launch.set.get("TIDEPOOL_EXTRACT").map(String::as_str),
            Some("/source/extract")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn actor_build_environment_overrides_cargo_config_wrappers_inside_mount() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let visible = root.path().join("visible");
        let resource = root.path().join("build-resource");
        let relative_target = Path::new(".shoal/build/cargo");
        for path in [
            workspace.join("src"),
            workspace.join(".cargo"),
            workspace.join(relative_target),
            visible.join(relative_target),
            resource.clone(),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[package]\nname = \"actor-mount-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            workspace.join("src/lib.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .unwrap();
        std::fs::write(workspace.join(".cargo/config.toml"),
            "[build]\nrustc-wrapper = \"/host-only/compiler-wrapper\"\nrustc-workspace-wrapper = \"/host-only/workspace-wrapper\"\n").unwrap();
        let boundary =
            ProcessMountBoundary::new(&workspace, [workspace.clone()], [workspace.clone()])
                .unwrap()
                .with_project_root(&visible)
                .unwrap()
                .with_writable_overlay(&resource, visible.join(relative_target))
                .unwrap();
        let invocation = boundary.wrap(
            "bwrap",
            ProcessInvocation {
                program: "cargo".into(),
                args: vec!["check".into(), "--offline".into(), "--quiet".into()],
            },
        );
        let environment = actor_launch_environment(BTreeMap::new(), false, Some(relative_target));
        let mut command = std::process::Command::new(invocation.program);
        command.args(invocation.args).envs(environment.set);
        for name in environment.unset {
            command.env_remove(name);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(resource.join("debug/deps").is_dir());
        assert!(!workspace.join(relative_target).join("debug").exists());
    }

    struct ScriptedPush {
        fail: std::sync::atomic::AtomicBool,
        messages: std::sync::Mutex<Vec<String>>,
    }

    #[test]
    fn durable_actor_events_are_typed_and_legacy_rows_remain_readable() {
        let request = DurableActorEvent::Typed(TypedActorEvent::SessionReady {
            sequence: 4,
            request: tidepool_actor::RequestId(7),
            input_type: "Candidate".into(),
            message: "Review it.".into(),
        });
        let encoded = serde_json::to_value(&request).expect("serialize typed request event");
        assert_eq!(encoded["type"], "sessionReady");
        assert_eq!(encoded["request"], 7);
        assert_eq!(request.render(None), "Review it.");

        let watch = DurableActorEvent::Typed(TypedActorEvent::WatchChanged {
            notification: tidepool_actor::WatchNotification {
                owner: tidepool_actor::ActorRef {
                    id: tidepool_actor::ActorId(1),
                    incarnation: tidepool_actor::Incarnation(1),
                },
                watch: tidepool_actor::WatchId(9),
                label: "join".into(),
                previous: tidepool_actor::WatchStateProjection::Pending,
                current: tidepool_actor::WatchStateProjection::Ready,
                transition: tidepool_actor::WatchTransition::Ready,
                occurred_at_unix_ms: 754_000,
                sequence: tidepool_actor::ActorEventSequence(3),
                watermark: tidepool_actor::ActorEventSequence(3),
            },
        });
        assert!(watch.render(Some(0)).contains("+12m34s since actor launch"));
        assert!(watch.render(None).contains("elapsed time unavailable"));
        assert!(watch
            .render(Some(800_000))
            .contains("elapsed time unavailable"));
        let encoded = serde_json::to_value(&watch).expect("serialize typed watch event");
        assert_eq!(encoded["type"], "watchChanged");
        assert_eq!(encoded["watch"], 9);
        assert_eq!(
            serde_json::from_value::<DurableActorEvent>(serde_json::json!("old notice"))
                .expect("decode legacy actor event"),
            DurableActorEvent::Text("old notice".into())
        );
    }

    impl InteractiveAgentBackend for ScriptedPush {
        fn prepare_native_tool_policy(
            &self,
            _policy: InteractiveNativeToolPolicy,
            _staging_root: &Path,
        ) -> Result<Vec<InteractivePolicyMount>, AgentBackendError> {
            Ok(Vec::new())
        }

        fn render(
            &self,
            _spec: &InteractiveAgentSpec,
        ) -> Result<InteractiveAgentCommand, AgentBackendError> {
            Err(AgentBackendError::ProtocolRejected {
                detail: "render is outside this delivery test".into(),
            })
        }

        fn push<'a>(
            &'a self,
            _cwd: &'a str,
            _thread: &'a QueueReadyThread,
            message: &'a str,
        ) -> InteractiveFuture<'a, ()> {
            Box::pin(async move {
                self.messages.lock().unwrap().push(message.into());
                if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                    Err(AgentBackendError::BackendUnavailable {
                        detail: "temporary push failure".into(),
                    })
                } else {
                    Ok(())
                }
            })
        }

        fn archive<'a>(
            &'a self,
            _cwd: &'a str,
            _thread: &'a QueueReadyThread,
        ) -> InteractiveFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn launch_shutdown_preserves_socket_error_and_completed_results() {
        let root = tempfile::tempdir().unwrap();
        let actor = ActorRef::first(tidepool_actor::ActorId(80));
        let successful_path = root.path().join("successful");
        let failed_path = root.path().join("failed");
        let mut successful = SocketDirectory::create(successful_path.clone()).unwrap();
        successful.work_may_exist();
        let mut failed = SocketDirectory::create(failed_path.clone()).unwrap();
        failed.work_may_exist();
        let error = socket_launch_failure(
            actor,
            InteractiveOperation::LaunchProcess,
            "launch cancelled after hosted work submission",
            failed,
        );
        let mut launches = JoinSet::new();
        launches.spawn(async move { ((), Ok(Some(successful))) });
        launches.spawn(async move { ((), Err(error)) });
        launches.spawn(async { ((), Ok(None)) });
        let outcome = drain_launches_for_shutdown(&mut launches, Duration::from_secs(1)).await;
        assert_eq!(outcome.completed.len(), 1);
        assert_eq!(outcome.completed[0].path(), successful_path);
        assert!(
            matches!(outcome.failures.as_slice(), [LaunchShutdownFailure::Launch(error)]
            if error.actor == actor && error.detail.contains("socket cleanup failed:") && error.detail.contains("unconfirmed"))
        );
        assert!(failed_path.exists());
        assert!(launches.is_empty());
        // This is the same value the production caller transfers into retirement.
        assert!(matches!(
            socket_cleanup_outcome(outcome.completed.into_iter().next().unwrap()),
            CleanupComponentOutcome::Failed { .. }
        ));
        assert!(successful_path.exists());
    }

    #[tokio::test]
    async fn launch_shutdown_retains_join_failure_without_a_deployment() {
        let mut launches: JoinSet<((), Result<Option<()>, InteractiveApplicationError>)> =
            JoinSet::new();
        launches.spawn(async { panic!("launch task panicked before result") });
        let outcome = drain_launches_for_shutdown(&mut launches, Duration::from_secs(1)).await;
        assert!(outcome.completed.is_empty());
        assert!(
            matches!(outcome.failures.as_slice(), [LaunchShutdownFailure::Join(error)] if error.is_panic())
        );
    }

    #[tokio::test]
    async fn launch_shutdown_timeout_preserves_partial_success_and_reports_uncertain_abort() {
        let root = tempfile::tempdir().unwrap();
        let ready_path = root.path().join("ready");
        let pending_path = root.path().join("pending");
        let mut ready = SocketDirectory::create(ready_path.clone()).unwrap();
        ready.work_may_exist();
        let mut pending = SocketDirectory::create(pending_path.clone()).unwrap();
        pending.work_may_exist();
        let mut launches = JoinSet::new();
        launches.spawn(async move { ((), Ok(Some(ready))) });
        launches.spawn(async move {
            let _retained = pending;
            std::future::pending::<(
                (),
                Result<Option<SocketDirectory>, InteractiveApplicationError>,
            )>()
            .await
        });
        let outcome = drain_launches_for_shutdown(&mut launches, Duration::from_millis(100)).await;
        assert_eq!(outcome.completed.len(), 1);
        assert_eq!(outcome.completed[0].path(), ready_path);
        assert!(outcome
            .failures
            .iter()
            .any(|failure| matches!(failure, LaunchShutdownFailure::TimedOut { pending: 1 })));
        // Confirm the test task stops, without upgrading the recorded uncertainty.
        while !launches.is_empty() {
            let result = tokio::time::timeout(Duration::from_secs(1), launches.join_next())
                .await
                .unwrap()
                .unwrap();
            assert!(result.unwrap_err().is_cancelled());
        }
        assert!(pending_path.exists());
        assert!(ready_path.exists());
        assert!(!outcome.failures.is_empty());
    }

    #[tokio::test]
    async fn socket_preparation_cleans_failed_inbox_open_and_preserves_collision() {
        let root = tempfile::tempdir().unwrap();
        let actor = ActorRef::first(tidepool_actor::ActorId(77));
        let socket_path = root.path().join("socket");
        let rows = root.path().join("rows");
        let cursor = root.path().join("cursor");
        std::fs::write(&cursor, b"not a checkpoint").unwrap();
        let error = prepare_socket_inbox(actor, socket_path.clone(), rows.clone(), cursor.clone())
            .err()
            .expect("invalid inbox checkpoint must fail preparation");
        assert!(matches!(
            error.operation,
            InteractiveOperation::PrepareRuntime
        ));
        assert!(error.detail.contains("expected"), "{error}");
        // BindToolHost succeeded before the malformed checkpoint was read. Its
        // named socket and exclusively created parent must both be removed.
        assert!(!socket_path.join("host-tools.sock").exists());
        assert!(!socket_path.exists());
        assert_eq!(std::fs::read(&cursor).unwrap(), b"not a checkpoint");

        std::fs::create_dir(&socket_path).unwrap();
        let preexisting = UnixListener::bind(socket_path.join("host-tools.sock")).unwrap();
        std::fs::write(socket_path.join("marker"), b"belongs to another owner").unwrap();
        assert!(prepare_socket_inbox(actor, socket_path.clone(), rows, cursor).is_err());
        assert!(socket_path.join("host-tools.sock").exists());
        assert_eq!(
            std::fs::read(socket_path.join("marker")).unwrap(),
            b"belongs to another owner"
        );
        drop(preexisting);
    }

    #[tokio::test]
    async fn socket_preparation_cleans_bind_failure() {
        let root = tempfile::tempdir().unwrap();
        // Valid directory component, but longer than Unix socket sockaddr paths.
        let path = root.path().join("s".repeat(150));
        let error = prepare_socket_inbox(
            ActorRef::first(tidepool_actor::ActorId(79)),
            path.clone(),
            root.path().join("rows"),
            root.path().join("cursor"),
        )
        .err()
        .expect("overlong socket endpoint must fail to bind");
        assert!(matches!(
            error.operation,
            InteractiveOperation::BindToolHost
        ));
        assert!(!path.exists());
        assert!(!root.path().join("rows").exists());
    }

    #[tokio::test]
    async fn socket_postsubmission_error_and_retirement_report_retention() {
        let root = tempfile::tempdir().unwrap();
        let actor = ActorRef::first(tidepool_actor::ActorId(78));
        for retire in [false, true] {
            let path = root.path().join(if retire { "retire" } else { "launch" });
            let (mut socket, listener, _) = prepare_socket_inbox(
                actor,
                path.clone(),
                root.path().join("rows"),
                root.path().join("cursor"),
            )
            .unwrap();
            socket.work_may_exist();
            // Dropping/aborting the listener does not prove accepted hosted work
            // or a native process is finished.
            drop(listener);
            if retire {
                assert!(matches!(socket_cleanup_outcome(socket),
                    CleanupComponentOutcome::Failed { detail } if detail.contains("unconfirmed")));
            } else {
                let error = socket_launch_failure(
                    actor,
                    InteractiveOperation::LaunchProcess,
                    "launch timed out",
                    socket,
                );
                assert!(error
                    .detail
                    .contains("launch timed out; socket cleanup failed:"));
                assert!(error.detail.contains("unconfirmed"));
            }
            assert!(path.join("host-tools.sock").exists());
        }
    }

    #[tokio::test]
    async fn notification_admission_and_poll_preserve_typed_request_bindings() {
        let mut campaign = test_campaign::TestCampaign::start().await;
        let root = campaign.root_installation.policy.clone();
        let setup = dispatch_haskell_script(
            root.as_ref(),
            include_str!("actor_host/notification_setup.hs"),
        )
        .await;
        assert_eq!(setup["status"], "committed", "{setup:?}");
        let child = match tokio::time::timeout(Duration::from_secs(30), campaign.deployments.recv())
            .await
            .unwrap()
            .unwrap()
        {
            LocalResidentDeployment::PolicyInstalled(child) => child,
            _ => panic!("expected recipient policy"),
        };
        let activation =
            match tokio::time::timeout(Duration::from_secs(30), campaign.deployments.recv())
                .await
                .unwrap()
                .unwrap()
            {
                LocalResidentDeployment::SessionReady { activation } => activation,
                _ => panic!("expected original request activation"),
            };
        assert_eq!(activation.id.actor(), child.actor.identity());
        let directory = tempfile::tempdir().unwrap();
        // Distinct fresh hierarchies exercise both strict directory owners at
        // the authored notification/inbox seam; syscall denial is tested by node.
        let inbox = ActorInbox::open(
            directory.path().join("rows-tree/deep/rows"),
            directory.path().join("checkpoint-tree/deep/cursor"),
        )
        .unwrap();
        let inbox_key = "notification-test-inbox";
        let policy = root.clone();
        let send = tokio::spawn(async move {
            dispatch_haskell_script(
                policy.as_ref(),
                "Right receipt <- notify worker \"one-way notice\"",
            )
            .await
        });
        let command =
            match tokio::time::timeout(Duration::from_secs(30), campaign.deployments.recv())
                .await
                .unwrap()
                .unwrap()
            {
                LocalResidentDeployment::NotificationSend(command) => command,
                _ => panic!("notification fabricated another activation"),
            };
        assert_eq!(command.owner(), campaign.actor.identity());
        assert_eq!(command.target(), child.actor.identity());
        // This is real durable admission through the interpreter handoff, not a
        // native presentation fixture. Production send remains unavailable.
        let envelope = inbox
            .publish_tracked(
                DurableActorEvent::Text(command.message().to_owned()),
                NotificationProvenance {
                    sender: command.owner(),
                    target: command.target(),
                },
            )
            .unwrap();
        command.admitted(inbox_key.into(), envelope.sequence);
        let sent = send.await.unwrap();
        assert_eq!(sent["status"], "committed", "{sent:?}");
        let policy = root.clone();
        let poll = tokio::spawn(async move {
            dispatch_haskell_script(policy.as_ref(), "pollNotification receipt").await
        });
        let poll_command =
            match tokio::time::timeout(Duration::from_secs(30), campaign.deployments.recv())
                .await
                .unwrap()
                .unwrap()
            {
                LocalResidentDeployment::NotificationPoll(command) => command,
                _ => panic!("poll fabricated another activation"),
            };
        let result =
            observe_notification_receipt(&poll_command, child.actor.identity(), inbox_key, &inbox);
        assert_eq!(result, Ok(tidepool_actor::NotificationState::Accepted));
        assert_eq!(
            observe_notification_receipt(
                &poll_command,
                child.actor.identity(),
                "foreign-inbox",
                &inbox
            ),
            Err(tidepool_actor::NotificationError::InvalidReceipt)
        );
        let foreign_directory = tempfile::tempdir().unwrap();
        let foreign = ActorInbox::open(
            foreign_directory.path().join("rows-tree/deep/rows"),
            foreign_directory.path().join("checkpoint-tree/deep/cursor"),
        )
        .unwrap();
        assert_eq!(
            observe_notification_receipt(
                &poll_command,
                child.actor.identity(),
                inbox_key,
                &foreign
            ),
            Err(tidepool_actor::NotificationError::Unavailable)
        );
        foreign
            .publish_tracked(
                DurableActorEvent::Text("another sender".into()),
                NotificationProvenance {
                    sender: child.actor.identity(),
                    target: child.actor.identity(),
                },
            )
            .unwrap();
        assert_eq!(
            observe_notification_receipt(
                &poll_command,
                child.actor.identity(),
                inbox_key,
                &foreign
            ),
            Err(tidepool_actor::NotificationError::Unauthorized)
        );
        let stale = ActorRef {
            incarnation: tidepool_actor::Incarnation(child.actor.identity().incarnation.0 + 1),
            ..child.actor.identity()
        };
        assert_eq!(
            observe_notification_receipt(&poll_command, stale, inbox_key, &inbox),
            Err(tidepool_actor::NotificationError::InvalidReceipt)
        );
        drop(inbox);
        let inbox = ActorInbox::open(
            directory.path().join("rows-tree/deep/rows"),
            directory.path().join("checkpoint-tree/deep/cursor"),
        )
        .unwrap();
        assert_eq!(
            observe_notification_receipt(&poll_command, child.actor.identity(), inbox_key, &inbox),
            Ok(tidepool_actor::NotificationState::Accepted)
        );
        // Submitted transport acceptance is explicitly NOT model presentation.
        inbox
            .begin_tracked_delivery(envelope.sequence)
            .unwrap()
            .submitted()
            .unwrap();
        assert_eq!(
            observe_notification_receipt(&poll_command, child.actor.identity(), inbox_key, &inbox),
            Ok(tidepool_actor::NotificationState::Unconfirmed)
        );
        poll_command.observed(result);
        let observed = poll.await.unwrap();
        assert_eq!(observed["status"], "committed", "{observed:?}");
        assert!(
            observed.to_string().contains("NotificationAccepted"),
            "{observed:?}"
        );
        let unchanged =
            dispatch_haskell_script(child.policy.as_ref(), "inspectFull sessionInput").await;
        assert_eq!(unchanged["status"], "committed", "{unchanged:?}");
        assert!(
            unchanged.to_string().contains("original assignment"),
            "{unchanged:?}"
        );
        assert!(
            campaign.deployments.try_recv().is_err(),
            "notification created an assignment/wake obligation"
        );
        let reply =
            dispatch_haskell_script(child.policy.as_ref(), "respond (sessionInput :: Text)").await;
        assert_eq!(reply["status"], "replied", "{reply:?}");
        let answer =
            dispatch_haskell_script(root.as_ref(), "inspectFull <$> pollResponse answer").await;
        assert_eq!(answer["status"], "committed", "{answer:?}");
        assert!(
            answer.to_string().contains("original assignment"),
            "{answer:?}"
        );
        let root_reply = dispatch_haskell_script(root.as_ref(), ":type respond").await;
        assert!(
            root_reply
                .to_string()
                .to_lowercase()
                .contains("not in scope"),
            "{root_reply:?}"
        );
        let idle_setup = dispatch_haskell_script(
            root.as_ref(),
            "idle <- startAgent (readonlyAgent \"idle-notification-recipient\")",
        )
        .await;
        assert_eq!(idle_setup["status"], "committed", "{idle_setup:?}");
        let idle = match tokio::time::timeout(Duration::from_secs(30), campaign.deployments.recv())
            .await
            .unwrap()
            .unwrap()
        {
            LocalResidentDeployment::PolicyInstalled(child) => child,
            _ => panic!("expected never-assigned recipient policy"),
        };
        let policy = root.clone();
        let idle_send = tokio::spawn(async move {
            dispatch_haskell_script(
                policy.as_ref(),
                "Right idleReceipt <- notify idle \"idle notice\"",
            )
            .await
        });
        let command =
            match tokio::time::timeout(Duration::from_secs(30), campaign.deployments.recv())
                .await
                .unwrap()
                .unwrap()
            {
                LocalResidentDeployment::NotificationSend(command) => command,
                _ => panic!("idle notification fabricated request activation"),
            };
        assert_eq!(command.target(), idle.actor.identity());
        let idle_directory = tempfile::tempdir().unwrap();
        let idle_inbox = ActorInbox::open(
            idle_directory.path().join("rows-tree/deep/rows"),
            idle_directory.path().join("checkpoint-tree/deep/cursor"),
        )
        .unwrap();
        let row = idle_inbox
            .publish_tracked(
                DurableActorEvent::Text(command.message().into()),
                NotificationProvenance {
                    sender: command.owner(),
                    target: command.target(),
                },
            )
            .unwrap();
        command.admitted("idle-inbox".into(), row.sequence);
        let admitted = idle_send.await.unwrap();
        assert_eq!(admitted["status"], "committed", "{admitted:?}");
        for name in ["respond", "sessionReply", "sessionInput"] {
            let absent =
                dispatch_haskell_script(idle.policy.as_ref(), &format!(":type {name}")).await;
            assert!(
                absent.to_string().to_lowercase().contains("not in scope"),
                "idle recipient gained {name}: {absent:?}"
            );
        }
        assert!(
            campaign.deployments.try_recv().is_err(),
            "idle admission fabricated an activation"
        );
        campaign.forest.shutdown().await;
        campaign.hosted.await.unwrap();
    }

    #[tokio::test]
    async fn notification_barrier_never_enters_legacy_push_or_batch_ack() {
        let root = tempfile::tempdir().unwrap();
        let inbox = Arc::new(
            ActorInbox::open(root.path().join("rows"), root.path().join("cursor")).unwrap(),
        );
        let target = ActorRef::first(tidepool_actor::ActorId(7));
        inbox
            .publish(DurableActorEvent::Text("ordinary prefix".into()))
            .unwrap();
        inbox
            .publish_tracked(
                DurableActorEvent::Text("one-way text".into()),
                NotificationProvenance {
                    sender: ActorRef::first(tidepool_actor::ActorId(8)),
                    target,
                },
            )
            .unwrap();
        inbox
            .publish(DurableActorEvent::Text("ordinary suffix".into()))
            .unwrap();
        // An old envelope decoder ignores receipt metadata but must accept every
        // payload before it gets the chance to reject the upgraded checkpoint.
        // A schema-invalid final row could otherwise trigger old tail repair.
        #[derive(Deserialize)]
        struct OldTextEnvelope {
            sequence: u64,
            payload: String,
        }
        let rows = std::fs::read_to_string(root.path().join("rows")).unwrap();
        let old_rows = rows
            .lines()
            .map(|line| serde_json::from_str::<OldTextEnvelope>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(old_rows.len(), 3);
        assert_eq!(old_rows[1].sequence, 2);
        assert_eq!(old_rows[1].payload, "one-way text");
        let backend = ScriptedPush {
            fail: std::sync::atomic::AtomicBool::new(false),
            messages: std::sync::Mutex::new(Vec::new()),
        };
        let binding = root.path().join("binding.json");
        tidepool_agent::accept_interactive_session_binding(
            &binding,
            tidepool_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
            BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into()),
            None,
        )
        .await
        .unwrap();
        let thread = tidepool_agent::read_interactive_binding(&binding)
            .await
            .unwrap();
        let observation = tidepool_actor::ActorRuntimeObservationHandle::default();
        for _ in 0..2 {
            deliver_pending(target, &inbox, &thread, &backend, root.path(), &observation)
                .await
                .unwrap();
        }
        assert_eq!(*backend.messages.lock().unwrap(), vec!["ordinary prefix"]);
        assert_eq!(inbox.cursor(), 1);
        assert_eq!(inbox.watermark(), 3);
        assert!(matches!(
            inbox.pending(),
            Err(tidepool_node::InboxError::TrackedBarrier { sequence: 2 })
        ));
        assert!(inbox.legacy_pending_prefix().unwrap().is_empty());
        assert!(matches!(
            inbox.observe_receipt(2).unwrap(),
            tidepool_node::ReceiptLookup::Retained(evidence)
                if evidence.phase == tidepool_node::DeliveryPhase::Accepted
        ));
    }

    #[tokio::test]
    async fn native_push_acknowledges_only_after_acceptance_and_retries_the_same_row() {
        let root = tempfile::tempdir().expect("inbox root");
        let inbox = Arc::new(
            ActorInbox::open(root.path().join("rows"), root.path().join("cursor"))
                .expect("open inbox"),
        );
        inbox
            .publish(DurableActorEvent::Text("child completed".into()))
            .expect("publish");
        let backend = ScriptedPush {
            fail: std::sync::atomic::AtomicBool::new(true),
            messages: std::sync::Mutex::new(Vec::new()),
        };
        let binding_path = root.path().join("binding.json");
        tidepool_agent::accept_interactive_session_binding(
            &binding_path,
            tidepool_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
            BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into()),
            None,
        )
        .await
        .unwrap();
        let thread = tidepool_agent::read_interactive_binding(&binding_path)
            .await
            .unwrap();
        let actor = ActorRef::first(tidepool_actor::ActorId(7));
        let observation = tidepool_actor::ActorRuntimeObservationHandle::default();
        assert_eq!(
            orient_launch_instructions("event", &observation.snapshot()),
            "event"
        );
        observation.publish_launch_role(tidepool_actor::EffectiveRole::research(), 0);
        observation.publish_workspace(tidepool_actor::ActorWorkspaceObservation {
            workspace_path: "/tmp/visible".into(),
            host_storage_path: "/host/research".into(),
            worktree_id: Some("research-tree".into()),
            expected_branch: Some("research".into()),
        });
        let orientation = observation.snapshot().launch_orientation().unwrap();
        assert!(orientation.contains("role=Research"));
        assert!(orientation.contains("native_tools=InspectionOnly"));
        assert!(orientation.contains("workspace=InspectOnly"));
        assert!(orientation.contains("descendant_depth=0"));
        assert!(orientation.contains("workspace_path=\"/tmp/visible\" (native tools)"));
        assert!(orientation.contains("expected_branch=\"research\""));
        assert!(!orientation.contains("sessionReply"));
        assert_eq!(
            orient_launch_instructions("launch instructions", &observation.snapshot()),
            format!("launch instructions\n\n{orientation}")
        );

        assert!(
            deliver_pending(actor, &inbox, &thread, &backend, root.path(), &observation,)
                .await
                .is_err()
        );
        assert_eq!(inbox.pending().expect("pending after refusal").len(), 1);
        assert_eq!(
            observation.snapshot().activation_kind,
            tidepool_actor::ActorActivationKind::RootStarted
        );
        inbox
            .publish(DurableActorEvent::Text("second event".into()))
            .expect("publish second event");

        backend
            .fail
            .store(false, std::sync::atomic::Ordering::SeqCst);
        deliver_pending(actor, &inbox, &thread, &backend, root.path(), &observation)
            .await
            .expect("retry accepted");
        assert!(inbox.pending().expect("acked inbox").is_empty());
        assert_eq!(
            *backend.messages.lock().unwrap(),
            [
                "child completed".to_owned(),
                "child completed\n\nsecond event".to_owned(),
            ]
        );
        assert_eq!(
            observation.snapshot().activation_kind,
            tidepool_actor::ActorActivationKind::EventsActivated {
                inbox_sequences: vec![1, 2]
            }
        );

        // First and retained requests use the same unwrapped message boundary.
        for sequence in [1, 2] {
            let message = format!("Request {sequence}: review\n\nReturn with `respond`.");
            inbox
                .publish(DurableActorEvent::Typed(TypedActorEvent::SessionReady {
                    sequence,
                    request: tidepool_actor::RequestId(sequence),
                    input_type: "Text".into(),
                    message: message.clone(),
                }))
                .unwrap();
            backend
                .fail
                .store(true, std::sync::atomic::Ordering::SeqCst);
            assert!(
                deliver_pending(actor, &inbox, &thread, &backend, root.path(), &observation)
                    .await
                    .is_err()
            );
            backend
                .fail
                .store(false, std::sync::atomic::Ordering::SeqCst);
            deliver_pending(actor, &inbox, &thread, &backend, root.path(), &observation)
                .await
                .unwrap();
            let messages = backend.messages.lock().unwrap();
            assert_eq!(&messages[messages.len() - 2..], &[message.clone(), message]);
            assert!(inbox.pending().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn retired_delivery_is_forced_without_claiming_tool_service_cleanup() {
        let actor = ActorRef::first(tidepool_actor::ActorId(7));
        let mut delivery = tokio::spawn(std::future::pending::<()>());
        let outcome = stop_retired_delivery(actor, &mut delivery, Duration::ZERO).await;
        assert!(delivery.is_finished());
        assert_eq!(outcome, CleanupComponentOutcome::Forced);
    }

    #[test]
    fn degraded_cleanup_receipt_preserves_each_component_without_becoming_fleet_failure() {
        let receipt = InteractiveCleanupReceipt {
            actor: ActorRef::first(tidepool_actor::ActorId(7)),
            components: vec![
                CleanupComponentReceipt {
                    component: CleanupComponent::Process,
                    outcome: CleanupComponentOutcome::Failed {
                        detail: "pane already unavailable".into(),
                    },
                },
                CleanupComponentReceipt {
                    component: CleanupComponent::Delivery,
                    outcome: CleanupComponentOutcome::Completed,
                },
                CleanupComponentReceipt {
                    component: CleanupComponent::WorktreeBinding,
                    outcome: CleanupComponentOutcome::Failed {
                        detail: "binding journal unavailable".into(),
                    },
                },
            ],
        };

        assert!(receipt.degraded());
        assert_eq!(receipt.components.len(), 3);
        let rendered = receipt.render();
        assert!(rendered.contains("Process: pane already unavailable"));
        assert!(rendered.contains("WorktreeBinding: binding journal unavailable"));
        assert!(rendered.contains("permanent host and sibling actors remain available"));
    }

    #[test]
    fn worker_workspaces_are_distinct_linked_worktrees_in_one_git_namespace() {
        let repository = tidepool_worktree::testing::TestRepo::init().unwrap();
        repository
            .writer()
            .commit_file("README.md", "source\n", "seed")
            .unwrap();
        let storage = tempfile::tempdir().unwrap();
        let (manager, _bindings) =
            actor_worktree_resources_at(storage.path(), repository.path()).unwrap();
        let first = manager
            .create(&WorktreeSpec::from_current_repository("first-worker"))
            .unwrap();
        let second = manager
            .create(&WorktreeSpec::from_current_repository("second-worker"))
            .unwrap();

        assert_ne!(first.id(), second.id());
        assert_ne!(first.cwd(), second.cwd());
        assert!(!first.cwd().starts_with(repository.path()));
        assert!(!second.cwd().starts_with(repository.path()));
        assert!(first.cwd().join(".git").is_file());
        assert!(second.cwd().join(".git").is_file());
        assert_eq!(
            tidepool_worktree::git::inspect::git_common_dir(manager.git(), first.cwd()).unwrap(),
            repository.path().join(".git")
        );
        assert_eq!(
            tidepool_worktree::git::inspect::git_common_dir(manager.git(), second.cwd()).unwrap(),
            repository.path().join(".git")
        );
        assert_eq!(
            std::fs::read_to_string(repository.path().join("README.md")).unwrap(),
            "source\n"
        );
    }

    #[tokio::test]
    async fn typed_reply_settles_response_and_wakes_registered_watch() {
        fn fixture_items(source: &'static str) -> Vec<&'static str> {
            source
                .split("\n-- TIDEPOOL-ITEM --\n")
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .collect()
        }

        let test_campaign::TestCampaign {
            _repository,
            _runtime,
            session_root,
            worktrees,
            bindings,
            authority,
            actor,
            hosted,
            mut deployments,
            root_installation,
            ..
        } = test_campaign::TestCampaign::start().await;

        let ergonomics = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            include_str!("actor_host_fixtures/generic_actor/workbench_ergonomics.hs"),
        )
        .await;
        assert_eq!(ergonomics["status"], "committed", "{ergonomics:?}");
        assert_eq!(ergonomics["items"][0]["status"], "diagnostic");
        assert_eq!(ergonomics["items"][1]["status"], "committed");
        assert_eq!(ergonomics["items"][2]["status"], "committed");
        assert_eq!(ergonomics["items"][3]["status"], "committed");
        assert!(ergonomics["items"][3]["warnings"]
            .as_array()
            .is_some_and(|warnings| !warnings.is_empty()));
        assert_eq!(ergonomics["items"][4]["status"], "committed");
        assert_eq!(ergonomics["items"][5]["output"], "7");
        assert_eq!(ergonomics["items"][6]["output"], "value=7");
        for index in 7..=8 {
            let item = &ergonomics["items"][index];
            assert_eq!(item["status"], "committed", "{item:?}");
        }

        // Execute the mounted documentation itself, including its real
        // multiline delimiters, rather than a separately maintained example.
        let workbench_doc = include_str!("../../prompts/shoal/docs/workbench.md");
        let (_, example) = workbench_doc.split_once("```haskell\n").unwrap();
        let (example, _) = example.split_once("```").unwrap();
        let documented = dispatch_haskell_script(root_installation.policy.as_ref(), example).await;
        assert_eq!(documented["status"], "committed", "{documented:?}");
        let items = documented["items"].as_array().unwrap();
        assert!(
            items.iter().all(|item| item["status"] == "committed"),
            "{documented:?}"
        );
        assert_eq!(items[items.len() - 2]["output"], "True");
        assert_eq!(items[items.len() - 1]["output"], "score=7");

        let setup_policy = Arc::clone(&root_installation.policy);
        let submitted = tokio::spawn(async move {
            dispatch_haskell(
                setup_policy.as_ref(),
                fixture_items(include_str!(
                    "actor_host_fixtures/generic_actor/reply_watch_roundtrip.hs"
                )),
            )
            .await
        });
        let child_installations = tokio::time::timeout(Duration::from_secs(180), async {
            let mut children = Vec::new();
            while children.len() < 3 {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::PolicyInstalled(installation))
                        if installation.actor.identity() != actor.identity() =>
                    {
                        children.push(installation);
                    }
                    Some(LocalResidentDeployment::Retired { actor, terminal }) => {
                        panic!("actor {actor:?} retired before child installation: {terminal:?}")
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before child installation"),
                }
            }
            children
        })
        .await
        .expect("child installation timeout");
        let worker_installation = child_installations
            .iter()
            .find(|installation| installation.label.ends_with("/worker"))
            .expect("worker installation")
            .clone();
        let witness_installation = child_installations
            .iter()
            .find(|installation| installation.label.ends_with("/witness"))
            .expect("witness installation")
            .clone();
        let scaffold_installation = child_installations
            .iter()
            .find(|installation| installation.label.ends_with("/scaffold"))
            .expect("scaffold installation")
            .clone();
        for installation in &child_installations {
            assert_eq!(installation.context_parent, Some(actor.identity()));
            assert_eq!(
                installation.fork_group,
                Some(tidepool_actor::ForkGroupId(1))
            );
            let expected_role = if installation.label.ends_with("/scaffold") {
                tidepool_actor::ActorRole::Coding
            } else {
                tidepool_actor::ActorRole::Research
            };
            assert_eq!(installation.effective_role.role(), expected_role);
            authority.install_grant(
                installation.actor.identity().into(),
                worktree_grant(expected_role),
            );
            let [worktree_id] = installation.launch_worktrees.as_slice() else {
                panic!("forked research actor did not receive one named worktree")
            };
            let worktree = worktrees
                .lookup(&tidepool_worktree::WorktreeId::from_raw(worktree_id))
                .expect("named worktree lookup")
                .expect("named worktree remains registered");
            assert_eq!(
                worktree.branch().as_str(),
                tidepool_repr::ActorPath::parse(&installation.label)
                    .expect("allocated actor path")
                    .git_branch()
            );
            let principal = WorktreePrincipal::exact_actor(
                &runtime_namespace(session_root.path()),
                installation.actor.identity().id.0,
                installation.actor.identity().incarnation.0,
            );
            assert!(installation.worktree_custody.is_some());
            assert_eq!(
                bindings.lock().current(worktree.id()).unwrap().agent(),
                &principal
            );
        }
        worker_installation
            .fork_gate
            .as_ref()
            .expect("context-fork child has an admission gate")
            .mark_ready()
            .expect("test host marks first child provider ready");
        assert!(
            !submitted.is_finished(),
            "one ready sibling must not publish a partially admitted unfold"
        );
        witness_installation
            .fork_gate
            .as_ref()
            .expect("context-fork sibling has an admission gate")
            .mark_ready()
            .expect("test host marks second child provider ready");
        scaffold_installation
            .fork_gate
            .as_ref()
            .expect("context-fork scaffold has an admission gate")
            .mark_ready()
            .expect("test host marks scaffold provider ready");
        let submitted = tokio::time::timeout(Duration::from_secs(120), submitted)
            .await
            .expect("request setup timed out")
            .expect("request setup task");
        assert_eq!(submitted["status"], "committed", "{submitted:?}");
        assert!(submitted["items"].as_array().is_some_and(|items| items
            .iter()
            .any(|item| item["operations"]
                .as_array()
                .is_some_and(|operations| !operations.is_empty()))));

        let pending_status =
            dispatch_haskell_script(root_installation.policy.as_ref(), ":status").await;
        let pending_status = pending_status["items"][0]["output"]
            .as_str()
            .expect("status output");
        assert!(
            pending_status.contains("application=attached"),
            "{pending_status}"
        );
        assert!(
            pending_status.contains("responses:")
                && pending_status.contains("\"worker\"")
                && pending_status.contains("\"witness\""),
            "{pending_status}"
        );
        assert!(
            pending_status.contains("watches:") && pending_status.contains("\"both-ready\""),
            "{pending_status}"
        );
        assert!(pending_status.contains("after=5min"), "{pending_status}");
        assert!(
            pending_status.contains("actors:\n")
                && pending_status.contains("role=Research")
                && pending_status.contains("role=Coding")
                && pending_status.contains("bound_worktree=Some("),
            "{pending_status}"
        );
        let lineage = dispatch_haskell_script(root_installation.policy.as_ref(), ":lineage").await;
        let lineage = lineage["items"][0]["output"]
            .as_str()
            .expect("lineage output");
        for field in [
            "context_parent=",
            "haskell_scope=",
            "provider_thread=",
            "provider_parent_thread=",
            "cache_boundary=",
            "cached_input=",
            "uncached_input=",
        ] {
            assert!(lineage.contains(field), "missing {field}: {lineage}");
        }
        let launch_receipt = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "(forkedLaunch (first3 workers), forkedLaunch (second3 workers), forkedLaunch (third3 workers))",
        )
        .await;
        let launch_receipt = launch_receipt["items"][0]["output"]
            .as_str()
            .expect("branch receipt output");
        assert!(
            launch_receipt.contains("ActorPath \"reply-watch/roundtrip/worker\"")
                && launch_receipt.contains("ActorPath \"reply-watch/roundtrip/witness\"")
                && launch_receipt.contains("ActorPath \"reply-watch/roundtrip/scaffold\"")
                && launch_receipt.matches("forkGroupIdentity = 1").count() == 3,
            "{launch_receipt}"
        );
        let scaffold_watch = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "scaffoldReadiness <- watch (case watchLabel \"scaffold-ready\" of { Right value -> value; Left _ -> error \"fixture watch\" }) (awaitSettledFork (third3 workers))",
        )
        .await;
        assert_eq!(scaffold_watch["status"], "committed", "{scaffold_watch:?}");

        let activations = tokio::time::timeout(Duration::from_secs(10), async {
            let mut activations = Vec::new();
            while activations.len() < 3 {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if child_installations
                            .iter()
                            .any(|child| child.actor.identity() == activation.id.actor()) =>
                    {
                        activations.push(activation);
                    }
                    Some(LocalResidentDeployment::Retired { actor, terminal }) => {
                        panic!("actor {actor:?} retired before request activation: {terminal:?}")
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before request activation"),
                }
            }
            activations
        })
        .await
        .expect("request activation timeout");
        let worker_activation = activations
            .iter()
            .find(|activation| activation.id.actor() == worker_installation.actor.identity())
            .expect("worker activation");
        let witness_activation = activations
            .iter()
            .find(|activation| activation.id.actor() == witness_installation.actor.identity())
            .expect("witness activation");
        let scaffold_activation = activations
            .iter()
            .find(|activation| activation.id.actor() == scaffold_installation.actor.identity())
            .expect("scaffold activation");
        assert_eq!(worker_activation.input_type, "Int");
        assert_eq!(witness_activation.input_type, "Text");
        assert_eq!(scaffold_activation.input_type, "Text");

        let replied = dispatch_haskell_script(
            worker_installation.policy.as_ref(),
            ":type sessionReply\n:type respond\nrespond (ReplyReport (sessionInput + sharedDelta))",
        )
        .await;
        assert_eq!(replied["status"], "replied", "{replied:?}");
        assert_eq!(
            replied["items"][2]["terminalTransfer"], "replyAccepted",
            "{replied:?}"
        );
        assert!(replied["items"][2]["operations"]
            .as_array()
            .is_some_and(|operations| operations.iter().any(|operation| {
                operation["effect"] == "reply" && operation["disposition"] == "committed"
            })));
        let witnessed = dispatch_haskell_script(
            witness_installation.policy.as_ref(),
            "respond (EchoReport sessionInput)",
        )
        .await;
        assert_eq!(witnessed["status"], "replied", "{witnessed:?}");

        let notification = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::WatchChanged { notification })
                        if notification.owner == actor.identity() =>
                    {
                        break notification;
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before watch transition"),
                }
            }
        })
        .await
        .expect("watch transition timeout");
        assert_eq!(
            notification.transition,
            tidepool_actor::WatchTransition::Ready
        );

        let observed = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "inspectFull <$> pollResponse (forkedResponse (first3 workers))\ninspectFull <$> pollResponse (forkedResponse (second3 workers))\ninspectFull <$> pollWatch readiness",
        )
        .await;
        assert_eq!(observed["status"], "committed", "{observed:?}");
        assert!(
            observed["items"][0]["output"]
                .as_str()
                .is_some_and(|output| {
                    output.contains("ResponseReady")
                        && output.contains("responseValue = ReplyReport 42")
                        && output.contains("responseWorktree = WorktreeObserved")
                }),
            "{observed:?}"
        );
        assert!(
            observed["items"][1]["output"]
                .as_str()
                .is_some_and(|output| {
                    output.contains("ResponseReady")
                        && output.contains("responseValue = EchoReport \"cache\"")
                }),
            "{observed:?}"
        );
        assert!(
            observed["items"][2]["output"]
                .as_str()
                .is_some_and(|output| {
                    output.contains("WatchReady")
                        && output.contains("ReplyReport 42")
                        && output.contains("EchoReport \"cache\"")
                }),
            "{observed:?}"
        );

        let group_observation = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "fmap (length . groupRoster) <$> observeForkGroup (forkGroupHandle (first3 workers))",
        )
        .await;
        assert_eq!(
            group_observation["status"], "committed",
            "{group_observation:?}"
        );
        assert_eq!(group_observation["items"][0]["output"], "Just 3");

        let ready_status =
            dispatch_haskell_script(root_installation.policy.as_ref(), ":status").await;
        let ready_status = ready_status["items"][0]["output"]
            .as_str()
            .expect("status output");
        assert!(ready_status.contains("\"scaffold\""), "{ready_status}");
        assert!(ready_status.contains("\"worker\""), "{ready_status}");
        assert!(ready_status.contains("\"witness\""), "{ready_status}");
        assert!(ready_status.contains("\"both-ready\""), "{ready_status}");

        let cleanup_preview = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "staleCleanup <- planCleanup (forkGroupHandle (first3 workers))",
        )
        .await;
        assert_eq!(
            cleanup_preview["status"], "committed",
            "{cleanup_preview:?}"
        );

        let followup = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            include_str!("actor_host_fixtures/generic_actor/late_refinement.hs"),
        )
        .await;
        assert_eq!(followup["status"], "committed", "{followup:?}");
        let refinement_doc = include_str!("../../prompts/shoal/docs/refinement.md");
        let (_, example) = refinement_doc.split_once("```haskell\n").unwrap();
        let (example, _) = example.split_once("```").unwrap();
        let refinement = dispatch_haskell_script(root_installation.policy.as_ref(), example).await;
        assert_eq!(refinement["status"], "committed", "{refinement:?}");
        let followup_binding =
            dispatch_haskell_script(root_installation.policy.as_ref(), "let followup = revision")
                .await;
        assert_eq!(
            followup_binding["status"], "committed",
            "{followup_binding:?}"
        );
        let followup_watch = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "followupReadiness <- watch (case watchLabel \"revision-ready\" of { Right value -> value; Left _ -> error \"fixture watch\" }) (awaitResponse followup)",
        )
        .await;
        assert_eq!(followup_watch["status"], "committed", "{followup_watch:?}");
        let followup_activation = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == worker_installation.actor.identity() =>
                    {
                        break activation;
                    }
                    Some(LocalResidentDeployment::Retired { actor, terminal }) => {
                        panic!("actor {actor:?} retired before follow-up activation: {terminal:?}")
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before follow-up activation"),
                }
            }
        })
        .await
        .expect("follow-up activation timeout");
        assert!(followup_activation.input_type.ends_with("LateRefinement"));
        let followup_reply = dispatch_haskell_script(
            worker_installation.policy.as_ref(),
            "respond (LateReport (refinementTransform sessionInput (refinementValue sessionInput) + sharedDelta))",
        )
        .await;
        assert_eq!(followup_reply["status"], "replied", "{followup_reply:?}");
        let followup_notification = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::WatchChanged { notification })
                        if notification.owner == actor.identity() =>
                    {
                        break notification;
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before follow-up watch transition"),
                }
            }
        })
        .await
        .expect("follow-up watch transition timeout");
        assert_eq!(
            followup_notification.transition,
            tidepool_actor::WatchTransition::Ready
        );
        let followup_result = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "inspectFull <$> pollResponse followup",
        )
        .await;
        assert_eq!(
            followup_result["status"], "committed",
            "{followup_result:?}"
        );
        assert!(
            followup_result["items"][0]["output"]
                .as_str()
                .is_some_and(|output| output.contains("responseValue = LateReport 101")),
            "{followup_result:?}"
        );

        let stale_cleanup = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "executeCleanup staleCleanup",
        )
        .await;
        assert_eq!(stale_cleanup["status"], "committed", "{stale_cleanup:?}");
        let refusal = stale_cleanup["items"][0]["output"].as_str().unwrap();
        assert!(refusal.contains("CleanupStalePlan"), "{refusal}");
        assert!(!refusal.contains("CleanupStoppedActor"), "{refusal}");
        assert!(
            refusal.contains("cleanupReceiptComplete = False"),
            "{refusal}"
        );

        let scaffold_policy = Arc::clone(&scaffold_installation.policy);
        let nested_submitted = tokio::spawn(async move {
            dispatch_haskell_script(
                scaffold_policy.as_ref(),
                "nested <- unfold (subgroup (case forkGroupLabel \"leaves\" of { Right value -> value; Left _ -> error \"fixture subgroup\" })) ((,) <$> child (coding @ReplyReport (case branchLabel \"implementation\" of { Right value -> value; Left _ -> error \"fixture leaf\" }) boundHead (7 :: Int)) <*> child (coding @EchoReport (case branchLabel \"verification\" of { Right value -> value; Left _ -> error \"fixture leaf\" }) boundHead (\"nested\" :: Text)))",
            )
            .await
        });
        let nested_installations = tokio::time::timeout(Duration::from_secs(60), async {
            let mut children = Vec::new();
            while children.len() < 2 {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::PolicyInstalled(installation))
                        if installation.context_parent
                            == Some(scaffold_installation.actor.identity()) =>
                    {
                        children.push(installation);
                    }
                    Some(LocalResidentDeployment::Retired { actor, terminal }) => {
                        panic!("nested actor {actor:?} retired during admission: {terminal:?}")
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed during nested admission"),
                }
            }
            children
        })
        .await
        .expect("nested child installation timeout");
        let implementation = nested_installations
            .iter()
            .find(|installation| installation.label.ends_with("/implementation"))
            .expect("nested implementation installation")
            .clone();
        let verification = nested_installations
            .iter()
            .find(|installation| installation.label.ends_with("/verification"))
            .expect("nested verification installation")
            .clone();
        let scaffold_tree_id = tidepool_worktree::WorktreeId::from_raw(
            scaffold_installation.launch_worktrees[0].clone(),
        );
        let scaffold_tree = worktrees
            .lookup(&scaffold_tree_id)
            .expect("scaffold lookup")
            .expect("scaffold worktree retained");
        let scaffold_head = worktrees
            .git()
            .try_run(scaffold_tree.cwd(), &["rev-parse", "HEAD"])
            .expect("scaffold head")
            .trimmed()
            .to_string();
        for installation in &nested_installations {
            assert_eq!(
                installation.effective_role.role(),
                tidepool_actor::ActorRole::Coding
            );
            assert_eq!(
                installation.effective_role.descendants().maximum_depth + 1,
                scaffold_installation
                    .effective_role
                    .descendants()
                    .maximum_depth
            );
            assert!(installation
                .effective_role
                .effect_keys()
                .contains(&tidepool_actor::ActorEffectKey::Forks));
            let worktree_id =
                tidepool_worktree::WorktreeId::from_raw(installation.launch_worktrees[0].clone());
            let worktree = worktrees
                .lookup(&worktree_id)
                .expect("nested worktree lookup")
                .expect("nested worktree retained");
            assert_eq!(worktree.source_head().as_str(), scaffold_head);
            assert_eq!(
                worktree.branch().as_str(),
                tidepool_repr::ActorPath::parse(&installation.label)
                    .expect("allocated nested actor path")
                    .git_branch()
            );
            let principal = WorktreePrincipal::exact_actor(
                &runtime_namespace(session_root.path()),
                installation.actor.identity().id.0,
                installation.actor.identity().incarnation.0,
            );
            assert!(installation.worktree_custody.is_some());
            assert_eq!(
                bindings.lock().current(worktree.id()).unwrap().agent(),
                &principal
            );
            installation
                .fork_gate
                .as_ref()
                .expect("nested fork gate")
                .mark_ready()
                .expect("mark nested provider ready");
        }
        let nested_submitted = tokio::time::timeout(Duration::from_secs(120), nested_submitted)
            .await
            .expect("nested unfold timed out")
            .expect("nested unfold task");
        assert_eq!(
            nested_submitted["status"], "committed",
            "{nested_submitted:?}"
        );
        let nested_watch = dispatch_haskell_script(
            scaffold_installation.policy.as_ref(),
            "nestedReady <- watch (case watchLabel \"leaves-ready\" of { Right value -> value; Left _ -> error \"fixture watch\" }) ((,) <$> awaitFork (fst nested) <*> awaitFork (snd nested))",
        )
        .await;
        assert_eq!(nested_watch["status"], "committed", "{nested_watch:?}");

        let nested_activations = tokio::time::timeout(Duration::from_secs(10), async {
            let mut activations = Vec::new();
            while activations.len() < 2 {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if nested_installations
                            .iter()
                            .any(|child| child.actor.identity() == activation.id.actor()) =>
                    {
                        activations.push(activation);
                    }
                    Some(LocalResidentDeployment::Retired { actor, terminal }) => {
                        panic!("nested actor {actor:?} retired before activation: {terminal:?}")
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before nested activation"),
                }
            }
            activations
        })
        .await
        .expect("nested activation timeout");
        assert!(nested_activations.iter().any(|activation| {
            activation.id.actor() == implementation.actor.identity()
                && activation.input_type == "Int"
        }));
        assert!(nested_activations.iter().any(|activation| {
            activation.id.actor() == verification.actor.identity()
                && activation.input_type == "Text"
        }));

        for (installation, path, contents) in [
            (&implementation, "implementation.txt", "implemented\n"),
            (&verification, "verification.txt", "verified\n"),
        ] {
            let tree = worktrees
                .lookup(&tidepool_worktree::WorktreeId::from_raw(
                    installation.launch_worktrees[0].clone(),
                ))
                .expect("lookup nested commit tree")
                .expect("nested commit tree retained");
            std::fs::write(tree.cwd().join(path), contents).expect("write nested candidate");
            worktrees
                .git()
                .try_run(tree.cwd(), &["add", path])
                .expect("stage nested candidate");
            worktrees
                .git()
                .try_run(tree.cwd(), &["commit", "-m", path])
                .expect("commit nested candidate");
        }
        let implementation_reply = dispatch_haskell_script(
            implementation.policy.as_ref(),
            "respond (ReplyReport (sessionInput + sharedDelta))",
        )
        .await;
        assert_eq!(
            implementation_reply["status"], "replied",
            "{implementation_reply:?}"
        );
        let peer_setup = dispatch_haskell_script(
            verification.policy.as_ref(),
            include_str!("actor_host_fixtures/generic_actor/peer_revision.hs"),
        )
        .await;
        assert_eq!(peer_setup["status"], "committed", "{peer_setup:?}");
        let peer_repair = dispatch_haskell_script(verification.policy.as_ref(), example).await;
        assert_eq!(peer_repair["status"], "committed", "{peer_repair:?}");
        let repair_watch_example = refinement_doc
            .split("```haskell\n")
            .nth(2)
            .unwrap()
            .split_once("```")
            .unwrap()
            .0;
        let peer_watch =
            dispatch_haskell_script(verification.policy.as_ref(), repair_watch_example).await;
        assert_eq!(peer_watch["status"], "committed", "{peer_watch:?}");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == implementation.actor.identity() =>
                    {
                        break
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before peer repair"),
                }
            }
        })
        .await
        .expect("peer repair activation timeout");
        let repaired = dispatch_haskell_script(
            implementation.policy.as_ref(),
            "respond (ReplyReport (sessionInput + sharedDelta))",
        )
        .await;
        assert_eq!(repaired["status"], "replied", "{repaired:?}");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::WatchChanged { notification })
                        if notification.owner == verification.actor.identity()
                            && notification.transition
                                == tidepool_actor::WatchTransition::Ready =>
                    {
                        break
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before peer review wake"),
                }
            }
        })
        .await
        .expect("peer review wake timeout");
        let peer_result = dispatch_haskell_script(
            verification.policy.as_ref(),
            "inspectFull <$> pollWatch repairReady",
        )
        .await;
        assert_eq!(peer_result["status"], "committed", "{peer_result:?}");
        assert!(
            peer_result["items"][0]["output"]
                .as_str()
                .is_some_and(|output| output.contains("ReplyReport 11")),
            "{peer_result:?}"
        );
        let verification_reply = dispatch_haskell_script(
            verification.policy.as_ref(),
            "respond (EchoReport sessionInput)",
        )
        .await;
        assert_eq!(
            verification_reply["status"], "replied",
            "{verification_reply:?}"
        );
        let nested_notification = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::WatchChanged { notification })
                        if notification.owner == scaffold_installation.actor.identity() =>
                    {
                        break notification;
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before nested watch wake"),
                }
            }
        })
        .await
        .expect("nested watch wake timeout");
        assert_eq!(
            nested_notification.transition,
            tidepool_actor::WatchTransition::Ready
        );

        let folded = dispatch_haskell_script(
            scaffold_installation.policy.as_ref(),
            ":{\nevidenceOf :: ResponseResult a -> (WorktreeReceipt, GitOid)\nevidenceOf result = case responseWorktree result of { WorktreeObserved receipt _ submission -> (receipt, case submittedHead submission of { OnBranch _ oid -> oid; Detached oid -> oid }); _ -> error \"expected worktree evidence\" }\nmergeObserved :: WorktreeHandle -> Text -> ResponseResult a -> Eff ScaffoldActorEffects (Either WorktreeError MergeOutcome)\nmergeObserved target message result = let (receipt, source) = evidenceOf result in tryMerge MergeRequest { mergeSourceHead = source, mergeSourceBranch = Just (branch receipt), mergeTargetWorktree = worktreeId target, mergeMessage = message }\n:}\nnestedObserved <- pollWatch nestedReady\nlet nestedResults = case nestedObserved of { WatchReady values -> values; _ -> error \"expected ready nested watch\" }\ntargetResult <- boundWorktree\nlet targetTree = case targetResult of { Right value -> value; Left _ -> error \"expected bound scaffold tree\" }\nmergeImplementation <- mergeObserved targetTree \"merge nested implementation\" (fst nestedResults)\nmergeVerification <- mergeObserved targetTree \"merge nested verification\" (snd nestedResults)\nrespond (ScaffoldReport \"folded\")",
        )
        .await;
        assert_eq!(folded["status"], "replied", "{folded:?}");
        assert!(scaffold_tree.cwd().join("implementation.txt").is_file());
        assert!(scaffold_tree.cwd().join("verification.txt").is_file());

        let scaffold_notification = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::WatchChanged { notification })
                        if notification.owner == actor.identity() =>
                    {
                        break notification
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before scaffold watch wake"),
                }
            }
        })
        .await
        .expect("scaffold watch wake timeout");
        assert_eq!(
            scaffold_notification.transition,
            tidepool_actor::WatchTransition::Ready
        );
        let scaffold_result = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "inspectFull <$> pollWatch scaffoldReadiness",
        )
        .await;
        assert!(scaffold_result["items"][0]["output"]
            .as_str()
            .is_some_and(|output| {
                output.contains("ReplyAvailable") && output.contains("ScaffoldReport \"folded\"")
            }));

        for installation in child_installations
            .iter()
            .chain(nested_installations.iter())
        {
            installation
                .runtime_observation
                .publish_provider_observation(tidepool_agent::ProviderObservation {
                    turn: Some(tidepool_agent::ProviderTurnObservation {
                        thread: format!("test-{}", installation.actor.identity().id.0),
                        turn: "completed".into(),
                        revision: 1,
                        state: tidepool_agent::ProviderTurnState::Succeeded,
                    }),
                    ..Default::default()
                });
        }
        let cleanup_doc = include_str!("../../prompts/shoal/docs/cleanup.md");
        let cleanup_examples = cleanup_doc
            .split("```haskell\n")
            .skip(1)
            .map(|section| section.split_once("```").unwrap().0)
            .collect::<Vec<_>>();
        let cleanup_plan = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            &format!("let oneWorker = first3 workers\n{}", cleanup_examples[0]),
        )
        .await;
        assert_eq!(cleanup_plan["status"], "committed", "{cleanup_plan:?}");
        assert!(
            cleanup_plan["items"][2]["output"]
                .as_str()
                .is_some_and(|output| output.contains("cleanupPlanRefusal = Nothing")),
            "{cleanup_plan:?}"
        );

        let stale_runtime = &child_installations[0].runtime_observation;
        let confirmed_turn = stale_runtime.snapshot().provider_turn;
        stale_runtime.mark_provider_observation_stale();
        let blocked = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "executeCleanup cleanupPlan",
        )
        .await;
        assert_eq!(blocked["status"], "committed", "{blocked:?}");
        let blocked_output = blocked["items"][0]["output"].as_str().unwrap();
        assert!(blocked_output.contains("CleanupBlocked"), "{blocked:?}");
        assert!(!blocked_output.contains("CleanupForgot"), "{blocked:?}");
        stale_runtime.publish_provider_observation(tidepool_agent::ProviderObservation {
            turn: confirmed_turn,
            ..Default::default()
        });

        let cleanup = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            &format!("{}\ncleanupReceipt", cleanup_examples[1]),
        )
        .await;
        assert_eq!(cleanup["status"], "committed", "{cleanup:?}");
        assert!(
            cleanup["items"][1]["output"]
                .as_str()
                .is_some_and(|output| {
                    output.contains("cleanupReceiptComplete = True")
                        && output.contains("CleanupGroupRetired")
                }),
            "{cleanup:?}"
        );

        let cleanup_retry = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "executeCleanup cleanupPlan",
        )
        .await;
        assert_eq!(cleanup_retry["status"], "committed", "{cleanup_retry:?}");
        assert!(cleanup_retry["items"][0]["output"]
            .as_str()
            .is_some_and(|output| output.contains("cleanupReceiptComplete = True")));

        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "test complete".into(),
            })
            .await
            .expect("shutdown root");
        hosted.await.expect("root actor task");
    }
}
