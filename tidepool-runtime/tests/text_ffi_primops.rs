use frunk::HNil;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_runtime::{compile_and_run, compile_and_run_pure};
use tidepool_codegen::host_fns;
use tidepool_mcp;

// ---------------------------------------------------------------------------
// Helpers (copied from sort_crash.rs)
// ---------------------------------------------------------------------------

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

/// Effect decls for tests — Console + KV + Fs
fn test_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::console_decl(),
        tidepool_mcp::kv_decl(),
        tidepool_mcp::fs_decl(),
    ]
}

fn mcp_source_with_helpers(lines: &[&str], helpers: &[&str]) -> String {
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let helpers: Vec<String> = helpers.iter().map(|s| s.to_string()).collect();
    tidepool_mcp::template_haskell(&preamble, &stack, &lines, &[], &helpers)
}

fn plain_source(body: &str) -> String {
    format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, MagicHash, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T
import GHC.Exts
import Control.Monad.Freer

result :: _
result = {body}
"#
    )
}

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

// ---------------------------------------------------------------------------
// Effect handlers for testing
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Section 1: Direct Rust Unit Tests
// ---------------------------------------------------------------------------

#[test]
fn test_measure_off_positive_ascii() {
    let data = b"hello";
    let addr = data.as_ptr() as i64;
    let result = host_fns::runtime_text_measure_off(addr, 0, 5);
    assert_eq!(result, 5); // 5 ASCII chars = 5 bytes
}

#[test]
fn test_measure_off_positive_utf8() {
    let data = "café".as_bytes();
    let addr = data.as_ptr() as i64;
    let result = host_fns::runtime_text_measure_off(addr, 0, 4); // 4 chars
    assert_eq!(result, 5); // 'é' is 2 bytes
}

#[test]
fn test_measure_off_negative_ascii() {
    // text-2 calls with negative cnt to count chars in a byte range.
    // Our current implementation in host_fns.rs returns 0 for negative cnt.
    // This documents the discrepancy with text-2's _hs_text_measure_off.
    let data = b"hello";
    let addr = data.as_ptr() as i64;
    let result = host_fns::runtime_text_measure_off(addr, 0, -5);
    // text-2 expects: -5 (negative of byte count traversed backwards)
    assert_eq!(result, -5, "negative cnt should count backwards like text-2's C impl");
}

#[test]
fn test_measure_off_zero() {
    let data = b"hello";
    let addr = data.as_ptr() as i64;
    let result = host_fns::runtime_text_measure_off(addr, 0, 0);
    assert_eq!(result, 0);
}

// ---------------------------------------------------------------------------
// Section 2: Integration tests through JIT
// ---------------------------------------------------------------------------

#[test]
fn test_jit_text_length_pure() {
    let res = run_plain(r#"T.length "hello""#);
    assert_eq!(res, serde_json::json!(5));
}

#[test]
fn test_jit_text_length_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hello" "val")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.length keys)"#,
    ]);
    // Bug: bridge-injected Text (from KvKeys) often returns negative or wrong length.
    assert_eq!(json, serde_json::json!([5]));
}

#[test]
fn test_jit_text_take_pure() {
    let res = run_plain(r#"T.take 2 "hello""#);
    assert_eq!(res, serde_json::json!("he"));
}

#[test]
fn test_jit_text_take_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hello" "val")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.take 2) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["he"]));
}

#[test]
fn test_jit_text_drop_pure() {
    let res = run_plain(r#"T.drop 3 "hello""#);
    assert_eq!(res, serde_json::json!("lo"));
}

#[test]
fn test_jit_text_drop_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hello" "val")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.drop 3) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["lo"]));
}

#[test]
fn test_jit_text_reverse_pure() {
    let res = run_plain(r#"T.reverse "hello""#);
    assert_eq!(res, serde_json::json!("olleh"));
}

#[test]
fn test_jit_text_reverse_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hello" "val")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map T.reverse keys)"#,
    ]);
    assert_eq!(json, serde_json::json!(["olleh"]));
}

#[test]
fn test_jit_text_split_at_pure() {
    let res = run_plain(r#"let (a, b) = T.splitAt 3 "hello" in (a, b)"#);
    assert_eq!(res, serde_json::json!(["hel", "lo"]));
}

#[test]
fn test_jit_text_split_at_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "hello" "val")"#,
        r#"keys <- send KvKeys"#,
        r#"pure (map (T.splitAt 3) keys)"#,
    ]);
    assert_eq!(json, serde_json::json!([["hel", "lo"]]));
}

#[test]
fn test_jit_text_find_pure() {
    let res = run_plain(r#"T.find (== 'e') "hello""#);
    assert_eq!(res, serde_json::json!("e"));
}

// ---------------------------------------------------------------------------
// Section 3: Test unbox_addr behavior
// ---------------------------------------------------------------------------

#[test]
fn test_unbox_addr_bytearray_bug() {
    // Tests whether unbox_addr correctly handles ByteArray# by adding +8 offset.
    // If it doesn't, indexWord8OffAddr# will read from the length header.
    let helpers = &[
        r#"{-# LANGUAGE MagicHash, UnboxedTuples #-}"#,
        r#"import GHC.Exts"#,
        r#"import GHC.Word"#,
        r#"import Data.Text.Internal (Text(..))"#,
        r#"checkBA :: Text -> Int"#,
        r#"checkBA (Text ba (I# off) (I# len)) = "#,
        r#"  let "#,
        r#"    w1 = fromIntegral (W8# (indexWord8Array# ba off)) "#,
        r#"    w2 = fromIntegral (W8# (indexWord8OffAddr# (unsafeCoerce# ba) off)) "#,
        r#"  in if w1 == (w2 :: Int) then 1 else 0"#,
    ];
    
    let (json, _) = run_mcp_effectful_with_helpers(
        &[
            r#"send (KvSet "abc" "val")"#,
            r#"keys <- send KvKeys"#,
            r#"pure (map checkBA keys)"#,
        ],
        helpers
    );
    // If the bug is present, indexWord8OffAddr# reads from the header, returning a mismatch.
    assert_eq!(json, serde_json::json!([1]));
}
