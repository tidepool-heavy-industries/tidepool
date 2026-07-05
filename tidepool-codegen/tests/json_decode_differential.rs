//! Differential coverage for the pure `JsonDecode` primop
//! (`eitherDecodeValue :: Text -> Either Text Value`): the Cranelift JIT and the
//! tree-walker must agree on the aeson `Either Text Value` they build from a JSON
//! `Text`, for scalars, arrays, nested objects, and malformed input
//! (`Left <serde error>`).
//!
//! Both engines dispatch to the SAME Rust builder (`tidepool_eval::json`), so
//! this is really a check that the JIT's heap materialization + the eval's
//! in-place `Value` construction observe identically — rendered back to
//! canonical JSON and compared against each other AND an expected string.

use tidepool_repr::datacon::{DataCon, SrcBang};
use tidepool_repr::types::{DataConId, Literal, PrimOpKind};
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, TreeBuilder};

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, Value, VecHeap};

// Fixed, distinct ids for the aeson `Value` / `Maybe` / `Data.Map` / list /
// `Text` closure the primop constructs. Names + arities are what the primop's
// `JsonConIds::from_table` resolves against.
const LEFT: u64 = 101;
const RIGHT: u64 = 102;
const OBJECT: u64 = 103;
const ARRAY: u64 = 104;
const STRING: u64 = 105;
const NUMBER: u64 = 106;
const NUMBER_I: u64 = 107;
const BOOL: u64 = 108;
const NULL: u64 = 109;
const TRUE: u64 = 110;
const FALSE: u64 = 111;
const BIN: u64 = 112;
const TIP: u64 = 113;
const I_HASH: u64 = 114;
const TEXT: u64 = 115;
const CONS: u64 = 116;
const NIL: u64 = 117;

fn dc(id: u64, name: &str, tag: u32, arity: u32, qual: Option<&str>) -> DataCon {
    DataCon {
        id: DataConId(id),
        name: name.to_string(),
        tag,
        rep_arity: arity,
        field_bangs: vec![SrcBang::NoSrcBang; arity as usize],
        qualified_name: qual.map(|s| s.to_string()),
    }
}

/// A `DataConTable` carrying exactly the constructors `eitherDecodeValue` needs.
fn aeson_table() -> DataConTable {
    let mut t = DataConTable::new();
    t.insert(dc(LEFT, "Left", 1, 1, Some("Data.Either.Left")));
    t.insert(dc(RIGHT, "Right", 2, 1, Some("Data.Either.Right")));
    t.insert(dc(
        OBJECT,
        "Object",
        1,
        1,
        Some("Tidepool.Aeson.Value.Object"),
    ));
    t.insert(dc(ARRAY, "Array", 2, 1, Some("Tidepool.Aeson.Value.Array")));
    t.insert(dc(
        STRING,
        "String",
        3,
        1,
        Some("Tidepool.Aeson.Value.String"),
    ));
    t.insert(dc(
        NUMBER,
        "Number",
        4,
        1,
        Some("Tidepool.Aeson.Value.Number"),
    ));
    t.insert(dc(
        NUMBER_I,
        "NumberI",
        7,
        1,
        Some("Tidepool.Aeson.Value.NumberI"),
    ));
    t.insert(dc(BOOL, "Bool", 5, 1, Some("Tidepool.Aeson.Value.Bool")));
    t.insert(dc(NULL, "Null", 6, 0, Some("Tidepool.Aeson.Value.Null")));
    t.insert(dc(FALSE, "False", 1, 0, Some("GHC.Types.False")));
    t.insert(dc(TRUE, "True", 2, 0, Some("GHC.Types.True")));
    t.insert(dc(TIP, "Tip", 1, 0, Some("Data.Map.Tip")));
    t.insert(dc(BIN, "Bin", 2, 5, Some("Data.Map.Bin")));
    t.insert(dc(I_HASH, "I#", 1, 1, Some("GHC.Types.I#")));
    t.insert(dc(TEXT, "Text", 1, 3, Some("Data.Text.Internal.Text")));
    t.insert(dc(NIL, "[]", 1, 0, Some("GHC.Types.[]")));
    t.insert(dc(CONS, ":", 2, 2, Some("GHC.Types.:")));
    t
}

