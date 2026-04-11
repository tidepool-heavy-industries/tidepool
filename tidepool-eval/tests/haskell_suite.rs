//! End-to-end tests: Haskell source → GHC → Translate → CBOR → Rust deser → eval
//!
//! Each test loads a pre-compiled CBOR fixture from haskell/test/suite_cbor/,
//! deserializes it, evaluates it, and asserts the expected result.
//!
//! GHC at -O2 wraps all values in boxing constructors (I# for Int, C# for Char,
//! D# for Double). The `unbox` helper strips these automatically.

// Suite.hs has a literal `lit_double_pi = 3.14159` that tests a specific float
// literal flows through the pipeline correctly. Not a math constant mistake.
#![allow(clippy::approx_constant)]

use tidepool_eval::{deep_force, env_from_datacon_table, eval, Value, VecHeap};
use tidepool_repr::serial::read::{read_cbor, read_metadata};
use tidepool_repr::{DataConTable, Literal};

static META: &[u8] = include_bytes!("../../haskell/test/suite_cbor/meta.cbor");

fn table() -> DataConTable {
    read_metadata(META).unwrap().0
}

fn eval_fixture(cbor: &[u8]) -> Value {
    let expr = read_cbor(cbor).unwrap();
    let table = table();
    let env = env_from_datacon_table(&table);
    let mut heap = VecHeap::new();
    let val = eval(&expr, &env, &mut heap).unwrap();
    deep_force(val, &mut heap).unwrap()
}

/// Unwrap GHC boxing: I# x → x, C# x → x, D# x → x, W# x → x.
fn unbox(val: &Value, table: &DataConTable) -> Value {
    if let Value::Con(id, fields) = val {
        if let Some(name) = table.name_of(*id) {
            if matches!(name, "I#" | "C#" | "D#" | "W#") && fields.len() == 1 {
                return fields[0].clone();
            }
        }
    }
    val.clone()
}

fn assert_int(val: &Value, expected: i64, table: &DataConTable) {
    let inner = unbox(val, table);
    match inner {
        Value::Lit(Literal::LitInt(n)) => {
            assert_eq!(n, expected, "expected Int {expected}, got {n}")
        }
        ref other => panic!("expected Int {expected}, got {other:?}"),
    }
}

fn assert_bool(val: &Value, expected: bool, table: &DataConTable) {
    if let Value::Con(id, fields) = val {
        assert!(fields.is_empty(), "Bool should be nullary, got {fields:?}");
        let name = table.name_of(*id).unwrap();
        let actual = name == "True";
        assert_eq!(actual, expected, "expected Bool {expected}, got {name}");
        return;
    }
    panic!("expected Bool, got {val:?}");
}

fn assert_char(val: &Value, expected: char, table: &DataConTable) {
    let inner = unbox(val, table);
    match inner {
        Value::Lit(Literal::LitChar(c)) => {
            assert_eq!(c, expected, "expected Char {expected:?}, got {c:?}")
        }
        ref other => panic!("expected Char {expected:?}, got {other:?}"),
    }
}

fn assert_double(val: &Value, expected: f64, table: &DataConTable) {
    let inner = unbox(val, table);
    match inner {
        Value::Lit(Literal::LitDouble(bits)) => {
            let actual = f64::from_bits(bits);
            assert!(
                (actual - expected).abs() < 1e-10,
                "expected Double {expected}, got {actual}"
            );
        }
        ref other => panic!("expected Double {expected}, got {other:?}"),
    }
}

/// Unwrap a Maybe value.
fn unwrap_maybe(val: &Value, table: &DataConTable) -> Option<Value> {
    if let Value::Con(id, fields) = val {
        let name = table.name_of(*id).unwrap();
        match name {
            "Nothing" => {
                assert!(fields.is_empty());
                return None;
            }
            "Just" => {
                assert_eq!(fields.len(), 1);
                return Some(fields[0].clone());
            }
            _ => {}
        }
    }
    panic!("expected Maybe, got {val:?}");
}

