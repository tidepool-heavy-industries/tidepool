//! Exact process-supervisor custody in the existing application lifecycle row.
//! Process cleanup does not establish HTTP/resident-work quiescence or settle a
//! workspace lease. The direct scope adapter below remains test-only scaffolding
//! for exercising the underlying namespace owner.

use std::{path::PathBuf, sync::Arc, time::Instant};

#[cfg(test)]
use std::fs::File;

use tidepool_actor::{ActorRef, ActorTerminal};
#[cfg(test)]
use tidepool_node::{
    LaunchReservation, ScopeCapability, ServiceEnvironment, ServiceScopeCleanup, ServiceScopeError,
};
use tidepool_node::{
    ProcessSupervisorClient, ProcessSupervisorError, ProcessSupervisorObservation,
    ProcessSupervisorRecovery,
};

use super::ActorWorkspaceCustody;

#[derive(Default)]
pub(super) enum LaunchCustody {
    #[default]
    Unclaimed,
    ScopedClaimed,
    #[cfg(test)]
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
    SupervisorPending(SupervisorRecoveryKey),
    Supervisor {
        client: ProcessSupervisorClient,
        recovery: SupervisorRecoveryKey,
    },
    Finalized,
    #[cfg(test)]
    Spawning,
    #[cfg(test)]
    NotSpawned(ServiceScopeError),
    #[cfg(test)]
    Owned(ScopeCapability),
}

/// Limited recovery coordinates retained by the lifecycle row before tmux
/// submission. They can observe/stop/finalize an exact helper, but cannot
/// prepare, pin, or release its payload.
pub(super) struct SupervisorRecoveryKey {
    socket_path: PathBuf,
    launch_id: String,
    recovery_secret: String,
}

impl SupervisorRecoveryKey {
    pub(super) fn new(socket_path: PathBuf, launch_id: String, recovery_secret: String) -> Self {
        Self {
            socket_path,
            launch_id,
            recovery_secret,
        }
    }