/// `eitherDecodeValue <text>` where `<text>` is a literal `Text ByteArray# Int# Int#`.
fn build_decode(input: &str) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let bytes = input.as_bytes().to_vec();
    let len = bytes.len() as i64;
    let ba = b.push(CoreFrame::Lit(Literal::LitByteArray(bytes)));
    let off = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let ln = b.push(CoreFrame::Lit(Literal::LitInt(len)));
    let text = b.push(CoreFrame::Con {
        tag: DataConId(TEXT),
        fields: vec![ba, off, ln],
    });
    let _root = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::JsonDecode,
        args: vec![text],
    });
    b.build()
}

// ----- canonical rendering of the resulting `Maybe Value` -------------------

fn lit_int(v: &Value) -> i64 {
    match v {
        Value::Lit(Literal::LitInt(n)) => *n,
        Value::Lit(Literal::LitWord(n)) => *n as i64,
        Value::Con(_, f) if f.len() == 1 => lit_int(&f[0]), // I# n
        other => panic!("expected Int, got {other:?}"),
    }
}

fn lit_double(v: &Value) -> f64 {
    match v {
        Value::Lit(Literal::LitDouble(b)) => f64::from_bits(*b),
        Value::Con(_, f) if f.len() == 1 => lit_double(&f[0]),
        other => panic!("expected Double, got {other:?}"),
    }
}

fn text_str(v: &Value) -> String {
    let fields = match v {
        Value::Con(_, f) if f.len() == 3 => f,
        other => panic!("expected Text Con, got {other:?}"),
    };
    let off = lit_int(&fields[1]).max(0) as usize;
    let len = lit_int(&fields[2]).max(0) as usize;
    let bytes: Vec<u8> = match &fields[0] {
        Value::ByteArray(a) => a.lock().unwrap().clone(),
        Value::Lit(Literal::LitByteArray(b)) | Value::Lit(Literal::LitString(b)) => b.clone(),
        other => panic!("expected Text bytes, got {other:?}"),
    };
    let end = off.saturating_add(len).min(bytes.len());
    String::from_utf8_lossy(&bytes[off.min(end)..end]).into_owned()
}

/// In-order collect of a `Data.Map` (`Bin size k v l r` / `Tip`) into sorted kv.
fn collect_map(v: &Value, table: &DataConTable, out: &mut Vec<(String, String)>) {
    match v {
        Value::Con(id, fields) => match table.name_of(*id) {
            Some("Tip") => {}
            Some("Bin") => {
                // Bin size key val left right
                collect_map(&fields[3], table, out);
                out.push((text_str(&fields[1]), render_value(&fields[2], table)));
                collect_map(&fields[4], table, out);
            }
            other => panic!("expected Bin/Tip, got {other:?}"),
        },
        other => panic!("expected Map Con, got {other:?}"),
    }
}

fn collect_list(v: &Value, table: &DataConTable, out: &mut Vec<String>) {
    match v {
        Value::Con(id, fields) => match table.name_of(*id) {
            Some("[]") => {}
            Some(":") => {
                out.push(render_value(&fields[0], table));
                collect_list(&fields[1], table, out);
            }
            other => panic!("expected list cons, got {other:?}"),
        },
        other => panic!("expected list Con, got {other:?}"),
    }
}