/// Unwrap an Either value.
fn unwrap_either(val: &Value, table: &DataConTable) -> Result<Value, Value> {
    if let Value::Con(id, fields) = val {
        let name = table.name_of(*id).unwrap();
        match name {
            "Left" => {
                assert_eq!(fields.len(), 1);
                return Err(fields[0].clone());
            }
            "Right" => {
                assert_eq!(fields.len(), 1);
                return Ok(fields[0].clone());
            }
            _ => {}
        }
    }
    panic!("expected Either, got {val:?}");
}

/// Unwrap a tuple constructor.
fn unwrap_tuple(val: &Value) -> &[Value] {
    if let Value::Con(_, fields) = val {
        return fields;
    }
    panic!("expected tuple, got {val:?}");
}

/// Collect a Haskell String (list of Char) into a Rust String.
fn collect_string(val: &Value, table: &DataConTable) -> String {
    let mut result = String::new();
    let mut cur = val;
    loop {
        match cur {
            Value::Con(id, fields) => {
                let name = table.name_of(*id).unwrap();
                if name == "[]" {
                    break;
                } else if name == ":" {
                    assert_eq!(fields.len(), 2, "(:) should have 2 fields");
                    let ch = unbox(&fields[0], table);
                    if let Value::Lit(Literal::LitChar(c)) = ch {
                        result.push(c);
                    } else {
                        panic!("expected Char in string cons, got {ch:?}");
                    }
                    cur = &fields[1];
                } else {
                    panic!("expected [] or (:), got {name}");
                }
            }
            other => panic!("expected string cons cell, got {other:?}"),
        }
    }
    result
}

/// Collect a Haskell List of Int into a Vec<i64>.
fn collect_int_list(val: &Value, table: &DataConTable) -> Vec<i64> {
    let mut result = Vec::new();
    let mut cur = val;
    loop {
        match cur {
            Value::Con(id, fields) => {
                let name = table.name_of(*id).expect("DataCon ID not found");
                if name == "[]" {
                    break;
                } else if name == ":" {
                    assert_eq!(fields.len(), 2, "(:) should have 2 fields");
                    let inner = unbox(&fields[0], table);
                    if let Value::Lit(Literal::LitInt(n)) = inner {
                        result.push(n);
                    } else {
                        panic!("expected Int in list, got {inner:?}");
                    }
                    cur = &fields[1];
                } else {
                    panic!("expected [] or (:), got {name}");
                }
            }
            other => panic!("expected list cons cell, got {other:?}"),
        }
    }
    result
}

// =============================================================================
// Macros
// =============================================================================

macro_rules! suite_int {
    ($name:ident, $expected:expr) => {
        #[test]
        fn $name() {
            static CBOR: &[u8] = include_bytes!(concat!(
                "../../haskell/test/suite_cbor/",
                stringify!($name),
                ".cbor"
            ));
            let val = eval_fixture(CBOR);
            let table = table();
            assert_int(&val, $expected, &table);
        }
    };
}

macro_rules! suite_int_list {
    ($name:ident, $expected:expr) => {
        #[test]
        fn $name() {
            static CBOR: &[u8] = include_bytes!(concat!(
                "../../haskell/test/suite_cbor/",
                stringify!($name),
                ".cbor"
            ));
            let val = eval_fixture(CBOR);
            let table = table();
            let actual = collect_int_list(&val, &table);
            assert_eq!(actual, $expected);
        }
    };
}

macro_rules! suite_bool {
    ($name:ident, $expected:expr) => {
        #[test]
        fn $name() {
            static CBOR: &[u8] = include_bytes!(concat!(
                "../../haskell/test/suite_cbor/",
                stringify!($name),
                ".cbor"
            ));
            let val = eval_fixture(CBOR);
            let table = table();
            assert_bool(&val, $expected, &table);
        }
    };
}

macro_rules! suite_char {
    ($name:ident, $expected:expr) => {
        #[test]
        fn $name() {
            static CBOR: &[u8] = include_bytes!(concat!(
                "../../haskell/test/suite_cbor/",
                stringify!($name),
                ".cbor"
            ));
            let val = eval_fixture(CBOR);
            let table = table();
            assert_char(&val, $expected, &table);
        }
    };
}

