//! Composition root for the first actor-native interactive swarm.
//!
//! The daemon owns resident Haskell scheduling and exact actor lifecycle. One
//! stock interactive agent is attached to each installed Haskell MCP policy;
//! tmux is process ownership and observability, never message transport.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use frunk::HNil;
use rmcp::ServiceExt;
use tidepool_actor::{
    ActorDescriptor, ActorEffectProfile, ActorPlacement, ActorRef, ActorRegistry,
    ActorWorkbenchSource, ResidentActorHost, ResidentActorRoot, ResidentLifecyclePolicy,
    ResidentMcpInstallation,
};
use tidepool_agent::{
    native_interactive_backend, read_interactive_binding, InteractiveAgentBackend,
    InteractiveNodeLaunch, ReasoningEffort,
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
use tokio::net::UnixListener;
use tokio::sync::{mpsc, watch};

const POLICY_MODULE: &str = "Tidepool.Actors.DevSwarm";
const POLICY_ENTRY: &str = "rootPolicy";
const POLICY_EFFECTS: &str = "RootEffects";

pub struct ActorHostConfig {
    pub workspace: PathBuf,
    pub policy_root: PathBuf,
    pub node_program: String,
    pub tmux_session: String,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
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

struct NodeDeployment {
    actor: ActorRef,
    pane: TmuxPaneId,
    service: tokio::task::JoinHandle<Result<(), String>>,
    socket_root: PathBuf,
}

pub async fn run(config: ActorHostConfig) -> Result<(), Box<dyn std::error::Error>> {
    let run_id = uuid::Uuid::new_v4();
    let run_root = tidepool_runtime::paths::cache_dir()
        .join("actor-host")
        .join(run_id.to_string());
    std::fs::create_dir_all(&run_root)?;

    let registry = ActorRegistry::new();
    let (source, root) = compile_root(&config, &run_root)?;
    let mut host = ResidentActorHost::new(
        registry,
        source,
        Arc::new(NoResidentProvider),
        None,
        ResidentLifecyclePolicy::default(),
    )?;
    let installations = host.take_mcp_installations()?;
    let root_actor = host.launch_root(root).await?;

    let tmux = TmuxSession::new(&config.tmux_session);
    tmux.ensure().await?;
    let backend = native_interactive_backend();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let mut host_task =
        tokio::spawn(host.run_until_shutdown(wait_for_shutdown(shutdown_rx.clone())));
    let mut nodes_task = tokio::spawn(run_nodes(
        installations,
        root_actor,
        config,
        run_root,
        tmux,
        backend,
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
        Nodes(Result<(), String>),
    }
    let first = tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal?;
            FirstStop::Signal
        }
        result = &mut host_task => FirstStop::Host(result.map_err(join_error)?),
        result = &mut nodes_task => FirstStop::Nodes(result.map_err(join_error)?),
    };
    shutdown.send_replace(true);

    match first {
        FirstStop::Signal => {
            host_task.await.map_err(join_error)??;
            nodes_task
                .await
                .map_err(join_error)?
                .map_err(runtime_error)?;
        }
        FirstStop::Host(result) => {
            result?;
            nodes_task
                .await
                .map_err(join_error)?
                .map_err(runtime_error)?;
        }
        FirstStop::Nodes(result) => {
            result.map_err(runtime_error)?;
            host_task.await.map_err(join_error)??;
        }
    }
    Ok(())
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
    include.push(crate::prelude::ensure_prelude()?);
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
    .with_profile(ActorEffectProfile::ReadWrite);
    Ok((
        ActorWorkbenchSource::new(preamble, include),
        ResidentActorRoot::new(descriptor, machine, outcome),
    ))
}

async fn run_nodes(
    mut installations: mpsc::UnboundedReceiver<ResidentMcpInstallation>,
    root: ActorRef,
    config: ActorHostConfig,
    run_root: PathBuf,
    tmux: TmuxSession,
    backend: Arc<dyn InteractiveAgentBackend>,
    shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    let mut deployments = Vec::new();
    let mut health = tokio::time::interval(Duration::from_millis(250));
    let failure = loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown.clone()) => break None,
            _ = health.tick() => {
                if let Some(deployment) = deployments
                    .iter()
                    .find(|deployment: &&NodeDeployment| deployment.service.is_finished())
                {
                    break Some(format!(
                        "interactive node for actor {:?} exited before host shutdown",
                        deployment.actor
                    ));
                }
            }
            installation = installations.recv() => {
                let Some(installation) = installation else { break None };
                match launch_node(
                    installation,
                    root,
                    &config,
                    &run_root,
                    &tmux,
                    Arc::clone(&backend),
                ).await {
                    Ok(deployment) => deployments.push(deployment),
                    Err(error) => break Some(error),
                }
            }
        }
    };

    let mut cleanup_failure = None;
    for deployment in &deployments {
        if let Err(error) = tmux.kill_pane(&deployment.pane).await {
            cleanup_failure
                .get_or_insert_with(|| format!("stop actor {:?}: {error}", deployment.actor));
        }
    }
    for mut deployment in deployments {
        match tokio::time::timeout(Duration::from_secs(5), &mut deployment.service).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => {
                cleanup_failure.get_or_insert(error);
            }
            Ok(Err(error)) => {
                cleanup_failure.get_or_insert_with(|| format!("actor MCP service task: {error}"));
            }
            Err(_) => {
                deployment.service.abort();
                cleanup_failure.get_or_insert_with(|| {
                    format!(
                        "actor {:?} MCP service did not stop after its pane exited",
                        deployment.actor
                    )
                });
            }
        }
        let _ = std::fs::remove_dir_all(&deployment.socket_root);
    }
    match (failure, cleanup_failure) {
        (Some(error), Some(cleanup)) => Err(format!("{error}; cleanup: {cleanup}")),
        (Some(error), None) | (None, Some(error)) => Err(error),
        (None, None) => Ok(()),
    }
}

