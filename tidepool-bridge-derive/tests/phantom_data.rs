//! `PhantomData<T>` fields are excluded from the Core representation by the
//! `FromCore`/`ToCore` derives.
//!
//! Two properties to guard:
//!
//! 1. **Arity**: a Rust struct/variant with `N` total fields and `K` phantom
//!    fields encodes to a Core `Con` with arity `N - K`. The derived decoder
//!    must look up the constructor at the *Core* arity, not the Rust arity.
//! 2. **No bound leak**: `T` in `PhantomData<T>` must not require `FromCore`
//!    / `ToCore`. The phantom-only struct below uses a `T` that intentionally
//!    has no bridge impls; this file failing to compile would prove the bound
//!    leaked.

use std::marker::PhantomData;

use tidepool_bridge::{FromCore, ToCore};
use tidepool_bridge_derive::{FromCore, ToCore};
use tidepool_eval::Value;
use tidepool_repr::{DataCon, DataConId, DataConTable};
use tidepool_testing::gen::datacon_table::standard_datacon_table;

/// A type with no `FromCore`/`ToCore` impls. Using it inside `PhantomData<_>`
/// proves the derive does not require bridge bounds on phantom type params.
#[derive(Debug, PartialEq, Eq)]
struct NoBridgeImpl;

// ──────────────────────────────────────────────────────────────────────
// Enum coverage
// ──────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
enum WithPhantom {
    /// Phantom-only variant: 1 Rust field, 0 Core fields.
    #[core(name = "Tagged")]
    Tagged(PhantomData<NoBridgeImpl>),

    /// Mixed: 1 Rust phantom + 1 Rust real = 1 Core field.
    #[core(name = "Mixed")]
    Mixed(PhantomData<NoBridgeImpl>, i64),

    /// Real-only: 1 Rust real = 1 Core field. Sanity check that the
    /// non-phantom path is unaffected.
    #[core(name = "Plain")]
    Plain(i64),
}

fn build_enum_table() -> DataConTable {
    let mut t = standard_datacon_table();
    t.insert(DataCon {
        id: DataConId(1000),
        name: "Tagged".into(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    t.insert(DataCon {
        id: DataConId(1001),
        name: "Mixed".into(),
        tag: 2,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
    });
    t.insert(DataCon {
        id: DataConId(1002),
        name: "Plain".into(),
        tag: 3,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
    });
    t
}

#[test]
fn phantom_only_variant_encodes_zero_core_fields() {
    let table = build_enum_table();
    let v = WithPhantom::Tagged(PhantomData);
    let encoded = v.to_value(&table).expect("Tagged encode");
    match encoded {
        Value::Con(id, ref fields) => {
            assert_eq!(id, DataConId(1000));
            assert!(
                fields.is_empty(),
                "phantom-only variant must encode to a 0-field Con, got {} fields",
                fields.len()
            );
        }
        _ => panic!("expected Con, got {:?}", encoded),
    }
    let decoded = WithPhantom::from_value(&encoded, &table).expect("Tagged decode");
    assert_eq!(decoded, v);
}

#[test]
fn mixed_variant_encodes_only_real_fields() {
    let table = build_enum_table();
    let v = WithPhantom::Mixed(PhantomData, 42);
    let encoded = v.to_value(&table).expect("Mixed encode");
    match &encoded {
        Value::Con(id, fields) => {
            assert_eq!(*id, DataConId(1001));
            assert_eq!(
                fields.len(),
                1,
                "mixed variant must encode 1 Core field (the Int), not 2",
            );
        }
        _ => panic!("expected Con, got {:?}", encoded),
    }
    let decoded = WithPhantom::from_value(&encoded, &table).expect("Mixed decode");
    assert_eq!(decoded, v);
}

#[test]
fn real_only_variant_unaffected_by_phantom_path() {
    let table = build_enum_table();
    let v = WithPhantom::Plain(7);
    let encoded = v.to_value(&table).expect("Plain encode");
    match &encoded {
        Value::Con(id, fields) => {
            assert_eq!(*id, DataConId(1002));
            assert_eq!(fields.len(), 1);
        }
        _ => panic!("expected Con, got {:?}", encoded),
    }
    let decoded = WithPhantom::from_value(&encoded, &table).expect("Plain decode");
    assert_eq!(decoded, v);
}

// ──────────────────────────────────────────────────────────────────────
// Struct coverage
// ──────────────────────────────────────────────────────────────────────

/// Mixed struct: phantom field interleaved with a real field. Tests that the
/// derived destructure pattern (`name: _` for phantom slots) and the encoded
/// field order both stay correct.
#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
#[core(name = "MixStruct")]
struct MixStruct {
    payload: i64,
    _marker: PhantomData<NoBridgeImpl>,
}

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
#[core(name = "PhantomStruct")]
struct PhantomStruct {
    _marker: PhantomData<NoBridgeImpl>,
}

fn build_struct_table() -> DataConTable {
    let mut t = standard_datacon_table();
    t.insert(DataCon {
        id: DataConId(2000),
        name: "MixStruct".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
    });
    t.insert(DataCon {
        id: DataConId(2001),
        name: "PhantomStruct".into(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    t
}

#[test]
fn struct_with_mixed_fields_round_trips() {
    let table = build_struct_table();
    let v = MixStruct {
        payload: 99,
        _marker: PhantomData,
    };
    let encoded = v.to_value(&table).expect("MixStruct encode");
    match &encoded {
        Value::Con(id, fields) => {
            assert_eq!(*id, DataConId(2000));
            assert_eq!(
                fields.len(),
                1,
                "MixStruct has 1 real + 1 phantom field; Core encoding must be arity 1",
            );
        }
        _ => panic!("expected Con, got {:?}", encoded),
    }
    let decoded = MixStruct::from_value(&encoded, &table).expect("MixStruct decode");
    assert_eq!(decoded, v);
}

#[test]
fn phantom_only_struct_encodes_zero_fields() {
    let table = build_struct_table();
    let v = PhantomStruct {
        _marker: PhantomData,
    };
    let encoded = v.to_value(&table).expect("PhantomStruct encode");
    match &encoded {
        Value::Con(id, fields) => {
            assert_eq!(*id, DataConId(2001));
            assert!(fields.is_empty(), "phantom-only struct encodes 0 fields");
        }
        _ => panic!("expected Con, got {:?}", encoded),
    }
    let decoded = PhantomStruct::from_value(&encoded, &table).expect("PhantomStruct decode");
    assert_eq!(decoded, v);
}
