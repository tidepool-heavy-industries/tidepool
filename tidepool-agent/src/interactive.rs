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
use std::{collections::BTreeMap, path::PathBuf};

use crate::{AgentBackendError, BackendThreadId, ReasoningEffort};
use tidepool_actor::ActorRef;
use tidepool_node::NodeCredential;

pub(crate) const ENV_PROXY_ENDPOINT: &str = "TIDEPOOL_ACTOR_PROXY_ENDPOINT";
pub(crate) const ENV_ACTOR_ID: &str = "TIDEPOOL_ACTOR_ID";
pub(crate) const ENV_ACTOR_INCARNATION: &str = "TIDEPOOL_ACTOR_INCARNATION";
pub(crate) const ENV_PROXY_CREDENTIAL: &str = "TIDEPOOL_ACTOR_PROXY_CREDENTIAL";
pub(crate) const ENV_ACTOR_BINDING_PATH: &str = "TIDEPOOL_ACTOR_BINDING_PATH";
pub(crate) const ENV_ACTOR_WORKSPACE: &str = "TIDEPOOL_ACTOR_WORKSPACE";

/// Environment binding inherited by one interactive agent and its MCP proxy.
///
/// This is the one typed boundary that knows the private environment protocol
/// consumed by the proxy; composition roots never spell or parse those
/// variable names.
pub struct InteractiveProxyBinding {
    pub actor: ActorRef,
    pub endpoint: PathBuf,
    pub credential: NodeCredential,
    pub binding_path: PathBuf,
    pub workspace: PathBuf,
}

impl InteractiveProxyBinding {
    #[must_use]
    pub fn environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                ENV_PROXY_ENDPOINT.into(),
                self.endpoint.display().to_string(),
            ),
            (ENV_ACTOR_ID.into(), self.actor.id.0.to_string()),
            (
                ENV_ACTOR_INCARNATION.into(),
                self.actor.incarnation.0.to_string(),
            ),
            (ENV_PROXY_CREDENTIAL.into(), self.credential.0.clone()),
            (
                ENV_ACTOR_BINDING_PATH.into(),
                self.binding_path.display().to_string(),
            ),
            (
                ENV_ACTOR_WORKSPACE.into(),
                self.workspace.display().to_string(),
            ),
        ])
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
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub developer_instructions: String,
    /// First user message. A fresh stock TUI needs this to create the rollout
    /// addressed by subsequent native push operations.
    pub initial_prompt: Option<String>,
    pub native_sandbox: InteractiveNativeSandbox,
    pub mcp: InteractiveMcpServer,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_actor::{ActorId, ActorRef};

    #[test]
    fn proxy_binding_is_the_only_environment_protocol_encoder() {
        let launch = InteractiveProxyBinding {
            actor: ActorRef::first(ActorId(7)),
            endpoint: "/tmp/actor.sock".into(),
            credential: NodeCredential("secret".into()),
            binding_path: "/tmp/binding.json".into(),
            workspace: "/tmp/work".into(),
        };
        let environment = launch.environment();
        assert_eq!(environment[ENV_ACTOR_ID], "7");
        assert_eq!(environment[ENV_ACTOR_INCARNATION], "1");
        assert_eq!(environment[ENV_PROXY_CREDENTIAL], "secret");
        assert_eq!(environment[ENV_ACTOR_WORKSPACE], "/tmp/work");
    }
}
