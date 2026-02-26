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
    let preamble = tidepool_mcp::build_preamble(&decls);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let helpers: Vec<String> = helpers.iter().map(|s| s.to_string()).collect();
    tidepool_mcp::template_haskell(&preamble, &stack, &lines, &[], &helpers, None)
}

fn mcp_source_with_imports(lines: &[&str], helpers: &[&str], imports: &[&str]) -> String {
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let helpers: Vec<String> = helpers.iter().map(|s| s.to_string()).collect();
    let imports: Vec<String> = imports.iter().map(|s| s.to_string()).collect();
    tidepool_mcp::template_haskell(&preamble, &stack, &lines, &imports, &helpers, None)
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
            let (expr, table) =
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
    vec![
        "Data.Aeson (Value(..), object, (.=), encode, decode, toJSON, fromJSON, Result(..))",
        "Data.Aeson.Lens (key, nth, _String, _Number, _Bool, _Array, _Object, _Integer, _Double)",
        "qualified Data.Aeson as Aeson",
        "qualified Data.Aeson.Key as Key",
        "qualified Data.Aeson.KeyMap as KM",
        "qualified Data.Vector as V",
        "Control.Lens (preview, toListOf, (^?), (^..), (&), (.~), (%~), to, _Just, traverse)",
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

fn run_aeson_with_input(lines: &[&str], input: serde_json::Value) -> serde_json::Value {
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines_owned: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let imports: Vec<String> = aeson_import_strs().iter().map(|s| s.to_string()).collect();
    let src = tidepool_mcp::template_haskell(
        &preamble, &stack, &lines_owned, &imports, &[], Some(&input),
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
    let json = run_aeson(&[
        r#"pure (object ["name" .= ("Alice" :: Text), "age" .= (30 :: Int)])"#,
    ]);
    assert_eq!(json["constructor"], "Object");
    // The exact structure depends on how aeson's Object renders through our bridge.
    // At minimum, it should not crash.
}

/// Construct a JSON array via toJSON
#[test]
#[ignore] // needs Array#-aware heap bridge rendering for Vector internals
fn test_aeson_array_tojson() {
    let json = run_aeson(&[
        r#"pure (toJSON [1, 2, 3 :: Int])"#,
    ]);
    // aeson's toJSON [Int] produces Array (Vector Value)
    // Our bridge should traverse the Vector's Array# internals
    assert!(!json.is_null(), "toJSON [1,2,3] should not be null");
}

/// Construct Aeson.Null
#[test]
fn test_aeson_null() {
    let json = run_aeson(&[
        r#"pure Aeson.Null"#,
    ]);
    // Should render as the "Null" constructor
    assert_eq!(json, serde_json::json!("Null"));
}

/// Construct Aeson.Bool
#[test]
fn test_aeson_bool_true() {
    let json = run_aeson(&[
        r#"pure (Aeson.Bool True)"#,
    ]);
    // Bool True wraps our True constructor
    assert_eq!(json, serde_json::json!({"constructor": "Bool", "fields": [true]}));
}

/// Construct Aeson.String
#[test]
fn test_aeson_string() {
    let json = run_aeson(&[
        r#"pure (Aeson.String "hello world")"#,
    ]);
    assert_eq!(json, serde_json::json!({"constructor": "String", "fields": ["hello world"]}));
}

/// Construct Aeson.Number from Int
#[test]
fn test_aeson_number_int() {
    let json = run_aeson(&[
        r#"pure (toJSON (42 :: Int))"#,
    ]);
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
    assert_eq!(json, serde_json::json!(42.0));
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
    assert_eq!(json, serde_json::json!(20.0));
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

/// Use (.~) to set a field value
#[test]
#[ignore] // Requires Integer arithmetic (Scientific internals) — needs GMP FFI support
fn test_aeson_lens_set_field() {
    let json = run_aeson(&[
        r#"let obj = object ["x" .= (1 :: Int)]"#,
        r#"let modified = obj & key "x" . _Number .~ 999"#,
        r#"pure (modified ^? key "x" . _Number)"#,
    ]);
    assert_eq!(json, serde_json::json!(999.0));
}

/// Use (%~) to modify a field value
#[test]
#[ignore] // Requires Integer arithmetic (Scientific internals) — needs GMP FFI support
fn test_aeson_lens_modify_field() {
    let json = run_aeson(&[
        r#"let obj = object ["count" .= (10 :: Int)]"#,
        r#"let modified = obj & key "count" . _Number %~ (+ 5)"#,
        r#"pure (modified ^? key "count" . _Number)"#,
    ]);
    assert_eq!(json, serde_json::json!(15.0));
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
    assert_eq!(json, serde_json::json!(85.0));
}

/// Build nested JSON, modify inner field, extract result
#[test]
#[ignore] // Requires Integer arithmetic (Scientific internals) — needs GMP FFI support
fn test_aeson_multistage_nested_modify() {
    let json = run_aeson(&[
        r#"let config = object ["db" .= object ["port" .= (5432 :: Int), "host" .= ("localhost" :: Text)]]"#,
        r#"let updated = config & key "db" . key "port" . _Number .~ 3306"#,
        r#"pure (updated ^? key "db" . key "port" . _Number)"#,
    ]);
    assert_eq!(json, serde_json::json!(3306.0));
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
        r#"send (KvSet "data" "hello world")"#,
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
    assert_eq!(json, serde_json::json!(28.0));
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
    assert_eq!(json, serde_json::json!(8080.0));
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
    assert_eq!(json, serde_json::json!([10.0, 20.0, 30.0]));
}

/// Input injection with effect: extract from input, print, return
#[test]
fn test_aeson_input_with_effect() {
    let input_val = serde_json::json!({"greeting": "Hello from JSON!"});
    let imports: Vec<&str> = aeson_import_strs();
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines = vec![
        r#"case input ^? key "greeting" . _String of"#.to_string(),
        r#"  Just g  -> send (Print g)"#.to_string(),
        r#"  Nothing -> pure ()"#.to_string(),
        r#"pure (input ^? key "greeting" . _String)"#.to_string(),
    ];
    let imports_owned: Vec<String> = imports.iter().map(|s| s.to_string()).collect();
    let src = tidepool_mcp::template_haskell(
        &preamble, &stack, &lines, &imports_owned, &[], Some(&input_val),
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
    let json = run_mcp(&["pure (\"hello\" ++ \" world\")"]);
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
    let (json, _) = run_mcp_effectful(&["send (KvSet \"k\" \"v\")", "send (KvGet \"k\")"]);
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
    let (json, console) = run_mcp_effectful_with_helpers(
        &[
            "send (KvSet \"a\" \"data\")",
            "persist \"a\" \"out.txt\"",
            "pure \"done\"",
        ],
        &[r#"persist :: Text -> Text -> Eff '[Console, KV, Fs] ()
persist key filename = do
  val <- send (KvGet key)
  case val of
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
    let json = run_mcp(&["pure (map (\\c -> chr (ord c + 1)) \"Hello\")"]);
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
        r#"send (KvSet "hello" "world")"#,
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
        r#"send (KvSet "hello" "world")"#,
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
        r#"send (KvSet "hello" "world")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.take 3) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["hel"]));
}

/// Stripping a namespace prefix from KvKeys results is a common pattern.
/// Bug: T.drop on pure text returns "" (pre-existing measureOff/pure-text bug).
#[test]
#[ignore = "pre-existing: T.drop broken on pure text (not bridge-specific)"]
fn test_kv_keys_strip_namespace_prefix() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "ns:foo" "v1")"#,
        r#"send (KvSet "ns:bar" "v2")"#,
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
        r#"send (KvSet "hello" "world")"#,
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
        r#"send (KvSet "nonempty" "v")"#,
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
        r#"send (KvSet "hello" "world")"#,
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
        r#"send (KvSet "hello" "world")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (\k -> T.append k "!") keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["hello!"]));
}

/// T.splitOn on a Text key from KvKeys.
#[test]
#[ignore = "pre-existing: T.splitOn crashes on bridge text (ByteArray# layout mismatch)"]
fn test_kv_keys_text_split_on() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "a:b:c" "v")"#,
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
        r#"send (KvSet "hello" "world")"#,
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
        r#"send (KvSet "prefix:a" "v1")"#,
        r#"send (KvSet "other:b" "v2")"#,
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
        r#"send (KvSet "foo bar baz" "v")"#,
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
        r#"send (KvSet "hello" "world")"#,
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
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe (-999) T.length v)"#,
    ]);
    assert_eq!(json, serde_json::json!(5));
}

