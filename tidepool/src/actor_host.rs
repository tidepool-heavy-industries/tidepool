//! Composition root for the first actor-native interactive swarm.
//!
//! The daemon owns resident Haskell scheduling and exact actor lifecycle. One
//! stock interactive agent is attached to each installed Haskell MCP policy;
//! tmux is process ownership and observability, never message transport.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use frunk::{hlist, HCons, HNil};
use parking_lot::Mutex;
use rmcp::ServiceExt;
use tidepool_actor::{
    ActorDescriptor, ActorEffectProfile, ActorExitKind, ActorPlacement, ActorRef, ActorRegistry,
    ActorTerminal, ActorWorkbenchSource, ExternalApplicationFailure,
    ExternalApplicationFailureClass, ExternalFailureDisposition, ResidentActorDeployment,
    ResidentActorHost, ResidentActorHostControl, ResidentActorRoot, ResidentLifecyclePolicy,
    ResidentMcpInstallation,
};
use tidepool_agent::{
    native_interactive_backend, read_interactive_binding, BackendThreadId, InteractiveAgentBackend,
    InteractiveAgentSpec, InteractiveLaunchMode, InteractiveMcpServer, InteractiveProxyBinding,
    ReasoningEffort,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_handlers::{ActorWorktreeAuthority, ActorWorktreeHandler, WorktreeHandler};
use tidepool_mcp::{CapturedOutput, DynamicMcpServer};
use tidepool_model::{ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse};
use tidepool_node::{
    accept_proxy, DurableInbox, NodeCredential, TmuxLaunch, TmuxPaneId, TmuxSession,
};
use tidepool_repr::SessionId;
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, ModuleEnv, ResidentSession,
    SessionLib, TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_worktree::{
    ActiveBinding, AgentRef as WorktreePrincipal, BindingTable, BindingTerminal, GitCli,
    WorktreeHandle, WorktreeId, WorktreeManager, WorktreeRegistry,
};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

const POLICY_MODULE: &str = "Tidepool.Actors.DevSwarm";
const POLICY_ENTRY: &str = "rootPolicy";
const POLICY_EFFECTS: &str = "RootEffects";
const APPLICATION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
const APPLICATION_TASK_GRACE_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

type ShoalHandlerStack = HCons<ActorWorktreeHandler, HNil>;
type ShoalRoot = ResidentActorRoot<ShoalHandlerStack, CapturedOutput>;

#[derive(Clone)]
pub struct ActorHostConfig {
    pub workspace: PathBuf,
    pub policy_root: PathBuf,
    pub run_root: PathBuf,
    pub root_binding_path: PathBuf,
    pub proxy_program: String,
    pub proxy_args: Vec<String>,
    pub tmux_session: String,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub root_launch_mode: InteractiveLaunchMode,
    pub pane_environment: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub enum ActorHostReadiness {
    /// The root pane is selected and accepts its first real User input, but a
    /// fresh backend conversation does not yet have a thread identity.
    AwaitingInput { root: ActorRef },
    /// The MCP sidecar proved the exact surrounding thread, enabling native
    /// lifecycle pushes without changing application ownership.
    Ready {
        root: ActorRef,
        thread: BackendThreadId,
    },
}

struct NoResidentProvider;

impl ModelProvider for NoResidentProvider {
    async fn complete(
        &self,
        _request: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        Err(ProviderError::Api(
            "this initial interactive policy has no resident deliberation provider".into(),
        ))
    }
}

struct InteractiveDeployment {
    actor: ActorRef,
    pane: TmuxPaneId,
    workspace: PathBuf,
    inbox: Arc<DurableInbox<String>>,
    connection: InteractiveConnection,
    service: tokio::task::JoinHandle<Result<(), InteractiveApplicationError>>,
    socket_root: PathBuf,
    worktree: Option<WorktreeHandle>,
    worktree_binding: Option<ActiveBinding>,
    failure_reported: bool,
}

enum InteractiveConnection {
    // Pane, inbox, proxy listener, and cleanup are already owned in this state.
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
    inbox: Arc<DurableInbox<String>>,
    message: String,
}

#[derive(Debug, Clone, Copy)]
enum InteractiveOperation {
    BindWorktree,
    PrepareRuntime,
    BindProxy,
    BuildCommand,
    BuildPolicy,
    AcceptProxy,
    ServeMcp,
    LaunchProcess,
    DiscoverBinding,
    StopProcess,
}

impl fmt::Display for InteractiveOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::BindWorktree => "bind actor worktree",
            Self::PrepareRuntime => "prepare runtime",
            Self::BindProxy => "bind proxy",
            Self::BuildCommand => "build agent command",
            Self::BuildPolicy => "build MCP policy",
            Self::AcceptProxy => "accept proxy",
            Self::ServeMcp => "serve MCP",
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
            Self::BindProxy
            | Self::AcceptProxy
            | Self::ServeMcp
            | Self::DiscoverBinding
            | Self::PrepareRuntime => ExternalApplicationFailureClass::ProxyStartup,
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
    registry: ActorRegistry,
    root: ActorRef,
    config: ActorHostConfig,
    run_root: PathBuf,
    tmux: TmuxSession,
    backend: Arc<dyn InteractiveAgentBackend>,
    worktrees: WorktreeManager,
    bindings: Arc<Mutex<BindingTable>>,
    control: ResidentActorHostControl,
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
    config: ActorHostConfig,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
) -> Result<(), Box<dyn std::error::Error>> {
    let run_root = config.run_root.clone();
    std::fs::create_dir_all(&run_root)?;

    let registry = ActorRegistry::new();
    let (worktrees, bindings) = actor_worktree_resources(&config.workspace)?;
    let bindings = Arc::new(Mutex::new(bindings));
    let worktree_authority =
        ActorWorktreeAuthority::new(runtime_namespace(&run_root), Arc::clone(&bindings));
    let (source, root) = compile_root(
        &config,
        &run_root,
        worktrees.clone(),
        worktree_authority.clone(),
    )?;
    let mut host = ResidentActorHost::new(
        registry.clone(),
        source,
        Arc::new(NoResidentProvider),
        None,
        ResidentLifecyclePolicy::default(),
    )?;
    let deployments = host.take_deployments()?;
    let control = host.control();
    let root_actor = host.launch_root(root).await?;
    worktree_authority.install_root(root_actor.into());

    let tmux = TmuxSession::new(&config.tmux_session)?;
    if !tmux.exists().await? {
        return Err(runtime_error(format!(
            "Shoal tmux session {:?} does not exist",
            config.tmux_session
        )));
    }
    let backend = native_interactive_backend();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let mut host_task =
        tokio::spawn(host.run_until_shutdown(wait_for_shutdown(shutdown_rx.clone())));
    let mut applications_task = tokio::spawn(run_interactive_applications(
        deployments,
        InteractiveFleet {
            registry,
            root: root_actor,
            config,
            run_root,
            tmux,
            backend,
            worktrees,
            bindings,
            control,
            readiness,
        },
        shutdown_rx,
    ));

    enum FirstStop {
        Signal,
        Host(
            Result<
                tidepool_actor::ResidentHostShutdownReport,
                tidepool_actor::ResidentActorHostError,
            >,
        ),
        Applications(Result<(), String>),
    }
    let first = tokio::select! {
        signal = operator_shutdown() => {
            signal?;
            FirstStop::Signal
        }
        result = &mut host_task => FirstStop::Host(result.map_err(join_error)?),
        result = &mut applications_task => FirstStop::Applications(result.map_err(join_error)?),
    };
    shutdown.send_replace(true);

    match first {
        FirstStop::Signal => {
            host_task.await.map_err(join_error)??;
            await_applications(&mut applications_task).await?;
        }
        FirstStop::Host(result) => {
            result?;
            await_applications(&mut applications_task).await?;
        }
        FirstStop::Applications(result) => {
            result.map_err(runtime_error)?;
            host_task.await.map_err(join_error)??;
        }
    }
    Ok(())
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
    Ok((
        WorktreeManager::new(GitCli::new(), registry, root.join("checkouts"), workspace),
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
        tidepool_mcp::actor_mcp_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::deliberate_decl(),
        tidepool_mcp::fs_read_decl(),
        tidepool_mcp::worktree_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&declarations)?;
    let mut include = effects.include_paths().to_vec();
    include.push(config.policy_root.clone());
    include.push(crate::haskell_sources::ensure_stdlib()?);
    let preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble(&declarations, false),
        POLICY_MODULE,
    );
    let templates = resident_workbench_templates(&preamble, POLICY_EFFECTS, "");
    let include_refs: Vec<_> = include.iter().map(PathBuf::as_path).collect();
    let session_root = run_root.join("haskell-session");
    std::fs::create_dir_all(&session_root)?;
    let compiled = match run_turn(HaskellTurnRequest {
        turn_text: POLICY_ENTRY,
        templates: &templates,
        include: &include_refs,
        session_root: &session_root,
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })? {
        TurnResult::Expr { compiled, .. } => compiled,
        other => {
            return Err(runtime_error(format!(
                "root policy is not an expression: {other:?}"
            )))
        }
    };

    let session = fresh_session_id();
    let library = SessionLib::open(session, &session_root, ModuleEnv::standalone_default())?
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
        "devswarm_root_policy",
        &compiled.expr,
        &compiled.table,
        &compiled.asks,
    )?;
    let descriptor = ActorDescriptor::new(
        "devswarm-root",
        ["ActorMcp", "Actor", "Worktree"],
        ActorPlacement {
            session,
            resource_scope: RealmId::fresh(),
            lexical_scope: ScopeId::ROOT,
        },
    )
    // Profiles classify resident Haskell rows, not the native Codex sandbox.
    // The root allocates worktrees and may attenuate children to ReadOnly.
    .with_profile(ActorEffectProfile::ReadWrite);
    Ok((
        ActorWorkbenchSource::new(preamble, include),
        ResidentActorRoot::new(descriptor, machine, outcome),
    ))
}

