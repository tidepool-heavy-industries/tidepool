//! Actor-local custom-tool projection of the persistent Haskell workbench.

use std::sync::Arc;

use tidepool_runtime::session::{
    WorkbenchItemReceipt, WorkbenchItemStatus, WorkbenchRequest, WorkbenchResponse,
    WorkbenchRunStatus,
};
use tidepool_tool::{CustomToolDeclaration, HostedTool, ToolArguments, ToolInvocation};

use crate::resident_tools::{
    ResidentToolClient, ResidentToolEndpoint, ResidentToolError, ResidentToolFuture,
};

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
                description: "Run a GHCi-style script in this actor's persistent session. Send raw input without JSON or Markdown fences. Outside `:{` / `:}`, each colon-prefixed line is one reserved command and every other nonblank line is one Haskell input unit. Inside `:{` / `:}`, the entire body is one GHC input unit: ordinary declaration groups are valid, effect sequences belong in `do`, and persisting several effect results requires one outer tuple or record pattern binding. Units execute in order and stop at the first rejection; earlier successful units remain committed. A rejected effectful unit does not install its projected bindings and does not roll back effects already performed. Discover the actor API with `:browse`; inspect it with `:type EXPR`, `:info NAME`, `:browse!`, and `:bindings`. `sessionInput` is the stable typed input for this activation. Return executable work with `complete action`; for example, `complete $ nextTurn $ assemble <$> waitOn actorA <*> waitOn actorB` settles the tool call immediately, waits outside inference, and reactivates this same agent with the typed result.".into(),
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
            "Use tidepool_actor.haskell as the primary actor orchestration surface. Its raw payload is a GHCi-style script, not JSON or Markdown. Outside :{ / :}, each nonblank line is one input unit. A fenced body is one GHC input unit: use ordinary declaration groups, put effect sequences in do, and use one outer tuple or record pattern binding to persist several results. Units run in order and preserve successful prefixes; effects are not rolled back when a unit rejects. Start discovery with :browse; use :type, :info, :browse!, and :bindings for detail. Use native coding tools for repository work.",
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
            let request = match WorkbenchRequest::from_ghci_input(&source) {
                Ok(request) => request,
                Err(error) => {
                    return serde_json::to_value(WorkbenchResponse {
                        status: WorkbenchRunStatus::Rejected,
                        items: vec![WorkbenchItemReceipt {
                            index: 0,
                            status: WorkbenchItemStatus::Rejected,
                            output: error.to_string(),
                        }],
                        next_index: 0,
                        total: 1,
                    })
                    .map_err(|error| ResidentToolError::Failed(error.to_string()));
                }
            };
            client.dispatch_workbench(request).await
        })
    }
}
