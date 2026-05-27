//! Regression coverage for #312: `T.replace` / `T.breakOn` returning null pointer
//! on multi-line / composite-return inputs.
//!
//! These cover the pure JIT path (`compile_and_run_pure`). Sister suite in
//! `tidepool-mcp/tests/text_breakon_replace_mcp.rs` covers the effect-dispatch path.

use serde_json::json;
use std::path::Path;

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

fn run(body: &str) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
default (Int, Text)

result :: _
result = {body}
"#
    );
    let pp = prelude_path();
    let include = [pp.as_path()];
    tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure failed")
        .to_json()
}

#[test]
fn replace_single_line() {
    assert_eq!(
        run(r#"T.replace "world" "there" "hello world""#),
        json!("hello there"),
    );
}

#[test]
fn replace_multiline_inline_newlines() {
    assert_eq!(
        run(r#"T.replace "target" "X" "line one\nline two with target here\nline three""#),
        json!("line one\nline two with X here\nline three"),
    );
}

#[test]
fn replace_with_unlines_body() {
    assert_eq!(
        run(
            r#"T.replace "target" "REPLACED" (T.unlines [T.pack "line one", T.pack "line two with target here", T.pack "line three"])"#
        ),
        json!("line one\nline two with REPLACED here\nline three\n"),
    );
}

#[test]
fn replace_no_match() {
    assert_eq!(
        run(r#"T.replace "nope" "X" "hello world""#),
        json!("hello world")
    );
}

#[test]
fn replace_newline_in_input() {
    assert_eq!(run(r#"T.replace "b" "X" "a\nb\nc""#), json!("a\nX\nc"));
}

#[test]
fn breakon_no_match() {
    assert_eq!(
        run(r#"let (a, b) = T.breakOn "X" "hello world" in (T.length a, T.length b)"#),
        json!([11, 0]),
    );
}

#[test]
fn breakon_match_at_zero() {
    assert_eq!(
        run(r#"let (a, b) = T.breakOn "hello" "hello world" in (T.length a, T.length b)"#),
        json!([0, 11]),
    );
}

#[test]
fn breakon_short_needle() {
    assert_eq!(
        run(r#"let (a, b) = T.breakOn "lo" "hello world" in (T.length a, T.length b)"#),
        json!([3, 8]),
    );
}

#[test]
fn breakon_unlines_body_length_pair() {
    assert_eq!(
        run(
            r#"let { body = T.unlines [T.pack "line one", T.pack "line two with target here", T.pack "line three"] ; (a, b) = T.breakOn "target" body } in (T.length a, T.length b)"#
        ),
        json!([23, 23]),
    );
}

#[test]
fn breakon_just_newline_needle() {
    assert_eq!(
        run(r#"let (a, b) = T.breakOn "\n" "a\nb" in (T.length a, T.length b)"#),
        json!([1, 2]),
    );
}
