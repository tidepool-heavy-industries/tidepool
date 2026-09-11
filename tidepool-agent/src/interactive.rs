//! Backend-neutral ownership seam for long-lived interactive agent applications.
//!
//! This is deliberately separate from [`crate::backend::AgentBackend`]. A
//! headless worker exposes a stepwise turn protocol; an interactive agent owns
//! its native conversation and terminal UI. Tidepool launches and supervises
//! the latter, pushes messages through its supported channel, and services its
//! actor-scoped hosted tools. Pretending those are the same lifecycle would make
//! either side lie.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::{AgentBackendError, BackendThreadId, ReasoningEffort};

/// An interactive conversation whose durable rollout can be addressed by a
/// separate native queue or archive process.
///
/// Only the interactive binding owner can construct this proof after the
/// hosted-session readiness contract has been durably recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueReadyThread {
    thread: BackendThreadId,
    input_control_socket: Option<PathBuf>,
}

impl QueueReadyThread {
    pub(crate) fn new(thread: BackendThreadId) -> Self {
        Self {
            thread,
            input_control_socket: None,
        }
    }

    pub(crate) fn with_input_control(mut self, socket: Option<PathBuf>) -> Self {
        self.input_control_socket = socket;
        self
    }

    pub(crate) fn input_control_socket(&self) -> Option<&Path> {
        self.input_control_socket.as_deref()
    }

    /// Whether this exact TUI binding advertises normal active-input delivery.
    #[must_use]
    pub fn supports_active_input(&self) -> bool {
        self.input_control_socket.is_some()
    }

    #[must_use]
    pub fn id(&self) -> &BackendThreadId {
        &self.thread
    }
}

/// One exact, behaviorally verified interactive-agent installation.
///
/// Shoal resolves this once before it mutates tmux state, then passes the
/// value through its private host-process boundary. Every launch and lifecycle
/// command therefore addresses the same executable rather than consulting
/// `PATH` again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveAgentInstallation {
    executable: PathBuf,
    version: String,
}

impl InteractiveAgentInstallation {
    pub(crate) fn new(executable: PathBuf, version: String) -> Self {
        Self {
            executable,
            version,
        }
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
}

/// Delivery certainty is control flow: a failure before submission allows the
/// assignment to continue; uncertainty after submission must keep its fence.
/// Details describe this delivery operation, not the original agent run.
#[derive(Debug, thiserror::Error)]
pub enum UpdatePresentationError {
    #[error("update was not submitted: {0}")]
    NotSubmitted(String),
    #[error("update presentation is unconfirmed: {0}")]
    Unconfirmed(String),
}

pub type UpdatePresentationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), UpdatePresentationError>> + Send + 'a>>;

/// A boxed asynchronous operation at the backend-neutral boundary.
pub type InteractiveFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, AgentBackendError>> + Send + 'a>>;

/// How a long-lived agent conversation begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractiveLaunchMode {
    Fresh,
    Resume(BackendThreadId),
    Fork {
        parent: BackendThreadId,
        after_call: String,
    },
}

/// Which layer owns native filesystem containment for an interactive agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveNativeSandbox {
    /// Let the backend confine writes to its workspace.
    BackendWorkspaceWrite,
    /// A validated outer process mount boundary owns containment, so the
    /// backend must not install its conflicting `.git`-protecting sandbox.
    HostMountBoundary,
}

/// Native command policy selected by the actor's effective role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveNativeToolPolicy {
    /// Ordinary coding and orchestration tools are available.
    Standard,
    /// Source inspection remains available, while common build and artifact
    /// producers are rejected by the backend before process execution.
    InspectionOnly,
}

/// One backend-owned policy directory to mount over its ordinary config
/// directory for a single interactive process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractivePolicyMount {
    pub source: PathBuf,
    pub target: PathBuf,
}

/// A backend-rendered interactive process invocation.
///
/// Process ownership stays with the deployment adapter (tmux for Shoal). The
/// backend owns only the exact executable and arguments required by its native
/// client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveAgentCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// Whether autonomous goals belong to this interactive process or its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveGoalPolicy {
    /// Preserve the operator's configured autonomous goal behavior.
    Configured,
    /// The host owns assignments and continuation; do not inherit or run goals.
    Disabled,
}

/// Select the process-tool owner without changing filesystem authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveShellTools {
    Native,
    Hosted,
}

