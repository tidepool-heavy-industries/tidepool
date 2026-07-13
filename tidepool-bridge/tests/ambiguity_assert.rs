//! Cross-module unqualified-name ambiguity must be handled resiliently
//! (diagnostic + deterministic fallback) rather than silently, in the
//! hand-written `FromCore`/`ToCore` impls.
//!
//! The hand-written impls in `tidepool-bridge/src/impls.rs` resolve
//! unqualified constructor names (e.g. "I#") via `get_resilient(table, name,
//! arity)`, which tries the arity-qualified `get_by_name_arity` first and
//! only falls back to the first `get_all_by_name` match (with a
//! `cfg(debug_assertions)` diagnostic) when arity resolution also fails to
//! disambiguate. As of PR #293 (`45516fe8`, softening PR #291's over-strict
//! debug_asserts), an ambiguous match no longer panics — it returns the
//! fallback match deterministically. The fallback path still isn't module-
//! qualified and should eventually migrate to a `get_by_qualified_name`.
//!
//! Without this regression test the fallback behavior could be silently
//! changed (e.g. picking a different match, or reintroducing a panic) and
//! the existing roundtrip / proptest suites would not notice — they all
//! build tables with unique names.

use tidepool_bridge::ToCore;
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
    });
    t.insert(DataCon {
        id: DataConId(200),
        name: "I#".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: Some("UserDefined.I#".into()),
    });
    t
}

#[test]
#[cfg(debug_assertions)]
fn i_hash_ambiguity_no_longer_panics() {
    let table = ambiguous_i_hash_table();
    // The i64 ToCore impl looks up "I#" via get_resilient; it now issues
    // a diagnostic instead of panicking and falls back to a deterministic match.
    // get_by_name_arity returns the LAST inserted match (rev order).
    let result = 42i64
        .to_value(&table)
        .expect("unambiguous I# must encode cleanly");
    if let tidepool_eval::Value::Con(id, _) = result {
        // Should return the last match in the table: DataConId(200)
        assert_eq!(id, DataConId(200));
    } else {
        panic!("expected Value::Con");
    }
}

/// Sanity check that the assertion does NOT fire when the table is
/// well-formed (single entry per name) — guards against false positives
/// where the assert would trip on legitimate single-module compilation.
#[test]
fn unambiguous_i_hash_does_not_trip_assert() {
    let mut t = DataConTable::new();
    t.insert(DataCon {
        id: DataConId(100),
        name: "I#".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: Some("GHC.Internal.Types.I#".into()),
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
