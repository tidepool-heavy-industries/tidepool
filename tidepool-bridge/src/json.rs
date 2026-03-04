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
    // Prefer qualified name lookup; fall back to arity-based for legacy CBOR.
    let bin_id = table
        .get_by_qualified_name("Data.Map.Bin")
        .or_else(|| table.get_by_name_arity("Bin", 5))
        .ok_or_else(|| BridgeError::UnknownDataConName("Bin".into()))?;
    let tip_id = table
        .get_by_qualified_name("Data.Map.Tip")
        .or_else(|| table.get_companion(bin_id, "Tip", 0))
        .or_else(|| table.get_by_name_arity("Tip", 0))
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
                qualified_name: None,
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
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, _) => {
                assert_eq!(table.name_of(*id), Some("Object"));
            }
            _ => panic!("Expected Con(Object)"),
        }
    }

    // --- Exhaustive round-trip tests ---

    #[test]
    fn test_bool_false_to_value() {
        let table = json_test_table();
        let val = serde_json::json!(false).to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Bool"));
                match &fields[0] {
                    Value::Con(inner_id, _) => assert_eq!(table.name_of(*inner_id), Some("False")),
                    _ => panic!("Expected Con(False)"),
                }
            }
            _ => panic!("Expected Con(Bool)"),
        }
    }

    #[test]
    fn test_number_negative() {
        let table = json_test_table();
        let val = serde_json::json!(-1).to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Number"));
                match &fields[0] {
                    Value::Lit(Literal::LitDouble(bits)) => {
                        assert_eq!(f64::from_bits(*bits), -1.0);
                    }
                    _ => panic!("Expected LitDouble"),
                }
            }
            _ => panic!("Expected Con(Number)"),
        }
    }

    #[test]
    fn test_number_zero() {
        let table = json_test_table();
        let val = serde_json::json!(0).to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Number"));
                match &fields[0] {
                    Value::Lit(Literal::LitDouble(bits)) => {
                        assert_eq!(f64::from_bits(*bits), 0.0);
                    }
                    _ => panic!("Expected LitDouble"),
                }
            }
            _ => panic!("Expected Con(Number)"),
        }
    }

    #[test]
    fn test_string_empty() {
        let table = json_test_table();
        let val = serde_json::json!("").to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("String"));
                assert_eq!(fields.len(), 1);
                match &fields[0] {
                    Value::Con(inner_id, _) => {
                        assert_eq!(table.name_of(*inner_id), Some("Text"));
                    }
                    _ => panic!("Expected Con(Text)"),
                }
            }
            _ => panic!("Expected Con(String)"),
        }
    }

    #[test]
    fn test_string_unicode() {
        let table = json_test_table();
        let val = serde_json::json!("héllo 🌊").to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("String"));
                assert_eq!(fields.len(), 1);
            }
            _ => panic!("Expected Con(String)"),
        }
    }

    #[test]
    fn test_array_empty() {
        let table = json_test_table();
        let val = serde_json::json!([]).to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Array"));
                assert_eq!(fields.len(), 1);
                // Inner should be empty list ([] constructor)
                match &fields[0] {
                    Value::Con(nil_id, nil_fields) => {
                        assert_eq!(table.name_of(*nil_id), Some("[]"));
                        assert!(nil_fields.is_empty());
                    }
                    _ => panic!("Expected Con([])"),
                }
            }
            _ => panic!("Expected Con(Array)"),
        }
    }

    #[test]
    fn test_array_single() {
        let table = json_test_table();
        let val = serde_json::json!([1]).to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Array"));
                // Inner should be a cons cell (:)
                match &fields[0] {
                    Value::Con(cons_id, cons_fields) => {
                        assert_eq!(table.name_of(*cons_id), Some(":"));
                        assert_eq!(cons_fields.len(), 2);
                        // Head should be Number(1)
                        match &cons_fields[0] {
                            Value::Con(num_id, _) => {
                                assert_eq!(table.name_of(*num_id), Some("Number"));
                            }
                            _ => panic!("Expected Con(Number)"),
                        }
                        // Tail should be []
                        match &cons_fields[1] {
                            Value::Con(nil_id, _) => {
                                assert_eq!(table.name_of(*nil_id), Some("[]"));
                            }
                            _ => panic!("Expected Con([])"),
                        }
                    }
                    _ => panic!("Expected Con(:)"),
                }
            }
            _ => panic!("Expected Con(Array)"),
        }
    }

    #[test]
    fn test_array_nested() {
        let table = json_test_table();
        let val = serde_json::json!([[1, 2], [3]]).to_value(&table).unwrap();
        match &val {
            Value::Con(id, _) => assert_eq!(table.name_of(*id), Some("Array")),
            _ => panic!("Expected Con(Array)"),
        }
    }

    #[test]
    fn test_object_empty() {
        let table = json_test_table();
        let val = serde_json::json!({}).to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Object"));
                assert_eq!(fields.len(), 1);
                // Inner should be Tip (empty map)
                match &fields[0] {
                    Value::Con(tip_id, tip_fields) => {
                        assert_eq!(table.name_of(*tip_id), Some("Tip"));
                        assert!(tip_fields.is_empty());
                    }
                    _ => panic!("Expected Con(Tip)"),
                }
            }
            _ => panic!("Expected Con(Object)"),
        }
    }

    #[test]
    fn test_object_single_key() {
        let table = json_test_table();
        let val = serde_json::json!({"a": 1}).to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Object"));
                // Inner should be Bin(size=1, key, value, Tip, Tip)
                match &fields[0] {
                    Value::Con(bin_id, bin_fields) => {
                        assert_eq!(table.name_of(*bin_id), Some("Bin"));
                        assert_eq!(bin_fields.len(), 5);
                        // size field: I#(1)
                        match &bin_fields[0] {
                            Value::Con(i_id, i_fields) => {
                                assert_eq!(table.name_of(*i_id), Some("I#"));
                                match &i_fields[0] {
                                    Value::Lit(Literal::LitInt(n)) => assert_eq!(*n, 1),
                                    _ => panic!("Expected LitInt(1)"),
                                }
                            }
                            _ => panic!("Expected Con(I#)"),
                        }
                        // left and right should be Tip
                        match &bin_fields[3] {
                            Value::Con(tip_id, _) => {
                                assert_eq!(table.name_of(*tip_id), Some("Tip"));
                            }
                            _ => panic!("Expected Con(Tip)"),
                        }
                        match &bin_fields[4] {
                            Value::Con(tip_id, _) => {
                                assert_eq!(table.name_of(*tip_id), Some("Tip"));
                            }
                            _ => panic!("Expected Con(Tip)"),
                        }
                    }
                    _ => panic!("Expected Con(Bin)"),
                }
            }
            _ => panic!("Expected Con(Object)"),
        }
    }

    #[test]
    fn test_object_two_keys() {
        let table = json_test_table();
        let val = serde_json::json!({"a": 1, "b": 2}).to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Object"));
                // Root should be Bin
                match &fields[0] {
                    Value::Con(bin_id, bin_fields) => {
                        assert_eq!(table.name_of(*bin_id), Some("Bin"));
                        assert_eq!(bin_fields.len(), 5);
                        // size = 2
                        match &bin_fields[0] {
                            Value::Con(_, i_fields) => match &i_fields[0] {
                                Value::Lit(Literal::LitInt(n)) => assert_eq!(*n, 2),
                                _ => panic!("Expected LitInt(2)"),
                            },
                            _ => panic!("Expected I#"),
                        }
                    }
                    _ => panic!("Expected Con(Bin)"),
                }
            }
            _ => panic!("Expected Con(Object)"),
        }
    }

    #[test]
    fn test_object_three_keys_balanced() {
        let table = json_test_table();
        let val = serde_json::json!({"a": 1, "b": 2, "c": 3}).to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Object"));
                // Root Bin should have non-Tip children (balanced tree)
                match &fields[0] {
                    Value::Con(bin_id, bin_fields) => {
                        assert_eq!(table.name_of(*bin_id), Some("Bin"));
                        // At least one child should be a Bin (not both Tip)
                        let left_is_bin =
                            matches!(&bin_fields[3], Value::Con(id, _) if table.name_of(*id) == Some("Bin"));
                        let right_is_bin =
                            matches!(&bin_fields[4], Value::Con(id, _) if table.name_of(*id) == Some("Bin"));
                        assert!(
                            left_is_bin || right_is_bin,
                            "Expected at least one Bin child for 3-entry map"
                        );
                    }
                    _ => panic!("Expected Con(Bin)"),
                }
            }
            _ => panic!("Expected Con(Object)"),
        }
    }

    #[test]
    fn test_object_large() {
        let table = json_test_table();
        let mut map = serde_json::Map::new();
        for i in 0..12 {
            map.insert(format!("key_{}", i), serde_json::json!(i));
        }
        let json = serde_json::Value::Object(map);
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, _) => assert_eq!(table.name_of(*id), Some("Object")),
            _ => panic!("Expected Con(Object)"),
        }
    }

    #[test]
    fn test_deep_nesting() {
        let table = json_test_table();
        let json = serde_json::json!({"a": {"b": {"c": 1}}});
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id), Some("Object"));
                // Drill into the map → key "a" → value is Object → key "b" → ...
                // Just verify it doesn't panic and is Object
                assert_eq!(fields.len(), 1);
            }
            _ => panic!("Expected Con(Object)"),
        }
    }

    #[test]
    fn test_mixed_types() {
        let table = json_test_table();
        let json = serde_json::json!({
            "s": "str",
            "n": 42,
            "b": true,
            "a": [1, 2],
            "o": {"x": 1},
            "null": null
        });
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, _) => assert_eq!(table.name_of(*id), Some("Object")),
            _ => panic!("Expected Con(Object)"),
        }
    }

    #[test]
    fn test_ambiguous_tip_with_companion() {
        // Simulate Data.Map and Data.Set both having Tip/0
        let mut table = json_test_table();
        // Add a second "Tip" with a far-away ID (simulating Data.Set.Tip)
        table.insert(DataCon {
            id: DataConId(500),
            name: "Tip".into(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: None,
        });
        // keymap_to_value should still work because it uses get_companion
        // to find the Tip closest to Bin
        let json = serde_json::json!({"key": "value"});
        let val = json.to_value(&table).unwrap();
        match &val {
            Value::Con(id, _) => assert_eq!(table.name_of(*id), Some("Object")),
            _ => panic!("Expected Con(Object)"),
        }
    }

    #[test]
    fn test_qualified_name_resolves_ambiguous_map_constructors() {
        // Build a table where Bin/Tip from Data.Map AND Data.Set are present
        // with qualified names — the qualified path should pick the right ones.
        let mut t = DataConTable::new();
        let cons: &[(&str, u32, u32)] = &[
            ("Object", 0, 1),
            ("Array", 1, 1),
            ("String", 2, 1),
            ("Number", 3, 1),
            ("Bool", 4, 1),
            ("Null", 5, 0),
            ("True", 8, 0),
            ("False", 9, 0),
            ("[]", 10, 0),
            (":", 11, 2),
            ("Text", 12, 3),
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
            });
        }
        // Data.Map constructors with qualified names
        t.insert(DataCon {
            id: DataConId(100),
            name: "Bin".into(),
            tag: 1,
            rep_arity: 5,
            field_bangs: vec![],
            qualified_name: Some("Data.Map.Bin".into()),
        });
        t.insert(DataCon {
            id: DataConId(101),
            name: "Tip".into(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("Data.Map.Tip".into()),
        });
        // Data.Set constructors with SAME unqualified names
        t.insert(DataCon {
            id: DataConId(200),
            name: "Bin".into(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
            qualified_name: Some("Data.Set.Bin".into()),
        });
        t.insert(DataCon {
            id: DataConId(201),
            name: "Tip".into(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("Data.Set.Tip".into()),
        });

        // keymap_to_value should resolve via qualified names
        let json = serde_json::json!({"a": 1, "b": 2});
        let val = json.to_value(&t).unwrap();
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(t.name_of(*id), Some("Object"));
                // Inner map should use Data.Map.Bin (id=100), not Data.Set.Bin
                match &fields[0] {
                    Value::Con(bin_id, _) => {
                        assert_eq!(*bin_id, DataConId(100));
                    }
                    _ => panic!("Expected Con(Bin)"),
                }
            }
            _ => panic!("Expected Con(Object)"),
        }
    }
}
