//! Actor-local custom-tool projection of the persistent Haskell workbench.

use std::sync::Arc;

use exomonad_tool::{HostedTool, ToolArguments, ToolImplementation, ToolInvocation};
use tidepool_runtime::session::WorkbenchRequest;

use crate::prompt_catalog::PromptId;
use crate::resident_tools::{
    ResidentToolClient, ResidentToolDispatchFuture, ResidentToolEndpoint, ResidentToolError,
    ResidentToolFuture, ResidentToolResponse,
};

pub const HASKELL_TOOL: &str = "haskell";

pub struct ResidentInteractivePolicy {
    tools: Arc<[HostedTool]>,
    client: ResidentToolClient,
    installed_tools: Arc<crate::resident_workbench::InstalledToolsState>,
    issued_tools: Option<crate::InstalledToolLease>,
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

    /// `tools` is the caller's custom set. A caller that reprojects an
    /// already-built policy's own `tools()` hands back the reserved declarations
    /// below; dropping them here keeps this constructor
    /// idempotent instead of requiring every caller to know and repeat the
    /// exact reserved-name set.
    pub fn local_with_tools(actor: crate::LocalActorRef, tools: Vec<HostedTool>) -> Self {
        Self::local_with_installation(
            actor,
            tools,
            Arc::new(crate::resident_workbench::InstalledToolsState::default()),
        )
    }

    pub(crate) fn local_with_installation(
        actor: crate::LocalActorRef,
        tools: Vec<HostedTool>,
        installed_tools: Arc<crate::resident_workbench::InstalledToolsState>,
    ) -> Self {
        let custom = tools.into_iter().filter(|tool| {
            !matches!(
                tool.name(),
                crate::status_tool::STATUS_TOOL
                    | crate::reload_spec_tool::RELOAD_SPEC_TOOL
                    | crate::reload_helpers_tool::RELOAD_HELPERS_TOOL
            )
        });
        Self {
            tools: std::iter::once(crate::status_tool::declaration())
                .chain(std::iter::once(crate::reload_spec_tool::declaration()))
                .chain(std::iter::once(crate::reload_helpers_tool::declaration()))
                .chain(custom)
                .collect::<Vec<_>>()
                .into(),
            client: ResidentToolClient::local(actor),
            installed_tools,
            issued_tools: None,
        }
    }

    fn with_client(client: ResidentToolClient) -> Self {
        Self {
            tools: vec![
                crate::status_tool::declaration(),
                crate::reload_spec_tool::declaration(),
                crate::reload_helpers_tool::declaration(),
            ]
            .into(),
            client,
            installed_tools: Arc::default(),
            issued_tools: None,
        }
    }
}

pub(crate) fn project_tools(
    declarations: Vec<exomonad_tool::ToolDeclaration>,
) -> Result<Vec<HostedTool>, ResidentToolError> {
    use exomonad_tool::ToolKind;
    let mut names = std::collections::HashSet::from([
        crate::status_tool::STATUS_TOOL.to_string(),
        crate::reload_spec_tool::RELOAD_SPEC_TOOL.to_string(),
        crate::reload_helpers_tool::RELOAD_HELPERS_TOOL.to_string(),
    ]);
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
                ToolKind::Raw | ToolKind::Call | ToolKind::Notify => {
                    HostedTool::try_from(declaration).map_err(ResidentToolError::from)
                }
                _ => Err(ResidentToolError::InvalidInvocation(format!(
                    "tool {:?} is not an interactive call with supported input",
                    declaration.name
                ))),
            }
        })
        .collect()
}

fn request_for_tool(
    declaration: &HostedTool,
    arguments: ToolArguments,
) -> Result<WorkbenchRequest, ResidentToolError> {
    match declaration.implementation() {
        ToolImplementation::ResidentHandler => {
            let arguments = match arguments {
                ToolArguments::Raw(text) => serde_json::Value::String(text),
                ToolArguments::Structured(value) => value,
            };
            Ok(WorkbenchRequest::for_tool(
                declaration.name().into(),
                arguments,
            ))
        }
        ToolImplementation::HaskellCell => {
            let ToolArguments::Raw(source) = arguments else {
                return Err(ResidentToolError::InvalidInvocation(
                    "native Haskell cell requires raw source input".into(),
                ));
            };
            Ok(WorkbenchRequest::from_cell_input(&source))
        }
    }
}

// Builtins are served by their owning actor routes and are absent from the
// compiled AgentSpec. Only exact owned declarations use that admission path.
fn selected_contract(declaration: &HostedTool) -> Option<HostedTool> {
    if declaration == &crate::status_tool::declaration()
        || declaration == &crate::reload_spec_tool::declaration()
        || declaration == &crate::reload_helpers_tool::declaration()
    {
        None
    } else {
        Some(declaration.clone())
    }
}

fn haskell_tool_instructions() -> &'static str {
    PromptId::HaskellToolInstructions.body()
}

impl ResidentToolEndpoint for ResidentInteractivePolicy {
    fn expand_display_boxed(&self, identity: (i64, i64, i64), key: i64) -> ResidentToolFuture {
        let client = self.client.clone();
        Box::pin(async move { client.expand_display(identity, key).await })
    }

    fn snapshot_for_request(&self) -> Result<Arc<dyn ResidentToolEndpoint>, ResidentToolError> {
        let issued_tools = self
            .issued_tools
            .clone()
            .or_else(|| self.installed_tools.current())
            .ok_or_else(|| {
                ResidentToolError::Unavailable("actor has no installed tool/source snapshot".into())
            })?;
        Ok(Arc::new(Self {
            tools: self.tools.clone(),
            client: self.client.clone(),
            installed_tools: self.installed_tools.clone(),
            issued_tools: Some(issued_tools),
        }))
    }

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

