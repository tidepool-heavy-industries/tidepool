//! Immutable model authority issued from an admitted provider catalog.

use exomonad_tool::{ActorEffectKey, ModelFamily, ModelOperation, ModelPermission};
use serde::{Deserialize, Serialize};
use tidepool_bridge_effects::{ModelGrantRequest, ModelSelectionEnvelope};

use crate::{Backend, Model, Reasoning};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelPermissionError {
    #[error("catalog, model key, provider and model identifiers must be nonempty")]
    EmptyIdentity,
    #[error("model permission family differs from its provider")]
    FamilyMismatch,
    #[error("model permission requires at least one effort")]
    EmptyEfforts,
    #[error("model effort belongs to a different provider family")]
    EffortFamilyMismatch,
    #[error("requested model or effort exceeds parent authority")]
    ExceedsParent,
    #[error("declared model effects differ from model permissions")]
    EffectMismatch,
    #[error("declared effects contain duplicates")]
    DuplicateEffects,
    #[error("catalog model key resolves to conflicting admitted provider identities")]
    ConflictingIdentity,
    #[error("captured model history requires the same admitted catalog, provider and family")]
    IncompatibleHistory,
    #[error("model child admission requires ForkAgent or SpawnAgent")]
    InvalidChildOperation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdmittedModelIdentity {
    catalog_identity: String,
    key: String,
    provider: String,
    model: Model,
}

impl<'de> Deserialize<'de> for AdmittedModelIdentity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            catalog_identity: String,
            key: String,
            provider: String,
            model: Model,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.catalog_identity, wire.key, wire.provider, wire.model)
            .map_err(serde::de::Error::custom)
    }
}

impl AdmittedModelIdentity {
    pub fn new(
        catalog_identity: String,
        key: String,
        provider: String,
        model: Model,
    ) -> Result<Self, ModelPermissionError> {
        if [
            catalog_identity.as_str(),
            key.as_str(),
            provider.as_str(),
            model.value(),
        ]
        .iter()
        .any(|s| s.trim().is_empty())
        {
            return Err(ModelPermissionError::EmptyIdentity);
        }
        Ok(Self {
            catalog_identity,
            key,
            provider,
            model,
        })
    }

