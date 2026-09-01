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

use frunk::HNil;
use rmcp::ServiceExt;
use tidepool_actor::{
    ActorDescriptor, ActorEffectProfile, ActorExitKind, ActorPlacement, ActorRef, ActorRegistry,
    ActorTerminal, ActorWorkbenchSource, ResidentActorDeployment, ResidentActorHost,
    ResidentActorRoot, ResidentLifecyclePolicy, ResidentMcpInstallation,
};
use tidepool_agent::{
    native_interactive_backend, read_interactive_binding, BackendThreadId, InteractiveAgentBackend,
    InteractiveAgentSpec, InteractiveLaunchMode, InteractiveMcpServer, InteractiveProxyBinding,
    ReasoningEffort,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
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
use tidepool_worktree::{GitCli, WorktreeHandle, WorktreeManager, WorktreeRegistry, WorktreeSpec};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

const POLICY_MODULE: &str = "Tidepool.Actors.DevSwarm";
const POLICY_ENTRY: &str = "rootPolicy";
const POLICY_EFFECTS: &str = "RootEffects";
const APPLICATION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
const APPLICATION_TASK_GRACE_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

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
pub struct ActorHostReady {
    pub root: ActorRef,
    pub thread: BackendThreadId,
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
    thread: BackendThreadId,
    inbox: Arc<DurableInbox<String>>,
    delivery_shutdown: Option<oneshot::Sender<()>>,
    delivery: tokio::task::JoinHandle<()>,
    service: tokio::task::JoinHandle<Result<(), InteractiveApplicationError>>,
    socket_root: PathBuf,
    worktree: Option<WorktreeHandle>,
}

struct OwnerNotification {
    inbox: Arc<DurableInbox<String>>,
    message: String,
}

#[derive(Debug, Clone, Copy)]
enum InteractiveOperation {
    CreateWorktree,
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
            Self::CreateWorktree => "create worktree",
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
    readiness: oneshot::Sender<ActorHostReady>,
}

#[derive(Clone)]
struct InteractiveLaunchContext {
    root: ActorRef,
    config: ActorHostConfig,
    run_root: PathBuf,
    tmux: TmuxSession,
    backend: Arc<dyn InteractiveAgentBackend>,
    worktrees: WorktreeManager,
}

pub async fn run(
    config: ActorHostConfig,
    readiness: oneshot::Sender<ActorHostReady>,
) -> Result<(), Box<dyn std::error::Error>> {
    let run_root = config.run_root.clone();
    std::fs::create_dir_all(&run_root)?;

    let registry = ActorRegistry::new();
    let (source, root) = compile_root(&config, &run_root)?;
    let mut host = ResidentActorHost::new(
        registry.clone(),
        source,
        Arc::new(NoResidentProvider),
        None,
        ResidentLifecyclePolicy::default(),
    )?;
    let deployments = host.take_deployments()?;
    let root_actor = host.launch_root(root).await?;

    let tmux = TmuxSession::new(&config.tmux_session)?;
    if !tmux.exists().await? {
        return Err(runtime_error(format!(
            "Shoal tmux session {:?} does not exist",
            config.tmux_session
        )));
    }
    let backend = native_interactive_backend();
    let worktrees = actor_worktree_manager(&config.workspace)?;
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

fn actor_worktree_manager(
    workspace: &Path,
) -> Result<WorktreeManager, tidepool_worktree::WorktreeError> {
    let project = blake3::hash(workspace.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string();
    let root = tidepool_runtime::paths::cache_dir()
        .join("shoal")
        .join("actor-worktrees")
        .join(project);
    actor_worktree_manager_at(&root, workspace)
}

fn actor_worktree_manager_at(
    root: &Path,
    workspace: &Path,
) -> Result<WorktreeManager, tidepool_worktree::WorktreeError> {
    let registry = WorktreeRegistry::open(root.join("registry"))?;
    Ok(WorktreeManager::new(
        GitCli::new(),
        registry,
        root.join("checkouts"),
        workspace,
    ))
}

fn compile_root(
    config: &ActorHostConfig,
    run_root: &Path,
) -> Result<
    (
        ActorWorkbenchSource,
        ResidentActorRoot<HNil, CapturedOutput>,
    ),
    Box<dyn std::error::Error>,
> {
    let declarations = [
        tidepool_mcp::actor_mcp_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::deliberate_decl(),
        tidepool_mcp::fs_read_decl(),
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
        HNil,
        CapturedOutput::new(),
        include.clone(),
        DEFAULT_NURSERY_SIZE,
        Some(library),
    )?;
    machine.set_effect_execution(
        EffectRunPolicy::SuspendAll,
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
        ["ActorMcp", "Actor"],
        ActorPlacement {
            session,
            resource_scope: RealmId::fresh(),
            lexical_scope: ScopeId::ROOT,
        },
    )
    // The bootstrap root currently uses a strict subset of the experimental
    // ReadOnly row and starts only ReadOnly children. Its profile is the spawn
    // ceiling, not a claim about native Codex process authority.
    .with_profile(ActorEffectProfile::ReadOnly);
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
        readiness,
    } = fleet;
    let launch_context = InteractiveLaunchContext {
        root,
        config,
        run_root,
        tmux: tmux.clone(),
        backend,
        worktrees,
    };
    let mut readiness = Some(readiness);
    let mut deployments = Vec::new();
    let mut launches = JoinSet::new();
    let mut pending_launches = HashMap::new();
    let mut retirements = JoinSet::new();
    let mut notifications = JoinSet::new();
    let mut health = tokio::time::interval(Duration::from_secs(1));
    let failure = loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown.clone()) => break None,
            _ = health.tick() => {
                if let Some(deployment) = deployments
                    .iter()
                    .find(|deployment: &&InteractiveDeployment| {
                        deployment.service.is_finished() || deployment.delivery.is_finished()
                    })
                {
                    break Some(format!(
                        "interactive application for actor {:?} exited before host shutdown",
                        deployment.actor
                    ));
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
                            retirements.spawn(async move {
                                retire_interactive_application(deployment, &tmux).await
                            });
                        }
                    }
                }
            }
            launched = launches.join_next(), if !launches.is_empty() => {
                match launched {
                    Some(Ok((actor, Ok(Some(deployment))))) => {
                        pending_launches.remove(&actor);
                        if deployment.actor == root {
                            if let Some(sender) = readiness.take() {
                                let _ = sender.send(ActorHostReady {
                                    root,
                                    thread: deployment.thread.clone(),
                                });
                            }
                        }
                        deployments.push(deployment);
                    }
                    Some(Ok((actor, Ok(None)))) => {
                        pending_launches.remove(&actor);
                    }
                    Some(Ok((actor, Err(error)))) => {
                        pending_launches.remove(&actor);
                        break Some(error.to_string());
                    }
                    Some(Err(error)) => break Some(format!("interactive launch task: {error}")),
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
            if let Ok((_actor, Ok(Some(deployment)))) = result {
                completed.push(deployment);
            }
        }
        completed
    })
    .await;
    match launch_cleanup {
        Ok(completed) => deployments.extend(completed),
        Err(_) => launches.abort_all(),
    }
    for deployment in deployments {
        let tmux = tmux.clone();
        retirements.spawn(async move { retire_interactive_application(deployment, &tmux).await });
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
    mut cancelled: oneshot::Receiver<()>,
) -> Result<Option<InteractiveDeployment>, InteractiveApplicationError> {
    let InteractiveLaunchContext {
        root,
        config,
        run_root,
        tmux,
        backend,
        worktrees,
    } = context;
    let actor = installation.actor;
    let worktree = if actor == root {
        None
    } else {
        let label = format!("shoal-{}-{}", actor.id.0, actor.incarnation.0);
        Some(
            tokio::task::spawn_blocking(move || {
                worktrees.create(&WorktreeSpec::from_current_repository(label))
            })
            .await
            .map_err(|error| application_error(actor, InteractiveOperation::CreateWorktree, error))?
            .map_err(|error| {
                application_error(actor, InteractiveOperation::CreateWorktree, error)
            })?,
        )
    };
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
    let initial_prompt = initial_prompt(actor == root, &launch_mode);
    let proxy_environment = binding.environment();
    let spec = InteractiveAgentSpec {
        mode: launch_mode,
        model: config.model.clone(),
        effort: config.effort,
        developer_instructions,
        initial_prompt: Some(initial_prompt),
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

    let thread = tokio::select! {
        biased;
        _ = &mut cancelled => {
            abandon_interactive_application(&tmux, &pane, service, &socket_root).await;
            return Ok(None);
        }
        result = wait_for_binding(actor, &binding_path, &service) => match result {
            Ok(thread) => thread,
            Err(error) => {
                abandon_interactive_application(&tmux, &pane, service, &socket_root).await;
                return Err(error);
            }
        }
    };
    if let Some(expected) = expected_resume {
        if expected != thread {
            abandon_interactive_application(&tmux, &pane, service, &socket_root).await;
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
    let (delivery_shutdown, stop_delivery) = oneshot::channel();
    let delivery = tokio::spawn(run_delivery_pump(
        actor,
        Arc::clone(&inbox),
        thread.clone(),
        backend,
        workspace,
        stop_delivery,
    ));
    Ok(Some(InteractiveDeployment {
        actor,
        pane,
        thread,
        inbox,
        delivery_shutdown: Some(delivery_shutdown),
        delivery,
        service,
        socket_root,
        worktree,
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
    let workspace = deployments
        .iter()
        .find(|application| application.actor == actor)
        .and_then(|application| application.worktree.as_ref())
        .map(|worktree| {
            format!(
                " Its retained worktree is {} on branch {}.",
                worktree.cwd().display(),
                worktree.branch()
            )
        })
        .unwrap_or_default();
    let message = format!(
        "Tidepool lifecycle: child {:?} ({:?}) {kind}: {}.{workspace} Inspect and await its exact typed result through your actor tools.",
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
) -> Result<(), InteractiveApplicationError> {
    if let Some(shutdown) = deployment.delivery_shutdown.take() {
        let _ = shutdown.send(());
    }
    tmux.kill_pane(&deployment.pane).await.map_err(|error| {
        application_error(deployment.actor, InteractiveOperation::StopProcess, error)
    })?;
    tokio::join!(
        stop_retired_mcp_service(
            deployment.actor,
            &mut deployment.service,
            APPLICATION_TASK_GRACE_TIMEOUT,
        ),
        stop_retired_delivery(
            deployment.actor,
            &mut deployment.delivery,
            APPLICATION_TASK_GRACE_TIMEOUT,
        ),
    );
    let _ = std::fs::remove_dir_all(&deployment.socket_root);
    Ok(())
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

async fn wait_for_binding(
    actor: ActorRef,
    path: &Path,
    service: &tokio::task::JoinHandle<Result<(), InteractiveApplicationError>>,
) -> Result<tidepool_agent::BackendThreadId, InteractiveApplicationError> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if service.is_finished() {
                return Err(application_error(
                    actor,
                    InteractiveOperation::DiscoverBinding,
                    "MCP service stopped before rollout binding",
                ));
            }
            if let Ok(thread) = read_interactive_binding(path).await {
                return Ok(thread);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| {
        application_error(
            actor,
            InteractiveOperation::DiscoverBinding,
            format!("rollout binding timed out at {}", path.display()),
        )
    })?
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
        format!("You are a Tidepool root actor. Orchestrate through supervised workers instead of implementing changes in the shared source checkout. Your actor-scoped MCP tools define the typed actor-control protocol; native coding tools remain a separate execution surface. Use the actor tools to start and await workers. Rust owns process and actor lifecycle. A child-exit wake is informational: retrieve the exact typed result through await_worker.{continuity}")
    } else {
        "You are a Tidepool worker actor. Your process working directory is an owned retained git worktree; never edit the parent checkout. Your actor-scoped MCP tools define the typed assignment and completion protocol; native coding tools remain a separate execution surface. Retrieve your assignment, do the work, commit coherent changes when the assignment calls for edits, then call finish_work exactly once with its typed result. Rust owns process and actor lifecycle.".into()
    }
}

fn initial_prompt(root: bool, mode: &InteractiveLaunchMode) -> String {
    if root {
        if matches!(mode, InteractiveLaunchMode::Resume(_)) {
            "A fresh Tidepool actor incarnation is now attached to this retained conversation. Reconcile with its current typed tools, report its status, and do not rely on actor-runtime facts from the previous incarnation."
        } else {
            "Initialize your Tidepool root actor through its typed tools, report its status, and end this turn."
        }
    } else {
        "Initialize this Tidepool worker through its typed tools. Retrieve the typed startup assignment, complete it, and submit the result with finish_work."
    }
    .into()
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
        let manager = actor_worktree_manager_at(storage.path(), repository.path()).unwrap();
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
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .to_path_buf();
        let runtime = tempfile::tempdir().unwrap();
        let config = ActorHostConfig {
            policy_root: crate::haskell_sources::ensure_actor_policy().unwrap(),
            workspace,
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
        let registry = ActorRegistry::new();
        let (source, root) =
            compile_root(&config, session_root.path()).expect("compile root policy");
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
                "await_worker"
            ]
        );
        assert!(policy
            .declarations()
            .iter()
            .all(|declaration| declaration.output_schema.is_some()));
        assert_eq!(
            policy
                .declarations()
                .iter()
                .find(|declaration| declaration.name == "await_worker")
                .expect("await worker declaration")
                .kind,
            tidepool_agent::ToolKind::Update
        );

        let server = DynamicMcpServer::from_resident_policy(policy).expect("root MCP server");
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
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({
                "tag": "WorkerStarted",
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
        assert_ne!(worker.actor, actor);
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

        let collect = serde_json::json!({"workKey": "review-1"})
            .as_object()
            .expect("collect arguments")
            .clone();
        let collected = server
            .dispatch_tool("await_worker", collect)
            .await
            .expect("collect worker");
        assert_eq!(
            collected.structured_content,
            Some(serde_json::json!({
                "tag": "WorkerCollected",
                "workKey": "review-1",
                "outcome": {
                    "tag": "WorkCompleted",
                    "result": {
                        "summary": "boundary is clean",
                        "evidence": ["focused test"]
                    }
                }
            }))
        );
        let pending = server
            .dispatch_tool("list_workers", serde_json::Map::new())
            .await
            .expect("list workers");
        assert_eq!(
            pending.structured_content,
            Some(serde_json::json!({"workKeys": []}))
        );
        request_shutdown.send(()).expect("request shutdown");
        hosted.await.expect("host task").expect("shutdown host");
    }
}
