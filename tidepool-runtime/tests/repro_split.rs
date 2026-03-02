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
fn test_repro_split_hang() {
    let json = run_plain("T.split (== 'x') \"abc\"");
    assert_eq!(json, serde_json::json!(["abc"]));
}

#[test]
fn test_repro_split_match() {
    let json = run_plain("T.split (== ':') \"a:b:c\"");
    assert_eq!(json, serde_json::json!(["a", "b", "c"]));
}

#[test]
fn test_text_length() {
    let json = run_plain("T.length \"hello\"");
    assert_eq!(json, serde_json::json!(5));
}

#[test]
fn test_text_length_empty() {
    let json = run_plain("T.length \"\"");
    assert_eq!(json, serde_json::json!(0));
}

#[test]
fn test_text_length_pack() {
    let json = run_plain("T.length (T.pack ['h', 'e', 'l', 'l', 'o'])");
    assert_eq!(json, serde_json::json!(5));
}

#[test]
fn test_text_null_empty() {
    let json = run_plain("T.null \"\"");
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_text_null_t_empty() {
    let json = run_plain("T.null T.empty");
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_text_null_nonempty() {
    let json = run_plain("T.null \"abc\"");
    assert_eq!(json, serde_json::json!(false));
}

#[test]
fn test_text_drop() {
    let json = run_plain("T.drop 2 \"hello\"");
    assert_eq!(json, serde_json::json!("llo"));
}

#[test]
fn test_text_drop_single() {
    let json = run_plain("T.drop 1 \"a\"");
    assert_eq!(json, serde_json::json!(""));
}

#[test]
fn test_filter() {
    let json = run_plain("T.filter (== 'a') \"abc\"");
    assert_eq!(json, serde_json::json!("a"));
}

#[test]
fn test_filter_list() {
    let json = run_plain("filter (== 'a') (['a', 'b', 'c'] :: [Char])");
    // [Char] is rendered as string
    assert_eq!(json, serde_json::json!("a"));
}

#[test]
fn test_break() {
    let json = run_plain("T.break (== 'x') \"abc\"");
    assert_eq!(json, serde_json::json!(["abc", ""]));
}

#[test]
fn test_split_on() {
    let json = run_plain("T.splitOn \":\" \"a:b:c\"");
    assert_eq!(json, serde_json::json!(["a", "b", "c"]));
}

#[test]
fn test_split_single() {
    let json = run_plain("T.split (== 'a') \"a\"");
    assert_eq!(json, serde_json::json!(["", ""]));
}

#[test]
fn test_split_false() {
    let json = run_plain("T.split (const False) \"abc\"");
    assert_eq!(json, serde_json::json!(["abc"]));
}

#[test]
fn test_split_true() {
    let json = run_plain("T.split (const True) \"abc\"");
    assert_eq!(json, serde_json::json!(["", "", "", ""]));
}

#[test]
fn test_split_empty() {
    let json = run_plain("T.split (== 'x') \"\"");
    assert_eq!(json, serde_json::json!([""]));
}

#[test]
fn test_split_first() {
    let json = run_plain("T.split (== 'a') \"abc\"");
    assert_eq!(json, serde_json::json!(["", "bc"]));
}

#[test]

fn test_list_text() {
    let json = run_plain("([\"a\", \"b\"] :: [T.Text])");

    assert_eq!(json, serde_json::json!(["a", "b"]));
}

#[test]

fn test_cmptest() {
    let src = std::fs::read_to_string("tests/haskell/cmptest.hs").unwrap();

    let pp = prelude_path();

    let include = [pp.as_path()];

    let val = tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure failed");

    assert_eq!(val.to_json(), serde_json::json!(1));
}

#[test]

fn test_my_split() {
    let src = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}

module Test where

import Tidepool.Prelude

import qualified Data.Text as T



mySplit :: (Char -> Bool) -> T.Text -> [T.Text]

mySplit p t | T.null t = [""]

            | otherwise = let (l, r) = T.break p t

                          in l : if T.null r then [] else mySplit p (T.drop 1 r)



result = mySplit (const False) "abc"

"#;

    let pp = prelude_path();

    let include = [pp.as_path()];

    let val = tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure failed");

    assert_eq!(val.to_json(), serde_json::json!(["abc"]));
}
