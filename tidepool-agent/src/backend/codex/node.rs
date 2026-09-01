//! Stock-TUI rollout binding and push operations.
//!
//! The interactive process owns its conversation. Its stdio MCP child learns
//! the exact surrounding rollout from process ancestry, persists that binding,
//! and reports it to Tidepool. Subsequent delivery uses the public queue
//! command; no app-server impersonation or transcript mutation is involved.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::interactive::{
    ENV_NODE_ACTOR_ID, ENV_NODE_BINDING_PATH, ENV_NODE_CREDENTIAL, ENV_NODE_DEVELOPER_INSTRUCTIONS,
    ENV_NODE_ENDPOINT, ENV_NODE_INCARNATION, ENV_NODE_MODEL, ENV_NODE_REASONING_EFFORT,
    ENV_NODE_WORKSPACE,
};
use crate::{
    AgentBackendError, BackendThreadId, InteractiveAgentBackend, InteractiveAgentProcess,
    InteractiveAgentSpec, InteractiveFuture, InteractiveLaunchMode, ReasoningEffort,
};
use tidepool_actor::{ActorId, ActorRef, Incarnation};
use tidepool_node::{NodeCredential, NodeHandshake};

/// Run either the pane-owning host or its stdio MCP child.
pub async fn run_from_env() -> Result<(), AgentBackendError> {
    match std::env::args().nth(1).as_deref() {
        Some("host") => run_host_from_env().await,
        Some("proxy") => run_sidecar_from_env().await,
        Some(mode) => Err(AgentBackendError::ProtocolRejected {
            detail: format!("unknown interactive node mode {mode:?}; expected `host` or `proxy`"),
        }),
        None => Err(AgentBackendError::ProtocolRejected {
            detail: "interactive node mode is required (`host` or `proxy`)".into(),
        }),
    }
}

/// Own one stock Codex TUI for the lifetime of the surrounding tmux pane.
async fn run_host_from_env() -> Result<(), AgentBackendError> {
    let executable = std::env::current_exe()
        .map_err(|error| unavailable("resolve interactive node executable", error))?;
    let executable = executable
        .to_str()
        .ok_or_else(|| AgentBackendError::ProtocolRejected {
            detail: "interactive node executable path is not UTF-8".into(),
        })?
        .to_string();
    let workspace = required_env(ENV_NODE_WORKSPACE)?;
    let effort = optional_effort()?;
    let spec = InteractiveAgentSpec {
        mode: InteractiveLaunchMode::Fresh,
        cwd: workspace.clone(),
        model: optional_env(ENV_NODE_MODEL),
        effort,
        developer_instructions: required_env(ENV_NODE_DEVELOPER_INSTRUCTIONS)?,
        mcp: crate::InteractiveMcpServer {
            name: "tidepool_actor".into(),
            command: executable,
            args: vec!["proxy".into()],
            cwd: workspace,
            forward_env: vec![
                ENV_NODE_ENDPOINT.into(),
                ENV_NODE_ACTOR_ID.into(),
                ENV_NODE_INCARNATION.into(),
                ENV_NODE_CREDENTIAL.into(),
                ENV_NODE_BINDING_PATH.into(),
                ENV_NODE_WORKSPACE.into(),
            ],
            required: true,
        },
    };
    let backend = CodexInteractiveBackend;
    let mut process = backend.launch(spec).await?;
    process.wait().await
}

/// Run the concrete stdio sidecar configured by the daemon-owned launch.
///
/// Rollout discovery is a background task: the TUI may require MCP
/// initialization before it creates its session file, so binding discovery
/// must never delay or deadlock the MCP handshake.
pub async fn run_sidecar_from_env() -> Result<(), AgentBackendError> {
    let endpoint = required_path(ENV_NODE_ENDPOINT)?;
    let binding_path = required_path(ENV_NODE_BINDING_PATH)?;
    let workspace = required_path(ENV_NODE_WORKSPACE)?;
    let actor = ActorRef {
        id: ActorId(required_u64(ENV_NODE_ACTOR_ID)?),
        incarnation: Incarnation(required_u64(ENV_NODE_INCARNATION)?),
    };
    let credential = NodeCredential(required_env(ENV_NODE_CREDENTIAL)?);
    let handshake = NodeHandshake::current(actor, credential);

    let discovery = tokio::spawn(discover_and_bind(
        std::process::id(),
        workspace,
        binding_path,
    ));
    let proxy = tidepool_node::proxy_stdio(&endpoint, &handshake)
        .await
        .map_err(|error| AgentBackendError::BackendUnavailable {
            detail: error.to_string(),
        });
    discovery.abort();
    proxy
}

