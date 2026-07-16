//! Regression tests for nubBy stack overflow on large lists.
//!
//! `nubBy` is O(n²) recursive (each step calls `filter` on the remaining list).
//! The JIT doesn't apply TCO to non-tail calls, so the peak stack depth is:
//!   nubBy(N) × filter(N) × comparator_depth
//!
//! With heavyweight comparators (tuple pattern matching + Text ops), the
//! per-comparison stack cost is ~15 function-call frames. At 175 items with
//! 4-tuple destructuring + Text concatenation + equality:
//!   175 × 174 × ~15 ≈ 2800 peak frames × ~12KB/frame ≈ 33MB
//! This exceeds even the MCP eval thread's 32MB stack.
//!
//! Bug manifests as either:
//!   - SIGSEGV (stack guard page hit)
//!   - tag 255 / "application of non-closure" (stack corruption before guard)
//!
//! Fixed by tail-recursive shadows of `filter`, `nubBy`, and `nub` in
//! Tidepool.Prelude (accumulator-based, all recursive calls in tail position
//! → TCO applies → no stack growth for the outer loop).

use tidepool_testing::eval_harness::prelude_path;

fn run_result(body: &str) -> Result<serde_json::Value, tidepool_runtime::RuntimeError> {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}}
module Test where
import Tidepool.Prelude

result :: Int
result = {body}
"#
    );
    let pp = prelude_path();
    let include = [pp.as_path()];
    let val = tidepool_runtime::compile_and_run_pure(&src, "result", &include)?;
    Ok(val.to_json())
}

/// Run on a thread with signal protection installed and a specific stack size.
/// Returns Ok(json), Err(runtime_error_string), or Err("thread panicked").
fn run_protected(stack_bytes: usize, body: &str) -> Result<serde_json::Value, String> {
    let body = body.to_string();
    std::thread::Builder::new()
        .stack_size(stack_bytes)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            run_result(&body)
        })
        .unwrap()
        .join()
        .map_err(|_| "thread panicked (likely stack overflow / uncaught signal)".to_string())?
        .map_err(|e| format!("{}", e))
}

// =========================================================================
// GREEN: These pass — lightweight comparators, small lists, or large stacks.
// =========================================================================

/// nubBy on 50 Ints with simple equality — trivial workload.
#[test]
fn test_nubby_int_50_green() {
    let json = run_result("length (nubBy (\\a b -> a == b) [1..50 :: Int])").unwrap();
    assert_eq!(json, serde_json::json!(50));
}

/// nubBy on 200 Ints with simple equality — lightweight per-comparison cost.
#[test]
fn test_nubby_int_200_green() {
    let json = run_result("length (nubBy (\\a b -> a == b) [1..200 :: Int])").unwrap();
    assert_eq!(json, serde_json::json!(200));
}

/// nubBy with show+pack comparator on 30 items — small list, safe.
#[test]
fn test_nubby_text_key_30_green() {
    let json =
        run_result("length (nubBy (\\a b -> pack (show a) == pack (show b)) [1..30 :: Int])")
            .unwrap();
    assert_eq!(json, serde_json::json!(30));
}

/// nubBy with 3-tuple + Text key on 50 items, 16MB stack — enough headroom.
#[test]
fn test_nubby_triple_50_16mb_green() {
    let result = run_protected(
        16 * 1024 * 1024,
        concat!(
            "let xs = map (\\i -> (i, i * 2, pack (show i))) [1..50 :: Int] ",
            "in length (nubBy (\\(a,_,_) (b,_,_) -> a == b) xs)",
        ),
    );
    assert!(result.is_ok(), "should succeed on 16MB: {:?}", result.err());
    assert_eq!(result.unwrap(), serde_json::json!(50));
}

// =========================================================================
// Previously RED (stack overflow), now GREEN after tail-recursive shadows.
// =========================================================================

/// nubBy with show+pack comparator on 200 items, 8MB stack.
/// Each comparison: Int→show→pack→Text concat→Text equality (~15 calls).
/// Peak depth: ~200 + 199*15 ≈ 3185 frames × ~12KB ≈ 38MB > 8MB.
#[test]
fn test_nubby_text_key_200_8mb_red() {
    let result = run_protected(
        8 * 1024 * 1024,
        "length (nubBy (\\a b -> pack (show a) == pack (show b)) [1..200 :: Int])",
    );
    assert!(
        result.is_ok(),
        "nubBy with text key on 200 items (8MB stack) should succeed: {:?}",
        result.err()
    );
}

/// nubBy with 4-tuple destructuring + Text key on 175 items, 32MB stack.
/// This exactly reproduces the MCP eval failure: nubBy on sgFind results
/// (240 Match values with 5 Text fields). We use 4-tuples with Text fields
/// as a proxy for Match, and 32MB to match the eval thread stack.
#[test]
fn test_nubby_composite_key_175_32mb_red() {
    let result = run_protected(
        32 * 1024 * 1024,
        concat!(
            "let xs = map (\\i -> (i, i * 100, pack (show i), pack (show (i*2)))) [1..175 :: Int] ",
            "in length (nubBy (\\(a,_,c,_) (b,_,d,_) -> ",
            "    (c <> pack \":\" <> d) == (pack (show b) <> pack \":\" <> pack (show (b*2)))) xs)",
        ),
    );
    assert!(
        result.is_ok(),
        "nubBy with composite text key on 175 4-tuples (32MB) should succeed: {:?}",
        result.err()
    );
}

/// nubBy on 500 Ints with simple equality, 4MB stack.
/// Even lightweight comparators hit limits at this size on a small stack.
/// Peak depth: ~500 + 499*3 ≈ 2000 frames × ~12KB ≈ 24MB > 4MB.
#[test]
fn test_nubby_int_500_4mb_red() {
    let result = run_protected(
        4 * 1024 * 1024,
        "length (nubBy (\\a b -> a == b) [1..500 :: Int])",
    );
    assert!(
        result.is_ok(),
        "nubBy on 500 Ints (4MB stack) should succeed: {:?}",
        result.err()
    );
}
