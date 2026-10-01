use std::{collections::HashMap, sync::Arc};

use harness::finalize::FunctionToolSchema;

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
    projection: Result<Arc<EmbeddedToolProjection>, String>,
}

impl EmbeddedPolicyInstallation {
    pub(super) fn from_installation(installation: &LocalResidentInstallation) -> Self {
        Self::new(installation.actor.identity(), installation.policy.clone())
    }

    fn new(actor: ActorRef, policy: Arc<dyn ResidentToolEndpoint>) -> Self {
        let projection = project_tools(policy.tools()).map(Arc::new);
        Self {
            actor,
            policy,
            projection,
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
        let projection = self
            .projection
            .as_ref()
            .map_err(|error| ResidentToolError::Unavailable(error.clone()))?
            .clone();
        Ok(EmbeddedPolicySnapshot {
            #[cfg(test)]
            actor: self.actor,
            policy,
            manifest: projection.manifest.clone(),
            schemas: projection.schemas.clone(),
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
    schemas: Arc<HashMap<String, FunctionToolSchema>>,
}

struct EmbeddedToolProjection {
    manifest: Arc<harness::embedding::EmbeddedToolManifest>,
    schemas: Arc<HashMap<String, FunctionToolSchema>>,
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
        let arguments = match arguments {
            ToolArguments::Structured(value) => {
                let decoded = match self.schemas.get(&name) {
                    Some(schema) => match schema.decode_arguments(value) {
                        Ok(value) => value,
                        Err(error) => {
                            return Box::pin(async move {
                                Err(ResidentToolError::InvalidInvocation(error.to_string()))
                            })
                        }
                    },
                    None => value,
                };
                ToolArguments::Structured(decoded)
            }
            raw => raw,
        };
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

fn project_tools(tools: &[HostedTool]) -> Result<EmbeddedToolProjection, String> {
    let mut projected = Vec::with_capacity(tools.len());
    let mut schemas = HashMap::new();
    for tool in tools {
        projected.push(match tool {
            HostedTool::Function(declaration) => {
                let schema =
                    FunctionToolSchema::new(declaration.input_schema.clone()).map_err(|error| {
                        format!("invalid tool schema for {}: {error}", declaration.name)
                    })?;
                let tool = json!({
                    "type": "function",
                    "name": declaration.name,
                    "description": declaration.description,
                    "parameters": schema.parameters(),
                    "strict": true,
                });
                schemas.insert(declaration.name.clone(), schema);
                tool
            }
            HostedTool::Custom(declaration) => json!({
                "type": "custom",
                "name": declaration.name,
                "description": declaration.description,
                "format": { "type": "text" },
            }),
        });
    }
    let manifest = harness::embedding::EmbeddedToolManifest::new(projected)
        .map_err(|error| error.to_string())?;
    Ok(EmbeddedToolProjection {
        manifest: Arc::new(manifest),
        schemas: Arc::new(schemas),
    })
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
                .unwrap_or_else(|| policy_with_tools(self.marker, self.tools.clone())))
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
                "parameters": {"type": "object", "properties": {}, "required": [], "additionalProperties": false},
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
    async fn installed_builtin_tools_project_strict_schemas_and_preserve_host_omission_defaults() {
        let campaign = super::super::test_campaign::TestCampaign::start().await;
        // Take the real installed declarations, rather than fixtures that copy
        // the three actor-local schemas and can drift from their owners.
        let tools = campaign.root_installation.policy.tools().to_vec();
        let snapshot = EmbeddedPolicyInstallation::new(
            campaign.actor.identity(),
            policy_with_tools("builtin-projection", tools),
        )
        .request_snapshot()
        .unwrap();
        for (name, field) in [
            ("status", "view"),
            ("reload_agent_spec", "also_check"),
            ("reload_helpers", "also_check"),
        ] {
            let declaration = snapshot
                .tools()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap();
            assert_eq!(declaration["strict"], true);
            assert_eq!(declaration["parameters"]["required"], json!([field]));
            assert_eq!(declaration["parameters"]["additionalProperties"], false);
            assert_eq!(
                declaration["parameters"]["properties"][field]["anyOf"][1],
                json!({"type":"null"})
            );
            let result = snapshot
                .dispatch(
                    name.into(),
                    ToolArguments::Structured(json!({(field):null})),
                    context(),
                    None,
                )
                .await
                .unwrap();
            assert_eq!(result["arguments"]["structured"], json!({}));
        }
        let request = harness::transport::ResponsesRequest {
            input: vec![],
            instructions: String::new(),
            tools: snapshot.manifest.tools().clone(),
            tools_allowed: None,
            model: "test-model".into(),
            pinned_effort: harness::model::Effort::Low,
            session_id: "builtin-contract".into(),
        };
        harness::transport::client::request_body(&request).unwrap();
        campaign
            .actor
            .shutdown(exomonad_actor::ActorTerminal {
                kind: exomonad_actor::ActorExitKind::Completed,
                summary: "builtin schema contract checked".into(),
            })
            .await
            .unwrap();
        campaign.hosted.await.unwrap();
    }

    #[test]
    fn unsupported_installed_schema_refuses_request_before_dispatch() {
        let unsupported = HostedTool::Function(ToolDeclaration {
            name: "unsupported".into(),
            description: "Unsupported input constraint".into(),
            input_schema: json!({"type":"object","properties":{"value":{"type":"string","pattern":".*"}}}),
            output_schema: None,
            kind: ToolKind::Call,
        });
        let installation = EmbeddedPolicyInstallation::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            policy_with_tools("never-dispatched", vec![unsupported]),
        );
        assert!(matches!(
            installation.request_snapshot(),
            Err(ResidentToolError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn ambiguous_optional_null_arguments_are_refused_before_host_dispatch() {
        let ambiguous = HostedTool::Function(ToolDeclaration {
            name: "ambiguous".into(),
            description: "Overlapping union input".into(),
            input_schema: json!({
                "type":"object","properties":{"choice":{"anyOf":[
                    {"type":"object","properties":{"value":{"type":"string"}},"required":[]},
                    {"type":"object","properties":{"value":{"type":["string","null"]}},"required":["value"]}
                ]}},"required":["choice"]
            }),
            output_schema: None,
            kind: ToolKind::Call,
        });
        let snapshot = EmbeddedPolicyInstallation::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            policy_with_tools("never-dispatched", vec![ambiguous]),
        )
        .request_snapshot()
        .unwrap();
        assert!(matches!(
            snapshot
                .dispatch(
                    "ambiguous".into(),
                    ToolArguments::Structured(json!({"choice":{"value":null}})),
                    context(),
                    None
                )
                .await,
            Err(ResidentToolError::InvalidInvocation(_))
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
