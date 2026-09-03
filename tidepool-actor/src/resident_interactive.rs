//! Actor-local custom-tool projection of the persistent Haskell workbench.

use std::sync::Arc;

use tidepool_runtime::session::WorkbenchRequest;
use tidepool_tool::{CustomToolDeclaration, HostedTool, ToolArguments, ToolInvocation};

use crate::resident_tools::{ResidentToolClient, ResidentToolEndpoint, ResidentToolFuture};

pub const HASKELL_TOOL: &str = "haskell";

pub struct ResidentInteractivePolicy {
    tools: Arc<[HostedTool]>,
    client: ResidentToolClient,
}

impl ResidentInteractivePolicy {
    pub(crate) fn local(actor: crate::LocalActorRef) -> Self {
        Self::with_client(ResidentToolClient::local(actor))
    }

    fn with_client(client: ResidentToolClient) -> Self {
        Self {
            tools: vec![HostedTool::Custom(CustomToolDeclaration {
                name: HASKELL_TOOL.into(),
                description: "Run one raw GHCi-style Haskell item in this actor's persistent session. Send Haskell source directly, without JSON or Markdown fences. Ordinary declarations, expressions, and pattern bindings survive later calls; inspect them with `:type EXPR`, `:info NAME`, and `:bindings`. Successful items commit immediately, so after a rejection retry only the rejected item and anything that depended on it. `sessionInput` is the stable typed input for this activation. Actor operations are ordinary Haskell effects. Return executable work with `complete action`; for example, `complete $ nextTurn $ assemble <$> waitOn actorA <*> waitOn actorB` settles this tool call, waits outside the model turn, and reactivates this same agent with the typed result.".into(),
            })]
            .into(),
            client,
        }
    }
}

impl ResidentToolEndpoint for ResidentInteractivePolicy {
    fn tools(&self) -> &[HostedTool] {
        &self.tools
    }

    fn instructions(&self) -> Option<&str> {
        Some(
            "Use tidepool_actor.haskell as the primary actor orchestration surface. Its raw payload is one Haskell item, not JSON or Markdown: build persistent declarations and live bindings, query types with :type/:info/:bindings, and use native coding tools for repository work.",
        )
    }

    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
        let client = self.client.clone();
        Box::pin(async move {
            if invocation.name != HASKELL_TOOL {
                return Err(crate::ResidentToolError::Failed(format!(
                    "unknown actor workbench tool `{}`",
                    invocation.name
                )));
            }
            let ToolArguments::Raw(source) = invocation.arguments else {
                return Err(crate::ResidentToolError::Failed(
                    "actor Haskell tool received structured arguments".into(),
                ));
            };
            let request = WorkbenchRequest {
                items: vec![source],
                input: None,
                verbose: None,
            };
            client.dispatch_workbench(request).await
        })
    }
}