async fn launch_node(
    installation: ResidentMcpInstallation,
    root: ActorRef,
    config: &ActorHostConfig,
    run_root: &Path,
    tmux: &TmuxSession,
    backend: Arc<dyn InteractiveAgentBackend>,
) -> Result<NodeDeployment, String> {
    let actor = installation.actor;
    let node_root = run_root.join(format!("{}-{}", actor.id.0, actor.incarnation.0));
    std::fs::create_dir_all(&node_root).map_err(|error| error.to_string())?;
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
    std::fs::create_dir_all(&socket_root).map_err(|error| error.to_string())?;
    let endpoint = socket_root.join("mcp.sock");
    let listener = UnixListener::bind(&endpoint).map_err(|error| error.to_string())?;
    let credential = NodeCredential(uuid::Uuid::new_v4().to_string());
    let binding_path = node_root.join("binding.json");
    let inbox = DurableInbox::<String>::open(
        node_root.join("inbox.jsonl"),
        node_root.join("inbox.cursor"),
    )
    .map_err(|error| error.to_string())?;
    let launch = InteractiveNodeLaunch {
        actor,
        endpoint,
        credential: credential.clone(),
        binding_path: binding_path.clone(),
        workspace: config.workspace.clone(),
        model: config.model.clone(),
        effort: config.effort,
        developer_instructions: developer_instructions(actor == root),
        initial_prompt: initial_prompt(actor == root),
    };
    let server = DynamicMcpServer::from_resident_policy(installation.policy)
        .map_err(|error| error.to_string())?;
    let service = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        let (_handshake, stream) = accept_proxy(stream, |candidate| {
            if candidate.actor != actor {
                return Err("actor identity does not match this endpoint".into());
            }
            if candidate.credential != credential {
                return Err("node launch credential is invalid".into());
            }
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())?;
        let (read, write) = stream.into_split();
        server
            .serve((read, write))
            .await
            .map_err(|error| error.to_string())?
            .waiting()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    });
    let pane = match tmux
        .spawn_window(&TmuxLaunch {
            window_name: format!("actor-{}-{}", actor.id.0, actor.incarnation.0),
            cwd: config.workspace.clone(),
            program: config.node_program.clone(),
            args: vec!["host".into()],
            environment: launch.environment(),
        })
        .await
    {
        Ok(pane) => pane,
        Err(error) => {
            service.abort();
            let _ = service.await;
            let _ = std::fs::remove_dir_all(&socket_root);
            return Err(error.to_string());
        }
    };

    let thread = match wait_for_binding(&binding_path, &service).await {
        Ok(thread) => thread,
        Err(error) => {
            abandon_node(tmux, &pane, service, &socket_root).await;
            return Err(error);
        }
    };
    let initial = if actor == root {
        "Runtime binding confirmed."
    } else {
        "Runtime binding confirmed; continue the typed startup assignment."
    };
    if let Err(error) = inbox.publish(initial.into()) {
        abandon_node(tmux, &pane, service, &socket_root).await;
        return Err(error.to_string());
    }
    let cwd = config.workspace.to_string_lossy();
    let pending = match inbox.pending() {
        Ok(pending) => pending,
        Err(error) => {
            abandon_node(tmux, &pane, service, &socket_root).await;
            return Err(error.to_string());
        }
    };
    for message in pending {
        if let Err(error) = backend.push(&cwd, &thread, &message.payload).await {
            abandon_node(tmux, &pane, service, &socket_root).await;
            return Err(error.to_string());
        }
        if let Err(error) = inbox.acknowledge(message.sequence) {
            abandon_node(tmux, &pane, service, &socket_root).await;
            return Err(error.to_string());
        }
    }
    Ok(NodeDeployment {
        actor,
        pane,
        service,
        socket_root,
    })
}

