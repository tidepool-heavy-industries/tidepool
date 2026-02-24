/// Reproducer for MCP `pure (sort [3,1,2 :: Int])` crash and broader
/// freer-simple integration tests matching the exact source templates
/// the MCP server generates.
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use frunk::HNil;
use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_eval::value::Value;
use tidepool_effect::error::EffectError;
use tidepool_runtime::{compile_and_run, compile_and_run_pure, compile_haskell};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

/// Effect decls for tests — Console + KV + Fs (no SG, since tests don't have an SG handler).
fn test_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![tidepool_mcp::console_decl(), tidepool_mcp::kv_decl(), tidepool_mcp::fs_decl()]
}

/// Build the exact Haskell source the MCP server generates for a given
/// set of do-notation lines with Console/KV/Fs effects.
fn mcp_source(lines: &[&str]) -> String {
    mcp_source_with_helpers(lines, &[])
}

fn mcp_source_with_helpers(lines: &[&str], helpers: &[&str]) -> String {
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let helpers: Vec<String> = helpers.iter().map(|s| s.to_string()).collect();
    tidepool_mcp::template_haskell(&preamble, &stack, &lines, &[], &helpers)
}

fn mcp_source_with_imports(lines: &[&str], helpers: &[&str], imports: &[&str]) -> String {
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let helpers: Vec<String> = helpers.iter().map(|s| s.to_string()).collect();
    let imports: Vec<String> = imports.iter().map(|s| s.to_string()).collect();
    tidepool_mcp::template_haskell(&preamble, &stack, &lines, &imports, &helpers)
}

fn run_mcp_with_imports(lines: &[&str], helpers: &[&str], imports: &[&str]) -> serde_json::Value {
    let src = mcp_source_with_imports(lines, helpers, imports);
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

/// Build a plain (non-effect) Haskell module with the prelude.
fn plain_source(body: &str) -> String {
    format!(
r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T
import Control.Monad.Freer

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

use tidepool_codegen::jit_machine::JitEffectMachine;

/// Compile-only (no execution). Returns Ok(node_count) or Err(message).
fn compile_only(src: &str) -> Result<usize, String> {
    let pp = prelude_path();
    let src = src.to_owned();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let (expr, table) = compile_haskell(&src, "result", &include)
                .map_err(|e| format!("{:?}", e))?;
            // Also test JIT compilation
            let _machine = JitEffectMachine::compile(&expr, &table, 1 << 20)
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

// ---------------------------------------------------------------------------
// Effect handlers for testing
// ---------------------------------------------------------------------------

// Tag 0: Console effect
#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

struct TestConsole {
    lines: Arc<Mutex<Vec<String>>>,
}

impl TestConsole {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        (TestConsole { lines: lines.clone() }, lines)
    }
}

impl EffectHandler for TestConsole {
    type Request = ConsoleReq;
    fn handle(&mut self, req: ConsoleReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ConsoleReq::Print(s) => {
                self.lines.lock().unwrap().push(s);
                cx.respond(())
            }
        }
    }
}

// Tag 1: KV effect
#[derive(FromCore)]
enum KvReq {
    #[core(name = "KvGet")]
    Get(String),
    #[core(name = "KvSet")]
    Set(String, String),
    #[core(name = "KvDelete")]
    Delete(String),
    #[core(name = "KvKeys")]
    Keys,
}

struct TestKv {
    store: HashMap<String, String>,
}

impl TestKv {
    fn new() -> Self {
        TestKv { store: HashMap::new() }
    }
}

impl EffectHandler for TestKv {
    type Request = KvReq;
    fn handle(&mut self, req: KvReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            KvReq::Get(k) => {
                let val: Option<String> = self.store.get(&k).cloned();
                cx.respond(val)
            }
            KvReq::Set(k, v) => {
                self.store.insert(k, v);
                cx.respond(())
            }
            KvReq::Delete(k) => {
                self.store.remove(&k);
                cx.respond(())
            }
            KvReq::Keys => {
                let keys: Vec<String> = self.store.keys().cloned().collect();
                cx.respond(keys)
            }
        }
    }
}

// Tag 2: Fs effect (stub)
#[derive(FromCore)]
enum FsReq {
    #[core(name = "FsRead")]
    Read(String),
    #[core(name = "FsWrite")]
    Write(String, String),
}

struct TestFs;

impl EffectHandler for TestFs {
    type Request = FsReq;
    fn handle(&mut self, req: FsReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            FsReq::Read(_) => cx.respond("stub".to_string()),
            FsReq::Write(_, _) => cx.respond(()),
        }
    }
}

