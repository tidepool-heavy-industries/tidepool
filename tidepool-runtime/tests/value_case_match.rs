//! Red/green tests for Value constructor case-matching through the JIT.
//!
//! The Aeson Value type has 6 constructors: Object, Array, String, Number, Bool, Null.
//! When Haskell code case-matches on a Value, the JIT must dispatch to the correct
//! alternative based on the constructor tag stored in the heap object. If the tags
//! used by Con emission don't match the tags expected by case alternatives, we get
//! SIGILL (exhausted case branch → Cranelift trap).

mod common;

use tidepool_repr::Literal;
use tidepool_runtime::{compile_and_run_pure, Value};

fn prelude_path() -> std::path::PathBuf {
    common::prelude_path()
}

/// Compile Haskell source and run a target binding through the JIT.
fn run(src: &str, target: &str) -> Value {
    let pp = prelude_path();
    let src = src.to_owned();
    let target = target.to_owned();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            compile_and_run_pure(&src, &target, &include)
                .expect("compile_and_run_pure failed")
                .into_value()
        })
        .unwrap()
        .join()
        .unwrap()
}

/// Extract an Int from either a raw LitInt or a boxed I# constructor.
fn expect_int(val: &Value) -> i64 {
    match val {
        Value::Lit(Literal::LitInt(n)) => *n,
        Value::Con(_, fields) if !fields.is_empty() => {
            if let Value::Lit(Literal::LitInt(n)) = &fields[0] {
                *n
            } else {
                panic!("expected boxed int, got: {:?}", val)
            }
        }
        other => panic!("expected int, got: {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Test 1: Case match on each Value constructor individually
// ---------------------------------------------------------------------------

#[test]
fn case_match_value_null() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = case Null of
  Object _ -> 1
  Array _  -> 2
  String _ -> 3
  Number _ -> 4
  Bool _   -> 5
  Null     -> 6
"#;
    let val = run(src, "result");
    assert_eq!(expect_int(&val), 6, "Null should match the Null branch");
}

#[test]
fn case_match_value_bool() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = case Bool True of
  Object _ -> 1
  Array _  -> 2
  String _ -> 3
  Number _ -> 4
  Bool _   -> 5
  Null     -> 6
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        5,
        "Bool True should match the Bool branch"
    );
}

#[test]
fn case_match_value_number() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = case Number 3.14 of
  Object _ -> 1
  Array _  -> 2
  String _ -> 3
  Number _ -> 4
  Bool _   -> 5
  Null     -> 6
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        4,
        "Number 3.14 should match the Number branch"
    );
}

#[test]
fn case_match_value_string() {
    let src = r#"
{-# LANGUAGE OverloadedStrings #-}
module Test where
import Tidepool.Aeson.Value

result :: Int
result = case String "hello" of
  Object _ -> 1
  Array _  -> 2
  String _ -> 3
  Number _ -> 4
  Bool _   -> 5
  Null     -> 6
"#;
    let val = run(src, "result");
    assert_eq!(expect_int(&val), 3, "String should match the String branch");
}

#[test]
fn case_match_value_array() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = case Array [] of
  Object _ -> 1
  Array _  -> 2
  String _ -> 3
  Number _ -> 4
  Bool _   -> 5
  Null     -> 6
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        2,
        "Array [] should match the Array branch"
    );
}

#[test]
fn case_match_value_object() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = case emptyObject of
  Object _ -> 1
  Array _  -> 2
  String _ -> 3
  Number _ -> 4
  Bool _   -> 5
  Null     -> 6
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        1,
        "emptyObject should match the Object branch"
    );
}

// ---------------------------------------------------------------------------
// Test 2: valSize-like pattern (the exact pattern from the MCP preamble)
// ---------------------------------------------------------------------------

#[test]
fn valsize_pattern_on_string() {
    let src = r#"
{-# LANGUAGE OverloadedStrings #-}
module Test where
import Tidepool.Aeson.Value
import qualified Data.Text as T

valSize :: Value -> Int
valSize v = case v of
  String t -> T.length t + 2
  Number _ -> 8
  Bool b   -> if b then 4 else 5
  Null     -> 4
  Array _  -> 99
  Object _ -> 99

result :: Int
result = valSize (String "hello")
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        7,
        "valSize (String \"hello\") = 5 + 2 = 7"
    );
}

#[test]
fn valsize_pattern_on_array() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

