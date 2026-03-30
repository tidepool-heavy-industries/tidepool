use serde_json::json;
use tidepool_eval::value::Value;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::types::{DataConId, Literal};

const MAX_DEPTH: usize = 1000;
const MAX_LIST_LEN: usize = 10000;

/// Opaque result of evaluating a Haskell expression.
/// Bundles the computed `Value` with the `DataConTable` needed to render constructor names.
#[derive(Debug)]
pub struct EvalResult {
    value: Value,
    table: DataConTable,
}

impl EvalResult {
    pub(crate) fn new(value: Value, table: DataConTable) -> Self {
        Self { value, table }
    }

    /// Render the result as structured JSON.
    pub fn to_json(&self) -> serde_json::Value {
        self.into()
    }

    /// Pretty-print the JSON representation.
    pub fn to_string_pretty(&self) -> String {
        let j = self.to_json();
        // For simple scalars, use compact form
        match &j {
            serde_json::Value::Number(_) | serde_json::Value::Bool(_) | serde_json::Value::Null => {
                j.to_string()
            }
            serde_json::Value::String(s) => {
                // Check if it's a single char or short string — use compact
                if s.len() <= 80 {
                    j.to_string()
                } else {
                    serde_json::to_string_pretty(&j).unwrap_or_else(|_| j.to_string())
                }
            }
            _ => serde_json::to_string_pretty(&j).unwrap_or_else(|_| j.to_string()),
        }
    }

    /// Consume and return the inner Value (escape hatch for callers that need raw access).
    pub fn into_value(self) -> Value {
        self.value
    }

    /// Borrow the inner Value.
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Borrow the DataConTable.
    pub fn table(&self) -> &DataConTable {
        &self.table
    }
}

impl From<&EvalResult> for serde_json::Value {
    fn from(result: &EvalResult) -> Self {
        value_to_json(&result.value, &result.table, 0)
    }
}

impl From<EvalResult> for serde_json::Value {
    fn from(result: EvalResult) -> Self {
        (&result).into()
    }
}

fn con_name(id: DataConId, table: &DataConTable) -> &str {
    table.name_of(id).unwrap_or("<unknown>")
}

