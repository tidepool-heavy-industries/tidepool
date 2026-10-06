//! Model-facing contract for publishing this actor's editable Haskell helpers.

use exomonad_tool::{HostedTool, ToolDeclaration, ToolKind};
use serde::Deserialize;

pub(crate) const RELOAD_HELPERS_TOOL: &str = "reload_helpers";

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReloadHelpersArguments {
    #[serde(default)]
    pub(crate) also_check: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("reload_helpers arguments must be an object with an optional `also_check` list of module names: {0}")]
pub(crate) struct ReloadHelpersInputError(String);

pub(crate) fn declaration() -> HostedTool {
    HostedTool::Function(ToolDeclaration {
        schedule: Default::default(),
        implementation: Default::default(),
        effect_keys: Vec::new(),
        name: RELOAD_HELPERS_TOOL.into(),
        description: "Publish reusable Haskell helpers for this actor's later cells. Edit .exomonad/helpers/SessionHelpers.hs and SessionHelpers.* modules, then call reload_helpers; saving files alone does not activate them. New helper modules are allowed. The source owner typechecks before publication: an invalid draft stays on disk while the last valid revision remains active. Pass {\"also_check\":[\"SessionHelpers.Extra\"]} to widen the checked module set. Read the receipt for the published revision or refusal. Publication is actor-local; existing captured values retain their definitions. For AgentSpec handler changes use reload_agent_spec; this tool does not replace handlers or the registered tool list.".into(),
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
) -> Result<ReloadHelpersArguments, ReloadHelpersInputError> {
    serde_json::from_value::<ReloadHelpersArguments>(arguments)
        .map_err(|error| ReloadHelpersInputError(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_helpers_has_optional_check_modules_and_bounded_description() {
        let HostedTool::Function(tool) = declaration() else {
            panic!("reload_helpers must be a function tool");
        };
        assert!(tool.description.chars().count() <= 1024);
        assert_eq!(tool.input_schema["type"], "object");
        assert_eq!(tool.input_schema["additionalProperties"], false);
        assert_eq!(
            parse(serde_json::json!({})).unwrap(),
            ReloadHelpersArguments::default()
        );
        assert_eq!(
            parse(serde_json::json!({"also_check": ["SessionHelpers.Extra"]}))
                .unwrap()
                .also_check,
            vec!["SessionHelpers.Extra"]
        );
        assert!(parse(serde_json::json!({"unknown": true})).is_err());
    }
}