valSize :: Value -> Int
valSize v = case v of
  String _ -> 10
  Number _ -> 8
  Bool _   -> 5
  Null     -> 4
  Array _  -> 99
  Object _ -> 100

result :: Int
result = valSize (Array [Null, Null])
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        99,
        "valSize (Array [...]) should match Array branch"
    );
}

#[test]
fn valsize_pattern_on_object() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = case emptyObject of
  String _ -> 10
  Number _ -> 8
  Bool _   -> 5
  Null     -> 4
  Array _  -> 99
  Object _ -> 100
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        100,
        "valSize emptyObject should match Object branch"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Case match with field extraction (not just tag dispatch)
// ---------------------------------------------------------------------------

#[test]
fn case_extract_array_length() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = case Array [Null, Null, Null] of
  Array xs -> length xs
  _        -> 0
"#;
    let val = run(src, "result");
    assert_eq!(expect_int(&val), 3, "Should extract array and get length 3");
}

#[test]
fn case_extract_string_value() {
    let src = r#"
{-# LANGUAGE OverloadedStrings #-}
module Test where
import Tidepool.Aeson.Value
import qualified Data.Text as T

result :: Int
result = case String "abc" of
  String t -> T.length t
  _        -> 0
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        3,
        "Should extract string and get length 3"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Default branch (wildcard) matching
// ---------------------------------------------------------------------------

#[test]
fn case_default_branch() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = case Number 1.0 of
  Array _  -> 1
  Object _ -> 2
  _        -> 99
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        99,
        "Number should fall through to default branch"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Nested Value case matching
// ---------------------------------------------------------------------------

#[test]
fn nested_value_case() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

classify :: Value -> Int
classify v = case v of
  Array xs -> case xs of
    []    -> 10
    (x:_) -> case x of
      Null     -> 11
      Number _ -> 12
      _        -> 13
  Object _ -> 20
  _        -> 30

result :: Int
result = classify (Array [Number 42.0])
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        12,
        "Nested case: Array [Number _] should give 12"
    );
}

// ---------------------------------------------------------------------------
// Test 6: toJSON + case match (construct via typeclass, then dispatch)
// ---------------------------------------------------------------------------

#[test]
fn tojson_then_case_match() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = case toJSON (42 :: Int) of
  Number _ -> 1
  _        -> 0
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        1,
        "toJSON 42 should produce Number, matching Number branch"
    );
}

#[test]
fn tojson_list_then_case_match() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = case toJSON [1 :: Int, 2, 3] of
  Array xs -> length xs
  _        -> 0
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        3,
        "toJSON [1,2,3] should produce Array with 3 elements"
    );
}

// ---------------------------------------------------------------------------
// Test 7: dataToTag# behavior (via derived Ord on Value)
//
// Value derives Ord. GHC's derived Ord may use dataToTag# internally.
// If dataToTag# returns DataConId hashes instead of 0-based constructor
// indices, the ordering will be wrong (hash order ≠ declaration order).
// ---------------------------------------------------------------------------

#[test]
fn value_ord_instance() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

-- Value constructors in declaration order: Object, Array, String, Number, Bool, Null
-- With derived Ord, Object < Array < String < Number < Bool < Null

result :: Int
result = case compare (Array []) Null of
  LT -> 1
  EQ -> 2
  GT -> 3
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        1,
        "Array < Null in derived Ord (declaration order)"
    );
}

#[test]
fn value_eq_same_constructor() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = if Null == Null then 1 else 0
"#;
    let val = run(src, "result");
    assert_eq!(expect_int(&val), 1, "Null == Null should be True");
}

#[test]
fn value_eq_different_constructor() {
    let src = r#"
module Test where
import Tidepool.Aeson.Value

result :: Int
result = if Array [] == Null then 1 else 0
"#;
    let val = run(src, "result");
    assert_eq!(expect_int(&val), 0, "Array [] /= Null");
}

// ---------------------------------------------------------------------------
// Test 8: Prelude import path (the preamble uses Tidepool.Prelude, not
// Tidepool.Aeson.Value directly — uses NoImplicitPrelude to avoid ambiguity)
// ---------------------------------------------------------------------------

#[test]
fn prelude_value_case_match() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)

result :: Int
result = case toJSON [1 :: Int, 2, 3] of
  Array _  -> 1
  Object _ -> 2
  _        -> 0
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        1,
        "toJSON [1,2,3] via Prelude should match Array"
    );
}

