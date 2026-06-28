//! Wave 3b hardening — DIMENSION: shadowing & generations.
//!
//! Adversarial integration tests driving the REAL `tidepool-repl` entry point
//! (session_open / session_def / session_eval / session_cmd / session_close)
//! over multiple turns. Focus: what happens when a NAME is rebound (value or
//! function) or a TYPE is redefined across generations.
//!
//! THE KEY HYPOTHESIS (case 1): `Session::live_val_modules()` collects EVERY
//! still-live `Val.G<g>` module (binding_table.rs `live_modules()` iterates the
//! append-only `live` map, which retains shadowed old gens), and
//! `session_imports()` both injects AND `import`s all of them. After rebinding
//! `x`, both `Tidepool.Session.Val.G1` (exports `x`) and `…Val.G2` (exports `x`)
//! are imported unqualified → GHC ambiguous-occurrence error at the reference.
//!
//! Each test skips cleanly when the session-aware extract is unavailable.

mod common;
use common::*;

/// CASE 1 — Rebind a value name; newest must win at the reference.
///
/// Sequence: `x <- pure (1 :: Int)` ; `x <- pure (2 :: Int)` ; `x + 1`.
/// EXPECT: 3 (newest binding wins).
/// SUSPECTED BUG: both Val.G1 and Val.G2 export `x`, imported unqualified →
/// ambiguous occurrence compile error at `x + 1`.
///
/// FIXED (was BUG #1): session.rs now imports only the CURRENT gen per name
/// (`current_val_modules` via `iter_current`) while still INJECTING every live
/// gen (`live_val_modules`), so the reference resolves `x` unambiguously to the
/// newest binding. Previously this failed with GHC-87543 "Ambiguous occurrence
/// `x' — either Val.G1.x or Val.G2.x". Now PASSES with newest-wins => 3.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebind_value_name_newest_wins() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("x <- pure (1 :: Int)")
        .await
        .expect_ok("bind x=1");
    repl.eval("x <- pure (2 :: Int)")
        .await
        .expect_ok("rebind x=2");

    let t = repl.eval("x + 1").await;
    // BUG (if this fails as error): rebinding a value name leaves BOTH gen
    // modules imported unqualified, so `x` is ambiguous. Expected newest-wins=3.
    let out = t.expect_ok("reference rebound x (expected newest-wins => 3)");
    assert!(
        out.contains('3'),
        "rebind value: expected 3 (newest x=2, +1), got: {out}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 2 — Rebind a name at a DIFFERENT type; newest type must win.
///
/// `x <- pure (1 :: Int)` ; `x <- pure (T.pack "hi")` ; `T.length x` => 2.
/// Fixed by BUG-2 (commit caf3f4b: resolve home-library functions in session
/// extract). Previously crashed with kind=4 TypeMetadata "forced type metadata
/// (should be dead code)" — the trigger was a Tier-0 Text bind while ANY prior
/// binding was live. BUG-2 fixed the home-library function resolution that
/// caused the TypeMetadata forcing. Covered by text_bind.rs green suite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebind_value_different_type() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("x <- pure (1 :: Int)")
        .await
        .expect_ok("bind x :: Int");
    repl.eval("x <- pure (T.pack \"hi\")")
        .await
        .expect_ok("rebind x :: Text");

    let t = repl.eval("T.length x").await;
    // BUG (if error): the newest type (Text) should win; a stale Int iface for
    // the shadowed `x` must not clash. Expected T.length "hi" => 2.
    let out = t.expect_ok("reference rebound x :: Text (expected T.length => 2)");
    assert!(
        out.contains('2'),
        "rebind type: expected 2 (T.length \"hi\"), got: {out}"
    );

    repl.close().await.expect_ok("close");
}

