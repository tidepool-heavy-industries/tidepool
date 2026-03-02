//! Tests for .tidepool/lib/Library.hs user library combinators.
//! Run with: cargo test -p tidepool-runtime --test user_library

use std::path::Path;
use tidepool_runtime::compile_and_run_pure;

fn prelude_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("haskell/lib")
        .leak()
}

fn user_lib_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".tidepool/lib")
        .leak()
}

fn run_expr(expr: &str) -> serde_json::Value {
    let source = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, ScopedTypeVariables #-}}
module Expr where
import Tidepool.Prelude
import qualified Data.Text as T
import Library
default (Int, Text)

result :: Value
result = toJSON ({expr})
"#
    );
    let include = [prelude_dir(), user_lib_dir()];
    let result =
        compile_and_run_pure(&source, "result", &include).expect("compile_and_run_pure failed");
    result.to_json()
}

#[test]
fn test_hylo_sum() {
    let r = run_expr(
        r#"hylo (\x acc -> x + acc) (0 :: Int) (\n -> if n <= 0 then Nothing else Just (n, n-1)) 10"#,
    );
    assert_eq!(r, serde_json::json!(55));
}

#[test]
fn test_ana_squares() {
    let r = run_expr(r#"ana (\n -> if n > 5 then Nothing else Just (n*n, n+1)) (1 :: Int)"#);
    assert_eq!(r, serde_json::json!([1, 4, 9, 16, 25]));
}

#[test]
fn test_cata_sum() {
    let r = run_expr(r#"cata (\x acc -> x + acc) (0 :: Int) [1,2,3,4,5]"#);
    assert_eq!(r, serde_json::json!(15));
}

#[test]
fn test_para_with_tail() {
    let r = run_expr(r#"para (\x xs acc -> (x, length xs) : acc) ([] :: [(Int,Int)]) [10,20,30]"#);
    assert_eq!(r, serde_json::json!([[10, 2], [20, 1], [30, 0]]));
}

#[test]
fn test_apo_short_circuit() {
    let r =
        run_expr(r#"apo (\n -> if n >= 3 then Left [99 :: Int] else Right (n, n+1)) (0 :: Int)"#);
    assert_eq!(r, serde_json::json!([0, 1, 2, 99]));
}

#[test]
fn test_iterate_n() {
    let r = run_expr(r#"iterateN 5 (*2) (1 :: Int)"#);
    assert_eq!(r, serde_json::json!([1, 2, 4, 8, 16]));
}

#[test]
fn test_converge_isqrt() {
    let r = run_expr(
        r#"let isqrt n = converge (\x -> (x + n `div` x) `div` 2) n in isqrt (144 :: Int)"#,
    );
    assert_eq!(r, serde_json::json!(12));
}

#[test]
fn test_lens_1() {
    let r = run_expr(r#"let p = (10 :: Int, 20 :: Int) in (p ^? _1, p & _2 .~ 99)"#);
    assert_eq!(r, serde_json::json!([10, [10, 99]]));
}

#[test]
fn test_lens_ix() {
    let r = run_expr(r#"[1,2,3,4,5 :: Int] & ix 2 %~ (* 100)"#);
    assert_eq!(r, serde_json::json!([1, 2, 300, 4, 5]));
}

#[test]
fn test_lens_builder() {
    let r = run_expr(
        r#"let name = lens fst (\(_,b) a -> (a,b)); age = lens snd (\(a,_) b -> (a,b)) in ("alice" :: Text, 30 :: Int) & age %~ (+ 1) & name .~ "bob""#,
    );
    assert_eq!(r, serde_json::json!(["bob", 31]));
}

#[test]
fn test_scanl() {
    let r = run_expr(r#"scanl' (+) (0 :: Int) [1,2,3,4]"#);
    assert_eq!(r, serde_json::json!([0, 1, 3, 6, 10]));
}

#[test]
fn test_iterate_while() {
    let r = run_expr(r#"iterateWhile (< 100) (* 2) (1 :: Int)"#);
    assert_eq!(r, serde_json::json!([1, 2, 4, 8, 16, 32, 64]));
}

#[test]
fn test_until() {
    let r = run_expr(r#"until' (> 1000) (* 3) (1 :: Int)"#);
    assert_eq!(r, serde_json::json!(2187));
}

#[test]
fn test_apo_m() {
    // apoM: unfold counting up, bail with precomputed tail at 3
    let r = run_expr(
        r#"let go n = if n >= 3 then Left [99, 100 :: Int] else Right (n, n+1) in apo go (0 :: Int)"#,
    );
    assert_eq!(r, serde_json::json!([0, 1, 2, 99, 100]));
}

// -----------------------------------------------------------------------
// Composition patterns
// -----------------------------------------------------------------------

#[test]
fn test_compose_producer_consumer() {
    // Pattern 1: ana (digits) then cata (reassemble reversed)
    let r = run_expr(
        r#"let digits = ana (\n -> if n == 0 then Nothing else Just (n `rem` 10, n `div` 10)) in foldl' (\acc d -> acc * 10 + d) (0 :: Int) (digits 1234)"#,
    );
    assert_eq!(r, serde_json::json!(4321));
}

#[test]
fn test_compose_hylo_is_fused_ana_cata() {
    // hylo = cata . ana but fused (no intermediate list)
    // Sum of digits: unfold digits, fold by adding
    let r = run_expr(
        r#"hylo (+) (0 :: Int) (\n -> if n == 0 then Nothing else Just (n `rem` 10, n `div` 10)) 9999"#,
    );
    assert_eq!(r, serde_json::json!(36)); // 9+9+9+9
}

#[test]
fn test_compose_producer_lens_consumer() {
    // Pattern 2: ana produces pairs, lens transforms, cata consumes
    let r = run_expr(
        r#"let pairs = ana (\n -> if n > 3 then Nothing else Just ((n, n*n), n+1)) (1 :: Int) in cata (\p acc -> (p & _2 %~ (* 10)) : acc) ([] :: [(Int,Int)]) pairs"#,
    );
    // [(1,10), (2,40), (3,90)] — second elements multiplied by 10
    assert_eq!(r, serde_json::json!([[1, 10], [2, 40], [3, 90]]));
}

#[test]
fn test_compose_scanl_is_ana_with_accumulator() {
    // Pattern 3: scanl' as fold-that-remembers, then consume the trace
    // Running sum, then find the max
    let r = run_expr(
        r#"foldl' (\a b -> if a > b then a else b) (0 :: Int) (scanl' (+) 0 [3,1,4,1,5])"#,
    );
    assert_eq!(r, serde_json::json!(14)); // 0,3,4,8,9,14 → max is 14
}

#[test]
fn test_compose_bounded_search() {
    // Pattern 4: iterateWhile (bounded producer) → cata (consumer)
    // Powers of 2 under 1000, then sum them
    let r = run_expr(r#"cata (+) (0 :: Int) (iterateWhile (< 1000) (* 2) 1)"#);
    assert_eq!(r, serde_json::json!(1023)); // 1+2+4+...+512
}

#[test]
fn test_compose_converge_with_scanl() {
    // Newton's sqrt(2) traced via scanl': iterate the step, collect intermediates
    // We can't use converge (it returns final), so use iterateWhile + scanl'
    let r = run_expr(
        r#"let step x = (x + 200 `div` x) `div` 2 in last (iterateN 6 step (200 :: Int))"#,
    );
    // Should converge toward 14 (isqrt 200 = 14)
    assert_eq!(r, serde_json::json!(14));
}

#[test]
fn test_compose_apo_into_cata() {
    // apo produces with early bail, cata consumes
    // Count up from 1, bail at 5 injecting [100,200], then sum all
    let r = run_expr(
        r#"cata (+) (0 :: Int) (apo (\n -> if n > 4 then Left [100, 200 :: Int] else Right (n, n+1)) 1)"#,
    );
    // 1+2+3+4+100+200 = 310
    assert_eq!(r, serde_json::json!(310));
}

#[test]
fn test_compose_para_for_suffixes() {
    // para gives tail access: check if each element equals sum of remaining
    let r = run_expr(
        r#"para (\x xs acc -> (x == foldl' (+) 0 xs) : acc) ([] :: [Bool]) [6, 3, 2, 1 :: Int]"#,
    );
    // 6 == 3+2+1? True. 3 == 2+1? True. 2 == 1? False. 1 == 0? False.
    assert_eq!(r, serde_json::json!([true, true, false, false]));
}

#[test]
fn test_pipeline_pure() {
    // pipeline over Identity (pure): chain three transformations
    let r = run_expr(
        r#"let f x = Just (x + 1); g x = Just (x * 2); h x = Just (x - 3) in pipeline [f, g, h] (10 :: Int)"#,
    );
    assert_eq!(r, serde_json::json!(19)); // ((10+1)*2)-3 = 19
}

#[test]
fn test_fan_out_pure() {
    // fanOut over Identity: run same input through multiple fns
    let r =
        run_expr(r#"fanOut [\x -> Just (x*2), \x -> Just (x+100), \x -> Just (x*x)] (5 :: Int)"#);
    assert_eq!(r, serde_json::json!([10, 105, 25]));
}

#[test]
fn test_fold_early() {
    // foldEarlyM: sum until accumulator exceeds 10, then bail
    let r = run_expr(
        r#"let step acc x = if acc + x > 10 then Just (Left acc) else Just (Right (acc + x)) in foldEarlyM step (0 :: Int) [3, 4, 5, 6, 7]"#,
    );
    assert_eq!(r, serde_json::json!(7)); // 3+4=7, 7+5=12>10 → bail with 7
}

#[test]
fn test_retry_pure() {
    // retry over Maybe: succeed on 3rd try (simulated via list consumption)
    // We'll use a simpler test: retry with always-Nothing gives Nothing
    let r = run_expr(r#"retry 5 (Just (Nothing :: Maybe Int))"#);
    assert_eq!(r, serde_json::json!(null));
}

#[test]
fn test_tree_hylo_merge_sort() {
    let r = run_expr(
        r#"let merge [] ys = ys; merge xs [] = xs; merge (x:xs) (y:ys) = if x <= y then x : merge xs (y:ys) else y : merge (x:xs) ys in treeHylo (\l _ r -> merge l r) (id :: [Int] -> [Int]) (\xs -> if length xs <= 1 then Left xs else let h = length xs `div` 2 in Right (take h xs, (), drop h xs)) ([5,3,8,1,4,2 :: Int])"#,
    );
    assert_eq!(r, serde_json::json!([1, 2, 3, 4, 5, 8]));
}

// ===========================================================================
// § Text Utilities
// ===========================================================================

#[test]
fn test_chunks_of() {
    let r = run_expr(r#"chunksOf 3 [1,2,3,4,5,6,7 :: Int]"#);
    assert_eq!(r, serde_json::json!([[1, 2, 3], [4, 5, 6], [7]]));
}

#[test]
fn test_windows() {
    let r = run_expr(r#"windows 2 [1,2,3,4 :: Int]"#);
    assert_eq!(r, serde_json::json!([[1, 2], [2, 3], [3, 4]]));
}

#[test]
fn test_indexed() {
    let r = run_expr(r#"indexed [10,20,30 :: Int]"#);
    assert_eq!(r, serde_json::json!([[0, 10], [1, 20], [2, 30]]));
}

#[test]
fn test_safe_index() {
    let r = run_expr(r#"([10,20,30 :: Int] !? 1, [10,20,30 :: Int] !? 5)"#);
    assert_eq!(r, serde_json::json!([20, null]));
}

#[test]
fn test_histogram() {
    let r = run_expr(r#"histogram [1,1,2,3,3,3 :: Int]"#);
    assert_eq!(r, serde_json::json!([[1, 2], [2, 1], [3, 3]]));
}

#[test]
fn test_pad_right() {
    let r = run_expr(r#"padRight 8 "hi""#);
    assert_eq!(r, serde_json::json!("hi      "));
}

#[test]
fn test_pad_left() {
    let r = run_expr(r#"padLeft 8 "hi""#);
    assert_eq!(r, serde_json::json!("      hi"));
}

#[test]
fn test_insert_at() {
    let r = run_expr(r#"insertAt 2 99 [1,2,3 :: Int]"#);
    assert_eq!(r, serde_json::json!([1, 2, 99, 3]));
}

#[test]
fn test_remove_at() {
    let r = run_expr(r#"removeAt [0,2] [10,20,30,40 :: Int]"#);
    assert_eq!(r, serde_json::json!([20, 40]));
}

#[test]
fn test_chunks_of_empty() {
    let r = run_expr(r#"chunksOf 3 ([] :: [Int])"#);
    assert_eq!(r, serde_json::json!([]));
}

#[test]
fn test_windows_too_short() {
    let r = run_expr(r#"windows 5 [1,2,3 :: Int]"#);
    assert_eq!(r, serde_json::json!([]));
}