// ---------------------------------------------------------------------------
// Test 9: show on Value (derived Show, may trigger case-matching internally)
// ---------------------------------------------------------------------------

#[test]
fn show_value_null() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T

result :: Int
result = T.length (pack (show Null))
"#;
    let val = run(src, "result");
    // show Null should produce "Null" (4 chars)
    assert_eq!(
        expect_int(&val),
        4,
        "show Null should produce \"Null\" (4 chars)"
    );
}

#[test]
fn show_value_object() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)
import Tidepool.Aeson.Value (emptyObject)
import qualified Data.Text as T

result :: Int
result = T.length (pack (show emptyObject))
"#;
    let val = run(src, "result");
    // show (Object (fromList [])) produces something non-empty
    assert!(
        expect_int(&val) > 0,
        "show emptyObject should produce non-empty string"
    );
}

// ---------------------------------------------------------------------------
// Test 10: object construction + case match (the exact pattern in paginateResult)
// ---------------------------------------------------------------------------

#[test]
fn object_construction_then_case() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude hiding (error)

valSize :: Value -> Int
valSize v = case v of
  String t -> len t + 2
  Number _ -> 8
  Bool b   -> if b then 4 else 5
  Null     -> 4
  Array _  -> 99
  Object _ -> 100

result :: Int
result = valSize (object ["name" .= (42 :: Int)])
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        100,
        "object [...] should match Object branch in valSize"
    );
}

// ---------------------------------------------------------------------------
// Test 11: Map + Value interaction (toList on Map Key Value → case match)
// ---------------------------------------------------------------------------

#[test]
fn map_value_case_match() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Map.Strict as Map

classify :: Value -> Int
classify v = case v of
  Object m -> Map.size m
  Array xs -> length xs
  _        -> 0

result :: Int
result = classify (object ["a" .= (1 :: Int), "b" .= (2 :: Int)])
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        2,
        "Object with 2 keys should return Map.size = 2"
    );
}

// ---------------------------------------------------------------------------
// Test 12: The exact truncGo/valSize pattern from the MCP preamble
// This is the closest reproduction of the actual failing code.
// ---------------------------------------------------------------------------

#[test]
fn preamble_valsize_truncgo_pattern() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Tidepool.Aeson.KeyMap as KM

valSize :: Value -> Int
valSize v = case v of
  String t -> T.length t + 2
  Number _ -> 8
  Bool b   -> if b then 4 else 5
  Null     -> 4
  Array xs -> arrSz xs 2
  Object m -> objSz (KM.toList m) 2

arrSz :: [Value] -> Int -> Int
arrSz [] acc = acc
arrSz [x] acc = acc + valSize x
arrSz (x:xs) acc = arrSz xs (acc + valSize x + 2)

objSz :: [(Key, Value)] -> Int -> Int
objSz [] acc = acc
objSz [(k,v)] acc = acc + T.length (KM.toText k) + 4 + valSize v
objSz ((k,v):rest) acc = objSz rest (acc + T.length (KM.toText k) + 4 + valSize v + 2)

truncGo :: Int -> Int -> Value -> (Value, Int, [(Int, Value)])
truncGo bud nid v
  | valSize v <= bud = (v, nid, [])
  | otherwise = case v of
      Array xs -> (Array xs, nid, [])
      Object _ -> (v, nid, [])
      String t -> (String (T.take 10 t), nid, [])
      _ -> (v, nid, [])

result :: Int
result =
  let v = object ["name" .= ("Alice" :: Text), "age" .= (30 :: Int)]
      (_, nid, _) = truncGo 1000 0 v
  in nid
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        0,
        "truncGo with large budget should return nid=0 (no truncation)"
    );
}

// ---------------------------------------------------------------------------
// Test 13: Value sort/nub (triggers Ord instance which may use dataToTag#)
// ---------------------------------------------------------------------------

#[test]
fn value_sort() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)

result :: Int
result = len (sort [Null, Bool True, Number 1.0, Bool False, Null])
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        5,
        "sort on [Value] should preserve length"
    );
}

#[test]
fn value_nub() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)

result :: Int
result = len (nub [Null, Null, Bool True, Bool True, Number 1.0])
"#;
    let val = run(src, "result");
    assert_eq!(
        expect_int(&val),
        3,
        "nub should remove duplicate Nulls and Bools"
    );
}