/// Backend-neutral configuration frozen when an interactive process starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveAgentSpec {
    pub mode: InteractiveLaunchMode,
    pub goal_policy: InteractiveGoalPolicy,
    pub shell_tools: InteractiveShellTools,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    /// Host-owned, immutable base instructions shared across this run.
    /// Must be an absolute path readable in the launched process.
    pub base_instructions_file: PathBuf,
    pub developer_instructions: String,
    /// Optional first user message. Hosted agents do not need a synthetic
    /// message because their session handshake publishes only queue-ready
    /// conversations.
    pub initial_prompt: Option<String>,
    pub native_sandbox: InteractiveNativeSandbox,
    /// Actor-scoped host dynamic tools served over HTTP/1.1 on this Unix
    /// socket. The deployment adapter binds it before calling `render`.
    pub host_tools_socket: PathBuf,
}

/// Operations a concrete interactive-agent adapter must provide.
///
/// The composition root owns processes and durable delivery. `render` is pure
/// command construction; `push` is only the final backend hop to an
/// already-bound exact conversation.
pub trait InteractiveAgentBackend: Send + Sync {
    /// Execute and control a command through the already-bound native process owner.
    fn command<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        _id: &'a str,
        _operation: NativeCommandOperation,
    ) -> InteractiveFuture<'a, NativeCommandReply> {
        Box::pin(async {
            Err(AgentBackendError::ProtocolRejected {
                detail: "native commands are unsupported".into(),
            })
        })
    }
    /// Control a process-owned workspace publication lease. Transport errors are
    /// unconfirmed: retain the same durable sequence until it can be reconciled.
    fn workspace_publication<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        _sequence: std::num::NonZeroU64,
        _operation: PublicationOperation,
    ) -> InteractiveFuture<'a, PublicationReply> {
        Box::pin(async {
            Ok(PublicationReply::Unavailable {
                detail: "workspace publication is unsupported".into(),
            })
        })
    }
    /// Materialize backend-native command policy under `staging_root`.
    ///
    /// The deployment owner installs returned directories as read-only mount
    /// overlays for this process only. The standard policy needs no overlay.
    fn prepare_native_tool_policy(
        &self,
        policy: InteractiveNativeToolPolicy,
        staging_root: &Path,
    ) -> Result<Vec<InteractivePolicyMount>, AgentBackendError>;

    fn render(
        &self,
        spec: &InteractiveAgentSpec,
    ) -> Result<InteractiveAgentCommand, AgentBackendError>;

    fn push<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a QueueReadyThread,
        message: &'a str,
    ) -> InteractiveFuture<'a, ()>;

    /// Present an update in the existing conversation at a safe model boundary,
    /// waking it if idle. Success requires observed model-visible insertion with
    /// the supplied correlation key, not merely acceptance into a queue.
    fn present_update<'a>(
        &'a self,
        _cwd: &'a str,
        _thread: &'a QueueReadyThread,
        _key: &'a str,
        _message: &'a str,
    ) -> UpdatePresentationFuture<'a> {
        Box::pin(async {
            Err(UpdatePresentationError::NotSubmitted(
                "this backend does not support confirmed active updates".into(),
            ))
        })
    }

    /// Usage and provider turn health from one durable observation.
    /// `None` means unavailable, never measured zero or an idle process.
    fn observe<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, Option<tidepool_model::ProviderObservation>> {
        Box::pin(async { Ok(None) })
    }

    fn archive<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, ()>;
}

#[derive(Clone, Debug)]
pub enum NativeCommandOperation {
    Start(tidepool_bridge_effects::CommandSpec),
    Wait,
    Output(usize),
    Read {
        stream: tidepool_bridge_effects::CommandStream,
        position: tidepool_bridge_effects::CommandPosition,
    },
    Input(String),
    CloseInput,
    Resize {
        rows: u16,
        columns: u16,
    },
    Cancel,
}

#[derive(Clone, Debug)]
pub enum NativeCommandReply {
    Pending,
    Finished { exit_code: i32, cancelled: bool },
    Unconfirmed(String),
    Output(tidepool_bridge_effects::CommandOutput),
    Page(tidepool_bridge_effects::CommandPage),
    Acknowledged,
}

#[derive(Clone, Copy, Debug)]
pub enum PublicationOperation {
    Begin {
        expected: Option<PublicationIdentity>,
    },
    Finish {
        expected: PublicationIdentity,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicationIdentity {
    pub pid: u32,
    pub start_ticks: u64,
    pub mount_namespace_inode: u64,
}

#[derive(Debug)]
pub enum PublicationReply {
    Ready {
        /// Socket owner PID in the host's namespace; `pid` is native-local.
        peer_pid: u32,
        pid: u32,
        start_ticks: u64,
        mount_namespace_inode: u64,
        cgroup_path: PathBuf,
    },
    Settled,
    Busy,
    Conflict,
    Unavailable {
        detail: String,
    },
}