async fn run_interactive_applications(
    mut lifecycle: mpsc::UnboundedReceiver<ResidentActorDeployment>,
    fleet: InteractiveFleet,
    shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    let InteractiveFleet {
        registry,
        root,
        config,
        run_root,
        tmux,
        backend,
        worktrees,
        bindings,
        control,
        readiness,
    } = fleet;
    let launch_context = InteractiveLaunchContext {
        root,
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
                    deployments[index].failure_reported = true;
                    let detail = "interactive application exited before actor settlement".to_string();
                    if actor == root {
                        break Some(format!("root {actor:?}: {detail}"));
                    }
                    let result = control
                        .fail_external_application(
                            actor,
                            ExternalApplicationFailure {
                                class: ExternalApplicationFailureClass::UnexpectedExit,
                                detail,
                            },
                        )
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
                    ResidentActorDeployment::PolicyInstalled(installation) => {
                        let context = launch_context.clone();
                        let actor = installation.actor;
                        let (cancel, cancelled) = oneshot::channel();
                        let previous = pending_launches.insert(actor, cancel);
                        debug_assert!(previous.is_none(), "one launch per exact actor incarnation");
                        launches.spawn(async move {
                            let result = launch_interactive_application(
                                installation,
                                context,
                                cancelled,
                            ).await;
                            (actor, result)
                        });
                    }
                    ResidentActorDeployment::Retired { actor, terminal } => {
                        if let Some(cancel) = pending_launches.remove(&actor) {
                            let _ = cancel.send(());
                        }
                        match prepare_owner_notification(
                            actor,
                            &terminal,
                            &registry,
                            &deployments,
                        ) {
                            Ok(Some(notification)) => {
                                notifications.spawn(publish_owner_notification(notification));
                            }
                            Ok(None) => {}
                            Err(error) => break Some(error),
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
                }
            }
            launched = launches.join_next(), if !launches.is_empty() => {
                match launched {
                    Some(Ok((actor, Ok(Some(launched))))) => {
                        pending_launches.remove(&actor);
                        let deployment = launched.deployment;
                        if actor == root {
                            let _ = readiness.send(ActorHostReadiness::AwaitingInput { root });
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
                    Some(Ok((actor, Ok(None)))) => {
                        pending_launches.remove(&actor);
                    }
                    Some(Ok((actor, Err(error)))) => {
                        pending_launches.remove(&actor);
                        if actor == root {
                            break Some(error.to_string());
                        }
                        let result = control
                            .fail_external_application(
                                actor,
                                ExternalApplicationFailure {
                                    class: error.operation.failure_class(),
                                    detail: error.detail,
                                },
                            )
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
                        if actor == root {
                            let _ = readiness.send(ActorHostReadiness::Ready {
                                root,
                                thread,
                            });
                        }
                    }
                    Some(Ok((actor, Err(error)))) => {
                        let Some(deployment) = deployments.iter_mut().find(|app| app.actor == actor) else {
                            continue;
                        };
                        deployment.failure_reported = true;
                        if actor == root {
                            break Some(error.to_string());
                        }
                        let result = control
                            .fail_external_application(
                                actor,
                                ExternalApplicationFailure {
                                    class: error.operation.failure_class(),
                                    detail: error.detail,
                                },
                            )
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

    for (_, cancel) in pending_launches.drain() {
        let _ = cancel.send(());
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
    installation: ResidentMcpInstallation,
    context: InteractiveLaunchContext,
    cancelled: oneshot::Receiver<()>,
) -> Result<Option<LaunchedInteractiveApplication>, InteractiveApplicationError> {
    let actor = installation.actor;
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
    installation: &ResidentMcpInstallation,
    context: &InteractiveLaunchContext,
) -> Result<Option<(WorktreeHandle, ActiveBinding)>, InteractiveApplicationError> {
    let actor = installation.actor;
    if actor == context.root {
        if installation.launch_worktrees.is_empty() {
            return Ok(None);
        }
        return Err(application_error(
            actor,
            InteractiveOperation::BindWorktree,
            "the root application may not carry a child worktree recipe",
        ));
    }
    let [raw_id] = installation.launch_worktrees.as_slice() else {
        return Err(application_error(
            actor,
            InteractiveOperation::BindWorktree,
            format!(
                "an interactive worker requires exactly one worktree recipe, received {}",
                installation.launch_worktrees.len()
            ),
        ));
    };
    if !WorktreeId::is_path_safe(raw_id) {
        return Err(application_error(
            actor,
            InteractiveOperation::BindWorktree,
            "the worktree recipe carried an invalid durable id",
        ));
    }
    let id = WorktreeId::from_raw(raw_id.clone());
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
    installation: ResidentMcpInstallation,
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
    let _ = worktrees;
    let workspace = worktree.as_ref().map_or_else(
        || config.workspace.clone(),
        |handle| handle.cwd().to_path_buf(),
    );
    if cancelled.try_recv().is_ok() {
        return Ok(None);
    }
    let actor_root = run_root.join(format!("{}-{}", actor.id.0, actor.incarnation.0));
    std::fs::create_dir_all(&actor_root)
        .map_err(|error| application_error(actor, InteractiveOperation::PrepareRuntime, error))?;
    let run_socket_id = run_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("run");
    let socket_root = std::env::temp_dir().join(format!(
        "tidepool-{}-{}-{}",
        &run_socket_id[..run_socket_id.len().min(8)],
        actor.id.0,
        actor.incarnation.0
    ));
    std::fs::create_dir_all(&socket_root)
        .map_err(|error| application_error(actor, InteractiveOperation::PrepareRuntime, error))?;
    let endpoint = socket_root.join("mcp.sock");
    let listener = UnixListener::bind(&endpoint)
        .map_err(|error| application_error(actor, InteractiveOperation::BindProxy, error))?;
    let credential = NodeCredential(uuid::Uuid::new_v4().to_string());
    let binding_path = if actor == root {
        config.root_binding_path.clone()
    } else {
        actor_root.join("binding.json")
    };
    let inbox = Arc::new(
        DurableInbox::<String>::open(
            actor_root.join("inbox.jsonl"),
            actor_root.join("inbox.cursor"),
        )
        .map_err(|error| application_error(actor, InteractiveOperation::PrepareRuntime, error))?,
    );
    let binding = InteractiveProxyBinding {
        actor,
        endpoint,
        credential: credential.clone(),
        binding_path: binding_path.clone(),
        workspace: workspace.clone(),
    };
    let launch_mode = if actor == root {
        config.root_launch_mode.clone()
    } else {
        InteractiveLaunchMode::Fresh
    };
    let expected_resume = match &launch_mode {
        InteractiveLaunchMode::Resume(thread) => Some(thread.clone()),
        InteractiveLaunchMode::Fresh | InteractiveLaunchMode::Fork(_) => None,
    };
    let developer_instructions = developer_instructions(actor == root, &launch_mode);
    let initial_prompt = initial_prompt(actor == root);
    let proxy_environment = binding.environment();
    let spec = InteractiveAgentSpec {
        mode: launch_mode,
        model: config.model.clone(),
        effort: config.effort,
        developer_instructions,
        initial_prompt,
        mcp: InteractiveMcpServer {
            name: "tidepool_actor".into(),
            command: config.proxy_program.clone(),
            args: config.proxy_args.clone(),
            cwd: workspace.to_string_lossy().into_owned(),
            forward_env: proxy_environment.keys().cloned().collect(),
            required: true,
        },
    };
    let command = backend
        .render(&spec)
        .map_err(|error| application_error(actor, InteractiveOperation::BuildCommand, error))?;
    let server = DynamicMcpServer::from_resident_policy(installation.policy)
        .map_err(|error| application_error(actor, InteractiveOperation::BuildPolicy, error))?;
    let service = tokio::spawn(async move {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| application_error(actor, InteractiveOperation::AcceptProxy, error))?;
        let (_handshake, stream) = accept_proxy(stream, |candidate| {
            if candidate.actor != actor {
                return Err("actor identity does not match this endpoint".into());
            }
            if candidate.credential != credential {
                return Err("actor proxy credential is invalid".into());
            }
            Ok(())
        })
        .await
        .map_err(|error| application_error(actor, InteractiveOperation::AcceptProxy, error))?;
        let (read, write) = stream.into_split();
        server
            .serve((read, write))
            .await
            .map_err(|error| application_error(actor, InteractiveOperation::ServeMcp, error))?
            .waiting()
            .await
            .map_err(|error| application_error(actor, InteractiveOperation::ServeMcp, error))?;
        Ok(())
    });
    if cancelled.try_recv().is_ok() {
        service.abort();
        let _ = service.await;
        let _ = std::fs::remove_dir_all(&socket_root);
        return Ok(None);
    }
    let pane = match tokio::time::timeout(
        PROCESS_OPERATION_TIMEOUT,
        tmux.spawn_window(&TmuxLaunch {
            window_name: format!("actor-{}-{}", actor.id.0, actor.incarnation.0),
            cwd: workspace.clone(),
            program: command.program,
            args: command.args,
            environment: {
                let mut environment = config.pane_environment.clone();
                environment.extend(proxy_environment);
                environment
            },
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
                actor,
                InteractiveOperation::LaunchProcess,
                error,
            ));
        }
        Err(_) => {
            service.abort();
            let _ = service.await;
            let _ = std::fs::remove_dir_all(&socket_root);
            return Err(application_error(
                actor,
                InteractiveOperation::LaunchProcess,
                format!("tmux launch exceeded {PROCESS_OPERATION_TIMEOUT:?}"),
            ));
        }
    };

    if actor == root {
        if let Err(error) = tmux.select_window_for_pane(&pane).await {
            abandon_interactive_application(&tmux, &pane, service, &socket_root).await;
            return Err(application_error(
                actor,
                InteractiveOperation::LaunchProcess,
                error,
            ));
        }
    }
    if cancelled.try_recv().is_ok() {
        abandon_interactive_application(&tmux, &pane, service, &socket_root).await;
        return Ok(None);
    }
    Ok(Some(LaunchedInteractiveApplication {
        deployment: InteractiveDeployment {
            actor,
            pane,
            workspace,
            inbox,
            connection: InteractiveConnection::AwaitingBinding,
            service,
            socket_root,
            worktree,
            worktree_binding: None,
            failure_reported: false,
        },
        binding: InteractiveBindingRequest {
            path: binding_path,
            expected: expected_resume,
        },
    }))
}

async fn deliver_pending(
    inbox: &Arc<DurableInbox<String>>,
    thread: &BackendThreadId,
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
        backend
            .push(&cwd, thread, &message.payload)
            .await
            .map_err(|error| error.to_string())?;
        let ack_inbox = Arc::clone(inbox);
        tokio::task::spawn_blocking(move || ack_inbox.acknowledge(message.sequence))
            .await
            .map_err(|error| format!("inbox acknowledgement task: {error}"))?
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn run_delivery_pump(
    actor: ActorRef,
    inbox: Arc<DurableInbox<String>>,
    thread: BackendThreadId,
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
                let result = deliver_pending(&inbox, &thread, backend.as_ref(), &workspace).await;
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
    actor: ActorRef,
    terminal: &ActorTerminal,
    registry: &ActorRegistry,
    deployments: &[InteractiveDeployment],
) -> Result<Option<OwnerNotification>, String> {
    let Some(owner) = registry.owner(actor).map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let Some(owner_application) = deployments.iter().find(|app| app.actor == owner) else {
        return Ok(None);
    };
    let descriptor = registry
        .descriptor(actor)
        .map_err(|error| error.to_string())?;
    let kind = match terminal.kind {
        ActorExitKind::Completed => "completed",
        ActorExitKind::Failed => "failed",
        ActorExitKind::Cancelled => "was cancelled",
    };
    let worker_handle = deployments
        .iter()
        .find(|application| application.actor == actor)
        .and_then(|application| application.worktree.as_ref())
        .map(|worktree| {
            format!(
                " The worker handle is {{\"workerId\":\"{}\"}}.",
                worktree.id()
            )
        })
        .unwrap_or_default();
    let message = format!(
        "Tidepool lifecycle: child {:?} ({:?}) {kind}: {}.{worker_handle} This wake is the cue to call collect_worker once for its nonblocking, replayable typed result; it carries correlation only. If that worker was already acknowledged, no further action is required.",
        descriptor.label(),
        actor,
        terminal.summary
    );
    Ok(Some(OwnerNotification {
        inbox: Arc::clone(&owner_application.inbox),
        message,
    }))
}

async fn publish_owner_notification(notification: OwnerNotification) -> Result<(), String> {
    tokio::task::spawn_blocking(move || notification.inbox.publish(notification.message))
        .await
        .map_err(|error| format!("owner inbox publisher task: {error}"))?
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
        stop_retired_mcp_service(
            deployment.actor,
            &mut deployment.service,
            APPLICATION_TASK_GRACE_TIMEOUT,
        ),
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
        (None, None) => Ok(()),
    }
}

/// Settle actor-local tasks after the actor has already reached a terminal
/// state. A client that keeps its MCP transport open cannot invalidate the
/// retained actor result or fail unrelated actors; after the grace period the
/// host owns forced cancellation.
async fn stop_retired_mcp_service(
    actor: ActorRef,
    service: &mut tokio::task::JoinHandle<Result<(), InteractiveApplicationError>>,
    grace: Duration,
) {
    match tokio::time::timeout(grace, &mut *service).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => {
            tracing::warn!(actor = ?actor, %error, "retired actor MCP service stopped with an error");
        }
        Ok(Err(error)) => {
            tracing::warn!(actor = ?actor, %error, "retired actor MCP service task failed");
        }
        Err(_) => {
            tracing::debug!(actor = ?actor, "forcing retired actor MCP service to stop");
            service.abort();
            let _ = service.await;
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
) -> Result<tidepool_agent::BackendThreadId, InteractiveApplicationError> {
    let mut binding_poll = tokio::time::interval(Duration::from_millis(100));
    let mut pane_health = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = binding_poll.tick() => {
                if let Ok(thread) = read_interactive_binding(&request.path).await {
                    if let Some(expected) = &request.expected {
                        if expected != &thread {
                            return Err(application_error(
                                actor,
                                InteractiveOperation::DiscoverBinding,
                                format!(
                                    "resume published thread {} instead of retained thread {}",
                                    thread.0, expected.0
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

fn developer_instructions(root: bool, mode: &InteractiveLaunchMode) -> String {
    if root {
        let continuity = if matches!(mode, InteractiveLaunchMode::Resume(_)) {
            " This is a new actor incarnation attached to a retained conversation. Previous actor handles, workers, pending exits, inbox messages, and resident Haskell state were not restored; reconcile through the current actor tools before acting on transcript references."
        } else {
            ""
        };
        format!("You are a Tidepool root actor. Orchestrate through supervised workers instead of implementing changes in the shared source checkout. Your actor-scoped MCP tools define the typed actor-control protocol; native coding tools remain a separate execution surface. Rust owns process and actor lifecycle. Start workers, then continue only immediately runnable orchestration. WorkerPending is a cooperative yield signal, not an invitation to poll: never sleep or repeatedly call collect_worker. When no other work is runnable, end the current turn. Shoal will initiate a new turn after a child lifecycle transition; on that informational wake, call collect_worker once for the exact typed result. A delayed wake for an already acknowledged worker requires no action.{continuity}")
    } else {
        "You are a Tidepool worker actor. Your process working directory is an owned retained git worktree; never edit the parent checkout. Your actor-scoped MCP tools define the typed assignment and completion protocol; native coding tools remain a separate execution surface. Retrieve your assignment, do the work, commit coherent changes when the assignment calls for edits, then call finish_work exactly once with its typed result. Rust owns process and actor lifecycle.".into()
    }
}

fn initial_prompt(root: bool) -> Option<String> {
    if root {
        None
    } else {
        Some(
            "Initialize this Tidepool worker through its typed tools. Retrieve the typed startup assignment, complete it, and submit the result with finish_work."
                .into(),
        )
    }
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
    use tidepool_actor::ResidentHostParkedKind;
    use tidepool_agent::{
        AgentBackendError, InteractiveAgentCommand, InteractiveAgentSpec, InteractiveFuture,
    };
    use tidepool_testing::eval_harness;
    use tidepool_worktree::WorktreeSpec;

    #[tokio::test]
    async fn idle_application_waits_for_its_first_real_conversation_binding() {
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
        tidepool_agent::persist_interactive_binding(&path, thread.clone())
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), &mut binding)
                .await
                .unwrap()
                .unwrap(),
            thread
        );
        session.kill().await.unwrap();
    }

    #[test]
    fn root_starts_idle_while_workers_receive_their_assignment_kickoff() {
        assert_eq!(initial_prompt(true), None);
        assert!(initial_prompt(false)
            .expect("worker kickoff")
            .contains("Retrieve the typed startup assignment"));

        let resumed = developer_instructions(
            true,
            &InteractiveLaunchMode::Resume(BackendThreadId("retained-thread".into())),
        );
        assert!(resumed.contains("Previous actor handles"));
        assert!(resumed.contains("were not restored"));
        assert!(resumed.contains("WorkerPending is a cooperative yield signal"));
        assert!(resumed.contains("never sleep or repeatedly call collect_worker"));
    }

    struct ScriptedPush {
        fail: std::sync::atomic::AtomicBool,
        messages: std::sync::Mutex<Vec<String>>,
    }

    impl InteractiveAgentBackend for ScriptedPush {
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
            _thread: &'a BackendThreadId,
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
            _thread: &'a BackendThreadId,
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
        inbox.publish("child completed".into()).expect("publish");
        let backend = ScriptedPush {
            fail: std::sync::atomic::AtomicBool::new(true),
            messages: std::sync::Mutex::new(Vec::new()),
        };
        let thread = BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into());

        assert!(deliver_pending(&inbox, &thread, &backend, root.path())
            .await
            .is_err());
        assert_eq!(inbox.pending().expect("pending after refusal").len(), 1);

        backend
            .fail
            .store(false, std::sync::atomic::Ordering::SeqCst);
        deliver_pending(&inbox, &thread, &backend, root.path())
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
            stop_retired_mcp_service(actor, &mut service, Duration::ZERO),
            stop_retired_delivery(actor, &mut delivery, Duration::ZERO),
        );

        assert!(service.is_finished());
        assert!(delivery.is_finished());
    }

    #[test]
    fn worker_workspaces_are_distinct_managed_worktrees_outside_the_source_checkout() {
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
        assert_eq!(
            std::fs::read_to_string(repository.path().join("README.md")).unwrap(),
            "source\n"
        );
    }

    #[tokio::test]
    async fn bundled_devswarm_policy_installs_the_root_tool_surface() {
        eval_harness::require_extract();
        let repository = tidepool_worktree::testing::TestRepo::init().unwrap();
        repository
            .writer()
            .commit_file("README.md", "source\n", "seed")
            .unwrap();
        let workspace = repository.path().to_path_buf();
        let runtime = tempfile::tempdir().unwrap();
        let config = ActorHostConfig {
            policy_root: crate::haskell_sources::ensure_actor_policy().unwrap(),
            workspace: workspace.clone(),
            run_root: runtime.path().join("run"),
            root_binding_path: runtime.path().join("root-binding.json"),
            proxy_program: "unused-in-compile-test".into(),
            proxy_args: vec!["proxy".into()],
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
        let registry = ActorRegistry::new();
        let (source, root) =
            compile_root(&config, session_root.path(), worktrees, authority.clone())
                .expect("compile root policy");
        let mut host = ResidentActorHost::new(
            registry,
            source,
            Arc::new(NoResidentProvider),
            None,
            ResidentLifecyclePolicy::default(),
        )
        .expect("construct host");
        let mut deployments = host.take_deployments().expect("take deployment stream");
        let actor = host.launch_root(root).await.expect("launch root");
        authority.install_root(actor.into());
        let report = host.run_until_idle().await.expect("install policy");
        assert!(report.failures.is_empty());
        assert_eq!(report.parked[&ResidentHostParkedKind::McpPolicy], 1);
        let ResidentActorDeployment::PolicyInstalled(root_installation) =
            deployments.try_recv().expect("root policy installation")
        else {
            panic!("root policy retired before installation");
        };
        assert_eq!(root_installation.actor, actor);
        let policy = root_installation.policy;
        let names = policy
            .declarations()
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "actor_status",
                "spawn_worker",
                "list_workers",
                "collect_worker",
                "ack_worker"
            ]
        );
        assert!(policy
            .declarations()
            .iter()
            .all(|declaration| declaration.output_schema.is_some()));
        let collect_worker = policy
            .declarations()
            .iter()
            .find(|declaration| declaration.name == "collect_worker")
            .expect("collect worker declaration");
        assert_eq!(collect_worker.kind, tidepool_agent::ToolKind::Update);
        assert!(collect_worker.description.contains("do not sleep or poll"));
        assert!(collect_worker
            .description
            .contains("Shoal will wake the root"));

        let server = DynamicMcpServer::from_resident_policy(policy).expect("root MCP server");
        let control = host.control();
        let (request_shutdown, shutdown_requested) = tokio::sync::oneshot::channel();
        let hosted = tokio::spawn(host.run_until_shutdown(async move {
            let _ = shutdown_requested.await;
        }));
        let arguments = serde_json::json!({
            "workKey": "review-1",
            "assignment": "inspect one focused boundary"
        })
        .as_object()
        .expect("object arguments")
        .clone();
        let result = server
            .dispatch_tool("spawn_worker", arguments)
            .await
            .expect("spawn tool transport");
        if result.is_error.unwrap_or(false) {
            request_shutdown.send(()).expect("request shutdown");
            let report = hosted.await.expect("host task").expect("shutdown host");
            panic!("{result:?}; host failures: {:?}", report.run.failures);
        }
        let started = result.structured_content.expect("typed worker acceptance");
        assert_eq!(started["tag"], "WorkerAccepted", "{started}");
        assert_eq!(started["workKey"], "review-1");
        let worker_handle = started["worker"].clone();
        let retry = server
            .dispatch_tool(
                "spawn_worker",
                serde_json::json!({
                    "workKey": "review-1",
                    "assignment": "inspect one focused boundary"
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .await
            .expect("retry identical spawn");
        assert_eq!(retry.structured_content, Some(started.clone()));
        let conflict = server
            .dispatch_tool(
                "spawn_worker",
                serde_json::json!({
                    "workKey": "review-1",
                    "assignment": "a changed assignment must not attach"
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .await
            .expect("conflicting spawn");
        assert_eq!(
            conflict.structured_content,
            Some(serde_json::json!({
                "tag": "WorkerKeyConflict",
                "workKey": "review-1"
            }))
        );
        let worker = tokio::time::timeout(Duration::from_secs(1), deployments.recv())
            .await
            .expect("worker installation timeout")
            .expect("worker policy installation");
        let ResidentActorDeployment::PolicyInstalled(worker) = worker else {
            panic!("worker policy retired before installation");
        };
        assert_eq!(worker.launch_worktrees.len(), 1);
        assert_ne!(worker.actor, actor);
        let worker_tree = WorktreeId::from_raw(worker.launch_worktrees[0].clone());
        let worker_principal = WorktreePrincipal::exact_actor(
            &runtime_namespace(session_root.path()),
            worker.actor.id.0,
            worker.actor.incarnation.0,
        );
        let worker_binding = bindings
            .lock()
            .bind(&worker_tree, &worker_principal, current_time_ms())
            .expect("bind exact worker before it observes submission");
        let worker_names = worker
            .policy
            .declarations()
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            worker_names,
            ["actor_status", "current_assignment", "finish_work"]
        );
        assert_eq!(
            worker
                .policy
                .declarations()
                .iter()
                .find(|declaration| declaration.name == "finish_work")
                .expect("finish declaration")
                .kind,
            tidepool_agent::ToolKind::Finish
        );
        let worker_actor = worker.actor;
        let worker_server =
            DynamicMcpServer::from_resident_policy(worker.policy).expect("worker MCP server");
        let pending_collect = server
            .dispatch_tool(
                "collect_worker",
                serde_json::json!({"worker": worker_handle.clone()})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .expect("nonblocking pending collection");
        assert_eq!(
            pending_collect.structured_content,
            Some(serde_json::json!({
                "tag": "WorkerPending",
                "worker": worker_handle.clone()
            }))
        );
        let assignment = worker_server
            .dispatch_tool("current_assignment", serde_json::Map::new())
            .await
            .expect("read typed assignment");
        assert_eq!(
            assignment.structured_content,
            Some(serde_json::json!({
                "assignment": "inspect one focused boundary"
            }))
        );
        let finish = serde_json::json!({
            "summary": "boundary is clean",
            "evidence": ["focused test"]
        })
        .as_object()
        .expect("finish arguments")
        .clone();
        let finished = worker_server
            .dispatch_tool("finish_work", finish)
            .await
            .expect("finish worker");
        assert_eq!(
            finished.structured_content,
            Some(serde_json::json!({"accepted": true}))
        );
        let retired = tokio::time::timeout(Duration::from_secs(1), deployments.recv())
            .await
            .expect("worker retirement timeout")
            .expect("worker retirement");
        assert!(matches!(
            retired,
            ResidentActorDeployment::Retired { actor, terminal }
                if actor == worker_actor && terminal.kind == ActorExitKind::Completed
        ));
        worker_binding
            .complete(&mut bindings.lock())
            .expect("settle worker binding");

        let collect = serde_json::json!({"worker": worker_handle.clone()})
            .as_object()
            .expect("collect arguments")
            .clone();
        let collected = server
            .dispatch_tool("collect_worker", collect)
            .await
            .expect("collect worker");
        let collected = collected.structured_content.expect("typed collection");
        assert_eq!(collected["tag"], "WorkerCollected");
        assert_eq!(collected["outcome"]["tag"], "WorkCompleted");
        assert_eq!(
            collected["outcome"]["receipt"]["authoredReport"]["summary"],
            "boundary is clean"
        );
        let replayed = server
            .dispatch_tool(
                "collect_worker",
                serde_json::json!({"worker": worker_handle.clone()})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .expect("replay collection")
            .structured_content
            .expect("typed replay");
        assert_eq!(replayed, collected);
        assert_eq!(
            collected["outcome"]["receipt"]["repository"]["workingState"]["changes"]["staged"],
            serde_json::json!([])
        );
        let pending = server
            .dispatch_tool("list_workers", serde_json::Map::new())
            .await
            .expect("list workers");
        assert_eq!(
            pending.structured_content,
            Some(serde_json::json!({
                "workers": [{
                    "workKey": "review-1",
                    "worker": worker_handle,
                    "phase": "WorkerCollectedPhase"
                }]
            }))
        );
        let acknowledged = server
            .dispatch_tool(
                "ack_worker",
                serde_json::json!({"worker": worker_handle.clone()})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .expect("acknowledge collection");
        assert_eq!(
            acknowledged.structured_content,
            Some(serde_json::json!({
                "tag": "WorkerAcknowledged",
                "worker": worker_handle.clone()
            }))
        );
        let after_ack = server
            .dispatch_tool(
                "collect_worker",
                serde_json::json!({"worker": worker_handle})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .expect("collect after acknowledgement");
        assert_eq!(
            after_ack.structured_content.unwrap()["tag"],
            "WorkerCollectionAcknowledged"
        );
        let acknowledged_retry = server
            .dispatch_tool(
                "spawn_worker",
                serde_json::json!({
                    "workKey": "review-1",
                    "assignment": "inspect one focused boundary"
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .await
            .expect("retry acknowledged spawn")
            .structured_content
            .expect("typed acknowledged retry");
        assert_eq!(acknowledged_retry["tag"], "WorkerAlreadyAcknowledged");

        let failed_start = server
            .dispatch_tool(
                "spawn_worker",
                serde_json::json!({
                    "workKey": "launch-failure",
                    "assignment": "this application will fail before launch"
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .await
            .expect("accept failure probe")
            .structured_content
            .expect("typed failure probe acceptance");
        let failed_handle = failed_start["worker"].clone();
        let failed_installation = deployments
            .recv()
            .await
            .expect("failure probe installation");
        let ResidentActorDeployment::PolicyInstalled(failed_installation) = failed_installation
        else {
            panic!("failure probe retired before installation");
        };
        assert_eq!(
            control
                .fail_external_application(
                    failed_installation.actor,
                    ExternalApplicationFailure {
                        class: ExternalApplicationFailureClass::ProcessLaunch,
                        detail: "scripted launch refusal".into(),
                    },
                )
                .await
                .expect("report exact application failure"),
            tidepool_actor::ExternalFailureDisposition::Applied
        );
        let failed_retirement = deployments.recv().await.expect("failed child retirement");
        assert!(matches!(
            failed_retirement,
            ResidentActorDeployment::Retired { actor, terminal }
                if actor == failed_installation.actor && terminal.kind == ActorExitKind::Failed
        ));
        let failed_collection = server
            .dispatch_tool(
                "collect_worker",
                serde_json::json!({"worker": failed_handle})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .expect("collect application failure")
            .structured_content
            .expect("typed application failure");
        assert_eq!(failed_collection["outcome"]["tag"], "WorkFailed");

        let recovery = server
            .dispatch_tool(
                "spawn_worker",
                serde_json::json!({
                    "workKey": "after-failure",
                    "assignment": "prove the root remains operational"
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .await
            .expect("root starts another child after failure");
        assert_eq!(
            recovery.structured_content.unwrap()["tag"],
            "WorkerAccepted"
        );
        assert!(matches!(
            deployments.recv().await,
            Some(ResidentActorDeployment::PolicyInstalled(_))
        ));
        request_shutdown.send(()).expect("request shutdown");
        hosted.await.expect("host task").expect("shutdown host");
    }
}
