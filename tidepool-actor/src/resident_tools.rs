//! Transport-neutral handle for one resident Haskell tool policy.
//!
//! The policy owns no machine, continuation, scheduler, or host protocol. It
//! exposes an immutable tool surface and submits typed invocations to the
//! owning local actor.

use std::sync::Arc;
use std::{future::Future, pin::Pin};

use tidepool_runtime::session::{ResidentHole, WorkbenchExecutionId, WorkbenchRequest};
use tidepool_tool::{HostedTool, ToolArguments, ToolInvocation, ToolInvocationContext};
use tokio::sync::oneshot;

/// A policy waiting for its next invocation.
pub(crate) struct ResidentToolAwait {
    pub(crate) continuation: ResidentHole,
    pub(crate) declarations: Vec<tidepool_tool::ToolDeclaration>,
    pub(crate) synopsis: String,
    pub(crate) initial_user_message: Option<String>,
}

/// A completed invocation waiting for Rust to acknowledge its result.
pub(crate) struct ResidentToolReply {
    pub(crate) continuation: ResidentHole,
    pub(crate) result: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentToolError {
    #[error("resident tool policy is unavailable: {0}")]
    Unavailable(String),
    #[error("invalid resident tool invocation: {0}")]
    InvalidInvocation(String),
    #[error(transparent)]
    Invocation(#[from] crate::KernelInvocationFailure),
    #[error("could not encode resident tool response: {0}")]
    Encoding(#[from] serde_json::Error),
}

/// A cloneable request handle for one exact actor incarnation.
///
/// Calls are serialized because a resident actor has one turn at a time. The
/// owning actor retains and resumes the Haskell continuation through every
/// boundary reached by the tool handler.
pub struct ResidentToolPolicy {
    tools: Arc<[HostedTool]>,
    instructions: Option<String>,
    client: ResidentToolClient,
}

pub type ResidentToolFuture =
    Pin<Box<dyn Future<Output = Result<serde_json::Value, ResidentToolError>> + Send + 'static>>;

/// Transport-neutral interface projected by an actor-local tool host.
/// Implementations retain actor admission and execution ownership behind
/// their dispatcher; a concrete host sees only declarations and typed
/// invocations.
pub trait ResidentToolEndpoint: Send + Sync {
    fn tools(&self) -> &[HostedTool];
    fn instructions(&self) -> Option<&str>;
    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture;
}

#[derive(Clone)]
pub(crate) struct ResidentToolClient {
    actor: crate::LocalActorRef,
    dispatch_gate: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WorkbenchCallKey {
    thread_id: String,
    turn_id: String,
    call_id: String,
    namespace: Option<String>,
}

impl From<ToolInvocationContext> for WorkbenchCallKey {
    fn from(context: ToolInvocationContext) -> Self {
        Self {
            thread_id: context.thread_id,
            turn_id: context.turn_id,
            call_id: context.call_id,
            namespace: context.namespace,
        }
    }
}

fn execution_id(actor: crate::ActorRef, operation: &WorkbenchCallKey) -> WorkbenchExecutionId {
    fn field(hasher: &mut blake3::Hasher, value: &[u8]) {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tidepool.workbench-execution.v1\0");
    hasher.update(&actor.id.0.to_le_bytes());
    hasher.update(&actor.incarnation.0.to_le_bytes());
    field(&mut hasher, operation.thread_id.as_bytes());
    field(&mut hasher, operation.turn_id.as_bytes());
    field(&mut hasher, operation.call_id.as_bytes());
    match &operation.namespace {
        Some(namespace) => {
            hasher.update(&[1]);
            field(&mut hasher, namespace.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    let mut digest = [0_u8; 16];
    digest.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    WorkbenchExecutionId::from_digest(digest)
}

impl ResidentToolClient {
    pub(crate) fn local(actor: crate::LocalActorRef) -> Self {
        Self {
            actor,
            dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) async fn dispatch(
        &self,
        invocation: ToolInvocation,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let _turn = self.dispatch_gate.lock().await;
        let (response, receive) = oneshot::channel();
        self.actor
            .address()
            .send_message(crate::KernelMessage::Tool {
                invocation,
                reply: response.into(),
            })
            .map_err(|_| ResidentToolError::Unavailable("the owning actor has stopped".into()))?;
        receive
            .await
            .map_err(|_| {
                ResidentToolError::Unavailable(
                    "the actor stopped before settling the invocation".into(),
                )
            })?
            .map_err(ResidentToolError::Invocation)
    }

    pub(crate) async fn dispatch_workbench(
        &self,
        mut request: WorkbenchRequest,
        invocation: Option<ToolInvocationContext>,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let _turn = self.dispatch_gate.lock().await;
        if let Some(invocation) = invocation {
            let operation = WorkbenchCallKey::from(invocation);
            request = request.with_execution_id(execution_id(self.actor.identity(), &operation));
        }
        let (response, receive) = oneshot::channel();
        self.actor
            .address()
            .send_message(crate::KernelMessage::Workbench {
                request,
                reply: response.into(),
            })
            .map_err(|_| ResidentToolError::Unavailable("the owning actor has stopped".into()))?;
        let reply = receive.await.map_err(|_| {
            ResidentToolError::Unavailable(
                "the actor stopped before settling the workbench invocation".into(),
            )
        })?;
        let response = reply.map_err(ResidentToolError::Invocation)?;
        serde_json::to_value(response).map_err(ResidentToolError::Encoding)
    }
}

impl ResidentToolPolicy {
    #[must_use]
    pub fn tools(&self) -> &[HostedTool] {
        &self.tools
    }

    #[must_use]
    pub fn instructions(&self) -> Option<&str> {
        self.instructions.as_deref()
    }

    pub async fn dispatch(
        &self,
        invocation: ToolInvocation,
    ) -> Result<serde_json::Value, ResidentToolError> {
        if !matches!(&invocation.arguments, ToolArguments::Structured(_)) {
            return Err(ResidentToolError::InvalidInvocation(
                "function tool received raw arguments".into(),
            ));
        }
        self.client.dispatch(invocation).await
    }
}

impl ResidentToolEndpoint for ResidentToolPolicy {
    fn tools(&self) -> &[HostedTool] {
        self.tools()
    }

    fn instructions(&self) -> Option<&str> {
        self.instructions()
    }

    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
        let client = self.client.clone();
        Box::pin(async move {
            if !matches!(&invocation.arguments, ToolArguments::Structured(_)) {
                return Err(ResidentToolError::InvalidInvocation(
                    "function tool received raw arguments".into(),
                ));
            }
            client.dispatch(invocation).await
        })
    }
}

pub(crate) fn install_local_resident_tools(
    actor: crate::LocalActorRef,
    awaiting: &ResidentToolAwait,
) -> ResidentToolPolicy {
    ResidentToolPolicy {
        tools: awaiting
            .declarations
            .iter()
            .cloned()
            .map(HostedTool::Function)
            .collect::<Vec<_>>()
            .into(),
        instructions: (!awaiting.synopsis.is_empty()).then(|| awaiting.synopsis.clone()),
        client: ResidentToolClient::local(actor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call_key(call_id: &str) -> WorkbenchCallKey {
        ToolInvocationContext {
            thread_id: "thread".into(),
            turn_id: "turn".into(),
            call_id: call_id.into(),
            namespace: Some("actor".into()),
        }
        .into()
    }

    #[test]
    fn execution_identity_is_exact_to_actor_and_hosted_call() {
        let actor = crate::ActorRef::first(crate::ActorId(7));
        let original = execution_id(actor, &call_key("call-1"));
        assert_eq!(original, execution_id(actor, &call_key("call-1")));
        assert_ne!(original, execution_id(actor, &call_key("call-2")));
        assert_ne!(
            original,
            execution_id(
                crate::ActorRef {
                    id: actor.id,
                    incarnation: crate::Incarnation(2),
                },
                &call_key("call-1")
            )
        );
    }
}
