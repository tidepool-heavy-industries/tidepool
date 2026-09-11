//! Interactive Codex installation, launch, and lifecycle operations.
//!
//! Shoal resolves and behaviorally probes one absolute executable before it
//! mutates tmux state. The resulting value is the sole program used for TUI
//! launch, queue delivery, and archival. Actor tools are supplied through the
//! fork's HTTP/1.1-over-UDS host dynamic-tool boundary.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::{BufReader, Read};
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
    QueueReadyThread, ReasoningEffort,
};
use tidepool_model::ProviderObservation;

#[path = "active_update.rs"]
mod active_update;
#[allow(dead_code)]
#[path = "input_control.rs"]
mod input_control;
#[path = "rollout_usage.rs"]
mod rollout_usage;

pub use rollout_usage::{
    read_bounded_usage, BoundedUsageRecord, BoundedUsageReport, UsageDiagnostic, UsageIssue,
    UsageLimit, UsageProvenance, UsageReadLimits, UsageSelection, UsageSourceCoverage,
    UsageSourceState,
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
pub const HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION: u32 = 3;

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
        &["fork", "--help"],
        "destination-owned invocation forks",
        &["--destination-local", "--after-call"],
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
    installation(executable, version)
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
    installation(executable, version)
}

fn installation(
    executable: PathBuf,
    version: String,
) -> Result<InteractiveAgentInstallation, AgentBackendError> {
    let executable_sha256 = hash_executable(&executable)?;
    let package_root = executable
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "bin"))
        .and_then(Path::parent)
        .map(Path::to_owned);
    Ok(InteractiveAgentInstallation::new(
        executable,
        version,
        executable_sha256,
        package_root,
    ))
}

fn hash_executable(executable: &Path) -> Result<String, AgentBackendError> {
    let file = std::fs::File::open(executable)
        .map_err(|error| unavailable("hash interactive Codex executable", error))?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| unavailable("hash interactive Codex executable", error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let bytes = digest.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[(byte >> 4) as usize]));
        encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    Ok(encoded)
}

fn resolve_executable() -> Result<PathBuf, AgentBackendError> {
    resolve_executable_from(std::env::var_os(ENV_INTERACTIVE_CODEX_BIN))
}