async fn discover_and_bind(parent_pid: u32, cwd: PathBuf, binding_path: PathBuf) {
    loop {
        match discover_parent_rollout(parent_pid, &cwd) {
            Ok(thread) => match write_binding(&binding_path, thread).await {
                Ok(()) => return,
                Err(error) => tracing::debug!(%error, "rollout binding write not ready"),
            },
            Err(error) => tracing::debug!(%error, "interactive rollout not discoverable yet"),
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn required_env(name: &'static str) -> Result<String, AgentBackendError> {
    std::env::var(name).map_err(|_| AgentBackendError::ProtocolRejected {
        detail: format!("interactive node launch is missing {name}"),
    })
}

fn optional_env(name: &'static str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn optional_effort() -> Result<Option<ReasoningEffort>, AgentBackendError> {
    let Some(value) = optional_env(ENV_NODE_REASONING_EFFORT) else {
        return Ok(None);
    };
    match value.as_str() {
        "low" => Ok(Some(ReasoningEffort::Low)),
        "medium" => Ok(Some(ReasoningEffort::Medium)),
        "high" => Ok(Some(ReasoningEffort::High)),
        _ => Err(AgentBackendError::ProtocolRejected {
            detail: format!(
                "interactive node has invalid {ENV_NODE_REASONING_EFFORT}={value:?}; expected low, medium, or high"
            ),
        }),
    }
}

fn required_path(name: &'static str) -> Result<PathBuf, AgentBackendError> {
    required_env(name).map(PathBuf::from)
}

fn required_u64(name: &'static str) -> Result<u64, AgentBackendError> {
    let value = required_env(name)?;
    value
        .parse()
        .map_err(|_| AgentBackendError::ProtocolRejected {
            detail: format!("interactive node launch has invalid {name}={value:?}"),
        })
}

/// Adapter for an ordinary interactive TUI and its public push commands.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexInteractiveBackend;

struct CodexInteractiveProcess {
    child: tokio::process::Child,
}

impl InteractiveAgentProcess for CodexInteractiveProcess {
    fn wait(&mut self) -> InteractiveFuture<'_, ()> {
        Box::pin(async move {
            let status = self
                .child
                .wait()
                .await
                .map_err(|error| unavailable("wait for interactive process", error))?;
            if status.success() {
                Ok(())
            } else {
                Err(AgentBackendError::RunFailed {
                    detail: format!("interactive process exited with {status}"),
                })
            }
        })
    }

    fn shutdown(mut self: Box<Self>) -> InteractiveFuture<'static, ()> {
        Box::pin(async move {
            self.child
                .start_kill()
                .map_err(|error| unavailable("stop interactive process", error))?;
            self.child
                .wait()
                .await
                .map_err(|error| unavailable("reap interactive process", error))?;
            Ok(())
        })
    }
}

impl InteractiveAgentBackend for CodexInteractiveBackend {
    fn launch(
        &self,
        spec: InteractiveAgentSpec,
    ) -> InteractiveFuture<'_, Box<dyn InteractiveAgentProcess>> {
        Box::pin(async move {
            let mut command = command_for(&spec)?;
            let child = command
                .spawn()
                .map_err(|error| unavailable("launch interactive process", error))?;
            Ok(Box::new(CodexInteractiveProcess { child }) as Box<dyn InteractiveAgentProcess>)
        })
    }

    fn push<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a BackendThreadId,
        message: &'a str,
    ) -> InteractiveFuture<'a, ()> {
        Box::pin(queue_message(Path::new(cwd), thread, message))
    }

    fn archive<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a BackendThreadId,
    ) -> InteractiveFuture<'a, ()> {
        Box::pin(archive_thread(Path::new(cwd), thread))
    }
}

