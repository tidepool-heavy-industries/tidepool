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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

// ===========================================================================
// Query operations
// ===========================================================================

#[test]
fn test_text_length_pure() {
    let json = run_plain(r#"T.length ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!(5));
}

#[test]
fn test_text_length_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe (-999) T.length v)"#,
    ]);
    assert_eq!(json, serde_json::json!(5));
}

#[test]
fn test_text_null_pure() {
    let json = run_plain(r#"T.null ("" :: Text)"#);
    assert_eq!(json, serde_json::json!(true));
    let json = run_plain(r#"T.null ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!(false));
}

#[test]
fn test_text_null_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k1" "")"#,
        r#"send (KvSet "k2" "hello")"#,
        r#"v1 <- send (KvGet "k1")"#,
        r#"v2 <- send (KvGet "k2")"#,
        r#"pure (maybe False T.null v1, maybe True T.null v2)"#,
    ]);
    assert_eq!(json, serde_json::json!([true, false]));
}

#[test]
fn test_text_head_pure() {
    let json = run_plain(r#"T.head ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!('h'));
}

#[test]
fn test_text_head_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe ' ' T.head v)"#,
    ]);
    assert_eq!(json, serde_json::json!('h'));
}

#[test]
fn test_text_last_pure() {
    let json = run_plain(r#"T.last ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!('o'));
}

#[test]
fn test_text_last_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe ' ' T.last v)"#,
    ]);
    assert_eq!(json, serde_json::json!('o'));
}

// ===========================================================================
// Slice operations
// ===========================================================================

#[test]
fn test_text_take_pure() {
    let json = run_plain(r#"T.take 3 ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!("hel"));
}

#[test]
fn test_text_take_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.take 3) v)"#,
    ]);
    assert_eq!(json, serde_json::json!("hel"));
}

#[test]
fn test_text_drop_pure() {
    let json = run_plain(r#"T.drop 2 ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!("llo"));
}

#[test]
fn test_text_drop_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.drop 2) v)"#,
    ]);
    assert_eq!(json, serde_json::json!("llo"));
}

#[test]
fn test_text_take_while_pure() {
    let json = run_plain(r#"T.takeWhile (/= 'l') ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!("he"));
}

#[test]
fn test_text_take_while_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.takeWhile (/= 'l')) v)"#,
    ]);
    assert_eq!(json, serde_json::json!("he"));
}

#[test]
fn test_text_drop_while_pure() {
    let json = run_plain(r#"T.dropWhile (/= 'l') ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!("llo"));
}

#[test]
fn test_text_drop_while_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.dropWhile (/= 'l')) v)"#,
    ]);
    assert_eq!(json, serde_json::json!("llo"));
}

#[test]
fn test_text_split_at_pure() {
    let json = run_plain(r#"T.splitAt 3 ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!(["hel", "lo"]));
}

#[test]
fn test_text_split_at_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe ("none", "none") (T.splitAt 3) v)"#,
    ]);
    assert_eq!(json, serde_json::json!(["hel", "lo"]));
}

// ===========================================================================
// Transform operations
// ===========================================================================

#[test]
fn test_text_reverse_pure() {
    let json = run_plain(r#"T.reverse ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!("olleh"));
}

#[test]
fn test_text_reverse_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" T.reverse v)"#,
    ]);
    assert_eq!(json, serde_json::json!("olleh"));
}

#[test]
fn test_text_to_upper_pure() {
    let json = run_plain(r#"T.toUpper ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!("HELLO"));
}

#[test]
fn test_text_to_upper_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" T.toUpper v)"#,
    ]);
    assert_eq!(json, serde_json::json!("HELLO"));
}

#[test]
fn test_text_to_lower_pure() {
    let json = run_plain(r#"T.toLower ("HELLO" :: Text)"#);
    assert_eq!(json, serde_json::json!("hello"));
}

#[test]
fn test_text_to_lower_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "HELLO")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" T.toLower v)"#,
    ]);
    assert_eq!(json, serde_json::json!("hello"));
}

#[test]
fn test_text_map_pure() {
    let json = run_plain(r#"T.map (\c -> if c == 'h' then 'J' else c) ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!("Jello"));
}

#[test]
fn test_text_map_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.map (\c -> if c == 'h' then 'J' else c)) v)"#,
    ]);
    assert_eq!(json, serde_json::json!("Jello"));
}

#[test]
fn test_text_filter_pure() {
    let json = run_plain(r#"T.filter (/= 'l') ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!("heo"));
}

#[test]
fn test_text_filter_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.filter (/= 'l')) v)"#,
    ]);
    assert_eq!(json, serde_json::json!("heo"));
}

// ===========================================================================
// Search operations
// ===========================================================================

#[test]
fn test_text_is_prefix_of_pure() {
    let json = run_plain(r#"T.isPrefixOf "he" ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_text_is_prefix_of_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe False (T.isPrefixOf "he") v)"#,
    ]);
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_text_is_suffix_of_pure() {
    let json = run_plain(r#"T.isSuffixOf "lo" ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_text_is_suffix_of_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe False (T.isSuffixOf "lo") v)"#,
    ]);
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_text_is_infix_of_pure() {
    let json = run_plain(r#"T.isInfixOf "ell" ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_text_is_infix_of_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe False (T.isInfixOf "ell") v)"#,
    ]);
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_text_find_pure() {
    let json = run_plain(r#"T.find (== 'e') ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!('e'));
}

#[test]
fn test_text_find_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe ' ' (\t -> fromMaybe '?' (T.find (== 'e') t)) v)"#,
    ]);
    assert_eq!(json, serde_json::json!('e'));
}

