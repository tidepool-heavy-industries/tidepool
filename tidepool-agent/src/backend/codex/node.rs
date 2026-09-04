//! Interactive Codex installation, launch, and lifecycle operations.
//!
//! Shoal resolves and behaviorally probes one absolute executable before it
//! mutates tmux state. The resulting value is the sole program used for TUI
//! launch, queue delivery, and archival. Actor tools are supplied through the
//! fork's HTTP/1.1-over-UDS host dynamic-tool boundary.

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tidepool_extract_cmd::exec_check::is_readable_executable_file;
use tidepool_repr::version_ladder::{self, LadderError};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::{
    AgentBackendError, BackendThreadId, InteractiveAgentBackend, InteractiveAgentCommand,
    InteractiveAgentInstallation, InteractiveAgentSpec, InteractiveFuture, InteractiveLaunchMode,
    InteractiveNativeSandbox, InteractiveNativeToolPolicy, InteractivePolicyMount,
    QueueReadyThread, ReasoningEffort, TokenUsage,
};

const ENV_INTERACTIVE_CODEX_BIN: &str = "TIDEPOOL_INTERACTIVE_CODEX_BIN";
const PROBE_DEADLINE: Duration = Duration::from_secs(10);
const CLI_DEADLINE: Duration = Duration::from_secs(30);
const CAPTURE_LIMIT: usize = 64 * 1024;
const INSPECTION_POLICY_FILE: &str = "tidepool-inspection.rules";
const INSPECTION_POLICY: &str = r#"
prefix_rule(
    pattern = [["cargo", "rustc", "rustdoc", "rustfmt", "cabal", "stack", "ghc", "ghci", "hpack", "make", "gmake", "ninja", "cmake", "meson", "just", "gradle", "mvn", "ant"]],
    decision = "forbidden",
    justification = "Inspection-only actors inspect existing evidence. Delegate builds and artifact production to a coding actor.",
)
prefix_rule(
    pattern = ["nix", ["build", "develop", "shell", "run"]],
    decision = "forbidden",
    justification = "Inspection-only actors do not enter build environments. Delegate validation to a coding actor.",
)
prefix_rule(
    pattern = [["npm", "npx", "yarn", "pnpm", "bun", "deno"], ["run", "test", "build", "install", "add", "exec", "fmt"]],
    decision = "forbidden",
    justification = "Inspection-only actors do not run package, build, test, generator, or formatter commands.",
)
prefix_rule(
    pattern = ["go", ["build", "test", "generate", "install", "run"]],
    decision = "forbidden",
    justification = "Inspection-only actors do not build, test, generate, install, or run project programs.",
)
prefix_rule(
    pattern = [["pytest", "tox", "nox", "prettier", "black", "ruff", "clang-format", "goimports"]],
    decision = "forbidden",
    justification = "Inspection-only actors do not run tests, formatters, or generators.",
)
prefix_rule(
    pattern = ["git", ["add", "am", "apply", "cherry-pick", "commit", "merge", "rebase", "reset", "restore", "switch"]],
    decision = "forbidden",
    justification = "Inspection-only actors may inspect Git but must delegate repository mutation to a coding actor.",
)
"#;
pub const HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION: u32 = 2;

/// Resolve and behaviorally verify the interactive Codex executable.
///
/// A configured override is strict: an invalid value never falls through to a
/// different `codex` on `PATH`.
pub async fn resolve_installation() -> Result<InteractiveAgentInstallation, AgentBackendError> {
    let executable = resolve_executable()?;
    let version_output = probe(&executable, &["--version"], "read version").await?;
    require_probe(
        &executable,
        &["--help"],
        "host dynamic tools",
        &["--host-dynamic-tools-socket"],
    )
    .await?;
    require_probe(
        &executable,
        &["queue", "--help"],
        "queue delivery",
        &["--thread", "--message"],
    )
    .await?;
    require_probe(
        &executable,
        &["archive", "--help"],
        "conversation archival",
        &[],
    )
    .await?;
    let version = first_nonempty_line(&version_output).ok_or_else(|| {
        AgentBackendError::ProtocolRejected {
            detail: "interactive Codex returned an empty version".into(),
        }
    })?;
    Ok(InteractiveAgentInstallation::new(executable, version))
}

