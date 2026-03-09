//! Reproduction test for SIGILL on showDouble through the full 10-effect dispatch path.
//!
//! The MCP server uses `Eff '[Console, KV, Fs, SG, Http, Exec, Meta, Git, Llm, Ask]`.
//! The bug only manifests through `compile_and_run` (effect dispatch loop), not
//! through `compile_and_run_pure` (direct heap read).

mod common;

use tidepool_bridge_derive::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::value::Value;
use tidepool_repr::DataConId;
use tidepool_runtime::compile_and_run;

fn prelude_path() -> std::path::PathBuf {
    common::prelude_path()
}

// ---------------------------------------------------------------------------
// Mock effect handlers — one per MCP effect type
// ---------------------------------------------------------------------------

// 0: Console
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
            ConsoleReq::Print(msg) => {
                eprintln!("[Console] Print: {}", msg);
                cx.respond(())
            }
        }
    }
}

// 1: KV
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

use std::collections::HashMap;

/// Mock KV store that stores serde_json::Value (like the real MCP).
/// The real MCP handler converts Value -> serde_json on Set,
/// and returns Option<serde_json::Value> on Get (which goes through
/// ToCore for serde_json::Value).
struct MockKv {
    store: HashMap<String, serde_json::Value>,
}
impl MockKv {
    fn new() -> Self {
        Self {
            store: HashMap::new(),
        }
    }
}
impl EffectHandler for MockKv {
    type Request = KvReq;
    fn handle(&mut self, req: KvReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            KvReq::KvGet(key) => {
                // Real MCP stores serde_json::Value, returns Option<serde_json::Value>
                let val: Option<serde_json::Value> = self.store.get(&key).cloned();
                cx.respond(val)
            }
            KvReq::KvSet(key, val) => {
                // Convert internal Value to serde_json before storing
                let json_val = tidepool_runtime::value_to_json(&val, cx.table(), 0);
                self.store.insert(key, json_val);
                cx.respond(())
            }
            KvReq::KvDelete(key) => {
                self.store.remove(&key);
                cx.respond(())
            }
            KvReq::KvKeys => {
                let keys: Vec<String> = self.store.keys().cloned().collect();
                cx.respond(keys)
            }
        }
    }
}

// 2: Fs (stub — returns empty/defaults)
#[derive(FromCore)]
enum FsReq {
    #[core(name = "FsRead")]
    FsRead(String),
    #[core(name = "FsWrite")]
    FsWrite(String, String),
    #[core(name = "FsListDir")]
    FsListDir(String),
    #[core(name = "FsGlob")]
    FsGlob(String),
    #[core(name = "FsExists")]
    FsExists(String),
    #[core(name = "FsMetadata")]
    FsMetadata(String),
}
struct MockFs;
impl EffectHandler for MockFs {
    type Request = FsReq;
    fn handle(&mut self, req: FsReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            FsReq::FsRead(_) => cx.respond(String::new()),
            FsReq::FsWrite(_, _) => cx.respond(()),
            FsReq::FsListDir(_) | FsReq::FsGlob(_) => {
                let empty: Vec<String> = vec![];
                cx.respond(empty)
            }
            FsReq::FsExists(_) => cx.respond(false),
            FsReq::FsMetadata(_) => cx.respond((0i64, false, false)),
        }
    }
}

// 3: SG (stub)
#[derive(FromCore)]
enum SgReq {
    #[core(name = "SgFind")]
    SgFind(String, String, String, Vec<String>),
    #[core(name = "SgPreview")]
    SgPreview(String, String, String, Vec<String>),
    #[core(name = "SgReplace")]
    SgReplace(String, String, String, Vec<String>),
    #[core(name = "SgRuleFind")]
    SgRuleFind(String, Value, Vec<String>),
    #[core(name = "SgRuleReplace")]
    SgRuleReplace(String, Value, String, Vec<String>),
}
struct MockSg;
impl EffectHandler for MockSg {
    type Request = SgReq;
    fn handle(&mut self, req: SgReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            SgReq::SgFind(_, _, _, _)
            | SgReq::SgPreview(_, _, _, _)
            | SgReq::SgRuleFind(_, _, _) => {
                let empty: Vec<Value> = vec![];
                cx.respond(empty)
            }
            SgReq::SgReplace(_, _, _, _) | SgReq::SgRuleReplace(_, _, _, _) => cx.respond(0i64),
        }
    }
}

// 4: Http (stub)
#[derive(FromCore)]
enum HttpReq {
    #[core(name = "HttpGet")]
    HttpGet(String),
    #[core(name = "HttpPost")]
    HttpPost(String, Value),
    #[core(name = "HttpRequest")]
    HttpRequest(String, String, Vec<(String, String)>, String),
}
struct MockHttp;
impl EffectHandler for MockHttp {
    type Request = HttpReq;
    fn handle(&mut self, _req: HttpReq, cx: &EffectContext) -> Result<Value, EffectError> {
        // Return Null
        cx.respond(())
    }
}

