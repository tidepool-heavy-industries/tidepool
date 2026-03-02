//! Bridge between `serde_json::Value` and Tidepool Core values.
//!
//! Converts serde_json JSON values to the vendored Tidepool.Aeson.Value ADT
//! representation in Core. Constructor names match exactly:
//!   Value = Object | Array | String | Number | Bool | Null
//!
//! KeyMap is backed by Data.Map.Strict (Map Key Value), so objects are
//! represented as balanced binary trees of (Key, Value) pairs.

use crate::error::BridgeError;
use crate::traits::ToCore;
use tidepool_eval::Value;
use tidepool_repr::{DataConTable, Literal};

/// Convert a `serde_json::Value` to a Tidepool Core `Value` matching the
/// vendored `Tidepool.Aeson.Value` Haskell type.
///
/// The resulting Core value can be passed to Haskell code that expects
/// `Value` (the aeson-compatible type) and accessed via lens combinators.
impl ToCore for serde_json::Value {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        // Use get_by_name_arity to disambiguate aeson Value constructors from
        // GHC-internal types that share the same unqualified name (e.g. "Array").
        match self {
            serde_json::Value::Null => {
                let id = table
                    .get_by_name_arity("Null", 0)
                    .ok_or_else(|| BridgeError::UnknownDataConName("Null".into()))?;
                Ok(Value::Con(id, vec![]))
            }

            serde_json::Value::Bool(b) => {
                let id = table
                    .get_by_name_arity("Bool", 1)
                    .ok_or_else(|| BridgeError::UnknownDataConName("Bool".into()))?;
                let inner = (*b).to_value(table)?;
                Ok(Value::Con(id, vec![inner]))
            }

            serde_json::Value::Number(n) => {
                let id = table
                    .get_by_name_arity("Number", 1)
                    .ok_or_else(|| BridgeError::UnknownDataConName("Number".into()))?;
                let f = n.as_f64().unwrap_or(0.0);
                Ok(Value::Con(
                    id,
                    vec![Value::Lit(Literal::LitDouble(f.to_bits()))],
                ))
            }

            serde_json::Value::String(s) => {
                let id = table
                    .get_by_name_arity("String", 1)
                    .ok_or_else(|| BridgeError::UnknownDataConName("String".into()))?;
                let inner = s.clone().to_value(table)?;
                Ok(Value::Con(id, vec![inner]))
            }

            serde_json::Value::Array(arr) => {
                let id = table
                    .get_by_name_arity("Array", 1)
                    .ok_or_else(|| BridgeError::UnknownDataConName("Array".into()))?;
                // Vendored Value uses [Value] for Array (cons-list, not Vector).
                let elements: Result<Vec<Value>, BridgeError> =
                    arr.iter().map(|v| v.to_value(table)).collect();
                let list = elements?.to_value(table)?;
                Ok(Value::Con(id, vec![list]))
            }

            serde_json::Value::Object(map) => {
                let id = table
                    .get_by_name_arity("Object", 1)
                    .ok_or_else(|| BridgeError::UnknownDataConName("Object".into()))?;
                // KeyMap = Map Key Value (backed by Data.Map.Strict)
                // Map is a balanced binary tree:
                //   data Map k v = Bin !Int !k !v !(Map k v) !(Map k v) | Tip
                let map_val = keymap_to_value(map, table)?;
                Ok(Value::Con(id, vec![map_val]))
            }
        }
    }
}

