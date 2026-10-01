use std::sync::Arc;

use exomonad_actor::{
    ActorRef, HostedCheckpointCapture, LocalResidentInstallation, ResidentToolEndpoint,
    ResidentToolError, ResidentToolFuture,
};
use exomonad_tool::{HostedTool, ToolArguments, ToolInvocation, ToolInvocationContext};
use serde_json::{json, Value};

/// The published transport entry for an actor. Each model request pins its
/// current handler and source through `request_snapshot`.
pub(super) struct EmbeddedPolicyInstallation {
    actor: ActorRef,
    policy: Arc<dyn ResidentToolEndpoint>,
    manifest: Result<Arc<harness::embedding::EmbeddedToolManifest>, String>,
}

impl EmbeddedPolicyInstallation {
    pub(super) fn from_installation(installation: &LocalResidentInstallation) -> Self {
        Self::new(installation.actor.identity(), installation.policy.clone())
    }

    fn new(actor: ActorRef, policy: Arc<dyn ResidentToolEndpoint>) -> Self {
        let manifest = harness::embedding::EmbeddedToolManifest::new(project_tools(policy.tools()))
            .map(Arc::new)
            .map_err(|error| error.to_string());
        Self {
            actor,
            policy,
            manifest,
        }
    }

    pub(super) fn actor(&self) -> ActorRef {
        self.actor
    }

    pub(super) fn complete(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        self.policy.complete_boxed(boundary)
    }

    pub(super) fn request_snapshot(&self) -> Result<EmbeddedPolicySnapshot, ResidentToolError> {
        let policy = self.policy.snapshot_for_request()?;
        let declared = self.policy.tools();
        let issued = policy.tools();
        if !std::ptr::eq(declared, issued) && declared != issued {
            return Err(ResidentToolError::Unavailable(
                "request endpoint changed its installed tool declarations".into(),
            ));
        }
        let manifest = self
            .manifest
            .as_ref()
            .map_err(|error| ResidentToolError::Unavailable(error.clone()))?
            .clone();
        Ok(EmbeddedPolicySnapshot {
            #[cfg(test)]
            actor: self.actor,
            policy,
            manifest,
        })
    }
}

/// One issued request keeps the manifest and exact actor-owned handler/source
/// lease together. The opaque endpoint refuses unsupported snapshots.
pub(super) struct EmbeddedPolicySnapshot {
    #[cfg(test)]
    actor: ActorRef,
    policy: Arc<dyn ResidentToolEndpoint>,
    manifest: Arc<harness::embedding::EmbeddedToolManifest>,
}

impl EmbeddedPolicySnapshot {
    #[cfg(test)]
    pub(super) fn actor(&self) -> ActorRef {
        self.actor
    }

    pub(super) fn tools(&self) -> &[Value] {
        self.manifest.tools()
    }

    pub(super) fn manifest(&self) -> Arc<harness::embedding::EmbeddedToolManifest> {
        self.manifest.clone()
    }

    pub(super) fn dispatch(
        &self,
        name: String,
        arguments: ToolArguments,
        context: ToolInvocationContext,
        checkpoint_capture: Option<Arc<dyn HostedCheckpointCapture>>,
    ) -> ResidentToolFuture {
        self.policy.dispatch_with_checkpoint_boxed(
            ToolInvocation {
                context: Some(context),
                name,
                arguments,
            },
            checkpoint_capture,
        )
    }

    pub(super) async fn cancel(
        &self,
        context: ToolInvocationContext,
    ) -> Result<exomonad_actor::WorkbenchCancellationOutcome, ResidentToolError> {
        self.policy.cancel_workbench_boxed(context).await
    }
}

