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

/// Installation-owned executable entry and its exact compiler row.
#[derive(Debug, Clone)]
pub(crate) struct AdmittedEndpoint {
    plan: exomonad_tool::EndpointPlan,
    execution_effects: tidepool_runtime::session::HaskellTypeSource,
}

#[derive(Debug, Clone)]
pub(crate) struct AfterToolEntry {
    execution_effects: tidepool_runtime::session::HaskellTypeSource,
}

impl AdmittedEndpoint {
    pub(crate) fn plan(&self) -> &exomonad_tool::EndpointPlan {
        &self.plan
    }
    pub(crate) fn execution_effects(&self) -> &tidepool_runtime::session::HaskellTypeSource {
        &self.execution_effects
    }
}

impl AfterToolEntry {
    pub(crate) fn execution_effects(&self) -> &tidepool_runtime::session::HaskellTypeSource {
        &self.execution_effects
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InitialContextEntry {
    pub(crate) execution_effects: tidepool_runtime::session::HaskellTypeSource,
}

pub(crate) struct AdmittedInstallation {
    pub(crate) endpoints: Vec<AdmittedEndpoint>,
    pub(crate) after_tool: Option<AfterToolEntry>,
    pub(crate) initial_context: Option<InitialContextEntry>,
}

pub(crate) fn admit_installation(
    installation: SpecInstallation,
    admitted_base: &[ActorEffectKey],
    installed_support: &[ToolEffectKey],
    dispatcher_effects: &tidepool_runtime::session::HaskellTypeSource,
) -> Result<AdmittedInstallation, crate::ResidentActorWorkbenchError> {
    let SpecInstallation {
        tools,
        slots,
        slot_effect_keys,
    } = installation;
    let error = |error: String| crate::ResidentActorWorkbenchError::ActorProtocol(error);
    validate_installation(
        &tools,
        &slots,
        &slot_effect_keys,
        admitted_base,
        installed_support,
    )
    .map_err(|failure| error(failure.to_string()))?;
    let mut names = HashSet::from([
        crate::status_tool::STATUS_TOOL.to_string(),
        crate::reload_tool::RELOAD_TOOL.to_string(),
    ]);
    let endpoints = tools
        .into_iter()
        .map(|tool| {
            if tool.name.is_empty()
                || tool.name.len() > 64
                || !tool
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_')
                || !names.insert(tool.name.clone())
            {
                return Err(error(format!(
                    "invalid, duplicate or reserved tool name {:?}",
                    tool.name
                )));
            }
            let plan = exomonad_tool::EndpointPlan::admit(tool)
                .map_err(|failure| error(failure.to_string()))?;
            let execution_effects = match plan.execution() {
                exomonad_tool::EndpointExecution::ResidentAsync(_)
                | exomonad_tool::EndpointExecution::ResidentSync(_) => dispatcher_effects.clone(),
                exomonad_tool::EndpointExecution::HaskellAsync
                | exomonad_tool::EndpointExecution::HaskellSync => {
                    let mut imports = tidepool_runtime::session::SourceImports::default();
                    let names = plan
                        .effect_keys()
                        .iter()
                        .map(|key| {
                            let identity = key
                                .nominal_identity()
                                .expect("admission requires a nominal effect owner");
                            imports.extend_text(&format!("qualified {}", identity.module));
                            format!("{}.{}", identity.module, identity.name)
                        })
                        .collect::<Vec<_>>();
                    tidepool_runtime::session::HaskellTypeSource::new(
                        format!("'[{}]", names.join(", ")),
                        imports,
                    )
                }
            };
            Ok(AdmittedEndpoint {
                plan,
                execution_effects,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let after_tool = slots
        .iter()
        .any(|slot| slot == "afterTool")
        .then(|| AfterToolEntry {
            execution_effects: dispatcher_effects.clone(),
        });
    let initial_context =
        slots
            .iter()
            .any(|slot| slot == "initialContext")
            .then(|| InitialContextEntry {
                execution_effects: dispatcher_effects.clone(),
            });
    Ok(AdmittedInstallation {
        endpoints,
        after_tool,
        initial_context,
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
/// is the complete intersection of role grants and actual interpreter support,
/// observed on the machine that compiles this exact installation.
pub(crate) fn validate_installation(
    tools: &[ToolDeclaration],
    slots: &[String],
    slot_effect_keys: &BTreeMap<String, Vec<ToolEffectKey>>,
    admitted_base: &[ActorEffectKey],
    installed_support: &[ToolEffectKey],
) -> Result<(), ToolContractError> {
    let base: Vec<_> = admitted_base
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
            admitted_base,
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
        if slot != "afterTool" && slot != "initialContext" {
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
            admitted_base,
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
            key if key.is_invocation_only() && scheduling == ToolScheduling::Async => {
                return Err(ToolContractError::AsyncContext(name.into()))
            }
            ToolEffectKey::Actor(effect) if !granted.contains(&effect) => {
                return Err(ToolContractError::Ungranted {
                    tool: name.into(),
                    effect,
                });
            }
            _ => {}
        }
        if !installed.contains(&key) || key.nominal_identity().is_none() {
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
    fn installation_issues_only_supported_plans_and_an_optional_typed_slot() {
        let jev = ActorEffectKey::Jev;
        let support = [jev.into(), ToolEffectKey::ContextReadWrite];
        let dispatcher = tidepool_runtime::session::HaskellTypeSource::from("InstallerRow");
        let make = |tool, slots, slot_effect_keys| SpecInstallation {
            tools: vec![tool],
            slots,
            slot_effect_keys,
        };
        let valid = native(ToolScheduling::Async, vec![jev.into()]);
        let installed = admit_installation(
            make(valid.clone(), vec![], BTreeMap::new()),
            &[jev],
            &support,
            &dispatcher,
        )
        .unwrap();
        assert_eq!(installed.endpoints.len(), 1);
        assert!(installed.after_tool.is_none());
        assert_eq!(
            installed.endpoints[0].execution_effects.expression(),
            "'[Tidepool.Effects.Core.Jev]"
        );
        let installed = admit_installation(
            make(
                valid.clone(),
                vec!["afterTool".into()],
                BTreeMap::from([("afterTool".into(), vec![jev.into()])]),
            ),
            &[jev],
            &support,
            &dispatcher,
        )
        .unwrap();
        assert_eq!(installed.after_tool.unwrap().execution_effects, dispatcher);
        for slots in [
            vec![],
            vec!["afterTool"],
            vec!["initialContext"],
            vec!["afterTool", "initialContext"],
        ] {
            let installed = admit_installation(
                make(
                    valid.clone(),
                    slots.iter().map(|slot| (*slot).into()).collect(),
                    slots
                        .iter()
                        .map(|slot| ((*slot).into(), vec![jev.into()]))
                        .collect(),
                ),
                &[jev],
                &support,
                &dispatcher,
            )
            .unwrap();
            assert_eq!(installed.after_tool.is_some(), slots.contains(&"afterTool"));
            assert_eq!(
                installed.initial_context.is_some(),
                slots.contains(&"initialContext")
            );
            if let Some(initial) = installed.initial_context {
                assert_eq!(initial.execution_effects, dispatcher);
            }
        }

        for invalid in [
            ToolDeclaration {
                kind: ToolKind::Call,
                ..valid.clone()
            },
            ToolDeclaration {
                effect_keys: vec![ToolEffectKey::ContextReadWrite],
                ..valid.clone()
            },
            ToolDeclaration {
                name: crate::status_tool::STATUS_TOOL.into(),
                ..valid.clone()
            },
            ToolDeclaration {
                name: "bad-name".into(),
                ..valid.clone()
            },
            ToolDeclaration {
                implementation: ToolImplementation::ResidentHandler,
                effect_keys: vec![],
                ..valid.clone()
            },
        ] {
            assert!(admit_installation(
                make(invalid, vec![], BTreeMap::new()),
                &[jev],
                &support,
                &dispatcher
            )
            .is_err());
        }
        assert!(admit_installation(
            make(
                valid,
                vec![],
                BTreeMap::from([("afterTool".into(), vec![jev.into()])])
            ),
            &[jev],
            &support,
            &dispatcher
        )
        .is_err());
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
    fn legacy_decoded_key_does_not_assert_interpreter_availability() {
        let legacy: ToolEffectKey = serde_json::from_str("\"Forks\"").unwrap();
        for support in [vec![], vec![legacy]] {
            let selected = native(ToolScheduling::Async, vec![legacy]);
            assert!(matches!(
                validate(selected, &[ActorEffectKey::Forks], &support),
                Err(ToolContractError::Unsupported { effect, .. }) if effect == legacy
            ));
        }
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
    #[test]
    fn canonical_rows_follow_the_exact_admitted_base() {
        let support = [
            ActorEffectKey::Console.into(),
            ToolEffectKey::ContextReadWrite,
        ];
        let mut async_tool = native(ToolScheduling::Async, vec![ActorEffectKey::Console.into()]);
        async_tool.implementation = ToolImplementation::ResidentHandler;
        validate(async_tool.clone(), &[ActorEffectKey::Console], &support).unwrap();
        let mut sync_tool = async_tool.clone();
        sync_tool.schedule = ToolScheduling::BeforeNextInference;
        sync_tool
            .effect_keys
            .insert(0, ToolEffectKey::ContextReadWrite);
        validate(sync_tool, &[ActorEffectKey::Console], &support).unwrap();
        // A descriptor compiled against the wider role cannot be paired with
        // the dispatcher compiled against the narrower supported base.
        async_tool.effect_keys.push(ActorEffectKey::Source.into());
        assert!(matches!(
            validate(async_tool, &[ActorEffectKey::Console], &support),
            Err(ToolContractError::CanonicalRow(_))
        ));
        let selected = native(ToolScheduling::Async, vec![ActorEffectKey::Source.into()]);
        assert!(matches!(
            validate(selected, &[ActorEffectKey::Console], &support),
            Err(ToolContractError::Ungranted { .. })
        ));
    }
}
