//! Transport-neutral handle for one resident Haskell tool policy.
//!
//! The policy owns no machine, continuation, scheduler, or host protocol. It
//! exposes an immutable tool surface and submits typed invocations to the
//! owning local actor.

use std::collections::HashMap;
use std::sync::Arc;
use std::{future::Future, pin::Pin};

use tidepool_runtime::session::{ResidentHole, WorkbenchRequest};
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
    completed_workbench_calls: Arc<parking_lot::Mutex<WorkbenchCallLedger>>,
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

#[derive(Clone)]
struct CompletedWorkbenchCall {
    request: WorkbenchRequest,
    reply: crate::KernelWorkbenchReply,
}

#[derive(Default)]
struct WorkbenchCallLedger {
    completed: HashMap<WorkbenchCallKey, CompletedWorkbenchCall>,
}

impl WorkbenchCallLedger {
    fn lookup(
        &self,
        operation: &WorkbenchCallKey,
        request: &WorkbenchRequest,
    ) -> Result<Option<crate::KernelWorkbenchReply>, ResidentToolError> {
        let Some(completed) = self.completed.get(operation) else {
            return Ok(None);
        };
        if completed.request != *request {
            return Err(ResidentToolError::InvalidInvocation(
                "one hosted call identity was retried with different Haskell input".into(),
            ));
        }
        Ok(Some(completed.reply.clone()))
    }

    fn record(
        &mut self,
        operation: WorkbenchCallKey,
        request: WorkbenchRequest,
        reply: crate::KernelWorkbenchReply,
    ) {
        self.completed
            .insert(operation, CompletedWorkbenchCall { request, reply });
    }
}

impl ResidentToolClient {
    pub(crate) fn local(actor: crate::LocalActorRef) -> Self {
        Self {
            actor,
            dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
            completed_workbench_calls: Arc::new(parking_lot::Mutex::new(
                WorkbenchCallLedger::default(),
            )),
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
        request: WorkbenchRequest,
        invocation: Option<ToolInvocationContext>,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let _turn = self.dispatch_gate.lock().await;
        let operation = invocation.map(WorkbenchCallKey::from);
        let cached = operation
            .as_ref()
            .map(|operation| {
                self.completed_workbench_calls
                    .lock()
                    .lookup(operation, &request)
            })
            .transpose()?
            .flatten();
        if let Some(reply) = cached {
            return reply
                .map_err(ResidentToolError::Invocation)
                .and_then(|response| {
                    serde_json::to_value(response).map_err(ResidentToolError::Encoding)
                });
        }
        let cache_request = operation.as_ref().map(|_| request.clone());
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
        if let (Some(operation), Some(request)) = (operation, cache_request) {
            self.completed_workbench_calls
                .lock()
                .record(operation, request, reply.clone());
        }
        let response = reply.map_err(ResidentToolError::Invocation)?;
        serde_json::to_value(response).map_err(ResidentToolError::Encoding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_runtime::session::{
        WorkbenchItemReceipt, WorkbenchItemStatus, WorkbenchResponse, WorkbenchRunStatus,
    };

    fn call_key(call_id: &str) -> WorkbenchCallKey {
        ToolInvocationContext {
            thread_id: "thread".into(),
            turn_id: "turn".into(),
            call_id: call_id.into(),
            namespace: Some("actor".into()),
        }
        .into()
    }

    fn request(source: &str) -> WorkbenchRequest {
        WorkbenchRequest::from_ghci_input(source).expect("workbench request")
    }

    fn reply(output: &str) -> crate::KernelWorkbenchReply {
        Ok(WorkbenchResponse {
            status: WorkbenchRunStatus::Committed,
            items: vec![WorkbenchItemReceipt {
                index: 0,
                status: WorkbenchItemStatus::Committed,
                output: output.into(),
                warnings: Vec::new(),
            }],
            next_index: 1,
            total: 1,
        })
    }

    #[test]
    fn exact_hosted_call_retry_reuses_only_the_matching_receipt() {
        let mut ledger = WorkbenchCallLedger::default();
        let original = request("action");
        ledger.record(call_key("call-1"), original.clone(), reply("once"));

        assert_eq!(
            ledger.lookup(&call_key("call-1"), &original).unwrap(),
            Some(reply("once"))
        );
        assert!(matches!(
            ledger.lookup(&call_key("call-1"), &request("different")),
            Err(ResidentToolError::InvalidInvocation(_))
        ));
        assert_eq!(ledger.lookup(&call_key("call-2"), &original).unwrap(), None);
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