fn resolve_executable_from(configured: Option<OsString>) -> Result<PathBuf, AgentBackendError> {
    let configured = configured.ok_or_else(|| AgentBackendError::BackendUnavailable {
        detail: format!(
            "{ENV_INTERACTIVE_CODEX_BIN} must explicitly name the pinned interactive Codex executable"
        ),
    })?;
    let path = PathBuf::from(configured);
    if !path.is_absolute() || !is_readable_executable_file(&path) {
        return Err(AgentBackendError::BackendUnavailable {
            detail: format!(
                "{ENV_INTERACTIVE_CODEX_BIN} must name an absolute readable executable file: {}",
                path.display()
            ),
        });
    }
    std::fs::canonicalize(&path)
        .map_err(|error| unavailable("canonicalize interactive Codex", error))
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
    fn command<'a>(
        &'a self,
        thread: &'a QueueReadyThread,
        id: &'a str,
        operation: crate::NativeCommandOperation,
    ) -> InteractiveFuture<'a, crate::NativeCommandReply> {
        Box::pin(super::commands::request(thread, id, operation))
    }

    fn bind_input<'a>(&'a self, thread: &'a QueueReadyThread) -> crate::InteractiveInputFuture<'a> {
        Box::pin(input_control::bind(thread))
    }

    fn submit_input<'a>(
        &'a self,
        thread: &'a QueueReadyThread,
        envelope: &'a crate::InteractiveInputEnvelope,
    ) -> crate::InteractiveInputFuture<'a> {
        Box::pin(input_control::submit(thread, envelope))
    }

    fn query_input<'a>(
        &'a self,
        thread: &'a QueueReadyThread,
        id: &'a crate::InputOperationId,
    ) -> crate::InteractiveInputFuture<'a> {
        Box::pin(input_control::query(thread, id))
    }

    fn withdraw_input<'a>(
        &'a self,
        thread: &'a QueueReadyThread,
        id: &'a crate::InputOperationId,
    ) -> crate::InteractiveInputFuture<'a> {
        Box::pin(input_control::withdraw(thread, id))
    }

    fn seal_input_producer<'a>(
        &'a self,
        thread: &'a QueueReadyThread,
        producer: &'a crate::InputProducerId,
    ) -> crate::InputProducerControlFuture<'a> {
        Box::pin(input_control::seal(thread, producer))
    }

    fn acknowledge_input_prefix<'a>(
        &'a self,
        thread: &'a QueueReadyThread,
        producer: &'a crate::InputProducerId,
        through_sequence: std::num::NonZeroU64,
    ) -> crate::InputProducerControlFuture<'a> {
        Box::pin(input_control::acknowledge(
            thread,
            producer,
            through_sequence,
        ))
    }

    fn workspace_publication<'a>(
        &'a self,
        thread: &'a QueueReadyThread,
        sequence: std::num::NonZeroU64,
        operation: crate::interactive::PublicationOperation,
    ) -> InteractiveFuture<'a, crate::interactive::PublicationReply> {
        Box::pin(super::workspace_publication::request(
            thread, sequence, operation,
        ))
    }
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

    fn present_update<'a>(
        &'a self,
        _cwd: &'a str,
        thread: &'a QueueReadyThread,
        key: &'a str,
        message: &'a str,
    ) -> crate::UpdatePresentationFuture<'a> {
        Box::pin(active_update::present(thread, key, message))
    }

    fn observe<'a>(
        &'a self,
        thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, Option<ProviderObservation>> {
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
) -> Result<Option<ProviderObservation>, AgentBackendError> {
    let Some(path) = find_rollout(sessions, thread, 4)? else {
        return Ok(None);
    };
    let file = std::fs::File::open(&path)
        .map_err(|error| unavailable("open Codex rollout for usage", error))?;
    rollout_usage::observe(BufReader::new(file), thread)
        .map(Some)
        .map_err(|error| unavailable("read Codex rollout usage", error))
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
    if !spec.base_instructions_file.is_absolute() {
        return Err(AgentBackendError::ProtocolRejected {
            detail: "base instructions file must be absolute".into(),
        });
    }
    let base_instructions_file = spec.base_instructions_file.to_str().ok_or_else(|| {
        AgentBackendError::ProtocolRejected {
            detail: "base instructions path is not UTF-8".into(),
        }
    })?;
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
        InteractiveLaunchMode::Fork { parent, after_call } => {
            validate_thread(parent)?;
            command
                .arg("fork")
                .arg(&parent.0)
                .arg("--destination-local")
                .arg("--after-call")
                .arg(after_call);
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
    // Hosted responses are bounded to 64 KiB, including their recovery receipt.
    // Codex estimates four bytes per token; retain that response in model history
    // instead of silently clipping it again after displaying the original in TUI.
    command.args(["-c", "tool_output_token_limit=16384"]);
    // The host owns the actor tree and reply routing. Native collaboration
    // would create a second, unrelated tree inside this actor's provider thread.
    for feature in [
        "multi_agent",
        "multi_agent_v2",
        "code_mode",
        "code_mode_only",
    ] {
        command.arg("--disable").arg(feature);
    }
    if spec.goal_policy == crate::InteractiveGoalPolicy::Disabled {
        command.arg("--disable").arg("goals");
    }
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
    // The file override takes precedence over operator/project model prompts
    // and inherited base instructions. Every mode gets the same frozen bytes.
    push_config_string(
        &mut command,
        "model_instructions_file",
        base_instructions_file,
    )?;
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
    input_control_socket: Option<PathBuf>,
}

impl RolloutBinding {
    /// V4 certifies that the host session callback ran only after Codex made
    /// the rollout durably discoverable to separate lifecycle processes.
    // V5 also retains the exact TUI-owned input endpoint, when negotiated.
    const VERSION: u32 = 5;

    fn new(thread: BackendThreadId) -> Result<Self, AgentBackendError> {
        validate_thread(&thread)?;
        Ok(Self {
            version: Self::VERSION,
            thread,
            input_control_socket: None,
        })
    }
}

fn binding_v4_to_v5(
    mut value: serde_json::Value,
) -> Result<serde_json::Value, version_ladder::MigrationError> {
    value = version_ladder::set_version(value, 5);
    value["input_control_socket"] = serde_json::Value::Null;
    Ok(value)
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
        4,
        RolloutBinding::VERSION,
        &[binding_v4_to_v5],
    )
    .map_err(|error| binding_version_error(path, error))?;
    let binding: RolloutBinding =
        serde_json::from_value(value).map_err(|error| AgentBackendError::ProtocolRejected {
            detail: format!("invalid rollout binding {}: {error}", path.display()),
        })?;
    validate_thread(&binding.thread)?;
    if binding
        .input_control_socket
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(AgentBackendError::ProtocolRejected {
            detail: "input control socket must be absolute".into(),
        });
    }
    Ok(QueueReadyThread::new(binding.thread).with_input_control(binding.input_control_socket))
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
    input_control_socket: Option<PathBuf>,
) -> Result<(), AgentBackendError> {
    if protocol_version != HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION {
        return Err(AgentBackendError::ProtocolRejected {
            detail: format!(
                "unsupported host dynamic-tools protocol version {protocol_version}; expected {HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION}"
            ),
        });
    }
    let mut binding = RolloutBinding::new(thread)?;
    if input_control_socket
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(AgentBackendError::ProtocolRejected {
            detail: "input control socket must be absolute".into(),
        });
    }
    binding.input_control_socket = input_control_socket;
    persist_binding(path, &binding).await?;
    Ok(())
}

