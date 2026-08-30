use frunk::hlist;
use proptest::prelude::*;
use tidepool_bridge::{BridgeError, FromCore};
use tidepool_effect::dispatch::{DispatchEffect, EffectContext, EffectHandler, Response};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::{DataConId, Literal};

const FIRST: DataConId = DataConId(1);
const SECOND: DataConId = DataConId(2);
const UNKNOWN: DataConId = DataConId(3);

fn table() -> DataConTable {
    let mut table = DataConTable::new();
    for (id, name) in [
        (FIRST, "FirstRequest"),
        (SECOND, "SecondRequest"),
        (UNKNOWN, "UnknownRequest"),
    ] {
        table.insert(DataCon {
            id,
            name: name.into(),
            tag: id.0 as u32,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: Some(format!("Test.{name}")),
            type_name: "TestRequest".into(),
        });
    }
    table
}

fn request(id: DataConId, value: i64) -> Value {
    Value::Con(id, vec![Value::Lit(Literal::LitInt(value))])
}

fn decode_request(value: &Value, expected_id: DataConId) -> Result<i64, BridgeError> {
    let Value::Con(id, fields) = value else {
        return Err(BridgeError::UnknownDataCon(DataConId(0)));
    };
    if *id != expected_id {
        return Err(BridgeError::UnknownDataCon(*id));
    }
    if fields.len() != 1 {
        return Err(BridgeError::ArityMismatch {
            con: expected_id,
            expected: 1,
            got: fields.len(),
        });
    }
    match fields[0] {
        Value::Lit(Literal::LitInt(n)) => Ok(n),
        ref other => Err(BridgeError::TypeMismatch {
            expected: "LitInt".into(),
            got: format!("{other:?}"),
        }),
    }
}

struct FirstRequest(i64);
impl tidepool_bridge::sealed::FromCoreSealed for FirstRequest {}
impl FromCore for FirstRequest {
    fn from_value(value: &Value, _table: &DataConTable) -> Result<Self, BridgeError> {
        decode_request(value, FIRST).map(Self)
    }
}

struct SecondRequest(i64);
impl tidepool_bridge::sealed::FromCoreSealed for SecondRequest {}
impl FromCore for SecondRequest {
    fn from_value(value: &Value, _table: &DataConTable) -> Result<Self, BridgeError> {
        decode_request(value, SECOND).map(Self)
    }
}

struct FirstHandler;
impl EffectHandler for FirstHandler {
    type Request = FirstRequest;

    fn handle(
        &mut self,
        request: FirstRequest,
        _cx: &EffectContext<'_>,
    ) -> Result<Response, EffectError> {
        Ok(Value::Lit(Literal::LitInt(request.0 + 10)).into())
    }
}

struct SecondHandler;
impl EffectHandler for SecondHandler {
    type Request = SecondRequest;

    fn handle(
        &mut self,
        request: SecondRequest,
        _cx: &EffectContext<'_>,
    ) -> Result<Response, EffectError> {
        Ok(Value::Lit(Literal::LitInt(request.0 + 20)).into())
    }
}

fn completed_int(response: Option<Response>) -> i64 {
    match response {
        Some(Response::Complete(Value::Lit(Literal::LitInt(n)))) => n,
        other => panic!("expected a completed integer response, got {other:?}"),
    }
}

proptest! {
    /// Handler order is composition order, not protocol identity. A constructor
    /// reaches the same handler no matter where that handler sits in the HList.
    #[test]
    fn routes_by_nominal_constructor(value in any::<i64>()) {
        let table = table();
        let cx = EffectContext::with_user(&table, &());
        let first = request(FIRST, value);
        let second = request(SECOND, value);
        let mut forward = hlist![FirstHandler, SecondHandler];
        let mut reverse = hlist![SecondHandler, FirstHandler];

        prop_assert_eq!(
            completed_int(forward.dispatch(&first, &cx).unwrap()),
            value.wrapping_add(10),
        );
        prop_assert_eq!(
            completed_int(reverse.dispatch(&first, &cx).unwrap()),
            value.wrapping_add(10),
        );
        prop_assert_eq!(
            completed_int(forward.dispatch(&second, &cx).unwrap()),
            value.wrapping_add(20),
        );
        prop_assert_eq!(
            completed_int(reverse.dispatch(&second, &cx).unwrap()),
            value.wrapping_add(20),
        );
    }
}

#[test]
fn unknown_constructor_is_left_unhandled() {
    let table = table();
    let cx = EffectContext::with_user(&table, &());
    let mut handlers = hlist![FirstHandler, SecondHandler];
    assert!(handlers
        .dispatch(&request(UNKNOWN, 42), &cx)
        .unwrap()
        .is_none());
}

#[test]
fn malformed_owned_constructor_does_not_fall_through() {
    let table = table();
    let cx = EffectContext::with_user(&table, &());
    let malformed = Value::Con(FIRST, vec![]);
    let mut handlers = hlist![FirstHandler, SecondHandler];

    assert!(matches!(
        handlers.dispatch(&malformed, &cx),
        Err(EffectError::Bridge(BridgeError::ArityMismatch { .. }))
    ));
}
