//! Reproducer for crash: Text equality + filter under GC pressure.
//!
//! `filter (== w) xs` on `[Text]` crashes (connection closed / SIGILL)
//! when multiple calls force enough allocation to trigger GC.
//! The same pattern on `[Int]` works fine, implicating Text's inner
//! ByteArray# pointer during GC copying.

use std::path::Path;

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

fn run(body: &str) -> serde_json::Value {
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
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            tidepool_runtime::compile_and_run_pure(&src, "result", &include)
                .expect("compile_and_run_pure failed")
                .to_json()
        })
        .unwrap()
        .join()
        .expect("thread panicked")
}

// --- Int baseline (should pass) ---

#[test]
fn int_filter_count_works() {
    let json = run(
        r#"
        let count :: Int -> [Int] -> Int
            count w xs = length (filter (== w) xs)
            nums = [1,2,3,1,2,1] :: [Int]
        in (count 1 nums, count 2 nums, count 3 nums)
        "#,
    );
    assert_eq!(json, serde_json::json!([3, 2, 1]));
}

// --- Text crash reproducers ---

/// Just Text equality — no list operations.
#[test]
fn text_eq_basic() {
    let json = run(r#"("hello" :: T.Text) == ("hello" :: T.Text)"#);
    assert_eq!(json, serde_json::json!(true));
}

/// Text equality in a pair — two equality checks.
#[test]
fn text_eq_pair() {
    let json = run(
        r#"(("a" :: T.Text) == ("a" :: T.Text), ("a" :: T.Text) == ("b" :: T.Text))"#,
    );
    assert_eq!(json, serde_json::json!([true, false]));
}

/// Text equality with a let-bound variable.
#[test]
fn text_eq_let_bound() {
    let json = run(
        r#"let w = "hello" :: T.Text in (w == "hello", w == "world")"#,
    );
    assert_eq!(json, serde_json::json!([true, false]));
}

/// Simple recursive count on Text list — no filter, just manual recursion.
#[test]
fn text_count_manual_single() {
    let json = run(
        r#"
        let count :: T.Text -> [T.Text] -> Int
            count w [] = 0
            count w (x:xs) = if x == w then 1 + count w xs else count w xs
        in count ("a" :: T.Text) (["a", "b", "a"] :: [T.Text])
        "#,
    );
    assert_eq!(json, serde_json::json!(2));
}

/// Two manual count calls — minimal crash case.
#[test]
fn text_count_manual_two() {
    let json = run(
        r#"
        let count :: T.Text -> [T.Text] -> Int
            count w [] = 0
            count w (x:xs) = if x == w then 1 + count w xs else count w xs
            words = ["a", "b", "a"] :: [T.Text]
        in (count "a" words, count "b" words)
        "#,
    );
    assert_eq!(json, serde_json::json!([2, 1]));
}

/// Same but with Int — does Int crash too?
#[test]
fn int_count_manual_two() {
    let json = run(
        r#"
        let count :: Int -> [Int] -> Int
            count w [] = 0
            count w (x:xs) = if x == w then 1 + count w xs else count w xs
            nums = [1, 2, 1] :: [Int]
        in (count 1 nums, count 2 nums)
        "#,
    );
    assert_eq!(json, serde_json::json!([2, 1]));
}

/// Two traversals of same Text list without equality — just length.
#[test]
fn text_length_twice() {
    let json = run(
        r#"
        let words = ["a", "b", "c"] :: [T.Text]
        in (length words, length words)
        "#,
    );
    assert_eq!(json, serde_json::json!([3, 3]));
}

/// Two traversals with head — access first element twice.
#[test]
fn text_head_twice() {
    let json = run(
        r#"
        let words = ["hello", "world"] :: [T.Text]
        in (head words, head words)
        "#,
    );
    assert_eq!(json, serde_json::json!(["hello", "hello"]));
}

/// Simplest shared Text: let-bind and use twice.
#[test]
fn text_let_use_twice() {
    let json = run(
        r#"
        let t = "hello" :: T.Text
        in (t, t)
        "#,
    );
    assert_eq!(json, serde_json::json!(["hello", "hello"]));
}

/// Shared Int: let-bind and use twice (should work).
#[test]
fn int_let_use_twice() {
    let json = run(r#"let n = 42 :: Int in (n, n)"#);
    assert_eq!(json, serde_json::json!([42, 42]));
}

/// Shared [Int] list: length twice.
#[test]
fn int_list_length_twice() {
    let json = run(
        r#"let xs = [1,2,3] :: [Int] in (length xs, length xs)"#,
    );
    assert_eq!(json, serde_json::json!([3, 3]));
}

/// Shared [Char] list: length twice.
#[test]
fn char_list_length_twice() {
    let json = run(
        r#"let xs = ['a','b','c'] in (length xs, length xs)"#,
    );
    assert_eq!(json, serde_json::json!([3, 3]));
}

/// Shared singleton Text list: length twice.
#[test]
fn text_singleton_length_twice() {
    let json = run(
        r#"let xs = ["a"] :: [T.Text] in (length xs, length xs)"#,
    );
    assert_eq!(json, serde_json::json!([1, 1]));
}

/// Non-shared: two separate Text lists.
#[test]
fn text_separate_lists() {
    let json = run(
        r#"(length (["a"] :: [T.Text]), length (["b"] :: [T.Text]))"#,
    );
    assert_eq!(json, serde_json::json!([1, 1]));
}

/// Shared but length 0 (empty text list).
#[test]
fn text_empty_list_twice() {
    let json = run(
        r#"let xs = [] :: [T.Text] in (length xs, length xs)"#,
    );
    assert_eq!(json, serde_json::json!([0, 0]));
}

/// head of a singleton text list, once (not shared).
#[test]
fn text_singleton_head_once() {
    let json = run(
        r#"head (["hello"] :: [T.Text])"#,
    );
    assert_eq!(json, serde_json::json!("hello"));
}

/// Shared [Maybe Int] list: length twice (Maybe has 2 constructors).
#[test]
fn maybe_list_length_twice() {
    let json = run(
        r#"let xs = [Just (1 :: Int), Nothing, Just 2] in (length xs, length xs)"#,
    );
    assert_eq!(json, serde_json::json!([3, 3]));
}

/// Shared [(Int,Int)] list: length twice (tuple has 2 fields).
#[test]
fn pair_list_length_twice() {
    let json = run(
        r#"let xs = [(1::Int, 2::Int), (3, 4)] in (length xs, length xs)"#,
    );
    assert_eq!(json, serde_json::json!([2, 2]));
}

/// Shared text built from T.pack instead of OverloadedStrings.
#[test]
fn text_pack_length_twice() {
    let json = run(
        r#"let xs = [T.pack "a"] in (length xs, length xs)"#,
    );
    assert_eq!(json, serde_json::json!([1, 1]));
}

/// Shared Text list with T.singleton.
#[test]
fn text_singleton_char_length_twice() {
    let json = run(
        r#"let xs = [T.singleton 'a'] in (length xs, length xs)"#,
    );
    assert_eq!(json, serde_json::json!([1, 1]));
}

/// Non-list: shared Text in a tuple accessed twice via fst.
#[test]
fn text_tuple_fst_twice() {
    let json = run(
        r#"
        let p = ("hello" :: T.Text, 42 :: Int)
        in (fst p, fst p)
        "#,
    );
    assert_eq!(json, serde_json::json!(["hello", "hello"]));
}



/// Length of [Text] once, but return as tuple with duplicate.
#[test]
fn text_list_length_dup_tuple() {
    let json = run(
        r#"let n = length (["a"] :: [T.Text]) in (n, n)"#,
    );
    assert_eq!(json, serde_json::json!([1, 1]));
}

/// Int version of same — control.
#[test]
fn int_list_length_dup_tuple() {
    let json = run(
        r#"let n = length ([1] :: [Int]) in (n, n)"#,
    );
    assert_eq!(json, serde_json::json!([1, 1]));
}

/// Write `(length xs, length xs)` directly — same Core as the dup_tuple.
#[test]
fn text_list_length_inline_twice() {
    let json = run(
        r#"(length (["a"] :: [T.Text]), length (["a"] :: [T.Text]))"#,
    );
    assert_eq!(json, serde_json::json!([1, 1]));
}

/// Create a list, don't access it twice — just return it.
#[test]
fn text_list_return_only() {
    let json = run(r#"["hello", "world"] :: [T.Text]"#);
    assert_eq!(json, serde_json::json!(["hello", "world"]));
}

/// Same list, access once via length.
#[test]
fn text_list_length_once() {
    let json = run(
        r#"let xs = ["a", "b"] :: [T.Text] in length xs"#,
    );
    assert_eq!(json, serde_json::json!(2));
}

/// Single filter call on Text — baseline.
#[test]
fn text_filter_single_call() {
    let json = run(
        r#"
        let count :: T.Text -> [T.Text] -> Int
            count w xs = length (filter (== w) xs)
        in count ("a" :: T.Text) (["a", "b", "a"] :: [T.Text])
        "#,
    );
    assert_eq!(json, serde_json::json!(2));
}

/// Two filter calls — this is the minimal crash case from MCP testing.
#[test]
fn text_filter_two_calls() {
    let json = run(
        r#"
        let count :: T.Text -> [T.Text] -> Int
            count w xs = length (filter (== w) xs)
            words = ["apple", "banana", "apple"] :: [T.Text]
        in (count "apple" words, count "banana" words)
        "#,
    );
    assert_eq!(json, serde_json::json!([2, 1]));
}

/// Three filter calls — the original crash scenario.
#[test]
fn text_filter_three_calls() {
    let json = run(
        r#"
        let count :: T.Text -> [T.Text] -> Int
            count w xs = length (filter (== w) xs)
            words = ["apple", "banana", "apple", "cherry", "banana", "apple"] :: [T.Text]
        in (count "apple" words, count "banana" words, count "cherry" words)
        "#,
    );
    assert_eq!(json, serde_json::json!([3, 2, 1]));
}

/// Manual recursive count (no filter builtin) to isolate the issue.
#[test]
fn text_manual_count() {
    let json = run(
        r#"
        let count :: T.Text -> [T.Text] -> Int
            count w [] = 0
            count w (x:xs) = if x == w then 1 + count w xs else count w xs
            words = ["apple", "banana", "apple"] :: [T.Text]
        in (count "apple" words, count "banana" words)
        "#,
    );
    assert_eq!(json, serde_json::json!([2, 1]));
}

/// Text equality in a list comprehension context.
#[test]
fn text_list_comp_eq() {
    let json = run(
        r#"
        let words = ["a", "b", "a", "c", "a"] :: [T.Text]
        in length [x | x <- words, x == "a"]
        "#,
    );
    assert_eq!(json, serde_json::json!(3));
}
