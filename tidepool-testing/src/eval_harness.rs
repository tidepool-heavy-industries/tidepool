//! Fluent eval-pipeline harness for Tidepool integration tests.
//!
//! # Why this exists
//!
//! ~250 test sites across `tidepool-runtime/tests` and `tidepool-repl/tests`
//! hand-roll the same setup: derive the Prelude include dir, spawn an 8-256 MiB
//! stack thread (deep [`Value`] spines overflow the default 2 MiB test-thread
//! stack), call one of
//! `compile_and_run` / `compile_and_run_pure` / `compile_haskell`, then unwrap
//! the [`EvalResult`]. Effectful tests additionally re-declare the base MCP
//! GADT stack verbatim (~60 lines each) and re-implement mock handlers.
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
//! // Effectful, against the base MCP stack + mock handlers:
//! use tidepool_testing::eval_harness::mock;
//! let src = mock::mcp_module("result :: M Value\nresult = pure (toJSON (1 :: Int))");
//! let out = EvalHarness::new()
//!     .with_stdlib()
//!     .run(&src, "result", mock::min_stack());
//! assert!(out.is_ok());
//! ```
//!
//! Guard suites that need GHC with [`require_extract`], which panics loudly
//! (naming the fix) rather than silently `return`ing — a silent skip reports
//! as a nextest PASS, defeating every downstream receipt check. GHC-heavy
//! tests are excluded from the default nextest filter for exactly this
//! reason; [`require_extract`] only ever fires on a direct
//! `--ignore-default-filter` invocation missing the environment, which is a
//! caller error, not a legitimate skip. [`extract_available`] remains for the
//! rare caller that branches on toolchain presence without a bare skip.

use std::path::{Path, PathBuf};

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_repr::DataConTable;
use tidepool_runtime::{
    compile_and_run, compile_and_run_pure, compile_and_run_with_nursery_size, compile_haskell,
    compile_targets, CompileError, CompileResult, CompiledArtifacts, DispatchEffect, EvalResult,
    RuntimeError, Value, DEFAULT_NURSERY_SIZE, EVAL_STACK_SIZE,
};

/// Repo root, derived from this crate's manifest dir (`<root>/tidepool-testing`).
pub fn repo_root() -> PathBuf {
    #[allow(
        clippy::expect_used,
        reason = "tidepool-testing has a parent (repo root)"
    )]
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-testing has a parent (repo root)")
        .to_path_buf()
}

/// The Haskell stdlib / Prelude include dir (`<root>/haskell/lib`).
///
/// Every effectful or Prelude-using test needs this on the include path.
pub fn prelude_path() -> PathBuf {
    repo_root().join("haskell").join("lib")
}

/// The user verb-library dir (`<root>/.tidepool/lib`).
///
/// Tests that exercise `.tidepool/lib` modules need this on the include path.
pub fn user_lib_dir() -> PathBuf {
    repo_root().join(".tidepool").join("lib")
}

/// The generated `Tidepool.Effects.Core` + `Tidepool.Effects` module dirs for
/// the standard MCP effect set, BOTH needed on the include path (the shim's
/// `import Tidepool.Effects.Core` resolves against the first).
///
/// Needed on the include path when a test pulls in a `.tidepool/lib` module (or
/// anything that `import`s the generated effects module). Wraps
/// [`tidepool_mcp::ensure_effects_module`] over [`tidepool_mcp::standard_decls`].
pub fn effects_include() -> [PathBuf; 2] {
    #[allow(clippy::expect_used, reason = "write Tidepool.Effects module")]
    tidepool_mcp::ensure_effects_module(&tidepool_mcp::standard_decls())
        .expect("write Tidepool.Effects module")
        .include_paths()
}

const EXTRACT_ENV: &str = "TIDEPOOL_EXTRACT";
const EXTRACT_WORKER_ENV: &str = "TIDEPOOL_EXTRACT_WORKER";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExtractBinaryKind {
    Frontend,
    Worker,
    Unknown,
}

