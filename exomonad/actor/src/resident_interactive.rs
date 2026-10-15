//! Actor-local custom-tool projection of the persistent Haskell workbench.

use std::sync::Arc;

use exomonad_tool::{HostedTool, ToolArguments, ToolInvocation};
#[cfg(test)]
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
    reply_activation: Option<Arc<crate::RequestReplyActivation>>,
    declarations: Arc<parking_lot::Mutex<Option<RequestDeclarations>>>,
}

struct RequestDeclarations {
    installed: Arc<[HostedTool]>,
    reply: Option<Arc<crate::RequestReplyActivation>>,
    exposed: Arc<[HostedTool]>,
}

impl ResidentInteractivePolicy {
    /// Project only the reserved status control of this incarnation.
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
        let custom = tools
            .into_iter()
            .filter(|tool| !matches!(tool.name(), crate::status_tool::STATUS_TOOL));
        Self {
            tools: std::iter::once(crate::status_tool::declaration())
                .chain(custom)
                .collect::<Vec<_>>()
                .into(),
            client: ResidentToolClient::local(actor),
            installed_tools,
            issued_tools: None,
            reply_activation: None,
            declarations: Arc::default(),
        }
    }

    fn with_client(client: ResidentToolClient) -> Self {
        Self {
            tools: vec![crate::status_tool::declaration()].into(),
            client,
            installed_tools: Arc::default(),
            issued_tools: None,
            reply_activation: None,
            declarations: Arc::default(),
        }
    }
}

fn is_builtin(name: &str) -> bool {
    matches!(name, crate::status_tool::STATUS_TOOL)
}

fn haskell_tool_instructions() -> &'static str {
    PromptId::HaskellToolInstructions.body()
}

impl ResidentToolEndpoint for ResidentInteractivePolicy {
    fn request_reply_activation(&self) -> Option<Arc<crate::RequestReplyActivation>> {
        self.reply_activation.clone()
    }
    fn argument_stage(&self, name: &str) -> crate::HostedInputStage {
        if self
            .reply_activation
            .as_ref()
            .and_then(|reply| reply.select(name))
            .is_some()
        {
            crate::HostedInputStage::ProviderWire
        } else {
            crate::HostedInputStage::HostArguments
        }
    }
    fn invocation_role(&self, name: &str) -> crate::HostedInvocationRole {
        if self
            .reply_activation
            .as_ref()
            .and_then(|reply| reply.select(name))
            .is_some()
        {
            crate::HostedInvocationRole::Settlement
        } else if is_builtin(name) {
            crate::HostedInvocationRole::Observation
        } else {
            crate::HostedInvocationRole::Domain
        }
    }

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
        let reply_activation = if self.issued_tools.is_some() {
            self.reply_activation.clone()
        } else {
            self.installed_tools.active_reply()
        };
        let tools = if let Some(installed) = issued_tools.installation().tools() {
            let installed = installed.declaration_snapshot();
            let mut cached = self.declarations.lock();
            if let Some(cached) = cached.as_ref().filter(|cached| {
                Arc::ptr_eq(&cached.installed, &installed)
                    && match (&cached.reply, &reply_activation) {
                        (None, None) => true,
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        _ => false,
                    }
            }) {
                cached.exposed.clone()
            } else {
                let exposed: Arc<[HostedTool]> = std::iter::once(crate::status_tool::declaration())
                    .chain(installed.iter().cloned())
                    .chain(
                        reply_activation
                            .iter()
                            .flat_map(|reply| reply.declarations().to_vec()),
                    )
                    .collect::<Vec<_>>()
                    .into();
                *cached = Some(RequestDeclarations {
                    installed,
                    reply: reply_activation.clone(),
                    exposed: exposed.clone(),
                });
                exposed
            }
        } else {
            self.tools.clone()
        };
        Ok(Arc::new(Self {
            tools,
            client: self.client.clone(),
            installed_tools: self.installed_tools.clone(),
            issued_tools: Some(issued_tools),
            reply_activation,
            declarations: self.declarations.clone(),
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

    fn declaration_snapshot(&self) -> Arc<[HostedTool]> {
        self.tools.clone()
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
        let reply_activation = if self.issued_tools.is_some() {
            self.reply_activation.clone()
        } else {
            self.installed_tools.active_reply()
        };
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
                    return Ok(crate::ActorWorkbenchInvocation::inspection(
                        crate::status_tool::inspection_request(arguments)?,
                        installed_tools.ok(),
                        capture,
                        context,
                    ));
                }
                let lease = installed_tools?;
                let selected = reply_activation
                    .as_ref()
                    .and_then(|reply| reply.select(&invocation.name))
                    .or_else(|| lease.select(&invocation.name))
                    .ok_or_else(|| {
                        ResidentToolError::InvalidInvocation(format!(
                            "unknown actor tool {:?}",
                            invocation.name
                        ))
                    })?;
                let admitted = selected.admit(arguments)?;
                Ok(crate::ActorWorkbenchInvocation::installed(
                    admitted,
                    Default::default(),
                    capture,
                    context,
                ))
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
        for builtin in [crate::status_tool::declaration()] {
            assert!(is_builtin(builtin.name()));
            let request = crate::status_tool::inspection_request(ToolArguments::Structured(
                serde_json::json!({}),
            ))
            .unwrap()
            .into_request();
            assert_eq!(request.tool_call().unwrap().name, builtin.name());
            assert!(
                crate::status_tool::inspection_request(ToolArguments::Raw("source".into()))
                    .is_err()
            );
        }
        assert!(!is_builtin("haskell"));
        assert!(!is_builtin("reload"));
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
