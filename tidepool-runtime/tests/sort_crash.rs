/// Reproducer for MCP `pure (sort [3,1,2 :: Int])` crash and broader
/// freer-simple integration tests matching the exact source templates
/// the MCP server generates.
use std::path::Path;
use frunk::HNil;
use tidepool_runtime::{compile_and_run, compile_and_run_pure, compile_haskell};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

/// Build the exact Haskell source the MCP server generates for a given
/// set of do-notation lines with Console/KV/Fs effects.
fn mcp_source(lines: &[&str]) -> String {
    let mut s = String::from(
r#"{-# LANGUAGE DataKinds, TypeOperators, FlexibleContexts, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
module Expr where
import Prelude hiding (reverse, splitAt, span, break, init, words, lines, unlines, unwords, concatMap, dropWhile)
import Control.Monad.Freer
import Tidepool.Prelude

data Console a where
  Print :: String -> Console ()

data KV a where
  KvGet :: String -> KV (Maybe String)
  KvSet :: String -> String -> KV ()
  KvDelete :: String -> KV ()
  KvKeys :: KV [String]

data Fs a where
  FsRead :: String -> Fs String
  FsWrite :: String -> String -> Fs ()

result :: Eff '[Console, KV, Fs] _
result = do
"#);
    for line in lines {
        s.push_str("  ");
        s.push_str(line);
        s.push('\n');
    }
    s
}

/// Build a plain (non-effect) Haskell module with the prelude.
fn plain_source(body: &str) -> String {
    format!(
r#"{{-# LANGUAGE NoImplicitPrelude, PartialTypeSignatures #-}}
module Test where
import Prelude hiding (reverse, splitAt, span, break, init, words, lines, unlines, unwords, concatMap, dropWhile)
import Tidepool.Prelude

result :: _
result = {body}
"#)
}

/// Run a test on a thread with 8MB stack. Returns the JSON result.
fn run_mcp(lines: &[&str]) -> serde_json::Value {
    let src = mcp_source(lines);
    let pp = prelude_path();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let val = compile_and_run(&src, "result", &include, &mut HNil, &())
                .expect("compile_and_run failed");
            val.to_json()
        })
        .unwrap()
        .join()
        .expect("thread panicked")
}

/// Compile-only (no execution). Returns Ok(node_count) or Err(message).
fn compile_only(src: &str) -> Result<usize, String> {
    let pp = prelude_path();
    let src = src.to_owned();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let (expr, _table) = compile_haskell(&src, "result", &include)
                .map_err(|e| format!("{:?}", e))?;
            Ok(expr.nodes.len())
        })
        .unwrap()
        .join()
        .expect("thread panicked")
}

/// Run plain (non-effect) source. Returns the JSON result.
fn run_plain(body: &str) -> serde_json::Value {
    let src = plain_source(body);
    let pp = prelude_path();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let val = compile_and_run_pure(&src, "result", &include)
                .expect("compile_and_run_pure failed");
            val.to_json()
        })
        .unwrap()
        .join()
        .expect("thread panicked")
}

// ===========================================================================
// Plain (non-effect) prelude tests
// ===========================================================================

#[test]
#[ignore]
fn test_plain_sort() {
    let json = run_plain("sort [3, 1, 2 :: Int]");
    assert_eq!(json, serde_json::json!([1, 2, 3]));
}

// ===========================================================================
// MCP-style freer-simple tests (Eff '[Console, KV, Fs] _)
// ===========================================================================

#[test]
#[ignore]
fn test_mcp_pure_lit() {
    let json = run_mcp(&["pure (42 :: Int)"]);
    assert_eq!(json, serde_json::json!(42));
}

#[test]
#[ignore]
fn test_mcp_pure_list() {
    let json = run_mcp(&["pure [1,2,3 :: Int]"]);
    assert_eq!(json, serde_json::json!([1, 2, 3]));
}

#[test]
#[ignore]
fn test_mcp_pure_string() {
    let json = run_mcp(&["pure \"hello\""]);
    assert_eq!(json, serde_json::json!("hello"));
}

