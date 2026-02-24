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

macro_rules! text_test_case {
    (
        pure: $pure_name:ident,
        bridge: $bridge_name:ident,
        expr: $pure_expr:expr,
        lines: [$($bridge_line:expr),* $(,)?],
        expected: $expected:expr
        $(, pure_ignore: $pure_ignore:expr)?
        $(, bridge_ignore: $bridge_ignore:expr)?
    ) => {
        #[test]
        $(#[ignore = $pure_ignore])?
        fn $pure_name() {
            let json = run_plain($pure_expr);
            assert_eq!(json, $expected);
        }

        #[test]
        $(#[ignore = $bridge_ignore])?
        fn $bridge_name() {
            let (json, _) = run_mcp_effectful(&[
                $($bridge_line),*
            ]);
            assert_eq!(json, $expected);
        }
    };
}

// ===========================================================================
// Query operations
// ===========================================================================

text_test_case!(
    pure: test_text_length_pure,
    bridge: test_text_length_bridge,
    expr: r#"T.length ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe (-999) T.length v)"#,
    ],
    expected: serde_json::json!(5),
    bridge_ignore: "Known bridge bug: length returns -5 for 'hello'"
);

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

text_test_case!(
    pure: test_text_head_pure,
    bridge: test_text_head_bridge,
    expr: r#"T.head ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe ' ' T.head v)"#,
    ],
    expected: serde_json::json!("h")
);

text_test_case!(
    pure: test_text_last_pure,
    bridge: test_text_last_bridge,
    expr: r#"T.last ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe ' ' T.last v)"#,
    ],
    expected: serde_json::json!("o")
);

// ===========================================================================
// Slice operations
// ===========================================================================

text_test_case!(
    pure: test_text_take_pure,
    bridge: test_text_take_bridge,
    expr: r#"T.take 3 ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.take 3) v)"#,
    ],
    expected: serde_json::json!("hel"),
    pure_ignore: "Known bug: T.take pure literal returns full string",
    bridge_ignore: "Known bridge bug: take treats Text as empty or returns full string"
);

text_test_case!(
    pure: test_text_drop_pure,
    bridge: test_text_drop_bridge,
    expr: r#"T.drop 2 ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.drop 2) v)"#,
    ],
    expected: serde_json::json!("llo"),
    pure_ignore: "Known bug: T.drop pure literal returns empty string",
    bridge_ignore: "Known bridge bug: drop returns empty string for any positive n"
);

text_test_case!(
    pure: test_text_take_while_pure,
    bridge: test_text_take_while_bridge,
    expr: r#"T.takeWhile (/= 'l') ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.takeWhile (/= 'l')) v)"#,
    ],
    expected: serde_json::json!("he")
);

text_test_case!(
    pure: test_text_drop_while_pure,
    bridge: test_text_drop_while_bridge,
    expr: r#"T.dropWhile (/= 'l') ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.dropWhile (/= 'l')) v)"#,
    ],
    expected: serde_json::json!("llo")
);

text_test_case!(
    pure: test_text_split_at_pure,
    bridge: test_text_split_at_bridge,
    expr: r#"T.splitAt 3 ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe ("none", "none") (T.splitAt 3) v)"#,
    ],
    expected: serde_json::json!(["hel", "lo"]),
    pure_ignore: "Known bug: T.splitAt pure literal triggers NullPointer",
    bridge_ignore: "Known bridge bug: splitAt returns ([], full)"
);

// ===========================================================================
// Transform operations
// ===========================================================================

text_test_case!(
    pure: test_text_reverse_pure,
    bridge: test_text_reverse_bridge,
    expr: r#"T.reverse ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" T.reverse v)"#,
    ],
    expected: serde_json::json!("olleh"),
    pure_ignore: "CRASH: reverse on pure literal Text triggers SIGABRT",
    bridge_ignore: "CRASH: reverse on bridge Text triggers SIGABRT"
);

