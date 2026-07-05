//! Reproduction test for SIGILL on showDouble through the full 10-effect dispatch path.
//!
//! The MCP server uses `Eff '[Console, KV, Fs, SG, Http, Exec, Meta, Git, Llm, Ask]`.
//! The bug only manifests through `compile_and_run` (effect dispatch loop), not
//! through `compile_and_run_pure` (direct heap read).
//!
//! The 10-effect GADT preamble and the ten mock handlers used to be re-declared
//! verbatim in every test in this file (~2300 lines). They now live once in
//! `tidepool_testing::eval_harness::mock` — `mock::mcp_module(body)` prepends the
//! canonical preamble and `mock::min_stack()` is the matching handler HList.

use serde_json::Value as Json;
use tidepool_testing::eval_harness::mock::{self, MockConsole, MockKv};
use tidepool_testing::eval_harness::EvalHarness;

/// Compile+run `body` (helper defs + `result`) against the canonical 10-effect
/// MCP stack and its mock handlers, returning the rendered JSON.
fn run10(body: &str) -> Json {
    EvalHarness::new()
        .with_stdlib()
        .run(&mock::mcp_module(body), "result", mock::min_stack())
        .json()
}

/// The recursive `valSize`/`arrSz`/`objSz` helper trio the paginator uses —
/// shared by the bisection tests below.
const VALSIZE: &str = r#"valSize :: Value -> Int
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
objSz ((k,v):rest) acc = objSz rest (acc + T.length (KM.toText k) + 4 + valSize v + 2)"#;

/// Minimal test: showDouble on non-constant through 10-effect dispatch.
/// The simplest reproduction of the MCP SIGILL bug.
#[test]
fn show_double_10_effects_minimal() {
    let json = run10(
        r#"result :: M Value
result = do
  let xs = [10 :: Int, 20, 30]
      n = length xs
      d = fromIntegral n :: Double
  pure (toJSON (pack (showDouble d)))"#,
    );
    eprintln!("Result: {json}");
    assert!(json.is_string(), "Expected string result, got: {json}");
}

/// Full MCP reproduction: 10-effect dispatch with KvSet + paginateResult + showDouble.
#[test]
fn show_double_10_effects_with_paginate() {
    let body = format!(
        r#"say :: Text -> M ()
say t = do
  send (Print t)
  v <- send (KvGet "__sayChars")
  let cur = case v of {{ Just b -> case b ^? _Int of {{ Just n -> n; _ -> 0 }}; Nothing -> 0 }}
  send (KvSet "__sayChars" (toJSON (cur + T.length t)))

showI :: Int -> Text
showI n = show n

{VALSIZE}

paginateResult :: Int -> Value -> M Value
paginateResult budget val
  | valSize val <= budget = pure val
  | otherwise = pure val

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of {{ Just b -> case b ^? _Int of {{ Just n -> n; _ -> 0 }}; Nothing -> 0 }}
  paginateResult (max 100 (4096 - _sayC)) (toJSON _r)"#
    );
    let json = run10(&body);
    eprintln!("Result: {json}");
    assert!(json.is_string(), "Expected string result, got: {json}");
}

/// Bisection: same code with only 2 effects (Console, KV).
/// If this passes but the 10-effect version fails, the bug is in union tag dispatch.
#[test]
fn show_double_2_effects_same_code() {
    let src = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
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

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Int of { Just n -> n; _ -> 0 }; Nothing -> 0 }
  pure (toJSON _r)
"#;
    let json = EvalHarness::new()
        .with_stdlib()
        .run(src, "result", frunk::hlist![MockConsole, MockKv::new()])
        .json();
    eprintln!("Result: {json}");
    assert!(json.is_string(), "Expected string result, got: {json}");
}

/// Bisection: 10 effects, KvSet+KvGet but NO paginateResult/valSize.
#[test]
fn show_double_10_effects_kvsetget_only() {
    let json = run10(
        r#"result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Int of { Just n -> n; _ -> 0 }; Nothing -> 0 }
  pure (toJSON _r)"#,
    );
    eprintln!("Result: {json}");
    assert!(json.is_string(), "Expected string result, got: {json}");
}

