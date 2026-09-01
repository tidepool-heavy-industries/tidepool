//! Rust-owned terminal cleanup for resident actor subtrees.
//!
//! This component publishes terminal lifecycle first so no new work can enter,
//! runs each captured cooperative shutdown hook, then authoritatively closes
//! every machine realm even if a hook or earlier cleanup attempt fails.

use std::time::Duration;

use tidepool_effect::dispatch::DispatchEffect;
use tidepool_runtime::session::OutputSink;

use crate::{
    ActorRef, ActorRegistry, ActorRegistryError, ActorTerminal, ResidentActorRunner,
    ResidentActorWorkbenchError,
};

const DEFAULT_SHUTDOWN_ADMISSION_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentLifecyclePolicy {
    shutdown_admission_timeout: Duration,
}

impl ResidentLifecyclePolicy {
    #[must_use]
    pub fn new(shutdown_admission_timeout: Duration) -> Self {
        Self {
            shutdown_admission_timeout,
        }
    }
}

impl Default for ResidentLifecyclePolicy {
    fn default() -> Self {
        Self::new(DEFAULT_SHUTDOWN_ADMISSION_TIMEOUT)
    }
}

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
    #[error("cooperative shutdown failed for actor {actor:?}: {source}")]
    Shutdown {
        actor: ActorRef,
        #[source]
        source: ResidentActorWorkbenchError,
    },
}

pub struct ResidentActorLifecycle<H, O> {
    registry: ActorRegistry,
    runner: ResidentActorRunner<H, O>,
    policy: ResidentLifecyclePolicy,
}

impl<H, O> ResidentActorLifecycle<H, O> {
    #[must_use]
    pub fn new(registry: ActorRegistry, runner: ResidentActorRunner<H, O>) -> Self {
        Self::with_policy(registry, runner, ResidentLifecyclePolicy::default())
    }

    #[must_use]
    pub fn with_policy(
        registry: ActorRegistry,
        runner: ResidentActorRunner<H, O>,
        policy: ResidentLifecyclePolicy,
    ) -> Self {
        Self {
            registry,
            runner,
            policy,
        }
    }

    pub(crate) fn registry(&self) -> ActorRegistry {
        self.registry.clone()
    }

    pub(crate) fn runner(&self) -> ResidentActorRunner<H, O> {
        self.runner.clone()
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
        let actors = self.registry.finish_for_cleanup(actor, terminal)?;
        self.cleanup(actors).await
    }

    pub(crate) async fn abort_starting(
        &self,
        starting: crate::StartingActor,
        terminal: ActorTerminal,
    ) -> Result<(), ResidentLifecycleError> {
        let cleanup = self.registry.abort_start_for_cleanup(starting, terminal)?;
        self.cleanup(cleanup).await
    }

    pub(crate) async fn cleanup_terminal(
        &self,
        cleanup: crate::registry::ActorCleanupBatch,
    ) -> Result<(), ResidentLifecycleError> {
        self.cleanup(cleanup).await
    }

    async fn cleanup(
        &self,
        actors: crate::registry::ActorCleanupBatch,
    ) -> Result<(), ResidentLifecycleError> {
        let mut first_shutdown_error = None;
        let mut first_cleanup_error = None;
        for cleanup in actors.into_actors().rev() {
            let realm = cleanup.context.placement.resource_scope;
            if let Some(shutdown) = cleanup.shutdown {
                if let Err(source) = self
                    .runner
                    .run_shutdown(
                        cleanup.context.clone(),
                        shutdown,
                        realm,
                        cleanup.terminal.kind,
                        self.policy.shutdown_admission_timeout,
                    )
                    .await
                {
                    let _ = self
                        .registry
                        .record_shutdown_hook_failed(cleanup.actor, source.to_string());
                    if first_shutdown_error.is_none() {
                        first_shutdown_error = Some(ResidentLifecycleError::Shutdown {
                            actor: cleanup.actor,
                            source,
                        });
                    }
                }
            }
            if let Err(source) = self.runner.close_realm(cleanup.context, realm).await {
                if first_cleanup_error.is_none() {
                    first_cleanup_error = Some(ResidentLifecycleError::Cleanup {
                        actor: cleanup.actor,
                        source,
                    });
                }
            }
        }
        first_cleanup_error
            .or(first_shutdown_error)
            .map_or(Ok(()), Err)
    }
}