/// Reconstitute an installation already verified by Shoal's parent process.
pub fn installation_from_parts(
    executable: PathBuf,
    version: String,
) -> Result<InteractiveAgentInstallation, AgentBackendError> {
    if !executable.is_absolute() || !is_readable_executable_file(&executable) {
        return Err(AgentBackendError::BackendUnavailable {
            detail: format!(
                "interactive Codex executable is no longer usable: {}",
                executable.display()
            ),
        });
    }
    if version.trim().is_empty() {
        return Err(AgentBackendError::ProtocolRejected {
            detail: "interactive Codex version is empty".into(),
        });
    }
    Ok(InteractiveAgentInstallation::new(executable, version))
}

fn resolve_executable() -> Result<PathBuf, AgentBackendError> {
    resolve_executable_from(
        std::env::var_os(ENV_INTERACTIVE_CODEX_BIN),
        std::env::var_os("PATH"),
    )
}

fn resolve_executable_from(
    configured: Option<OsString>,
    search_path: Option<OsString>,
) -> Result<PathBuf, AgentBackendError> {
    if let Some(configured) = configured {
        let path = PathBuf::from(configured);
        if !path.is_absolute() || !is_readable_executable_file(&path) {
            return Err(AgentBackendError::BackendUnavailable {
                detail: format!(
                    "{ENV_INTERACTIVE_CODEX_BIN} must name an absolute readable executable file: {}",
                    path.display()
                ),
            });
        }
        return std::fs::canonicalize(&path)
            .map_err(|error| unavailable("canonicalize interactive Codex", error));
    }

    let path = search_path.ok_or_else(|| AgentBackendError::BackendUnavailable {
        detail: format!("{ENV_INTERACTIVE_CODEX_BIN} is unset and PATH is unavailable"),
    })?;
    for directory in std::env::split_paths(&path) {
        for name in executable_names() {
            let candidate = directory.join(name);
            if is_readable_executable_file(&candidate) {
                return std::fs::canonicalize(&candidate)
                    .map_err(|error| unavailable("canonicalize interactive Codex", error));
            }
        }
    }
    Err(AgentBackendError::BackendUnavailable {
        detail: format!(
            "interactive Codex was not found; set {ENV_INTERACTIVE_CODEX_BIN} to the custom build"
        ),
    })
}

#[cfg(windows)]
fn executable_names() -> &'static [&'static str] {
    &["codex.exe", "codex"]
}

#[cfg(not(windows))]
fn executable_names() -> &'static [&'static str] {
    &["codex"]
}

async fn require_probe(
    executable: &Path,
    args: &[&str],
    capability: &str,
    needles: &[&str],
) -> Result<(), AgentBackendError> {
    let output = probe(executable, args, capability).await?;
    if needles.iter().all(|needle| output.contains(needle)) {
        return Ok(());
    }
    Err(AgentBackendError::ProtocolRejected {
        detail: format!(
            "interactive Codex lacks required {capability} support ({})",
            needles.join(", ")
        ),
    })
}

async fn probe(
    executable: &Path,
    args: &[&str],
    operation: &str,
) -> Result<String, AgentBackendError> {
    let output = run_captured(executable, None, args.iter().copied(), PROBE_DEADLINE).await?;
    if !output.status.success() {
        return Err(AgentBackendError::RunFailed {
            detail: format!(
                "interactive Codex {operation} probe failed ({}): {}",
                output.status,
                output.stderr.trim()
            ),
        });
    }
    Ok(format!("{}\n{}", output.stdout, output.stderr))
}

