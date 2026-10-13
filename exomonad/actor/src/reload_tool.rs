//! Model-facing contract for the actor-local `reload` tool.
//!
//! Reload is explicit and never happens on file save: an agent edits its spec
//! with ordinary file tools and then asks for it. The ask is scoped to the
//! actor that made it and never upgrades a child.

use exomonad_tool::{HostedTool, ToolDeclaration, ToolKind};
use serde::Deserialize;

pub(crate) const RELOAD_TOOL: &str = "reload";

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReloadArguments {
    /// Modules to pull into the reload's checked closure beyond the configured
    /// list and the spec module itself. Widening is safe; narrowing is what
    /// produces surprising partial activation, so only widening is offered.
    #[serde(default)]
    pub(crate) also_check: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "reload arguments must be an object with an optional `also_check` list of module names: {0}"
)]
pub(crate) struct ReloadInputError(String);

pub(crate) fn declaration() -> HostedTool {
    HostedTool::Function(ToolDeclaration {
        schedule: Default::default(),
        implementation: Default::default(),
        effect_keys: Vec::new(),
        name: RELOAD_TOOL.into(),
        description: "Activate edited Haskell source, compatible AgentSpec handlers and workspace prompts together. Saving files alone does not reload. Optional also_check widens the validated module graph. A changed configuration starts an empty notebook; conversation, memory and owned resources persist. An unchanged configuration preserves notebook names. Invalid source, a changed registered tool surface or cancellation before commit preserves the active generation and draft files. Existing accepted requests and children keep their issued generation. Read the typed receipt; publication uncertainty blocks new admission and requires reconciliation.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "also_check": {
                    "type": "array",
                    "items": {"type": "string"}
                }
            },
            "additionalProperties": false
        }),
        output_schema: None,
        kind: ToolKind::Call,
    })
}

pub(crate) fn parse(arguments: serde_json::Value) -> Result<ReloadArguments, ReloadInputError> {
    serde_json::from_value::<ReloadArguments>(arguments)
        .map_err(|error| ReloadInputError(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The provider refuses a tool whose description is longer than this, and
    /// the host then refuses to start. Every tool an actor declares for itself
    /// is checked here, because no hosted test crosses the provider boundary.
    const PROVIDER_DESCRIPTION_LIMIT: usize = 1024;

    #[test]
    fn every_actor_local_tool_fits_the_providers_description_limit() {
        for tool in [declaration(), crate::status_tool::declaration()] {
            let length = tool.description().chars().count();
            assert!(
                length <= PROVIDER_DESCRIPTION_LIMIT,
                "`{}` describes itself in {length} characters; the limit is {PROVIDER_DESCRIPTION_LIMIT}",
                tool.name()
            );
        }
    }

    #[test]
    fn declaration_advertises_an_object_with_one_optional_widening_list() {
        let HostedTool::Function(tool) = declaration() else {
            panic!("reload must be a function tool");
        };
        assert_eq!(tool.name, RELOAD_TOOL);
        assert_eq!(tool.input_schema["type"], "object");
        assert_eq!(tool.input_schema["additionalProperties"], false);
        assert!(tool.input_schema.get("required").is_none());
        assert_eq!(
            parse(serde_json::json!({})).unwrap(),
            ReloadArguments::default()
        );
        assert_eq!(
            parse(serde_json::json!({"also_check": ["Project.Helper"]}))
                .unwrap()
                .also_check,
            vec!["Project.Helper".to_string()]
        );
        assert!(parse(serde_json::json!({"widen": []})).is_err());
    }
}
