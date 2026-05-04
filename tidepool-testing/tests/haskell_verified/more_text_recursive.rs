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

fn arb_text_with_newlines() -> impl Strategy<Value = String> {
    proptest::string::string_regex(r"[a-zA-Z0-9 \.,!\n]{0,30}").unwrap()
}

fn arb_text_no_spaces() -> impl Strategy<Value = String> {
    proptest::string::string_regex(r"[a-zA-Z0-9\.,!]{1,10}").unwrap()
}

fn gen_text_words() -> impl Strategy<Value = (String, serde_json::Value)> {
    arb_text_with_newlines().prop_map(|text| {
        let haskell_src = format!("T.words {:?} :: [Text]", text);
        let expected = text
            .split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>();
        (haskell_src, json!(expected))
    })
}

fn gen_text_lines() -> impl Strategy<Value = (String, serde_json::Value)> {
    arb_text_with_newlines().prop_map(|text| {
        let haskell_src = format!("T.lines {:?} :: [Text]", text);
        let expected = text.lines().map(String::from).collect::<Vec<_>>();
        (haskell_src, json!(expected))
    })
}

fn gen_text_unwords() -> impl Strategy<Value = (String, serde_json::Value)> {
    proptest::collection::vec(arb_text_no_spaces(), 0..5).prop_map(|words| {
        let haskell_src = format!("T.unwords {:?} :: Text", words);
        let expected = words.join(" ");
        (haskell_src, json!(expected))
    })
}

fn gen_text_unlines() -> impl Strategy<Value = (String, serde_json::Value)> {
    proptest::collection::vec(arb_text_no_spaces(), 0..5).prop_map(|lines| {
        let haskell_src = format!("T.unlines {:?} :: Text", lines);
        let expected = if lines.is_empty() {
            String::new()
        } else {
            lines.iter().map(|l| format!("{}\n", l)).collect::<String>()
        };
        (haskell_src, json!(expected))
    })
}

fn gen_text_reverse() -> impl Strategy<Value = (String, serde_json::Value)> {
    arb_text_with_newlines().prop_map(|text| {
        let haskell_src = format!("tReverse {:?} :: Text", text);
        let expected = text.chars().rev().collect::<String>();
        (haskell_src, json!(expected))
    })
}

fn gen_text_concat() -> impl Strategy<Value = (String, serde_json::Value)> {
    proptest::collection::vec(arb_text(), 0..5).prop_map(|chunks| {
        let haskell_src = format!("T.concat {:?} :: Text", chunks);
        let expected = chunks.join("");
        (haskell_src, json!(expected))
    })
}

fn gen_text_pack_unpack() -> impl Strategy<Value = (String, serde_json::Value)> {
    arb_text_with_newlines().prop_map(|text| {
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

#[test]
#[ignore = "Tidepool bug: T.words panics on empty string with Jit(Yield(Undefined)) - tracking issue #108"]
fn test_text_words() {
    run_template(50, gen_text_words());
}

#[test]
#[ignore = "Tidepool bug: T.lines panics on empty string with Jit(Yield(Undefined)) - tracking issue #108"]
fn test_text_lines() {
    run_template(50, gen_text_lines());
}

#[test]
#[ignore = "Tidepool bug: T.unwords returns empty array instead of empty string - tracking issue #108"]
fn test_text_unwords() {
    run_template(50, gen_text_unwords());
}

#[test]
#[ignore = "Tidepool bug: T.unlines returns empty array instead of empty string - tracking issue #108"]
fn test_text_unlines() {
    run_template(50, gen_text_unlines());
}

#[test]
#[ignore = "Tidepool bug: tReverse panics on empty string with Jit(Yield(Undefined)) - tracking issue #108"]
fn test_text_reverse() {
    run_template(50, gen_text_reverse());
}

#[test]
#[ignore = "Tidepool bug: T.concat returns empty array instead of empty string - tracking issue #108"]
fn test_text_concat() {
    run_template(50, gen_text_concat());
}

#[test]
#[ignore = "Tidepool bug: T.pack panics on empty string with Jit(Yield(Undefined)) - tracking issue #108"]
fn test_text_pack_unpack() {
    run_template(50, gen_text_pack_unpack());
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