pub async fn copy_binding(path: &Path, thread: &QueueReadyThread) -> Result<(), AgentBackendError> {
    let mut binding = RolloutBinding::new(thread.id().clone())?;
    binding.input_control_socket = thread.input_control_socket().map(Path::to_owned);
    persist_binding(path, &binding).await
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    const THREAD: &str = "01a05a16-97f5-7722-aa8d-467e01e2e5b4";

    fn installation() -> InteractiveAgentInstallation {
        InteractiveAgentInstallation::new(
            PathBuf::from("/nix/store/custom-codex/bin/codex"),
            "codex 1".into(),
            "fixture-sha256".into(),
            Some(PathBuf::from("/nix/store/custom-codex")),
        )
    }

    #[cfg(unix)]
    #[test]
    fn installation_records_exact_executable_and_package_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let package = directory.path().join("codex-package");
        let bin = package.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let executable = bin.join("codex");
        std::fs::write(&executable, b"fixture executable bytes").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();

        let installation = installation_from_parts(executable.clone(), "codex fixture".into())
            .expect("readable fixture installation");
        assert_eq!(installation.executable(), executable);
        assert_eq!(installation.package_root(), Some(package.as_path()));
        assert_eq!(
            installation.executable_sha256(),
            "f67bea1e29bf7fa00d04549495d9d5d3bf5fc92aa36a59fc471f792d6c8b153c"
        );
    }

    fn spec(mode: InteractiveLaunchMode) -> InteractiveAgentSpec {
        InteractiveAgentSpec {
            mode,
            goal_policy: crate::InteractiveGoalPolicy::Configured,
            model: Some("gpt-test".to_string()),
            effort: Some(ReasoningEffort::Medium),
            base_instructions_file: "/tmp/tidepool/prompts/shared base.md".into(),
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
        let socket = dir.path().join("input.sock");
        accept_session_binding(
            &path,
            HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
            thread.clone(),
            Some(socket.clone()),
        )
        .await
        .unwrap();
        let ready = read_binding(&path).await.unwrap();
        assert_eq!(ready.id(), &thread);
        assert_eq!(ready.input_control_socket(), Some(socket.as_path()));
        let copied = dir.path().join("copied.json");
        copy_binding(&copied, &ready).await.unwrap();
        assert_eq!(read_binding(&copied).await.unwrap(), ready);
        let encoded: RolloutBinding =
            serde_json::from_slice(&tokio::fs::read(path).await.unwrap()).unwrap();
        assert_eq!(encoded.version, 5);
        assert_eq!(encoded.thread, thread);
    }

    #[tokio::test]
    async fn v4_binding_retains_queue_readiness_without_inventing_input_support() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binding.json");
        tokio::fs::write(&path, format!(r#"{{"version":4,"thread":"{THREAD}"}}"#))
            .await
            .unwrap();
        let ready = read_binding(&path).await.unwrap();
        assert_eq!(ready.id(), &BackendThreadId(THREAD.into()));
        assert_eq!(ready.input_control_socket(), None);
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

        tokio::fs::write(&path, format!(r#"{{"version": 6, "thread": "{THREAD}"}}"#))
            .await
            .unwrap();
        let error = read_binding(&path).await.unwrap_err();
        assert!(error.to_string().contains("uses future version 6"));
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
        let error = accept_session_binding(&path, 1, BackendThreadId(THREAD.to_string()), None)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported host dynamic-tools protocol version 1; expected 3"));
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
    fn fork_selects_destination_runtime_and_exact_call_without_effort_dependency() {
        for effort in [
            None,
            Some(ReasoningEffort::Low),
            Some(ReasoningEffort::High),
        ] {
            let mut requested = spec(InteractiveLaunchMode::Fork {
                parent: BackendThreadId(THREAD.into()),
                after_call: "hosted-call-17".into(),
            });
            requested.effort = effort;
            let command = command_for(&installation(), &requested).unwrap();
            assert_eq!(
                &command.args[..5],
                &[
                    "fork",
                    THREAD,
                    "--destination-local",
                    "--after-call",
                    "hosted-call-17"
                ]
            );
            assert_eq!(
                command
                    .args
                    .iter()
                    .any(|arg| arg.contains("model_reasoning_effort")),
                effort.is_some()
            );
        }
    }

    #[test]
    fn all_launch_modes_select_the_frozen_base_and_disable_native_collaboration() {
        for mode in [
            InteractiveLaunchMode::Fresh,
            InteractiveLaunchMode::Resume(BackendThreadId(THREAD.into())),
            InteractiveLaunchMode::Fork {
                parent: BackendThreadId(THREAD.into()),
                after_call: "call".into(),
            },
        ] {
            let requested = spec(mode);
            let command = command_for(&installation(), &requested).unwrap();
            let overrides = command
                .args
                .windows(2)
                .filter(|args| args[0] == "-c")
                .map(|args| args[1].parse::<toml_edit::DocumentMut>().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                overrides
                    .iter()
                    .filter_map(|doc| doc.get("model_instructions_file"))
                    .map(|item| item.as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec![requested.base_instructions_file.to_str().unwrap()]
            );
            assert!(overrides.iter().any(|doc| doc
                .get("developer_instructions")
                .and_then(|value| value.as_str())
                == Some("actor charter")));
            assert!(overrides.iter().any(|doc| doc
                .get("tool_output_token_limit")
                .and_then(|value| value.as_integer())
                == Some(16384)));
            for feature in ["multi_agent", "multi_agent_v2"] {
                assert!(command
                    .args
                    .windows(2)
                    .any(|args| args == ["--disable", feature]));
            }
        }
        let mut requested = spec(InteractiveLaunchMode::Fresh);
        requested.base_instructions_file = "relative.md".into();
        assert!(matches!(
            command_for(&installation(), &requested),
            Err(AgentBackendError::ProtocolRejected { .. })
        ));
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
    fn hosted_launches_disable_native_collaboration_for_every_launch_mode() {
        for mode in [
            InteractiveLaunchMode::Fresh,
            InteractiveLaunchMode::Resume(BackendThreadId(THREAD.into())),
            InteractiveLaunchMode::Fork {
                parent: BackendThreadId(THREAD.into()),
                after_call: "hosted-call-17".into(),
            },
        ] {
            let mut requested = spec(mode);
            requested.model = Some("gpt-5.6-sol".into());
            requested.effort = Some(ReasoningEffort::Low);
            let command = command_for(&installation(), &requested).unwrap();
            for feature in ["multi_agent", "multi_agent_v2"] {
                assert!(command
                    .args
                    .windows(2)
                    .any(|args| args == ["--disable", feature]));
            }
            assert!(command
                .args
                .windows(2)
                .any(|args| args == ["--model", "gpt-5.6-sol"]));
            assert!(command
                .args
                .iter()
                .any(|arg| arg == "model_reasoning_effort=\"low\""));
        }
    }

    #[test]
    fn goal_policy_is_preserved_for_fresh_resumed_and_forked_launches() {
        for mode in [
            InteractiveLaunchMode::Fresh,
            InteractiveLaunchMode::Resume(BackendThreadId(THREAD.into())),
            InteractiveLaunchMode::Fork {
                parent: BackendThreadId(THREAD.into()),
                after_call: "hosted-call-17".into(),
            },
        ] {
            for policy in [
                crate::InteractiveGoalPolicy::Configured,
                crate::InteractiveGoalPolicy::Disabled,
            ] {
                let mut requested = spec(mode.clone());
                requested.goal_policy = policy;
                let command = command_for(&installation(), &requested).unwrap();
                assert_eq!(
                    command
                        .args
                        .windows(2)
                        .any(|args| args == ["--disable", "goals"]),
                    policy == crate::InteractiveGoalPolicy::Disabled,
                );
            }
        }
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
        let error = resolve_executable_from(Some(OsString::from("relative-codex"))).unwrap_err();
        assert!(error.to_string().contains(ENV_INTERACTIVE_CODEX_BIN));
    }

    #[cfg(unix)]
    #[test]
    fn unset_explicit_binary_rejects_a_readable_path_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let candidate = directory.path().join("codex");
        std::fs::write(&candidate, b"fixture executable").unwrap();
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_readable_executable_file(&candidate));

        let error = resolve_executable_from(None).unwrap_err();
        assert!(error.to_string().contains(ENV_INTERACTIVE_CODEX_BIN));
        assert!(error.to_string().contains("explicitly name the pinned"));
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
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"019fe92a-1a66-7820-9481-c0a2d108aba1\"}}\n",
                "{\"payload\":{\"type\":\"other\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{",
                "\"input_tokens\":100,\"cached_input_tokens\":80,\"output_tokens\":7,",
                "\"reasoning_output_tokens\":3,\"total_tokens\":107}}}}\n"
            ),
        )
        .unwrap();

        let usage = read_rollout_usage(root.path(), thread).unwrap().unwrap();
        let usage = usage.usage.unwrap();
        assert_eq!(usage.first.usage.input_tokens, 100);
        assert_eq!(usage.first.usage.cached_input_tokens, 80);
        assert_eq!(usage.latest.usage.total_tokens, 107);

        let invalid = serde_json::json!({
            "input_tokens": 10,
            "cached_input_tokens": 11,
            "output_tokens": 0,
            "reasoning_output_tokens": 0,
            "total_tokens": 10
        });
        assert_eq!(rollout_usage::parse_usage(&invalid), None);
    }

    #[test]
    fn rollout_usage_selects_own_first_and_latest_after_delayed_poll() {
        let root = tempfile::tempdir().unwrap();
        let thread = "child";
        let path = root.path().join("rollout-child.jsonl");
        let usage = serde_json::json!({"input_tokens": 100, "cached_input_tokens": 80,
            "output_tokens": 7, "reasoning_output_tokens": 3, "total_tokens": 107});
        let event = serde_json::json!({"type": "event_msg", "timestamp": "2026-09-05T00:00:00Z",
            "payload": {"type": "token_count", "info": {"last_token_usage": usage}}});
        let lines = [
            serde_json::json!({"type": "session_meta", "payload": {"id": "parent"}}),
            event.clone(),
            serde_json::json!({"type": "session_meta", "payload": {"id": thread}}),
            event.clone(),
            event,
        ];
        std::fs::write(
            &path,
            format!(
                "{}\n{{\"type\":",
                lines
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .unwrap();
        let snapshot = read_rollout_usage(root.path(), thread).unwrap().unwrap();
        let usage = snapshot.usage.as_ref().unwrap();
        assert_eq!(usage.first.id, "child:3");
        assert_eq!(usage.latest.id, "child:4");
        assert_eq!(usage.first.usage, usage.latest.usage);
        assert_eq!(
            read_rollout_usage(root.path(), thread).unwrap(),
            Some(snapshot)
        );
    }
}
