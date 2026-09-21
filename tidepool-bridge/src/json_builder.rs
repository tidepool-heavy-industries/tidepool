//! serde_json → the runtime `HaskellValue` (the vendored `Tidepool.Aeson.Value` ADT).
//!
//! This is the ONE source of truth for how a parsed JSON document is built as a
//! Tidepool `HaskellValue`, shared by:
//!   - `tidepool-bridge`'s `impl ToHaskell for serde_json::Value` (effect results
//!     that hand JSON back to Haskell), which delegates here.
//!
//! Representation (must match `haskell/lib/Tidepool/Aeson/Value.hs` at -O2):
//!   Value = Object !Object | Array [Value] | String !Text | Number !Scientific
//!         | Bool !Bool | Null
//! with `Object` backed by `Data.Map.Strict` (`Bin`/`Tip` balanced tree, the
//! `!Int` size boxed as `I#`), lists as `:`/`[]` cons cells, and `Text` stored
//! as the GHC worker `Text ByteArray# Int# Int#` (UTF-8 bytes, offset 0).
//!
//! Every JSON number rides `Number !Scientific` — `Scientific coeff exp` denotes
//! `coeff * 10^exp` and stays exact, the coefficient an exact `Integer`
//! (`IS`/`IP`/`IN`) and the exponent an `Int`. No Double rounding, so a >i64 or
//! high-precision literal survives round-trip intact (BUG-8).
//!
//! The HaskellValue-level shape primitives (Text/list/Map/number construction) live
//! in [`crate::shapes`] — this module owns only the JSON-document policy
//! (key sorting and resolving [`JsonConIds`]) on top of them.

use crate::value::HaskellValue;
use tidepool_repr::{DataConId, DataConTable};

/// `DataConId`s of every constructor needed to build a `HaskellValue`.
/// (see [`crate::env::EvalIds`]), with no borrow of the originating `DataConTable`.
///
#[derive(Debug, Clone, Copy)]
pub struct JsonConIds {
    /// `Object` constructor (arity 1, wraps the backing `Data.Map`).
    pub object: DataConId,
    /// `Array` constructor (arity 1, wraps the backing cons list).
    pub array: DataConId,
    /// `String` constructor (arity 1, wraps a `Text`).
    pub string: DataConId,
    /// `Number` constructor (arity 1, wraps a `Scientific`).
    pub number: DataConId,
    /// `Scientific` (arity 2: coefficient `Integer`, base10Exponent `Int`).
    pub scientific: DataConId,
    /// `Integer` constructors for the `Scientific` coefficient.
    pub is: DataConId,
    /// `IP` — the positive-multi-limb `Integer` constructor.
    pub ip: DataConId,
    /// `IN` — the negative-multi-limb `Integer` constructor.
    pub in_: DataConId,
    /// `Bool` constructor (arity 1, wraps the `True`/`False` payload).
    pub bool_con: DataConId,
    /// `Null` constructor (arity 0).
    pub null: DataConId,
    /// `True` constructor (arity 0), the `Bool` payload.
    pub true_con: DataConId,
    /// `False` constructor (arity 0), the `Bool` payload.
    pub false_con: DataConId,
    /// `Data.Map.Strict.Bin` — internal balanced-tree node backing `Object`.
    pub bin: DataConId,
    /// `Data.Map.Strict.Tip` — the empty-map leaf backing `Object`.
    pub tip: DataConId,
    /// `I#` — boxed-`Int#` constructor, used for map sizes and integers.
    pub i_hash: DataConId,
    /// `Text` constructor (arity 3, the GHC `Text ByteArray# Int# Int#` worker).
    pub text: DataConId,
    /// `:` — cons constructor backing `Array`'s element list.
    pub cons: DataConId,
    /// `[]` — nil constructor backing `Array`'s element list.
    pub nil: DataConId,
}

impl JsonConIds {
    /// Resolve constructor ids from a table. Returns `None` if any Haskell value,
    /// `Data.Map` / `Text` constructor is absent.
    ///
    /// Tip resolution uses `get_companion` first so that when both
    /// `Data.Map.Tip` and `Data.Set.Tip` are present (cross-module closure) the
    /// `Tip` that is actually a sibling of the resolved `Bin` is chosen.
    pub fn from_table(table: &DataConTable) -> Option<Self> {
        let bin = table
            .get_by_qualified_name("Data.Map.Bin")
            .or_else(|| table.get_by_name_arity("Bin", 5))?;
        let tip = table
            .get_by_qualified_name("Data.Map.Tip")
            .or_else(|| table.get_companion(bin, "Tip", 0))
            .or_else(|| table.get_by_name_arity("Tip", 0))?;
        Some(JsonConIds {
            object: table.get_by_name_arity("Object", 1)?,
            array: table.get_by_name_arity("Array", 1)?,
            string: table.get_by_name_arity("String", 1)?,
            number: table.get_by_name_arity("Number", 1)?,
            scientific: table.get_by_name_arity("Scientific", 2)?,
            is: table.get_by_name_arity("IS", 1)?,
            ip: table.get_by_name_arity("IP", 1)?,
            in_: table.get_by_name_arity("IN", 1)?,
            bool_con: table.get_by_name_arity("Bool", 1)?,
            null: table.get_by_name_arity("Null", 0)?,
            true_con: table.get_by_name_arity("True", 0)?,
            false_con: table.get_by_name_arity("False", 0)?,
            bin,
            tip,
            i_hash: table.get_by_name_arity("I#", 1)?,
            text: table.get_by_name_arity("Text", 3)?,
            cons: table.get_by_name_arity(":", 2)?,
            nil: table.get_by_name_arity("[]", 0)?,
        })
    }
}

