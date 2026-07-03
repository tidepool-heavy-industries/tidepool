//! Issue #331: `MonadFail (Eff effs)` — refutable do-binds (`Just x <- e`) must
//! compile AND run through the real eval pipeline.
//!
//! The instance lives in the generated `Tidepool.Effects` module
//! (`effects_module_source`) as `instance MonadFail (Eff effs) where
//! fail = error . T.pack`, routing GHC's desugared do-block pattern-match
//! failure through our JIT-safe `error` — the SAME abort path as `error`.
//!
//! This suite drives the FULL server pipeline (standard_decls → build_preamble
//! → template_haskell → ensure_effects_module → compile_and_run), mirroring
//! `text_breakon_replace_mcp.rs`:
//!   1. a refutable bind whose match SUCCEEDS binds and continues, and
//!   2. a refutable bind whose match FAILS aborts with a CLEAN Haskell error
//!      (not a SIGSEGV/hang), classified as `haskell-error`.

use serde_json::json;
use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_mcp::FailureClass;
use tidepool_runtime::compile_and_run;

fn prelude_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("haskell/lib")
        .leak()
}

fn user_lib_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".tidepool/lib")
        .leak()
}

struct MockDispatcher;

impl DispatchEffect<()> for MockDispatcher {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        Err(tidepool_effect::error::EffectError::UnhandledEffect { tag })
    }
}

/// Run `code` through the real MCP pipeline. `Ok` → the result JSON;
/// `Err` → the runtime error string (the eval aborted).
fn run_mcp(code: &str) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", "", None, None);

    let pp = prelude_dir();
    let ulp = user_lib_dir();
    assert!(
        ulp.join("Library.hs").exists(),
        ".tidepool/lib/Library.hs not found"
    );
    let eff = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .leak() as &Path;
    let include = [pp, ulp, eff];

    let mut dispatcher = MockDispatcher;
    compile_and_run(&source, "result", &include, &mut dispatcher, &())
        .map(|v| v.to_json())
        .map_err(|e| e.to_string())
}

/// A refutable bind whose match succeeds: `x` binds to 5 and the do-block
/// continues, so the eval returns 6. Proves `Just x <- e` COMPILES (needs the
/// `MonadFail` instance) and runs.
#[test]
fn refutable_bind_success_runs() {
    let code = "do\n  Just x <- pure (Just (5 :: Int))\n  pure (x + 1)";
    assert_eq!(run_mcp(code).expect("eval should succeed"), json!(6));
}

/// A refutable bind whose match FAILS: `Just x <- pure Nothing` desugars to a
/// `fail` call, which our instance routes through `error`. The eval must abort
/// with a CLEAN Haskell error (not SIGSEGV / hang), carrying the desugared
/// pattern-match-failure message, and classify as `haskell-error`.
#[test]
fn refutable_bind_failure_clean_error() {
    let code = "do\n  Just x <- pure (Nothing :: Maybe Int)\n  pure (x :: Int)";
    let err = run_mcp(code).expect_err("eval should abort on the failed bind");

    // Clean Haskell abort (routed through our `error`), NOT a codegen crash.
    let class = FailureClass::classify_error_text(&err);
    assert_eq!(
        class,
        FailureClass::HaskellError,
        "expected a clean haskell-error, got {} (err: {err})",
        class.tag()
    );

    // The message is GHC's desugared do-block pattern-match failure text.
    assert!(
        err.contains("Pattern match failure"),
        "error should carry the desugared pattern-match-failure message; got: {err}"
    );
}
