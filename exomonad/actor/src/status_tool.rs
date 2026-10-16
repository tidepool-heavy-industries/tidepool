//! Model-facing contract for the actor-local `status` tool.

use exomonad_tool::{HostedTool, ToolDeclaration, ToolKind};
use serde::Deserialize;

pub(crate) const STATUS_TOOL: &str = "status";

pub(crate) struct StatusInspectionRequest(serde_json::Value);

impl StatusInspectionRequest {
    pub(crate) fn into_request(self) -> crate::WorkbenchRequest {
        crate::WorkbenchRequest::for_tool(STATUS_TOOL.into(), self.0)
    }
}

pub(crate) fn inspection_request(
    arguments: exomonad_tool::ToolArguments,
) -> Result<StatusInspectionRequest, crate::ResidentToolError> {
    let exomonad_tool::ToolArguments::Structured(value @ serde_json::Value::Object(_)) = arguments
    else {
        return Err(crate::ResidentToolError::InvalidInvocation(
            "invalid argument kind for status".into(),
        ));
    };
    Ok(StatusInspectionRequest(value))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StatusView {
    /// Only the roster rows whose rendered line changed since this caller's
    /// previous summary-family status call, then one `N unchanged since`
    /// line. The default when `view` is omitted; with no previous call it
    /// renders the full summary.
    #[default]
    Changed,
    /// Every roster row. `all` names the same view.
    #[serde(alias = "all")]
    Summary,
    Detailed,
    /// The revision identities this actor works against: operator checkout,
    /// its own checked head, its assignment base, live children's checked
    /// heads, and the installed source layer against disk. Also part of
    /// `detailed`.
    Revisions,
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
    /// Every retained watch (id, label, state, registered/transitioned
    /// times) and every response still pending, so a model can see what
    /// it is waiting on without compiling a `pollWatch` cell. Settled
    /// values still require `pollWatch`/`pollResponse`.
    Watches,
    Operation,
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
    operation: Option<exomonad_tool::OriginalOperation>,
    after: Option<OperationReferenceCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OperationReferenceStage {
    Unfinalized,
    Retained,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperationReferenceCursor {
    pub stage: OperationReferenceStage,
    pub operation: exomonad_tool::OriginalOperation,
}

impl OperationReferenceCursor {
    pub(crate) fn key(&self) -> (bool, &exomonad_tool::OriginalOperation) {
        (
            self.stage == OperationReferenceStage::Retained,
            &self.operation,
        )
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "status arguments must be an object with optional `view` (changed, summary, all, revisions, detailed, recovery, lineage, trace, bindings, live, watches, operation): {0}"
)]
pub(crate) struct StatusInputError(String);

fn operation_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "origin": {"type":"object", "properties": {
                "kind":{"type":"string","enum":["embedded"]},
                "run":{"type":"string"}, "actor":{"type":"string"}, "incarnation":{"type":"string"}
            }, "required":["kind","run","actor","incarnation"], "additionalProperties":false},
            "request_id":{"type":"string"}, "call_id":{"type":"string"}
        }, "required":["origin","request_id","call_id"], "additionalProperties":false
    })
}

pub(crate) fn declaration() -> HostedTool {
    HostedTool::Function(ToolDeclaration {
        schedule: Default::default(),
        implementation: Default::default(),
        effect_keys: Vec::new(),
        name: STATUS_TOOL.into(),
        description: "Inspect this actor and its workbench without running a Haskell cell. Omit view (or use changed) for compact roster rows changed since your last changed/summary/all call; the first call shows the full compact roster. summary/all shows every compact row. Choose {\"view\":\"recovery\"} for replayed declarations and losses after machine replacement; bindings for currently available names; live for retained command jobs and binding provenance; watches for watch states and pending responses; revisions for checkout, assignment and active source identities/drift; detailed for provider health and expanded actor state; lineage for ancestry; trace for provider usage and prompt identity. Use view=operation without operation to discover exact references (unfinalized first; 32 per page; pass next_after as after). Supply an operation reference with full origin, request_id and call_id to read execution, native finalization, durable settlement and delivery independently, without awaiting a notebook cell. Status does not acknowledge watches or retrieve settled values; use pollWatch/pollResponse for those values.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "view": {
                    "type": "string",
                    "enum": ["changed", "summary", "all", "revisions", "detailed", "recovery", "lineage", "trace", "bindings", "live", "watches", "operation"]
                }
, "operation": operation_schema(), "after": {
    "type":"object", "properties": {
        "stage":{"type":"string","enum":["unfinalized","retained"]},
        "operation":operation_schema()
    }, "required":["stage","operation"], "additionalProperties":false
}
            },
            "additionalProperties": false
        }),
        output_schema: None,
        kind: ToolKind::Call,
    })
}