/// Bisection: 10 effects, KvSet+KvGet + valSize (arrSz/objSz style).
#[test]
fn show_double_10_effects_recursive_valsize() {
    let body = format!(
        r#"showI :: Int -> Text
showI n = show n

{VALSIZE}

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of {{ Just b -> case b ^? _Int of {{ Just n -> n; _ -> 0 }}; Nothing -> 0 }}
  let sz = valSize (toJSON _r)
  pure (toJSON sz)"#
    );
    let json = run10(&body);
    eprintln!("Result: {json}");
}

/// Full MCP reproduction: 10 effects + Library + full paginateResult + ask.
/// Uses the exact same Haskell source shape the MCP server generates.
#[test]
fn show_double_10_effects_full_mcp_with_library() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let user_lib = manifest.parent().unwrap().join(".tidepool").join("lib");
    if !user_lib.join("Library.hs").exists() {
        eprintln!("Skipping: .tidepool/lib/Library.hs not found");
        return;
    }

    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Tidepool.Aeson.KeyMap as KM
import qualified Data.List as L
import qualified Tidepool.TextFormat as TF
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
data SG a where
  SgFind :: Text -> Text -> Text -> [Text] -> SG [Value]
  SgPreview :: Text -> Text -> Text -> [Text] -> SG [Value]
  SgReplace :: Text -> Text -> Text -> [Text] -> SG Int
  SgRuleFind :: Text -> Value -> [Text] -> SG [Value]
  SgRuleReplace :: Text -> Value -> Text -> [Text] -> SG Int
data Http a where
  HttpGet :: Text -> Http Value
  HttpPost :: Text -> Value -> Http Value
  HttpRequest :: Text -> Text -> [(Text,Text)] -> Text -> Http Value
data Exec a where
  Run :: Text -> Exec (Int, Text, Text)
  RunIn :: Text -> Text -> Exec (Int, Text, Text)
  RunJson :: Text -> Exec Value
data Meta a where
  MetaConstructors :: Meta [(Text, Int)]
  MetaLookupCon :: Text -> Meta (Maybe (Int, Int))
  MetaPrimOps :: Meta [Text]
  MetaEffects :: Meta [Text]
  MetaDiagnostics :: Meta [Text]
  MetaVersion :: Meta Text
  MetaHelp :: Meta [Text]
data Git a where
  GitLog :: Text -> Int -> Git [Value]
  GitShow :: Text -> Git Value
  GitDiff :: Text -> Git [Value]
  GitBlame :: Text -> Int -> Int -> Git [Value]
  GitTree :: Text -> Text -> Git [Value]
  GitBranches :: Git [Value]
data Llm a where
  LlmChat :: Text -> Llm Text
  LlmStructured :: Text -> Value -> Llm Value
data Ask a where
  Ask :: Text -> Ask Value

type M = Eff '[Console, KV, Fs, SG, Http, Exec, Meta, Git, Llm, Ask]

say :: Text -> M ()
say t = do
  send (Print t)
  v <- send (KvGet "__sayChars")
  let cur = case v of { Just b -> case b ^? _Int of { Just n -> n; _ -> 0 }; Nothing -> 0 }
  send (KvSet "__sayChars" (toJSON (cur + T.length t)))

