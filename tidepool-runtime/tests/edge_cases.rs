use tidepool_testing::eval_harness::EvalHarness;

fn run_plain(body: &str) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T
import Prelude (Bounded(..))

result :: _
result = {body}
"#
    );
    EvalHarness::new()
        .with_stdlib()
        .run_pure(&src, "result")
        .expect("compile_and_run_pure failed")
        .to_json()
}

#[test]
fn test_numeric_max_bound() {
    let json = run_plain("(maxBound :: Int)");
    assert_eq!(json, serde_json::json!(i64::MAX));
}

#[test]
fn test_numeric_min_bound() {
    let json = run_plain("(minBound :: Int)");
    assert_eq!(json, serde_json::json!(i64::MIN));
}

#[test]
fn test_numeric_abs_min_bound() {
    let json = run_plain("abs (minBound :: Int)");
    assert_eq!(json, serde_json::json!(i64::MIN));
}

#[test]
fn test_numeric_negate_min_bound() {
    let json = run_plain("negate (minBound :: Int)");
    assert_eq!(json, serde_json::json!(i64::MIN));
}

#[test]
fn test_numeric_infinity() {
    let json = run_plain("(2 :: Double) ** (1024 :: Double)");
    assert!(json.is_null());
}

#[test]
fn test_numeric_nan() {
    let json = run_plain("0.0 / 0.0 :: Double");
    assert!(json.is_null());
}

#[test]
fn test_unicode_length() {
    // "héllo" is 5 CHARACTERS (6 bytes in UTF-8) — length is character count,
    // matching base/text. The byte-count behavior this test once pinned was
    // the plan-02 H1 literal-decode bug.
    let json = run_plain("len \"héllo\"");
    assert_eq!(json, serde_json::json!(5));
}

#[test]
fn test_unicode_upper() {
    let json = run_plain("T.toUpper \"café\"");
    assert_eq!(json, serde_json::json!("CAFÉ"));
}

#[test]
fn test_unicode_reverse() {
    let json = run_plain("tReverse \"abc\"");
    assert_eq!(json, serde_json::json!("cba"));
}

#[test]
fn test_empty_reverse() {
    let json = run_plain("reverse ([] :: [Int])");
    assert_eq!(json, serde_json::json!([]));
}

#[test]
fn test_empty_sort() {
    let json = run_plain("sort ([] :: [Int])");
    assert_eq!(json, serde_json::json!([]));
}

#[test]
fn test_empty_sum() {
    let json = run_plain("sum ([] :: [Int])");
    assert_eq!(json, serde_json::json!(0));
}

#[test]
fn test_empty_product() {
    let json = run_plain("product ([] :: [Int])");
    assert_eq!(json, serde_json::json!(1));
}
