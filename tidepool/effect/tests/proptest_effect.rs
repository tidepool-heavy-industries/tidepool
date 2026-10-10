#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
use frunk::hlist;
use proptest::prelude::*;
use tidepool_bridge::{BridgeError, FromHaskell, HaskellValue};
use tidepool_effect::dispatch::{DispatchEffect, EffectContext, EffectHandler, Response};
use tidepool_effect::error::EffectError;
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
        table
            .insert_checked(DataCon {
                identity: tidepool_repr::execution_schema::SymbolIdentity {
                    unit: "fixture".into(),
                    module: "Fixture".into(),
                    namespace: "constructor".into(),
                    occurrence: name.to_owned(),
                    record_parent: None,
                },
                id,
                name: name.into(),
                tag: id.0 as u32,
                rep_arity: 1,
                field_bangs: vec![],
                qualified_name: Some(format!("Test.{name}")),
                type_name: "TestRequest".into(),
            })
            .expect("valid fixture metadata");
    }
    table
}

fn request(id: DataConId, value: i64) -> HaskellValue {
    HaskellValue::Con(id, vec![HaskellValue::Lit(Literal::LitInt(value))])
}

fn decode_request(value: &HaskellValue, expected_id: DataConId) -> Result<i64, BridgeError> {
    let HaskellValue::Con(id, fields) = value else {
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
        HaskellValue::Lit(Literal::LitInt(n)) => Ok(n),
        ref other => Err(BridgeError::TypeMismatch {
            expected: "LitInt".into(),
            got: format!("{other:?}"),
        }),
    }
}

struct FirstRequest(i64);
impl tidepool_bridge::sealed::FromHaskellSealed for FirstRequest {}
impl FromHaskell for FirstRequest {
    fn from_value(value: &HaskellValue, _table: &DataConTable) -> Result<Self, BridgeError> {
        decode_request(value, FIRST).map(Self)
    }
}

struct SecondRequest(i64);
impl tidepool_bridge::sealed::FromHaskellSealed for SecondRequest {}
impl FromHaskell for SecondRequest {
    fn from_value(value: &HaskellValue, _table: &DataConTable) -> Result<Self, BridgeError> {
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
        Ok(HaskellValue::Lit(Literal::LitInt(request.0 + 10)).into())
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
        Ok(HaskellValue::Lit(Literal::LitInt(request.0 + 20)).into())
    }
}

#[test]
fn prepared_dispatch_keeps_external_work_owned_and_preserves_principal() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tidepool_effect::dispatch::{DeferredEffect, EffectDispatch};
    use tidepool_repr::PrincipalId;

    struct DeferredHandler(Arc<AtomicUsize>);
    impl EffectHandler for DeferredHandler {
        type Request = FirstRequest;

        fn handle(
            &mut self,
            _request: FirstRequest,
            _cx: &EffectContext<'_>,
        ) -> Result<Response, EffectError> {
            panic!("deferred handler must not execute synchronously")
        }

        fn prepare(
            &mut self,
            request: FirstRequest,
            cx: &EffectContext<'_>,
        ) -> Result<EffectDispatch, EffectError> {
            let principal = cx.principal();
            let calls = Arc::clone(&self.0);
            Ok(EffectDispatch::Deferred(DeferredEffect::blocking(
                move || {
                    assert_eq!(principal, PrincipalId::new(17, 3));
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(HaskellValue::Lit(Literal::LitInt(request.0 + 1)).into())
                },
            )))
        }
    }

    let table = table();
    let cx = EffectContext::with_principal(&table, PrincipalId::new(17, 3), &());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut handlers = hlist![DeferredHandler(Arc::clone(&calls)), SecondHandler];
    let deferred = handlers.prepare_dispatch(&request(FIRST, 41), &cx).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let EffectDispatch::Deferred(DeferredEffect::Blocking(work)) = deferred else {
        panic!("expected owned blocking work")
    };
    assert_eq!(
        completed_int(Some(work.into_inner().unwrap()().unwrap()), &table),
        42
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        handlers.prepare_dispatch(&request(UNKNOWN, 0), &cx),
        Ok(EffectDispatch::Unhandled)
    ));
}

fn completed_int(response: Option<Response>, table: &DataConTable) -> i64 {
    match response.map(|response| response.to_value(table)) {
        Some(Ok(HaskellValue::Lit(Literal::LitInt(n)))) => n,
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
            completed_int(forward.dispatch(&first, &cx).unwrap(), &table),
            value.wrapping_add(10),
        );
        prop_assert_eq!(
            completed_int(reverse.dispatch(&first, &cx).unwrap(), &table),
            value.wrapping_add(10),
        );
        prop_assert_eq!(
            completed_int(forward.dispatch(&second, &cx).unwrap(), &table),
            value.wrapping_add(20),
        );
        prop_assert_eq!(
            completed_int(reverse.dispatch(&second, &cx).unwrap(), &table),
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
    let malformed = HaskellValue::Con(FIRST, vec![]);
    let mut handlers = hlist![FirstHandler, SecondHandler];

    assert!(matches!(
        handlers.dispatch(&malformed, &cx),
        Err(EffectError::Bridge(BridgeError::ArityMismatch { .. }))
    ));
}