fn classify_extract_output(stderr: &[u8]) -> ExtractBinaryKind {
    if stderr.starts_with(b"Usage: tidepool-extract [") {
        ExtractBinaryKind::Frontend
    } else if stderr.starts_with(b"worker requires") {
        ExtractBinaryKind::Worker
    } else {
        ExtractBinaryKind::Unknown
    }
}

fn probe_extract(bin: &Path) -> ExtractBinaryKind {
    std::process::Command::new(bin)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .output()
        .map(|out| classify_extract_output(&out.stderr))
        .unwrap_or(ExtractBinaryKind::Unknown)
}

fn cabal_worker(haskell: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("cabal")
        .args(["list-bin", "tidepool-extract-bin"])
        .current_dir(haskell)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }

    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (probe_extract(&path) == ExtractBinaryKind::Worker).then_some(path)
}

/// Derive and install the extractor frontend and worker environment, returning
/// whether the session-aware toolchain is actually usable.
///
/// This is the one place the "where is the extract binary" question is answered,
/// so individual tests stop copy-pasting `cabal list-bin` paths (or, worse,
/// `/nix/store` literals):
///
/// 1. If `TIDEPOOL_EXTRACT` names the session-aware Rust frontend, keep it.
/// 2. Otherwise pair `<root>/target/debug/tidepool-extract` with the Cabal-built
///    `tidepool-extract-bin` worker.
/// 3. Otherwise try the session-aware frontend on `PATH`.
///
/// Frontend and worker identities are deliberately distinct. The worker speaks
/// a private, versioned protocol and is never a valid value of
/// `TIDEPOOL_EXTRACT`.
pub fn extract_env() -> bool {
    if let Some(bin) = std::env::var_os(EXTRACT_ENV) {
        return probe_extract(Path::new(&bin)) == ExtractBinaryKind::Frontend;
    }

    let haskell = repo_root().join("haskell");
    let frontend = repo_root().join("target/debug/tidepool-extract");
    if probe_extract(&frontend) == ExtractBinaryKind::Frontend {
        let worker = std::env::var_os(EXTRACT_WORKER_ENV)
            .map(PathBuf::from)
            .filter(|path| probe_extract(path) == ExtractBinaryKind::Worker)
            .or_else(|| cabal_worker(&haskell));
        if let Some(worker) = worker {
            // Install the pair only after both halves satisfy their contracts.
            std::env::set_var(EXTRACT_WORKER_ENV, worker);
            std::env::set_var(EXTRACT_ENV, frontend);
            return true;
        }
    }

    let path_frontend = Path::new("tidepool-extract");
    if probe_extract(path_frontend) == ExtractBinaryKind::Frontend {
        std::env::set_var(EXTRACT_ENV, path_frontend);
        return true;
    }

    false
}

#[cfg(test)]
mod extract_env_tests {
    use super::{classify_extract_output, ExtractBinaryKind};

    #[test]
    fn extractor_roles_are_not_interchangeable() {
        assert_eq!(
            classify_extract_output(b"Usage: tidepool-extract [OPTIONS] <file.hs> ...\n"),
            ExtractBinaryKind::Frontend
        );
        assert_eq!(
            classify_extract_output(b"worker requires exactly one versioned request\n"),
            ExtractBinaryKind::Worker
        );
        assert_eq!(
            classify_extract_output(b"Usage: tidepool-extract-bin [OPTIONS]\n"),
            ExtractBinaryKind::Unknown
        );
    }
}

/// Resolve and install the session-aware extractor pair when available.
///
/// This is the standard availability guard for suites that need GHC (CI without
/// the Nix shell). It has the same environment-installing behavior as
/// [`extract_env`]; the separate name exists for readable test guards.
pub fn extract_available() -> bool {
    extract_env()
}