fn project_tools(tools: &[HostedTool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| match tool {
            HostedTool::Function(declaration) => json!({
                "type": "function",
                "name": declaration.name,
                "description": declaration.description,
                "parameters": declaration.input_schema,
                "strict": true,
            }),
            HostedTool::Custom(declaration) => json!({
                "type": "custom",
                "name": declaration.name,
                "description": declaration.description,
                "format": { "type": "text" },
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use exomonad_tool::{CustomToolDeclaration, ToolDeclaration, ToolKind};
    use std::future::ready;

    struct ProbePolicy {
        tools: Vec<HostedTool>,
        marker: &'static str,
        request_policy: Option<Arc<dyn ResidentToolEndpoint>>,
    }

    impl ResidentToolEndpoint for ProbePolicy {
        fn snapshot_for_request(&self) -> Result<Arc<dyn ResidentToolEndpoint>, ResidentToolError> {
            Ok(self
                .request_policy
                .clone()
                .unwrap_or_else(|| policy(self.marker)))
        }
        fn tools(&self) -> &[HostedTool] {
            &self.tools
        }

        fn instructions(&self) -> Option<&str> {
            None
        }

        fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
            let marker = self.marker;
            Box::pin(ready(Ok(json!({
                "marker": marker,
                "name": invocation.name,
                "context": invocation.context.as_ref().map(|context| context.call_id.as_str()),
                "arguments": match invocation.arguments {
                    ToolArguments::Raw(raw) => json!({"raw": raw}),
                    ToolArguments::Structured(value) => json!({"structured": value}),
                },
            }))))
        }
    }

    fn policy(marker: &'static str) -> Arc<dyn ResidentToolEndpoint> {
        policy_with_tools(marker, declared_tools())
    }

    fn declared_tools() -> Vec<HostedTool> {
        vec![
            HostedTool::Custom(CustomToolDeclaration {
                name: "haskell".into(),
                description: "Run a notebook cell".into(),
            }),
            HostedTool::Function(ToolDeclaration {
                name: "lookup".into(),
                description: "Look up a value".into(),
                input_schema: json!({"type": "object", "properties": {}}),
                output_schema: None,
                kind: ToolKind::Call,
            }),
        ]
    }

    fn policy_with_tools(
        marker: &'static str,
        tools: Vec<HostedTool>,
    ) -> Arc<dyn ResidentToolEndpoint> {
        Arc::new(ProbePolicy {
            marker,
            tools,
            request_policy: None,
        })
    }

    fn reloading_policy() -> Arc<dyn ResidentToolEndpoint> {
        let pinned = policy("issued-handler");
        Arc::new(ProbePolicy {
            tools: declared_tools(),
            marker: "stale-handler",
            request_policy: Some(pinned),
        })
    }

    fn context() -> ToolInvocationContext {
        ToolInvocationContext::external(
            "embedded-run".into(),
            "request-1".into(),
            "call-1".into(),
            None,
            Some("embedded".into()),
        )
    }

    #[test]
    fn installed_policy_projects_raw_and_strict_function_tools() {
        let snapshot = EmbeddedPolicyInstallation::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            policy("first"),
        )
        .request_snapshot()
        .unwrap();
        assert_eq!(
            snapshot.actor(),
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7))
        );
        assert_eq!(
            snapshot.tools()[0],
            json!({
                "type": "custom",
                "name": "haskell",
                "description": "Run a notebook cell",
                "format": {"type": "text"},
                "async": true,
            })
        );
        assert_eq!(
            snapshot.tools()[1],
            json!({
                "type": "function",
                "name": "lookup",
                "description": "Look up a value",
                "parameters": {"type": "object", "properties": {}},
                "strict": true,
                "async": true,
            })
        );
    }

    #[test]
    fn requests_share_the_installed_manifest_and_refuse_changed_declarations() {
        let actor = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7));
        let installation = EmbeddedPolicyInstallation::new(actor, policy("first"));
        let first = installation.request_snapshot().unwrap();
        let second = installation.request_snapshot().unwrap();
        assert!(Arc::ptr_eq(&first.manifest, &second.manifest));
        let incompatible = Arc::new(ProbePolicy {
            tools: declared_tools(),
            marker: "original",
            request_policy: Some(policy_with_tools("changed", vec![])),
        });
        assert!(matches!(
            EmbeddedPolicyInstallation::new(actor, incompatible).request_snapshot(),
            Err(ResidentToolError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn request_manifest_and_handler_come_from_the_same_reloaded_endpoint() {
        let snapshot = EmbeddedPolicyInstallation::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            reloading_policy(),
        )
        .request_snapshot()
        .unwrap();

        assert_eq!(snapshot.tools().len(), 2);
        assert_eq!(snapshot.tools()[0]["name"], "haskell");
        let result = snapshot
            .dispatch(
                "haskell".into(),
                ToolArguments::Raw("1".into()),
                context(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result["marker"], "issued-handler");
        assert_eq!(result["name"], "haskell");
    }

    #[tokio::test]
    async fn captured_transport_dispatches_both_input_kinds_after_new_installation() {
        let actor = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7));
        let first = EmbeddedPolicyInstallation::new(actor, policy("first"))
            .request_snapshot()
            .unwrap();
        let second = EmbeddedPolicyInstallation::new(actor, policy("second"))
            .request_snapshot()
            .unwrap();
        let raw = first
            .dispatch(
                "haskell".into(),
                ToolArguments::Raw("λ = 1".into()),
                context(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            raw,
            json!({
                "marker": "first", "name": "haskell", "context": "call-1",
                "arguments": {"raw": "λ = 1"},
            })
        );
        let structured = second
            .dispatch(
                "lookup".into(),
                ToolArguments::Structured(json!({"key": "x"})),
                context(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            structured,
            json!({
                "marker": "second", "name": "lookup", "context": "call-1",
                "arguments": {"structured": {"key": "x"}},
            })
        );
        assert!(matches!(
            first
                .dispatch(
                    "haskell".into(),
                    ToolArguments::Raw("again".into()),
                    context(),
                    None,
                )
                .await,
            Ok(value) if value["marker"] == "first"
        ));
    }
}