macro_rules! suite_double {
    ($name:ident, $expected:expr) => {
        #[test]
        fn $name() {
            static CBOR: &[u8] = include_bytes!(concat!(
                "../../haskell/test/suite_cbor/",
                stringify!($name),
                ".cbor"
            ));
            let val = eval_fixture(CBOR);
            let table = table();
            assert_double(&val, $expected, &table);
        }
    };
}

// =============================================================================
// Int literals (5)
// =============================================================================

suite_int!(lit_42, 42);
suite_int!(lit_zero, 0);
suite_int!(lit_neg7, -7);
suite_int!(lit_large, 1_000_000);
suite_int!(lit_neg_large, -999_999);

// =============================================================================
// Other literals (5)
// =============================================================================

suite_char!(lit_char_a, 'a');
suite_char!(lit_char_z, 'z');
suite_char!(lit_char_newline, '\n');
suite_double!(lit_double_pi, 3.14159);
suite_double!(lit_double_neg, -2.5);

// =============================================================================
// Arithmetic (12)
// =============================================================================

suite_int!(add_simple, 3);
suite_int!(sub_simple, 7);
suite_int!(mul_simple, 42);
suite_int!(nested_arith, 21);
suite_int!(arith_precedence, 14);
suite_int!(arith_left_assoc, 5);
suite_int!(arith_neg_result, -7);
suite_int!(arith_mul_zero, 0);
suite_int!(arith_mul_one, 42);
suite_double!(arith_double_add, 4.0);
suite_double!(arith_double_mul, 6.0);
suite_double!(arith_double_sub, 6.5);
suite_int!(prim_quot_rem_int, 31);
suite_int!(prim_quot_rem_word, 31);

// =============================================================================
// Comparisons (8)
// =============================================================================

suite_bool!(cmp_eq_true, true);
suite_bool!(cmp_eq_false, false);
suite_bool!(cmp_ne_true, true);
suite_bool!(cmp_lt_true, true);
suite_bool!(cmp_lt_false, false);
suite_bool!(cmp_gt_true, true);
suite_bool!(cmp_le_eq, true);
suite_bool!(cmp_ge_eq, true);

// =============================================================================
// Let bindings (8)
// =============================================================================

suite_int!(let_simple, 10);
suite_int!(let_two, 30);
suite_int!(let_nested, 12);
suite_int!(let_shadow, 20);
suite_int!(let_unused, 99);
suite_int!(let_chain, 3);
suite_int!(let_complex, 26);
suite_int!(let_body_only, 42);

// =============================================================================
// LetRec (8)
// =============================================================================

suite_int!(letrec_fact5, 120);
suite_int!(letrec_fib10, 55);
suite_int!(letrec_countdown, 0);
suite_int!(letrec_sum_to, 55);
suite_int!(letrec_pow, 1024);
suite_int!(letrec_gcd, 6);
suite_bool!(letrec_even_odd, true);
suite_int!(letrec_ackermann, 9);

// =============================================================================
// Case / pattern match (15)
// =============================================================================

suite_int!(case_just, 42);
suite_int!(case_nothing, 0);
suite_int!(case_true, 1);
suite_int!(case_false, 0);
suite_int!(case_left, 10);
suite_int!(case_right, 20);
suite_int!(case_nested_just, 99);
suite_int!(case_pair, 30);
suite_int!(case_triple, 6);
suite_int!(case_default, 99);
suite_bool!(case_bool_and, true);
suite_bool!(case_bool_or, true);
suite_int!(case_nested_case, 1);
suite_int!(case_either_nested, 7);
suite_int!(case_wildcard_pair, 20);

// =============================================================================
// Data constructors (10)
// =============================================================================

#[test]
fn con_just() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/con_just.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let inner = unwrap_maybe(&val, &table).expect("expected Just");
    assert_int(&inner, 42, &table);
}

