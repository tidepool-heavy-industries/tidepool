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
    /// First user message. A fresh stock TUI needs this to create the rollout
    /// addressed by subsequent native push operations.
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
    fn render(
        &self,
        spec: &InteractiveAgentSpec,
    ) -> Result<InteractiveAgentCommand, AgentBackendError>;

    fn push<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a BackendThreadId,
        message: &'a str,
    ) -> InteractiveFuture<'a, ()>;

    fn archive<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a BackendThreadId,
    ) -> InteractiveFuture<'a, ()>;
}
