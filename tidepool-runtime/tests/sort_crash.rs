use frunk::HNil;
/// Reproducer for MCP `pure (sort [3,1,2 :: Int])` crash and broader
/// freer-simple integration tests matching the exact source templates
/// the MCP server generates.
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
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
    vec![
        tidepool_mcp::console_decl(),
        tidepool_mcp::kv_decl(),
        tidepool_mcp::fs_decl(),
    ]
}

/// Build the exact Haskell source the MCP server generates for a given
/// set of do-notation lines with Console/KV/Fs effects.
fn mcp_source(lines: &[&str]) -> String {
    mcp_source_with_helpers(lines, &[])
}

fn mcp_source_with_helpers(lines: &[&str], helpers: &[&str]) -> String {
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let helpers: Vec<String> = helpers.iter().map(|s| s.to_string()).collect();
    tidepool_mcp::template_haskell(&preamble, &stack, &lines, &[], &helpers, None, None)
}

fn mcp_source_with_imports(lines: &[&str], helpers: &[&str], imports: &[&str]) -> String {
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let helpers: Vec<String> = helpers.iter().map(|s| s.to_string()).collect();
    let imports: Vec<String> = imports.iter().map(|s| s.to_string()).collect();
    tidepool_mcp::template_haskell(&preamble, &stack, &lines, &imports, &helpers, None, None)
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
"#
    )
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
            let (expr, table, _) =
                compile_haskell(&src, "result", &include).map_err(|e| format!("{:?}", e))?;
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
        (
            TestConsole {
                lines: lines.clone(),
            },
            lines,
        )
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
    Set(String, Value),
    #[core(name = "KvDelete")]
    Delete(String),
    #[core(name = "KvKeys")]
    Keys,
}

struct TestKv {
    store: HashMap<String, Value>,
}

impl TestKv {
    fn new() -> Self {
        TestKv {
            store: HashMap::new(),
        }
    }
}

