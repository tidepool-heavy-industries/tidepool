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
    pub(crate) fn local(actor: crate::LocalActorRef) -> Self {
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
    fn tools(&self) -> &[HostedTool] {
        &self.tools
    }

    fn instructions(&self) -> Option<&str> {
        Some(haskell_tool_instructions())
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
