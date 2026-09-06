//! Staged own-spawn custody guard, not a production launch switch. Namespace
//! cleanup does not establish HTTP/resident-work quiescence or settle a lease.

use std::{fs::File, sync::Arc, time::Instant};

use tidepool_actor::{ActorRef, ActorTerminal};
use tidepool_node::{
    PreparedServiceScope, ServiceEnvironment, ServiceScope, ServiceScopeCleanup, ServiceScopeError,
};

use super::ActorWorkspaceCustody;

#[derive(Default)]
pub(super) enum LaunchCustody {
    #[default]
    Unclaimed,
    ScopedClaimed,
    ScopedNotSpawned,
    Legacy,
}

#[derive(Default)]
pub(super) struct CustodyState {
    pub(super) launch: LaunchCustody,
    pub(super) terminal: Option<ActorTerminal>,
}

#[derive(Debug, thiserror::Error)]
enum ScopedSpawnError {
    #[error("scope claim does not name this exact installed actor")]
    WrongActor,
    #[error("scope claim requires its installed lease")]
    MissingLease,
    #[error("scope claim is already consumed or legacy-fenced")]
    AlreadyClaimed,
    #[error("scope claim requires a live actor")]
    ActorStopped,
    #[error("scope was not spawned: {0}")]
    PreSpawn(ServiceScopeError),
}

struct ScopedResources {
    custody: Arc<ActorWorkspaceCustody>,
    scope: ServiceScope,
}

/// Noncloneable and without a constructor accepting ServiceScope or a cleanup
/// receipt. The private spawn operation pairs the claim with its own result.
struct ScopedCustodyOwner {
    resources: Option<ScopedResources>,
}

impl ScopedCustodyOwner {
    fn spawn(
        custody: Arc<ActorWorkspaceCustody>,
        actor: ActorRef,
        prepared: PreparedServiceScope,
        environment: ServiceEnvironment,
        output: File,
    ) -> Result<Self, ScopedSpawnError> {
        {
            let mut state = custody.state.lock();
            if custody.actor != actor {
                return Err(ScopedSpawnError::WrongActor);
            }
            if custody.binding.is_none() {
                return Err(ScopedSpawnError::MissingLease);
            }
            if !matches!(state.launch, LaunchCustody::Unclaimed) {
                return Err(ScopedSpawnError::AlreadyClaimed);
            }
            if state.terminal.is_some() {
                return Err(ScopedSpawnError::ActorStopped);
            }
            // The immutable, noncloneable ActiveBinding is the generation
            // authority. No copied identity or table read creates another lease.
            state.launch = LaunchCustody::ScopedClaimed;
        }
        match prepared.spawn(environment, output) {
            Ok(scope) => Ok(Self {
                resources: Some(ScopedResources { custody, scope }),
            }),
            Err(error) => {
                // The synchronous API guarantees Err is before successful spawn.
                // Never erase a concurrent legacy fence, and never allow retry.
                let mut state = custody.state.lock();
                if matches!(state.launch, LaunchCustody::ScopedClaimed) {
                    state.launch = LaunchCustody::ScopedNotSpawned;
                }
                Err(ScopedSpawnError::PreSpawn(error))
            }
        }
    }

    fn pin(&mut self, deadline: Instant) -> Result<(), ServiceScopeError> {
        self.resources
            .as_mut()
            .expect("owned resources")
            .scope
            .pin_init(deadline)
    }

    fn stop(&mut self, deadline: Instant) -> Result<ScopedCleanupObservation, ServiceScopeError> {
        let resources = self.resources.as_mut().expect("owned resources");
        let status = resources.scope.terminate_and_wait(deadline)?;
        let state = resources.custody.state.lock();
        Ok(match &state.terminal {
            None => ScopedCleanupObservation::ProcessStoppedActorActive(status),
            Some(terminal) => ScopedCleanupObservation::ProcessStoppedHostWorkPending {
                status,
                terminal: terminal.clone(),
            },
        })
    }
}

impl Drop for ScopedCustodyOwner {
    fn drop(&mut self) {
        // Even a successfully stopped namespace does not discharge host work.
        // This also covers a spawn_blocking result whose receiver disappeared.
        // Retain the entire owner, not just a row with lost process handles.
        if let Some(resources) = self.resources.take() {
            tracing::error!(actor = ?resources.custody.actor, "retaining scoped custody: host quiescence unsupported");
            std::mem::forget(resources);
        }
    }
}

/// Reporting only. Copying the contained ServiceScopeCleanup cannot authorize
/// settlement; there is deliberately no function accepting it as authority.
enum ScopedCleanupObservation {
    ProcessStoppedActorActive(ServiceScopeCleanup),
    ProcessStoppedHostWorkPending {
        status: ServiceScopeCleanup,
        terminal: ActorTerminal,
    },
}

/// Uninhabited until HTTP tasks AND resident effects have an owning contract.
enum HostWorkQuiescence {}

#[cfg(test)]
mod tests;
