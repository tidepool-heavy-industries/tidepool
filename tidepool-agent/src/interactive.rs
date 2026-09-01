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

pub(crate) const ENV_NODE_ENDPOINT: &str = "TIDEPOOL_NODE_ENDPOINT";
pub(crate) const ENV_NODE_ACTOR_ID: &str = "TIDEPOOL_NODE_ACTOR_ID";
pub(crate) const ENV_NODE_INCARNATION: &str = "TIDEPOOL_NODE_INCARNATION";
pub(crate) const ENV_NODE_CREDENTIAL: &str = "TIDEPOOL_NODE_CREDENTIAL";
pub(crate) const ENV_NODE_BINDING_PATH: &str = "TIDEPOOL_NODE_BINDING_PATH";
pub(crate) const ENV_NODE_WORKSPACE: &str = "TIDEPOOL_NODE_WORKSPACE";
pub(crate) const ENV_NODE_MODEL: &str = "TIDEPOOL_NODE_MODEL";
pub(crate) const ENV_NODE_REASONING_EFFORT: &str = "TIDEPOOL_NODE_REASONING_EFFORT";
pub(crate) const ENV_NODE_DEVELOPER_INSTRUCTIONS: &str = "TIDEPOOL_NODE_DEVELOPER_INSTRUCTIONS";

/// Deployment inputs for one pane-owned interactive agent incarnation.
///
/// This is the one typed boundary that knows the private environment protocol
/// consumed by `tidepool-agent-node`; composition roots never spell or parse
/// those variable names.
pub struct InteractiveNodeLaunch {
    pub actor: ActorRef,
    pub endpoint: PathBuf,
    pub credential: NodeCredential,
    pub binding_path: PathBuf,
    pub workspace: PathBuf,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub developer_instructions: String,
}

impl InteractiveNodeLaunch {
    #[must_use]
    pub fn environment(&self) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::from([
            (
                ENV_NODE_ENDPOINT.into(),
                self.endpoint.display().to_string(),
            ),
            (ENV_NODE_ACTOR_ID.into(), self.actor.id.0.to_string()),
            (
                ENV_NODE_INCARNATION.into(),
                self.actor.incarnation.0.to_string(),
            ),
            (ENV_NODE_CREDENTIAL.into(), self.credential.0.clone()),
            (
                ENV_NODE_BINDING_PATH.into(),
                self.binding_path.display().to_string(),
            ),
            (
                ENV_NODE_WORKSPACE.into(),
                self.workspace.display().to_string(),
            ),
            (
                ENV_NODE_DEVELOPER_INSTRUCTIONS.into(),
                self.developer_instructions.clone(),
            ),
        ]);
        if let Some(model) = &self.model {
            environment.insert(ENV_NODE_MODEL.into(), model.clone());
        }
        if let Some(effort) = self.effort {
            environment.insert(
                ENV_NODE_REASONING_EFFORT.into(),
                match effort {
                    ReasoningEffort::Low => "low",
                    ReasoningEffort::Medium => "medium",
                    ReasoningEffort::High => "high",
                }
                .into(),
            );
        }
        environment
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

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_actor::{ActorId, ActorRef};

    #[test]
    fn node_launch_is_the_only_environment_protocol_encoder() {
        let launch = InteractiveNodeLaunch {
            actor: ActorRef::first(ActorId(7)),
            endpoint: "/tmp/actor.sock".into(),
            credential: NodeCredential("secret".into()),
            binding_path: "/tmp/binding.json".into(),
            workspace: "/tmp/work".into(),
            model: Some("model-name".into()),
            effort: Some(ReasoningEffort::Medium),
            developer_instructions: "typed tools first".into(),
        };
        let environment = launch.environment();
        assert_eq!(environment[ENV_NODE_ACTOR_ID], "7");
        assert_eq!(environment[ENV_NODE_INCARNATION], "1");
        assert_eq!(environment[ENV_NODE_CREDENTIAL], "secret");
        assert_eq!(environment[ENV_NODE_MODEL], "model-name");
        assert_eq!(environment[ENV_NODE_REASONING_EFFORT], "medium");
        assert_eq!(
            environment[ENV_NODE_DEVELOPER_INSTRUCTIONS],
            "typed tools first"
        );
    }
}