/// CONTROL for CASE 2 — bind a Text value as the FIRST/ONLY binding (no rebind).
///
/// open; `s <- pure (T.pack "hi")` ; `T.length s` => 2.
/// Disambiguates the CASE 2 crash: if THIS also dies with `kind=4 TypeMetadata`
/// / "forced type metadata (should be dead code)", then binding ANY Text value
/// is broken (general Tier-0 force bug, high value) — NOT rebind-specific.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_bind_text_no_rebind() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let bind = repl.eval("s <- pure (T.pack \"hi\")").await;
    // BUG (if this errors with the same TypeMetadata yield): binding a plain Text
    // value is broken regardless of shadowing — the CASE 2 crash is NOT
    // rebind-specific.
    bind.expect_ok("first bind s :: Text (control — no rebind)");

    let out = repl.eval("T.length s").await;
    let out = out.expect_ok("reference s :: Text (expected T.length => 2)");
    assert!(
        out.contains('2'),
        "control text bind: expected 2 (T.length \"hi\"), got: {out}"
    );

    repl.close().await.expect_ok("close");
}

/// DIAGNOSTIC for CASE 2 — bind a Text under a DIFFERENT name while an Int
/// binding is already live (no rebind of the same name).
///
/// open; `x <- pure (1 :: Int)`; `y <- pure (T.pack "hi")` ; `T.length y` => 2.
/// This was a BUG-2 diagnostic: both this test and CASE 2 crashed identically
/// with kind=4 TypeMetadata "forced type metadata (should be dead code)", proving
/// the bug was "Tier-0 Text bind while ANY prior binding is live" (not
/// rebind-same-name-specific). Fixed by BUG-2 (commit caf3f4b). Now PASSES.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn different_name_text_after_int() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("x <- pure (1 :: Int)")
        .await
        .expect_ok("bind x :: Int");
    let bind = repl.eval("y <- pure (T.pack \"hi\")").await;
    // Different name, no rebind — does a Text Tier-0 bind survive a live prior
    // binding? (See doc comment for the localization this answers.)
    bind.expect_ok("bind y :: Text with prior Int live (diagnostic)");

    let out = repl.eval("T.length y").await;
    let out = out.expect_ok("reference y :: Text (expected T.length => 2)");
    assert!(
        out.contains('2'),
        "diagnostic text bind: expected 2, got: {out}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 3 — Redefine a FUNCTION (Lane A latest-wins).
///
/// def `g x = x + (1 :: Int)`; eval `pure (g 10)` => 11;
/// def `g x = x + (100 :: Int)`; eval `pure (g 10)` => 110.
/// Lane A regenerates the `Lib.G<g>` module each def; only the CURRENT module is
/// imported by `session_imports()` (single `current_module()`), so this is the
/// path most likely to actually work. A failure here is a deeper regression.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redefine_function_latest_wins() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("g x = x + (1 :: Int)").await.expect_ok("def g v1");
    let out = repl.eval_ok("pure (g 10)").await;
    assert!(out.contains("11"), "g v1: expected 11, got: {out}");

    repl.def("g x = x + (100 :: Int)")
        .await
        .expect_ok("def g v2");
    let out2 = repl.eval_ok("pure (g 10)").await;
    assert!(
        out2.contains("110"),
        "g v2: expected 110 (latest def wins), got: {out2}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 4 — Redefine a TYPE: the honest CURRENT CONTRACT (GHCi-correct behavior).
///
/// Each `Lib.G<g>` regenerates `data Color` as a DISTINCT type, so a value bound
/// against the old gen cannot re-match a redefined `Color`. This test pins the
/// ACTUAL behavior: (a) the redefine + binding/matching NEW-gen values works;
/// (b) re-matching an OLD-gen value after the redefine FAILS GRACEFULLY (clean
/// MCP error, GHC-83865 type mismatch) and the session SURVIVES (later turns
/// still run).
///
/// # GHCi Parity — this IS the correct contract
///
/// This graceful-failure behavior exactly matches GHCi's semantics when a `data`
/// type is redefined mid-session:
///
///   ghci> data Color = Red | Green
///   ghci> let c = Green          -- c :: Color (v1)
///   ghci> data Color = Red | Green | Blue
///   ghci> case c of { Green -> 1; _ -> 0 }
///   -- type error: `c :: Color` (the v1 type) but `Green` resolves to the v2 `Color`
///
/// GHCi makes the old value keep its old type — it is STILL usable through
/// old-typed code (e.g. code compiled before the redefine). But mixing the old
/// value with the new constructors is a LOUD TYPE ERROR because the two `Color`
/// types are distinct nominal types with potentially different runtime
/// representations. There is no SOUND way to auto-coerce an old-typed value to the
/// new type. In our gen-versioned module scheme each `Lib.G<g>` is exactly that
/// generational boundary: `Green` in `Lib.G2` names the new type's constructor,
/// and `c` (bound against `Lib.G1.Color`) is a different type. GHC surfaces this
/// correctly as a type mismatch rather than a silent runtime corruption.
///
/// Graceful failure = GHCi-correct. No aspirational coexistence test remains
/// (it was deleted — see git log; it was NOT a bug that warranted fixing).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redefine_type_old_binding_orphaned_gracefully() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("data Color = Red | Green")
        .await
        .expect_ok("def Color v1");
    repl.eval("c <- pure Green").await.expect_ok("bind c=Green");
    let out = repl
        .eval("case c of { Green -> (1 :: Int); _ -> 0 }")
        .await;
    assert!(
        out.expect_ok("case c (Color v1)").contains('1'),
        "case c: expected 1, got: {}",
        out.text
    );

    // Redefine + bind/match a NEW-gen value — this WORKS (new type, current gen).
    repl.def("data Color = Red | Green | Blue")
        .await
        .expect_ok("def Color v2");
    repl.eval("c2 <- pure Blue").await.expect_ok("bind c2=Blue");
    let out2 = repl
        .eval("case c2 of { Blue -> (2 :: Int); _ -> 0 }")
        .await;
    assert!(
        out2.expect_ok("case c2 (Color v2)").contains('2'),
        "case c2: expected 2 (Blue), got: {}",
        out2.text
    );

    // Re-matching the OLD `c` after the redefine FAILS GRACEFULLY: `Green`
    // resolves to Color(G2) but `c :: Color(G1)` → GHC-83865 mismatch, surfaced
    // as a clean MCP error (NOT a crash/hang).
    let orphan = repl
        .eval("case c of { Green -> (1 :: Int); _ -> 0 }")
        .await;
    orphan.expect_err("old-gen c re-match after redefine should fail gracefully");

    // The session SURVIVES the orphaned-reference error — a later turn still runs
    // and the new-gen binding still resolves.
    let survive = repl
        .eval("case c2 of { Blue -> (2 :: Int); _ -> 0 }")
        .await;
    assert!(
        survive
            .expect_ok("session survives orphan error (c2 still matches)")
            .contains('2'),
        "post-orphan c2: expected 2, got: {}",
        survive.text
    );

    repl.close().await.expect_ok("close");
}

/// CASE 5 — `:bindings` after a rebind lists the name exactly ONCE (newest).
///
/// `iter_current()` is keyed by name, so the JSON should carry a single `"x"`.
/// This is the cheap structural check that shadowing collapses the view even if
/// the reference path (case 1) is broken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bindings_after_rebind_lists_once() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("x <- pure (1 :: Int)")
        .await
        .expect_ok("bind x=1");
    repl.eval("x <- pure (2 :: Int)")
        .await
        .expect_ok("rebind x=2");

    let t = repl.cmd(":bindings").await;
    let out = t.expect_ok(":bindings");
    let occurrences = out.matches("\"x\"").count();
    assert_eq!(
        occurrences, 1,
        ":bindings should list `x` exactly once (newest), got {occurrences}: {out}"
    );

    repl.close().await.expect_ok("close");
}
