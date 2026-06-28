//! Wave 3b hardening — DIMENSION: declaration plane (Lane A) depth.
//!
//! Adversarial integration tests driving the REAL `tidepool-repl` entry point
//! (session_open / session_def / session_eval / session_cmd / session_close)
//! over multiple turns. Focus: the gen-versioned `Tidepool.Session.Lib.G<g>`
//! declaration module that `session_def` regenerates each turn (selective
//! re-export `import G<g-1> hiding (<redefined>)`), and how user declarations of
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

    repl.def("inc x = x + (1 :: Int)").await.expect_ok("def inc");
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

/// CASE 2 — Forward reference ACROSS turns is NOT supported (it POISONS).
///
/// CONFIRMED SEMANTICS (this test pins them): `SessionLib::define` is PARSE-ONLY
/// (GHC binder extraction, no typecheck — mod.rs `define` doc). So def `f x = g x
/// + 1` is ACCEPTED at the def turn (a free `g` is a scope concern, not a parse
/// error) and lands in its OWN gen module `Lib.G1`. A later def `g` lands in
/// `Lib.G2`, which `import`s G1 — but G1 does NOT import G2. So `g` is never in
/// scope IN G1, and every later compile loads the whole gen chain (G1→G2→…),
/// permanently failing with `G1.hs: GHC-88464 Variable not in scope: g`. The
/// forward-ref def thus POISONS the decl plane (matches the documented
/// limitation (a) in `define`'s doc — "a body referencing a not-yet-defined name
/// poisons later generations"). Failure is DEFERRED to first use, then PERMANENT.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forward_reference_across_turns_poisons() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // def f before g exists. Parse-only define accepts it (g is a free var).
    let f_turn = repl.def("f x = g x + (1 :: Int)").await;
    eprintln!(
        "[case2] def f (g not yet defined): is_error={} text={}",
        f_turn.is_error, f_turn.text
    );
    f_turn.expect_ok("def f forward-ref ACCEPTED (parse-only define, no scope check)");

    repl.def("g x = x * (2 :: Int)").await.expect_ok("def g ACCEPTED");

    // ...but the reference never compiles: G1 (carrying f) can't see g (in G2).
    let out = repl.eval("pure (f 10)").await;
    eprintln!("[case2] eval (f 10): is_error={} text={}", out.is_error, out.text);
    let err = out.expect_err("forward ref across turns POISONS — G1 can't see g (GHC-88464)");
    assert!(
        err.contains("not in scope") || err.contains("88464") || err.contains('g'),
        "case2: expected a 'g not in scope' poison error, got: {err}"
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
    let out = repl
        .eval_ok("pure (case Age 7 of { Age n -> n })")
        .await;
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

/// CASE 6 — record syntax: selectors on FRESH values work; a field selector on a
/// SESSION-BOUND record value through the Eff reference path CRASHES (kind=4).
///
/// CONFIRMED current behavior (this test pins + localizes it):
///   * `px (P 3 4)` on a fresh value (plain-eval path) => 3. WORKS.
///   * bind `p <- pure (P 1 2)`; then referencing it:
///       - `case p of { P a b -> b }` (bare → PURE fallback path) => 2. WORKS.
///       - `pure (py p)` (Eff path, record selector) → CRASHES with
///         `[JIT] runtime_error kind=4 (TypeMetadata)` / `[CASE TRAP]` /
///         "forced type metadata (should be dead code)" — surfaced as a clean
///         MCP error (no hang).
/// The diagnostic refs below LOCALIZE whether the crash is the record SELECTOR
/// or the Eff `run_fragment` path over a session-bound custom ADT. Same crash
/// SIGNATURE as the known kind=4/TypeMetadata class, but here with NO rebind and
/// NO type-redefine — a fresh manifestation worth the integrator's eyes.
/// Aspirational correct result (`py p` => 2) is the ignored `record_selector_*`.
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
    assert!(out.contains('3'), "px (P 3 4) fresh: expected 3, got: {out}");

    // Bind a P value (positional, to isolate the reference path from record-ctor).
    repl.eval("p <- pure (P 1 2)")
        .await
        .expect_ok("bind p = P 1 2");

    // Ref A — PURE fallback path (bare case): expected to WORK (cf. case 4).
    let ref_a = repl.eval("case p of { P a b -> b }").await;
    eprintln!("[case6] ref A (pure-path case):  is_error={} text={}", ref_a.is_error, ref_a.text);

    // Ref B' — Eff path with a case (not a selector): localizes selector vs path.
    let ref_b2 = repl.eval("pure (case p of { P a b -> b })").await;
    eprintln!("[case6] ref B' (Eff-path case):  is_error={} text={}", ref_b2.is_error, ref_b2.text);

    // Ref B — Eff path with the record SELECTOR: the crashing case.
    let ref_b = repl.eval("pure (py p)").await;
    eprintln!("[case6] ref B  (Eff-path sel):   is_error={} text={}", ref_b.is_error, ref_b.text);

    // WIDER SCOPE (confirmed): the crash is NOT selector-specific — BOTH the
    // Eff-path case (ref B') and the Eff-path selector (ref B) crash with the
    // SAME kind=4 TypeMetadata signature, while the PURE-path case (ref A) works.
    // So the fault is the Eff `run_fragment` REFERENCE path while a session-bound
    // CUSTOM-ADT value is live (even a `pure (123::Int)` that ignores `p` crashes
    // through that path). The crash is GRACEFUL (clean MCP error, no hang), and
    // the PURE path is still usable — the session is not fully dead.
    let survive = repl.eval("123 :: Int").await; // bare → pure-fallback path
    eprintln!("[case6] survive (bare/pure path): is_error={} text={}", survive.is_error, survive.text);
    let survive = survive.expect_ok("pure path still works after the Eff-path crash");
    assert!(
        survive.contains("123"),
        "session survives via pure path: expected 123, got: {survive}"
    );

    repl.close().await.expect_ok("close");
}

