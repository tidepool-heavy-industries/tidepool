use std::fmt;

use serde::{Deserialize, Serialize};
use tidepool_repr::SessionId;
use tidepool_runtime::session::{ResidentHole, RootCustody};

use crate::{ActorRef, ActorRegistryError};

/// Process-local identity of one synchronous actor call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallId(pub u64);

/// Process-local identity of one accepted mailbox message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub u64);

/// Process-local identity of one parked exact-incarnation wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WaitId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkedObligation {
    Call(CallId),
    Wait(WaitId),
}

/// The installed one-message receiver for an exact actor incarnation.
///
/// The parked authored continuation consumes `next`; the rooted rank-N
/// handler consumes the next protocol request. Both have move-only Rust
/// custody and therefore live in the one actor registry, not in a parallel
/// program table; this imposes no linearity discipline on authored Haskell.
pub(crate) struct InstalledReceiver {
    pub(crate) site: u64,
    pub(crate) continuation: ResidentHole,
    pub(crate) handler: RootCustody,
}

/// The stable actor state committed at the end of one mailbox turn.
/// Reply publication and this transition share one registry linearization
/// point, so a caller never observes a result before the callee advances.
pub(crate) enum InstalledActorState {
    Receiving(InstalledReceiver),
    Completed(crate::ActorTerminal),
}

pub(crate) struct KernelValue {
    pub(crate) continuation: ResidentHole,
    pub(crate) value: RootCustody,
}

pub(crate) enum ResidentOutbound {
    Call {
        target: ActorRef,
        continuation: ResidentHole,
        request: MailboxValue,
    },
    Cast {
        target: ActorRef,
        continuation: ResidentHole,
        request: MailboxValue,
    },
}

pub(crate) struct ResidentWaitRequest {
    pub(crate) target: ActorRef,
    pub(crate) continuation: ResidentHole,
}

/// One live Haskell value under exclusive machine-root custody.
///
/// The session tag lets the actor kernel reject a cross-machine delivery
/// before the custody token leaves its envelope. Dropping this value drops
/// [`RootCustody`], which queues the underlying root for release by its
/// originating resident session.
#[must_use = "a live mailbox value must be delivered or deliberately dropped"]
pub struct MailboxValue {
    session: SessionId,
    root: MailboxRoot,
}

enum MailboxRoot {
    Runtime(RootCustody),
    #[cfg(test)]
    Probe {
        _drop: DropProbe,
    },
}

impl MailboxValue {
    pub fn new(session: SessionId, custody: RootCustody) -> Self {
        Self {
            session,
            root: MailboxRoot::Runtime(custody),
        }
    }

    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Recover custody after the actor kernel has validated the destination
    /// session. This consumes the envelope's ownership token exactly once.
    pub fn into_custody(self) -> RootCustody {
        match self.root {
            MailboxRoot::Runtime(custody) => custody,
            #[cfg(test)]
            MailboxRoot::Probe { .. } => panic!("test root has no runtime custody"),
        }
    }

    #[cfg(test)]
    pub(crate) fn probe(
        session: SessionId,
        dropped: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            session,
            root: MailboxRoot::Probe {
                _drop: DropProbe(dropped),
            },
        }
    }
}

impl fmt::Debug for MailboxValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MailboxValue")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
struct DropProbe(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[cfg(test)]
impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MailboxFailure {
    #[error(transparent)]
    Registry(#[from] ActorRegistryError),
    #[error("synchronous call {caller:?} -> {target:?} would create a cycle")]
    CallCycle { caller: ActorRef, target: ActorRef },
    #[error(
        "mailbox value belongs to session {value}, but actor {actor:?} belongs to session {actor_session}"
    )]
    MachineBoundary {
        actor: ActorRef,
        actor_session: SessionId,
        value: SessionId,
    },
    #[error(
        "actors {caller:?} and {target:?} belong to different sessions ({caller_session} and {target_session})"
    )]
    ActorMachineBoundary {
        caller: ActorRef,
        caller_session: SessionId,
        target: ActorRef,
        target_session: SessionId,
    },
    #[error("call {0:?} is unknown or already consumed")]
    UnknownCall(CallId),
    #[error("resident mailbox settlement had invalid shape: {0}")]
    SettlementShape(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallFailure {
    TargetExited(ActorRef),
    DeliveryAbandoned(ActorRef),
}

/// Linear handle for polling the one result of a synchronous call. Dropping
/// an unsettled ticket cancels the obligation and releases kernel-owned roots.
pub struct CallTicket {
    pub(crate) id: CallId,
    pub(crate) caller: ActorRef,
    pub(crate) target: ActorRef,
    pub(crate) registry: crate::ActorRegistry,
    pub(crate) settled: bool,
}

impl CallTicket {
    #[must_use]
    pub fn id(&self) -> CallId {
        self.id
    }

    pub fn poll(&mut self) -> Result<CallStatus, MailboxFailure> {
        let registry = self.registry.clone();
        registry.poll_call(self)
    }
}

impl fmt::Debug for CallTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallTicket")
            .field("id", &self.id)
            .field("caller", &self.caller)
            .field("target", &self.target)
            .field("settled", &self.settled)
            .finish()
    }
}

impl Drop for CallTicket {
    fn drop(&mut self) {
        if !self.settled {
            let registry = self.registry.clone();
            registry.cancel_call(self);
        }
    }
}

#[derive(Debug)]
pub enum CallStatus {
    Pending,
    Reply(MailboxValue),
    Failed(CallFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitObservation {
    Pending,
    Exited(crate::ActorTerminal),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WaitError {
    #[error(transparent)]
    Registry(#[from] ActorRegistryError),
    #[error(
        "waiter {waiter:?} and target {target:?} belong to different sessions ({waiter_session} and {target_session})"
    )]
    MachineBoundary {
        waiter: ActorRef,
        waiter_session: SessionId,
        target: ActorRef,
        target_session: SessionId,
    },
    #[error("wait {waiter:?} -> {target:?} would create a parked-obligation cycle")]
    WaitCycle { waiter: ActorRef, target: ActorRef },
    #[error("wait {0:?} is unknown or already consumed")]
    UnknownWait(WaitId),
}

/// Linear parked wait. Dropping it unregisters the waiter; polling consumes
/// only an immutable terminal result for the exact target incarnation.
pub struct WaitTicket {
    pub(crate) id: WaitId,
    pub(crate) waiter: ActorRef,
    pub(crate) target: ActorRef,
    pub(crate) registry: crate::ActorRegistry,
    pub(crate) settled: bool,
}

impl WaitTicket {
    #[must_use]
    pub fn id(&self) -> WaitId {
        self.id
    }

    pub fn poll(&mut self) -> Result<ExitObservation, WaitError> {
        let registry = self.registry.clone();
        registry.poll_wait(self)
    }
}

impl fmt::Debug for WaitTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WaitTicket")
            .field("id", &self.id)
            .field("waiter", &self.waiter)
            .field("target", &self.target)
            .field("settled", &self.settled)
            .finish()
    }
}

impl Drop for WaitTicket {
    fn drop(&mut self) {
        if !self.settled {
            let registry = self.registry.clone();
            registry.cancel_wait(self);
        }
    }
}