// ===========================================================================
// Tests for showDouble on non-constant-foldable Doubles (SIGILL regression)
// ===========================================================================

#[test]
fn show_double_constant() {
    // Baseline: showDouble on a literal (GHC constant-folds this)
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T

result :: Int
result = T.length (pack (showDouble 3.14))
"#;
    let val = run(src, "result");
    assert!(
        expect_int(&val) > 0,
        "showDouble 3.14 should produce non-empty string"
    );
}

#[test]
fn show_double_non_constant_length() {
    // showDouble on fromIntegral (length xs) — non-constant-foldable
    // Note: with a literal list [10,20,30], GHC constant-folds length.
    // Use a list constructed to prevent folding.
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T

result :: Int
result =
  let xs = stake 3 [1 :: Int ..]
      n = length xs
      d = fromIntegral n :: Double
  in T.length (pack (showDouble d))
"#;
    let val = run(src, "result");
    assert!(
        expect_int(&val) > 0,
        "showDouble on non-constant Double should work"
    );
}

#[test]
fn show_double_non_constant_recursive() {
    // showDouble on result of a recursive function
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T

go :: Int -> Int
go 0 = 0
go n = go (n - 1)

result :: Int
result =
  let d = fromIntegral (go 3) :: Double
  in T.length (pack (showDouble d))
"#;
    let val = run(src, "result");
    assert!(
        expect_int(&val) > 0,
        "showDouble on recursive result should work"
    );
}

#[test]
fn show_double_via_show_class() {
    // show (Double) via the Show typeclass — calls $fShowDouble_$cshow
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T

go :: Int -> Int
go 0 = 0
go n = go (n - 1)

result :: Int
result =
  let d = fromIntegral (go 3) :: Double
  in T.length (show d)
"#;
    let val = run(src, "result");
    assert!(
        expect_int(&val) > 0,
        "show on non-constant Double should work"
    );
}

#[test]
fn show_value_number_non_constant() {
    // show (Number d) where d is non-constant — the paginateResult crash path
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T

go :: Int -> Int
go 0 = 0
go n = go (n - 1)

result :: Int
result =
  let d = fromIntegral (go 3) :: Double
      v = Number d
  in T.length (show v)
"#;
    let val = run(src, "result");
    assert!(
        expect_int(&val) > 0,
        "show (Number non-constant) should work"
    );
}

#[test]
fn show_double_from_infinite_list() {
    // showDouble on a value derived from an infinite list
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T

result :: Int
result =
  let xs = stake 3 [1 :: Int ..]
      s = foldl' (+) 0 xs
      d = fromIntegral s :: Double
  in T.length (pack (showDouble d))
"#;
    let val = run(src, "result");
    assert!(
        expect_int(&val) > 0,
        "showDouble on infinite list sum should work"
    );
}

#[test]
fn show_value_number_from_tojson_int() {
    // Reproduces the paginateResult crash: show on Number where the Double
    // comes from toJSON (someInt) — i.e., Number (fromIntegral n) where n
    // is computed at runtime.
    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Tidepool.Aeson.KeyMap as KM

valSize :: Value -> Int
valSize v = case v of
  String t -> T.length t + 2
  Number _ -> 8
  Bool b -> if b then 4 else 5
  Null -> 4
  Array xs -> 2 + foldl' (\a x -> a + valSize x + 2) 0 xs
  Object m -> 2 + foldl' (\a (k,v') -> a + T.length (KM.toText k) + 4 + valSize v' + 2) 0 (KM.toList m)

result :: Int
result =
  let val = object ["name" .= ("Alice" :: Text), "age" .= (30 :: Int)]
      stubs = [(0 :: Int, val)]
      stubInfo = Array (map (\(sid, sv) -> object ["id" .= ("stub_" <> show sid), "size" .= toJSON (valSize sv)]) stubs)
  in T.length (show stubInfo)
"#;
    let val = run(src, "result");
    assert!(
        expect_int(&val) > 0,
        "show stubInfo with Number from valSize should work"
    );
}

#[test]
fn show_number_from_runtime_int() {
    // Minimal: show (Number (fromIntegral n)) where n is not constant-foldable
    let src = r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T

go :: Int -> Int
go 0 = 0
go n = 1 + go (n - 1)

result :: Int
result =
  let n = go 5
      v = Number (fromIntegral n)
  in T.length (show v)
"#;
    let val = run(src, "result");
    assert!(
        expect_int(&val) > 0,
        "show (Number (fromIntegral (go 5))) should work"
    );
}