/// Run an effectful MCP test with real Console/KV/Fs handlers.
/// Returns (json_result, console_lines).
fn run_mcp_effectful(lines: &[&str]) -> (serde_json::Value, Vec<String>) {
    run_mcp_effectful_with_helpers(lines, &[])
}

fn run_mcp_effectful_with_helpers(lines: &[&str], helpers: &[&str]) -> (serde_json::Value, Vec<String>) {
    let src = mcp_source_with_helpers(lines, helpers);
    let pp = prelude_path();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let (console, captured) = TestConsole::new();
            let kv = TestKv::new();
            let mut handlers = frunk::hlist![console, kv, TestFs];
            let val = compile_and_run(&src, "result", &include, &mut handlers, &())
                .expect("compile_and_run failed");
            let json = val.to_json();
            let lines = captured.lock().unwrap().clone();
            (json, lines)
        })
        .unwrap()
        .join()
        .expect("thread panicked")
}

// ===========================================================================
// Plain (non-effect) prelude tests
// ===========================================================================

#[test]

fn test_plain_sort() {
    let json = run_plain("sort [3, 1, 2 :: Int]");
    assert_eq!(json, serde_json::json!([1, 2, 3]));
}

#[test]

fn test_eq_char() {
    let json = run_plain("'a' == 'a'");
    assert_eq!(json, serde_json::json!(true));
}

#[test]

fn test_eq_text_empty() {
    let json = run_plain("(\"\" :: Text) == \"\"");
    assert_eq!(json, serde_json::json!(true));
}

#[test]

fn test_and() {
    let json = run_plain("True && True");
    assert_eq!(json, serde_json::json!(true));
}

#[test]

fn test_case_eq_char() {
    let json = run_plain("case \"a\" of { (x:_) -> x == 'a'; [] -> False }");
    assert_eq!(json, serde_json::json!(true));
}

#[test]

fn test_eq_text_simple() {
    let json = run_plain("(\"a\" :: Text) == \"a\"");
    assert_eq!(json, serde_json::json!(true));
}

#[test]

fn test_eq_text_multi_char() {
    let json = run_plain("(\"hello\" :: Text) == \"hello\"");
    assert_eq!(json, serde_json::json!(true));
}

#[test]

fn test_eq_text_diff_content() {
    let json = run_plain("(\"abc\" :: Text) == \"abd\"");
    assert_eq!(json, serde_json::json!(false));
}

#[test]

fn test_eq_text_diff_length() {
    let json = run_plain("(\"ab\" :: Text) == \"abc\"");
    assert_eq!(json, serde_json::json!(false));
}

#[test]

fn test_eq_text_diff() {
    let json = run_plain("(\"a\" :: Text) == \"b\"");
    assert_eq!(json, serde_json::json!(false));
}

#[test]

fn test_show_int() {
    let json = run_plain("showInt (123 :: Int)");
    assert_eq!(json, serde_json::json!("123"));
}

#[test]

fn test_show_int_neg() {
    let json = run_plain("showInt (-456 :: Int)");
    assert_eq!(json, serde_json::json!("-456"));
}

// Bisect: show machinery frontier tests
#[test]

fn test_show_generic_int() {
    let json = run_plain("show (42 :: Int)");
    assert_eq!(json, serde_json::json!("42"));
}

#[test]

fn test_compile_show_char() {
    let src = plain_source("show 'a'");
    let count = compile_only(&src).expect("compile_only failed");
    println!("Compiled show 'a' to {} nodes", count);
}

#[test]

fn test_show_char() {
    let json = run_plain("show 'a'");
    assert_eq!(json, serde_json::json!("'a'"));
}

