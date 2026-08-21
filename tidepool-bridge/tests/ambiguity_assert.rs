//! Cross-module unqualified-name ambiguity must fail LOUDLY, not silently
//! pick a constructor, in the hand-written `FromCore`/`ToCore` impls.
//!
//! The hand-written impls in `tidepool-bridge/src/impls.rs` resolve
//! unqualified constructor names (e.g. "I#") via `get_resilient(table, name,
//! arity)`. It used to fall back to an arbitrary same-name/arity match
//! (deterministic, but not correctness-preserving: insertion order — not
//! type identity — decided which constructor won) and, absent an exact
//! arity match, to the first same-name entry at ANY arity — which could
//! build a `Value::Con` whose metadata arity disagreed with its actual
//! field count. Both fallbacks are gone: `get_resilient` now returns `None`
//! whenever resolution isn't unique, and every callsite's existing
//! `.ok_or_else(...)` turns that into a `BridgeError` instead of a
//! silently-wrong encode.
//!
//! Without this regression test the strict behavior could be silently
//! reintroduced as a fallback and the existing roundtrip / proptest suites
//! would not notice — they all build tables with unique names.

use tidepool_bridge::{BridgeError, ToCore};
use tidepool_repr::{DataCon, DataConId, DataConTable};

/// Build a table containing two `I#` entries at distinct `DataConId`s,
/// simulating cross-module compilation where the same unqualified name
/// is introduced by independently-compiled modules.
fn ambiguous_i_hash_table() -> DataConTable {
    let mut t = DataConTable::new();
    t.insert(DataCon {
        id: DataConId(100),
        name: "I#".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: Some("GHC.Internal.Types.I#".into()),
        type_name: String::new(),
    });
    t.insert(DataCon {
        id: DataConId(200),
        name: "I#".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: Some("UserDefined.I#".into()),
        type_name: String::new(),
    });
    t
}

/// Two distinct `I#` constructors sharing name+arity must make `to_value`
/// fail with `UnknownDataConName` (the same error an entirely-absent
/// constructor produces) — never silently resolve to one of them by
/// insertion order.
#[test]
fn i_hash_ambiguity_errors_instead_of_picking_last_inserted() {
    let table = ambiguous_i_hash_table();
    let err = 42i64
        .to_value(&table)
        .expect_err("ambiguous I# must not encode — no single correct choice exists");
    assert!(
        matches!(err, BridgeError::UnknownDataConName(ref s) if s == "I#"),
        "expected UnknownDataConName(\"I#\"), got {err:?}"
    );
}

/// Sanity check that resolution does NOT fail when the table is
/// well-formed (single entry per name) — guards against false positives
/// where the strict path would trip on legitimate single-module compilation.
#[test]
fn unambiguous_i_hash_resolves_cleanly() {
    let mut t = DataConTable::new();
    t.insert(DataCon {
        id: DataConId(100),
        name: "I#".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: Some("GHC.Internal.Types.I#".into()),
        type_name: String::new(),
    });
    let result = 42i64
        .to_value(&t)
        .expect("unambiguous I# must encode cleanly");
    if let tidepool_eval::Value::Con(id, _) = result {
        assert_eq!(id, DataConId(100));
    } else {
        panic!("expected Value::Con");
    }
}

/// A same-named constructor at a DIFFERENT arity must not be used as a
/// fallback: `get_resilient` must report absence, not build a `Value::Con`
/// whose metadata arity disagrees with the field it's about to carry.
#[test]
fn wrong_arity_same_name_is_not_a_fallback_candidate() {
    let mut t = DataConTable::new();
    // "I#" exists, but only at arity 2 — never the arity-1 shape i64's
    // ToCore impl requests. The old fallback (`matches.first().copied()`)
    // would have returned this id anyway.
    t.insert(DataCon {
        id: DataConId(100),
        name: "I#".into(),
        tag: 1,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: Some("Some.Other.I#".into()),
        type_name: String::new(),
    });
    let err = 42i64
        .to_value(&t)
        .expect_err("no arity-1 I# present — must not fall back to the arity-2 entry");
    assert!(
        matches!(err, BridgeError::UnknownDataConName(ref s) if s == "I#"),
        "expected UnknownDataConName(\"I#\"), got {err:?}"
    );
}