impl EffectHandler for TestKv {
    type Request = KvReq;
    fn handle(&mut self, req: KvReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            KvReq::Get(k) => {
                let val: Option<Value> = self.store.get(&k).cloned();
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
#[allow(dead_code)]
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

fn run_mcp_effectful_with_helpers(
    lines: &[&str],
    helpers: &[&str],
) -> (serde_json::Value, Vec<String>) {
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
// Aeson / lens-aeson helpers
// ===========================================================================

fn aeson_import_strs() -> Vec<&'static str> {
    // Unqualified aeson/lens symbols now come from Tidepool.Prelude.
    // Only qualified imports needed here.
    vec![
        "qualified Tidepool.Aeson as Aeson",
        "qualified Tidepool.Aeson.KeyMap as KM",
    ]
}

fn run_aeson(lines: &[&str]) -> serde_json::Value {
    run_mcp_with_imports(lines, &[], &aeson_import_strs())
}

fn run_aeson_with_helpers(lines: &[&str], helpers: &[&str]) -> serde_json::Value {
    run_mcp_with_imports(lines, helpers, &aeson_import_strs())
}

fn run_aeson_effectful(lines: &[&str]) -> (serde_json::Value, Vec<String>) {
    run_aeson_effectful_with_helpers(lines, &[])
}

fn run_aeson_effectful_with_helpers(
    lines: &[&str],
    helpers: &[&str],
) -> (serde_json::Value, Vec<String>) {
    let imports: Vec<&str> = aeson_import_strs();
    let src = mcp_source_with_imports(lines, helpers, &imports);
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

fn run_aeson_effectful_with_input(
    lines: &[&str],
    helpers: &[&str],
    input: serde_json::Value,
) -> (serde_json::Value, Vec<String>) {
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines_owned: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let imports: Vec<String> = aeson_import_strs().iter().map(|s| s.to_string()).collect();
    let helpers_owned: Vec<String> = helpers.iter().map(|s| s.to_string()).collect();
    let src = tidepool_mcp::template_haskell(
        &preamble,
        &stack,
        &lines_owned,
        &imports,
        &helpers_owned,
        Some(&input),
        None,
    );
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

fn run_aeson_with_input(lines: &[&str], input: serde_json::Value) -> serde_json::Value {
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines_owned: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let imports: Vec<String> = aeson_import_strs().iter().map(|s| s.to_string()).collect();
    let src = tidepool_mcp::template_haskell(
        &preamble,
        &stack,
        &lines_owned,
        &imports,
        &[],
        Some(&input),
        None,
    );
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

// ===========================================================================
// Aeson / lens-aeson tests
// ===========================================================================

// --- Basic construction ---

/// Construct a simple JSON object with object/.=
#[test]
fn test_aeson_object_simple() {
    let json = run_aeson(&[r#"pure (object ["name" .= ("Alice" :: Text), "age" .= (30 :: Int)])"#]);
    // Object now renders as a proper JSON object
    assert!(json.is_object());
}

/// Construct a JSON array via toJSON
#[test]
fn test_aeson_array_tojson() {
    let json = run_aeson(&[r#"pure (toJSON [1, 2, 3 :: Int])"#]);
    // aeson's toJSON [Int] produces Array (Vector Value)
    // Our bridge should traverse the Vector's Array# internals
    assert!(!json.is_null(), "toJSON [1,2,3] should not be null");
}

/// Construct Aeson.Null
#[test]
fn test_aeson_null() {
    let json = run_aeson(&[r#"pure Aeson.Null"#]);
    assert_eq!(json, serde_json::json!(null));
}

/// Construct Aeson.Bool
#[test]
fn test_aeson_bool_true() {
    let json = run_aeson(&[r#"pure (Aeson.Bool True)"#]);
    assert_eq!(json, serde_json::json!(true));
}

/// Construct Aeson.String
#[test]
fn test_aeson_string() {
    let json = run_aeson(&[r#"pure (Aeson.String "hello world")"#]);
    assert_eq!(json, serde_json::json!("hello world"));
}

/// Construct Aeson.Number from Int
#[test]
fn test_aeson_number_int() {
    let json = run_aeson(&[r#"pure (toJSON (42 :: Int))"#]);
    // toJSON Int produces Number (Scientific)
    assert!(!json.is_null());
}

// --- lens-aeson key access ---

/// Use (^?) key to extract a string from an object
#[test]
fn test_aeson_lens_key_string() {
    let json = run_aeson(&[
        r#"let obj = object ["name" .= ("Alice" :: Text), "age" .= (30 :: Int)]"#,
        r#"pure (obj ^? key "name" . _String)"#,
    ]);
    // Should be Just "Alice" → renders as "Alice"
    assert_eq!(json, serde_json::json!("Alice"));
}

/// Use (^?) key to extract an integer from an object
#[test]
fn test_aeson_lens_key_integer() {
    let json = run_aeson(&[
        r#"let obj = object ["x" .= (42 :: Int), "y" .= (99 :: Int)]"#,
        r#"pure (obj ^? key "x" . _Number)"#,
    ]);
    assert_eq!(json, serde_json::json!(42));
}

/// Key lookup on missing field returns Nothing → null
#[test]
fn test_aeson_lens_key_missing() {
    let json = run_aeson(&[
        r#"let obj = object ["x" .= (1 :: Int)]"#,
        r#"pure (obj ^? key "missing" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!(null));
}

/// Nested key access: key "a" . key "b"
#[test]
fn test_aeson_lens_nested_key() {
    let json = run_aeson(&[
        r#"let inner = object ["b" .= ("deep" :: Text)]"#,
        r#"let outer = object ["a" .= inner]"#,
        r#"pure (outer ^? key "a" . key "b" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!("deep"));
}

/// Array indexing with nth
#[test]
fn test_aeson_lens_nth() {
    let json = run_aeson(&[
        r#"let arr = toJSON [10, 20, 30 :: Int]"#,
        r#"pure (arr ^? nth 1 . _Number)"#,
    ]);
    assert_eq!(json, serde_json::json!(20));
}

/// Use (^..) to extract all strings from an object
#[test]
fn test_aeson_lens_traverse_strings() {
    let json = run_aeson(&[
        r#"let obj = object ["a" .= ("x" :: Text), "b" .= (1 :: Int), "c" .= ("y" :: Text)]"#,
        r#"pure (obj ^.. key "a" . _String)"#,
    ]);
    // ^.. on a single key gives a list of 0 or 1
    assert_eq!(json, serde_json::json!(["x"]));
}

// --- Modification via lens ---

/// Use (.~) to set a field value (uses _String to avoid Scientific/GMP)
#[test]
fn test_aeson_lens_set_field() {
    let json = run_aeson(&[
        r#"let obj = object ["x" .= ("old" :: Text)]"#,
        r#"let modified = obj & key "x" . _String .~ "new""#,
        r#"pure (modified ^? key "x" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!("new"));
}

/// Use (%~) to modify a field value (uses _String to avoid Scientific/GMP)
#[test]
fn test_aeson_lens_modify_field() {
    let json = run_aeson(&[
        r#"let obj = object ["greeting" .= ("hello" :: Text)]"#,
        r#"let modified = obj & key "greeting" . _String %~ T.toUpper"#,
        r#"pure (modified ^? key "greeting" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!("HELLO"));
}

// --- Multi-stage: construct, inspect, transform ---

/// Build object, extract field, use it in computation
#[test]
fn test_aeson_multistage_extract_compute() {
    let json = run_aeson_with_helpers(
        &[
            r#"let person = object ["name" .= ("Alice" :: Text), "score" .= (85 :: Int)]"#,
            r#"let mScore = person ^? key "score" . _Number"#,
            r#"pure mScore"#,
        ],
        &[],
    );
    assert_eq!(json, serde_json::json!(85));
}

/// Build nested JSON, modify inner field, extract result (uses _String to avoid Scientific/GMP)
#[test]
fn test_aeson_multistage_nested_modify() {
    let json = run_aeson(&[
        r#"let config = object ["db" .= object ["host" .= ("localhost" :: Text), "env" .= ("dev" :: Text)]]"#,
        r#"let updated = config & key "db" . key "env" . _String .~ "prod""#,
        r#"pure (updated ^? key "db" . key "env" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!("prod"));
}

// --- Multi-effect: aeson + Console + KV ---

/// Print a JSON field via Console effect
#[test]
fn test_aeson_effect_print_field() {
    let (json, console) = run_aeson_effectful(&[
        r#"let person = object ["name" .= ("Bob" :: Text)]"#,
        r#"case person ^? key "name" . _String of"#,
        r#"  Just n  -> send (Print n)"#,
        r#"  Nothing -> send (Print "unknown")"#,
        r#"pure (42 :: Int)"#,
    ]);
    assert_eq!(json, serde_json::json!(42));
    assert_eq!(console, vec!["Bob"]);
}

/// Store JSON string in KV, retrieve and lens into it
#[test]
fn test_aeson_effect_kv_store_retrieve() {
    let (json, _) = run_aeson_effectful(&[
        r#"kvSet "data" (toJSON ("hello world" :: Text))"#,
        r#"raw <- send (KvGet "data")"#,
        r#"pure raw"#,
    ]);
    // Test KV store/retrieve roundtrip with Text values
    assert_eq!(json, serde_json::json!("hello world"));
}

/// Build aeson Value, print multiple fields via Console
#[test]
fn test_aeson_effect_multi_print() {
    let (json, console) = run_aeson_effectful(&[
        r#"let record = object ["first" .= ("Jane" :: Text), "last" .= ("Doe" :: Text), "age" .= (28 :: Int)]"#,
        r#"case record ^? key "first" . _String of"#,
        r#"  Just f -> send (Print f)"#,
        r#"  Nothing -> pure ()"#,
        r#"case record ^? key "last" . _String of"#,
        r#"  Just l -> send (Print l)"#,
        r#"  Nothing -> pure ()"#,
        r#"pure (record ^? key "age" . _Number)"#,
    ]);
    assert_eq!(json, serde_json::json!(28));
    assert_eq!(console, vec!["Jane", "Doe"]);
}

// --- JSON input injection ---

/// Use the `input` field to inject a JSON value
#[test]
fn test_aeson_input_simple() {
    let json = run_aeson_with_input(
        &[r#"pure (input ^? key "name" . _String)"#],
        serde_json::json!({"name": "Alice", "age": 30}),
    );
    assert_eq!(json, serde_json::json!("Alice"));
}

/// Input injection with nested object
#[test]
fn test_aeson_input_nested() {
    let json = run_aeson_with_input(
        &[r#"pure (input ^? key "config" . key "port" . _Number)"#],
        serde_json::json!({"config": {"port": 8080, "host": "localhost"}}),
    );
    assert_eq!(json, serde_json::json!(8080));
}

/// Input injection with array access
#[test]
fn test_aeson_input_array() {
    let json = run_aeson_with_input(
        &[r#"pure (input ^? nth 2 . _String)"#],
        serde_json::json!(["a", "b", "c", "d"]),
    );
    assert_eq!(json, serde_json::json!("c"));
}

/// Input injection with multi-stage transformation
#[test]
fn test_aeson_input_transform() {
    let json = run_aeson_with_input(
        &[
            r#"let scores = input ^.. key "scores" . _Array . traverse . _Number"#,
            r#"pure (scores)"#,
        ],
        serde_json::json!({"scores": [10, 20, 30]}),
    );
    assert_eq!(json, serde_json::json!([10, 20, 30]));
}

/// Input injection with effect: extract from input, print, return
#[test]
fn test_aeson_input_with_effect() {
    let input_val = serde_json::json!({"greeting": "Hello from JSON!"});
    let imports: Vec<&str> = aeson_import_strs();
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines = vec![
        r#"case input ^? key "greeting" . _String of"#.to_string(),
        r#"  Just g  -> send (Print g)"#.to_string(),
        r#"  Nothing -> pure ()"#.to_string(),
        r#"pure (input ^? key "greeting" . _String)"#.to_string(),
    ];
    let imports_owned: Vec<String> = imports.iter().map(|s| s.to_string()).collect();
    let src = tidepool_mcp::template_haskell(
        &preamble,
        &stack,
        &lines,
        &imports_owned,
        &[],
        Some(&input_val),
        None,
    );
    let pp = prelude_path();
    let result = std::thread::Builder::new()
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
        .expect("thread panicked");
    assert_eq!(result.0, serde_json::json!("Hello from JSON!"));
    assert_eq!(result.1, vec!["Hello from JSON!"]);
}

// --- Helper-driven complex patterns ---

/// Use a top-level helper function over aeson Values
#[test]
fn test_aeson_helper_extract_names() {
    let json = run_aeson_with_helpers(
        &[
            r#"let people = toJSON [object ["name" .= ("Alice" :: Text)], object ["name" .= ("Bob" :: Text)]]"#,
            r#"pure (extractNames people)"#,
        ],
        &[
            "extractNames :: Aeson.Value -> [Text]\nextractNames v = v ^.. _Array . traverse . key \"name\" . _String",
        ],
    );
    assert_eq!(json, serde_json::json!(["Alice", "Bob"]));
}

/// Complex pipeline: build, count items via helper
#[test]
fn test_aeson_helper_pipeline() {
    let json = run_aeson_with_helpers(
        &[
            r#"let items = toJSON [object ["name" .= (n :: Text)] | n <- ["alice", "bob", "carol"]]"#,
            r#"pure (countItems items)"#,
        ],
        &[
            "countItems :: Aeson.Value -> Int\ncountItems v = length (v ^.. _Array . traverse . key \"name\" . _String)",
        ],
    );
    assert_eq!(json, serde_json::json!(3));
}

// --- Deep-nesting regression (CBOR recursion limit) ---

/// _Int lens on object field — now uses Double internally (no Scientific/GMP).
#[test]
fn test_aeson_lens_int_prism() {
    let json = run_aeson(&[
        r#"let obj = object ["x" .= (42 :: Int)]"#,
        r#"pure (obj ^? key "x" . _Int)"#,
    ]);
    assert_eq!(json, serde_json::json!(42));
}

/// _Bool lens on object field
#[test]
fn test_aeson_lens_bool_prism() {
    let json = run_aeson(&[
        r#"let obj = object ["flag" .= True]"#,
        r#"pure (obj ^? key "flag" . _Bool)"#,
    ]);
    assert_eq!(json, serde_json::json!(true));
}

/// traverse over _Array to extract all strings — deep nesting from Vector + lens
#[test]
fn test_aeson_lens_array_traverse_strings() {
    let json = run_aeson(&[
        r#"let obj = object ["tags" .= (["a", "b", "c"] :: [Text])]"#,
        r#"pure (obj ^.. key "tags" . _Array . traverse . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!(["a", "b", "c"]));
}

/// Multi-key extraction from nested object — deep composition chain
#[test]
fn test_aeson_lens_nested_deep() {
    let json = run_aeson(&[
        r#"let v = object ["meta" .= object ["ver" .= (1 :: Int), "ok" .= True]]"#,
        r#"let ver = v ^? key "meta" . key "ver" . _Number"#,
        r#"let ok = v ^? key "meta" . key "ok" . _Bool"#,
        r#"pure (ver, ok)"#,
    ]);
    assert_eq!(json, serde_json::json!([1, true]));
}

/// _Number now returns Double directly (no Scientific).
#[test]
fn test_aeson_number_prism_double() {
    let json = run_aeson(&[
        r#"let s = toJSON (42 :: Int)"#,
        r#"let n = s ^? _Number"#,
        r#"pure n"#,
    ]);
    assert_eq!(json, serde_json::json!(42));
}

/// _Double prism — now trivially wraps _Number (no Scientific).
#[test]
fn test_aeson_lens_double_prism() {
    let json = run_aeson(&[
        r#"let obj = object ["x" .= (3.14 :: Double)]"#,
        r#"pure (obj ^? key "x" . _Double)"#,
    ]);
    assert_eq!(json, serde_json::json!(3.14));
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
fn test_plain_map_fromlist() {
    // Plain (non-effect) Map.fromList to isolate whether the error is from
    // effect wrapping or from the map code itself.
    let src = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T
import Control.Monad.Freer
import qualified Data.Map.Strict as Map

result :: _
result = Map.toList (Map.fromList [(1::Int,10::Int),(2,20)])
"#;
    let pp = prelude_path();
    let src = src.to_owned();
    let json = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let val = compile_and_run_pure(&src, "result", &include)
                .expect("compile_and_run_pure failed");
            val.to_json()
        })
        .unwrap()
        .join()
        .expect("thread panicked");
    assert_eq!(json, serde_json::json!([[1, 10], [2, 20]]));
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
    let json = run_plain("show (123 :: Int)");
    assert_eq!(json, serde_json::json!("123"));
}

#[test]

fn test_show_int_neg() {
    let json = run_plain("show (-456 :: Int)");
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
fn test_show_aeson_value() {
    // Regression: derived Show for Value crashed on Number !Double due to
    // $fShowDouble_$sshowSignedFloat pulling in floatToDigits/Integer.
    // Fixed by intercepting at binding level in Translate.hs.
    let (json, logs) = run_mcp_effectful(&["let v = toJSON (42 :: Int)", "say (show v)", "pure v"]);
    assert_eq!(json, serde_json::json!(42));
    assert_eq!(logs, vec!["Number 42.0"]);
}

#[test]
fn test_show_value_in_list() {
    // ShowS continuation must be preserved for show on lists of Values.
    let (json, _logs) = run_mcp_effectful(&[
        r#"let vs = [toJSON (1 :: Int), toJSON True]"#,
        r#"pure (toJSON (show vs))"#,
    ]);
    assert_eq!(json, serde_json::json!("[Number 1.0,Bool True]"));
}

#[test]
fn test_tojson_string() {
    // toJSON @String should produce String "hello", not Array of chars.
    let (json, _logs) = run_mcp_effectful(&[r#"pure (toJSON ("hello" :: String))"#]);
    assert_eq!(json, serde_json::json!("hello"));
}

#[test]
fn test_value_safe_lookup() {
    // (?.) operator for safe key lookup on Value objects.
    let (json, _logs) = run_mcp_effectful(&[
        r#"let v = object ["x" .= (42 :: Int), "y" .= True]"#,
        r#"pure (toJSON (v ?. "x", v ?. "missing"))"#,
    ]);
    assert_eq!(json, serde_json::json!([42, null]));
}

#[test]
fn test_lookupkey_nested() {
    // Monadic chaining with lookupKey for deep access.
    let (json, _logs) = run_mcp_effectful(&[
        r#"let v = object ["a" .= object ["b" .= (99 :: Int)]]"#,
        r#"let r = do { a <- lookupKey "a" v; lookupKey "b" a }"#,
        r#"pure (toJSON r)"#,
    ]);
    assert_eq!(json, serde_json::json!(99));
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

// ---------------------------------------------------------------------------
// Full MCP preamble (all 8 effects) — matches the real MCP server exactly
// ---------------------------------------------------------------------------

fn full_mcp_decls() -> Vec<tidepool_mcp::EffectDecl> {
    tidepool_mcp::standard_decls()
}

fn full_mcp_source(lines: &[&str]) -> String {
    let decls = full_mcp_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let imports = tidepool_mcp::aeson_imports();
    tidepool_mcp::template_haskell(&preamble, &stack, &lines, &imports, &[], None, None)
}

fn run_full_mcp(lines: &[&str]) -> serde_json::Value {
    let src = full_mcp_source(lines);
    let pp = prelude_path();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            eprintln!("[test] compile_and_run starting...");
            let val = compile_and_run(&src, "result", &include, &mut HNil, &())
                .expect("compile_and_run failed");
            eprintln!("[test] compile_and_run succeeded");
            val.to_json()
        })
        .unwrap()
        .join()
        .expect("thread panicked")
}

/// Full MCP preamble (all 8 effects): pure (42 :: Int)
#[test]
fn test_full_mcp_pure_42() {
    let json = run_full_mcp(&["pure (42 :: Int)"]);
    assert_eq!(json, serde_json::json!(42));
}

/// Full MCP preamble with signal_safety::install() — matches real MCP server
#[test]
fn test_full_mcp_with_signal_install() {
    let src = full_mcp_source(&["pure (42 :: Int)"]);
    let pp = prelude_path();
    let json = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            // This is what the MCP server does before compile_and_run
            tidepool_codegen::signal_safety::install();
            let include = [pp.as_path()];
            eprintln!("[test+signal] compile_and_run starting...");
            let val = compile_and_run(&src, "result", &include, &mut HNil, &())
                .expect("compile_and_run failed");
            eprintln!("[test+signal] compile_and_run succeeded");
            val.to_json()
        })
        .unwrap()
        .join()
        .expect("thread panicked");
    assert_eq!(json, serde_json::json!(42));
}

/// Full MCP preamble with the same tokio+channel pattern the MCP server uses.
/// This exactly mirrors tidepool-mcp/src/lib.rs:1142-1222.
#[tokio::test]
async fn test_full_mcp_async_channel_pattern() {
    let src = full_mcp_source(&["pure (42 :: Int)"]);
    let pp = prelude_path();

    // Same channel types as MCP server
    let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    let handle = std::thread::Builder::new()
        .name("tidepool-eval".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            let include = [pp.as_path()];
            eprintln!("[test+async] compile_and_run starting...");
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                compile_and_run(&src, "result", &include, &mut HNil, &())
            }));
            eprintln!("[test+async] compile_and_run finished");
            match result {
                Ok(Ok(val)) => {
                    let json = val.to_json();
                    let _ = session_tx.send(format!("OK:{}", json));
                }
                Ok(Err(e)) => {
                    let _ = session_tx.send(format!("ERR:{}", e));
                }
                Err(panic) => {
                    let msg = if let Some(s) = panic.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = panic.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "unknown panic".into()
                    };
                    let _ = session_tx.send(format!("PANIC:{}", msg));
                }
            }
        })
        .unwrap();

    // Same timeout pattern as MCP server (30s)
    let eval_timeout = tokio::time::Duration::from_secs(30);
    match tokio::time::timeout(eval_timeout, session_rx.recv()).await {
        Ok(Some(msg)) => {
            eprintln!("[test+async] received: {}", msg);
            assert!(msg.starts_with("OK:"), "expected OK, got: {}", msg);
            let json: serde_json::Value = serde_json::from_str(&msg[3..]).unwrap();
            assert_eq!(json, serde_json::json!(42));
        }
        Ok(None) => panic!("channel closed without message (thread crashed)"),
        Err(_) => panic!("TIMEOUT: eval did not complete in 30s"),
    }
    handle.join().expect("eval thread panicked");
}

/// Full MCP preamble: pure string
#[test]
fn test_full_mcp_pure_string() {
    let json = run_full_mcp(&["pure \"hello\""]);
    assert_eq!(json, serde_json::json!("hello"));
}

/// Full MCP preamble: pure list
#[test]
fn test_full_mcp_pure_list() {
    let json = run_full_mcp(&["pure [1,2,3 :: Int]"]);
    assert_eq!(json, serde_json::json!([1, 2, 3]));
}

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
    let json = run_mcp(&["let x = 10 :: Int", "pure (x + 5)"]);
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
    // Use Text append (<>) instead of String (++) — toJSON @String uses the [a]
    // instance which maps toJSON per-char, while toJSON @Text produces a single string.
    let json = run_mcp(&["pure ((\"hello\" :: Text) <> \" world\")"]);
    assert_eq!(json, serde_json::json!("hello world"));
}

#[test]

fn test_mcp_multi_line_do() {
    let json = run_mcp(&["let xs = [1,2,3 :: Int]", "let ys = map (*2) xs", "pure ys"]);
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
    let (json, _) =
        run_mcp_effectful(&["kvSet \"k\" (toJSON (\"v\" :: Text))", "send (KvGet \"k\")"]);
    assert_eq!(json, serde_json::json!("v")); // Just "v" → unwrapped
}

#[test]

fn test_effect_kv_get_missing() {
    let (json, _) = run_mcp_effectful(&["send (KvGet \"nope\")"]);
    assert_eq!(json, serde_json::json!(null)); // Nothing → null
}

#[test]

fn test_effect_kv_delete_then_get() {
    let (json, _) = run_mcp_effectful(&[
        "kvSet \"k\" (toJSON (\"v\" :: Text))",
        "send (KvDelete \"k\")",
        "send (KvGet \"k\")",
    ]);
    assert_eq!(json, serde_json::json!(null)); // Nothing after delete
}

#[test]

fn test_effect_kv_keys() {
    let (json, _) = run_mcp_effectful(&[
        "kvSet \"a\" (toJSON (\"1\" :: Text))",
        "kvSet \"b\" (toJSON (\"2\" :: Text))",
        "kvSet \"c\" (toJSON (\"3\" :: Text))",
        "send KvKeys",
    ]);
    // Keys come back as a list, order may vary
    let arr = json.as_array().expect("expected array");
    assert_eq!(arr.len(), 3);
    let mut keys: Vec<String> = arr
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["a", "b", "c"]);
}

#[test]

fn test_effect_console_print() {
    let (json, lines) = run_mcp_effectful(&["send (Print \"hello\")", "pure (42 :: Int)"]);
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
        "kvSet \"x\" (toJSON (\"42\" :: Text))",
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
        "kvSet \"flag\" (toJSON (\"yes\" :: Text))",
        "v <- send (KvGet \"flag\")",
        "case v of { Just _ -> kvSet \"result\" (toJSON (\"found\" :: Text)); Nothing -> kvSet \"result\" (toJSON (\"missing\" :: Text)) }",
        "send (KvGet \"result\")",
    ]);
    assert_eq!(json, serde_json::json!("found")); // Just "found" → unwrapped
}

#[test]

fn test_effect_kv_overwrite() {
    let (json, _) = run_mcp_effectful(&[
        "kvSet \"k\" (toJSON (\"old\" :: Text))",
        "kvSet \"k\" (toJSON (\"new\" :: Text))",
        "send (KvGet \"k\")",
    ]);
    assert_eq!(json, serde_json::json!("new")); // Just "new" → unwrapped
}

#[test]
fn test_effect_words() {
    // FsRead returns "stub" by default in our mock
    let (json, _) = run_mcp_effectful(&["s <- send (FsRead \"file.txt\")", "pure (words s)"]);
    assert_eq!(json, serde_json::json!(["stub"]));
}

#[test]

fn test_effect_words_custom() {
    let (json, _) = run_mcp_effectful(&["pure (words \"  hello   world  \")"]);
    assert_eq!(json, serde_json::json!(["hello", "world"]));
}

// ===========================================================================
// Helper function tests (multi-arg lambdas calling effects)
// ===========================================================================

#[test]
fn test_effect_helper_two_args() {
    // Reproducer: two-arg helper function that calls KvGet + case + KvSet/FsWrite.
    // Panicked with "no entry found for key" (HashMap Index on missing VarId).
    let (json, _console) = run_mcp_effectful_with_helpers(
        &[
            "kvSet \"a\" (toJSON (\"data\" :: Text))",
            "persist \"a\" \"out.txt\"",
            "pure \"done\"",
        ],
        &[r#"persist :: Text -> Text -> Eff '[Console, KV, Fs] ()
persist key filename = do
  val <- send (KvGet key)
  case val >>= (^? _String) of
    Nothing -> send (Print "nothing")
    Just content -> send (FsWrite filename content)"#],
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
    // Use T.pack to convert [Char] result to Text before toJSON serializes it.
    // Without this, toJSON @[Char] maps per-char → Array of single-char strings.
    let json = run_mcp(&["pure (T.pack (map (\\c -> chr (ord c + 1)) (T.unpack \"Hello\")))"]);
    assert_eq!(json, serde_json::json!("Ifmmp"));
}

#[test]
fn test_filter_string_literal() {
    let json = run_plain("filter (\\c -> c /= 'l') \"Hello World\"");
    assert_eq!(json, serde_json::json!("Heo Word"));
}

#[test]
fn test_show_4_tuple() {
    let json = run_plain("show (1::Int, True, 'x', \"hi\" :: Text)");
    assert_eq!(json, serde_json::json!("(1,True,'x',\"hi\")"));
}

// ---------------------------------------------------------------------------
// Diagnostic tests for JIT runtime cleanup issues
// ---------------------------------------------------------------------------

#[test]
fn test_diag_map_compile_twice() {
    // Does JIT compilation alone crash on 2nd invocation?
    let src = mcp_source_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int),(2,20)]",
            "pure (Map.toList m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    let r1 = compile_only(&src);
    assert!(r1.is_ok(), "first compile failed: {:?}", r1);
    let r2 = compile_only(&src);
    assert!(r2.is_ok(), "second compile failed: {:?}", r2);
}

#[test]
fn test_diag_map_run_then_compile() {
    // Run a map test, then try to compile (not run) again.
    let json = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int),(2,20)]",
            "pure (Map.toList m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json, serde_json::json!([[1, 10], [2, 20]]));
    // Now compile-only (no execution):
    let src = mcp_source_with_imports(
        &[
            "let m = Map.fromList [(3::Int,30::Int)]",
            "pure (Map.toList m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    let r = compile_only(&src);
    assert!(r.is_ok(), "second compile-only failed: {:?}", r);
}

#[test]
fn test_diag_map_run_then_run_simple() {
    // Run a map test, then try to run a simple non-map test.
    let json = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int),(2,20)]",
            "pure (Map.toList m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json, serde_json::json!([[1, 10], [2, 20]]));
    // Now run something simple:
    let json2 = run_mcp(&["pure (42 :: Int)"]);
    assert_eq!(json2, serde_json::json!(42));
}

#[test]
fn test_diag_simple_run_then_map_run() {
    // Run a simple test, then try to run a map test.
    let json = run_mcp(&["pure (42 :: Int)"]);
    assert_eq!(json, serde_json::json!(42));
    // Now run a map test:
    let json2 = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int),(2,20)]",
            "pure (Map.toList m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json2, serde_json::json!([[1, 10], [2, 20]]));
}

#[test]
fn test_diag_simple_run_twice() {
    // Run two simple tests back to back.
    let json = run_mcp(&["pure (42 :: Int)"]);
    assert_eq!(json, serde_json::json!(42));
    let json2 = run_mcp(&["pure (99 :: Int)"]);
    assert_eq!(json2, serde_json::json!(99));
}

#[test]
fn test_diag_map_run_twice() {
    // Regression test: two sequential map runs in separate JIT machines.
    let json1 = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int)]",
            "pure (Map.toList m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json1, serde_json::json!([[1, 10]]));
    let json2 = run_mcp_with_imports(
        &[
            "let m = Map.fromList [(2::Int,20::Int)]",
            "pure (Map.toList m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    assert_eq!(json2, serde_json::json!([[2, 20]]));
}

#[test]
fn test_diag_map_run_twice_big_nursery() {
    // Same as map_run_twice but with 512MB nursery to test nursery overflow hypothesis.
    use tidepool_runtime::compile_and_run_with_nursery_size;
    let src1 = mcp_source_with_imports(
        &[
            "let m = Map.fromList [(1::Int,10::Int)]",
            "pure (Map.toList m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    let src2 = mcp_source_with_imports(
        &[
            "let m = Map.fromList [(2::Int,20::Int)]",
            "pure (Map.toList m)",
        ],
        &[],
        &["qualified Data.Map.Strict as Map"],
    );
    let pp = prelude_path();
    let pp2 = pp.clone();
    let json = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let val = compile_and_run_with_nursery_size(
                &src1,
                "result",
                &include,
                &mut HNil,
                &(),
                1 << 29,
            )
            .expect("first run failed");
            val.to_json()
        })
        .unwrap()
        .join()
        .expect("thread panicked");
    assert_eq!(json, serde_json::json!([[1, 10]]));

    let json2 = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp2.as_path()];
            let val = compile_and_run_with_nursery_size(
                &src2,
                "result",
                &include,
                &mut HNil,
                &(),
                1 << 29,
            )
            .expect("second run failed");
            val.to_json()
        })
        .unwrap()
        .join()
        .expect("thread panicked");
    assert_eq!(json2, serde_json::json!([[2, 20]]));
}

// ===========================================================================
// KV Text bridge bugs
//
// When KvKeys returns Rust String keys converted via ToCore for String, the
// resulting Text values have broken length/offset fields. T.length returns a
// wrong (often negative) value, and T.drop/T.take treat the Text as empty.
// Operations that scan raw bytes (T.isPrefixOf, ==) still work; operations
// that use the len field (T.length, T.drop, T.take, T.splitAt) do not.
// ===========================================================================

/// T.length on a Text key returned by KvKeys should equal the number of chars.
/// Bug: observed to return a negative value (e.g. -5 for "hello").
#[test]
fn test_kv_keys_text_length() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("world" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.length keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([5]));
}

/// T.drop on a Text key from KvKeys should drop the first n characters.
/// Bug: T.drop n k returns "" for any positive n because T.drop checks
/// n >= T.length k, and if T.length k is negative, any positive n satisfies
/// the check and the empty string is returned.
#[test]
fn test_kv_keys_text_drop() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("world" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.drop 2) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["llo"]));
}

/// T.take on a Text key from KvKeys should return the first n characters.
/// Bug: T.take n k returns the full string (behaves as n <= 0) or empty
/// string when the len field is wrong.
#[test]
fn test_kv_keys_text_take() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("world" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.take 3) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["hel"]));
}

/// Stripping a namespace prefix from KvKeys results is a common pattern.
/// Bug: T.drop on pure text returns "" (pre-existing measureOff/pure-text bug).
#[test]
fn test_kv_keys_strip_namespace_prefix() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "ns:foo" (toJSON ("v1" :: Text))"#,
        r#"kvSet "ns:bar" (toJSON ("v2" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"let stripped = sort (map (T.drop 3) keys)"#,
        r#"pure stripped"#,
    ]);
    assert_eq!(json, serde_json::json!(["bar", "foo"]));
}

/// T.splitAt on a Text key from KvKeys.
/// Bug: T.splitAt n k returns ("", k) because n >= T.length k (negative).
#[test]
fn test_kv_keys_text_split_at() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("world" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.splitAt 3) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([["hel", "lo"]]));
}

/// T.null on a non-empty key from KvKeys should return False.
/// Characterises whether T.null (len == 0 check) is affected by the bug.
#[test]
fn test_kv_keys_text_null() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "nonempty" (toJSON ("v" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.null keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([false]));
}

/// T.reverse on a Text key from KvKeys.
/// Characterises whether byte-scanning ops are affected alongside len-based ops.
#[test]
fn test_kv_keys_text_reverse() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("world" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.reverse keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["olleh"]));
}

/// T.append with a KvKeys key as the left operand.
/// Characterises whether append works despite broken len field.
#[test]
fn test_kv_keys_text_append() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("world" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (\k -> T.append k "!") keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["hello!"]));
}

