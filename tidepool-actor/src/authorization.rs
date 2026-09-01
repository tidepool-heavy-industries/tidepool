use tidepool_effect::{EffectContext, EffectError, EffectHandler, Response};
use tidepool_repr::PrincipalId;

use crate::{ActorEffectProfile, ActorRef, ActorRegistry};

/// Nominal operation families governed by an actor effect profile.
///
/// Concrete request decoding remains the inner handler's responsibility. This
/// classification is selected where that typed handler is composed; neither a
/// freer-simple union position nor rendered request text participates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorOperationClass {
    FsRead,
    FsWrite,
}

/// A profile decision for one exact execution principal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActorEffectRefusal {
    #[error("the system principal has no actor effect profile")]
    SystemPrincipal,
    #[error("actor principal {principal:?} is unknown")]
    Unknown { principal: PrincipalId },
    #[error("actor principal {given:?} is stale; current incarnation is {current:?}")]
    Stale { given: ActorRef, current: ActorRef },
    #[error("actor {actor:?} has exited")]
    Exited { actor: ActorRef },
    #[error("actor {actor:?} with profile {profile:?} may not perform {operation:?}")]
    ProfileDenied {
        actor: ActorRef,
        profile: ActorEffectProfile,
        operation: ActorOperationClass,
    },
}

/// Decorate one nominal effect handler with the actor registry's profile gate.
///
/// The wrapper is deliberately oblivious to the request representation. HList
/// dispatch first recognizes and decodes `H::Request`; only then does this
/// wrapper authorize the operation class assigned at the composition root.
#[derive(Clone)]
pub struct ActorProfileHandler<H> {
    registry: ActorRegistry,
    operation: ActorOperationClass,
    inner: H,
}

impl<H> ActorProfileHandler<H> {
    #[must_use]
    pub fn new(registry: ActorRegistry, operation: ActorOperationClass, inner: H) -> Self {
        Self {
            registry,
            operation,
            inner,
        }
    }

    #[must_use]
    pub fn into_inner(self) -> H {
        self.inner
    }
}

impl<U, H> EffectHandler<U> for ActorProfileHandler<H>
where
    H: EffectHandler<U>,
{
    type Request = H::Request;

    fn handle(
        &mut self,
        request: Self::Request,
        context: &EffectContext<'_, U>,
    ) -> Result<Response, EffectError> {
        self.registry
            .authorize_effect(context.principal(), self.operation)
            .map_err(|error| EffectError::Handler(error.to_string()))?;
        self.inner.handle(request, context)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
    use tidepool_eval::Value;
    use tidepool_repr::{DataConTable, Literal, PrincipalId, SessionId};

    use super::*;
    use crate::{ActorDescriptor, ActorExitKind, ActorPlacement, ActorTerminal, StartInitiator};

    fn descriptor(label: &str, profile: ActorEffectProfile) -> ActorDescriptor {
        ActorDescriptor::new(
            label,
            profile.effect_names().iter().copied(),
            ActorPlacement {
                session: SessionId(1),
                resource_scope: RealmId::ROOT,
                lexical_scope: ScopeId::ROOT,
            },
        )
        .with_profile(profile)
    }

    fn ready_actor(registry: &ActorRegistry, profile: ActorEffectProfile) -> ActorRef {
        let starting = registry
            .begin_start(
                None,
                descriptor("profile-test", profile),
                StartInitiator::Runtime,
            )
            .expect("allocate actor");
        registry.publish_ready(starting).expect("publish actor")
    }

    #[test]
    fn profile_decision_table_is_principal_and_lifecycle_aware() {
        let registry = ActorRegistry::new();
        let initializing = registry
            .begin_start(
                None,
                descriptor("initializing", ActorEffectProfile::ReadOnly),
                StartInitiator::Runtime,
            )
            .expect("allocate initializing actor");
        let initializing_actor = initializing.actor();
        assert_eq!(
            registry.authorize_effect(initializing_actor.into(), ActorOperationClass::FsRead),
            Ok(())
        );
        let reader = registry
            .publish_ready(initializing)
            .expect("publish reader");
        let writer = ready_actor(&registry, ActorEffectProfile::ReadWrite);

        assert_eq!(
            registry.authorize_effect(reader.into(), ActorOperationClass::FsRead),
            Ok(())
        );
        assert!(matches!(
            registry.authorize_effect(reader.into(), ActorOperationClass::FsWrite),
            Err(ActorEffectRefusal::ProfileDenied { actor, .. }) if actor == reader
        ));
        assert_eq!(
            registry.authorize_effect(writer.into(), ActorOperationClass::FsRead),
            Ok(())
        );
        assert_eq!(
            registry.authorize_effect(writer.into(), ActorOperationClass::FsWrite),
            Ok(())
        );
        assert_eq!(
            registry.authorize_effect(PrincipalId::SYSTEM, ActorOperationClass::FsRead),
            Err(ActorEffectRefusal::SystemPrincipal)
        );
        assert!(matches!(
            registry.authorize_effect(PrincipalId::new(999, 1), ActorOperationClass::FsRead),
            Err(ActorEffectRefusal::Unknown { .. })
        ));
        assert!(matches!(
            registry.authorize_effect(
                PrincipalId::new(reader.id.0, reader.incarnation.0 + 1),
                ActorOperationClass::FsRead,
            ),
            Err(ActorEffectRefusal::Stale { current, .. }) if current == reader
        ));

        registry
            .finish(
                writer,
                ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "done".into(),
                },
            )
            .expect("finish writer");
        assert_eq!(
            registry.authorize_effect(writer.into(), ActorOperationClass::FsRead),
            Err(ActorEffectRefusal::Exited { actor: writer })
        );
    }

    struct CountingHandler(Arc<AtomicUsize>);

    impl EffectHandler for CountingHandler {
        type Request = Value;

        fn handle(
            &mut self,
            request: Self::Request,
            _context: &EffectContext<'_>,
        ) -> Result<Response, EffectError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Response::Complete(request))
        }
    }

    #[test]
    fn denied_wrapper_never_delegates_to_the_concrete_handler() {
        let registry = ActorRegistry::new();
        let reader = ready_actor(&registry, ActorEffectProfile::ReadOnly);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut handler = ActorProfileHandler::new(
            registry,
            ActorOperationClass::FsWrite,
            CountingHandler(Arc::clone(&calls)),
        );
        let table = DataConTable::new();
        let context = EffectContext::with_principal(&table, reader.into(), &());

        assert!(handler
            .handle(Value::Lit(Literal::LitInt(0)), &context)
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