fn command_for(spec: &InteractiveAgentSpec) -> Result<Command, AgentBackendError> {
    validate_mcp_name(&spec.mcp.name)?;
    let mut command = Command::new("codex");
    match &spec.mode {
        InteractiveLaunchMode::Fresh => {}
        InteractiveLaunchMode::Resume(thread) => {
            validate_thread(thread)?;
            command.arg("resume").arg(&thread.0);
        }
        InteractiveLaunchMode::Fork(thread) => {
            validate_thread(thread)?;
            command.arg("fork").arg(&thread.0);
        }
    }
    command
        .arg("--ask-for-approval")
        .arg("never")
        .arg("--sandbox")
        .arg("danger-full-access");
    if let Some(model) = &spec.model {
        command.arg("--model").arg(model);
    }
    if let Some(effort) = spec.effort {
        push_config_string(
            &mut command,
            "model_reasoning_effort",
            match effort {
                ReasoningEffort::Low => "low",
                ReasoningEffort::Medium => "medium",
                ReasoningEffort::High => "high",
            },
        )?;
    }
    push_config_string(
        &mut command,
        "developer_instructions",
        &spec.developer_instructions,
    )?;

    let prefix = format!("mcp_servers.{}", spec.mcp.name);
    push_config_string(
        &mut command,
        &format!("{prefix}.command"),
        &spec.mcp.command,
    )?;
    push_config_value(
        &mut command,
        &format!("{prefix}.args"),
        serde_json::to_string(&spec.mcp.args).map_err(config_encode_error)?,
    );
    // Override a same-named project/global server completely. Values arrive
    // only through the explicit inherited-name membrane below.
    push_config_value(&mut command, &format!("{prefix}.env"), "{}".into());
    push_config_string(&mut command, &format!("{prefix}.cwd"), &spec.mcp.cwd)?;
    let mut forward_env = spec.mcp.forward_env.clone();
    forward_env.extend(["TMUX".to_string(), "TMUX_PANE".to_string()]);
    forward_env.sort();
    forward_env.dedup();
    push_config_value(
        &mut command,
        &format!("{prefix}.env_vars"),
        serde_json::to_string(&forward_env).map_err(config_encode_error)?,
    );
    push_config_value(
        &mut command,
        &format!("{prefix}.required"),
        spec.mcp.required.to_string(),
    );
    push_config_value(&mut command, &format!("{prefix}.enabled"), "true".into());

    command
        .current_dir(&spec.cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    Ok(command)
}

fn push_config_string(
    command: &mut Command,
    key: &str,
    value: &str,
) -> Result<(), AgentBackendError> {
    push_config_value(
        command,
        key,
        serde_json::to_string(value).map_err(config_encode_error)?,
    );
    Ok(())
}

fn push_config_value(command: &mut Command, key: &str, value: String) {
    command.arg("-c").arg(format!("{key}={value}"));
}

fn config_encode_error(error: serde_json::Error) -> AgentBackendError {
    AgentBackendError::ProtocolRejected {
        detail: format!("cannot encode interactive launch configuration: {error}"),
    }
}

fn validate_mcp_name(name: &str) -> Result<(), AgentBackendError> {
    if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Ok(());
    }
    Err(AgentBackendError::ProtocolRejected {
        detail: format!("invalid MCP server name {name:?}"),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutBinding {
    pub version: u32,
    pub thread: BackendThreadId,
}

impl RolloutBinding {
    /// V1 trusted Codex MCP `_meta.threadId`, which may identify the hosted
    /// conversation rather than this local resumable TUI rollout. V2 is
    /// discovered from the owning process ancestry and open session file.
    pub const VERSION: u32 = 2;

    pub fn new(thread: BackendThreadId) -> Result<Self, AgentBackendError> {
        validate_thread(&thread)?;
        Ok(Self {
            version: Self::VERSION,
            thread,
        })
    }
}

pub async fn read_binding(path: &Path) -> Result<RolloutBinding, AgentBackendError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| unavailable("read rollout binding", error))?;
    let binding: RolloutBinding =
        serde_json::from_slice(&bytes).map_err(|error| AgentBackendError::ProtocolRejected {
            detail: format!("invalid rollout binding {}: {error}", path.display()),
        })?;
    if binding.version != RolloutBinding::VERSION {
        return Err(AgentBackendError::ProtocolRejected {
            detail: format!(
                "unsupported rollout binding version {} (expected {})",
                binding.version,
                RolloutBinding::VERSION
            ),
        });
    }
    validate_thread(&binding.thread)?;
    Ok(binding)
}

pub async fn write_binding(path: &Path, thread: BackendThreadId) -> Result<(), AgentBackendError> {
    let binding = RolloutBinding::new(thread)?;
    let bytes = serde_json::to_vec_pretty(&binding).map_err(|error| {
        AgentBackendError::ProtocolRejected {
            detail: format!("cannot encode rollout binding: {error}"),
        }
    })?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| unavailable("create rollout binding directory", error))?;
    }
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || tidepool_atomic_write::write_durable(&path, &bytes))
        .await
        .map_err(|error| AgentBackendError::BackendUnavailable {
            detail: format!("rollout binding writer failed: {error}"),
        })?
        .map_err(|error| AgentBackendError::BackendUnavailable {
            detail: format!("cannot persist rollout binding: {error}"),
        })
}