// 5: Exec (stub)
#[derive(FromCore)]
enum ExecReq {
    #[core(name = "Run")]
    Run(String),
    #[core(name = "RunIn")]
    RunIn(String, String),
    #[core(name = "RunJson")]
    RunJson(String),
}
struct MockExec;
impl EffectHandler for MockExec {
    type Request = ExecReq;
    fn handle(&mut self, req: ExecReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ExecReq::Run(_) | ExecReq::RunIn(_, _) => {
                cx.respond((0i64, String::new(), String::new()))
            }
            ExecReq::RunJson(_) => cx.respond(()),
        }
    }
}

// 6: Meta (stub)
#[derive(FromCore)]
enum MetaReq {
    #[core(name = "MetaConstructors")]
    MetaConstructors,
    #[core(name = "MetaLookupCon")]
    MetaLookupCon(String),
    #[core(name = "MetaPrimOps")]
    MetaPrimOps,
    #[core(name = "MetaEffects")]
    MetaEffects,
    #[core(name = "MetaDiagnostics")]
    MetaDiagnostics,
    #[core(name = "MetaVersion")]
    MetaVersion,
    #[core(name = "MetaHelp")]
    MetaHelp,
}
struct MockMeta;
impl EffectHandler for MockMeta {
    type Request = MetaReq;
    fn handle(&mut self, req: MetaReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            MetaReq::MetaConstructors => {
                let empty: Vec<(String, i64)> = vec![];
                cx.respond(empty)
            }
            MetaReq::MetaPrimOps
            | MetaReq::MetaEffects
            | MetaReq::MetaDiagnostics
            | MetaReq::MetaHelp => {
                let empty: Vec<String> = vec![];
                cx.respond(empty)
            }
            MetaReq::MetaLookupCon(_) => {
                let nothing: Option<(i64, i64)> = None;
                cx.respond(nothing)
            }
            MetaReq::MetaVersion => cx.respond(String::from("test")),
        }
    }
}

// 7: Git (stub)
#[derive(FromCore)]
enum GitReq {
    #[core(name = "GitLog")]
    GitLog(String, i64),
    #[core(name = "GitShow")]
    GitShow(String),
    #[core(name = "GitDiff")]
    GitDiff(String),
    #[core(name = "GitBlame")]
    GitBlame(String, i64, i64),
    #[core(name = "GitTree")]
    GitTree(String, String),
    #[core(name = "GitBranches")]
    GitBranches,
}
struct MockGit;
impl EffectHandler for MockGit {
    type Request = GitReq;
    fn handle(&mut self, req: GitReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            GitReq::GitLog(_, _)
            | GitReq::GitDiff(_)
            | GitReq::GitBlame(_, _, _)
            | GitReq::GitTree(_, _)
            | GitReq::GitBranches => {
                let empty: Vec<Value> = vec![];
                cx.respond(empty)
            }
            GitReq::GitShow(_) => cx.respond(()),
        }
    }
}

// 8: Llm (stub)
#[derive(FromCore)]
enum LlmReq {
    #[core(name = "LlmChat")]
    LlmChat(String),
    #[core(name = "LlmStructured")]
    LlmStructured(String, Value),
}
struct MockLlm;
impl EffectHandler for MockLlm {
    type Request = LlmReq;
    fn handle(&mut self, req: LlmReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            LlmReq::LlmChat(_) => cx.respond(String::from("mock")),
            LlmReq::LlmStructured(_, _) => cx.respond(()),
        }
    }
}

