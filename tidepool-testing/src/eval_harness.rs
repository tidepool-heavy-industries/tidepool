//! Fluent eval-pipeline harness for Tidepool integration tests.
//!
//! # Why this exists
//!
//! ~250 test sites across `tidepool-runtime/tests` and `tidepool-repl/tests`
//! hand-roll the same setup: derive the Prelude include dir, spawn an 8-256 MiB
//! stack thread (deep [`Value`] spines overflow the default 2 MiB test-thread
//! stack — see the "Host stack-overflow class" note), call one of
//! `compile_and_run` / `compile_and_run_pure` / `compile_haskell`, then unwrap
//! the [`EvalResult`]. Effectful tests additionally re-declare the full 10-effect
//! MCP GADT stack verbatim (~60 lines each) and re-implement ten mock handlers.
//!
//! [`EvalHarness`] centralizes all of that behind a builder while still driving
//! the REAL `tidepool_runtime` entry points (it *wraps*, never reimplements —
//! tests must keep exercising the production compile→JIT→dispatch path).
//!
//! # New tests use this
//!
//! ```no_run
//! use tidepool_testing::eval_harness::EvalHarness;
//!
//! // Pure expression (no effects):
//! let out = EvalHarness::new().with_stdlib().run_pure(
//!     "module Test where\nn :: Int\nn = 2 + 3",
//!     "n",
//! );
//! assert_eq!(out.json(), serde_json::json!(5));
//!
//! // Effectful, against the canonical 10-effect MCP stack + mock handlers:
//! use tidepool_testing::eval_harness::mock;
//! let src = mock::mcp_module("result :: M Value\nresult = pure (toJSON (1 :: Int))");
//! let out = EvalHarness::new()
//!     .with_stdlib()
//!     .run(&src, "result", mock::min_stack());
//! assert!(out.is_ok());
//! ```
//!
//! Guard suites that need GHC with [`extract_available`] (or the builder's
//! [`EvalHarness::with_extract_env`]) so they skip cleanly outside `nix develop`.

use std::path::{Path, PathBuf};

use tidepool_runtime::{
    compile_and_run, compile_and_run_pure, compile_and_run_with_nursery_size, compile_haskell,
    CompileError, CompileResult, DispatchEffect, EvalResult, RuntimeError, Value, EVAL_STACK_SIZE,
};

/// Repo root, derived from this crate's manifest dir (`<root>/tidepool-testing`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-testing has a parent (repo root)")
        .to_path_buf()
}

/// The Haskell stdlib / Prelude include dir (`<root>/haskell/lib`).
///
/// Every effectful or Prelude-using test needs this on the include path; it was
/// previously re-derived by a local `prelude_path()` in a dozen files.
pub fn prelude_path() -> PathBuf {
    repo_root().join("haskell").join("lib")
}

/// The generated `Tidepool.Effects` module dir for the standard MCP effect set.
///
/// Needed on the include path when a test pulls in a `.tidepool/lib` module (or
/// anything that `import`s the generated effects module). Wraps
/// [`tidepool_mcp::ensure_effects_module`] over [`tidepool_mcp::standard_decls`].
pub fn effects_include() -> PathBuf {
    tidepool_mcp::ensure_effects_module(&tidepool_mcp::standard_decls())
        .expect("write Tidepool.Effects module")
}

