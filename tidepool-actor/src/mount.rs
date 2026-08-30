use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_repr::PrincipalId;
use tidepool_runtime::session::{OutputSink, ResidentError, ResidentSession, SessionRunContext};

use crate::{ActorRef, ActorRegistry, ActorRegistryError, ActorTurnKind, TurnLease};

/// Actor-owned portion of a resident machine mount. Machine checkout remains
/// in `tidepool-runtime`; this value prevents scope and authority selection
/// from drifting apart at the actor boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorSessionContext {
    pub actor: ActorRef,
    pub resource_scope: RealmId,
    pub lexical_scope: ScopeId,
}

impl ActorSessionContext {
    #[must_use]
    pub fn run_context(self) -> SessionRunContext {
        SessionRunContext::new(
            self.resource_scope,
            self.lexical_scope,
            PrincipalId::from(self.actor),
        )
    }
}

/// Narrow target seam used to mount the real resident session without moving
/// machine ownership into the actor registry.
pub trait ActorRunTarget {
    type Error;

    fn install_actor_context(&mut self, context: SessionRunContext) -> Result<(), Self::Error>;
}

impl<H, O> ActorRunTarget for ResidentSession<H, O>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    type Error = ResidentError;

    fn install_actor_context(&mut self, context: SessionRunContext) -> Result<(), Self::Error> {
        self.set_run_context(context)
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
    context: ActorSessionContext,
    kind: ActorTurnKind,
) -> Result<TurnLease, MountActorTurnError<Target::Error>>
where
    Target: ActorRunTarget,
{
    let lease = registry.begin_turn(context.actor, kind)?;
    target
        .install_actor_context(context.run_context())
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
        fail: bool,
    }

    impl ActorRunTarget for FakeTarget {
        type Error = &'static str;

        fn install_actor_context(&mut self, context: SessionRunContext) -> Result<(), Self::Error> {
            if self.fail {
                Err("dead scope")
            } else {
                self.installed = Some(context);
                Ok(())
            }
        }
    }

    fn ready_actor(registry: &ActorRegistry) -> ActorRef {
        let starting = registry
            .begin_start(
                None,
                ActorDescriptor {
                    label: "actor".into(),
                    effect_stack: vec![],
                    session: tidepool_repr::SessionId(1),
                },
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
        let context = ActorSessionContext {
            actor,
            resource_scope: RealmId(11),
            lexical_scope: ScopeId::ROOT,
        };
        let lease = mount_actor_turn(&registry, &mut target, context, ActorTurnKind::Haskell)
            .expect("mount actor");

        assert_eq!(target.installed, Some(context.run_context()));
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
            fail: true,
        };
        let result = mount_actor_turn(
            &registry,
            &mut target,
            ActorSessionContext {
                actor,
                resource_scope: RealmId(11),
                lexical_scope: ScopeId::ROOT,
            },
            ActorTurnKind::Haskell,
        );
        assert!(matches!(
            result,
            Err(MountActorTurnError::Target("dead scope"))
        ));
        registry
            .begin_turn(actor, ActorTurnKind::Provider)
            .expect("failed mount released lease");
    }
}
