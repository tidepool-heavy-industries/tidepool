//! Validation for the metadata compiled alongside one retained AgentSpec.
//! Authored rows, installed interpreters and resource authority remain separate.

use exomonad_tool::{
    ActorEffectKey, ToolDeclaration, ToolEffectKey, ToolImplementation, ToolKind, ToolScheduling,
};
use std::collections::{BTreeMap, HashSet};

/// Both products of one source installation are published together.
pub(crate) struct SpecInstallation {
    pub(crate) tools: Vec<ToolDeclaration>,
    pub(crate) slots: Vec<String>,
    pub(crate) slot_effect_keys: BTreeMap<String, Vec<ToolEffectKey>>,
}

pub(crate) fn decode_installation(
    installation: serde_json::Value,
) -> Result<SpecInstallation, crate::ResidentActorWorkbenchError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Wire {
        tools: Vec<ToolDeclaration>,
        #[serde(default)]
        slots: Vec<String>,
        #[serde(default)]
        slot_effect_keys: BTreeMap<String, Vec<ToolEffectKey>>,
    }
    let decode_error = |error| {
        crate::ResidentActorWorkbenchError::ActorProtocol(format!("tool installation: {error}"))
    };
    if installation.is_array() {
        let tools = serde_json::from_value(installation).map_err(decode_error)?;
        return Ok(SpecInstallation {
            tools,
            slots: Vec::new(),
            slot_effect_keys: BTreeMap::new(),
        });
    }
    let Wire {
        tools,
        slots,
        slot_effect_keys,
    } = serde_json::from_value(installation).map_err(decode_error)?;
    Ok(SpecInstallation {
        tools,
        slots,
        slot_effect_keys,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolContractError {
    #[error("duplicate installed tool {0:?}")]
    DuplicateTool(String),
    #[error("tool {tool:?} repeats effect {effect:?}")]
    DuplicateEffect { tool: String, effect: ToolEffectKey },
    #[error("async tool {0:?} cannot use ContextReadWrite")]
    AsyncContext(String),
    #[error("tool {tool:?} requests an ungranted effect {effect:?}")]
    Ungranted {
        tool: String,
        effect: ActorEffectKey,
    },
    #[error("tool {tool:?} requests an unsupported interpreter {effect:?}")]
    Unsupported { tool: String, effect: ToolEffectKey },
    #[error("compiled tool {0:?} must declare its exact canonical compiled row")]
    CanonicalRow(String),
    #[error("native Haskell cell tool {0:?} must have raw text input")]
    NativeInput(String),
    #[error("unsupported installed slot {0:?}")]
    UnknownSlot(String),
    #[error("installed slot metadata differs from the declared slots")]
    SlotMetadata,
}

/// Validate before publishing declarations or the dispatcher root. The base
/// list is the same typed authority used to compile this exact installation.
pub(crate) fn validate_installation(
    tools: &[ToolDeclaration],
    slots: &[String],
    slot_effect_keys: &BTreeMap<String, Vec<ToolEffectKey>>,
    granted_effects: &[ActorEffectKey],
    installed_support: &[ToolEffectKey],
) -> Result<(), ToolContractError> {
    let base: Vec<_> = granted_effects
        .iter()
        .copied()
        .map(ToolEffectKey::Actor)
        .collect();
    let mut names = HashSet::new();
    for tool in tools {
        if !names.insert(&tool.name) {
            return Err(ToolContractError::DuplicateTool(tool.name.clone()));
        }
        if tool.implementation == ToolImplementation::HaskellCell && tool.kind != ToolKind::Raw {
            return Err(ToolContractError::NativeInput(tool.name.clone()));
        }
        let mut canonical = base.clone();
        if tool.schedule == ToolScheduling::BeforeNextInference {
            canonical.insert(0, ToolEffectKey::ContextReadWrite);
        }
        if tool.implementation == ToolImplementation::ResidentHandler
            && tool.effect_keys != canonical
        {
            return Err(ToolContractError::CanonicalRow(tool.name.clone()));
        }
        validate_row(
            &tool.name,
            tool.schedule,
            &tool.effect_keys,
            granted_effects,
            installed_support,
        )?;
    }
    let declared: HashSet<_> = slots.iter().map(String::as_str).collect();
    if declared.len() != slots.len()
        || declared.len() != slot_effect_keys.len()
        || slot_effect_keys
            .keys()
            .any(|slot| !declared.contains(slot.as_str()))
    {
        return Err(ToolContractError::SlotMetadata);
    }
    for slot in slots {
        if slot != "afterTool" {
            return Err(ToolContractError::UnknownSlot(slot.clone()));
        }
        let keys = slot_effect_keys
            .get(slot)
            .ok_or(ToolContractError::SlotMetadata)?;
        if keys != &base {
            return Err(ToolContractError::CanonicalRow(slot.clone()));
        }
        validate_row(
            slot,
            ToolScheduling::Async,
            keys,
            granted_effects,
            installed_support,
        )?;
    }
    Ok(())
}

fn validate_row(
    name: &str,
    scheduling: ToolScheduling,
    keys: &[ToolEffectKey],
    granted: &[ActorEffectKey],
    installed: &[ToolEffectKey],
) -> Result<(), ToolContractError> {
    let mut seen = HashSet::new();
    for &key in keys {
        if !seen.insert(key) {
            return Err(ToolContractError::DuplicateEffect {
                tool: name.into(),
                effect: key,
            });
        }
        match key {
            ToolEffectKey::ContextReadWrite if scheduling == ToolScheduling::Async => {
                return Err(ToolContractError::AsyncContext(name.into()))
            }
            ToolEffectKey::Actor(effect) if !granted.contains(&effect) => {
                return Err(ToolContractError::Ungranted {
                    tool: name.into(),
                    effect,
                })
            }
            _ => {}
        }
        if !installed.contains(&key) {
            return Err(ToolContractError::Unsupported {
                tool: name.into(),
                effect: key,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn native(schedule: ToolScheduling, keys: Vec<ToolEffectKey>) -> ToolDeclaration {
        ToolDeclaration {
            name: "edit".into(),
            description: "Edit".into(),
            input_schema: serde_json::json!({"type":"string"}),
            output_schema: None,
            kind: ToolKind::Raw,
            schedule,
            implementation: ToolImplementation::HaskellCell,
            effect_keys: keys,
        }
    }
    fn validate(
        tool: ToolDeclaration,
        granted: &[ActorEffectKey],
        support: &[ToolEffectKey],
    ) -> Result<(), ToolContractError> {
        validate_installation(&[tool], &[], &BTreeMap::new(), granted, support)
    }
    #[test]
    fn installer_metadata_rejects_malformed_slots_and_unknown_effect_keys() {
        assert!(decode_installation(serde_json::json!({"tools":[],"slots":"afterTool"})).is_err());
        assert!(decode_installation(
            serde_json::json!({"tools":[],"slotEffectKeys":{"afterTool":["UnregisteredEffect"]}})
        )
        .is_err());
        let installation=decode_installation(serde_json::json!({"tools":[],"slots":["afterTool"],"slotEffectKeys":{"afterTool":["Jev"]}})).unwrap();
        assert!(validate_installation(
            &installation.tools,
            &installation.slots,
            &installation.slot_effect_keys,
            &[ActorEffectKey::Jev],
            &[ActorEffectKey::Jev.into()]
        )
        .is_ok());
    }

    #[test]
    fn mode_support_and_authority_are_separate_installation_checks() {
        let context = ToolEffectKey::ContextReadWrite;
        let jev = ToolEffectKey::Actor(ActorEffectKey::Jev);
        assert_eq!(
            validate(
                native(ToolScheduling::Async, vec![context]),
                &[],
                &[context]
            ),
            Err(ToolContractError::AsyncContext("edit".into()))
        );
        assert_eq!(
            validate(
                native(ToolScheduling::BeforeNextInference, vec![context]),
                &[],
                &[]
            ),
            Err(ToolContractError::Unsupported {
                tool: "edit".into(),
                effect: context
            })
        );
        assert_eq!(
            validate(native(ToolScheduling::Async, vec![jev]), &[], &[jev]),
            Err(ToolContractError::Ungranted {
                tool: "edit".into(),
                effect: ActorEffectKey::Jev
            })
        );
        assert!(validate(
            native(ToolScheduling::BeforeNextInference, vec![context, jev]),
            &[ActorEffectKey::Jev],
            &[context, jev]
        )
        .is_ok());
    }
    #[test]
    fn compiled_metadata_must_match_the_same_installer_row() {
        let jev = ActorEffectKey::Jev;
        let mut tool = native(ToolScheduling::Async, vec![]);
        tool.implementation = ToolImplementation::ResidentHandler;
        assert_eq!(
            validate(tool.clone(), &[jev], &[jev.into()]),
            Err(ToolContractError::CanonicalRow("edit".into()))
        );
        tool.effect_keys = vec![jev.into()];
        assert!(validate(tool, &[jev], &[jev.into()]).is_ok());
    }
    #[test]
    fn selected_rows_and_slots_reject_duplicates_and_hidden_context() {
        let context = ToolEffectKey::ContextReadWrite;
        assert_eq!(
            validate(
                native(ToolScheduling::BeforeNextInference, vec![context, context]),
                &[],
                &[context]
            ),
            Err(ToolContractError::DuplicateEffect {
                tool: "edit".into(),
                effect: context
            })
        );
        assert_eq!(
            validate_installation(&[], &["afterTool".into()], &BTreeMap::new(), &[], &[]),
            Err(ToolContractError::SlotMetadata)
        );
        let slot = BTreeMap::from([("afterTool".into(), vec![context])]);
        assert_eq!(
            validate_installation(&[], &["afterTool".into()], &slot, &[], &[context]),
            Err(ToolContractError::CanonicalRow("afterTool".into()))
        );
    }
}