/// T.splitOn on a Text key from KvKeys.
#[test]
fn test_kv_keys_text_split_on() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "a:b:c" (toJSON ("v" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (splitOn ":") keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([["a", "b", "c"]]));
}

/// Equality comparison on a Text key from KvKeys should work correctly.
/// Characterises whether == (byte comparison) survives the len-field bug.
#[test]
fn test_kv_keys_text_eq() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("world" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (== "hello") keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([true]));
}

/// T.isPrefixOf on a Text key from KvKeys — observed to work in live testing.
/// Characterises which operations survive the len-field bug.
#[test]
fn test_kv_keys_text_is_prefix_of() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "prefix:a" (toJSON ("v1" :: Text))"#,
        r#"kvSet "other:b" (toJSON ("v2" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"let matching = filter (T.isPrefixOf "prefix:") keys"#,
        r#"pure (length matching)"#,
    ]);
    assert_eq!(json, serde_json::json!(1));
}

/// T.words on a Text key from KvKeys that contains spaces.
/// Bug: T.words uses len-bounded iteration; likely broken.
#[test]
fn test_kv_keys_text_words() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "foo bar baz" (toJSON ("v" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (concatMap words keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["foo", "bar", "baz"]));
}

/// toUpper on a Text key from KvKeys.
/// Characterises whether character-mapping ops are affected.
#[test]
fn test_kv_keys_text_to_upper() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("world" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map toUpper keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["HELLO"]));
}

/// T.length on a value retrieved via KvGet (in tests, values also come through
/// ToCore for String). Pair with test_kv_keys_text_length to scope the bug:
/// if both fail, all ToCore String paths are broken; if only Keys fails,
/// the bug is specific to the Vec<String> key conversion.
#[test]
fn test_kv_get_value_text_length() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "k" (toJSON ("hello" :: Text))"#,
        r#"v <- kvGet "k""#,
        r#"let t = v >>= (^? _String)"#,
        r#"pure (maybe (-999) T.length t)"#,
    ]);
    assert_eq!(json, serde_json::json!(5));
}

/// T.drop on a value retrieved via KvGet.
/// Pair with test_kv_keys_text_drop to scope the bug.
#[test]
fn test_kv_get_value_text_drop() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "k" (toJSON ("hello" :: Text))"#,
        r#"v <- kvGet "k""#,
        r#"let t = v >>= (^? _String)"#,
        r#"pure (maybe "none" (T.drop 2) t)"#,
    ]);
    assert_eq!(json, serde_json::json!("llo"));
}

/// Roundtrip: set namespaced keys, get keys, filter by prefix, drop prefix, sort.
/// This is the canonical KV namespace pattern that currently breaks end-to-end.
#[test]
fn test_kv_keys_namespace_roundtrip() {
    let (json, _) = run_mcp_effectful(&[
        r#"mapM_ (\(k,v) -> kvSet k (toJSON v)) [("cache:one","1"),("cache:two","2"),("cache:three","3")]"#,
        r#"keys <- send KvKeys"#,
        r#"let cacheKeys = filter (T.isPrefixOf "cache:") keys"#,
        r#"let names = sort (map (T.drop 6) cacheKeys)"#,
        r#"pure names"#,
    ]);
    assert_eq!(json, serde_json::json!(["one", "three", "two"]));
}

// ===========================================================================
// Granular primop tests — exercise specific JIT code paths on bridge text.
// Each test targets a single primop or small cluster, with bridge-injected
// Text values that go through value_to_heap → ByteArray# Lit → primop.
// ===========================================================================

// --- FfiTextMeasureOff (used by T.length, T.take, T.drop, T.splitAt) --------

/// T.length on bridge text: exercises _hs_text_measure_off with cnt=maxBound.
/// The FFI should return -(char_count), GHC negates to get length.
#[test]
fn test_primop_measure_off_length_ascii() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.length keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([5]));
}

/// T.length on empty bridge text.
#[test]
fn test_primop_measure_off_length_empty() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.length keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([0]));
}

/// T.take on bridge text: exercises _hs_text_measure_off with small cnt.
/// Should return bytes consumed (non-negative).
#[test]
fn test_primop_measure_off_take_ascii() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.take 3) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["hel"]));
}

/// T.take more than length — should return entire text.
#[test]
fn test_primop_measure_off_take_all() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hi" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.take 10) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["hi"]));
}

/// T.drop on bridge text.
#[test]
fn test_primop_measure_off_drop_ascii() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.drop 2) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["llo"]));
}

/// T.drop all — should return empty text.
#[test]
fn test_primop_measure_off_drop_all() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hi" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.drop 10) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([""]));
}

/// T.splitAt on bridge text — uses measureOff for both take and drop.
#[test]
fn test_primop_measure_off_split_at() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.splitAt 2) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([["he", "llo"]]));
}

/// T.length + T.take + T.drop consistency check on bridge text.
#[test]
fn test_primop_measure_off_consistency() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "abcde" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"let k = head keys"#,
        r#"pure (T.length k, T.take 3 k, T.drop 3 k, T.take 3 k `T.append` T.drop 3 k == k)"#,
    ]);
    assert_eq!(json, serde_json::json!([5, "abc", "de", true]));
}

// --- FfiTextReverse ----------------------------------------------------------

/// T.reverse on bridge text: exercises _hs_text_reverse(dst, src, off, len).
#[test]
fn test_primop_reverse_ascii() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.reverse keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["olleh"]));
}

/// T.reverse on single-char bridge text.
#[test]
fn test_primop_reverse_single() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "x" (toJSON ("v" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.reverse keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["x"]));
}

/// T.reverse involution: reverse(reverse(x)) == x.
#[test]
fn test_primop_reverse_involution() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "abcde" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (\k -> T.reverse (T.reverse k) == k) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([true]));
}

// --- IndexWord8Array (used by toUpper, toLower, T.map, T.filter) -------------

/// toUpper on bridge text: uses indexWord8Array# to read bytes + upperMapping.
#[test]
fn test_primop_index_word8_to_upper() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map toUpper keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["HELLO"]));
}

/// toLower on bridge text.
#[test]
fn test_primop_index_word8_to_lower() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "WORLD" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map toLower keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["world"]));
}

/// T.filter on bridge text — iterates bytes with indexWord8Array#.
#[test]
fn test_primop_index_word8_filter() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "a1b2c3" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"let isAlpha c = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')"#,
        r#"pure (map (T.filter isAlpha) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["abc"]));
}

// --- CompareByteArrays (used by Text Ord instance: ==, compare, sort) --------

/// Text equality on bridge text — uses compareByteArrays#.
#[test]
fn test_primop_compare_eq() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (== "hello") keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([true]));
}

/// Text compare on bridge text.
#[test]
fn test_primop_compare_ordering() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "banana" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"let k = head keys"#,
        r#"pure (compare k "apple", compare k "banana", compare k "cherry")"#,
    ]);
    // "banana" > "apple", == "banana", < "cherry"
    assert_eq!(json, serde_json::json!(["GT", "EQ", "LT"]));
}

/// Sort on multiple bridge texts — exercises compareByteArrays# in merge sort.
#[test]
fn test_primop_compare_sort() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "cherry" (toJSON ("1" :: Text))"#,
        r#"kvSet "apple" (toJSON ("2" :: Text))"#,
        r#"kvSet "banana" (toJSON ("3" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (sort keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["apple", "banana", "cherry"]));
}

// --- FfiTextMemchr (used by text search operations) --------------------------

/// T.find on bridge text — uses memchr to locate bytes.
#[test]
fn test_primop_memchr_find() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.find (== 'l')) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["l"]));
}

/// T.find not found.
#[test]
fn test_primop_memchr_find_missing() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.find (== 'z')) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([null]));
}

// --- Composite: multiple primops in sequence ---------------------------------

/// T.take on T.reverse of bridge text.
#[test]
fn test_primop_composite_take_reverse() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.take 3 . T.reverse) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["oll"]));
}

/// T.length after T.drop on bridge text.
#[test]
fn test_primop_composite_length_after_drop() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.length . T.drop 2) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([3]));
}

/// words on bridge text — exercises measureOff + cons cell construction.
#[test]
fn test_primop_composite_words() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "hello world foo" (toJSON ("x" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"pure (concatMap words keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["hello", "world", "foo"]));
}

/// Bridge text through KvGet (not just KvKeys).
#[test]
fn test_primop_kvget_measure_off() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "k" (toJSON ("hello world" :: Text))"#,
        r#"v <- kvGet "k""#,
        r#"let t = v >>= (^? _String)"#,
        r#"pure (fmap (\s -> (T.length s, T.take 5 s, T.drop 6 s)) t)"#,
    ]);
    assert_eq!(json, serde_json::json!([11, "hello", "world"]));
}

/// Bridge text from KvGet through T.reverse.
#[test]
fn test_primop_kvget_reverse() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "k" (toJSON ("abcde" :: Text))"#,
        r#"v <- kvGet "k""#,
        r#"let t = v >>= (^? _String)"#,
        r#"pure (fmap T.reverse t)"#,
    ]);
    assert_eq!(json, serde_json::json!("edcba"));
}

/// Bridge text from KvGet through toUpper.
#[test]
fn test_primop_kvget_to_upper() {
    let (json, _) = run_mcp_effectful(&[
        r#"kvSet "k" (toJSON ("hello" :: Text))"#,
        r#"v <- kvGet "k""#,
        r#"let t = v >>= (^? _String)"#,
        r#"pure (fmap toUpper t)"#,
    ]);
    assert_eq!(json, serde_json::json!("HELLO"));
}

// ===========================================================================
// Vendored Aeson comprehensive test suite
//
// Tests the full Tidepool.Aeson stack: Value construction, ToJSON class,
// lens-based access/modification, composition with effects, input injection,
// helper functions, and edge cases.
// ===========================================================================

// ---------------------------------------------------------------------------
// 1. Value construction: every constructor
// ---------------------------------------------------------------------------

