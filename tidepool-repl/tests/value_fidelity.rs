//! Wave 3b hardening — DIMENSION E: value-plane fidelity.
//!
//! Adversarial integration tests driving the REAL `tidepool-repl` entry point
//! (session_open / session_run / session_close — the harness `def`/`eval`/`cmd`
//! helpers are thin 1-item `session_run` wrappers) over multiple turns, per the
//! standing rule (production tool dispatch over real turns, organic GC on the
//! 2 MiB nursery — never a bespoke harness or forced GC).
//!
//! Focus: do bound values of every interesting SHAPE round-trip with fidelity —
//! Text (first-class), values that reference earlier bindings at bind time,
//! Tier-1 closures capturing a session value that must survive GC,
//! nested/recursive ADTs (real con names, not `<unknown>`), Maybe/Either,
//! structured JSON `Value` (Tier-0 + DataConTable), and lists.
//!
//! Each test skips cleanly when the session-aware extract is unavailable, and
//! ALWAYS ends with `repl.close().await` (teardown discipline).
//!
//! Regression gate: these tests guard the now-fixed kind=4 TypeMetadata force bug
//! (fixed in GhcPipeline.runSessionPipeline — see text_bind.rs header for the
//! root cause). All tests assert SUCCESS; inline regression-witness comments mark
//! the turns that previously fired the kind=4 crash.

mod common;
use common::*;

/// CASE 1 — Text is first-class: bind a Text, then read/transform it.
///
/// `s <- pure (T.pack "hi")` ; `T.length s` => 2 ; `T.unpack s` ; `T.toUpper s`.
/// Text binds (Tier-0 deep_force of a Text heap object) + references back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_first_class_bind_and_reference() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("s <- pure (T.pack \"hi\")")
        .await
        .expect_ok("bind s :: Text");

    let len = repl.eval("T.length s").await;
    assert!(
        len.expect_ok("T.length s").contains('2'),
        "T.length s: expected 2, got: {}",
        len.text
    );

    let unpacked = repl.eval("T.unpack s").await;
    assert!(
        unpacked.expect_ok("T.unpack s").contains("hi"),
        "T.unpack s: expected hi, got: {}",
        unpacked.text
    );

    // Regression witness: was the kind=4 TypeMetadata crash (now fixed). Previously
    // materializing a Text result fired "forced type metadata (should be dead code)"
    // even with a valid bound `s`; the fix in GhcPipeline.runSessionPipeline
    // resolved it. Now asserts success.
    let upper = repl.eval("pure (T.toUpper s)").await;
    assert!(
        upper.expect_ok("T.toUpper s").contains("HI"),
        "T.toUpper s: expected HI, got: {}",
        upper.text
    );

    repl.close().await.expect_ok("close");
}

/// CASE 2 — THE KEY DIAGNOSTIC: a Tier-0 Text bind under a DIFFERENT name while
/// a prior (Int) binding is live. NO rebind of the same name.
///
/// open; `x <- pure (1 :: Int)`; `y <- pure (T.pack "hi")`; `T.length y` => 2.
///
/// We have a CONFIRMED bug where rebinding to Text crashes with
/// `kind=4 TypeMetadata` in Tier-0 deep_force. This isolates WHETHER the crash
/// needs the same name:
///   - If THIS crashes → bug is GENERAL: "Tier-0 Text bind crashes whenever ANY
///     prior binding is live (even a different name)".
///   - If THIS passes → the crash is specific to rebinding the SAME name.
///
/// VERDICT (this run): THIS CRASHES. `y <- pure (T.pack "hi")` with the Int `x`
/// live dies at BIND time with `kind=4 (TypeMetadata) msg="hi"` →
/// "bind runtime error: ... forced type metadata (should be dead code)".
/// Corroborated by `text_first_class_bind_and_reference`, where binding a Text as
/// the FIRST/ONLY binding (no prior) does NOT crash. ⇒ The bug is GENERAL: a
/// Tier-0 Text bind crashes whenever ANY prior binding is live (even a DIFFERENT
/// name). It is NOT rebind-same-name-specific. Root-cause hypothesis: with a
/// prior binding live, the bind turn injects that binding's `Val.G<g>` module and
/// seeds its slot into the `ExternalEnv` (session.rs `run_bind` ~L321/L365); the
/// subsequent `deep_force` (forced=true, session.rs ~L368/L375
/// `run_fragment_and_bind`) traverses the Text spine and reaches a TypeMetadata
/// node it treats as dead code → kind=4 yield. The shallow-Int prior binding
/// itself forces fine, so the defect is in deep_force of the Text payload under a
/// multi-module-inject context, not in the prior value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_bind_with_prior_live_binding_diagnostic() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("x <- pure (1 :: Int)")
        .await
        .expect_ok("bind x :: Int");

    // The diagnostic bind. If the bug is general (prior-binding-present), this is
    // where `kind=4 TypeMetadata` / "forced type metadata (should be dead code)"
    // surfaces.
    let bind = repl.eval("y <- pure (T.pack \"hi\")").await;
    // Regression witness: was the kind=4 TypeMetadata crash (now fixed). A Tier-0
    // Text bind with a prior live binding used to fire here; now asserts success.
    bind.expect_ok("bind y :: Text with prior Int live (KEY DIAGNOSTIC)");

    let out = repl.eval("T.length y").await;
    assert!(
        out.expect_ok("T.length y").contains('2'),
        "diagnostic text bind: expected 2, got: {}",
        out.text
    );

    repl.close().await.expect_ok("close");
}