/// Panic loudly if the GHC-tier toolchain isn't reachable, instead of the
/// caller silently `return`ing (which nextest reports as a PASS). This is the
/// standard guard for a GHC-heavy integration test: such tests are excluded
/// from the default nextest filter, so this only ever fires on a direct
/// `--ignore-default-filter` invocation missing the environment — a caller
/// error, not a legitimate skip.
pub fn require_extract() {
    if !extract_available() {
        panic!(
            "TIDEPOOL_EXTRACT not set and no working tidepool-extract toolchain found — \
             this GHC-tier test cannot run vacuously. Set TIDEPOOL_EXTRACT (or run inside \
             `nix develop`) or run via scripts/battery.sh, which derives it."
        );
    }
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
    #[allow(
        clippy::expect_used,
        reason = "spawn/join of the eval-stack thread does not fail in practice"
    )]
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

    /// Consume and return the owned [`EvalResult`], panicking with `ctx` + error.
    pub fn expect(self, ctx: &str) -> EvalResult {
        self.0.unwrap_or_else(|e| panic!("{ctx}: {e}"))
    }

    /// Consume and return the owned [`EvalResult`] (panics on error).
    pub fn unwrap(self) -> EvalResult {
        #[allow(clippy::expect_used, reason = "expected successful eval")]
        self.0.expect("expected successful eval")
    }

    /// The raw `Result` (escape hatch for tests asserting on specific errors).
    pub fn into_result(self) -> Result<EvalResult, RuntimeError> {
        self.0
    }
}

/// As [`Outcome`], but for one target of an [`EvalHarness::compile_many`]
/// bundle, run via [`EvalHarness::run_target`]/[`run_target_owned`]. A
/// pre-compiled target has no [`CompileResult`] of its own to hand
/// `EvalResult::new` (crate-private in `tidepool-runtime`), so this wraps the
/// raw `Value` + [`DataConTable`] pair the JIT run itself produces and
/// renders JSON through the same public [`tidepool_runtime::value_to_json`]
/// path `EvalResult::to_json` uses internally.
pub struct TargetOutcome(Result<(Value, DataConTable), RuntimeError>);

impl TargetOutcome {
    /// Did evaluation succeed?
    pub fn is_ok(&self) -> bool {
        self.0.is_ok()
    }

    /// The runtime error, if evaluation failed.
    pub fn err(&self) -> Option<&RuntimeError> {
        self.0.as_ref().err()
    }

    fn result(&self) -> &(Value, DataConTable) {
        self.0
            .as_ref()
            .unwrap_or_else(|e| panic!("expected successful eval, got error: {e}"))
    }

    /// Borrow the computed [`Value`] (panics on error).
    pub fn value(&self) -> &Value {
        &self.result().0
    }

    /// Render the result as JSON (panics on error).
    pub fn json(&self) -> serde_json::Value {
        let (value, table) = self.result();
        tidepool_runtime::value_to_json(value, table, 0)
    }

    /// Consume and return the owned `(Value, DataConTable)` pair, panicking
    /// with `ctx` + error.
    pub fn expect(self, ctx: &str) -> (Value, DataConTable) {
        self.0.unwrap_or_else(|e| panic!("{ctx}: {e}"))
    }