// 9: Ask (stub)
#[derive(FromCore)]
enum AskReq {
    #[core(name = "Ask")]
    Ask(String),
}
struct MockAsk;
impl EffectHandler for MockAsk {
    type Request = AskReq;
    fn handle(&mut self, _req: AskReq, cx: &EffectContext) -> Result<Value, EffectError> {
        // Return a string response
        cx.respond(String::from("stub_response"))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Minimal test: showDouble on non-constant through 10-effect dispatch.
/// This is the simplest reproduction of the MCP SIGILL bug.
#[test]
fn show_double_10_effects_minimal() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
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

result :: M Value
result = do
  let xs = [10 :: Int, 20, 30]
      n = length xs
      d = fromIntegral n :: Double
  pure (toJSON (pack (showDouble d)))
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect showDouble should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
    assert!(json.is_string(), "Expected string result, got: {}", json);
}

/// Full MCP reproduction: 10-effect dispatch with KvSet + paginateResult + showDouble.
/// This matches the exact code path the MCP server takes.
#[test]
fn show_double_10_effects_with_paginate() {
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
  let cur = case v of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  send (KvSet "__sayChars" (toJSON (cur + T.length t)))

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
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  paginateResult (max' 100 (4096 - _sayC)) (toJSON _r)
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect paginated showDouble should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
    assert!(json.is_string(), "Expected string result, got: {}", json);
}

/// Bisection test: same code with only 2 effects (Console, KV).
/// If this passes but the 10-effect version fails, the bug is in union tag dispatch.
#[test]
fn show_double_2_effects_same_code() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
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
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  pure (toJSON _r)
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![MockConsole, MockKv::new()];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("2-effect showDouble should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
    assert!(json.is_string(), "Expected string result, got: {}", json);
}

/// Bisection: 10 effects, KvSet+KvGet but NO paginateResult/valSize.
/// If this passes but with_paginate fails, the bug is in valSize/paginateResult.
#[test]
fn show_double_10_effects_kvsetget_only() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
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

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  pure (toJSON _r)
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect kvset+get showDouble should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
    assert!(json.is_string(), "Expected string result, got: {}", json);
}

/// Bisection: 10 effects, KvSet+KvGet + valSize (arrSz/objSz style).
/// Uses the same recursive helpers as MCP paginateResult.
#[test]
fn show_double_10_effects_recursive_valsize() {
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

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  let sz = valSize (toJSON _r)
  pure (toJSON sz)
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect recursive valSize showDouble should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
}

/// Full MCP reproduction: 10 effects + Library + full paginateResult + ask.
/// Uses the exact same Haskell source as the MCP server generates.
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
  let cur = case v of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
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
      String t -> let keep = max' 10 (bud - 30)
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
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  paginateResult (max' 100 (4096 - _sayC)) (toJSON _r)
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include: Vec<&std::path::Path> = vec![pp.as_path(), user_lib.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("Full MCP with Library showDouble should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
}

/// Bisection: exactly the paginate test but without `say` function.
#[test]
fn show_double_10_effects_paginate_no_say() {
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
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  paginateResult (max' 100 (4096 - _sayC)) (toJSON _r)
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect paginate no say showDouble should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
}

/// Bisection: inline paginateResult — call valSize directly in do block.
#[test]
fn show_double_10_effects_inline_paginate() {
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

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  let budget = max' 100 (4096 - _sayC)
  let val = toJSON _r
  if valSize val <= budget
    then pure val
    else pure val
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect inline paginate showDouble should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
}

/// Bisection: compute valSize but still return val (not sz).
/// If this fails, the bug is about returning the Value after case-matching it.
#[test]
fn show_double_10_effects_valsize_return_val() {
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

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  let val = toJSON _r
      _sz = valSize val
  pure val
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect valSize return val should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
    assert!(json.is_string(), "Expected string, got: {}", json);
}

/// Bisection: seq valSize then return val (force evaluation but no conditional).
#[test]
fn show_double_10_effects_seq_valsize() {
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
    else pure val
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect seq valSize should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
    assert!(json.is_string(), "Expected string, got: {}", json);
}

/// Bisection: full KvSet/KvGet + if valSize val <= budget.
/// Like inline_paginate but with sz as let-binding.
#[test]
fn show_double_10_effects_full_with_let_sz() {
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

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  let val = toJSON _r
      budget = max' 100 (4096 - _sayC)
      sz = valSize val
  if sz <= budget
    then pure val
    else pure val
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect full with let sz should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
    assert!(json.is_string(), "Expected string, got: {}", json);
}

/// Bisection: KvSet + showDouble + valSize comparison + return val. No KvGet.
#[test]
fn show_double_10_effects_kvset_no_kvget() {
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
    else pure val
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect kvset no kvget should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
    assert!(json.is_string(), "Expected string, got: {}", json);
}

/// Bisection: KvSet + KvGet + case match on result, then JUST return val.
/// No valSize. Tests if KvGet + case match on Maybe is the trigger.
#[test]
fn show_double_10_effects_kvget_case_then_val() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
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

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  pure (toJSON _r)
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect kvget case then val should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
    assert!(json.is_string(), "Expected string, got: {}", json);
}

/// Bisection: KvGet+case + valSize but NO conditional. Just use sz.
#[test]
fn show_double_10_effects_kvget_valsize_no_cond() {
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

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  let val = toJSON _r
      sz = valSize val
      budget = max' 100 (4096 - _sayC)
  pure (toJSON (sz + budget))
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect kvget valSize no cond should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
}

/// Bisection: 10 effects, KvSet+KvGet + valSize (but simplified paginateResult).
/// Tests whether valSize's case-match on Value is the crash point.
#[test]
fn show_double_10_effects_with_valsize() {
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
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  let sz = valSize (toJSON _r)
  pure (toJSON sz)
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect valSize showDouble should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
}

/// Test with runtime-computed Double (non-constant-foldable) through effect dispatch.
/// Uses `stake 3 [1..]` to prevent GHC constant folding.
#[test]
fn show_double_10_effects_infinite_list() {
    let src = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
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

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = stake 3 [1 :: Int ..]
        s = foldl' (+) 0 xs
        d = fromIntegral s :: Double
    pure (pack (showDouble d))
  pure (toJSON _r)
"#;
    let pp = prelude_path();
    let src_owned = src.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![
                MockConsole,
                MockKv::new(),
                MockFs,
                MockSg,
                MockHttp,
                MockExec,
                MockMeta,
                MockGit,
                MockLlm,
                MockAsk
            ];
            compile_and_run(&src_owned, "result", &include, &mut handlers, &())
                .expect("10-effect infinite list showDouble should not crash")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    eprintln!("Result: {}", json);
    assert!(json.is_string(), "Expected string result, got: {}", json);
}