#[test]
fn show_double_mcp_preamble_context() {
    // Reproduce the FULL MCP context: Eff stack return type means GHC compiles
    // differently (continuations, effect dispatch). The result is Eff-wrapped
    // but we use run_pure which will see the Leaf/Node continuation tree.
    //
    // The key insight: when result :: Eff '[...] Value, the showDouble call
    // is inside a continuation closure, and GHC may optimize differently.
    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Tidepool.Aeson.KeyMap as KM
import qualified Data.List as L
import qualified Tidepool.Text as TT
import qualified Tidepool.Table as Tab
import Control.Monad.Freer hiding (run)
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

data SG a where
  SgFind :: Text -> Text -> [Text] -> SG [Value]

data Http a where
  HttpGet :: Text -> Http Value

data Exec a where
  Run :: Text -> Exec (Int, Text, Text)

data Meta a where
  MetaVersion :: Meta Text

data Git a where
  GitLog :: Text -> Int -> Git [Value]

data Llm a where
  LlmChat :: Text -> Llm Text

data Ask a where
  Ask :: Text -> Ask Value

type M = Eff '[Console, KV, Fs, SG, Http, Exec, Meta, Git, Llm, Ask]

showI :: Int -> Text
showI n = show n

valSize :: Value -> Int
valSize v = case v of
  String t -> T.length t + 2
  Number _ -> 8
  Bool b -> if b then 4 else 5
  Null -> 4
  Array xs -> arrSz xs 2
  Object m -> objSz (KM.toList m) 2

arrSz :: [Value] -> Int -> Int
arrSz [] acc = acc
arrSz [x] acc = acc + valSize x
arrSz (x:xs) acc = arrSz xs (acc + valSize x + 2)

objSz :: [(Key, Value)] -> Int -> Int
objSz [] acc = acc
objSz [(k,v)] acc = acc + T.length (KM.toText k) + 4 + valSize v
objSz ((k,v):rest) acc = objSz rest (acc + T.length (KM.toText k) + 4 + valSize v + 2)

-- Eff-wrapped result: the showDouble call is inside the Eff continuation tree.
-- This matches the MCP paginateResult path.
result :: Text
result =
  let val = object ["name" .= ("Alice" :: Text), "age" .= (30 :: Int)]
      stubs = [(0 :: Int, val)]
      stubInfo = Array (map (\(sid, sv) -> object ["id" .= ("stub_" <> showI sid), "size" .= toJSON (valSize sv)]) stubs)
  in show stubInfo
"#;
    let val = run(src, "result");
    match &val {
        Value::Con(_, fields) => {
            assert!(
                !fields.is_empty(),
                "show stubInfo should produce non-empty Text"
            );
        }
        _ => {}
    }
}

/// Test showDouble in an effectful context using compile_and_run with actual
/// effect handlers — this is the MCP execution path.
#[test]
fn show_double_effectful_paginate() {
    use tidepool_bridge_derive::FromCore;
    use tidepool_effect::{EffectContext, EffectError, EffectHandler};
    use tidepool_runtime::compile_and_run;

    #[derive(FromCore)]
    enum ConsoleReq {
        #[core(name = "Print")]
        Print(String),
    }

    struct MockConsole;
    impl EffectHandler for MockConsole {
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
        KvGet(String),
        #[core(name = "KvSet")]
        KvSet(String, Value),
        #[core(name = "KvDelete")]
        KvDelete(String),
        #[core(name = "KvKeys")]
        KvKeys,
    }

    struct MockKv;
    impl EffectHandler for MockKv {
        type Request = KvReq;
        fn handle(&mut self, req: KvReq, cx: &EffectContext) -> Result<Value, EffectError> {
            match req {
                KvReq::KvGet(_) => {
                    // Return Nothing (tag 0 with no fields)
                    Ok(Value::Con(tidepool_repr::DataConId(0), vec![]))
                }
                KvReq::KvSet(_, _) => cx.respond(()),
                KvReq::KvDelete(_) => cx.respond(()),
                KvReq::KvKeys => {
                    // Return empty list
                    Ok(Value::Con(tidepool_repr::DataConId(0), vec![]))
                }
            }
        }
    }

    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Tidepool.Aeson.KeyMap as KM
import Control.Monad.Freer hiding (run)
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

type M = Eff '[Console, KV]

showI :: Int -> Text
showI n = show n

valSize :: Value -> Int
valSize v = case v of
  String t -> T.length t + 2
  Number _ -> 8
  Bool b -> if b then 4 else 5
  Null -> 4
  Array xs -> 2 + foldl' (\a x -> a + valSize x + 2) 0 xs
  Object m -> 2 + foldl' (\a (k,v') -> a + T.length (KM.toText k) + 4 + valSize v' + 2) 0 (KM.toList m)

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = stake 3 [1 :: Int ..]
        s = foldl' (+) 0 xs
        d = fromIntegral s :: Double
    pure (pack (showDouble d))
  let sz = valSize (toJSON _r)
  pure (toJSON sz)
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![MockConsole, MockKv];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("compile_and_run with effects failed")
        })
        .unwrap()
        .join()
        .unwrap();
    // Just verify it doesn't crash
    let _ = result.to_json();
}

