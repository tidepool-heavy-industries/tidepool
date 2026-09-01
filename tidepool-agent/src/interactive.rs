//! Backend-neutral ownership seam for long-lived interactive agent applications.
//!
//! This is deliberately separate from [`crate::backend::AgentBackend`]. A
//! headless worker exposes a stepwise turn protocol; an interactive agent owns
//! its native conversation and terminal UI. Tidepool launches and supervises
//! the latter, pushes messages through its supported channel, and services its
//! actor-scoped MCP child. Pretending those are the same lifecycle would make
//! either side lie.

use std::future::Future;
use std::pin::Pin;

use crate::{AgentBackendError, BackendThreadId, ReasoningEffort};

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

/// One stdio MCP server installed only for this interactive process.
///
/// `forward_env` contains names, not values. The process launcher chooses the
/// launch environment; this list only tells the agent which of those values
/// its MCP child is allowed to inherit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveMcpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub forward_env: Vec<String>,
    pub required: bool,
}

/// Backend-neutral configuration frozen when an interactive process starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveAgentSpec {
    pub mode: InteractiveLaunchMode,
    pub cwd: String,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub developer_instructions: String,
    pub mcp: InteractiveMcpServer,
}

/// Exact process-instance ownership returned by an interactive launch.
///
/// Conversation binding is established by the MCP child after launch, so a
/// process handle intentionally carries no guessed thread id.
pub trait InteractiveAgentProcess: Send {
    /// Wait for the native application to exit normally.
    fn wait(&mut self) -> InteractiveFuture<'_, ()>;

    /// Stop and confirm reaping of this exact process instance.
    fn shutdown(self: Box<Self>) -> InteractiveFuture<'static, ()>;
}

/// Operations a concrete interactive-agent adapter must provide.
///
/// The central node service owns durable delivery and retry. `push` is only
/// the final backend hop to an already-bound exact conversation.
pub trait InteractiveAgentBackend: Send + Sync {
    fn launch(
        &self,
        spec: InteractiveAgentSpec,
    ) -> InteractiveFuture<'_, Box<dyn InteractiveAgentProcess>>;

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
