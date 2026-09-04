//! Composition root for the first actor-native interactive swarm.
//!
//! The daemon owns resident Haskell scheduling and exact actor lifecycle. One
//! stock interactive agent is attached to each installed Haskell tool policy;
//! tmux is process ownership and observability, never message transport.

mod prompt_catalog;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use frunk::{hlist, HCons, HNil};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tidepool_actor::{
    spawn_resident_root, ActorDescriptor, ActorEffectProfile, ActorExitKind, ActorPlacement,
    ActorRef, ActorTerminal, ActorWorkbenchSource, ExternalApplicationFailure,
    ExternalApplicationFailureClass, ExternalFailureDisposition, LocalActorRef,
    LocalResidentDeployment, LocalResidentInstallation, ResidentActorRoot,
};
use tidepool_agent::{
    native_interactive_backend, read_interactive_binding, BackendThreadId, InteractiveAgentBackend,
    InteractiveAgentInstallation, InteractiveAgentSpec, InteractiveLaunchMode,
    InteractiveNativeSandbox, InteractiveNativeToolPolicy, InteractivePolicyMount,
    QueueReadyThread, ReasoningEffort,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_handlers::{ActorWorktreeAuthority, ActorWorktreeHandler, WorktreeHandler};
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
    ActiveBinding, AgentRef as WorktreePrincipal, BindingTable, BindingTerminal, GitCli,
    WorktreeHandle, WorktreeId, WorktreeManager, WorktreeRegistry,
};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

use self::prompt_catalog::PromptId;

/// Every interactive actor sees its own repository at this path. Bubblewrap
/// mount namespaces make the shared name safe across concurrent actors, while
/// Codex needs only one persisted project-trust decision.
pub(crate) const ACTOR_PROJECT_ROOT: &str = "/tmp/tidepool-actor-workspace";

const DRIVER_MODULE: &str = "Tidepool.Actors.Internal.ShoalDriver";
const WORKBENCH_SURFACE_MODULE: &str = "Tidepool.Actors.Shoal";
const DRIVER_ENTRY: &str = "rootDriver";
const DRIVER_EFFECTS: &str = "RootEffects";
const CHILD_LIFECYCLE_NOTICE: &str = "A child actor changed lifecycle state.";
const APPLICATION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
const APPLICATION_TASK_GRACE_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

type ShoalHandlerStack = HCons<ActorWorktreeHandler, HNil>;
type ShoalRoot = ResidentActorRoot<ShoalHandlerStack, CapturedOutput>;

#[derive(Clone)]
pub struct ActorHostConfig {
    pub workspace: PathBuf,
    pub haskell_root: PathBuf,
    pub run_root: PathBuf,
    pub root_binding_path: PathBuf,
    pub interactive_agent: InteractiveAgentInstallation,
    pub tmux_session: String,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub root_launch_mode: InteractiveLaunchMode,
    pub pane_environment: std::collections::BTreeMap<String, String>,
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
    actor: ActorRef,
    local_actor: LocalActorRef,
    pane: TmuxPaneId,
    workspace: PathBuf,
    inbox: Arc<DurableInbox<DurableActorEvent>>,
    connection: InteractiveConnection,
    service: tokio::task::JoinHandle<Result<(), InteractiveApplicationError>>,
    socket_root: PathBuf,
    worktree_binding: Option<ActiveBinding>,
    failure_reported: bool,
    last_activation_sequence: u64,
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
    path: PathBuf,
    expected: Option<BackendThreadId>,
}

struct LaunchedInteractiveApplication {
    deployment: InteractiveDeployment,
    binding: InteractiveBindingRequest,
}