pub(crate) fn parse(arguments: serde_json::Value) -> Result<StatusView, StatusInputError> {
    let arguments = parse_arguments(arguments)?;
    Ok(arguments.view)
}

fn parse_arguments(arguments: serde_json::Value) -> Result<StatusArguments, StatusInputError> {
    let arguments: StatusArguments =
        serde_json::from_value(arguments).map_err(|error| StatusInputError(error.to_string()))?;
    match (&arguments.view, &arguments.operation) {
        (StatusView::Operation, Some(operation))
            if operation.is_complete()
                && matches!(
                    operation.origin,
                    exomonad_tool::ConversationOrigin::Embedded { .. }
                ) => {}
        (StatusView::Operation, None) => {}
        (StatusView::Operation, _) => {
            return Err(StatusInputError(
                "operation view requires the exact embedded origin, request_id and call_id".into(),
            ));
        }
        (_, Some(_)) => {
            return Err(StatusInputError(
                "operation is only valid with view=operation".into(),
            ));
        }
        _ => {}
    }
    if let Some(after) = &arguments.after {
        if arguments.view != StatusView::Operation
            || arguments.operation.is_some()
            || !after.operation.is_complete()
            || !matches!(
                after.operation.origin,
                exomonad_tool::ConversationOrigin::Embedded { .. }
            )
        {
            return Err(StatusInputError(
                "after is only valid for operation discovery and must be a complete reference"
                    .into(),
            ));
        }
    }
    Ok(arguments)
}

pub(crate) fn operation(
    arguments: serde_json::Value,
) -> Result<Option<exomonad_tool::OriginalOperation>, StatusInputError> {
    Ok(parse_arguments(arguments)?.operation)
}

pub(crate) fn operation_after(
    arguments: serde_json::Value,
) -> Result<Option<OperationReferenceCursor>, StatusInputError> {
    Ok(parse_arguments(arguments)?.after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_view_requires_full_identity_and_cannot_select_an_observer() {
        let identity = serde_json::json!({"origin":{"kind":"embedded","run":"run","actor":"/root","incarnation":"1"},"request_id":"request","call_id":"call"});
        let input = serde_json::json!({"view":"operation","operation":identity});
        assert_eq!(parse(input.clone()).unwrap(), StatusView::Operation);
        assert!(operation(input).unwrap().unwrap().is_complete());
        assert_eq!(
            operation(serde_json::json!({"view":"operation"})).unwrap(),
            None
        );
        let cursor = serde_json::json!({"stage":"retained","operation":identity});
        assert!(
            operation_after(serde_json::json!({"view":"operation","after":cursor}))
                .unwrap()
                .is_some()
        );
        for invalid in [
            serde_json::json!({"view":"summary","after":cursor}),
            serde_json::json!({"view":"operation","operation":identity,"after":cursor}),
            serde_json::json!({"view":"operation","after":{"stage":"unknown","operation":identity}}),
            serde_json::json!({"view":"operation","after":{"stage":"retained","operation":{"origin":{"kind":"external","principal":"other"},"request_id":"request","call_id":"call"}}}),
            serde_json::json!({"view":"operation","operation":{"call_id":"call"}}),
            serde_json::json!({"operation":identity}),
            serde_json::json!({"view":"operation","operation":identity,"observer":"/other"}),
            serde_json::json!({"view":"operation","operation":{"origin":{"kind":"embedded","run":"run","actor":"/root","incarnation":""},"request_id":"request","call_id":"call"}}),
        ] {
            assert!(parse(invalid).is_err());
        }
    }

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
            serde_json::json!([
                "changed",
                "summary",
                "all",
                "revisions",
                "detailed",
                "recovery",
                "lineage",
                "trace",
                "bindings",
                "live",
                "watches",
                "operation"
            ])
        );
        assert!(tool.input_schema.get("required").is_none());
    }

    #[test]
    fn parser_defaults_and_rejects_invalid_arguments() {
        assert_eq!(parse(serde_json::json!({})).unwrap(), StatusView::Changed);
        for (name, expected) in [
            ("changed", StatusView::Changed),
            ("summary", StatusView::Summary),
            ("all", StatusView::Summary),
            ("revisions", StatusView::Revisions),
            ("detailed", StatusView::Detailed),
            ("recovery", StatusView::Recovery),
            ("lineage", StatusView::Lineage),
            ("trace", StatusView::Trace),
            ("bindings", StatusView::Bindings),
            ("live", StatusView::Live),
            ("watches", StatusView::Watches),
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
