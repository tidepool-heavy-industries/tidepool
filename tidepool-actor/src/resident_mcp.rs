//! Transport-neutral handle for one resident Haskell MCP policy.
//!
//! The policy owns no machine, continuation, or scheduler. It only exposes an
//! immutable tool surface and submits invocations to `ResidentActorHost`, the
//! sole owner of actor execution.

use std::sync::Arc;
use std::{future::Future, pin::Pin};

use tidepool_runtime::session::{ResidentHole, WorkbenchRequest};
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

#[derive(Clone)]
enum ResidentMcpTransport {
    Legacy(mpsc::UnboundedSender<ResidentMcpInvocation>),
    Local(crate::LocalActorRef),
}

#[derive(Clone)]
pub(crate) struct ResidentMcpClient {
    actor: ActorRef,
    transport: ResidentMcpTransport,
    dispatch_gate: Arc<tokio::sync::Mutex<()>>,
}

impl ResidentMcpClient {
    pub(crate) fn new(
        actor: ActorRef,
        requests: mpsc::UnboundedSender<ResidentMcpInvocation>,
    ) -> Self {
        Self {
            actor,
            transport: ResidentMcpTransport::Legacy(requests),
            dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) fn local(actor: crate::LocalActorRef) -> Self {
        Self {
            actor: actor.identity(),
            transport: ResidentMcpTransport::Local(actor),
            dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) async fn dispatch(
        &self,
        name: String,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ResidentMcpError> {
        let _turn = self.dispatch_gate.lock().await;
        match &self.transport {
            ResidentMcpTransport::Legacy(requests) => {
                let (response, receive) = oneshot::channel();
                requests
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
            ResidentMcpTransport::Local(actor) => {
                let (response, receive) = oneshot::channel();
                actor
                    .address()
                    .send_message(crate::KernelMessage::Mcp {
                        name,
                        arguments,
                        reply: response.into(),
                    })
                    .map_err(|_| {
                        ResidentMcpError::Unavailable("the owning actor has stopped".into())
                    })?;
                receive
                    .await
                    .map_err(|_| {
                        ResidentMcpError::Unavailable(
                            "the actor stopped before settling the invocation".into(),
                        )
                    })?
                    .map_err(|error| ResidentMcpError::Failed(error.to_string()))
            }
        }
    }

    pub(crate) async fn dispatch_workbench(
        &self,
        request: WorkbenchRequest,
    ) -> Result<serde_json::Value, ResidentMcpError> {
        match &self.transport {
            ResidentMcpTransport::Legacy(_) => {
                let arguments = serde_json::to_value(request)
                    .map_err(|error| ResidentMcpError::Failed(error.to_string()))?;
                self.dispatch(
                    crate::resident_interactive::SESSION_RUN_TOOL.into(),
                    arguments,
                )
                .await
            }
            ResidentMcpTransport::Local(actor) => {
                let _turn = self.dispatch_gate.lock().await;
                let (response, receive) = oneshot::channel();
                actor
                    .address()
                    .send_message(crate::KernelMessage::Workbench {
                        request,
                        reply: response.into(),
                    })
                    .map_err(|_| {
                        ResidentMcpError::Unavailable("the owning actor has stopped".into())
                    })?;
                let response = receive.await.map_err(|_| {
                    ResidentMcpError::Unavailable(
                        "the actor stopped before settling the workbench invocation".into(),
                    )
                })?;
                let response =
                    response.map_err(|error| ResidentMcpError::Failed(error.to_string()))?;
                serde_json::to_value(response)
                    .map_err(|error| ResidentMcpError::Failed(error.to_string()))
            }
        }
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
        let client = self.client.clone();
        Box::pin(async move { client.dispatch(name, arguments).await })
    }
}

pub(crate) fn install_local_resident_mcp(
    actor: crate::LocalActorRef,
    awaiting: &ResidentMcpAwait,
) -> ResidentMcpPolicy {
    ResidentMcpPolicy {
        declarations: awaiting.declarations.clone().into(),
        instructions: (!awaiting.synopsis.is_empty()).then(|| awaiting.synopsis.clone()),
        client: ResidentMcpClient::local(actor),
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
