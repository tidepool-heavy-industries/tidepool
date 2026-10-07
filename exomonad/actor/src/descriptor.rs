use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};

use crate::{
    ActorCapabilities, ActorEffectProfile, ActorPlacement, ActorRef, ActorSessionContext,
    ActorSourceImports,
};

/// The host or lineage owner chooses persistence independently of actor names
/// and paths. Durable actors require an initialized public surface before use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ActorPersistencePolicy {
    #[default]
    Ephemeral,
    Durable,
}

/// Immutable execution attributes selected before an actor is spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorDescriptor {
    label: String,
    profile: ActorEffectProfile,
    effect_policy: EffectRunPolicy,
    live_payload: LivePayloadPolicy,
    placement: ActorPlacement,
    source_imports: ActorSourceImports,
    capabilities: ActorCapabilities,
    creator: Option<ActorRef>,
    supervisor_parent: Option<ActorRef>,
    context_parent: Option<ActorRef>,
    actor_path: Option<tidepool_repr::ActorPath>,
    persistence_policy: ActorPersistencePolicy,
    fork_group: Option<crate::ForkGroupId>,
    fork_effort: Option<crate::ForkEffort>,
    model: Option<crate::Model>,
    instructions: Option<String>,
    fork_budget: Option<(i64, i64)>,
    fork_boundary: Option<tidepool_runtime::session::WorkbenchForkBoundary>,
    checkpoint_token: Option<String>,
    source_layer: std::sync::Arc<[std::path::PathBuf]>,
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
            capabilities: ActorCapabilities::default(),
            creator: None,
            supervisor_parent: None,
            context_parent: None,
            actor_path: None,
            persistence_policy: ActorPersistencePolicy::Ephemeral,
            fork_group: None,
            fork_effort: None,
            model: None,
            instructions: None,
            fork_budget: None,
            fork_boundary: None,
            checkpoint_token: None,
            source_layer: std::sync::Arc::from([]),
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn persistence_policy(&self) -> ActorPersistencePolicy {
        self.persistence_policy
    }

    #[must_use]
    pub fn with_persistence_policy(mut self, policy: ActorPersistencePolicy) -> Self {
        self.persistence_policy = policy;
        self
    }

    #[must_use]
    pub fn model(&self) -> Option<&crate::Model> {
        self.model.as_ref()
    }

    #[must_use]
    pub fn model_name(&self) -> Option<&str> {
        self.model.as_ref().map(crate::Model::value)
    }

    #[must_use]
    pub fn with_model(mut self, model: Option<crate::Model>) -> Self {
        self.model = model;
        self
    }

    /// Authored behavior; does not grant runtime authority.
    #[must_use]
    pub fn instructions(&self) -> Option<&str> {
        self.instructions.as_deref()
    }

    #[must_use]
    pub fn with_instructions(mut self, instructions: Option<String>) -> Self {
        self.instructions = instructions;
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

    pub fn checkpoint_token(&self) -> Option<&str> {
        self.checkpoint_token.as_deref()
    }

    pub(crate) fn with_checkpoint_token(mut self, token: Option<String>) -> Self {
        self.checkpoint_token = token;
        self
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
    pub fn capabilities(&self) -> &ActorCapabilities {
        &self.capabilities
    }

    #[must_use]
    pub fn with_capabilities(mut self, capabilities: ActorCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Root identity follows ancestry, independently of available effects.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.creator.is_none()
            && self.supervisor_parent.is_none()
            && self.context_parent.is_none()
            && self.actor_path.as_ref().is_some_and(
                |path| matches!(path.segments(), [segment] if segment.as_str() == "root"),
            )
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
    pub fn creator(&self) -> Option<ActorRef> {
        self.creator
    }

    #[must_use]
    pub fn with_creator(mut self, actor: ActorRef) -> Self {
        self.creator = Some(actor);
        self
    }

    #[must_use]
    pub fn with_supervisor_parent(mut self, parent: impl Into<Option<ActorRef>>) -> Self {
        self.supervisor_parent = parent.into();
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

    /// Give this actor private helper include roots ahead of the shared run
    /// graph. Selected once before the actor is spawned.
    #[must_use]
    pub fn with_source_layer(mut self, layer: Vec<std::path::PathBuf>) -> Self {
        self.source_layer = layer.into();
        self
    }

    /// Replace the placement's lexical scope alone, after construction — for
    /// an eligible `SelectedContext` launch whose entry crosses to a fresh
    /// child session (`crate::resident_actor`'s `try_start_child`, once
    /// `ResidentActorRunner::provision_child_session` has built it): the
    /// scope minted at capture time belonged to the PARENT's scope forest,
    /// never valid on the child's own session, so the child's actual
    /// lexical scope (minted on the child, once it exists) replaces it here
    /// before the actor admits. Every other placement field is unaffected.
    #[must_use]
    pub(crate) fn with_lexical_scope(mut self, scope: tidepool_codegen::scope::ScopeId) -> Self {
        self.placement.lexical_scope = scope;
        self
    }

    /// Replace the placement's session alone — for a launch
    /// `child_session_eligibility` marked eligible for its own machine
    /// (and so already minted a fresh session id for at capture time), but
    /// whose host offers no dedicated-machine primitive at all
    /// (`ResidentActorRunner::supports_child_sessions` false: no factory,
    /// no bootstrap program installed). Falls back to the launching
    /// session exactly as every launch behaved before per-actor machines,
    /// rather than failing the whole launch over a host capability nothing
    /// asked for. The resource scope and lexical scope minted at capture
    /// time are unaffected: for an ordinary (non-context-fork) launch they
    /// were already minted on the checked-out PARENT session, so they stay
    /// valid once the placement's session reverts to match it.
    #[must_use]
    pub(crate) fn with_session(mut self, session: tidepool_repr::SessionId) -> Self {
        self.placement.session = session;
        self
    }

    #[must_use]
    pub fn source_layer(&self) -> &[std::path::PathBuf] {
        &self.source_layer
    }

    #[must_use]
    pub fn session_context(&self, actor: ActorRef) -> ActorSessionContext {
        ActorSessionContext {
            actor,
            placement: self.placement,
            effect_policy: self.effect_policy,
            live_payload: self.live_payload,
            source_imports: self.source_imports.clone(),
            haskell_effects_alias: self.capabilities.haskell_effects_type(),
            source_layer: self.source_layer.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> ActorDescriptor {
        ActorDescriptor::new(
            "an arbitrary human label",
            ActorPlacement {
                session: tidepool_repr::SessionId(1),
                lexical_scope: tidepool_codegen::scope::ScopeId(1),
                resource_scope: tidepool_codegen::suspension::RealmId(1),
            },
        )
    }

    #[test]
    fn root_identity_requires_canonical_path_and_independent_ancestry() {
        let unbound = descriptor();
        assert!(!unbound.is_root());
        let root = unbound
            .with_actor_path(tidepool_repr::ActorPath::parse("root").unwrap())
            .with_capabilities(ActorCapabilities::default().with_effect_keys(Vec::new()));
        assert!(
            root.is_root(),
            "effect availability does not identify the root"
        );
        let parent = ActorRef::first(crate::ActorId(1));
        assert!(!root.clone().with_creator(parent).is_root());
        assert!(!root.clone().with_supervisor_parent(parent).is_root());
        assert!(!root.with_context_parent(parent).is_root());
    }
}
