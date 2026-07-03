//! Value-level Haskell data-shape facts — the ONE home for how common GHC
//! runtime shapes look as `tidepool_eval::Value` trees, independent of any
//! heap layout.
//!
//! A "shape fact" is the Value-tree encoding of a Haskell data type as GHC
//! -O2 Core sees it:
//!   - `Text` as the worker `Text ByteArray# Int# Int#` (UTF-8 bytes), with
//!     three accepted backing forms: raw `Value::ByteArray`, `LitString`, and
//!     any number of lifted `Con("ByteArray", [..])` wrapper layers (sliced
//!     Texts from `splitOn` etc. stack them);
//!   - boxed machine numbers as `I#`/`W#`/`C#`/`D#`/`F#` single-field cons
//!     (possibly nested);
//!   - `Bool` as the nullary `True`/`False` cons;
//!   - `Data.Map.Strict` as `Bin size k v l r`/`Tip` with the `!Int` size
//!     boxed as `I#`;
//!   - GHC bignums (`IP`/`IN` payloads) as ByteArrays of little-endian 64-bit
//!     limbs (`bigNatLitBytes`);
//!   - the vendored aeson `Value`'s exact-int policy: i64-representable JSON
//!     numbers ride `NumberI`, everything else rides the Double-backed
//!     `Number`.
//!
//! Encode and decode of the same shape live here, side by side —
//! `tidepool-runtime`'s renderer, `tidepool-bridge`'s FromCore/ToCore impls,
//! and `tidepool-eval`'s `JsonDecode` primop read/write these shapes through
//! this module so they agree by construction.
//!
//! What does NOT live here:
//!   - heap BYTE layouts — `tidepool-codegen/src/heap_bridge.rs` owns the
//!     HeapObject encoding (and `tidepool-testing/src/compare.rs` documents
//!     its own mirror of it);
//!   - presentation policy — UTF-8 error surfaces, JSON depth/length
//!     truncation, and error-type mapping stay with each caller. Decoders
//!     here return raw bytes / `Option` / typed errors and let the caller
//!     choose the surface.
//!
//! Constructors take pre-resolved `DataConId`s (each caller has its own
//! resolution policy: `JsonConIds`, bridge `get_resilient`, …). Decoders take
//! a `&DataConTable` and recognize constructors BY NAME (`name_of`), which
//! tolerates duplicate same-name cons from cross-module closures.

use crate::value::{SharedByteArray, Value};
use std::sync::{Arc, Mutex, PoisonError};
use tidepool_repr::{DataConId, DataConTable, Literal};

/// The sentinel `DataConId` under which heap readers surface `Array#` /
/// `SmallArray#` payloads as `Con(ARRAY_SENTINEL, elems)` — a bare element
/// vector with no real constructor. Produced by
/// `tidepool-codegen/src/heap_bridge.rs` (and its documented mirror in
/// `tidepool-testing/src/compare.rs`); consumed by e.g. the renderer's
/// `Vector` arm.
///
/// CONTRACT: `0` can collide with a legitimate `DataConId` in any table, so
/// consumers must rely on the surrounding typed context (e.g. "this is the
/// `Array#` field of a `Vector` con") — never on the id alone.
pub const ARRAY_SENTINEL: DataConId = DataConId(0);

/// True when `id` resolves to a constructor named `name`. Name-based (not
/// id-equality against a single lookup) so duplicate same-name cons from
/// cross-module closures are all recognized.
fn is_con_named(id: DataConId, name: &str, table: &DataConTable) -> bool {
    table.name_of(id) == Some(name)
}

// ---------------------------------------------------------------------------
// Boxed machine numbers: I# / W# / C# / D# / F#
// ---------------------------------------------------------------------------