    fn retained_operation(
        &self,
        invocation: exomonad_tool::ToolInvocationContext,
    ) -> Result<crate::HostedOperationSettlement, ResidentToolError> {
        self.client.retained_operation(invocation)
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

    fn abort_boxed(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        let client = self.client.clone();
        Box::pin(async move { client.abort(boundary).await })
    }

    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolDispatchFuture {
        self.dispatch_with_checkpoint_boxed(invocation, None)
    }

    fn dispatch_with_checkpoint_boxed(
        &self,
        invocation: ToolInvocation,
        capture: Option<std::sync::Arc<dyn crate::HostedCheckpointCapture>>,
    ) -> ResidentToolDispatchFuture {
        self.dispatch_with_context_boxed(invocation, capture, None)
    }

    fn dispatch_with_context_boxed(
        &self,
        invocation: ToolInvocation,
        capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
        context: Option<Arc<dyn crate::HostedContextBinding>>,
    ) -> ResidentToolDispatchFuture {
        let client = self.client.clone();
        let tools = self.tools.clone();
        let installed_tools = self.issued_tools.clone();
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
            let request = request_for_tool(declaration, invocation.arguments)?;
            client
                .dispatch_workbench_issued_with_context(
                    request,
                    invocation.context,
                    installed_tools,
                    capture,
                    context,
                    selected_contract(declaration),
                )
                .await
                .map(ResidentToolResponse::Workbench)
        })
    }

    fn cancel_workbench_boxed(
        &self,
        invocation: exomonad_tool::ToolInvocationContext,
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

    fn raw(name: &str) -> exomonad_tool::ToolDeclaration {
        exomonad_tool::ToolDeclaration {
            schedule: Default::default(),
            implementation: Default::default(),
            effect_keys: Vec::new(),
            name: name.into(),
            description: "literal input".into(),
            input_schema: serde_json::json!({"type":"string"}),
            output_schema: None,
            kind: exomonad_tool::ToolKind::Raw,
        }
    }

    #[test]
    fn project_tools_checks_names_and_supported_input_before_publication() {
        for name in [
            "",
            "status",
            "reload_agent_spec",
            "reload_helpers",
            "bad.name",
            "λ",
            &"a".repeat(65),
        ] {
            assert!(project_tools(vec![raw(name)]).is_err(), "{name}");
        }
        assert!(project_tools(vec![raw("lookup")]).is_ok());
        assert!(project_tools(vec![raw("bash"), raw("bash")]).is_err());
        let mut structured = raw("structured");
        structured.kind = exomonad_tool::ToolKind::Call;
        assert!(project_tools(vec![structured.clone()]).is_err());
        structured.input_schema = serde_json::json!({"type": "null"});
        assert!(matches!(
            project_tools(vec![structured.clone()]),
            Err(ResidentToolError::Declaration(
                exomonad_tool::ToolDeclarationError::FunctionInputMustBeObject { name }
            )) if name == "structured"
        ));
        structured.input_schema = serde_json::json!({"type":"object","properties":{}});
        let projected = project_tools(vec![raw("bash"), structured]).unwrap();
        assert!(matches!(projected[0], HostedTool::Custom(_)));
        assert!(matches!(projected[1], HostedTool::Function(_)));
        for kind in [
            exomonad_tool::ToolKind::Update,
            exomonad_tool::ToolKind::Finish,
        ] {
            let mut unsupported = raw("stateful");
            unsupported.kind = kind;
            assert!(project_tools(vec![unsupported]).is_err());
        }
    }

    #[test]
    fn builtin_dispatch_uses_owned_admission_without_a_spec_leaf() {
        for builtin in [
            crate::status_tool::declaration(),
            crate::reload_spec_tool::declaration(),
            crate::reload_helpers_tool::declaration(),
        ] {
            let request =
                request_for_tool(&builtin, ToolArguments::Structured(serde_json::json!({})))
                    .unwrap();
            assert_eq!(request.tool_call().unwrap().name, builtin.name());
            assert!(selected_contract(&builtin).is_none());
            let mut altered = builtin.clone();
            let HostedTool::Function(tool) = &mut altered else {
                unreachable!()
            };
            tool.schedule = exomonad_tool::ToolScheduling::BeforeNextInference;
            assert!(selected_contract(&altered).is_some());
        }
        let authored = HostedTool::try_from(raw("authored")).unwrap();
        assert_eq!(selected_contract(&authored), Some(authored.clone()));
    }

    #[test]
    fn native_dispatch_is_selected_by_implementation_not_name() {
        let mut source = raw("arbitrary_name");
        source.implementation = ToolImplementation::HaskellCell;
        let native = HostedTool::try_from(source).unwrap();
        let request = request_for_tool(&native, ToolArguments::Raw("pure ()".into())).unwrap();
        assert_eq!(request.cell_source(), Some("pure ()"));
        assert!(request.tool_call().is_none());
        let handler = HostedTool::try_from(raw("haskell")).unwrap();
        let request = request_for_tool(&handler, ToolArguments::Raw("literal".into())).unwrap();
        assert_eq!(request.tool_call().unwrap().name, "haskell");
        assert!(request.cell_source().is_none());
        assert!(project_tools(vec![raw("haskell")]).is_ok());
    }

    #[test]
    fn hosted_tool_surfaces_use_the_catalog_verbatim() {
        assert_eq!(
            haskell_tool_instructions(),
            PromptId::HaskellToolInstructions.body()
        );
        assert_eq!(crate::status_tool::declaration().name(), "status");
    }
}
