use tidepool_bridge::HaskellValue;
use tidepool_bridge::{get_qualified, BridgeError, FromHaskell, ToHaskell};
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
    request: &HaskellValue,
    table: &DataConTable,
) -> Result<ActorRef, ActorWaitError> {
    let ActorReq::ActorWaitWith((actor_id, incarnation)) = ActorReq::from_value(request, table)?
    else {
        return Err(ActorWaitError::UnexpectedRequest);
    };
    decode_address(actor_id, incarnation)
}

pub(crate) fn decode_poll_target(
    request: &HaskellValue,
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
) -> Result<HaskellValue, BridgeError> {
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
    let qualified = format!("Tidepool.Effects.Core.{name}");
    let constructor = get_qualified(table, &qualified, arity)
        .ok_or_else(|| BridgeError::UnknownDataConName(qualified))?;
    Ok(HaskellValue::Con(constructor, fields))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{DataCon, DataConId};

    fn insert(table: &mut DataConTable, id: u64, qualified_name: &str) {
        table.insert(DataCon {
            id: DataConId(id),
            name: "ActorCompletedStatus".into(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some(qualified_name.into()),
            type_name: "ActorTerminalStatus".into(),
        });
    }

    fn completed() -> ActorTerminal {
        ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: String::new(),
        }
    }

    #[test]
    fn terminal_status_uses_canonical_constructor_among_impostors() {
        let mut table = DataConTable::new();
        insert(&mut table, 1, "Tidepool.Effects.Core.ActorCompletedStatus");
        insert(&mut table, 2, "User.ActorCompletedStatus");

        let value = actor_terminal_value(&completed(), &table).unwrap();
        assert!(matches!(value, HaskellValue::Con(DataConId(1), ref fields) if fields.is_empty()));
    }

    #[test]
    fn terminal_status_rejects_impostor_only_table() {
        let mut table = DataConTable::new();
        insert(&mut table, 2, "User.ActorCompletedStatus");

        assert!(matches!(
            actor_terminal_value(&completed(), &table),
            Err(BridgeError::UnknownDataConName(ref name))
                if name == "Tidepool.Effects.Core.ActorCompletedStatus"
        ));
    }
}