/// Convert a tidepool Value to serde_json::Value using the DataConTable for constructor names.
pub fn value_to_json(val: &Value, table: &DataConTable, depth: usize) -> serde_json::Value {
    if depth > MAX_DEPTH {
        return json!("<depth limit>");
    }
    let d = depth + 1;

    match val {
        // Literals
        Value::Lit(lit) => literal_to_json(lit),

        // Constructors — pattern match on known names
        Value::Con(id, fields) => {
            let name = con_name(*id, table);
            match (name, fields.as_slice()) {
                // Booleans
                ("True", []) => json!(true),
                ("False", []) => json!(false),

                // Unit
                ("()", []) => json!(null),

                // Maybe
                ("Nothing", []) => json!(null),
                ("Just", [x]) => value_to_json(x, table, d),

                // freer-simple Pure
                ("Pure", [x]) => value_to_json(x, table, d),

                // Boxing constructors: I#, W#, C#, D#, F#
                ("I#", [x]) | ("W#", [x]) | ("D#", [x]) | ("F#", [x]) => value_to_json(x, table, d),
                ("C#", [x]) => value_to_json(x, table, d),

                // Text constructor: Text ByteArray off len → JSON string
                // ByteArray# may be raw Value::ByteArray or lifted Con("ByteArray", [Value::ByteArray(..)])
                ("Text", [ba_val, off_val, len_val]) => {
                    // Recursively unwrap Con("ByteArray", [x]) layers to find
                    // the raw ByteArray#. Sliced Text values (from splitOn etc.)
                    // can produce multiple wrapping layers.
                    let raw_ba = {
                        let mut cur = ba_val;
                        loop {
                            match cur {
                                Value::ByteArray(bs) => break Some(bs.clone()),
                                Value::Con(id, fields)
                                    if con_name(*id, table) == "ByteArray" && fields.len() == 1 =>
                                {
                                    cur = &fields[0];
                                }
                                _ => break None,
                            }
                        }
                    };
                    if let Some(bs) = raw_ba {
                        let borrowed = bs.lock().unwrap_or_else(|e| e.into_inner());
                        let off = extract_boxed_int(off_val, table).unwrap_or(0) as usize;
                        let len = extract_boxed_int(len_val, table).unwrap_or(borrowed.len() as i64)
                            as usize;
                        let end = (off + len).min(borrowed.len());
                        match std::str::from_utf8(&borrowed[off..end]) {
                            Ok(s) => json!(s),
                            Err(_) => json!(format!("<Text invalid UTF-8 len={}>", len)),
                        }
                    } else {
                        let field_jsons: Vec<serde_json::Value> =
                            fields.iter().map(|f| value_to_json(f, table, d)).collect();
                        json!({"constructor": "Text", "fields": field_jsons})
                    }
                }

                // List: try to collect as array or string
                ("[]", []) => {
                    // Empty list
                    json!([])
                }
                (":", [head, tail]) => collect_list(head, tail, table, d),

                // Tuples: (,), (,,), (,,,), etc.
                (n, fields)
                    if n.starts_with('(')
                        && n.ends_with(')')
                        && n.chars().all(|c| c == '(' || c == ')' || c == ',')
                        && fields.len() >= 2 =>
                {
                    let elems: Vec<serde_json::Value> =
                        fields.iter().map(|f| value_to_json(f, table, d)).collect();
                    json!(elems)
                }

                // Integer constructors (GHC.Num.Integer)
                // IS Int# — small integer (fits in machine word)
                ("IS", [x]) => value_to_json(x, table, d),
                // IP ByteArray# — positive big integer (not yet supported, show as string)
                ("IP", _) => json!("<big-integer>"),
                // IN ByteArray# — negative big integer (not yet supported, show as string)
                ("IN", _) => json!("<big-integer>"),

                // Scientific (Data.Scientific) — coefficient × 10^exponent
                ("Scientific", [coeff, exp_val]) => {
                    let c = match value_to_json(coeff, table, d) {
                        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
                        _ => 0,
                    };
                    let e = match value_to_json(exp_val, table, d) {
                        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
                        _ => 0,
                    };
                    // When exponent >= 0, produce an integer JSON number
                    if e >= 0 {
                        let val = c * 10i64.pow(e as u32);
                        json!(val)
                    } else {
                        let val = c as f64 * 10f64.powi(e as i32);
                        json!(val)
                    }
                }

                // Aeson Value constructors
                ("Null", []) => json!(null),
                ("Bool", [x]) => value_to_json(x, table, d),
                ("Number", [x]) => value_to_json(x, table, d),
                ("String", [x]) => value_to_json(x, table, d),
                ("Array", [vec_val]) => value_to_json(vec_val, table, d),
                ("Object", [map_val]) => map_to_json_object(map_val, table, d),

                // Data.Vector.Vector: worker-wrapper inlines fields as
                // Vector Int# Int# (Array# a). The Array# contents come from
                // heap_bridge as Con(DataConId(0), elems). Extract and render
                // the elements directly rather than delegating to value_to_json
                // (which would hit the generic constructor case for the nameless Con).
                ("Vector", fields) => {
                    // Find the Array# field: it's the Con(_, elems) with elements,
                    // typically the last field (after Int# offset and length).
                    let array_elems = fields.iter().rev().find_map(|f| match f {
                        Value::Con(_, elems) if !elems.is_empty() => Some(elems),
                        _ => None,
                    });
                    if let Some(elems) = array_elems {
                        let arr: Vec<serde_json::Value> =
                            elems.iter().map(|e| value_to_json(e, table, d)).collect();
                        json!(arr)
                    } else {
                        json!([])
                    }
                }

                // Generic constructor
                (_name, fields) => {
                    if fields.is_empty() {
                        json!(name)
                    } else {
                        let field_jsons: Vec<serde_json::Value> =
                            fields.iter().map(|f| value_to_json(f, table, d)).collect();
                        json!({
                            "constructor": name,
                            "fields": field_jsons
                        })
                    }
                }
            }
        }

        // Closures / thunks — opaque
        Value::Closure(_, _, _) => json!("<closure>"),
        Value::ThunkRef(_) => json!("<thunk>"),
        Value::JoinCont(_, _, _) => json!("<join>"),
        Value::ConFun(id, _, _) => {
            let name = con_name(*id, table);
            json!(format!("<partially-applied {}>", name))
        }
        Value::ByteArray(bs) => {
            let borrowed = bs.lock().unwrap_or_else(|e| e.into_inner());
            match std::str::from_utf8(&borrowed) {
                Ok(s) => json!(s),
                Err(_) => json!(format!("<ByteArray# len={}>", borrowed.len())),
            }
        }
    }
}

