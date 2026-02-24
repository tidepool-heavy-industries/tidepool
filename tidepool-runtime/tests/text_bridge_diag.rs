/// Diagnostic tests for the Text bridge round-trip bug.
///
/// Bug: T.length returns -5 instead of 5 for "hello" when passed through the bridge.
/// Bisection results:
/// 1. T.length works fine for literals (control test).
/// 2. T.length returns -byte_length for any Text constructed at runtime (bridge or manual).
/// 3. T.unpack works fine on bridge-injected Text (bytes are correct).
/// 4. T.drop 1 on "hello" returns an empty string [0, 0] because it sees the negative length.
/// 5. Text fields (off, len) are correctly set to [0, 5] by the bridge.
/// 6. The issue seems to be that T.length (the function) is returning negate . len for runtime Text.

use frunk::HNil;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_runtime::{compile_and_run, compile_and_run_pure};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

fn test_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::console_decl(),
        tidepool_mcp::kv_decl(),
    ]
}

fn mcp_source_with_imports(lines: &[&str], imports: &[&str]) -> String {
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let imports: Vec<String> = imports.iter().map(|s| s.to_string()).collect();
    tidepool_mcp::template_haskell(&preamble, &stack, &lines, &imports, &[])
}

fn run_mcp_effectful(lines: &[&str]) -> serde_json::Value {
    let imports = [
        "Data.Text.Internal (Text(..))",
        "Data.Array.Byte (ByteArray(..))",
        "GHC.Exts"
    ];
    let src = mcp_source_with_imports(lines, &imports);
    let pp = prelude_path();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let (console, _) = TestConsole::new();
            let kv = TestKv::new();
            let mut handlers = frunk::hlist![console, kv];
            let val = compile_and_run(&src, "result", &include, &mut handlers, &())
                .expect("compile_and_run failed");
            val.to_json()
        })
        .unwrap()
        .join()
        .expect("thread panicked")
}

fn run_plain(body: &str) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, MagicHash, UnboxedTuples, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T
import Data.Text.Internal (Text(..))
import Data.Array.Byte (ByteArray(..))
import GHC.Exts
import Control.Monad.Freer

result :: _
result = {body}
"#
    );
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

#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

struct TestConsole;

impl TestConsole {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        (TestConsole, Arc::new(Mutex::new(Vec::new())))
    }
}

impl EffectHandler for TestConsole {
    type Request = ConsoleReq;
    fn handle(&mut self, req: ConsoleReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ConsoleReq::Print(_) => cx.respond(()),
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
            KvReq::Get(k) => cx.respond(self.store.get(&k).cloned()),
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

// ===========================================================================
// Diagnostic Tests
// ===========================================================================

/// 1. Pure Haskell T.length — literal. Should return 5. (Control test)
#[test]
fn test_diag_pure_length() {
    let json = run_plain(r#"T.length ("hello" :: T.Text)"#);
    assert_eq!(json, serde_json::json!(5));
}

/// 2. Bridge T.length via KvGet — reproduces the bug.
#[test]
fn test_diag_bridge_length() {
    let json = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe (-999) T.length v)"#,
    ]);
    // Bug: currently returns -5
    assert_eq!(json, serde_json::json!(5));
}

/// 3. Manual Text construction from literal fields.
#[test]
fn test_diag_manual_construction() {
    let json = run_plain(r#"
        let !(Text ba off len) = ("hello" :: T.Text) in
        let manual = Text ba off len in
        T.length manual
    "#);
    // Bug: currently returns -5
    assert_eq!(json, serde_json::json!(5));
}

/// 4. T.unpack on bridge text. Verifies bytes and fields are basically correct.
#[test]
fn test_diag_unpack() {
    let json = run_mcp_effectful(&[
        r#"send (KvSet "k" "abc")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure $ case v of
            Nothing -> []
            Just t -> map ord (T.unpack t)"#,
    ]);
    // 'a' = 97, 'b' = 98, 'c' = 99
    assert_eq!(json, serde_json::json!([97, 98, 99]));
}

/// 5. Comparing fields (off, len) of bridge text.
#[test]
fn test_diag_fields() {
    let json = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure $ case v of
            Nothing -> [-1, -1]
            Just (Text _ off len) -> [off, len]"#,
    ]);
    assert_eq!(json, serde_json::json!([0, 5]));
}

/// 6. T.length on bridge text with different lengths.
#[test]
fn test_diag_various_lengths() {
    let json = run_mcp_effectful(&[
        r#"send (KvSet "" "")"#,
        r#"send (KvSet "a" "a")"#,
        r#"send (KvSet "ab" "ab")"#,
        r#"send (KvSet "hello world" "hello world")"#,
        r#"v0 <- send (KvGet "")"#,
        r#"v1 <- send (KvGet "a")"#,
        r#"v2 <- send (KvGet "ab")"#,
        r#"v3 <- send (KvGet "hello world")"#,
        r#"pure [ maybe (-1) T.length v0
             , maybe (-1) T.length v1
             , maybe (-1) T.length v2
             , maybe (-1) T.length v3
             ]"#,
    ]);
    // Bug: returns [0, -1, -2, -11]
    assert_eq!(json, serde_json::json!([0, 1, 2, 11]));
}

/// 7. UTF-8 multibyte on bridge text.
#[test]
fn test_diag_utf8() {
    let json = run_mcp_effectful(&[
        r#"send (KvSet "k" "café")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe (-1) T.length v)"#,
    ]);
    // "café" is 4 characters, 5 bytes (é is C3 A9)
    // Bug: returns -5 (negate . byte_length)
    assert_eq!(json, serde_json::json!(4));
}

/// 8. T.null on bridge text.
#[test]
fn test_diag_null() {
    let json = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe True T.null v)"#,
    ]);
    assert_eq!(json, serde_json::json!(false));
}

/// 9. Runtime-constructed length via drop 0.
#[test]
fn test_diag_drop0_length() {
    let json = run_mcp_effectful(&[
        r#"let literal = "hello" :: T.Text"#,
        r#"pure (T.length (T.drop 0 literal))"#,
    ]);
    // Bug: returns -5
    assert_eq!(json, serde_json::json!(5));
}

/// 10. Fields of dropped text.
#[test]
fn test_diag_drop1_fields() {
    let json = run_mcp_effectful(&[
        r#"let literal = "hello" :: T.Text"#,
        r#"let dropped = T.drop 1 literal"#,
        r#"pure $ case dropped of Text _ o l -> [o, l]"#,
    ]);
    // Bug: returns [0, 0] because drop sees negative length and treats as "drop more than length"
    assert_eq!(json, serde_json::json!([1, 4]));
}