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
//!   Value = Object !Object | Array [Value] | String !Text | Number !Double
//!         | Bool !Bool | Null | NumberI !Int
//! with `Object` backed by `Data.Map.Strict` (`Bin`/`Tip` balanced tree, the
//! `!Int` size boxed as `I#`), lists as `:`/`[]` cons cells, and `Text` stored
//! as the GHC worker `Text ByteArray# Int# Int#` (UTF-8 bytes, offset 0).
//!
//! Machine ints ride `NumberI` (exact); genuine floats ride `Number` (Double) —
//! same BUG-8 split the bridge already used.
//!
//! The Value-level shape primitives (Text/list/Map/number construction) live
//! in [`crate::shapes`] — this module owns only the JSON-document policy
//! (key sorting, Maybe wrapping, the `JsonConIds` cache) on top of them.

use crate::value::Value;
use std::cell::Cell;
use tidepool_repr::{DataConId, DataConTable};

/// `DataConId`s of every constructor needed to build a `Value` (and optionally a
/// `Maybe Value`). `Copy` so it can be cached in a thread-local by value, with no
/// borrow of the originating `DataConTable`.
///
/// `just`/`nothing` are `Option<DataConId>` because `json_to_value` does not
/// need them — only `decode_json_str` (the `JsonDecode` primop) does. This lets
/// `tidepool-bridge`'s `ToCore for serde_json::Value` use `from_table` even when
/// the program's `DataConTable` has no `Maybe` in scope.
#[derive(Debug, Clone, Copy)]
pub struct JsonConIds {
    /// `Just` constructor (arity 1) — `None` when `Maybe` is not in scope.
    pub just: Option<DataConId>,
    /// `Nothing` constructor (arity 0) — `None` when `Maybe` is not in scope.
    pub nothing: Option<DataConId>,
    pub object: DataConId,
    pub array: DataConId,
    pub string: DataConId,
    pub number: DataConId,
    pub number_i: DataConId,
    pub bool_con: DataConId,
    pub null: DataConId,
    pub true_con: DataConId,
    pub false_con: DataConId,
    pub bin: DataConId,
    pub tip: DataConId,
    pub i_hash: DataConId,
    pub text: DataConId,
    pub cons: DataConId,
    pub nil: DataConId,
}

impl JsonConIds {
    /// Resolve constructor ids from a table. Returns `None` if any core `Value` /
    /// `Data.Map` / `Text` constructor is absent. `just`/`nothing` are optional:
    /// they are set to `Some` only when `Maybe` is in scope. Callers that need
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
            just: table.get_by_name_arity("Just", 1),
            nothing: table.get_by_name_arity("Nothing", 0),
            object: table.get_by_name_arity("Object", 1)?,
            array: table.get_by_name_arity("Array", 1)?,
            string: table.get_by_name_arity("String", 1)?,
            number: table.get_by_name_arity("Number", 1)?,
            number_i: table.get_by_name_arity("NumberI", 1)?,
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
        serde_json::Value::Number(n) => crate::shapes::json_number(n, ids.number_i, ids.number),
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

/// Parse a JSON document and wrap the result: `Just v` on success, `Nothing` on
/// any parse error. This is the semantics of `decodeJson :: Text -> Maybe Value`.
///
/// Returns `None` (rather than panicking) when `ids.just` or `ids.nothing` are
/// absent, so callers can surface a clean error. In practice this only happens
/// when the `DataConTable` lacks a `Maybe` closure — programs that call
/// `decodeJson` always have it in scope.
pub fn decode_json_str(input: &str, ids: &JsonConIds) -> Option<Value> {
    let just = ids.just?;
    let nothing = ids.nothing?;
    match serde_json::from_str::<serde_json::Value>(input) {
        Ok(j) => Some(Value::Con(just, vec![json_to_value(&j, ids)])),
        Err(_) => Some(Value::Con(nothing, vec![])),
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