#[test]

fn test_show_string() {
    let json = run_plain("show \"hello\"");
    assert_eq!(json, serde_json::json!("\"hello\""));
}

#[test]

fn test_show_maybe_int() {
    let json = run_plain("show (Just 42 :: Maybe Int)");
    assert_eq!(json, serde_json::json!("Just 42"));
}

#[test]

fn test_show_maybe_string() {
    let json = run_plain("show (Just \"hello\" :: Maybe String)");
    assert_eq!(json, serde_json::json!("Just \"hello\""));
}

#[test]

fn test_nub_string() {
    // Test list equality via the Eq [Char] specialization substitute
    let json = run_plain("nub [\"a\", \"a\"]");
    assert_eq!(json, serde_json::json!(["a"]));
}

#[test]

fn test_intercalate() {
    let json = run_plain("intercalate \", \" [\"a\", \"b\", \"c\"]");
    assert_eq!(json, serde_json::json!("a, b, c"));
}

#[test]

fn test_is_prefix_of() {
    let json = run_plain("isPrefixOf \"abc\" \"abcdef\"");
    assert_eq!(json, serde_json::json!(true));
}

#[test]

fn test_replicate() {
    let json = run_plain("replicate 3 \"a\"");
    assert_eq!(json, serde_json::json!(["a", "a", "a"]));
}

// ===========================================================================
// MCP-style freer-simple tests (Eff '[Console, KV, Fs] _)
// ===========================================================================

#[test]

fn test_mcp_pure_lit() {
    let json = run_mcp(&["pure (42 :: Int)"]);
    assert_eq!(json, serde_json::json!(42));
}

#[test]

fn test_mcp_pure_list() {
    let json = run_mcp(&["pure [1,2,3 :: Int]"]);
    assert_eq!(json, serde_json::json!([1, 2, 3]));
}

#[test]

fn test_mcp_pure_string() {
    let json = run_mcp(&["pure \"hello\""]);
    assert_eq!(json, serde_json::json!("hello"));
}

#[test]

fn test_mcp_pure_bool() {
    let json = run_mcp(&["pure True"]);
    assert_eq!(json, serde_json::json!(true));
}

#[test]

fn test_mcp_pure_pair() {
    let json = run_mcp(&["pure (1 :: Int, True)"]);
    // Pair rendered as 2-element array
    match json {
        serde_json::Value::Array(ref arr) if arr.len() == 2 => {}
        other => panic!("expected 2-tuple, got: {}", other),
    }
}

#[test]

fn test_mcp_let_binding() {
    let json = run_mcp(&[
        "let x = 10 :: Int",
        "pure (x + 5)",
    ]);
    assert_eq!(json, serde_json::json!(15));
}

#[test]

fn test_mcp_reverse() {
    let json = run_mcp(&["pure (reverse [1,2,3 :: Int])"]);
    assert_eq!(json, serde_json::json!([3, 2, 1]));
}

#[test]

fn test_mcp_map() {
    let json = run_mcp(&["pure (map (+1) [1,2,3 :: Int])"]);
    assert_eq!(json, serde_json::json!([2, 3, 4]));
}

#[test]

fn test_mcp_filter() {
    let json = run_mcp(&["pure (filter (> 2) [1,2,3,4,5 :: Int])"]);
    assert_eq!(json, serde_json::json!([3, 4, 5]));
}

#[test]

fn test_mcp_words() {
    let json = run_mcp(&["pure (words \"hello world\")"]);
    assert_eq!(json, serde_json::json!(["hello", "world"]));
}

#[test]

fn test_mcp_length() {
    let json = run_mcp(&["pure (length [10,20,30 :: Int])"]);
    assert_eq!(json, serde_json::json!(3));
}

#[test]

fn test_mcp_take() {
    let json = run_mcp(&["pure (take 2 [1,2,3,4 :: Int])"]);
    assert_eq!(json, serde_json::json!([1, 2]));
}

#[test]

fn test_mcp_string_append() {
    let json = run_mcp(&["pure (\"hello\" ++ \" world\")"]);
    assert_eq!(json, serde_json::json!("hello world"));
}

