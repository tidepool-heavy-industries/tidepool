//! Session GC stress with the post-GC heap verifier ON.
//!
//! Reproducer hunt for a field SIGSEGV (2026-07-10): in a live repl session
//! that had tenured several MB of Text substrate across turns (git log,
//! readGlob corpus, derived Maps), a fused `Map.map` fold running
//! `T.lines`/`T.isInfixOf` over the tenured Map crashed with
//! `JIT signal: SIGSEGV` — twice, then succeeded on retry over the SAME
//! bindings. Timing-dependent, session-only (the one-shot equivalent in
//! `tidepool-runtime/tests/gc_stress_text_fold.rs` is clean across 100+
//! verified collections). The session-specific surface is the bind/tenure
//! machinery: every bind Cheney-copies its value closure into old-space and
//! leaves TAG_FORWARDED stubs in the live nursery.
//!
//! This suite replays the field turn shape through the REAL `dispatch_tool`
//! entry point on the harness's small (2 MiB) nursery, with
//! `set_heap_verify(true)` so every minor collection walks the full live set
//! and panics at the first dangling/from-space pointer — surfacing corruption
//! at its source instead of as a SIGSEGV collections later.
//!
//! A failure here is a GC/tenure bug, never user error.

mod common;
use common::*;

/// The field sequence: helper decl → big Text substrate bind (tenure) →
/// derived-Map bind (tenure of shared structure) → closure decl → fused fold
/// expression → fold-result bind → repeat folds. Verifier on throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_text_substrate_folds_heap_verified() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    tidepool_codegen::host_fns::set_heap_verify(true);
    tidepool_codegen::host_fns::set_gc_poison(true);
    let repl = Repl::new();

    // Turn 1: helper decl (field: topOf).
    repl.def("keyOf :: T.Text -> T.Text\nkeyOf p = T.take 2 p")
        .await
        .expect_ok("decl keyOf");

    // Turn 2: bind a multi-MB synthetic corpus — one tenure of a large graph.
    repl.eval(
        "corpus <- pure [ (showT i <> \".rs\", T.unlines [ \"fn f\" <> showT i <> \"_\" <> showT j <> \"() { \" <> (if (i+j) `mod` 7 == 0 then \"unsafe { p }\" else \"x+1\") <> \" }\" | j <- [1 .. 250 + (i*37) `mod` 400 :: Int] ]) | i <- [1 .. 120 :: Int] ]",
    )
    .await
    .expect_ok("bind corpus");

    // Turn 3: derived Map bind — tenures a graph SHARING Texts with `corpus`
    // (the overlapping-tenure/forwarding seam).
    repl.eval("byCrate <- pure (Map.fromListWith (<>) [ (keyOf k, [(k, t)]) | (k, t) <- corpus ])")
        .await
        .expect_ok("bind byCrate");

    // Turn 4: closure over the tenured Map (field: density/countIn —
    // Tier1Closure bindings).
    repl.eval("let countIn needle = Map.map (\\fs -> sum [ length (filter (T.isInfixOf needle) (T.lines t)) | (_, t) <- fs ]) byCrate")
        .await
        .expect_ok("bind countIn closure");

    // Turn 5: the crashing shape — fused fold over the tenured substrate as a
    // bare expression (field crash #1 was this shape).
    let t = repl.eval("sum (Map.elems (countIn \"unsafe \"))").await;
    t.expect_ok("fused fold expression");
    assert!(
        t.contains("value"),
        "fold turn must render a value: {}",
        t.text
    );

    // Turn 6: the same fold BOUND (field crash #2 was a bind: tenure of a
    // result computed by folding old-space data through fresh nursery churn).
    repl.eval("cnts <- pure (countIn \"unsafe \")")
        .await
        .expect_ok("bind fold result");

    // Turns 7..12: repeat folds with varying needles over the same tenured
    // substrate — varies GC-trigger phase against a growing old-space.
    for (i, needle) in ["fn ", "x+1", "() {", "unsafe", "_9", "f1"]
        .iter()
        .enumerate()
    {
        repl.eval(&format!(
            "sum (Map.elems (countIn \"{needle}\")) + sum (Map.elems cnts)"
        ))
        .await
        .expect_ok(&format!("repeat fold {i}"));
    }

    // The probe proves nothing unless minor GCs actually ran and were walked.
    let verified = tidepool_codegen::host_fns::heap_verify_run_count();
    assert!(
        verified > 0,
        "session finished without a single verified GC — grow the corpus"
    );
    eprintln!("[gc-session-stress] verified collections: {verified}");
}

/// Accumulator rebind churn: rebind a name from its own prior value while big
/// tenured structures are live — each rebind re-tenures a graph overlapping
/// the previous generation's (forwarding-stub seam, `acc <- pure (x : acc)`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_rebind_accumulator_heap_verified() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    tidepool_codegen::host_fns::set_heap_verify(true);
    tidepool_codegen::host_fns::set_gc_poison(true);
    let repl = Repl::new();

    repl.eval("acc <- pure ([] :: [T.Text])")
        .await
        .expect_ok("seed acc");
    for i in 0..6 {
        // Cons-heavy chunks (flat `T.replicate` Texts are malloc'd byte
        // arrays OUTSIDE the GC heap and never trigger a collection): 30k
        // small Texts per rebind keeps the nursery churning.
        repl.eval(&format!(
            "acc <- pure (map showT [{i} * 100000 .. {i} * 100000 + 30000 :: Int] <> acc)"
        ))
        .await
        .expect_ok(&format!("rebind acc {i}"));
    }
    let t = repl.eval("sum (map T.length acc)").await;
    t.expect_ok("fold acc");

    let verified = tidepool_codegen::host_fns::heap_verify_run_count();
    assert!(verified > 0, "no verified GC ran — grow the chunks");
    eprintln!("[gc-session-stress] verified collections: {verified}");
}