/// Walk a Data.Map.Strict Bin/Tip tree and collect key-value pairs into a JSON object.
/// Keys are Text values (Key newtype is erased by GHC).
fn map_to_json_object(val: &Value, table: &DataConTable, depth: usize) -> serde_json::Value {
    let mut entries = serde_json::Map::new();
    collect_map_entries(val, table, depth, &mut entries);
    serde_json::Value::Object(entries)
}

fn collect_map_entries(
    val: &Value,
    table: &DataConTable,
    depth: usize,
    out: &mut serde_json::Map<String, serde_json::Value>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    if let Value::Con(id, fields) = val {
        let name = con_name(*id, table);
        match (name, fields.as_slice()) {
            ("Tip", []) => {}
            // Bin size key value left right
            ("Bin", [_size, k, v, left, right]) => {
                collect_map_entries(left, table, depth + 1, out);
                let key_str = match value_to_json(k, table, depth + 1) {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                out.insert(key_str, value_to_json(v, table, depth + 1));
                collect_map_entries(right, table, depth + 1, out);
            }
            _ => {}
        }
    }
}

/// Extract an i64 from a potentially boxed Int value (LitInt or I#(I#(...(LitInt)))).
/// Recursively unwraps nested Con("I#", [x]) layers.
fn extract_boxed_int(val: &Value, table: &DataConTable) -> Option<i64> {
    let mut cur = val;
    loop {
        match cur {
            Value::Lit(Literal::LitInt(n)) => return Some(*n),
            Value::Con(id, fields) if fields.len() == 1 && table.get_by_name("I#") == Some(*id) => {
                cur = &fields[0];
            }
            _ => return None,
        }
    }
}

/// Try to extract a single char from a Value.
/// Handles: LitChar, C#(LitChar), C#(Text(ByteArray(1 byte), 0, 1)).
fn extract_char(val: &Value, table: &DataConTable) -> Option<char> {
    match val {
        Value::Lit(Literal::LitChar(c)) => Some(*c),
        Value::Con(id, fields) if con_name(*id, table) == "C#" && fields.len() == 1 => {
            extract_char_inner(&fields[0], table)
        }
        _ => None,
    }
}

/// Extract a char from the inner value of a C# constructor (or bare value).
fn extract_char_inner(val: &Value, table: &DataConTable) -> Option<char> {
    match val {
        Value::Lit(Literal::LitChar(c)) => Some(*c),
        // Text(ByteArray#, off, len) where len == 1 — single-byte char
        Value::Con(id, fields) if con_name(*id, table) == "Text" && fields.len() == 3 => {
            let len = extract_boxed_int(&fields[2], table)?;
            if len != 1 {
                return None;
            }
            let off = extract_boxed_int(&fields[1], table).unwrap_or(0) as usize;
            // Unwrap ByteArray layers to get the raw bytes
            let raw_ba = {
                let mut cur = &fields[0];
                loop {
                    match cur {
                        Value::ByteArray(bs) => break Some(bs.clone()),
                        Value::Con(cid, cfields)
                            if con_name(*cid, table) == "ByteArray" && cfields.len() == 1 =>
                        {
                            cur = &cfields[0];
                        }
                        _ => break None,
                    }
                }
            };
            let bs = raw_ba?;
            let borrowed = bs.lock().unwrap_or_else(|e| e.into_inner());
            let byte = *borrowed.get(off)?;
            Some(byte as char)
        }
        _ => None,
    }
}

fn literal_to_json(lit: &Literal) -> serde_json::Value {
    match lit {
        Literal::LitInt(n) => json!(n),
        Literal::LitWord(n) => json!(n),
        Literal::LitChar(c) => json!(c.to_string()),
        Literal::LitString(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) => json!(s),
            Err(_) => json!(format!("<binary:{} bytes>", bytes.len())),
        },
        Literal::LitFloat(bits) => {
            let f = f32::from_bits(*bits as u32) as f64;
            if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
                json!(f as i64)
            } else {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(json!(null))
            }
        }
        Literal::LitDouble(bits) => {
            let f = f64::from_bits(*bits);
            // If the double is integral and fits in i64, emit as integer
            if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
                json!(f as i64)
            } else {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(json!(null))
            }
        }
    }
}

