//! Shoal process composition and one-command tmux bootstrap.
//!
//! One host process owns every resident Haskell actor. Interactive actors are
//! ordinary interactive-agent TUIs launched directly in tmux panes. Each actor
//! receives its resident tools through an actor-scoped Unix socket.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tidepool_actor::ActorRef;
use tidepool_agent::{
    copy_interactive_binding, read_interactive_binding, BackendThreadId,
    InteractiveAgentInstallation, InteractiveLaunchMode, ReasoningEffort,
};
use tidepool_node::{TmuxLaunch, TmuxSession};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tracing::Instrument;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

use crate::actor_host::ACTOR_PROJECT_ROOT;

const STATUS_VERSION: u32 = 4;
const INTERACTIVE_START_TIMEOUT: Duration = Duration::from_secs(120);
const SHOAL_EXCLUDE: &str = "/.shoal/";
const SHOAL_CONFIG: &str = ".shoal/config.toml";
const DEFAULT_CONFIG: &str = r#"[defaults]
model = "gpt-5.6-sol"
effort = "low"
"#;
const ENV_PACKAGED_CODEX_CLOSURE: &str = "TIDEPOOL_SHOAL_CODEX_CLOSURE";
const ENV_NIX_STORE_BIN: &str = "TIDEPOOL_SHOAL_NIX_STORE_BIN";
const GC_ROOT_TIMEOUT: Duration = Duration::from_secs(30);
const GC_ROOT_ERROR_LIMIT: usize = 16 * 1024;
const BOUNDARY_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const BOUNDARY_PROBE_ERROR_LIMIT: usize = 16 * 1024;
const COMPILER_DAEMON_START_TIMEOUT: Duration = Duration::from_secs(30);
const COMPILER_DAEMON_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub struct NewOptions {
    pub path: Option<PathBuf>,
}

pub struct InitOptions {
    pub workspace: Option<PathBuf>,
    pub session: Option<String>,
    pub recreate: bool,
    pub no_attach: bool,
    pub model: Option<String>,
    pub effort: Option<ShoalEffort>,
}

pub struct HostOptions {
    pub workspace: PathBuf,
    pub session: String,
    pub run_id: String,
    pub run_root: PathBuf,
    pub status_path: PathBuf,
    pub root_binding_path: PathBuf,
    pub interactive_agent: InteractiveAgentInstallation,
    pub resume_root: bool,
    pub agent: ShoalAgentDefaults,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShoalAgentDefaults {
    pub model: String,
    pub effort: ShoalEffort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShoalEffort {
    Low,
    Medium,
    High,
}

impl From<ReasoningEffort> for ShoalEffort {
    fn from(value: ReasoningEffort) -> Self {
        match value {
            ReasoningEffort::Low => Self::Low,
            ReasoningEffort::Medium => Self::Medium,
            ReasoningEffort::High => Self::High,
        }
    }
}

impl From<ShoalEffort> for ReasoningEffort {
    fn from(value: ShoalEffort) -> Self {
        match value {
            ShoalEffort::Low => Self::Low,
            ShoalEffort::Medium => Self::Medium,
            ShoalEffort::High => Self::High,
        }
    }
}

impl std::fmt::Display for ShoalEffort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShoalConfig {
    defaults: ShoalAgentDefaults,
}

/// Initialize the smallest repository that can host a Shoal ensemble.
///
/// The command creates no product scaffold. `.shoal/` is runtime-owned and
/// excluded locally through Git metadata, so using Shoal cannot dirty the
/// repository or impose an ignore rule on collaborators.
pub async fn new(options: NewOptions) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = match options.path {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    if workspace.exists() {
        let mut entries = std::fs::read_dir(&workspace)?;
        if entries.next().transpose()?.is_some() {
            return Err(runtime_error(format!(
                "shoal new requires an empty directory: {}",
                workspace.display()
            )));
        }
    } else {
        std::fs::create_dir_all(&workspace)?;
    }
    run_git(&workspace, &["init", "--quiet"]).await?;

    let state = workspace.join(".shoal");
    std::fs::create_dir_all(state.join("logs"))?;
    std::fs::create_dir_all(state.join("sessions"))?;
    ensure_project_config(&workspace)?;
    install_local_exclude(&workspace)?;

    run_git(
        &workspace,
        &[
            "-c",
            "user.name=Shoal",
            "-c",
            "user.email=shoal@localhost",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Initialize Shoal workspace",
        ],
    )
    .await?;

    let workspace = std::fs::canonicalize(workspace)?;
    println!(
        "Initialized empty Shoal workspace at {}",
        workspace.display()
    );
    println!("agent defaults: {}", workspace.join(SHOAL_CONFIG).display());
    Ok(())
}

fn ensure_project_config(
    workspace: &Path,
) -> Result<ShoalAgentDefaults, Box<dyn std::error::Error>> {
    let path = workspace.join(SHOAL_CONFIG);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            tidepool_atomic_write::write_best_effort(&path, DEFAULT_CONFIG.as_bytes())?;
            DEFAULT_CONFIG.to_owned()
        }
        Err(error) => {
            return Err(runtime_error(format!(
                "cannot read Shoal configuration {}: {error}",
                path.display()
            )))
        }
    };
    let config: ShoalConfig = toml::from_str(&text).map_err(|error| {
        runtime_error(format!(
            "invalid Shoal configuration {}: {error}",
            path.display()
        ))
    })?;
    validate_agent_defaults(
        config.defaults,
        &format!("Shoal configuration {}", path.display()),
    )
}