/// Derive and install the `TIDEPOOL_EXTRACT` env var, returning whether the
/// extract toolchain is actually usable.
///
/// This is the one place the "where is the extract binary" question is answered,
/// so individual tests stop copy-pasting `cabal list-bin` paths (or, worse,
/// `/nix/store` literals):
///
/// 1. If `TIDEPOOL_EXTRACT` is already set and runs, keep it.
/// 2. Otherwise try `cabal list-bin tidepool-extract-bin` in `<root>/haskell`
///    (the dev build — tracks your working tree).
/// 3. Otherwise fall back to the checked-in nix-profile wrapper
///    `<root>/haskell/tidepool-extract`.
///
/// Returns `true` iff the resolved binary runs and prints its usage banner
/// (a no-args invocation — the extract binary has no version flag; any flag it
/// doesn't recognize is treated as an input file and fails). The banner is on
/// stderr — stdout always carries the fixed-shape diagnostics JSON, even for
/// this no-args case (`{"version":1,"diagnostics":[]}`).
pub fn extract_env() -> bool {
    fn runs(bin: &str) -> bool {
        std::process::Command::new(bin)
            .stdout(std::process::Stdio::null())
            .output()
            .map(|out| out.status.success() && out.stderr.starts_with(b"Usage:"))
            .unwrap_or(false)
    }

    if let Ok(bin) = std::env::var("TIDEPOOL_EXTRACT") {
        if runs(&bin) {
            return true;
        }
    }

    let haskell = repo_root().join("haskell");

    // 2. cabal list-bin (dev build).
    if let Ok(out) = std::process::Command::new("cabal")
        .args(["list-bin", "tidepool-extract-bin"])
        .current_dir(&haskell)
        .output()
    {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() && Path::new(&path).exists() {
                std::env::set_var("TIDEPOOL_EXTRACT", &path);
                if runs(&path) {
                    return true;
                }
            }
        }
    }

    // 3. Checked-in nix-profile wrapper.
    let wrapper = haskell.join("tidepool-extract");
    if wrapper.exists() {
        let p = wrapper.to_string_lossy().to_string();
        std::env::set_var("TIDEPOOL_EXTRACT", &p);
        if runs(&p) {
            return true;
        }
    }

    false
}

/// True iff the session-aware `tidepool-extract` is reachable — the standard
/// skip guard for suites that need GHC (CI without the nix shell). Unlike
/// [`extract_env`] this does not mutate the environment beyond what a lookup of
/// an already-set `TIDEPOOL_EXTRACT` implies.
pub fn extract_available() -> bool {
    extract_env()
}

/// Run `f` on a fresh thread with the JIT eval stack size ([`EVAL_STACK_SIZE`]).
///
/// Deep `Value` spines recurse on `Drop`; the default ~2 MiB test-thread stack
/// overflows (silent thread death → hang). Every hand-rolled `run` helper did
/// this dance; call this instead.
pub fn with_eval_stack<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(EVAL_STACK_SIZE)
        .spawn(f)
        .expect("spawn eval-stack thread")
        .join()
        .expect("eval-stack thread panicked")
}

/// The outcome of a compile+run: a thin, assertion-friendly wrapper over
/// `Result<EvalResult, RuntimeError>` with typed accessors so tests read
/// `out.json()` / `out.value()` / `out.err()` instead of matching by hand.
pub struct Outcome(Result<EvalResult, RuntimeError>);

impl Outcome {
    /// Did evaluation succeed?
    pub fn is_ok(&self) -> bool {
        self.0.is_ok()
    }

    /// The runtime error, if evaluation failed.
    pub fn err(&self) -> Option<&RuntimeError> {
        self.0.as_ref().err()
    }

    /// Borrow the successful [`EvalResult`], panicking with the error otherwise.
    pub fn result(&self) -> &EvalResult {
        self.0
            .as_ref()
            .unwrap_or_else(|e| panic!("expected successful eval, got error: {e}"))
    }

    /// Borrow the computed [`Value`] (panics on error).
    pub fn value(&self) -> &Value {
        self.result().value()
    }

    /// Render the result as JSON (panics on error).
    pub fn json(&self) -> serde_json::Value {
        self.result().to_json()
    }

    /// JSON if successful, else `None` (for tests that tolerate either).
    pub fn try_json(&self) -> Option<serde_json::Value> {
        self.0.as_ref().ok().map(|r| r.to_json())
    }

