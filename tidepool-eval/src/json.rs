//! serde_json → eval `Value` (the vendored `Tidepool.Aeson.Value` ADT).
//!
//! This is the ONE source of truth for how a parsed JSON document is built as a
//! Tidepool Core `Value`, shared by:
//!   - the pure `JsonDecode` primop, on BOTH the tree-walker (`eval.rs`) and the
//!     Cranelift JIT (via the `runtime_json_decode` host fn in
//!     `tidepool-codegen`), so the two agree by construction, and
//!   - `tidepool-bridge`'s `impl ToCore for serde_json::Value` (effect results
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
//! The Value-level shape primitives (Text/list/Map/number construction) live
//! in [`crate::shapes`] — this module owns only the JSON-document policy
//! (key sorting, Either wrapping, the `JsonConIds` cache) on top of them.

use crate::value::Value;
use std::cell::Cell;
use tidepool_repr::{DataConId, DataConTable};

/// `DataConId`s of every constructor needed to build a `Value` (and optionally an
/// `Either Text Value`). `Copy` so it can be cached in a thread-local by value,
/// with no borrow of the originating `DataConTable`.
///
/// `left`/`right` are `Option<DataConId>` because `json_to_value` does not
/// need them — only `decode_json_str` (the `JsonDecode` primop) does. This lets
/// `tidepool-bridge`'s `ToCore for serde_json::Value` use `from_table` even when
/// the program's `DataConTable` has no `Either` in scope.
#[derive(Debug, Clone, Copy)]
pub struct JsonConIds {
    /// `Left` constructor (arity 1) — `None` when `Either` is not in scope.
    pub left: Option<DataConId>,
    /// `Right` constructor (arity 1) — `None` when `Either` is not in scope.
    pub right: Option<DataConId>,
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
    /// Resolve constructor ids from a table. Returns `None` if any core `Value` /
    /// `Data.Map` / `Text` constructor is absent. `left`/`right` are optional:
    /// they are set to `Some` only when `Either` is in scope. Callers that need
    /// `decode_json_str` (the `JsonDecode` primop) must check that both are
    /// `Some`; callers that only need `json_to_value` (e.g. `tidepool-bridge`)
    /// can ignore them.
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
            left: table.get_by_name_arity("Left", 1),
            right: table.get_by_name_arity("Right", 1),
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
fn text_value(s: &str, ids: &JsonConIds) -> Value {
    crate::shapes::make_text(s, ids.text)
}

/// Build a `[Value]` cons list (`:`/`[]`) from already-converted elements.
fn list_value(items: Vec<Value>, ids: &JsonConIds) -> Value {
    crate::shapes::make_list(items, ids.cons, ids.nil)
}

/// Build a `Data.Map.Strict.Map Key Value` from key-sorted entries by
/// divide-and-conquer (`Bin size k v left right` / `Tip`, size boxed as `I#`).
fn map_value(entries: &[(&String, &serde_json::Value)], ids: &JsonConIds) -> Value {
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

/// Convert a parsed `serde_json::Value` to the eval `Value` for the vendored
/// aeson `Value` type. Recursion depth is bounded by serde_json's own nesting
/// limit (128 by default), so this never approaches host-stack exhaustion.
pub fn json_to_value(j: &serde_json::Value, ids: &JsonConIds) -> Value {
    match j {
        serde_json::Value::Null => Value::Con(ids.null, vec![]),
        serde_json::Value::Bool(b) => {
            let inner = Value::Con(if *b { ids.true_con } else { ids.false_con }, vec![]);
            Value::Con(ids.bool_con, vec![inner])
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
        serde_json::Value::String(s) => Value::Con(ids.string, vec![text_value(s, ids)]),
        serde_json::Value::Array(arr) => {
            let items = arr.iter().map(|v| json_to_value(v, ids)).collect();
            Value::Con(ids.array, vec![list_value(items, ids)])
        }
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            Value::Con(ids.object, vec![map_value(&entries, ids)])
        }
    }
}

/// The exact node count of the `Value` [`json_to_value`] would build for `j` —
/// equal to `json_to_value(j, ids).node_count()` for ANY `ids`, since
/// [`Value::node_count`] walks tree SHAPE and ignores constructor ids. Lets a
/// caller reject a response that would overflow the effect-response
/// materialization cap with a TYPED error, before it reaches the generic
/// mid-effect abort in `tidepool-codegen`.
///
/// Counting the serde tree directly under-counts badly: bridging a JSON object
/// to a `Data.Map` spine adds a `Bin` node, a boxed `I#` size, and a boxed
/// `Text` key PER ENTRY (an object bridges several-fold larger than its serde
/// node count), which is why an approximate serde-side guard let object-heavy
/// responses slip past and abort. This builds the real `Value` and counts it —
/// the same work the machine does on the abort path (`resp_val.node_count()`),
/// so the numbers agree by construction; cheap relative to the network fetch.
#[must_use]
pub fn bridged_node_count(j: &serde_json::Value) -> usize {
    // node_count is shape-only, so every id can be the same placeholder.
    let z = DataConId(0);
    let ids = JsonConIds {
        left: None,
        right: None,
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

/// Parse a JSON document and wrap the result: `Right v` on success, `Left <err>`
/// (the serde_json error message as a `Text`) on any parse error. This is the
/// semantics of the internal `eitherDecodeValue :: Text -> Either Text Value`
/// primop that the public `eitherDecode` is derived from.
///
/// Returns `None` (rather than panicking) when `ids.left` or `ids.right` are
/// absent, so callers can surface a clean error. In practice this only happens
/// when the `DataConTable` lacks `Either` in scope — programs that reach the
/// JSON-decode primop always have it in scope.
pub fn decode_json_str(input: &str, ids: &JsonConIds) -> Option<Value> {
    let left = ids.left?;
    let right = ids.right?;
    match serde_json::from_str::<serde_json::Value>(input) {
        Ok(j) => Some(Value::Con(right, vec![json_to_value(&j, ids)])),
        Err(e) => Some(Value::Con(left, vec![text_value(&e.to_string(), ids)])),
    }
}

thread_local! {
    /// The aeson-`Value` constructor ids for the current eval, cached by
    /// `env_from_datacon_table` (the universal eval-setup chokepoint). Read by
    /// the `JsonDecode` primop arm in `eval.rs`. `None` when the closure isn't
    /// in scope, or before any env has been built on this thread.
    static JSON_CON_IDS: Cell<Option<JsonConIds>> = const { Cell::new(None) };
}

/// Cache the JSON constructor ids for this thread (called from
/// `env_from_datacon_table`). `None` clears them.
pub fn set_json_con_ids(ids: Option<JsonConIds>) {
    JSON_CON_IDS.with(|c| c.set(ids));
}

/// The JSON constructor ids cached for this thread, if any.
pub fn json_con_ids() -> Option<JsonConIds> {
    JSON_CON_IDS.with(|c| c.get())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            left: None,
            right: None,
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
