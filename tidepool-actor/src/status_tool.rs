//! Model-facing contract for the actor-local `status` tool.

use serde::Deserialize;
use tidepool_tool::{HostedTool, ToolDeclaration, ToolKind};

pub(crate) const STATUS_TOOL: &str = "status";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StatusView {
    #[default]
    Summary,
    Detailed,
    Recovery,
    Lineage,
    Trace,
    Bindings,
    /// Collectors and the command jobs they watch (finished or not);
    /// bindings with the session generation that defines them, the exact
    /// source of their defining cell, and the execution id that submitted
    /// it. A view over data the actor and workbench already retain — see
    /// `resident_actor::live_status_text`.
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusDiscovery {
    Recovery,
    Bindings,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArguments {
    #[serde(default)]
    view: StatusView,
}

#[derive(Debug, thiserror::Error)]
#[error("status arguments must be an object with optional `view` (summary, detailed, recovery, lineage, trace, bindings, live): {0}")]
pub(crate) struct StatusInputError(String);

pub(crate) fn declaration() -> HostedTool {
    HostedTool::Function(ToolDeclaration {
        name: STATUS_TOOL.into(),
        description: "Inspect this actor and its workbench. Example: {\"view\":\"recovery\"}. Omit view for a compact summary; use detailed, lineage, trace, bindings, or live for other perspectives.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "view": {
                    "type": "string",
                    "enum": ["summary", "detailed", "recovery", "lineage", "trace", "bindings", "live"]
                }
            },
            "additionalProperties": false
        }),
        output_schema: None,
        kind: ToolKind::Call,
    })
}

pub(crate) fn parse(arguments: serde_json::Value) -> Result<StatusView, StatusInputError> {
    serde_json::from_value::<StatusArguments>(arguments)
        .map(|arguments| arguments.view)
        .map_err(|error| StatusInputError(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_advertises_canonical_object_and_all_views() {
        let HostedTool::Function(tool) = declaration() else {
            panic!("status must be a function tool");
        };
        assert_eq!(tool.name, STATUS_TOOL);
        assert_eq!(tool.input_schema["type"], "object");
        assert_eq!(tool.input_schema["additionalProperties"], false);
        assert_eq!(
            tool.input_schema["properties"]["view"]["enum"],
            serde_json::json!(["summary", "detailed", "recovery", "lineage", "trace", "bindings", "live"])
        );
        assert!(tool.input_schema.get("required").is_none());
    }

    #[test]
    fn parser_defaults_and_rejects_invalid_arguments() {
        assert_eq!(parse(serde_json::json!({})).unwrap(), StatusView::Summary);
        for (name, expected) in [
            ("summary", StatusView::Summary),
            ("detailed", StatusView::Detailed),
            ("recovery", StatusView::Recovery),
            ("lineage", StatusView::Lineage),
            ("trace", StatusView::Trace),
            ("bindings", StatusView::Bindings),
            ("live", StatusView::Live),
        ] {
            assert_eq!(parse(serde_json::json!({"view": name})).unwrap(), expected);
        }
        for value in [
            serde_json::json!("summary"),
            serde_json::json!({"view": "missing"}),
            serde_json::json!({"view": null}),
            serde_json::json!({"extra": true}),
        ] {
            assert!(parse(value).is_err());
        }
    }
}
