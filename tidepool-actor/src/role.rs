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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRole {
    role: ActorRole,
    native_tools: NativeToolClass,
    workspace: WorkspaceAccess,
    descendants: DescendantBudget,
    prompt_profile: &'static str,
}

impl EffectiveRole {
    #[must_use]
    pub const fn root() -> Self {
        Self::new(
            ActorRole::Root,
            NativeToolClass::Coding,
            WorkspaceAccess::WritableBound,
            DescendantBudget {
                maximum_depth: 8,
                maximum_active_children: 32,
            },
            "root-v1",
        )
    }

    #[must_use]
    pub const fn research() -> Self {
        Self::new(
            ActorRole::Research,
            NativeToolClass::InspectionOnly,
            WorkspaceAccess::InspectOnly,
            DescendantBudget {
                maximum_depth: 0,
                maximum_active_children: 0,
            },
            "research-v1",
        )
    }

    #[must_use]
    pub const fn coding() -> Self {
        Self::new(
            ActorRole::Coding,
            NativeToolClass::Coding,
            WorkspaceAccess::WritableBound,
            DescendantBudget {
                maximum_depth: 0,
                maximum_active_children: 0,
            },
            "coding-v1",
        )
    }

    #[must_use]
    pub const fn scaffolding(descendants: DescendantBudget) -> Self {
        Self::new(
            ActorRole::Scaffolding,
            NativeToolClass::Coding,
            WorkspaceAccess::WritableBound,
            descendants,
            "scaffolding-v1",
        )
    }

    #[must_use]
    pub const fn integration() -> Self {
        Self::new(
            ActorRole::Integration,
            NativeToolClass::Integration,
            WorkspaceAccess::WritableBound,
            DescendantBudget {
                maximum_depth: 0,
                maximum_active_children: 0,
            },
            "integration-v1",
        )
    }

    const fn new(
        role: ActorRole,
        native_tools: NativeToolClass,
        workspace: WorkspaceAccess,
        descendants: DescendantBudget,
        prompt_profile: &'static str,
    ) -> Self {
        Self {
            role,
            native_tools,
            workspace,
            descendants,
            prompt_profile,
        }
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
    pub const fn permits_child(&self, child: &Self) -> bool {
        child.descendants.maximum_depth < self.descendants.maximum_depth
            && child.descendants.maximum_active_children <= self.descendants.maximum_active_children
            && native_rank(child.native_tools) <= native_rank(self.native_tools)
            && workspace_rank(child.workspace) <= workspace_rank(self.workspace)
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
}
