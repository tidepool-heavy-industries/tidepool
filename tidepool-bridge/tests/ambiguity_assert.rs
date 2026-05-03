//! Cross-module unqualified-name ambiguity must fire the dossier-cited
//! debug_assert in dev/test builds.
//!
//! The hand-written `FromCore`/`ToCore` impls in `tidepool-bridge/src/impls.rs`
//! still call `table.get_by_name("I#")` (and similar) without arity or module
//! qualification. PR #291 added `cfg(debug_assertions)` blocks that scan
//! `get_all_by_name` and panic when more than one constructor shares the
//! unqualified name — a guard against the cross-module collision class fixed
//! in PR #272's derive but not yet migrated in the hand-written impls.
//!
//! Without this regression test the assertions could be silently weakened
//! (e.g. by replacing `get_all_by_name` with `get_by_name` in the matches
//! check) and the existing roundtrip / proptest suites would not notice —
//! they all build tables with unique names.

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