/// Build the worker `Text ByteArray# Int# Int#` for a UTF-8 string (offset 0).
fn text_value(s: &str, ids: &JsonConIds) -> HaskellValue {
    crate::shapes::make_text(s, ids.text)
}

/// Build a `[HaskellValue]` cons list (`:`/`[]`) from already-converted elements.
fn list_value(items: Vec<HaskellValue>, ids: &JsonConIds) -> HaskellValue {
    crate::shapes::make_list(items, ids.cons, ids.nil)
}

/// Build a `Data.Map.Strict.Map Key HaskellValue` from key-sorted entries by
/// divide-and-conquer (`Bin size k v left right` / `Tip`, size boxed as `I#`).
fn map_value(entries: &[(&String, &serde_json::Value)], ids: &JsonConIds) -> HaskellValue {
    if entries.is_empty() {
        return crate::shapes::map_tip(ids.tip);
    }
    let mid = entries.len() / 2;
    let (k, v) = entries[mid];
    let left = map_value(&entries[..mid], ids);
    let right = map_value(&entries[mid + 1..], ids);
    crate::shapes::map_bin_node(
        entries.len() as i64,
        text_value(k, ids),
        json_to_value(v, ids),
        left,
        right,
        ids.bin,
        ids.i_hash,
    )
}

/// Convert a parsed `serde_json::Value` to the eval `HaskellValue` for the vendored
/// aeson `Value` type. Recursion depth is bounded by serde_json's own nesting
/// limit (128 by default), so this never approaches host-stack exhaustion.
pub fn json_to_value(j: &serde_json::Value, ids: &JsonConIds) -> HaskellValue {
    match j {
        serde_json::Value::Null => HaskellValue::Con(ids.null, vec![]),
        serde_json::Value::Bool(b) => {
            let inner = HaskellValue::Con(if *b { ids.true_con } else { ids.false_con }, vec![]);
            HaskellValue::Con(ids.bool_con, vec![inner])
        }
        serde_json::Value::Number(n) => crate::shapes::scientific_from_number(
            n,
            &crate::shapes::NumberConIds {
                number: ids.number,
                scientific: ids.scientific,
                is: ids.is,
                ip: ids.ip,
                in_: ids.in_,
            },
        ),
        serde_json::Value::String(s) => HaskellValue::Con(ids.string, vec![text_value(s, ids)]),
        serde_json::Value::Array(arr) => {
            let items = arr.iter().map(|v| json_to_value(v, ids)).collect();
            HaskellValue::Con(ids.array, vec![list_value(items, ids)])
        }
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            HaskellValue::Con(ids.object, vec![map_value(&entries, ids)])
        }
    }
}