fn render_value(v: &Value, table: &DataConTable) -> String {
    match v {
        Value::Con(id, fields) => match table.name_of(*id) {
            Some("Object") => {
                let mut kv = Vec::new();
                collect_map(&fields[0], table, &mut kv);
                let body: Vec<String> = kv
                    .into_iter()
                    .map(|(k, val)| format!("{k:?}:{val}"))
                    .collect();
                format!("{{{}}}", body.join(","))
            }
            Some("Array") => {
                let mut items = Vec::new();
                collect_list(&fields[0], table, &mut items);
                format!("[{}]", items.join(","))
            }
            Some("String") => format!("{:?}", text_str(&fields[0])),
            Some("Number") => format!("{}", lit_double(&fields[0])),
            Some("NumberI") => format!("{}", lit_int(&fields[0])),
            Some("Bool") => match &fields[0] {
                Value::Con(bid, _) => match table.name_of(*bid) {
                    Some("True") => "true".into(),
                    Some("False") => "false".into(),
                    other => panic!("expected True/False, got {other:?}"),
                },
                other => panic!("expected Bool inner, got {other:?}"),
            },
            Some("Null") => "null".into(),
            other => panic!("unexpected Value constructor {other:?}"),
        },
        other => panic!("expected Value Con, got {other:?}"),
    }
}

/// Render the top-level `Either Text Value`: `left:<msg>` or the decoded value.
fn render_either(v: &Value, table: &DataConTable) -> String {
    match v {
        Value::Con(id, fields) => match table.name_of(*id) {
            Some("Left") => format!("left:{}", text_str(&fields[0])),
            Some("Right") => render_value(&fields[0], table),
            other => panic!("expected Either, got {other:?}"),
        },
        other => panic!("expected Either Con, got {other:?}"),
    }
}

fn eval_render(input: &str) -> String {
    let expr = build_decode(input);
    let table = aeson_table();
    let env = env_from_datacon_table(&table);
    let mut heap = VecHeap::new();
    let raw = eval(&expr, &env, &mut heap).expect("eval");
    let forced = deep_force(raw, &mut heap).expect("deep_force");
    render_either(&forced, &table)
}

fn jit_render(input: &str) -> String {
    let expr = build_decode(input);
    let table = aeson_table();
    let mut m = JitEffectMachine::compile(&expr, &table, 256 * 1024).expect("JIT compile");
    let raw = m.run_pure().expect("JIT run");
    render_either(&raw, &table)
}

fn assert_agree(input: &str, expected: &str) {
    let ev = eval_render(input);
    let jit = jit_render(input);
    assert_eq!(ev, expected, "eval-vs-expected for input {input:?}");
    assert_eq!(jit, expected, "jit-vs-expected for input {input:?}");
    assert_eq!(ev, jit, "eval/jit divergence for input {input:?}");
}

#[test]
fn scalars_agree() {
    assert_agree("42", "42");
    assert_agree("-7", "-7");
    assert_agree("3.5", "3.5");
    assert_agree("true", "true");
    assert_agree("false", "false");
    assert_agree("null", "null");
    assert_agree("\"hi\"", "\"hi\"");
    assert_agree("\"\"", "\"\"");
}

#[test]
fn arrays_agree() {
    assert_agree("[]", "[]");
    assert_agree("[1,2,3]", "[1,2,3]");
    assert_agree("[1,\"a\",true,null]", "[1,\"a\",true,null]");
    assert_agree("[[1],[2,3]]", "[[1],[2,3]]");
}

#[test]
fn objects_and_nesting_agree() {
    assert_agree("{}", "{}");
    // Keys render in sorted order (Data.Map inorder) regardless of input order.
    assert_agree("{\"b\":1,\"a\":2}", "{\"a\":2,\"b\":1}");
    assert_agree(
        "{\"name\":\"x\",\"tags\":[1,2],\"meta\":{\"ok\":true}}",
        "{\"meta\":{\"ok\":true},\"name\":\"x\",\"tags\":[1,2]}",
    );
}

/// Malformed input yields `Left <serde error>` on BOTH engines, with an
/// identical message (both share `decode_json_str`). The exact wording is
/// serde_json's, so assert the shape + cross-engine agreement, not a pinned string.
#[test]
fn malformed_is_left_on_both() {
    for input in ["{not json", "", "[1,2", "truex"] {
        let ev = eval_render(input);
        let jit = jit_render(input);
        assert!(ev.starts_with("left:"), "eval not Left for {input:?}: {ev}");
        assert!(jit.starts_with("left:"), "jit not Left for {input:?}: {jit}");
        assert_eq!(ev, jit, "eval/jit divergence for input {input:?}");
    }
}
