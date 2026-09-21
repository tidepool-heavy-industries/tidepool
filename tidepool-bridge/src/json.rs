//! Bridge between `serde_json::Value` and materialized Tidepool values.
//!
//! Delegates to the bridge-owned shared JSON builder used by native host
//! functions and effect results.

use crate::error::BridgeError;
use crate::traits::{sealed::ToHaskellSealed, ToHaskell};
use crate::Value;
use tidepool_repr::DataConTable;

impl ToHaskellSealed for serde_json::Value {}

/// Convert a `serde_json::Value` to a Tidepool `Value` matching the
/// vendored `Tidepool.Aeson.Value` Haskell type.
///
/// Uses the bridge-owned JSON materialization builder.
impl ToHaskell for serde_json::Value {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let ids = crate::json_builder::JsonConIds::from_table(table).ok_or_else(|| {
            BridgeError::UnknownDataConName(
                "aeson Value constructors (Object/Array/String/…) not in scope".into(),
            )
        })?;
        Ok(crate::json_builder::json_to_value(self, &ids))
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

    /// Smoke test for the public JSON conversion path. Detailed shape coverage
    /// lives beside the shared JSON builder and in the native JsonDecode tests.
    #[test]
    fn to_value_uses_the_shared_json_builder() {
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
