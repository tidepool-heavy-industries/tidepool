//! Actor-local custom-tool projection of the persistent Haskell workbench.

use std::sync::Arc;

use tidepool_runtime::session::{
    WorkbenchItemReceipt, WorkbenchItemStatus, WorkbenchRequest, WorkbenchResponse,
    WorkbenchRunStatus,
};
use tidepool_tool::{CustomToolDeclaration, HostedTool, ToolArguments, ToolInvocation};

use crate::prompt_catalog::PromptId;
use crate::resident_tools::{
    ResidentToolClient, ResidentToolEndpoint, ResidentToolError, ResidentToolFuture,
};

pub const HASKELL_TOOL: &str = "haskell";

pub struct ResidentInteractivePolicy {
    tools: Arc<[HostedTool]>,
    client: ResidentToolClient,
}

impl ResidentInteractivePolicy {
    /// Project the persistent Haskell workbench of this exact actor incarnation.
    /// All dispatch, completion, reattachment and sealing target this same actor;
    /// construction neither creates a session nor grants additional authority.
    ///
    /// Host composition should construct this projection from its owned actor
    /// rather than accept an independently supplied endpoint/actor pair. Each
    /// projection has a client serialization gate; the actor mailbox remains
    /// the shared admission and execution owner across multiple projections.
    pub fn local(actor: crate::LocalActorRef) -> Self {
        Self::with_client(ResidentToolClient::local(actor))
    }

    fn with_client(client: ResidentToolClient) -> Self {
        Self {
            tools: vec![haskell_tool_declaration()].into(),
            client,
        }
    }
}

fn haskell_tool_declaration() -> HostedTool {
    HostedTool::Custom(CustomToolDeclaration {
        name: HASKELL_TOOL.into(),
        description: PromptId::HaskellToolDescription.body().into(),
    })
}

fn haskell_tool_instructions() -> &'static str {
    PromptId::HaskellToolInstructions.body()
}

impl ResidentToolEndpoint for ResidentInteractivePolicy {
    fn seal_hosted_work_boxed(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<crate::HostedWorkSeal, ResidentToolError>>
                + Send
                + 'static,
        >,
    > {
        let client = self.client.clone();
        Box::pin(async move { client.seal().await })
    }

    fn tools(&self) -> &[HostedTool] {
        &self.tools
    }

    fn instructions(&self) -> Option<&str> {
        Some(haskell_tool_instructions())
    }

    fn reattach_boxed(&self) -> ResidentToolFuture {
        let client = self.client.clone();
        Box::pin(async move { client.reattach().await })
    }

    fn complete_boxed(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        let client = self.client.clone();
        Box::pin(async move { client.complete(boundary).await })
    }

    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
        let client = self.client.clone();
        Box::pin(async move {
            if invocation.name != HASKELL_TOOL {
                return Err(crate::ResidentToolError::InvalidInvocation(format!(
                    "unknown actor workbench tool `{}`",
                    invocation.name
                )));
            }
            let ToolArguments::Raw(source) = invocation.arguments else {
                return Err(crate::ResidentToolError::InvalidInvocation(
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
                            warnings: Vec::new(),
                            installed_bindings: Vec::new(),
                            operations: Vec::new(),
                            terminal_transfer: None,
                        }],
                        next_index: 0,
                        total: 1,
                    })
                    .map_err(ResidentToolError::Encoding);
                }
            };
            client.dispatch_workbench(request, invocation.context).await
        })
    }

    fn cancel_workbench_boxed(
        &self,
        invocation: tidepool_tool::ToolInvocationContext,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        crate::WorkbenchCancellationOutcome,
                        crate::ResidentToolError,
                    >,
                > + Send
                + 'static,
        >,
    > {
        let client = self.client.clone();
        Box::pin(async move { client.cancel_workbench(invocation).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_tool_surfaces_use_the_catalog_verbatim() {
        assert_eq!(
            haskell_tool_declaration().description(),
            PromptId::HaskellToolDescription.body()
        );
        assert_eq!(
            haskell_tool_instructions(),
            PromptId::HaskellToolInstructions.body()
        );
    }
}

#[cfg(test)]
#[path = "hosted_lifecycle_tests.rs"]
mod lifecycle_tests;
