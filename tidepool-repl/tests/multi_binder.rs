//! Wave-3b hardening — DIMENSION: multi-binder / pattern binds.
//!
//! Drives the REAL `tidepool-repl` MCP entry point over real turns (per the
//! standing rule — no bespoke wiring) and probes what happens when a single
//! bind statement introduces MORE THAN ONE name: a tuple bind `(a, b) <- e`,
//! a `let`-tuple `let (x, y) = e`, a three-tuple, etc.
//!
//! HISTORY: the original dispatcher silently kept only the FIRST binder of a
//! turn (`binders.first()` / `pure {binder}`), so `(a, b) <- pure (1, 2)` bound
//! only `a` (=1, the first *component*, not the tuple) and dropped `b` with no
//! signal. That footgun was replaced (session.rs::run_eval ~L192) with LOUD
//! REJECTION: a bind introducing >1 name returns a clean error naming the
//! dropped components, and the session stays usable. Full N-name value-plane
//! support (root each component) is a tracked FEATURE — captured by the single
//! `#[ignore]` test below so we don't lose the intent.
//!
//! Each test guards on `extract_available()` and skips cleanly otherwise.
//! Ignore stderr noise like `Could not find module …Val.G…` (unrelated
//! reference-path fallback).

mod common;
use common::*;

/// CASE 1 — `let` single bind (control).
/// `let x = (5 :: Int)` then `x + 1` => 6. Pins down that the single-binder
/// `let` path is healthy (the loud-rejection guard must NOT catch single binds).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn let_single_bind() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.eval("let x = (5 :: Int)").await;
    t.expect_ok("let x = 5");

    let t = repl.eval("x + 1").await;
    let out = t.expect_ok("x + 1");
    assert!(out.contains("6"), "let-single: expected 6, got: {out}");

    repl.close().await;
}

/// CASE 2 — Tuple bind is LOUDLY REJECTED, and the session survives.
/// `(a, b) <- pure ((1 :: Int), (2 :: Int))` must ERROR with a message naming
/// the dropped components (a, b) — NOT silently bind only `a`. A following good
/// turn must then succeed, proving the rejection left the session usable and
/// that no partial binding leaked into scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tuple_bind_rejected_loudly() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let bind = repl.eval("(a, b) <- pure ((1 :: Int), (2 :: Int))").await;
    eprintln!("[tuple_bind] bind turn -> is_error={} text={}", bind.is_error, bind.text);
    let err = bind.expect_err("tuple bind should be rejected loudly");
    assert!(
        err.contains("multi-binder"),
        "rejection should mention multi-binder support, got: {err}"
    );
    assert!(
        err.contains("2 names") && err.contains("a, b"),
        "rejection should name the dropped components (a, b), got: {err}"
    );

    // Neither component leaked into scope.
    let listing = repl.cmd(":bindings").await;
    let out = listing.expect_ok(":bindings after rejected tuple bind");
    eprintln!("[tuple_bind] :bindings after reject -> {out}");
    assert!(
        !out.contains("\"a\"") && !out.contains("\"b\""),
        "rejected tuple bind must leave NOTHING bound, got: {out}"
    );

    // Session survived: a fresh good bind + reference works.
    repl.eval("let z = (9 :: Int)").await.expect_ok("let z after reject");
    let t = repl.eval("z").await;
    let zout = t.expect_ok("z after reject");
    assert!(zout.contains("9"), "post-reject z: expected 9, got: {zout}");

    repl.close().await;
}

/// CASE 3 — `let`-tuple is LOUDLY REJECTED, session survives.
/// `let (x, y) = ((10 :: Int), (20 :: Int))` must error naming x, y (the
/// `let`-tuple path is NOT exempt from the guard), and a later good turn works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn let_tuple_rejected_loudly() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let bind = repl.eval("let (x, y) = ((10 :: Int), (20 :: Int))").await;
    eprintln!("[let_tuple] bind turn -> is_error={} text={}", bind.is_error, bind.text);
    let err = bind.expect_err("let-tuple should be rejected loudly");
    assert!(err.contains("multi-binder"), "should mention multi-binder, got: {err}");
    assert!(
        err.contains("2 names") && err.contains("x, y"),
        "should name x, y, got: {err}"
    );

    // Session survived.
    repl.eval("let w = (7 :: Int)").await.expect_ok("let w after reject");
    let t = repl.eval("w").await;
    assert!(t.expect_ok("w after reject").contains("7"), "post-reject w: {}", t.text);

    repl.close().await;
}

/// CASE 4 — Three-tuple is LOUDLY REJECTED with all three names, session survives.
/// `(p, q, r) <- pure ((1::Int),(2::Int),(3::Int))` → error naming p, q, r
/// ("3 names"). Confirms the guard is arity-independent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_tuple_rejected_loudly() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let bind = repl
        .eval("(p, q, r) <- pure ((1::Int),(2::Int),(3::Int))")
        .await;
    eprintln!("[three_tuple] bind turn -> is_error={} text={}", bind.is_error, bind.text);
    let err = bind.expect_err("three-tuple should be rejected loudly");
    assert!(err.contains("multi-binder"), "should mention multi-binder, got: {err}");
    assert!(
        err.contains("3 names") && err.contains("p, q, r"),
        "should name all three (p, q, r), got: {err}"
    );

    // Session survived.
    repl.eval("let s = (5 :: Int)").await.expect_ok("let s after reject");
    let t = repl.eval("s").await;
    assert!(t.expect_ok("s after reject").contains("5"), "post-reject s: {}", t.text);

    repl.close().await;
}

/// CASE 5 — FEATURE (aspirational, ignored): full multi-binder value-plane
/// support. When each component is rooted individually, `(a, b) <- pure (1, 2)`
/// would bind BOTH and `a + b` would yield 3. Captured so the intent isn't lost
/// behind the current loud rejection. Un-ignore when the feature lands.
#[ignore = "FEATURE: full multi-binder value-plane support (root each component)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tuple_bind_both_components_feature() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("(a, b) <- pure ((1 :: Int), (2 :: Int))")
        .await
        .expect_ok("tuple bind (feature)");

    let t = repl.eval("a + b").await;
    let out = t.expect_ok("a + b (feature)");
    assert!(out.contains("3"), "feature: expected a + b == 3, got: {out}");

    repl.close().await;
}
