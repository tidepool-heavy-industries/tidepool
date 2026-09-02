//! Shoal process composition and one-command tmux bootstrap.
//!
//! One host process owns every resident Haskell actor. Interactive actors are
//! ordinary Codex TUIs launched directly in tmux panes; `shoal proxy` is only
//! the authenticated stdio MCP transport child that Codex requires.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tidepool_actor::ActorRef;
use tidepool_agent::{
    persist_interactive_binding, read_interactive_binding, BackendThreadId, InteractiveLaunchMode,
    ReasoningEffort,
};
use tidepool_node::{TmuxLaunch, TmuxSession};
use tokio::sync::mpsc;

use crate::actor_host::ACTOR_PROJECT_ROOT;

const STATUS_VERSION: u32 = 2;
const INTERACTIVE_START_TIMEOUT: Duration = Duration::from_secs(120);

pub struct InitOptions {
    pub workspace: Option<PathBuf>,
    pub session: Option<String>,
    pub recreate: bool,
    pub no_attach: bool,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
}

pub struct HostOptions {
    pub workspace: PathBuf,
    pub session: String,
    pub run_id: String,
    pub run_root: PathBuf,
    pub status_path: PathBuf,
    pub root_binding_path: PathBuf,
    pub resume_root: bool,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStatus {
    pub version: u32,
    pub run_id: String,
    pub workspace: PathBuf,
    pub session: String,
    pub phase: RunPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunPhase {
    Starting,
    AwaitingInput {
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
    preflight(&workspace).await?;
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
    let run_root = tidepool_runtime::paths::cache_dir()
        .join("shoal")
        .join("runs")
        .join(&run_id);
    std::fs::create_dir_all(&run_root)?;
    let status_path = run_root.join("status.json");
    write_status(
        &status_path,
        &RunStatus::new(&run_id, &workspace, &session_name, RunPhase::Starting),
    )?;

    let executable = current_executable()?;
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
    ];
    if options.recreate {
        args.push("--resume-root".into());
    }
    if let Some(model) = options.model {
        args.extend(["--model".into(), model]);
    }
    if let Some(effort) = options.effort {
        args.extend(["--effort".into(), effort_name(effort).into()]);
    }
    let launch = tmux
        .create(&TmuxLaunch {
            window_name: "Host".into(),
            cwd: workspace.clone(),
            program: executable,
            args,
            environment: pane_environment(),
            unset_environment: std::collections::BTreeSet::new(),
        })
        .await;
    if let Err(error) = launch {
        let failed = RunStatus::new(
            &run_id,
            &workspace,
            &session_name,
            RunPhase::Failed {
                error: error.to_string(),
            },
        );
        write_status(&status_path, &failed)?;
        return Err(error.into());
    }
    println!("log:    {}", log_path.display());

    let interactive = match wait_until_interactive(&tmux, &status_path, &run_id).await {
        Ok(interactive) => interactive,
        Err(failure) => {
            let _ = tmux.kill().await;
            return Err(failure);
        }
    };
    match &interactive.phase {
        RunPhase::AwaitingInput { root_actor } => println!(
            "Shoal ready in tmux session {session_name:?}: actor {root_actor:?} is idle; its conversation will bind on the first real input"
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

pub async fn host(options: HostOptions) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!(
        run_id = %options.run_id,
        session = %options.session,
        workspace = %options.workspace.display(),
        "starting Shoal actor host"
    );
    let result = run_host(&options).await;
    let failed = result.is_err();
    let settled = settle_host_result(result, &options);
    if failed {
        if let Err(error) = &settled {
            tracing::error!(error = %error, "Shoal actor host failed");
        }
        if let Ok(tmux) = TmuxSession::new(options.session.clone()) {
            let _ = tmux.kill().await;
        }
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

    let policy_root = crate::haskell_sources::ensure_actor_policy()?;
    let (readiness_tx, mut readiness_rx) = mpsc::unbounded_channel();
    let run = crate::actor_host::run(
        crate::actor_host::ActorHostConfig {
            workspace: options.workspace.clone(),
            policy_root,
            run_root: options.run_root.clone(),
            root_binding_path: options.run_root.join("root-binding.json"),
            proxy_program: current_executable()?,
            proxy_args: vec!["proxy".into()],
            tmux_session: options.session.clone(),
            model: options.model.clone(),
            effort: options.effort,
            root_launch_mode,
            pane_environment: pane_environment(),
        },
        readiness_tx,
    );

    tokio::pin!(run);
    loop {
        tokio::select! {
            readiness = readiness_rx.recv() => match readiness {
                Some(crate::actor_host::ActorHostReadiness::AwaitingInput { root }) => {
                    let status = RunStatus::new(
                        &options.run_id,
                        &options.workspace,
                        &options.session,
                        RunPhase::AwaitingInput { root_actor: root },
                    );
                    write_status(&options.status_path, &status)?;
                }
                Some(crate::actor_host::ActorHostReadiness::Ready { root, thread }) => {
                    persist_interactive_binding(
                        &options.root_binding_path,
                        thread.clone(),
                    )
                    .await?;
                    let status = RunStatus::new(
                        &options.run_id,
                        &options.workspace,
                        &options.session,
                        RunPhase::Ready {
                            root_actor: root,
                            root_thread: thread,
                        },
                    );
                    write_status(&options.status_path, &status)?;
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
        .map(InteractiveLaunchMode::Resume)
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
    let status = RunStatus::new(&options.run_id, &options.workspace, &options.session, phase);
    if let Err(error) = write_status(&options.status_path, &status) {
        tracing::error!(error = %error, "could not publish terminal Shoal status");
    }
    result
}

async fn preflight(workspace: &Path) -> Result<(), Box<dyn std::error::Error>> {
    crate::haskell_sources::ensure_stdlib()?;
    crate::haskell_sources::ensure_actor_policy()?;
    tidepool_runtime::toolchain::bind_extract_endpoint()?;

    // Codex keys its interactive trust decision by the path visible inside
    // its process. Every actor gets an isolated repository mounted at this one
    // stable slot, so this creates one durable entry rather than one per run.
    std::fs::create_dir_all(ACTOR_PROJECT_ROOT)?;
    tidepool_agent::trust_interactive_project(Path::new(ACTOR_PROJECT_ROOT))?;

    let output = tokio::process::Command::new("codex")
        .args(["queue", "--help"])
        .output()
        .await
        .map_err(|source| {
            runtime_error(format!("Codex with queue support is required: {source}"))
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !stdout.contains("--thread") || !stdout.contains("--message") {
        return Err(runtime_error(
            "installed Codex lacks `codex queue --thread --message` support",
        ));
    }

    let boundary = tokio::process::Command::new(tidepool_node::BUBBLEWRAP_PROGRAM)
        .args(["--bind", "/", "/", "--ro-bind"])
        .arg(workspace)
        .arg(workspace)
        .args(["--chdir"])
        .arg(workspace)
        // Resolve through the dev/runtime PATH. NixOS deliberately does not
        // provide the FHS `/bin/true` path.
        .args(["--", "true"])
        .output()
        .await
        .map_err(|source| {
            runtime_error(format!(
                "Bubblewrap is required for Shoal actor worktrees: {source}"
            ))
        })?;
    if !boundary.status.success() {
        return Err(runtime_error(format!(
            "Bubblewrap cannot establish the Shoal process boundary: {}",
            String::from_utf8_lossy(&boundary.stderr).trim()
        )));
    }
    Ok(())
}

async fn wait_until_interactive(
    tmux: &TmuxSession,
    status_path: &Path,
    run_id: &str,
) -> Result<RunStatus, Box<dyn std::error::Error>> {
    tokio::time::timeout(INTERACTIVE_START_TIMEOUT, async {
        loop {
            if let Ok(bytes) = tokio::fs::read(status_path).await {
                let status: RunStatus = serde_json::from_slice(&bytes)?;
                if status.run_id == run_id {
                    match &status.phase {
                        RunPhase::AwaitingInput { .. } | RunPhase::Ready { .. } => {
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

impl RunStatus {
    fn new(run_id: &str, workspace: &Path, session: &str, phase: RunPhase) -> Self {
        Self {
            version: STATUS_VERSION,
            run_id: run_id.into(),
            workspace: workspace.into(),
            session: session.into(),
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
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(tidepool_codegen::debug::tracing_env_filter("info"))
        .with_writer(Mutex::new(file))
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

fn effort_name(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
}

/// Variables whose current values must override a possibly older tmux server
/// environment. Keep this membrane narrow: proxy credentials are added per
/// actor, and tmux supplies fresh `TMUX`/`TMUX_PANE` identities itself.
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
    fn shoal_logs_live_under_the_repository_local_state_directory() {
        assert_eq!(
            shoal_log_path(Path::new("/tmp/project"), "run-1"),
            Path::new("/tmp/project/.shoal/logs/run-1.log")
        );
    }

    #[test]
    fn run_status_round_trips_as_a_closed_sum() {
        let root_actor = ActorRef::first(tidepool_actor::ActorId(9));
        for phase in [
            RunPhase::AwaitingInput { root_actor },
            RunPhase::Ready {
                root_actor,
                root_thread: BackendThreadId("thread".into()),
            },
        ] {
            let status = RunStatus::new("run-1", Path::new("/tmp/work"), "shoal-work", phase);
            let encoded = serde_json::to_vec(&status).unwrap();
            assert_eq!(
                serde_json::from_slice::<RunStatus>(&encoded).unwrap(),
                status
            );
        }
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
            resume_root: false,
            model: None,
            effort: None,
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
}