struct OwnerNotification {
    inbox: Arc<DurableInbox<DurableActorEvent>>,
    event: DurableActorEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum DurableActorEvent {
    Typed(TypedActorEvent),
    Legacy(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum TypedActorEvent {
    SessionReady {
        sequence: u64,
        request: tidepool_actor::RequestId,
        input_type: String,
        message: String,
    },
    WatchChanged {
        watch: tidepool_actor::WatchId,
        transition: tidepool_actor::WatchTransition,
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

    fn render(&self) -> String {
        match self {
            Self::Typed(TypedActorEvent::SessionReady { message, .. }) | Self::Legacy(message) => {
                message.clone()
            }
            Self::Typed(TypedActorEvent::WatchChanged { watch, transition }) => format!(
                "Typed watch {} changed to {transition:?}. Inspect it with `pollWatch`; the handle is authoritative.",
                watch.0,
            ),
            Self::Typed(TypedActorEvent::ChildExited) => CHILD_LIFECYCLE_NOTICE.into(),
        }
    }
}

struct PendingInteractiveLaunch {
    cancel: oneshot::Sender<()>,
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
    BuildPolicy,
    ServeToolHost,
    LaunchProcess,
    DiscoverBinding,
    StopProcess,
}

impl fmt::Display for InteractiveOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::BindWorktree => "bind actor worktree",
            Self::PrepareRuntime => "prepare runtime",
            Self::BindToolHost => "bind tool host",
            Self::BuildCommand => "build agent command",
            Self::BuildPolicy => "build resident tool policy",
            Self::ServeToolHost => "serve host dynamic tools",
            Self::LaunchProcess => "launch agent process",
            Self::DiscoverBinding => "discover conversation binding",
            Self::StopProcess => "stop agent process",
        };
        formatter.write_str(name)
    }
}

impl InteractiveOperation {
    fn failure_class(self) -> ExternalApplicationFailureClass {
        match self {
            Self::BindWorktree => ExternalApplicationFailureClass::WorktreeBinding,
            Self::BuildCommand | Self::BuildPolicy => {
                ExternalApplicationFailureClass::CommandConstruction
            }
            Self::LaunchProcess => ExternalApplicationFailureClass::ProcessLaunch,
            Self::BindToolHost
            | Self::ServeToolHost
            | Self::DiscoverBinding
            | Self::PrepareRuntime => ExternalApplicationFailureClass::ToolHostStartup,
            Self::StopProcess => ExternalApplicationFailureClass::UnexpectedExit,
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
}

#[derive(Clone)]
struct InteractiveLaunchContext {
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

    let (worktrees, bindings) = actor_worktree_resources(&config.workspace)?;
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
    let mut recovery = 0_u64;

    loop {
        let session_root = if recovery == 0 {
            run_root.clone()
        } else {
            run_root.join(format!("root-recovery-{recovery}"))
        };
        let (source, root) = compile_root(
            &config,
            &session_root,
            worktrees.clone(),
            worktree_authority.clone(),
        )?;
        let (root_actor, mut root_task, deployments) = spawn_resident_root(source, root).await?;
        worktree_authority.install_root(root_actor.identity().into());

        let (shutdown, shutdown_rx) = watch::channel(false);
        let mut applications_task = tokio::spawn(run_interactive_applications(
            deployments,
            InteractiveFleet {
                root: root_actor.clone(),
                config: config.clone(),
                run_root: run_root.clone(),
                tmux: tmux.clone(),
                backend: Arc::clone(&backend),
                worktrees: worktrees.clone(),
                bindings: Arc::clone(&bindings),
                readiness: readiness.clone(),
            },
            shutdown_rx,
        ));

        enum FirstStop {
            Signal,
            Root,
            Applications(Result<(), String>),
        }
        let first = tokio::select! {
            signal = operator_shutdown() => {
                signal?;
                FirstStop::Signal
            }
            result = &mut root_task => {
                result.map_err(join_error)?;
                FirstStop::Root
            },
            result = &mut applications_task => FirstStop::Applications(result.map_err(join_error)?),
        };
        shutdown.send_replace(true);

        match first {
            FirstStop::Signal => {
                shutdown_root(
                    &root_actor,
                    &mut root_task,
                    ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: "Shoal operator requested shutdown".into(),
                    },
                )
                .await?;
                await_applications(&mut applications_task).await?;
                return Ok(());
            }
            FirstStop::Root => {
                await_applications(&mut applications_task).await?;
                let terminal = root_actor.terminal().get().ok_or_else(|| {
                    runtime_error("Shoal root stopped without publishing a terminal result")
                })?;
                if matches!(
                    prepare_root_recovery(
                        &mut config,
                        root_actor.identity(),
                        terminal,
                        &mut recovery,
                    )
                    .await?,
                    RootRunDisposition::Complete
                ) {
                    return Ok(());
                }
            }
            FirstStop::Applications(result) => {
                let terminal = root_actor.terminal().get();
                let application_error = result.err().map(runtime_error);
                let shutdown_result = shutdown_root(
                    &root_actor,
                    &mut root_task,
                    ActorTerminal {
                        kind: ActorExitKind::Failed,
                        summary: "Shoal interactive application fleet stopped".into(),
                    },
                )
                .await;
                match (application_error, shutdown_result) {
                    (Some(error), Ok(())) => return Err(error),
                    (Some(error), Err(shutdown)) => {
                        return Err(runtime_error(format!("{error}; cleanup: {shutdown}")))
                    }
                    (None, Err(error)) => return Err(error),
                    (None, Ok(())) => {
                        let terminal = terminal.ok_or_else(|| {
                            runtime_error(
                                "Shoal interactive application fleet stopped while the root was live",
                            )
                        })?;
                        if matches!(
                            prepare_root_recovery(
                                &mut config,
                                root_actor.identity(),
                                terminal,
                                &mut recovery,
                            )
                            .await?,
                            RootRunDisposition::Complete
                        ) {
                            return Ok(());
                        }
                    }
                }
            }
        }
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
    if terminal.kind == ActorExitKind::Completed {
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

async fn shutdown_root(
    root: &LocalActorRef,
    task: &mut tokio::task::JoinHandle<()>,
    terminal: ActorTerminal,
) -> Result<(), Box<dyn std::error::Error>> {
    match tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, root.shutdown(terminal)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(
                actor = ?root.identity(),
                %error,
                "Shoal root shutdown failed; killing the exact actor"
            );
            root.address().kill();
        }
        Err(_) => {
            tracing::warn!(
                actor = ?root.identity(),
                "Shoal root did not acknowledge shutdown; killing the exact actor"
            );
            root.address().kill();
        }
    }
    match tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, &mut *task).await {
        Ok(result) => result.map_err(join_error),
        Err(_) => {
            task.abort();
            Err(runtime_error(format!(
                "Shoal root did not stop within {APPLICATION_SHUTDOWN_TIMEOUT:?}"
            )))
        }
    }
}

async fn await_applications(
    task: &mut tokio::task::JoinHandle<Result<(), String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    match tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, &mut *task).await {
        Ok(result) => result.map_err(join_error)?.map_err(runtime_error),
        Err(_) => {
            task.abort();
            Err(runtime_error(format!(
                "interactive fleet did not stop within {APPLICATION_SHUTDOWN_TIMEOUT:?}"
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
        BindingTable::open(root.join("bindings"))?,
    ))
}

fn runtime_namespace(run_root: &Path) -> String {
    run_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-runtime")
        .to_owned()
}

fn compile_root(
    config: &ActorHostConfig,
    run_root: &Path,
    worktrees: WorktreeManager,
    worktree_authority: ActorWorktreeAuthority,
) -> Result<(ActorWorkbenchSource, ShoalRoot), Box<dyn std::error::Error>> {
    let declarations = [
        tidepool_mcp::agent_session_decl(),
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::fs_read_decl(),
        tidepool_mcp::worktree_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&declarations)?;
    let mut include = effects.include_paths().to_vec();
    include.push(config.haskell_root.clone());
    include.push(crate::haskell_sources::ensure_stdlib()?);
    let preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble_with_companions(
            &declarations,
            false,
            tidepool_mcp::CompanionImports::Omit,
        ),
        DRIVER_MODULE,
    );
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

    let session = fresh_session_id();
    let library = SessionLib::open(
        session,
        &session_root,
        tidepool_mcp::session_decl_module_env_with_companions(
            &declarations,
            false,
            tidepool_mcp::CompanionImports::Omit,
        ),
    )?
    .with_validation_include(include.clone());
    let mut machine = ResidentSession::bootstrap(
        &compiled.expr,
        compiled.table.clone(),
        hlist![ActorWorktreeHandler::new(
            WorktreeHandler::from_manager(worktrees),
            worktree_authority,
        )],
        CapturedOutput::new(),
        include.clone(),
        DEFAULT_NURSERY_SIZE,
        Some(library),
    )?;
    machine.set_effect_execution(
        EffectRunPolicy::HandleOrSuspend,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    );
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
            lexical_scope: ScopeId::ROOT,
        },
    )
    // Profiles classify resident Haskell rows, not the native Codex sandbox.
    // The root allocates worktrees and may attenuate children to ReadOnly.
    .with_profile(ActorEffectProfile::ReadWrite);
    Ok((
        ActorWorkbenchSource::new(preamble, include)
            .with_default_browse_module(WORKBENCH_SURFACE_MODULE),
        ResidentActorRoot::new(descriptor, machine, outcome),
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

async fn run_interactive_applications(
    mut lifecycle: mpsc::UnboundedReceiver<LocalResidentDeployment>,
    fleet: InteractiveFleet,
    shutdown: watch::Receiver<bool>,
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
    } = fleet;
    let root_identity = root.identity();
    let launch_context = InteractiveLaunchContext {
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
    let mut pending_launches = HashMap::new();
    let mut retirements = JoinSet::new();
    let mut notifications = JoinSet::new();
    let mut health = tokio::time::interval(Duration::from_secs(1));
    let failure = loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown.clone()) => break None,
            _ = health.tick() => {
                if let Some(index) = deployments.iter().position(|deployment| {
                    !deployment.failure_reported
                        && (deployment.service.is_finished()
                            || matches!(
                                &deployment.connection,
                                InteractiveConnection::Bound { delivery, .. }
                                    if delivery.is_finished()
                            ))
                }) {
                    let actor = deployments[index].actor;
                    let local_actor = deployments[index].local_actor.clone();
                    deployments[index].failure_reported = true;
                    let detail = "interactive application exited before actor settlement".to_string();
                    if actor == root_identity {
                        break Some(format!("root {actor:?}: {detail}"));
                    }
                    let result = local_actor
                        .report_external_failure(ExternalApplicationFailure {
                            class: ExternalApplicationFailureClass::UnexpectedExit,
                            detail,
                        })
                        .await;
                    match result {
                        Ok(ExternalFailureDisposition::Applied | ExternalFailureDisposition::AlreadyTerminal) => {}
                        Ok(ExternalFailureDisposition::UnknownOrStale) => {
                            break Some(format!("resident host rejected the exact deployed actor {actor:?} as unknown or stale"));
                        }
                        Err(error) => {
                            break Some(format!("report child application failure for {actor:?}: {error}"));
                        }
                    }
                }
            }
            event = lifecycle.recv() => {
                let Some(event) = event else { break None };
                match event {
                    LocalResidentDeployment::PolicyInstalled(installation) => {
                        let context = launch_context.clone();
                        let actor = installation.actor.identity();
                        let (cancel, cancelled) = oneshot::channel();
                        let previous = pending_launches.insert(
                            actor,
                            PendingInteractiveLaunch {
                                cancel,
                            },
                        );
                        debug_assert!(previous.is_none(), "one launch per exact actor incarnation");
                        launches.spawn(async move {
                            let local_actor = installation.actor.clone();
                            let result = launch_interactive_application(
                                installation,
                                context,
                                cancelled,
                            ).await;
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
                                break Some(error);
                            }
                            application.last_activation_sequence = sequence;
                        }
                    }
                    LocalResidentDeployment::Retired { actor, terminal } => {
                        let pending = pending_launches.remove(&actor);
                        if let Some(pending) = pending {
                            let _ = pending.cancel.send(());
                        }
                        if let Some(index) = deployments.iter().position(|app| app.actor == actor) {
                            let deployment = deployments.swap_remove(index);
                            let tmux = tmux.clone();
                            let bindings = Arc::clone(&bindings);
                            let binding_terminal = if terminal.kind == ActorExitKind::Completed {
                                BindingTerminal::Completed
                            } else {
                                BindingTerminal::Released
                            };
                            retirements.spawn(async move {
                                retire_interactive_application(
                                    deployment,
                                    &tmux,
                                    &bindings,
                                    binding_terminal,
                                ).await
                            });
                        }
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
                        notifications.spawn(publish_inbox_event(
                            Arc::clone(&application.inbox),
                            DurableActorEvent::Typed(TypedActorEvent::WatchChanged {
                                watch: notification.watch,
                                transition: notification.transition,
                            }),
                        ));
                    }
                }
            }
            launched = launches.join_next(), if !launches.is_empty() => {
                match launched {
                    Some(Ok((local_actor, Ok(Some(launched))))) => {
                        let actor = local_actor.identity();
                        pending_launches.remove(&actor);
                        let deployment = launched.deployment;
                        if actor == root_identity {
                            let _ = readiness.send(ActorHostReadiness::AwaitingBinding { root: root_identity });
                        }
                        let pane = deployment.pane.clone();
                        let tmux = tmux.clone();
                        binding_discoveries.spawn(async move {
                            let result = discover_interactive_binding(
                                actor,
                                launched.binding,
                                &tmux,
                                &pane,
                            )
                            .await;
                            (actor, result)
                        });
                        deployments.push(deployment);
                    }
                    Some(Ok((local_actor, Ok(None)))) => {
                        let actor = local_actor.identity();
                        pending_launches.remove(&actor);
                    }
                    Some(Ok((local_actor, Err(error)))) => {
                        let actor = local_actor.identity();
                        pending_launches.remove(&actor);
                        if actor == root_identity {
                            break Some(error.to_string());
                        }
                        let result = local_actor
                            .report_external_failure(ExternalApplicationFailure {
                                class: error.operation.failure_class(),
                                detail: error.detail,
                            })
                            .await;
                        match result {
                            Ok(ExternalFailureDisposition::Applied | ExternalFailureDisposition::AlreadyTerminal) => {}
                            Ok(ExternalFailureDisposition::UnknownOrStale) => {
                                break Some(format!("resident host rejected the exact launching actor {actor:?} as unknown or stale"));
                            }
                            Err(report_error) => {
                                break Some(format!(
                                    "report child launch failure for {actor:?}: {report_error}"
                                ));
                            }
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
                            stop_delivery,
                        ));
                        deployment.connection = InteractiveConnection::Bound {
                            delivery_shutdown,
                            delivery,
                        };
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
                        deployment.failure_reported = true;
                        if actor == root_identity {
                            break Some(error.to_string());
                        }
                        let Some(local_actor) = deployments
                            .iter()
                            .find(|application| application.actor == actor)
                            .map(|application| application.local_actor.clone())
                        else {
                            break Some(format!("lost exact local actor for failed application {actor:?}"));
                        };
                        let result = local_actor
                            .report_external_failure(ExternalApplicationFailure {
                                class: error.operation.failure_class(),
                                detail: error.detail,
                            })
                            .await;
                        match result {
                            Ok(ExternalFailureDisposition::Applied | ExternalFailureDisposition::AlreadyTerminal) => {}
                            Ok(ExternalFailureDisposition::UnknownOrStale) => {
                                break Some(format!("resident host rejected the exact binding actor {actor:?} as unknown or stale"));
                            }
                            Err(report_error) => {
                                break Some(format!("report child binding failure for {actor:?}: {report_error}"));
                            }
                        }
                    }
                    Some(Err(error)) => break Some(format!("interactive binding task: {error}")),
                    None => {}
                }
            }
            retired = retirements.join_next(), if !retirements.is_empty() => {
                match retired {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(error))) => break Some(error.to_string()),
                    Some(Err(error)) => break Some(format!("interactive retirement task: {error}")),
                    None => {}
                }
            }
            notified = notifications.join_next(), if !notifications.is_empty() => {
                match notified {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(error))) => break Some(error),
                    Some(Err(error)) => break Some(format!("owner notification task: {error}")),
                    None => {}
                }
            }
        }
    };

    for (_, pending) in pending_launches.drain() {
        let _ = pending.cancel.send(());
    }
    let launch_cleanup = tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        let mut completed = Vec::new();
        while let Some(result) = launches.join_next().await {
            if let Ok((_actor, Ok(Some(launched)))) = result {
                completed.push(launched.deployment);
            }
        }
        completed
    })
    .await;
    match launch_cleanup {
        Ok(completed) => deployments.extend(completed),
        Err(_) => launches.abort_all(),
    }
    binding_discoveries.abort_all();
    while binding_discoveries.join_next().await.is_some() {}
    for deployment in deployments {
        let tmux = tmux.clone();
        let bindings = Arc::clone(&bindings);
        retirements.spawn(async move {
            retire_interactive_application(deployment, &tmux, &bindings, BindingTerminal::Released)
                .await
        });
    }
    let notification_cleanup = tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        let mut failure = None;
        while let Some(result) = notifications.join_next().await {
            let result = result
                .map_err(|error| format!("owner notification task: {error}"))
                .and_then(|result| result);
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
            let result = result
                .map_err(|error| format!("interactive retirement task: {error}"))
                .and_then(|result| result.map_err(|error| error.to_string()));
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        failure
    })
    .await
    .unwrap_or_else(|_| Some("interactive application cleanup timed out".into()));
    let cleanup_failure = match (notification_cleanup, cleanup_failure) {
        (Some(notification), Some(retirement)) => Some(format!("{notification}; {retirement}")),
        (Some(error), None) | (None, Some(error)) => Some(error),
        (None, None) => None,
    };
    match (failure, cleanup_failure) {
        (Some(error), Some(cleanup)) => Err(format!("{error}; cleanup: {cleanup}")),
        (Some(error), None) | (None, Some(error)) => Err(error),
        (None, None) => Ok(()),
    }
}