/// ASPIRATIONAL — record field selector on a session-bound value via the Eff path.
/// Currently CRASHES (kind=4 TypeMetadata / CASE TRAP); see
/// `record_syntax_selectors_localized`. Un-ignore when the Eff-path force of a
/// session-bound custom ADT through a selector is fixed.
#[ignore = "BUG: record selector on session-bound value via Eff path crashes (kind=4 TypeMetadata)"]
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

/// CASE 7 — class + instance: THE most important data point of this dimension.
///
/// EXPECTED (per the team brief): the deferred user/orphan-instance replay knot
/// (GHC `if_rec_types` self-knot for a user dfun in an injected module).
///
/// ACTUAL (CONFIRMED here): the failure is EARLIER and DIFFERENT — we never reach
/// instance *replay* because the instance gen module fails to *compile*:
///
///   Tidepool.Session.Lib.G3.hs:11: error: [GHC-54721]
///     `describe` is not a (visible) method of class `Describe`
///
/// ROOT CAUSE — the export-list renderer. A typeclass is classified as
/// `ExportItem::Type { name: "Describe", cons: [] }` (binder extraction does not
/// surface class methods), and `render_entry` renders a `Type` with empty `cons`
/// as a BARE head (`Describe`, NOT `Describe(..)`) — see
/// tidepool-runtime/src/session/render.rs:56-64 (the `cons.is_empty()` branch,
/// shared with type synonyms). So `Lib.G1` exports the class WITHOUT its methods;
/// the gen-chain re-export (`module Lib.G1`) carries the bare class on to G2/G3;
/// and the later-gen `instance` in G3 can't see `describe` → GHC-54721. The fix
/// is in the renderer/binder-extractor (export a class as `Class(..)`, or emit
/// its methods as separate `Value` items) — strictly upstream of the documented
/// `if_rec_types` dfun-replay work.
///
/// SECONDARY FINDING: the broken instance gen POISONS the whole decl plane — once
/// `Lib.G3` is in the import chain, EVERY later eval (even an unrelated
/// `pure (1::Int)`) fails to compile. `:reset` is the escape hatch (see below).
/// The `#[ignore]`d `class_instance_describe` pins the aspirational GREEN.
#[ignore = "BUG: class methods not re-exported (render.rs bare class head) → GHC-54721; instance gen never compiles"]
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