fn first_nonempty_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// Adapter for the verified interactive TUI and its public lifecycle commands.
#[derive(Debug, Clone)]
pub struct CodexInteractiveBackend {
    installation: InteractiveAgentInstallation,
}

impl CodexInteractiveBackend {
    #[must_use]
    pub fn new(installation: InteractiveAgentInstallation) -> Self {
        Self { installation }
    }
}

impl InteractiveAgentBackend for CodexInteractiveBackend {
    fn prepare_native_tool_policy(
        &self,
        policy: InteractiveNativeToolPolicy,
        staging_root: &Path,
    ) -> Result<Vec<InteractivePolicyMount>, AgentBackendError> {
        prepare_native_tool_policy(policy, staging_root)
    }

    fn render(
        &self,
        spec: &InteractiveAgentSpec,
    ) -> Result<InteractiveAgentCommand, AgentBackendError> {
        command_for(&self.installation, spec)
    }

    fn push<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a QueueReadyThread,
        message: &'a str,
    ) -> InteractiveFuture<'a, ()> {
        Box::pin(queue_message(
            &self.installation,
            Path::new(cwd),
            thread,
            message,
        ))
    }

    fn usage<'a>(
        &'a self,
        thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, Option<TokenUsage>> {
        let sessions = super::isolation::codex_home().join("sessions");
        let thread = thread.id().0.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || read_rollout_usage(&sessions, &thread))
                .await
                .map_err(|error| AgentBackendError::BackendUnavailable {
                    detail: format!("Codex usage reader task failed: {error}"),
                })?
        })
    }

    fn archive<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, ()> {
        Box::pin(archive_thread(&self.installation, Path::new(cwd), thread))
    }
}

fn read_rollout_usage(
    sessions: &Path,
    thread: &str,
) -> Result<Option<TokenUsage>, AgentBackendError> {
    let Some(path) = find_rollout(sessions, thread, 4)? else {
        return Ok(None);
    };
    let file = std::fs::File::open(&path)
        .map_err(|error| unavailable("open Codex rollout for usage", error))?;
    let mut latest = None;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|error| unavailable("read Codex rollout usage", error))?;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(usage) = value
            .pointer("/payload/info/last_token_usage")
            .and_then(parse_rollout_usage)
        else {
            continue;
        };
        latest = Some(usage);
    }
    Ok(latest)
}

