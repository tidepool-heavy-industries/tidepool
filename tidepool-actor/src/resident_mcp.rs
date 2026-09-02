//! Transport-neutral handle for one resident Haskell MCP policy.
//!
//! The policy owns no machine, continuation, or scheduler. It only exposes an
//! immutable tool surface and submits invocations to `ResidentActorHost`, the
//! sole owner of actor execution.

use std::sync::Arc;
use std::{future::Future, pin::Pin};

use tidepool_runtime::session::ResidentHole;
use tokio::sync::{mpsc, oneshot};

use crate::ActorRef;

/// A policy waiting for its next invocation.
pub(crate) struct ResidentMcpAwait {
    pub(crate) continuation: ResidentHole,
    pub(crate) declarations: Vec<tidepool_tool::ToolDeclaration>,
    pub(crate) synopsis: String,
    pub(crate) initial_user_message: Option<String>,
}

/// A completed invocation waiting for Rust to acknowledge its result.
pub(crate) struct ResidentMcpReply {
    pub(crate) continuation: ResidentHole,
    pub(crate) result: serde_json::Value,
}

pub(crate) struct ResidentMcpInvocation {
    pub(crate) actor: ActorRef,
    pub(crate) name: String,
    pub(crate) arguments: serde_json::Value,
    pub(crate) response: oneshot::Sender<Result<serde_json::Value, String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentMcpError {
    #[error("resident MCP policy is unavailable: {0}")]
    Unavailable(String),
    #[error("resident MCP invocation failed: {0}")]
    Failed(String),
}

/// A cloneable request handle for one exact actor incarnation.
///
/// Calls are serialized because a resident actor has one turn at a time. The
/// host retains and resumes the Haskell continuation through every actor
/// boundary reached by the tool handler.
pub struct ResidentMcpPolicy {
    declarations: Arc<[tidepool_tool::ToolDeclaration]>,
    instructions: Option<String>,
    client: ResidentMcpClient,
}

pub type ResidentMcpFuture =
    Pin<Box<dyn Future<Output = Result<serde_json::Value, ResidentMcpError>> + Send + 'static>>;

/// Transport-neutral interface projected by an actor-local MCP server.
/// Implementations retain actor admission and execution ownership behind
/// their dispatcher; the projection sees only declarations and structured
/// results.
pub trait ResidentMcpEndpoint: Send + Sync {
    fn declarations(&self) -> &[tidepool_tool::ToolDeclaration];
    fn instructions(&self) -> Option<&str>;
    fn dispatch_boxed(&self, name: String, arguments: serde_json::Value) -> ResidentMcpFuture;
}

pub(crate) struct ResidentMcpClient {
    pub(crate) actor: ActorRef,
    pub(crate) requests: mpsc::UnboundedSender<ResidentMcpInvocation>,
    pub(crate) dispatch_gate: Arc<tokio::sync::Mutex<()>>,
}

impl ResidentMcpClient {
    pub(crate) fn new(
        actor: ActorRef,
        requests: mpsc::UnboundedSender<ResidentMcpInvocation>,
    ) -> Self {
        Self {
            actor,
            requests,
            dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) async fn dispatch(
        &self,
        name: String,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ResidentMcpError> {
        let _turn = self.dispatch_gate.lock().await;
        let (response, receive) = oneshot::channel();
        self.requests
            .send(ResidentMcpInvocation {
                actor: self.actor,
                name,
                arguments,
                response,
            })
            .map_err(|_| {
                ResidentMcpError::Unavailable("the owning actor host has stopped".into())
            })?;
        receive
            .await
            .map_err(|_| {
                ResidentMcpError::Unavailable(
                    "the actor stopped before settling the invocation".into(),
                )
            })?
            .map_err(ResidentMcpError::Failed)
    }
}

impl ResidentMcpPolicy {
    #[must_use]
    pub fn declarations(&self) -> &[tidepool_tool::ToolDeclaration] {
        &self.declarations
    }

    #[must_use]
    pub fn instructions(&self) -> Option<&str> {
        self.instructions.as_deref()
    }

    pub async fn dispatch(
        &self,
        name: String,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ResidentMcpError> {
        self.client.dispatch(name, arguments).await
    }
}

impl ResidentMcpEndpoint for ResidentMcpPolicy {
    fn declarations(&self) -> &[tidepool_tool::ToolDeclaration] {
        self.declarations()
    }

    fn instructions(&self) -> Option<&str> {
        self.instructions()
    }

    fn dispatch_boxed(&self, name: String, arguments: serde_json::Value) -> ResidentMcpFuture {
        let client = ResidentMcpClient {
            actor: self.client.actor,
            requests: self.client.requests.clone(),
            dispatch_gate: Arc::clone(&self.client.dispatch_gate),
        };
        Box::pin(async move { client.dispatch(name, arguments).await })
    }
}

pub(crate) fn install_resident_mcp(
    actor: ActorRef,
    requests: mpsc::UnboundedSender<ResidentMcpInvocation>,
    awaiting: &ResidentMcpAwait,
) -> ResidentMcpPolicy {
    ResidentMcpPolicy {
        declarations: awaiting.declarations.clone().into(),
        instructions: (!awaiting.synopsis.is_empty()).then(|| awaiting.synopsis.clone()),
        client: ResidentMcpClient::new(actor, requests),
    }
}
