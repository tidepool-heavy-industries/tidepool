//! Tests for Data.Text splitting functions through the JIT.
//!
//! These exercise T.splitOn, T.words, T.lines, and T.split — the qualified
//! Data.Text versions, not the hand-rolled Prelude reimplementations.
//!
//! Tests use three preambles to bisect context-dependent failures:
//! - `run`: minimal (Prelude + qualified T)
//! - `run_freer`: Prelude + Freer + qualified T (no Library)
//! - `run_mcp`: full MCP preamble (Freer + Library + extra imports)

use std::path::Path;

use serde_json::json;

mod common;

fn user_lib_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".tidepool/lib")
        .leak()
}

fn run(body: &str) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
default (Int, Text)

result :: _
result = {body}
"#
    );
    let pp = common::prelude_path();
    let include = [pp.as_path()];
    let val = tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure failed");
    val.to_json()
}

/// Freer preamble: adds Control.Monad.Freer but NOT Library.
/// If this fails but `run` passes, Freer import changes GHC optimization.
fn run_freer(body: &str) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, GADTs, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import Control.Monad.Freer
default (Int, Text)

result :: _
result = {body}
"#
    );
    let pp = common::prelude_path();
    let include = [pp.as_path()];
    let val = tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure (freer) failed");
    val.to_json()
}

/// MCP-like preamble: includes Control.Monad.Freer, Library, effect GADTs.
/// Compiles as pure (no actual effect dispatch) but uses the same module
/// structure the MCP server generates.
fn run_mcp(body: &str) -> serde_json::Value {
    let ulp = user_lib_dir();
    if !ulp.join("Library.hs").exists() {
        panic!("Skipping: .tidepool/lib/Library.hs not found");
    }
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.List as L
import qualified Tidepool.Text as TT
import qualified Tidepool.Table as Tab
import Control.Monad.Freer hiding (run)
import Library
import qualified Prelude as P
default (Int, Text)
error :: Text -> a
error = P.error . T.unpack

result :: _
result = {body}
"#
    );
    let pp = common::prelude_path();
    let include = [pp.as_path(), ulp];
    let val = tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure (mcp) failed");
    val.to_json()
}

// ========== Minimal preamble (pure, no Freer) ==========