fn find_rollout(
    directory: &Path,
    thread: &str,
    depth: usize,
) -> Result<Option<PathBuf>, AgentBackendError> {
    if depth == 0 || !directory.exists() {
        return Ok(None);
    }
    let entries = std::fs::read_dir(directory)
        .map_err(|error| unavailable("enumerate Codex rollouts", error))?;
    for entry in entries {
        let entry = entry.map_err(|error| unavailable("enumerate Codex rollout entry", error))?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_rollout(&path, thread, depth - 1)? {
                return Ok(Some(found));
            }
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("rollout-") && name.contains(thread))
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn parse_rollout_usage(value: &serde_json::Value) -> Option<TokenUsage> {
    let usage = TokenUsage {
        input_tokens: value.get("input_tokens")?.as_i64()?,
        cached_input_tokens: value.get("cached_input_tokens")?.as_i64()?,
        output_tokens: value.get("output_tokens")?.as_i64()?,
        reasoning_output_tokens: value.get("reasoning_output_tokens")?.as_i64()?,
        total_tokens: value.get("total_tokens")?.as_i64()?,
    };
    (usage.input_tokens >= 0
        && usage.cached_input_tokens >= 0
        && usage.cached_input_tokens <= usage.input_tokens
        && usage.output_tokens >= 0
        && usage.reasoning_output_tokens >= 0
        && usage.total_tokens >= 0)
        .then_some(usage)
}

fn prepare_native_tool_policy(
    policy: InteractiveNativeToolPolicy,
    staging_root: &Path,
) -> Result<Vec<InteractivePolicyMount>, AgentBackendError> {
    prepare_native_tool_policy_in_home(policy, staging_root, &super::isolation::codex_home())
}

fn prepare_native_tool_policy_in_home(
    policy: InteractiveNativeToolPolicy,
    staging_root: &Path,
    codex_home: &Path,
) -> Result<Vec<InteractivePolicyMount>, AgentBackendError> {
    if policy == InteractiveNativeToolPolicy::Standard {
        return Ok(Vec::new());
    }

    let codex_rules = codex_home.join("rules");
    let staged_rules = staging_root.join("codex-rules");
    std::fs::create_dir_all(&staged_rules)
        .map_err(|error| unavailable("create inspection policy staging directory", error))?;
    let entries = std::fs::read_dir(&codex_rules).map_err(|error| {
        unavailable(
            &format!("read Codex policy directory {}", codex_rules.display()),
            error,
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| unavailable("read Codex policy entry", error))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("rules") {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| unavailable("inspect Codex policy entry", error))?;
        if !file_type.is_file() {
            continue;
        }
        std::fs::copy(&path, staged_rules.join(entry.file_name())).map_err(|error| {
            unavailable(&format!("stage Codex policy {}", path.display()), error)
        })?;
    }
    std::fs::write(staged_rules.join(INSPECTION_POLICY_FILE), INSPECTION_POLICY)
        .map_err(|error| unavailable("write inspection-only command policy", error))?;

    Ok(vec![InteractivePolicyMount {
        source: staged_rules,
        target: codex_rules,
    }])
}

fn command_for(
    installation: &InteractiveAgentInstallation,
    spec: &InteractiveAgentSpec,
) -> Result<InteractiveAgentCommand, AgentBackendError> {
    if !spec.host_tools_socket.is_absolute() {
        return Err(AgentBackendError::ProtocolRejected {
            detail: format!(
                "host dynamic-tools socket must be absolute: {}",
                spec.host_tools_socket.display()
            ),
        });
    }
    let program = installation
        .executable()
        .to_str()
        .ok_or_else(|| AgentBackendError::ProtocolRejected {
            detail: "interactive Codex executable path is not UTF-8".into(),
        })?
        .to_owned();
    let mut command = Command::new(installation.executable());
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
        .arg("--sandbox");
    command.arg(match spec.native_sandbox {
        InteractiveNativeSandbox::BackendWorkspaceWrite => "workspace-write",
        InteractiveNativeSandbox::HostMountBoundary => "danger-full-access",
    });
    command
        .arg("--host-dynamic-tools-socket")
        .arg(&spec.host_tools_socket);
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

    if let Some(prompt) = &spec.initial_prompt {
        command.arg(prompt);
    }

    Ok(InteractiveAgentCommand {
        program,
        args: command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
    })
}

fn push_config_string(
    command: &mut Command,
    key: &str,
    value: &str,
) -> Result<(), AgentBackendError> {
    command.arg("-c").arg(format!(
        "{key}={}",
        serde_json::to_string(value).map_err(config_encode_error)?
    ));
    Ok(())
}

fn config_encode_error(error: serde_json::Error) -> AgentBackendError {
    AgentBackendError::ProtocolRejected {
        detail: format!("cannot encode interactive launch configuration: {error}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RolloutBinding {
    version: u32,
    thread: BackendThreadId,
}

impl RolloutBinding {
    /// V4 certifies that the host session callback ran only after Codex made
    /// the rollout durably discoverable to separate lifecycle processes.
    const VERSION: u32 = 4;

    fn new(thread: BackendThreadId) -> Result<Self, AgentBackendError> {
        validate_thread(&thread)?;
        Ok(Self {
            version: Self::VERSION,
            thread,
        })
    }
}

pub async fn read_binding(path: &Path) -> Result<QueueReadyThread, AgentBackendError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| unavailable("read rollout binding", error))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| AgentBackendError::ProtocolRejected {
            detail: format!("invalid rollout binding {}: {error}", path.display()),
        })?;
    let found = version_ladder::found_version(&value);
    let value = version_ladder::migrate_to_current(
        value,
        found,
        RolloutBinding::VERSION,
        RolloutBinding::VERSION,
        &[],
    )
    .map_err(|error| binding_version_error(path, error))?;
    let binding: RolloutBinding =
        serde_json::from_value(value).map_err(|error| AgentBackendError::ProtocolRejected {
            detail: format!("invalid rollout binding {}: {error}", path.display()),
        })?;
    validate_thread(&binding.thread)?;
    Ok(QueueReadyThread::new(binding.thread))
}

fn binding_version_error(path: &Path, error: LadderError) -> AgentBackendError {
    let detail = match error {
        LadderError::BelowFloor { found, floor } => format!(
            "rollout binding {} uses version {found}, below the queue-readiness floor {floor}, and cannot prove durable queue readiness; start a fresh Shoal root instead of resuming this conversation",
            path.display()
        ),
        LadderError::UnsupportedVersion { found, current } => format!(
            "rollout binding {} uses future version {found}, but this Tidepool build supports through version {current}; resume with a newer Tidepool build",
            path.display()
        ),
        LadderError::Migration { from, source } => format!(
            "rollout binding {} migration from version {from} failed: {source}",
            path.display()
        ),
    };
    AgentBackendError::ProtocolRejected { detail }
}

pub async fn accept_session_binding(
    path: &Path,
    protocol_version: u32,
    thread: BackendThreadId,
) -> Result<(), AgentBackendError> {
    if protocol_version != HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION {
        return Err(AgentBackendError::ProtocolRejected {
            detail: format!(
                "unsupported host dynamic-tools protocol version {protocol_version}; expected {HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION}"
            ),
        });
    }
    let binding = RolloutBinding::new(thread)?;
    persist_binding(path, &binding).await?;
    Ok(())
}

pub async fn copy_binding(path: &Path, thread: &QueueReadyThread) -> Result<(), AgentBackendError> {
    persist_binding(path, &RolloutBinding::new(thread.id().clone())?).await
}

async fn persist_binding(path: &Path, binding: &RolloutBinding) -> Result<(), AgentBackendError> {
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

async fn queue_message(
    installation: &InteractiveAgentInstallation,
    cwd: &Path,
    thread: &QueueReadyThread,
    message: &str,
) -> Result<(), AgentBackendError> {
    let thread = thread.id();
    run_cli(
        installation,
        cwd,
        "queue",
        ["queue", "--thread", thread.0.as_str(), "--message", message],
    )
    .await
}

async fn archive_thread(
    installation: &InteractiveAgentInstallation,
    cwd: &Path,
    thread: &QueueReadyThread,
) -> Result<(), AgentBackendError> {
    let thread = thread.id();
    run_cli(installation, cwd, "archive", ["archive", thread.0.as_str()]).await
}

fn validate_thread(thread: &BackendThreadId) -> Result<(), AgentBackendError> {
    uuid::Uuid::parse_str(&thread.0)
        .map(|_| ())
        .map_err(|_| AgentBackendError::ProtocolRejected {
            detail: format!("invalid interactive thread id {:?}", thread.0),
        })
}

async fn run_cli<'a>(
    installation: &InteractiveAgentInstallation,
    cwd: &Path,
    operation: &'static str,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<(), AgentBackendError> {
    let output = run_captured(installation.executable(), Some(cwd), args, CLI_DEADLINE).await?;
    if output.status.success() {
        return Ok(());
    }
    Err(AgentBackendError::RunFailed {
        detail: format!(
            "interactive {operation} failed ({}): {}",
            output.status,
            output.stderr.trim()
        ),
    })
}

struct CapturedCommand {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

async fn run_captured<'a>(
    executable: &Path,
    cwd: Option<&Path>,
    args: impl IntoIterator<Item = &'a str>,
    deadline: Duration,
) -> Result<CapturedCommand, AgentBackendError> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .spawn()
        .map_err(|error| unavailable("spawn interactive Codex", error))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentBackendError::BackendUnavailable {
            detail: "interactive Codex stdout was not captured".into(),
        })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AgentBackendError::BackendUnavailable {
            detail: "interactive Codex stderr was not captured".into(),
        })?;
    let captured = tokio::time::timeout(deadline, async {
        tokio::try_join!(
            read_bounded(stdout, CAPTURE_LIMIT),
            read_bounded(stderr, CAPTURE_LIMIT),
            child.wait(),
        )
    })
    .await
    .map_err(|_| AgentBackendError::BackendUnavailable {
        detail: format!("interactive Codex command exceeded {deadline:?}"),
    })?
    .map_err(|error| unavailable("run interactive Codex", error))?;
    Ok(CapturedCommand {
        stdout: String::from_utf8_lossy(&captured.0).into_owned(),
        stderr: String::from_utf8_lossy(&captured.1).into_owned(),
        status: captured.2,
    })
}