async fn launch_interactive_application(
    installation: LocalResidentInstallation,
    context: InteractiveLaunchContext,
    cancelled: oneshot::Receiver<()>,
) -> Result<Option<LaunchedInteractiveApplication>, InteractiveApplicationError> {
    let actor = installation.actor.identity();
    let prepared = prepare_actor_worktree(&installation, &context)?;
    let worktree = prepared.as_ref().map(|(handle, _)| handle.clone());
    let result =
        launch_prepared_interactive_application(installation, context.clone(), worktree, cancelled)
            .await;
    match (result, prepared) {
        (Ok(Some(mut launched)), Some((_handle, binding))) => {
            launched.deployment.worktree_binding = Some(binding);
            Ok(Some(launched))
        }
        (Ok(Some(deployment)), None) => Ok(Some(deployment)),
        (Ok(None), Some((_handle, binding))) => {
            release_worktree_binding(&context.bindings, binding).map_err(|error| {
                application_error(actor, InteractiveOperation::BindWorktree, error)
            })?;
            Ok(None)
        }
        (Ok(None), None) => Ok(None),
        (Err(mut error), Some((_handle, binding))) => {
            if let Err(rollback) = release_worktree_binding(&context.bindings, binding) {
                error
                    .detail
                    .push_str(&format!("; binding rollback failed: {rollback}"));
            }
            Err(error)
        }
        (Err(error), None) => Err(error),
    }
}