fn validate_agent_defaults(
    mut defaults: ShoalAgentDefaults,
    source: &str,
) -> Result<ShoalAgentDefaults, Box<dyn std::error::Error>> {
    let model = defaults.model.trim();
    if model.is_empty() {
        return Err(runtime_error(format!("{source} selects an empty model")));
    }
    defaults.model = model.to_owned();
    Ok(defaults)
}

fn resolve_agent_defaults(
    configured: ShoalAgentDefaults,
    model: Option<String>,
    effort: Option<ShoalEffort>,
) -> Result<ShoalAgentDefaults, Box<dyn std::error::Error>> {
    let resolved = ShoalAgentDefaults {
        model: model.unwrap_or(configured.model),
        effort: effort.unwrap_or(configured.effort),
    };
    validate_agent_defaults(resolved, "resolved Shoal agent defaults")
}

fn install_local_exclude(workspace: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let exclude = workspace.join(".git/info/exclude");
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == SHOAL_EXCLUDE) {
        return Ok(());
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(SHOAL_EXCLUDE);
    updated.push('\n');
    tidepool_atomic_write::write_best_effort(&exclude, updated.as_bytes())?;
    Ok(())
}

async fn run_git(workspace: &Path, arguments: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = tokio::process::Command::new("git")
        .args(arguments)
        .current_dir(workspace)
        .output()
        .await?;
    if output.status.success() {
        return Ok(());
    }
    Err(runtime_error(format!(
        "git {} failed in {}: {}",
        arguments.join(" "),
        workspace.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStatus {
    pub version: u32,
    pub run_id: String,
    pub workspace: PathBuf,
    pub session: String,
    pub agent: ShoalAgentDefaults,
    pub phase: RunPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunPhase {
    Starting,
    AwaitingBinding {
        root_actor: ActorRef,
    },
    Ready {
        root_actor: ActorRef,
        root_thread: BackendThreadId,
    },
    Failed {
        error: String,
    },
    Exited,
}

pub async fn init(options: InitOptions) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = match options.workspace {
        Some(workspace) => workspace,
        None => {
            let cwd = std::env::current_dir()?;
            tidepool_runtime::paths::find_project_root(&cwd).unwrap_or(cwd)
        }
    };
    let workspace = std::fs::canonicalize(workspace)?;
    install_local_exclude(&workspace)?;
    let agent = resolve_agent_defaults(
        ensure_project_config(&workspace)?,
        options.model,
        options.effort,
    )?;
    retain_packaged_interactive_agent(&workspace).await?;
    let session_name = options
        .session
        .unwrap_or_else(|| default_session_name(&workspace));
    let tmux = TmuxSession::new(session_name.clone())?;

    if tmux.exists().await? && !options.recreate {
        return Err(runtime_error(format!(
            "tmux session {session_name:?} already exists; attach with `tmux attach -t {session_name}` or replace it with `shoal init --recreate`"
        )));
    }
    let session_root = shoal_state_root(&workspace)
        .join("sessions")
        .join(&session_name);
    std::fs::create_dir_all(&session_root)?;
    let root_binding_path = session_root.join("root-binding.json");
    let interactive_agent = preflight(&workspace).await?;
    if options.recreate {
        // Validate continuity before stopping a currently healthy session.
        // The host repeats this check at launch so a later disappearance also
        // fails closed.
        resolve_root_launch_mode(true, &root_binding_path).await?;
        tmux.kill().await?;
    } else {
        clear_fresh_root_binding(&root_binding_path)?;
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let log_path = shoal_log_path(&workspace, &run_id);
    let compiler_log_path = shoal_compiler_log_path(&workspace, &run_id);
    let run_root = tidepool_runtime::paths::cache_dir()
        .join("shoal")
        .join("runs")
        .join(&run_id);
    std::fs::create_dir_all(&run_root)?;
    let status_path = run_root.join("status.json");
    write_status(
        &status_path,
        &RunStatus::new(
            &run_id,
            &workspace,
            &session_name,
            agent.clone(),
            RunPhase::Starting,
        ),
    )?;

    let executable = current_executable()?;
    let compiler_socket = run_root.join("compiler.sock");
    let compiler_bin = tidepool_extract_cmd::resolve_bin()?.path;
    let compiler_program = compiler_bin
        .to_str()
        .ok_or_else(|| runtime_error("compiler executable path is not UTF-8"))?
        .to_owned();
    let daemon_launch = tmux
        .create(&compiler_daemon_launch(
            &workspace,
            &compiler_socket,
            compiler_program,
            &run_id,
            &compiler_log_path,
        ))
        .await;
    if let Err(error) = daemon_launch {
        write_startup_failure(
            &status_path,
            &run_id,
            &workspace,
            &session_name,
            &agent,
            &error,
        )?;
        return Err(error.into());
    }
    if let Err(error) = wait_until_compiler_daemon(&tmux, &compiler_socket).await {
        write_startup_failure(
            &status_path,
            &run_id,
            &workspace,
            &session_name,
            &agent,
            error.as_ref(),
        )?;
        let _ = tmux.kill().await;
        return Err(error);
    }

    let mut args = vec![
        "host".into(),
        "--workspace".into(),
        workspace.display().to_string(),
        "--session".into(),
        session_name.clone(),
        "--run-id".into(),
        run_id.clone(),
        "--run-root".into(),
        run_root.display().to_string(),
        "--status-path".into(),
        status_path.display().to_string(),
        "--root-binding-path".into(),
        root_binding_path.display().to_string(),
        "--interactive-agent-bin".into(),
        interactive_agent.executable().display().to_string(),
        "--interactive-agent-version".into(),
        interactive_agent.version().to_owned(),
    ];
    if options.recreate {
        args.push("--resume-root".into());
    }
    args.extend(["--model".into(), agent.model.clone()]);
    args.extend(["--effort".into(), agent.effort.to_string()]);
    let launch = tmux
        .spawn_window(&TmuxLaunch {
            window_name: "Host".into(),
            cwd: workspace.clone(),
            program: executable,
            args,
            environment: host_environment(&compiler_socket),
            unset_environment: std::collections::BTreeSet::new(),
        })
        .await;
    if let Err(error) = launch {
        write_startup_failure(
            &status_path,
            &run_id,
            &workspace,
            &session_name,
            &agent,
            &error,
        )?;
        let _ = tmux.kill().await;
        return Err(error.into());
    }
    println!("log:    {}", log_path.display());
    println!(
        "compiler: {} (tmux window Compiler)",
        compiler_socket.display()
    );

    let interactive = match wait_until_interactive(&tmux, &status_path, &run_id).await {
        Ok(interactive) => interactive,
        Err(failure) => {
            let _ = tmux.kill().await;
            return Err(failure);
        }
    };
    match &interactive.phase {
        RunPhase::AwaitingBinding { root_actor } => println!(
            "Shoal launched in tmux session {session_name:?}: actor {root_actor:?} is waiting for its queue-ready session handshake"
        ),
        RunPhase::Ready {
            root_actor,
            root_thread,
        } => println!(
            "Shoal ready in tmux session {session_name:?}: actor {root_actor:?}, thread {}",
            root_thread.0
        ),
        RunPhase::Starting | RunPhase::Failed { .. } | RunPhase::Exited => {
            unreachable!("wait_until_interactive returns only interactive phases")
        }
    }
    println!("status: {}", status_path.display());
    println!("agent:  model={} effort={}", agent.model, agent.effort);
    println!("config: {}", workspace.join(SHOAL_CONFIG).display());
    if options.no_attach {
        println!("attach: tmux attach -t {session_name}");
        println!("stop:   tmux kill-session -t {session_name}");
        Ok(())
    } else {
        tmux.attach_or_switch()
            .await
            .map_err(|failure| Box::new(failure) as Box<dyn std::error::Error>)
    }
}

fn compiler_daemon_launch(
    workspace: &Path,
    socket: &Path,
    program: String,
    run_id: &str,
    log_path: &Path,
) -> TmuxLaunch {
    TmuxLaunch {
        window_name: "Compiler".into(),
        cwd: workspace.into(),
        program,
        args: vec![
            "--daemon".into(),
            "--socket".into(),
            socket.display().to_string(),
            "--persistent".into(),
            "--run-id".into(),
            run_id.into(),
            "--log-path".into(),
            log_path.display().to_string(),
        ],
        environment: pane_environment(),
        unset_environment: std::collections::BTreeSet::new(),
    }
}

fn host_environment(compiler_socket: &Path) -> std::collections::BTreeMap<String, String> {
    let mut environment = pane_environment();
    environment.insert(
        tidepool_extract_cmd::DAEMON_SOCKET_ENV.into(),
        compiler_socket.display().to_string(),
    );
    environment
}

fn write_startup_failure(
    status_path: &Path,
    run_id: &str,
    workspace: &Path,
    session_name: &str,
    agent: &ShoalAgentDefaults,
    error: &dyn std::fmt::Display,
) -> Result<(), Box<dyn std::error::Error>> {
    write_status(
        status_path,
        &RunStatus::new(
            run_id,
            workspace,
            session_name,
            agent.clone(),
            RunPhase::Failed {
                error: error.to_string(),
            },
        ),
    )
}

async fn wait_until_compiler_daemon(
    tmux: &TmuxSession,
    socket: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = socket.to_owned();
    tokio::time::timeout(COMPILER_DAEMON_START_TIMEOUT, async {
        loop {
            let candidate = socket.clone();
            let ready = tokio::task::spawn_blocking(move || {
                tidepool_extract_cmd::preflight_compiler_daemon(&candidate)
            })
            .await
            .map_err(|error| runtime_error(format!("compiler preflight task failed: {error}")))?;
            if ready.is_ok() {
                return Ok(());
            }
            if !tmux.exists().await? {
                return Err(runtime_error(
                    "compiler daemon exited before becoming ready",
                ));
            }
            tokio::time::sleep(COMPILER_DAEMON_POLL_INTERVAL).await;
        }
    })
    .await
    .map_err(|_| {
        runtime_error(format!(
            "compiler daemon did not become ready within {COMPILER_DAEMON_START_TIMEOUT:?}"
        ))
    })?
}

/// Retain the Nix-packaged private interactive-agent closure without placing
/// its executable on the user's ordinary PATH. The project-local symlink is a
/// durable GC root under Shoal's runtime-owned state directory.
async fn retain_packaged_interactive_agent(
    workspace: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    retain_packaged_interactive_agent_from(
        workspace,
        std::env::var_os(ENV_PACKAGED_CODEX_CLOSURE).map(PathBuf::from),
        std::env::var_os(ENV_NIX_STORE_BIN).map(PathBuf::from),
    )
    .await
}

async fn retain_packaged_interactive_agent_from(
    workspace: &Path,
    target: Option<PathBuf>,
    nix_store: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (target, nix_store) = match (target, nix_store) {
        (None, None) => return Ok(()),
        (Some(target), Some(nix_store)) => (target, nix_store),
        _ => {
            return Err(runtime_error(format!(
                "{ENV_PACKAGED_CODEX_CLOSURE} and {ENV_NIX_STORE_BIN} must be supplied together"
            )))
        }
    };
    if !target.is_absolute() || !target.is_dir() {
        return Err(runtime_error(format!(
            "{ENV_PACKAGED_CODEX_CLOSURE} is not an absolute package directory: {}",
            target.display()
        )));
    }
    if !nix_store.is_absolute() || !nix_store.is_file() {
        return Err(runtime_error(format!(
            "{ENV_NIX_STORE_BIN} is not an absolute executable file: {}",
            nix_store.display()
        )));
    }
    let runtime = workspace.join(".shoal/runtime");
    std::fs::create_dir_all(&runtime)?;
    let link = runtime.join("interactive-agent");
    let mut child = tokio::process::Command::new(&nix_store)
        .arg("--realise")
        .arg(&target)
        .arg("--add-root")
        .arg(&link)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            runtime_error(format!(
                "cannot start {ENV_NIX_STORE_BIN} {}: {error}",
                nix_store.display()
            ))
        })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        runtime_error(format!(
            "{ENV_NIX_STORE_BIN} did not expose its diagnostic stream"
        ))
    })?;
    let result = tokio::time::timeout(GC_ROOT_TIMEOUT, async {
        tokio::try_join!(
            read_bounded_diagnostics(stderr, GC_ROOT_ERROR_LIMIT, "Nix GC-root creation"),
            child.wait()
        )
    })
    .await
    .map_err(|_| runtime_error(format!("Nix GC-root creation exceeded {GC_ROOT_TIMEOUT:?}")))?;
    let (stderr, status) = result.map_err(|error| {
        runtime_error(format!(
            "cannot register Nix GC root {}: {error}",
            link.display()
        ))
    })?;
    if !status.success() {
        return Err(runtime_error(format!(
            "cannot register Nix GC root {} ({status}): {}",
            link.display(),
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(())
}

async fn read_bounded_diagnostics(
    reader: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
    operation: &'static str,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{operation} diagnostics exceeded {limit} bytes"),
        ));
    }
    Ok(bytes)
}

pub async fn host(options: HostOptions) -> Result<(), Box<dyn std::error::Error>> {
    let log_path = shoal_log_path(&options.workspace, &options.run_id);
    tracing::info!(
        run_id = %options.run_id,
        session = %options.session,
        workspace = %options.workspace.display(),
        interactive_agent = %options.interactive_agent.executable().display(),
        interactive_agent_version = options.interactive_agent.version(),
        model = %options.agent.model,
        effort = %options.agent.effort,
        detailed_log = %log_path.display(),
        "starting Shoal actor host"
    );
    let result = run_host(&options).await;
    let settled = settle_host_result(result, &options);
    if let Err(error) = &settled {
        tracing::error!(run_id = %options.run_id, error = %error, "Shoal actor host failed");
        if let Ok(tmux) = TmuxSession::new(options.session.clone()) {
            let _ = tmux.kill().await;
        }
    } else {
        tracing::info!(run_id = %options.run_id, "Shoal actor host stopped");
    }
    settled
}

async fn run_host(options: &HostOptions) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&options.run_root)?;
    if let Some(parent) = options.root_binding_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let root_launch_mode =
        resolve_root_launch_mode(options.resume_root, &options.root_binding_path).await?;

    let haskell_root = crate::haskell_sources::ensure_shoal_haskell()?;
    let (readiness_tx, mut readiness_rx) = mpsc::unbounded_channel();
    let run = crate::actor_host::run(
        crate::actor_host::ActorHostConfig {
            workspace: options.workspace.clone(),
            haskell_root,
            run_root: options.run_root.clone(),
            root_binding_path: options.run_root.join("root-binding.json"),
            interactive_agent: options.interactive_agent.clone(),
            tmux_session: options.session.clone(),
            model: options.agent.model.clone(),
            effort: options.agent.effort.into(),
            root_launch_mode,
            pane_environment: pane_environment(),
        },
        readiness_tx,
    )
    .instrument(tracing::info_span!(
        "shoal_host",
        run_id = %options.run_id
    ));

    tokio::pin!(run);
    loop {
        tokio::select! {
            readiness = readiness_rx.recv() => match readiness {
                Some(crate::actor_host::ActorHostReadiness::AwaitingBinding { root }) => {
                    let status = RunStatus::new(
                        &options.run_id,
                        &options.workspace,
                        &options.session,
                        options.agent.clone(),
                        RunPhase::AwaitingBinding { root_actor: root },
                    );
                    write_status(&options.status_path, &status)?;
                    tracing::info!(
                        run_id = %options.run_id,
                        actor = ?root,
                        "Shoal root application launched; queue-ready session handshake pending"
                    );
                }
                Some(crate::actor_host::ActorHostReadiness::Ready { root, thread }) => {
                    let thread_id = thread.id().0.clone();
                    copy_interactive_binding(&options.root_binding_path, &thread).await?;
                    let status = RunStatus::new(
                        &options.run_id,
                        &options.workspace,
                        &options.session,
                        options.agent.clone(),
                        RunPhase::Ready {
                            root_actor: root,
                            root_thread: thread.id().clone(),
                        },
                    );
                    write_status(&options.status_path, &status)?;
                    tracing::info!(
                        run_id = %options.run_id,
                        actor = ?root,
                        thread = %thread_id,
                        "root interactive application ready"
                    );
                }
                None => return run.await,
            },
            result = &mut run => return result,
        }
    }
}

