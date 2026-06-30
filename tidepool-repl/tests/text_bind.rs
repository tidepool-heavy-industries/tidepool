//! Regression suite for the Tier-0 **Text** value-bind crash (BUG-2, FIXED).
//!
//! ## What the bug was
//! With ANY value binding live, the session REFERENCE/bind path crashed with
//!   `[JIT] runtime_error kind=4 (TypeMetadata)`
//!   → `yield error: forced type metadata (should be dead code)`
//! often preceded on stderr by `Could not find module 'Tidepool.Session.Val.G1'`.
//!
//! ## Root cause (fixed in GhcPipeline.runSessionPipeline)
//! The session extract compiled ONLY the turn target and resolved every
//! home-library function it called (`T.pack`/`T.toUpper`, `object`, `.=`,
//! `$fToJSONInt`, …) from their HPT interface unfoldings. `load'` provisions
//! those ifaces without -O2 unfoldings, so `resolveExternals` could not inline
//! them → it baked a poison ErrorSentinel for each, and `translateModuleClosed`'s
//! `trulyUnresolved` filter hid the failure → the sentinel fired at run as
//! kind=4. (The separate "Could not find module Val.G" stderr came from `load'`
//! compiling the target before the Val iface was injected.) The fix recompiles
//! every home module to full -O2 guts (the one-shot path's approach) and excludes
//! the target from the phase-1 `load'`; no library function is left unresolved.
//!
//! The controls below pin the boundary: `box_second_bind_replica` (no library
//! deps) always worked; `text_bind_headline_faithful` (Text second bind) and the
//! Eff-reference repros now pass with full value correctness (length / unpack /
//! toUpper / append), doubling as the acceptance gate.

mod common;
use common::*;

// ───────────────────────── PASSING CONTROLS / GUARDS ─────────────────────────

/// A Text bind as the ONLY binding works (no prior binding ⇒ no Val injection).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_bind_alone_control() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.eval("s <- pure (T.pack \"hi\")").await;
    assert!(
        t.expect_ok("bind s").contains("bound"),
        "bind s: {}",
        t.text
    );

    let t = repl.eval("T.length s").await;
    assert!(
        t.expect_ok("T.length s").contains("2"),
        "T.length s: {}",
        t.text
    );

    let t = repl.eval("T.unpack s").await;
    assert!(
        t.expect_ok("T.unpack s").contains("hi"),
        "T.unpack s: {}",
        t.text
    );

    repl.close().await.expect_ok("close");
}

/// DECISIVE CONTROL: byte-for-byte the GREEN headline turn sequence through a
/// SECOND bind, but of a Box (no library deps) — PASSES. The only difference
/// from `text_bind_headline_faithful` is Box vs `T.pack`; before the fix that
/// pinned the bug to library-function resolution, not a generic second-bind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn box_second_bind_replica() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;
    repl.def("data Box = Box Int").await.expect_ok("def Box");

    let t = repl.eval("x <- pure (42 :: Int)").await;
    assert!(
        t.expect_ok("bind x").contains("bound"),
        "bind x: {}",
        t.text
    );
    let t = repl.eval("x + 1").await;
    assert!(t.expect_ok("read x").contains("43"), "read x: {}", t.text);
    let t = repl.eval("b <- pure (Box 7)").await;
    assert!(
        t.expect_ok("bind b").contains("bound"),
        "bind b: {}",
        t.text
    );
    let t = repl.eval("case b of Box n -> n + 100").await;
    assert!(t.expect_ok("case b").contains("107"), "case b: {}", t.text);

    repl.close().await.expect_ok("close");
}

/// MINIMAL repro of the dominant kind=4 bug (now FIXED): with ANY binding live,
/// the Eff reference path used to crash regardless of the expression — even one
/// that ignores the binding. Now yields the value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eff_ref_pure_const_with_binding_live() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.eval("x <- pure (1 :: Int)").await;
    assert!(
        t.expect_ok("bind x").contains("bound"),
        "bind x: {}",
        t.text
    );

    // Eff reference run that does NOT touch x — should be 123 (was kind=4 before the fix).
    let t = repl.eval("pure (123 :: Int)").await;
    assert!(
        t.expect_ok("pure 123").contains("123"),
        "pure 123: {}",
        t.text
    );

    repl.close().await.expect_ok("close");
}

