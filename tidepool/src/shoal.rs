//! Shoal process composition and one-command tmux bootstrap.
//!
//! One host process owns every resident Haskell actor. Interactive actors are
//! ordinary Codex TUIs launched directly in tmux panes; `shoal proxy` is only
//! the authenticated stdio MCP transport child that Codex requires.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tidepool_actor::ActorRef;
use tidepool_agent::{
    read_interactive_binding, BackendThreadId, InteractiveLaunchMode, ReasoningEffort,
};
use tidepool_node::{TmuxLaunch, TmuxSession};
use tokio::sync::oneshot;

const STATUS_VERSION: u32 = 1;
const READY_TIMEOUT: Duration = Duration::from_secs(120);

pub struct InitOptions {
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
    let cwd = std::env::current_dir()?;
    let workspace = tidepool_runtime::paths::find_project_root(&cwd).unwrap_or(cwd);
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
    preflight().await?;
    if options.recreate {
        tmux.kill().await?;
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let run_root = tidepool_runtime::paths::cache_dir()
        .join("shoal")
        .join("runs")
        .join(&run_id);
    std::fs::create_dir_all(&run_root)?;
    let status_path = run_root.join("status.json");
    let session_root = workspace
        .join(".tidepool")
        .join("shoal")
        .join(&session_name);
    std::fs::create_dir_all(&session_root)?;
    let root_binding_path = session_root.join("root-binding.json");
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

    let ready = match wait_until_ready(&tmux, &status_path, &run_id).await {
        Ok(ready) => ready,
        Err(failure) => {
            let _ = tmux.kill().await;
            return Err(failure);
        }
    };
    let RunPhase::Ready {
        root_actor,
        root_thread,
    } = &ready.phase
    else {
        unreachable!("wait_until_ready returns only Ready")
    };
    println!(
        "Shoal ready in tmux session {session_name:?}: actor {root_actor:?}, thread {}",
        root_thread.0
    );
    println!("status: {}", status_path.display());
    if options.no_attach {
        println!("attach: tmux attach -t {session_name}");
        println!("logs:   tmux capture-pane -p -S -200 -t {session_name}:Host");
        println!("stop:   tmux kill-session -t {session_name}");
        Ok(())
    } else {
        tmux.attach_or_switch()
            .await
            .map_err(|failure| Box::new(failure) as Box<dyn std::error::Error>)
    }
}

pub async fn host(options: HostOptions) -> Result<(), Box<dyn std::error::Error>> {
    let result = run_host(&options).await;
    let failed = result.is_err();
    let settled = settle_host_result(result, &options);
    if failed {
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

    let root_launch_mode = if options.resume_root {
        match read_interactive_binding(&options.root_binding_path).await {
            Ok(thread) => InteractiveLaunchMode::Resume(thread),
            Err(error) => {
                eprintln!("shoal host: retained root binding unavailable; starting fresh: {error}");
                InteractiveLaunchMode::Fresh
            }
        }
    } else {
        InteractiveLaunchMode::Fresh
    };
    match tokio::fs::remove_file(&options.root_binding_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let policy_root = crate::haskell_sources::ensure_actor_policy()?;
    let (readiness_tx, readiness_rx) = oneshot::channel();
    let run = crate::actor_host::run(
        crate::actor_host::ActorHostConfig {
            workspace: options.workspace.clone(),
            policy_root,
            run_root: options.run_root.clone(),
            root_binding_path: options.root_binding_path.clone(),
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
    tokio::pin!(readiness_rx);
    tokio::select! {
        ready = &mut readiness_rx => {
            let ready = match ready {
                Ok(ready) => ready,
                Err(_) => return run.await,
            };
            let status = RunStatus::new(
                &options.run_id,
                &options.workspace,
                &options.session,
                RunPhase::Ready {
                    root_actor: ready.root,
                    root_thread: ready.thread,
                },
            );
            write_status(&options.status_path, &status)?;
        }
        result = &mut run => return result,
    }

    run.await
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
        eprintln!("shoal host: could not publish terminal status: {error}");
    }
    result
}

async fn preflight() -> Result<(), Box<dyn std::error::Error>> {
    crate::haskell_sources::ensure_stdlib()?;
    crate::haskell_sources::ensure_actor_policy()?;
    tidepool_runtime::toolchain::bind_extract_endpoint()?;

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
    Ok(())
}

async fn wait_until_ready(
    tmux: &TmuxSession,
    status_path: &Path,
    run_id: &str,
) -> Result<RunStatus, Box<dyn std::error::Error>> {
    tokio::time::timeout(READY_TIMEOUT, async {
        loop {
            if let Ok(bytes) = tokio::fs::read(status_path).await {
                let status: RunStatus = serde_json::from_slice(&bytes)?;
                if status.run_id == run_id {
                    match &status.phase {
                        RunPhase::Ready { .. } => return Ok(status),
                        RunPhase::Failed { error } => {
                            return Err(runtime_error(format!("Shoal host failed: {error}")))
                        }
                        RunPhase::Exited => {
                            return Err(runtime_error("Shoal host exited before becoming ready"))
                        }
                        RunPhase::Starting => {}
                    }
                }
            }
            if !tmux.exists().await? {
                return Err(runtime_error(
                    "Shoal tmux session exited before becoming ready",
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| {
        runtime_error(format!(
            "Shoal did not become ready within {READY_TIMEOUT:?}"
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
    fn run_status_round_trips_as_a_closed_sum() {
        let status = RunStatus::new(
            "run-1",
            Path::new("/tmp/work"),
            "shoal-work",
            RunPhase::Ready {
                root_actor: ActorRef::first(tidepool_actor::ActorId(9)),
                root_thread: BackendThreadId("thread".into()),
            },
        );
        let encoded = serde_json::to_vec(&status).unwrap();
        assert_eq!(
            serde_json::from_slice::<RunStatus>(&encoded).unwrap(),
            status
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
}