    /// Consume and return the owned [`EvalResult`], panicking with `ctx` + error.
    pub fn expect(self, ctx: &str) -> EvalResult {
        self.0.unwrap_or_else(|e| panic!("{ctx}: {e}"))
    }

    /// Consume and return the owned [`EvalResult`] (panics on error).
    pub fn unwrap(self) -> EvalResult {
        self.0.expect("expected successful eval")
    }

    /// The raw `Result` (escape hatch for tests asserting on specific errors).
    pub fn into_result(self) -> Result<EvalResult, RuntimeError> {
        self.0
    }
}

/// Fluent builder for the Tidepool eval pipeline. Configure the include path and
/// nursery, then call a terminal (`run` / `run_pure` / `compile`). Terminals run
/// on an [`EVAL_STACK_SIZE`] thread.
#[derive(Default, Clone)]
pub struct EvalHarness {
    includes: Vec<PathBuf>,
    nursery: Option<usize>,
}

impl EvalHarness {
    /// Empty harness (no includes).
    pub fn new() -> Self {
        Self::default()
    }

    /// Add the Haskell stdlib / Prelude include dir ([`prelude_path`]).
    pub fn with_stdlib(mut self) -> Self {
        self.includes.push(prelude_path());
        self
    }

    /// Add the generated `Tidepool.Effects` module dir ([`effects_include`]).
    pub fn with_effects_module(mut self) -> Self {
        self.includes.push(effects_include());
        self
    }

    /// Add an arbitrary include dir.
    pub fn with_include(mut self, dir: impl Into<PathBuf>) -> Self {
        self.includes.push(dir.into());
        self
    }

    /// Ensure `TIDEPOOL_EXTRACT` is derived/installed for this process. Does not
    /// change the harness; returns `self` for chaining. Pair with
    /// [`extract_available`] to decide whether to skip.
    pub fn with_extract_env(self) -> Self {
        let _ = extract_env();
        self
    }

    /// Override the JIT nursery size (default [`tidepool_runtime::DEFAULT_NURSERY_SIZE`]).
    pub fn with_nursery(mut self, bytes: usize) -> Self {
        self.nursery = Some(bytes);
        self
    }

    fn owned_includes(&self) -> Vec<PathBuf> {
        self.includes.clone()
    }

    /// Compile only (no JIT), returning the raw [`CompileResult`] — for tests
    /// that inspect the Core / DataConTable or assert on a [`CompileError`].
    pub fn compile(&self, source: &str, target: &str) -> Result<CompileResult, CompileError> {
        let includes = self.owned_includes();
        let refs: Vec<&Path> = includes.iter().map(|p| p.as_path()).collect();
        compile_haskell(source, target, &refs)
    }

    /// Compile + run a PURE expression (no effects / handlers).
    pub fn run_pure(&self, source: &str, target: &str) -> Outcome {
        let includes = self.owned_includes();
        let source = source.to_owned();
        let target = target.to_owned();
        Outcome(with_eval_stack(move || {
            let refs: Vec<&Path> = includes.iter().map(|p| p.as_path()).collect();
            compile_and_run_pure(&source, &target, &refs)
        }))
    }

    /// Compile + run an EFFECTFUL expression against `handlers` (user context
    /// `()`). `handlers` is any `frunk` HList of `EffectHandler`s (e.g.
    /// [`mock::min_stack`]).
    pub fn run<H>(&self, source: &str, target: &str, handlers: H) -> Outcome
    where
        H: DispatchEffect<()> + Send + 'static,
    {
        self.run_with(source, target, handlers, ())
    }

    /// As [`run`](Self::run) but with an explicit user context `U`.
    pub fn run_with<U, H>(&self, source: &str, target: &str, mut handlers: H, user: U) -> Outcome
    where
        U: Send + 'static,
        H: DispatchEffect<U> + Send + 'static,
    {
        let includes = self.owned_includes();
        let source = source.to_owned();
        let target = target.to_owned();
        let nursery = self.nursery;
        Outcome(with_eval_stack(move || {
            let refs: Vec<&Path> = includes.iter().map(|p| p.as_path()).collect();
            match nursery {
                Some(n) => compile_and_run_with_nursery_size(
                    &source,
                    &target,
                    &refs,
                    &mut handlers,
                    &user,
                    n,
                ),
                None => compile_and_run(&source, &target, &refs, &mut handlers, &user),
            }
        }))
    }