async fn resolve_root_launch_mode(
    resume: bool,
    binding_path: &Path,
) -> Result<InteractiveLaunchMode, Box<dyn std::error::Error>> {
    if !resume {
        return Ok(InteractiveLaunchMode::Fresh);
    }
    read_interactive_binding(binding_path)
        .await
        .map(|thread| InteractiveLaunchMode::Resume(thread.id().clone()))
        .map_err(|error| {
            runtime_error(format!(
                "cannot resume the requested root conversation from {}: {error}",
                binding_path.display()
            ))
        })
}

fn clear_fresh_root_binding(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(runtime_error(format!(
            "cannot clear stale root conversation binding {}: {error}",
            path.display()
        ))),
    }
}

fn settle_host_result(
    result: Result<(), Box<dyn std::error::Error>>,
    options: &HostOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let phase = match &result {
        Ok(()) => RunPhase::Exited,
        Err(failure) => RunPhase::Failed {
            error: failure.to_string(),
        },
    };
    let status = RunStatus::new(
        &options.run_id,
        &options.workspace,
        &options.session,
        options.agent.clone(),
        phase,
    );
    match (result, write_status(&options.status_path, &status)) {
        (result, Ok(())) => result,
        (Ok(()), Err(status_error)) => Err(status_error),
        (Err(run_error), Err(status_error)) => {
            tracing::error!(error = %status_error, "could not publish terminal Shoal status");
            Err(run_error)
        }
    }
}

