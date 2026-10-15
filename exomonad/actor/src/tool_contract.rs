//! Validation for the metadata compiled alongside one retained AgentSpec.
//! Authored rows, installed interpreters and resource authority remain separate.

use exomonad_tool::{
    ActorEffectKey, ToolDeclaration, ToolEffectKey, ToolImplementation, ToolKind, ToolScheduling,
};
use std::collections::{BTreeMap, HashSet};

#[cfg(test)]
#[path = "tool_contract/spec_preflight_tests.rs"]
mod spec_preflight_tests;

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

/// Checked metadata without a dispatcher, actor, workspace or resource allocation.
pub(crate) struct ValidatedSpecMetadata {
    pub(crate) plans: Vec<exomonad_tool::EndpointPlan>,
    pub(crate) slots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, tidepool_bridge_derive::ToHaskell)]
pub enum SpecValidationError {
    #[haskell(module = "Tidepool.Effects.Core")]
    #[error("spec preparation failed: {0}")]
    SpecPreparationFailed(String),
    #[haskell(module = "Tidepool.Effects.Core")]
    #[error("invalid spec metadata: {0}")]
    SpecMetadataInvalid(String),
    #[haskell(module = "Tidepool.Effects.Core")]
    #[error("spec effect mismatch: {0}")]
    SpecEffectMismatch(String),
    #[haskell(module = "Tidepool.Effects.Core")]
    #[error("spec interpreter unavailable: {0}")]
    SpecInterpreterUnavailable(String),
    #[haskell(module = "Tidepool.Effects.Core")]
    #[error("invalid spec input schema: {0}")]
    SpecInputSchemaInvalid(String),
    #[haskell(module = "Tidepool.Effects.Core")]
    #[error("spec provider projection invalid: {0}")]
    SpecProviderProjectionInvalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq, tidepool_bridge_derive::ToHaskell)]
pub enum SpecProvenance {
    #[haskell(module = "Tidepool.Effects.Core")]
    SpecProvenanceUnavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq, tidepool_bridge_derive::ToHaskell)]
pub enum SpecProviderValidation {
    #[haskell(module = "Tidepool.Effects.Core")]
    SpecProviderNotChecked,
    #[haskell(module = "Tidepool.Effects.Core")]
    SpecProviderChecked,
}

#[derive(Debug, Clone, PartialEq, Eq, tidepool_bridge_derive::ToHaskell)]
pub enum SpecValidationReceipt {
    #[haskell(module = "Tidepool.Effects.Core")]
    SpecValidationReceipt(
        Vec<String>,
        Vec<String>,
        SpecProvenance,
        SpecProviderValidation,
    ),
}

pub(crate) type ToolSurfaceValidator =
    dyn Fn(&[exomonad_tool::HostedTool]) -> Result<(), String> + Send + Sync;

pub(crate) fn preflight_spec_metadata(
    metadata: serde_json::Value,
    effects: &[ActorEffectKey],
    support: &[ToolEffectKey],
    validate_surface: Option<&ToolSurfaceValidator>,
) -> Result<SpecValidationReceipt, SpecValidationError> {
    let mut unique = HashSet::new();
    for key in effects {
        if !unique.insert(*key) {
            return Err(SpecValidationError::SpecEffectMismatch(
                "child effects contain duplicates".into(),
            ));
        }
        if !support.contains(&ToolEffectKey::Actor(*key)) {
            return Err(SpecValidationError::SpecInterpreterUnavailable(format!(
                "requested interpreter {key:?} is unavailable"
            )));
        }
    }
    let installation = decode_installation(metadata)
        .map_err(|error| SpecValidationError::SpecMetadataInvalid(error.to_string()))?;
    let admitted = admit_spec_metadata(installation, effects, support)?;
    let provider_validation = if let Some(validate) = validate_surface {
        let surface = admitted
            .plans
            .iter()
            .map(|plan| plan.surface())
            .collect::<Vec<_>>();
        validate(&surface).map_err(SpecValidationError::SpecProviderProjectionInvalid)?;
        SpecProviderValidation::SpecProviderChecked
    } else {
        SpecProviderValidation::SpecProviderNotChecked
    };
    Ok(SpecValidationReceipt::SpecValidationReceipt(
        admitted
            .plans
            .iter()
            .map(|plan| plan.name().to_owned())
            .collect(),
        effects.iter().map(|key| format!("{key:?}")).collect(),
        SpecProvenance::SpecProvenanceUnavailable(
            "an arbitrary spec value has no single issued source-module identity".into(),
        ),
        provider_validation,
    ))
}

