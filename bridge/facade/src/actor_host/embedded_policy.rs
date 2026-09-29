use std::sync::Arc;

use exomonad_actor::{
    ActorRef, LocalResidentInstallation, ResidentToolEndpoint, ResidentToolFuture,
};
use exomonad_tool::{HostedTool, ToolArguments, ToolInvocation, ToolInvocationContext};
use serde_json::{json, Value};

/// The actor installation supplies declarations and a transport dispatcher.
/// The installed Haskell handler still needs an actor-owned lease at request
/// admission; this snapshot alone does not freeze a later spec reload.
pub(super) struct EmbeddedPolicySnapshot {
    actor: ActorRef,
    policy: Arc<dyn ResidentToolEndpoint>,
    tools: Vec<Value>,
}

impl EmbeddedPolicySnapshot {
    pub(super) fn from_installation(installation: &LocalResidentInstallation) -> Self {
        Self::new(installation.actor.identity(), installation.policy.clone())
    }

    fn new(actor: ActorRef, policy: Arc<dyn ResidentToolEndpoint>) -> Self {
        let tools = project_tools(policy.tools());
        Self {
            actor,
            policy,
            tools,
        }
    }

    #[allow(
        dead_code,
        reason = "the bound harness host consumes these after integration"
    )]
    pub(super) fn actor(&self) -> ActorRef {
        self.actor
    }

    #[allow(
        dead_code,
        reason = "the bound harness host consumes these after integration"
    )]
    pub(super) fn tools(&self) -> &[Value] {
        &self.tools
    }

    #[allow(
        dead_code,
        reason = "the bound harness host consumes these after integration"
    )]
    pub(super) fn dispatch(
        &self,
        name: String,
        arguments: ToolArguments,
        context: ToolInvocationContext,
    ) -> ResidentToolFuture {
        self.policy.dispatch_boxed(ToolInvocation {
            context: Some(context),
            name,
            arguments,
        })
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
    }

    impl ResidentToolEndpoint for ProbePolicy {
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
        Arc::new(ProbePolicy {
            marker,
            tools: vec![
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
            ],
        })
    }

    fn context() -> ToolInvocationContext {
        ToolInvocationContext {
            context_call_id: None,
            thread_id: "embedded-run".into(),
            turn_id: "request-1".into(),
            call_id: "call-1".into(),
            namespace: Some("embedded".into()),
        }
    }

    #[test]
    fn installed_policy_projects_raw_and_strict_function_tools() {
        let snapshot = EmbeddedPolicySnapshot::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            policy("first"),
        );
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
            })
        );
    }

    #[tokio::test]
    async fn captured_transport_dispatches_both_input_kinds_after_new_installation() {
        let actor = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7));
        let first = EmbeddedPolicySnapshot::new(actor, policy("first"));
        let second = EmbeddedPolicySnapshot::new(actor, policy("second"));
        let raw = first
            .dispatch(
                "haskell".into(),
                ToolArguments::Raw("λ = 1".into()),
                context(),
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
            first.dispatch("haskell".into(), ToolArguments::Raw("again".into()), context()).await,
            Ok(value) if value["marker"] == "first"
        ));
    }
}
