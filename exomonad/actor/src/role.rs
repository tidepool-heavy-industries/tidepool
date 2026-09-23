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
    /// None leaves concurrency unbounded; Some(0) forbids descendants.
    pub maximum_active_children: Option<u16>,
}

/// Render the configured active-child bound for operator-facing observations.
#[must_use]
pub fn render_child_budget(maximum_active_children: Option<u16>) -> String {
    maximum_active_children.map_or_else(|| "unbounded".to_owned(), |children| children.to_string())
}

/// Host-configured ceiling on research subtrees, additionally bounded by the parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResearchPolicy {
    pub default_depth: u16,
    pub maximum_depth: u16,
    pub maximum_active_children: Option<u16>,
}

impl Default for ResearchPolicy {
    fn default() -> Self {
        Self {
            default_depth: 1,
            maximum_depth: 8,
            maximum_active_children: None,
        }
    }
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
    Sleep,
    Notifications,
    Jev,
    Commands,
    Console,
    Actor,
    Reflect,
    Lookup,
    RepoEvent,
    Journal,
    /// Reloading the source layer the holder's own cells compile against. The
    /// root's layer is the run's; an actor launched with a checkout has its
    /// own, captured from that checkout. The key names the verb, never the
    /// layer: which layer a call reaches is fixed when the actor is built, so
    /// holding this key in a checkout cannot reach the run's source.
    Source,
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
            Self::Sleep => "Sleep",
            Self::Notifications => "Notifications",
            Self::Jev => "Jev",
            Self::Commands => "Commands",
            Self::Console => "Console",
            Self::Actor => "Actor",
            Self::Reflect => "Reflect",
            Self::Lookup => "Lookup",
            Self::RepoEvent => "RepoEvent",
            Self::Journal => "Journal",
            Self::Source => "Source",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRole {
    role: ActorRole,
    native_tools: NativeToolClass,
    workspace: WorkspaceAccess,
    descendants: DescendantBudget,
    research_policy: ResearchPolicy,
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
                maximum_active_children: None,
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
                ActorEffectKey::Sleep,
                ActorEffectKey::Notifications,
                ActorEffectKey::Jev,
                ActorEffectKey::Commands,
                ActorEffectKey::Console,
                ActorEffectKey::Actor,
                ActorEffectKey::Reflect,
                ActorEffectKey::Lookup,
                ActorEffectKey::RepoEvent,
                ActorEffectKey::Journal,
                ActorEffectKey::Source,
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
                maximum_active_children: Some(0),
            },
            "research-v2",
            vec![
                ActorEffectKey::Replies,
                ActorEffectKey::Watches,
                ActorEffectKey::Forks,
                ActorEffectKey::ActorContext,
                ActorEffectKey::AgentInspection,
                ActorEffectKey::AgentControl,
                ActorEffectKey::BoundWorktree,
                ActorEffectKey::Sleep,
                ActorEffectKey::Notifications,
                ActorEffectKey::Jev,
                ActorEffectKey::Commands,
                ActorEffectKey::Console,
                ActorEffectKey::Actor,
                ActorEffectKey::Reflect,
                ActorEffectKey::Lookup,
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
                maximum_active_children: Some(0),
            },
            "coding-v3",
            vec![
                ActorEffectKey::Replies,
                ActorEffectKey::Watches,
                ActorEffectKey::Forks,
                ActorEffectKey::ActorContext,
                ActorEffectKey::AgentInspection,
                ActorEffectKey::AgentControl,
                ActorEffectKey::BoundWorktree,
                // A coding actor may allocate worktrees so that a node in a
                // recursive tree can create its own integration worktree and
                // hand it to a record actor it starts (`R.withWorktree`); the
                // grant (`allocate: true`) already followed the role.
                ActorEffectKey::WorktreeAllocation,
                ActorEffectKey::WorktreeIntegration,
                ActorEffectKey::Sleep,
                ActorEffectKey::Notifications,
                ActorEffectKey::Jev,
                ActorEffectKey::Commands,
                ActorEffectKey::Console,
                ActorEffectKey::Actor,
                ActorEffectKey::Reflect,
                ActorEffectKey::Lookup,
                // A coding actor reloads the source layer of the checkout it
                // holds, which is where its own authored Haskell lives. It
                // cannot reach the run's layer: its service is bound to its
                // own checkout when the actor is constructed.
                ActorEffectKey::Source,
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
                maximum_active_children: Some(0),
            },
            "integration-v1",
            vec![
                ActorEffectKey::Replies,
                ActorEffectKey::Watches,
                ActorEffectKey::ActorContext,
                ActorEffectKey::AgentInspection,
                ActorEffectKey::BoundWorktree,
                ActorEffectKey::WorktreeIntegration,
                ActorEffectKey::Sleep,
                ActorEffectKey::Notifications,
                ActorEffectKey::Jev,
                ActorEffectKey::Commands,
                ActorEffectKey::Console,
                ActorEffectKey::Actor,
                ActorEffectKey::Reflect,
                ActorEffectKey::Lookup,
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
            research_policy: ResearchPolicy::default(),
            prompt_profile,
            effect_keys,
        }
    }

    #[must_use]
    pub fn with_research_policy(mut self, policy: ResearchPolicy) -> Self {
        self.research_policy = policy;
        self
    }

    /// Inherit the host policy and spend one generation before applying the role cap.
    /// A descendant cannot refresh an exhausted research allowance by forking again.
    #[must_use]
    pub fn attenuate_child(&self, child: Self) -> Self {
        #[allow(
            clippy::expect_used,
            reason = "child_budget's only fallible step lives inside the \
                      `requested.map(..)` closure, which never runs when \
                      `requested` is None, so this exact call can't fail"
        )]
        self.child_budget(child, None)
            .expect("an omitted budget is valid")
    }

    /// Compute the policy used by context-fork admission without reserving resources.
    pub fn preview_child(
        &self,
        child: Self,
        requested: Option<(i64, i64)>,
    ) -> Result<Self, String> {
        if !child.respects_role_ceiling() {
            return Err("requested effect row exceeds or duplicates the role ceiling".into());
        }
        let child = self.child_budget(child, requested)?;
        if !self.permits_child(&child) {
            return Err("child role or descendant budget exceeds parent authority".into());
        }
        Ok(child)
    }

    fn child_budget(&self, mut child: Self, requested: Option<(i64, i64)>) -> Result<Self, String> {
        let requested = requested
            .map(|(depth, width)| {
                Ok::<_, String>(DescendantBudget {
                    maximum_depth: u16::try_from(depth)
                        .map_err(|_| "fork depth must be in 0..65535")?,
                    maximum_active_children: Some(
                        u16::try_from(width).map_err(|_| "fork width must be in 0..65535")?,
                    ),
                })
            })
            .transpose()?;
        child.research_policy = self.research_policy;
        child.descendants = DescendantBudget {
            maximum_depth: 0,
            maximum_active_children: Some(0),
        };
        if child.effect_keys.contains(&ActorEffectKey::Forks) {
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
            } else if child.role == ActorRole::Research && self.role != ActorRole::Research {
                child.descendants.maximum_depth = child
                    .descendants
                    .maximum_depth
                    .min(self.research_policy.default_depth);
            }
            if child.role == ActorRole::Research {
                child.descendants.maximum_depth = child
                    .descendants
                    .maximum_depth
                    .min(self.research_policy.maximum_depth);
                child.descendants.maximum_active_children = child
                    .descendants
                    .maximum_active_children
                    .into_iter()
                    .chain(self.research_policy.maximum_active_children)
                    .min();
            }
        }
        Ok(child)
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
            && self
                .descendants
                .maximum_active_children
                .is_none_or(|limit| {
                    child
                        .descendants
                        .maximum_active_children
                        .is_some_and(|child_limit| child_limit <= limit)
                })
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
    fn unlimited_concurrency_inherits_without_escaping_finite_authority() {
        let root = EffectiveRole::root();
        let child = root.preview_child(EffectiveRole::coding(), None).unwrap();
        assert_eq!(child.descendants().maximum_active_children, None);
        let research = root.preview_child(EffectiveRole::research(), None).unwrap();
        assert_eq!(research.descendants().maximum_active_children, None);
        let limited = root
            .preview_child(EffectiveRole::coding(), Some((4, 2)))
            .unwrap();
        let grandchild = limited
            .preview_child(EffectiveRole::coding(), None)
            .unwrap();
        assert_eq!(grandchild.descendants().maximum_active_children, Some(2));
        assert!(
            !limited.permits_child(
                &grandchild.clone().with_descendant_budget(DescendantBudget {
                    maximum_depth: 1,
                    maximum_active_children: None,
                })
            )
        );
        assert!(root.permits_child(&child));
    }

    #[test]
    fn accepted_role_is_monotone_across_all_projected_dimensions() {
        let root = EffectiveRole::root();
        assert!(root.permits_child(&EffectiveRole::research()));
        assert!(root.permits_child(&EffectiveRole::coding()));
        let scaffold = EffectiveRole::scaffolding(DescendantBudget {
            maximum_depth: 3,
            maximum_active_children: Some(4),
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
            "'[Replies, Watches, Forks, ActorContext, AgentInspection, AgentControl, BoundWorktree, Sleep, Notifications, Jev, Commands, Console, Actor, Reflect, Lookup]"
        );
        let narrow = EffectiveRole::coding().with_effect_keys(vec![ActorEffectKey::Replies]);
        assert_eq!(narrow.haskell_effects_type(), "'[Replies]");
        assert!(EffectiveRole::root().permits_child(&narrow));
    }

    #[test]
    fn coding_recursion_requires_budget_and_preserves_narrowing() {
        let coding = EffectiveRole::coding().with_descendant_budget(DescendantBudget {
            maximum_depth: 2,
            maximum_active_children: Some(3),
        });
        let child = EffectiveRole::coding().with_descendant_budget(DescendantBudget {
            maximum_depth: 1,
            maximum_active_children: Some(3),
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

    /// `Tidepool.Actors.Role` spells what a child DECLARES; the roles here are
    /// the ceilings those declarations must sit under. The two are different
    /// things — a ceiling is a maximum, not a request — so this checks the
    /// subset direction. Equality would be wrong and was: widening a ceiling
    /// and mirroring it into the Haskell alias made children demand effects
    /// whose handlers the recipe-check environment does not install.
    #[test]
    fn haskell_declared_rows_sit_under_the_rust_ceilings() {
        let source = include_str!("../../../bridge/haskell/actors/Tidepool/Actors/Role.hs");
        fn declared(source: &str, name: &str) -> Vec<String> {
            let start = source
                .find(&format!("type {name} ="))
                .unwrap_or_else(|| panic!("alias {name} missing from Role.hs"));
            let body = &source[start + format!("type {name} =").len()..];
            let end = body.find(']').expect("alias closes");
            body[..end]
                .replace(['\'', '[', '\n'], " ")
                .split(',')
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .collect()
        }
        for (name, role) in [
            ("ResearchEffects", EffectiveRole::research()),
            ("CodingEffects", EffectiveRole::coding()),
            ("IntegrationEffects", EffectiveRole::integration()),
            ("ActorEffects", EffectiveRole::root()),
        ] {
            let ceiling: Vec<&str> = role
                .effect_keys()
                .iter()
                .map(|effect| effect.haskell_name())
                .collect();
            for effect in declared(source, name) {
                assert!(
                    ceiling.contains(&effect.as_str()),
                    "{name} declares {effect}, which is outside the {:?} ceiling",
                    role.role()
                );
            }
        }
    }

    #[test]
    fn journal_is_available_to_the_root_only() {
        assert!(EffectiveRole::root()
            .effect_keys()
            .contains(&ActorEffectKey::Journal));
        assert!(!EffectiveRole::research()
            .effect_keys()
            .contains(&ActorEffectKey::Journal));
        assert!(!EffectiveRole::coding()
            .effect_keys()
            .contains(&ActorEffectKey::Journal));
        assert!(!EffectiveRole::integration()
            .effect_keys()
            .contains(&ActorEffectKey::Journal));
    }

    #[test]
    fn repository_events_are_available_to_the_root_only() {
        assert!(EffectiveRole::root()
            .effect_keys()
            .contains(&ActorEffectKey::RepoEvent));
        assert!(!EffectiveRole::research()
            .effect_keys()
            .contains(&ActorEffectKey::RepoEvent));
        assert!(!EffectiveRole::coding()
            .effect_keys()
            .contains(&ActorEffectKey::RepoEvent));
        assert!(!EffectiveRole::integration()
            .effect_keys()
            .contains(&ActorEffectKey::RepoEvent));
    }

    #[test]
    fn a_project_gate_row_sits_under_every_ceiling_the_root_can_start_it_with() {
        let gate = vec![
            ActorEffectKey::Replies,
            ActorEffectKey::Watches,
            ActorEffectKey::Forks,
            ActorEffectKey::ActorContext,
            ActorEffectKey::AgentInspection,
            ActorEffectKey::BoundWorktree,
            ActorEffectKey::Notifications,
            ActorEffectKey::Jev,
            ActorEffectKey::Commands,
            ActorEffectKey::Actor,
        ];
        let integrator = vec![
            ActorEffectKey::Replies,
            ActorEffectKey::BoundWorktree,
            ActorEffectKey::WorktreeIntegration,
            ActorEffectKey::Commands,
            ActorEffectKey::Actor,
        ];
        let router = vec![
            ActorEffectKey::Replies,
            ActorEffectKey::BoundWorktree,
            ActorEffectKey::WorktreeRegistry,
            ActorEffectKey::RepoEvent,
            ActorEffectKey::Commands,
            ActorEffectKey::Actor,
            ActorEffectKey::Notifications,
            ActorEffectKey::Jev,
            ActorEffectKey::Journal,
            ActorEffectKey::Sleep,
        ];
        let root = EffectiveRole::root();
        for (role, generations) in [
            (EffectiveRole::root(), 7),
            (EffectiveRole::coding(), 7),
            // A gate with no worktree of its own is a research role, and the
            // host's research policy spends it down to one generation — still
            // enough to admit a leaf reviewer.
            (EffectiveRole::research(), 1),
        ] {
            let actor = role.clone().with_effect_keys(gate.clone());
            assert!(
                actor.respects_role_ceiling(),
                "the gate row exceeds the {:?} ceiling",
                role.role()
            );
            let admitted = root
                .preview_child(actor, None)
                .expect("the root admits its own gate actor");
            // `Forks` in the row is what keeps a descendant budget at all: the
            // gate spends one of the root's eight generations and admits its
            // own reviewers out of the rest.
            assert_eq!(admitted.descendants().maximum_depth, generations);
            assert_eq!(admitted.effect_keys(), gate.as_slice());
        }
        for role in [EffectiveRole::root(), EffectiveRole::coding()] {
            let actor = role.clone().with_effect_keys(integrator.clone());
            assert!(
                actor.respects_role_ceiling(),
                "the integrator row exceeds the {:?} ceiling",
                role.role()
            );
            root.preview_child(actor, None)
                .expect("the root admits its own integrator");
        }
        {
            let role = EffectiveRole::root();
            let actor = role.clone().with_effect_keys(router.clone());
            assert!(
                actor.respects_role_ceiling(),
                "the router row exceeds the {:?} ceiling",
                role.role()
            );
            root.preview_child(actor, None)
                .expect("the root admits the journaled event router");
        }
        assert!(
            !EffectiveRole::research()
                .with_effect_keys(integrator)
                .respects_role_ceiling(),
            "an integrator without a worktree would have no integrate authority"
        );

        // The read-only reviewer the gate admits is a leaf one generation
        // below it, and its row must be a SUBSET of the gate's own row: a
        // narrow parent cannot hand out authority it does not hold.
        let gate_actor = root
            .preview_child(
                EffectiveRole::research().with_effect_keys(gate.clone()),
                None,
            )
            .expect("gate admitted");
        let reviewer = gate_actor
            .preview_child(
                EffectiveRole::research().with_effect_keys(vec![
                    ActorEffectKey::Replies,
                    ActorEffectKey::Watches,
                    ActorEffectKey::ActorContext,
                    ActorEffectKey::BoundWorktree,
                    ActorEffectKey::Notifications,
                    ActorEffectKey::Jev,
                    ActorEffectKey::Commands,
                    ActorEffectKey::Actor,
                ]),
                None,
            )
            .expect("the gate admits its own read-only reviewer");
        assert_eq!(
            reviewer.descendants(),
            DescendantBudget {
                maximum_depth: 0,
                maximum_active_children: Some(0)
            }
        );
        assert_eq!(reviewer.workspace(), WorkspaceAccess::InspectOnly);
        assert!(gate_actor.permits_child(&reviewer));
        // A gate without a worktree spends the research policy's one
        // generation on its reviewers: a reviewer that keeps `Forks` is still
        // admitted, but it is a leaf. The tree recurses through nodes that
        // hold worktrees, never through gates.
        let recursive = gate_actor
            .preview_child(
                EffectiveRole::research()
                    .with_effect_keys(vec![ActorEffectKey::Replies, ActorEffectKey::Forks]),
                None,
            )
            .expect("a forking reviewer is still admitted");
        assert_eq!(recursive.descendants().maximum_depth, 0);
        // The full research row is not a subset of the gate's row, so the gate
        // cannot widen a child past itself.
        assert!(gate_actor
            .preview_child(EffectiveRole::research(), None)
            .is_err());
    }

    #[test]
    fn research_policy_caps_subtrees_and_never_refreshes_spent_depth() {
        let root = EffectiveRole::root().with_research_policy(ResearchPolicy {
            maximum_depth: 2,
            maximum_active_children: Some(3),
            ..ResearchPolicy::default()
        });
        let coding = root.attenuate_child(EffectiveRole::coding());
        let research = coding
            .preview_child(EffectiveRole::research(), Some((2, 3)))
            .unwrap();
        assert_eq!(
            research.descendants(),
            DescendantBudget {
                maximum_depth: 2,
                maximum_active_children: Some(3)
            }
        );
        assert_eq!(research.native_tools(), NativeToolClass::InspectionOnly);
        assert_eq!(research.workspace(), WorkspaceAccess::InspectOnly);
        let child = research.attenuate_child(EffectiveRole::research());
        let leaf = child.attenuate_child(EffectiveRole::research());
        assert!(research.permits_child(&child));
        assert!(child.permits_child(&leaf));
        assert_eq!(leaf.descendants().maximum_depth, 0);
        assert!(!leaf.permits_child(&leaf.attenuate_child(EffectiveRole::research())));
        assert!(!research.permits_child(&research.attenuate_child(EffectiveRole::coding())));
        assert!(!research.permits_child(&research.attenuate_child(EffectiveRole::integration())));
        assert!(!research
            .clone()
            .with_effect_keys(vec![ActorEffectKey::WorktreeIntegration])
            .respects_role_ceiling());
    }

    #[test]
    fn research_defaults_allow_one_generation_and_parent_limits_always_win() {
        let root = EffectiveRole::root();
        let research = root.attenuate_child(EffectiveRole::research());
        assert_eq!(research.descendants().maximum_depth, 1);
        assert_eq!(
            research
                .attenuate_child(EffectiveRole::research())
                .descendants()
                .maximum_depth,
            0
        );
        let limited = root.clone().with_descendant_budget(DescendantBudget {
            maximum_depth: 1,
            maximum_active_children: Some(2),
        });
        assert_eq!(
            limited
                .attenuate_child(EffectiveRole::research())
                .descendants(),
            DescendantBudget {
                maximum_depth: 0,
                maximum_active_children: Some(2)
            }
        );
        let disabled = root.with_research_policy(ResearchPolicy {
            maximum_depth: 0,
            maximum_active_children: Some(0),
            ..ResearchPolicy::default()
        });
        assert_eq!(
            disabled
                .attenuate_child(EffectiveRole::research())
                .descendants(),
            DescendantBudget {
                maximum_depth: 0,
                maximum_active_children: Some(0)
            }
        );
        let explicit_leaf = EffectiveRole::research().with_effect_keys(vec![
            ActorEffectKey::Replies,
            ActorEffectKey::Watches,
            ActorEffectKey::ActorContext,
            ActorEffectKey::BoundWorktree,
        ]);
        assert_eq!(
            research.attenuate_child(explicit_leaf).descendants(),
            DescendantBudget {
                maximum_depth: 0,
                maximum_active_children: Some(0)
            }
        );
    }

    #[test]
    fn requested_research_budget_is_previewed_clamped_and_inherited() {
        let root = EffectiveRole::root().with_research_policy(ResearchPolicy {
            default_depth: 1,
            maximum_depth: 3,
            maximum_active_children: Some(4),
        });
        assert_eq!(
            root.preview_child(EffectiveRole::research(), None)
                .unwrap()
                .descendants()
                .maximum_depth,
            1
        );
        let coordinator = root
            .preview_child(EffectiveRole::research(), Some((9, 9)))
            .unwrap();
        assert_eq!(
            coordinator.descendants(),
            DescendantBudget {
                maximum_depth: 3,
                maximum_active_children: Some(4)
            }
        );
        let specialist = coordinator
            .preview_child(EffectiveRole::research(), None)
            .unwrap();
        assert_eq!(specialist.descendants().maximum_depth, 2);
        assert_eq!(
            specialist
                .preview_child(EffectiveRole::research(), Some((99, 99)))
                .unwrap()
                .descendants()
                .maximum_depth,
            1
        );
        assert!(root
            .preview_child(EffectiveRole::research(), Some((-1, 4)))
            .is_err());
        assert!(root
            .preview_child(EffectiveRole::research(), Some((1, 65536)))
            .is_err());
        assert!(coordinator
            .preview_child(EffectiveRole::coding(), Some((1, 1)))
            .is_err());
    }

    #[test]
    fn attenuating_descendants_preserves_every_other_role_dimension() {
        let narrow = EffectiveRole::scaffolding(DescendantBudget {
            maximum_depth: 3,
            maximum_active_children: Some(4),
        })
        .with_effect_keys(vec![ActorEffectKey::Replies, ActorEffectKey::Forks]);
        let attenuated = narrow.clone().with_descendant_budget(DescendantBudget {
            maximum_depth: 2,
            maximum_active_children: Some(4),
        });

        assert_eq!(attenuated.role(), narrow.role());
        assert_eq!(attenuated.native_tools(), narrow.native_tools());
        assert_eq!(attenuated.workspace(), narrow.workspace());
        assert_eq!(attenuated.prompt_profile(), narrow.prompt_profile());
        assert_eq!(attenuated.effect_keys(), narrow.effect_keys());
        assert_eq!(attenuated.descendants().maximum_depth, 2);
    }
}
