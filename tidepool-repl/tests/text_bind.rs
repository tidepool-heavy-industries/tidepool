//! Regression + characterization suite for the Tier-0 **Text** value-bind crash.
//!
//! ## What the bug actually is (characterized by this suite)
//! Binding a `Text` value as a SECOND bind (with a prior binding live) crashes:
//!   `[JIT] runtime_error kind=4 (TypeMetadata) msg="hi"`
//!   → `bind runtime error: yield error: forced type metadata (should be dead code)`
//! ALWAYS preceded on stderr by a GHC extract error:
//!   `Could not find module 'Tidepool.Session.Val.G1'`.
//!
//! The decisive controls below show it is **library-dependency-specific**, NOT a
//! generic second-bind problem:
//!   * `box_second_bind_replica` — def, x<-Int, read, **b<-Box 7** → PASSES.
//!   * `text_bind_headline_faithful` — IDENTICAL except **y<-T.pack "hi"** → FAILS.
//! Binding a value whose RHS pulls a library (`Data.Text`) triggers GHC's
//! depanal/downsweep, which drops the injected *source-less* `Val.G<g>` iface of
//! the prior binding from the module graph → the wrapped source's
//! `import Tidepool.Session.Val.G1` can't be found → the bind's Core is built
//! with a type-metadata ErrorSentinel that is applied at run/force time (kind=4).
//!
//! ## Why "Fix A" (deep_force tolerance) does NOT apply
//! The kind=4 error is raised during the EFFECTFUL fragment's JIT run inside
//! `run_fragment_and_bind` (the value cannot even be built), NOT while
//! NF-deep-forcing an already-built value. Tolerating kind=4 in `deep_force`
//! changes nothing (verified: the crash and stderr are byte-identical with the
//! tolerance applied). The real fix is in the extract (GhcPipeline / Session
//! iface injection across a library-dep downsweep). See the agent report.
//!
//! The `#[ignore]`d tests are live repros of the open bug — run with
//! `--ignored` once the extract fix lands; they assert full value correctness
//! (unpack / toUpper / append), so they double as the acceptance gate for B.

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
    assert!(t.expect_ok("bind s").contains("bound"), "bind s: {}", t.text);

    let t = repl.eval("T.length s").await;
    assert!(t.expect_ok("T.length s").contains("2"), "T.length s: {}", t.text);

    let t = repl.eval("T.unpack s").await;
    assert!(t.expect_ok("T.unpack s").contains("hi"), "T.unpack s: {}", t.text);

    repl.close().await.expect_ok("close");
}

/// DECISIVE CONTROL: byte-for-byte the GREEN headline turn sequence through a
/// SECOND bind, but of a Box (no library deps) — PASSES. The only difference
/// from `text_bind_headline_faithful` (which fails) is Box vs `T.pack`, proving
/// the bug is library-dependency-specific, not a generic second-bind defect.
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
    assert!(t.expect_ok("bind x").contains("bound"), "bind x: {}", t.text);
    let t = repl.eval("x + 1").await;
    assert!(t.expect_ok("read x").contains("43"), "read x: {}", t.text);
    let t = repl.eval("b <- pure (Box 7)").await;
    assert!(t.expect_ok("bind b").contains("bound"), "bind b: {}", t.text);
    let t = repl.eval("case b of Box n -> n + 100").await;
    assert!(t.expect_ok("case b").contains("107"), "case b: {}", t.text);

    repl.close().await.expect_ok("close");
}

/// MINIMAL repro of the dominant kind=4 bug (dim-d/f breakthrough): with ANY
/// binding live, the Eff reference path crashes regardless of the expression —
/// even one that ignores the binding. Diagnostic-only (ignored by default); run
/// with `--ignored` + TIDEPOOL_DUMP_CLOSED=result / TIDEPOOL_VARID_AUDIT=1.
#[ignore = "open bug: Eff reference path traps kind=4 with any binding live (pure(123) ignores the binding)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eff_ref_pure_const_with_binding_live() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.eval("x <- pure (1 :: Int)").await;
    assert!(t.expect_ok("bind x").contains("bound"), "bind x: {}", t.text);

    // Eff reference run that does NOT touch x — should be 123, crashes kind=4.
    let t = repl.eval("pure (123 :: Int)").await;
    assert!(t.expect_ok("pure 123").contains("123"), "pure 123: {}", t.text);

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
    assert!(t.expect_ok("pure 123").contains("123"), "pure 123: {}", t.text);

    repl.close().await.expect_ok("close");
}