#[test]

fn test_mcp_multi_line_do() {
    let json = run_mcp(&[
        "let xs = [1,2,3 :: Int]",
        "let ys = map (*2) xs",
        "pure ys",
    ]);
    assert_eq!(json, serde_json::json!([2, 4, 6]));
}

#[test]

fn test_mcp_sort() {
    // Prelude sort pulls in Ord typeclass dictionaries that --all-closed
    // extraction doesn't fully resolve → Jit(MissingConTags).
    // This test will pass once the extraction bug is fixed.
    let json = run_mcp(&["pure (sort [3,1,2 :: Int])"]);
    assert_eq!(json, serde_json::json!([1, 2, 3]));
}

#[test]

fn test_mcp_inline_sort() {
    // Inline sort (no Ord dictionary from prelude) — this worked in MCP.
    let json = run_mcp(&[
        "let { msort :: Ord a => [a] -> [a]; msort [] = []; msort [x] = [x]; msort xs = let (as,bs) = halve xs in merge (msort as) (msort bs); halve :: [a] -> ([a],[a]); halve [] = ([],[]); halve [x] = ([x],[]); halve (x:y:zs) = let (as,bs) = halve zs in (x:as, y:bs); merge :: Ord a => [a] -> [a] -> [a]; merge [] ys = ys; merge xs [] = xs; merge (x:xs) (y:ys) = if x <= y then x : merge xs (y:ys) else y : merge (x:xs) ys }",
        "pure (msort [3,1,2 :: Int])",
    ]);
    assert_eq!(json, serde_json::json!([1, 2, 3]));
}

// ===========================================================================
// Effectful tests (Console/KV/Fs handlers with real dispatch)
// ===========================================================================

#[test]

fn test_effect_kv_set_get() {
    // The original bug: () return from KvSet must be TAG_CON
    let (json, _) = run_mcp_effectful(&[
        "send (KvSet \"k\" \"v\")",
        "send (KvGet \"k\")",
    ]);
    assert_eq!(json, serde_json::json!("v")); // Just "v" → unwrapped
}

#[test]

fn test_effect_kv_get_missing() {
    let (json, _) = run_mcp_effectful(&[
        "send (KvGet \"nope\")",
    ]);
    assert_eq!(json, serde_json::json!(null)); // Nothing → null
}

#[test]

fn test_effect_kv_delete_then_get() {
    let (json, _) = run_mcp_effectful(&[
        "send (KvSet \"k\" \"v\")",
        "send (KvDelete \"k\")",
        "send (KvGet \"k\")",
    ]);
    assert_eq!(json, serde_json::json!(null)); // Nothing after delete
}

#[test]

fn test_effect_kv_keys() {
    let (json, _) = run_mcp_effectful(&[
        "send (KvSet \"a\" \"1\")",
        "send (KvSet \"b\" \"2\")",
        "send (KvSet \"c\" \"3\")",
        "send KvKeys",
    ]);
    // Keys come back as a list, order may vary
    let arr = json.as_array().expect("expected array");
    assert_eq!(arr.len(), 3);
    let mut keys: Vec<String> = arr.iter().map(|v| v.as_str().unwrap().to_string()).collect();
    keys.sort();
    assert_eq!(keys, vec!["a", "b", "c"]);
}

#[test]

fn test_effect_console_print() {
    let (json, lines) = run_mcp_effectful(&[
        "send (Print \"hello\")",
        "pure (42 :: Int)",
    ]);
    assert_eq!(json, serde_json::json!(42));
    assert_eq!(lines, vec!["hello"]);
}

#[test]

fn test_effect_console_multi_print() {
    let (json, lines) = run_mcp_effectful(&[
        "send (Print \"a\")",
        "send (Print \"b\")",
        "send (Print \"c\")",
        "pure \"done\"",
    ]);
    assert_eq!(json, serde_json::json!("done"));
    assert_eq!(lines, vec!["a", "b", "c"]);
}

#[test]

