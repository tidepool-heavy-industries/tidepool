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
}

impl Default for ActorCapabilities {
    fn default() -> Self {
        Self {
            descendants: DescendantBudget {
                maximum_depth: 8,
                maximum_active_children: None,
            },
            effect_keys: exomonad_tool::DEFAULT_ACTOR_EFFECTS.to_vec(),
        }
    }
}

impl ActorCapabilities {
    #[must_use]
    pub fn with_effect_keys(mut self, effect_keys: Vec<ActorEffectKey>) -> Self {
        self.effect_keys = effect_keys;
        self
    }

    #[must_use]
    pub fn with_descendant_budget(mut self, descendants: DescendantBudget) -> Self {
        self.descendants = descendants;
        self
    }

    #[must_use]
    pub const fn descendants(&self) -> DescendantBudget { self.descendants }

    #[must_use]
    pub fn effect_keys(&self) -> &[ActorEffectKey] { &self.effect_keys }

    #[must_use]
    pub fn haskell_effects_type(&self) -> String {
        format!("'[{}]", self.effect_keys.iter().map(|effect| effect.haskell_name()).collect::<Vec<_>>().join(", "))
    }

    #[must_use]
    pub fn accepts_effect_keys(&self, requested: &[ActorEffectKey]) -> bool {
        requested.iter().all(|effect| self.effect_keys.contains(effect))
    }

    #[must_use]
    pub fn missing_effect_names(&self, required: &[String]) -> Vec<String> {
        required.iter().filter(|name| !self.effect_keys.iter().any(|key| key.haskell_name() == name.as_str())).cloned().collect()
    }

    #[must_use]
    pub fn has_unique_effects(&self) -> bool {
        self.effect_keys.iter().enumerate().all(|(index, key)| !self.effect_keys[..index].contains(key))
    }

    #[must_use]
    pub fn attenuate_child(&self, child: Self) -> Self {
        // No requested numeric limit is present, so conversion cannot fail.
        self.child_budget(child, None).unwrap_or_else(|_| unreachable!("omitted budget is valid"))
    }

    pub fn preview_child(&self, child: Self, requested: Option<(i64, i64)>) -> Result<Self, String> {
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
        let requested = requested.map(|(depth, width)| {
            Ok::<_, String>(DescendantBudget {
                maximum_depth: u16::try_from(depth).map_err(|_| "spawn depth must be in 0..65535")?,
                maximum_active_children: Some(u16::try_from(width).map_err(|_| "spawn width must be in 0..65535")?),
            })
        }).transpose()?;
        child.descendants = DescendantBudget { maximum_depth: 0, maximum_active_children: Some(0) };
        if child.effect_keys.contains(&ActorEffectKey::Forks) {
            child.descendants = DescendantBudget {
                maximum_depth: self.descendants.maximum_depth.saturating_sub(1),
                maximum_active_children: self.descendants.maximum_active_children,
            };
            if let Some(requested) = requested {
                child.descendants.maximum_depth = child.descendants.maximum_depth.min(requested.maximum_depth);
                child.descendants.maximum_active_children = child.descendants.maximum_active_children.into_iter().chain(requested.maximum_active_children).min();
            }
        }
        Ok(child)
    }

    #[must_use]
    pub fn permits_child(&self, child: &Self) -> bool {
        child.has_unique_effects()
            && child.descendants.maximum_depth < self.descendants.maximum_depth
            && self.descendants.maximum_active_children.is_none_or(|limit| child.descendants.maximum_active_children.is_some_and(|child_limit| child_limit <= limit))
            && self.accepts_effect_keys(&child.effect_keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_effects_are_checked_against_available_effects() {
        let parent = ActorCapabilities::default().with_effect_keys(vec![ActorEffectKey::Replies, ActorEffectKey::Forks]);
        let allowed = parent.preview_child(ActorCapabilities::default().with_effect_keys(vec![ActorEffectKey::Replies]), None).unwrap();
        assert_eq!(allowed.descendants().maximum_depth, 0);
        assert!(parent.preview_child(ActorCapabilities::default().with_effect_keys(vec![ActorEffectKey::Commands]), None).is_err());
        assert!(parent.preview_child(ActorCapabilities::default().with_effect_keys(vec![ActorEffectKey::Replies, ActorEffectKey::Replies]), None).is_err());
    }

    #[test]
    fn descendant_limits_spend_depth_and_preserve_finite_parent_bound() {
        let parent = ActorCapabilities::default().with_descendant_budget(DescendantBudget { maximum_depth: 3, maximum_active_children: Some(2) });
        let child = parent.preview_child(ActorCapabilities::default(), Some((99, 99))).unwrap();
        assert_eq!(child.descendants(), DescendantBudget { maximum_depth: 2, maximum_active_children: Some(2) });
        let grandchild = child.preview_child(ActorCapabilities::default(), None).unwrap();
        assert_eq!(grandchild.descendants().maximum_depth, 1);
        let leaf = grandchild.preview_child(ActorCapabilities::default(), None).unwrap();
        assert!(leaf.preview_child(ActorCapabilities::default(), None).is_err());
        assert!(parent.preview_child(ActorCapabilities::default(), Some((-1, 1))).is_err());
        assert!(parent.preview_child(ActorCapabilities::default(), Some((1, 65536))).is_err());
    }

    #[test]
    fn missing_effects_use_the_exact_custom_effect_list() {
        let available = ActorCapabilities::default().with_effect_keys(vec![ActorEffectKey::Commands]);
        assert_eq!(available.missing_effect_names(&["Commands".into(), "Journal".into()]), vec!["Journal"]);
    }
}