pub async fn queue_message(
    cwd: &Path,
    thread: &BackendThreadId,
    message: &str,
) -> Result<(), AgentBackendError> {
    validate_thread(thread)?;
    run_cli(
        cwd,
        "queue",
        ["queue", "--thread", thread.0.as_str(), "--message", message],
    )
    .await
}

pub async fn archive_thread(cwd: &Path, thread: &BackendThreadId) -> Result<(), AgentBackendError> {
    validate_thread(thread)?;
    run_cli(cwd, "archive", ["archive", thread.0.as_str()]).await
}

/// Discover the rollout owned by the surrounding stock TUI.
///
/// MCP children are not guaranteed to be direct children of the TUI, so this
/// walks process ancestry and chooses the newest open non-subagent rollout
/// whose recorded working directory matches the actor workspace.
#[cfg(target_os = "linux")]
pub fn discover_parent_rollout(
    parent_pid: u32,
    cwd: &Path,
) -> Result<BackendThreadId, AgentBackendError> {
    let canonical_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_owned());
    let mut pid = parent_pid;
    for _ in 0..16 {
        let candidates = rollout_candidates(pid, &canonical_cwd)?;
        if let Some((_, thread)) = candidates.into_iter().max_by_key(|(modified, _)| *modified) {
            return Ok(thread);
        }
        let next = process_parent_pid(pid)?;
        if next == 0 || next == pid {
            break;
        }
        pid = next;
    }
    Err(AgentBackendError::BackendUnavailable {
        detail: "no open stock-TUI rollout matched the MCP process ancestry and workspace"
            .to_string(),
    })
}

#[cfg(target_os = "linux")]
fn rollout_candidates(
    pid: u32,
    canonical_cwd: &Path,
) -> Result<Vec<(std::time::SystemTime, BackendThreadId)>, AgentBackendError> {
    let entries = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .map_err(|error| unavailable("inspect MCP process ancestry", error))?;
    let mut candidates = Vec::new();
    for entry in entries {
        let target = match entry.and_then(|entry| std::fs::read_link(entry.path())) {
            Ok(target) => target,
            Err(_) => continue,
        };
        if target.extension().and_then(|value| value.to_str()) != Some("jsonl")
            || !target.to_string_lossy().contains("/.codex/sessions/")
        {
            continue;
        }
        let contents = match std::fs::read_to_string(&target) {
            Ok(contents) => contents,
            Err(_) => continue,
        };
        let value: serde_json::Value = match contents.lines().next().map(serde_json::from_str) {
            Some(Ok(value)) => value,
            _ => continue,
        };
        let payload = &value["payload"];
        if payload["thread_source"].as_str() == Some("subagent") {
            continue;
        }
        let Some(recorded_cwd) = payload["cwd"].as_str() else {
            continue;
        };
        let recorded_cwd =
            std::fs::canonicalize(recorded_cwd).unwrap_or_else(|_| PathBuf::from(recorded_cwd));
        if recorded_cwd != canonical_cwd {
            continue;
        }
        let Some(id) = payload["id"].as_str() else {
            continue;
        };
        let thread = BackendThreadId(id.to_owned());
        if validate_thread(&thread).is_err() {
            continue;
        }
        let modified = std::fs::metadata(&target)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| unavailable("inspect rollout descriptor", error))?;
        candidates.push((modified, thread));
    }
    Ok(candidates)
}

#[cfg(target_os = "linux")]
fn process_parent_pid(pid: u32) -> Result<u32, AgentBackendError> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|error| unavailable("inspect MCP process parent", error))?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:\t"))
        .and_then(|value| value.trim().parse().ok())
        .ok_or_else(|| AgentBackendError::ProtocolRejected {
            detail: format!("cannot parse parent pid for process {pid}"),
        })
}

fn validate_thread(thread: &BackendThreadId) -> Result<(), AgentBackendError> {
    uuid::Uuid::parse_str(&thread.0)
        .map(|_| ())
        .map_err(|_| AgentBackendError::ProtocolRejected {
            detail: format!("invalid interactive thread id {:?}", thread.0),
        })
}