/// CASE 3 — A bind that REFERENCES an earlier binding at bind time.
///
/// `k <- pure (5 :: Int)`; `m <- pure (k + 1)`; `m` => 6.
/// The `m` bind action reads `k` through the seeded ExternalEnv.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_references_earlier_binding() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("k <- pure (5 :: Int)")
        .await
        .expect_ok("bind k=5");
    repl.eval("m <- pure (k + 1)")
        .await
        .expect_ok("bind m = k + 1 (references k)");

    let out = repl.eval("m").await;
    assert!(
        out.expect_ok("reference m").contains('6'),
        "bind-references-earlier: expected 6, got: {}",
        out.text
    );

    repl.close().await.expect_ok("close");
}

/// CASE 4 — A Tier-1 closure capturing an earlier binding survives GC.
///
/// `base <- pure (100 :: Int)`; `f <- pure (\n -> n + base)`; heavy fold (forces
/// minor GC on the 2 MiB nursery); `f 5` => 105.
/// Proves the closure's captured session value (`base`) stays live across GC.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closure_captures_binding_survives_gc() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("base <- pure (100 :: Int)")
        .await
        .expect_ok("bind base=100");
    repl.eval("f <- pure (\\n -> n + base)")
        .await
        .expect_ok("bind f (captures base)");

    // ORGANIC GC: ~6 MiB of transient cons into the 2 MiB nursery.
    let fold = repl.eval("foldl' (+) (0 :: Int) [1..200000]").await;
    assert!(
        fold.expect_ok("heavy fold (force GC)")
            .contains("20000100000"),
        "fold: got {}",
        fold.text
    );

    let out = repl.eval("f 5").await;
    assert!(
        out.expect_ok("f 5 after GC").contains("105"),
        "closure-captures-survives-GC: expected 105, got: {}",
        out.text
    );

    repl.close().await.expect_ok("close");
}

/// CASE 5 — Nested/recursive ADT: bind a tree, sum it a later turn.
///
/// def `data Tree = Leaf Int | Node Tree Tree`;
/// `t <- pure (Node (Leaf 1) (Node (Leaf 2) (Leaf 3)))`;
/// def `sumT t = case t of { Leaf n -> n; Node a b -> sumT a + sumT b }`;
/// `pure (sumT t)` => 6.
/// Real con names (Leaf/Node) must resolve from the merged session DataConTable
/// against the tenured heap value (not `<unknown>`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_recursive_adt_bind_and_sum() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("data Tree = Leaf Int | Node Tree Tree")
        .await
        .expect_ok("def Tree");
    repl.eval("t <- pure (Node (Leaf 1) (Node (Leaf 2) (Leaf 3)))")
        .await
        .expect_ok("bind t (nested Tree)");
    repl.def("sumT t = case t of { Leaf n -> n; Node a b -> sumT a + sumT b }")
        .await
        .expect_ok("def sumT");

    // Regression witness: was the kind=4 TypeMetadata crash (now fixed). Spine
    // traversal of a deep bound ADT used to fire here; now asserts success.
    let out = repl.eval("pure (sumT t)").await;
    assert!(
        out.expect_ok("sumT t").contains('6'),
        "nested ADT sum: expected 6, got: {}",
        out.text
    );

    repl.close().await.expect_ok("close");
}