fn test_effect_mixed_console_kv() {
    let (json, lines) = run_mcp_effectful(&[
        "send (Print \"storing\")",
        "send (KvSet \"x\" \"42\")",
        "v <- send (KvGet \"x\")",
        "send (Print \"loaded\")",
        "pure v",
    ]);
    assert_eq!(json, serde_json::json!("42")); // Just "42" → unwrapped
    assert_eq!(lines, vec!["storing", "loaded"]);
}

#[test]

fn test_effect_kv_conditional() {
    let (json, _) = run_mcp_effectful(&[
        "send (KvSet \"flag\" \"yes\")",
        "v <- send (KvGet \"flag\")",
        "case v of { Just _ -> send (KvSet \"result\" \"found\"); Nothing -> send (KvSet \"result\" \"missing\") }",
        "send (KvGet \"result\")",
    ]);
    assert_eq!(json, serde_json::json!("found")); // Just "found" → unwrapped
}

#[test]

fn test_effect_kv_overwrite() {
    let (json, _) = run_mcp_effectful(&[
        "send (KvSet \"k\" \"old\")",
        "send (KvSet \"k\" \"new\")",
        "send (KvGet \"k\")",
    ]);
    assert_eq!(json, serde_json::json!("new")); // Just "new" → unwrapped
}

#[test]

fn test_effect_words() {
    // FsRead returns "stub" by default in our mock
    let (json, _) = run_mcp_effectful(&[
        "s <- send (FsRead \"file.txt\")",
        "pure (words s)",
    ]);
    assert_eq!(json, serde_json::json!(["stub"]));
}

#[test]

fn test_effect_words_custom() {
    let (json, _) = run_mcp_effectful(&[
        "pure (words \"  hello   world  \")",
    ]);
    assert_eq!(json, serde_json::json!(["hello", "world"]));
}

// ===========================================================================
// Helper function tests (multi-arg lambdas calling effects)
// ===========================================================================

#[test]
fn test_effect_helper_two_args() {
    // Reproducer: two-arg helper function that calls KvGet + case + KvSet/FsWrite.
    // Panicked with "no entry found for key" (HashMap Index on missing VarId).
    let (json, console) = run_mcp_effectful_with_helpers(
        &[
            "send (KvSet \"a\" \"data\")",
            "persist \"a\" \"out.txt\"",
            "pure \"done\"",
        ],
        &[
r#"persist :: Text -> Text -> Eff '[Console, KV, Fs] ()
persist key filename = do
  val <- send (KvGet key)
  case val of
    Nothing -> send (Print "nothing")
    Just content -> send (FsWrite filename content)"#,
        ],
    );
    assert_eq!(json, serde_json::json!("done"));
}

// ===========================================================================
// Fat interface Rec group tests (cross-module join points)
// ===========================================================================

#[test]
fn test_map_difference_with() {
    // Map.differenceWith uses fat interface bindings that may contain
    // Rec groups with join points. Previously failed with
    // "Jump to unknown label JoinId(...)" because Rec groups were
    // flattened and join point siblings were lost.
    let json = run_mcp_with_imports(
        &[
            "let m1 = Map.fromList [(1::Int,10::Int),(2,20),(3,30)]",
            "let m2 = Map.fromList [(2::Int,15::Int),(3,40)]",
            "let m' = Map.differenceWith (\\a b -> if a > b then Just (a - b) else Nothing) m1 m2",
            "pure (Map.toList m')",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json, serde_json::json!([[1, 10], [2, 5]]));
}

#[test]
fn test_map_intersection_with() {
    // Another Map operation that exercises fat interface Rec groups.
    let json = run_mcp_with_imports(
        &[
            "let m1 = Map.fromList [(1::Int,10::Int),(2,20),(3,30)]",
            "let m2 = Map.fromList [(2::Int,100::Int),(3,200)]",
            "pure (Map.toList (Map.intersectionWith (+) m1 m2))",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json, serde_json::json!([[2, 120], [3, 230]]));
}

#[test]
fn test_map_find_with_default() {
    let json = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int),(3,30),(5,50)]",
            "pure (Map.findWithDefault 0 3 m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json, serde_json::json!(30));
}

