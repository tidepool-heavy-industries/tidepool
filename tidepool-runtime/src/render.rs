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
                ("I#", [x]) | ("W#", [x]) | ("D#", [x]) | ("F#", [x]) => {
                    value_to_json(x, table, d)
                }
                ("C#", [x]) => value_to_json(x, table, d),

                // List: try to collect as array or string
                ("[]", []) => {
                    // Empty list
                    json!([])
                }
                (":", [head, tail]) => {
                    collect_list(head, tail, table, d)
                }

                // Tuples: (,), (,,), (,,,), etc.
                (n, fields) if n.starts_with('(') && n.ends_with(')') && n.chars().all(|c| c == '(' || c == ')' || c == ',') && fields.len() >= 2 => {
                    let elems: Vec<serde_json::Value> = fields.iter().map(|f| value_to_json(f, table, d)).collect();
                    json!(elems)
                }

                // Generic constructor
                (_name, fields) => {
                    if fields.is_empty() {
                        json!(name)
                    } else {
                        let field_jsons: Vec<serde_json::Value> = fields.iter().map(|f| value_to_json(f, table, d)).collect();
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
    }
}

fn literal_to_json(lit: &Literal) -> serde_json::Value {
    match lit {
        Literal::LitInt(n) => json!(n),
        Literal::LitWord(n) => json!(n),
        Literal::LitChar(c) => json!(c.to_string()),
        Literal::LitString(bytes) => {
            match std::str::from_utf8(bytes) {
                Ok(s) => json!(s),
                Err(_) => json!(format!("<binary:{} bytes>", bytes.len())),
            }
        }
        Literal::LitFloat(bits) => {
            let f = f32::from_bits(*bits as u32) as f64;
            serde_json::Number::from_f64(f).map(serde_json::Value::Number).unwrap_or(json!(null))
        }
        Literal::LitDouble(bits) => {
            let f = f64::from_bits(*bits);
            serde_json::Number::from_f64(f).map(serde_json::Value::Number).unwrap_or(json!(null))
        }
    }
}

/// Collect a cons-chain into a JSON array, or a JSON string if all elements are chars.
fn collect_list(head: &Value, tail: &Value, table: &DataConTable, depth: usize) -> serde_json::Value {
    let mut elems = vec![head];
    let mut current = tail;
    let mut count = 1usize;

    loop {
        if count >= MAX_LIST_LEN {
            // Truncate
            let mut arr: Vec<serde_json::Value> = elems.iter().map(|e| value_to_json(e, table, depth)).collect();
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
                        let mut arr: Vec<serde_json::Value> = elems.iter().map(|e| value_to_json(e, table, depth)).collect();
                        arr.push(value_to_json(current, table, depth));
                        return json!(arr);
                    }
                }
            }
            _ => {
                // Non-constructor tail (thunk, etc)
                let mut arr: Vec<serde_json::Value> = elems.iter().map(|e| value_to_json(e, table, depth)).collect();
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
        let arr: Vec<serde_json::Value> = elems.iter().map(|e| value_to_json(e, table, depth)).collect();
        json!(arr)
    }
}
