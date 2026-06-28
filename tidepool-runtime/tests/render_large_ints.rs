mod common;

use tidepool_runtime::compile_and_run_pure;

fn prelude_path() -> std::path::PathBuf {
    common::prelude_path()
}

fn run(src: &str, target: &str) -> tidepool_runtime::EvalResult {
    let pp = prelude_path();
    let src = src.to_owned();
    let target = target.to_owned();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            compile_and_run_pure(&src, &target, &include).unwrap()
        })
        .unwrap()
        .join()
        .unwrap()
}

/// Int > 2^53: must render as exact i64, not f64-approximated.
/// Regression for BUG-8: `pure (9007199254740993 :: Int)` returned 9007199254740992.
#[test]
fn test_large_int_renders_exact() {
    let src = "module Test where\n\
               largeInt :: Int\n\
               largeInt = 9007199254740993\n";
    let result = run(src, "largeInt");
    let j = result.to_json();
    // The rendered string must be the exact 17-digit number, not 9007199254740992.
    assert_eq!(
        j.to_string(),
        "9007199254740993",
        "Int > 2^53 must render exactly (got {:?})",
        j
    );
}

/// Int at exactly 2^53: boundary case — must also be exact.
#[test]
fn test_int_at_2pow53_exact() {
    let src = "module Test where\n\
               boundary :: Int\n\
               boundary = 9007199254740992\n";
    let result = run(src, "boundary");
    let j = result.to_json();
    assert_eq!(j.to_string(), "9007199254740992");
}

/// Big Integer literal: IP BigNat# must render as exact decimal string.
/// Tests the IP-constructor rendering path in render.rs.
#[test]
fn test_big_integer_literal_renders_exact() {
    // 25! = 15511210043330985984000000
    let src = "module Test where\n\
               bigLit :: Integer\n\
               bigLit = 15511210043330985984000000\n";
    let result = run(src, "bigLit");
    assert_eq!(
        result.to_json(),
        serde_json::json!("15511210043330985984000000"),
        "big Integer literal must render as exact decimal string"
    );
}

/// Computed big Integer: product [1..25] :: Integer.
/// This exercises the full Integer arithmetic path in the JIT.
#[test]
fn test_big_integer_product_renders_exact() {
    let src = "module Test where\n\
               bigProduct :: Integer\n\
               bigProduct = product [1..25]\n";
    let result = run(src, "bigProduct");
    // 25! = 15511210043330985984000000
    assert_eq!(
        result.to_json(),
        serde_json::json!("15511210043330985984000000"),
        "product [1..25] :: Integer must render as exact decimal (got {:?})",
        result.to_json()
    );
}
