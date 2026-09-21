//! Haskell-value-level Haskell data-shape facts — the ONE home for how common GHC
//! runtime shapes look as `tidepool_bridge::HaskellValue` trees, independent of any
//! heap layout.
//!
//! A "shape fact" is the materialized HaskellValue-tree encoding of a Haskell data
//! type:
//!   - `Text` as the worker `Text ByteArray# Int# Int#` (UTF-8 bytes), with
//!     raw `HaskellValue::ByteArray`, owned `LitString`/`LitByteArray` snapshots, and
//!     any number of lifted `Con("ByteArray", [..])` wrapper layers (sliced
//!     Texts from `splitOn` etc. stack them);
//!   - boxed machine numbers as `I#`/`W#`/`C#`/`D#`/`F#` single-field cons
//!     (possibly nested);
//!   - `Bool` as the nullary `True`/`False` cons;
//!   - `Data.Map.Strict` as `Bin size k v l r`/`Tip` with the `!Int` size
//!     boxed as `I#`;
//!   - GHC bignums (`IP`/`IN` payloads) as ByteArrays of little-endian 64-bit
//!     limbs (`bigNatLitBytes`);
//!   - the vendored aeson `Value`'s number policy: every JSON number rides
//!     `Number !Scientific`, the coefficient×10^exponent form that stays exact
//!     (the coefficient an exact `Integer` — `IS`/`IP`/`IN` — the exponent an
//!     `Int`).
//!
//! Encode and decode of the same shape live here, side by side —
//! `tidepool-runtime`'s renderer, `tidepool-bridge`'s FromHaskell/ToHaskell impls,
//! and the native `JsonDecode` primop read/write these shapes through this
//! module so they agree by construction.
//!
//! What does NOT live here:
//!   - prepared heap layouts and observation — `tidepool-codegen` owns them;
//!   - presentation policy — UTF-8 error surfaces, JSON depth/length
//!     truncation, and error-type mapping stay with each caller. Decoders
//!     here return raw bytes / `Option` / typed errors and let the caller
//!     choose the surface.
//!
//! Constructors take pre-resolved `DataConId`s (each caller has its own
//! resolution policy: `JsonConIds`, bridge `get_resilient`, …). Decoders take
//! a `&DataConTable` and recognize constructors BY NAME (`name_of`), which
//! tolerates duplicate same-name cons from cross-module closures.

use crate::value::{SharedByteArray, HaskellValue};
use std::sync::{Arc, Mutex, PoisonError};
use tidepool_repr::{DataConId, DataConTable, Literal};