#[test]
fn con_nothing() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/con_nothing.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    assert!(unwrap_maybe(&val, &table).is_none());
}

#[test]
fn con_pair() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/con_pair.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let fields = unwrap_tuple(&val);
    assert_eq!(fields.len(), 2);
    assert_int(&fields[0], 10, &table);
    assert_int(&fields[1], 20, &table);
}

#[test]
fn con_triple() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/con_triple.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let fields = unwrap_tuple(&val);
    assert_eq!(fields.len(), 3);
    assert_int(&fields[0], 1, &table);
    assert_int(&fields[1], 2, &table);
    assert_int(&fields[2], 3, &table);
}

#[test]
fn con_left() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/con_left.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let inner = unwrap_either(&val, &table).unwrap_err();
    assert_int(&inner, 10, &table);
}

#[test]
fn con_right() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/con_right.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let inner = unwrap_either(&val, &table).unwrap();
    assert_int(&inner, 20, &table);
}

#[test]
fn con_nested_just() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/con_nested_just.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let outer = unwrap_maybe(&val, &table).expect("expected Just");
    let inner = unwrap_maybe(&outer, &table).expect("expected Just");
    assert_int(&inner, 99, &table);
}

#[test]
fn con_nested_nothing() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/con_nested_nothing.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let outer = unwrap_maybe(&val, &table).expect("expected Just");
    assert!(unwrap_maybe(&outer, &table).is_none());
}

suite_bool!(con_true, true);
suite_bool!(con_false, false);

// =============================================================================
// Lambda / application (8)
// =============================================================================

suite_int!(app_identity, 42);
suite_int!(app_const, 10);
suite_int!(app_compose, 11);
suite_int!(app_nested_lam, 7);
suite_int!(app_thrice, 3);
suite_int!(app_twice, 16);
suite_int!(app_church_zero, 0);
suite_int!(app_church_two, 2);

// =============================================================================
// Higher-order (8)
// =============================================================================

suite_int!(ho_myfoldr, 15);
suite_int!(ho_myfoldl, 15);
suite_int!(ho_mymap_len, 3);
suite_int!(ho_myfilter_len, 3);
suite_bool!(ho_myany, true);
suite_bool!(ho_myall, false);
suite_int!(ho_myzipwith, 11);
suite_int!(ho_myconcatmap, 6);

// =============================================================================
// If-then-else / guards (5)
// =============================================================================

suite_int!(ite_simple, 1);
suite_int!(ite_false, 0);
suite_int!(ite_nested, 2);
suite_int!(ite_abs, 5);
suite_int!(ite_signum, -1);

// =============================================================================
// Edge cases (8)
// =============================================================================

suite_int!(edge_deep_let, 10);
suite_int!(edge_large_tuple, 15);
suite_bool!(edge_nullary_con, true);
suite_int!(edge_id_chain, 42);
suite_int!(edge_const_chain, 42);
suite_int!(edge_case_of_case, 1);
suite_int!(edge_deep_nesting, 6);
suite_int!(edge_mutual_data, 42);

// =============================================================================
// Prelude functions (closure resolution) (10)
// =============================================================================

suite_bool!(prelude_null_empty, true);
suite_bool!(prelude_null_nonempty, false);
suite_int!(prelude_length, 5);
suite_int!(prelude_take, 3);
suite_int!(prelude_map, 3);
suite_int!(prelude_filter, 3);
suite_bool!(prelude_or, true);
suite_bool!(prelude_and, false);
suite_bool!(prelude_eq_int, true);
suite_int!(prelude_string_append, 11);

suite_int!(prelude_take_cons, 3);
suite_bool!(prelude_eq_string_true, true);
suite_bool!(prelude_eq_string_false, false);

// =============================================================================
// Show (7)
// =============================================================================

#[test]
fn show_int() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showInt.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let s = collect_string(&val, &table);
    assert_eq!(s, "42", "expected \"42\", got \"{s}\"");
}

#[test]
fn show_int_neg() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showIntNeg.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let s = collect_string(&val, &table);
    assert_eq!(s, "-7", "expected \"-7\", got \"{s}\"");
}