async fn abandon_node(
    tmux: &TmuxSession,
    pane: &TmuxPaneId,
    service: tokio::task::JoinHandle<Result<(), String>>,
    socket_root: &Path,
) {
    let _ = tmux.kill_pane(pane).await;
    service.abort();
    let _ = service.await;
    let _ = std::fs::remove_dir_all(socket_root);
}

async fn wait_for_binding(
    path: &Path,
    service: &tokio::task::JoinHandle<Result<(), String>>,
) -> Result<tidepool_agent::BackendThreadId, String> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if service.is_finished() {
                return Err("actor MCP service stopped before rollout binding".into());
            }
            if let Ok(thread) = read_interactive_binding(path).await {
                return Ok(thread);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| {
        format!(
            "interactive rollout binding timed out at {}",
            path.display()
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

fn developer_instructions(root: bool) -> String {
    let role = if root { "root" } else { "worker" };
    format!(
        "You are a Tidepool {role} actor. Your actor-scoped MCP tools are the authoritative typed interaction surface. Use those tools to inspect and act; Rust owns process and actor lifecycle."
    )
}

fn initial_prompt(root: bool) -> String {
    if root {
        "Initialize your Tidepool root actor through its typed tools, report its status, and end this turn."
    } else {
        "Initialize this Tidepool worker through its typed tools. Retrieve the typed startup assignment and begin it."
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
    use tidepool_testing::eval_harness;

    #[tokio::test]
    async fn checked_in_devswarm_policy_installs_the_root_tool_surface() {
        eval_harness::require_extract();
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .to_path_buf();
        let config = ActorHostConfig {
            policy_root: workspace.join("haskell/actors"),
            workspace,
            node_program: "unused-in-compile-test".into(),
            tmux_session: "unused-in-compile-test".into(),
            model: None,
            effort: None,
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
        let mut installations = host
            .take_mcp_installations()
            .expect("take deployment stream");
        let actor = host.launch_root(root).await.expect("launch root");
        let report = host.run_until_idle().await.expect("install policy");
        assert!(report.failures.is_empty());
        assert_eq!(report.parked[&ResidentHostParkedKind::McpPolicy], 1);
        let root_installation = installations.try_recv().expect("root policy installation");
        assert_eq!(root_installation.actor, actor);
        let policy = root_installation.policy;
        let names = policy
            .declarations()
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["actor_status", "spawn_worker"]);

        let server = DynamicMcpServer::from_resident_policy(policy).expect("root MCP server");
        let (request_shutdown, shutdown_requested) = tokio::sync::oneshot::channel();
        let hosted = tokio::spawn(host.run_until_shutdown(async move {
            let _ = shutdown_requested.await;
        }));
        let arguments = serde_json::json!({"assignment": "inspect one focused boundary"})
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
        let worker = tokio::time::timeout(Duration::from_secs(1), installations.recv())
            .await
            .expect("worker installation timeout")
            .expect("worker policy installation");
        assert_ne!(worker.actor, actor);
        let worker_names = worker
            .policy
            .declarations()
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(worker_names, ["actor_status", "current_assignment"]);
        request_shutdown.send(()).expect("request shutdown");
        hosted.await.expect("host task").expect("shutdown host");
    }
}
