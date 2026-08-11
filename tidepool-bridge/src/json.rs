//! Bridge between `serde_json::Value` and Tidepool Core values.
//!
//! Delegates entirely to `tidepool_eval::json` — the single shared builder used
//! by both the `JsonDecode` primop (eval + JIT) and bridge effect results. See
//! `tidepool-eval/src/json.rs` for the canonical representation docs.

use crate::error::BridgeError;
use crate::traits::{sealed::ToCoreSealed, ToCore};
use tidepool_eval::Value;
use tidepool_repr::DataConTable;

impl ToCoreSealed for serde_json::Value {}

/// Convert a `serde_json::Value` to a Tidepool Core `Value` matching the
/// vendored `Tidepool.Aeson.Value` Haskell type.
///
/// Delegates to `tidepool_eval::json::json_to_value` — the same builder the
/// `JsonDecode` primop uses, so JIT, eval, and bridge effect results all agree
/// by construction.
impl ToCore for serde_json::Value {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let ids = tidepool_eval::json::JsonConIds::from_table(table).ok_or_else(|| {
            BridgeError::UnknownDataConName(
                "aeson Value constructors (Object/Array/String/…) not in scope".into(),
            )
        })?;
        Ok(tidepool_eval::json::json_to_value(self, &ids))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{DataCon, DataConId};

    /// Build a DataConTable with all constructors needed for JSON values.
    fn json_test_table() -> DataConTable {
        let mut t = DataConTable::new();
        let cons = [
            // Value constructors
            ("Object", 0, 1),
            ("Array", 1, 1),
            ("String", 2, 1),
            ("Number", 3, 1),
            ("Bool", 4, 1),
            ("Null", 5, 0),
            // Map constructors
            ("Bin", 6, 5),
            ("Tip", 7, 0),
            // Bool values
            ("True", 8, 0),
            ("False", 9, 0),
            // List
            ("[]", 10, 0),
            (":", 11, 2),
            // Number carrier: Scientific coefficient×10^exponent (exact)
            ("Scientific", 1, 2),
            // Integer constructors for the Scientific coefficient
            ("IS", 1, 1),
            ("IP", 2, 1),
            ("IN", 3, 1),
            // Text
            ("Text", 12, 3),
            // Int boxing
            ("I#", 13, 1),
        ];

        for (i, (name, tag, arity)) in cons.iter().enumerate() {
            t.insert(DataCon {
                id: DataConId(i as u64),
                name: (*name).into(),
                tag: *tag,
                rep_arity: *arity,
                field_bangs: vec![],
                qualified_name: None,
                type_name: String::new(),
            });
        }
        t
    }

    /// Smoke test for the shim itself: delegation to `tidepool_eval::json`
    /// actually happens and produces a `Value`. Shape coverage (Object/Array/
    /// String/Number/Bool/Null, Map Bin/Tip, the Scientific coefficient
    /// encoding) lives in `tidepool_eval::json`'s own tests and
    /// `codegen/tests/json_decode_differential.rs`, which pin the shape
    /// through the real `JsonDecode` primop path on both eval and the JIT.
    #[test]
    fn to_value_delegates_to_eval_json() {
        let table = json_test_table();
        let val = serde_json::Value::Null.to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Null"));
                assert!(fields.is_empty());
            }
            _ => panic!("Expected Con(Null)"),
        }
    }

    /// The shim's one genuine failure mode: a table with none of the aeson
    /// `Value` constructors in scope (no `JsonDecode`/JSON-effect use in the
    /// program) must report `UnknownDataConName`, not panic or silently
    /// build a garbage `Value`.
    #[test]
    fn to_value_reports_unknown_dataconname_when_json_constructors_absent() {
        let table = DataConTable::new();
        let err = serde_json::json!({"a": 1}).to_value(&table).unwrap_err();
        assert!(
            matches!(err, BridgeError::UnknownDataConName(_)),
            "expected UnknownDataConName, got {err:?}"
        );
    }
}