/// Collect a cons-chain into a JSON array, or a JSON string if all elements are chars.
fn collect_list(
    head: &Value,
    tail: &Value,
    table: &DataConTable,
    depth: usize,
) -> serde_json::Value {
    let mut elems = vec![head];
    let mut current = tail;
    let mut count = 1usize;

    loop {
        if count >= MAX_LIST_LEN {
            // Truncate
            let mut arr: Vec<serde_json::Value> = elems
                .iter()
                .map(|e| value_to_json(e, table, depth))
                .collect();
            arr.push(json!("..."));
            return json!(arr);
        }
        match current {
            Value::Con(id, fields) => {
                let name = con_name(*id, table);
                match (name, fields.as_slice()) {
                    ("[]", []) => break,
                    (":", [h, t]) => {
                        elems.push(h);
                        current = t;
                        count += 1;
                    }
                    _ => {
                        // Malformed list tail
                        let mut arr: Vec<serde_json::Value> = elems
                            .iter()
                            .map(|e| value_to_json(e, table, depth))
                            .collect();
                        arr.push(value_to_json(current, table, depth));
                        return json!(arr);
                    }
                }
            }
            _ => {
                // Non-constructor tail (thunk, etc)
                let mut arr: Vec<serde_json::Value> = elems
                    .iter()
                    .map(|e| value_to_json(e, table, depth))
                    .collect();
                arr.push(value_to_json(current, table, depth));
                return json!(arr);
            }
        }
    }

    // Check if all elements are chars → render as string.
    // Chars can appear as:
    //   1. LitChar(c) — bare char literal
    //   2. Con(C#, [LitChar(c)]) — boxed char
    //   3. Con(C#, [Con(Text, [ByteArray(1 byte), 0, 1])]) — char as single-byte Text
    let mut all_chars = true;
    let mut char_buf = String::new();
    for e in &elems {
        if let Some(c) = extract_char(e, table) {
            char_buf.push(c);
        } else {
            all_chars = false;
            break;
        }
    }

    if all_chars && !char_buf.is_empty() {
        json!(char_buf)
    } else {
        let arr: Vec<serde_json::Value> = elems
            .iter()
            .map(|e| value_to_json(e, table, depth))
            .collect();
        json!(arr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tidepool_repr::datacon::DataCon;
    use tidepool_repr::types::DataConId;

    fn test_table() -> DataConTable {
        let mut t = DataConTable::new();
        let cons = [
            (0, "Nothing", 0),
            (1, "Just", 1),
            (2, "True", 0),
            (3, "False", 0),
            (4, "()", 0),
            (5, "I#", 1),
            (6, "C#", 1),
            (7, ":", 2),
            (8, "[]", 0),
            (9, "Text", 3),
            (10, "(,)", 2),
            (11, "(,,)", 3),
            (12, "ByteArray", 1),
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

    #[test]
    fn test_render_lit_int() {
        let table = test_table();
        let val = Value::Lit(Literal::LitInt(42));
        assert_eq!(value_to_json(&val, &table, 0), json!(42));
    }

    #[test]
    fn test_render_lit_string() {
        let table = test_table();
        let val = Value::Lit(Literal::LitString(b"hello".to_vec()));
        assert_eq!(value_to_json(&val, &table, 0), json!("hello"));
    }

    #[test]
    fn test_render_bool() {
        let table = test_table();
        let true_val = Value::Con(table.get_by_name("True").unwrap(), vec![]);
        let false_val = Value::Con(table.get_by_name("False").unwrap(), vec![]);
        assert_eq!(value_to_json(&true_val, &table, 0), json!(true));
        assert_eq!(value_to_json(&false_val, &table, 0), json!(false));
    }

    #[test]
    fn test_render_option() {
        let table = test_table();
        let nothing = Value::Con(table.get_by_name("Nothing").unwrap(), vec![]);
        let just = Value::Con(
            table.get_by_name("Just").unwrap(),
            vec![Value::Lit(Literal::LitInt(42))],
        );
        assert_eq!(value_to_json(&nothing, &table, 0), json!(null));
        assert_eq!(value_to_json(&just, &table, 0), json!(42));
    }

    #[test]
    fn test_render_unit() {
        let table = test_table();
        let unit = Value::Con(table.get_by_name("()").unwrap(), vec![]);
        assert_eq!(value_to_json(&unit, &table, 0), json!(null));
    }

    #[test]
    fn test_render_list_int() {
        let table = test_table();
        let nil_id = table.get_by_name("[]").unwrap();
        let cons_id = table.get_by_name(":").unwrap();

        // [1, 2]
        let list = Value::Con(
            cons_id,
            vec![
                Value::Lit(Literal::LitInt(1)),
                Value::Con(
                    cons_id,
                    vec![Value::Lit(Literal::LitInt(2)), Value::Con(nil_id, vec![])],
                ),
            ],
        );
        assert_eq!(value_to_json(&list, &table, 0), json!([1, 2]));
    }

    #[test]
    fn test_render_text() {
        let table = test_table();
        let text_id = table.get_by_name("Text").unwrap();
        let ba = Value::ByteArray(Arc::new(Mutex::new(b"hello".to_vec())));
        let val = Value::Con(
            text_id,
            vec![
                ba,
                Value::Lit(Literal::LitInt(0)),
                Value::Lit(Literal::LitInt(5)),
            ],
        );
        assert_eq!(value_to_json(&val, &table, 0), json!("hello"));
    }

    #[test]
    fn test_render_list_string() {
        let table = test_table();
        let nil_id = table.get_by_name("[]").unwrap();
        let cons_id = table.get_by_name(":").unwrap();

        // ["a", "b"]
        let list = Value::Con(
            cons_id,
            vec![
                Value::Lit(Literal::LitString(b"a".to_vec())),
                Value::Con(
                    cons_id,
                    vec![
                        Value::Lit(Literal::LitString(b"b".to_vec())),
                        Value::Con(nil_id, vec![]),
                    ],
                ),
            ],
        );
        assert_eq!(value_to_json(&list, &table, 0), json!(["a", "b"]));
    }

    #[test]
    fn test_render_tuple() {
        let table = test_table();
        let pair_id = table.get_by_name("(,)").unwrap();
        let triple_id = table.get_by_name("(,,)").unwrap();

        let pair = Value::Con(
            pair_id,
            vec![
                Value::Lit(Literal::LitInt(1)),
                Value::Lit(Literal::LitInt(2)),
            ],
        );
        let triple = Value::Con(
            triple_id,
            vec![
                Value::Lit(Literal::LitInt(1)),
                Value::Lit(Literal::LitInt(2)),
                Value::Lit(Literal::LitInt(3)),
            ],
        );

        assert_eq!(value_to_json(&pair, &table, 0), json!([1, 2]));
        assert_eq!(value_to_json(&triple, &table, 0), json!([1, 2, 3]));
    }

    #[test]
    fn test_render_char_list_as_string() {
        let table = test_table();
        let nil_id = table.get_by_name("[]").unwrap();
        let cons_id = table.get_by_name(":").unwrap();
        let c_hash_id = table.get_by_name("C#").unwrap();

        // ['h', 'i']
        let list = Value::Con(
            cons_id,
            vec![
                Value::Con(c_hash_id, vec![Value::Lit(Literal::LitChar('h'))]),
                Value::Con(
                    cons_id,
                    vec![
                        Value::Con(c_hash_id, vec![Value::Lit(Literal::LitChar('i'))]),
                        Value::Con(nil_id, vec![]),
                    ],
                ),
            ],
        );
        assert_eq!(value_to_json(&list, &table, 0), json!("hi"));
    }
}
