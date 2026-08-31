use std::path::{Path, PathBuf};

use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{Generation, PrincipalId, SessionId};
use tidepool_runtime::session::{
    MaterializedFacade, OutputSink, ResidentError, ResidentSession, SessionCompileView,
    SessionRunContext, SourceImports,
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
pub struct ActorSourceImports {
    imports: SourceImports,
}

impl ActorSourceImports {
    #[must_use]
    pub fn from_exact_facades<'a>(
        facades: impl IntoIterator<Item = &'a MaterializedFacade>,
    ) -> Self {
        let facades: Vec<_> = facades.into_iter().collect();
        Self {
            imports: SourceImports::from_specs(facades.iter().map(|facade| facade.module_name())),
        }
    }
}

/// An actor's exact, owned source-side compilation snapshot.
///
/// Construction validates the session and lexical scope against the registry
/// context before pairing them with the actor's explicit facade imports. A
/// compiler can therefore consume this value without separately carrying an
/// ambient session view or caller-selected import set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorCompileView {
    session: SessionCompileView,
    external: SourceImports,
}

impl ActorCompileView {
    /// Extend this exact actor view with modules GHC named while describing a
    /// typed suspension. These are compiler-derived type dependencies, not an
    /// authored escape hatch around [`ActorSourceImports`]. Returning another
    /// immutable view keeps turn compilation and persisted declaration source
    /// on the same import set.
    pub(crate) fn with_type_modules(mut self, modules: &[String]) -> Self {
        for module in modules {
            self.external.extend_text(module);
        }
        self
    }

    /// Exact external, declaration, and live-value imports for a turn template.
    #[must_use]
    pub fn turn_imports(&self) -> String {
        self.session.turn_imports(&self.external)
    }

    /// Preserve only explicit actor imports with a declaration. Session
    /// ancestry is supplied by the declaration plane itself.
    #[must_use]
    pub fn declaration_source(&self, body: &str) -> String {
        self.external.declaration_source(body)
    }

    #[must_use]
    pub fn include_paths(&self, base: &[PathBuf]) -> Vec<PathBuf> {
        self.session.include_paths(base)
    }

    #[must_use]
    pub fn session_root(&self) -> &Path {
        self.session.session_root()
    }

    #[must_use]
    pub fn injected_module_names(&self) -> Vec<String> {
        self.session.injected_module_names()
    }

    #[must_use]
    pub fn next_value_generation(&self) -> Generation {
        self.session.next_value_generation()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActorCompileViewError {
    #[error("actor source view belongs to session {actual:?}, expected {expected:?}")]
    WrongSession {
        expected: SessionId,
        actual: SessionId,
    },
    #[error("actor source view belongs to lexical scope {actual:?}, expected {expected:?}")]
    WrongLexicalScope { expected: ScopeId, actual: ScopeId },
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

    /// Bind an immutable session snapshot to this actor's exact import
    /// membrane. A parent, sibling, or foreign-session view is rejected before
    /// any source is rendered.
    pub fn compile_view(
        &self,
        session: SessionCompileView,
    ) -> Result<ActorCompileView, ActorCompileViewError> {
        if session.session() != self.placement.session {
            return Err(ActorCompileViewError::WrongSession {
                expected: self.placement.session,
                actual: session.session(),
            });
        }
        if session.lexical_scope() != self.placement.lexical_scope {
            return Err(ActorCompileViewError::WrongLexicalScope {
                expected: self.placement.lexical_scope,
                actual: session.lexical_scope(),
            });
        }
        Ok(ActorCompileView {
            session,
            external: self.source_imports.imports.clone(),
        })
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

pub(crate) fn install_actor_context<Target>(
    target: &mut Target,
    context: &ActorSessionContext,
) -> Result<(), Target::Error>
where
    Target: ActorRunTarget,
{
    target.install_actor_execution(
        context.run_context(),
        context.effect_policy,
        context.live_payload,
    )
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
    install_actor_context(target, &context).map_err(MountActorTurnError::Target)?;
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
                ActorDescriptor::new(
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
        assert_eq!(context.effect_policy, EffectRunPolicy::HandleOrSuspend);
        assert_eq!(
            context.live_payload,
            tidepool_effect::LivePayloadPolicy::HASKELL_EFFECT_VALUE
        );
        assert!(matches!(
            registry.begin_turn(actor, ActorTurnKind::AgentSession),
            Err(ActorRegistryError::Busy { .. })
        ));
        drop(lease);
        registry
            .begin_turn(actor, ActorTurnKind::AgentSession)
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
            .begin_turn(actor, ActorTurnKind::AgentSession)
            .expect("failed mount released lease");
    }
}