text_test_case!(
    pure: test_text_to_upper_pure,
    bridge: test_text_to_upper_bridge,
    expr: r#"T.toUpper ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" T.toUpper v)"#,
    ],
    expected: serde_json::json!("HELLO")
);

text_test_case!(
    pure: test_text_to_lower_pure,
    bridge: test_text_to_lower_bridge,
    expr: r#"T.toLower ("HELLO" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "HELLO")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" T.toLower v)"#,
    ],
    expected: serde_json::json!("hello")
);

text_test_case!(
    pure: test_text_map_pure,
    bridge: test_text_map_bridge,
    expr: r#"T.map (\c -> if c == 'h' then 'J' else c) ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.map (\c -> if c == 'h' then 'J' else c)) v)"#,
    ],
    expected: serde_json::json!("Jello")
);

text_test_case!(
    pure: test_text_filter_pure,
    bridge: test_text_filter_bridge,
    expr: r#"T.filter (/= 'l') ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.filter (/= 'l')) v)"#,
    ],
    expected: serde_json::json!("heo")
);

// ===========================================================================
// Search operations
// ===========================================================================

text_test_case!(
    pure: test_text_is_prefix_of_pure,
    bridge: test_text_is_prefix_of_bridge,
    expr: r#"T.isPrefixOf "he" ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe False (T.isPrefixOf "he") v)"#,
    ],
    expected: serde_json::json!(true)
);

text_test_case!(
    pure: test_text_is_suffix_of_pure,
    bridge: test_text_is_suffix_of_bridge,
    expr: r#"T.isSuffixOf "lo" ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe False (T.isSuffixOf "lo") v)"#,
    ],
    expected: serde_json::json!(true)
);

text_test_case!(
    pure: test_text_is_infix_of_pure,
    bridge: test_text_is_infix_of_bridge,
    expr: r#"T.isInfixOf "ell" ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe False (T.isInfixOf "ell") v)"#,
    ],
    expected: serde_json::json!(true)
);

text_test_case!(
    pure: test_text_find_pure,
    bridge: test_text_find_bridge,
    expr: r#"fromMaybe '?' (T.find (== 'e') ("hello" :: Text))"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe ' ' (\t -> fromMaybe '?' (T.find (== 'e') t)) v)"#,
    ],
    expected: serde_json::json!("e")
);

// ===========================================================================
// Split/join operations
// ===========================================================================

text_test_case!(
    pure: test_text_split_on_pure,
    bridge: test_text_split_on_bridge,
    expr: r#"T.splitOn ":" ("a:b:c" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "a:b:c")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe [] (T.splitOn ":") v)"#,
    ],
    expected: serde_json::json!(["a", "b", "c"]),
    bridge_ignore: "CRASH: splitOn on bridge Text triggers SIGABRT"
);

text_test_case!(
    pure: test_text_words_pure,
    bridge: test_text_words_bridge,
    expr: r#"T.words ("foo bar baz" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "foo bar baz")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe [] T.words v)"#,
    ],
    expected: serde_json::json!(["foo", "bar", "baz"])
);

text_test_case!(
    pure: test_text_lines_pure,
    bridge: test_text_lines_bridge,
    expr: r#"T.lines (T.intercalate (T.singleton '\n') ["line1", "line2"])"#,
    lines: [
        r#"let s = T.intercalate (T.singleton '\n') ["line1", "line2"]"#,
        r#"send (KvSet "k" s)"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe [] T.lines v)"#,
    ],
    expected: serde_json::json!(["line1", "line2"]),
    pure_ignore: "Known bug: T.lines returns [full] instead of [line1, line2]",
    bridge_ignore: "Known bug: T.lines returns [full] instead of [line1, line2]"
);