/// The sentinel `DataConId` under which heap readers surface `Array#` /
/// `SmallArray#` payloads as `Con(ARRAY_SENTINEL, elems)` — a bare element
/// vector with no real constructor. Produced by
/// prepared-heap observation; consumed by e.g. the renderer's `Vector` arm.
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
        pub fn $fn_name(v: &HaskellValue, table: &DataConTable) -> Option<$ty> {
            let mut cur = v;
            loop {
                match cur {
                    HaskellValue::Lit($pat) => return Some($out),
                    HaskellValue::Con(id, fields)
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
pub fn box_int(n: i64, i_hash: DataConId) -> HaskellValue {
    HaskellValue::Con(i_hash, vec![HaskellValue::Lit(Literal::LitInt(n))])
}

/// Box a `Word` as `W#(LitWord n)`.
pub fn box_word(n: u64, w_hash: DataConId) -> HaskellValue {
    HaskellValue::Con(w_hash, vec![HaskellValue::Lit(Literal::LitWord(n))])
}

/// Box a `Double` as `D#(LitDouble bits)`.
pub fn box_double(f: f64, d_hash: DataConId) -> HaskellValue {
    HaskellValue::Con(d_hash, vec![HaskellValue::Lit(Literal::LitDouble(f.to_bits()))])
}

/// Box a `Float` as `F#(LitFloat bits)`.
pub fn box_float(f: f32, f_hash: DataConId) -> HaskellValue {
    HaskellValue::Con(
        f_hash,
        vec![HaskellValue::Lit(Literal::LitFloat(f.to_bits() as u64))],
    )
}

/// Box a `Char` as `C#(LitChar c)`.
pub fn box_char(c: char, c_hash: DataConId) -> HaskellValue {
    HaskellValue::Con(c_hash, vec![HaskellValue::Lit(Literal::LitChar(c))])
}

/// Extract a `char` from any of its HaskellValue shapes:
///   1. bare `LitChar`;
///   2. bare target-width `LitWord` for an unboxed `Char#`;
///   3. `C#` around either literal;
///   4. `C#(Text(backing, off, 1))` — a char smuggled as a single-BYTE Text;
///      the byte at `off` is read as a raw 0–255 code point (historical
///      renderer semantics, NOT UTF-8 decoding).
pub fn unbox_char(v: &HaskellValue, table: &DataConTable) -> Option<char> {
    match v {
        HaskellValue::Lit(Literal::LitChar(_) | Literal::LitWord(_)) => char_payload(v, table),
        HaskellValue::Con(id, fields) if fields.len() == 1 && is_con_named(*id, "C#", table) => {
            char_payload(&fields[0], table)
        }
        _ => None,
    }
}

fn char_payload(v: &HaskellValue, table: &DataConTable) -> Option<char> {
    match v {
        HaskellValue::Lit(Literal::LitChar(c)) => Some(*c),
        HaskellValue::Lit(Literal::LitWord(code_point)) => {
            u32::try_from(*code_point).ok().and_then(char::from_u32)
        }
        HaskellValue::Con(id, fields) if fields.len() == 3 && is_con_named(*id, "Text", table) => {
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
pub fn unbox_bool(v: &HaskellValue, table: &DataConTable) -> Option<bool> {
    match v {
        HaskellValue::Con(id, fields) if fields.is_empty() => match table.name_of(*id) {
            Some("True") => Some(true),
            Some("False") => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Build the nullary `True`/`False` con.
pub fn make_bool(b: bool, true_id: DataConId, false_id: DataConId) -> HaskellValue {
    HaskellValue::Con(if b { true_id } else { false_id }, vec![])
}

// ---------------------------------------------------------------------------
// Text — worker `Text ByteArray# Int# Int#`
// ---------------------------------------------------------------------------

/// Build the worker `Text ByteArray# Int# Int#` for a UTF-8 string
/// (offset 0, raw `HaskellValue::ByteArray` backing).
pub fn make_text(s: &str, text_id: DataConId) -> HaskellValue {
    let bytes = s.as_bytes().to_vec();
    let len = bytes.len() as i64;
    HaskellValue::Con(
        text_id,
        vec![
            HaskellValue::ByteArray(Arc::new(Mutex::new(bytes))),
            HaskellValue::Lit(Literal::LitInt(0)),
            HaskellValue::Lit(Literal::LitInt(len)),
        ],
    )
}

/// Unwrap a Text backing field to its raw bytes, given a recognizer for the
/// lifted `Con("ByteArray", [..])` wrapper layer. Accepts a raw
/// `HaskellValue::ByteArray` or an owned `LitString`/`LitByteArray` snapshot
/// unconditionally (literals are copied into a fresh `SharedByteArray`);
/// table-free callers that cannot
/// recognize the wrapper con (no `DataConId` for it in hand) pass `|_|
/// false` and simply won't unwrap that form.
fn text_backing_with(
    v: &HaskellValue,
    is_bytearray_con: &dyn Fn(DataConId) -> bool,
) -> Option<SharedByteArray> {
    let mut cur = v;
    loop {
        match cur {
            HaskellValue::ByteArray(bs) => return Some(bs.clone()),
            // Prepared observation publishes owned byte snapshots; both literal
            // forms are data, with no live heap or mutator handle to retain.
            HaskellValue::Lit(Literal::LitString(bytes) | Literal::LitByteArray(bytes)) => {
                return Some(Arc::new(Mutex::new(bytes.clone())));
            }
            HaskellValue::Con(id, fields) if fields.len() == 1 && is_bytearray_con(*id) => {
                cur = &fields[0];
            }
            _ => return None,
        }
    }
}

/// Unwrap a Text backing field to its raw bytes. Accepts a raw
/// `HaskellValue::ByteArray`, an owned `LitString`/`LitByteArray` snapshot, or any
/// number of lifted `Con("ByteArray", [..])` wrapper layers around them.
/// Literal bytes are copied into a fresh `SharedByteArray`.
pub fn text_backing(v: &HaskellValue, table: &DataConTable) -> Option<SharedByteArray> {
    text_backing_with(v, &|id| is_con_named(id, "ByteArray", table))
}

/// Table-free unbox of an `Int`/`I#`-boxed field: a bare `Lit(LitInt)`, or
/// any number of nested boxing layers recognized by `is_int_con` (a
/// caller-supplied `DataConId` predicate, not a table name lookup).
fn unbox_int_with(v: &HaskellValue, is_int_con: &dyn Fn(DataConId) -> bool) -> Option<i64> {
    let mut cur = v;
    loop {
        match cur {
            HaskellValue::Lit(Literal::LitInt(n)) => return Some(*n),
            HaskellValue::Con(id, fields) if fields.len() == 1 && is_int_con(*id) => {
                cur = &fields[0];
            }
            _ => return None,
        }
    }
}

/// Why a strict Text decode was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextShapeError {
    /// Not a 3-field Text worker con.
    #[error("not a 3-field Text worker constructor")]
    WrongShape,
    /// The backing field has no recognizable byte form (see [`text_backing`]).
    #[error("Text backing field has no recognizable byte form")]
    BadBacking,
    /// An off/len field did not unbox to an Int.
    #[error("Text off/len field did not unbox to an Int")]
    BadOffLen,
    /// Negative or out-of-bounds slice.
    #[error("Text slice out of bounds: off={off}, len={len}, backing len={ba_len}")]
    BadSlice {
        /// The requested offset.
        off: i64,
        /// The requested length.
        len: i64,
        /// The backing byte array's actual length.
        ba_len: usize,
    },
}

/// STRICT slice policy: reject negative or out-of-bounds `off`/`len` with a
/// typed error instead of clamping. Returns the raw slice bytes; UTF-8
/// validation is the caller's policy.
pub fn text_bytes_checked(
    fields: &[HaskellValue],
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
    fields: &[HaskellValue],
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
pub fn text_bytes_clamped(fields: &[HaskellValue], table: &DataConTable) -> Option<Vec<u8>> {
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
pub fn make_list(items: Vec<HaskellValue>, cons_id: DataConId, nil_id: DataConId) -> HaskellValue {
    let mut acc = HaskellValue::Con(nil_id, vec![]);
    for v in items.into_iter().rev() {
        acc = HaskellValue::Con(cons_id, vec![v, acc]);
    }
    acc
}

// ---------------------------------------------------------------------------
// Data.Map.Strict — Bin/Tip balanced tree, size boxed as I#
// ---------------------------------------------------------------------------

/// The empty map.
pub fn map_tip(tip_id: DataConId) -> HaskellValue {
    HaskellValue::Con(tip_id, vec![])
}

/// One `Bin size k v l r` node. The leading `!Int` (subtree size) is boxed as
/// `I#(size)` to match GHC's heap.
pub fn map_bin_node(
    size: i64,
    key: HaskellValue,
    val: HaskellValue,
    left: HaskellValue,
    right: HaskellValue,
    bin_id: DataConId,
    i_hash: DataConId,
) -> HaskellValue {
    HaskellValue::Con(bin_id, vec![box_int(size, i_hash), key, val, left, right])
}

/// Build a balanced `Data.Map.Strict` from key-sorted entries by
/// divide-and-conquer.
pub fn make_map_from_sorted(
    entries: Vec<(HaskellValue, HaskellValue)>,
    bin_id: DataConId,
    tip_id: DataConId,
    i_hash: DataConId,
) -> HaskellValue {
    fn go(
        entries: &mut [Option<(HaskellValue, HaskellValue)>],
        bin: DataConId,
        tip: DataConId,
        i: DataConId,
    ) -> HaskellValue {
        if entries.is_empty() {
            return map_tip(tip);
        }
        let size = entries.len() as i64;
        let mid = entries.len() / 2;
        #[allow(clippy::expect_used, reason = "entry taken twice")]
        let (k, v) = entries[mid].take().expect("entry taken twice");
        let (l, r) = entries.split_at_mut(mid);
        let left = go(l, bin, tip, i);
        let right = go(&mut r[1..], bin, tip, i);
        map_bin_node(size, k, v, left, right, bin, i)
    }
    let mut entries: Vec<Option<(HaskellValue, HaskellValue)>> = entries.into_iter().map(Some).collect();
    go(&mut entries, bin_id, tip_id, i_hash)
}

/// In-order walk of a `Bin`/`Tip` tree, calling `f(key, value, node_depth)`
/// for each entry. `depth` is the depth of the ROOT node; every level (and
/// each entry's callback) sees `depth + 1`, matching the renderer's
/// historical depth accounting. Nodes deeper than `max_depth`, and any
/// non-`Bin`/`Tip` value, are silently skipped.
pub fn walk_map_entries<'a>(
    v: &'a HaskellValue,
    table: &DataConTable,
    depth: usize,
    max_depth: usize,
    f: &mut dyn FnMut(&'a HaskellValue, &'a HaskellValue, usize),
) {
    if depth > max_depth {
        return;
    }
    if let HaskellValue::Con(id, fields) = v {
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
// Vendored aeson HaskellValue — exact-int number policy
// ---------------------------------------------------------------------------

/// Constructor ids for building an aeson `Number (Scientific coeff exp)`.
#[derive(Clone, Copy)]
pub struct NumberConIds {
    /// `Number` constructor (arity 1, wraps a `Scientific`).
    pub number: DataConId,
    /// `Scientific` constructor (arity 2: coefficient `Integer`, base10Exponent `Int`).
    pub scientific: DataConId,
    /// `IS` — the single-machine-word `Integer` constructor.
    pub is: DataConId,
    /// `IP` — the positive-multi-limb `Integer` constructor.
    pub ip: DataConId,
    /// `IN` — the negative-multi-limb `Integer` constructor.
    pub in_: DataConId,
}

/// Split a JSON number token (`serde_json` under `arbitrary_precision` hands us
/// the exact source text) into an integer `coefficient` decimal string and a
/// `base10Exponent`, such that the value equals `coefficient * 10^exponent`.
/// e.g. `"3.14"` → `("314", -2)`, `"1e10"` → `("1", 10)`, `"-0.001"` → `("-1", -3)`.
pub fn parse_decimal_token(tok: &str) -> (String, i64) {
    let (sign, rest) = match tok.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", tok.strip_prefix('+').unwrap_or(tok)),
    };
    let (mantissa, exp_part) = match rest.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i64>().unwrap_or(0)),
        None => (rest, 0),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    // coefficient digits = int ++ frac; exponent shifts down by the frac length.
    let mut digits = String::with_capacity(int_part.len() + frac_part.len());
    digits.push_str(int_part);
    digits.push_str(frac_part);
    let exponent = exp_part - frac_part.len() as i64;
    // Strip leading zeros (keep at least one digit) so `integer_from_decimal`'s
    // i64 fast-path fires whenever possible.
    let trimmed = digits.trim_start_matches('0');
    let coeff = if trimmed.is_empty() {
        "0".to_string()
    } else {
        format!("{sign}{trimmed}")
    };
    (coeff, exponent)
}

/// Whether `tok` (a raw JSON number token, e.g. from `serde_json::Number::as_str`
/// under `arbitrary_precision`) has an exponent part [`parse_decimal_token`]
/// cannot represent as an `i64` — the case it silently maps to `unwrap_or(0)`
/// (`1e99999999999999999999` would decode as `1×10⁰` instead of erroring).
/// `parse_decimal_token` itself stays infallible (shared by `tidepool-bridge`
/// and `tidepool-mcp`'s eval-prep source rendering); callers at an untrusted
/// JSON-text boundary can use this to reject the token before building on it.
pub fn decimal_token_exponent_overflows(tok: &str) -> bool {
    let rest = tok
        .strip_prefix('-')
        .or_else(|| tok.strip_prefix('+'))
        .unwrap_or(tok);
    let mantissa_and_exp = rest.split_once(['e', 'E']);
    match mantissa_and_exp {
        Some((_, exp_part)) => exp_part.parse::<i64>().is_err(),
        None => false,
    }
}

/// Build an exact aeson `Number (Scientific coeff exp)` from a parsed JSON
/// number. No precision is lost: the coefficient rides an exact `Integer`
/// (`IS`/`IP`/`IN`) and the base-10 exponent an `Int`. Requires
/// `serde_json`'s `arbitrary_precision` so `n.as_str()` is the exact token.
pub fn scientific_from_number(n: &serde_json::Number, ids: &NumberConIds) -> HaskellValue {
    let (coeff, exp) = parse_decimal_token(n.as_str());
    let coefficient = integer_from_decimal(&coeff, ids.is, ids.ip, ids.in_);
    let sci = HaskellValue::Con(
        ids.scientific,
        vec![coefficient, HaskellValue::Lit(Literal::LitInt(exp))],
    );
    HaskellValue::Con(ids.number, vec![sci])
}

// ---------------------------------------------------------------------------
// GHC bignum limbs (IP/IN payloads)
// ---------------------------------------------------------------------------

/// Build a GHC `Integer` heap value from an exact decimal string (the inverse of
/// [`bignat_bytes_to_decimal`]). `IS Int#` when the value fits a machine `Int`;
/// otherwise `IP`/`IN` carrying the magnitude as little-endian u64 limb bytes
/// (8 bytes per limb, least-significant first — exactly the layout the decoder
/// reads back). This is what lets a >i64 JSON integer decode to an exact
/// `Scientific` coefficient instead of a lossy `Double`.
pub fn integer_from_decimal(
    s: &str,
    is_id: DataConId,
    ip_id: DataConId,
    in_id: DataConId,
) -> HaskellValue {
    // Machine-Int fast path (the overwhelmingly common case).
    if let Ok(i) = s.parse::<i64>() {
        return HaskellValue::Con(is_id, vec![HaskellValue::Lit(Literal::LitInt(i))]);
    }
    let (neg, mag) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    // Horner over base 2^64: limbs = limbs*10 + digit, little-endian.
    let mut limbs: Vec<u64> = Vec::new();
    for b in mag.bytes() {
        let digit = (b - b'0') as u64;
        let mut carry = digit;
        for limb in limbs.iter_mut() {
            let v = (*limb as u128) * 10 + carry as u128;
            *limb = v as u64;
            carry = (v >> 64) as u64;
        }
        if carry != 0 {
            limbs.push(carry);
        }
    }
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
    let bytes: Vec<u8> = limbs.iter().flat_map(|l| l.to_le_bytes()).collect();
    let con = if neg { in_id } else { ip_id };
    HaskellValue::Con(con, vec![HaskellValue::Lit(Literal::LitByteArray(bytes))])
}

/// Unwrap the `BigNat#` payload of an `IP`/`IN` con to its raw limb bytes.
/// Accepts `HaskellValue::ByteArray`, `LitByteArray`, or ONE lifted
/// `Con("ByteArray", [..])` layer around either — exactly the forms heap
/// readers emit for bignum payloads. (Deliberately narrower than
/// [`text_backing`]: no `LitString`, one wrapper layer only, preserving the
/// historical renderer's accepted set byte-for-byte.)
pub fn bignat_backing_bytes(v: &HaskellValue, table: &DataConTable) -> Option<Vec<u8>> {
    fn raw(v: &HaskellValue) -> Option<Vec<u8>> {
        match v {
            HaskellValue::ByteArray(bs) => Some(bs.lock().unwrap_or_else(PoisonError::into_inner).clone()),
            HaskellValue::Lit(Literal::LitByteArray(bytes)) => Some(bytes.clone()),
            _ => None,
        }
    }
    match v {
        HaskellValue::Con(id, fields) if fields.len() == 1 && is_con_named(*id, "ByteArray", table) => {
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
    #[allow(clippy::expect_used, reason = "only ascii digits")]
    String::from_utf8(digits).expect("only ascii digits")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FromHaskell;
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
            (15, "Number", 1),
            (14, "Scientific", 2),
            (16, "IS", 1),
            (17, "IP", 1),
            (18, "IN", 1),
        ];
        for (id, name, arity) in cons {
            t.insert(DataCon {
                id: DataConId(id),
                name: name.into(),
                tag: id as u32,
                rep_arity: arity,
                field_bangs: vec![],
                qualified_name: None,
                type_name: String::new(),
            });
        }
        t
    }

    fn id(table: &DataConTable, name: &str) -> DataConId {
        table.get_by_name(name).unwrap()
    }

    /// The `Integer` builder is the exact inverse of `bignat_bytes_to_decimal`:
    /// small values box as `IS Int#`, big ones as `IP`/`IN` limb bytes, and both
    /// reconstruct to the original decimal string.
    #[test]
    fn integer_from_decimal_round_trips() {
        let (is, ip, in_) = (DataConId(100), DataConId(101), DataConId(102));
        let cases = [
            "0",
            "42",
            "-42",
            "9223372036854775807",               // i64::MAX
            "-9223372036854775808",              // i64::MIN
            "9223372036854775808",               // i64::MAX + 1 → IP
            "-9223372036854775809",              // i64::MIN - 1 → IN
            "265252859812191058636308480000000", // 30! (past u128)
            "-123456789012345678901234567890",
        ];
        for s in cases {
            let v = integer_from_decimal(s, is, ip, in_);
            let got = match &v {
                HaskellValue::Con(cid, f) if *cid == is => match &f[0] {
                    HaskellValue::Lit(Literal::LitInt(n)) => n.to_string(),
                    other => panic!("IS payload not LitInt: {other:?}"),
                },
                HaskellValue::Con(cid, f) if *cid == ip || *cid == in_ => {
                    let bytes = match &f[0] {
                        HaskellValue::Lit(Literal::LitByteArray(b)) => b.clone(),
                        other => panic!("IP/IN payload not LitByteArray: {other:?}"),
                    };
                    let mag = bignat_bytes_to_decimal(&bytes);
                    if *cid == in_ {
                        format!("-{mag}")
                    } else {
                        mag
                    }
                }
                other => panic!("unexpected Integer shape: {other:?}"),
            };
            assert_eq!(got, s, "round-trip mismatch for {s}");
            // Big values must NOT box as IS (that would silently cap at i64).
            if s.parse::<i64>().is_err() {
                assert!(
                    matches!(&v, HaskellValue::Con(cid, _) if *cid == ip || *cid == in_),
                    "{s} should be IP/IN, got {v:?}"
                );
            }
        }
    }

    #[test]
    fn unbox_int_unwraps_nested_boxes() {
        let t = test_table();
        let i = id(&t, "I#");
        let v = HaskellValue::Con(i, vec![HaskellValue::Con(i, vec![HaskellValue::Lit(Literal::LitInt(7))])]);
        assert_eq!(unbox_int(&v, &t), Some(7));
        assert_eq!(unbox_int(&HaskellValue::Lit(Literal::LitInt(3)), &t), Some(3));
        assert_eq!(unbox_int(&HaskellValue::Lit(Literal::LitWord(3)), &t), None);
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
        let text = HaskellValue::Con(
            id(&t, "Text"),
            vec![
                HaskellValue::ByteArray(Arc::new(Mutex::new(b"xy".to_vec()))),
                HaskellValue::Lit(Literal::LitInt(1)),
                HaskellValue::Lit(Literal::LitInt(1)),
            ],
        );
        let c = HaskellValue::Con(id(&t, "C#"), vec![text]);
        assert_eq!(unbox_char(&c, &t), Some('y'));
    }

    #[test]
    fn target_word_char_validates_code_points_and_converts_strings() {
        let table = test_table();
        let c_hash = id(&table, "C#");
        let cons = id(&table, ":");
        let nil = HaskellValue::Con(id(&table, "[]"), vec![]);
        for character in ['A', 'λ'] {
            let bare = HaskellValue::Lit(Literal::LitWord(character as u64));
            let boxed = HaskellValue::Con(c_hash, vec![bare.clone()]);
            assert_eq!(unbox_char(&bare, &table), Some(character));
            assert_eq!(unbox_char(&boxed, &table), Some(character));
        }
        for invalid in [0xd800, 0x11_0000, 0x1_0000_0041] {
            let bare = HaskellValue::Lit(Literal::LitWord(invalid));
            let boxed = HaskellValue::Con(c_hash, vec![bare.clone()]);
            assert_eq!(unbox_char(&bare, &table), None);
            assert_eq!(unbox_char(&boxed, &table), None);
        }
        let string = HaskellValue::Con(
            cons,
            vec![
                HaskellValue::Con(c_hash, vec![HaskellValue::Lit(Literal::LitWord('A' as u64))]),
                HaskellValue::Con(
                    cons,
                    vec![
                        HaskellValue::Con(c_hash, vec![HaskellValue::Lit(Literal::LitWord('λ' as u64))]),
                        nil,
                    ],
                ),
            ],
        );
        assert_eq!(String::from_value(&string, &table).unwrap(), "Aλ");
    }

    #[test]
    fn text_roundtrip_and_backing_forms() {
        let t = test_table();
        let text_id = id(&t, "Text");
        let v = make_text("hello", text_id);
        match &v {
            HaskellValue::Con(_, fields) => {
                assert_eq!(text_bytes_checked(fields, &t).unwrap(), b"hello");
                assert_eq!(text_bytes_clamped(fields, &t).unwrap(), b"hello");
            }
            _ => panic!("expected Con"),
        }
        // LitString backing + lifted ByteArray wrapper layers
        let wrapped = HaskellValue::Con(
            id(&t, "ByteArray"),
            vec![HaskellValue::Con(
                id(&t, "ByteArray"),
                vec![HaskellValue::Lit(Literal::LitString(b"abc".to_vec()))],
            )],
        );
        let fields = vec![
            wrapped,
            HaskellValue::Lit(Literal::LitInt(1)),
            HaskellValue::Lit(Literal::LitInt(2)),
        ];
        assert_eq!(text_bytes_checked(&fields, &t).unwrap(), b"bc");
    }

    #[test]
    fn text_lit_byte_array_snapshot_converts_with_and_without_table() {
        let t = test_table();
        let fields = vec![
            HaskellValue::Lit(Literal::LitByteArray(b"_hello_".to_vec())),
            HaskellValue::Lit(Literal::LitInt(1)),
            HaskellValue::Lit(Literal::LitInt(5)),
        ];
        let text = HaskellValue::Con(id(&t, "Text"), fields.clone());
        assert_eq!(String::from_value(&text, &t).unwrap(), "hello");
        assert_eq!(
            String::from_utf8(text_bytes_clamped_with(&fields, |_| false, |_| false).unwrap())
                .unwrap(),
            "hello"
        );

        let invalid_bounds = [
            HaskellValue::Lit(Literal::LitByteArray(b"hello".to_vec())),
            HaskellValue::Lit(Literal::LitInt(4)),
            HaskellValue::Lit(Literal::LitInt(2)),
        ];
        assert!(matches!(
            text_bytes_checked(&invalid_bounds, &t),
            Err(TextShapeError::BadSlice { .. })
        ));
        assert_eq!(
            text_bytes_clamped_with(&invalid_bounds, |_| false, |_| false).unwrap(),
            b"o"
        );

        let invalid_utf8_fields = vec![
            HaskellValue::Lit(Literal::LitByteArray(vec![0xff])),
            HaskellValue::Lit(Literal::LitInt(0)),
            HaskellValue::Lit(Literal::LitInt(1)),
        ];
        let invalid_utf8 = HaskellValue::Con(id(&t, "Text"), invalid_utf8_fields.clone());
        assert!(matches!(
            String::from_value(&invalid_utf8, &t),
            Err(crate::BridgeError::TypeMismatch { .. })
        ));
        assert!(String::from_utf8(
            text_bytes_clamped_with(&invalid_utf8_fields, |_| false, |_| false).unwrap()
        )
        .is_err());
    }

    #[test]
    fn text_checked_rejects_bad_slices() {
        let t = test_table();
        let mk = |off: i64, len: i64| {
            vec![
                HaskellValue::ByteArray(Arc::new(Mutex::new(b"hello".to_vec()))),
                HaskellValue::Lit(Literal::LitInt(off)),
                HaskellValue::Lit(Literal::LitInt(len)),
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
                HaskellValue::ByteArray(Arc::new(Mutex::new(b"hello".to_vec()))),
                HaskellValue::Lit(Literal::LitInt(off)),
                HaskellValue::Lit(Literal::LitInt(len)),
            ]
        };
        // negative off wraps huge → clamps to EMPTY, not offset 0
        assert_eq!(text_bytes_clamped(&mk(-1, 2), &t).unwrap(), b"");
        // len overrun clamps to backing end
        assert_eq!(text_bytes_clamped(&mk(3, 99), &t).unwrap(), b"lo");
        // non-int off/len default to 0 / backing len
        let fields = vec![
            HaskellValue::ByteArray(Arc::new(Mutex::new(b"hi".to_vec()))),
            HaskellValue::Con(id(&t, "True"), vec![]),
            HaskellValue::Con(id(&t, "True"), vec![]),
        ];
        assert_eq!(text_bytes_clamped(&fields, &t).unwrap(), b"hi");
    }

    #[test]
    fn map_walk_is_in_order_and_depth_capped() {
        let t = test_table();
        let (bin, tip, i) = (id(&t, "Bin"), id(&t, "Tip"), id(&t, "I#"));
        let entries: Vec<(HaskellValue, HaskellValue)> = (0..5)
            .map(|n| {
                (
                    HaskellValue::Lit(Literal::LitInt(n)),
                    HaskellValue::Lit(Literal::LitInt(n * 10)),
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
            HaskellValue::Con(_, fields) => assert_eq!(unbox_int(&fields[0], &t), Some(5)),
            _ => panic!("expected Bin"),
        }
        // depth cap silences everything
        let mut n = 0;
        walk_map_entries(&m, &t, 5, 4, &mut |_, _, _| n += 1);
        assert_eq!(n, 0);
    }

    #[test]
    fn parse_decimal_token_splits_coeff_and_exp() {
        assert_eq!(parse_decimal_token("3.14"), ("314".into(), -2));
        assert_eq!(parse_decimal_token("1e10"), ("1".into(), 10));
        assert_eq!(parse_decimal_token("-0.001"), ("-1".into(), -3));
        assert_eq!(parse_decimal_token("42"), ("42".into(), 0));
        assert_eq!(parse_decimal_token("1.5e-3"), ("15".into(), -4));
        assert_eq!(parse_decimal_token("0"), ("0".into(), 0));
        assert_eq!(parse_decimal_token("100"), ("100".into(), 0));
        // exact big integer past f64/i64 stays exact in the coefficient
        assert_eq!(
            parse_decimal_token("9007199254740993"),
            ("9007199254740993".into(), 0)
        );
    }

    /// F7: `decimal_token_exponent_overflows` must flag exactly the tokens
    /// whose exponent `parse_decimal_token` would otherwise silently zero.
    #[test]
    fn decimal_token_exponent_overflows_flags_unparseable_exponent() {
        assert!(decimal_token_exponent_overflows("1e99999999999999999999"));
        assert!(decimal_token_exponent_overflows("-1e99999999999999999999"));
        assert!(decimal_token_exponent_overflows(
            "1E999999999999999999999999"
        ));
        // Sanity: ordinary tokens (including large-but-representable exponents
        // and exponent-free tokens) are NOT flagged.
        assert!(!decimal_token_exponent_overflows("1e10"));
        assert!(!decimal_token_exponent_overflows("3.14"));
        assert!(!decimal_token_exponent_overflows("42"));
        assert!(!decimal_token_exponent_overflows("1e9223372036854775807"));
    }

    #[test]
    fn scientific_from_number_builds_exact_contract() {
        let ids = NumberConIds {
            number: DataConId(200),
            scientific: DataConId(201),
            is: DataConId(202),
            ip: DataConId(203),
            in_: DataConId(204),
        };
        // A >i64 integer: Number(Scientific(IP<limbs>, 0)) — exact, not Double.
        let big: serde_json::Number =
            serde_json::from_str("265252859812191058636308480000000").unwrap();
        match &scientific_from_number(&big, &ids) {
            HaskellValue::Con(num, nf) => {
                assert_eq!(*num, ids.number);
                match &nf[0] {
                    HaskellValue::Con(sci, sf) => {
                        assert_eq!(*sci, ids.scientific);
                        assert!(matches!(&sf[1], HaskellValue::Lit(Literal::LitInt(0))));
                        match &sf[0] {
                            HaskellValue::Con(c, cf) if *c == ids.ip => {
                                let bytes = match &cf[0] {
                                    HaskellValue::Lit(Literal::LitByteArray(b)) => b.clone(),
                                    o => panic!("coeff not bytes: {o:?}"),
                                };
                                assert_eq!(
                                    bignat_bytes_to_decimal(&bytes),
                                    "265252859812191058636308480000000"
                                );
                            }
                            o => panic!("coeff not IP: {o:?}"),
                        }
                    }
                    o => panic!("not Scientific: {o:?}"),
                }
            }
            o => panic!("not Number: {o:?}"),
        }
    }

    #[test]
    fn bignat_decode() {
        let t = test_table();
        assert_eq!(bignat_bytes_to_decimal(&[1, 0, 0, 0, 0, 0, 0, 0]), "1");
        let mut two_limbs = vec![0u8; 16];
        two_limbs[8] = 1;
        assert_eq!(
            bignat_bytes_to_decimal(&two_limbs),
            (1u128 << 64).to_string()
        );
        // three limbs → repeated-division path: 2^128
        let mut three = vec![0u8; 24];
        three[16] = 1;
        assert_eq!(
            bignat_bytes_to_decimal(&three),
            "340282366920938463463374607431768211456"
        );
        // backing forms
        let raw = HaskellValue::ByteArray(Arc::new(Mutex::new(vec![42, 0, 0, 0, 0, 0, 0, 0])));
        assert_eq!(
            bignat_backing_bytes(&raw, &t).as_deref(),
            Some(&[42u8, 0, 0, 0, 0, 0, 0, 0][..])
        );
        let lifted = HaskellValue::Con(
            id(&t, "ByteArray"),
            vec![HaskellValue::Lit(Literal::LitByteArray(vec![7]))],
        );
        assert_eq!(
            bignat_backing_bytes(&lifted, &t).as_deref(),
            Some(&[7u8][..])
        );
        assert_eq!(
            bignat_backing_bytes(&HaskellValue::Lit(Literal::LitString(b"x".to_vec())), &t),
            None
        );
    }
}