    /// As [`run`](Self::run) but hands `handlers` back alongside the
    /// [`Outcome`] — for dispatchers whose post-eval state (write counts,
    /// stored files, recorded calls) IS the assertion.
    pub fn run_owned<H>(&self, source: &str, target: &str, handlers: H) -> (Outcome, H)
    where
        H: DispatchEffect<()> + Send + 'static,
    {
        self.run_with_owned(source, target, handlers, ())
    }

    /// As [`run_with`](Self::run_with) but hands `handlers` back alongside the
    /// [`Outcome`].
    pub fn run_with_owned<U, H>(
        &self,
        source: &str,
        target: &str,
        mut handlers: H,
        user: U,
    ) -> (Outcome, H)
    where
        U: Send + 'static,
        H: DispatchEffect<U> + Send + 'static,
    {
        let includes = self.owned_includes();
        let source = source.to_owned();
        let target = target.to_owned();
        let nursery = self.nursery;
        let (result, handlers) = with_eval_stack(move || {
            let refs: Vec<&Path> = includes.iter().map(|p| p.as_path()).collect();
            let result = match nursery {
                Some(n) => compile_and_run_with_nursery_size(
                    &source,
                    &target,
                    &refs,
                    &mut handlers,
                    &user,
                    n,
                ),
                None => compile_and_run(&source, &target, &refs, &mut handlers, &user),
            };
            (result, handlers)
        });
        (Outcome(result), handlers)
    }
}

/// The canonical MCP effect stack: the 10-effect GADT preamble every effectful
/// runtime test used to re-declare verbatim, plus ten stub handlers and a
/// ready-made [`mock::min_stack`] `frunk` HList.
///
/// The GADT declarations here and the handler `enum` arities are kept in lockstep
/// on purpose — owning both in one place is what keeps them from drifting.
pub mod mock {
    use std::collections::HashMap;

    use tidepool_bridge_derive::FromCore;
    use tidepool_bridge_effects::{FileMeta, Proc};
    use tidepool_effect::{EffectContext, EffectError, EffectHandler, Response};
    use tidepool_eval::value::Value;

    /// The standard MCP module preamble: LANGUAGE pragmas, `module Expr`, the
    /// common imports, the 10-effect GADT declarations, and `type M = Eff '[…]`.
    /// Concatenate your helper defs + `result` after it, or use [`mcp_module`].
    pub const MCP_PREAMBLE: &str = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Tidepool.Aeson.KeyMap as KM
import Control.Monad.Freer hiding (run)
import qualified Prelude as P
default (Int, Text)
error :: Text -> a
error = P.error . T.unpack

-- Effect-local error ADTs. Production generates these from effect_defs.rs at
-- codegen time (see tidepool-mcp/src/effect_defs.rs); this static preamble
-- hand-declares the same shapes so GADT return types below can reference them.
data ExecError = ExecSpawn Text | ExecBadDir Text deriving (Show, Eq)
data HttpError = HttpInvalidUrl Text | HttpRestricted Text | HttpNetwork Text | HttpStatus Int Text | HttpTooLarge Int deriving (Show, Eq)
data GitError = GitBadRevspec Text | GitFailed Int Text deriving (Show, Eq)
data LlmError = LlmApi Text | LlmRefusal Text | LlmBudget deriving (Show, Eq)

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
  FsMetadata :: Text -> Fs (Maybe FileMeta)
data SG a where
  SgFind :: Text -> Text -> Text -> [Text] -> SG [Value]
  SgPreview :: Text -> Text -> Text -> [Text] -> SG [Value]
  SgReplace :: Text -> Text -> Text -> [Text] -> SG Int
  SgRuleFind :: Text -> Value -> [Text] -> SG [Value]
  SgRuleReplace :: Text -> Value -> Text -> [Text] -> SG Int
data Http a where
  HttpGet :: Text -> Http (Either HttpError Value)
  HttpPost :: Text -> Value -> Http (Either HttpError Value)
  HttpRequest :: Text -> Text -> [(Text,Text)] -> Text -> Http Value
data Exec a where
  Run :: Text -> Exec (Either ExecError Proc)
  RunIn :: Text -> Text -> Exec (Either ExecError Proc)
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
  GitShow :: Text -> Git (Either GitError Commit)
  GitDiff :: Text -> Git [Value]
  GitBlame :: Text -> Int -> Int -> Git [Value]
  GitTree :: Text -> Text -> Git [Value]
  GitBranches :: Git [Value]
data Llm a where
  LlmChat :: Text -> Llm Text
  LlmStructured :: Text -> Value -> Llm (Either LlmError Value)
data Ask a where
  Ask :: Text -> Ask Value

type M = Eff '[Console, KV, Fs, SG, Http, Exec, Meta, Git, Llm, Ask]
"#;