/// T.drop on a value retrieved via KvGet.
/// Pair with test_kv_keys_text_drop to scope the bug.
#[test]
fn test_kv_get_value_text_drop() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.drop 2) v)"#,
    ]);
    assert_eq!(json, serde_json::json!("llo"));
}

/// Roundtrip: set namespaced keys, get keys, filter by prefix, drop prefix, sort.
/// This is the canonical KV namespace pattern that currently breaks end-to-end.
#[test]
fn test_kv_keys_namespace_roundtrip() {
    let (json, _) = run_mcp_effectful(&[
        r#"mapM_ (\(k,v) -> send (KvSet k v)) [("cache:one","1"),("cache:two","2"),("cache:three","3")]"#,
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
        r#"send (KvSet "hello" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.length keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([5]));
}

/// T.length on empty bridge text.
#[test]
fn test_primop_measure_off_length_empty() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "" "x")"#,
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
        r#"send (KvSet "hello" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.take 3) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["hel"]));
}

/// T.take more than length — should return entire text.
#[test]
fn test_primop_measure_off_take_all() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hi" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.take 10) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["hi"]));
}

/// T.drop on bridge text.
#[test]
fn test_primop_measure_off_drop_ascii() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hello" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.drop 2) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["llo"]));
}

/// T.drop all — should return empty text.
#[test]
fn test_primop_measure_off_drop_all() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hi" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.drop 10) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([""]));
}

