//! Available actor effects and independently attenuated descendant limits.

pub use exomonad_tool::ActorEffectKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DescendantBudget {
    pub maximum_depth: u16,
    /// None leaves concurrency unbounded; Some(0) forbids descendants.
    pub maximum_active_children: Option<u16>,
}

#[must_use]
pub fn render_child_budget(maximum_active_children: Option<u16>) -> String {
    maximum_active_children.map_or_else(|| "unbounded".to_owned(), |children| children.to_string())
}

/// Effect availability does not authorize access to concrete resources.
/// Workspaces, processes and provider attachments retain their own grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorCapabilities {
    descendants: DescendantBudget,
    effect_keys: Vec<ActorEffectKey>,
    model_permissions: crate::ModelPermissions,
    conversation_permission: Option<crate::ConversationPermission>,
}

impl Default for ActorCapabilities {
    fn default() -> Self {
        Self {
            descendants: DescendantBudget {
                maximum_depth: 8,
                maximum_active_children: None,
            },
            effect_keys: exomonad_tool::DEFAULT_ACTOR_EFFECTS
                .iter()
                .copied()
                .filter(|key| key.model_permission().is_none())
                .collect(),
            model_permissions: crate::ModelPermissions::default(),
            conversation_permission: None,
        }
    }
}

impl ActorCapabilities {
    #[must_use]
    pub fn with_effect_keys(mut self, effect_keys: Vec<ActorEffectKey>) -> Self {
        self.effect_keys = effect_keys
            .into_iter()
            .filter(|key| key.model_permission().is_none())
            .collect();
        self.effect_keys
            .extend(self.model_permissions.effect_keys());
        self
    }

    /// Admission checks the authored row before constructing its derived projection.
    pub fn admit(
        effect_keys: Vec<ActorEffectKey>,
        model_permissions: crate::ModelPermissions,
        conversation_permission: Option<crate::ConversationPermission>,
    ) -> Result<Self, crate::ModelPermissionError> {
        model_permissions.validate_declared_effects(&effect_keys)?;
        Ok(Self::default()
            .with_model_permissions(model_permissions)
            .with_effect_keys(effect_keys)
            .with_conversation_permission(conversation_permission))
    }

    #[must_use]
    pub fn with_model_permissions(mut self, model_permissions: crate::ModelPermissions) -> Self {
        self.effect_keys
            .retain(|key| key.model_permission().is_none());
        self.effect_keys.extend(model_permissions.effect_keys());
        self.model_permissions = model_permissions;
        self
    }

    #[must_use]
    pub fn with_conversation_permission(
        mut self,
        permission: Option<crate::ConversationPermission>,
    ) -> Self {
        self.conversation_permission = permission;
        self
    }

    pub fn model_permissions(&self) -> &crate::ModelPermissions {
        &self.model_permissions
    }
    pub fn conversation_permission(&self) -> Option<&crate::ConversationPermission> {
        self.conversation_permission.as_ref()
    }

    /// Admit one model child before allocating its workspace or provider session.
    pub fn admit_model_child(
        &self,
        permission: crate::ModelPermission,
        selection: &tidepool_bridge_effects::ModelSelectionEnvelope,
        effects: Vec<ActorEffectKey>,
        requests: &[tidepool_bridge_effects::ModelGrantRequest],
        requested_budget: Option<(i64, i64)>,
    ) -> Result<Self, String> {
        use crate::{ModelOperation, ModelPermissionError};
        if !matches!(
            permission.operation,
            ModelOperation::ForkAgent | ModelOperation::SpawnAgent
        ) {
            return Err(ModelPermissionError::InvalidChildOperation.to_string());
        }
        let conversation = self
            .model_permissions
            .conversation(permission.operation, selection)
            .map_err(|e| e.to_string())?;
        if conversation.model().family() != permission.family {
            return Err(ModelPermissionError::FamilyMismatch.to_string());
        }
        if permission.operation == ModelOperation::ForkAgent {
            let source = self
                .conversation_permission
                .as_ref()
                .ok_or_else(|| ModelPermissionError::IncompatibleHistory.to_string())?;
            if source.model().catalog_identity() != conversation.model().catalog_identity()
                || source.model().provider() != conversation.model().provider()
                || source.model().family() != conversation.model().family()
            {
                return Err(ModelPermissionError::IncompatibleHistory.to_string());
            }
        }
        let permissions = self
            .model_permissions
            .admit_child_requests(requests)
            .map_err(|e| e.to_string())?;
        let child =
            Self::admit(effects, permissions, Some(conversation)).map_err(|e| e.to_string())?;
        self.preview_child(child, requested_budget)
    }