/// Construct Null via qualified name
#[test]
fn test_vendored_aeson_null_qualified() {
    let json = run_aeson(&[r#"pure Aeson.Null"#]);
    assert_eq!(json, serde_json::json!(null));
}

/// Construct Bool True and Bool False
#[test]
fn test_vendored_aeson_bool_both() {
    let t = run_aeson(&[r#"pure (Aeson.Bool True)"#]);
    let f = run_aeson(&[r#"pure (Aeson.Bool False)"#]);
    assert_eq!(t, serde_json::json!(true));
    assert_eq!(f, serde_json::json!(false));
}

/// Construct String with various content
#[test]
fn test_vendored_aeson_string_unicode() {
    let json = run_aeson(&[r#"pure (Aeson.String "hello world")"#]);
    assert_eq!(json, serde_json::json!("hello world"));
}

/// Construct Number via toJSON Int
#[test]
fn test_vendored_aeson_number_via_tojson() {
    let json = run_aeson(&[r#"pure (toJSON (99 :: Int))"#]);
    // Number wraps Scientific — should be non-null
    assert!(!json.is_null());
}

/// Construct Array via toJSON list
#[test]
fn test_vendored_aeson_array_via_tojson() {
    let json = run_aeson(&[r#"pure (toJSON ([1, 2, 3] :: [Int]))"#]);
    assert!(!json.is_null());
}

/// Construct empty object
#[test]
fn test_vendored_aeson_empty_object() {
    let json = run_aeson(&[r#"pure (object [])"#]);
    assert_eq!(json, serde_json::json!({}));
}

/// Construct empty array
#[test]
fn test_vendored_aeson_empty_array() {
    let json = run_aeson(&[r#"pure (toJSON ([] :: [Int]))"#]);
    assert!(!json.is_null());
}

// ---------------------------------------------------------------------------
// 2. ToJSON instances
// ---------------------------------------------------------------------------

/// ToJSON Text
#[test]
fn test_vendored_tojson_text() {
    let json = run_aeson(&[r#"pure (toJSON ("hello" :: Text))"#]);
    assert_eq!(json, serde_json::json!("hello"));
}

/// ToJSON Bool
#[test]
fn test_vendored_tojson_bool() {
    let json = run_aeson(&[r#"pure (toJSON True)"#]);
    assert_eq!(json, serde_json::json!(true));
}

/// ToJSON Maybe — Just wraps, Nothing becomes Null
#[test]
fn test_vendored_tojson_maybe() {
    let just = run_aeson(&[r#"pure (toJSON (Just ("x" :: Text)))"#]);
    let nothing = run_aeson(&[r#"pure (toJSON (Nothing :: Maybe Text))"#]);
    assert_eq!(just, serde_json::json!("x"));
    assert_eq!(nothing, serde_json::json!(null));
}

/// ToJSON nested list
#[test]
fn test_vendored_tojson_nested_list() {
    let json = run_aeson(&[r#"pure (toJSON [toJSON [1 :: Int, 2], toJSON [3 :: Int, 4]])"#]);
    assert!(!json.is_null());
}

// ---------------------------------------------------------------------------
// 3. object / (.=) construction patterns
// ---------------------------------------------------------------------------

/// Object with mixed value types
#[test]
fn test_vendored_object_mixed_types() {
    let json = run_aeson(&[
        r#"let v = object ["name" .= ("Alice" :: Text), "active" .= True, "score" .= (100 :: Int)]"#,
        r#"pure v"#,
    ]);
    assert!(json.is_object());
    assert_eq!(json["name"], serde_json::json!("Alice"));
    assert_eq!(json["active"], serde_json::json!(true));
}

/// Object with nested objects
#[test]
fn test_vendored_object_nested() {
    let json = run_aeson(&[
        r#"let inner = object ["x" .= (1 :: Int), "y" .= (2 :: Int)]"#,
        r#"let outer = object ["point" .= inner, "label" .= ("origin" :: Text)]"#,
        r#"pure (outer ^? key "point" . key "x" . _Number)"#,
    ]);
    assert_eq!(json, serde_json::json!(1));
}

/// Object with array values
#[test]
fn test_vendored_object_with_arrays() {
    let json = run_aeson(&[
        r#"let v = object ["tags" .= (["a", "b", "c"] :: [Text]), "count" .= (3 :: Int)]"#,
        r#"pure (v ^.. key "tags" . _Array . traverse . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!(["a", "b", "c"]));
}

/// Object from list comprehension
#[test]
fn test_vendored_object_from_comprehension() {
    let json = run_aeson_with_helpers(
        &[
            r#"let pairs = [show i .= (i :: Int) | i <- [1, 2, 3]]"#,
            r#"pure (object pairs)"#,
        ],
        &[],
    );
    assert!(json.is_object());
}

// ---------------------------------------------------------------------------
// 4. Lens traversals — basic
// ---------------------------------------------------------------------------

/// key on nested 3-level object
#[test]
fn test_vendored_lens_triple_nested_key() {
    let json = run_aeson(&[
        r#"let v = object ["a" .= object ["b" .= object ["c" .= ("found" :: Text)]]]"#,
        r#"pure (v ^? key "a" . key "b" . key "c" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!("found"));
}

/// nth on nested array
#[test]
fn test_vendored_lens_nth_nested() {
    let json = run_aeson(&[
        r#"let arr = toJSON [toJSON [10 :: Int, 20], toJSON [30 :: Int, 40]]"#,
        r#"pure (arr ^? nth 1 . nth 0 . _Number)"#,
    ]);
    assert_eq!(json, serde_json::json!(30));
}

/// ^.. with _Array to get all elements
#[test]
fn test_vendored_lens_array_tolist() {
    let json = run_aeson(&[
        r#"let arr = toJSON ["x" :: Text, "y", "z"]"#,
        r#"pure (arr ^.. _Array . traverse . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!(["x", "y", "z"]));
}

/// _Bool prism roundtrip
#[test]
fn test_vendored_lens_bool_prism_both() {
    let t = run_aeson(&[r#"pure (Aeson.Bool True ^? _Bool)"#]);
    let f = run_aeson(&[r#"pure (Aeson.Bool False ^? _Bool)"#]);
    assert_eq!(t, serde_json::json!(true));
    assert_eq!(f, serde_json::json!(false));
}

/// _String prism on non-string returns Nothing
#[test]
fn test_vendored_lens_prism_mismatch() {
    let json = run_aeson(&[r#"pure (Aeson.Bool True ^? _String)"#]);
    assert_eq!(json, serde_json::json!(null));
}

/// _Number prism on non-number returns Nothing
#[test]
fn test_vendored_lens_number_mismatch() {
    let json = run_aeson(&[r#"pure (Aeson.String "hi" ^? _Number)"#]);
    assert_eq!(json, serde_json::json!(null));
}

/// _Object prism extracts the map
#[test]
fn test_vendored_lens_object_prism() {
    let json = run_aeson(&[
        r#"let obj = object ["k" .= ("v" :: Text)]"#,
        r#"let isObj = case obj ^? _Object of { Just _ -> True; Nothing -> False }"#,
        r#"pure isObj"#,
    ]);
    assert_eq!(json, serde_json::json!(true));
}

/// _Array prism on non-array returns Nothing
#[test]
fn test_vendored_lens_array_prism_mismatch() {
    let json = run_aeson(&[r#"pure (Aeson.String "hi" ^? _Array)"#]);
    assert_eq!(json, serde_json::json!(null));
}

// ---------------------------------------------------------------------------
// 5. Lens modification (.~ and %~)
// ---------------------------------------------------------------------------

/// Set nested field via (.~)
#[test]
fn test_vendored_lens_set_nested() {
    let json = run_aeson(&[
        r#"let v = object ["user" .= object ["name" .= ("old" :: Text)]]"#,
        r#"let v2 = v & key "user" . key "name" . _String .~ "new""#,
        r#"pure (v2 ^? key "user" . key "name" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!("new"));
}

/// Modify via (%~) with Text function
#[test]
fn test_vendored_lens_modify_toupper() {
    let json = run_aeson(&[
        r#"let v = object ["msg" .= ("hello" :: Text)]"#,
        r#"let v2 = v & key "msg" . _String %~ T.toUpper"#,
        r#"pure (v2 ^? key "msg" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!("HELLO"));
}

/// Set a Bool field
#[test]
fn test_vendored_lens_set_bool() {
    let json = run_aeson(&[
        r#"let v = object ["active" .= False]"#,
        r#"let v2 = v & key "active" . _Bool .~ True"#,
        r#"pure (v2 ^? key "active" . _Bool)"#,
    ]);
    assert_eq!(json, serde_json::json!(true));
}

/// Set on missing key — no-op (lens traversal doesn't create keys)
#[test]
fn test_vendored_lens_set_missing_noop() {
    let json = run_aeson(&[
        r#"let v = object ["a" .= (1 :: Int)]"#,
        r#"let v2 = v & key "missing" . _String .~ "x""#,
        r#"pure (v2 ^? key "missing" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!(null));
}

// ---------------------------------------------------------------------------
// 6. Composition with effects
// ---------------------------------------------------------------------------

/// Construct JSON, print each field name via Console
#[test]
fn test_vendored_effect_print_field_names() {
    let (json, console) = run_aeson_effectful(&[
        r#"let record = object ["alpha" .= ("a" :: Text), "beta" .= ("b" :: Text), "gamma" .= ("c" :: Text)]"#,
        r#"let vals = record ^.. key "alpha" . _String"#,
        r#"mapM_ (send . Print) vals"#,
        r#"let vals2 = record ^.. key "beta" . _String"#,
        r#"mapM_ (send . Print) vals2"#,
        r#"let vals3 = record ^.. key "gamma" . _String"#,
        r#"mapM_ (send . Print) vals3"#,
        r#"pure (length vals + length vals2 + length vals3)"#,
    ]);
    assert_eq!(json, serde_json::json!(3));
    assert_eq!(console, vec!["a", "b", "c"]);
}

/// Build JSON, store stringified field in KV, retrieve it
#[test]
fn test_vendored_effect_json_to_kv() {
    let (json, _) = run_aeson_effectful(&[
        r#"let person = object ["name" .= ("Eve" :: Text)]"#,
        r#"case person ^? key "name" . _String of"#,
        r#"  Just n -> kvSet "cached_name" (toJSON n)"#,
        r#"  Nothing -> pure ()"#,
        r#"result <- send (KvGet "cached_name")"#,
        r#"pure result"#,
    ]);
    assert_eq!(json, serde_json::json!("Eve"));
}

/// Multiple KV ops driven by JSON data
#[test]
fn test_vendored_effect_json_driven_kv_batch() {
    let (json, console) = run_aeson_effectful_with_helpers(
        &[
            r#"let users = [object ["name" .= (n :: Text)] | n <- ["alice", "bob", "carol"]]"#,
            r#"storeAll users"#,
            r#"k <- send KvKeys"#,
            r#"mapM_ (send . Print) (sort k)"#,
            r#"pure (length k)"#,
        ],
        &["storeAll :: [Aeson.Value] -> Eff '[Console, KV, Fs] ()\n\
             storeAll [] = pure ()\n\
             storeAll (u:us) = do\n\
             \x20 case u ^? key \"name\" . _String of\n\
             \x20   Just n -> kvSet n (toJSON n)\n\
             \x20   Nothing -> pure ()\n\
             \x20 storeAll us"],
    );
    assert_eq!(json, serde_json::json!(3));
    assert_eq!(console, vec!["alice", "bob", "carol"]);
}

/// Pattern match on Maybe from lens, branch effects
#[test]
fn test_vendored_effect_conditional_on_lens() {
    let (json, console) = run_aeson_effectful(&[
        r#"let cfg = object ["debug" .= True]"#,
        r#"case cfg ^? key "debug" . _Bool of"#,
        r#"  Just True -> send (Print "debug mode on")"#,
        r#"  _         -> send (Print "debug mode off")"#,
        r#"pure ("ok" :: Text)"#,
    ]);
    assert_eq!(json, serde_json::json!("ok"));
    assert_eq!(console, vec!["debug mode on"]);
}

// ---------------------------------------------------------------------------
// 7. Input injection — varied shapes
// ---------------------------------------------------------------------------

/// Inject flat object, extract multiple fields
#[test]
fn test_vendored_input_multi_field() {
    let json = run_aeson_with_input(
        &[
            r#"let n = input ^? key "name" . _String"#,
            r#"let a = input ^? key "age" . _Number"#,
            r#"pure (n, a)"#,
        ],
        serde_json::json!({"name": "Alice", "age": 30}),
    );
    assert_eq!(json, serde_json::json!(["Alice", 30]));
}

/// Inject deeply nested JSON
#[test]
fn test_vendored_input_deep_nested() {
    let json = run_aeson_with_input(
        &[r#"pure (input ^? key "a" . key "b" . key "c" . _String)"#],
        serde_json::json!({"a": {"b": {"c": "deep"}}}),
    );
    assert_eq!(json, serde_json::json!("deep"));
}

/// Inject array of objects, extract field from each
#[test]
fn test_vendored_input_array_of_objects() {
    let json = run_aeson_with_input(
        &[r#"pure (input ^.. _Array . traverse . key "name" . _String)"#],
        serde_json::json!([
            {"name": "Alice", "score": 90},
            {"name": "Bob", "score": 85},
            {"name": "Carol", "score": 92}
        ]),
    );
    assert_eq!(json, serde_json::json!(["Alice", "Bob", "Carol"]));
}

/// Inject null
#[test]
fn test_vendored_input_null() {
    let json = run_aeson_with_input(
        &[
            r#"let isNull = case input ^? _String of { Just _ -> False; Nothing -> True }"#,
            r#"pure isNull"#,
        ],
        serde_json::Value::Null,
    );
    assert_eq!(json, serde_json::json!(true));
}

/// Inject boolean
#[test]
fn test_vendored_input_bool() {
    let json = run_aeson_with_input(&[r#"pure (input ^? _Bool)"#], serde_json::json!(true));
    assert_eq!(json, serde_json::json!(true));
}

/// Inject string
#[test]
fn test_vendored_input_string() {
    let json = run_aeson_with_input(
        &[r#"pure (input ^? _String)"#],
        serde_json::json!("hello world"),
    );
    assert_eq!(json, serde_json::json!("hello world"));
}

/// Inject number
#[test]
fn test_vendored_input_number() {
    let json = run_aeson_with_input(&[r#"pure (input ^? _Number)"#], serde_json::json!(42));
    assert_eq!(json, serde_json::json!(42));
}

/// Inject array of primitives, sum them via _Number
#[test]
fn test_vendored_input_array_sum() {
    let json = run_aeson_with_input(
        &[
            r#"let nums = input ^.. _Array . traverse . _Number"#,
            r#"pure (length nums)"#,
        ],
        serde_json::json!([10, 20, 30, 40, 50]),
    );
    assert_eq!(json, serde_json::json!(5));
}

// ---------------------------------------------------------------------------
// 8. Helper functions — data processing patterns
// ---------------------------------------------------------------------------

/// Helper: extract all string values from any JSON value (recursive-ish)
#[test]
fn test_vendored_helper_extract_all_strings() {
    let json = run_aeson_with_helpers(
        &[
            r#"let v = object ["a" .= ("x" :: Text), "b" .= object ["c" .= ("y" :: Text)]]"#,
            r#"pure (allStrings v)"#,
        ],
        &["allStrings :: Aeson.Value -> [Text]\n\
             allStrings v = case v of\n\
             \x20 Aeson.String s -> [s]\n\
             \x20 Aeson.Object _ -> v ^.. Aeson.members . to allStrings . traverse\n\
             \x20 Aeson.Array _ -> v ^.. Aeson.values . to allStrings . traverse\n\
             \x20 _ -> []"],
    );
    // Map iteration order is alphabetical for our vendored KeyMap (Data.Map.Strict)
    assert_eq!(json, serde_json::json!(["x", "y"]));
}

/// Helper: count total entries in an object
#[test]
fn test_vendored_helper_count_keys() {
    let json = run_aeson_with_helpers(
        &[
            r#"let v = object ["a" .= (1 :: Int), "b" .= (2 :: Int), "c" .= (3 :: Int)]"#,
            r#"pure (countKeys v)"#,
        ],
        &["countKeys :: Aeson.Value -> Int\n\
             countKeys (Aeson.Object o) = KM.size o\n\
             countKeys _ = 0"],
    );
    assert_eq!(json, serde_json::json!(3));
}

/// Helper: filter array elements by predicate
#[test]
fn test_vendored_helper_filter_array() {
    let json = run_aeson_with_helpers(
        &[
            r#"let arr = toJSON ["alice" :: Text, "bob", "anna", "ben"]"#,
            r#"pure (filterStrings (T.isPrefixOf "a") arr)"#,
        ],
        &["filterStrings :: (Text -> Bool) -> Aeson.Value -> [Text]\n\
             filterStrings p v = filter p (v ^.. _Array . traverse . _String)"],
    );
    assert_eq!(json, serde_json::json!(["alice", "anna"]));
}

/// Helper: build summary from structured data
#[test]
fn test_vendored_helper_build_summary() {
    let json = run_aeson_with_helpers(
        &[
            r#"let people = toJSON [object ["name" .= ("Alice" :: Text), "dept" .= ("eng" :: Text)], object ["name" .= ("Bob" :: Text), "dept" .= ("eng" :: Text)], object ["name" .= ("Carol" :: Text), "dept" .= ("sales" :: Text)]]"#,
            r#"pure (summarize people)"#,
        ],
        &["summarize :: Aeson.Value -> (Int, [Text])\n\
             summarize v = let names = v ^.. _Array . traverse . key \"name\" . _String\n\
             \x20             in (length names, names)"],
    );
    assert_eq!(json, serde_json::json!([3, ["Alice", "Bob", "Carol"]]));
}

// ---------------------------------------------------------------------------
// 9. Composition patterns — multi-step pipelines
// ---------------------------------------------------------------------------

/// Build → modify → extract pipeline
#[test]
fn test_vendored_pipeline_build_modify_extract() {
    let json = run_aeson(&[
        r#"let v = object ["status" .= ("pending" :: Text), "retries" .= (0 :: Int)]"#,
        r#"let v2 = v & key "status" . _String .~ "complete""#,
        r#"let status = v2 ^? key "status" . _String"#,
        r#"pure status"#,
    ]);
    assert_eq!(json, serde_json::json!("complete"));
}

/// Build → inspect → branch → build new
#[test]
fn test_vendored_pipeline_inspect_branch() {
    let json = run_aeson(&[
        r#"let req = object ["method" .= ("GET" :: Text), "path" .= ("/api" :: Text)]"#,
        r#"let resp = case req ^? key "method" . _String of"#,
        r#"             Just "GET"  -> object ["status" .= (200 :: Int), "body" .= ("ok" :: Text)]"#,
        r#"             _           -> object ["status" .= (405 :: Int), "body" .= ("not allowed" :: Text)]"#,
        r#"pure (resp ^? key "status" . _Number)"#,
    ]);
    assert_eq!(json, serde_json::json!(200));
}

/// Multiple independent extractions from same value
#[test]
fn test_vendored_pipeline_multi_extract() {
    let json = run_aeson(&[
        r#"let record = object ["first" .= ("Jane" :: Text), "last" .= ("Doe" :: Text), "active" .= True]"#,
        r#"let first = record ^? key "first" . _String"#,
        r#"let last_ = record ^? key "last" . _String"#,
        r#"let active = record ^? key "active" . _Bool"#,
        r#"pure (first, last_, active)"#,
    ]);
    assert_eq!(json, serde_json::json!(["Jane", "Doe", true]));
}

/// Chain of modifications
#[test]
fn test_vendored_pipeline_chain_modifications() {
    let json = run_aeson(&[
        r#"let v = object ["a" .= ("x" :: Text), "b" .= ("y" :: Text), "c" .= ("z" :: Text)]"#,
        r#"let v2 = v & key "a" . _String .~ "X""#,
        r#"let v3 = v2 & key "b" . _String .~ "Y""#,
        r#"let v4 = v3 & key "c" . _String .~ "Z""#,
        r#"pure (v4 ^.. key "a" . _String, v4 ^.. key "b" . _String, v4 ^.. key "c" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!([["X"], ["Y"], ["Z"]]));
}

// ---------------------------------------------------------------------------
// 10. Effect + JSON pipelines
// ---------------------------------------------------------------------------

/// Effectful pipeline: build JSON, extract fields, print, store in KV, read back
#[test]
fn test_vendored_effect_full_pipeline() {
    let (json, console) = run_aeson_effectful(&[
        r#"let user = object ["name" .= ("Dave" :: Text), "role" .= ("admin" :: Text)]"#,
        r#"case user ^? key "name" . _String of"#,
        r#"  Just n -> do { send (Print n); kvSet "user" (toJSON n) }"#,
        r#"  Nothing -> pure ()"#,
        r#"stored <- send (KvGet "user")"#,
        r#"pure stored"#,
    ]);
    assert_eq!(json, serde_json::json!("Dave"));
    assert_eq!(console, vec!["Dave"]);
}

/// Effectful: iterate over JSON array, print each name
#[test]
fn test_vendored_effect_iterate_array() {
    let (json, console) = run_aeson_effectful(&[
        r#"let names = toJSON ["Alice" :: Text, "Bob", "Carol"]"#,
        r#"let extracted = names ^.. _Array . traverse . _String"#,
        r#"mapM_ (send . Print) extracted"#,
        r#"pure (length extracted)"#,
    ]);
    assert_eq!(json, serde_json::json!(3));
    assert_eq!(console, vec!["Alice", "Bob", "Carol"]);
}

/// Effectful: conditional logic based on JSON field
#[test]
fn test_vendored_effect_json_conditional() {
    let (json, console) = run_aeson_effectful(&[
        r#"let cfg = object ["verbose" .= True, "prefix" .= (">>>" :: Text)]"#,
        r#"let isVerbose = fromMaybe False (cfg ^? key "verbose" . _Bool)"#,
        r#"let prefix = fromMaybe "" (cfg ^? key "prefix" . _String)"#,
        r#"when isVerbose (send (Print (prefix `T.append` " starting")))"#,
        r#"pure isVerbose"#,
    ]);
    assert_eq!(json, serde_json::json!(true));
    assert_eq!(console, vec![">>> starting"]);
}

// ---------------------------------------------------------------------------
// 11. Input injection + effects
// ---------------------------------------------------------------------------

/// Inject JSON, process with effects
#[test]
fn test_vendored_input_with_effects() {
    let input_val = serde_json::json!({"items": ["task1", "task2", "task3"]});
    let imports: Vec<&str> = aeson_import_strs();
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines = vec![
        r#"let items = input ^.. key "items" . _Array . traverse . _String"#.to_string(),
        r#"mapM_ (send . Print) items"#.to_string(),
        r#"pure (length items)"#.to_string(),
    ];
    let imports_owned: Vec<String> = imports.iter().map(|s| s.to_string()).collect();
    let src = tidepool_mcp::template_haskell(
        &preamble,
        &stack,
        &lines,
        &imports_owned,
        &[],
        Some(&input_val),
        None,
    );
    let pp = prelude_path();
    let result = std::thread::Builder::new()
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
        .expect("thread panicked");
    assert_eq!(result.0, serde_json::json!(3));
    assert_eq!(result.1, vec!["task1", "task2", "task3"]);
}

// ---------------------------------------------------------------------------
// 12. Edge cases
// ---------------------------------------------------------------------------

/// Empty string in JSON
#[test]
fn test_vendored_edge_empty_string() {
    let json = run_aeson(&[r#"pure (Aeson.String "")"#]);
    assert_eq!(json, serde_json::json!(""));
}

/// Single-element object
#[test]
fn test_vendored_edge_single_key_object() {
    let json = run_aeson(&[
        r#"let v = object ["only" .= ("one" :: Text)]"#,
        r#"pure (v ^? key "only" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!("one"));
}

/// Single-element array
#[test]
fn test_vendored_edge_single_element_array() {
    let json = run_aeson(&[
        r#"let arr = toJSON ["solo" :: Text]"#,
        r#"pure (arr ^? nth 0 . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!("solo"));
}

/// Out-of-bounds nth returns Nothing
#[test]
fn test_vendored_edge_nth_out_of_bounds() {
    let json = run_aeson(&[
        r#"let arr = toJSON [1 :: Int, 2, 3]"#,
        r#"pure (arr ^? nth 99 . _Number)"#,
    ]);
    assert_eq!(json, serde_json::json!(null));
}

/// Negative nth returns Nothing
#[test]
fn test_vendored_edge_nth_negative() {
    let json = run_aeson(&[
        r#"let arr = toJSON [1 :: Int, 2, 3]"#,
        r#"pure (arr ^? nth (-1) . _Number)"#,
    ]);
    assert_eq!(json, serde_json::json!(null));
}

/// Key with special characters
#[test]
fn test_vendored_edge_special_key_chars() {
    let json = run_aeson(&[
        r#"let v = object ["key with spaces" .= ("val" :: Text)]"#,
        r#"pure (v ^? key "key with spaces" . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!("val"));
}

/// Object with many keys (stress test for Map-backed KeyMap)
#[test]
fn test_vendored_edge_many_keys() {
    let json = run_aeson_with_helpers(
        &[
            r#"let pairs = [show i .= (i :: Int) | i <- [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]]"#,
            r#"let obj = object pairs"#,
            r#"pure (obj ^? key "5" . _Number)"#,
        ],
        &[],
    );
    assert_eq!(json, serde_json::json!(5));
}

/// Nested modification preserves other fields
#[test]
fn test_vendored_edge_modify_preserves_siblings() {
    let json = run_aeson(&[
        r#"let v = object ["x" .= ("a" :: Text), "y" .= ("b" :: Text)]"#,
        r#"let v2 = v & key "x" . _String .~ "A""#,
        r#"let x = v2 ^? key "x" . _String"#,
        r#"let y = v2 ^? key "y" . _String"#,
        r#"pure (x, y)"#,
    ]);
    assert_eq!(json, serde_json::json!(["A", "b"]));
}

/// Prism composition: _Array . traverse . _String filters non-strings
#[test]
fn test_vendored_edge_traverse_filters() {
    let json = run_aeson(&[
        r#"let arr = toJSON [Aeson.String "a", Aeson.Bool True, Aeson.String "b", Aeson.Null]"#,
        r#"pure (arr ^.. _Array . traverse . _String)"#,
    ]);
    assert_eq!(json, serde_json::json!(["a", "b"]));
}

/// toJSON on pair produces two-element array
#[test]
fn test_vendored_edge_tojson_pair() {
    let json = run_aeson(&[r#"pure (toJSON ("key" :: Text, "value" :: Text))"#]);
    // Our ToJSON (a,b) produces Array [toJSON a, toJSON b]
    assert!(!json.is_null());
}

// ---------------------------------------------------------------------------
// 13. KeyMap qualified usage
// ---------------------------------------------------------------------------

/// Use KM.size on an object
#[test]
fn test_vendored_keymap_size() {
    let json = run_aeson(&[
        r#"let obj = object ["a" .= (1 :: Int), "b" .= (2 :: Int)]"#,
        r#"case obj ^? _Object of"#,
        r#"  Just m -> pure (KM.size m)"#,
        r#"  Nothing -> pure (0 :: Int)"#,
    ]);
    assert_eq!(json, serde_json::json!(2));
}

/// Use KM.keys to list all keys
#[test]
fn test_vendored_keymap_keys() {
    let json = run_aeson_with_helpers(
        &[
            r#"let obj = object ["alpha" .= (1 :: Int), "beta" .= (2 :: Int)]"#,
            r#"pure (getKeyNames obj)"#,
        ],
        &["getKeyNames :: Aeson.Value -> [Text]\n\
             getKeyNames v = case v ^? _Object of\n\
             \x20 Just m -> map Aeson.toText (KM.keys m)\n\
             \x20 Nothing -> []"],
    );
    // Map keys are sorted alphabetically
    assert_eq!(json, serde_json::json!(["alpha", "beta"]));
}

/// lookupKey from Prelude on extracted object
#[test]
fn test_vendored_keymap_lookup() {
    let (json, _logs) = run_mcp_effectful(&[
        r#"let obj = object ["x" .= ("found" :: Text)]"#,
        r#"pure (toJSON (lookupKey "x" obj))"#,
    ]);
    // Should find String "found"
    assert_eq!(json, serde_json::json!("found"));
}

// ===========================================================================
// 14. Complex effect orchestration with aeson lens
// ===========================================================================

/// State machine: JSON config drives a multi-step KV workflow.
/// Reads a "commands" array from input, each command is an object with
/// "op" (set/get/delete) and "key"/"value" fields. Executes them in order,
/// collects results, returns them as a JSON array.
#[test]
fn test_orchestrate_json_command_interpreter() {
    let input = serde_json::json!({
        "commands": [
            {"op": "set", "key": "color", "value": "blue"},
            {"op": "set", "key": "size", "value": "large"},
            {"op": "get", "key": "color"},
            {"op": "delete", "key": "size"},
            {"op": "get", "key": "size"}
        ]
    });
    let (json, logs) = run_aeson_effectful_with_input(
        &[
            r#"let cmds = input ^.. key "commands" . _Array . traverse"#,
            r#"results <- mapM runCmd cmds"#,
            r#"pure results"#,
        ],
        &["runCmd :: Aeson.Value -> M Aeson.Value\n\
             runCmd cmd = do\n\
             \x20 let op  = fromMaybe \"\" (cmd ^? key \"op\" . _String)\n\
             \x20 let k   = fromMaybe \"\" (cmd ^? key \"key\" . _String)\n\
             \x20 let v   = fromMaybe \"\" (cmd ^? key \"value\" . _String)\n\
             \x20 case op of\n\
             \x20   \"set\" -> do\n\
             \x20     kvSet k (toJSON v)\n\
             \x20     send (Print (\"SET \" `T.append` k `T.append` \"=\" `T.append` v))\n\
             \x20     pure (object [\"status\" .= (\"ok\" :: Text)])\n\
             \x20   \"get\" -> do\n\
             \x20     mval <- send (KvGet k)\n\
             \x20     pure (object [\"key\" .= k, \"value\" .= mval])\n\
             \x20   \"delete\" -> do\n\
             \x20     send (KvDelete k)\n\
             \x20     send (Print (\"DEL \" `T.append` k))\n\
             \x20     pure (object [\"status\" .= (\"deleted\" :: Text)])\n\
             \x20   _ -> pure (object [\"error\" .= (\"unknown op\" :: Text)])"],
        input,
    );
    // Should be an array of 5 results
    let arr = json.as_array().expect("result should be array");
    assert_eq!(arr.len(), 5);
    // Check console logged SET and DEL operations
    assert!(logs.iter().any(|l| l.contains("SET color=blue")));
    assert!(logs.iter().any(|l| l.contains("DEL size")));
}

/// Build a JSON report by folding over data with KV accumulation.
/// Processes a list of "transactions", accumulates running totals in KV,
/// and builds a summary JSON object.
#[test]
fn test_orchestrate_transaction_ledger() {
    let (_json, logs) = run_aeson_effectful_with_helpers(
        &[
            r#"let txns = [ object ["item" .= ("apple" :: Text), "qty" .= (3 :: Int)]"#,
            r#"            , object ["item" .= ("banana" :: Text), "qty" .= (7 :: Int)]"#,
            r#"            , object ["item" .= ("apple" :: Text), "qty" .= (2 :: Int)]"#,
            r#"            ]"#,
            r#"mapM_ processTxn txns"#,
            r#"send (Print "ledger complete")"#,
            r#"ks <- send KvKeys"#,
            r#"totals <- mapM (\k -> do { v <- send (KvGet k); pure (object ["item" .= k, "total" .= v]) }) ks"#,
            r#"pure totals"#,
        ],
        &[
            "processTxn :: Aeson.Value -> M ()\n\
             processTxn txn = do\n\
             \x20 let item = fromMaybe \"\" (txn ^? key \"item\" . _String)\n\
             \x20 let qty  = fromMaybe 0 (txn ^? key \"qty\" . _Int)\n\
             \x20 prev <- send (KvGet item)\n\
             \x20 let old = case prev >>= (^? _String) of { Just p -> maybe 0 fromIntegral (readMaybeInt p); Nothing -> 0 :: Int }\n\
             \x20 let new_ = old + fromIntegral qty\n\
             \x20 kvSet item (toJSON (show new_))\n\
             \x20 send (Print (item `T.append` \": \" `T.append` show new_))",
            "readMaybeInt :: Text -> Maybe Int\n\
             readMaybeInt t = case unpack t of\n\
             \x20 [] -> Nothing\n\
             \x20 cs -> Just (foldl' (\\acc c -> acc * 10 + fromEnum c - 48) 0 cs)",
        ],
    );
    assert!(logs.iter().any(|l| l == "ledger complete"));
    // Should have printed running totals
    assert!(logs.iter().any(|l| l.contains("apple")));
    assert!(logs.iter().any(|l| l.contains("banana")));
}

/// JSON schema validator: check that an input object has required fields
/// with correct types, report violations via Console, return pass/fail.
#[test]
fn test_orchestrate_schema_validator() {
    let input = serde_json::json!({
        "name": "Alice",
        "age": 30,
        "email": null,
        "active": true
    });
    let (_json, logs) = run_aeson_effectful_with_input(
        &[
            r#"let schema = [ ("name", "string"), ("age", "number"), ("email", "string"), ("active", "bool") ] :: [(Text, Text)]"#,
            r#"errors <- mapM (checkField input) schema"#,
            r#"let errs = catMaybes errors"#,
            r#"mapM_ (\e -> send (Print e)) errs"#,
            r#"pure (object ["valid" .= (null errs), "errors" .= errs])"#,
        ],
        &[
            "checkField :: Aeson.Value -> (Text, Text) -> M (Maybe Text)\n\
             checkField obj (name, typ) = do\n\
             \x20 let field = obj ^? key name\n\
             \x20 case field of\n\
             \x20   Nothing -> pure (Just (name `T.append` \": missing\"))\n\
             \x20   Just v  -> case typ of\n\
             \x20     \"string\" -> case v ^? _String of\n\
             \x20       Just _  -> pure Nothing\n\
             \x20       Nothing -> pure (Just (name `T.append` \": expected string\"))\n\
             \x20     \"number\" -> case v ^? _Number of\n\
             \x20       Just _  -> pure Nothing\n\
             \x20       Nothing -> pure (Just (name `T.append` \": expected number\"))\n\
             \x20     \"bool\" -> case v ^? _Bool of\n\
             \x20       Just _  -> pure Nothing\n\
             \x20       Nothing -> pure (Just (name `T.append` \": expected bool\"))\n\
             \x20     _ -> pure (Just (name `T.append` \": unknown type\"))",
        ],
        input,
    );
    // "email" is null, not a string — should fail validation
    assert!(logs.iter().any(|l| l.contains("email: expected string")));
}

/// JSON→KV cache pattern: serialize object fields into KV store,
/// then reconstruct a new JSON object from KV reads.
#[test]
fn test_orchestrate_serialize_to_kv_and_reconstruct() {
    let (json, _logs) = run_aeson_effectful_with_helpers(
        &[
            // Build and persist
            r#"let person = object ["name" .= ("Bob" :: Text), "role" .= ("admin" :: Text), "level" .= (5 :: Int)]"#,
            r#"persistObject "user:1" person"#,
            // Reconstruct from KV
            r#"reconstructed <- reconstructObject "user:1" ["name", "role", "level"]"#,
            r#"pure reconstructed"#,
        ],
        &[
            "persistObject :: Text -> Aeson.Value -> M ()\n\
             persistObject prefix obj = do\n\
             \x20 case obj ^? _Object of\n\
             \x20   Nothing -> pure ()\n\
             \x20   Just m -> do\n\
             \x20     let pairs = KM.toList m\n\
             \x20     mapM_ (\\(k, v) -> do\n\
             \x20       let fieldKey = prefix `T.append` \":\" `T.append` Aeson.toText k\n\
             \x20       let val = case v ^? _String of\n\
             \x20                   Just s -> s\n\
             \x20                   Nothing -> case v ^? _Int of\n\
             \x20                     Just n -> show (fromIntegral n :: Int)\n\
             \x20                     Nothing -> \"null\"\n\
             \x20       kvSet fieldKey (toJSON val)\n\
             \x20       ) pairs",
            "reconstructObject :: Text -> [Text] -> M Aeson.Value\n\
             reconstructObject prefix fields = do\n\
             \x20 pairs <- mapM (\\f -> do\n\
             \x20   let fieldKey = prefix `T.append` \":\" `T.append` f\n\
             \x20   mval <- send (KvGet fieldKey)\n\
             \x20   pure (f .= fromMaybe \"\" (mval >>= (^? _String)))\n\
             \x20   ) fields\n\
             \x20 pure (object pairs)",
        ],
    );
    // The reconstructed object should be a proper JSON object
    assert!(json.is_object());
}

/// Nested lens pipeline: deeply transform a config object.
/// Reads a config, modifies nested values via lens composition,
/// logs changes, returns the modified config.
#[test]
fn test_orchestrate_config_transform() {
    let (json, logs) = run_aeson_effectful_with_helpers(
        &[
            r#"let cfg = object [ "server" .= object [ "host" .= ("localhost" :: Text), "port" .= (8080 :: Int) ]"#,
            r#"                 , "db" .= object [ "host" .= ("db.local" :: Text), "pool" .= (5 :: Int) ]"#,
            r#"                 , "debug" .= True ]"#,
            // Apply transformations
            r#"let cfg2 = cfg & key "server" . key "port" . _Int .~ 9090"#,
            r#"let cfg3 = cfg2 & key "debug" . _Bool .~ False"#,
            // Log what changed
            r#"let oldPort = cfg ^? key "server" . key "port" . _Int"#,
            r#"let newPort = cfg3 ^? key "server" . key "port" . _Int"#,
            r#"send (Print ("port: " `T.append` show oldPort `T.append` " -> " `T.append` show newPort))"#,
            r#"send (Print ("debug: " `T.append` show (cfg3 ^? key "debug" . _Bool)))"#,
            // Extract just the server section
            r#"let serverHost = fromMaybe "" (cfg3 ^? key "server" . key "host" . _String)"#,
            r#"pure serverHost"#,
        ],
        &[],
    );
    assert_eq!(json, "localhost");
    assert!(logs.iter().any(|l| l.contains("port:")));
    assert!(logs.iter().any(|l| l.contains("debug:")));
}

/// Map-reduce over JSON array: extract, transform, aggregate.
/// Given an array of product objects, compute total revenue per category.
#[test]
fn test_orchestrate_map_reduce_products() {
    let json = run_aeson_with_helpers(
        &[
            r#"let products = [ object ["cat" .= ("fruit" :: Text), "price" .= (3 :: Int), "qty" .= (10 :: Int)]"#,
            r#"               , object ["cat" .= ("veg" :: Text), "price" .= (2 :: Int), "qty" .= (5 :: Int)]"#,
            r#"               , object ["cat" .= ("fruit" :: Text), "price" .= (5 :: Int), "qty" .= (3 :: Int)]"#,
            r#"               , object ["cat" .= ("veg" :: Text), "price" .= (4 :: Int), "qty" .= (8 :: Int)]"#,
            r#"               ]"#,
            r#"let revenues = map (\p -> (fromMaybe "" (p ^? key "cat" . _String), revenue p)) products"#,
            r#"let sorted = sortBy (\(a,_) (b,_) -> compare a b) revenues"#,
            r#"let grouped = groupByKey sorted"#,
            r#"pure (map (\(k, vs) -> object ["category" .= k, "total" .= sumInts vs]) grouped)"#,
        ],
        &[
            "revenue :: Aeson.Value -> Int\n\
             revenue p = let { pr = fromMaybe 0 (p ^? key \"price\" . _Int) ; qt = fromMaybe 0 (p ^? key \"qty\" . _Int) } in pr * qt",
            "groupByKey :: [(Text, Int)] -> [(Text, [Int])]\n\
             groupByKey [] = []\n\
             groupByKey ((k,v):rest) =\n\
             \x20 let (same, diff) = span (\\(k2,_) -> k2 == k) rest\n\
             \x20 in (k, v : map snd same) : groupByKey diff",
            "sumInts :: [Int] -> Int\n\
             sumInts = foldl' (+) 0",
        ],
    );
    let arr = json.as_array().expect("should be array");
    assert_eq!(arr.len(), 2); // fruit, veg
}

/// Recursive JSON tree walker: flatten a nested tree structure into a list.
/// The tree has "value" and optional "children" fields.
#[test]
fn test_orchestrate_tree_flatten() {
    let json = run_aeson_with_helpers(
        &[
            r#"let tree = object [ "value" .= (1 :: Int)"#,
            r#"                  , "children" .= ([ object [ "value" .= (2 :: Int), "children" .= ([] :: [Aeson.Value]) ]"#,
            r#"                                   , object [ "value" .= (3 :: Int)"#,
            r#"                                            , "children" .= ([ object [ "value" .= (4 :: Int), "children" .= ([] :: [Aeson.Value]) ] ])"#,
            r#"                                            ]"#,
            r#"                                   ] :: [Aeson.Value])"#,
            r#"                  ]"#,
            r#"pure (flattenTree tree)"#,
        ],
        &["flattenTree :: Aeson.Value -> [Int]\n\
             flattenTree node =\n\
             \x20 let val = fromMaybe 0 (node ^? key \"value\" . _Int)\n\
             \x20     kids = node ^.. key \"children\" . _Array . traverse\n\
             \x20 in fromIntegral val : concatMap flattenTree kids"],
    );
    let arr = json.as_array().expect("should be array");
    let vals: Vec<i64> = arr.iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(vals, vec![1, 2, 3, 4]);
}

/// JSON diff: compare two objects, report added/removed/changed fields.
#[test]
fn test_orchestrate_json_diff() {
    let (json, logs) = run_aeson_effectful_with_helpers(
        &[
            r#"let old = object ["a" .= (1 :: Int), "b" .= (2 :: Int), "c" .= (3 :: Int)]"#,
            r#"let new_ = object ["a" .= (1 :: Int), "b" .= (99 :: Int), "d" .= (4 :: Int)]"#,
            r#"diffObjects old new_"#,
            r#"pure ("done" :: Text)"#,
        ],
        &[
            "diffObjects :: Aeson.Value -> Aeson.Value -> M ()\n\
             diffObjects old new_ = do\n\
             \x20 case (old ^? _Object, new_ ^? _Object) of\n\
             \x20   (Just om, Just nm) -> do\n\
             \x20     let oldKeys = map Aeson.toText (KM.keys om)\n\
             \x20     let newKeys = map Aeson.toText (KM.keys nm)\n\
             \x20     let removed = filter (\\k -> not (k `elem` newKeys)) oldKeys\n\
             \x20     let added   = filter (\\k -> not (k `elem` oldKeys)) newKeys\n\
             \x20     let common  = filter (\\k -> k `elem` newKeys) oldKeys\n\
             \x20     mapM_ (\\k -> send (Print (\"REMOVED: \" `T.append` k))) removed\n\
             \x20     mapM_ (\\k -> send (Print (\"ADDED: \" `T.append` k))) added\n\
             \x20     mapM_ (\\k -> do\n\
             \x20       let ov = old ^? key k . _Int\n\
             \x20       let nv = new_ ^? key k . _Int\n\
             \x20       when (ov /= nv) (send (Print (\"CHANGED: \" `T.append` k `T.append` \" \" `T.append` show ov `T.append` \"->\" `T.append` show nv)))\n\
             \x20       ) common\n\
             \x20   _ -> pure ()",
        ],
    );
    assert_eq!(json, "done");
    assert!(logs.iter().any(|l| l.contains("REMOVED: c")));
    assert!(logs.iter().any(|l| l.contains("ADDED: d")));
    assert!(logs.iter().any(|l| l.contains("CHANGED: b")));
}

/// Pipeline: parse input config → validate → transform → persist to KV → return summary.
/// Full end-to-end workflow combining input injection, lens, effects.
#[test]
fn test_orchestrate_full_etl_pipeline() {
    let input = serde_json::json!({
        "users": [
            {"name": "Alice", "score": 95},
            {"name": "Bob", "score": 40},
            {"name": "Charlie", "score": 78}
        ],
        "threshold": 50
    });
    let (_json, logs) = run_aeson_effectful_with_input(
        &[
            r#"let users = input ^.. key "users" . _Array . traverse"#,
            r#"let thresh = fromMaybe 50 (input ^? key "threshold" . _Int)"#,
            r#"send (Print ("threshold: " `T.append` show (fromIntegral thresh :: Int)))"#,
            // Filter and transform
            r#"let passing = filter (\u -> maybe False (>= thresh) (u ^? key "score" . _Int)) users"#,
            r#"let names = catMaybes (map (\u -> u ^? key "name" . _String) passing)"#,
            // Persist each passing user
            r#"mapM_ (\n -> kvSet ("pass:" `T.append` n) (toJSON ("true" :: Text))) names"#,
            r#"send (Print ("passing: " `T.append` show (length names)))"#,
            r#"pure (object ["count" .= length names, "names" .= names])"#,
        ],
        &[],
        input,
    );
    assert!(logs.iter().any(|l| l.contains("threshold: 50")));
    assert!(logs.iter().any(|l| l.contains("passing: 2")));
}

/// Accumulator pattern: fold over JSON array, building a new JSON object
/// incrementally and logging each step.
#[test]
fn test_orchestrate_fold_build_json() {
    let (_json, logs) = run_aeson_effectful_with_helpers(
        &[
            r#"let items = [("x", 10), ("y", 20), ("z", 30)] :: [(Text, Int)]"#,
            r#"result <- foldM buildStep (object []) items"#,
            r#"pure result"#,
        ],
        &[
            "buildStep :: Aeson.Value -> (Text, Int) -> M Aeson.Value\n\
             buildStep acc (k, v) = do\n\
             \x20 let acc2 = acc & key k . _Int .~ fromIntegral v\n\
             \x20 let count = case acc2 ^? _Object of\n\
             \x20               Just m  -> KM.size m\n\
             \x20               Nothing -> 0\n\
             \x20 send (Print (\"added \" `T.append` k `T.append` \", total fields: \" `T.append` show count))\n\
             \x20 pure acc2",
        ],
    );
    assert!(logs.len() >= 3);
    assert!(logs.iter().any(|l| l.contains("added x")));
    assert!(logs.iter().any(|l| l.contains("added z")));
}

/// Conditional branching: different effect chains based on JSON field values.
/// Simulates a simple routing/dispatch system.
#[test]
fn test_orchestrate_dispatch_on_type() {
    let input = serde_json::json!({
        "type": "greeting",
        "payload": {"name": "World", "lang": "en"}
    });
    let (_json, logs) = run_aeson_effectful_with_input(
        &[
            r#"let typ = fromMaybe "" (input ^? key "type" . _String)"#,
            r#"let payload = fromMaybe (object []) (input ^? key "payload")"#,
            r#"result <- dispatch typ payload"#,
            r#"pure result"#,
        ],
        &["dispatch :: Text -> Aeson.Value -> M Aeson.Value\n\
             dispatch typ payload = case typ of\n\
             \x20 \"greeting\" -> do\n\
             \x20   let name = fromMaybe \"stranger\" (payload ^? key \"name\" . _String)\n\
             \x20   let msg = \"Hello, \" `T.append` name `T.append` \"!\"\n\
             \x20   send (Print msg)\n\
             \x20   kvSet \"last_greeting\" (toJSON msg)\n\
             \x20   pure (object [\"response\" .= msg])\n\
             \x20 \"farewell\" -> do\n\
             \x20   send (Print \"Goodbye!\")\n\
             \x20   pure (object [\"response\" .= (\"Goodbye!\" :: Text)])\n\
             \x20 _ -> do\n\
             \x20   send (Print (\"unknown type: \" `T.append` typ))\n\
             \x20   pure (object [\"error\" .= (\"unknown\" :: Text)])"],
        input,
    );
    assert!(logs.iter().any(|l| l == "Hello, World!"));
}

/// Multi-pass processing: first pass collects metadata into KV,
/// second pass uses it to enrich the original data.
#[test]
fn test_orchestrate_two_pass_enrich() {
    let (json, logs) = run_aeson_effectful_with_helpers(
        &[
            r#"let records = [ object ["id" .= (1 :: Int), "cat" .= ("A" :: Text)]"#,
            r#"              , object ["id" .= (2 :: Int), "cat" .= ("B" :: Text)]"#,
            r#"              , object ["id" .= (3 :: Int), "cat" .= ("A" :: Text)]"#,
            r#"              , object ["id" .= (4 :: Int), "cat" .= ("A" :: Text)]"#,
            r#"              ]"#,
            // Pass 1: count per category
            r#"mapM_ countCat records"#,
            r#"send (Print "pass 1 done")"#,
            // Pass 2: enrich with count
            r#"enriched <- mapM enrichRecord records"#,
            r#"send (Print "pass 2 done")"#,
            r#"pure (length enriched)"#,
        ],
        &[
            "countCat :: Aeson.Value -> M ()\n\
             countCat rec = do\n\
             \x20 let cat = fromMaybe \"\" (rec ^? key \"cat\" . _String)\n\
             \x20 let k = \"count:\" `T.append` cat\n\
             \x20 prev <- send (KvGet k)\n\
             \x20 let n = case prev >>= (^? _String) of { Just p -> readInt p; Nothing -> 0 :: Int }\n\
             \x20 kvSet k (toJSON (show (n + 1)))",
            "enrichRecord :: Aeson.Value -> M Aeson.Value\n\
             enrichRecord rec = do\n\
             \x20 let cat = fromMaybe \"\" (rec ^? key \"cat\" . _String)\n\
             \x20 cnt <- send (KvGet (\"count:\" `T.append` cat))\n\
             \x20 let cntVal = case cnt >>= (^? _String) of { Just p -> readInt p; Nothing -> 0 :: Int }\n\
             \x20 pure (rec & key \"catCount\" . _Int .~ fromIntegral cntVal)",
            "readInt :: Text -> Int\n\
             readInt t = foldl' (\\acc c -> acc * 10 + fromEnum c - 48) 0 (unpack t)",
        ],
    );
    assert!(logs.iter().any(|l| l == "pass 1 done"));
    assert!(logs.iter().any(|l| l == "pass 2 done"));
    assert_eq!(json, 4);
}

/// Lens-heavy: compose multiple nested traversals and modifications
/// in a single expression chain.
#[test]
fn test_orchestrate_lens_composition_chain() {
    let json = run_aeson_with_helpers(
        &[
            r#"let db = object [ "tables" .= ([ object [ "name" .= ("users" :: Text)"#,
            r#"                                        , "rows" .= ([ object ["id" .= (1 :: Int), "active" .= True]"#,
            r#"                                                     , object ["id" .= (2 :: Int), "active" .= False]"#,
            r#"                                                     ] :: [Aeson.Value])"#,
            r#"                                        ]"#,
            r#"                                ] :: [Aeson.Value])"#,
            r#"                ]"#,
            // Extract all IDs from all tables' rows
            r#"let ids = db ^.. key "tables" . _Array . traverse . key "rows" . _Array . traverse . key "id" . _Int"#,
            // Count active rows
            r#"let actives = db ^.. key "tables" . _Array . traverse . key "rows" . _Array . traverse . key "active" . _Bool"#,
            r#"let activeCount = length (filter (\x -> x) actives)"#,
            r#"pure (object ["ids" .= (map fromIntegral ids :: [Int]), "activeCount" .= activeCount])"#,
        ],
        &[],
    );
    assert!(json.is_object());
}

/// Error recovery pattern: try operations, catch failures via Maybe,
/// log problems, continue with defaults.
#[test]
fn test_orchestrate_graceful_degradation() {
    let input = serde_json::json!({
        "required": {"name": "test"},
        "optional": null
    });
    let (_json, logs) = run_aeson_effectful_with_input(
        &[
            // Required field — will succeed
            r#"let name = fromMaybe "unnamed" (input ^? key "required" . key "name" . _String)"#,
            r#"send (Print ("name: " `T.append` name))"#,
            // Optional nested field — will fail gracefully
            r#"let extra = input ^? key "optional" . key "detail" . _String"#,
            r#"when (isNothing extra) (send (Print "optional.detail missing, using default"))"#,
            r#"let detail = fromMaybe "default_detail" extra"#,
            // Deep optional that doesn't exist at all
            r#"let deep = input ^? key "nonexistent" . key "very" . key "deep" . _Int"#,
            r#"let deepVal = fromMaybe 0 (fmap fromIntegral deep) :: Int"#,
            r#"send (Print ("deep: " `T.append` show deepVal))"#,
            r#"pure (object ["name" .= name, "detail" .= detail, "deep" .= deepVal])"#,
        ],
        &[],
        input,
    );
    assert!(logs.iter().any(|l| l == "name: test"));
    assert!(logs.iter().any(|l| l.contains("missing, using default")));
    assert!(logs.iter().any(|l| l == "deep: 0"));
}

/// State machine with JSON-encoded state: each step reads current state
/// from KV, applies a transition, persists the new state.
#[test]
fn test_orchestrate_kv_state_machine() {
    let (_json, logs) = run_aeson_effectful_with_helpers(
        &[
            // Initialize state
            r#"let init = object ["step" .= (0 :: Int), "data" .= ([] :: [Int])]"#,
            r#"kvSet "state" (toJSON (encodeSimple init))"#,
            // Run 5 transitions
            r#"mapM_ (\_ -> transition) [1..5 :: Int]"#,
            // Read final state
            r#"finalRaw <- send (KvGet "state")"#,
            r#"let finalStr = finalRaw >>= (^? _String)"#,
            r#"send (Print ("final: " `T.append` fromMaybe "" finalStr))"#,
            r#"pure (fromMaybe "" finalStr)"#,
        ],
        &[
            "transition :: M ()\n\
             transition = do\n\
             \x20 ms <- send (KvGet \"state\")\n\
             \x20 case ms >>= (^? _String) of\n\
             \x20   Nothing -> pure ()\n\
             \x20   Just s -> do\n\
             \x20     let step = readInt (T.takeWhile (\\c -> c /= ',') (T.drop 1 s))\n\
             \x20     let newStep = step + 1\n\
             \x20     let newState = \"(\" `T.append` show newStep `T.append` \")\"\n\
             \x20     kvSet \"state\" (toJSON newState)\n\
             \x20     send (Print (\"step -> \" `T.append` show newStep))",
            "encodeSimple :: Aeson.Value -> Text\n\
             encodeSimple v = case v ^? key \"step\" . _Int of\n\
             \x20 Just n -> \"(\" `T.append` show (fromIntegral n :: Int) `T.append` \")\"\n\
             \x20 Nothing -> \"(0)\"",
            "readInt :: Text -> Int\n\
             readInt t = foldl' (\\acc c -> acc * 10 + fromEnum c - 48) 0 (unpack t)",
        ],
    );
    // Should have logged 5 transitions
    let step_logs: Vec<_> = logs.iter().filter(|l| l.contains("step ->")).collect();
    assert_eq!(step_logs.len(), 5);
}

/// JSON array manipulation: zip, interleave, chunk.
#[test]
fn test_orchestrate_array_gymnastics() {
    let json = run_aeson_with_helpers(
        &[
            r#"let xs = map (\n -> object ["n" .= n, "sq" .= (n * n)]) [1..6 :: Int]"#,
            // Take first 3
            r#"let first3 = take 3 xs"#,
            // Take last 3
            r#"let last3 = drop 3 xs"#,
            // Zip them into pairs
            r#"let zipped = zipWith (\a b -> object ["a" .= (a ^? key "n" . _Int), "b" .= (b ^? key "n" . _Int)]) first3 last3"#,
            // Extract all "sq" values from original
            r#"let squares = catMaybes (map (\x -> fmap fromIntegral (x ^? key "sq" . _Int) :: Maybe Int) xs)"#,
            r#"pure (object ["zipped" .= zipped, "squares" .= squares])"#,
        ],
        &[],
    );
    assert!(json.is_object());
}

/// Build a mini DSL: JSON objects represent "instructions" that get
/// interpreted into effects.
#[test]
fn test_orchestrate_instruction_dsl() {
    let (json, logs) = run_aeson_effectful_with_helpers(
        &[
            r#"let program = [ object ["instr" .= ("log" :: Text), "msg" .= ("hello" :: Text)]"#,
            r#"              , object ["instr" .= ("store" :: Text), "k" .= ("x" :: Text), "v" .= ("42" :: Text)]"#,
            r#"              , object ["instr" .= ("log" :: Text), "msg" .= ("stored x" :: Text)]"#,
            r#"              , object ["instr" .= ("load" :: Text), "k" .= ("x" :: Text)]"#,
            r#"              ]"#,
            r#"results <- mapM execInstr program"#,
            r#"pure (catMaybes results)"#,
        ],
        &["execInstr :: Aeson.Value -> M (Maybe Aeson.Value)\n\
             execInstr instr = do\n\
             \x20 let op = fromMaybe \"\" (instr ^? key \"instr\" . _String)\n\
             \x20 case op of\n\
             \x20   \"log\" -> do\n\
             \x20     let msg = fromMaybe \"\" (instr ^? key \"msg\" . _String)\n\
             \x20     send (Print msg)\n\
             \x20     pure Nothing\n\
             \x20   \"store\" -> do\n\
             \x20     let k = fromMaybe \"\" (instr ^? key \"k\" . _String)\n\
             \x20     let v = fromMaybe \"\" (instr ^? key \"v\" . _String)\n\
             \x20     kvSet k (toJSON v)\n\
             \x20     pure Nothing\n\
             \x20   \"load\" -> do\n\
             \x20     let k = fromMaybe \"\" (instr ^? key \"k\" . _String)\n\
             \x20     v <- send (KvGet k)\n\
             \x20     pure (Just (object [\"loaded\" .= k, \"value\" .= v]))\n\
             \x20   _ -> pure Nothing"],
    );
    assert!(logs.iter().any(|l| l == "hello"));
    assert!(logs.iter().any(|l| l == "stored x"));
    // Should have one "load" result
    let arr = json.as_array().expect("should be array");
    assert_eq!(arr.len(), 1);
}

/// Lens set on deeply nested array element, then read it back.
#[test]
fn test_orchestrate_deep_array_modify() {
    let json = run_aeson(&[
        r#"let matrix = object ["grid" .= ([ [1,2,3], [4,5,6], [7,8,9] ] :: [[Int]])]"#,
        // Read the center element: grid[1][1]
        r#"let center = matrix ^? key "grid" . nth 1 . nth 1 . _Int"#,
        r#"pure center"#,
    ]);
    // [1][1] = 5
    assert_eq!(json, 5);
}

/// Combine toJSON with lens to do round-trip transformations.
#[test]
fn test_orchestrate_tojson_lens_roundtrip() {
    let json = run_aeson(&[
        // Build structured data via toJSON
        r#"let pairs = [("alpha", 1), ("beta", 2), ("gamma", 3)] :: [(Text, Int)]"#,
        r#"let asJson = toJSON pairs"#,
        // Now traverse the resulting array of pairs
        r#"let firstPairs = asJson ^.. _Array . traverse . nth 0 . _String"#,
        r#"pure firstPairs"#,
    ]);
    let arr = json.as_array().expect("should be array");
    let vals: Vec<&str> = arr.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(vals, vec!["alpha", "beta", "gamma"]);
}

/// Stress: build a moderately large JSON structure and traverse it.
#[test]
fn test_orchestrate_moderate_scale() {
    let json = run_aeson_with_helpers(
        &[
            r#"let items = map mkItem [1..10 :: Int]"#,
            r#"let catalog = object ["items" .= items, "count" .= (10 :: Int)]"#,
            // Sum all prices
            r#"let prices = catalog ^.. key "items" . _Array . traverse . key "price" . _Int"#,
            r#"let total = foldl' (\acc x -> acc + fromIntegral x) (0 :: Int) prices"#,
            r#"pure total"#,
        ],
        &["mkItem :: Int -> Aeson.Value\n\
             mkItem i = object [ \"id\" .= i\n\
             \x20                , \"name\" .= (\"item_\" `T.append` show i)\n\
             \x20                , \"price\" .= (i * 10)\n\
             \x20                , \"tags\" .= ([\"t\" `T.append` show i, \"all\"] :: [Text]) ]"],
    );
    // Sum of 10+20+...+100 = 550
    assert_eq!(json, 550);
}

#[test]
fn test_ir_dump_pap() {
    let src = mcp_source(&[
        r#"kvSet "xx1" (toJSON ("v1" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"let results = map (T.drop 2) keys"#,
        r#"pure (T.length (head results))"#,
    ]);
    let pp = prelude_path();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let (expr, _table, _) = compile_haskell(&src, "result", &include).expect("compile");
            let ir = tidepool_repr::pretty::pretty_print(&expr);
            std::fs::write("/tmp/ir_pap.txt", &ir).unwrap();
            eprintln!("PAP IR: {} nodes, {} bytes", expr.nodes.len(), ir.len());
        })
        .unwrap()
        .join()
        .expect("join");
}

#[test]
fn test_ir_dump_eta() {
    let src = mcp_source(&[
        r#"kvSet "xx1" (toJSON ("v1" :: Text))"#,
        r#"keys <- send KvKeys"#,
        r#"let results = map (\x -> T.drop 2 x) keys"#,
        r#"pure (T.length (head results))"#,
    ]);
    let pp = prelude_path();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let (expr, _table, _) = compile_haskell(&src, "result", &include).expect("compile");
            let ir = tidepool_repr::pretty::pretty_print(&expr);
            std::fs::write("/tmp/ir_eta.txt", &ir).unwrap();
            eprintln!("Eta IR: {} nodes, {} bytes", expr.nodes.len(), ir.len());
        })
        .unwrap()
        .join()
        .expect("join");
}

#[test]
fn test_group_b_simple_thunk() {
    let json = run_aeson(&[
        r#"let xs = [True, False]"#,
        r#"case head xs of { True -> pure (toJSON (1.0 :: Double)); False -> pure (toJSON (0.0 :: Double)) }"#,
    ]);
    // Aeson Number now renders directly as a number
    assert_eq!(json, serde_json::json!(1));
}

// ---------------------------------------------------------------------------
// ToCore JSON Value constructor ID tests
// ---------------------------------------------------------------------------

/// Diagnostic: dump the DataConTable entries for aeson Value constructor names.
/// This reveals whether `get_by_name("Array")` etc. return the right DataConId.
#[test]
fn test_tocore_json_datacon_ids_match_haskell() {
    use tidepool_bridge::ToCore;

    // Compile a Haskell program that constructs each aeson Value variant
    let src = mcp_source_with_imports(
        &[
            r#"pure (Aeson.Array [Aeson.String "x", Aeson.Number 1.0, Aeson.Bool True, Aeson.Null])"#,
        ],
        &[],
        &aeson_import_strs(),
    );
    let pp = prelude_path();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let val = compile_and_run(&src, "result", &include, &mut HNil, &())
                .expect("compile_and_run failed");
            let table = val.table().clone();
            let haskell_val = val.into_value();

            // The Haskell-produced value is Array [String "x", Number 1.0, Bool True, Null]
            // Extract the Array constructor ID from the Haskell-side value
            let haskell_array_id = match &haskell_val {
                Value::Con(id, _) => *id,
                other => panic!("expected Con(Array, ...), got {:?}", other),
            };
            let haskell_array_name = table.name_of(haskell_array_id).unwrap();
            assert_eq!(haskell_array_name, "Array", "Haskell value should be Array constructor");

            // Now construct the same thing via ToCore
            let json = serde_json::json!(["x", 1.0, true, null]);
            let tocore_val = json.to_value(&table).expect("ToCore should succeed");
            let tocore_array_id = match &tocore_val {
                Value::Con(id, _) => *id,
                other => panic!("expected Con(Array, ...) from ToCore, got {:?}", other),
            };

            // Dump all names that ToCore uses
            for name in &["Object", "Array", "String", "Number", "Bool", "Null", "Key", "Bin", "Tip"] {
                let id = table.get_by_name(name);
                eprintln!("  get_by_name({:?}) = {:?}", name, id);
            }
            // Dump ALL entries named "Array" or "String"
            for dc in table.iter() {
                if dc.name == "Array" || dc.name == "String" || dc.name == "Object" || dc.name == "Number" || dc.name == "Bool" || dc.name == "Null" {
                    eprintln!("  DC: name={:?} id={:?} tag={} arity={}", dc.name, dc.id, dc.tag, dc.rep_arity);
                }
            }

            // THE KEY ASSERTION: these should be the same DataConId
            assert_eq!(
                haskell_array_id, tocore_array_id,
                "ToCore Array DataConId ({:?}, name={:?}) != Haskell Array DataConId ({:?}, name={:?})",
                tocore_array_id, table.name_of(tocore_array_id),
                haskell_array_id, table.name_of(haskell_array_id),
            );

            // Also check String
            let haskell_elems = match &haskell_val {
                Value::Con(_, fields) => fields,
                _ => unreachable!(),
            };
            // Array field is a cons list; first element is the first Value in the array
            // Navigate: Array [list] -> (:) head tail -> head = String "x"
            let first_elem = match &haskell_elems[0] {
                Value::Con(_, fields) => &fields[0], // (:) cons → head
                other => panic!("expected cons list, got {:?}", other),
            };
            let haskell_string_id = match first_elem {
                Value::Con(id, _) => *id,
                other => panic!("expected Con(String, ...), got {:?}", other),
            };

            let json_str = serde_json::json!("x");
            let tocore_str = json_str.to_value(&table).expect("ToCore String");
            let tocore_string_id = match &tocore_str {
                Value::Con(id, _) => *id,
                other => panic!("expected Con(String, ...) from ToCore, got {:?}", other),
            };

            assert_eq!(
                haskell_string_id, tocore_string_id,
                "ToCore String DataConId ({:?}, name={:?}) != Haskell String DataConId ({:?}, name={:?})",
                tocore_string_id, table.name_of(tocore_string_id),
                haskell_string_id, table.name_of(haskell_string_id),
            );

            eprintln!("Haskell Array ID: {:?}, ToCore Array ID: {:?}", haskell_array_id, tocore_array_id);
            eprintln!("Haskell String ID: {:?}, ToCore String ID: {:?}", haskell_string_id, tocore_string_id);
        })
        .unwrap()
        .join()
        .expect("thread panicked");
}

/// Test that _Array lens works on ToCore-produced Values (currently fails).
#[test]
fn test_tocore_json_array_lens_works() {
    // Compile Haskell that takes an input Value and extracts via _Array lens
    let input = serde_json::json!(["keep", "refactor", "skip"]);
    let json = run_aeson_with_input(&[r#"pure (input ^.. _Array . traverse . _String)"#], input);
    // Should extract the three strings
    assert_eq!(json, serde_json::json!(["keep", "refactor", "skip"]));
}

// ===========================================================================
// JSON decode tests
// ===========================================================================

#[test]
fn test_decode_object() {
    let json = run_aeson(&[
        r#"let t = "{\"name\": \"alice\", \"age\": 30}" :: Text"#,
        r#"case decode t of { Just v -> pure v; Nothing -> pure Null }"#,
    ]);
    // Age is a Double in our Value type, extract and check
    assert_eq!(json.get("name"), Some(&serde_json::json!("alice")));
    let age = json.get("age").unwrap().as_f64().unwrap();
    assert!((age - 30.0).abs() < 0.001);
}

#[test]
fn test_decode_array() {
    let json = run_aeson(&[
        r#"let t = "[true, null, \"hi\"]" :: Text"#,
        r#"pure (decode t)"#,
    ]);
    assert_eq!(json, serde_json::json!([true, null, "hi"]));
}

#[test]
fn test_decode_invalid() {
    let json = run_aeson(&[r#"pure (decode "{bad}" :: Maybe Value)"#]);
    assert_eq!(json, serde_json::json!(null));
}

#[test]
fn test_decode_nested_then_lens() {
    let json = run_aeson(&[
        r#"let t = "{\"items\": [{\"x\": 1}, {\"x\": 2}]}" :: Text"#,
        r#"case decode t of"#,
        r#"  Just v -> do"#,
        r#"    let xs = v ^.. key "items" . _Array . traverse . key "x" . _Int"#,
        r#"    pure (map fromIntegral xs :: [Int])"#,
        r#"  Nothing -> pure ([] :: [Int])"#,
    ]);
    assert_eq!(json, serde_json::json!([1, 2]));
}

#[test]
fn test_decode_escapes() {
    let json = run_aeson(&[
        r#"let t = "{\"msg\": \"line1\\nline2\\ttab\"}" :: Text"#,
        r#"case decode t of { Just v -> pure (v ?. "msg"); Nothing -> pure Nothing }"#,
    ]);
    assert_eq!(json, serde_json::json!("line1\nline2\ttab"));
}

#[test]
fn test_decode_empty_structures() {
    let json = run_aeson(&[r#"pure (decode "{}" :: Maybe Value, decode "[]" :: Maybe Value)"#]);
    assert_eq!(json, serde_json::json!([{}, []]));
}

#[test]
fn test_decode_scientific_notation() {
    let json = run_aeson(&[
        r#"case decode "1.5e2" of { Just (Number d) -> pure (truncate d :: Int); _ -> pure (0 :: Int) }"#,
    ]);
    assert_eq!(json, serde_json::json!(150));
}

#[test]
fn test_sort_values() {
    let json = run_aeson(&[
        r#"let vs = [String "c", String "a", String "b"]"#,
        r#"pure (sort vs)"#,
    ]);
    assert_eq!(json, serde_json::json!(["a", "b", "c"]));
}

// ===========================================================================
// P1: Char predicates
// ===========================================================================

#[test]
fn test_is_digit_true() {
    let json = run_plain("isDigit '5'");
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_is_digit_false() {
    let json = run_plain("isDigit 'a'");
    assert_eq!(json, serde_json::json!(false));
}

#[test]
fn test_is_alpha_true() {
    let json = run_plain("isAlpha 'z'");
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_is_alpha_false() {
    let json = run_plain("isAlpha '3'");
    assert_eq!(json, serde_json::json!(false));
}

#[test]
fn test_is_alpha_num() {
    let json = run_plain("(isAlphaNum 'a', isAlphaNum '5', isAlphaNum '!')");
    assert_eq!(json, serde_json::json!([true, true, false]));
}

#[test]
fn test_is_space() {
    let json = run_plain("(isSpace ' ', isSpace '\\t', isSpace 'x')");
    assert_eq!(json, serde_json::json!([true, true, false]));
}

#[test]
fn test_is_upper_lower() {
    let json = run_plain("(isUpper 'A', isUpper 'a', isLower 'z', isLower 'Z')");
    assert_eq!(json, serde_json::json!([true, false, true, false]));
}

#[test]
fn test_digit_to_int() {
    let json = run_plain("(digitToInt '0', digitToInt '9', digitToInt 'a', digitToInt 'F')");
    assert_eq!(json, serde_json::json!([0, 9, 10, 15]));
}

#[test]
fn test_to_lower_char() {
    let json = run_plain("map toLowerChar (unpack \"Hello\")");
    assert_eq!(json, serde_json::json!("hello"));
}

#[test]
fn test_to_upper_char() {
    let json = run_plain("map toUpperChar (unpack \"hello\")");
    assert_eq!(json, serde_json::json!("HELLO"));
}

// ===========================================================================
// P1: Monomorphic numeric helpers
// ===========================================================================

#[test]
fn test_abs_prime() {
    let json = run_plain("(abs' 5, abs' (-3), abs' 0)");
    assert_eq!(json, serde_json::json!([5, 3, 0]));
}

#[test]
fn test_signum_prime() {
    let json = run_plain("(signum' 10, signum' (-5), signum' 0)");
    assert_eq!(json, serde_json::json!([1, -1, 0]));
}

#[test]
fn test_min_max_prime() {
    let json = run_plain("(min' 3 7, max' 3 7)");
    assert_eq!(json, serde_json::json!([3, 7]));
}

// ===========================================================================
// P2: Additional list combinators
// ===========================================================================

#[test]
fn test_elem_index() {
    let json = run_plain("(elemIndex 3 [1,2,3,4 :: Int], elemIndex 9 [1,2,3 :: Int])");
    assert_eq!(json, serde_json::json!([2, null]));
}

#[test]
fn test_find_index() {
    let json = run_plain("findIndex (> 3) [1,2,3,4,5 :: Int]");
    assert_eq!(json, serde_json::json!(3));
}

#[test]
fn test_zip3() {
    let json = run_plain("zip3 [1,2 :: Int] [10,20 :: Int] [100,200 :: Int]");
    assert_eq!(json, serde_json::json!([[1, 10, 100], [2, 20, 200]]));
}

#[test]
fn test_unzip3() {
    let json = run_plain("unzip3 [(1,10,100),(2,20,200) :: (Int,Int,Int)]");
    assert_eq!(json, serde_json::json!([[1, 2], [10, 20], [100, 200]]));
}

// ===========================================================================
// P3: Tidepool.Text module
// ===========================================================================

#[test]
fn test_camel_to_snake() {
    let json = run_mcp_with_imports(
        &[r#"pure (camelToSnake "helloWorld" :: Text)"#],
        &[],
        &["Tidepool.Text"],
    );
    // camelToSnake prepends _ before uppercase: "helloWorld" -> "_hello_world" with leading _
    // Actually let me check: go ('h':cs) -> 'h' : go cs, then 'e', ..., then 'W' -> '_':'w': go cs
    // Wait no - first char 'h' is not upper, so it stays. Then 'W' is upper -> '_':'w'
    // So result is "hello_world"
    assert_eq!(json, serde_json::json!("hello_world"));
}

#[test]
fn test_snake_to_camel() {
    let json = run_mcp_with_imports(
        &[r#"pure (snakeToCamel "hello_world" :: Text)"#],
        &[],
        &["Tidepool.Text"],
    );
    assert_eq!(json, serde_json::json!("helloWorld"));
}

#[test]
fn test_capitalize() {
    let json = run_mcp_with_imports(
        &[r#"pure (capitalize "hello" :: Text)"#],
        &[],
        &["Tidepool.Text"],
    );
    assert_eq!(json, serde_json::json!("Hello"));
}

#[test]
fn test_title_case() {
    let json = run_mcp_with_imports(
        &[r#"pure (titleCase "hello world" :: Text)"#],
        &[],
        &["Tidepool.Text"],
    );
    assert_eq!(json, serde_json::json!("Hello World"));
}

#[test]
fn test_slugify() {
    let json = run_mcp_with_imports(
        &[r#"pure (slugify "Hello, World!" :: Text)"#],
        &[],
        &["Tidepool.Text"],
    );
    assert_eq!(json, serde_json::json!("hello-world"));
}

#[test]
fn test_truncate_text() {
    let json = run_mcp_with_imports(
        &[r#"pure (truncateText 8 "Hello, World!" :: Text)"#],
        &[],
        &["Tidepool.Text"],
    );
    assert_eq!(json, serde_json::json!("Hello..."));
}

#[test]
fn test_truncate_text_short() {
    let json = run_mcp_with_imports(
        &[r#"pure (truncateText 20 "Hello" :: Text)"#],
        &[],
        &["Tidepool.Text"],
    );
    assert_eq!(json, serde_json::json!("Hello"));
}

#[test]
fn test_indent() {
    let json = run_mcp_with_imports(
        &[r#"pure (indent 4 "line1\nline2" :: Text)"#],
        &[],
        &["Tidepool.Text"],
    );
    assert_eq!(json, serde_json::json!("    line1\n    line2\n"));
}

#[test]
fn test_pad_left_right() {
    let json = run_mcp_with_imports(
        &[r#"pure (padLeft 8 '0' "42", padRight 8 '.' "hi")"#],
        &[],
        &["Tidepool.Text"],
    );
    assert_eq!(json, serde_json::json!(["00000042", "hi......"]));
}

#[test]
fn test_center() {
    let json = run_mcp_with_imports(
        &[r#"pure (center 10 '-' "hi" :: Text)"#],
        &[],
        &["Tidepool.Text"],
    );
    assert_eq!(json, serde_json::json!("----hi----"));
}

// ===========================================================================
// P4: Tidepool.Table module
// ===========================================================================

#[test]
fn test_parse_csv() {
    let json = run_mcp_with_imports(
        &[r#"pure (parseCsv "name,age\nAlice,30\nBob,25" :: [[Text]])"#],
        &[],
        &["Tidepool.Table"],
    );
    assert_eq!(
        json,
        serde_json::json!([["name", "age"], ["Alice", "30"], ["Bob", "25"]])
    );
}

#[test]
fn test_parse_tsv() {
    let json = run_mcp_with_imports(
        &[r#"pure (parseTsv "a\tb\n1\t2" :: [[Text]])"#],
        &[],
        &["Tidepool.Table"],
    );
    assert_eq!(json, serde_json::json!([["a", "b"], ["1", "2"]]));
}

#[test]
fn test_column() {
    let json = run_mcp_with_imports(
        &[
            r#"let rows = parseCsv "name,age\nAlice,30\nBob,25""#,
            r#"pure (column 0 rows :: [Text])"#,
        ],
        &[],
        &["Tidepool.Table"],
    );
    assert_eq!(json, serde_json::json!(["name", "Alice", "Bob"]));
}

#[test]
fn test_render_table() {
    let json = run_mcp_with_imports(
        &[
            r#"let rows = [["Name","Age"],["Alice","30"],["Bob","25"]] :: [[Text]]"#,
            r#"pure (renderTable rows :: Text)"#,
        ],
        &[],
        &["Tidepool.Table"],
    );
    // Each row should be pipe-delimited with padding
    let s = json.as_str().unwrap();
    assert!(s.contains("| Name  | Age |"), "got: {s}");
    assert!(s.contains("| Alice | 30  |"), "got: {s}");
}

#[test]
fn test_sort_by_column() {
    let json = run_mcp_with_imports(
        &[
            r#"let rows = parseCsv "name,age\nCharlie,20\nAlice,30\nBob,25""#,
            r#"pure (sortByColumn 0 rows :: [[Text]])"#,
        ],
        &[],
        &["Tidepool.Table"],
    );
    // Header stays, rest sorted by name
    assert_eq!(
        json,
        serde_json::json!([
            ["name", "age"],
            ["Alice", "30"],
            ["Bob", "25"],
            ["Charlie", "20"]
        ])
    );
}

// ===========================================================================
// Pagination tests
// ===========================================================================

/// Small values pass through paginateResult unchanged (fast path).
#[test]
fn test_paginate_small_passthrough() {
    let json = run_aeson(&["pure [1, 2, 3 :: Int]"]);
    assert_eq!(json, serde_json::json!([1, 2, 3]));
}

/// Large string gets truncated by paginateResult.
#[test]
fn test_paginate_large_string() {
    let json = run_aeson(&[
        r#"let s10 = "abcdefghij""#,
        r#"    s100 = s10 <> s10 <> s10 <> s10 <> s10 <> s10 <> s10 <> s10 <> s10 <> s10"#,
        r#"    s1000 = s100 <> s100 <> s100 <> s100 <> s100 <> s100 <> s100 <> s100 <> s100 <> s100"#,
        r#"    s5000 = s1000 <> s1000 <> s1000 <> s1000 <> s1000"#,
        r#"pure (String s5000)"#,
    ]);
    // Result should be a truncated string with a size marker
    let s = json.as_str().expect("should be string");
    assert!(
        s.contains("...[5000 chars]"),
        "should contain size marker, got: {}...{}",
        &s[..50],
        &s[s.len() - 30..]
    );
    assert!(s.len() < 5000, "should be truncated, len={}", s.len());
}

/// Say output is a normal Print effect — no truncation. Full text passes through.
#[test]
fn test_paginate_say_passthrough() {
    let (json, output) = run_aeson_effectful(&[
        r#"let s10 = "abcdefghij""#,
        r#"    s100 = s10 <> s10 <> s10 <> s10 <> s10 <> s10 <> s10 <> s10 <> s10 <> s10"#,
        r#"    s1000 = s100 <> s100 <> s100 <> s100 <> s100 <> s100 <> s100 <> s100 <> s100 <> s100"#,
        r#"    s5000 = s1000 <> s1000 <> s1000 <> s1000 <> s1000"#,
        r#"say s5000"#,
        r#"pure (42 :: Int)"#,
    ]);
    assert_eq!(json, serde_json::json!(42));
    assert_eq!(output.len(), 1);
    let line = &output[0];
    // say is a normal effect now — full text passes through
    assert_eq!(
        line.len(),
        5000,
        "say should pass full text, got len={}",
        line.len()
    );
}