/// T.splitAt on bridge text — uses measureOff for both take and drop.
#[test]
fn test_primop_measure_off_split_at() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hello" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.splitAt 2) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([["he", "llo"]]));
}

/// T.length + T.take + T.drop consistency check on bridge text.
#[test]
fn test_primop_measure_off_consistency() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "abcde" "x")"#,
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
        r#"send (KvSet "hello" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.reverse keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["olleh"]));
}

/// T.reverse on single-char bridge text.
#[test]
fn test_primop_reverse_single() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "x" "v")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.reverse keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["x"]));
}

/// T.reverse involution: reverse(reverse(x)) == x.
#[test]
fn test_primop_reverse_involution() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "abcde" "x")"#,
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
        r#"send (KvSet "hello" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map toUpper keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["HELLO"]));
}

/// toLower on bridge text.
#[test]
fn test_primop_index_word8_to_lower() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "WORLD" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map toLower keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["world"]));
}

/// T.filter on bridge text — iterates bytes with indexWord8Array#.
#[test]
fn test_primop_index_word8_filter() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "a1b2c3" "x")"#,
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
        r#"send (KvSet "hello" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (== "hello") keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([true]));
}

/// Text compare on bridge text.
#[test]
fn test_primop_compare_ordering() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "banana" "x")"#,
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
        r#"send (KvSet "cherry" "1")"#,
        r#"send (KvSet "apple" "2")"#,
        r#"send (KvSet "banana" "3")"#,
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
        r#"send (KvSet "hello" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.find (== 'l')) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["l"]));
}

/// T.find not found.
#[test]
fn test_primop_memchr_find_missing() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hello" "x")"#,
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
        r#"send (KvSet "hello" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.take 3 . T.reverse) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["oll"]));
}

/// T.length after T.drop on bridge text.
#[test]
fn test_primop_composite_length_after_drop() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hello" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.length . T.drop 2) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([3]));
}

/// words on bridge text — exercises measureOff + cons cell construction.
#[test]
fn test_primop_composite_words() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hello world foo" "x")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (concatMap words keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["hello", "world", "foo"]));
}

/// Bridge text through KvGet (not just KvKeys).
#[test]
fn test_primop_kvget_measure_off() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello world")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (fmap (\t -> (T.length t, T.take 5 t, T.drop 6 t)) v)"#,
    ]);
    assert_eq!(json, serde_json::json!([11, "hello", "world"]));
}

/// Bridge text from KvGet through T.reverse.
#[test]
fn test_primop_kvget_reverse() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "abcde")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (fmap T.reverse v)"#,
    ]);
    assert_eq!(json, serde_json::json!("edcba"));
}

/// Bridge text from KvGet through toUpper.
#[test]
fn test_primop_kvget_to_upper() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (fmap toUpper v)"#,
    ]);
    assert_eq!(json, serde_json::json!("HELLO"));
}
