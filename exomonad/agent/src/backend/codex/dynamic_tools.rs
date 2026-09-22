//! Hand-rolled `dynamicTools` declaration types.
//!
//! `codex-codes` 0.146.4 does not generate these at all. In the CLI's own
//! source, `ThreadStartParams.dynamic_tools` carries
//! `#[experimental("thread/start.dynamicTools")]`
//! (`codex-rs/app-server-protocol/src/protocol/v2/thread.rs` @ tag
//! `rust-v0.146.0`), and the crate's type generation is driven from the
//! generated JSON Schema, which drops experimental-gated fields — so neither
//! the field nor `DynamicToolSpec`/`DynamicToolFunctionSpec` ever reach it. See
//! `fixtures/app-server-0.146.0/PROTOCOL-NOTES.md` for the full sourcing.
//!
//! Wire shapes below are transcribed from `codex-rs/protocol/src/dynamic_tools.rs`
//! at the same tag, and sent over `codex_codes`'s raw `request()` escape
//! hatch rather than through a typed client method, since no typed method
//! for this field exists to use.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DynamicToolSpec {
    Function(DynamicToolFunctionSpec),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolFunctionSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub defer_loading: bool,
}

/// `thread/start` params, hand-rolled because `dynamicTools` is missing from
/// `codex_codes::ThreadStartParams`. Deliberately narrow — only the fields
/// this adapter needs, not a general-purpose replacement for the typed struct.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartWithDynamicTools {
    pub ephemeral: bool,
    pub dynamic_tools: Vec<DynamicToolSpec>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_spec_matches_the_documented_wire_shape() {
        let spec = DynamicToolSpec::Function(DynamicToolFunctionSpec {
            name: "ask_parent".to_string(),
            description: "Ask the parent.".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            defer_loading: false,
        });
        let value = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "function",
                "name": "ask_parent",
                "description": "Ask the parent.",
                "inputSchema": {"type": "object"}
            }),
            "deferLoading must be omitted when false, per skip_serializing_if"
        );
    }

    #[test]
    fn thread_start_with_dynamic_tools_serializes_camel_case() {
        let params = ThreadStartWithDynamicTools {
            ephemeral: true,
            dynamic_tools: vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: "ask_parent".to_string(),
                description: "d".to_string(),
                input_schema: serde_json::json!({}),
                defer_loading: false,
            })],
        };
        let value = serde_json::to_value(&params).unwrap();
        assert_eq!(value["ephemeral"], serde_json::json!(true));
        assert_eq!(
            value["dynamicTools"][0]["type"],
            serde_json::json!("function")
        );
    }
}
