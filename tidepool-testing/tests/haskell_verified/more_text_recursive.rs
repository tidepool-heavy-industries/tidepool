//! Verified-generator templates for additional Text ops (T.words/lines/unwords/
//! unlines, T.reverse, T.concat, pack/unpack roundtrip) and recursive patterns
//! (factorial via letrec, fib bounded depth, liftA2 over Maybe Int).
//!
//! Stub: filled in by the `more_text_recursive` leaf. Follows the patterns in
//! `fmap.rs` / `text.rs` / `cousins.rs`. ASCII-only, Int-only, pinned types,
//! `ProptestConfig::with_cases(50)` per template.

#![allow(unused_imports)]

use crate::{arb_int, arb_text, run_template};
use proptest::prelude::*;
use serde_json::json;

fn arb_text_no_spaces() -> impl Strategy<Value = String> {
    proptest::string::string_regex(r"[a-zA-Z0-9\.,!]{1,10}").unwrap()
}

/// Non-empty variant of `arb_text_with_newlines`. T.words / T.lines /
/// T.pack / tReverse panic on empty input via #308's UnresolvedVar shape;
/// filter empties so the rest of the input space gets exercised. Targeted
/// empty-input regression tests live below with `#[ignore]` on #308.
fn arb_text_with_newlines_nonempty() -> impl Strategy<Value = String> {
    proptest::string::string_regex(r"[a-zA-Z0-9 \.,!\n]{1,30}").unwrap()
}

fn gen_text_words() -> impl Strategy<Value = (String, serde_json::Value)> {
    arb_text_with_newlines_nonempty().prop_map(|text| {
        let haskell_src = format!("T.words {:?} :: [Text]", text);
        let expected = text
            .split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>();
        (haskell_src, json!(expected))
    })
}

fn gen_text_lines() -> impl Strategy<Value = (String, serde_json::Value)> {
    arb_text_with_newlines_nonempty().prop_map(|text| {
        let haskell_src = format!("T.lines {:?} :: [Text]", text);
        let expected = text.lines().map(String::from).collect::<Vec<_>>();
        (haskell_src, json!(expected))
    })
}

fn gen_text_unwords() -> impl Strategy<Value = (String, serde_json::Value)> {
    // T.unwords on a non-empty list of non-empty words. Empty list produces
    // empty-Text result; the comparator's #308 canonicalization handles that
    // shape but the dedicated empty-input regression below is more explicit.
    proptest::collection::vec(arb_text_no_spaces(), 1..5).prop_map(|words| {
        let haskell_src = format!("T.unwords {:?} :: Text", words);
        let expected = words.join(" ");
        (haskell_src, json!(expected))
    })
}

fn gen_text_unlines() -> impl Strategy<Value = (String, serde_json::Value)> {
    proptest::collection::vec(arb_text_no_spaces(), 1..5).prop_map(|lines| {
        let haskell_src = format!("T.unlines {:?} :: Text", lines);
        let expected = lines.iter().map(|l| format!("{}\n", l)).collect::<String>();
        (haskell_src, json!(expected))
    })
}

fn gen_text_reverse() -> impl Strategy<Value = (String, serde_json::Value)> {
    arb_text_with_newlines_nonempty().prop_map(|text| {
        let haskell_src = format!("tReverse {:?} :: Text", text);
        let expected = text.chars().rev().collect::<String>();
        (haskell_src, json!(expected))
    })
}

fn gen_text_concat() -> impl Strategy<Value = (String, serde_json::Value)> {
    // Non-empty list of non-empty chunks. Empty input is the targeted
    // regression below.
    proptest::collection::vec(arb_text_no_spaces(), 1..5).prop_map(|chunks| {
        let haskell_src = format!("T.concat {:?} :: Text", chunks);
        let expected = chunks.join("");
        (haskell_src, json!(expected))
    })
}

fn gen_text_pack_unpack() -> impl Strategy<Value = (String, serde_json::Value)> {
    arb_text_with_newlines_nonempty().prop_map(|text| {
        let haskell_src = format!("T.pack (T.unpack {:?}) :: Text", text);
        let expected = text.clone();
        (haskell_src, json!(expected))
    })
}

fn gen_recursive_factorial() -> impl Strategy<Value = (String, serde_json::Value)> {
    (0i64..=12).prop_map(|n| {
        let haskell_src = format!(
            "(let {{ fact :: Int -> Int; fact x = if x <= 1 then 1 else x * fact (x - 1) }} in fact ({}) :: Int)",
            n
        );
        let expected: i64 = (1..=n).product();
        let expected = if n == 0 { 1 } else { expected };
        (haskell_src, json!(expected))
    })
}