#[test]
fn show_char_a() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showCharA.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let s = collect_string(&val, &table);
    assert_eq!(s, "'a'", "expected \"'a'\", got \"{s}\"");
}

#[test]
fn show_hello() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showHello.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let s = collect_string(&val, &table);
    assert_eq!(s, "\"hello\"", "expected \"\\\"hello\\\"\", got \"{s}\"");
}

#[test]
fn show_maybe_int() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showMaybeInt.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let s = collect_string(&val, &table);
    assert_eq!(s, "Just 42", "expected \"Just 42\", got \"{s}\"");
}

#[test]
fn show_maybe_nothing() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showMaybeNothing.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let s = collect_string(&val, &table);
    assert_eq!(s, "Nothing", "expected \"Nothing\", got \"{s}\"");
}

#[test]
fn show_bool() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showBool.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let s = collect_string(&val, &table);
    assert_eq!(s, "True", "expected \"True\", got \"{s}\"");
}

#[test]
fn show_double() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showDouble.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let s = collect_string(&val, &table);
    assert_eq!(s, "3.14", "expected \"3.14\", got \"{s}\"");
}

#[test]
fn show_double_int() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showDoubleInt.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let s = collect_string(&val, &table);
    assert_eq!(s, "42.0", "expected \"42.0\", got \"{s}\"");
}

#[test]
fn show_double_text() {
    // T.pack (showDouble' 3.14) — produces Text, not String
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showDoubleText.cbor");
    let val = eval_fixture(CBOR);
    // Text is Con("Text", [ByteArray#, Int#, Int#]) — just check it doesn't crash
    eprintln!("show_double_text result: {:?}", val);
}

#[test]
fn show_double_prelude() {
    // show (3.14 :: Double) using Prelude's show (not showDouble')
    // GHC specializes this to $fShowDouble_$sshowSignedFloat — our intercept rewrites it
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showDoublePrelude.cbor");
    let val = eval_fixture(CBOR);
    let table = table();
    let s = collect_string(&val, &table);
    assert_eq!(s, "3.14");
}

#[test]
fn show_double_prelude_text() {
    // T.pack (show (3.14 :: Double)) — the MCP use case
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/showDoublePreludeText.cbor");
    let val = eval_fixture(CBOR);
    eprintln!("show_double_prelude_text result: {:?}", val);
}

// =============================================================================
// Lazy thunk tests (8)
// =============================================================================

suite_int_list!(thunk_repeat, vec![1, 1, 1, 1, 1]);
suite_int_list!(thunk_iterate, vec![0, 1, 2, 3, 4]);
suite_int_list!(thunk_cycle, vec![1, 2, 3, 1, 2, 3, 1]);
suite_int_list!(thunk_zipwith, vec![10, 21, 32]);
suite_int_list!(thunk_zipwith_inf, vec![100, 102, 104, 106]);
suite_int_list!(thunk_map_inf, vec![0, 2, 4, 6, 8]);
suite_int_list!(thunk_letrec_knot, vec![1, 1, 1, 1, 1]);

#[test]
fn thunk_blackhole() {
    static CBOR: &[u8] = include_bytes!("../../haskell/test/suite_cbor/thunk_blackhole.cbor");
    let expr = read_cbor(CBOR).unwrap();
    let table = table();
    let env = env_from_datacon_table(&table);
    let mut heap = VecHeap::new();
    let res = eval(&expr, &env, &mut heap);

    // Evaluation might hit BlackHole directly if it's not wrapped in a Con,
    // or deep_force might hit it.
    match res {
        Err(tidepool_eval::EvalError::InfiniteLoop(_)) => {}
        Ok(val) => {
            let res_force = deep_force(val, &mut heap);
            match res_force {
                Err(tidepool_eval::EvalError::InfiniteLoop(_)) => {}
                other => panic!("expected InfiniteLoop, got {other:?}"),
            }
        }
        other => panic!("expected InfiniteLoop, got {other:?}"),
    }
}
