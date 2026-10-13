//! Derive lookup refuses multiple nominal owners before checking shape.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
use tidepool_bridge::HaskellValue;
use tidepool_bridge::{BridgeError, FromHaskell, ToHaskell};
use tidepool_bridge_derive::{FromHaskell, ToHaskell};
use tidepool_repr::{DataCon, DataConId, DataConTable};
use tidepool_test_data::standard_datacon_table;

#[derive(Debug, PartialEq, Eq, FromHaskell, ToHaskell)]
enum Alpha {
    #[haskell(name = "Read")]
    Read(i64),
}

#[derive(Debug, PartialEq, Eq, FromHaskell, ToHaskell)]
enum Beta {
    #[haskell(name = "Read")]
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
    t.insert_checked(DataCon {
        identity: tidepool_repr::execution_schema::testing::metadata_identity(
            tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "Pattern.Memory".into(),
                namespace: "constructor".into(),
                occurrence: "Read".into(),
                record_parent: None,
            },
            "FixtureType",
        ),
        id: alpha_id,
        name: "Read".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: Some("Pattern.Memory.Read".into()),
        type_name: String::new(),
    })
    .expect("valid fixture metadata");
    t.insert_checked(DataCon {
        identity: tidepool_repr::execution_schema::testing::metadata_identity(
            tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "Pattern.File".into(),
                namespace: "constructor".into(),
                occurrence: "Read".into(),
                record_parent: None,
            },
            "FixtureType",
        ),
        id: beta_id,
        name: "Read".into(),
        tag: 1,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: Some("Pattern.File.Read".into()),
        type_name: String::new(),
    })
    .expect("valid fixture metadata");
    (t, alpha_id, beta_id)
}

#[test]
fn differing_arities_do_not_select_nominal_authority() {
    let (table, _, _) = ambiguous_table();
    assert!(matches!(
        Alpha::Read(17).to_value(&table),
        Err(BridgeError::AmbiguousDataConNameArity { .. })
    ));
    assert!(matches!(
        Beta::Read(3, 42).to_value(&table),
        Err(BridgeError::AmbiguousDataConNameArity { .. })
    ));
}

#[test]
fn unique_owner_roundtrips_and_repeated_admission_is_idempotent() {
    let (original_table, alpha_id, _) = ambiguous_table();
    let row = original_table.get(alpha_id).unwrap().clone();
    let mut table = standard_datacon_table();
    table.extend_checked([row.clone(), row]).unwrap();
    let original = Alpha::Read(17);
    let encoded = original.to_value(&table).unwrap();
    assert_eq!(Alpha::from_value(&encoded, &table).unwrap(), original);
}

#[test]
fn decoding_refuses_ambiguous_authority_before_shape() {
    let (table, _, beta_id) = ambiguous_table();
    let dummy = HaskellValue::Con(DataConId(0), vec![]);
    let beta_value = HaskellValue::Con(beta_id, vec![dummy.clone(), dummy]);
    assert!(matches!(
        Alpha::from_value(&beta_value, &table),
        Err(BridgeError::UnknownDataCon(id)) if id == beta_id
    ));
}

#[test]
fn same_symbol_family_revisions_refuse_arity_selection() {
    let (original_table, alpha_id, _) = ambiguous_table();
    let first = original_table.get(alpha_id).unwrap().clone();
    let mut next = first.clone();
    next.id = DataConId(200);
    next.rep_arity = 2;
    next.identity.family.version = tidepool_repr::execution_schema::TypeVersion::GhcAbi([1; 16]);
    let mut table = standard_datacon_table();
    table.extend_checked([first, next]).unwrap();
    assert!(matches!(
        Alpha::Read(17).to_value(&table),
        Err(BridgeError::AmbiguousDataConNameArity { .. })
    ));
    assert!(matches!(
        Beta::Read(3, 42).to_value(&table),
        Err(BridgeError::AmbiguousDataConNameArity { .. })
    ));
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
    // Both constructor identities remain visible in the refusal.
    t.insert_checked(DataCon {
        identity: tidepool_repr::execution_schema::testing::metadata_identity(
            tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "Pattern.Memory".into(),
                namespace: "constructor".into(),
                occurrence: "Read".into(),
                record_parent: None,
            },
            "FixtureType",
        ),
        id: DataConId(300),
        name: "Read".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: Some("Pattern.Memory.Read".into()),
        type_name: String::new(),
    })
    .expect("valid fixture metadata");
    t.insert_checked(DataCon {
        identity: tidepool_repr::execution_schema::testing::metadata_identity(
            tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "Pattern.File".into(),
                namespace: "constructor".into(),
                occurrence: "Read".into(),
                record_parent: None,
            },
            "FixtureType",
        ),
        id: DataConId(301),
        name: "Read".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: Some("Pattern.File.Read".into()),
        type_name: String::new(),
    })
    .expect("valid fixture metadata");

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
            assert!(candidates
                .iter()
                .any(|candidate| candidate.symbol.module == "Pattern.Memory"
                    && candidate.symbol.occurrence == "Read"));
            assert!(candidates
                .iter()
                .any(|candidate| candidate.symbol.module == "Pattern.File"
                    && candidate.symbol.occurrence == "Read"));
        }
        other => panic!("expected AmbiguousDataConNameArity, got {other:?}"),
    }
}

#[test]
fn unknown_name_reports_arity() {
    // When a constructor name exists but not at the requested arity, we should
    // get UnknownDataConNameArity identifying the name and the expected arity.
    let mut t = standard_datacon_table();
    t.insert_checked(DataCon {
        identity: tidepool_repr::execution_schema::testing::metadata_identity(
            tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "Fixture".into(),
                namespace: "constructor".into(),
                occurrence: "Read".into(),
                record_parent: None,
            },
            "FixtureType",
        ),
        id: DataConId(200),
        name: "Read".into(),
        tag: 1,
        rep_arity: 5, // neither Alpha's 1 nor Beta's 2
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    })
    .expect("valid fixture metadata");

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