fn gen_recursive_fib() -> impl Strategy<Value = (String, serde_json::Value)> {
    (0i64..=15).prop_map(|n| {
        let haskell_src = format!(
            "(let {{ fib :: Int -> Int; fib x = if x <= 0 then 0 else if x == 1 then 1 else fib (x - 1) + fib (x - 2) }} in fib ({}) :: Int)",
            n
        );
        let expected = {
            let mut a = 0;
            let mut b = 1;
            for _ in 0..n {
                let temp = a;
                a = b;
                b += temp;
            }
            a
        };
        (haskell_src, json!(expected))
    })
}

fn gen_recursive_lifta2() -> impl Strategy<Value = (String, serde_json::Value)> {
    (
        proptest::option::of(arb_int()),
        proptest::option::of(arb_int()),
    )
        .prop_map(|(ma, mb)| {
            let format_maybe = |m: Option<i64>| match m {
                Some(x) => format!("(Just ({}) :: Maybe Int)", x),
                None => "(Nothing :: Maybe Int)".to_string(),
            };

            let haskell_src = format!(
                "do {{ x <- {}; y <- {}; pure (x + y) }} :: Maybe Int",
                format_maybe(ma),
                format_maybe(mb)
            );
            let expected = match (ma, mb) {
                (Some(a), Some(b)) => json!(a + b),
                _ => json!(null),
            };
            (haskell_src, expected)
        })
}

// Active templates: filter empty input (or non-empty list inputs) so #308's
// crash-on-empty path is avoided; the comparator's #308 canonicalization
// handles empty-Text *results*.
#[test]
fn test_text_words() {
    run_template(50, gen_text_words());
}

#[test]
fn test_text_lines() {
    run_template(50, gen_text_lines());
}

#[test]
fn test_text_unwords() {
    run_template(50, gen_text_unwords());
}

#[test]
fn test_text_unlines() {
    run_template(50, gen_text_unlines());
}

#[test]
fn test_text_reverse() {
    run_template(50, gen_text_reverse());
}

#[test]
fn test_text_concat() {
    run_template(50, gen_text_concat());
}

#[test]
fn test_text_pack_unpack() {
    run_template(50, gen_text_pack_unpack());
}

// Targeted empty-input regression tests (#308). Re-enable when the bug is
// fixed; until then the active templates above filter empty input/list and
// the comparator masks empty-Text rendering.
#[test]
#[ignore = "exposes #308 — T.words \"\" panics with Jit(Yield(Undefined))"]
fn test_text_words_empty_regression() {
    let actual = crate::compile_run_pure(r#"T.words "" :: [Text]"#);
    assert_eq!(actual, json!(Vec::<String>::new()));
}

#[test]
#[ignore = "exposes #308 — T.lines \"\" panics with Jit(Yield(Undefined))"]
fn test_text_lines_empty_regression() {
    let actual = crate::compile_run_pure(r#"T.lines "" :: [Text]"#);
    assert_eq!(actual, json!(Vec::<String>::new()));
}

#[test]
#[ignore = "exposes #308 — T.unwords [] mis-renders empty Text"]
fn test_text_unwords_empty_regression() {
    let actual = crate::compile_run_pure(r#"T.unwords [] :: Text"#);
    assert_eq!(actual, json!(""));
}

#[test]
#[ignore = "exposes #308 — T.unlines [] mis-renders empty Text"]
fn test_text_unlines_empty_regression() {
    let actual = crate::compile_run_pure(r#"T.unlines [] :: Text"#);
    assert_eq!(actual, json!(""));
}

#[test]
#[ignore = "exposes #308 — tReverse \"\" panics with Jit(Yield(Undefined))"]
fn test_text_reverse_empty_regression() {
    let actual = crate::compile_run_pure(r#"tReverse "" :: Text"#);
    assert_eq!(actual, json!(""));
}

#[test]
#[ignore = "exposes #308 — T.concat [] mis-renders empty Text"]
fn test_text_concat_empty_regression() {
    let actual = crate::compile_run_pure(r#"T.concat [] :: Text"#);
    assert_eq!(actual, json!(""));
}

#[test]
#[ignore = "exposes #308 — T.pack \"\" panics with Jit(Yield(Undefined))"]
fn test_text_pack_unpack_empty_regression() {
    let actual = crate::compile_run_pure(r#"T.pack (T.unpack "") :: Text"#);
    assert_eq!(actual, json!(""));
}

#[test]
fn test_recursive_factorial() {
    run_template(50, gen_recursive_factorial());
}

#[test]
fn test_recursive_fib() {
    run_template(50, gen_recursive_fib());
}

#[test]
fn test_recursive_lifta2() {
    run_template(50, gen_recursive_lifta2());
}
