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
        value_to_json(&self.value, &self.table, 0)
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

fn con_name(id: DataConId, table: &DataConTable) -> &str {
    table.name_of(id).unwrap_or("<unknown>")
}

fn value_to_json(val: &Value, table: &DataConTable, depth: usize) -> serde_json::Value {
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
                    let raw_ba = match ba_val {
                        Value::ByteArray(bs) => Some(bs.clone()),
                        Value::Con(id, fields)
                            if con_name(*id, table) == "ByteArray" && fields.len() == 1 =>
                        {
                            if let Value::ByteArray(bs) = &fields[0] {
                                Some(bs.clone())
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(bs) = raw_ba {
                        let borrowed = bs.lock().unwrap();
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
                        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
                        _ => 0.0,
                    };
                    let e = match value_to_json(exp_val, table, d) {
                        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
                        _ => 0,
                    };
                    let val = c * 10f64.powi(e as i32);
                    json!(val)
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
            let borrowed = bs.lock().unwrap();
            match std::str::from_utf8(&borrowed) {
                Ok(s) => json!(s),
                Err(_) => json!(format!("<ByteArray# len={}>", borrowed.len())),
            }
        }
    }
}

/// Extract an i64 from a potentially boxed Int value (LitInt or I#(LitInt)).
fn extract_boxed_int(val: &Value, table: &DataConTable) -> Option<i64> {
    match val {
        Value::Lit(Literal::LitInt(n)) => Some(*n),
        Value::Con(id, fields) if fields.len() == 1 => {
            if table.get_by_name("I#") == Some(*id) {
                if let Value::Lit(Literal::LitInt(n)) = &fields[0] {
                    return Some(*n);
                }
            }
            None
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
            serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or(json!(null))
        }
        Literal::LitDouble(bits) => {
            let f = f64::from_bits(*bits);
            serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or(json!(null))
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

    // Check if all elements are chars → render as string
    let mut all_chars = true;
    let mut char_buf = String::new();
    for e in &elems {
        match e {
            Value::Lit(Literal::LitChar(c)) => char_buf.push(*c),
            Value::Con(id, fields) if con_name(*id, table) == "C#" && fields.len() == 1 => {
                if let Value::Lit(Literal::LitChar(c)) = &fields[0] {
                    char_buf.push(*c);
                } else {
                    all_chars = false;
                    break;
                }
            }
            _ => {
                all_chars = false;
                break;
            }
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