async fn read_bounded(
    reader: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("captured output exceeded {limit} bytes"),
        ));
    }
    Ok(bytes)
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

    fn installation() -> InteractiveAgentInstallation {
        InteractiveAgentInstallation::new(
            PathBuf::from("/nix/store/custom-codex/bin/codex"),
            "codex 1".into(),
        )
    }

    fn spec(mode: InteractiveLaunchMode) -> InteractiveAgentSpec {
        InteractiveAgentSpec {
            mode,
            model: Some("gpt-test".to_string()),
            effort: Some(ReasoningEffort::Medium),
            developer_instructions: "actor charter".to_string(),
            initial_prompt: Some("initialize through typed tools".to_string()),
            native_sandbox: InteractiveNativeSandbox::HostMountBoundary,
            host_tools_socket: "/tmp/tidepool/host-tools.sock".into(),
        }
    }

    #[tokio::test]
    async fn binding_round_trip_is_current_and_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binding.json");
        let thread = BackendThreadId(THREAD.to_string());
        accept_session_binding(&path, HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION, thread.clone())
            .await
            .unwrap();
        assert_eq!(read_binding(&path).await.unwrap().id(), &thread);
        let encoded: RolloutBinding =
            serde_json::from_slice(&tokio::fs::read(path).await.unwrap()).unwrap();
        assert_eq!(encoded.version, 4);
        assert_eq!(encoded.thread, thread);
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

        tokio::fs::write(&path, format!(r#"{{"version": 3, "thread": "{THREAD}"}}"#))
            .await
            .unwrap();
        let error = read_binding(&path).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot prove durable queue readiness"));
        assert!(error.to_string().contains("start a fresh Shoal root"));

        tokio::fs::write(&path, format!(r#"{{"version": 5, "thread": "{THREAD}"}}"#))
            .await
            .unwrap();
        let error = read_binding(&path).await.unwrap_err();
        assert!(error.to_string().contains("uses future version 5"));
        assert!(error.to_string().contains("newer Tidepool build"));

        assert!(matches!(
            RolloutBinding::new(BackendThreadId("not-a-thread".to_string())),
            Err(AgentBackendError::ProtocolRejected { .. })
        ));
    }

    #[tokio::test]
    async fn unsupported_session_protocol_cannot_mint_or_persist_queue_readiness() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binding.json");
        let error = accept_session_binding(&path, 1, BackendThreadId(THREAD.to_string()))
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported host dynamic-tools protocol version 1; expected 2"));
        assert!(!path.exists());
    }

    #[test]
    fn fresh_launch_uses_exact_binary_and_host_tools_socket() {
        let command = command_for(&installation(), &spec(InteractiveLaunchMode::Fresh)).unwrap();
        assert_eq!(command.program, "/nix/store/custom-codex/bin/codex");
        assert!(!command
            .args
            .iter()
            .any(|arg| arg == "resume" || arg == "fork"));
        assert!(command.args.windows(2).any(|args| {
            args == [
                "--host-dynamic-tools-socket",
                "/tmp/tidepool/host-tools.sock",
            ]
        }));
        assert!(command
            .args
            .iter()
            .any(|arg| arg == "initialize through typed tools"));
        assert!(!command.args.iter().any(|arg| arg.contains("mcp_servers")));
    }

    #[test]
    fn resume_keeps_global_options_after_the_subcommand() {
        let command = command_for(
            &installation(),
            &spec(InteractiveLaunchMode::Resume(BackendThreadId(
                THREAD.into(),
            ))),
        )
        .unwrap();
        assert_eq!(command.args[0], "resume");
        assert_eq!(command.args[1], THREAD);
        assert!(command.args.windows(2).any(|args| {
            args == [
                "--host-dynamic-tools-socket",
                "/tmp/tidepool/host-tools.sock",
            ]
        }));
    }

    #[test]
    fn host_socket_must_be_absolute() {
        let mut spec = spec(InteractiveLaunchMode::Fresh);
        spec.host_tools_socket = "relative.sock".into();
        assert!(matches!(
            command_for(&installation(), &spec),
            Err(AgentBackendError::ProtocolRejected { .. })
        ));
    }

    #[test]
    fn inspection_policy_preserves_user_rules_and_adds_build_denials() {
        let root = tempfile::tempdir().unwrap();
        let codex_home = root.path().join("codex-home");
        let rules = codex_home.join("rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(
            rules.join("operator.rules"),
            "prefix_rule(pattern=[\"rg\"], decision=\"allow\")\n",
        )
        .unwrap();

        let mounts = prepare_native_tool_policy_in_home(
            InteractiveNativeToolPolicy::InspectionOnly,
            &root.path().join("actor-policy"),
            &codex_home,
        )
        .unwrap();

        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].target, rules);
        assert!(mounts[0].source.join("operator.rules").is_file());
        let policy =
            std::fs::read_to_string(mounts[0].source.join(INSPECTION_POLICY_FILE)).unwrap();
        for expected in ["cargo", "nix", "pytest", "git", "forbidden"] {
            assert!(policy.contains(expected));
        }

        let standard = prepare_native_tool_policy_in_home(
            InteractiveNativeToolPolicy::Standard,
            &root.path().join("unused"),
            &codex_home,
        )
        .unwrap();
        assert!(standard.is_empty());
        assert!(!root.path().join("unused").exists());
    }

    #[test]
    fn invalid_explicit_binary_never_falls_through_to_path() {
        let error = resolve_executable_from(
            Some(OsString::from("relative-codex")),
            Some(std::env::var_os("PATH").unwrap_or_default()),
        )
        .unwrap_err();
        assert!(error.to_string().contains(ENV_INTERACTIVE_CODEX_BIN));
    }

    #[test]
    fn durable_rollout_usage_preserves_reported_cache_measurement() {
        let root = tempfile::tempdir().unwrap();
        let day = root.path().join("2026/09/04");
        std::fs::create_dir_all(&day).unwrap();
        let thread = "019fe92a-1a66-7820-9481-c0a2d108aba1";
        let rollout = day.join(format!("rollout-now-{thread}.jsonl"));
        std::fs::write(
            rollout,
            concat!(
                "{\"payload\":{\"type\":\"other\"}}\n",
                "{\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{",
                "\"input_tokens\":100,\"cached_input_tokens\":80,\"output_tokens\":7,",
                "\"reasoning_output_tokens\":3,\"total_tokens\":107}}}}\n"
            ),
        )
        .unwrap();

        let usage = read_rollout_usage(root.path(), thread).unwrap().unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.cached_input_tokens, 80);
        assert_eq!(usage.total_tokens, 107);

        let invalid = serde_json::json!({
            "input_tokens": 10,
            "cached_input_tokens": 11,
            "output_tokens": 0,
            "reasoning_output_tokens": 0,
            "total_tokens": 10
        });
        assert_eq!(parse_rollout_usage(&invalid), None);
    }
}