    pub fn admit_record_child(
        &self,
        effects: Vec<ActorEffectKey>,
        requests: &[tidepool_bridge_effects::ModelGrantRequest],
        requested_budget: Option<(i64, i64)>,
    ) -> Result<Self, String> {
        let permissions = self
            .model_permissions
            .admit_child_requests(requests)
            .map_err(|e| e.to_string())?;
        let child = Self::admit(effects, permissions, None).map_err(|e| e.to_string())?;
        self.preview_child(child, requested_budget)
    }

    #[must_use]
    pub fn with_descendant_budget(mut self, descendants: DescendantBudget) -> Self {
        self.descendants = descendants;
        self
    }

    #[must_use]
    pub const fn descendants(&self) -> DescendantBudget {
        self.descendants
    }

    #[must_use]
    pub fn effect_keys(&self) -> &[ActorEffectKey] {
        &self.effect_keys
    }

    #[must_use]
    pub fn haskell_effects_type(&self) -> String {
        format!(
            "'[{}]",
            self.effect_keys
                .iter()
                .map(|effect| effect.haskell_name())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    #[must_use]
    pub fn accepts_effect_keys(&self, requested: &[ActorEffectKey]) -> bool {
        requested
            .iter()
            .all(|effect| self.effect_keys.contains(effect))
    }

    #[must_use]
    pub fn missing_effect_names(&self, required: &[String]) -> Vec<String> {
        required
            .iter()
            .filter(|name| {
                !self
                    .effect_keys
                    .iter()
                    .any(|key| key.haskell_name() == name.as_str())
            })
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn has_unique_effects(&self) -> bool {
        self.effect_keys
            .iter()
            .enumerate()
            .all(|(index, key)| !self.effect_keys[..index].contains(key))
    }

    #[must_use]
    pub fn attenuate_child(&self, child: Self) -> Self {
        // No requested numeric limit is present, so conversion cannot fail.
        self.child_budget(child, None)
            .unwrap_or_else(|_| unreachable!("omitted budget is valid"))
    }

    pub fn preview_child(
        &self,
        child: Self,
        requested: Option<(i64, i64)>,
    ) -> Result<Self, String> {
        if !child.has_unique_effects() {
            return Err("requested effects contain duplicates".into());
        }
        let child = self.child_budget(child, requested)?;
        if !self.permits_child(&child) {
            return Err("child effects or descendant limits exceed parent authority".into());
        }
        Ok(child)
    }

    fn child_budget(&self, mut child: Self, requested: Option<(i64, i64)>) -> Result<Self, String> {
        let requested = requested
            .map(|(depth, width)| {
                Ok::<_, String>(DescendantBudget {
                    maximum_depth: u16::try_from(depth)
                        .map_err(|_| "spawn depth must be in 0..65535")?,
                    maximum_active_children: Some(
                        u16::try_from(width).map_err(|_| "spawn width must be in 0..65535")?,
                    ),
                })
            })
            .transpose()?;
        child.descendants = DescendantBudget {
            maximum_depth: 0,
            maximum_active_children: Some(0),
        };
        if child.effect_keys.contains(&ActorEffectKey::AgentLaunch)
            || child.model_permissions.grants().iter().any(|grant| {
                matches!(
                    grant.permission().operation,
                    crate::ModelOperation::ForkAgent | crate::ModelOperation::SpawnAgent
                )
            })
        {
            child.descendants = DescendantBudget {
                maximum_depth: self.descendants.maximum_depth.saturating_sub(1),
                maximum_active_children: self.descendants.maximum_active_children,
            };
            if let Some(requested) = requested {
                child.descendants.maximum_depth =
                    child.descendants.maximum_depth.min(requested.maximum_depth);
                child.descendants.maximum_active_children = child
                    .descendants
                    .maximum_active_children
                    .into_iter()
                    .chain(requested.maximum_active_children)
                    .min();
            }
        }
        Ok(child)
    }

    #[must_use]
    pub fn permits_child(&self, child: &Self) -> bool {
        self.descendants.maximum_active_children != Some(0)
            && child.has_unique_effects()
            && child.descendants.maximum_depth < self.descendants.maximum_depth
            && self
                .descendants
                .maximum_active_children
                .is_none_or(|limit| {
                    child
                        .descendants
                        .maximum_active_children
                        .is_some_and(|child_limit| child_limit <= limit)
                })
            && self.accepts_effect_keys(&child.effect_keys)
            && child
                .model_permissions
                .is_subset_of(&self.model_permissions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_effects_are_checked_against_available_effects() {
        let parent = ActorCapabilities::default()
            .with_effect_keys(vec![ActorEffectKey::Replies, ActorEffectKey::AgentLaunch]);
        let allowed = parent
            .preview_child(
                ActorCapabilities::default().with_effect_keys(vec![ActorEffectKey::Replies]),
                None,
            )
            .unwrap();
        assert_eq!(allowed.descendants().maximum_depth, 0);
        assert!(parent
            .preview_child(
                ActorCapabilities::default().with_effect_keys(vec![ActorEffectKey::Commands]),
                None
            )
            .is_err());
        assert!(parent
            .preview_child(
                ActorCapabilities::default()
                    .with_effect_keys(vec![ActorEffectKey::Replies, ActorEffectKey::Replies]),
                None
            )
            .is_err());
    }

    #[test]
    fn descendant_limits_spend_depth_and_preserve_finite_parent_bound() {
        let parent = ActorCapabilities::default().with_descendant_budget(DescendantBudget {
            maximum_depth: 3,
            maximum_active_children: Some(2),
        });
        let child = parent
            .preview_child(ActorCapabilities::default(), Some((99, 99)))
            .unwrap();
        assert_eq!(
            child.descendants(),
            DescendantBudget {
                maximum_depth: 2,
                maximum_active_children: Some(2)
            }
        );
        let grandchild = child
            .preview_child(ActorCapabilities::default(), None)
            .unwrap();
        assert_eq!(grandchild.descendants().maximum_depth, 1);
        let leaf = grandchild
            .preview_child(ActorCapabilities::default(), None)
            .unwrap();
        assert!(leaf
            .preview_child(ActorCapabilities::default(), None)
            .is_err());
        assert!(parent
            .preview_child(ActorCapabilities::default(), Some((-1, 1)))
            .is_err());
        assert!(parent
            .preview_child(ActorCapabilities::default(), Some((1, 65536)))
            .is_err());
    }

    #[test]
    fn zero_active_child_limit_refuses_admission_even_with_remaining_depth() {
        let parent = ActorCapabilities::default().with_descendant_budget(DescendantBudget {
            maximum_depth: 3,
            maximum_active_children: Some(0),
        });
        assert!(parent
            .preview_child(ActorCapabilities::default(), None)
            .is_err());
    }

    #[test]
    fn missing_effects_use_the_exact_custom_effect_list() {
        let available =
            ActorCapabilities::default().with_effect_keys(vec![ActorEffectKey::Commands]);
        assert_eq!(
            available.missing_effect_names(&["Commands".into(), "Journal".into()]),
            vec!["Journal"]
        );
    }
}