async fn preflight(
    workspace: &Path,
) -> Result<InteractiveAgentInstallation, Box<dyn std::error::Error>> {
    crate::haskell_sources::ensure_stdlib()?;
    crate::haskell_sources::ensure_shoal_haskell()?;
    tidepool_runtime::toolchain::bind_extract_endpoint()?;

    // Codex keys its interactive trust decision by the path visible inside
    // its process. Every actor gets an isolated repository mounted at this one
    // stable slot, so this creates one durable entry rather than one per run.
    std::fs::create_dir_all(ACTOR_PROJECT_ROOT)?;
    tidepool_agent::trust_interactive_project(Path::new(ACTOR_PROJECT_ROOT))?;

    let interactive_agent = tidepool_agent::resolve_native_interactive_agent().await?;

    let mut boundary = tokio::process::Command::new(tidepool_node::BUBBLEWRAP_PROGRAM);
    boundary
        .args(["--bind", "/", "/", "--ro-bind"])
        .arg(workspace)
        .arg(workspace)
        .args(["--chdir"])
        .arg(workspace)
        // Resolve through the dev/runtime PATH. NixOS deliberately does not
        // provide the FHS `/bin/true` path.
        .args(["--", "true"])
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut boundary = boundary.spawn().map_err(|source| {
        runtime_error(format!(
            "Bubblewrap is required for Shoal actor worktrees: {source}"
        ))
    })?;
    let stderr = boundary.stderr.take().ok_or_else(|| {
        runtime_error("Bubblewrap capability probe did not expose its diagnostic stream")
    })?;
    let result = tokio::time::timeout(BOUNDARY_PROBE_TIMEOUT, async {
        tokio::try_join!(
            read_bounded_diagnostics(
                stderr,
                BOUNDARY_PROBE_ERROR_LIMIT,
                "Bubblewrap capability probe"
            ),
            boundary.wait()
        )
    })
    .await
    .map_err(|_| {
        runtime_error(format!(
            "Bubblewrap capability probe exceeded {BOUNDARY_PROBE_TIMEOUT:?}"
        ))
    })?;
    let (stderr, status) = result.map_err(|source| {
        runtime_error(format!(
            "Bubblewrap capability probe could not complete: {source}"
        ))
    })?;
    if !status.success() {
        return Err(runtime_error(format!(
            "Bubblewrap cannot establish the Shoal process boundary: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(interactive_agent)
}

async fn wait_until_interactive(
    tmux: &TmuxSession,
    status_path: &Path,
    run_id: &str,
) -> Result<RunStatus, Box<dyn std::error::Error>> {
    tokio::time::timeout(INTERACTIVE_START_TIMEOUT, async {
        loop {
            if let Ok(bytes) = tokio::fs::read(status_path).await {
                let status = decode_run_status(&bytes)?;
                if status.run_id == run_id {
                    match &status.phase {
                        RunPhase::AwaitingBinding { .. } | RunPhase::Ready { .. } => {
                            return Ok(status)
                        }
                        RunPhase::Failed { error } => {
                            return Err(runtime_error(format!("Shoal host failed: {error}")))
                        }
                        RunPhase::Exited => {
                            return Err(runtime_error(
                                "Shoal host exited before becoming interactive",
                            ))
                        }
                        RunPhase::Starting => {}
                    }
                }
            }
            if !tmux.exists().await? {
                return Err(runtime_error(
                    "Shoal tmux session exited before becoming interactive",
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| {
        runtime_error(format!(
            "Shoal did not become interactive within {INTERACTIVE_START_TIMEOUT:?}"
        ))
    })?
}

fn decode_run_status(bytes: &[u8]) -> Result<RunStatus, Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| runtime_error("Shoal run status has no valid version"))?;
    if version != STATUS_VERSION {
        return Err(runtime_error(format!(
            "unsupported Shoal run status version {version} (expected {STATUS_VERSION})"
        )));
    }
    Ok(serde_json::from_value(value)?)
}

impl RunStatus {
    fn new(
        run_id: &str,
        workspace: &Path,
        session: &str,
        agent: ShoalAgentDefaults,
        phase: RunPhase,
    ) -> Self {
        Self {
            version: STATUS_VERSION,
            run_id: run_id.into(),
            workspace: workspace.into(),
            session: session.into(),
            agent,
            phase,
        }
    }
}

fn write_status(path: &Path, status: &RunStatus) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = serde_json::to_vec_pretty(status)?;
    tidepool_atomic_write::write_best_effort(path, &bytes)?;
    Ok(())
}

fn shoal_state_root(workspace: &Path) -> PathBuf {
    workspace.join(".shoal")
}

pub fn shoal_log_path(workspace: &Path, run_id: &str) -> PathBuf {
    shoal_state_root(workspace)
        .join("logs")
        .join(format!("{run_id}.log"))
}

pub fn shoal_compiler_log_path(workspace: &Path, run_id: &str) -> PathBuf {
    shoal_state_root(workspace)
        .join("logs")
        .join(format!("{run_id}-compiler.log"))
}

fn host_pane_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::new("warn,tidepool::shoal=info,tidepool::actor_host=info")
}

fn host_tracing_subscriber<D, P>(
    detailed_writer: D,
    pane_writer: P,
    detailed_filter: tracing_subscriber::EnvFilter,
) -> impl tracing::Subscriber + Send + Sync
where
    D: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    P: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    let detailed = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(detailed_writer)
        .with_filter(detailed_filter);
    let pane = tracing_subscriber::fmt::layer()
        .compact()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_writer(pane_writer)
        .with_filter(host_pane_filter());
    tracing_subscriber::registry().with(detailed).with(pane)
}

pub fn init_host_tracing(
    workspace: &Path,
    run_id: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = shoal_log_path(workspace, run_id);
    let parent = path
        .parent()
        .ok_or_else(|| runtime_error("Shoal log path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    host_tracing_subscriber(
        Mutex::new(file),
        std::io::stderr,
        tidepool_codegen::debug::tracing_env_filter("info"),
    )
    .try_init()
    .map_err(|error| runtime_error(format!("could not initialize Shoal tracing: {error}")))?;
    tidepool_codegen::debug::init_logging();
    Ok(path)
}

fn default_session_name(workspace: &Path) -> String {
    let source = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    let mut slug = String::new();
    let mut separator = false;
    for character in source.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator && !slug.is_empty() {
            slug.push('-');
            separator = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug.push_str("workspace");
    }
    format!("shoal-{slug}")
}

fn current_executable() -> Result<String, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    executable
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| runtime_error("Shoal executable path is not UTF-8"))
}

/// Variables whose current values must override a possibly older tmux server
/// environment. Tmux supplies fresh `TMUX`/`TMUX_PANE` identities itself.
fn pane_environment() -> std::collections::BTreeMap<String, String> {
    const NAMES: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "TMPDIR",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "TIDEPOOL_EXTRACT",
        "TIDEPOOL_EXTRACT_WORKER",
        "TIDEPOOL_PRELUDE_DIR",
        "TIDEPOOL_GHC_LIBDIR",
        "TIDEPOOL_COMPILE_CACHE_DIR",
        "TIDEPOOL_BUILD_PRODUCTS_DIR",
        "TIDEPOOL_CONFIG_DIR",
        "TIDEPOOL_TOOLCHAIN_STAMP",
        "TIDEPOOL_TOOLCHAIN_HANDSHAKE",
        "CODEX_HOME",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GEMINI_API_KEY",
        "SSL_CERT_FILE",
        "NIX_SSL_CERT_FILE",
        "SSH_AUTH_SOCK",
        "RUST_LOG",
    ];
    NAMES
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).into(), value))
        })
        .collect()
}

