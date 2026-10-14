//! Actor-local custom-tool projection of the persistent Haskell workbench.

use std::sync::Arc;

use exomonad_tool::{HostedTool, ToolArguments, ToolInvocation};
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
    issued_tools: Option<crate::resident_workbench::ActorRequestSnapshot>,
}

impl ResidentInteractivePolicy {
    /// Project only the reserved status and reload controls of this incarnation.
    /// This constructor does not expose Haskell or other authored AgentSpec tools.
    /// Retain `LocalResidentDeployment::PolicyInstalled`'s installation policy
    /// for authored tools. Native notebook tests use the workbench client's
    /// admission path rather than this controls-only projection.
    ///
    /// All dispatch, completion, reattachment and sealing target this same actor;
    /// construction neither creates a session nor grants additional authority.
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
                crate::status_tool::STATUS_TOOL | crate::reload_tool::RELOAD_TOOL
            )
        });
        Self {
            tools: std::iter::once(crate::status_tool::declaration())
                .chain(std::iter::once(crate::reload_tool::declaration()))
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
                crate::reload_tool::declaration(),
            ]
            .into(),
            client,
            installed_tools: Arc::default(),
            issued_tools: None,
        }
    }
}

fn builtin_request(
    name: &str,
    arguments: ToolArguments,
) -> Result<WorkbenchRequest, ResidentToolError> {
    let ToolArguments::Structured(value @ serde_json::Value::Object(_)) = arguments else {
        return Err(ResidentToolError::InvalidInvocation(format!(
            "invalid argument kind for {name:?}"
        )));
    };
    Ok(WorkbenchRequest::for_tool(name.into(), value))
}

fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        crate::status_tool::STATUS_TOOL | crate::reload_tool::RELOAD_TOOL
    )
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
            .or_else(|| self.installed_tools.snapshot())
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

    fn request_instructions(&self) -> Option<&str> {
        self.issued_tools
            .as_ref()?
            .generation()
            .map(|generation| generation.prompts.instructions())
    }

    fn request_generation(&self) -> Option<tidepool_runtime::session::NotebookGenerationId> {
        self.issued_tools
            .as_ref()?
            .generation()
            .map(|generation| generation.id)
    }

    fn retained_operation(
        &self,
        invocation: exomonad_tool::ToolInvocationContext,
    ) -> Result<crate::HostedOperationSettlement, ResidentToolError> {
        self.client.retained_operation(invocation)
    }

    fn reconcile_workbench_boxed(
        &self,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
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
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> ResidentToolFuture {
        let client = self.client.clone();
        Box::pin(async move { client.complete(boundary).await })
    }

    fn abort_boxed(
        &self,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
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
        let arguments = Ok(invocation.arguments.clone());
        self.dispatch_validated_with_context_boxed(invocation, arguments, capture, context)
    }

    fn dispatch_validated_with_context_boxed(
        &self,
        invocation: ToolInvocation,
        arguments: Result<ToolArguments, ResidentToolError>,
        capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
        context: Option<Arc<dyn crate::HostedContextBinding>>,
    ) -> ResidentToolDispatchFuture {
        let client = self.client.clone();
        let installed_tools = self
            .issued_tools
            .clone()
            .map(Ok)
            .unwrap_or_else(|| self.installed_tools.snapshot_for_call());
        Box::pin(async move {
            let issued = client.issue_workbench_call(invocation.context.clone());
            let validated = (|| {
                let arguments = arguments?;
                if is_builtin(&invocation.name) {
                    return Ok(crate::ActorWorkbenchInvocation::issued(
                        builtin_request(&invocation.name, arguments)?,
                        installed_tools.ok(),
                        capture,
                        context,
                    ));
                }
                let lease = installed_tools?;
                let selected = lease.select(&invocation.name).ok_or_else(|| {
                    ResidentToolError::InvalidInvocation(format!(
                        "unknown actor tool {:?}",
                        invocation.name
                    ))
                })?;
                let admitted = selected.admit(arguments)?;
                Ok(crate::ActorWorkbenchInvocation::installed(admitted, Default::default(), capture, context))
            })();
            let prepared = match validated {
                Ok(validated) => validated,
                Err(error) => {
                    client.reject_workbench_call(&issued, &error);
                    return Err(error);
                }
            };
            client
                .dispatch_prepared_workbench(prepared, issued)
                .await
                .map(ResidentToolResponse::Workbench)
        })
    }

    fn cancel_workbench_boxed(
        &self,
        invocation: exomonad_tool::ToolInvocationContext,
        cause: crate::WorkbenchCancellationCause,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<crate::WorkbenchCancellationOutcome, crate::ResidentToolError>,
                > + Send
                + 'static,
        >,
    > {
        let client = self.client.clone();
        Box::pin(async move { client.cancel_workbench(invocation, cause).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_have_an_explicit_route_separate_from_installed_endpoints() {
        for builtin in [
            crate::status_tool::declaration(),
            crate::reload_tool::declaration(),
        ] {
            assert!(is_builtin(builtin.name()));
            let request = builtin_request(
                builtin.name(),
                ToolArguments::Structured(serde_json::json!({})),
            )
            .unwrap();
            assert_eq!(request.tool_call().unwrap().name, builtin.name());
            assert!(builtin_request(builtin.name(), ToolArguments::Raw("source".into())).is_err());
        }
        assert!(!is_builtin("haskell"));
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
