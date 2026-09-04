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

use crate::{AgentBackendError, BackendThreadId, ReasoningEffort, TokenUsage};

/// An interactive conversation whose durable rollout can be addressed by a
/// separate native queue or archive process.
///
/// Only the interactive binding owner can construct this proof after the
/// hosted-session readiness contract has been durably recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueReadyThread(BackendThreadId);

impl QueueReadyThread {
    pub(crate) fn new(thread: BackendThreadId) -> Self {
        Self(thread)
    }

    #[must_use]
    pub fn id(&self) -> &BackendThreadId {
        &self.0
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

/// A boxed asynchronous operation at the backend-neutral boundary.
pub type InteractiveFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, AgentBackendError>> + Send + 'a>>;

/// How a long-lived agent conversation begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractiveLaunchMode {
    Fresh,
    Resume(BackendThreadId),
    Fork(BackendThreadId),
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

/// Backend-neutral configuration frozen when an interactive process starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveAgentSpec {
    pub mode: InteractiveLaunchMode,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
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

    /// Latest usage the backend durably reported for this conversation.
    /// `None` means the backend has not reported it, never a measured miss.
    fn usage<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, Option<TokenUsage>> {
        Box::pin(async { Ok(None) })
    }

    fn archive<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, ()>;
}
