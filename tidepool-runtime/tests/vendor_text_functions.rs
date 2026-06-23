//! Phase-2 correctness + landmine gate for the vendored `Tidepool.Data.Text`.
//!
//! Every vendored predicate function is exercised under the operator-section
//! predicate via `map` (the canonical PAP trigger), BOTH directly (`T.foo`,
//! T = vendored home body) AND through a home-module wrapper
//! (`Tidepool.Internal.DataTextProbe`, mirroring a Prelude shadow / lib verb). Both must
//! match GHC semantics. If the vendoring works, all of these are GREEN — the
//! external-package landmine (`takewhile-shadow-load-bearing`) is dissolved.

use serde_json::{json, Value};
use std::path::Path;

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

fn run(body: &str) -> Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as DT
import qualified Tidepool.Data.Text as T
import Tidepool.Internal.DataTextProbe
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

const SLASH: &str = r#"[DT.pack "hello/world", DT.pack "foo/bar", DT.pack "noSlash"]"#;
const DIGIT: &str = r#"[DT.pack "123abc", DT.pack "42", DT.pack "abc", DT.pack "9z9"]"#;

// ---- takeWhile / dropWhile family ----
#[test]
fn takewhile_direct_and_wrapped() {
    assert_eq!(
        run(&format!(r#"map (T.takeWhile (/= '/')) {SLASH}"#)),
        json!(["hello", "foo", "noSlash"])
    );
    assert_eq!(
        run(&format!(r#"map (twW (/= '/')) {SLASH}"#)),
        json!(["hello", "foo", "noSlash"])
    );
}
#[test]
fn dropwhile_direct_and_wrapped() {
    assert_eq!(
        run(&format!(r#"map (T.dropWhile (/= '/')) {SLASH}"#)),
        json!(["/world", "/bar", ""])
    );
    assert_eq!(
        run(&format!(r#"map (dwW (/= '/')) {SLASH}"#)),
        json!(["/world", "/bar", ""])
    );
}
#[test]
fn takewhileend_wrapped() {
    // takeWhileEnd (/= '/') "hello/world" = "world"
    assert_eq!(
        run(&format!(r#"map (twEndW (/= '/')) {SLASH}"#)),
        json!(["world", "bar", "noSlash"])
    );
}
#[test]
fn dropwhileend_wrapped() {
    // dropWhileEnd (/= '/') "hello/world" = "hello/"
    assert_eq!(
        run(&format!(r#"map (dwEndW (/= '/')) {SLASH}"#)),
        json!(["hello/", "foo/", ""])
    );
}
#[test]
fn droparound_wrapped() {
    // dropAround (== 'x') "xxhixx" = "hi"
    assert_eq!(run(r#"daW (== 'x') (DT.pack "xxhixx")"#), json!("hi"));
}

// ---- span / break / split ----
#[test]
fn span_wrapped() {
    assert_eq!(
        run(r#"spanW (/= '/') (DT.pack "hello/world")"#),
        json!(["hello", "/world"])
    );
}
#[test]
fn break_wrapped() {
    assert_eq!(
        run(r#"breakW (== '/') (DT.pack "hello/world")"#),
        json!(["hello", "/world"])
    );
}
#[test]
fn split_wrapped() {
    assert_eq!(
        run(r#"splitW (== ',') (DT.pack "a,bb,c")"#),
        json!(["a", "bb", "c"])
    );
}

// ---- filter / partition ----
#[test]
fn filter_direct_and_wrapped() {
    assert_eq!(
        run(&format!(r#"map (T.filter (/= '/')) {SLASH}"#)),
        json!(["helloworld", "foobar", "noSlash"])
    );
    assert_eq!(
        run(&format!(r#"map (filterW (/= '/')) {SLASH}"#)),
        json!(["helloworld", "foobar", "noSlash"])
    );
}
#[test]
fn partition_wrapped() {
    // partition isDigit "a1b2" = ("12","ab")
    assert_eq!(
        run(r#"partitionW (\c -> c >= '0' && c <= '9') (DT.pack "a1b2")"#),
        json!(["12", "ab"])
    );
}

// ---- all / any / find / findIndex ----
#[test]
fn all_direct_and_wrapped() {
    assert_eq!(
        run(&format!(r#"map (T.all (/= '/')) {SLASH}"#)),
        json!([false, false, true])
    );
    assert_eq!(
        run(&format!(r#"map (allW (/= '/')) {SLASH}"#)),
        json!([false, false, true])
    );
}
#[test]
fn any_wrapped() {
    assert_eq!(
        run(&format!(r#"map (anyW (== '/')) {SLASH}"#)),
        json!([true, true, false])
    );
}
#[test]
fn find_wrapped() {
    assert_eq!(
        run(&format!(r#"map (findW (== '/')) {SLASH}"#)),
        json!(["/", "/", null])
    );
}
#[test]
fn findindex_wrapped() {
    // findIndex (== '/') over SLASH: positions 5, 3, none
    assert_eq!(
        run(&format!(r#"map (findIndexW (== '/')) {SLASH}"#)),
        json!([5, 3, null])
    );
}
#[test]
fn findindex_digits() {
    // findIndex isDigit "abc1" = 3 ; "42" = 0 ; "abc" = none ; "9z9" = 0
    assert_eq!(
        run(&format!(
            r#"map (findIndexW (\c -> c >= '0' && c <= '9')) {DIGIT}"#
        )),
        json!([0, 0, null, 0])
    );
}

// ---- groupBy ----
#[test]
fn groupby_wrapped() {
    // groupBy (==) "aabbbc" = ["aa","bbb","c"]
    assert_eq!(
        run(r#"groupByW (==) (DT.pack "aabbbc")"#),
        json!(["aa", "bbb", "c"])
    );
}
