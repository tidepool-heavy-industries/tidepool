//! Wave 3b hardening — DIMENSION: declaration plane (Lane A) depth.
//!
//! Adversarial integration tests driving the REAL `tidepool-repl` entry point
//! (session_open / session_run / session_close — the harness `def`/`eval`/`cmd`
//! helpers are thin 1-item `session_run` wrappers) over multiple turns. Focus:
//! the gen-versioned `Tidepool.Session.Lib.G<g>` declaration module that the
//! `def` block-runner item regenerates each turn (selective re-export
//! `import G<g-1> hiding (<redefined>)`), and how user declarations of
//! every shape (functions, ADTs, type aliases, newtypes, records, classes +
//! instances) accumulate and interact across turns.
//!
//! Key source under test:
//!   - tidepool-runtime/src/session/mod.rs  (SessionLib::define — parse-only
//!     binder extraction, append to the decl log, regenerate the module)
//!   - tidepool-runtime/src/session/render.rs (render_module — selective
//!     re-export / `hiding` shadow / latest-wins)
//!   - tidepool-repl/src/session.rs (run_def, session_imports)
//!
//! Each test skips cleanly when the session-aware extract is unavailable.

mod common;
use common::*;

/// CASE 1 — Multiple defs accumulate and INTERACT.
///
/// def `inc`; def `twice` (references the earlier `inc`); eval `twice 10` => 12.
/// The regenerated `Lib.G2` re-exports `Lib.G1`, so `twice`'s body sees `inc`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn defs_accumulate_and_interact() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("inc x = x + (1 :: Int)")
        .await
        .expect_ok("def inc");
    repl.def("twice x = inc (inc x)")
        .await
        .expect_ok("def twice (references inc)");

    let out = repl.eval_ok("pure (twice 10)").await;
    assert!(
        out.contains("12"),
        "accumulate+interact: expected 12 (twice 10 = inc (inc 10)), got: {out}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 2 — Forward reference ACROSS turns is REJECTED at define-time (BUG-A fix).
///
/// NEW SEMANTICS: `SessionLib::define` now validates the candidate gen module via
/// GHC after binder extraction. `def "f x = g x + 1"` before `g` exists FAILS at
/// define time (GHC reports `Variable not in scope: g`), the log is rolled back,
/// and the session remains fully usable. Once `g` is defined first, both `g` and
/// `f` can be defined and evaluated successfully — no poison.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forward_reference_across_turns_poisons() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // def f before g exists — define-time validation catches the forward ref.
    let f_turn = repl.def("f x = g x + (1 :: Int)").await;
    eprintln!(
        "[case2] def f (g not yet defined): is_error={} text={}",
        f_turn.is_error, f_turn.text
    );
    let err = f_turn.expect_err("def f forward-ref REJECTED at define-time (g not in scope)");
    assert!(
        err.contains("not in scope") || err.contains("g"),
        "case2: expected a 'g not in scope' error at define-time, got: {err}"
    );

    // The session is NOT poisoned: subsequent work proceeds normally.
    repl.def("g x = x * (2 :: Int)")
        .await
        .expect_ok("def g OK after rejected forward ref");
    repl.def("f x = g x + (1 :: Int)")
        .await
        .expect_ok("def f OK now that g is defined");

    let out = repl.eval_ok("pure (f 10)").await;
    assert!(
        out.contains("21"),
        "case2: session not poisoned — f 10 = g 10 + 1 = 21, got: {out}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 2b — Mutual/forward reference WITHIN ONE def turn DOES work.
///
/// Both binders live in the SAME gen module, so they see each other. This is the
/// supported way to express forward/mutual references (contrast case 2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutual_reference_single_turn_works() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // f references g; both defined in one turn → one gen module → both in scope.
    repl.def("f2 x = g2 x + (1 :: Int)\ng2 x = x * (2 :: Int)")
        .await
        .expect_ok("def f2+g2 in one turn");

    let out = repl.eval_ok("pure (f2 10)").await;
    assert!(
        out.contains("21"),
        "single-turn mutual ref: expected 21 (g2 10 + 1 = 21), got: {out}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 3 — Redefine a function: latest-wins via the `hiding` shadow.
///
/// def `k x = x + 1`; `k 5` => 6; def `k x = x + 100`; `k 5` => 105.
/// `Lib.G2` imports `Lib.G1 hiding (k)` and re-declares `k`, so the newest body
/// wins at the reference.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redefine_function_latest_wins() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("k x = x + (1 :: Int)").await.expect_ok("def k v1");
    let out = repl.eval_ok("pure (k 5)").await;
    assert!(out.contains('6'), "k v1: expected 6, got: {out}");

    repl.def("k x = x + (100 :: Int)")
        .await
        .expect_ok("def k v2");
    let out2 = repl.eval_ok("pure (k 5)").await;
    assert!(
        out2.contains("105"),
        "k v2: expected 105 (latest def wins via `hiding (k)`), got: {out2}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 4 — Multi-constructor ADT: define once, use as a STABLE type across turns.
///
/// def `data Shape = Circle Int | Rect Int Int`; eval an immediate case => 5;
/// bind a `Shape` value; later case-match the bound value => 12. (Not a type
/// REDEFINITION — that's dim-B's orphan case; here the type is stable.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multicon_adt_value_and_case() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("data Shape = Circle Int | Rect Int Int")
        .await
        .expect_ok("def Shape");

    // Immediate use (no live bindings → plain-eval path).
    let out = repl
        .eval_ok("pure (case Circle 5 of { Circle r -> r; Rect w h -> w * h })")
        .await;
    assert!(out.contains('5'), "case Circle 5: expected 5, got: {out}");

    // Bind a Shape value, then case-match it on a later turn (reference path).
    repl.eval("sh <- pure (Rect 3 4)")
        .await
        .expect_ok("bind sh = Rect 3 4");
    let out2 = repl
        .eval_ok("case sh of { Circle r -> r; Rect w h -> w * h }")
        .await;
    assert!(
        out2.contains("12"),
        "case sh (Rect 3 4): expected 12 (3*4), got: {out2}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 5 — type alias + newtype.
///
/// def `type Name = T.Text`; def `newtype Age = Age Int`; eval
/// `case Age 7 of Age n -> n` => 7. The alias renders as a bare export entry
/// (no `(..)`), the newtype as `Age(..)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn type_alias_and_newtype() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("type Name = T.Text")
        .await
        .expect_ok("def type alias Name");
    repl.def("newtype Age = Age Int")
        .await
        .expect_ok("def newtype Age");

    // Use the newtype (unwrap via case).
    let out = repl.eval_ok("pure (case Age 7 of { Age n -> n })").await;
    assert!(out.contains('7'), "newtype unwrap: expected 7, got: {out}");

    // Exercise the alias too: a Name value round-trips through T.length.
    let out2 = repl
        .eval_ok("pure (T.length (T.pack \"hey\" :: Name))")
        .await;
    assert!(
        out2.contains('3'),
        "type alias Name used: expected 3, got: {out2}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 6 — record syntax on session-bound values, ALL PATHS. Historically the
/// Eff reference path over a live session-bound custom ADT crashed kind=4
/// TypeMetadata (selector AND case alike; pure-fallback path worked). Fixed
/// collaterally 2026-07-02 (verbatim-wrapper + LetRec-knot emit work); this
/// test now ASSERTS correct values on every path it previously only logged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn record_syntax_selectors_localized() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("data P = P { px :: Int, py :: Int }")
        .await
        .expect_ok("def record P");

    // Fresh value, plain-eval path: selector works.
    let out = repl.eval_ok("pure (px (P 3 4))").await;
    assert!(
        out.contains('3'),
        "px (P 3 4) fresh: expected 3, got: {out}"
    );

    // Bind a P value (positional, to isolate the reference path from record-ctor).
    repl.eval("p <- pure (P 1 2)")
        .await
        .expect_ok("bind p = P 1 2");

    // Ref A — PURE fallback path (bare case).
    let ref_a = repl.eval_ok("case p of { P a b -> b }").await;
    assert!(ref_a.contains('2'), "pure-path case: expected 2, got: {ref_a}");

    // Ref B' — Eff path with a case (not a selector).
    let ref_b2 = repl.eval_ok("pure (case p of { P a b -> b })").await;
    assert!(ref_b2.contains('2'), "Eff-path case: expected 2, got: {ref_b2}");

    // Ref B — Eff path with the record SELECTOR (the historical kind=4 crash).
    let ref_b = repl.eval_ok("pure (py p)").await;
    assert!(ref_b.contains('2'), "Eff-path selector: expected 2, got: {ref_b}");
    let survive = repl.eval("123 :: Int").await; // bare → pure-fallback path
    eprintln!(
        "[case6] survive (bare/pure path): is_error={} text={}",
        survive.is_error, survive.text
    );
    let survive = survive.expect_ok("pure path still works after the Eff-path crash");
    assert!(
        survive.contains("123"),
        "session survives via pure path: expected 123, got: {survive}"
    );

    repl.close().await.expect_ok("close");
}

/// Record field selector on a session-bound value via the Eff path — was the
/// last standing kind=4 TypeMetadata crash, fixed collaterally 2026-07-02 by
/// the verbatim-wrapper + LetRec-knot emit work (verified live: bare selector,
/// record-dot, and a mapped section `(.py)` over two session binds all
/// return correct values). Un-ignored the same day.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn record_selector_on_bound_value_via_eff_path() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("data P = P { px :: Int, py :: Int }")
        .await
        .expect_ok("def record P");
    repl.eval("p <- pure (P { px = 1, py = 2 })")
        .await
        .expect_ok("bind p via record syntax");
    let out = repl.eval_ok("pure (py p)").await;
    assert!(out.contains('2'), "py p: expected 2, got: {out}");

    repl.close().await.expect_ok("close");
}

/// CASE 7 — class + instance: class exports with `(..)` so methods are visible.
///
/// FIX (BUG-C): the extractor now emits `EClass name [methods]` (not `EType name
/// []`), and render.rs renders `Class(..)` (not a bare head). This makes the
/// class methods visible when the instance gen module compiles.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn class_instance_describe() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("class Describe a where { describe :: a -> T.Text }")
        .await
        .expect_ok("def class Describe");
    repl.def("data Animal = Cat | Dog")
        .await
        .expect_ok("def data Animal");
    repl.def("instance Describe Animal where { describe Cat = T.pack \"cat\"; describe Dog = T.pack \"dog\" }")
        .await
        .expect_ok("def instance Describe Animal");

    let out = repl.eval_ok("pure (describe Cat)").await;
    assert!(
        out.contains("cat"),
        "describe Cat: expected \"cat\", got: {out}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 7 (no-poison) — class + data + instance all compile; describe works;
/// an unrelated eval after the instance is NOT poisoned (BUG-C fixed).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn class_instance_poisons_until_reset() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let c = repl
        .def("class Describe a where { describe :: a -> T.Text }")
        .await;
    eprintln!(
        "[case7] def class:    is_error={} text={}",
        c.is_error, c.text
    );
    c.expect_ok("def class Describe");

    let d = repl.def("data Animal = Cat | Dog").await;
    eprintln!(
        "[case7] def data:     is_error={} text={}",
        d.is_error, d.text
    );
    d.expect_ok("def data Animal");

    let i = repl
        .def("instance Describe Animal where { describe Cat = T.pack \"cat\"; describe Dog = T.pack \"dog\" }")
        .await;
    eprintln!(
        "[case7] def instance: is_error={} text={}",
        i.is_error, i.text
    );
    i.expect_ok("def instance Describe Animal");

    let out = repl.eval_ok("pure (describe Cat)").await;
    assert!(
        out.contains("cat"),
        "describe Cat: expected \"cat\", got: {out}"
    );

    // No poison: an unrelated eval still works after the instance.
    let ok = repl.eval_ok("pure (2 :: Int)").await;
    assert!(
        ok.contains('2'),
        "unrelated eval after class+instance: expected 2 (no poison), got: {ok}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 8 — decl/Prelude name collision: user decl shadows Prelude (BUG-7 fixed).
///
/// `over` is a `Control.Lens` re-export in the eval preamble's unqualified
/// `Tidepool.Prelude`. After BUG-7 fix: the session assembler extends the
/// `import Tidepool.Prelude hiding (error)` line to also hide every value-binder
/// the session has defined. So when the user defines `over`, the preamble
/// becomes `import Tidepool.Prelude hiding (error, over)`, and the eval module
/// resolves `over` unambiguously to the session-defined version. Expected: 6.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decl_prelude_collision_is_graceful() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // The gen module uses the lens-free standalone surface, so the def succeeds.
    let def = repl.def("over x = x + (1 :: Int)").await;
    eprintln!(
        "[case8] def over: is_error={} text={}",
        def.is_error, def.text
    );
    def.expect_ok("def over succeeds (gen module is lens-free)");

    // BUG-7 fix: preamble hides `over` from Prelude → user decl wins.
    let used = repl.eval("pure (over 5)").await;
    eprintln!(
        "[case8] eval over: is_error={} text={}",
        used.is_error, used.text
    );
    let out = used.expect_ok("user `over` shadows Prelude.over — no ambiguous-occurrence");
    assert!(
        out.contains('6'),
        "user decl wins: expected 6 (over 5 = 5 + 1), got: {out}"
    );

    // Session stays fully usable after the shadow eval.
    repl.def("noclash x = x + (1 :: Int)")
        .await
        .expect_ok("session still usable after shadow eval (def)");
    let ok = repl.eval_ok("pure (noclash 9)").await;
    assert!(ok.contains("10"), "post-shadow: expected 10, got: {ok}");

    repl.close().await.expect_ok("close");
}

/// CASE 9 — empty / garbage declarations: empty is a no-op, garbage fails cleanly.
///
/// RE-1 fix: `def ""` (empty) is a no-op — returns Ok without bumping the gen.
/// `def "@@@ not haskell"` → parse error, rejected cleanly. The session survives
/// both and a following good def + eval works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_and_garbage_decls_survive() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Capture gen before the empty def so we can assert it is unchanged after.
    let before = repl
        .cmd(":bindings")
        .await
        .expect_ok(":bindings before")
        .to_string();

    let empty = repl.def("").await;
    eprintln!(
        "[case9] def \"\":    is_error={} text={}",
        empty.is_error, empty.text
    );
    // RE-1 fix: empty def is a no-op — Ok, does not bump the generation.
    empty.expect_ok("def \"\" is a no-op (Ok, no gen bump)");

    let after = repl
        .cmd(":bindings")
        .await
        .expect_ok(":bindings after")
        .to_string();
    // Both :bindings responses contain `"generation":N`; they must agree.
    fn extract_gen(s: &str) -> Option<u64> {
        let key = "\"generation\":";
        let start = s.find(key)? + key.len();
        s[start..]
            .trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    }
    assert_eq!(
        extract_gen(&before),
        extract_gen(&after),
        "empty def must not bump the generation (before={before}, after={after})"
    );

    let garbage = repl.def("@@@ not haskell").await;
    eprintln!(
        "[case9] def garbage: is_error={} text={}",
        garbage.is_error, garbage.text
    );
    garbage.expect_err("garbage decl `@@@ not haskell` must be rejected cleanly");

    // The session survives: a following good def + eval works.
    repl.def("good x = x")
        .await
        .expect_ok("good def after bad decls");
    let out = repl.eval_ok("pure (good (7 :: Int))").await;
    assert!(
        out.contains('7'),
        "post-garbage good def: expected 7, got: {out}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 10 — a bad decl does NOT poison the log.
///
/// def `data = oops` (a parse error) → rejected; the decl log is left untouched
/// (`define` only appends after a successful binder extraction), so a following
/// `good2` def + use works and is not contaminated by the bad text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_decl_does_not_poison_log() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let bad = repl.def("data = oops").await;
    eprintln!(
        "[case10] def `data = oops`: is_error={} text={}",
        bad.is_error, bad.text
    );
    bad.expect_err("`data = oops` is a parse error → rejected, log untouched");

    repl.def("good2 x = x * (3 :: Int)")
        .await
        .expect_ok("good2 def after rejected bad decl");
    let out = repl.eval_ok("pure (good2 4)").await;
    assert!(
        out.contains("12"),
        "log not poisoned: good2 4 expected 12, got: {out}"
    );

    repl.close().await.expect_ok("close");
}
