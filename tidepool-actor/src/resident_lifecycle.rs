//! Rust-owned terminal cleanup for resident actor subtrees.
//!
//! Cooperative typed shutdown will run before this boundary. This component
//! is the authoritative fallback: publish terminal lifecycle first so no new
//! work can enter, then close every captured machine realm even if one cleanup
//! attempt fails.

use tidepool_effect::dispatch::DispatchEffect;
use tidepool_runtime::session::OutputSink;

use crate::{
    ActorRef, ActorRegistry, ActorRegistryError, ActorTerminal, ResidentActorRunner,
    ResidentActorWorkbenchError,
};

#[derive(Debug, thiserror::Error)]
pub enum ResidentLifecycleError {
    #[error(transparent)]
    Registry(#[from] ActorRegistryError),
    #[error("failed to close resident resources for actor {actor:?}: {source}")]
    Cleanup {
        actor: ActorRef,
        #[source]
        source: ResidentActorWorkbenchError,
    },
}

pub struct ResidentActorLifecycle<H, O> {
    registry: ActorRegistry,
    runner: ResidentActorRunner<H, O>,
}

impl<H, O> ResidentActorLifecycle<H, O> {
    #[must_use]
    pub fn new(registry: ActorRegistry, runner: ResidentActorRunner<H, O>) -> Self {
        Self { registry, runner }
    }
}

impl<H, O> ResidentActorLifecycle<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// Force one exact actor and its owned subtree terminal, then close every
    /// resident resource realm. All realms are attempted; the first cleanup
    /// error is returned only after the rest have run.
    pub async fn force_terminate(
        &self,
        actor: ActorRef,
        terminal: ActorTerminal,
    ) -> Result<(), ResidentLifecycleError> {
        let mut pending = vec![actor];
        let mut actors = Vec::new();
        while let Some(current) = pending.pop() {
            pending.extend(self.registry.children(current)?);
            actors.push((current, self.registry.session_context(current)?));
        }

        self.registry.finish(actor, terminal)?;

        let mut first_error = None;
        for (current, context) in actors.into_iter().rev() {
            let realm = context.placement.resource_scope;
            if let Err(source) = self.runner.close_realm(context, realm).await {
                if first_error.is_none() {
                    first_error = Some(ResidentLifecycleError::Cleanup {
                        actor: current,
                        source,
                    });
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