/// CONTROL: the SAME Eff reference run with NO binding live works (plain path).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eff_ref_pure_const_no_binding_control() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.eval("pure (123 :: Int)").await;
    assert!(
        t.expect_ok("pure 123").contains("123"),
        "pure 123: {}",
        t.text
    );

    repl.close().await.expect_ok("close");
}

// ───────────── REGRESSION GATES (were the open BUG-2 repros) ─────────────

/// DECISIVE repro: identical to `box_second_bind_replica` except the second
/// bind is a Text. Was: "Could not find module Val.G1" + kind=4; now passes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_bind_headline_faithful() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;
    repl.def("data Box = Box Int").await.expect_ok("def Box");

    let t = repl.eval("x <- pure (42 :: Int)").await;
    assert!(
        t.expect_ok("bind x").contains("bound"),
        "bind x: {}",
        t.text
    );
    let t = repl.eval("x + 1").await;
    assert!(t.expect_ok("read x").contains("43"), "read x: {}", t.text);

    // The library-dep (Data.Text) second bind that triggers the downsweep loss.
    let t = repl.eval("y <- pure (T.pack \"hi\")").await;
    assert!(
        t.expect_ok("bind y").contains("bound"),
        "bind y: {}",
        t.text
    );

    // Full VALUE-CORRECTNESS gate for the eventual fix (length/unpack/toUpper/append).
    let t = repl.eval("T.length y").await;
    assert!(
        t.expect_ok("T.length y").contains("2"),
        "T.length y: {}",
        t.text
    );
    let t = repl.eval("T.unpack y").await;
    assert!(
        t.expect_ok("T.unpack y").contains("hi"),
        "T.unpack y: {}",
        t.text
    );
    let t = repl.eval("pure (T.unpack (T.toUpper y))").await;
    assert!(
        t.expect_ok("toUpper y").contains("HI"),
        "toUpper y: {}",
        t.text
    );
    let t = repl.eval("T.unpack (T.append y (T.pack \"!\"))").await;
    assert!(
        t.expect_ok("append y").contains("hi!"),
        "append y: {}",
        t.text
    );

    repl.close().await.expect_ok("close");
}

/// Same-name rebind: `x <- Int` then `x <- Text`. Same root cause (fixed).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_rebind_same_name() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.eval("x <- pure (1 :: Int)").await;
    assert!(
        t.expect_ok("bind x int").contains("bound"),
        "bind x int: {}",
        t.text
    );
    let t = repl.eval("x + 1").await; // read between (headline-style)
    assert!(t.expect_ok("read x").contains("2"), "read x: {}", t.text);

    let t = repl.eval("x <- pure (T.pack \"hi\")").await;
    assert!(
        t.expect_ok("rebind x text").contains("bound"),
        "rebind x: {}",
        t.text
    );

    let t = repl.eval("T.length x").await;
    assert!(
        t.expect_ok("T.length x").contains("2"),
        "T.length x: {}",
        t.text
    );
    let t = repl.eval("T.unpack x").await;
    assert!(
        t.expect_ok("T.unpack x").contains("hi"),
        "T.unpack x: {}",
        t.text
    );

    repl.close().await.expect_ok("close");
}

/// Longer multibyte-capable Text as a second bind — same root cause (fixed);
/// the length/round-trip assertions are the value-correctness gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_bind_longer_with_prior() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.eval("n <- pure (0 :: Int)").await;
    assert!(
        t.expect_ok("bind n").contains("bound"),
        "bind n: {}",
        t.text
    );
    let t = repl.eval("n + 1").await;
    assert!(t.expect_ok("read n").contains("1"), "read n: {}", t.text);

    let t = repl.eval("y <- pure (T.pack \"hello world\")").await;
    assert!(
        t.expect_ok("bind y").contains("bound"),
        "bind y: {}",
        t.text
    );

    let t = repl.eval("T.length y").await;
    assert!(
        t.expect_ok("T.length y").contains("11"),
        "T.length y: {}",
        t.text
    );
    let t = repl.eval("T.unpack y").await;
    assert!(
        t.expect_ok("T.unpack y").contains("hello world"),
        "T.unpack y: {}",
        t.text
    );

    repl.close().await.expect_ok("close");
}