    /// The raw `Result` (escape hatch for tests asserting on specific errors).
    pub fn into_result(self) -> Result<(Value, DataConTable), RuntimeError> {
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

    /// Add the generated `Tidepool.Effects.Core` + `Tidepool.Effects` module
    /// dirs ([`effects_include`]).
    pub fn with_effects_module(mut self) -> Self {
        self.includes.extend(effects_include());
        self
    }

    /// Add an arbitrary include dir.
    pub fn with_include(mut self, dir: impl Into<PathBuf>) -> Self {
        self.includes.push(dir.into());
        self
    }

    /// Add several arbitrary include dirs at once — e.g. a non-standard
    /// [`tidepool_mcp::ensure_effects_module`] result's
    /// [`tidepool_mcp::EffectsModuleDirs::include_paths`].
    pub fn with_includes(mut self, dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.includes.extend(dirs);
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

    /// Compile MULTIPLE named top-level bindings sharing ONE module against
    /// ONE `tidepool-extract` spawn ([`compile_targets`]'s N-in-one-spawn
    /// mode — see `tidepool-runtime/CLAUDE.md`'s "Compile cache" section and
    /// `plans/test-time-cut.md`'s §3: an extra target in the same spawn costs
    /// ~2% more wall time, not another full GHC session). Run each target
    /// independently afterward via [`run_target`](Self::run_target)/
    /// [`run_target_owned`](Self::run_target_owned) — own handler instance,
    /// own dispatch history per target, same isolation as N separate
    /// `#[test]` fns, one spawn instead of N.
    pub fn compile_many(
        &self,
        source: &str,
        targets: &[&str],
    ) -> Result<CompiledArtifacts, CompileError> {
        let includes = self.owned_includes();
        compile_targets(source, targets, &includes, None, |_, _, _| {})
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

    /// Run one target out of a [`compile_many`](Self::compile_many) bundle
    /// against `handlers` (user context `()`) — own [`EVAL_STACK_SIZE`]
    /// thread, own dispatch, no additional `tidepool-extract` spawn (the
    /// target's Core was already produced by `compile_many`). Mirrors
    /// [`run`](Self::run) but takes the pre-compiled [`CompiledArtifacts`]
    /// and a target name instead of source.
    pub fn run_target<H>(
        &self,
        artifacts: &CompiledArtifacts,
        target: &str,
        handlers: H,
    ) -> TargetOutcome
    where
        H: DispatchEffect<()> + Send + 'static,
    {
        self.run_target_with(artifacts, target, handlers, ())
    }

    /// As [`run_target`](Self::run_target) but with an explicit user context `U`.
    pub fn run_target_with<U, H>(
        &self,
        artifacts: &CompiledArtifacts,
        target: &str,
        handlers: H,
        user: U,
    ) -> TargetOutcome
    where
        U: Send + 'static,
        H: DispatchEffect<U> + Send + 'static,
    {
        self.run_target_with_owned(artifacts, target, handlers, user)
            .0
    }

    /// As [`run_target`](Self::run_target) but hands `handlers` back
    /// alongside the [`TargetOutcome`] — for dispatchers whose post-eval
    /// state (write counts, stored files, recorded calls) IS the assertion.
    pub fn run_target_owned<H>(
        &self,
        artifacts: &CompiledArtifacts,
        target: &str,
        handlers: H,
    ) -> (TargetOutcome, H)
    where
        H: DispatchEffect<()> + Send + 'static,
    {
        self.run_target_with_owned(artifacts, target, handlers, ())
    }

    /// As [`run_target_with`](Self::run_target_with) but hands `handlers`
    /// back alongside the [`TargetOutcome`].
    pub fn run_target_with_owned<U, H>(
        &self,
        artifacts: &CompiledArtifacts,
        target: &str,
        mut handlers: H,
        user: U,
    ) -> (TargetOutcome, H)
    where
        U: Send + 'static,
        H: DispatchEffect<U> + Send + 'static,
    {
        let expr = artifacts
            .targets
            .get(target)
            .unwrap_or_else(|| panic!("compile_many did not produce target {target:?}"))
            .expr
            .clone();
        let mut table = artifacts.table.clone();
        let has_io = artifacts.warnings.has_io;
        let nursery = self.nursery.unwrap_or(DEFAULT_NURSERY_SIZE);
        let (result, handlers) = with_eval_stack(move || {
            let result: Result<(Value, DataConTable), RuntimeError> = (|| {
                if has_io {
                    return Err(RuntimeError::Compile(CompileError::IOTypeDetected));
                }
                table.populate_siblings_from_expr(&expr);
                let mut machine = JitEffectMachine::compile(&expr, &table, nursery)?;
                let value = machine.run(&table, &mut handlers, &user)?;
                Ok((value, table))
            })();
            (result, handlers)
        });
        (TargetOutcome(result), handlers)
    }

    /// Run one target out of a [`compile_many`](Self::compile_many) bundle as
    /// a PURE (non-`Eff`) expression — the multi-target sibling of
    /// [`run_pure`](Self::run_pure). Skips freer-simple effect dispatch
    /// entirely (`JitEffectMachine::run_pure`, mirroring
    /// `compile_and_run_pure`'s single-target path): a target compiled
    /// through [`compile_many`]/`compile_targets` carries no freer-simple
    /// `Val`/`Pure` wrapper for a plain (non-`Eff`) binding, so running it
    /// through the effectful [`run_target`](Self::run_target) path fails with
    /// "missing freer-simple constructor 'Val'" — this is the correct entry
    /// point for a pure check-list/family-bundle target.
    pub fn run_target_pure(&self, artifacts: &CompiledArtifacts, target: &str) -> TargetOutcome {
        let expr = artifacts
            .targets
            .get(target)
            .unwrap_or_else(|| panic!("compile_many did not produce target {target:?}"))
            .expr
            .clone();
        let mut table = artifacts.table.clone();
        let has_io = artifacts.warnings.has_io;
        let nursery = self.nursery.unwrap_or(DEFAULT_NURSERY_SIZE);
        let result = with_eval_stack(move || {
            (|| {
                if has_io {
                    return Err(RuntimeError::Compile(CompileError::IOTypeDetected));
                }
                table.populate_siblings_from_expr(&expr);
                let mut machine = JitEffectMachine::compile(&expr, &table, nursery)?;
                let value = machine.run_pure()?;
                Ok((value, table))
            })()
        });
        TargetOutcome(result)
    }
}

/// The base MCP effect stack (Console, KV, FsRead, FsWrite, Http, Exec, Llm, Git, Time,
/// Entropy, Ask, RunLLMTurn) as a hand-maintained GADT preamble, plus matching
/// stub handlers and a ready-made [`mock::min_stack`] `frunk` HList.
///
/// The GADT preamble text and stub handlers below are a STATIC mirror of the
/// real stack, not a derivation — it exists so callers can compile a
/// self-contained module without wiring `with_effects_module()`/
/// `Tidepool.Orchestrate`. Because *those* are hand-maintained, they CAN
/// drift from `tidepool_mcp::standard_decls()` (that's exactly what happened
/// when the SG effect was cut and Lsp/Time were added — see f1a480e6). This
/// preamble text/`min_stack()`'s HList still declare `Fork` (tag 11) as a
/// KNOWN, intentional divergence from `standard_decls()` (vestigial-
/// subsystems review §4 narrowed `standard_decls()` to the ordinary
/// one-shot/REPL roster, which never services `Fork`) — several tests
/// dispatch `Fork` through this mock stack directly (not through the session
/// engine's suspend/resume parser, so the gap the review names doesn't apply
/// here) and still need it declared; shrinking this hand-maintained mirror is
/// a separate migration, not required by that fix.
/// [`EFFECT_NAMES`] itself is no longer part of that hand-maintained surface:
/// it's computed straight from `tidepool_mcp::standard_decls()` (this crate
/// already depends on `tidepool-mcp` as a normal dependency), so it cannot
/// independently drift, and (as of the `Fork` narrowing above) is one effect
/// SHORTER than this module's own hand-maintained preamble/`min_stack()`.
/// `mock_stack_matches_production`
/// (`tidepool-runtime/tests/effect_stack/mock_stack_lockstep.rs`) still pins
/// it against `tidepool_mcp::standard_decls()` as a regression guard against
/// a future hand-maintained list creeping back in.
pub mod mock {
    use std::collections::HashMap;
    use std::sync::LazyLock;

    use tidepool_bridge_derive::FromCore;
    use tidepool_bridge_effects::{FileMeta, Proc};
    use tidepool_effect::{EffectContext, EffectError, EffectHandler, Response};
    use tidepool_eval::value::Value;

    /// The base MCP effect names, in stack order — derived directly from
    /// `tidepool_mcp::standard_decls()`, not hand-copied, so this list cannot
    /// drift from production on its own. `mock_stack_matches_production`
    /// pins it against a second, independent call to the same function.
    pub static EFFECT_NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        tidepool_mcp::standard_decls()
            .into_iter()
            .map(|d| d.type_name)
            .collect()
    });

    /// The standard MCP module preamble: LANGUAGE pragmas, `module Expr`, the
    /// common imports, the base-stack GADT declarations, and `type M = Eff '[…]`.
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
-- `HttpError`/`GitError`/`LlmError` (and `FsError`, never declared here) are
-- NOT hand-declared: they live in the stable `Tidepool.Records.Stable` module
-- (`stable_errors true`, effect_defs.rs) and arrive already in scope via
-- `import Tidepool.Prelude` above — a duplicate inline decl here would
-- conflict with that import instead of merely drifting from it.
data ExecError = ExecSpawn Text | ExecBadDir Text deriving (Show, Eq)

data Console a where
  Print :: Text -> Console ()
data KV a where
  KvGet :: Text -> KV (Maybe Value)
  KvSet :: Text -> Value -> KV ()
  KvDelete :: Text -> KV ()
  KvKeys :: KV [Text]
data FsRead a where
  FsRead :: Text -> FsRead Text
  FsListDir :: Text -> FsRead [Text]
  FsGlob :: Text -> FsRead [Text]
  FsExists :: Text -> FsRead Bool
  FsMetadata :: Text -> FsRead (Maybe FileMeta)
data FsWrite a where
  FsWrite :: Text -> Text -> FsWrite ()
data Http a where
  HttpGet :: Text -> Http (Either HttpError Value)
  HttpPost :: Text -> Value -> Http (Either HttpError Value)
  HttpRequest :: Text -> Text -> [(Text,Text)] -> Text -> Http Value
data Exec a where
  Run :: Text -> Exec (Either ExecError Proc)
  RunIn :: Text -> Text -> Exec (Either ExecError Proc)
  RunJson :: Text -> Exec Value
data Git a where
  GitLog :: Int -> Git (Either GitError [Value])
  GitStatus :: Git (Either GitError [Value])
  GitDiffStat :: Text -> Git (Either GitError [Value])
  GitShow :: Text -> Git (Either GitError Commit)
data Llm a where
  LlmChat :: Text -> Llm Text
  LlmStructured :: Text -> Value -> Llm (Either LlmError Value)
data Time a where
  TimeNow :: Time Int
data Entropy a where
  EntropySeed :: Entropy Int
data Ask a where
  Ask :: Text -> Ask Value
data RunLLMTurn a where
  RunLLMTurnStub :: Text -> RunLLMTurn Value
data Fork a where
  ForkWith :: Int -> Text -> Fork Value
  ForkAllWith :: Int -> [Text] -> Fork Value

type M = Eff '[Console, KV, FsRead, FsWrite, Http, Exec, Llm, Git, Time, Entropy, Ask, RunLLMTurn, Fork]
"#;

    /// [`MCP_PREAMBLE`] followed by `body` (your helper defs + `result`). The
    /// standard way to build a source string for the 13-effect stack.
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

    // 2: FsRead (stub)
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum FsReadReq {
        #[core(name = "FsRead")]
        FsRead(String),
        #[core(name = "FsListDir")]
        FsListDir(String),
        #[core(name = "FsGlob")]
        FsGlob(String),
        #[core(name = "FsExists")]
        FsExists(String),
        #[core(name = "FsMetadata")]
        FsMetadata(String),
    }
    pub struct MockFsRead;
    impl EffectHandler for MockFsRead {
        type Request = FsReadReq;
        fn handle(&mut self, req: FsReadReq, cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                FsReadReq::FsRead(_) => cx.respond(String::new()),
                FsReadReq::FsListDir(_) | FsReadReq::FsGlob(_) => {
                    let empty: Vec<String> = vec![];
                    cx.respond(empty)
                }
                FsReadReq::FsExists(_) => cx.respond(false),
                FsReadReq::FsMetadata(_) => cx.respond(Some(FileMeta {
                    size: 0,
                    is_file: false,
                    is_dir: false,
                })),
            }
        }
    }