kvGet :: Text -> M (Maybe Value)
kvGet = send . KvGet
kvSet :: Text -> Value -> M ()
kvSet k v = send (KvSet k v)

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
truncArr :: Int -> Int -> [Value] -> ([Value], Int, [(Int, Value)])
truncArr _ nid [] = ([], nid, [])
truncArr bud nid (x:xs)
  | bud <= 30 = ([marker], nid + 1, [(nid, Array (x:xs))])
  | sz <= bud = let (r, nid', s) = truncArr (bud - sz - 2) nid xs in (x : r, nid', s)
  | otherwise = let m = String ("[~" <> showI sz <> " chars -> stub_" <> showI nid <> "]")
                    (r, nid', s) = truncArr (bud - 50) (nid + 1) xs
                in (m : r, nid', (nid, x) : s)
  where sz = valSize x
        n = 1 + length xs
        tsz = sz + arrSz xs 0
        marker = String ("[" <> showI n <> " more, ~" <> showI tsz <> " chars -> stub_" <> showI nid <> "]")
truncKvs :: Int -> Int -> [(Key, Value)] -> ([(Key, Value)], Int, [(Int, Value)])
truncKvs _ nid [] = ([], nid, [])
truncKvs bud nid ((k,v):rest)
  | bud <= 30 = ([(KM.fromText "...", String marker)], nid + 1, [(nid, object (map (\(k',v') -> KM.toText k' .= v') ((k,v):rest)))])
  | sz <= bud = let (r, nid', s) = truncKvs (bud - sz - 2) nid rest in ((k,v) : r, nid', s)
  | otherwise = let m = String ("[~" <> showI (valSize v) <> " chars -> stub_" <> showI nid <> "]")
                    (r, nid', s) = truncKvs (bud - 50) (nid + 1) rest
                in ((k, m) : r, nid', (nid, v) : s)
  where sz = T.length (KM.toText k) + 4 + valSize v
        n = 1 + length rest
        tsz = sz + objSz rest 0
        marker = "[" <> showI n <> " more fields, ~" <> showI tsz <> " chars -> stub_" <> showI nid <> "]"
truncGo :: Int -> Int -> Value -> (Value, Int, [(Int, Value)])
truncGo bud nid v
  | valSize v <= bud = (v, nid, [])
  | otherwise = case v of
      Array xs -> let (items, nid', stubs) = truncArr bud nid xs in (Array items, nid', stubs)
      Object m -> let (pairs, nid', stubs) = truncKvs bud nid (KM.toList m)
                  in (object (map (\(k',v') -> KM.toText k' .= v') pairs), nid', stubs)
      String t -> let keep = max 10 (bud - 30)
                  in (String (T.take keep t <> "...[" <> showI (T.length t) <> " chars]"), nid, [])
      _ -> (v, nid, [])
truncVal :: Int -> Value -> (Value, [(Int, Value)])
truncVal budget val = let (v, _, stubs) = truncGo budget 0 val in (v, stubs)
lookupStub :: Int -> [(Int, Value)] -> Maybe Value
lookupStub _ [] = Nothing
lookupStub sid ((k,v):rest) = if sid == k then Just v else lookupStub sid rest
paginateResult :: Int -> Value -> M Value
paginateResult budget val
  | valSize val <= budget = pure val
  | otherwise = do
      let (truncated, stubs) = truncVal budget val
      case stubs of
        [] -> pure truncated
        _ -> do
          let stubInfo = Array (map (\(sid, sv) -> object ["id" .= ("stub_" <> showI sid), "size" .= toJSON (valSize sv)]) stubs)
          resp <- send (Ask ("[Pagination] truncated: " <> show truncated <> " stubs: " <> show stubInfo))
          case resp ^? _String of
            Just s -> case parseIntM (T.drop 5 s) of
              Just sid -> case lookupStub sid stubs of
                Just subtree -> paginateResult budget subtree
                Nothing -> pure truncated
              Nothing -> pure truncated
            _ -> pure truncated

result :: M Value
result = do
  kvSet "__sayChars" (toJSON (0 :: Int))
  _r <- do
    let n = length [10 :: Int, 20, 30]
    pure (showDouble (fromIntegral n))
  _scV <- kvGet "__sayChars"
  let _sayC = case _scV of { Just b -> case b ^? _Int of { Just n -> n; _ -> 0 }; Nothing -> 0 }
  paginateResult (max 100 (4096 - _sayC)) (toJSON _r)
"#;

    let json = EvalHarness::new()
        .with_stdlib()
        .with_include(user_lib)
        .with_effects_module()
        .run(src, "result", mock::min_stack())
        .json();
    eprintln!("Result: {json}");
}

/// Bisection: exactly the paginate test but without `say` function.
#[test]
fn show_double_10_effects_paginate_no_say() {
    let body = format!(
        r#"showI :: Int -> Text
showI n = show n

{VALSIZE}

paginateResult :: Int -> Value -> M Value
paginateResult budget val
  | valSize val <= budget = pure val
  | otherwise = pure val

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of {{ Just b -> case b ^? _Int of {{ Just n -> n; _ -> 0 }}; Nothing -> 0 }}
  paginateResult (max 100 (4096 - _sayC)) (toJSON _r)"#
    );
    let json = run10(&body);
    eprintln!("Result: {json}");
    assert!(json.is_string(), "Expected string result, got: {json}");
}

/// Bisection: inline paginateResult — call valSize directly in do block.
#[test]
fn show_double_10_effects_inline_paginate() {
    let body = format!(
        r#"showI :: Int -> Text
showI n = show n

{VALSIZE}

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of {{ Just b -> case b ^? _Int of {{ Just n -> n; _ -> 0 }}; Nothing -> 0 }}
  let budget = max 100 (4096 - _sayC)
  let val = toJSON _r
  if valSize val <= budget
    then pure val
    else pure val"#
    );
    let json = run10(&body);
    eprintln!("Result: {json}");
}

/// Bisection: compute valSize but still return val (not sz).
#[test]
fn show_double_10_effects_valsize_return_val() {
    let body = format!(
        r#"{VALSIZE}

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of {{ Just b -> case b ^? _Int of {{ Just n -> n; _ -> 0 }}; Nothing -> 0 }}
  let val = toJSON _r
      _sz = valSize val
  pure val"#
    );
    let json = run10(&body);
    eprintln!("Result: {json}");
    assert!(json.is_string(), "Expected string, got: {json}");
}

/// Bisection: seq valSize then return val (force evaluation but no conditional).
#[test]
fn show_double_10_effects_seq_valsize() {
    let body = format!(
        r#"{VALSIZE}

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  let val = toJSON _r
      sz = valSize val
  if sz <= 4096
    then pure val
    else pure val"#
    );
    let json = run10(&body);
    eprintln!("Result: {json}");
    assert!(json.is_string(), "Expected string, got: {json}");
}

/// Bisection: full KvSet/KvGet + if valSize val <= budget (sz as let-binding).
#[test]
fn show_double_10_effects_full_with_let_sz() {
    let body = format!(
        r#"{VALSIZE}

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of {{ Just b -> case b ^? _Int of {{ Just n -> n; _ -> 0 }}; Nothing -> 0 }}
  let val = toJSON _r
      budget = max 100 (4096 - _sayC)
      sz = valSize val
  if sz <= budget
    then pure val
    else pure val"#
    );
    let json = run10(&body);
    eprintln!("Result: {json}");
    assert!(json.is_string(), "Expected string, got: {json}");
}

/// Bisection: KvSet + showDouble + valSize comparison + return val. No KvGet.
#[test]
fn show_double_10_effects_kvset_no_kvget() {
    let body = format!(
        r#"{VALSIZE}

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  let val = toJSON _r
      sz = valSize val
  if sz <= 4096
    then pure val
    else pure val"#
    );
    let json = run10(&body);
    eprintln!("Result: {json}");
    assert!(json.is_string(), "Expected string, got: {json}");
}

/// Bisection: KvSet + KvGet + case match on result, then JUST return val.
#[test]
fn show_double_10_effects_kvget_case_then_val() {
    let json = run10(
        r#"result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Int of { Just n -> n; _ -> 0 }; Nothing -> 0 }
  pure (toJSON _r)"#,
    );
    eprintln!("Result: {json}");
    assert!(json.is_string(), "Expected string, got: {json}");
}

/// Bisection: KvGet+case + valSize but NO conditional. Just use sz.
#[test]
fn show_double_10_effects_kvget_valsize_no_cond() {
    let body = format!(
        r#"{VALSIZE}

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of {{ Just b -> case b ^? _Int of {{ Just n -> n; _ -> 0 }}; Nothing -> 0 }}
  let val = toJSON _r
      sz = valSize val
      budget = max 100 (4096 - _sayC)
  pure (toJSON (sz + budget))"#
    );
    let json = run10(&body);
    eprintln!("Result: {json}");
}

/// Bisection: 10 effects, KvSet+KvGet + valSize (foldl' variant, simplified).
#[test]
fn show_double_10_effects_with_valsize() {
    let json = run10(
        r#"valSize :: Value -> Int
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
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Int of { Just n -> n; _ -> 0 }; Nothing -> 0 }
  let sz = valSize (toJSON _r)
  pure (toJSON sz)"#,
    );
    eprintln!("Result: {json}");
}

/// Test with runtime-computed Double (non-constant-foldable) through effect dispatch.
/// Uses `stake 3 [1..]` to prevent GHC constant folding.
#[test]
fn show_double_10_effects_infinite_list() {
    let json = run10(
        r#"result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = stake 3 [1 :: Int ..]
        s = foldl' (+) 0 xs
        d = fromIntegral s :: Double
    pure (pack (showDouble d))
  pure (toJSON _r)"#,
    );
    eprintln!("Result: {json}");
    assert!(json.is_string(), "Expected string result, got: {json}");
}
