//! Wave 3b hardening — DIMENSION: shadowing & generations.
//!
//! Adversarial integration tests driving the REAL `tidepool-repl` entry point
//! (session_run — the harness `def`/`eval`/`cmd`
//! helpers are thin 1-item `session_run` wrappers) over multiple turns.
//! Focus: what happens when a NAME is rebound (value or function) or a TYPE is
//! redefined across generations.
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
}

/// CASE 1b — SELF-REFERENTIAL rebind (the accumulator idiom).
///
/// `n <- pure (1 :: Int)` ; `n <- pure (n + 1)` ; `n <- pure (n + 1)` ; `n` => 3.
/// The RHS references the name being rebound, so GHCi `>>=` semantics apply:
/// each `pure (n + 1)` reads the PRIOR `n` and the new bind shadows it.
///
/// WAS A BUG: the "a pure bind is a declaration" route lowered `n <- pure (n+1)`
/// to the top-level decl `n = n + 1`, which is RECURSIVE in Haskell — forcing it
/// self-forced to a `blackhole detected (thunk forced itself)` runtime error.
/// FIXED: `self_referential_monadic_pure_bind` diverts these to the
/// materialize/shadow path (`run_bind`), which compiles the RHS against the
/// imported PRIOR `n`. (CASE 1 above doesn't catch this — its RHS is a constant.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_referential_rebind_reads_prior() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();

    repl.eval("n <- pure (1 :: Int)")
        .await
        .expect_ok("bind n=1");
    repl.eval("n <- pure (n + 1)")
        .await
        .expect_ok("accumulate n=n+1 (must read prior n, not self-force)");
    repl.eval("n <- pure (n + 1)")
        .await
        .expect_ok("accumulate again");

    let out = repl.eval("n").await;
    let out = out.expect_ok("read accumulated n (expected 3, not a blackhole)");
    assert!(
        out.contains('3'),
        "self-referential accumulator: expected 3 (1 -> 2 -> 3), got: {out}"
    );
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
/// Graceful failure = GHCi-correct: the old binding is orphaned by a clean type
/// mismatch, which is the right behavior, not a bug to design coexistence around.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redefine_type_old_binding_orphaned_gracefully() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();

    repl.def("data Color = Red | Green")
        .await
        .expect_ok("def Color v1");
    repl.eval("c <- pure Green").await.expect_ok("bind c=Green");
    let out = repl.eval("case c of { Green -> (1 :: Int); _ -> 0 }").await;
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
    let out2 = repl.eval("case c2 of { Blue -> (2 :: Int); _ -> 0 }").await;
    assert!(
        out2.expect_ok("case c2 (Color v2)").contains('2'),
        "case c2: expected 2 (Blue), got: {}",
        out2.text
    );

    // Re-matching the OLD `c` after the redefine FAILS GRACEFULLY: `Green`
    // resolves to Color(G2) but `c :: Color(G1)` → GHC-83865 mismatch, surfaced
    // as a clean MCP error (NOT a crash/hang).
    let orphan = repl.eval("case c of { Green -> (1 :: Int); _ -> 0 }").await;
    orphan.expect_err("old-gen c re-match after redefine should fail gracefully");

    // The session SURVIVES the orphaned-reference error — a later turn still runs
    // and the new-gen binding still resolves.
    let survive = repl.eval("case c2 of { Blue -> (2 :: Int); _ -> 0 }").await;
    assert!(
        survive
            .expect_ok("session survives orphan error (c2 still matches)")
            .contains('2'),
        "post-orphan c2: expected 2, got: {}",
        survive.text
    );
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
}