macro_rules! unbox_lit {
    ($fn_name:ident, $con:literal, $ty:ty, $pat:pat => $out:expr) => {
        /// Unwrap a possibly-boxed literal: accepts the bare literal or any
        /// number of nested single-field boxing-con layers.
        pub fn $fn_name(v: &Value, table: &DataConTable) -> Option<$ty> {
            let mut cur = v;
            loop {
                match cur {
                    Value::Lit($pat) => return Some($out),
                    Value::Con(id, fields)
                        if fields.len() == 1 && is_con_named(*id, $con, table) =>
                    {
                        cur = &fields[0];
                    }
                    _ => return None,
                }
            }
        }
    };
}

unbox_lit!(unbox_int, "I#", i64, Literal::LitInt(n) => *n);
unbox_lit!(unbox_word, "W#", u64, Literal::LitWord(n) => *n);
unbox_lit!(unbox_double, "D#", f64, Literal::LitDouble(bits) => f64::from_bits(*bits));
unbox_lit!(unbox_float, "F#", f32, Literal::LitFloat(bits) => f32::from_bits(*bits as u32));

/// Box an `Int` as `I#(LitInt n)`.
pub fn box_int(n: i64, i_hash: DataConId) -> Value {
    Value::Con(i_hash, vec![Value::Lit(Literal::LitInt(n))])
}

/// Box a `Word` as `W#(LitWord n)`.
pub fn box_word(n: u64, w_hash: DataConId) -> Value {
    Value::Con(w_hash, vec![Value::Lit(Literal::LitWord(n))])
}

/// Box a `Double` as `D#(LitDouble bits)`.
pub fn box_double(f: f64, d_hash: DataConId) -> Value {
    Value::Con(d_hash, vec![Value::Lit(Literal::LitDouble(f.to_bits()))])
}

/// Box a `Float` as `F#(LitFloat bits)`.
pub fn box_float(f: f32, f_hash: DataConId) -> Value {
    Value::Con(f_hash, vec![Value::Lit(Literal::LitFloat(f.to_bits() as u64))])
}

/// Box a `Char` as `C#(LitChar c)`.
pub fn box_char(c: char, c_hash: DataConId) -> Value {
    Value::Con(c_hash, vec![Value::Lit(Literal::LitChar(c))])
}

/// Extract a `char` from any of its Value shapes:
///   1. bare `LitChar`;
///   2. `C#(LitChar)`;
///   3. `C#(Text(backing, off, 1))` — a char smuggled as a single-BYTE Text;
///      the byte at `off` is read as a raw 0–255 code point (historical
///      renderer semantics, NOT UTF-8 decoding).
pub fn unbox_char(v: &Value, table: &DataConTable) -> Option<char> {
    match v {
        Value::Lit(Literal::LitChar(c)) => Some(*c),
        Value::Con(id, fields) if fields.len() == 1 && is_con_named(*id, "C#", table) => {
            char_payload(&fields[0], table)
        }
        _ => None,
    }
}

