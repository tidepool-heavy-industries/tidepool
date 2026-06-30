//! Wave-3b hardening — DIMENSION: multi-binder / pattern binds.
//!
//! Drives the REAL `tidepool-repl` MCP entry point over real turns (per the
//! standing rule — no bespoke wiring) and probes what happens when a single
//! bind statement introduces MORE THAN ONE name: a tuple bind `(a, b) <- e`,
//! a `let`-tuple `let (x, y) = e`, a three-tuple, etc.
//!
//! BUG-5 SHIPS: flat-tuple multi-binder binds now root EACH component. A bind
//! `(a, b) <- pure (1, 2)` makes both `a` and `b` independently referenceable
//! and GC-safe. Nested tuple patterns also work (each variable is bound flat).
//!
//! REJECTION guard: patterns whose result type is not a matching tuple
//! (e.g. type errors, non-tuple constructors) still loudly reject via a GHC
//! compile error from the extract.
//!
//! Each test guards on `extract_available()` and skips cleanly otherwise.

mod common;
use common::*;

/// CASE 1 — `let` single bind (control).
/// `let x = (5 :: Int)` then `x + 1` => 6. Pins down that the single-binder
/// `let` path is healthy (multi-bind must NOT interfere with single binds).
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

/// CASE 2 — Tuple bind both components (the BUG-5 headline).
/// `(a, b) <- pure ((1 :: Int), (2 :: Int))` binds both `a` and `b`.
/// Both are independently referenceable; `a + b == 3`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tuple_bind_both_components() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let bind = repl.eval("(a, b) <- pure ((1 :: Int), (2 :: Int))").await;
    eprintln!(
        "[tuple_bind] bind turn -> is_error={} text={}",
        bind.is_error, bind.text
    );
    bind.expect_ok("tuple bind should succeed");

    // Both components are in scope.
    let listing = repl.cmd(":bindings").await;
    let out = listing.expect_ok(":bindings after tuple bind");
    eprintln!("[tuple_bind] :bindings -> {out}");
    assert!(
        out.contains("\"a\"") && out.contains("\"b\""),
        "both a and b must be bound, got: {out}"
    );

    // Components are independently referenceable.
    let ta = repl.eval("a").await;
    let aout = ta.expect_ok("a");
    assert!(aout.contains("1"), "a should be 1, got: {aout}");

    let tb = repl.eval("b").await;
    let bout = tb.expect_ok("b");
    assert!(bout.contains("2"), "b should be 2, got: {bout}");

    // Sum is correct.
    let t = repl.eval("a + b").await;
    let out = t.expect_ok("a + b");
    assert!(out.contains("3"), "expected a + b == 3, got: {out}");

    repl.close().await;
}

/// CASE 3 — `let`-tuple works.
/// `let (x, y) = ((10 :: Int), (20 :: Int))` binds both x and y;
/// `x + y == 30`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn let_tuple_works() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let bind = repl.eval("let (x, y) = ((10 :: Int), (20 :: Int))").await;
    eprintln!(
        "[let_tuple] bind turn -> is_error={} text={}",
        bind.is_error, bind.text
    );
    bind.expect_ok("let-tuple bind should succeed");

    let t = repl.eval("x + y").await;
    let out = t.expect_ok("x + y");
    assert!(out.contains("30"), "expected x + y == 30, got: {out}");

    repl.close().await;
}

/// CASE 4 — Three-tuple works.
/// `(p, q, r) <- pure ((1::Int),(2::Int),(3::Int))` binds all three;
/// `p + q + r == 6`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_tuple_works() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let bind = repl
        .eval("(p, q, r) <- pure ((1::Int),(2::Int),(3::Int))")
        .await;
    eprintln!(
        "[three_tuple] bind turn -> is_error={} text={}",
        bind.is_error, bind.text
    );
    bind.expect_ok("three-tuple bind should succeed");

    let t = repl.eval("p + q + r").await;
    let out = t.expect_ok("p + q + r");
    assert!(out.contains("6"), "expected p + q + r == 6, got: {out}");

    repl.close().await;
}

/// CASE 5 — The original feature test (was #[ignore], now active).
/// `(a, b) <- pure (1, 2)` then `a + b == 3`. Kept as an independent guard.
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
    assert!(
        out.contains("3"),
        "feature: expected a + b == 3, got: {out}"
    );

    repl.close().await;
}

/// CASE 6 — Tuple components survive an organic GC.
/// Binds `(a, b)`, then runs a heavy foldl' that allocates ~6 MiB into the
/// 2 MiB nursery (forcing real minor collections), then verifies `a + b` still
/// yields 3. This is the GC-rooting guard: if either slot dangled after
/// collection, `a + b` would crash or produce garbage instead of 3.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tuple_bind_components_survive_gc() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Bind the tuple.
    repl.eval("(a, b) <- pure ((1 :: Int), (2 :: Int))")
        .await
        .expect_ok("tuple bind");

    // Heavy allocation: foldl' over 200k elements allocates ~6 MiB of transient
    // cons cells into the 2 MiB nursery → multiple real minor collections.
    let _ = repl
        .eval("foldl' (+) (0 :: Int) [1..200000]")
        .await
        .expect_ok("gc stressor");

    // Both components must still resolve correctly post-GC.
    let t = repl.eval("a + b").await;
    let out = t.expect_ok("a + b post-GC");
    assert!(
        out.contains("3"),
        "post-GC: expected a + b == 3, got: {out}"
    );

    // Individual components also survive.
    let ta = repl.eval("a").await;
    assert!(
        ta.expect_ok("a post-GC").contains("1"),
        "post-GC a should be 1"
    );
    let tb = repl.eval("b").await;
    assert!(
        tb.expect_ok("b post-GC").contains("2"),
        "post-GC b should be 2"
    );

    repl.close().await;
}

/// CASE 7 — Type-mismatch multi-bind is LOUDLY REJECTED (GHC compile error).
/// `(a, b) <- pure (42 :: Int)` has 2 binders but the action returns a plain
/// `Int`, not a 2-tuple. GHC reports a type error at compile time — a loud,
/// clean rejection. The session must remain usable after the error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_type_rejected_loudly() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let bind = repl.eval("(a, b) <- pure (42 :: Int)").await;
    eprintln!(
        "[mismatch] bind turn -> is_error={} text={}",
        bind.is_error, bind.text
    );
    // Must error — can't match (a,b) pattern against Int.
    bind.expect_err("type-mismatch multi-bind should be rejected");

    // Neither variable leaked into scope.
    let listing = repl.cmd(":bindings").await;
    let out = listing.expect_ok(":bindings after rejected mismatch bind");
    assert!(
        !out.contains("\"a\"") && !out.contains("\"b\""),
        "rejected bind must leave nothing bound, got: {out}"
    );

    // Session survived: a fresh bind works.
    repl.eval("let z = (9 :: Int)")
        .await
        .expect_ok("let z after reject");
    let t = repl.eval("z").await;
    assert!(
        t.expect_ok("z after reject").contains("9"),
        "post-reject z: {}",
        t.text
    );

    repl.close().await;
}