/// CASE 6 — decl→value MIGRATION + a later `let` reference (the honest-decl-plane
/// regression). A pure bind lands on the decl plane, a self-referential rebind
/// migrates it to the value plane; a subsequent `let` (a decl-plane item) that
/// references it must see the LIVE value, not the stale decl.
///
/// WAS A BUG: `bind_materialized` evicted the value from the `pure_binds` map but
/// left the decl (`x = []`) in `SessionLib` forever, so `let y = length x`
/// compiled against the stale `x = []` → `y == 0`, while a bare `length x` read
/// the value plane → correct. FIXED: `bind_materialized` now retracts `x` from
/// the decl plane, so the `let` fails to resolve `x` there and materializes
/// (seeing the value plane). Bare and `let` reads must AGREE.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migrated_name_read_from_later_let() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();

    repl.eval("x <- pure ([] :: [Int])")
        .await
        .expect_ok("bind x=[] (decl plane)");
    repl.eval("x <- pure (0 : x)")
        .await
        .expect_ok("migrate x to value plane (self-ref)");

    // A `let` (decl-plane item) referencing the migrated name.
    repl.eval("let y = length x")
        .await
        .expect_ok("let y = length x (must see the value plane, not stale decl)");
    let out = repl.eval("y").await;
    let out = out.expect_ok("read y");
    assert!(
        out.contains('1'),
        "let over migrated name: expected 1 (length [0]), got: {out}"
    );
    // Bare expression and the `let` must agree.
    let bare = repl.eval_ok("length x").await;
    assert!(bare.contains('1'), "bare length x: expected 1, got: {bare}");
}

/// CASE 7 — accumulator, then a `let` fold. The pattern the repl exists for:
/// build a list across turns with self-referential rebinds, then a `let` that
/// folds it must see every element (not a stale empty decl).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accumulate_then_let_fold() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();

    repl.eval("acc <- pure ([] :: [Int])")
        .await
        .expect_ok("acc=[]");
    for v in ["1", "2", "3"] {
        repl.eval(&format!("acc <- pure ({v} : acc)"))
            .await
            .expect_ok("accumulate");
    }
    repl.eval("let total = sum acc")
        .await
        .expect_ok("let total = sum acc (fold over migrated accumulator)");
    let out = repl.eval_ok("total").await;
    assert!(
        out.contains('6'),
        "accumulate then fold: expected 6 (1+2+3), got: {out}"
    );
}

/// CASE 8 — a function `def` referencing a migrated name CLOSES OVER the live
/// value (GHCi parity: a top-level definition at the prompt sees earlier
/// bindings). Decl turns are val-scoped (`Session::define_scoped`), so the
/// decl plane resolves a materialized heap value through its injected
/// `Val.G<g>` iface. Capture is at DEFINITION time: a later rebind of `x`
/// must not retro-change `g`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn def_referencing_migrated_name_closes_over_value() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();

    repl.eval("x <- pure (1 :: Int)")
        .await
        .expect_ok("bind x=1 (decl plane)");
    repl.eval("x <- pure (x + 1)")
        .await
        .expect_ok("migrate x to value plane");

    repl.eval("g y = y + x")
        .await
        .expect_ok("def referencing a migrated value closes over it");
    let out = repl.eval_ok("g 1").await;
    assert!(out.contains('3'), "g 1 == 1 + x(=2), got: {out}");

    // Capture-at-definition: rebinding x must not retro-change g.
    repl.eval("x <- pure (100 :: Int)")
        .await
        .expect_ok("rebind x after g captured it");
    let out = repl.eval_ok("g 1").await;
    assert!(
        out.contains('3'),
        "g keeps the x it captured at definition, got: {out}"
    );
}

/// CASE 9 — REGRESSION GUARD: a `let` referencing a decl-plane binding that was
/// NEVER migrated must still resolve on the decl plane (retraction must not fire
/// when there is no migration, and generalization is preserved).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn let_referencing_unmigrated_decl_still_works() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();

    repl.eval("n <- pure (5 :: Int)")
        .await
        .expect_ok("bind n=5 (decl plane, not migrated)");
    repl.eval("let m = n + 1")
        .await
        .expect_ok("let m = n + 1 (decl→decl reference)");
    let out = repl.eval_ok("m").await;
    assert!(
        out.contains('6'),
        "decl-plane let reference: expected 6, got: {out}"
    );
    // A fresh polymorphic pure bind still generalizes (retraction machinery
    // didn't disturb the decl plane's generalization).
    repl.eval("xs <- pure []")
        .await
        .expect_ok("xs=[] generalizes");
    let len = repl.eval_ok("length xs").await;
    assert!(
        len.contains('0'),
        "xs generalized + usable: expected 0, got: {len}"
    );
}
