//! One accepted actor role projected into runtime, workspace, prompt, and status policy.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorRole {
    Root,
    Research,
    Coding,
    Scaffolding,
    Integration,
    Inherited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeToolClass {
    InspectionOnly,
    Coding,
    Integration,
    Inherited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspaceAccess {
    None,
    InspectOnly,
    WritableBound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DescendantBudget {
    pub maximum_depth: u16,
    pub maximum_active_children: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorEffectKey {
    Replies,
    Watches,
    Forks,
    ActorContext,
    AgentLaunch,
    AgentInspection,
    AgentControl,
    BoundWorktree,
    WorktreeRegistry,
    WorktreeAllocation,
    WorktreeIntegration,
}

impl ActorEffectKey {
    const fn haskell_name(self) -> &'static str {
        match self {
            Self::Replies => "Replies",
            Self::Watches => "Watches",
            Self::Forks => "Forks",
            Self::ActorContext => "ActorContext",
            Self::AgentLaunch => "AgentLaunch",
            Self::AgentInspection => "AgentInspection",
            Self::AgentControl => "AgentControl",
            Self::BoundWorktree => "BoundWorktree",
            Self::WorktreeRegistry => "WorktreeRegistry",
            Self::WorktreeAllocation => "WorktreeAllocation",
            Self::WorktreeIntegration => "WorktreeIntegration",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRole {
    role: ActorRole,
    native_tools: NativeToolClass,
    workspace: WorkspaceAccess,
    descendants: DescendantBudget,
    prompt_profile: &'static str,
    effect_keys: Vec<ActorEffectKey>,
}

impl EffectiveRole {
    #[must_use]
    pub fn root() -> Self {
        Self::new(
            ActorRole::Root,
            NativeToolClass::Coding,
            WorkspaceAccess::WritableBound,
            DescendantBudget {
                maximum_depth: 8,
                maximum_active_children: 32,
            },
            "root-v1",
            vec![
                ActorEffectKey::Replies,
                ActorEffectKey::Watches,
                ActorEffectKey::Forks,
                ActorEffectKey::ActorContext,
                ActorEffectKey::AgentLaunch,
                ActorEffectKey::AgentInspection,
                ActorEffectKey::AgentControl,
                ActorEffectKey::BoundWorktree,
                ActorEffectKey::WorktreeRegistry,
                ActorEffectKey::WorktreeAllocation,
                ActorEffectKey::WorktreeIntegration,
            ],
        )
    }

    #[must_use]
    pub fn research() -> Self {
        Self::new(
            ActorRole::Research,
            NativeToolClass::InspectionOnly,
            WorkspaceAccess::InspectOnly,
            DescendantBudget {
                maximum_depth: 0,
                maximum_active_children: 0,
            },
            "research-v1",
            vec![
                ActorEffectKey::Replies,
                ActorEffectKey::Watches,
                ActorEffectKey::ActorContext,
                ActorEffectKey::BoundWorktree,
            ],
        )
    }

    #[must_use]
    pub fn coding() -> Self {
        Self::new(
            ActorRole::Coding,
            NativeToolClass::Coding,
            WorkspaceAccess::WritableBound,
            DescendantBudget {
                maximum_depth: 0,
                maximum_active_children: 0,
            },
            "coding-v2",
            vec![
                ActorEffectKey::Replies,
                ActorEffectKey::Watches,
                ActorEffectKey::Forks,
                ActorEffectKey::ActorContext,
                ActorEffectKey::AgentInspection,
                ActorEffectKey::AgentControl,
                ActorEffectKey::BoundWorktree,
                ActorEffectKey::WorktreeIntegration,
            ],
        )
    }

    #[must_use]
    pub fn scaffolding(descendants: DescendantBudget) -> Self {
        Self::new(
            ActorRole::Scaffolding,
            NativeToolClass::Coding,
            WorkspaceAccess::WritableBound,
            descendants,
            "scaffolding-v1",
            Self::coding().effect_keys,
        )
    }

    #[must_use]
    pub fn integration() -> Self {
        Self::new(
            ActorRole::Integration,
            NativeToolClass::Integration,
            WorkspaceAccess::WritableBound,
            DescendantBudget {
                maximum_depth: 0,
                maximum_active_children: 0,
            },
            "integration-v1",
            vec![
                ActorEffectKey::Replies,
                ActorEffectKey::Watches,
                ActorEffectKey::ActorContext,
                ActorEffectKey::AgentInspection,
                ActorEffectKey::BoundWorktree,
                ActorEffectKey::WorktreeIntegration,
            ],
        )
    }

    fn new(
        role: ActorRole,
        native_tools: NativeToolClass,
        workspace: WorkspaceAccess,
        descendants: DescendantBudget,
        prompt_profile: &'static str,
        effect_keys: Vec<ActorEffectKey>,
    ) -> Self {
        Self {
            role,
            native_tools,
            workspace,
            descendants,
            prompt_profile,
            effect_keys,
        }
    }

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
    pub const fn role(&self) -> ActorRole {
        self.role
    }
    #[must_use]
    pub const fn native_tools(&self) -> NativeToolClass {
        self.native_tools
    }
    #[must_use]
    pub const fn workspace(&self) -> WorkspaceAccess {
        self.workspace
    }
    #[must_use]
    pub const fn descendants(&self) -> DescendantBudget {
        self.descendants
    }
    #[must_use]
    pub const fn prompt_profile(&self) -> &'static str {
        self.prompt_profile
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
    pub fn respects_role_ceiling(&self) -> bool {
        let ceiling = match self.role {
            ActorRole::Root | ActorRole::Inherited => Self::root(),
            ActorRole::Research => Self::research(),
            ActorRole::Coding => Self::coding(),
            ActorRole::Scaffolding => Self::scaffolding(self.descendants),
            ActorRole::Integration => Self::integration(),
        };
        ceiling.accepts_effect_keys(&self.effect_keys)
            && self
                .effect_keys
                .iter()
                .enumerate()
                .all(|(index, key)| !self.effect_keys[..index].contains(key))
    }

    #[must_use]
    pub fn permits_child(&self, child: &Self) -> bool {
        child.descendants.maximum_depth < self.descendants.maximum_depth
            && child.descendants.maximum_active_children <= self.descendants.maximum_active_children
            && native_rank(child.native_tools) <= native_rank(self.native_tools)
            && workspace_rank(child.workspace) <= workspace_rank(self.workspace)
            && self.accepts_effect_keys(&child.effect_keys)
    }
}

const fn native_rank(class: NativeToolClass) -> u8 {
    match class {
        NativeToolClass::InspectionOnly => 0,
        NativeToolClass::Coding | NativeToolClass::Integration => 1,
        NativeToolClass::Inherited => 2,
    }
}

const fn workspace_rank(access: WorkspaceAccess) -> u8 {
    match access {
        WorkspaceAccess::None => 0,
        WorkspaceAccess::InspectOnly => 1,
        WorkspaceAccess::WritableBound => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_role_is_monotone_across_all_projected_dimensions() {
        let root = EffectiveRole::root();
        assert!(root.permits_child(&EffectiveRole::research()));
        assert!(root.permits_child(&EffectiveRole::coding()));
        let scaffold = EffectiveRole::scaffolding(DescendantBudget {
            maximum_depth: 3,
            maximum_active_children: 4,
        });
        assert!(root.permits_child(&scaffold));
        assert!(scaffold.permits_child(&EffectiveRole::coding()));
        assert!(!EffectiveRole::research().permits_child(&EffectiveRole::coding()));
        assert!(!EffectiveRole::coding().permits_child(&EffectiveRole::research()));
    }

    #[test]
    fn exact_effect_row_is_rendered_from_stable_keys() {
        assert_eq!(
            EffectiveRole::research().haskell_effects_type(),
            "'[Replies, Watches, ActorContext, BoundWorktree]"
        );
        let narrow = EffectiveRole::coding().with_effect_keys(vec![ActorEffectKey::Replies]);
        assert_eq!(narrow.haskell_effects_type(), "'[Replies]");
        assert!(EffectiveRole::root().permits_child(&narrow));
    }

    #[test]
    fn coding_recursion_requires_budget_and_preserves_narrowing() {
        let coding = EffectiveRole::coding().with_descendant_budget(DescendantBudget {
            maximum_depth: 2,
            maximum_active_children: 3,
        });
        let child = EffectiveRole::coding().with_descendant_budget(DescendantBudget {
            maximum_depth: 1,
            maximum_active_children: 3,
        });
        assert!(coding.respects_role_ceiling());
        assert!(coding.permits_child(&child));
        assert!(!child.permits_child(&coding));
        assert!(!EffectiveRole::coding().permits_child(&child));
        assert!(!coding
            .clone()
            .with_effect_keys(EffectiveRole::research().effect_keys)
            .permits_child(&child));
        assert!(!coding
            .clone()
            .with_effect_keys(vec![ActorEffectKey::AgentLaunch])
            .respects_role_ceiling());
        assert_eq!(
            coding.effect_keys(),
            EffectiveRole::scaffolding(coding.descendants()).effect_keys()
        );
    }

    #[test]
    fn attenuating_descendants_preserves_every_other_role_dimension() {
        let narrow = EffectiveRole::scaffolding(DescendantBudget {
            maximum_depth: 3,
            maximum_active_children: 4,
        })
        .with_effect_keys(vec![ActorEffectKey::Replies, ActorEffectKey::Forks]);
        let attenuated = narrow.clone().with_descendant_budget(DescendantBudget {
            maximum_depth: 2,
            maximum_active_children: 4,
        });

        assert_eq!(attenuated.role(), narrow.role());
        assert_eq!(attenuated.native_tools(), narrow.native_tools());
        assert_eq!(attenuated.workspace(), narrow.workspace());
        assert_eq!(attenuated.prompt_profile(), narrow.prompt_profile());
        assert_eq!(attenuated.effect_keys(), narrow.effect_keys());
        assert_eq!(attenuated.descendants().maximum_depth, 2);
    }
}
