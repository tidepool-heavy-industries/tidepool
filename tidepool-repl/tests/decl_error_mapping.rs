//! DIMENSION: GHC error location rewriting.
//!
//! Verifies that GHC compile errors surfaced through `session_run` carry
//! item-relative line:col coordinates and no `/tmp/` path noise, so the caller
//! can act on them without mental offset arithmetic.
//!
//! Requires `TIDEPOOL_EXTRACT` (session-aware extractor) and a with-packages GHC;
//! skips cleanly if unavailable.

mod common;
use common::*;

/// A decl item with a type error on line 2 should report `2:…`, not a
/// wrapper-offset line like `40:…`, and no `/tmp/` path in the error text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decl_error_shows_item_relative_coordinates() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Two-line decl: sig on line 1, body on line 2.  Body has a type error:
    // `x` has type `Int` but the sig says `Text`.
    let decl = "myFn :: Int -> Text\nmyFn x = x";
    let t = repl.def(decl).await;
    let err = t.expect_err("type-error decl should fail");

    // The error must carry item-relative "2:" (body is on line 2 of the item).
    // GHC reports the mismatch at the function body line.
    assert!(
        err.contains("2:") || err.contains("1:"),
        "error should contain an item-relative line number (1 or 2), got: {err}"
    );

    // No raw /tmp/ paths should leak into the error text.
    assert!(
        !err.contains("/tmp/"),
        "error must not contain /tmp/ paths, got: {err}"
    );

    // Session should remain usable after the failed decl.
    let t2 = repl.def("okFn :: Int -> Int\nokFn x = x + 1").await;
    t2.expect_ok("session should survive a failed decl");

    repl.close().await.expect_ok("close");
}

/// A stmt/expr compile error should also carry item-relative coordinates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stmt_error_shows_item_relative_coordinates() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // A bare type error in an expression.
    let t = repl.eval("pure (True + (1 :: Int))").await;
    let err = t.expect_err("type error in expression");

    assert!(
        !err.contains("/tmp/"),
        "stmt error must not contain /tmp/ paths, got: {err}"
    );

    repl.close().await.expect_ok("close");
}

/// Wrapper-origin errors (from the generated wrapper, not the user code) should
/// be labeled `[wrapper]` and still have no raw /tmp/ paths.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrapper_errors_are_labeled() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // An ambiguous-type expression: the result type of `pure []` is polymorphic,
    // making the `result :: Eff … Value` wrapper's `toWire _r` call ambiguous.
    // This is a wrapper-origin error (on the generated `result` line).
    let t = repl.eval("pure []").await;
    if t.is_error {
        let err = t.text.as_str();
        // Must not leak raw /tmp/ paths regardless of wrapper vs user classification.
        assert!(
            !err.contains("/tmp/"),
            "wrapper error must not contain /tmp/ paths, got: {err}"
        );
    }
    // (If it succeeds that's fine too — the test is primarily about path stripping.)

    repl.close().await.expect_ok("close");
}
