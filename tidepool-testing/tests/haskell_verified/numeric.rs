//! Verified-generator templates for Int arithmetic, comparison, and list reductions.
//!
//! Stub: filled in by the `numeric` leaf. Follows the patterns in
//! `fmap.rs` / `text.rs` / `cousins.rs`. ASCII-only, Int-only, pinned types,
//! `ProptestConfig::with_cases(50)` per template.

#![allow(unused_imports)]

use crate::{arb_int, run_template};
use proptest::prelude::*;
use serde_json::json;

fn hs_div(a: i64, b: i64) -> i64 {
    let d = a / b;
    let r = a % b;
    if r != 0 && ((a < 0) ^ (b < 0)) {
        d - 1
    } else {
        d
    }
}

fn hs_mod(a: i64, b: i64) -> i64 {
    let r = a % b;
    if r != 0 && ((a < 0) ^ (b < 0)) {
        r + b
    } else {
        r
    }
}

// Template 1 (numeric-binop): +, -, *, `div`, `mod`
fn gen_numeric_binop() -> impl Strategy<Value = (String, serde_json::Value)> {
    let ops = prop_oneof![
        Just("+"),
        Just("-"),
        Just("*"),
        Just("`div`"),
        Just("`mod`")
    ];
    (arb_int(), arb_int(), ops)
        .prop_filter("div/mod by zero", |&(_, b, op)| {
            b != 0 || (op != "`div`" && op != "`mod`")
        })
        .prop_map(|(a, b, op)| {
            let src = format!("(({}) {} ({})) :: Int", a, op, b);
            let expected = match op {
                "+" => a.wrapping_add(b),
                "-" => a.wrapping_sub(b),
                "*" => a.wrapping_mul(b),
                "`div`" => hs_div(a, b),
                "`mod`" => hs_mod(a, b),
                _ => unreachable!(),
            };
            (src, json!(expected))
        })
}

#[test]
fn test_numeric_binop() {
    run_template(50, gen_numeric_binop());
}

// Template 2 (numeric-unary): abs', signum'
fn gen_numeric_unary() -> impl Strategy<Value = (String, serde_json::Value)> {
    let ops = prop_oneof![Just("abs'"), Just("signum'")];
    (arb_int(), ops).prop_map(|(n, op)| {
        let src = format!("{} ({}) :: Int", op, n);
        let expected = match op {
            "abs'" => n.abs(),
            "signum'" => n.signum(),
            _ => unreachable!(),
        };
        (src, json!(expected))
    })
}

#[test]
fn test_numeric_unary() {
    run_template(50, gen_numeric_unary());
}

// Template 3 (numeric-cmp): ==, /=, <, >, <=, >=
fn gen_numeric_cmp() -> impl Strategy<Value = (String, serde_json::Value)> {
    let ops = prop_oneof![
        Just("=="),
        Just("/="),
        Just("<"),
        Just(">"),
        Just("<="),
        Just(">=")
    ];
    (arb_int(), arb_int(), ops).prop_map(|(a, b, op)| {
        let src = format!("(({}) {} ({})) :: Bool", a, op, b);
        let expected = match op {
            "==" => a == b,
            "/=" => a != b,
            "<" => a < b,
            ">" => a > b,
            "<=" => a <= b,
            ">=" => a >= b,
            _ => unreachable!(),
        };
        (src, json!(expected))
    })
}

#[test]
fn test_numeric_cmp() {
    run_template(50, gen_numeric_cmp());
}

// Template 4 (numeric-minmax): min', max'
fn gen_numeric_minmax() -> impl Strategy<Value = (String, serde_json::Value)> {
    let ops = prop_oneof![Just("min'"), Just("max'")];
    (arb_int(), arb_int(), ops).prop_map(|(a, b, op)| {
        let src = format!("{} ({}) ({}) :: Int", op, a, b);
        let expected = match op {
            "min'" => a.min(b),
            "max'" => a.max(b),
            _ => unreachable!(),
        };
        (src, json!(expected))
    })
}

#[test]
fn test_numeric_minmax() {
    run_template(50, gen_numeric_minmax());
}

// Template 5 (numeric-list-reduction): sum, product
fn gen_numeric_list_reduction() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        proptest::collection::vec(arb_int(), 0..=8).prop_map(|xs| {
            let xs_str = xs
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let src = format!("sum [{}] :: Int", xs_str);
            let expected: i64 = xs.iter().sum();
            (src, json!(expected))
        }),
        proptest::collection::vec(arb_int(), 0..=5).prop_map(|xs| {
            let xs_str = xs
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let src = format!("product [{}] :: Int", xs_str);
            let expected: i64 = xs.iter().product();
            (src, json!(expected))
        })
    ]
}

#[test]
fn test_numeric_list_reduction() {
    run_template(50, gen_numeric_list_reduction());
}