    // 3: FsWrite (stub)
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum FsWriteReq {
        #[core(name = "FsWrite")]
        FsWrite(String, String),
    }
    pub struct MockFsWrite;
    impl EffectHandler for MockFsWrite {
        type Request = FsWriteReq;
        fn handle(
            &mut self,
            _req: FsWriteReq,
            cx: &EffectContext,
        ) -> Result<Response, EffectError> {
            cx.respond(())
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

    // 7: Git (stub)
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum GitReq {
        #[core(name = "GitLog")]
        GitLog(i64),
        #[core(name = "GitStatus")]
        GitStatus,
        #[core(name = "GitDiffStat")]
        GitDiffStat(String),
        #[core(name = "GitShow")]
        GitShow(String),
    }
    pub struct MockGit;
    impl EffectHandler for MockGit {
        type Request = GitReq;
        fn handle(&mut self, req: GitReq, cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                GitReq::GitLog(_) | GitReq::GitStatus | GitReq::GitDiffStat(_) => {
                    let empty: Vec<Value> = vec![];
                    cx.respond(Ok::<Vec<Value>, String>(empty))
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

    // 6: Llm (stub)
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

    // 8: Time (stub)
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum TimeReq {
        #[core(name = "TimeNow")]
        TimeNow,
    }
    pub struct MockTime;
    impl EffectHandler for MockTime {
        type Request = TimeReq;
        fn handle(&mut self, req: TimeReq, cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                TimeReq::TimeNow => cx.respond(0i64),
            }
        }
    }

    // 9: Entropy (stub — fixed deterministic seed, never real OS entropy).
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum EntropyReq {
        #[core(name = "EntropySeed")]
        EntropySeed,
    }
    pub struct MockEntropy;
    impl EffectHandler for MockEntropy {
        type Request = EntropyReq;
        fn handle(&mut self, req: EntropyReq, cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                EntropyReq::EntropySeed => cx.respond(42i64),
            }
        }
    }

    // 10: Ask (stub)
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

    // 11: RunLLMTurn (stub — self-iterating-harness WS-B split this out of
    // Ask; this mock harness dispatches every tag through the handler HList
    // (no suspend-tag threshold), so it needs its own stub same as MockAsk).
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum RunLLMTurnReq {
        #[core(name = "RunLLMTurnStub")]
        RunLLMTurnStub(String),
    }
    pub struct MockRunLLMTurn;
    impl EffectHandler for MockRunLLMTurn {
        type Request = RunLLMTurnReq;
        fn handle(
            &mut self,
            _req: RunLLMTurnReq,
            cx: &EffectContext,
        ) -> Result<Response, EffectError> {
            cx.respond(serde_json::json!("stub_response"))
        }
    }

    // 12: Fork (stub — the answerer parallel-delegation effect; same
    // dispatch-every-tag reasoning as MockRunLLMTurn).
    #[derive(FromCore)]
    #[allow(dead_code)]
    pub enum ForkReq {
        #[core(name = "ForkWith")]
        ForkWith(i64, String),
        #[core(name = "ForkAllWith")]
        ForkAllWith(i64, Vec<String>),
    }
    pub struct MockFork;
    impl EffectHandler for MockFork {
        type Request = ForkReq;
        fn handle(&mut self, _req: ForkReq, cx: &EffectContext) -> Result<Response, EffectError> {
            cx.respond(serde_json::json!("stub_response"))
        }
    }

    /// The base-stack mock handler HList, in stack order (matches
    /// [`EFFECT_NAMES`]) `[Console, KV, FsRead, FsWrite, Http, Exec, Llm, Git, Time,
    /// Entropy, Ask, RunLLMTurn, Fork]` — pass straight to
    /// [`super::EvalHarness::run`].
    pub fn min_stack() -> frunk::HList!(
        MockConsole,
        MockKv,
        MockFsRead,
        MockFsWrite,
        MockHttp,
        MockExec,
        MockLlm,
        MockGit,
        MockTime,
        MockEntropy,
        MockAsk,
        MockRunLLMTurn,
        MockFork
    ) {
        frunk::hlist![
            MockConsole,
            MockKv::new(),
            MockFsRead,
            MockFsWrite,
            MockHttp,
            MockExec,
            MockLlm,
            MockGit,
            MockTime,
            MockEntropy,
            MockAsk,
            MockRunLLMTurn,
            MockFork
        ]
    }
}
