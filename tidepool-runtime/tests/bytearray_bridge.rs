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
// Helpers (copied from sort_crash.rs)
// ---------------------------------------------------------------------------

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

fn test_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::console_decl(),
        tidepool_mcp::kv_decl(),
        tidepool_mcp::fs_decl(),
    ]
}

fn mcp_source(lines: &[&str]) -> String {
    let decls = test_decls();
    let preamble = tidepool_mcp::build_preamble(&decls);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    tidepool_mcp::template_haskell(&preamble, &stack, &lines, &[], &[])
}

fn plain_source(body: &str) -> String {
    format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, MagicHash, UnboxedTuples, PartialTypeSignatures, BangPatterns #-}}
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

fn run_mcp_effectful(lines: &[&str]) -> (serde_json::Value, Vec<String>) {
    let src = mcp_source(lines);
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
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_sizeof_bytearray_pure() {
    let json = run_plain(r#"
        case newByteArray# 10# realWorld# of
            (# _, mba #) -> case unsafeFreezeByteArray# mba realWorld# of
                (# _, ba #) -> I# (sizeofByteArray# ba)
    "#);
    assert_eq!(json, serde_json::json!(10));
}

#[test]
fn test_sizeof_bytearray_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "key" "hello")"#,
        r#"mval <- send (KvGet "key")"#,
        r#"case mval of
            Nothing -> pure (-1)
            Just t -> pure (T.length t)"#
    ]);
    assert_eq!(json, serde_json::json!(5));
}

#[test]
fn test_index_word8_pure() {
    let json = run_plain(r#"
        case newByteArray# 5# realWorld# of
            (# _, mba #) -> case writeWord8Array# mba 0# (wordToWord8# 72##) realWorld# of
                _ -> case writeWord8Array# mba 1# (wordToWord8# 101##) realWorld# of
                    _ -> case writeWord8Array# mba 2# (wordToWord8# 108##) realWorld# of
                        _ -> case writeWord8Array# mba 3# (wordToWord8# 108##) realWorld# of
                            _ -> case writeWord8Array# mba 4# (wordToWord8# 111##) realWorld# of
                                _ -> case unsafeFreezeByteArray# mba realWorld# of
                                    (# _, ba #) -> I# (word2Int# (word8ToWord# (indexWord8Array# ba 1#)))
    "#);
    assert_eq!(json, serde_json::json!(101)); // 'e'
}

#[test]
fn test_index_word8_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "key" "hello")"#,
        r#"mval <- send (KvGet "key")"#,
        r#"case mval of
            Nothing -> pure (-1)
            Just t -> pure (ord (T.index t 1))"#
    ]);
    assert_eq!(json, serde_json::json!(101)); // 'e'
}

#[test]
fn test_compare_bytearrays_pure() {
    let json = run_plain(r#"
        let mkBA str = case newByteArray# 5# realWorld# of
                (# _, mba #) -> case writeWord8Array# mba 0# (wordToWord8# 72##) realWorld# of
                    _ -> case writeWord8Array# mba 1# (wordToWord8# 101##) realWorld# of
                        _ -> case writeWord8Array# mba 2# (wordToWord8# 108##) realWorld# of
                            _ -> case writeWord8Array# mba 3# (wordToWord8# 108##) realWorld# of
                                _ -> case writeWord8Array# mba 4# (wordToWord8# 111##) realWorld# of
                                    _ -> case unsafeFreezeByteArray# mba realWorld# of
                                        (# _, ba #) -> ba
        in case mkBA "hello" of
            ba1 -> case mkBA "hello" of
                ba2 -> I# (compareByteArrays# ba1 0# ba2 0# 5#)
    "#);
    assert_eq!(json, serde_json::json!(0));
}

#[test]
fn test_compare_bytearrays_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "key1" "hello")"#,
        r#"send (KvSet "key2" "hello")"#,
        r#"m1 <- send (KvGet "key1")"#,
        r#"m2 <- send (KvGet "key2")"#,
        r#"case (m1, m2) of
            (Just t1, Just t2) -> pure (t1 == t2)
            _ -> pure False"#
    ]);
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_copy_bytearray_pure() {
    let json = run_plain(r#"
        case newByteArray# 5# realWorld# of
            (# _, mba #) -> case writeWord8Array# mba 0# (wordToWord8# 72##) realWorld# of
                _ -> case writeWord8Array# mba 1# (wordToWord8# 101##) realWorld# of
                    _ -> case writeWord8Array# mba 2# (wordToWord8# 108##) realWorld# of
                        _ -> case writeWord8Array# mba 3# (wordToWord8# 108##) realWorld# of
                            _ -> case writeWord8Array# mba 4# (wordToWord8# 111##) realWorld# of
                                _ -> case unsafeFreezeByteArray# mba realWorld# of
                                    (# _, src #) -> case newByteArray# 5# realWorld# of
                                        (# _, mba' #) -> case copyByteArray# src 0# mba' 0# 5# realWorld# of
                                            _ -> case unsafeFreezeByteArray# mba' realWorld# of
                                                (# _, dest #) -> I# (word2Int# (word8ToWord# (indexWord8Array# dest 4#)))
    "#);
    assert_eq!(json, serde_json::json!(111)); // 'o'
}

#[test]
fn test_copy_bytearray_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "key" "hello")"#,
        r#"mval <- send (KvGet "key")"#,
        r#"case mval of
            Nothing -> pure "none"
            Just t -> pure (T.reverse t)"#
    ]);
    assert_eq!(json, serde_json::json!("olleh"));
}

#[test]
fn test_sizeof_bytearray_empty_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "key" "")"#,
        r#"mval <- send (KvGet "key")"#,
        r#"case mval of
            Nothing -> pure (-1)
            Just t -> pure (T.length t)"#
    ]);
    assert_eq!(json, serde_json::json!(0));
}

#[test]
fn test_sizeof_bytearray_single_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "key" "x")"#,
        r#"mval <- send (KvGet "key")"#,
        r#"case mval of
            Nothing -> pure (-1)
            Just t -> pure (T.length t)"#
    ]);
    assert_eq!(json, serde_json::json!(1));
}