#[test]
fn test_map_find_with_default_missing() {
    // findWithDefault when key is absent — exercises the default-value branch
    let json = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int),(3,30),(5,50)]",
            "pure (Map.findWithDefault 999 2 m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json, serde_json::json!(999));
}

#[test]
fn test_map_alter_insert() {
    // Map.alter with a lambda that inserts — lambda crosses join boundary
    let json = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int),(3,30)]",
            "let m' = Map.alter (\\_ -> Just 20) 2 m",
            "pure (Map.toList m')",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json, serde_json::json!([[1, 10], [2, 20], [3, 30]]));
}

#[test]
fn test_map_alter_delete() {
    // Map.alter with a lambda that deletes
    let json = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int),(2,20),(3,30)]",
            "let m' = Map.alter (\\_ -> Nothing) 2 m",
            "pure (Map.toList m')",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json, serde_json::json!([[1, 10], [3, 30]]));
}

#[test]
fn test_map_update_with_key() {
    // Map.updateWithKey passes a (key, value) -> Maybe value lambda into tree walk
    let json = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,100::Int),(2,200),(3,300)]",
            "let m' = Map.updateWithKey (\\k v -> if k == 2 then Nothing else Just (v + k)) 2 m",
            "pure (Map.toList m')",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json, serde_json::json!([[1, 100], [3, 300]]));
}

#[test]
fn test_map_map_with_key() {
    // Map.mapWithKey — transforms values using key, lambda crosses join boundary
    let json = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int),(2,20),(3,30)]",
            "pure (Map.toList (Map.mapWithKey (\\k v -> k * 100 + v) m))",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json, serde_json::json!([[1, 110], [2, 220], [3, 330]]));
}

#[test]
fn test_map_foldr_with_key() {
    // Map.foldrWithKey — fold with 3-arg lambda (k -> v -> acc -> acc)
    let json = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int),(2,20),(3,30)]",
            "pure (Map.foldrWithKey (\\k v acc -> acc + k * v) (0::Int) m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    // 1*10 + 2*20 + 3*30 = 10 + 40 + 90 = 140
    assert_eq!(json, serde_json::json!(140));
}

#[test]
fn test_map_union_with_key() {
    // Map.unionWithKey — merge with key-dependent combining function
    let json = run_mcp_with_imports(
        &[
            "let m1 = Map.fromList [(1::Int,10::Int),(2,20)]",
            "let m2 = Map.fromList [(2::Int,200::Int),(3,300)]",
            "pure (Map.toList (Map.unionWithKey (\\k a b -> k + a + b) m1 m2))",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    // key 1: only in m1 -> 10
    // key 2: both -> 2 + 20 + 200 = 222
    // key 3: only in m2 -> 300
    assert_eq!(json, serde_json::json!([[1, 10], [2, 222], [3, 300]]));
}

#[test]
fn test_map_filter_with_key() {
    // Map.filterWithKey — predicate lambda crossing join boundary
    let json = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int),(2,20),(3,30),(4,40)]",
            "pure (Map.toList (Map.filterWithKey (\\k v -> k + v > 25) m))",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    // k=1,v=10: 11 <= 25 no; k=2,v=20: 22 <= 25 no; k=3,v=30: 33 > 25 yes; k=4,v=40: 44 > 25 yes
    assert_eq!(json, serde_json::json!([[3, 30], [4, 40]]));
}

#[test]
fn test_map_over_string_literal() {
    let json = run_mcp(&[
        "pure (map (\\c -> chr (ord c + 1)) \"Hello\")",
    ]);
    assert_eq!(json, serde_json::json!("Ifmmp"));
}

#[test]
fn test_filter_string_literal() {
    let json = run_plain("filter (\\c -> c /= 'l') \"Hello World\"");
    assert_eq!(json, serde_json::json!("Heo Word"));
}

#[test]
#[ignore] // SIGILL on main — pre-existing issue
fn test_show_4_tuple() {
    let json = run_plain("show (1::Int, True, 'x', \"hi\" :: Text)");
    assert_eq!(json, serde_json::json!("(1,True,'x',\"hi\")"));
}