// ───────────── OPEN-BUG REPROS (ignored until the extract fix lands) ─────────────

/// DECISIVE repro: identical to `box_second_bind_replica` except the second
/// bind is a Text. FAILS today: "Could not find module Val.G1" + kind=4.
#[ignore = "open bug: Text (library-dep) second-bind drops injected Val.G iface → kind=4 TypeMetadata; fix is in the extract (GhcPipeline), not codegen"]
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
    assert!(t.expect_ok("bind x").contains("bound"), "bind x: {}", t.text);
    let t = repl.eval("x + 1").await;
    assert!(t.expect_ok("read x").contains("43"), "read x: {}", t.text);

    // The library-dep (Data.Text) second bind that triggers the downsweep loss.
    let t = repl.eval("y <- pure (T.pack \"hi\")").await;
    assert!(t.expect_ok("bind y").contains("bound"), "bind y: {}", t.text);

    // Full VALUE-CORRECTNESS gate for the eventual fix (length/unpack/toUpper/append).
    let t = repl.eval("T.length y").await;
    assert!(t.expect_ok("T.length y").contains("2"), "T.length y: {}", t.text);
    let t = repl.eval("T.unpack y").await;
    assert!(t.expect_ok("T.unpack y").contains("hi"), "T.unpack y: {}", t.text);
    let t = repl.eval("pure (T.unpack (T.toUpper y))").await;
    assert!(t.expect_ok("toUpper y").contains("HI"), "toUpper y: {}", t.text);
    let t = repl.eval("T.unpack (T.append y (T.pack \"!\"))").await;
    assert!(t.expect_ok("append y").contains("hi!"), "append y: {}", t.text);

    repl.close().await.expect_ok("close");
}

/// Same-name rebind: `x <- Int` then `x <- Text`. Same root cause.
#[ignore = "open bug: same as text_bind_headline_faithful (library-dep second bind)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_rebind_same_name() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.eval("x <- pure (1 :: Int)").await;
    assert!(t.expect_ok("bind x int").contains("bound"), "bind x int: {}", t.text);
    let t = repl.eval("x + 1").await; // read between (headline-style)
    assert!(t.expect_ok("read x").contains("2"), "read x: {}", t.text);

    let t = repl.eval("x <- pure (T.pack \"hi\")").await;
    assert!(t.expect_ok("rebind x text").contains("bound"), "rebind x: {}", t.text);

    let t = repl.eval("T.length x").await;
    assert!(t.expect_ok("T.length x").contains("2"), "T.length x: {}", t.text);
    let t = repl.eval("T.unpack x").await;
    assert!(t.expect_ok("T.unpack x").contains("hi"), "T.unpack x: {}", t.text);

    repl.close().await.expect_ok("close");
}

/// Longer multibyte-capable Text as a second bind — same root cause; the
/// length/round-trip assertions are the value-correctness gate for the fix.
#[ignore = "open bug: same as text_bind_headline_faithful (library-dep second bind)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_bind_longer_with_prior() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.eval("n <- pure (0 :: Int)").await;
    assert!(t.expect_ok("bind n").contains("bound"), "bind n: {}", t.text);
    let t = repl.eval("n + 1").await;
    assert!(t.expect_ok("read n").contains("1"), "read n: {}", t.text);

    let t = repl.eval("y <- pure (T.pack \"hello world\")").await;
    assert!(t.expect_ok("bind y").contains("bound"), "bind y: {}", t.text);

    let t = repl.eval("T.length y").await;
    assert!(t.expect_ok("T.length y").contains("11"), "T.length y: {}", t.text);
    let t = repl.eval("T.unpack y").await;
    assert!(
        t.expect_ok("T.unpack y").contains("hello world"),
        "T.unpack y: {}",
        t.text
    );

    repl.close().await.expect_ok("close");
}
