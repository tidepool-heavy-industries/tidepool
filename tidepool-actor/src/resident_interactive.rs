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

    pub fn local_with_tools(actor: crate::LocalActorRef, tools: Vec<HostedTool>) -> Self {
        Self {
            tools: std::iter::once(haskell_tool_declaration())
                .chain(tools)
                .collect::<Vec<_>>()
                .into(),
            client: ResidentToolClient::local(actor),
        }
    }

    fn with_client(client: ResidentToolClient) -> Self {
        Self {
            tools: vec![haskell_tool_declaration()].into(),
            client,
        }
    }
}

pub(crate) fn project_tools(
    declarations: Vec<tidepool_tool::ToolDeclaration>,
) -> Result<Vec<HostedTool>, ResidentToolError> {
    use tidepool_tool::ToolKind;
    let mut names = std::collections::HashSet::from([HASKELL_TOOL.to_string()]);
    declarations
        .into_iter()
        .map(|declaration| {
            if declaration.name.is_empty()
                || declaration.name.len() > 64
                || !declaration
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_')
                || !names.insert(declaration.name.clone())
            {
                return Err(ResidentToolError::InvalidInvocation(format!(
                    "invalid, duplicate or reserved tool name {:?}",
                    declaration.name
                )));
            }
            match declaration.kind {
                ToolKind::Raw => Ok(declaration.into()),
                ToolKind::Call | ToolKind::Notify
                    if declaration
                        .input_schema
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        == Some("object") =>
                {
                    Ok(HostedTool::Function(declaration))
                }
                _ => Err(ResidentToolError::InvalidInvocation(format!(
                    "tool {:?} is not an interactive call with supported input",
                    declaration.name
                ))),
            }
        })
        .collect()
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

    fn output_format(&self) -> crate::ResidentToolOutput {
        crate::ResidentToolOutput::Workbench
    }

    fn instructions(&self) -> Option<&str> {
        Some(haskell_tool_instructions())
    }

    fn reconcile_workbench_boxed(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        crate::WorkbenchBoundaryReconciliation,
                        crate::ResidentToolError,
                    >,
                > + Send
                + 'static,
        >,
    > {
        let client = self.client.clone();
        Box::pin(async move { client.reconcile_workbench(boundary).await })
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
        let tools = self.tools.clone();
        Box::pin(async move {
            let declaration = tools
                .iter()
                .find(|tool| tool.name() == invocation.name)
                .ok_or_else(|| {
                    ResidentToolError::InvalidInvocation(format!(
                        "unknown actor tool {:?}",
                        invocation.name
                    ))
                })?;
            if !declaration.accepts(&invocation.arguments) {
                return Err(ResidentToolError::InvalidInvocation(format!(
                    "invalid argument kind for {:?}",
                    invocation.name
                )));
            }
            if invocation.name != HASKELL_TOOL {
                let arguments = match invocation.arguments {
                    ToolArguments::Raw(text) => serde_json::Value::String(text),
                    ToolArguments::Structured(value) => value,
                };
                return client
                    .dispatch_workbench(
                        WorkbenchRequest::for_tool(invocation.name, arguments),
                        invocation.context,
                    )
                    .await;
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
                    Output = Result<crate::WorkbenchCancellationOutcome, crate::ResidentToolError>,
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

    fn raw(name: &str) -> tidepool_tool::ToolDeclaration {
        tidepool_tool::ToolDeclaration {
            name: name.into(),
            description: "literal input".into(),
            input_schema: serde_json::json!({"type":"string"}),
            output_schema: None,
            kind: tidepool_tool::ToolKind::Raw,
        }
    }

    #[test]
    fn project_tools_checks_names_and_supported_input_before_publication() {
        for name in ["", "haskell", "bad.name", "λ", &"a".repeat(65)] {
            assert!(project_tools(vec![raw(name)]).is_err(), "{name}");
        }
        assert!(project_tools(vec![raw("bash"), raw("bash")]).is_err());
        let mut structured = raw("structured");
        structured.kind = tidepool_tool::ToolKind::Call;
        assert!(project_tools(vec![structured.clone()]).is_err());
        structured.input_schema = serde_json::json!({"type":"object","properties":{}});
        let projected = project_tools(vec![raw("bash"), structured]).unwrap();
        assert!(matches!(projected[0], HostedTool::Custom(_)));
        assert!(matches!(projected[1], HostedTool::Function(_)));
        for kind in [
            tidepool_tool::ToolKind::Update,
            tidepool_tool::ToolKind::Finish,
        ] {
            let mut unsupported = raw("stateful");
            unsupported.kind = kind;
            assert!(project_tools(vec![unsupported]).is_err());
        }
    }

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