fn runtime_error(message: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::other(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_agent_defaults() -> ShoalAgentDefaults {
        ShoalAgentDefaults {
            model: "test-model".into(),
            effort: ShoalEffort::Medium,
        }
    }

    async fn git_stdout(workspace: &Path, arguments: &[&str]) -> String {
        let output = tokio::process::Command::new("git")
            .args(arguments)
            .current_dir(workspace)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "git {}: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[tokio::test]
    async fn new_creates_only_ignored_shoal_state_and_an_empty_base_commit() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("project");

        new(NewOptions {
            path: Some(workspace.clone()),
        })
        .await
        .unwrap();

        assert!(workspace.join(".shoal/logs").is_dir());
        assert!(workspace.join(".shoal/sessions").is_dir());
        assert_eq!(
            ensure_project_config(&workspace).unwrap(),
            ShoalAgentDefaults {
                model: "gpt-5.6-sol".into(),
                effort: ShoalEffort::Low,
            }
        );
        assert!(std::fs::read_to_string(workspace.join(".git/info/exclude"))
            .unwrap()
            .lines()
            .any(|line| line == SHOAL_EXCLUDE));
        assert_eq!(git_stdout(&workspace, &["status", "--short"]).await, "");
        assert_eq!(
            git_stdout(&workspace, &["show", "--format=%s", "--no-patch", "HEAD"])
                .await
                .trim(),
            "Initialize Shoal workspace"
        );
        assert_eq!(
            git_stdout(&workspace, &["ls-tree", "--name-only", "HEAD"]).await,
            ""
        );
    }

    #[tokio::test]
    async fn new_refuses_to_claim_a_nonempty_directory() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("owned.txt"), "user data").unwrap();

        let error = new(NewOptions {
            path: Some(workspace.path().into()),
        })
        .await
        .unwrap_err();

        assert!(error.to_string().contains("requires an empty directory"));
        assert!(!workspace.path().join(".git").exists());
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("owned.txt")).unwrap(),
            "user data"
        );
    }

    #[test]
    fn project_names_become_valid_stable_session_names() {
        assert_eq!(
            default_session_name(Path::new("/tmp/My Project...")),
            "shoal-my-project"
        );
        assert_eq!(
            default_session_name(Path::new("/tmp/🐟")),
            "shoal-workspace"
        );
    }

    #[test]
    fn project_agent_defaults_are_explicit_and_cli_overrides_are_per_field() {
        let workspace = tempfile::tempdir().unwrap();
        let configured = ensure_project_config(workspace.path()).unwrap();
        assert!(workspace.path().join(SHOAL_CONFIG).is_file());

        assert_eq!(
            resolve_agent_defaults(configured.clone(), None, None).unwrap(),
            configured
        );
        assert_eq!(
            resolve_agent_defaults(
                configured,
                Some("  override-model  ".into()),
                Some(ShoalEffort::High),
            )
            .unwrap(),
            ShoalAgentDefaults {
                model: "override-model".into(),
                effort: ShoalEffort::High,
            }
        );
    }

    #[test]
    fn invalid_project_agent_defaults_fail_at_the_configuration_boundary() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join(SHOAL_CONFIG);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[defaults]\nmodel = \"\"\neffort = \"low\"\n").unwrap();
        let error = ensure_project_config(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("selects an empty model"));

        std::fs::write(
            &path,
            "[defaults]\nmodel = \"test-model\"\neffort = \"furious\"\n",
        )
        .unwrap();
        let error = ensure_project_config(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("invalid Shoal configuration"));
    }

    #[test]
    fn shoal_logs_live_under_the_repository_local_state_directory() {
        assert_eq!(
            shoal_log_path(Path::new("/tmp/project"), "run-1"),
            Path::new("/tmp/project/.shoal/logs/run-1.log")
        );
        assert_eq!(
            shoal_compiler_log_path(Path::new("/tmp/project"), "run-1"),
            Path::new("/tmp/project/.shoal/logs/run-1-compiler.log")
        );
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

    impl CapturedWriter {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    #[test]
    fn host_tracing_fans_out_safe_info_but_keeps_source_debug_in_the_file() {
        let detailed = CapturedWriter::default();
        let pane = CapturedWriter::default();
        let subscriber = host_tracing_subscriber(
            detailed.clone(),
            pane.clone(),
            tracing_subscriber::EnvFilter::new("debug"),
        );

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "tidepool::shoal",
                run_id = "run-visible",
                "host lifecycle visible"
            );
            tracing::debug!(
                target: "tidepool::shoal",
                source = "sensitive Haskell source",
                "host diagnostic detail"
            );
        });

        let detailed = detailed.text();
        let pane = pane.text();
        assert!(detailed.contains("host lifecycle visible"));
        assert!(pane.contains("host lifecycle visible"));
        assert!(detailed.contains("sensitive Haskell source"));
        assert!(!pane.contains("sensitive Haskell source"));
        assert!(!detailed.contains('\u{1b}'));
        assert!(!pane.contains('\u{1b}'));
    }

    #[test]
    fn run_status_round_trips_as_a_closed_sum() {
        let root_actor = ActorRef::first(tidepool_actor::ActorId(9));
        for phase in [
            RunPhase::AwaitingBinding { root_actor },
            RunPhase::Ready {
                root_actor,
                root_thread: BackendThreadId("thread".into()),
            },
        ] {
            let status = RunStatus::new(
                "run-1",
                Path::new("/tmp/work"),
                "shoal-work",
                test_agent_defaults(),
                phase,
            );
            let encoded = serde_json::to_vec(&status).unwrap();
            assert_eq!(status.version, STATUS_VERSION);
            assert_eq!(decode_run_status(&encoded).unwrap(), status);
        }

        let old = serde_json::json!({
            "version": 3,
            "run_id": "run-1",
            "workspace": "/tmp/work",
            "session": "shoal-work",
            "phase": {"state": "awaiting_input", "root_actor": root_actor},
        });
        assert!(decode_run_status(&serde_json::to_vec(&old).unwrap())
            .unwrap_err()
            .to_string()
            .contains("unsupported Shoal run status version 3"));
    }

    #[test]
    fn compiler_daemon_is_tmux_owned_and_only_the_host_receives_its_socket() {
        let workspace = Path::new("/tmp/workspace");
        let socket = Path::new("/tmp/run/compiler.sock");
        let log_path = Path::new("/tmp/workspace/.shoal/logs/run-1-compiler.log");
        let launch = compiler_daemon_launch(
            workspace,
            socket,
            "/tmp/tidepool-extract".into(),
            "run-1",
            log_path,
        );

        assert_eq!(launch.window_name, "Compiler");
        assert_eq!(launch.cwd, workspace);
        assert_eq!(launch.program, "/tmp/tidepool-extract");
        assert_eq!(
            launch.args,
            [
                "--daemon",
                "--socket",
                "/tmp/run/compiler.sock",
                "--persistent",
                "--run-id",
                "run-1",
                "--log-path",
                "/tmp/workspace/.shoal/logs/run-1-compiler.log"
            ]
        );
        assert!(!launch
            .environment
            .contains_key(tidepool_extract_cmd::DAEMON_SOCKET_ENV));
        assert_eq!(
            host_environment(socket)
                .get(tidepool_extract_cmd::DAEMON_SOCKET_ENV)
                .map(String::as_str),
            Some("/tmp/run/compiler.sock")
        );
    }

    #[test]
    fn host_failure_publishes_its_exact_terminal_diagnostic() {
        let root = tempfile::tempdir().unwrap();
        let status_path = root.path().join("status.json");
        let options = HostOptions {
            workspace: root.path().into(),
            session: "shoal-test".into(),
            run_id: "run-failed".into(),
            run_root: root.path().join("run"),
            status_path: status_path.clone(),
            root_binding_path: root.path().join("binding.json"),
            interactive_agent: tidepool_agent::native_interactive_agent_from_parts(
                std::env::current_exe().unwrap(),
                "test installation".into(),
            )
            .unwrap(),
            resume_root: false,
            agent: test_agent_defaults(),
        };
        let result = settle_host_result(Err(runtime_error("compile exploded")), &options);
        assert!(result.is_err());
        let status: RunStatus =
            serde_json::from_slice(&std::fs::read(status_path).unwrap()).unwrap();
        assert_eq!(
            status.phase,
            RunPhase::Failed {
                error: "compile exploded".into()
            }
        );
    }

    #[tokio::test]
    async fn requested_resume_fails_closed_without_a_valid_retained_binding() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing-binding.json");
        let error = resolve_root_launch_mode(true, &missing)
            .await
            .expect_err("resume must not silently become fresh");
        assert!(error
            .to_string()
            .contains("cannot resume the requested root conversation"));
        assert_eq!(
            resolve_root_launch_mode(false, &missing).await.unwrap(),
            InteractiveLaunchMode::Fresh
        );

        let legacy = root.path().join("v3-binding.json");
        tokio::fs::write(
            &legacy,
            r#"{"version":3,"thread":"01a05a16-97f5-7722-aa8d-467e01e2e5b4"}"#,
        )
        .await
        .unwrap();
        let error = resolve_root_launch_mode(true, &legacy)
            .await
            .expect_err("v3 cannot certify queue readiness");
        assert!(error.to_string().contains("start a fresh Shoal root"));
    }

    #[test]
    fn fresh_launch_discards_a_previous_runs_conversation_binding() {
        let root = tempfile::tempdir().unwrap();
        let binding = root.path().join("root-binding.json");
        std::fs::write(&binding, "stale").unwrap();

        clear_fresh_root_binding(&binding).unwrap();
        assert!(!binding.exists());
        clear_fresh_root_binding(&binding).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn packaged_interactive_agent_gets_a_replaceable_project_gc_root() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let fake_nix_store = workspace.path().join("nix-store");
        std::fs::write(&fake_nix_store, "#!/bin/sh\nln -sfn \"$2\" \"$4\"\n").unwrap();
        std::fs::set_permissions(&fake_nix_store, std::fs::Permissions::from_mode(0o700)).unwrap();
        retain_packaged_interactive_agent_from(
            workspace.path(),
            Some(first.path().to_path_buf()),
            Some(fake_nix_store.clone()),
        )
        .await
        .unwrap();
        let link = workspace.path().join(".shoal/runtime/interactive-agent");
        assert_eq!(std::fs::read_link(&link).unwrap(), first.path());

        retain_packaged_interactive_agent_from(
            workspace.path(),
            Some(second.path().to_path_buf()),
            Some(fake_nix_store),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_link(link).unwrap(), second.path());
    }
}
