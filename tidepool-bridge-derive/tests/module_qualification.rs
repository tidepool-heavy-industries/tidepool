//! `#[core(module = "...")]` qualified-name disambiguation.
//!
//! When two types declare variants that share both unqualified name AND arity
//! but come from different source modules, the plain name+arity lookup can't
//! pick between them. Tagging each variant with its source module enables the
//! derive to emit `DataConTable::get_by_qualified_name("<module>.<name>")`
//! instead, producing a single unambiguous `DataConId`.

use tidepool_bridge::{BridgeError, FromCore, ToCore};
use tidepool_bridge_derive::{FromCore, ToCore};
use tidepool_eval::Value;
use tidepool_repr::{DataCon, DataConId, DataConTable};

// Nullary variants keep the test focused on DataCon lookup — avoids pulling
// in `Text` / `C#` / list-constructor table-setup just to carry a payload.
#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
enum Alpha {
    #[core(module = "TestMod.Alpha", name = "Read")]
    Read,
}

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
enum Beta {
    #[core(module = "TestMod.Beta", name = "Read")]
    Read,
}

/// Build a table containing both `TestMod.Alpha.Read` and
/// `TestMod.Beta.Read` at distinct `DataConId`s with identical
/// unqualified names and arities.
fn build_collision_table() -> (DataConTable, DataConId, DataConId) {
    let mut table = DataConTable::new();
    let alpha_id = DataConId(1);
    let beta_id = DataConId(2);

    table.insert(DataCon {
        id: alpha_id,
        name: "Read".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: Some("TestMod.Alpha.Read".to_string()),
    });
    table.insert(DataCon {
        id: beta_id,
        name: "Read".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: Some("TestMod.Beta.Read".to_string()),
    });
    (table, alpha_id, beta_id)
}

#[test]
fn alpha_roundtrips_when_beta_shares_name() {
    let (table, _, _) = build_collision_table();
    let original = Alpha::Read;
    let value = original.to_value(&table).expect("to_value");
    match &value {
        Value::Con(id, _) => assert_eq!(id.0, 1, "alpha encode should use TestMod.Alpha.Read id"),
        other => panic!("expected Con, got {other:?}"),
    }
    let decoded = Alpha::from_value(&value, &table).expect("from_value");
    assert_eq!(decoded, original);
}

#[test]
fn beta_roundtrips_when_alpha_shares_name() {
    let (table, _, _) = build_collision_table();
    let original = Beta::Read;
    let value = original.to_value(&table).expect("to_value");
    match &value {
        Value::Con(id, _) => assert_eq!(id.0, 2, "beta encode should use TestMod.Beta.Read id"),
        other => panic!("expected Con, got {other:?}"),
    }
    let decoded = Beta::from_value(&value, &table).expect("from_value");
    assert_eq!(decoded, original);
}

#[test]
fn alpha_decode_rejects_beta_shaped_value() {
    // A `Value::Con` carrying Beta's DataConId must NOT decode as Alpha.
    // With qualified lookup, Alpha's decoder looks up TestMod.Alpha.Read's id
    // (which is 1), the incoming id is 2 (Beta), so they differ → the
    // branch short-circuits and the decoder reports UnknownDataCon.
    let (table, _alpha_id, beta_id) = build_collision_table();
    let beta_value = Value::Con(beta_id, vec![]);
    let err = Alpha::from_value(&beta_value, &table).expect_err("alpha should reject beta id");
    match err {
        BridgeError::UnknownDataCon(id) => assert_eq!(id, beta_id),
        other => panic!("expected UnknownDataCon(Beta), got {other:?}"),
    }
}

#[test]
fn unknown_qualified_name_reports_qualified_error() {
    // Build a table that is MISSING both qualified entries. The derive's
    // lookup must then surface the `UnknownDataConQualified` variant, which
    // carries the full qualified path for diagnostic clarity.
    let table = DataConTable::new();
    let err = Alpha::Read
        .to_value(&table)
        .expect_err("encode must fail without qualified entry");
    match err {
        BridgeError::UnknownDataConQualified { qualified_name } => {
            assert_eq!(qualified_name, "TestMod.Alpha.Read");
        }
        other => panic!("expected UnknownDataConQualified, got {other:?}"),
    }
}
