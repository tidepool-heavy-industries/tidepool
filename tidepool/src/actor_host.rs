//! Composition root for the first actor-native interactive swarm.
//!
//! The daemon owns resident Haskell scheduling and exact actor lifecycle. One
//! stock interactive agent is attached to each installed Haskell MCP policy;
//! tmux is process ownership and observability, never message transport.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use frunk::{hlist, HCons, HNil};
use parking_lot::Mutex;
use rmcp::ServiceExt;
use tidepool_actor::{
    spawn_resident_root, ActorDescriptor, ActorEffectProfile, ActorExitKind, ActorPlacement,
    ActorRef, ActorTerminal, ActorWorkbenchSource, ExternalApplicationFailure,
    ExternalApplicationFailureClass, ExternalFailureDisposition, LocalActorRef,
    LocalResidentDeployment, LocalResidentInstallation, ResidentActorRoot,
};
use tidepool_agent::{
    native_interactive_backend, read_interactive_binding, BackendThreadId, InteractiveAgentBackend,
    InteractiveAgentSpec, InteractiveLaunchMode, InteractiveMcpServer, InteractiveNativeSandbox,
    InteractiveProxyBinding, ReasoningEffort,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_handlers::{ActorWorktreeAuthority, ActorWorktreeHandler, WorktreeHandler};
use tidepool_mcp::{CapturedOutput, DynamicMcpServer};
use tidepool_model::{ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse};
use tidepool_node::{
    accept_proxy, DurableInbox, NodeCredential, ProcessInvocation, ProcessMountBoundary,
    TmuxLaunch, TmuxPaneId, TmuxSession, BUBBLEWRAP_PROGRAM,
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

/// Every interactive actor sees its own repository at this path. Bubblewrap
/// mount namespaces make the shared name safe across concurrent actors, while
/// Codex needs only one persisted project-trust decision.
pub(crate) const ACTOR_PROJECT_ROOT: &str = "/tmp/tidepool-actor-workspace";

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
    local_actor: LocalActorRef,
    label: String,
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

struct PendingInteractiveLaunch {
    cancel: oneshot::Sender<()>,
    worker_handle: Option<String>,
    label: String,
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
    config: ActorHostConfig,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
) -> Result<(), Box<dyn std::error::Error>> {
    let run_root = config.run_root.clone();
    std::fs::create_dir_all(&run_root)?;

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
    let (root_actor, mut root_task, deployments) =
        spawn_resident_root(source, Arc::new(NoResidentProvider), None, root).await?;
    worktree_authority.install_root(root_actor.identity().into());

    let tmux = TmuxSession::new(&config.tmux_session)?;
    if !tmux.exists().await? {
        return Err(runtime_error(format!(
            "Shoal tmux session {:?} does not exist",
            config.tmux_session
        )));
    }
    let backend = native_interactive_backend();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let mut applications_task = tokio::spawn(run_interactive_applications(
        deployments,
        InteractiveFleet {
            root: root_actor.clone(),
            config,
            run_root,
            tmux,
            backend,
            worktrees,
            bindings,
            readiness,
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
            root_actor
                .shutdown(ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "Shoal operator requested shutdown".into(),
                })
                .await?;
            root_task.await.map_err(join_error)?;
            await_applications(&mut applications_task).await?;
        }
        FirstStop::Root => {
            await_applications(&mut applications_task).await?;
        }
        FirstStop::Applications(result) => {
            result.map_err(runtime_error)?;
            root_actor
                .shutdown(ActorTerminal {
                    kind: ActorExitKind::Failed,
                    summary: "Shoal interactive application fleet stopped".into(),
                })
                .await?;
            root_task.await.map_err(join_error)?;
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
    })
    .map_err(render_root_compile_failure)?
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => {
            return Err(runtime_error(format!(
                "root policy is not an expression: {other:?}"
            )))
        }
    };

    let session = fresh_session_id();
    let library = SessionLib::open(
        session,
        &session_root,
        tidepool_mcp::session_decl_module_env(&declarations, false),
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
        "devswarm_root_policy",
        &compiled.expr,
        &compiled.table,
        &compiled.asks,
    )?;
    let descriptor = ActorDescriptor::new(
        "devswarm-root",
        ["AgentSession", "Actor", "Worktree"],
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
                    label: "<actor-policy>",
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
    runtime_error(format!("root actor policy compilation failed:\n{detail}"))
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
    let mut retired_correlations: HashMap<ActorRef, (String, Option<String>)> = HashMap::new();
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
                        let worker_handle = (actor != root_identity)
                            .then_some(installation.launch_worktrees.as_slice())
                            .and_then(|worktrees| match worktrees {
                                [worktree] => Some(worktree.clone()),
                                _ => None,
                            });
                        let (cancel, cancelled) = oneshot::channel();
                        let previous = pending_launches.insert(
                            actor,
                            PendingInteractiveLaunch {
                                cancel,
                                worker_handle,
                                label: installation.label.clone(),
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
                    LocalResidentDeployment::Retired { actor, terminal } => {
                        let pending = pending_launches.remove(&actor);
                        if let Some(pending) = pending {
                            let _ = pending.cancel.send(());
                            retired_correlations.insert(
                                actor,
                                (pending.label, pending.worker_handle),
                            );
                        }
                        if let Some(index) = deployments.iter().position(|app| app.actor == actor) {
                            let deployment = deployments.swap_remove(index);
                            retired_correlations.entry(actor).or_insert_with(|| {
                                (
                                    deployment.label.clone(),
                                    deployment
                                        .worktree
                                        .as_ref()
                                        .map(|worktree| worktree.id().to_string()),
                                )
                            });
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
                    LocalResidentDeployment::ChildExited(notice) => {
                        let retired = retired_correlations.remove(&notice.child.identity());
                        if let Some(notification) = prepare_owner_notification(
                            &notice,
                            &deployments,
                            retired.as_ref(),
                        ) {
                            notifications.spawn(publish_owner_notification(notification));
                        }
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
                            let _ = readiness.send(ActorHostReadiness::AwaitingInput { root: root_identity });
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

/// Serve one actor policy for the actor's lifetime, not for the lifetime of a
/// particular proxy process. Codex legitimately reconnects while refreshing
/// its MCP inventory; a transport disconnect must not make the resident actor
/// lose its interaction surface.
async fn serve_actor_mcp_endpoint(
    listener: UnixListener,
    server: DynamicMcpServer,
    actor: ActorRef,
    credential: NodeCredential,
) -> Result<(), InteractiveApplicationError> {
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| application_error(actor, InteractiveOperation::AcceptProxy, error))?;
        let connection = async {
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
                .clone()
                .serve((read, write))
                .await
                .map_err(|error| application_error(actor, InteractiveOperation::ServeMcp, error))?
                .waiting()
                .await
                .map_err(|error| application_error(actor, InteractiveOperation::ServeMcp, error))?;
            Ok::<_, InteractiveApplicationError>(())
        }
        .await;
        if let Err(error) = connection {
            tracing::warn!(actor = ?actor, %error, "actor MCP connection ended with an error; accepting a replacement");
        }
    }
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
    let process_boundary = ProcessMountBoundary::new(
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
    if cancelled.try_recv().is_ok() {
        return Ok(None);
    }
    let actor_root = run_root.join(format!(
        "{}-{}",
        actor_identity.id.0, actor_identity.incarnation.0
    ));
    std::fs::create_dir_all(&actor_root).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
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
    std::fs::create_dir_all(&socket_root).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let endpoint = socket_root.join("mcp.sock");
    let listener = UnixListener::bind(&endpoint).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::BindProxy, error)
    })?;
    let credential = NodeCredential(uuid::Uuid::new_v4().to_string());
    let binding_path = if actor_identity == root {
        config.root_binding_path.clone()
    } else {
        actor_root.join("binding.json")
    };
    let inbox = Arc::new(
        DurableInbox::<String>::open(
            actor_root.join("inbox.jsonl"),
            actor_root.join("inbox.cursor"),
        )
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?,
    );
    let binding = InteractiveProxyBinding {
        actor: actor_identity,
        endpoint,
        credential: credential.clone(),
        binding_path: binding_path.clone(),
        workspace: agent_workspace.clone(),
    };
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
    let proxy_environment = binding.environment();
    let spec = InteractiveAgentSpec {
        mode: launch_mode,
        model: config.model.clone(),
        effort: config.effort,
        developer_instructions,
        initial_prompt: installation.initial_user_message.clone(),
        native_sandbox: InteractiveNativeSandbox::HostMountBoundary,
        mcp: InteractiveMcpServer {
            name: "tidepool_actor".into(),
            command: config.proxy_program.clone(),
            args: config.proxy_args.clone(),
            cwd: agent_workspace.to_string_lossy().into_owned(),
            forward_env: proxy_environment.keys().cloned().collect(),
            required: true,
        },
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
    let server = DynamicMcpServer::from_resident_policy(installation.policy).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::BuildPolicy, error)
    })?;
    let service = tokio::spawn(serve_actor_mcp_endpoint(
        listener,
        server,
        actor_identity,
        credential,
    ));
    if cancelled.try_recv().is_ok() {
        service.abort();
        let _ = service.await;
        let _ = std::fs::remove_dir_all(&socket_root);
        return Ok(None);
    }
    let launch_environment = actor_launch_environment(
        config.pane_environment.clone(),
        proxy_environment,
        actor_identity == root,
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
    Ok(Some(LaunchedInteractiveApplication {
        deployment: InteractiveDeployment {
            actor: actor_identity,
            local_actor: actor,
            label: installation.label,
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
    actor_local: BTreeMap<String, String>,
    is_root: bool,
) -> ActorLaunchEnvironment {
    inherited.extend(actor_local);
    let unset = workspace_local_toolchain_pins(is_root);
    inherited.retain(|name, _| !unset.contains(name));
    ActorLaunchEnvironment {
        set: inherited,
        unset,
    }
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
    notice: &tidepool_actor::ChildExitNotice,
    deployments: &[InteractiveDeployment],
    retired: Option<&(String, Option<String>)>,
) -> Option<OwnerNotification> {
    let actor = notice.child.identity();
    let terminal = &notice.terminal;
    let owner_application = deployments.iter().find(|app| app.actor == notice.owner)?;
    let label = deployments
        .iter()
        .find(|application| application.actor == actor)
        .map(|application| application.label.as_str())
        .or_else(|| retired.map(|(label, _)| label.as_str()))
        .unwrap_or("child");
    let pending_worker_handle = retired.and_then(|(_, handle)| handle.as_deref());
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
                " The Haskell handle is `WorkerHandle \"{}\"`.",
                worktree.id()
            )
        })
        .or_else(|| {
            pending_worker_handle
                .map(|worktree| format!(" The Haskell handle is `WorkerHandle \"{worktree}\"`."))
        })
        .unwrap_or_default();
    let message = format!(
        "Tidepool lifecycle: child {:?} ({:?}) {kind}: {}.{worker_handle} This wake is the cue to use `session_run`, call `collectWorkerResult handle sessionInput` once, and return its updated records with `complete`; it carries correlation only. If that worker was already acknowledged, no further action is required.",
        label,
        actor,
        terminal.summary
    );
    Some(OwnerNotification {
        inbox: Arc::clone(&owner_application.inbox),
        message,
    })
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
        stop_retired_mcp_service(deployment.actor, &mut deployment.service,),
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

/// Stop the actor-lifetime MCP listener after its application pane is gone.
/// Individual proxy connections are deliberately replaceable, so normal
/// endpoint completion is not a useful retirement signal.
async fn stop_retired_mcp_service(
    actor: ActorRef,
    service: &mut tokio::task::JoinHandle<Result<(), InteractiveApplicationError>>,
) {
    service.abort();
    if let Err(error) = service.await {
        if !error.is_cancelled() {
            tracing::warn!(actor = ?actor, %error, "retired actor MCP service task failed");
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

fn developer_instructions(root: bool, owns_worktree: bool, mode: &InteractiveLaunchMode) -> String {
    if root {
        let continuity = if matches!(mode, InteractiveLaunchMode::Resume(_)) {
            " This is a new actor incarnation attached to a retained conversation. Previous actor handles, workers, pending exits, inbox messages, and resident Haskell state were not restored; old Haskell bindings are dead. Reconcile through the current session before acting on transcript references."
        } else {
            ""
        };
        format!("You are a Tidepool root actor. Orchestrate through supervised workers instead of implementing changes in the shared source checkout. `tidepool_actor.session_run` is your primary GHCi-like orchestration surface: persistent Haskell declarations and live values survive calls, and `sessionInput :: [WorkerRecord]` is the current exact root state. Use `startWorker`, `listWorkerState`, `collectWorkerResult`, and `acknowledgeWorker`; return the next root state with `complete records`. Scaffold stable interfaces first, integrate that candidate, then start every independent seam before awaiting results. The checkout is writable so you can review and integrate accepted candidates. Worker worktrees share this repository's ordinary object and branch namespace: inspect submitted OIDs or branches directly. For dependent work, integrate the predecessor before starting its successor. Native coding tools remain a separate execution surface; Rust owns process and lifecycle. `WorkerPending` is a cooperative yield signal: never sleep or poll. End the turn when nothing else is runnable; Shoal will initiate a new turn after child lifecycle transitions.{continuity}")
    } else if owns_worktree {
        "You are a Tidepool worker actor. Your process working directory is an owned retained linked Git worktree. Its working files, index, and HEAD are isolated; commits, branches, refs, configuration, and objects share the root repository's ordinary Git namespace. Use ordinary Git workflows freely inside this worktree. The initial User message is your Haskell-authored assignment, also mounted as `sessionInput :: Text`. Use native coding tools for repository work and `tidepool_actor.session_run` as the GHCi-like typed completion surface. Finish exactly once with Haskell such as `complete (WorkerReport { summary = ..., evidence = [...] })`; Rust then observes repository truth and owns lifecycle.".into()
    } else {
        "You are a Tidepool actor with read-only access to the shared source checkout and no owned coding worktree. Use `tidepool_actor.session_run` as your primary GHCi-like actor surface. The initial User message, when present, is Haskell-authored and mounted as `sessionInput`. You may define typed protocols, orchestrate children permitted by your effect profile, inspect the repository, and return the session's expected value with `complete`; do not claim or attempt source-checkout mutation authority.".into()
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
        ToolDeclaration, ToolKind,
    };
    use tidepool_testing::eval_harness;
    use tidepool_worktree::WorktreeSpec;

    #[tokio::test]
    async fn actor_mcp_inventory_survives_a_proxy_reconnect() {
        let actor = ActorRef::first(tidepool_actor::ActorId(7));
        let credential = NodeCredential("reconnect-secret".into());
        let server = DynamicMcpServer::new(
            vec![ToolDeclaration {
                name: "session_run".into(),
                description: "Run persistent Haskell".into(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: Some(serde_json::json!({"type": "object"})),
                kind: ToolKind::Call,
            }],
            Some("Persistent actor session".into()),
            |_, _| Box::pin(async { Ok(serde_json::json!({})) }),
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let endpoint = root.path().join("actor.sock");
        let listener = UnixListener::bind(&endpoint).unwrap();
        let service = tokio::spawn(serve_actor_mcp_endpoint(
            listener,
            server,
            actor,
            credential.clone(),
        ));
        let handshake = tidepool_node::NodeHandshake::current(actor, credential);

        for _ in 0..2 {
            let stream = tidepool_node::connect_proxy(&endpoint, &handshake)
                .await
                .unwrap();
            let client = ().serve(stream).await.unwrap();
            let inventory = client.peer().list_tools(None).await.unwrap();
            assert_eq!(inventory.tools.len(), 1);
            assert_eq!(inventory.tools[0].name, "session_run");
            client.cancel().await.unwrap();
        }

        service.abort();
        let _ = service.await;
    }

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
    fn root_instructions_preserve_idle_and_resume_contracts() {
        let resumed = developer_instructions(
            true,
            false,
            &InteractiveLaunchMode::Resume(BackendThreadId("retained-thread".into())),
        );
        assert!(resumed.contains("Previous actor handles"));
        assert!(resumed.contains("were not restored"));
        assert!(resumed.contains("`WorkerPending` is a cooperative yield signal"));
        assert!(resumed.contains("never sleep or poll"));
        assert!(resumed.contains("inspect submitted OIDs or branches directly"));
        assert!(resumed.contains("start every independent seam before awaiting results"));
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
        assert!(instructions.contains("read-only access to the shared source checkout"));
        assert!(instructions.contains("orchestrate children"));
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
            BTreeMap::from([("TIDEPOOL_ACTOR_PROXY_ENDPOINT".into(), "socket".into())]),
            false,
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
        assert_eq!(launch.set.get("PATH").map(String::as_str), Some("/bin"));
        assert_eq!(
            launch
                .set
                .get("TIDEPOOL_ACTOR_PROXY_ENDPOINT")
                .map(String::as_str),
            Some("socket")
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
            BTreeMap::new(),
            true,
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
            stop_retired_mcp_service(actor, &mut service),
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
    async fn bundled_devswarm_exposes_haskell_session_and_recursive_actor_fanout() {
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
        let (source, root) =
            compile_root(&config, session_root.path(), worktrees, authority.clone())
                .expect("compile root policy");
        let (actor, hosted, mut deployments) =
            spawn_resident_root(source, Arc::new(NoResidentProvider), None, root)
                .await
                .expect("spawn resident root");
        authority.install_root(actor.identity().into());
        let LocalResidentDeployment::PolicyInstalled(root_installation) =
            deployments.try_recv().expect("root policy installation")
        else {
            panic!("root policy retired before installation");
        };
        assert_eq!(root_installation.actor.identity(), actor.identity());
        assert_eq!(root_installation.initial_user_message, None);
        let policy = root_installation.policy;
        let names = policy
            .declarations()
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["session_run"]);
        assert!(policy
            .declarations()
            .iter()
            .all(|declaration| declaration.output_schema.is_some()));
        let session_run = &policy.declarations()[0];
        assert_eq!(session_run.kind, tidepool_agent::ToolKind::Call);
        assert!(session_run.description.contains("GHCi-style"));

        let server = DynamicMcpServer::from_resident_policy(policy).expect("root MCP server");
        let bound_start = server
            .dispatch_tool(
                "session_run",
                serde_json::json!({
                    "items": [
                        "input",
                        "waveAssignment <- pure (\"inspect one focused boundary\" :: Text)",
                        "firstWorker <- startWorker \"review-1\" waveAssignment sessionInput"
                    ],
                    "input": {"wave": "parallel"}
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .await
            .expect("bind the first started worker");
        assert!(!bound_start.is_error.unwrap_or(false), "{bound_start:?}");
        assert_eq!(
            bound_start.structured_content.as_ref().unwrap()["status"],
            "committed",
            "{bound_start:?}"
        );
        let result = server
            .dispatch_tool(
                "session_run",
                serde_json::json!({
                    "items": [
                        "firstWorker",
                        "do { (_, next) <- startWorker \"review-2\" \"inspect a disjoint boundary\" (snd firstWorker); complete next }"
                    ]
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .await
            .expect("reuse the effectful binding in the next root Haskell session");
        assert!(!result.is_error.unwrap_or(false), "{result:?}");
        assert_eq!(
            result.structured_content.as_ref().unwrap()["status"],
            "completed",
            "{result:?}"
        );
        let reopened = server
            .dispatch_tool(
                "session_run",
                serde_json::json!({
                    "items": ["sessionInput"]
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .await
            .expect("force state returned into the next root Haskell session");
        assert!(
            !reopened.is_error.unwrap_or(false),
            "reopened root session lost its live input: {reopened:?}"
        );
        assert_eq!(
            reopened.structured_content.as_ref().unwrap()["status"],
            "committed",
            "{reopened:?}"
        );
        let mut worker_prompts = Vec::new();
        let mut recursive_worker = None;
        for _ in 0..2 {
            let worker = tokio::time::timeout(Duration::from_secs(30), deployments.recv())
                .await
                .expect("worker installation timeout")
                .expect("worker session installation");
            let LocalResidentDeployment::PolicyInstalled(worker) = worker else {
                panic!("worker retired before session installation");
            };
            assert_eq!(worker.launch_worktrees.len(), 1);
            worker_prompts.push(worker.initial_user_message.clone().unwrap());
            assert_eq!(
                worker
                    .policy
                    .declarations()
                    .iter()
                    .map(|declaration| declaration.name.as_str())
                    .collect::<Vec<_>>(),
                ["session_run"]
            );
            if recursive_worker.is_none() {
                recursive_worker = Some(worker);
            }
        }
        worker_prompts.sort();
        assert_eq!(
            worker_prompts,
            [
                "inspect a disjoint boundary",
                "inspect one focused boundary"
            ]
        );
        let recursive_worker = recursive_worker.expect("one recursive worker");
        let worker_tree = WorktreeId::from_raw(recursive_worker.launch_worktrees[0].clone());
        let worker_principal = WorktreePrincipal::exact_actor(
            &runtime_namespace(session_root.path()),
            recursive_worker.actor.identity().id.0,
            recursive_worker.actor.identity().incarnation.0,
        );
        let worker_binding = bindings
            .lock()
            .bind(&worker_tree, &worker_principal, current_time_ms())
            .expect("bind exact worker before it observes submission");
        let recursive_server = DynamicMcpServer::from_resident_policy(recursive_worker.policy)
            .expect("recursive worker MCP server");
        let recursive_result = recursive_server
            .dispatch_tool(
                "session_run",
                serde_json::json!({
                    "items": [
                        "data NestedProtocol result",
                        "nestedDefinition :: ActorDefinition () NestedProtocol ()\nnestedDefinition = ActorDefinition { label = \"nested-review\", effectProfile = ReadOnly, initialization = pure, behavior = \\_ _ -> agentSession (Just \"inspect nested boundary\") (), visibleToChild = [], onShutdown = const (pure ()) }",
                        "do { _ <- startActor nestedDefinition (); complete (WorkerReport { summary = \"spawned nested actor\", evidence = [] }) }"
                    ]
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .await
            .expect("run recursive worker session");
        assert!(
            !recursive_result.is_error.unwrap_or(false),
            "{recursive_result:?}"
        );
        assert_eq!(
            recursive_result.structured_content.as_ref().unwrap()["status"],
            "completed",
            "{recursive_result:?}"
        );
        let collect_source = format!(
            "do {{ (_, next) <- collectWorkerResult (WorkerHandle \"{}\") sessionInput; complete next }}",
            worker_tree.as_str()
        );
        let after_child_exit = server
            .dispatch_tool(
                "session_run",
                serde_json::json!({
                    "items": [collect_source]
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .await
            .expect("force carried root state after one child exits");
        assert!(
            !after_child_exit.is_error.unwrap_or(false),
            "a child exit corrupted the root session input: {after_child_exit:?}"
        );
        assert_eq!(
            after_child_exit.structured_content.as_ref().unwrap()["status"],
            "completed",
            "{after_child_exit:?}"
        );
        let nested = tokio::time::timeout(Duration::from_secs(30), deployments.recv())
            .await
            .expect("nested installation timeout")
            .expect("nested session installation");
        let LocalResidentDeployment::PolicyInstalled(nested) = nested else {
            panic!("nested actor retired before session installation");
        };
        assert_eq!(
            nested.initial_user_message.as_deref(),
            Some("inspect nested boundary")
        );
        worker_binding
            .release(&mut bindings.lock())
            .expect("release worker binding");
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "test complete".into(),
            })
            .await
            .expect("shutdown root");
        hosted.await.expect("root actor task");
        assert!(!result.is_error.unwrap_or(false), "{result:?}");
    }
}