/// The exact node count of the `HaskellValue` [`json_to_value`] would build for `j` —
/// equal to `json_to_value(j, ids).node_count()` for ANY `ids`, since
/// [`HaskellValue::node_count`] walks tree SHAPE and ignores constructor ids. Lets a
/// caller reject a response that would overflow the effect-response
/// materialization cap with a TYPED error, before it reaches the generic
/// mid-effect abort in `tidepool-codegen`.
///
/// Counting the serde tree directly under-counts badly: bridging a JSON object
/// to a `Data.Map` spine adds a `Bin` node, a boxed `I#` size, and a boxed
/// `Text` key PER ENTRY (an object bridges several-fold larger than its serde
/// node count), which is why an approximate serde-side guard let object-heavy
/// responses slip past and abort. This builds the real `HaskellValue` and counts it —
/// the same work the machine does on the abort path (`resp_val.node_count()`),
/// so the numbers agree by construction; cheap relative to the network fetch.
#[must_use]
pub fn bridged_node_count(j: &serde_json::Value) -> usize {
    // node_count is shape-only, so every id can be the same placeholder.
    let z = DataConId(0);
    let ids = JsonConIds {
        object: z,
        array: z,
        string: z,
        number: z,
        scientific: z,
        is: z,
        ip: z,
        in_: z,
        bool_con: z,
        null: z,
        true_con: z,
        false_con: z,
        bin: z,
        tip: z,
        i_hash: z,
        text: z,
        cons: z,
        nil: z,
    };
    json_to_value(j, &ids).node_count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataCon;

    /// Build a DataConTable with all constructors needed for JSON values.
    fn json_test_table() -> DataConTable {
        let mut t = DataConTable::new();
        let cons = [
            // HaskellValue constructors
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

    /// `Data.Map` and `Data.Set` both register a bare-name `Tip`; `from_table`
    /// must still resolve via `get_companion` — the `Tip` that is actually a
    /// sibling of the resolved `Bin` — not just the first bare-name match.
    #[test]
    fn json_con_ids_resolves_tip_companion_over_bare_name_collision() {
        let mut table = json_test_table();
        // Add a second "Tip" with a far-away id (simulating Data.Set.Tip).
        table.insert(DataCon {
            id: DataConId(500),
            name: "Tip".into(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: None,
            type_name: String::new(),
        });
        let ids = JsonConIds::from_table(&table).expect("table has all JSON constructors");
        let json = serde_json::json!({"key": "value"});
        let val = json_to_value(&json, &ids);
        match &val {
            HaskellValue::Con(id, _) => assert_eq!(*id, ids.object),
            other => panic!("expected Con(Object), got {other:?}"),
        }
    }

    /// When `Bin`/`Tip` from `Data.Map` AND `Data.Set` are both present with
    /// qualified names, `from_table` must resolve via the qualified path and
    /// pick the `Data.Map` pair, not whichever bare name comes first.
    #[test]
    fn json_con_ids_resolves_ambiguous_map_constructors_via_qualified_name() {
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
            // Number carrier: Scientific coefficient×10^exponent (exact, BUG-8)
            ("Scientific", 1, 2),
            ("IS", 1, 1),
            ("IP", 2, 1),
            ("IN", 3, 1),
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
                type_name: String::new(),
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
            type_name: String::new(),
        });
        t.insert(DataCon {
            id: DataConId(101),
            name: "Tip".into(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("Data.Map.Tip".into()),
            type_name: String::new(),
        });
        // Data.Set constructors with SAME unqualified names
        t.insert(DataCon {
            id: DataConId(200),
            name: "Bin".into(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
            qualified_name: Some("Data.Set.Bin".into()),
            type_name: String::new(),
        });
        t.insert(DataCon {
            id: DataConId(201),
            name: "Tip".into(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("Data.Set.Tip".into()),
            type_name: String::new(),
        });

        let ids = JsonConIds::from_table(&t).expect("table has all JSON constructors");
        let json = serde_json::json!({"a": 1, "b": 2});
        let val = json_to_value(&json, &ids);
        match &val {
            HaskellValue::Con(id, fields) => {
                assert_eq!(*id, ids.object);
                // Inner map should use Data.Map.Bin (id=100), not Data.Set.Bin
                match &fields[0] {
                    HaskellValue::Con(bin_id, _) => assert_eq!(*bin_id, DataConId(100)),
                    other => panic!("expected Con(Bin), got {other:?}"),
                }
            }
            other => panic!("expected Con(Object), got {other:?}"),
        }
    }

    /// Naive serde node count (what an approximate guard would use).
    fn serde_nodes(j: &serde_json::Value) -> usize {
        match j {
            serde_json::Value::Array(a) => 1 + a.iter().map(serde_nodes).sum::<usize>(),
            serde_json::Value::Object(m) => 1 + m.values().map(serde_nodes).sum::<usize>(),
            _ => 1,
        }
    }

    /// A JSON object bridges several-fold larger than its serde tree — each
    /// entry adds a `Bin` node, a boxed `I#` size, and a boxed `Text` key. This
    /// gap is exactly why a serde-side guard under-counts and lets object-heavy
    /// responses reach the abort; `bridged_node_count` measures the real size.
    #[test]
    fn object_bridges_several_fold_larger_than_serde_tree() {
        let obj = serde_json::json!({
            "a": 1, "b": 2, "c": 3, "d": 4, "e": 5, "f": 6, "g": 7, "h": 8,
        });
        let serde = serde_nodes(&obj);
        let bridged = bridged_node_count(&obj);
        assert!(
            bridged >= 4 * serde,
            "object should bridge much larger: serde={serde}, bridged={bridged}"
        );
    }

    /// `bridged_node_count` is independent of which ids are used (shape-only),
    /// so the placeholder-ids count equals a real-ids build's `node_count`.
    #[test]
    fn bridged_count_is_id_independent() {
        let j = serde_json::json!({"xs": [1, 2, 3], "s": "hi", "nested": {"k": true}});
        let z = DataConId(7); // arbitrary non-zero ids
        let ids = JsonConIds {
            object: z,
            array: z,
            string: z,
            number: z,
            scientific: z,
            is: z,
            ip: z,
            in_: z,
            bool_con: z,
            null: z,
            true_con: z,
            false_con: z,
            bin: z,
            tip: z,
            i_hash: z,
            text: z,
            cons: z,
            nil: z,
        };
        assert_eq!(bridged_node_count(&j), json_to_value(&j, &ids).node_count());
    }
}