/// CASE 6a — Maybe: bind `Just 7`, case-match it.
///
/// `mb <- pure (Just (7 :: Int))`; `case mb of { Just n -> n; Nothing -> 0 }` => 7.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maybe_bind_and_case() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("mb <- pure (Just (7 :: Int))")
        .await
        .expect_ok("bind mb = Just 7");
    let out = repl.eval("case mb of { Just n -> n; Nothing -> 0 }").await;
    assert!(
        out.expect_ok("case mb").contains('7'),
        "Maybe case: expected 7, got: {}",
        out.text
    );

    repl.close().await.expect_ok("close");
}

/// CASE 6b — Either: bind `Left 1`, case-match it.
///
/// `e <- pure (Left (1 :: Int) :: Either Int Int)`;
/// `case e of { Left a -> a; Right b -> b }` => 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn either_bind_and_case() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("e <- pure (Left (1 :: Int) :: Either Int Int)")
        .await
        .expect_ok("bind e = Left 1");
    let out = repl.eval("case e of { Left a -> a; Right b -> b }").await;
    assert!(
        out.expect_ok("case e").contains('1'),
        "Either case: expected 1, got: {}",
        out.text
    );

    repl.close().await.expect_ok("close");
}

/// CASE 7 — Structured JSON `Value`: bind an `object`, read a field back.
///
/// `v <- pure (object [("a", toJSON (1 :: Int)), ("b", toJSON (2 :: Int))])`;
/// `renderJson v` (and an optics read `v ^? key "a" . _Integer`).
/// Tier-0 structured value + DataConTable round-trip.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structured_json_value_bind_and_read() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("v <- pure (object [(\"a\", toJSON (1 :: Int)), (\"b\", toJSON (2 :: Int))])")
        .await
        .expect_ok("bind v = object {a:1,b:2}");

    // Regression witness: was the kind=4 TypeMetadata crash (now fixed). Reading
    // back a bound JSON `Value` used to fire here; now asserts success.
    let rendered = repl.eval("renderJson v").await;
    let r = rendered.expect_ok("renderJson v");
    assert!(
        r.contains("\\\"a\\\"") || r.contains("\"a\"") || r.contains('a'),
        "renderJson v: expected to mention field a, got: {r}"
    );
    assert!(
        r.contains('1') && r.contains('2'),
        "renderJson v: expected 1 and 2, got: {r}"
    );

    // Optics read of a single field — should be 1.
    let field = repl.eval("v ^? key \"a\" . _Integer").await;
    assert!(
        field.expect_ok("v ^? key a . _Integer").contains('1'),
        "v ^? key a: expected 1, got: {}",
        field.text
    );

    repl.close().await.expect_ok("close");
}

/// CASE 8 — A list binding survives GC; length + sum read back.
///
/// `xs <- pure [1..1000 :: Int]`; heavy fold (GC); `pure (length xs)` => 1000;
/// `pure (sum xs)` => 500500.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_bind_survives_gc() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("xs <- pure [1..1000 :: Int]")
        .await
        .expect_ok("bind xs = [1..1000]");

    // ORGANIC GC.
    let fold = repl.eval("foldl' (+) (0 :: Int) [1..200000]").await;
    assert!(
        fold.expect_ok("heavy fold (force GC)")
            .contains("20000100000"),
        "fold: got {}",
        fold.text
    );

    // Regression witness: was the kind=4 TypeMetadata crash (now fixed). Forcing
    // the bound list spine used to fire here even though the result is an Int;
    // now asserts success.
    let len = repl.eval("pure (length xs)").await;
    assert!(
        len.expect_ok("length xs after GC").contains("1000"),
        "length xs: expected 1000, got: {}",
        len.text
    );

    let total = repl.eval("pure (sum xs)").await;
    assert!(
        total.expect_ok("sum xs after GC").contains("500500"),
        "sum xs: expected 500500, got: {}",
        total.text
    );

    repl.close().await.expect_ok("close");
}

/// CASE 9 — A function bound, then a value bound FROM applying it.
///
/// `g <- pure (\n -> n * 2 :: Int)`; `h <- pure (g 21)`; `h` => 42.
/// The closure is applied at bind time and its result rooted as a Tier-0 value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn function_applied_at_bind_time() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("g <- pure (\\n -> n * 2 :: Int)")
        .await
        .expect_ok("bind g");
    repl.eval("h <- pure (g 21)")
        .await
        .expect_ok("bind h = g 21");

    let out = repl.eval("h").await;
    assert!(
        out.expect_ok("reference h").contains("42"),
        "function-applied-at-bind: expected 42, got: {}",
        out.text
    );

    repl.close().await.expect_ok("close");
}