impl SpecValidationError {
    fn from_contract(failure: ToolContractError) -> Self {
        match failure {
            ToolContractError::Unsupported { .. } => {
                Self::SpecInterpreterUnavailable(failure.to_string())
            }
            ToolContractError::Ungranted { .. }
            | ToolContractError::AsyncContext(_)
            | ToolContractError::CanonicalRow(_) => Self::SpecEffectMismatch(failure.to_string()),
            other => Self::SpecMetadataInvalid(other.to_string()),
        }
    }
}

pub(crate) fn admit_spec_metadata(
    installation: SpecInstallation,
    admitted_base: &[ActorEffectKey],
    installed_support: &[ToolEffectKey],
) -> Result<ValidatedSpecMetadata, SpecValidationError> {
    admit_spec_metadata_for(installation, admitted_base, installed_support, false)
}

pub(crate) fn admit_reply_spec_metadata(
    installation: SpecInstallation,
    installed_support: &[ToolEffectKey],
) -> Result<ValidatedSpecMetadata, SpecValidationError> {
    admit_spec_metadata_for(
        installation,
        &[ActorEffectKey::Replies],
        installed_support,
        true,
    )
}

fn admit_spec_metadata_for(
    installation: SpecInstallation,
    admitted_base: &[ActorEffectKey],
    installed_support: &[ToolEffectKey],
    reply: bool,
) -> Result<ValidatedSpecMetadata, SpecValidationError> {
    if reply
        && (installation.tools.len() != 1
            || !installation.slots.is_empty()
            || !installation.slot_effect_keys.is_empty()
            || installation.tools.iter().any(|tool| {
                tool.name != crate::request_reply_tool::REPLY_TOOL
                    || tool.schedule != ToolScheduling::BeforeNextInference
                    || tool.kind != ToolKind::Call
                    || tool.implementation != ToolImplementation::ResidentHandler
            }))
    {
        return Err(SpecValidationError::SpecMetadataInvalid(
            "request reply installation must contain only its synchronous compiled reply endpoint"
                .into(),
        ));
    }
    let SpecInstallation {
        mut tools,
        slots,
        slot_effect_keys,
    } = installation;
    let error = SpecValidationError::SpecMetadataInvalid;
    validate_installation(
        &tools,
        &slots,
        &slot_effect_keys,
        admitted_base,
        installed_support,
    )
    .map_err(SpecValidationError::from_contract)?;
    // Native notebook declarations select an actor-effect subset. Synchronous
    // invocation capabilities are supplied by the installed scheduling contract.
    for tool in &mut tools {
        if tool.implementation == ToolImplementation::HaskellCell
            && tool.schedule == ToolScheduling::BeforeNextInference
        {
            let subset = tool
                .effect_keys
                .iter()
                .copied()
                .filter(|effect| !effect.is_invocation_only())
                .collect::<Vec<_>>();
            tool.effect_keys = ToolEffectKey::ALL
                .iter()
                .copied()
                .filter(|effect| effect.is_invocation_only())
                .chain(subset)
                .collect();
            validate_row(
                &tool.name,
                tool.schedule,
                &tool.effect_keys,
                admitted_base,
                installed_support,
            )
            .map_err(SpecValidationError::from_contract)?;
        }
    }
    let mut names = HashSet::from([crate::status_tool::STATUS_TOOL.to_string()]);
    if !reply {
        names.insert(crate::request_reply_tool::REPLY_TOOL.into());
    }
    let plans = tools
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
            exomonad_tool::EndpointPlan::admit(tool).map_err(|failure| match failure {
                exomonad_tool::ToolDeclarationError::FunctionInputMustBeObject { .. } => {
                    SpecValidationError::SpecInputSchemaInvalid(failure.to_string())
                }
                other => SpecValidationError::SpecMetadataInvalid(other.to_string()),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ValidatedSpecMetadata { plans, slots })
}

pub(crate) fn admit_installation(
    installation: SpecInstallation,
    admitted_base: &[ActorEffectKey],
    installed_support: &[ToolEffectKey],
    dispatcher_effects: &tidepool_runtime::session::HaskellTypeSource,
) -> Result<AdmittedInstallation, crate::ResidentActorWorkbenchError> {
    admit_installation_for(
        installation,
        admitted_base,
        installed_support,
        dispatcher_effects,
        false,
    )
}

pub(crate) fn admit_reply_installation(
    installation: SpecInstallation,
    installed_support: &[ToolEffectKey],
    dispatcher_effects: &tidepool_runtime::session::HaskellTypeSource,
) -> Result<AdmittedInstallation, crate::ResidentActorWorkbenchError> {
    admit_installation_for(
        installation,
        &[ActorEffectKey::Replies],
        installed_support,
        dispatcher_effects,
        true,
    )
}

fn admit_installation_for(
    installation: SpecInstallation,
    admitted_base: &[ActorEffectKey],
    installed_support: &[ToolEffectKey],
    dispatcher_effects: &tidepool_runtime::session::HaskellTypeSource,
    reply: bool,
) -> Result<AdmittedInstallation, crate::ResidentActorWorkbenchError> {
    let ValidatedSpecMetadata { plans, slots } =
        admit_spec_metadata_for(installation, admitted_base, installed_support, reply).map_err(
            |failure| crate::ResidentActorWorkbenchError::ActorProtocol(failure.to_string()),
        )?;
    let endpoints = plans
        .into_iter()
        .map(|plan| {
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
            AdmittedEndpoint {
                plan,
                execution_effects,
            }
        })
        .collect();
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
    #[error("async tool {0:?} cannot use invocation-only effects")]
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
            canonical.splice(
                0..0,
                ToolEffectKey::ALL
                    .iter()
                    .copied()
                    .filter(|effect| effect.is_invocation_only()),
            );
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
                return Err(ToolContractError::AsyncContext(name.into()));
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
    fn reply_endpoint_is_reserved_and_overlay_admission_is_narrow() {
        let keys = ToolEffectKey::ALL
            .iter()
            .copied()
            .filter(|key| key.is_invocation_only())
            .chain([ToolEffectKey::Actor(ActorEffectKey::Replies)])
            .collect::<Vec<_>>();
        let declaration = ToolDeclaration {
            name: crate::request_reply_tool::REPLY_TOOL.into(),
            description: "Return report".into(),
            input_schema: serde_json::json!({"type":"object","properties":{"result":{"type":"string"}},"required":["result"],"additionalProperties":false}),
            output_schema: None,
            kind: ToolKind::Call,
            schedule: ToolScheduling::BeforeNextInference,
            implementation: ToolImplementation::ResidentHandler,
            effect_keys: keys.clone(),
        };
        let make = |tool| SpecInstallation {
            tools: vec![tool],
            slots: vec![],
            slot_effect_keys: BTreeMap::new(),
        };
        assert!(
            admit_spec_metadata(make(declaration.clone()), &[ActorEffectKey::Replies], &keys)
                .is_err()
        );
        let admitted = admit_reply_spec_metadata(make(declaration.clone()), &keys).unwrap();
        assert_eq!(admitted.plans.len(), 1);
        for mutation in 0..8 {
            let mut changed = make(declaration.clone());
            match mutation {
                0 => changed.tools[0].name = "other".into(),
                1 => changed.tools[0].schedule = ToolScheduling::Async,
                2 => changed.tools[0].kind = ToolKind::Raw,
                3 => changed.tools[0].implementation = ToolImplementation::HaskellCell,
                4 => changed.tools[0]
                    .effect_keys
                    .push(ActorEffectKey::Source.into()),
                5 => changed.slots.push("initialContext".into()),
                6 => changed.tools.push(declaration.clone()),
                _ => changed.tools[0].input_schema = serde_json::json!({"type":"string"}),
            }
            assert!(
                admit_reply_spec_metadata(changed, &keys).is_err(),
                "mutation {mutation}"
            );
        }
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
            assert!(
                admit_installation(
                    make(invalid, vec![], BTreeMap::new()),
                    &[jev],
                    &support,
                    &dispatcher
                )
                .is_err()
            );
        }
        assert!(
            admit_installation(
                make(
                    valid,
                    vec![],
                    BTreeMap::from([("afterTool".into(), vec![jev.into()])])
                ),
                &[jev],
                &support,
                &dispatcher
            )
            .is_err()
        );
    }

    #[test]
    fn installer_metadata_rejects_malformed_slots_and_unknown_effect_keys() {
        assert!(decode_installation(serde_json::json!({"tools":[],"slots":"afterTool"})).is_err());
        assert!(decode_installation(
            serde_json::json!({"tools":[],"slotEffectKeys":{"afterTool":["UnregisteredEffect"]}})
        )
        .is_err());
        let installation=decode_installation(serde_json::json!({"tools":[],"slots":["afterTool"],"slotEffectKeys":{"afterTool":["Jev"]}})).unwrap();
        assert!(
            validate_installation(
                &installation.tools,
                &installation.slots,
                &installation.slot_effect_keys,
                &[ActorEffectKey::Jev],
                &[ActorEffectKey::Jev.into()]
            )
            .is_ok()
        );
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
        assert!(
            validate(
                native(ToolScheduling::BeforeNextInference, vec![context, jev]),
                &[ActorEffectKey::Jev],
                &[context, jev]
            )
            .is_ok()
        );
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
    fn native_sync_notebook_issues_the_catalogue_prefix_over_its_granted_subset() {
        let actor = ActorEffectKey::Console;
        let prefix = ToolEffectKey::ALL
            .iter()
            .copied()
            .filter(|effect| effect.is_invocation_only())
            .collect::<Vec<_>>();
        let support = prefix
            .iter()
            .copied()
            .chain([actor.into()])
            .collect::<Vec<_>>();
        let installation = |keys| SpecInstallation {
            tools: vec![native(ToolScheduling::BeforeNextInference, keys)],
            slots: Vec::new(),
            slot_effect_keys: BTreeMap::new(),
        };
        for keys in [
            vec![actor.into()],
            vec![ToolEffectKey::ContextReadWrite, actor.into()],
        ] {
            let selected = admit_installation(
                installation(keys),
                &[actor],
                &support,
                &"UnusedDispatcher".into(),
            )
            .unwrap();
            let expected = prefix
                .iter()
                .copied()
                .chain([actor.into()])
                .collect::<Vec<_>>();
            assert_eq!(selected.endpoints[0].plan().effect_keys(), expected);
            assert!(
                selected.endpoints[0]
                    .execution_effects()
                    .expression()
                    .contains("Tidepool.Effects.Core.Reload")
            );
        }
        let missing_reload = support
            .iter()
            .copied()
            .filter(|effect| *effect != ToolEffectKey::Reload)
            .collect::<Vec<_>>();
        assert!(
            admit_installation(
                installation(vec![actor.into()]),
                &[actor],
                &missing_reload,
                &"UnusedDispatcher".into()
            )
            .is_err()
        );
        assert!(
            admit_installation(
                installation(vec![ActorEffectKey::Web.into()]),
                &[actor],
                &support,
                &"UnusedDispatcher".into()
            )
            .is_err()
        );
    }

    #[test]
    fn canonical_rows_follow_the_exact_admitted_base() {
        let support = [
            ActorEffectKey::Console.into(),
            ToolEffectKey::ContextReadWrite,
            ToolEffectKey::Reload,
        ];
        let mut async_tool = native(ToolScheduling::Async, vec![ActorEffectKey::Console.into()]);
        async_tool.implementation = ToolImplementation::ResidentHandler;
        validate(async_tool.clone(), &[ActorEffectKey::Console], &support).unwrap();
        let mut sync_tool = async_tool.clone();
        sync_tool.schedule = ToolScheduling::BeforeNextInference;
        sync_tool.effect_keys.splice(
            0..0,
            [ToolEffectKey::ContextReadWrite, ToolEffectKey::Reload],
        );
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
