use std::path::Path;

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

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
    let pp = prelude_path();
    let include = [pp.as_path()];
    let val = tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure failed");
    val.to_json()
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
    let json = run_plain("abs' (minBound :: Int)");
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
    // NOTE: Tidepool's Text length currently returns the number of BYTES in UTF-8,
    // not the number of characters. "héllo" is 5 characters but 6 bytes.
    let json = run_plain("len \"héllo\"");
    assert_eq!(json, serde_json::json!(6));
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
