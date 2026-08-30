use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{PrincipalId, SessionId};
use tidepool_runtime::session::{
    MaterializedFacade, OutputSink, ResidentError, ResidentSession, SessionRunContext,
    SourceImports,
};

use crate::{ActorRef, ActorRegistry, ActorRegistryError, ActorTurnKind, TurnLease};

/// Immutable location of one actor incarnation in the resident Haskell
/// machine. The registry owns this mapping; turn callers select an actor, not
/// an independently assembled set of scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorPlacement {
    pub session: SessionId,
    pub resource_scope: RealmId,
    pub lexical_scope: ScopeId,
}

/// Exact model-visible declaration imports for one actor incarnation.
///
/// The only public constructor accepts materialized exact-export facades, so
/// actor startup cannot smuggle an ambient parent module or arbitrary import
/// string across the fresh-scope boundary. Standard Tidepool imports remain
/// part of the shared turn template rather than this actor-local membrane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActorSourceImports(SourceImports);

impl ActorSourceImports {
    #[must_use]
    pub fn from_exact_facades<'a>(
        facades: impl IntoIterator<Item = &'a MaterializedFacade>,
    ) -> Self {
        Self(SourceImports::from_specs(
            facades.into_iter().map(MaterializedFacade::module_name),
        ))
    }

    #[must_use]
    pub fn source_imports(&self) -> &SourceImports {
        &self.0
    }
}

/// Actor-owned portion of a resident machine mount. Machine checkout remains
/// in `tidepool-runtime`; this value prevents scope and authority selection
/// from drifting apart at the actor boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorSessionContext {
    pub actor: ActorRef,
    pub placement: ActorPlacement,
    pub effect_policy: EffectRunPolicy,
    pub live_payload: LivePayloadPolicy,
    pub source_imports: ActorSourceImports,
}

impl ActorSessionContext {
    #[must_use]
    pub fn run_context(&self) -> SessionRunContext {
        SessionRunContext::new(
            self.placement.resource_scope,
            self.placement.lexical_scope,
            PrincipalId::from(self.actor),
        )
    }
}

/// Narrow target seam used to mount the real resident session without moving
/// machine ownership into the actor registry.
pub trait ActorRunTarget {
    type Error;

    fn install_actor_execution(
        &mut self,
        context: SessionRunContext,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
    ) -> Result<(), Self::Error>;
}

impl<H, O> ActorRunTarget for ResidentSession<H, O>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    type Error = ResidentError;

    fn install_actor_execution(
        &mut self,
        context: SessionRunContext,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
    ) -> Result<(), Self::Error> {
        self.set_actor_execution(context, effect_policy, live_payload)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MountActorTurnError<TargetError> {
    #[error(transparent)]
    Registry(#[from] ActorRegistryError),
    #[error("resident target rejected actor context: {0}")]
    Target(TargetError),
}

/// Atomically, from the caller's perspective, admit one actor turn and install
/// its scopes/principal on a checked-out resident target. If context
/// installation fails, the local lease drops before the error escapes.
pub fn mount_actor_turn<Target>(
    registry: &ActorRegistry,
    target: &mut Target,
    actor: ActorRef,
    kind: ActorTurnKind,
) -> Result<TurnLease, MountActorTurnError<Target::Error>>
where
    Target: ActorRunTarget,
{
    let lease = registry.begin_turn(actor, kind)?;
    let context = lease.session_context();
    target
        .install_actor_execution(
            context.run_context(),
            context.effect_policy,
            context.live_payload,
        )
        .map_err(MountActorTurnError::Target)?;
    Ok(lease)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorDescriptor, StartInitiator};

    #[derive(Default)]
    struct FakeTarget {
        installed: Option<SessionRunContext>,
        effect_policy: Option<EffectRunPolicy>,
        live_payload: Option<LivePayloadPolicy>,
        fail: bool,
    }

    impl ActorRunTarget for FakeTarget {
        type Error = &'static str;

        fn install_actor_execution(
            &mut self,
            context: SessionRunContext,
            effect_policy: EffectRunPolicy,
            live_payload: LivePayloadPolicy,
        ) -> Result<(), Self::Error> {
            if self.fail {
                Err("dead scope")
            } else {
                self.installed = Some(context);
                self.effect_policy = Some(effect_policy);
                self.live_payload = Some(live_payload);
                Ok(())
            }
        }
    }

    fn ready_actor(registry: &ActorRegistry) -> ActorRef {
        let starting = registry
            .begin_start(
                None,
                ActorDescriptor::all_suspended(
                    "actor",
                    std::iter::empty::<String>(),
                    ActorPlacement {
                        session: tidepool_repr::SessionId(1),
                        resource_scope: RealmId(11),
                        lexical_scope: ScopeId::ROOT,
                    },
                ),
                StartInitiator::Runtime,
            )
            .expect("begin startup");
        registry.publish_ready(starting).expect("publish actor")
    }

    #[test]
    fn installs_exact_actor_principal_and_holds_turn_lease() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let mut target = FakeTarget::default();
        let context = registry.session_context(actor).expect("actor context");
        let lease = mount_actor_turn(&registry, &mut target, actor, ActorTurnKind::Haskell)
            .expect("mount actor");

        assert_eq!(target.installed, Some(context.run_context()));
        assert_eq!(target.effect_policy, Some(context.effect_policy));
        assert_eq!(target.live_payload, Some(context.live_payload));
        assert_eq!(context.effect_policy, EffectRunPolicy::SuspendAll);
        assert_eq!(
            context.live_payload,
            tidepool_effect::LivePayloadPolicy::HASKELL_EFFECT_VALUE
        );
        assert!(matches!(
            registry.begin_turn(actor, ActorTurnKind::Provider),
            Err(ActorRegistryError::Busy { .. })
        ));
        drop(lease);
        registry
            .begin_turn(actor, ActorTurnKind::Provider)
            .expect("lease released");
    }

    #[test]
    fn target_rejection_releases_actor_admission() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let mut target = FakeTarget {
            installed: None,
            effect_policy: None,
            live_payload: None,
            fail: true,
        };
        let result = mount_actor_turn(&registry, &mut target, actor, ActorTurnKind::Haskell);
        assert!(matches!(
            result,
            Err(MountActorTurnError::Target("dead scope"))
        ));
        registry
            .begin_turn(actor, ActorTurnKind::Provider)
            .expect("failed mount released lease");
    }
}
