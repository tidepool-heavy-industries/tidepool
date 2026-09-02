//! Actor-local MCP projection of the persistent Haskell workbench.

use std::sync::Arc;

use tidepool_runtime::session::{WorkbenchRequest, WorkbenchResponse};
use tidepool_tool::{ToolDeclaration, ToolKind};

use crate::resident_mcp::{
    ResidentMcpClient, ResidentMcpEndpoint, ResidentMcpFuture, ResidentMcpInvocation,
};
use crate::ActorRef;

pub(crate) const SESSION_RUN_TOOL: &str = "session_run";

pub struct ResidentInteractivePolicy {
    declarations: Arc<[ToolDeclaration]>,
    client: ResidentMcpClient,
}

impl ResidentInteractivePolicy {
    pub(crate) fn new(
        actor: ActorRef,
        requests: tokio::sync::mpsc::UnboundedSender<ResidentMcpInvocation>,
    ) -> Self {
        Self::with_client(ResidentMcpClient::new(actor, requests))
    }

    pub(crate) fn local(actor: crate::LocalActorRef) -> Self {
        Self::with_client(ResidentMcpClient::local(actor))
    }

    fn with_client(client: ResidentMcpClient) -> Self {
        let input_schema = schemars::schema_for!(WorkbenchRequest).as_value().clone();
        Self {
            declarations: vec![ToolDeclaration {
                name: SESSION_RUN_TOOL.into(),
                description: "Run ordered GHCi-style Haskell items in this actor's persistent session. Declarations and bindings survive later calls. Actor operations are ordinary Haskell effects; start every independent child before awaiting any result. Complete the enclosing typed agent session with `complete value` only when its fixed Haskell result is ready.".into(),
                input_schema,
                output_schema: Some(schemars::schema_for!(WorkbenchResponse).as_value().clone()),
                kind: ToolKind::Call,
            }]
            .into(),
            client,
        }
    }
}

impl ResidentMcpEndpoint for ResidentInteractivePolicy {
    fn declarations(&self) -> &[ToolDeclaration] {
        &self.declarations
    }

    fn instructions(&self) -> Option<&str> {
        Some(
            "Use session_run as the primary actor orchestration surface. Build persistent Haskell declarations and live bindings; use native coding tools for repository work.",
        )
    }

    fn dispatch_boxed(&self, name: String, arguments: serde_json::Value) -> ResidentMcpFuture {
        let client = self.client.clone();
        Box::pin(async move {
            if name != SESSION_RUN_TOOL {
                return Err(crate::ResidentMcpError::Failed(format!(
                    "unknown actor workbench tool `{name}`"
                )));
            }
            let request = serde_json::from_value(arguments)
                .map_err(|error| crate::ResidentMcpError::Failed(error.to_string()))?;
            client.dispatch_workbench(request).await
        })
    }
}
