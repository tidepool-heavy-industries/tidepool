use tidepool_bridge::HaskellValue;
use tidepool_bridge::{get_qualified, BridgeError, FromHaskell, HaskellVisitor, ToHaskell};
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

impl tidepool_bridge::sealed::ToHaskellSealed for ActorTerminal {}

/// Stream immutable terminal metadata. Successful domain data remains in the
/// shared Haskell exit cell carried by the exact actor reference.
impl ToHaskell for ActorTerminal {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let (name, arity) = match self.kind {
            ActorExitKind::Completed => ("ActorCompletedStatus", 0),
            ActorExitKind::Failed => ("ActorFailedStatus", 1),
            ActorExitKind::Cancelled => ("ActorCancelledStatus", 1),
        };
        let qualified = format!("Tidepool.Effects.Core.{name}");
        let constructor = get_qualified(table, &qualified, arity)
            .ok_or(BridgeError::UnknownDataConName(qualified))?;
        visitor.begin_constructor(constructor, arity as usize)?;
        if arity == 1 {
            self.summary.visit(table, visitor)?;
        }
        visitor.end_constructor()
    }
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

        let value = completed().to_value(&table).unwrap();
        assert!(matches!(value, HaskellValue::Con(DataConId(1), ref fields) if fields.is_empty()));
    }

    #[test]
    fn terminal_status_rejects_impostor_only_table() {
        let mut table = DataConTable::new();
        insert(&mut table, 2, "User.ActorCompletedStatus");

        assert!(matches!(
            completed().to_value(&table),
            Err(BridgeError::UnknownDataConName(ref name))
                if name == "Tidepool.Effects.Core.ActorCompletedStatus"
        ));
    }

    fn terminal_table() -> DataConTable {
        let mut table = tidepool_test_data::standard_datacon_table();
        for (id, name, arity) in [
            (100, "ActorCompletedStatus", 0),
            (101, "ActorFailedStatus", 1),
            (102, "ActorCancelledStatus", 1),
        ] {
            table.insert(DataCon {
                id: DataConId(id),
                name: name.into(),
                tag: (id - 99) as u32,
                rep_arity: arity,
                field_bangs: vec![tidepool_repr::datacon::SrcBang::NoSrcBang; arity as usize],
                qualified_name: Some(format!("Tidepool.Effects.Core.{name}")),
                type_name: "ActorTerminalStatus".into(),
            });
        }
        table
    }

    #[test]
    fn wait_and_poll_sources_preserve_terminal_summaries() {
        let table = terminal_table();
        for (kind, expected) in [
            (ActorExitKind::Completed, 100),
            (ActorExitKind::Failed, 101),
            (ActorExitKind::Cancelled, 102),
        ] {
            let terminal = ActorTerminal {
                kind,
                summary: "terminal detail: λ".into(),
            };
            let value = terminal.to_value(&table).unwrap();
            let HaskellValue::Con(id, fields) = &value else {
                panic!("terminal must be a constructor");
            };
            assert_eq!(*id, DataConId(expected));
            if expected == 100 {
                assert!(fields.is_empty());
            } else {
                assert_eq!(fields.len(), 1);
                assert_eq!(
                    String::from_value(&fields[0], &table).unwrap(),
                    terminal.summary
                );
            }
            let polled = Some(terminal).to_value(&table).unwrap();
            let HaskellValue::Con(just, ref fields) = polled else {
                panic!("poll result must be a constructor");
            };
            assert_eq!(just, get_qualified(&table, "GHC.Maybe.Just", 1).unwrap());
            assert_eq!(fields.len(), 1);
            assert!(matches!(&fields[0], HaskellValue::Con(id, _) if *id == DataConId(expected)));
        }
        let pending = Option::<ActorTerminal>::None.to_value(&table).unwrap();
        assert!(matches!(pending, HaskellValue::Con(id, ref fields)
            if Some(id) == get_qualified(&table, "GHC.Maybe.Nothing", 0) && fields.is_empty()));
    }

    #[test]
    fn terminal_source_rejects_wrong_representation_arity() {
        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(103),
            name: "ActorFailedStatus".into(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("Tidepool.Effects.Core.ActorFailedStatus".into()),
            type_name: "ActorTerminalStatus".into(),
        });
        let terminal = ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "failure".into(),
        };
        assert!(matches!(
            terminal.to_value(&table),
            Err(BridgeError::UnknownDataConName(_))
        ));
    }

    #[test]
    fn terminal_source_propagates_sink_failure_and_remains_reusable() {
        struct RejectBytes;
        impl HaskellVisitor for RejectBytes {
            fn begin_constructor(&mut self, _: DataConId, _: usize) -> Result<(), BridgeError> {
                Ok(())
            }
            fn end_constructor(&mut self) -> Result<(), BridgeError> {
                Ok(())
            }
            fn literal(&mut self, _: tidepool_repr::Literal) -> Result<(), BridgeError> {
                Ok(())
            }
            fn byte_array(&mut self, _: Vec<u8>) -> Result<(), BridgeError> {
                Err(BridgeError::UnsupportedType("sink rejected bytes".into()))
            }
        }
        let table = terminal_table();
        let terminal = ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "failure".into(),
        };
        assert_eq!(
            terminal.visit(&table, &mut RejectBytes),
            Err(BridgeError::UnsupportedType("sink rejected bytes".into()))
        );
        assert!(terminal.to_value(&table).is_ok());
    }
}
