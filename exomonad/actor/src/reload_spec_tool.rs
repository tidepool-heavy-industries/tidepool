//! Model-facing contract for the actor-local `reload_agent_spec` tool.
//!
//! Reload is explicit and never happens on file save: an agent edits its spec
//! with ordinary file tools and then asks for it. The ask is scoped to the
//! actor that made it and never upgrades a child.

use exomonad_tool::{HostedTool, ToolDeclaration, ToolKind};
use serde::Deserialize;

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
        schedule: Default::default(),
        implementation: Default::default(),
        effect_keys: Vec::new(),
        name: RELOAD_SPEC_TOOL.into(),
        description: "Activate edited AgentSpec handler implementations for later calls by this actor. Edit the active source, then call; saving alone does not reload. Optional also_check lists extra modules to typecheck. Two stages: publish checked source, then rebuild and swap the spec. A later spec compilation or surface refusal keeps the previous handlers but DOES NOT undo source publication: cells already see the new source. Read both stages in the receipt. Tool names, descriptions, schemas, kinds, scheduling, implementation kinds, effect profiles and order must match the registered surface; differences require a new actor incarnation. Draft files remain on disk after refusal. Accepted calls keep their original handlers; children are not upgraded. Use reload_helpers for helper-only edits.".into(),
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