fn prepare_actor_worktree(
    installation: &LocalResidentInstallation,
    context: &InteractiveLaunchContext,
) -> Result<Option<(WorktreeHandle, ActiveBinding)>, InteractiveApplicationError> {
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
    let binding = context
        .bindings
        .lock()
        .bind(handle.id(), &principal, current_time_ms())
        .map_err(|error| application_error(actor, InteractiveOperation::BindWorktree, error))?;
    Ok(Some((handle, binding)))
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

fn release_worktree_binding(
    bindings: &Arc<Mutex<BindingTable>>,
    binding: ActiveBinding,
) -> Result<(), tidepool_worktree::WorktreeError> {
    binding.release(&mut bindings.lock())
}

fn current_time_ms() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

async fn launch_prepared_interactive_application(
    installation: LocalResidentInstallation,
    context: InteractiveLaunchContext,
    worktree: Option<WorktreeHandle>,
    mut cancelled: oneshot::Receiver<()>,
) -> Result<Option<LaunchedInteractiveApplication>, InteractiveApplicationError> {
    let InteractiveLaunchContext {
        root,
        config,
        run_root,
        tmux,
        backend,
        worktrees,
        bindings: _,
    } = context;
    let actor = installation.actor;
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
        &config.workspace,
        worktree.as_ref().map(WorktreeHandle::cwd),
        &git_common_dir,
    );
    let agent_workspace = PathBuf::from(ACTOR_PROJECT_ROOT);
    let native_tool_policy = if actor_identity != root && worktree.is_none() {
        InteractiveNativeToolPolicy::InspectionOnly
    } else {
        InteractiveNativeToolPolicy::Standard
    };
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
    let build_output = if actor_identity == root || worktree.is_some() {
        let relative = PathBuf::from(".shoal").join("build").join(format!(
            "actor-{}-{}",
            actor_identity.id.0, actor_identity.incarnation.0
        ));
        std::fs::create_dir_all(workspace.join(&relative)).map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?;
        Some(agent_workspace.join(relative))
    } else {
        None
    };
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
    std::fs::create_dir(&socket_root).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_root, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| application_error(actor_identity, InteractiveOperation::PrepareRuntime, error),
        )?;
    }
    let endpoint = socket_root.join("host-tools.sock");
    let listener = UnixListener::bind(&endpoint).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::BindToolHost, error)
    })?;
    let binding_path = if actor_identity == root {
        config.root_binding_path.clone()
    } else {
        actor_root.join("binding.json")
    };
    let inbox = Arc::new(
        DurableInbox::<DurableActorEvent>::open(
            actor_root.join("inbox.jsonl"),
            actor_root.join("inbox.cursor"),
        )
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?,
    );
    let launch_mode = if actor_identity == root {
        config.root_launch_mode.clone()
    } else {
        InteractiveLaunchMode::Fresh
    };
    let expected_resume = match &launch_mode {
        InteractiveLaunchMode::Resume(thread) => Some(thread.clone()),
        InteractiveLaunchMode::Fresh | InteractiveLaunchMode::Fork(_) => None,
    };
    let developer_instructions =
        developer_instructions(actor_identity == root, worktree.is_some(), &launch_mode);
    let spec = InteractiveAgentSpec {
        mode: launch_mode,
        model: config.model.clone(),
        effort: config.effort,
        developer_instructions,
        initial_prompt: installation.initial_user_message.clone(),
        native_sandbox: InteractiveNativeSandbox::HostMountBoundary,
        host_tools_socket: endpoint,
    };
    let command = backend.render(&spec).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::BuildCommand, error)
    })?;
    let command = process_boundary.wrap(
        BUBBLEWRAP_PROGRAM,
        ProcessInvocation {
            program: command.program,
            args: command.args,
        },
    );
    let server = crate::host_dynamic_tools::HostDynamicToolService::new(
        installation.policy,
        binding_path.clone(),
        expected_resume.clone(),
    )
    .map_err(|error| application_error(actor_identity, InteractiveOperation::BuildPolicy, error))?;
    let service = tokio::spawn(async move {
        server.serve(listener).await.map_err(|error| {
            application_error(actor_identity, InteractiveOperation::ServeToolHost, error)
        })
    });
    if cancelled.try_recv().is_ok() {
        service.abort();
        let _ = service.await;
        let _ = std::fs::remove_dir_all(&socket_root);
        return Ok(None);
    }
    let launch_environment = actor_launch_environment(
        config.pane_environment.clone(),
        actor_identity == root,
        build_output.as_deref(),
    );
    let pane = match tokio::time::timeout(
        PROCESS_OPERATION_TIMEOUT,
        tmux.spawn_window(&TmuxLaunch {
            window_name: format!(
                "actor-{}-{}",
                actor_identity.id.0, actor_identity.incarnation.0
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
            service.abort();
            let _ = service.await;
            let _ = std::fs::remove_dir_all(&socket_root);
            return Err(application_error(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                error,
            ));
        }
        Err(_) => {
            service.abort();
            let _ = service.await;
            let _ = std::fs::remove_dir_all(&socket_root);
            return Err(application_error(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                format!("tmux launch exceeded {PROCESS_OPERATION_TIMEOUT:?}"),
            ));
        }
    };

    if actor_identity == root {
        if let Err(error) = tmux.select_window_for_pane(&pane).await {
            abandon_interactive_application(&tmux, &pane, service, &socket_root).await;
            return Err(application_error(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                error,
            ));
        }
    }
    if cancelled.try_recv().is_ok() {
        abandon_interactive_application(&tmux, &pane, service, &socket_root).await;
        return Ok(None);
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
    Ok(Some(LaunchedInteractiveApplication {
        deployment: InteractiveDeployment {
            actor: actor_identity,
            local_actor: actor,
            pane,
            workspace,
            inbox,
            connection: InteractiveConnection::AwaitingBinding,
            service,
            socket_root,
            worktree_binding: None,
            failure_reported: false,
            last_activation_sequence: 0,
        },
        binding: InteractiveBindingRequest {
            path: binding_path,
            expected: expected_resume,
        },
    }))
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
    }
    let unset = workspace_local_toolchain_pins(is_root);
    inherited.retain(|name, _| !unset.contains(name));
    ActorLaunchEnvironment {
        set: inherited,
        unset,
    }
}

async fn deliver_pending(
    actor: ActorRef,
    inbox: &Arc<DurableInbox<DurableActorEvent>>,
    thread: &QueueReadyThread,
    backend: &dyn InteractiveAgentBackend,
    workspace: &Path,
) -> Result<(), String> {
    let cwd = workspace.to_string_lossy();
    let pending_inbox = Arc::clone(inbox);
    let pending = tokio::task::spawn_blocking(move || pending_inbox.pending())
        .await
        .map_err(|error| format!("inbox reader task: {error}"))?
        .map_err(|error| error.to_string())?;
    for message in pending {
        let inbox_sequence = message.sequence;
        backend
            .push(&cwd, thread, &message.payload.render())
            .await
            .map_err(|error| error.to_string())?;
        let ack_inbox = Arc::clone(inbox);
        tokio::task::spawn_blocking(move || ack_inbox.acknowledge(message.sequence))
            .await
            .map_err(|error| format!("inbox acknowledgement task: {error}"))?
            .map_err(|error| error.to_string())?;
        tracing::info!(
            actor = ?actor,
            inbox_sequence,
            "actor activation delivered"
        );
    }
    Ok(())
}

async fn run_delivery_pump(
    actor: ActorRef,
    inbox: Arc<DurableInbox<DurableActorEvent>>,
    thread: QueueReadyThread,
    backend: Arc<dyn InteractiveAgentBackend>,
    workspace: PathBuf,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut health = tokio::time::interval(Duration::from_secs(1));
    let mut last_error = None;
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = health.tick() => {
                let result = deliver_pending(actor, &inbox, &thread, backend.as_ref(), &workspace).await;
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
        }
    }
}

fn prepare_owner_notification(
    notice: &tidepool_actor::ChildExitNotice,
    deployments: &[InteractiveDeployment],
) -> Option<OwnerNotification> {
    let owner_application = deployments.iter().find(|app| app.actor == notice.owner)?;
    Some(OwnerNotification {
        inbox: Arc::clone(&owner_application.inbox),
        event: DurableActorEvent::Typed(TypedActorEvent::ChildExited),
    })
}

async fn publish_owner_notification(notification: OwnerNotification) -> Result<(), String> {
    publish_inbox_event(notification.inbox, notification.event).await
}

async fn publish_inbox_event(
    inbox: Arc<DurableInbox<DurableActorEvent>>,
    event: DurableActorEvent,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || inbox.publish(event))
        .await
        .map_err(|error| format!("actor inbox publisher task: {error}"))?
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn retire_interactive_application(
    mut deployment: InteractiveDeployment,
    tmux: &TmuxSession,
    bindings: &Arc<Mutex<BindingTable>>,
    binding_terminal: BindingTerminal,
) -> Result<(), InteractiveApplicationError> {
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
    let stop_error =
        tmux.kill_pane(&deployment.pane).await.err().map(|error| {
            application_error(deployment.actor, InteractiveOperation::StopProcess, error)
        });
    tokio::join!(
        stop_retired_tool_service(deployment.actor, &mut deployment.service,),
        async {
            if let Some(delivery) = delivery.as_mut() {
                stop_retired_delivery(deployment.actor, delivery, APPLICATION_TASK_GRACE_TIMEOUT)
                    .await;
            }
        },
    );
    let _ = std::fs::remove_dir_all(&deployment.socket_root);
    let binding_error = if let Some(binding) = deployment.worktree_binding.take() {
        let result = match binding_terminal {
            BindingTerminal::Completed => binding.complete(&mut bindings.lock()),
            BindingTerminal::Released => binding.release(&mut bindings.lock()),
        };
        result.err().map(|error| {
            application_error(deployment.actor, InteractiveOperation::BindWorktree, error)
        })
    } else {
        None
    };
    match (stop_error, binding_error) {
        (Some(mut stop), Some(binding)) => {
            stop.detail
                .push_str(&format!("; binding settlement failed: {binding}"));
            Err(stop)
        }
        (Some(error), None) | (None, Some(error)) => Err(error),
        (None, None) => {
            tracing::info!(
                actor = ?deployment.actor,
                terminal = ?binding_terminal,
                "interactive application retired"
            );
            Ok(())
        }
    }
}

/// Stop the actor-lifetime host-tools listener after its application pane is gone.
async fn stop_retired_tool_service(
    actor: ActorRef,
    service: &mut tokio::task::JoinHandle<Result<(), InteractiveApplicationError>>,
) {
    service.abort();
    if let Err(error) = service.await {
        if !error.is_cancelled() {
            tracing::warn!(actor = ?actor, %error, "retired actor tool service task failed");
        }
    }
}

async fn stop_retired_delivery(
    actor: ActorRef,
    delivery: &mut tokio::task::JoinHandle<()>,
    grace: Duration,
) {
    match tokio::time::timeout(grace, &mut *delivery).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(actor = ?actor, %error, "retired actor inbox task failed");
        }
        Err(_) => {
            tracing::debug!(actor = ?actor, "forcing retired actor inbox task to stop");
            delivery.abort();
            let _ = delivery.await;
        }
    }
}