// ===========================================================================
// Split/join operations
// ===========================================================================

#[test]
fn test_text_split_on_pure() {
    let json = run_plain(r#"T.splitOn ":" ("a:b:c" :: Text)"#);
    assert_eq!(json, serde_json::json!(["a", "b", "c"]));
}

#[test]
fn test_text_split_on_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "a:b:c")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe [] (T.splitOn ":") v)"#,
    ]);
    assert_eq!(json, serde_json::json!(["a", "b", "c"]));
}

#[test]
fn test_text_words_pure() {
    let json = run_plain(r#"T.words ("foo bar baz" :: Text)"#);
    assert_eq!(json, serde_json::json!(["foo", "bar", "baz"]));
}

#[test]
fn test_text_words_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "foo bar baz")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe [] T.words v)"#,
    ]);
    assert_eq!(json, serde_json::json!(["foo", "bar", "baz"]));
}

#[test]
fn test_text_lines_pure() {
    let json = run_plain(r#"T.lines (T.intercalate (T.singleton '\n') ["line1", "line2"])"#);
    assert_eq!(json, serde_json::json!(["line1", "line2"]));
}

#[test]
fn test_text_lines_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"let s = T.intercalate (T.singleton '\n') ["line1", "line2"]"#,
        r#"send (KvSet "k" s)"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe [] T.lines v)"#,
    ]);
    assert_eq!(json, serde_json::json!(["line1", "line2"]));
}

#[test]
fn test_text_intercalate_pure() {
    let json = run_plain(r#"T.intercalate ", " (["a", "b", "c"] :: [Text])"#);
    assert_eq!(json, serde_json::json!("a, b, c"));
}

#[test]
fn test_text_intercalate_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k1" "a")"#,
        r#"send (KvSet "k2" "b")"#,
        r#"v1 <- send (KvGet "k1")"#,
        r#"v2 <- send (KvGet "k2")"#,
        r#"let list = catMaybes [v1, v2]"#,
        r#"pure (T.intercalate ", " list)"#,
    ]);
    assert_eq!(json, serde_json::json!("a, b"));
}

// ===========================================================================
// Combination operations
// ===========================================================================

#[test]
fn test_text_append_pure() {
    let json = run_plain(r#"T.append "foo" "bar""#);
    assert_eq!(json, serde_json::json!("foobar"));
}

#[test]
fn test_text_append_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "foo")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (\t -> T.append t "bar") v)"#,
    ]);
    assert_eq!(json, serde_json::json!("foobar"));
}

#[test]
fn test_text_concat_pure() {
    let json = run_plain(r#"T.concat (["foo", "bar", "baz"] :: [Text])"#);
    assert_eq!(json, serde_json::json!("foobarbaz"));
}

#[test]
fn test_text_concat_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k1" "foo")"#,
        r#"send (KvSet "k2" "bar")"#,
        r#"v1 <- send (KvGet "k1")"#,
        r#"v2 <- send (KvGet "k2")"#,
        r#"let list = catMaybes [v1, v2]"#,
        r#"pure (T.concat list)"#,
    ]);
    assert_eq!(json, serde_json::json!("foobar"));
}

#[test]
fn test_text_cons_pure() {
    let json = run_plain(r#"T.cons 'f' "oo""#);
    assert_eq!(json, serde_json::json!("foo"));
}

#[test]
fn test_text_cons_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "oo")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.cons 'f') v)"#,
    ]);
    assert_eq!(json, serde_json::json!("foo"));
}

#[test]
fn test_text_snoc_pure() {
    let json = run_plain(r#"T.snoc "fo" 'o'"#);
    assert_eq!(json, serde_json::json!("foo"));
}

#[test]
fn test_text_snoc_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "fo")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (\t -> T.snoc t 'o') v)"#,
    ]);
    assert_eq!(json, serde_json::json!("foo"));
}

// ===========================================================================
// Comparison
// ===========================================================================

#[test]
fn test_text_eq_pure() {
    let json = run_plain(r#"("hello" :: Text) == "hello""#);
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_text_eq_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe False (== "hello") v)"#,
    ]);
    assert_eq!(json, serde_json::json!(true));
}

#[test]
fn test_text_compare_pure() {
    let json = run_plain(r#"compare ("apple" :: Text) "banana""#);
    assert_eq!(json, serde_json::json!("LT"));
}

#[test]
fn test_text_compare_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "apple")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "EQ" (\t -> show (compare t "banana")) v)"#,
    ]);
    assert_eq!(json, serde_json::json!("LT"));
}

// ===========================================================================
// Conversion
// ===========================================================================

#[test]
fn test_text_show_pure() {
    let json = run_plain(r#"show ("hello" :: Text)"#);
    assert_eq!(json, serde_json::json!("\"hello\""));
}

#[test]
fn test_text_show_bridge() {
    let (json, _) = run_mcp_effectful(&[
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" show v)"#,
    ]);
    assert_eq!(json, serde_json::json!("\"hello\""));
}

#[test]
fn test_text_singleton_pure() {
    let json = run_plain(r#"T.singleton 'a'"#);
    assert_eq!(json, serde_json::json!("a"));
}

#[test]
fn test_text_singleton_bridge() {
    // T.singleton doesn't take a bridge Text, but it returns a Text.
    // It's included in the list to test.
    let json = run_plain(r#"T.singleton 'a'"#);
    assert_eq!(json, serde_json::json!("a"));
}