    pub fn catalog_identity(&self) -> &str {
        &self.catalog_identity
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn model(&self) -> &Model {
        &self.model
    }
    pub fn family(&self) -> ModelFamily {
        family(self.model.backend())
    }
}

fn family(backend: Backend) -> ModelFamily {
    match backend {
        Backend::OpenAIPlan => ModelFamily::OpenAI,
        Backend::GeminiInteractions => ModelFamily::Argon,
        Backend::DeepSeek => ModelFamily::DeepSeek,
    }
}

fn validate_efforts(
    model: &AdmittedModelIdentity,
    efforts: &[Reasoning],
) -> Result<(), ModelPermissionError> {
    if efforts.is_empty() {
        return Err(ModelPermissionError::EmptyEfforts);
    }
    if efforts
        .iter()
        .any(|e| family(e.backend()) != model.family())
    {
        return Err(ModelPermissionError::EffortFamilyMismatch);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelGrant {
    permission: ModelPermission,
    model: AdmittedModelIdentity,
    efforts: Vec<Reasoning>,
}

impl ModelGrant {
    pub fn new(
        permission: ModelPermission,
        model: AdmittedModelIdentity,
        mut efforts: Vec<Reasoning>,
    ) -> Result<Self, ModelPermissionError> {
        if permission.family != model.family() {
            return Err(ModelPermissionError::FamilyMismatch);
        }
        validate_efforts(&model, &efforts)?;
        deduplicate(&mut efforts);
        Ok(Self {
            permission,
            model,
            efforts,
        })
    }

    pub fn permission(&self) -> ModelPermission {
        self.permission
    }
    pub fn model(&self) -> &AdmittedModelIdentity {
        &self.model
    }
    pub fn efforts(&self) -> &[Reasoning] {
        &self.efforts
    }
}

fn deduplicate<T: PartialEq>(values: &mut Vec<T>) {
    let mut index = 0;
    while index < values.len() {
        if values[..index].contains(&values[index]) {
            values.remove(index);
        } else {
            index += 1;
        }
    }
}

/// Delegation and bounded invocation grants. Absence grants no model operation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ModelPermissions(Vec<ModelGrant>);

impl<'de> Deserialize<'de> for ModelPermissions {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(Vec::<ModelGrant>::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl ModelPermissions {
    pub fn new(grants: Vec<ModelGrant>) -> Result<Self, ModelPermissionError> {
        let mut result: Vec<ModelGrant> = Vec::new();
        for grant in grants {
            let grant = ModelGrant::new(
                grant.permission,
                AdmittedModelIdentity::new(
                    grant.model.catalog_identity,
                    grant.model.key,
                    grant.model.provider,
                    grant.model.model,
                )?,
                grant.efforts,
            )?;
            if result.iter().any(|old| {
                old.model.catalog_identity == grant.model.catalog_identity
                    && old.model.key == grant.model.key
                    && old.model != grant.model
            }) {
                return Err(ModelPermissionError::ConflictingIdentity);
            }
            if let Some(existing) = result
                .iter_mut()
                .find(|old| old.permission == grant.permission && old.model == grant.model)
            {
                existing.efforts.extend(grant.efforts);
                deduplicate(&mut existing.efforts);
            } else {
                result.push(grant);
            }
        }
        Ok(Self(result))
    }

    pub fn grants(&self) -> &[ModelGrant] {
        &self.0
    }

    pub fn permits(
        &self,
        operation: ModelOperation,
        model: &AdmittedModelIdentity,
        effort: Reasoning,
    ) -> bool {
        self.0.iter().any(|g| {
            g.permission.operation == operation && g.model == *model && g.efforts.contains(&effort)
        })
    }

    pub fn is_subset_of(&self, parent: &Self) -> bool {
        self.0.iter().all(|g| {
            g.efforts
                .iter()
                .all(|&e| parent.permits(g.permission.operation, &g.model, e))
        })
    }

    pub fn effect_keys(&self) -> Vec<ActorEffectKey> {
        ActorEffectKey::ALL
            .iter()
            .copied()
            .filter(|key| {
                key.model_permission()
                    .is_some_and(|p| self.0.iter().any(|g| g.permission == p))
            })
            .collect()
    }

    pub fn validate_declared_effects(
        &self,
        effects: &[ActorEffectKey],
    ) -> Result<(), ModelPermissionError> {
        if effects
            .iter()
            .enumerate()
            .any(|(i, e)| effects[..i].contains(e))
        {
            return Err(ModelPermissionError::DuplicateEffects);
        }
        let declared: Vec<_> = effects
            .iter()
            .copied()
            .filter(|e| e.model_permission().is_some())
            .collect();
        let projected = self.effect_keys();
        if declared.len() != projected.len() || declared.iter().any(|e| !projected.contains(e)) {
            return Err(ModelPermissionError::EffectMismatch);
        }
        Ok(())
    }

    pub fn admit_child_requests(
        &self,
        requests: &[ModelGrantRequest],
    ) -> Result<Self, ModelPermissionError> {
        let mut grants = Vec::new();
        for request in requests {
            let parent = self
                .0
                .iter()
                .find(|g| {
                    g.permission.operation == request.operation
                        && g.model.catalog_identity == request.catalog_identity
                        && g.model.key == request.key
                })
                .ok_or(ModelPermissionError::ExceedsParent)?;
            let grant = ModelGrant::new(
                parent.permission,
                parent.model.clone(),
                request.efforts.clone(),
            )?;
            if grant.efforts.iter().any(|e| !parent.efforts.contains(e)) {
                return Err(ModelPermissionError::ExceedsParent);
            }
            grants.push(grant);
        }
        Self::new(grants)
    }

    pub fn conversation(
        &self,
        operation: ModelOperation,
        selection: &ModelSelectionEnvelope,
    ) -> Result<ConversationPermission, ModelPermissionError> {
        let grant = self
            .0
            .iter()
            .find(|g| {
                g.permission.operation == operation
                    && g.model.catalog_identity == selection.catalog_identity
                    && g.model.key == selection.key
                    && g.efforts.contains(&selection.reasoning)
            })
            .ok_or(ModelPermissionError::ExceedsParent)?;
        ConversationPermission::new(grant.model.clone(), grant.efforts.clone())
    }
}

/// The fixed conversation model and its allowed effort choices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationPermission {
    model: AdmittedModelIdentity,
    efforts: Vec<Reasoning>,
}

impl<'de> Deserialize<'de> for ConversationPermission {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            model: AdmittedModelIdentity,
            efforts: Vec<Reasoning>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let model = AdmittedModelIdentity::new(
            wire.model.catalog_identity,
            wire.model.key,
            wire.model.provider,
            wire.model.model,
        )
        .map_err(serde::de::Error::custom)?;
        Self::new(model, wire.efforts).map_err(serde::de::Error::custom)
    }
}

impl ConversationPermission {
    pub fn new(
        model: AdmittedModelIdentity,
        mut efforts: Vec<Reasoning>,
    ) -> Result<Self, ModelPermissionError> {
        validate_efforts(&model, &efforts)?;
        deduplicate(&mut efforts);
        Ok(Self { model, efforts })
    }
    pub fn model(&self) -> &AdmittedModelIdentity {
        &self.model
    }
    pub fn efforts(&self) -> &[Reasoning] {
        &self.efforts
    }
    pub fn permits(&self, model: &AdmittedModelIdentity, effort: Reasoning) -> bool {
        self.model == *model && self.efforts.contains(&effort)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorCapabilities, DeepSeekReasoning, OpenAIModel, OpenAIReasoning};
    use proptest::prelude::*;

    fn identity(key: u8) -> AdmittedModelIdentity {
        AdmittedModelIdentity::new(
            "catalog".into(),
            format!("model-{key}"),
            "provider".into(),
            Model::DeepSeekFlash,
        )
        .unwrap()
    }

    fn effort(index: u8) -> Reasoning {
        Reasoning::DeepSeekReasoning(match index {
            0 => DeepSeekReasoning::DeepSeekNone,
            1 => DeepSeekReasoning::DeepSeekLow,
            2 => DeepSeekReasoning::DeepSeekHigh,
            _ => DeepSeekReasoning::DeepSeekMax,
        })
    }

    fn operation(index: u8) -> ModelOperation {
        match index {
            0 => ModelOperation::ModelCall,
            1 => ModelOperation::ForkAgent,
            _ => ModelOperation::SpawnAgent,
        }
    }

    fn permissions(entries: &[(u8, u8, u8)]) -> ModelPermissions {
        ModelPermissions::new(
            entries
                .iter()
                .map(|&(op, key, e)| {
                    ModelGrant::new(
                        ModelPermission {
                            operation: operation(op),
                            family: ModelFamily::DeepSeek,
                        },
                        identity(key),
                        vec![effort(e)],
                    )
                    .unwrap()
                })
                .collect(),
        )
        .unwrap()
    }

    proptest! {
        #[test]
        fn grant_projection_and_runtime_checks_match_independent_relation(entries in prop::collection::vec((0u8..3, 0u8..3, 0u8..4), 0..40)) {
            let policy = permissions(&entries);
            for op in 0..3 {
                for key in 0..3 {
                    for e in 0..4 {
                        prop_assert_eq!(policy.permits(operation(op), &identity(key), effort(e)), entries.contains(&(op, key, e)));
                    }
                }
            }
            let projected = policy.effect_keys();
            for key in ActorEffectKey::ALL {
                if let Some(permission) = key.model_permission() {
                    let expected = permission.family == ModelFamily::DeepSeek && entries.iter().any(|&(op, _, _)| operation(op) == permission.operation);
                    prop_assert_eq!(projected.contains(key), expected);
                }
            }
            let restored: ModelPermissions = serde_json::from_slice(&serde_json::to_vec(&policy).unwrap()).unwrap();
            prop_assert_eq!(restored, policy);
        }

        #[test]
        fn subset_admission_matches_set_inclusion(parent in prop::collection::vec((0u8..3, 0u8..3, 0u8..4), 0..40), child in prop::collection::vec((0u8..3, 0u8..3, 0u8..4), 0..20)) {
            let parent_policy = permissions(&parent);
            let child_policy = permissions(&child);
            let expected = child.iter().all(|entry| parent.contains(entry));
            prop_assert_eq!(child_policy.is_subset_of(&parent_policy), expected);
            let parent_caps = ActorCapabilities::default().with_model_permissions(parent_policy.clone());
            let child_caps = ActorCapabilities::default().with_effect_keys(child_policy.effect_keys()).with_model_permissions(child_policy.clone());
            prop_assert_eq!(parent_caps.preview_child(child_caps, None).is_ok(), expected);
            let requests: Vec<_> = child.iter().map(|&(op, key, e)| ModelGrantRequest { operation: operation(op), catalog_identity: "catalog".into(), key: format!("model-{key}"), efforts: vec![effort(e)] }).collect();
            prop_assert_eq!(parent_policy.admit_child_requests(&requests).is_ok(), expected);
        }

        #[test]
        fn request_admission_checks_exact_family_row(entries in prop::collection::vec((0u8..3, 0u8..3, 0u8..4), 0..30)) {
            let policy = permissions(&entries);
            let mut row = policy.effect_keys();
            row.push(ActorEffectKey::Replies);
            prop_assert!(ActorCapabilities::admit(row.clone(), policy.clone(), None).is_ok());
            if !policy.grants().is_empty() {
                let remove = row.iter().position(|key| key.model_permission().is_some()).unwrap();
                row.remove(remove);
                prop_assert!(ActorCapabilities::admit(row, policy, None).is_err());
            }
        }
    }

    #[test]
    fn default_and_raw_family_keys_do_not_issue_model_authority() {
        let raw: Vec<_> = ActorEffectKey::ALL
            .iter()
            .copied()
            .filter(|key| key.model_permission().is_some())
            .collect();
        assert!(!raw.is_empty());
        assert!(ActorCapabilities::admit(raw.clone(), ModelPermissions::default(), None).is_err());
        let caps = ActorCapabilities::default().with_effect_keys(raw);
        assert!(caps.model_permissions().grants().is_empty());
        assert!(caps
            .effect_keys()
            .iter()
            .all(|key| key.model_permission().is_none()));
    }

    #[test]
    fn fixed_conversation_refuses_model_and_provider_substitution() {
        let model = identity(0);
        let grant = ConversationPermission::new(model.clone(), vec![effort(1), effort(2)]).unwrap();
        assert!(grant.permits(&model, effort(2)));
        assert!(!grant.permits(&model, effort(0)));
        let mut substitutions = Vec::new();
        let mut changed = model.clone();
        changed.catalog_identity.push('x');
        substitutions.push(changed);
        let mut changed = model.clone();
        changed.key.push('x');
        substitutions.push(changed);
        let mut changed = model.clone();
        changed.provider.push('x');
        substitutions.push(changed);
        let mut changed = model;
        changed.model = Model::DeepSeekPro;
        substitutions.push(changed);
        for changed in substitutions {
            assert!(!grant.permits(&changed, effort(1)));
        }
    }

    #[test]
    fn malformed_grants_and_durable_authority_are_rejected() {
        let permission = ModelPermission {
            operation: ModelOperation::ModelCall,
            family: ModelFamily::DeepSeek,
        };
        assert!(ModelGrant::new(permission, identity(0), vec![]).is_err());
        assert!(ModelGrant::new(
            permission,
            identity(0),
            vec![Reasoning::OpenAIReasoning(OpenAIReasoning::OpenAIHigh)]
        )
        .is_err());
        let other = AdmittedModelIdentity::new(
            "catalog".into(),
            "other".into(),
            "openai".into(),
            Model::OpenAIModel(OpenAIModel::Gpt61Sol),
        )
        .unwrap();
        assert!(ModelGrant::new(permission, other, vec![effort(0)]).is_err());
        let mut durable = serde_json::to_value(permissions(&[(0, 0, 1)])).unwrap();
        durable[0]["model"]["provider"] = serde_json::json!("");
        assert!(serde_json::from_value::<ModelPermissions>(durable).is_err());
        let mut conflicting = identity(0);
        conflicting.provider = "second-provider".into();
        assert_eq!(
            ModelPermissions::new(vec![
                ModelGrant::new(permission, identity(0), vec![effort(1)]).unwrap(),
                ModelGrant::new(permission, conflicting, vec![effort(2)]).unwrap()
            ]),
            Err(ModelPermissionError::ConflictingIdentity)
        );
    }

    #[test]
    fn child_admission_preserves_native_history_and_empty_delegation() {
        let source = identity(0);
        let target = identity(1);
        let mut foreign = identity(2);
        foreign.provider = "another-account".into();
        let mut foreign_catalog = identity(3);
        foreign_catalog.catalog_identity = "another-catalog".into();
        let fork = ModelPermission {
            operation: ModelOperation::ForkAgent,
            family: ModelFamily::DeepSeek,
        };
        let spawn = ModelPermission {
            operation: ModelOperation::SpawnAgent,
            family: ModelFamily::DeepSeek,
        };
        let mut grants = Vec::new();
        for model in [&target, &foreign, &foreign_catalog] {
            for permission in [fork, spawn] {
                grants.push(
                    ModelGrant::new(permission, model.clone(), vec![effort(1), effort(2)]).unwrap(),
                );
            }
        }
        let parent = ActorCapabilities::default()
            .with_model_permissions(ModelPermissions::new(grants).unwrap())
            .with_conversation_permission(Some(
                ConversationPermission::new(source, vec![effort(1)]).unwrap(),
            ));
        let registry = crate::ActorAdmissionRegistry::new();
        let issuer = crate::ActorRef::first(crate::ActorId(1));
        let token = registry.capture_checkpoint(
            "captured".into(),
            issuer,
            parent.clone(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            tidepool_repr::SessionId(7),
            tidepool_codegen::scope::ScopeId(3),
            crate::ContextCheckpointBoundary::external(
                "thread".into(),
                "call".into(),
                "call".into(),
            ),
        );
        let checkpoint = registry
            .checkpoint(&token, tidepool_repr::SessionId(7))
            .unwrap();
        registry.retire_actor(issuer);
        // Borrowed history comes from its retained issuer, independently of the caller.
        let parent = parent.with_conversation_permission(Some(
            ConversationPermission::new(foreign.clone(), vec![effort(1)]).unwrap(),
        ));
        let selection = |model: &AdmittedModelIdentity| ModelSelectionEnvelope {
            catalog_identity: model.catalog_identity.clone(),
            key: model.key.clone(),
            reasoning: effort(1),
        };
        let child = parent
            .admit_model_child(
                fork,
                &selection(&target),
                vec![],
                &[],
                None,
                Some(&checkpoint),
            )
            .unwrap();
        assert!(child.model_permissions().grants().is_empty());
        let conversation = child.conversation_permission().unwrap();
        assert!(conversation.permits(&target, effort(2)));
        assert!(!conversation.permits(&identity(0), effort(1)));
        for foreign in [&foreign, &foreign_catalog] {
            assert!(parent
                .admit_model_child(
                    fork,
                    &selection(foreign),
                    vec![],
                    &[],
                    None,
                    Some(&checkpoint)
                )
                .is_err());
            assert!(parent
                .admit_model_child(spawn, &selection(foreign), vec![], &[], None, None)
                .is_ok());
        }
        let wrong_family = ModelPermission {
            operation: ModelOperation::ForkAgent,
            family: ModelFamily::OpenAI,
        };
        assert!(parent
            .admit_model_child(
                wrong_family,
                &selection(&target),
                vec![],
                &[],
                None,
                Some(&checkpoint)
            )
            .is_err());
        assert!(parent
            .admit_model_child(fork, &selection(&target), vec![], &[], None, None)
            .is_err());
        assert!(parent
            .admit_model_child(
                spawn,
                &selection(&target),
                vec![],
                &[],
                None,
                Some(&checkpoint)
            )
            .is_err());
    }

    #[test]
    fn record_children_require_explicit_matching_model_grants() {
        let policy = permissions(&[(0, 0, 1), (0, 0, 2)]);
        let effects = policy.effect_keys();
        let parent = ActorCapabilities::default().with_model_permissions(policy);
        assert!(parent
            .admit_record_child(effects.clone(), &[], None)
            .is_err());
        let request = ModelGrantRequest {
            operation: ModelOperation::ModelCall,
            catalog_identity: "catalog".into(),
            key: "model-0".into(),
            efforts: vec![effort(1)],
        };
        assert!(parent
            .admit_record_child(vec![], &[request.clone()], None)
            .is_err());
        let child = parent
            .admit_record_child(effects, &[request], None)
            .unwrap();
        assert!(child.conversation_permission().is_none());
        assert!(child.model_permissions().permits(
            ModelOperation::ModelCall,
            &identity(0),
            effort(1)
        ));
        assert!(!child.model_permissions().permits(
            ModelOperation::ModelCall,
            &identity(0),
            effort(2)
        ));
    }
}
