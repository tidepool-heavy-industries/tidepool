use tidepool_bridge::{get_resilient, BridgeError, FromCore, ToCore};
use tidepool_eval::Value;
use tidepool_repr::DataConTable;

use crate::generated::actor::ActorReq;
use crate::{ActorExitKind, ActorId, ActorRef, ActorTerminal, Incarnation};

pub(crate) struct ResidentPollRequest {
    pub(crate) target: ActorRef,
    pub(crate) continuation: tidepool_runtime::session::ResidentHole,
}

#[derive(Debug, thiserror::Error)]
pub enum ActorWaitError {
    #[error("invalid actor routing identity ({actor_id}, {incarnation})")]
    InvalidIdentity { actor_id: i64, incarnation: i64 },
    #[error(transparent)]
    Decode(#[from] BridgeError),
    #[error("actor wait decoder received a non-wait request")]
    UnexpectedRequest,
}

pub(crate) fn decode_wait_target(
    request: &Value,
    table: &DataConTable,
) -> Result<ActorRef, ActorWaitError> {
    let ActorReq::ActorWaitWith((actor_id, incarnation)) = ActorReq::from_value(request, table)?
    else {
        return Err(ActorWaitError::UnexpectedRequest);
    };
    decode_address(actor_id, incarnation)
}

pub(crate) fn decode_poll_target(
    request: &Value,
    table: &DataConTable,
) -> Result<ActorRef, ActorWaitError> {
    let ActorReq::ActorPollWith((actor_id, incarnation)) = ActorReq::from_value(request, table)?
    else {
        return Err(ActorWaitError::UnexpectedRequest);
    };
    decode_address(actor_id, incarnation)
}

pub(crate) fn decode_address(actor_id: i64, incarnation: i64) -> Result<ActorRef, ActorWaitError> {
    let (Ok(actor_id), Ok(incarnation)) = (u64::try_from(actor_id), u64::try_from(incarnation))
    else {
        return Err(ActorWaitError::InvalidIdentity {
            actor_id,
            incarnation,
        });
    };
    Ok(ActorRef {
        id: ActorId(actor_id),
        incarnation: Incarnation(incarnation),
    })
}

/// Encode immutable terminal metadata. Successful domain data remains in the
/// shared Haskell exit cell carried by the exact actor reference.
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
