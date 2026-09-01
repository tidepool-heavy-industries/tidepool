use tidepool_bridge::{get_resilient, BridgeError, FromCore, ToCore};
use tidepool_eval::Value;
use tidepool_repr::DataConTable;

use crate::generated::actor::ActorReq;
use crate::{
    ActorExitKind, ActorId, ActorRef, ActorRegistry, ActorTerminal, Incarnation, WaitError, WaitId,
    WaitTicket,
};

/// A Haskell `awaitExit` parked against the registry's exact target incarnation.
///
/// The value is deliberately just a Rust wait ticket. The successful typed
/// exit remains in the managed Haskell cell carried by `ActorRef`; settling
/// this ticket supplies only the terminal metadata that sequences the cell
/// read in the resumed continuation.
pub struct ActorWait {
    ticket: WaitTicket,
}

#[derive(Debug, thiserror::Error)]
pub enum ActorWaitError {
    #[error("invalid actor routing identity ({actor_id}, {incarnation})")]
    InvalidIdentity { actor_id: i64, incarnation: i64 },
    #[error(transparent)]
    Decode(#[from] BridgeError),
    #[error(transparent)]
    Wait(#[from] WaitError),
    #[error("actor wait decoder received a non-wait request")]
    UnexpectedRequest,
}

impl ActorWait {
    pub(crate) fn id(&self) -> WaitId {
        self.ticket.id()
    }

    /// Decode `ActorWaitWith` and register the wait after the caller's active
    /// Haskell turn lease has been released.
    pub fn register(
        registry: &ActorRegistry,
        waiter: ActorRef,
        request: &Value,
        table: &DataConTable,
    ) -> Result<Self, ActorWaitError> {
        let target = Self::decode_target(request, table)?;
        Self::register_target(registry, waiter, target)
    }

    pub(crate) fn decode_target(
        request: &Value,
        table: &DataConTable,
    ) -> Result<ActorRef, ActorWaitError> {
        let ActorReq::ActorWaitWith((actor_id, incarnation)) =
            ActorReq::from_value(request, table)?
        else {
            return Err(ActorWaitError::UnexpectedRequest);
        };
        let (Ok(actor_id_u64), Ok(incarnation_u64)) =
            (u64::try_from(actor_id), u64::try_from(incarnation))
        else {
            return Err(ActorWaitError::InvalidIdentity {
                actor_id,
                incarnation,
            });
        };
        Ok(ActorRef {
            id: ActorId(actor_id_u64),
            incarnation: Incarnation(incarnation_u64),
        })
    }

    pub(crate) fn register_target(
        registry: &ActorRegistry,
        waiter: ActorRef,
        target: ActorRef,
    ) -> Result<Self, ActorWaitError> {
        Ok(Self {
            ticket: registry.register_wait(waiter, target)?,
        })
    }

    /// Poll without consuming a pending wait. A terminal result is immutable,
    /// so a later Haskell `awaitExit` may register independently and observe it
    /// again.
    pub fn poll(&mut self) -> Result<Option<ActorTerminal>, ActorWaitError> {
        match self.ticket.poll()? {
            crate::ExitObservation::Pending => Ok(None),
            crate::ExitObservation::Exited(terminal) => Ok(Some(terminal)),
        }
    }
}

/// Encode Rust's immutable terminal metadata as the actor effect's internal
/// Haskell status. This carries no successful domain value: completion merely
/// authorizes the resumed `Tidepool.Actor.awaitExit` to read its shared exit cell.
pub fn actor_terminal_value(
    terminal: &ActorTerminal,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let (name, arity, fields) = match terminal.kind {
        ActorExitKind::Completed => ("ActorCompletedStatus", 0, Vec::new()),
        ActorExitKind::Failed => (
            "ActorFailedStatus",
            1,
            vec![terminal.summary.to_value(table)?],
        ),
        ActorExitKind::Cancelled => (
            "ActorCancelledStatus",
            1,
            vec![terminal.summary.to_value(table)?],
        ),
    };
    let constructor = get_resilient(table, name, arity)
        .ok_or_else(|| BridgeError::UnknownDataConName(name.to_string()))?;
    Ok(Value::Con(constructor, fields))
}