#[test]
fn t_spliton_comma() {
    assert_eq!(run(r#"T.splitOn "," "a,b,c""#), json!(["a", "b", "c"]));
}

#[test]
fn t_spliton_slash() {
    assert_eq!(run(r#"T.splitOn "/" "foo/bar/baz""#), json!(["foo", "bar", "baz"]));
}

#[test]
fn t_spliton_no_match() {
    assert_eq!(run(r#"T.splitOn "," "hello""#), json!(["hello"]));
}

#[test]
fn t_spliton_empty_parts() {
    assert_eq!(run(r#"T.splitOn "," ",a,,b,""#), json!(["", "a", "", "b", ""]));
}

#[test]
fn t_spliton_multi_char_sep() {
    assert_eq!(run(r#"T.splitOn "::" "a::b::c""#), json!(["a", "b", "c"]));
}

#[test]
fn t_words_simple() {
    assert_eq!(run(r#"T.words "hello world""#), json!(["hello", "world"]));
}

#[test]
fn t_words_multiple_spaces() {
    assert_eq!(run(r#"T.words "hello  world  foo""#), json!(["hello", "world", "foo"]));
}

#[test]
fn t_lines_simple() {
    assert_eq!(run(r#"T.lines "a\nb\nc""#), json!(["a", "b", "c"]));
}

#[test]
fn t_split_predicate() {
    assert_eq!(run(r#"T.split (== '/') "foo/bar/baz""#), json!(["foo", "bar", "baz"]));
}

// ========== Freer preamble (Freer, no Library) ==========

#[test]
fn freer_t_spliton_comma() {
    assert_eq!(run_freer(r#"T.splitOn "," "a,b,c""#), json!(["a", "b", "c"]));
}

#[test]
fn freer_t_words() {
    assert_eq!(run_freer(r#"T.words "hello world""#), json!(["hello", "world"]));
}

#[test]
fn freer_t_lines() {
    assert_eq!(run_freer(r#"T.lines "a\nb\nc""#), json!(["a", "b", "c"]));
}

// ========== Bisect: does adding T.lines-using helpers break T.splitOn? ==========

/// Same as run_mcp but adds helper functions that reference T.lines
/// (like searchFiles/lineCount in the real MCP preamble).
fn run_mcp_with_helpers(body: &str) -> serde_json::Value {
    let ulp = user_lib_dir();
    if !ulp.join("Library.hs").exists() {
        panic!("Skipping: .tidepool/lib/Library.hs not found");
    }
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.List as L
import qualified Tidepool.Text as TT
import qualified Tidepool.Table as Tab
import Control.Monad.Freer hiding (run)
import Library
import qualified Prelude as P
default (Int, Text)
error :: Text -> a
error = P.error . T.unpack

helperUsingTLines :: Text -> [Text]
helperUsingTLines = T.lines

helperUsingTWords :: Text -> [Text]
helperUsingTWords = T.words

result :: _
result = {body}
"#
    );
    let pp = common::prelude_path();
    let include = [pp.as_path(), ulp];
    let val = tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure (mcp+helpers) failed");
    val.to_json()
}

/// Same as run but wraps in Eff monad + toJSON like MCP does.
fn run_eff(body: &str) -> serde_json::Value {
    let ulp = user_lib_dir();
    if !ulp.join("Library.hs").exists() {
        panic!("Skipping: .tidepool/lib/Library.hs not found");
    }
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.List as L
import qualified Tidepool.Text as TT
import qualified Tidepool.Table as Tab
import Control.Monad.Freer hiding (run)
import Library
import qualified Prelude as P
default (Int, Text)
error :: Text -> a
error = P.error . T.unpack

result :: _
result = toJSON ({body})
"#
    );
    let pp = common::prelude_path();
    let include = [pp.as_path(), ulp];
    let val = tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure (eff) failed");
    val.to_json()
}

/// Closest to real MCP: effect GADTs + Eff monad wrapping + toJSON.
fn run_full_mcp(body: &str) -> serde_json::Value {
    let ulp = user_lib_dir();
    if !ulp.join("Library.hs").exists() {
        panic!("Skipping: .tidepool/lib/Library.hs not found");
    }
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.List as L
import qualified Tidepool.Text as TT
import qualified Tidepool.Table as Tab
import Control.Monad.Freer hiding (run)
import Library
import qualified Prelude as P
default (Int, Text)
error :: Text -> a
error = P.error . T.unpack

data Console a where
  Print :: Text -> Console ()

data KV a where
  KvGet :: Text -> KV (Maybe Value)
  KvSet :: Text -> Value -> KV ()
  KvDelete :: Text -> KV ()
  KvKeys :: KV [Text]

data Fs a where
  FsRead :: Text -> Fs Text
  FsWrite :: Text -> Text -> Fs ()
  FsListDir :: Text -> Fs [Text]
  FsGlob :: Text -> Fs [Text]
  FsExists :: Text -> Fs Bool
  FsMetadata :: Text -> Fs (Int, Bool, Bool)

data Ask a where
  Ask :: Text -> Ask Value

type M = Eff '[Console, KV, Fs, Ask]

say :: Text -> M ()
say = send . Print
kvGet :: Text -> M (Maybe Value)
kvGet = send . KvGet
kvSet :: Text -> Value -> M ()
kvSet k v = send (KvSet k v)
kvDel :: Text -> M ()
kvDel = send . KvDelete
kvKeys :: M [Text]
kvKeys = send KvKeys
fsRead :: Text -> M Text
fsRead = send . FsRead
fsGlob :: Text -> M [Text]
fsGlob = send . FsGlob
ask :: Text -> M Value
ask = send . Ask

searchFiles :: Text -> Text -> M [(Text, Int, Text)]
searchFiles pat needle = do
  files <- fsGlob pat
  fmap concat $ forM files $ \path -> do
    content <- fsRead path
    let ls = zip [(1::Int)..] (T.lines content)
    pure [(path, n, l) | (n, l) <- ls, T.isInfixOf needle l]

lineCount :: Text -> M Int
lineCount path = length . T.lines <$> fsRead path

result :: Eff '[Console, KV, Fs, Ask] Value
result = do
  _r <- do
    {body}
  pure (toJSON _r)
"#
    );
    let pp = common::prelude_path();
    let include = [pp.as_path(), ulp];
    let val = tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure (full_mcp) failed");
    val.to_json()
}

#[test]
fn helpers_t_spliton() {
    assert_eq!(run_mcp_with_helpers(r#"T.splitOn "," "a,b,c""#), json!(["a", "b", "c"]));
}

#[test]
fn eff_t_spliton() {
    assert_eq!(run_eff(r#"T.splitOn "," "a,b,c""#), json!(["a", "b", "c"]));
}

#[test]
fn full_mcp_t_spliton() {
    let result = run_full_mcp(r#"pure (T.splitOn "," "a,b,c")"#);
    // Result goes through toJSON which wraps in Value constructor
    // Accept either direct array or Value-wrapped
    if result == json!(["a", "b", "c"]) {
        return;
    }
    // Value wrapper: {"constructor": "Val", "fields": [["a","b","c"]]}
    if let Some(fields) = result.get("fields") {
        if let Some(arr) = fields.get(0) {
            assert_eq!(arr, &json!(["a", "b", "c"]));
            return;
        }
    }
    panic!("unexpected result: {:?}", result);
}

// ========== MCP-like preamble (Freer + Library + everything) ==========

#[test]
fn mcp_t_spliton_comma() {
    assert_eq!(run_mcp(r#"T.splitOn "," "a,b,c""#), json!(["a", "b", "c"]));
}

#[test]
fn mcp_t_spliton_slash() {
    assert_eq!(run_mcp(r#"T.splitOn "/" "foo/bar/baz""#), json!(["foo", "bar", "baz"]));
}

#[test]
fn mcp_t_words() {
    assert_eq!(run_mcp(r#"T.words "hello world""#), json!(["hello", "world"]));
}

#[test]
fn mcp_t_lines() {
    assert_eq!(run_mcp(r#"T.lines "a\nb\nc""#), json!(["a", "b", "c"]));
}

#[test]
fn mcp_t_split() {
    assert_eq!(run_mcp(r#"T.split (== '/') "foo/bar/baz""#), json!(["foo", "bar", "baz"]));
}

// ========== Alpha-rename collision tests ==========
// These tests exercise scenarios where multiple inlined unfoldings
// contain local binders with potentially colliding GHC Uniques.
// Without alpha-renaming in resolveExternals, these can produce
// wrong results or crash.

/// Two functions that both use T.splitOn internally — their inlined
/// unfoldings from Data.Text share local lambda binders.
#[test]
fn collision_dual_spliton() {
    let result = run_mcp(
        r#"let { a = T.splitOn "," "x,y" ; b = T.splitOn ":" "1:2:3" } in (a, b)"#,
    );
    // Tuples render as arrays
    assert_eq!(result, json!([["x", "y"], ["1", "2", "3"]]));
}

/// Mix splitOn + intercalate — both pull in Data.Text internals
/// with overlapping local binders.
#[test]
fn collision_spliton_intercalate() {
    assert_eq!(
        run_mcp(r#"T.intercalate "-" (T.splitOn "," "a,b,c")"#),
        json!("a-b-c")
    );
}

/// Multiple text operations in sequence that all inline Data.Text locals.
#[test]
fn collision_multi_text_ops() {
    let result = run_mcp(
        r#"let { ws = T.words "hello world" ; ls = T.lines "a\nb" ; sp = T.splitOn "," "x,y" } in (ws, ls, sp)"#,
    );
    assert_eq!(result, json!([["hello", "world"], ["a", "b"], ["x", "y"]]));
}

/// Full MCP preamble with effect GADTs — the most collision-prone context
/// because many effect wrappers (kvGet, kvSet, fsRead, etc.) all inline
/// small lambdas with local binders.
#[test]
fn collision_full_mcp_multi_ops() {
    let result = run_full_mcp(r#"do
        let a = T.splitOn "," "1,2,3"
            b = T.words "hello world"
            c = T.intercalate ";" a
        pure (a, b, c)"#);
    // Result goes through toJSON
    if let Some(fields) = result.get("fields") {
        if let Some(arr) = fields.get(0) {
            assert_eq!(arr, &json!([["1", "2", "3"], ["hello", "world"], "1;2;3"]));
            return;
        }
    }
    // Direct result (no Value wrapper)
    assert_eq!(result, json!([["1", "2", "3"], ["hello", "world"], "1;2;3"]));
}

// ========== Adversarial alpha-rename tests ==========
// Designed to maximize binder collision probability by combining many
// Data.Text operations that share internal helpers (splitOn, intercalate,
// words, lines, replace, strip all use similar fold/unfold machinery).

/// Six simultaneous splitOn calls with different separators.
/// Maximizes reuse of Data.Text.splitOn's internal lambda binders.
#[test]
fn adversarial_six_splitons() {
    let result = run_mcp(r#"let { a = T.splitOn "," "a,b"
         ; b = T.splitOn ":" "1:2"
         ; c = T.splitOn "/" "x/y"
         ; d = T.splitOn "." "p.q"
         ; e = T.splitOn "-" "m-n"
         ; f = T.splitOn ";" "j;k"
         } in [a, b, c, d, e, f]"#);
    assert_eq!(result, json!([
        ["a","b"], ["1","2"], ["x","y"],
        ["p","q"], ["m","n"], ["j","k"]
    ]));
}

/// map over a list applying splitOn — forces the same inlined unfolding
/// to be used in a higher-order context.
#[test]
fn adversarial_map_spliton() {
    assert_eq!(
        run_mcp(r#"map (T.splitOn ",") ["a,b", "c,d", "e,f"]"#),
        json!([["a","b"], ["c","d"], ["e","f"]])
    );
}

/// Chained text transforms: split then rejoin then split again.
/// Each operation pulls in overlapping Data.Text internals.
#[test]
fn adversarial_split_rejoin_split() {
    assert_eq!(
        run_mcp(r#"T.splitOn "-" (T.intercalate "-" (T.splitOn "," "a,b,c"))"#),
        json!(["a", "b", "c"])
    );
}

/// Cross-module collision: Data.Text + Data.Map + Data.List operations
/// all inlined together. Different modules may reuse unique namespaces.
#[test]
fn adversarial_cross_module() {
    let result = run_mcp(r#"let { ws = T.words "k1 k2 k3"
         ; m = Map.fromList (zip ws [1::Int, 2, 3])
         ; ks = sort (Map.keys m)
         ; vs = Map.elems m
         } in (ks, vs)"#);
    assert_eq!(result, json!([["k1","k2","k3"], [1,2,3]]));
}

/// Text replace + splitOn + intercalate — three operations that share
/// the same internal fold/unfold machinery in Data.Text.
#[test]
fn adversarial_replace_split_join() {
    assert_eq!(
        run_mcp(r#"T.intercalate ";" (T.splitOn "," (T.replace "." "," "a.b.c"))"#),
        json!("a;b;c")
    );
}

/// Full MCP preamble: the original killer combo. Effect helpers (kvAll,
/// kvClear use lambdas with send/kvKeys/mapM) combined with text ops.
/// This is the exact pattern that triggered the original unique collision.
#[test]
fn adversarial_full_mcp_effect_plus_text() {
    let result = run_full_mcp(r#"do
        let items = T.splitOn "," "x,y,z"
            joined = T.intercalate ":" items
            ws = T.words "a b c"
            upper = map T.toUpper ws
        pure (items, joined, upper)"#);
    if let Some(fields) = result.get("fields") {
        if let Some(arr) = fields.get(0) {
            assert_eq!(arr, &json!([["x","y","z"], "x:y:z", ["A","B","C"]]));
            return;
        }
    }
    assert_eq!(result, json!([["x","y","z"], "x:y:z", ["A","B","C"]]));
}

/// Nested let bindings where inner bindings shadow outer text operations.
/// Tests that alpha-rename handles nested scopes correctly.
#[test]
fn adversarial_nested_lets() {
    let result = run_mcp(r#"let { outer = T.splitOn "," "a,b,c" } in
        let { inner = T.splitOn ":" (T.intercalate ":" outer) } in
        let { final' = T.intercalate "-" inner } in final'"#);
    assert_eq!(result, json!("a-b-c"));
}

/// concatMap with text splitting — maximizes lambda binder reuse
/// across multiple invocations of the same inlined function.
#[test]
fn adversarial_concatmap_split() {
    assert_eq!(
        run_mcp(r#"concatMap (T.splitOn ",") ["a,b", "c,d"]"#),
        json!(["a", "b", "c", "d"])
    );
}

/// filter + text predicates. Multiple Data.Text predicate functions
/// (isPrefixOf, isSuffixOf, isInfixOf) all inlined simultaneously.
#[test]
fn adversarial_text_predicates() {
    let result = run_mcp(r#"let { xs = ["hello", "world", "help", "held"]
         ; pre = filter (T.isPrefixOf "hel") xs
         ; suf = filter (T.isSuffixOf "ld") xs
         ; inf = filter (T.isInfixOf "or") xs
         } in (pre, suf, inf)"#);
    assert_eq!(result, json!([
        ["hello", "help", "held"],
        ["world", "held"],
        ["world"]
    ]));
}

/// CSV parsing: split lines, split fields, build Map, query — exercises
/// Data.Text + Data.Map + list ops all inlined from different modules.
#[test]
fn adversarial_csv_parse() {
    let result = run_mcp(
        r#"let { rows = T.lines "name,age\nalice,30\nbob,25"
             ; hdr = T.splitOn "," (head rows)
             ; dat = map (T.splitOn ",") (tail rows)
             ; recs = map (\r -> Map.fromList (zip hdr r)) dat
             ; names = map (\m -> Map.findWithDefault "" "name" m) recs
             } in names"#,
    );
    assert_eq!(result, json!(["alice", "bob"]));
}

/// Recursive text processing with accumulator — forces splitOn's
/// inlined unfolding to be reused across recursive calls.
#[test]
fn adversarial_recursive_split() {
    let result = run_mcp(
        r#"let { go acc [] = acc ; go acc (x:xs) = go (acc ++ T.splitOn "," x) xs
             } in T.intercalate ":" (go [] ["a,b", "c,d", "e,f"])"#,
    );
    assert_eq!(result, json!("a:b:c:d:e:f"));
}

/// sortBy + groupBy + text ops — exercises cross-module binder overlap
/// between Data.Text, Data.List, and Data.Char.
#[test]
fn adversarial_group_by_first_char() {
    let result = run_mcp(
        r#"let { fruits = T.splitOn "," "banana,apple,avocado,blueberry,cherry,apricot"
             ; sorted = sort fruits
             ; grouped = groupBy (\a b -> T.head a == T.head b) sorted
             ; summary = map (\g -> (T.singleton (T.head (head g)), length g)) grouped
             } in summary"#,
    );
    assert_eq!(result, json!([["a", 3], ["b", 2], ["c", 1]]));
}

// ========== Known bugs (not alpha-rename related) ==========

/// zipWith with a lambda that calls T.intercalate on (a ++ b).
/// Pre-existing JIT bug: silently drops intercalate on 2+ element lists.
#[test]
fn zipwith_intercalate_two_lists() {
    let result = run_mcp(
        r#"let { f :: [Text] -> [Text] -> Text ; f a b = T.intercalate "/" (a ++ b)
             } in zipWith f [["a","b"], ["c","d"]] [["1","2"], ["3","4"]]"#,
    );
    assert_eq!(result, json!(["a/b/1/2", "c/d/3/4"]));
}
