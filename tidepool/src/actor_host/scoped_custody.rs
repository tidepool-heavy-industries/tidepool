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
pub(super) enum ScopedClaimError {
    #[error("scope claim does not name this exact installed actor")]
    WrongActor,
    #[error("scope claim requires its installed lease")]
    MissingLease,
    #[error("scope claim is already consumed or legacy-fenced")]
    AlreadyClaimed,
    #[error("scope claim requires a live actor")]
    ActorStopped,
}

/// Addressable slot reserved by the existing host launch row before spawn.
/// Neither slot nor spawn closure owns a back-reference to the retention owner.
pub(super) enum ScopedProcessSlot {
    Reserved,
    Spawning,
    NotSpawned(ServiceScopeError),
    Owned(ServiceScope),
}

/// Only the existing host lifecycle map owns this noncloneable anchor. The
/// process slot has no custody back-reference and no task handle/cycle.
pub(super) struct ScopedHostRetention {
    custody: Arc<ActorWorkspaceCustody>,
    pub(super) slot: Arc<parking_lot::Mutex<ScopedProcessSlot>>,
}

pub(super) fn reserve(
    custody: Arc<ActorWorkspaceCustody>,
    actor: ActorRef,
) -> Result<ScopedHostRetention, ScopedClaimError> {
    {
        let mut state = custody.state.lock();
        if custody.actor != actor {
            return Err(ScopedClaimError::WrongActor);
        }
        if custody.binding.is_none() {
            return Err(ScopedClaimError::MissingLease);
        }
        if !matches!(state.launch, LaunchCustody::Unclaimed) {
            return Err(ScopedClaimError::AlreadyClaimed);
        }
        if state.terminal.is_some() {
            return Err(ScopedClaimError::ActorStopped);
        }
        state.launch = LaunchCustody::ScopedClaimed;
    }
    Ok(ScopedHostRetention {
        custody,
        slot: Arc::new(parking_lot::Mutex::new(ScopedProcessSlot::Reserved)),
    })
}

/// Store the exact synchronous result BEFORE completing any async notice.
/// Duplicate submissions cannot replace an owned scope. No closure owns custody.
pub(super) fn spawn_into(
    slot: Arc<parking_lot::Mutex<ScopedProcessSlot>>,
    prepared: PreparedServiceScope,
    environment: ServiceEnvironment,
    output: File,
) -> Result<(), ServiceScopeError> {
    {
        let mut state = slot.lock();
        if !matches!(*state, ScopedProcessSlot::Reserved) {
            return Err(ServiceScopeError::WrongPhase);
        }
        *state = ScopedProcessSlot::Spawning;
    }
    let result = prepared.spawn(environment, output);
    *slot.lock() = match result {
        Ok(scope) => ScopedProcessSlot::Owned(scope),
        Err(error) => ScopedProcessSlot::NotSpawned(error),
    };
    Ok(())
}

impl ScopedHostRetention {
    pub(super) fn terminal_until(&self, deadline: Instant) -> Option<Option<ActorTerminal>> {
        self.custody
            .state
            .try_lock_until(deadline)
            .map(|state| state.terminal.clone())
    }

    pub(super) fn pin(&mut self, deadline: Instant) -> Result<(), ServiceScopeError> {
        match &mut *self.slot.lock() {
            ScopedProcessSlot::Owned(scope) => scope.pin_init(deadline),
            _ => Err(ServiceScopeError::WrongPhase),
        }
    }

    /// The slot itself, not a completion-channel result, proves pre-spawn Err.
    /// This only removes the process fence; it does not assert settlement.
    pub(super) fn observe_not_spawned(&mut self) -> bool {
        if !matches!(*self.slot.lock(), ScopedProcessSlot::NotSpawned(_)) {
            return false;
        }
        let mut state = self.custody.state.lock();
        if matches!(state.launch, LaunchCustody::ScopedClaimed) {
            state.launch = LaunchCustody::ScopedNotSpawned;
        }
        true
    }

    pub(super) fn stop(
        &mut self,
        deadline: Instant,
    ) -> Result<ScopedCleanupObservation, ServiceScopeError> {
        let status = stop_slot(&self.slot, deadline)?;
        Ok(match &self.custody.state.lock().terminal {
            None => ScopedCleanupObservation::ProcessStoppedActorActive(status),
            Some(terminal) => ScopedCleanupObservation::ProcessStoppedHostWorkPending {
                status,
                terminal: terminal.clone(),
            },
        })
    }
}

/// Retirement workers may borrow only the slot. Losing their result cannot
/// remove it from the exact host map row, and the result remains status only.
pub(super) fn stop_slot(
    slot: &parking_lot::Mutex<ScopedProcessSlot>,
    deadline: Instant,
) -> Result<ServiceScopeCleanup, ServiceScopeError> {
    match &mut *slot.lock() {
        ScopedProcessSlot::Owned(scope) => scope.terminate_and_wait(deadline),
        _ => Err(ServiceScopeError::WrongPhase),
    }
}

/// Reporting only. Copying the contained ServiceScopeCleanup cannot authorize
/// settlement; there is deliberately no function accepting it as authority.
pub(super) enum ScopedCleanupObservation {
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