async fn run_cli<'a>(
    cwd: &Path,
    operation: &'static str,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<(), AgentBackendError> {
    let output = Command::new("codex")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|error| unavailable(operation, error))?;
    if output.status.success() {
        return Ok(());
    }
    Err(AgentBackendError::RunFailed {
        detail: format!(
            "interactive {operation} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    })
}

fn unavailable(operation: &str, error: impl std::fmt::Display) -> AgentBackendError {
    AgentBackendError::BackendUnavailable {
        detail: format!("{operation}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREAD: &str = "01a05a16-97f5-7722-aa8d-467e01e2e5b4";

    #[tokio::test]
    async fn binding_round_trip_is_current_and_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binding.json");
        let thread = BackendThreadId(THREAD.to_string());
        write_binding(&path, thread.clone()).await.unwrap();
        assert_eq!(
            read_binding(&path).await.unwrap(),
            RolloutBinding {
                version: RolloutBinding::VERSION,
                thread
            }
        );
    }

    #[tokio::test]
    async fn legacy_and_invalid_bindings_fail_typed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binding.json");
        tokio::fs::write(&path, format!(r#"{{"thread": "{THREAD}"}}"#))
            .await
            .unwrap();
        assert!(matches!(
            read_binding(&path).await,
            Err(AgentBackendError::ProtocolRejected { .. })
        ));

        assert!(matches!(
            RolloutBinding::new(BackendThreadId("not-a-thread".to_string())),
            Err(AgentBackendError::ProtocolRejected { .. })
        ));
    }

    #[test]
    fn fresh_tui_launch_is_idle_and_installs_only_its_scoped_mcp_server() {
        let spec = InteractiveAgentSpec {
            mode: InteractiveLaunchMode::Fresh,
            cwd: "/tmp/work".to_string(),
            model: Some("gpt-test".to_string()),
            effort: Some(ReasoningEffort::Medium),
            developer_instructions: "actor charter".to_string(),
            mcp: crate::InteractiveMcpServer {
                name: "tidepool_actor".to_string(),
                command: "tidepool-node".to_string(),
                args: vec!["proxy".to_string()],
                cwd: "/tmp/work".to_string(),
                forward_env: vec!["TIDEPOOL_NODE_ENDPOINT".to_string()],
                required: true,
            },
        };
        let command = command_for(&spec).unwrap();
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(command.as_std().get_program(), "codex");
        assert!(!args.iter().any(|arg| arg == "resume" || arg == "fork"));
        assert!(!args.iter().any(|arg| arg.contains("initial task")));
        assert!(args
            .windows(2)
            .any(|args| args == ["--ask-for-approval", "never"]));
        assert!(args
            .windows(2)
            .any(|args| args == ["--sandbox", "danger-full-access"]));
        assert!(args
            .iter()
            .any(|arg| arg.contains("mcp_servers.tidepool_actor.command")));
        assert!(args
            .iter()
            .any(|arg| arg.contains("developer_instructions")));
        assert!(args
            .iter()
            .any(|arg| arg == "mcp_servers.tidepool_actor.required=true"));
        assert!(args
            .iter()
            .any(|arg| arg == "mcp_servers.tidepool_actor.env={}"));
        let inherited = args
            .iter()
            .find(|arg| arg.starts_with("mcp_servers.tidepool_actor.env_vars="))
            .expect("scoped MCP inherited environment");
        assert!(inherited.contains("TMUX"));
        assert!(inherited.contains("TMUX_PANE"));
    }

    #[test]
    fn mcp_name_cannot_escape_its_config_namespace() {
        let mut spec = InteractiveAgentSpec {
            mode: InteractiveLaunchMode::Fresh,
            cwd: "/tmp/work".to_string(),
            model: None,
            effort: None,
            developer_instructions: String::new(),
            mcp: crate::InteractiveMcpServer {
                name: "bad.name".to_string(),
                command: "proxy".to_string(),
                args: Vec::new(),
                cwd: "/tmp/work".to_string(),
                forward_env: Vec::new(),
                required: true,
            },
        };
        assert!(matches!(
            command_for(&spec),
            Err(AgentBackendError::ProtocolRejected { .. })
        ));
        spec.mcp.name = "good_name-2".to_string();
        assert!(command_for(&spec).is_ok());
    }
}
