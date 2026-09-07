use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};

use crate::{
    ActorEffectProfile, ActorPlacement, ActorRef, ActorSessionContext, ActorSourceImports,
    EffectiveRole,
};

/// Immutable execution attributes selected before an actor is spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorDescriptor {
    label: String,
    profile: ActorEffectProfile,
    effect_policy: EffectRunPolicy,
    live_payload: LivePayloadPolicy,
    placement: ActorPlacement,
    source_imports: ActorSourceImports,
    role: EffectiveRole,
    supervisor_parent: Option<ActorRef>,
    context_parent: Option<ActorRef>,
    actor_path: Option<tidepool_repr::ActorPath>,
    fork_group: Option<crate::ForkGroupId>,
    fork_effort: Option<crate::ForkEffort>,
    model: Option<String>,
    fork_budget: Option<(i64, i64)>,
    fork_boundary: Option<tidepool_runtime::session::WorkbenchForkBoundary>,
}

impl ActorDescriptor {
    #[must_use]
    pub fn new(label: impl Into<String>, placement: ActorPlacement) -> Self {
        Self {
            label: label.into(),
            profile: ActorEffectProfile::ReadWrite,
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            placement,
            source_imports: ActorSourceImports::default(),
            role: EffectiveRole::coding(),
            supervisor_parent: None,
            context_parent: None,
            actor_path: None,
            fork_group: None,
            fork_effort: None,
            model: None,
            fork_budget: None,
            fork_boundary: None,
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    #[must_use]
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    #[must_use]
    pub fn fork_effort(&self) -> Option<crate::ForkEffort> {
        self.fork_effort
    }

    pub(crate) fn fork_budget(&self) -> Option<(i64, i64)> {
        self.fork_budget
    }

    pub(crate) fn with_fork_budget(mut self, budget: Option<(i64, i64)>) -> Self {
        self.fork_budget = budget;
        self
    }

    #[must_use]
    pub fn fork_boundary(&self) -> Option<&tidepool_runtime::session::WorkbenchForkBoundary> {
        self.fork_boundary.as_ref()
    }

    #[must_use]
    pub(crate) fn with_fork_boundary(
        mut self,
        boundary: Option<tidepool_runtime::session::WorkbenchForkBoundary>,
    ) -> Self {
        self.fork_boundary = boundary;
        self
    }

    #[must_use]
    pub fn with_fork_effort(mut self, effort: Option<crate::ForkEffort>) -> Self {
        self.fork_effort = effort;
        self
    }

    #[must_use]
    pub fn profile(&self) -> ActorEffectProfile {
        self.profile
    }

    #[must_use]
    pub fn effective_role(&self) -> &EffectiveRole {
        &self.role
    }

    #[must_use]
    pub fn with_effective_role(mut self, role: EffectiveRole) -> Self {
        self.role = role;
        self
    }

    #[must_use]
    pub fn context_parent(&self) -> Option<ActorRef> {
        self.context_parent
    }

    /// The actor that owns this actor's lifecycle, whether or not model
    /// context was inherited from it.
    #[must_use]
    pub fn supervisor_parent(&self) -> Option<ActorRef> {
        self.supervisor_parent
    }

    #[must_use]
    pub fn with_supervisor_parent(mut self, parent: ActorRef) -> Self {
        self.supervisor_parent = Some(parent);
        self
    }

    #[must_use]
    pub fn with_context_parent(mut self, parent: ActorRef) -> Self {
        self.context_parent = Some(parent);
        self
    }

    #[must_use]
    pub fn actor_path(&self) -> Option<&tidepool_repr::ActorPath> {
        self.actor_path.as_ref()
    }

    #[must_use]
    pub fn with_actor_path(mut self, path: tidepool_repr::ActorPath) -> Self {
        self.label = path.to_string();
        self.actor_path = Some(path);
        self
    }

    #[must_use]
    pub fn fork_group(&self) -> Option<crate::ForkGroupId> {
        self.fork_group
    }

    #[must_use]
    pub fn with_fork_group(mut self, group: crate::ForkGroupId) -> Self {
        self.fork_group = Some(group);
        self
    }

    #[must_use]
    pub fn with_profile(mut self, profile: ActorEffectProfile) -> Self {
        self.profile = profile;
        self
    }

    #[must_use]
    pub fn effect_policy(&self) -> EffectRunPolicy {
        self.effect_policy
    }

    #[must_use]
    pub fn live_payload_policy(&self) -> LivePayloadPolicy {
        self.live_payload
    }

    pub(crate) fn set_lexical_scope(&mut self, scope: tidepool_codegen::scope::ScopeId) {
        self.placement.lexical_scope = scope;
    }

    #[must_use]
    pub fn placement(&self) -> ActorPlacement {
        self.placement
    }

    #[must_use]
    pub fn with_source_imports(mut self, source_imports: ActorSourceImports) -> Self {
        self.source_imports = source_imports;
        self
    }

    #[must_use]
    pub fn source_imports(&self) -> &ActorSourceImports {
        &self.source_imports
    }

    #[must_use]
    pub fn session_context(&self, actor: ActorRef) -> ActorSessionContext {
        ActorSessionContext {
            actor,
            placement: self.placement,
            effect_policy: self.effect_policy,
            live_payload: self.live_payload,
            source_imports: self.source_imports.clone(),
            haskell_effects_alias: self.role.haskell_effects_type(),
        }
    }
}