/// Build a Data.Map.Strict.Map Key Value from a serde_json Map.
///
/// Map is:
///   Bin :: Int -> k -> v -> Map k v -> Map k v -> Map k v
///   Tip :: Map k v
///
/// We build a balanced tree by sorting keys and using divide-and-conquer.
fn keymap_to_value(
    map: &serde_json::Map<std::string::String, serde_json::Value>,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let bin_id = table
        .get_by_name_arity("Bin", 5)
        .ok_or_else(|| BridgeError::UnknownDataConName("Bin".into()))?;
    let tip_id = table
        .get_by_name_arity("Tip", 0)
        .ok_or_else(|| BridgeError::UnknownDataConName("Tip".into()))?;
    // Bin's first field is !Int — boxed on the heap as I#(Int#)
    let i_hash_id = table
        .get_by_name_arity("I#", 1)
        .ok_or_else(|| BridgeError::UnknownDataConName("I#".into()))?;

    // Collect and sort entries by key for balanced tree construction
    let mut entries: Vec<(&std::string::String, &serde_json::Value)> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));

    fn build_tree(
        entries: &[(&std::string::String, &serde_json::Value)],
        bin_id: tidepool_repr::DataConId,
        tip_id: tidepool_repr::DataConId,
        i_hash_id: tidepool_repr::DataConId,
        table: &DataConTable,
    ) -> Result<Value, BridgeError> {
        if entries.is_empty() {
            return Ok(Value::Con(tip_id, vec![]));
        }
        let mid = entries.len() / 2;
        let (k, v) = entries[mid];
        let left = build_tree(&entries[..mid], bin_id, tip_id, i_hash_id, table)?;
        let right = build_tree(&entries[mid + 1..], bin_id, tip_id, i_hash_id, table)?;

        // Key is a newtype for Text — GHC erases it, so store plain Text
        let key_val = k.clone().to_value(table)?;

        let json_val = v.to_value(table)?;
        // Bin's !Int field must be boxed as I#(n) to match GHC's heap representation
        let size = Value::Con(
            i_hash_id,
            vec![Value::Lit(Literal::LitInt(entries.len() as i64))],
        );

        // Bin size key value left right
        Ok(Value::Con(
            bin_id,
            vec![size, key_val, json_val, left, right],
        ))
    }

    build_tree(&entries, bin_id, tip_id, i_hash_id, table)
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
            });
        }
        t
    }

    #[test]
    fn test_null_to_value() {
        let table = json_test_table();
        let json = serde_json::Value::Null;
        let val = json.to_value(&table).unwrap();
        match val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(id), Some("Null"));
                assert!(fields.is_empty());
            }
            _ => panic!("Expected Con(Null)"),
        }
    }

    #[test]
    fn test_bool_to_value() {
        let table = json_test_table();
        let json = serde_json::json!(true);
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Bool"));
                assert_eq!(fields.len(), 1);
                // Inner should be True constructor
                match &fields[0] {
                    Value::Con(inner_id, _) => {
                        assert_eq!(table.name_of(*inner_id), Some("True"));
                    }
                    _ => panic!("Expected Con(True)"),
                }
            }
            _ => panic!("Expected Con(Bool)"),
        }
    }

    #[test]
    fn test_string_to_value() {
        let table = json_test_table();
        let json = serde_json::json!("hello");
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("String"));
                assert_eq!(fields.len(), 1);
                // Inner should be Text constructor
                match &fields[0] {
                    Value::Con(inner_id, _) => {
                        assert_eq!(table.name_of(*inner_id), Some("Text"));
                    }
                    _ => panic!("Expected Con(Text), got {:?}", fields[0]),
                }
            }
            _ => panic!("Expected Con(String)"),
        }
    }

    #[test]
    fn test_number_to_value() {
        let table = json_test_table();
        let json = serde_json::json!(42);
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Number"));
                assert_eq!(fields.len(), 1);
                // Inner should be LitDouble
                match &fields[0] {
                    Value::Lit(Literal::LitDouble(bits)) => {
                        assert_eq!(f64::from_bits(*bits), 42.0);
                    }
                    _ => panic!("Expected Lit(LitDouble), got {:?}", fields[0]),
                }
            }
            _ => panic!("Expected Con(Number)"),
        }
    }

    #[test]
    fn test_number_double_to_value() {
        let table = json_test_table();
        let json = serde_json::json!(3.14);
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Number"));
                assert_eq!(fields.len(), 1);
                match &fields[0] {
                    Value::Lit(Literal::LitDouble(bits)) => {
                        let f = f64::from_bits(*bits);
                        assert!((f - 3.14).abs() < 1e-10);
                    }
                    _ => panic!("Expected Lit(LitDouble), got {:?}", fields[0]),
                }
            }
            _ => panic!("Expected Con(Number)"),
        }
    }

    #[test]
    fn test_array_to_value() {
        let table = json_test_table();
        let json = serde_json::json!([1, 2, 3]);
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Array"));
                assert_eq!(fields.len(), 1);
                // Inner should be a cons list
            }
            _ => panic!("Expected Con(Array)"),
        }
    }

    #[test]
    fn test_object_to_value() {
        let table = json_test_table();
        let json = serde_json::json!({"name": "Alice", "age": 30});
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Object"));
                assert_eq!(fields.len(), 1);
                // Inner should be a Map (Bin or Tip)
            }
            _ => panic!("Expected Con(Object)"),
        }
    }

    #[test]
    fn test_nested_json() {
        let table = json_test_table();
        let json = serde_json::json!({
            "users": [
                {"name": "Alice", "active": true},
                {"name": "Bob", "active": false}
            ],
            "count": 2
        });
        // Should not panic
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, _) => {
                assert_eq!(table.name_of(*id), Some("Object"));
            }
            _ => panic!("Expected Con(Object)"),
        }
    }
}