async fn abandon_interactive_application(
    tmux: &TmuxSession,
    pane: &TmuxPaneId,
    service: tokio::task::JoinHandle<Result<(), InteractiveApplicationError>>,
    socket_root: &Path,
) {
    let _ = tmux.kill_pane(pane).await;
    service.abort();
    let _ = service.await;
    let _ = std::fs::remove_dir_all(socket_root);
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
                let panes = tmux.list_panes().await.map_err(|error| {
                    application_error(actor, InteractiveOperation::DiscoverBinding, error)
                })?;
                if !panes.contains(pane) {
                    return Err(application_error(
                        actor,
                        InteractiveOperation::DiscoverBinding,
                        "interactive application exited before conversation binding",
                    ));
                }
            }
        }
    }
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }
    while shutdown.changed().await.is_ok() {
        if *shutdown.borrow() {
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

fn developer_instructions(root: bool, owns_worktree: bool, mode: &InteractiveLaunchMode) -> String {
    if root {
        let mut instructions = PromptId::ShoalRoot.body().to_string();
        if matches!(mode, InteractiveLaunchMode::Resume(_)) {
            instructions.push_str(PromptId::RecreatedRoot.body());
        }
        instructions
    } else if owns_worktree {
        PromptId::WorktreeAgent.body().into()
    } else {
        PromptId::ReadonlyAgent.body().into()
    }
}

fn writable_repository_roots(
    root: bool,
    source: &Path,
    worker_worktree: Option<&Path>,
    git_common_dir: &Path,
) -> Vec<PathBuf> {
    let mut writable = if root {
        // Integration advances the source HEAD; child coding happens only in
        // the exact linked worktree granted to that child.
        vec![source.to_path_buf()]
    } else {
        worker_worktree.map(Path::to_path_buf).into_iter().collect()
    };
    // Linked worktrees intentionally share objects, refs, config, and
    // per-worktree administrative state. Making the resolved common dir
    // writable is what lets native Git behave normally inside every actor.
    writable.push(git_common_dir.to_path_buf());
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
    use super::*;
    use tidepool_agent::{
        AgentBackendError, InteractiveAgentCommand, InteractiveAgentSpec, InteractiveFuture,
    };
    use tidepool_testing::eval_harness;
    use tidepool_tool::{ToolArguments, ToolInvocation};
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
            let result = endpoint
                .dispatch_boxed(ToolInvocation {
                    context: None,
                    name: tidepool_actor::HASKELL_TOOL.into(),
                    arguments: ToolArguments::Raw(item.into()),
                })
                .await
                .unwrap_or_else(|error| {
                    panic!("Haskell item failed:\n{item}\n\n{error}\n\nprevious receipt: {last:?}")
                });
            last = Some(result);
        }
        last.expect("non-empty Haskell fixture")
    }

    async fn dispatch_haskell_script(
        endpoint: &dyn tidepool_actor::ResidentToolEndpoint,
        script: &str,
    ) -> serde_json::Value {
        endpoint
            .dispatch_boxed(ToolInvocation {
                context: None,
                name: tidepool_actor::HASKELL_TOOL.into(),
                arguments: ToolArguments::Raw(script.into()),
            })
            .await
            .unwrap_or_else(|error| panic!("Haskell script failed:\n{script}\n\n{error}"))
    }

    #[tokio::test]
    async fn idle_application_waits_for_its_queue_ready_binding() {
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
        let actor = ActorRef::first(tidepool_actor::ActorId(1));
        let binding = discover_interactive_binding(
            actor,
            InteractiveBindingRequest {
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
        let thread = BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into());
        tidepool_agent::accept_interactive_session_binding(
            &path,
            tidepool_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
            thread.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), &mut binding)
                .await
                .unwrap()
                .unwrap()
                .id(),
            &thread
        );
        session.kill().await.unwrap();
    }

    #[test]
    fn root_instructions_preserve_idle_and_resume_contracts() {
        let fresh = developer_instructions(true, false, &InteractiveLaunchMode::Fresh);
        let resumed = developer_instructions(
            true,
            false,
            &InteractiveLaunchMode::Resume(BackendThreadId("retained-thread".into())),
        );
        assert_eq!(fresh, PromptId::ShoalRoot.body());
        assert_eq!(
            resumed,
            format!(
                "{}{}",
                PromptId::ShoalRoot.body(),
                PromptId::RecreatedRoot.body()
            )
        );
        assert_eq!(resumed.matches(PromptId::RecreatedRoot.body()).count(), 1);
        let resumed = normalized_prompt(&resumed);
        assert!(resumed.contains("Previous actor handles"));
        assert!(resumed.contains("were not restored"));
        assert!(resumed.contains("not a prewritten actor program"));
        assert!(resumed.contains("Start agents once, submit independent requests before waiting"));
        assert!(resumed.contains("Project-specific worker ledgers"));
        assert!(resumed.contains("compact GHCi-style transcripts"));
        assert!(resumed.contains("typed Haskell state carries identities"));
        assert!(resumed.contains("Use `:status`"));
        assert!(resumed.contains("no completion, yield, or park operation"));
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

        let instructions = developer_instructions(false, false, &InteractiveLaunchMode::Fresh);
        assert_eq!(instructions, PromptId::ReadonlyAgent.body());
        let normalized = normalized_prompt(&instructions);
        assert!(normalized.contains("read-only access to the shared source checkout"));
        assert!(normalized.contains("Do not run builds, tests, formatters"));
        assert!(normalized.contains("actor with an owned coding worktree"));
        assert!(normalized.contains("orchestrate children"));

        let worker = developer_instructions(false, true, &InteractiveLaunchMode::Fresh);
        assert_eq!(worker, PromptId::WorktreeAgent.body());
        let normalized = normalized_prompt(&worker);
        assert!(normalized.contains("Inspect `:type respond`"));
        assert!(normalized.contains("irreversible terminal transfer"));
        assert!(normalized.contains("not actor termination"));
    }

    #[test]
    fn root_and_worker_share_git_metadata_but_not_working_tree_authority() {
        let source = Path::new("/source");
        let worker = Path::new("/workers/one");
        let common = Path::new("/source/.git");

        assert_eq!(
            writable_repository_roots(true, source, None, common),
            vec![source.to_path_buf(), common.to_path_buf()]
        );
        assert_eq!(
            writable_repository_roots(false, source, Some(worker), common),
            vec![worker.to_path_buf(), common.to_path_buf()]
        );
        assert_eq!(
            writable_repository_roots(false, source, None, common),
            vec![common.to_path_buf()]
        );
    }

    #[test]
    fn worker_launch_unsets_source_checkout_extractor_pins() {
        let launch = actor_launch_environment(
            BTreeMap::from([
                ("PATH".into(), "/bin".into()),
                ("TIDEPOOL_EXTRACT".into(), "/source/tidepool-extract".into()),
                (
                    "TIDEPOOL_EXTRACT_WORKER".into(),
                    "/source/tidepool-extract-worker".into(),
                ),
            ]),
            false,
            Some(Path::new(
                "/tmp/tidepool-actor-workspace/.shoal/build/actor-2-1",
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
            Some("/tmp/tidepool-actor-workspace/.shoal/build/actor-2-1")
        );
        assert_eq!(launch.set.get("PATH").map(String::as_str), Some("/bin"));
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

        let watch = DurableActorEvent::Typed(TypedActorEvent::WatchChanged {
            watch: tidepool_actor::WatchId(9),
            transition: tidepool_actor::WatchTransition::Ready,
        });
        let encoded = serde_json::to_value(&watch).expect("serialize typed watch event");
        assert_eq!(encoded["type"], "watchChanged");
        assert_eq!(encoded["watch"], 9);
        assert_eq!(
            serde_json::from_value::<DurableActorEvent>(serde_json::json!("old notice"))
                .expect("decode legacy actor event"),
            DurableActorEvent::Legacy("old notice".into())
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
    async fn native_push_acknowledges_only_after_acceptance_and_retries_the_same_row() {
        let root = tempfile::tempdir().expect("inbox root");
        let inbox = Arc::new(
            DurableInbox::open(root.path().join("rows"), root.path().join("cursor"))
                .expect("open inbox"),
        );
        inbox
            .publish(DurableActorEvent::Legacy("child completed".into()))
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
        )
        .await
        .unwrap();
        let thread = tidepool_agent::read_interactive_binding(&binding_path)
            .await
            .unwrap();
        let actor = ActorRef::first(tidepool_actor::ActorId(7));

        assert!(
            deliver_pending(actor, &inbox, &thread, &backend, root.path())
                .await
                .is_err()
        );
        assert_eq!(inbox.pending().expect("pending after refusal").len(), 1);

        backend
            .fail
            .store(false, std::sync::atomic::Ordering::SeqCst);
        deliver_pending(actor, &inbox, &thread, &backend, root.path())
            .await
            .expect("retry accepted");
        assert!(inbox.pending().expect("acked inbox").is_empty());
        assert_eq!(
            *backend.messages.lock().unwrap(),
            ["child completed", "child completed"]
        );
    }

    #[tokio::test]
    async fn retired_actor_tasks_are_forced_closed_without_becoming_actor_failures() {
        let actor = ActorRef::first(tidepool_actor::ActorId(7));
        let mut service = tokio::spawn(async {
            std::future::pending::<()>().await;
            Ok::<(), InteractiveApplicationError>(())
        });
        let mut delivery = tokio::spawn(std::future::pending::<()>());

        tokio::join!(
            stop_retired_tool_service(actor, &mut service),
            stop_retired_delivery(actor, &mut delivery, Duration::ZERO),
        );

        assert!(service.is_finished());
        assert!(delivery.is_finished());
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
    #[cfg(any())]
    #[ignore = "high-churn completion-era end-to-end fixture; replace with focused persistent-application request/watch scenarios before re-enabling"]
    async fn shoal_exposes_generic_haskell_actor_composition() {
        fn fixture_items(source: &'static str) -> Vec<&'static str> {
            source
                .split("\n-- TIDEPOOL-ITEM --\n")
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .collect()
        }

        eval_harness::require_extract();
        let repository = tidepool_worktree::testing::TestRepo::init().unwrap();
        repository
            .writer()
            .commit_file("README.md", "source\n", "seed")
            .unwrap();
        let workspace = repository.path().to_path_buf();
        let runtime = tempfile::tempdir().unwrap();
        let config = ActorHostConfig {
            haskell_root: crate::haskell_sources::ensure_shoal_haskell().unwrap(),
            workspace: workspace.clone(),
            run_root: runtime.path().join("run"),
            root_binding_path: runtime.path().join("root-binding.json"),
            interactive_agent: tidepool_agent::native_interactive_agent_from_parts(
                std::env::current_exe().unwrap(),
                "test installation".into(),
            )
            .unwrap(),
            tmux_session: "unused-in-compile-test".into(),
            model: None,
            effort: None,
            root_launch_mode: InteractiveLaunchMode::Fresh,
            pane_environment: std::collections::BTreeMap::new(),
        };
        let session_root = tempfile::tempdir().expect("session root");
        let (worktrees, bindings) =
            actor_worktree_resources_at(&runtime.path().join("worktrees"), &workspace)
                .expect("worktree resources");
        let bindings = Arc::new(Mutex::new(bindings));
        let authority = ActorWorktreeAuthority::new(
            runtime_namespace(session_root.path()),
            Arc::clone(&bindings),
        );
        let (source, root) =
            compile_root(&config, session_root.path(), worktrees, authority.clone())
                .expect("compile root driver");
        let (actor, hosted, mut deployments) = spawn_resident_root(source, root)
            .await
            .expect("spawn resident root");
        authority.install_root(actor.identity().into());
        let LocalResidentDeployment::PolicyInstalled(root_installation) =
            deployments.try_recv().expect("root driver installation")
        else {
            panic!("root driver retired before installation");
        };
        assert_eq!(root_installation.actor.identity(), actor.identity());
        assert_eq!(root_installation.initial_user_message, None);
        let policy = root_installation.policy;
        let names = policy
            .tools()
            .iter()
            .map(HostedTool::name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["haskell"]);
        assert!(policy
            .tools()
            .iter()
            .all(|tool| matches!(tool, HostedTool::Custom(_))));
        assert!(policy.tools()[0].description().contains("GHCi-style"));

        let completion_type = dispatch_haskell(policy.as_ref(), [":type complete"]).await;
        assert_eq!(
            completion_type["status"], "committed",
            "{completion_type:?}"
        );
        assert_eq!(
            completion_type["items"][0]["output"],
            "complete :: AgentAction RootEffects ()\n-> Eff (Complete (AgentAction RootEffects ()) : ActorEffects) ()"
        );

        let rejection_support = dispatch_haskell_script(
            policy.as_ref(),
            "type WrongCompletionPayload = Int\nimport qualified Tidepool.Agent.Completion as D",
        )
        .await;
        assert_eq!(
            rejection_support["status"], "committed",
            "{rejection_support:?}"
        );

        for wrong in fixture_items(include_str!(
            "actor_host_fixtures/generic_actor/completion_rejections.hs"
        )) {
            let rejected = dispatch_haskell_script(policy.as_ref(), wrong).await;
            assert_eq!(rejected["status"], "rejected", "{rejected:?}");
            let diagnostic = rejected["items"][0]["output"]
                .as_str()
                .expect("completion rejection diagnostic");
            assert!(diagnostic.contains("<input unit 1>"), "{diagnostic}");
            assert_ne!(diagnostic, "<opaque value>");

            let recovered =
                dispatch_haskell_script(policy.as_ref(), ":bindings\n:type complete").await;
            assert_eq!(recovered["status"], "committed", "{recovered:?}");
            assert_eq!(
                recovered["items"][1]["output"],
                completion_type["items"][0]["output"]
            );
        }

        let multiline_completion = dispatch_haskell_script(
            policy.as_ref(),
            include_str!("actor_host_fixtures/generic_actor/multiline_completion.hs"),
        )
        .await;
        assert_eq!(
            multiline_completion["status"], "completed",
            "{multiline_completion:?}"
        );
        let multiline_activation = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == actor.identity() =>
                    {
                        break activation.message
                    }
                    Some(_) => {}
                    None => panic!(
                        "resident deployment channel closed before multiline completion wake"
                    ),
                }
            }
        })
        .await
        .expect("multiline completion timeout");
        assert!(multiline_activation.contains("typed result"));

        let one_line_completion = dispatch_haskell_script(
            policy.as_ref(),
            "complete $ nextTurn $ liftAction $ pure ()",
        )
        .await;
        assert_eq!(
            one_line_completion["status"], "completed",
            "{one_line_completion:?}"
        );
        let one_line_activation = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == actor.identity() =>
                    {
                        break activation.message
                    }
                    Some(_) => {}
                    None => {
                        panic!("resident deployment channel closed before one-line completion wake")
                    }
                }
            }
        })
        .await
        .expect("one-line completion timeout");
        assert!(one_line_activation.contains("typed result"));

        let discovery = dispatch_haskell_script(
            policy.as_ref(),
            include_str!("actor_host_fixtures/generic_actor/workbench_discovery.hs"),
        )
        .await;
        assert_eq!(discovery["status"], "committed", "{discovery:?}");
        let initial_imports = discovery["items"][0]["output"]
            .as_str()
            .expect(":show imports output");
        assert_eq!(
            initial_imports.lines().next(),
            Some("import Tidepool.Actors.Shoal")
        );
        let browse = discovery["items"][1]["output"]
            .as_str()
            .expect(":browse output");
        assert!(browse.contains("ActorEffects"), "{browse}");
        for public in ["startAgent", "request", "waitReply", "stopAgent"] {
            assert!(browse.contains(public), "missing {public}: {browse}");
        }
        for internal in ["ActorDefinition", "startActor", "deliberate"] {
            assert!(!browse.contains(internal), "leaked {internal}: {browse}");
        }
        assert_eq!(
            discovery["items"][2]["status"], "committed",
            "{discovery:?}"
        );
        let later_imports = discovery["items"][4]["output"]
            .as_str()
            .expect("later :show imports output");
        assert_eq!(
            later_imports,
            format!("{initial_imports}\nimport qualified Data.Set as Set")
        );
        assert_eq!(
            discovery["items"][5]["status"], "committed",
            "{discovery:?}"
        );
        assert_eq!(
            discovery["items"][6]["status"], "committed",
            "{discovery:?}"
        );
        assert!(
            discovery["items"][7]["output"]
                .as_str()
                .unwrap_or_default()
                .contains("emptySet :: Set Int"),
            "{discovery:?}"
        );

        let unsupported_show = dispatch_haskell(policy.as_ref(), [":show modules"]).await;
        assert_eq!(
            unsupported_show["status"], "rejected",
            "{unsupported_show:?}"
        );
        assert!(
            unsupported_show["items"][0]["output"]
                .as_str()
                .unwrap_or_default()
                .contains(":show does not support `modules` (supported: :show imports)"),
            "{unsupported_show:?}"
        );
        let discovery_recovery =
            dispatch_haskell_script(policy.as_ref(), ":show imports\n:type Set.empty\n:bindings")
                .await;
        assert_eq!(
            discovery_recovery["status"], "committed",
            "{discovery_recovery:?}"
        );
        assert!(
            discovery_recovery["items"][0]["output"]
                .as_str()
                .unwrap_or_default()
                .contains("import qualified Data.Set as Set"),
            "{discovery_recovery:?}"
        );
        assert_eq!(
            discovery_recovery["items"][1]["status"], "committed",
            "{discovery_recovery:?}"
        );
        assert!(
            discovery_recovery["items"][2]["output"]
                .as_str()
                .unwrap_or_default()
                .contains("actorEffectsIdentity ::"),
            "{discovery_recovery:?}"
        );

        let worktree_response = dispatch_haskell(
            policy.as_ref(),
            fixture_items(include_str!(
                "actor_host_fixtures/generic_actor/root_worktree_response.hs"
            )),
        )
        .await;
        assert_eq!(
            worktree_response["status"], "completed",
            "an abstract interpreter-produced handle must cross into Haskell without ending the session: {worktree_response:?}"
        );
        let worktree_activation = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == actor.identity() =>
                    {
                        break activation.message
                    }
                    Some(_) => {}
                    None => panic!(
                        "resident deployment channel closed before worktree continuation wake"
                    ),
                }
            }
        })
        .await
        .expect("worktree continuation timeout");
        assert!(worktree_activation.contains("typed result"));
        let worktree_bindings =
            dispatch_haskell(policy.as_ref(), [":type sessionInput", ":bindings"]).await;
        assert_eq!(
            worktree_bindings["status"], "committed",
            "the interactive session must remain available after an effect response: {worktree_bindings:?}"
        );
        assert!(
            worktree_bindings["items"][0]["output"]
                .as_str()
                .unwrap_or_default()
                .contains("sessionInput :: Either WorktreeError WorktreeHandle"),
            "{worktree_bindings:?}"
        );

        let first_request = dispatch_haskell(
            policy.as_ref(),
            fixture_items(include_str!(
                "actor_host_fixtures/generic_actor/persistent_agent_start.hs"
            )),
        )
        .await;
        assert_eq!(first_request["status"], "completed", "{first_request:?}");
        let child_installation = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::PolicyInstalled(installation))
                        if installation.actor.identity() != actor.identity() =>
                    {
                        break installation;
                    }
                    Some(LocalResidentDeployment::Retired {
                        actor: retired,
                        terminal,
                    }) if retired != actor.identity() => {
                        panic!("persistent agent retired during launch: {retired:?}: {terminal:?}")
                    }
                    Some(LocalResidentDeployment::ChildExited { notice }) => panic!(
                        "child exited during persistent agent launch: {:?}",
                        notice.terminal
                    ),
                    Some(_) => {}
                    None => panic!("deployment channel closed before persistent agent launch"),
                }
            }
        })
        .await
        .expect("persistent agent launch timeout");
        assert_eq!(
            child_installation.initial_user_message.as_deref(),
            None,
            "startup attaches Codex without manufacturing a task turn"
        );
        assert_eq!(child_installation.launch_worktrees.len(), 1);
        let first_activation = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == child_installation.actor.identity() =>
                    {
                        break activation
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before first request activation"),
                }
            }
        })
        .await
        .expect("first persistent request timeout");
        assert!(
            first_activation
                .message
                .contains("Return a closure after inspecting the candidate."),
            "{}",
            first_activation.message
        );
        assert!(
            first_activation
                .message
                .contains("sessionInput :: Candidate"),
            "{}",
            first_activation.message
        );

        let first_reply = dispatch_haskell_script(
            child_installation.policy.as_ref(),
            ":type sessionInput\ncomplete $ case sessionInput of Candidate _ -> ((+ 1) :: Int -> Int)",
        )
        .await;
        assert_eq!(
            first_reply["items"][0]["output"], "sessionInput :: Candidate",
            "{first_reply:?}"
        );
        assert_eq!(first_reply["status"], "completed", "{first_reply:?}");
        let first_root_activation = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == actor.identity() =>
                    {
                        break activation
                    }
                    Some(LocalResidentDeployment::ChildExited { notice }) => {
                        panic!("child exited before first reply reached root: {notice:?}")
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before first reply reached root"),
                }
            }
        })
        .await
        .expect("first persistent reply timeout");
        if first_root_activation.reason != tidepool_actor::ActivationReason::ActionCompleted {
            let failure = dispatch_haskell_script(policy.as_ref(), "sessionInput").await;
            panic!(
                "first reply action failed: {}; {failure:?}",
                first_root_activation.message
            );
        }

        let second_request = dispatch_haskell_script(
            policy.as_ref(),
            include_str!("actor_host_fixtures/generic_actor/persistent_agent_second.hs"),
        )
        .await;
        assert_eq!(second_request["status"], "completed", "{second_request:?}");
        let second_activation = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == child_installation.actor.identity() =>
                    {
                        break activation;
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before second request activation"),
                }
            }
        })
        .await
        .expect("second persistent request timeout");
        assert!(
            second_activation
                .message
                .contains("Return a review report for this text."),
            "{}",
            second_activation.message
        );
        assert!(
            second_activation.message.contains("sessionInput :: Text"),
            "{}",
            second_activation.message
        );

        let second_reply = dispatch_haskell_script(
            child_installation.policy.as_ref(),
            ":type sessionInput\ncomplete (ReviewReport (T.length sessionInput))",
        )
        .await;
        assert_eq!(
            second_reply["items"][0]["output"], "sessionInput :: Text",
            "{second_reply:?}"
        );
        assert_eq!(second_reply["status"], "completed", "{second_reply:?}");
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == actor.identity() =>
                    {
                        break
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before second reply reached root"),
                }
            }
        })
        .await
        .expect("second persistent reply timeout");
        let persistent_result = dispatch_haskell_script(
            policy.as_ref(),
            "let (_, transform, _, ReviewReport score) = sessionInput in (transform 41, score)",
        )
        .await;
        assert_eq!(
            persistent_result["status"], "committed",
            "{persistent_result:?}"
        );
        assert_eq!(persistent_result["items"][0]["output"], "(42,5)");

        let stopped = dispatch_haskell_script(
            policy.as_ref(),
            "complete $ nextTurn $ let state@(agent, _, _, _) = sessionInput in liftAction (stopAgent agent) >> pure state",
        )
        .await;
        assert_eq!(stopped["status"], "completed", "{stopped:?}");
        let stopped_activation = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == actor.identity() =>
                    {
                        break activation;
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed after persistent agent stop"),
                }
            }
        })
        .await
        .expect("persistent agent stop activation timeout");
        assert_eq!(
            stopped_activation.reason,
            tidepool_actor::ActivationReason::ActionCompleted
        );
        let stopped_terminal = tokio::time::timeout(
            Duration::from_secs(30),
            child_installation.actor.terminal().wait(),
        )
        .await
        .expect("persistent agent stop timeout");
        assert_eq!(stopped_terminal.kind, ActorExitKind::Completed);

        let stale_reply = dispatch_haskell_script(
            policy.as_ref(),
            "complete $ nextTurn $ let (_, _, reply, _) = sessionInput in waitReply reply",
        )
        .await;
        assert_eq!(stale_reply["status"], "completed", "{stale_reply:?}");
        let failed_activation = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == actor.identity() =>
                    {
                        break activation;
                    }
                    Some(LocalResidentDeployment::Retired {
                        actor: retired,
                        terminal,
                    }) if retired == actor.identity() => {
                        panic!("stale reply retired the root actor: {terminal:?}")
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed after stale reply failure"),
                }
            }
        })
        .await
        .expect("stale reply failure activation timeout");
        assert_eq!(
            failed_activation.reason,
            tidepool_actor::ActivationReason::ActionFailed
        );
        assert!(
            actor.terminal().get().is_none(),
            "a failed action must leave the root incarnation live"
        );
        let failure = dispatch_haskell_script(policy.as_ref(), "sessionInput").await;
        let failure_output = failure["items"][0]["output"].as_str().unwrap_or_default();
        assert!(
            failure_output.contains("ReplyUnavailable")
                && failure_output.contains("target actor")
                && failure_output.contains("has exited"),
            "{failure:?}"
        );
        let retry = dispatch_haskell_script(policy.as_ref(), "1 + 1").await;
        assert_eq!(retry["status"], "committed", "{retry:?}");
        assert_eq!(retry["items"][0]["output"], "2", "{retry:?}");

        let advanced_actor_import =
            dispatch_haskell_script(policy.as_ref(), "import Tidepool.Actor").await;
        assert_eq!(
            advanced_actor_import["status"], "committed",
            "{advanced_actor_import:?}"
        );

        let live_action = dispatch_haskell(
            policy.as_ref(),
            fixture_items(include_str!(
                "actor_host_fixtures/generic_actor/root_live_action.hs"
            )),
        )
        .await;
        assert_eq!(live_action["status"], "completed", "{live_action:?}");
        let activation = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == actor.identity() =>
                    {
                        break activation.message;
                    }
                    Some(LocalResidentDeployment::ChildExited { notice })
                        if notice.owner == actor.identity() =>
                    {
                        panic!("an already-awaited child exit generated a redundant wake")
                    }
                    Some(_) => {}
                    None => panic!("resident deployment channel closed before continuation wake"),
                }
            }
        })
        .await
        .expect("live action continuation timeout");
        assert!(activation.contains("typed result"), "{activation}");
        let resumed_action = dispatch_haskell(
            policy.as_ref(),
            fixture_items(include_str!(
                "actor_host_fixtures/generic_actor/root_live_action_resume.hs"
            )),
        )
        .await;
        assert_eq!(resumed_action["status"], "completed", "{resumed_action:?}");

        let rejected_setup = dispatch_haskell(
            policy.as_ref(),
            fixture_items(include_str!(
                "actor_host_fixtures/generic_actor/root_rejected_actor_setup.hs"
            )),
        )
        .await;
        assert_eq!(rejected_setup["status"], "committed", "{rejected_setup:?}");
        let rejected = policy
            .dispatch_boxed(ToolInvocation {
                context: None,
                name: tidepool_actor::HASKELL_TOOL.into(),
                arguments: ToolArguments::Raw(
                    include_str!("actor_host_fixtures/generic_actor/root_rejected_actor_call.hs")
                        .into(),
                ),
            })
            .await
            .expect_err("a call to an exited exact actor must fail");
        assert!(
            rejected.to_string().contains("has exited"),
            "unexpected dead-actor failure: {rejected}"
        );
        let after_rejection = dispatch_haskell(policy.as_ref(), [":bindings"]).await;
        assert_eq!(
            after_rejection["status"], "committed",
            "a rejected actor operation must not consume the surrounding agent session: {after_rejection:?}"
        );
        let bindings = after_rejection["items"][0]["output"]
            .as_str()
            .expect("bindings output");
        assert!(bindings.contains("deadActor ::"), "{bindings}");
        assert!(bindings.contains("ActorDefinition"), "{bindings}");
        assert!(bindings.contains("sessionInput ::"), "{bindings}");

        let rendered = dispatch_haskell_script(
            policy.as_ref(),
            "data DisplayProbe = DisplayProbe { probeCode :: Int, probeMessage :: String } deriving Show\nDisplayProbe 7 \"ready\"",
        )
        .await;
        assert_eq!(rendered["status"], "committed", "{rendered:?}");
        assert_eq!(
            rendered["items"][1]["output"],
            "DisplayProbe {probeCode = 7, probeMessage = \"ready\"}"
        );
        assert!(
            !rendered["items"][1]["output"]
                .as_str()
                .unwrap_or_default()
                .contains("constructor"),
            "{rendered:?}"
        );

        let opaque =
            dispatch_haskell_script(policy.as_ref(), "opaqueProbe x = x\nopaqueProbe").await;
        assert_eq!(opaque["status"], "committed", "{opaque:?}");
        assert_eq!(opaque["items"][1]["output"], "<opaque value>");

        let compile_rejected = dispatch_haskell_script(
            policy.as_ref(),
            "compileAnchor = (1 :: Int)\ncompileBroken = True + 1",
        )
        .await;
        assert_eq!(
            compile_rejected["status"], "rejected",
            "{compile_rejected:?}"
        );
        let diagnostic = compile_rejected["items"][1]["output"]
            .as_str()
            .expect("rejected declaration diagnostic");
        assert!(diagnostic.contains("<input unit 2>:1:"), "{diagnostic}");
        assert!(!diagnostic.contains("SessionDecls.hs"), "{diagnostic}");

        let failed_action = dispatch_haskell(
            policy.as_ref(),
            fixture_items(include_str!(
                "actor_host_fixtures/generic_actor/root_failed_action.hs"
            )),
        )
        .await;
        assert_eq!(failed_action["status"], "completed", "{failed_action:?}");
        let failure_activation = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match deployments.recv().await {
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.id.actor() == actor.identity() =>
                    {
                        break activation.message;
                    }
                    Some(_) => {}
                    None => panic!("resident deployment channel closed before failure wake"),
                }
            }
        })
        .await
        .expect("failed action continuation timeout");
        assert!(failure_activation.contains("lifecycle failure"));
        let resumed_failure = dispatch_haskell(
            policy.as_ref(),
            fixture_items(include_str!(
                "actor_host_fixtures/generic_actor/root_failed_action_resume.hs"
            )),
        )
        .await;
        assert_eq!(
            resumed_failure["status"], "completed",
            "{resumed_failure:?}"
        );

        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "test complete".into(),
            })
            .await
            .expect("shutdown root");
        hosted.await.expect("root actor task");
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

        eval_harness::require_extract();
        let repository = tidepool_worktree::testing::TestRepo::init().unwrap();
        repository
            .writer()
            .commit_file("README.md", "source\n", "seed")
            .unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let config = ActorHostConfig {
            haskell_root: crate::haskell_sources::ensure_shoal_haskell().unwrap(),
            workspace: repository.path().to_path_buf(),
            run_root: runtime.path().join("run"),
            root_binding_path: runtime.path().join("root-binding.json"),
            interactive_agent: tidepool_agent::native_interactive_agent_from_parts(
                std::env::current_exe().unwrap(),
                "test installation".into(),
            )
            .unwrap(),
            tmux_session: "unused-in-reply-watch-test".into(),
            model: None,
            effort: None,
            root_launch_mode: InteractiveLaunchMode::Fresh,
            pane_environment: std::collections::BTreeMap::new(),
        };
        let session_root = tempfile::tempdir().expect("session root");
        let (worktrees, bindings) =
            actor_worktree_resources_at(&runtime.path().join("worktrees"), repository.path())
                .expect("worktree resources");
        let bindings = Arc::new(Mutex::new(bindings));
        let authority = ActorWorktreeAuthority::new(
            runtime_namespace(session_root.path()),
            Arc::clone(&bindings),
        );
        let (source, root) =
            compile_root(&config, session_root.path(), worktrees, authority.clone())
                .expect("compile permanent root");
        let (actor, hosted, mut deployments) = spawn_resident_root(source, root)
            .await
            .expect("spawn permanent root");
        authority.install_root(actor.identity().into());
        let LocalResidentDeployment::PolicyInstalled(root_installation) = deployments
            .recv()
            .await
            .expect("root application installation")
        else {
            panic!("root retired before installing its application")
        };

        let submitted = tokio::time::timeout(
            Duration::from_secs(120),
            dispatch_haskell(
                root_installation.policy.as_ref(),
                fixture_items(include_str!(
                    "actor_host_fixtures/generic_actor/reply_watch_roundtrip.hs"
                )),
            ),
        )
        .await
        .expect("request setup timed out");
        assert_eq!(submitted["status"], "committed", "{submitted:?}");

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
            pending_status.contains("responses pending=["),
            "{pending_status}"
        );
        assert!(
            pending_status.contains("watches pending=["),
            "{pending_status}"
        );

        let child_installation = loop {
            match deployments.recv().await {
                Some(LocalResidentDeployment::PolicyInstalled(installation))
                    if installation.actor.identity() != actor.identity() =>
                {
                    break installation
                }
                Some(_) => {}
                None => panic!("deployment channel closed before child installation"),
            }
        };
        let activation = loop {
            match deployments.recv().await {
                Some(LocalResidentDeployment::SessionReady { activation })
                    if activation.id.actor() == child_installation.actor.identity() =>
                {
                    break activation
                }
                Some(_) => {}
                None => panic!("deployment channel closed before request activation"),
            }
        };
        assert_eq!(activation.input_type, "Int");

        let replied = dispatch_haskell_script(
            child_installation.policy.as_ref(),
            ":type sessionReply\n:type respond\nrespond (ReplyReport (sessionInput + 1))",
        )
        .await;
        assert_eq!(replied["status"], "replied", "{replied:?}");

        let notification = loop {
            match deployments.recv().await {
                Some(LocalResidentDeployment::WatchChanged { notification })
                    if notification.owner == actor.identity() =>
                {
                    break notification
                }
                Some(_) => {}
                None => panic!("deployment channel closed before watch transition"),
            }
        };
        assert_eq!(
            notification.transition,
            tidepool_actor::WatchTransition::Ready
        );

        let observed = dispatch_haskell_script(
            root_installation.policy.as_ref(),
            "pollResponse response\npollWatch readiness",
        )
        .await;
        assert_eq!(observed["status"], "committed", "{observed:?}");
        assert!(
            observed["items"][0]["output"]
                .as_str()
                .is_some_and(|output| output.contains("ResponseReady (ReplyReport 42)")),
            "{observed:?}"
        );
        assert!(
            observed["items"][1]["output"]
                .as_str()
                .is_some_and(|output| output.contains("WatchReady (ReplyReport 42)")),
            "{observed:?}"
        );

        let ready_status =
            dispatch_haskell_script(root_installation.policy.as_ref(), ":status").await;
        let ready_status = ready_status["items"][0]["output"]
            .as_str()
            .expect("status output");
        assert!(
            ready_status.contains("responses pending=[]"),
            "{ready_status}"
        );
        assert!(
            ready_status.contains("responses pending=[] ready=["),
            "{ready_status}"
        );
        assert!(
            ready_status.contains("watches pending=[] ready=["),
            "{ready_status}"
        );

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
