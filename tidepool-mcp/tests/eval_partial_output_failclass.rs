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

    let captured = CapturedOutput::new();
    let captured_for_thread = captured.clone();

    // Run the eval on a worker thread with a LARGE stack and a HARD TIMEOUT.
    // - Large stack: a deep non-tail recursion reaches the JIT's call-depth
    //   yield (~20k frames) only if the native stack holds that many frames
    //   first; a default test-thread stack would native-overflow (SIGSEGV)
    //   before the clean yield. The server uses the same 256 MiB for eval.
    // - Timeout: compile_and_run blocks until the eval finishes. A snippet that
    //   LOOPS instead of crashing (e.g. a no-base-case `n + go (n+1)`, which GHC
    //   loopifies into a non-stack-growing spin) would otherwise hang the whole
    //   test binary forever. The timeout turns that into a fast, explicit panic.
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("eval-failclass".into())
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let include = [pp, ulp, eff];
            let mut handlers = frunk::hlist![ConsoleHandler];
            let err = compile_and_run(&source, "result", &include, &mut handlers, &captured_for_thread)
                .expect_err("eval was expected to fail")
                .to_string();
            let _ = tx.send(err);
        })
        .expect("spawn eval-failclass thread");

    let err = match rx.recv_timeout(std::time::Duration::from_secs(90)) {
        Ok(err) => err,
        Err(_) => panic!(
            "eval did not terminate within 90s — a failing snippet MUST crash \
             (stack overflow / Haskell error), not loop. A no-base-case non-tail \
             recursion compiles to a spinning loop; give the recursion a base case."
        ),
    };
    let _ = handle.join();
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
///
/// IGNORED pending a real codegen bug found while fixing this test's prior
/// infinite hang: `send (Print x) >> (pure $! <deep computation>)` resolves
/// `Print` to an unresolved variable (VarId 0xfe75…) at runtime, even though
/// `pure $! (go N)` alone yields cleanly and `run`/other effects work. The
/// worker-thread timeout in `run_capturing_expect_err` already removes the
/// hang; un-ignore once the Print/effect-seq resolution bug is fixed.
/// See plans/send-print-unresolved-bug.md.
#[ignore = "blocked on send(Print) >> (pure $! ...) unresolved-variable codegen bug; see plans/send-print-unresolved-bug.md"]
#[test]
fn say_then_stack_overflow_is_captured_and_classified() {
    let marker = "PARTIAL-OUTPUT-MARKER-yield";
    // `go` is non-tail (the `n +` keeps a live frame) and recurses far past the
    // JIT's call-depth budget before its base case, so `$!` forcing it to WHNF —
    // after the Print — blows the call stack → clean stack-overflow yield.
    // IMPORTANT: it MUST have a base case. A no-base-case `n + go (n + 1)` is
    // loopified by GHC into a non-stack-growing spin (it never overflows) and
    // would hang this test forever — see the worker-thread timeout above.
    // (Recursive helpers must live in the top-level `helpers` slot, not a
    // `where` on the eval expression, or the self-call resolves to an
    // unresolved external.)
    let (output, err) = run_capturing_expect_err(
        &format!(r#"send (Print (T.pack "{marker}")) >> (pure $! (go 5000000 :: Int))"#),
        "go :: Int -> Int\ngo n = if n <= 0 then 0 else n + go (n - 1)\n",
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