    /// [`MCP_PREAMBLE`] followed by `body` (your helper defs + `result`). The
    /// standard way to build a source string for the 10-effect stack.
    pub fn mcp_module(body: &str) -> String {
        format!("{MCP_PREAMBLE}\n{body}\n")
    }

    // 0: Console
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum ConsoleReq {
        #[core(name = "Print")]
        Print(String),
    }
    pub struct MockConsole;
    impl EffectHandler for MockConsole {
        type Request = ConsoleReq;
        fn handle(&mut self, req: ConsoleReq, cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                ConsoleReq::Print(msg) => {
                    eprintln!("[Console] Print: {msg}");
                    cx.respond(())
                }
            }
        }
    }

    // 1: KV — stores serde_json::Value like the real MCP.
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum KvReq {
        #[core(name = "KvGet")]
        KvGet(String),
        #[core(name = "KvSet")]
        KvSet(String, Value),
        #[core(name = "KvDelete")]
        KvDelete(String),
        #[core(name = "KvKeys")]
        KvKeys,
    }
    #[derive(Default)]
    pub struct MockKv {
        store: HashMap<String, serde_json::Value>,
    }
    impl MockKv {
        pub fn new() -> Self {
            Self::default()
        }
    }
    impl EffectHandler for MockKv {
        type Request = KvReq;
        fn handle(&mut self, req: KvReq, cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                KvReq::KvGet(key) => cx.respond(self.store.get(&key).cloned()),
                KvReq::KvSet(key, val) => {
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

    // 2: Fs (stub)
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum FsReq {
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
    pub struct MockFs;
    impl EffectHandler for MockFs {
        type Request = FsReq;
        fn handle(&mut self, req: FsReq, cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                FsReq::FsRead(_) => cx.respond(String::new()),
                FsReq::FsWrite(_, _) => cx.respond(()),
                FsReq::FsListDir(_) | FsReq::FsGlob(_) => {
                    let empty: Vec<String> = vec![];
                    cx.respond(empty)
                }
                FsReq::FsExists(_) => cx.respond(false),
                FsReq::FsMetadata(_) => cx.respond(Some(FileMeta {
                    size: 0,
                    is_file: false,
                    is_dir: false,
                })),
            }
        }
    }

    // 3: SG (stub)
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum SgReq {
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
    pub struct MockSg;
    impl EffectHandler for MockSg {
        type Request = SgReq;
        fn handle(&mut self, req: SgReq, cx: &EffectContext) -> Result<Response, EffectError> {
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
    #[allow(dead_code)]
    pub enum HttpReq {
        #[core(name = "HttpGet")]
        HttpGet(String),
        #[core(name = "HttpPost")]
        HttpPost(String, Value),
        #[core(name = "HttpRequest")]
        HttpRequest(String, String, Vec<(String, String)>, String),
    }
    pub struct MockHttp;
    impl EffectHandler for MockHttp {
        type Request = HttpReq;
        fn handle(&mut self, req: HttpReq, cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                HttpReq::HttpGet(_) | HttpReq::HttpPost(_, _) => {
                    cx.respond(Ok::<serde_json::Value, String>(serde_json::json!({})))
                }
                HttpReq::HttpRequest(_, _, _, _) => cx.respond(serde_json::json!({})),
            }
        }
    }

    // 5: Exec (stub)
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum ExecReq {
        #[core(name = "Run")]
        Run(String),
        #[core(name = "RunIn")]
        RunIn(String, String),
        #[core(name = "RunJson")]
        RunJson(String),
    }
    pub struct MockExec;
    impl EffectHandler for MockExec {
        type Request = ExecReq;
        fn handle(&mut self, req: ExecReq, cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                ExecReq::Run(_) | ExecReq::RunIn(_, _) => cx.respond(Ok::<Proc, String>(Proc {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })),
                ExecReq::RunJson(_) => cx.respond(()),
            }
        }
    }

    // 6: Meta (stub)
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum MetaReq {
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
    pub struct MockMeta;
    impl EffectHandler for MockMeta {
        type Request = MetaReq;
        fn handle(&mut self, req: MetaReq, cx: &EffectContext) -> Result<Response, EffectError> {
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
    #[allow(dead_code)]
    pub enum GitReq {
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
    pub struct MockGit;
    impl EffectHandler for MockGit {
        type Request = GitReq;
        fn handle(&mut self, req: GitReq, cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                GitReq::GitLog(_, _)
                | GitReq::GitDiff(_)
                | GitReq::GitBlame(_, _, _)
                | GitReq::GitTree(_, _)
                | GitReq::GitBranches => {
                    let empty: Vec<Value> = vec![];
                    cx.respond(empty)
                }
                GitReq::GitShow(_) => cx.respond(Ok::<tidepool_bridge_effects::GitCommit, String>(
                    tidepool_bridge_effects::GitCommit {
                        sha: String::new(),
                        subject: String::new(),
                        author: String::new(),
                        date: String::new(),
                        files: vec![],
                    },
                )),
            }
        }
    }

    // 8: Llm (stub)
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum LlmReq {
        #[core(name = "LlmChat")]
        LlmChat(String),
        #[core(name = "LlmStructured")]
        LlmStructured(String, Value),
    }
    pub struct MockLlm;
    impl EffectHandler for MockLlm {
        type Request = LlmReq;
        fn handle(&mut self, req: LlmReq, cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                LlmReq::LlmChat(_) => cx.respond(String::from("mock")),
                LlmReq::LlmStructured(_, _) => {
                    cx.respond(Ok::<serde_json::Value, String>(serde_json::json!({})))
                }
            }
        }
    }

    // 9: Ask (stub)
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum AskReq {
        #[core(name = "Ask")]
        Ask(String),
    }
    pub struct MockAsk;
    impl EffectHandler for MockAsk {
        type Request = AskReq;
        fn handle(&mut self, _req: AskReq, cx: &EffectContext) -> Result<Response, EffectError> {
            cx.respond(serde_json::json!("stub_response"))
        }
    }

    /// The canonical 10-effect mock handler HList, in stack order
    /// `[Console, KV, Fs, SG, Http, Exec, Meta, Git, Llm, Ask]` — pass straight
    /// to [`super::EvalHarness::run`].
    pub fn min_stack() -> frunk::HList!(
        MockConsole,
        MockKv,
        MockFs,
        MockSg,
        MockHttp,
        MockExec,
        MockMeta,
        MockGit,
        MockLlm,
        MockAsk
    ) {
        frunk::hlist![
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
        ]
    }
}
