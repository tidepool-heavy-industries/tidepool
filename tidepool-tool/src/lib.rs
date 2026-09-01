//! Transport-neutral model tool contracts shared by actor policies, agent
//! backends, and MCP projection.

#![warn(clippy::unwrap_used, clippy::expect_used)]

/// One model-visible tool declaration, independent of who serves it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDeclaration {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    pub kind: ToolKind,
}

/// Actor-policy meaning retained from the Haskell endpoint algebra.
///
/// This class describes resident policy control flow. In particular, `Call`
/// is not an MCP `readOnlyHint`: a call handler may still invoke effects that
/// mutate external state.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    #[default]
    Call,
    Notify,
    Update,
    Finish,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shape_uses_the_model_facing_schema_name() {
        let declaration = ToolDeclaration {
            name: "status".into(),
            description: "Read status".into(),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: Some(serde_json::json!({"type": "object"})),
            kind: ToolKind::Call,
        };
        let wire = serde_json::to_value(&declaration).expect("serialize declaration");
        assert_eq!(wire["inputSchema"], serde_json::json!({"type": "object"}));
        assert!(wire.get("input_schema").is_none());
        assert_eq!(wire["kind"], "call");
        assert_eq!(
            serde_json::from_value::<ToolDeclaration>(wire).expect("decode declaration"),
            declaration
        );
    }
}