text_test_case!(
    pure: test_text_intercalate_pure,
    bridge: test_text_intercalate_bridge,
    expr: r#"T.intercalate ", " (["a", "b", "c"] :: [Text])"#,
    lines: [
        r#"send (KvSet "k1" "a")"#,
        r#"send (KvSet "k2" "b")"#,
        r#"send (KvSet "k3" "c")"#,
        r#"v1 <- send (KvGet "k1")"#,
        r#"v2 <- send (KvGet "k2")"#,
        r#"v3 <- send (KvGet "k3")"#,
        r#"let list = catMaybes [v1, v2, v3]"#,
        r#"pure (T.intercalate ", " list)"#,
    ],
    expected: serde_json::json!("a, b, c")
);

// ===========================================================================
// Combination operations
// ===========================================================================

text_test_case!(
    pure: test_text_append_pure,
    bridge: test_text_append_bridge,
    expr: r#"T.append "foo" "bar""#,
    lines: [
        r#"send (KvSet "k" "foo")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (\t -> T.append t "bar") v)"#,
    ],
    expected: serde_json::json!("foobar")
);

text_test_case!(
    pure: test_text_concat_pure,
    bridge: test_text_concat_bridge,
    expr: r#"T.concat (["foo", "bar", "baz"] :: [Text])"#,
    lines: [
        r#"send (KvSet "k1" "foo")"#,
        r#"send (KvSet "k2" "bar")"#,
        r#"send (KvSet "k3" "baz")"#,
        r#"v1 <- send (KvGet "k1")"#,
        r#"v2 <- send (KvGet "k2")"#,
        r#"v3 <- send (KvGet "k3")"#,
        r#"let list = catMaybes [v1, v2, v3]"#,
        r#"pure (T.concat list)"#,
    ],
    expected: serde_json::json!("foobarbaz")
);

text_test_case!(
    pure: test_text_cons_pure,
    bridge: test_text_cons_bridge,
    expr: r#"T.cons 'f' "oo""#,
    lines: [
        r#"send (KvSet "k" "oo")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (T.cons 'f') v)"#,
    ],
    expected: serde_json::json!("foo"),
    pure_ignore: "Known bug: T.cons pure triggers UnexpectedHeapTag(0)"
);

text_test_case!(
    pure: test_text_snoc_pure,
    bridge: test_text_snoc_bridge,
    expr: r#"T.snoc "fo" 'o'"#,
    lines: [
        r#"send (KvSet "k" "fo")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" (\t -> T.snoc t 'o') v)"#,
    ],
    expected: serde_json::json!("foo"),
    pure_ignore: "Known bug: T.snoc pure literal triggers UnexpectedHeapTag(0)"
);

// ===========================================================================
// Comparison
// ===========================================================================

text_test_case!(
    pure: test_text_eq_pure,
    bridge: test_text_eq_bridge,
    expr: r#"("hello" :: Text) == "hello""#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe False (== "hello") v)"#,
    ],
    expected: serde_json::json!(true)
);

text_test_case!(
    pure: test_text_compare_pure,
    bridge: test_text_compare_bridge,
    expr: r#"compare ("apple" :: Text) "banana""#,
    lines: [
        r#"send (KvSet "k" "apple")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "EQ" (\t -> show (compare t "banana")) v)"#,
    ],
    expected: serde_json::json!("LT")
);

// ===========================================================================
// Conversion
// ===========================================================================

text_test_case!(
    pure: test_text_show_pure,
    bridge: test_text_show_bridge,
    expr: r#"show ("hello" :: Text)"#,
    lines: [
        r#"send (KvSet "k" "hello")"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (maybe "none" show v)"#,
    ],
    expected: serde_json::json!("\"hello\"")
);

#[test]
fn test_text_singleton_pure() {
    let json = run_plain(r#"T.singleton 'a'"#);
    assert_eq!(json, serde_json::json!("a"));
}

#[test]
fn test_text_singleton_bridge_roundtrip() {
    let (json, _) = run_mcp_effectful(&[
        r#"let s = T.singleton 'a'"#,
        r#"send (KvSet "k" s)"#,
        r#"v <- send (KvGet "k")"#,
        r#"pure (fromMaybe "none" v)"#,
    ]);
    assert_eq!(json, serde_json::json!("a"));
}
