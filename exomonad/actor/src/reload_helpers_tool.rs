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
        name: RELOAD_HELPERS_TOOL.into(),
        description: "Edit .exomonad/helpers/SessionHelpers.hs (and any SessionHelpers.* modules), then call reload_helpers to typecheck and publish them for later Haskell cells. New modules are allowed. Invalid drafts stay on disk while the last valid revision remains active. Publication is scoped to this actor; it does not change AgentSpec or the registered tool list. Supply also_check to include additional modules in the check.".into(),
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