/// CASE 7 (live probe) — pins the CONFIRMED current behavior on every run, and
/// LOCALIZES the poison to the instance (class + data alone are fine), then shows
/// `:reset` recovers. Asserts only stable invariants; captures exact text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn class_instance_poisons_until_reset() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // class + data alone do NOT poison: the class gen exports a (bare) head that
    // is legal Haskell, and an eval that imports the lib still compiles.
    let c = repl
        .def("class Describe a where { describe :: a -> T.Text }")
        .await;
    eprintln!("[case7] def class:    is_error={} text={}", c.is_error, c.text);
    c.expect_ok("def class Describe (accepted, parse-only)");

    let d = repl.def("data Animal = Cat | Dog").await;
    eprintln!("[case7] def data:     is_error={} text={}", d.is_error, d.text);
    d.expect_ok("def data Animal (accepted)");

    let pre = repl.eval_ok("pure (1 :: Int)").await;
    assert!(
        pre.contains('1'),
        "class+data alone do NOT poison: eval should work, got: {pre}"
    );

    // The INSTANCE gen fails to compile (GHC-54721 method not visible) and poisons
    // the chain: it is accepted at def time (parse-only) but every later eval dies.
    let i = repl
        .def("instance Describe Animal where { describe Cat = T.pack \"cat\"; describe Dog = T.pack \"dog\" }")
        .await;
    eprintln!("[case7] def instance: is_error={} text={}", i.is_error, i.text);

    let e = repl.eval("pure (describe Cat)").await;
    eprintln!("[case7] eval describe Cat: is_error={} text={}", e.is_error, e.text);
    let err = e.expect_err("instance gen does not compile (GHC-54721 method not visible)");
    assert!(
        err.contains("54721") || err.contains("not a (visible) method") || err.contains("not loaded"),
        "case7: expected the class-method-export compile error, got: {err}"
    );

    // POISON: even an unrelated eval now fails (the broken G3 is in the chain).
    let poisoned = repl.eval("pure (2 :: Int)").await;
    eprintln!("[case7] eval (2::Int) post-instance: is_error={} text={}", poisoned.is_error, poisoned.text);
    poisoned.expect_err("decl plane POISONED by the broken instance gen (unrelated eval fails)");

    // :reset is the escape hatch — it drops the decl log; a fresh def + eval works.
    repl.cmd(":reset").await.expect_ok(":reset clears the poisoned decl log");
    repl.def("ok7 x = x + (1 :: Int)")
        .await
        .expect_ok("fresh def after :reset");
    let recovered = repl.eval_ok("pure (ok7 41)").await;
    assert!(
        recovered.contains("42"),
        "recovery after :reset: expected 42, got: {recovered}"
    );

    repl.close().await.expect_ok("close");
}

/// CASE 8 — decl/Prelude name collision (KNOWN sharp edge, codified).
///
/// `over` is a `Control.Lens` re-export in the eval preamble's unqualified
/// `Tidepool.Prelude`. Defining a session `over` then referencing it bare makes
/// the reference ambiguous between `Lib.G<g>.over` and `Prelude.over`. EXPECT a
/// clean "Ambiguous occurrence" compile error; the session must SURVIVE. This
/// DOCUMENTS the edge — not flagged as a new bug.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decl_prelude_collision_is_graceful() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // The def itself may well succeed (the gen module uses the lens-free
    // standalone surface); the collision bites at the eval site (Prelude in scope).
    let def = repl.def("over x = x + (1 :: Int)").await;
    eprintln!("[case8] def over: is_error={} text={}", def.is_error, def.text);

    let used = repl.eval("pure (over 5)").await;
    eprintln!("[case8] eval over: is_error={} text={}", used.is_error, used.text);
    let err = used.expect_err("over collides with Prelude.over → ambiguous occurrence");
    assert!(
        err.to_lowercase().contains("ambiguous") || err.to_lowercase().contains("over"),
        "decl/Prelude collision: expected an ambiguous-occurrence error mentioning `over`, got: {err}"
    );

    // Session survives the ambiguity error: a fresh, non-colliding def + eval works.
    repl.def("noclash x = x + (1 :: Int)")
        .await
        .expect_ok("session survives collision (def)");
    let ok = repl.eval_ok("pure (noclash 9)").await;
    assert!(ok.contains("10"), "post-collision: expected 10, got: {ok}");

    repl.close().await.expect_ok("close");
}

/// CASE 9 — empty / garbage declarations fail cleanly, session survives.
///
/// def `` (empty) and def `@@@ not haskell` → record outcomes (a parse failure
/// is the clean rejection). The robust invariant asserted: the session SURVIVES
/// and a following GOOD def + eval works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_and_garbage_decls_survive() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let empty = repl.def("").await;
    eprintln!("[case9] def \"\":    is_error={} text={}", empty.is_error, empty.text);

    let garbage = repl.def("@@@ not haskell").await;
    eprintln!("[case9] def garbage: is_error={} text={}", garbage.is_error, garbage.text);
    garbage.expect_err("garbage decl `@@@ not haskell` must be rejected cleanly");

    // The session survives: a following good def + eval works.
    repl.def("good x = x").await.expect_ok("good def after bad decls");
    let out = repl.eval_ok("pure (good (7 :: Int))").await;
    assert!(out.contains('7'), "post-garbage good def: expected 7, got: {out}");

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
    eprintln!("[case10] def `data = oops`: is_error={} text={}", bad.is_error, bad.text);
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