fn char_payload(v: &Value, table: &DataConTable) -> Option<char> {
    match v {
        Value::Lit(Literal::LitChar(c)) => Some(*c),
        Value::Con(id, fields) if fields.len() == 3 && is_con_named(*id, "Text", table) => {
            let len = unbox_int(&fields[2], table)?;
            if len != 1 {
                return None;
            }
            let off = unbox_int(&fields[1], table).unwrap_or(0) as usize;
            let backing = text_backing(&fields[0], table)?;
            let bytes = backing.lock().unwrap_or_else(PoisonError::into_inner);
            bytes.get(off).map(|&b| b as char)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Bool
// ---------------------------------------------------------------------------

/// Decode a nullary `True`/`False` con. `None` for anything else (including
/// a True/False con that somehow carries fields).
pub fn unbox_bool(v: &Value, table: &DataConTable) -> Option<bool> {
    match v {
        Value::Con(id, fields) if fields.is_empty() => match table.name_of(*id) {
            Some("True") => Some(true),
            Some("False") => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Build the nullary `True`/`False` con.
pub fn make_bool(b: bool, true_id: DataConId, false_id: DataConId) -> Value {
    Value::Con(if b { true_id } else { false_id }, vec![])
}

// ---------------------------------------------------------------------------
// Text — worker `Text ByteArray# Int# Int#`
// ---------------------------------------------------------------------------

/// Build the worker `Text ByteArray# Int# Int#` for a UTF-8 string
/// (offset 0, raw `Value::ByteArray` backing).
pub fn make_text(s: &str, text_id: DataConId) -> Value {
    let bytes = s.as_bytes().to_vec();
    let len = bytes.len() as i64;
    Value::Con(
        text_id,
        vec![
            Value::ByteArray(Arc::new(Mutex::new(bytes))),
            Value::Lit(Literal::LitInt(0)),
            Value::Lit(Literal::LitInt(len)),
        ],
    )
}

/// Unwrap a Text backing field to its raw bytes, given a recognizer for the
/// lifted `Con("ByteArray", [..])` wrapper layer. Accepts a raw
/// `Value::ByteArray` or a `LitString` unconditionally (`LitString` is
/// copied into a fresh `SharedByteArray`); table-free callers that cannot
/// recognize the wrapper con (no `DataConId` for it in hand) pass `|_|
/// false` and simply won't unwrap that form.
fn text_backing_with(v: &Value, is_bytearray_con: &dyn Fn(DataConId) -> bool) -> Option<SharedByteArray> {
    let mut cur = v;
    loop {
        match cur {
            Value::ByteArray(bs) => return Some(bs.clone()),
            Value::Lit(Literal::LitString(bytes)) => {
                return Some(Arc::new(Mutex::new(bytes.clone())))
            }
            Value::Con(id, fields) if fields.len() == 1 && is_bytearray_con(*id) => {
                cur = &fields[0];
            }
            _ => return None,
        }
    }
}

/// Unwrap a Text backing field to its raw bytes. Accepts a raw
/// `Value::ByteArray`, a `LitString`, or any number of lifted
/// `Con("ByteArray", [..])` wrapper layers around either.
/// (`LitString` is copied into a fresh `SharedByteArray`.)
pub fn text_backing(v: &Value, table: &DataConTable) -> Option<SharedByteArray> {
    text_backing_with(v, &|id| is_con_named(id, "ByteArray", table))
}

/// Table-free unbox of an `Int`/`I#`-boxed field: a bare `Lit(LitInt)`, or
/// one layer of boxing recognized by `is_int_con` (a caller-supplied
/// `DataConId` comparison, not a table name lookup).
fn unbox_int_with(v: &Value, is_int_con: &dyn Fn(DataConId) -> bool) -> Option<i64> {
    let mut cur = v;
    loop {
        match cur {
            Value::Lit(Literal::LitInt(n)) => return Some(*n),
            Value::Con(id, fields) if fields.len() == 1 && is_int_con(*id) => {
                cur = &fields[0];
            }
            _ => return None,
        }
    }
}

/// Why a strict Text decode was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextShapeError {
    /// Not a 3-field Text worker con.
    WrongShape,
    /// The backing field has no recognizable byte form (see [`text_backing`]).
    BadBacking,
    /// An off/len field did not unbox to an Int.
    BadOffLen,
    /// Negative or out-of-bounds slice.
    BadSlice { off: i64, len: i64, ba_len: usize },
}

/// STRICT slice policy: reject negative or out-of-bounds `off`/`len` with a
/// typed error instead of clamping. Returns the raw slice bytes; UTF-8
/// validation is the caller's policy.
pub fn text_bytes_checked(
    fields: &[Value],
    table: &DataConTable,
) -> Result<Vec<u8>, TextShapeError> {
    if fields.len() != 3 {
        return Err(TextShapeError::WrongShape);
    }
    let backing = text_backing(&fields[0], table).ok_or(TextShapeError::BadBacking)?;
    let off_i = unbox_int(&fields[1], table).ok_or(TextShapeError::BadOffLen)?;
    let len_i = unbox_int(&fields[2], table).ok_or(TextShapeError::BadOffLen)?;
    let bytes = backing.lock().unwrap_or_else(PoisonError::into_inner);
    let bad = || TextShapeError::BadSlice {
        off: off_i,
        len: len_i,
        ba_len: bytes.len(),
    };
    // Validate as signed BEFORE casting: a negative offset cast to usize is
    // huge, and `off + len` then overflows. Checked arithmetic turns every
    // malformed slice into a clean Err. (proptest_boundary_roundtrip B2)
    let off = usize::try_from(off_i).map_err(|_| bad())?;
    let len = usize::try_from(len_i).map_err(|_| bad())?;
    let end = off
        .checked_add(len)
        .filter(|&e| e <= bytes.len())
        .ok_or_else(bad)?;
    Ok(bytes[off..end].to_vec())
}

/// LENIENT slice policy (the renderer's): never fails on a malformed slice.
/// An off/len field that does not unbox defaults to 0 / backing length; the
/// i64→usize cast wraps, so a NEGATIVE off clamps to an EMPTY slice (not
/// offset 0); both ends clamp into bounds so no slice can panic — this runs
/// in-process in the MCP server. (proptest_render_json B-panic)
///
/// Returns `None` only when the backing field has no recognizable byte form.
/// UTF-8 validation is the caller's policy.
///
/// Table-free generalization of the same policy, for callers holding
/// individually resolved `DataConId`s rather than a full `&DataConTable`
/// (e.g. `JsonDecode`'s tree-walker arm, which only caches a `JsonConIds`).
/// `is_bytearray_con`/`is_int_con` recognize the lifted `ByteArray` wrapper
/// / boxed `I#` cons respectively — pass `|_| false` for either when no
/// concrete id is reachable; that wrapper form then simply goes
/// unrecognized (`None`/default).
pub fn text_bytes_clamped_with(
    fields: &[Value],
    is_bytearray_con: impl Fn(DataConId) -> bool,
    is_int_con: impl Fn(DataConId) -> bool,
) -> Option<Vec<u8>> {
    if fields.len() != 3 {
        return None;
    }
    let backing = text_backing_with(&fields[0], &is_bytearray_con)?;
    let bytes = backing.lock().unwrap_or_else(PoisonError::into_inner);
    let off = unbox_int_with(&fields[1], &is_int_con).unwrap_or(0) as usize;
    let len = unbox_int_with(&fields[2], &is_int_con).unwrap_or(bytes.len() as i64) as usize;
    let off = off.min(bytes.len());
    let end = off.saturating_add(len).min(bytes.len());
    Some(bytes[off..end].to_vec())
}

/// Table-based entry point for [`text_bytes_clamped_with`]: recognizes the
/// `ByteArray`/`I#` wrapper cons by name via `table`.
pub fn text_bytes_clamped(fields: &[Value], table: &DataConTable) -> Option<Vec<u8>> {
    text_bytes_clamped_with(
        fields,
        |id| is_con_named(id, "ByteArray", table),
        |id| is_con_named(id, "I#", table),
    )
}

// ---------------------------------------------------------------------------
// Lists — `:` / `[]` cons cells
// ---------------------------------------------------------------------------

/// Build a cons list (`:`/`[]`) from already-converted elements.
pub fn make_list(items: Vec<Value>, cons_id: DataConId, nil_id: DataConId) -> Value {
    let mut acc = Value::Con(nil_id, vec![]);
    for v in items.into_iter().rev() {
        acc = Value::Con(cons_id, vec![v, acc]);
    }
    acc
}

// ---------------------------------------------------------------------------
// Data.Map.Strict — Bin/Tip balanced tree, size boxed as I#
// ---------------------------------------------------------------------------

/// The empty map.
pub fn map_tip(tip_id: DataConId) -> Value {
    Value::Con(tip_id, vec![])
}

/// One `Bin size k v l r` node. The leading `!Int` (subtree size) is boxed as
/// `I#(size)` to match GHC's heap.
pub fn map_bin_node(
    size: i64,
    key: Value,
    val: Value,
    left: Value,
    right: Value,
    bin_id: DataConId,
    i_hash: DataConId,
) -> Value {
    Value::Con(bin_id, vec![box_int(size, i_hash), key, val, left, right])
}

/// Build a balanced `Data.Map.Strict` from key-sorted entries by
/// divide-and-conquer.
pub fn make_map_from_sorted(
    entries: Vec<(Value, Value)>,
    bin_id: DataConId,
    tip_id: DataConId,
    i_hash: DataConId,
) -> Value {
    fn go(entries: &mut [Option<(Value, Value)>], bin: DataConId, tip: DataConId, i: DataConId) -> Value {
        if entries.is_empty() {
            return map_tip(tip);
        }
        let size = entries.len() as i64;
        let mid = entries.len() / 2;
        let (k, v) = entries[mid].take().expect("entry taken twice");
        let (l, r) = entries.split_at_mut(mid);
        let left = go(l, bin, tip, i);
        let right = go(&mut r[1..], bin, tip, i);
        map_bin_node(size, k, v, left, right, bin, i)
    }
    let mut entries: Vec<Option<(Value, Value)>> = entries.into_iter().map(Some).collect();
    go(&mut entries, bin_id, tip_id, i_hash)
}

/// In-order walk of a `Bin`/`Tip` tree, calling `f(key, value, node_depth)`
/// for each entry. `depth` is the depth of the ROOT node; every level (and
/// each entry's callback) sees `depth + 1`, matching the renderer's
/// historical depth accounting. Nodes deeper than `max_depth`, and any
/// non-`Bin`/`Tip` value, are silently skipped.
pub fn walk_map_entries<'a>(
    v: &'a Value,
    table: &DataConTable,
    depth: usize,
    max_depth: usize,
    f: &mut dyn FnMut(&'a Value, &'a Value, usize),
) {
    if depth > max_depth {
        return;
    }
    if let Value::Con(id, fields) = v {
        match (table.name_of(*id).unwrap_or(""), fields.as_slice()) {
            ("Tip", []) => {}
            ("Bin", [_size, k, val, left, right]) => {
                walk_map_entries(left, table, depth + 1, max_depth, f);
                f(k, val, depth + 1);
                walk_map_entries(right, table, depth + 1, max_depth, f);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Vendored aeson Value — exact-int number policy
// ---------------------------------------------------------------------------

/// Exact-int policy (BUG-8): i64-representable JSON numbers ride
/// `NumberI(LitInt)` so they never lose precision; everything else rides the
/// Double-backed `Number(LitDouble)`.
pub fn json_number(
    n: &serde_json::Number,
    number_i: DataConId,
    number: DataConId,
) -> Value {
    if let Some(i) = n.as_i64() {
        Value::Con(number_i, vec![Value::Lit(Literal::LitInt(i))])
    } else {
        let f = n.as_f64().unwrap_or(0.0);
        Value::Con(number, vec![Value::Lit(Literal::LitDouble(f.to_bits()))])
    }
}

// ---------------------------------------------------------------------------
// GHC bignum limbs (IP/IN payloads)
// ---------------------------------------------------------------------------

/// Unwrap the `BigNat#` payload of an `IP`/`IN` con to its raw limb bytes.
/// Accepts `Value::ByteArray`, `LitByteArray`, or ONE lifted
/// `Con("ByteArray", [..])` layer around either — exactly the forms heap
/// readers emit for bignum payloads. (Deliberately narrower than
/// [`text_backing`]: no `LitString`, one wrapper layer only, preserving the
/// historical renderer's accepted set byte-for-byte.)
pub fn bignat_backing_bytes(v: &Value, table: &DataConTable) -> Option<Vec<u8>> {
    fn raw(v: &Value) -> Option<Vec<u8>> {
        match v {
            Value::ByteArray(bs) => Some(
                bs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
            Value::Lit(Literal::LitByteArray(bytes)) => Some(bytes.clone()),
            _ => None,
        }
    }
    match v {
        Value::Con(id, fields) if fields.len() == 1 && is_con_named(*id, "ByteArray", table) => {
            raw(&fields[0])
        }
        other => raw(other),
    }
}

/// Convert little-endian bignat limb bytes (padded to an 8-byte boundary, as
/// produced by `bigNatLitBytes`) to an exact decimal string. Each 8-byte
/// chunk is one u64 limb; limbs are little-endian.
pub fn bignat_bytes_to_decimal(bytes: &[u8]) -> String {
    if bytes.is_empty() || bytes.iter().all(|&b| b == 0) {
        return "0".to_string();
    }
    let mut limbs: Vec<u64> = bytes
        .chunks(8)
        .map(|chunk| {
            let mut arr = [0u8; 8];
            arr[..chunk.len()].copy_from_slice(chunk);
            u64::from_le_bytes(arr)
        })
        .collect();
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
    if limbs.is_empty() {
        return "0".to_string();
    }
    if limbs.len() == 1 {
        return limbs[0].to_string();
    }
    if limbs.len() == 2 {
        let val = (limbs[1] as u128) << 64 | (limbs[0] as u128);
        return val.to_string();
    }
    // General: extract decimal digits via repeated division by 10.
    // dividend = (rem_u128 << 64) | limb fits comfortably in u128 (rem < 10).
    let mut digits: Vec<u8> = Vec::new();
    while !limbs.is_empty() {
        let mut rem: u128 = 0;
        for limb in limbs.iter_mut().rev() {
            let d = (rem << 64) | (*limb as u128);
            *limb = (d / 10) as u64;
            rem = d % 10;
        }
        digits.push(rem as u8 + b'0');
        while limbs.last() == Some(&0) {
            limbs.pop();
        }
    }
    digits.reverse();
    String::from_utf8(digits).expect("only ascii digits")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataCon;

    fn test_table() -> DataConTable {
        let mut t = DataConTable::new();
        let cons = [
            (1, "I#", 1),
            (2, "W#", 1),
            (3, "C#", 1),
            (4, "D#", 1),
            (5, "F#", 1),
            (6, "True", 0),
            (7, "False", 0),
            (8, "Text", 3),
            (9, "ByteArray", 1),
            (10, ":", 2),
            (11, "[]", 0),
            (12, "Bin", 5),
            (13, "Tip", 0),
            (14, "NumberI", 1),
            (15, "Number", 1),
        ];
        for (id, name, arity) in cons {
            t.insert(DataCon {
                id: DataConId(id),
                name: name.into(),
                tag: id as u32,
                rep_arity: arity,
                field_bangs: vec![],
                qualified_name: None,
            });
        }
        t
    }

    fn id(table: &DataConTable, name: &str) -> DataConId {
        table.get_by_name(name).unwrap()
    }

    #[test]
    fn unbox_int_unwraps_nested_boxes() {
        let t = test_table();
        let i = id(&t, "I#");
        let v = Value::Con(i, vec![Value::Con(i, vec![Value::Lit(Literal::LitInt(7))])]);
        assert_eq!(unbox_int(&v, &t), Some(7));
        assert_eq!(unbox_int(&Value::Lit(Literal::LitInt(3)), &t), Some(3));
        assert_eq!(unbox_int(&Value::Lit(Literal::LitWord(3)), &t), None);
    }

    #[test]
    fn box_unbox_roundtrips() {
        let t = test_table();
        assert_eq!(unbox_int(&box_int(-4, id(&t, "I#")), &t), Some(-4));
        assert_eq!(unbox_word(&box_word(9, id(&t, "W#")), &t), Some(9));
        assert_eq!(unbox_double(&box_double(1.5, id(&t, "D#")), &t), Some(1.5));
        assert_eq!(unbox_float(&box_float(2.5, id(&t, "F#")), &t), Some(2.5));
        assert_eq!(unbox_char(&box_char('λ', id(&t, "C#")), &t), Some('λ'));
        let tr = id(&t, "True");
        let fa = id(&t, "False");
        assert_eq!(unbox_bool(&make_bool(true, tr, fa), &t), Some(true));
        assert_eq!(unbox_bool(&make_bool(false, tr, fa), &t), Some(false));
    }

    #[test]
    fn char_from_single_byte_text() {
        let t = test_table();
        let text = Value::Con(
            id(&t, "Text"),
            vec![
                Value::ByteArray(Arc::new(Mutex::new(b"xy".to_vec()))),
                Value::Lit(Literal::LitInt(1)),
                Value::Lit(Literal::LitInt(1)),
            ],
        );
        let c = Value::Con(id(&t, "C#"), vec![text]);
        assert_eq!(unbox_char(&c, &t), Some('y'));
    }

    #[test]
    fn text_roundtrip_and_backing_forms() {
        let t = test_table();
        let text_id = id(&t, "Text");
        let v = make_text("hello", text_id);
        match &v {
            Value::Con(_, fields) => {
                assert_eq!(text_bytes_checked(fields, &t).unwrap(), b"hello");
                assert_eq!(text_bytes_clamped(fields, &t).unwrap(), b"hello");
            }
            _ => panic!("expected Con"),
        }
        // LitString backing + lifted ByteArray wrapper layers
        let wrapped = Value::Con(
            id(&t, "ByteArray"),
            vec![Value::Con(
                id(&t, "ByteArray"),
                vec![Value::Lit(Literal::LitString(b"abc".to_vec()))],
            )],
        );
        let fields = vec![
            wrapped,
            Value::Lit(Literal::LitInt(1)),
            Value::Lit(Literal::LitInt(2)),
        ];
        assert_eq!(text_bytes_checked(&fields, &t).unwrap(), b"bc");
    }

    #[test]
    fn text_checked_rejects_bad_slices() {
        let t = test_table();
        let mk = |off: i64, len: i64| {
            vec![
                Value::ByteArray(Arc::new(Mutex::new(b"hello".to_vec()))),
                Value::Lit(Literal::LitInt(off)),
                Value::Lit(Literal::LitInt(len)),
            ]
        };
        assert!(matches!(
            text_bytes_checked(&mk(-1, 2), &t),
            Err(TextShapeError::BadSlice { .. })
        ));
        assert!(matches!(
            text_bytes_checked(&mk(0, 6), &t),
            Err(TextShapeError::BadSlice { .. })
        ));
        assert!(matches!(
            text_bytes_checked(&mk(4, i64::MAX), &t),
            Err(TextShapeError::BadSlice { .. })
        ));
    }

    #[test]
    fn text_clamped_matches_renderer_policy() {
        let t = test_table();
        let mk = |off: i64, len: i64| {
            vec![
                Value::ByteArray(Arc::new(Mutex::new(b"hello".to_vec()))),
                Value::Lit(Literal::LitInt(off)),
                Value::Lit(Literal::LitInt(len)),
            ]
        };
        // negative off wraps huge → clamps to EMPTY, not offset 0
        assert_eq!(text_bytes_clamped(&mk(-1, 2), &t).unwrap(), b"");
        // len overrun clamps to backing end
        assert_eq!(text_bytes_clamped(&mk(3, 99), &t).unwrap(), b"lo");
        // non-int off/len default to 0 / backing len
        let fields = vec![
            Value::ByteArray(Arc::new(Mutex::new(b"hi".to_vec()))),
            Value::Con(id(&t, "True"), vec![]),
            Value::Con(id(&t, "True"), vec![]),
        ];
        assert_eq!(text_bytes_clamped(&fields, &t).unwrap(), b"hi");
    }

    #[test]
    fn map_walk_is_in_order_and_depth_capped() {
        let t = test_table();
        let (bin, tip, i) = (id(&t, "Bin"), id(&t, "Tip"), id(&t, "I#"));
        let entries: Vec<(Value, Value)> = (0..5)
            .map(|n| {
                (
                    Value::Lit(Literal::LitInt(n)),
                    Value::Lit(Literal::LitInt(n * 10)),
                )
            })
            .collect();
        let m = make_map_from_sorted(entries, bin, tip, i);
        let mut seen = vec![];
        walk_map_entries(&m, &t, 0, 1000, &mut |k, v, _| {
            seen.push((unbox_int(k, &t).unwrap(), unbox_int(v, &t).unwrap()));
        });
        assert_eq!(seen, vec![(0, 0), (1, 10), (2, 20), (3, 30), (4, 40)]);
        // root size is the full entry count, boxed as I#
        match &m {
            Value::Con(_, fields) => assert_eq!(unbox_int(&fields[0], &t), Some(5)),
            _ => panic!("expected Bin"),
        }
        // depth cap silences everything
        let mut n = 0;
        walk_map_entries(&m, &t, 5, 4, &mut |_, _, _| n += 1);
        assert_eq!(n, 0);
    }

    #[test]
    fn json_number_exact_int_policy() {
        let t = test_table();
        let (ni, nd) = (id(&t, "NumberI"), id(&t, "Number"));
        let big: serde_json::Number = serde_json::from_str("9007199254740993").unwrap();
        match &json_number(&big, ni, nd) {
            Value::Con(c, fields) => {
                assert_eq!(*c, ni);
                assert!(
                    matches!(fields.as_slice(), [Value::Lit(Literal::LitInt(9007199254740993))])
                );
            }
            _ => panic!("expected Con"),
        }
        let frac: serde_json::Number = serde_json::from_str("1.5").unwrap();
        match &json_number(&frac, ni, nd) {
            Value::Con(c, fields) => {
                assert_eq!(*c, nd);
                let bits = 1.5f64.to_bits();
                assert!(
                    matches!(fields.as_slice(), [Value::Lit(Literal::LitDouble(b))] if *b == bits)
                );
            }
            _ => panic!("expected Con"),
        }
    }

    #[test]
    fn bignat_decode() {
        let t = test_table();
        assert_eq!(bignat_bytes_to_decimal(&[1, 0, 0, 0, 0, 0, 0, 0]), "1");
        let mut two_limbs = vec![0u8; 16];
        two_limbs[8] = 1;
        assert_eq!(bignat_bytes_to_decimal(&two_limbs), (1u128 << 64).to_string());
        // three limbs → repeated-division path: 2^128
        let mut three = vec![0u8; 24];
        three[16] = 1;
        assert_eq!(
            bignat_bytes_to_decimal(&three),
            "340282366920938463463374607431768211456"
        );
        // backing forms
        let raw = Value::ByteArray(Arc::new(Mutex::new(vec![42, 0, 0, 0, 0, 0, 0, 0])));
        assert_eq!(
            bignat_backing_bytes(&raw, &t).as_deref(),
            Some(&[42u8, 0, 0, 0, 0, 0, 0, 0][..])
        );
        let lifted = Value::Con(
            id(&t, "ByteArray"),
            vec![Value::Lit(Literal::LitByteArray(vec![7]))],
        );
        assert_eq!(bignat_backing_bytes(&lifted, &t).as_deref(), Some(&[7u8][..]));
        assert_eq!(
            bignat_backing_bytes(&Value::Lit(Literal::LitString(b"x".to_vec())), &t),
            None
        );
    }
}