/// EXACT MCP reproduction: full preamble with all 10 effect types, Library,
/// pagination helpers, and the showDouble-triggering user code.
/// Source is loaded from the filesystem to match exactly what tidepool-extract sees.
#[test]
fn show_double_exact_mcp_repro() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repro_src = manifest
        .parent()
        .unwrap()
        .join("tidepool-runtime/tests/fixtures/mcp_showdouble_repro.hs");
    if !repro_src.exists() {
        eprintln!("Skipping: fixtures/mcp_showdouble_repro.hs not found");
        return;
    }
    let src = std::fs::read_to_string(&repro_src).unwrap();
    let pp = prelude_path();
    let user_lib = manifest.parent().unwrap().join(".tidepool").join("lib");
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let mut includes: Vec<&std::path::Path> = vec![pp.as_path()];
            if user_lib.exists() {
                includes.push(user_lib.as_path());
            }
            compile_and_run_pure(&src, "result", &includes)
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(
        result.is_ok(),
        "MCP repro should not crash: {:?}",
        result.err()
    );
}

/// Test showDouble in an Eff-wrapped result binding with Library import.
/// This matches what MCP does: module Expr with all effect types, Library,
/// and the paginateResult wrapper calling show on Value containing Number.
#[test]
fn show_double_in_eff_with_library() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Tidepool.Aeson.KeyMap as KM
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

data Ask a where
  Ask :: Text -> Ask Value

type M = Eff '[Console, KV, Ask]

showI :: Int -> Text
showI n = show n

valSize :: Value -> Int
valSize v = case v of
  String t -> T.length t + 2
  Number _ -> 8
  Bool b -> if b then 4 else 5
  Null -> 4
  Array xs -> 2 + foldl' (\a x -> a + valSize x + 2) 0 xs
  Object m -> 2 + foldl' (\a (k,v') -> a + T.length (KM.toText k) + 4 + valSize v' + 2) 0 (KM.toList m)

paginateResult :: Int -> Value -> M Value
paginateResult budget val
  | valSize val <= budget = pure val
  | otherwise = do
      let stubs = [(0 :: Int, val)]
          stubInfo = Array (map (\(sid, sv) -> object ["id" .= ("stub_" <> showI sid), "size" .= toJSON (valSize sv)]) stubs)
      resp <- send (Ask ("truncated: " <> show val <> " stubs: " <> show stubInfo))
      pure (case resp ^? _String of { Just _ -> val; _ -> val })

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = stake 3 [1 :: Int ..]
        s = foldl' (+) 0 xs
        d = fromIntegral s :: Double
    pure (pack (showDouble d))
  paginateResult 4096 (toJSON _r)
"#;
    let pp = prelude_path();
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let user_lib = manifest.parent().unwrap().join(".tidepool").join("lib");
    if !user_lib.join("Library.hs").exists() {
        eprintln!("Skipping: .tidepool/lib/Library.hs not found");
        return;
    }
    let src_owned = src.to_owned();
    let pp2 = pp.clone();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include: Vec<&std::path::Path> = vec![pp2.as_path(), user_lib.as_path()];
            compile_and_run_pure(&src_owned, "result", &include)
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(
        result.is_ok(),
        "Eff+Library showDouble should not crash: {:?}",
        result.err()
    );
}
