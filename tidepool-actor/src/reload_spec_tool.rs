//! Model-facing contract for the actor-local `reload_agent_spec` tool.
//!
//! Reload is explicit and never happens on file save: an agent edits its spec
//! with ordinary file tools and then asks for it. The ask is scoped to the
//! actor that made it and never upgrades a child.

use serde::Deserialize;
use tidepool_tool::{HostedTool, ToolDeclaration, ToolKind};

pub(crate) const RELOAD_SPEC_TOOL: &str = "reload_agent_spec";

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReloadSpecArguments {
    /// Modules to pull into the reload's checked closure beyond the configured
    /// list and the spec module itself. Widening is safe; narrowing is what
    /// produces surprising partial activation, so only widening is offered.
    #[serde(default)]
    pub(crate) also_check: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("reload_agent_spec arguments must be an object with an optional `also_check` list of module names: {0}")]
pub(crate) struct ReloadSpecInputError(String);

pub(crate) fn declaration() -> HostedTool {
    HostedTool::Function(ToolDeclaration {
        name: RELOAD_SPEC_TOOL.into(),
        description: "Rebuild YOUR OWN agent spec from the source in your own checkout, and serve later tool calls from the rebuilt implementations. Publishes your checkout's source layer first, then recompiles the spec against it. The DECLARED surface must not change: if the rebuilt spec declares a different tool name, description, schema, kind or order, the reload is refused, the difference is returned, and the previously installed record keeps answering — a changed surface takes effect at your next incarnation, because the tool list is registered once per session. A spec that does not typecheck is an ordinary refusal too: your edited files stay exactly as you wrote them and the previous spec stays active. A call already accepted keeps the implementation it started with. This reload is yours alone and never upgrades a child.".into(),
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

pub(crate) fn parse(
    arguments: serde_json::Value,
) -> Result<ReloadSpecArguments, ReloadSpecInputError> {
    serde_json::from_value::<ReloadSpecArguments>(arguments)
        .map_err(|error| ReloadSpecInputError(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_advertises_an_object_with_one_optional_widening_list() {
        let HostedTool::Function(tool) = declaration() else {
            panic!("reload_agent_spec must be a function tool");
        };
        assert_eq!(tool.name, RELOAD_SPEC_TOOL);
        assert_eq!(tool.input_schema["type"], "object");
        assert_eq!(tool.input_schema["additionalProperties"], false);
        assert!(tool.input_schema.get("required").is_none());
        assert_eq!(
            parse(serde_json::json!({})).unwrap(),
            ReloadSpecArguments::default()
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
