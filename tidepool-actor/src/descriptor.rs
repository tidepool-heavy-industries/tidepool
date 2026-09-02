use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};

use crate::{
    ActorEffectProfile, ActorPlacement, ActorRef, ActorSessionContext, ActorSourceImports,
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
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn profile(&self) -> ActorEffectProfile {
        self.profile
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
        }
    }
}
