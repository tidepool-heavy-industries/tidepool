use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

fn run_plain(body: &str) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude
import Tidepool.TextFormat
import qualified Data.Text as T
default (Int, Text)

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
fn test_camel_to_snake() {
    assert_eq!(
        run_plain(r#"camelToSnake "helloWorld""#),
        json!("hello_world")
    );
}

#[test]
fn test_snake_to_camel() {
    assert_eq!(
        run_plain(r#"snakeToCamel "hello_world""#),
        json!("helloWorld")
    );
}

#[test]
fn test_capitalize() {
    assert_eq!(run_plain(r#"capitalize "hello""#), json!("Hello"));
}

#[test]
fn test_title_case() {
    assert_eq!(
        run_plain(r#"titleCase "hello world""#),
        json!("Hello World")
    );
}

#[test]
fn test_slugify() {
    assert_eq!(run_plain(r#"slugify "Hello World""#), json!("hello-world"));
}

#[test]
fn test_truncate_text() {
    assert_eq!(run_plain(r#"truncateText 5 "Hello World""#), json!("He..."));
}

#[test]
fn test_pad_left() {
    assert_eq!(run_plain(r#"padLeft 10 "hello""#), json!("     hello"));
}

#[test]
fn test_pad_right() {
    assert_eq!(run_plain(r#"padRight 10 "hello""#), json!("hello     "));
}