    fn recover(&self, deadline: Instant) -> Result<ProcessSupervisorRecovery, ScopedProcessError> {
        ProcessSupervisorRecovery::recover(
            self.socket_path.clone(),
            self.launch_id.clone(),
            self.recovery_secret.clone(),
            remaining(deadline)?,
        )
        .map(|(client, _)| client)
        .map_err(Into::into)
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ScopedProcessError {
    #[error("scope operation is invalid in its current phase")]
    WrongPhase,
    #[error(transparent)]
    Supervisor(#[from] ProcessSupervisorError),
    #[cfg(test)]
    #[error(transparent)]
    Direct(#[from] ServiceScopeError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScopedProcessObservation {
    Reserved,
    Blocked,
    Pinned,
    Released,
    ReleaseUnconfirmed,
    Stopping,
    ProcessStopped,
}

fn supervisor_observation(value: ProcessSupervisorObservation) -> ScopedProcessObservation {
    match value {
        ProcessSupervisorObservation::Reserved => ScopedProcessObservation::Reserved,
        ProcessSupervisorObservation::Blocked => ScopedProcessObservation::Blocked,
        ProcessSupervisorObservation::Pinned => ScopedProcessObservation::Pinned,
        ProcessSupervisorObservation::Released => ScopedProcessObservation::Released,
        ProcessSupervisorObservation::ReleaseUnconfirmed => {
            ScopedProcessObservation::ReleaseUnconfirmed
        }
        ProcessSupervisorObservation::Stopping => ScopedProcessObservation::Stopping,
        ProcessSupervisorObservation::ProcessStopped | ProcessSupervisorObservation::NotSpawned => {
            ScopedProcessObservation::ProcessStopped
        }
        ProcessSupervisorObservation::LaunchFailed => ScopedProcessObservation::Stopping,
    }
}

fn remaining(deadline: Instant) -> Result<std::time::Duration, ScopedProcessError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(ScopedProcessError::WrongPhase)
}

/// Only the existing host lifecycle map owns this noncloneable anchor. The
/// process slot has no custody back-reference and no task handle/cycle.
pub(super) struct ScopedHostRetention {
    // Keep the exact installed lease owner alive without requiring production
    // callers to reconstruct its concrete Arc from the erased kernel handle.
    _custody: Option<Arc<dyn tidepool_actor::ForkWorkspaceCustody>>,
    #[cfg(test)]
    state: Arc<parking_lot::Mutex<CustodyState>>,
    pub(super) slot: Arc<parking_lot::Mutex<ScopedProcessSlot>>,
}

/// Source-checkout applications have process custody but no managed-worktree lease.
/// The existing lifecycle row still retains their exact supervisor capability.
pub(super) fn reserve_source_checkout() -> ScopedHostRetention {
    ScopedHostRetention {
        _custody: None,
        #[cfg(test)]
        state: Arc::new(parking_lot::Mutex::new(CustodyState::default())),
        slot: Arc::new(parking_lot::Mutex::new(ScopedProcessSlot::Reserved)),
    }
}

pub(super) fn reserve(
    custody: Arc<dyn tidepool_actor::ForkWorkspaceCustody>,
    actor: ActorRef,
) -> Result<ScopedHostRetention, ScopedClaimError> {
    let _state = {
        let Some(exact) =
            (custody.as_ref() as &dyn std::any::Any).downcast_ref::<ActorWorkspaceCustody>()
        else {
            return Err(ScopedClaimError::WrongActor);
        };
        let mut state = exact.state.lock();
        if exact.actor != actor {
            return Err(ScopedClaimError::WrongActor);
        }
        if exact.binding.lock().is_none() {
            return Err(ScopedClaimError::MissingLease);
        }
        if !matches!(state.launch, LaunchCustody::Unclaimed) {
            return Err(ScopedClaimError::AlreadyClaimed);
        }
        if state.terminal.is_some() {
            return Err(ScopedClaimError::ActorStopped);
        }
        state.launch = LaunchCustody::ScopedClaimed;
        drop(state);
        exact.state.clone()
    };
    Ok(ScopedHostRetention {
        _custody: Some(custody),
        #[cfg(test)]
        state: _state,
        slot: Arc::new(parking_lot::Mutex::new(ScopedProcessSlot::Reserved)),
    })
}

/// Store the exact synchronous result BEFORE completing any async notice.
/// Duplicate submissions cannot replace an owned scope. No closure owns custody.
#[cfg(test)]
pub(super) fn spawn_into(
    slot: Arc<parking_lot::Mutex<ScopedProcessSlot>>,
    prepared: LaunchReservation,
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

/// Install the already-paired, exact-launch client before reporting launch
/// readiness. A duplicate or late client can never replace row authority.
pub(super) fn install_supervisor(
    slot: &parking_lot::Mutex<ScopedProcessSlot>,
    client: ProcessSupervisorClient,
    observation: ProcessSupervisorObservation,
) -> Result<(), ScopedProcessError> {
    if observation != ProcessSupervisorObservation::Reserved {
        return Err(ScopedProcessError::WrongPhase);
    }
    let mut state = slot.lock();
    let recovery = match std::mem::replace(&mut *state, ScopedProcessSlot::Reserved) {
        ScopedProcessSlot::SupervisorPending(recovery) => recovery,
        other => {
            *state = other;
            return Err(ScopedProcessError::WrongPhase);
        }
    };
    *state = ScopedProcessSlot::Supervisor { client, recovery };
    Ok(())
}

/// Fence the exact helper launch before tmux submission. Failure after this
/// point retains limited recovery authority in the same lifecycle row.
pub(super) fn stage_supervisor(
    slot: &parking_lot::Mutex<ScopedProcessSlot>,
    recovery: SupervisorRecoveryKey,
) -> Result<(), ScopedProcessError> {
    let mut state = slot.lock();
    if !matches!(*state, ScopedProcessSlot::Reserved) {
        return Err(ScopedProcessError::WrongPhase);
    }
    *state = ScopedProcessSlot::SupervisorPending(recovery);
    Ok(())
}

pub(super) fn prepare_supervisor_slot(
    slot: &parking_lot::Mutex<ScopedProcessSlot>,
    deadline: Instant,
) -> Result<ScopedProcessObservation, ScopedProcessError> {
    match &mut *slot.lock() {
        ScopedProcessSlot::Supervisor { client, .. } => Ok(supervisor_observation(
            client.prepare(remaining(deadline)?)?,
        )),
        _ => Err(ScopedProcessError::WrongPhase),
    }
}

pub(super) fn pin_supervisor_slot(
    slot: &parking_lot::Mutex<ScopedProcessSlot>,
    deadline: Instant,
) -> Result<ScopedProcessObservation, ScopedProcessError> {
    match &mut *slot.lock() {
        ScopedProcessSlot::Supervisor { client, .. } => {
            Ok(supervisor_observation(client.pin(remaining(deadline)?)?))
        }
        _ => Err(ScopedProcessError::WrongPhase),
    }
}

pub(super) fn release_supervisor_slot(
    slot: &parking_lot::Mutex<ScopedProcessSlot>,
    deadline: Instant,
) -> Result<ScopedProcessObservation, ScopedProcessError> {
    match &mut *slot.lock() {
        ScopedProcessSlot::Supervisor { client, .. } => Ok(supervisor_observation(
            client.release(remaining(deadline)?)?,
        )),
        _ => Err(ScopedProcessError::WrongPhase),
    }
}

pub(super) fn observe_slot(
    slot: &parking_lot::Mutex<ScopedProcessSlot>,
    deadline: Instant,
) -> Result<ScopedProcessObservation, ScopedProcessError> {
    match &mut *slot.lock() {
        ScopedProcessSlot::Reserved => Ok(ScopedProcessObservation::Reserved),
        ScopedProcessSlot::Supervisor { client, .. } => Ok(supervisor_observation(
            client.observe(remaining(deadline)?)?,
        )),
        ScopedProcessSlot::SupervisorPending(_) => Ok(ScopedProcessObservation::Reserved),
        ScopedProcessSlot::Finalized => Ok(ScopedProcessObservation::ProcessStopped),
        #[cfg(test)]
        ScopedProcessSlot::Spawning => Ok(ScopedProcessObservation::Stopping),
        #[cfg(test)]
        ScopedProcessSlot::NotSpawned(_) => Ok(ScopedProcessObservation::ProcessStopped),
        #[cfg(test)]
        ScopedProcessSlot::Owned(scope) => Ok(match scope.observation()? {
            tidepool_node::ScopeObservation::Blocked => ScopedProcessObservation::Blocked,
            tidepool_node::ScopeObservation::Pinned => ScopedProcessObservation::Pinned,
            tidepool_node::ScopeObservation::Released => ScopedProcessObservation::Released,
            tidepool_node::ScopeObservation::ReleaseUnconfirmed => {
                ScopedProcessObservation::ReleaseUnconfirmed
            }
            tidepool_node::ScopeObservation::Stopping => ScopedProcessObservation::Stopping,
            tidepool_node::ScopeObservation::ProcessStopped(_) => {
                ScopedProcessObservation::ProcessStopped
            }
        }),
    }
}

impl ScopedHostRetention {
    #[cfg(test)]
    pub(super) fn pin(&mut self, deadline: Instant) -> Result<(), ServiceScopeError> {
        match &mut *self.slot.lock() {
            ScopedProcessSlot::Owned(scope) => scope.pin_init(deadline),
            _ => Err(ServiceScopeError::WrongPhase),
        }
    }

    /// The slot itself, not a completion-channel result, proves pre-spawn Err.
    /// This only removes the process fence; it does not assert settlement.
    #[cfg(test)]
    pub(super) fn observe_not_spawned(&mut self) -> bool {
        if !matches!(*self.slot.lock(), ScopedProcessSlot::NotSpawned(_)) {
            return false;
        }
        let mut state = self.state.lock();
        if matches!(state.launch, LaunchCustody::ScopedClaimed) {
            state.launch = LaunchCustody::ScopedNotSpawned;
        }
        true
    }

    #[cfg(test)]
    pub(super) fn stop(
        &mut self,
        deadline: Instant,
    ) -> Result<ScopedCleanupObservation, ServiceScopeError> {
        let status = stop_slot(&self.slot, deadline)?;
        Ok(match &self.state.lock().terminal {
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
#[cfg(test)]
pub(super) fn stop_slot(
    slot: &parking_lot::Mutex<ScopedProcessSlot>,
    deadline: Instant,
) -> Result<ServiceScopeCleanup, ServiceScopeError> {
    match &mut *slot.lock() {
        ScopedProcessSlot::Owned(scope) => scope.terminate_and_wait(deadline),
        _ => Err(ServiceScopeError::WrongPhase),
    }
}

/// Stop through the row's paired helper. A successful return is process-domain
/// evidence only; it cannot discharge hosted work or workspace custody.
pub(super) fn stop_supervisor_slot(
    slot: &parking_lot::Mutex<ScopedProcessSlot>,
    deadline: Instant,
) -> Result<ScopedProcessObservation, ScopedProcessError> {
    match &mut *slot.lock() {
        ScopedProcessSlot::Supervisor { client, .. } => {
            let observation = client.stop(remaining(deadline)?)?;
            if !matches!(
                observation,
                ProcessSupervisorObservation::ProcessStopped
                    | ProcessSupervisorObservation::NotSpawned
            ) {
                return Err(ScopedProcessError::WrongPhase);
            }
            Ok(ScopedProcessObservation::ProcessStopped)
        }
        ScopedProcessSlot::SupervisorPending(recovery) => {
            let mut client = recovery.recover(deadline)?;
            let observation = client.stop(remaining(deadline)?)?;
            if !matches!(
                observation,
                ProcessSupervisorObservation::ProcessStopped
                    | ProcessSupervisorObservation::NotSpawned
            ) {
                return Err(ScopedProcessError::WrongPhase);
            }
            Ok(ScopedProcessObservation::ProcessStopped)
        }
        ScopedProcessSlot::Finalized => Ok(ScopedProcessObservation::ProcessStopped),
        _ => Err(ScopedProcessError::WrongPhase),
    }
}

fn finalize_supervisor_slot(
    slot: &parking_lot::Mutex<ScopedProcessSlot>,
    deadline: Instant,
) -> Result<ScopedProcessObservation, ScopedProcessError> {
    let state = {
        let mut slot = slot.lock();
        std::mem::replace(&mut *slot, ScopedProcessSlot::Reserved)
    };
    let recovery = match state {
        ScopedProcessSlot::Supervisor { client, recovery } => {
            let remaining = match remaining(deadline) {
                Ok(remaining) => remaining,
                Err(error) => {
                    *slot.lock() = ScopedProcessSlot::SupervisorPending(recovery);
                    return Err(error);
                }
            };
            match client.finalize(remaining) {
                Ok(
                    ProcessSupervisorObservation::ProcessStopped
                    | ProcessSupervisorObservation::NotSpawned,
                ) => None,
                Ok(_) => Some((recovery, ScopedProcessError::WrongPhase)),
                Err(error) => Some((recovery, error.into())),
            }
        }
        ScopedProcessSlot::SupervisorPending(recovery) => {
            let client = match recovery.recover(deadline) {
                Ok(client) => client,
                Err(error) => {
                    *slot.lock() = ScopedProcessSlot::SupervisorPending(recovery);
                    return Err(error);
                }
            };
            let remaining = match remaining(deadline) {
                Ok(remaining) => remaining,
                Err(error) => {
                    *slot.lock() = ScopedProcessSlot::SupervisorPending(recovery);
                    return Err(error);
                }
            };
            match client.finalize(remaining) {
                Ok(
                    ProcessSupervisorObservation::ProcessStopped
                    | ProcessSupervisorObservation::NotSpawned,
                ) => None,
                Ok(_) => Some((recovery, ScopedProcessError::WrongPhase)),
                Err(error) => Some((recovery, error.into())),
            }
        }
        ScopedProcessSlot::Finalized => None,
        other => {
            *slot.lock() = other;
            return Err(ScopedProcessError::WrongPhase);
        }
    };
    if let Some((recovery, error)) = recovery {
        *slot.lock() = ScopedProcessSlot::SupervisorPending(recovery);
        return Err(error);
    }
    *slot.lock() = ScopedProcessSlot::Finalized;
    Ok(ScopedProcessObservation::ProcessStopped)
}

/// Production rows contain only a supervisor client. The direct arm exists in
/// test builds to keep the namespace owner's adversarial fixtures independent
/// of the protocol repair.
pub(super) fn stop_retained_slot(
    slot: &parking_lot::Mutex<ScopedProcessSlot>,
    deadline: Instant,
) -> Result<ScopedProcessObservation, ScopedProcessError> {
    #[cfg(test)]
    {
        let mut state = slot.lock();
        if let ScopedProcessSlot::Owned(scope) = &mut *state {
            scope.terminate_and_wait(deadline)?;
            return Ok(ScopedProcessObservation::ProcessStopped);
        }
    }
    let stopped = stop_supervisor_slot(slot, deadline)?;
    if stopped != ScopedProcessObservation::ProcessStopped {
        return Err(ScopedProcessError::WrongPhase);
    }
    finalize_supervisor_slot(slot, deadline)
}

/// Reporting only. Copying the contained ServiceScopeCleanup cannot authorize
/// settlement; there is deliberately no function accepting it as authority.
#[cfg(test)]
pub(super) enum ScopedCleanupObservation {
    ProcessStoppedActorActive(ServiceScopeCleanup),
    ProcessStoppedHostWorkPending {
        status: ServiceScopeCleanup,
        terminal: ActorTerminal,
    },
}

#[cfg(test)]
mod tests;