#[test]
#[ignore]
fn test_mcp_pure_bool() {
    let json = run_mcp(&["pure True"]);
    assert_eq!(json, serde_json::json!(true));
}

#[test]
#[ignore]
fn test_mcp_pure_pair() {
    let json = run_mcp(&["pure (1 :: Int, True)"]);
    // Pair rendered as 2-element array
    match json {
        serde_json::Value::Array(ref arr) if arr.len() == 2 => {}
        other => panic!("expected 2-tuple, got: {}", other),
    }
}

#[test]
#[ignore]
fn test_mcp_let_binding() {
    let json = run_mcp(&[
        "let x = 10 :: Int",
        "pure (x + 5)",
    ]);
    assert_eq!(json, serde_json::json!(15));
}

#[test]
#[ignore]
fn test_mcp_reverse() {
    let json = run_mcp(&["pure (reverse [1,2,3 :: Int])"]);
    assert_eq!(json, serde_json::json!([3, 2, 1]));
}

#[test]
#[ignore]
fn test_mcp_map() {
    let json = run_mcp(&["pure (map (+1) [1,2,3 :: Int])"]);
    assert_eq!(json, serde_json::json!([2, 3, 4]));
}

#[test]
#[ignore]
fn test_mcp_filter() {
    let json = run_mcp(&["pure (filter (> 2) [1,2,3,4,5 :: Int])"]);
    assert_eq!(json, serde_json::json!([3, 4, 5]));
}

#[test]
#[ignore]
fn test_mcp_words() {
    let json = run_mcp(&["pure (words \"hello world\")"]);
    assert_eq!(json, serde_json::json!(["hello", "world"]));
}

#[test]
#[ignore]
fn test_mcp_length() {
    let json = run_mcp(&["pure (length [10,20,30 :: Int])"]);
    assert_eq!(json, serde_json::json!(3));
}

#[test]
#[ignore]
fn test_mcp_take() {
    let json = run_mcp(&["pure (take 2 [1,2,3,4 :: Int])"]);
    assert_eq!(json, serde_json::json!([1, 2]));
}

#[test]
#[ignore]
fn test_mcp_string_append() {
    let json = run_mcp(&["pure (\"hello\" ++ \" world\")"]);
    assert_eq!(json, serde_json::json!("hello world"));
}

#[test]
#[ignore]
fn test_mcp_multi_line_do() {
    let json = run_mcp(&[
        "let xs = [1,2,3 :: Int]",
        "let ys = map (*2) xs",
        "pure ys",
    ]);
    assert_eq!(json, serde_json::json!([2, 4, 6]));
}

#[test]
#[ignore]
fn test_mcp_sort() {
    // Prelude sort pulls in Ord typeclass dictionaries that --all-closed
    // extraction doesn't fully resolve → Jit(MissingConTags).
    // This test will pass once the extraction bug is fixed.
    let json = run_mcp(&["pure (sort [3,1,2 :: Int])"]);
    assert_eq!(json, serde_json::json!([1, 2, 3]));
}

#[test]
#[ignore]
fn test_mcp_inline_sort() {
    // Inline sort (no Ord dictionary from prelude) — this worked in MCP.
    let json = run_mcp(&[
        "let { msort :: Ord a => [a] -> [a]; msort [] = []; msort [x] = [x]; msort xs = let (as,bs) = halve xs in merge (msort as) (msort bs); halve :: [a] -> ([a],[a]); halve [] = ([],[]); halve [x] = ([x],[]); halve (x:y:zs) = let (as,bs) = halve zs in (x:as, y:bs); merge :: Ord a => [a] -> [a] -> [a]; merge [] ys = ys; merge xs [] = xs; merge (x:xs) (y:ys) = if x <= y then x : merge xs (y:ys) else y : merge (x:xs) ys }",
        "pure (msort [3,1,2 :: Int])",
    ]);
    assert_eq!(json, serde_json::json!([1, 2, 3]));
}
