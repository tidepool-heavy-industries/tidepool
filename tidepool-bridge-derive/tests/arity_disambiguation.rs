//! Regression tests for DataCon lookup by (name, arity) in `FromCore`/`ToCore` derives.
//!
//! Two GADTs from different Haskell modules can declare same-named constructors
//! (e.g. `Pattern.Memory.Read` and `Pattern.File.Read`). The derive must
//! disambiguate by arity so decoding doesn't fail with "Unknown DataCon name".

use tidepool_bridge::{BridgeError, FromCore, ToCore};
use tidepool_bridge_derive::{FromCore, ToCore};
use tidepool_eval::Value;
use tidepool_repr::{DataCon, DataConId, DataConTable};
use tidepool_testing::gen::datacon_table::standard_datacon_table;

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
enum Alpha {
    #[core(name = "Read")]
    Read(i64),
}

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
enum Beta {
    #[core(name = "Read")]
    Read(i64, u64),
}

/// Build a DataConTable holding two constructors sharing the unqualified name
/// "Read" but with different arities — the scenario that was failing. Built
/// on top of the standard table so primitive boxing constructors (`I#`, `W#`)
/// are available for field encoding.
fn ambiguous_table() -> (DataConTable, DataConId, DataConId) {
    let mut t = standard_datacon_table();
    let alpha_id = DataConId(100);
    let beta_id = DataConId(101);
    t.insert(DataCon {
        id: alpha_id,
        name: "Read".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: Some("Pattern.Memory.Read".into()),
    });
    t.insert(DataCon {
        id: beta_id,
        name: "Read".into(),
        tag: 1,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: Some("Pattern.File.Read".into()),
    });
    (t, alpha_id, beta_id)
}

#[test]
fn alpha_roundtrips_when_beta_shares_name() {
    let (table, alpha_id, _) = ambiguous_table();
    let original = Alpha::Read(17);

    let encoded = original
        .to_value(&table)
        .expect("Alpha encode must succeed");
    match &encoded {
        Value::Con(id, fields) => {
            assert_eq!(*id, alpha_id, "must encode to the arity-1 Read id");
            assert_eq!(fields.len(), 1, "Alpha encodes with 1 Core field");
        }
        other => panic!("expected Con, got {:?}", other),
    }

    let decoded = Alpha::from_value(&encoded, &table).expect("Alpha decode must succeed");
    assert_eq!(original, decoded);
}

#[test]
fn beta_roundtrips_when_alpha_shares_name() {
    let (table, _, beta_id) = ambiguous_table();
    let original = Beta::Read(3, 42);

    let encoded = original.to_value(&table).expect("Beta encode must succeed");
    match &encoded {
        Value::Con(id, fields) => {
            assert_eq!(*id, beta_id, "must encode to the arity-2 Read id");
            assert_eq!(fields.len(), 2, "Beta encodes with 2 Core fields");
        }
        other => panic!("expected Con, got {:?}", other),
    }

    let decoded = Beta::from_value(&encoded, &table).expect("Beta decode must succeed");
    assert_eq!(original, decoded);
}

#[test]
fn alpha_decode_rejects_beta_shaped_value() {
    // Negative case: Alpha's decoder fed a Con tagged with the Beta id.
    // The id lookup resolves to the arity-1 Read; the supplied id is the
    // arity-2 Read; they differ, so decode must error (not silently succeed).
    let (table, _alpha_id, beta_id) = ambiguous_table();

    // Build a syntactically shaped Con with 2 dummy fields — Alpha's decoder
    // should bail at the id check before touching the fields.
    let dummy = Value::Con(DataConId(0), vec![]);
    let beta_value = Value::Con(beta_id, vec![dummy.clone(), dummy]);

    let result = Alpha::from_value(&beta_value, &table);
    match result {
        Err(BridgeError::UnknownDataCon(id)) => {
            assert_eq!(id, beta_id, "error should name the Beta id we supplied");
        }
        Err(other) => panic!("expected UnknownDataCon, got {:?}", other),
        Ok(v) => panic!("expected error, got successful decode: {:?}", v),
    }
}

#[test]
fn unknown_name_reports_arity() {
    // When a constructor name exists but not at the requested arity, we should
    // get UnknownDataConNameArity identifying the name and the expected arity.
    let mut t = standard_datacon_table();
    t.insert(DataCon {
        id: DataConId(200),
        name: "Read".into(),
        tag: 1,
        rep_arity: 5, // neither Alpha's 1 nor Beta's 2
        field_bangs: vec![],
        qualified_name: None,
    });

    let val = Alpha::Read(1);
    let err = val.to_value(&t).expect_err("no arity-1 Read present");
    match err {
        BridgeError::UnknownDataConNameArity { name, arity } => {
            assert_eq!(name, "Read");
            assert_eq!(arity, 1);
        }
        other => panic!("expected UnknownDataConNameArity, got {:?}", other),
    }
}
