//! End-to-end coverage for the failure-class tag + partial-output-on-failure
//! work (Phase 1 of the eval-ergonomics task).
//!
//! `handle_session_result` (where the tag is stamped and the captured output is
//! appended) is private to `tidepool-mcp`, and its wiring is covered by the
//! in-crate unit tests. THIS suite exercises the two halves on REAL runtime
//! data — a genuine eval that `say`s and then crashes through the real GHC +
//! JIT pipeline:
//!
//!   1. the `say` output really IS captured before the crash, and
//!   2. the REAL `RuntimeError` string classifies into the right
//!      `FailureClass` (so the tag the server stamps is correct).
//!
//! Together with the unit tests (which prove the server appends both), this
//! pins the full chain.
//!
//! Mirrors the harness in `text_breakon_replace_mcp.rs` (real `compile_and_run`
//! with the standard MCP preamble/effect stack), but with a Console handler
//! that captures `say` output into a `CapturedOutput`, exactly as the server's
//! `ConsoleHandler` does.

use std::path::Path;

use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_mcp::{CapturedOutput, FailureClass};
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

// Console is tag 0 in `standard_decls()`; a single-handler HList catches `say`
// (`send (Print t)`) and routes the text into the CapturedOutput state, exactly
// like the production `ConsoleHandler`. Any other effect falls through to HNil
// (UnhandledEffect) — these evals only print, so it is never reached.
#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

#[derive(Clone)]
struct ConsoleHandler;

impl EffectHandler<CapturedOutput> for ConsoleHandler {
    type Request = ConsoleReq;
    fn handle(
        &mut self,
        req: ConsoleReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            ConsoleReq::Print(s) => {
                cx.user().push(s);
                cx.respond(())
            }
        }
    }
}

/// Run `code` (with optional top-level `helpers`) through the real pipeline
/// with a capturing Console handler. Returns the (drained) captured output and
/// the runtime error string — the eval is expected to FAIL.
fn run_capturing_expect_err(code: &str, helpers: &str) -> (Vec<String>, String) {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", helpers, None, None);

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

    let captured = CapturedOutput::new();
    let mut handlers = frunk::hlist![ConsoleHandler];
    let err = compile_and_run(&source, "result", &include, &mut handlers, &captured)
        .expect_err("eval was expected to fail")
        .to_string();
    (captured.drain(), err)
}

/// A clean Haskell `error` after a `say`: the printed line survives, and the
/// error classifies as `haskell-error` (not a codegen bug).
#[test]
fn say_then_haskell_error_is_captured_and_classified() {
    let marker = "PARTIAL-OUTPUT-MARKER-haskell";
    // `send (Print ...)` fires only Console (tag 0); the `putStrLn` helper also
    // touches KV, which this single-handler HList does not handle.
    let (output, err) = run_capturing_expect_err(
        &format!(r#"send (Print (T.pack "{marker}")) >> (error (T.pack "boom") :: M Value)"#),
        "",
    );

    // 1. The pre-crash output is captured.
    assert!(
        output.iter().any(|l| l.contains(marker)),
        "captured output should contain the pre-crash marker; got {output:?}"
    );
    // 2. The REAL error string classifies as a clean Haskell error.
    let class = FailureClass::classify_error_text(&err);
    assert_eq!(
        class,
        FailureClass::HaskellError,
        "expected haskell-error for `error \"boom\"`, got {} (err: {err})",
        class.tag()
    );
}

/// Unbounded non-tail recursion after a `say`: the printed line survives, and
/// the stack-overflow yield classifies as `runtime-yield` (a user resource
/// problem, not a codegen bug).
#[test]
fn say_then_stack_overflow_is_captured_and_classified() {
    let marker = "PARTIAL-OUTPUT-MARKER-yield";
    // `go` is non-tail (the `n +` keeps a frame) and never terminates; `$!`
    // forces it to WHNF inside the M action — after the Print — so the JIT call
    // stack blows during execution → clean stack-overflow yield. (Recursive
    // helpers must live in the top-level `helpers` slot, not a `where` on the
    // eval expression, or the self-call resolves to an unresolved external.)
    let (output, err) = run_capturing_expect_err(
        &format!(r#"send (Print (T.pack "{marker}")) >> (pure $! (go 0 :: Int))"#),
        "go :: Int -> Int\ngo n = n + go (n + 1)\n",
    );

    assert!(
        output.iter().any(|l| l.contains(marker)),
        "captured output should contain the pre-crash marker; got {output:?}"
    );
    let class = FailureClass::classify_error_text(&err);
    assert_eq!(
        class,
        FailureClass::RuntimeYield,
        "expected runtime-yield for unbounded recursion, got {} (err: {err})",
        class.tag()
    );
}
