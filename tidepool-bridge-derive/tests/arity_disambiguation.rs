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
        type_name: String::new(),
    });
    t.insert(DataCon {
        id: beta_id,
        name: "Read".into(),
        tag: 1,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: Some("Pattern.File.Read".into()),
        type_name: String::new(),
    });
    (t, alpha_id, beta_id)
}

#[test]
fn arity_alpha_roundtrips_when_beta_shares_name() {
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

/// Two DISTINCT constructors sharing both the requested name AND arity
/// (e.g. two independently-compiled modules each contributing a `Read`
/// with the same shape) must make encode fail with `AmbiguousDataConNameArity`
/// naming both candidates — not silently resolve to whichever was inserted
/// last. Fails on the old code (`get_by_name_arity`'s "last matching entry"
/// tie-break), which would have picked one of the two Reads by insertion
/// order alone.
#[test]
fn true_ambiguity_reports_both_candidates_instead_of_picking_last() {
    let mut t = standard_datacon_table();
    // Two distinct arity-1 "Read" constructors — genuinely ambiguous, unlike
    // `ambiguous_table()` above where arity itself disambiguates Alpha/Beta.
    t.insert(DataCon {
        id: DataConId(300),
        name: "Read".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: Some("Pattern.Memory.Read".into()),
        type_name: String::new(),
    });
    t.insert(DataCon {
        id: DataConId(301),
        name: "Read".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: Some("Pattern.File.Read".into()),
        type_name: String::new(),
    });

    let err = Alpha::Read(17)
        .to_value(&t)
        .expect_err("two distinct Read/1 constructors must be ambiguous, not resolved");
    match err {
        BridgeError::AmbiguousDataConNameArity {
            name,
            arity,
            candidates,
        } => {
            assert_eq!(name, "Read");
            assert_eq!(arity, 1);
            assert_eq!(candidates.len(), 2);
            assert!(candidates.contains(&"Pattern.Memory.Read".to_string()));
            assert!(candidates.contains(&"Pattern.File.Read".to_string()));
        }
        other => panic!("expected AmbiguousDataConNameArity, got {other:?}"),
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
        type_name: String::new(),
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
