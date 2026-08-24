//! JIT surface coverage — executable probes that PIN the behavior of the
//! Tidepool eval surface exactly as the MCP user sees it. Two classes:
//!
//!   1. WORKS — a capability that must hold: its value is asserted verbatim
//!      (sum/Floating/round/even-odd/nub/Integer-defaulting/insertWith, the
//!      infinite-list idioms, the safe-head/wither/Category surface, …). A
//!      failure here is a regression in a shipped capability.
//!   2. FAILS-LOUDLY — an unsupported input (read/Integer-GMP, a near-DBL_MAX
//!      Double literal, `cycle`, non-tail-recursion overflow, `let`-in-braced-
//!      `do`, a giant lens fold) must fail with a CLEAN, named error — never a
//!      silent SIGILL/SIGSEGV or wrong output. Each such probe asserts the
//!      error text carries the expected marker.
//!
//! Harness mirrors `fmt_nonfinite.rs`: full MCP preamble (`build_preamble` +
//! `template_haskell`) + `compile_and_run`, on a 64 MiB thread with signal
//! safety installed so any HARD crash surfaces as a catchable thread panic
//! (turning a would-be silent SIGSEGV into a visible test failure / bug-find)
//! rather than aborting the test binary.
//!
//! Each probe is one eval. Because the MCP server renders results through this
//! exact `tidepool-runtime` `render.rs` path, the JSON pinned below is what a
//! real `tidepool` eval prints — these assertions ARE the user-visible
//! contract.
//!
//! # Family bundles vs standalone probes
//!
//! One compile per trivial pure probe used to be the norm here (~97 `#[test]`
//! fns, ~97 `tidepool-extract` spawns). Most of that long tail is now bundled:
//! one `#[test]` per stdlib module family (Prelude core, Show, Map, Aeson,
//! Aeson-generic-derive, Lens, Text, the safe-idiom surface, Data.Time, the
//! `[j|]`/`[fmt|]` quoters, FilePath), each running ONE eval that returns a
//! LIST OF FAILED CHECK NAMES via the `check` idiom (mirrors
//! `generic_form_roundtrip.rs`'s own idiom):
//!
//! ```haskell
//! check :: Text -> Bool -> [Text]
//! check nm ok = if ok then [] else [nm]
//! -- pure (concat [ check "name1" (bool1), check "name2" (bool2), ... ])
//! ```
//!
//! An empty list means every check passed; a non-empty list names exactly
//! which absorbed probe(s) regressed. **Add a new Prelude/stdlib function's
//! coverage by adding a `check` line to the relevant family bundle below**
//! (or a standalone probe when one of the exclusion classes below applies —
//! `haskell/CLAUDE.md`'s "Adding new Prelude functions" says the same).
//!
//! A probe stays STANDALONE (its own `#[test]`, own compile) only when it
//! falls into one of these classes — bundling any of these would either
//! break the check-list idiom (an eval that itself fails to compile/run
//! can't return a check list) or silently weaken what's actually being
//! tested:
//!
//!   (a) SANCTIONED-RED — currently fails by design, must keep failing
//!       individually, never inside a green bundle (see
//!       `plans/post-restart/gate-runbook.md`'s never-green list). No probe
//!       is in this class today: both former members —
//!       `qq_fmt_brace_inside_hole_non_string_expr_still_works` (fixed by
//!       giving `Tidepool.QQ.HsMeta.Translate.toExp` a `let`-in-hole case)
//!       and `works_from_json_float` (fixed by giving the extract translator
//!       a `decodeFloat_Int#` 2-result split mirroring the already-working
//!       `decodeDouble_Int64#` path) — are plain WORKS probes now, kept
//!       standalone under classes (d) and (g) respectively, not because
//!       they're still expected to fail.
//!   (b) COMPILE-FAIL probes — anything asserting a compile-time ERROR.
//!   (c) EFFECTS/DISPATCH probes — anything exercising a real dispatcher
//!       (not `NullDispatcher`); bundling would change dispatch
//!       interleaving.
//!   (d) DISTINCT-MECHANISM probes — pin a compiler/runtime mechanism
//!       (ConTags resolution, VarId disambiguation, TCO/call-depth) rather
//!       than a stdlib function's JIT-safety.
//!   (e) `works_stdlib_quoter_survives_extract` — the QQ-survival proof,
//!       load-bearing coverage, kept standalone by explicit instruction.
//!
//! Plus two classes not in the original five, both a "when in doubt, leave
//! standalone" extension of the same principle:
//!
//!   (f) RUNTIME fails_loudly probes — the eval itself errors, which is
//!       structurally incompatible with a bundle whose eval must SUCCEED to
//!       return a check list.
//!   (g) RENDER-FIDELITY probes — the property under test is the exact shape
//!       of the outer Rust `Value::to_json()` render (huge-integer/Double
//!       digit-exact fidelity), not anything a Haskell-side `==` can
//!       observe. Reducing these to a Haskell boolean would test a WEAKER
//!       property than the original. (`works_moderate_double_literals`,
//!       `works_exact_int_json`, `qq_json_exact_large_integer_literal`,
//!       `large_double_literal_on_jit`.)
//!
//! When in doubt, leave a probe standalone — the win is in the long tail of
//! trivial pure probes, not in forcing every last one into a bundle.

// `1.41421…` (sqrt 2) and `3.14` are pinned round-trip eval outputs, not math
// constants — assert them verbatim.
#![allow(clippy::approx_constant)]

use std::io::Write;
use std::path::Path;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext, Response};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;
use tidepool_testing::NullDispatcher;

/// `Fork`'s position in the stack `eval_with_dispatch` compiles against: 8
/// base effects at tags 0..7, then the interposed `Ask` (8), `RunLLMTurn`
/// (9), and `Fork` (10) — `standard_decls()`'s own order (base8 + Ask +
/// RunLLMTurn) with `Fork` appended explicitly, same as the harness's
/// `agent_decls` (`standard_decls()` itself stops at `RunLLMTurn` — the
/// ordinary session engine never services `Fork`).
///
/// `Tidepool.Fork`'s `forkFilter`/`forkMap` reach the machine through
/// `forkAllSited`, which sends on `Fork` — so their fanout dispatch arrives
/// at THIS tag, not `RunLLMTurn`'s and not `Ask`'s.
const FORK_TAG: u64 = 10;

/// Compile `code` (a single Haskell expression of type `M a`) under the full
/// MCP preamble and run it. Returns `Ok(json)` with the rendered result or
/// `Err(text)` with the failure message (compile error OR runtime yield error).
fn eval_raw(code: &str) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, "", "", None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    let include = [
        hs.as_path(),
        lib.as_path(),
        effects_dir.core.as_path(),
        effects_dir.shim.as_path(),
    ];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        // `Display` on `RuntimeError`/`CompileError::Diagnostics` is a terse
        // structural summary now (structured spans, not rendered text) — the
        // classifier's message is the joined diagnostic text these probes
        // actually assert markers against.
        Err(e) => Err(tidepool_runtime::classify(&e).message),
    }
}

/// Run a probe on the shared `EVAL_STACK_SIZE` (same as the MCP server's eval thread, so test and server can't drift) with signal safety installed. A hard
/// crash (uncaught SIGSEGV/SIGILL — i.e. a STILL-SILENT footgun) becomes a
/// thread panic, reported here as an `Err` rather than killing the process.
fn run_probe(code: &str) -> Result<serde_json::Value, String> {
    let code = code.to_string();
    std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            eval_raw(&code)
        })
        .unwrap()
        .join()
        .map_err(|_| {
            "thread panicked (HARD crash / uncaught signal — possible STILL-SILENT footgun)"
                .to_string()
        })?
}

/// Assert a probe succeeds and returns `expected`. Used for the "stale fear
/// that now works" class — a failure here is a REGRESSION.
fn works(code: &str, expected: serde_json::Value) {
    match run_probe(code) {
        Ok(got) => assert_eq!(
            got, expected,
            "\nWORKS probe returned the wrong value:\n  code: {code}\n  want: {expected}\n  got:  {got}"
        ),
        Err(e) => panic!(
            "\nWORKS probe REGRESSED (was supposed to succeed):\n  code: {code}\n  error: {e}"
        ),
    }
}

/// Assert a probe FAILS and the error text contains `marker`. Used for the
/// "must fail cleanly" class — a success here means the footgun silently
/// changed shape (re-pin it), and a missing marker means the error is no longer
/// the clean, named one we promise (possible silent-crash bug-find).
fn fails_loudly(code: &str, marker: &str) {
    match run_probe(code) {
        Ok(v) => panic!(
            "\nLOUD-FAIL probe unexpectedly SUCCEEDED:\n  code: {code}\n  got:  {v}\n  \
             (the footgun changed — verify it still fails, then re-pin this probe)"
        ),
        Err(e) => assert!(
            e.contains(marker),
            "\nLOUD-FAIL probe failed but WITHOUT the expected clean marker:\n  code: {code}\n  \
             want marker: {marker:?}\n  error: {e}\n  \
             (if this is a silent SIGILL/SIGSEGV or wrong shape, it's a BUG-FIND — report it)"
        ),
    }
}

/// As `fails_loudly`, but also asserts the failure classifies as
/// `FailureClass::Runtime`/`Phase::Run` — never `UserHaskell`/`Phase::Compile`.
/// For probes pinning the runtime-vs-compile failure story: e.g. the base
/// partials (2026-08-24 partials-allowed ruling) must fail the BLOCK at
/// runtime, not the compile, so a caller routing on the failure class is not
/// misled into "rewrite your code".
fn fails_loudly_as_runtime(code: &str, marker: &str) {
    let code = code.to_string();
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, &code, "", "", None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    let got = std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            let include = [
                hs.as_path(),
                lib.as_path(),
                effects_dir.core.as_path(),
                effects_dir.shim.as_path(),
            ];
            let mut d = NullDispatcher;
            compile_and_run(&src, "result", &include, &mut d, &())
        })
        .unwrap()
        .join()
        .expect("thread panicked (HARD crash / uncaught signal / possible STILL-SILENT footgun)");
    match got {
        Ok(v) => panic!(
            "\nLOUD-FAIL probe unexpectedly SUCCEEDED:\n  code: {code}\n  got:  {:?}\n  \
             (the footgun changed — verify it still fails, then re-pin this probe)",
            v.to_json()
        ),
        Err(e) => {
            let env = tidepool_runtime::classify(&e);
            assert_eq!(
                env.class,
                tidepool_runtime::FailureClass::Runtime,
                "\nexpected a RUNTIME failure, not {:?}:\n  code: {code}\n  message: {}",
                env.class,
                env.message
            );
            assert_eq!(
                env.phase,
                tidepool_runtime::Phase::Run,
                "\nexpected Run phase, not {:?}:\n  code: {code}\n  message: {}",
                env.phase,
                env.message
            );
            assert!(
                env.message.contains(marker),
                "\nLOUD-FAIL probe failed but WITHOUT the expected clean marker:\n  code: {code}\n  \
                 want marker: {marker:?}\n  error: {}\n  \
                 (if this is a silent SIGILL/SIGSEGV or wrong shape, it's a BUG-FIND — report it)",
                env.message
            );
        }
    }
}

/// As `eval_raw`, but injects extra `import` lines (one per line of `imports`).
fn eval_raw_with_imports(imports: &str, code: &str) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, imports, "", None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    let include = [
        hs.as_path(),
        lib.as_path(),
        effects_dir.core.as_path(),
        effects_dir.shim.as_path(),
    ];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        Err(e) => Err(tidepool_runtime::classify(&e).message),
    }
}

/// As `fails_loudly`, but injects extra `import` lines first (mirrors
/// `works_with_imports` alongside `eval_raw_with_imports`, including running
/// on the same stack-sized, signal-safe thread) — for probes that need a
/// quoter import (e.g. `"Tidepool.QQ (fmt, j)"`, no `import` keyword) to even
/// parse.
fn fails_loudly_with_imports(imports: &str, code: &str, marker: &str) {
    let imports = imports.to_string();
    let code = code.to_string();
    let imports_t = imports.clone();
    let code_t = code.clone();
    let got = std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            eval_raw_with_imports(&imports_t, &code_t)
        })
        .unwrap()
        .join()
        .map_err(|_| {
            "thread panicked (HARD crash / uncaught signal / possible STILL-SILENT footgun)"
                .to_string()
        })
        .and_then(|r| r);
    match got {
        Ok(v) => panic!(
            "\nLOUD-FAIL probe unexpectedly SUCCEEDED:\n  code: {code}\n  got:  {v}\n  \
             (the footgun changed — verify it still fails, then re-pin this probe)"
        ),
        Err(e) => assert!(
            e.contains(marker),
            "\nLOUD-FAIL probe failed but WITHOUT the expected clean marker:\n  code: {code}\n  \
             want marker: {marker:?}\n  error: {e}\n  \
             (if this is a silent SIGILL/SIGSEGV or wrong shape, it's a BUG-FIND — report it)"
        ),
    }
}

fn works_with_imports(imports: &str, code: &str, expected: serde_json::Value) {
    let imports = imports.to_string();
    let code = code.to_string();
    let imports_t = imports.clone();
    let code_t = code.clone();
    let got = std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            eval_raw_with_imports(&imports_t, &code_t)
        })
        .unwrap()
        .join()
        .map_err(|_| "thread panicked (HARD crash / uncaught signal)".to_string())
        .and_then(|r| r);
    match got {
        Ok(got) => assert_eq!(
            got, expected,
            "\nWORKS probe returned the wrong value:\n  code: {code}\n  want: {expected}\n  got:  {got}"
        ),
        Err(e) => panic!("\nWORKS probe REGRESSED (was supposed to succeed):\n  code: {code}\n  error: {e}"),
    }
}

/// As `eval_raw_with_imports`, but also splices `helpers` (extra top-level
/// declarations — a local `data` type a probe needs) into the generated
/// module.
fn eval_raw_with_helpers(
    imports: &str,
    helpers: &str,
    code: &str,
) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, imports, helpers, None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    let include = [
        hs.as_path(),
        lib.as_path(),
        effects_dir.core.as_path(),
        effects_dir.shim.as_path(),
    ];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        Err(e) => Err(tidepool_runtime::classify(&e).message),
    }
}

fn works_with_helpers(helpers: &str, code: &str, expected: serde_json::Value) {
    let helpers = helpers.to_string();
    let code = code.to_string();
    let helpers_t = helpers.clone();
    let code_t = code.clone();
    let got = std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            eval_raw_with_helpers("", &helpers_t, &code_t)
        })
        .unwrap()
        .join()
        .map_err(|_| "thread panicked (HARD crash / uncaught signal)".to_string())
        .and_then(|r| r);
    match got {
        Ok(got) => assert_eq!(
            got, expected,
            "\nWORKS probe returned the wrong value:\n  code: {code}\n  want: {expected}\n  got:  {got}"
        ),
        Err(e) => panic!("\nWORKS probe REGRESSED (was supposed to succeed):\n  code: {code}\n  error: {e}"),
    }
}

/// `[j|…|]`/`[fmt|…|]` are NOT auto-imported on this raw `template_haskell`
/// path (that injection is the live MCP server request handler's job, which
/// this harness bypasses) — every probe using them needs an explicit
/// `Tidepool.QQ (fmt, j)` import, same as `works_stdlib_quoter_survives_extract`
/// and `render/fmt_spec_reject.rs`/`render/fmt_nonfinite.rs` elsewhere in this
/// suite.
const QQ_IMPORTS: &str = "Tidepool.QQ (fmt, j)";

// =========================================================================
// FAMILY BUNDLES — one eval per stdlib module family, via the check-list
// idiom (`check :: Text -> Bool -> [Text]`, empty result = every absorbed
// probe's assertion still holds). See the module doc for the mapping
// principle and the exclusion classes that stay standalone below.
// =========================================================================

/// Prelude core: error-worker folds, nub, Floating ops, banker's rounding,
/// even/odd, untyped-helper Integer defaulting, lazy-safe takeWhile/span,
/// the `cycle` knot-tie, bounded consumption of infinite producers
/// (enumFrom/repeat/iterate, zipWith/filter/map), and `read`.
///
/// Absorbed: works_error_worker_folds, works_nub_dedup, works_floating_ops,
/// works_round_bankers, works_even_odd, works_integer_defaulting_untyped_helper,
/// works_lazy_safe_combinators, works_cycle_value_knot,
/// infinite_list_take_on_jit, infinite_list_transform_on_jit, read_on_jit,
/// read_double_on_jit.
#[test]
fn works_prelude_core_family() {
    works(
        r#"pure (concat
            [ check "error_worker_folds.sum" (sum [1..100::Int] == 5050)
            , check "error_worker_folds.product" (product [1..5::Int] == 120)
            , check "error_worker_folds.maximum" (maximum [3,1,4,1,5,9,2,6::Int] == 9)
            , check "error_worker_folds.minimum" (minimum [3,1,4,1,5,9::Int] == 1)
            , check "error_worker_folds.foldr1" (foldr1 (+) [1,2,3,4::Int] == 10)
            , check "base_partials.head" (head [1,2,3::Int] == 1)
            , check "base_partials.tail" (tail [1,2,3::Int] == [2,3])
            , check "base_partials.last" (last [1,2,3::Int] == 3)
            , check "base_partials.init" (init [1,2,3::Int] == [1,2])
            , check "base_partials.index" ([10,20,30::Int] !! 1 == 20)
            , check "base_partials.foldl1" (foldl1 (-) [10,1,2::Int] == 7)
            , check "base_partials.fromJust" (fromJust (Just (5::Int)) == 5)
            , check "nub_dedup" (nub [1,1,2,3,3,2::Int] == [1,2,3])
            , check "floating_ops.sqrt2" (sqrt (2.0::Double) == 1.4142135623730951)
            , check "floating_ops.exp0" (exp (0.0::Double) == 1.0)
            , check "floating_ops.log1" (log (1.0::Double) == 0.0)
            , check "round_bankers" (map (\d -> round d :: Int) [0.5, 1.5, 2.5, 3.5 :: Double] == [0,2,2,4])
            , check "even_odd.evens" (map even [1,2,3,4::Int] == [False,True,False,True])
            , check "even_odd.odds" (map odd [1,2,3,4::Int] == [True,False,True,False])
            , check "integer_defaulting_untyped_helper" (fac 10 == 3628800)
            , check "lazy_safe_combinators.takeWhile" (takeWhile (< 5) (enumFromTo 1 100 :: [Int]) == [1,2,3,4])
            , check "lazy_safe_combinators.span" (span (< 3) [1,2,3,4,5::Int] == ([1,2],[3,4,5]))
            , check "cycle_value_knot.cyc" (take 5 (cycle [1,2,3::Int]) == [1,2,3,1,2])
            , check "cycle_value_knot.qual" ((take 4 (P.cycle "ab") :: String) == "abab")
            , check "infinite_list_take.enumFrom" ((take 3 [0..] :: [Int]) == [0,1,2])
            , check "infinite_list_take.repeat" (take 3 (repeat (7::Int)) == [7,7,7])
            , check "infinite_list_take.iterate" (take 4 (iterate (*2) (1::Int)) == [1,2,4,8])
            , check "infinite_list_transform.zipWith" (zipWith (\a b -> a + b) [10,20,30::Int] [0..] == [10,21,32])
            , check "infinite_list_transform.filter" ((take 3 (filter even [0..]) :: [Int]) == [0,2,4])
            , check "infinite_list_transform.map" ((take 4 (map (*2) [0..]) :: [Int]) == [0,2,4,6])
            , check "read_on_jit" ((P.read "42" :: Int) == 42)
            , check "read_double_on_jit" ((P.read "42.5" :: Double) == 42.5)
            ])
         where { check nm ok = if ok then [] else [nm]; fac n = if n <= 1 then 1 else n * fac (n-1) }"#,
        serde_json::json!([]),
    );
}

/// `show` precedence (negative-Double parenthesization) and the
/// `showDouble` monomorphic shadow. Both compare Haskell `Text` (from
/// `show`), not outer JSON numeric encoding — safe to bundle despite
/// involving Doubles.
///
/// Absorbed: works_show_negative_double_parens, works_show_double_monomorphic_shadow.
#[test]
fn works_show_family() {
    works(
        r#"pure (concat
            [ check "show_negative_double_parens.nested" (show (Just (-2.5 :: Double)) == ("Just (-2.5)" :: Text))
            , check "show_negative_double_parens.top" (show (-2.5 :: Double) == ("-2.5" :: Text))
            , check "show_negative_double_parens.pos" (show (Just (1.5 :: Double)) == ("Just 1.5" :: Text))
            , check "show_negative_double_parens.int" (show (Just (-1 :: Int)) == ("Just (-1)" :: Text))
            , check "show_negative_double_parens.list" (show [Just (-2.5 :: Double), Nothing] == ("[Just (-2.5),Nothing]" :: Text))
            , check "show_double_monomorphic_shadow.pi" (show (3.14 :: Double) == ("3.14" :: Text))
            , check "show_double_monomorphic_shadow.one" (show (1.0 :: Double) == ("1.0" :: Text))
            , check "show_double_monomorphic_shadow.big" (show (1.0e10 :: Double) == ("1.0e10" :: Text))
            , check "show_double_monomorphic_shadow.neg" (show (-2.5 :: Double) == ("-2.5" :: Text))
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

/// `Data.Map.Strict` shadows: combining insert, `fromListWith` defaulting,
/// and both large-sorted-input scale probes.
///
/// Absorbed: works_map_insertwith, works_map_fromlistwith_default_resolves,
/// works_map_fromlist_large_sorted_after_depth_fix,
/// works_map_fromlistwith_large_sorted_safe.
#[test]
fn works_map_family() {
    works(
        r#"pure (concat
            [ check "map_insertwith" (Map.insertWith (+) ("a"::Text) (1::Int) (Map.fromList [("a",10),("b",2)]) == Map.fromList [("a",11),("b",2)])
            , check "map_fromlistwith_default_resolves" (Map.fromListWith (+) [("k", 1)] == Map.fromList [("k",1)])
            , check "map_fromlist_large_sorted_after_depth_fix" (Map.size (Map.fromList [(i, i) | i <- [1..12000 :: Int]]) == 12000)
            , check "map_fromlistwith_large_sorted_safe" (Map.size (Map.fromListWith const [(i, i) | i <- [1..12000 :: Int]]) == 12000)
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

/// A user-supplied `imports`-param qualified import resolves end to end
/// under a FRESH alias (`M`, distinct from the preamble's own `Map.`) —
/// pins the eval-imports-grammar contract (`tidepool_mcp::normalize_import_line`)
/// at the compile boundary: whichever canonical spelling a caller uses
/// (`qualified Data.Map.Strict as M`, its post-qualified twin, or either
/// with a leading `import ` keyword) normalizes to this same pre-qualified
/// line, so proving this ONE canonical form compiles and resolves pins all
/// of them.
#[test]
fn works_qualified_import_family() {
    works_with_imports(
        "qualified Data.Map.Strict as M",
        r#"pure (concat
            [ check "qualified_import_alias_resolves" (M.fromList [(1::Int, "a"::Text)] == M.singleton 1 "a")
            , check "qualified_import_alias_lookup" (M.lookup 1 (M.fromList [(1::Int, "a"::Text)]) == Just "a")
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

/// Aeson decode/lens-prism family: `fromJSON`/`eitherDecode`/`decode`,
/// bounded and unbounded numeric FromJSON instances (Int/Word/Integer/
/// Float/Char/()/tuples/Either), the `_Int`/`_Integer` prisms, and the
/// `[Char]`/`String` overlapping-instance pair (both directions). Aggregate
/// JSON values are compared via `renderJson` (Haskell-side Text, not the
/// outer Rust `to_json()` boundary — see `works_exact_int_json`'s carve-out
/// below for why that distinction matters).
///
/// Absorbed: works_from_json, works_either_decode, works_decode,
/// works_from_json_int_exact, works_from_json_int_rejects_fraction,
/// works_from_json_int_rejects_out_of_range, works_lens_int_floors,
/// works_lens_int_out_of_range_is_nothing, works_lens_integer_floors_and_is_unbounded,
/// works_from_json_string_overlapping_spike, works_from_json_char,
/// works_from_json_integer, works_from_json_word, works_from_json_unit,
/// works_from_json_tuples, works_from_json_either, works_to_json_string_overlapping.
///
/// `lens_int_floors`/`lens_integer_floors` were repinned (sibling audit for
/// `stdlib_regressions_02_medium::works_int_prism_floors_not_truncates`):
/// `_Int`/`_Integer` FLOOR a fractional Double toward negative infinity
/// (`"-3.7"` -> `-4`), matching upstream lens-aeson, not truncate toward
/// zero (`-3`) as these checks previously (incorrectly) pinned.
#[test]
fn works_aeson_family() {
    works(
        r#"pure (concat
            [ check "from_json.list_sum" ((case (fromJSON (toJSON [1,2,3::Int]) :: Result [Int]) of { Success xs -> sum xs; Error _ -> (-1) }) == 6)
            , check "from_json.mismatch_is_error" (case (fromJSON (toJSON ("hi"::Text)) :: Result Int) of { Success _ -> False; Error _ -> True })
            , check "either_decode.valid" (renderJson (case (eitherDecode "[1,2,3]" :: Either Text Value) of { Right v -> v; Left _ -> Null }) == "[1,2,3]")
            , check "either_decode.malformed_nonempty_error" (case (eitherDecode "{oops" :: Either Text Value) of { Left e -> T.length e > 0; Right _ -> False })
            , check "decode.valid_object" (renderJson (case (decode "{\"a\":1}" :: Maybe Value) of { Just v -> v; Nothing -> Null }) == "{\"a\":1}")
            , check "decode.malformed_nothing" (case (decode "nope" :: Maybe Value) of { Nothing -> True; Just _ -> False })
            , check "from_json_int_exact" ((case (eitherDecode "42" :: Either Text Int) of { Right i -> i; Left _ -> -999 }) == 42)
            , check "from_json_int_rejects_fraction.neg" (either (const True) (const False) (eitherDecode "-3.7" :: Either Text Int))
            , check "from_json_int_rejects_fraction.pos" (either (const True) (const False) (eitherDecode "3.7" :: Either Text Int))
            , check "from_json_int_rejects_out_of_range" (either (const True) (const False) (eitherDecode "99999999999999999999999999" :: Either Text Int))
            , check "lens_int_floors.neg" ((fromMaybe (-999) ((decode "-3.7" :: Maybe Value) >>= (^? _Int))) == (-4))
            , check "lens_int_floors.pos" ((fromMaybe (-999) ((decode "10.5" :: Maybe Value) >>= (^? _Int))) == 10)
            , check "lens_int_out_of_range_is_nothing" (not (isJust ((decode "99999999999999999999999999" :: Maybe Value) >>= (^? _Int))))
            , check "lens_integer_floors.neg" ((fromMaybe (-999) ((decode "-3.7" :: Maybe Value) >>= (^? _Integer))) == (-4))
            , check "lens_integer_unbounded" ((fromMaybe "MISSING" (show <$> ((decode "123456789012345678901234567890" :: Maybe Value) >>= (^? _Integer)))) == "123456789012345678901234567890")
            , check "from_json_string_overlapping_spike.s" ((either (const "ERR") id (eitherDecode "\"hi\"" :: Either Text String)) == "hi")
            , check "from_json_string_overlapping_spike.xs" ((either (const []) id (eitherDecode "[1,2,3]" :: Either Text [Int])) == [1,2,3])
            , check "from_json_char.ok" ((either (const '?') id (eitherDecode "\"a\"" :: Either Text Char)) == 'a')
            , check "from_json_char.tooLong" (either (const True) (const False) (eitherDecode "\"ab\"" :: Either Text Char))
            , check "from_json_char.empty" (either (const True) (const False) (eitherDecode "\"\"" :: Either Text Char))
            , check "from_json_integer.ok" ((show (either (const (-1 :: Integer)) id (eitherDecode "99999999999999999999999999" :: Either Text Integer))) == "99999999999999999999999999")
            , check "from_json_integer.fraction" (either (const True) (const False) (eitherDecode "3.7" :: Either Text Integer))
            , check "from_json_word.ok" ((either (const (0::Word)) id (eitherDecode "42" :: Either Text Word)) == 42)
            , check "from_json_word.negative" (either (const True) (const False) (eitherDecode "-1" :: Either Text Word))
            , check "from_json_word.fraction" (either (const True) (const False) (eitherDecode "3.7" :: Either Text Word))
            , check "from_json_unit.ok" (either (const False) (const True) (eitherDecode "null" :: Either Text ()))
            , check "from_json_unit.emptyArray" (either (const True) (const False) (eitherDecode "[]" :: Either Text ()))
            , check "from_json_unit.notNull" (either (const True) (const False) (eitherDecode "{}" :: Either Text ()))
            , check "from_json_tuples.ok" ((either (const (0::Int,0::Int)) id (eitherDecode "[1,2]" :: Either Text (Int, Int))) == (1,2))
            , check "from_json_tuples.wrongArity" (either (const True) (const False) (eitherDecode "[1,2,3]" :: Either Text (Int, Int)))
            , check "from_json_tuples.fiveOk" ((either (const [0,0,0,0,0::Int]) (\(a,b,c,d,e) -> [a,b,c,d,e]) (eitherDecode "[1,2,3,4,5]" :: Either Text (Int, Int, Int, Int, Int))) == [1,2,3,4,5])
            , check "from_json_either.left" ((case (eitherDecode "{\"Left\":1}" :: Either Text (Either Int Text)) of { Right (Left i) -> i; _ -> -999 }) == 1)
            , check "from_json_either.right" ((case (eitherDecode "{\"Right\":\"hi\"}" :: Either Text (Either Int Text)) of { Right (Right t) -> t; _ -> "ERR" }) == "hi")
            , check "from_json_either.badShape" (either (const True) (const False) (eitherDecode "{\"Wrong\":1}" :: Either Text (Either Int Text)))
            , check "to_json_string_overlapping.s" (renderJson (toJSON ("hi"::String)) == "\"hi\"")
            , check "to_json_string_overlapping.xs" (renderJson (toJSON ([1,2,3]::[Int])) == "[1,2,3]")
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

/// Generic FromJSON derivation over LOCAL sum types (needs top-level `data`
/// decls, hence `works_with_helpers` rather than a `where` clause): the
/// all-nullary bare-string-tag default, and the mixed-constructor
/// `TaggedObject` shape (nullary + record variants, unknown-tag / missing-tag
/// rejection).
///
/// Absorbed: works_generic_nullary_sum_still_bare_string, works_generic_tagged_sum.
#[test]
fn works_aeson_generic_derive_family() {
    works_with_helpers(
        "data Mode = Observing | Deciding | Acting deriving (Generic, Show, FromJSON)\n\
         data Shape = Circle { radius :: Double } | Square { side :: Double } | Origin deriving (Generic, Show, FromJSON)",
        r#"pure (concat
            [ check "generic_nullary_sum_still_bare_string" (case (eitherDecode "\"Deciding\"" :: Either Text Mode) of { Right Deciding -> True; _ -> False })
            , check "generic_tagged_sum.nullary" (case (eitherDecode "{\"tag\":\"Origin\"}" :: Either Text Shape) of { Right Origin -> True; _ -> False })
            , check "generic_tagged_sum.record" ((case (eitherDecode "{\"tag\":\"Circle\",\"radius\":2.5}" :: Either Text Shape) of { Right (Circle r) -> r; _ -> -1 }) == 2.5)
            , check "generic_tagged_sum.unknownTag" (either (const True) (const False) (eitherDecode "{\"tag\":\"Triangle\"}" :: Either Text Shape))
            , check "generic_tagged_sum.missingTag" (either (const True) (const False) (eitherDecode "{\"radius\":2.5}" :: Either Text Shape))
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

/// `Control.Lens` `_last`/`_head`/`_init`/`unsnoc` on a List (the
/// demand-analysis dead-arg fix) and `_last` on `Text` (the `Snoc` instance
/// that always worked, must keep working).
///
/// Absorbed: works_lens_last_init_unsnoc_on_list, works_lens_last_on_text.
#[test]
fn works_lens_family() {
    works_with_imports(
        "Control.Lens (_last, _head, _init, unsnoc)",
        r#"pure (concat
            [ check "lens_last_init_unsnoc_on_list.last" (([10,20,30::Int] ^? _last) == Just 30)
            , check "lens_last_init_unsnoc_on_list.head" (([10,20,30::Int] ^? _head) == Just 10)
            , check "lens_last_init_unsnoc_on_list.init" (([10,20,30::Int] ^? _init) == Just [10,20])
            , check "lens_last_init_unsnoc_on_list.unsnoc" (unsnoc [10,20,30::Int] == Just ([10,20],30))
            , check "lens_last_init_unsnoc_on_list.last_empty" ((([]::[Int]) ^? _last) == Nothing)
            , check "lens_last_init_unsnoc_on_list.init_empty" ((([]::[Int]) ^? _init) == Nothing)
            , check "lens_last_on_text" ((("abc"::Text) ^? _last) == Just 'c')
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

/// `Data.Text`/`Data.Char` family: empty-Text ops, vendored `lines`/`words`
/// guarded corecursion, `digitToInt`/`digitToIntMay`, the `Pack` dialect
/// (canonical-valid cases across String/Text/literal/`show`-output), and the
/// `Tidepool.Prelude` Text shadows (`replace`/`isSuffixOf`/`isInfixOf`/
/// `takeWhileT`/`dropWhileT`).
///
/// Absorbed: works_empty_text_ops, works_lines_words_vendored,
/// works_digit_to_int_hex, works_digit_to_int_may, works_pack_valid_canonical_string,
/// works_pack_identity_on_text, works_pack_string_literal_defaults,
/// works_pack_show_output_dialect_win, works_prelude_text_shadows_pinned.
#[test]
fn works_text_family() {
    works(
        r#"pure (concat
            [ check "empty_text_ops.isnull" (T.null ("" :: Text))
            , check "empty_text_ops.len" (T.length ("" :: Text) == 0)
            , check "empty_text_ops.eq" ("" == ("" :: Text))
            , check "empty_text_ops.split" (splitOn "/" "" == [""])
            , check "lines_words_vendored.n" (length (lines (T.replicate 20000 "x\n")) == 20000)
            , check "lines_words_vendored.trail" (lines "a\nb\n" == ["a","b"])
            , check "lines_words_vendored.mid" (lines "a\n\nb" == ["a","","b"])
            , check "lines_words_vendored.ws" (words " a b\tc " == ["a","b","c"])
            , check "digit_to_int_hex.a" (digitToInt 'a' == 10)
            , check "digit_to_int_hex.nine" (digitToInt '9' == 9)
            , check "digit_to_int_hex.big" (digitToInt 'F' == 15)
            , check "digit_to_int_may.hit" (digitToIntMay 'a' == Just 10)
            , check "digit_to_int_may.miss" (digitToIntMay 'z' == Nothing)
            , check "pack_valid_canonical_string" (T.pack ("abc" :: String) == "abc")
            , check "pack_identity_on_text" (T.pack ("already text" :: Text) == "already text")
            , check "pack_string_literal_defaults" (T.pack "lit" == "lit")
            , check "pack_show_output_dialect_win" (T.pack (show (42 :: Int)) == "42")
            , check "prelude_text_shadows_pinned.replace_basic" (replace "a" "o" "banana" == "bonono")
            , check "prelude_text_shadows_pinned.replace_no_match" (replace "z" "o" "banana" == "banana")
            , check "prelude_text_shadows_pinned.replace_empty_haystack" (replace "a" "o" "" == "")
            , check "prelude_text_shadows_pinned.is_suffix_true" (isSuffixOf "ana" "banana")
            , check "prelude_text_shadows_pinned.is_suffix_false" (not (isSuffixOf "xyz" "banana"))
            , check "prelude_text_shadows_pinned.is_suffix_empty" (isSuffixOf "" "banana")
            , check "prelude_text_shadows_pinned.is_infix_true" (isInfixOf "nan" "banana")
            , check "prelude_text_shadows_pinned.is_infix_false" (not (isInfixOf "xyz" "banana"))
            , check "prelude_text_shadows_pinned.is_infix_empty" (isInfixOf "" "banana")
            , check "prelude_text_shadows_pinned.take_while_section" (takeWhileT (/= ',') "a,b,c" == "a")
            , check "prelude_text_shadows_pinned.take_while_no_match" (takeWhileT (== 'z') "abc" == "")
            , check "prelude_text_shadows_pinned.take_while_empty" (takeWhileT (/= ',') "" == "")
            , check "prelude_text_shadows_pinned.drop_while_section" (dropWhileT (< 'c') "abcdef" == "cdef")
            , check "prelude_text_shadows_pinned.drop_while_no_match" (dropWhileT (== 'z') "abc" == "abc")
            , check "prelude_text_shadows_pinned.drop_while_empty" (dropWhileT (/= ',') "" == "")
            , check "prelude_text_shadows_pinned.take_while_partial_map" (map (takeWhileT (/= ',')) ["a,b", "c,d", "nocomma"] == ["a", "c", "nocomma"])
            , check "prelude_text_shadows_pinned.drop_while_filter_map" (map (dropWhileT (< 'c')) (filter (/= "") ["abcdef", "", "cba"]) == ["cdef", "cba"])
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

/// The safe-idiom surface: `headMay`/`lastMay`/`initMay`/`tailMay`/`atMay`,
/// `maximumMay`/`minimumMay`, `readMaybe`, `note`/`hush`, `wither`/`filterA`,
/// `ordNub`, `(>>>)`/`(<<<)`. Add a check line here when you add a Prelude
/// total-form or safe-idiom function.
///
/// Absorbed: works_safe_list_may_functions, works_safe_at_may,
/// works_safe_maximum_minimum_may, works_read_maybe, works_note_hush,
/// works_wither_filter_a, works_ord_nub, works_category_compose.
#[test]
fn works_safe_idiom_family() {
    works(
        r#"pure (concat
            [ check "safe_list_may_functions.head_some" (headMay [1,2,3::Int] == Just 1)
            , check "safe_list_may_functions.head_none" (headMay ([]::[Int]) == Nothing)
            , check "safe_list_may_functions.last_some" (lastMay [1,2,3::Int] == Just 3)
            , check "safe_list_may_functions.last_none" (lastMay ([]::[Int]) == Nothing)
            , check "safe_list_may_functions.init_some" (initMay [1,2,3::Int] == Just [1,2])
            , check "safe_list_may_functions.init_none" (initMay ([]::[Int]) == Nothing)
            , check "safe_list_may_functions.tail_some" (tailMay [1,2,3::Int] == Just [2,3])
            , check "safe_list_may_functions.tail_none" (tailMay ([]::[Int]) == Nothing)
            , check "safe_at_may.hit" (atMay [10,20,30::Int] 1 == Just 20)
            , check "safe_at_may.miss" (atMay [10,20,30::Int] 5 == Nothing)
            , check "safe_maximum_minimum_may.max_some" (maximumMay [3,1,4,1,5::Int] == Just 5)
            , check "safe_maximum_minimum_may.max_none" (maximumMay ([]::[Int]) == Nothing)
            , check "safe_maximum_minimum_may.min_some" (minimumMay [3,1,4,1,5::Int] == Just 1)
            , check "safe_maximum_minimum_may.min_none" (minimumMay ([]::[Int]) == Nothing)
            , check "read_maybe.ok" ((readMaybe "42" :: Maybe Int) == Just 42)
            , check "read_maybe.bad" ((readMaybe "abc" :: Maybe Int) == Nothing)
            , check "note_hush.note_some" ((note ("e"::Text) (Just (5::Int)) :: Either Text Int) == Right 5)
            , check "note_hush.note_none" ((note ("e"::Text) (Nothing :: Maybe Int) :: Either Text Int) == Left "e")
            , check "note_hush.hush_right" (hush (Right (5::Int) :: Either Text Int) == Just 5)
            , check "note_hush.hush_left" (hush (Left ("e"::Text) :: Either Text Int) == Nothing)
            , check "wither_filter_a.wither" (fromMaybe [] (wither (\x -> Just (if even x then Just x else Nothing)) [1,2,3,4,5,6::Int]) == [2,4,6])
            , check "wither_filter_a.filterA" (fromMaybe [] (filterA (\x -> Just (even x)) [1,2,3,4::Int]) == [2,4])
            , check "ord_nub" (ordNub [3,1,2,3,1::Int] == [3,1,2])
            , check "category_compose.gt" ((((subtract 1) :: Int -> Int) >>> (* 10)) 5 == 40)
            , check "category_compose.lt" ((((* 10) :: Int -> Int) <<< subtract 1) 5 == 40)
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

/// `Tidepool.Data.Time`: `toGregorian`/`formatDay`/ISO8601 parse-format
/// round trip, `formatISO8601`/`parseISO8601` (known/pre-epoch/timezone
/// instants), and the Int-only civil-date arithmetic
/// (`daysFromCivil`/`diffUTCTime`/`addUTCTime`/`epochMillis`, including the
/// banker's-rounding tie boundaries).
///
/// Absorbed: works_to_gregorian, works_format_day,
/// works_parse_iso8601_format_day_roundtrip, works_time_formatting_pinned,
/// works_time_arithmetic_pinned.
#[test]
fn works_time_family() {
    works(
        r#"pure (concat
            [ check "to_gregorian" (toGregorian (UTCTime 1709164800000) == (2024, 2, 29))
            , check "format_day" (formatDay (UTCTime 1709164800000) == "2024-02-29")
            , check "parse_iso8601_format_day_roundtrip" ((case parseISO8601 "2024-03-05T10:00:00Z" of { Right t -> formatDay t; Left e -> e }) == "2024-03-05")
            , check "time_formatting_pinned.known" (formatISO8601 (UTCTime 1700000000000) == "2023-11-14T22:13:20Z")
            , check "time_formatting_pinned.pre_epoch" (formatISO8601 (UTCTime (-1000)) == "1969-12-31T23:59:59Z")
            , check "time_formatting_pinned.roundtrip_tz" ((case parseISO8601 "2026-07-01T19:24:22-07:00" of { Right t -> formatISO8601 t; Left e -> e }) == "2026-07-02T02:24:22Z")
            , check "time_formatting_pinned.parse_tz_ms" ((case parseISO8601 "2026-07-01T19:24:22-07:00" of { Right t -> epochMillis t; Left _ -> (-1) }) == 1782959062000)
            , check "time_formatting_pinned.parse_epoch" ((case parseISO8601 "1970-01-01T00:00:00Z" of { Right t -> epochMillis t; Left _ -> (-1) }) == 0)
            , check "time_arithmetic_pinned.days_modern" (daysFromCivil 2024 2 29 == 19782)
            , check "time_arithmetic_pinned.days_epoch" (daysFromCivil 1970 1 1 == 0)
            , check "time_arithmetic_pinned.days_pre_epoch" (daysFromCivil 1969 12 31 == (-1))
            , check "time_arithmetic_pinned.diff_cross_epoch" (diffUTCTime (UTCTime 1000) (UTCTime (-500)) == 1.5)
            , check "time_arithmetic_pinned.diff_negative" (diffUTCTime (UTCTime 0) (UTCTime 5000) == (-5))
            , check "time_arithmetic_pinned.add_cross_epoch_neg" (epochMillis (addUTCTime (-1.5) (UTCTime 1000)) == (-500))
            , check "time_arithmetic_pinned.add_round_tie_down" (epochMillis (addUTCTime 0.0625 (UTCTime 0)) == 62)
            , check "time_arithmetic_pinned.add_round_tie_up" (epochMillis (addUTCTime 0.1875 (UTCTime 0)) == 188)
            , check "time_arithmetic_pinned.epoch_millis_pre_epoch" (epochMillis (UTCTime (-500)) == (-500))
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

/// The `[j|…|]`/`[fmt|…|]` quoters, same-shaped WORKS probes: exact-digit
/// fraction rendering via `renderJson`, pattern matching an exact large
/// integer literal, exponent-leading-zeros parsing, an escaped control
/// character, brace-inside-hole (string literal and doubled-brace escape),
/// and the real `[fmt|...|]` sign-aware zero-pad wiring (int and fraction).
///
/// NOT absorbed: `qq_json_exact_large_integer_literal` — looks same-shaped
/// but crosses the `to_json()` boundary for a large exact integer (render-
/// fidelity carve-out, standalone below).
///
/// Absorbed: qq_json_exact_fraction_literal_beyond_double_precision,
/// qq_json_pattern_matches_exact_large_integer,
/// qq_json_exponent_leading_zeros_still_parses,
/// qq_json_string_allows_escaped_control_char,
/// qq_fmt_brace_inside_hole_string_literal_still_works,
/// qq_fmt_doubled_brace_still_literal, works_fmt_qq_sign_aware_zero_pad.
#[test]
fn works_qq_family() {
    works_with_imports(
        QQ_IMPORTS,
        r#"pure (concat
            [ check "qq_json_exact_fraction_literal_beyond_double_precision" (renderJson [j|1.234567890123456789|] == "1.234567890123456789")
            , check "qq_json_pattern_matches_exact_large_integer" (case [j|123456789012345678|] of { [j|123456789012345678|] -> True; _ -> False })
            , check "qq_json_exponent_leading_zeros_still_parses" (renderJson [j|1e0000000001|] == "10")
            , check "qq_json_string_allows_escaped_control_char" (case [j|"a\u0001b"|] of { String t -> t == "a\SOHb"; _ -> False })
            , check "qq_fmt_brace_inside_hole_string_literal_still_works" ([fmt|{T.pack "a}b"}|] == "a}b")
            , check "qq_fmt_doubled_brace_still_literal" ([fmt|literal }} brace|] == "literal } brace")
            , check "fmt_qq_sign_aware_zero_pad.int_neg" ([fmt|{n:06d}|] == "-00042")
            , check "fmt_qq_sign_aware_zero_pad.frac_neg" ([fmt|{d:08.2f}|] == "-0003.14")
            ])
         where { check nm ok = if ok then [] else [nm]; n = (-42) :: Int; d = (-3.14159) :: Double }"#,
        serde_json::json!([]),
    );
}

/// `Tidepool.FilePath` POSIX fidelity: `normalise` (trailing/leading
/// separators, interior dots, clean-path no-ops) and the dotfile-extension
/// sibling-diff fix (`splitExtension`/`takeExtension`/`takeBaseName`/
/// `hasExtension`).
///
/// Absorbed: works_filepath_normalise_trailing_and_leading_separators,
/// works_filepath_normalise_interior_dots_and_clean_paths,
/// works_filepath_extension_dotfile_fidelity.
#[test]
fn works_filepath_family() {
    works(
        r#"pure (concat
            [ check "filepath_normalise.a_slash" (normalise "a/" == "a/")
            , check "filepath_normalise.test_many_slash" (normalise "/test////" == "/test/")
            , check "filepath_normalise.dot_slash" (normalise "./" == "./")
            , check "filepath_normalise.file_test_many_slash" (normalise "/file/test////" == "/file/test/")
            , check "filepath_normalise.dotdot_bob_fred_slash" (normalise "../bob/fred/" == "../bob/fred/")
            , check "filepath_normalise.bob_fred_dot" (normalise "bob/fred/." == "bob/fred/")
            , check "filepath_normalise.dot_bob_fred_slash" (normalise "./bob/fred/" == "bob/fred/")
            , check "filepath_normalise.empty" (normalise "" == ".")
            , check "filepath_normalise.double_leading_slash_home" (normalise "//home" == "/home")
            , check "filepath_normalise.backslash_literal" (normalise "/file/\\test////" == "/file/\\test/")
            , check "filepath_normalise.a_dot_b_dotdot_c" (normalise "a/./b/../c" == "a/b/../c")
            , check "filepath_normalise.test_dot_file" (normalise "/test/./file" == "/test/file")
            , check "filepath_normalise.file_dot_test" (normalise "/file/./test" == "/file/test")
            , check "filepath_normalise.test_file_dotdot_bob_fred_slash" (normalise "/test/file/../bob/fred/" == "/test/file/../bob/fred/")
            , check "filepath_normalise.a_dotdot_c" (normalise "/a/../c" == "/a/../c")
            , check "filepath_normalise.dot" (normalise "." == ".")
            , check "filepath_normalise.dot_dot" (normalise "./." == "./")
            , check "filepath_normalise.slash_dot_slash" (normalise "/./" == "/")
            , check "filepath_normalise.root" (normalise "/" == "/")
            , check "filepath_extension.take_extension_bashrc" (takeExtension ".bashrc" == ".bashrc")
            , check "filepath_extension.take_extension_dot" (takeExtension "." == ".")
            , check "filepath_extension.split_extension_bashrc" (splitExtension ".bashrc" == ("", ".bashrc"))
            , check "filepath_extension.take_base_name_bashrc" (takeBaseName ".bashrc" == "")
            , check "filepath_extension.has_extension_bashrc" (hasExtension ".bashrc")
            , check "filepath_extension.split_extension_crossing_slash" (splitExtension "file.txt/boris" == ("file.txt/boris", ""))
            , check "filepath_extension.take_extension_regular" (takeExtension "file.txt" == ".txt")
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

// =========================================================================
// STANDALONE — exclusion classes (a)-(g) from the module doc. Each group
// below is annotated with which class it belongs to.
// =========================================================================

// --- (g) RENDER-FIDELITY: outer-JSON rendering of a decoded Float, not
// anything a Haskell-side `==` can observe — kept standalone the same way
// works_exact_int_json is, not absorbed into works_aeson_family. ---

/// `FromJSON Float`, same shape as the already-pinned `FromJSON Double`
/// (aeson `FromJSON Float` routes through `parseRealFloat` —
/// https://hackage.haskell.org/package/aeson/docs/src/Data.Aeson.Types.FromJSON.html).
///
/// Formerly SANCTIONED-RED (`plans/post-restart/gate-runbook.md`): the
/// extract translator had no `decodeFloat_Int#` 2-result split — any Core
/// invoking it hit `splitUnaryMultiReturnPrimOp`'s generic-fallback loud
/// failure. Fixed structurally mirroring the already-working
/// `decodeDouble_Int64#`/`DecodeDoubleMantissa`/`DecodeDoubleExponent` path
/// at all four sites it touches: `splitUnaryMultiReturnPrimOp`
/// (`haskell/src/Tidepool/Translate.hs`), the `PrimOpKind::DecodeFloatMantissa`/
/// `DecodeFloatExponent` wire entries (`tidepool-repr/src/types.rs`), the
/// eval oracle (`tidepool-eval/src/eval.rs`), and the Cranelift lowering
/// (`tidepool-codegen/src/emit/primop.rs` + `host_fns/primops.rs`) — each
/// over Float's own IEEE754 single layout (8-bit exponent / 23-bit
/// mantissa, bias 127), NOT reusable via `float2Double#` widening first
/// (that decodes into Double's wider mantissa/exponent and gives a wrong
/// answer for Float). JIT/oracle agreement is pinned by
/// `tidepool-codegen/tests/proptest_primops_differential.rs`'s
/// `prop_decode_float`, mirroring `prop_decode_double`.
#[test]
fn works_from_json_float() {
    works(
        r#"pure (object ["ok" .= either (const (0 :: Float)) id (eitherDecode "3.5" :: Either Text Float), "notNumber" .= either (const True) (const False) (eitherDecode "\"x\"" :: Either Text Float)])"#,
        serde_json::json!({"ok": 3.5, "notNumber": true}),
    );
}

/// (d) DISTINCT-MECHANISM: `KnownSymbol` reflection — `symbolVal` recovering
/// a type-level `Symbol` as a runtime string. GHC constructs `KnownSymbol`
/// dictionaries magically (no ordinary instance declaration exists), so this
/// pins that the Core we serialize carries them in a form the JIT executes.
/// Load-bearing gate for the `askUserWith` typed-field-label design, where a
/// `#field` reference recovers its field name via `symbolVal` at runtime.
#[test]
fn works_known_symbol_reflection() {
    works_with_imports(
        "GHC.TypeLits (symbolVal)\nData.Proxy (Proxy(..))",
        r#"pure (toJSON [symbolVal (Proxy :: Proxy "iterations"), symbolVal (Proxy :: Proxy "verbose")])"#,
        serde_json::json!(["iterations", "verbose"]),
    );
}

// --- (g) RENDER-FIDELITY carve-outs — the property under test is the exact
// shape of the outer Rust `to_json()` render, not anything a Haskell-side
// `==` can observe. ---

/// Moderate Double literals render fine — documents the boundary for the
/// near-DBL_MAX GMP trap below (3.14, 1.0e10, 1.23e100 all stay within the
/// integerAdd/integerSub shims). `a`=3.14 renders compact (serde-exact); `b`
/// and `c` are integral → integer JSON. `c` (1.234…e100) is an extreme
/// whole-number magnitude, so it EXPANDS to the exact 101-digit integer
/// (16 significant digits × 10^85) rather than serde's compact `…e+100` — a
/// small coefficient with a large positive exponent is an integer, and the
/// erased `Value` carries nothing to mark it as double-origin. See
/// `large_double_literal_on_jit`.
///
/// RENDER-FIDELITY: the interesting property is the exact outer JSON digit
/// expansion, invisible to a Haskell-side `==` — kept standalone.
#[test]
fn works_moderate_double_literals() {
    let c = format!("1234567890123456{}", "0".repeat(85));
    let expected: serde_json::Value =
        serde_json::from_str(&format!("{{\"a\":3.14,\"b\":10000000000,\"c\":{c}}}")).unwrap();
    works(
        "pure (object [\"a\" .= (3.14 :: Double), \"b\" .= (1.0e10 :: Double), \
         \"c\" .= (1.234567890123456e100 :: Double)])",
        expected,
    );
}

/// A near-DBL_MAX Double LITERAL works on the JIT: the native-bignum
/// integerAdd/integerSub shims handle it. Moderate literals are also fine (see
/// `works_moderate_double_literals`). The value renders as the exact expanded
/// integer (179×10^306), not serde's compact `1.79e+308` — extreme
/// whole-number doubles expand (see `works_moderate_double_literals`). Assert
/// the value round-trips rather than pinning the 309-digit string.
///
/// RENDER-FIDELITY / structurally unique (uses `as_f64()` directly, not the
/// `works()` harness) — kept standalone.
#[test]
fn large_double_literal_on_jit() {
    let got = run_probe("pure (1.79e308 :: Double)").expect("large double literal should eval");
    assert_eq!(
        got.as_f64(),
        Some(1.79e308),
        "value must round-trip through the expanded-integer render; got {got}"
    );
}

/// Integers survive the JSON path exactly. The vendored aeson `Number` carries a
/// `Scientific` (exact `Integer` coefficient × 10^exponent), so an Int/Integer
/// rides its coefficient with no Double rounding past 2^53 — exact end-to-end
/// (ToJSON instances, [j|] integral literals, serde bridge, render arm, optics
/// `_Int`).
///
/// RENDER-FIDELITY: exact-precision survival across `to_json()` is the whole
/// point — kept standalone (NOT part of `works_aeson_family`).
#[test]
fn works_exact_int_json() {
    works(
        "pure (object [\"a\" .= (912345678901234567 :: Int), \"b\" .= (9007199254740993 :: Int), \"neg\" .= (-42 :: Int), \"rt\" .= (toJSON (9007199254740993 :: Int) ^? _Int)])",
        serde_json::json!({"a": 912345678901234567_i64, "b": 9007199254740993_i64, "neg": -42, "rt": 9007199254740993_i64}),
    );
}

/// `[j|…|]` integer literals beyond `Double`'s 53-bit mantissa parse EXACT:
/// the literal-syntax number path is integer digit accumulation only, never
/// `read :: Double`. Before the fix, `read "123456789012345678" :: Double`
/// rounds to the nearest representable double and loses the low digits.
///
/// RENDER-FIDELITY: looks same-shaped as the other `[j|]` probes bundled into
/// `works_qq_family`, but this one crosses the outer `to_json()` boundary for
/// a large EXACT integer (the property under test is the JSON wire's digit
/// fidelity, not a Haskell-observable value) — kept standalone.
#[test]
fn qq_json_exact_large_integer_literal() {
    works_with_imports(
        QQ_IMPORTS,
        "pure [j|123456789012345678|]",
        serde_json::json!(123456789012345678_i64),
    );
}

// --- Recursion-depth trio: TCO, non-tail overflow, and GHC loopification —
// (d) DISTINCT-MECHANISM (call-depth / TCO, not a stdlib function). ---

/// TCO: a deep TAIL-recursive loop (500k frames) returns cleanly — contrast
/// with `nontail_recursion_fails_loudly`. Pins "tail recursion is
/// unbounded", retiring the long-dead "recursion depth ~20 max" rule.
#[test]
fn works_tco_deep_tail_recursion() {
    works(
        "pure (go 500000 0 :: Int) where { go n acc = if n == 0 then acc else go (n-1) (acc+1) }",
        serde_json::json!(500000),
    );
}

/// NON-tail recursion overflows (~10-20K frames) with a CLEAN "stack overflow"
/// yield error — never SIGSEGV. (500k non-tail frames here.) Contrast
/// `works_tco_deep_tail_recursion`.
///
/// Guards the masked-StackOverflow case in effect_machine `parse_result`:
/// when the top-level result is itself a thunk,
/// the deep recursion runs inside `force_ptr`, so the depth guard sets
/// StackOverflow AFTER `parse_result`'s pre-force error check — the tag-0 poison
/// closure was then mis-reported as "unexpected heap tag: 0". `parse_result` now
/// re-checks `take_runtime_error()` after forcing. (The bug was invisible on the
/// MCP server, which surfaced the still-pending error via a later teardown path;
/// only the library/`compile_and_run` path exposed it.)
///
/// (f) RUNTIME fails_loudly — the eval itself errors, incompatible with the
/// check-list idiom.
#[test]
fn nontail_recursion_fails_loudly() {
    fails_loudly(
        "pure (go 500000 :: Int) where { go n = if n == 0 then 0 else 1 + go (n-1) }",
        "stack overflow",
    );
}

/// GHC -O2 loopifies a no-base-case non-tail recursion (`go n = n + go
/// (n+1)`) into a tight spin — it runs until the eval *timeout*, NOT until
/// "stack overflow". Contrast `nontail_recursion_fails_loudly` where
/// a base-case 500k-frame `1 + go(n-1)` does overflow.
///
/// Doc claim: "a no-base-case non-tail recursion (`go n = n + go (n+1)`) is
/// loopified by GHC into a non-stack-growing spin — it runs until the eval
/// timeout fires, not an overflow" (haskell/CLAUDE.md "Known Limits").
///
/// Verification: start the eval and wait 3 seconds. "stack overflow" within
/// that window means GHC didn't loopify → CLAIM FALSE. Timeout (still
/// spinning) or any other outcome → claim confirmed.
///
/// NOTE: the spawned thread leaks (keeps spinning until the test binary
/// exits). This is acceptable; the CPU burn terminates with the binary.
///
/// Structurally unique (mpsc/recv_timeout, no simple `works()` shape) —
/// kept standalone.
#[test]
fn claims_nobase_nontail_loopifies_not_overflows() {
    let (tx, rx) = std::sync::mpsc::channel::<Result<serde_json::Value, String>>();
    let _ = std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            let _ = tx.send(eval_raw("pure (go 0 :: Int) where { go n = n + go (n+1) }"));
        })
        .unwrap();
    // 3s is long enough for a real non-tail stack-overflow to manifest (~10-20K frames).
    match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(Err(e)) if e.contains("stack overflow") => {
            panic!(
                "CLAIM FALSE: no-base-case non-tail should loopify (GHC -O2 spin), \
                 not produce a stack overflow.\n\
                 haskell/CLAUDE.md 'Known Limits' claims this becomes a non-stack-growing spin.\n\
                 actual error: {e}"
            );
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // Still spinning → GHC loopified it, claim confirmed.
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!(
                "Thread exited without sending (panicked before eval_raw returned). \
                 Cannot confirm loopification claim."
            );
        }
        Ok(other) => {
            panic!("Unexpected outcome (expected timeout-spin or stack-overflow): {other:?}");
        }
    }
}

// --- (b) COMPILE-FAIL probes — assert a compile-time ERROR. ---

/// `let` in braced `do` (`do { let x = e; stmt }`) is a GHC PARSE error.
/// Clean compile-time failure.
#[test]
fn let_in_braced_do_fails_loudly() {
    fails_loudly("do { let x = 1 :: Int; pure x }", "parse error");
}

/// A malformed `[uri|...|]` quote is a COMPILE-TIME error naming the
/// precise reason — never a silent misparse. Asserts the real rejection
/// text `uriCheck` constructs (`Tidepool.QQ.Validate`) for a scheme-less
/// URI, the exact class of silent runtime trap the quoter exists to catch.
#[test]
fn stdlib_quoter_bad_input_fails_loudly_at_compile_time() {
    fails_loudly_with_imports(
        "Tidepool.QQ (uri)",
        r#"pure [uri|ftp://example.com|]"#,
        "URI must start with 'http://' or 'https://'",
    );
}

/// JSON permits an arbitrarily large exponent, but `pNumber` narrows the
/// exponent digits to `Int` (`fromInteger :: Integer -> Int`) before
/// splicing — unguarded, a pathological exponent like this one would
/// silently WRAP the Int rather than erroring, producing a wrong-but-quiet
/// `Scientific`. `maxExponentDigits` (Json.hs) rejects it instead. NOTE:
/// this probe has not executed — the marker is derived by hand from the
/// error string `pNumber` constructs (`"exponent has too many digits (" ++
/// show (length ds) ++ ") ..."`), not confirmed by a run.
#[test]
fn qq_json_exponent_overflow_rejected_not_wrapped() {
    fails_loudly_with_imports(QQ_IMPORTS, "pure [j|1e9999999|]", "too many digits");
}

/// A raw (unescaped) control character inside a `[j|…|]` string literal is a
/// compile-time error naming the offending code point — the JSON grammar the
/// quoter advertises never allowed a literal control byte inside a string.
#[test]
fn qq_json_string_rejects_unescaped_control_char() {
    fails_loudly_with_imports(QQ_IMPORTS, "pure [j|\"a\u{1}b\"|]", "control character");
}

/// A bare, unmatched `}` outside a hole is a compile-time error in
/// `[fmt|…|]` — matching the Python f-string grammar the module advertises
/// (a lone `}` is not allowed; `}}` is the literal-`}` escape).
#[test]
fn qq_fmt_rejects_bare_unmatched_brace() {
    fails_loudly_with_imports(QQ_IMPORTS, "pure [fmt|value } here|]", "unmatched '}'");
}

/// An unclosed `{` in `[fmt|…|]` now names the offset at which the quote body
/// ran out of input — previously this lexer error carried no position.
#[test]
fn qq_fmt_unclosed_brace_carries_offset() {
    fails_loudly_with_imports(QQ_IMPORTS, "pure [fmt|hello {name|]", "at offset");
}

// --- (f) RUNTIME fails_loudly — miscellaneous (the eval itself errors). ---

/// A large Value tree folded by a lens (`toJSON [1..20000] ^.. values`)
/// overflows with a CLEAN "stack overflow" yield error — NOT a SIGILL. The
/// cause is a non-tail lens fold, not general "complex traversal".
///
/// Guards the same masked-StackOverflow case as
/// `nontail_recursion_fails_loudly` (in `parse_result`).
#[test]
fn large_value_lens_fold_fails_loudly() {
    fails_loudly(
        "pure (length (toJSON [1..20000::Int] ^.. values) :: Int)",
        "stack overflow",
    );
}

/// `Prelude.splitOn` with an empty separator raises the canonical
/// `Data.Text.splitOn` exception instead of returning data — text-2.1.2
/// continues to document an empty delimiter as invalid input.
/// https://hackage.haskell.org/package/text-2.1.2/docs/Data-Text.html
#[test]
fn split_on_empty_needle_fails_loudly() {
    fails_loudly(r#"pure (length (splitOn "" "abc"))"#, "splitOn");
}

/// `digitToInt` THROWS on a non-hex-digit character, mirroring
/// `Data.Char.digitToInt` —
/// https://hackage.haskell.org/package/base/docs/Data-Char.html#v:digitToInt
#[test]
fn digit_to_int_non_hex_fails_loudly() {
    fails_loudly(r#"pure (digitToInt 'z')"#, "not a digit");
}

/// The base partials (2026-08-24 partials-allowed ruling — `haskell/CLAUDE.md`
/// "Adding new Prelude functions"/root `CLAUDE.md`'s Locked Decisions do not
/// name this, but `Tidepool.Prelude` no longer shadows `head`/`tail`/`last`/
/// `init`/`(!!)`/`foldr1`/`foldl1`/`fromJust` with `Unsatisfiable` stubs — they
/// now run with ordinary base semantics). `head`/`(!!)` on an empty/
/// out-of-range input must fail the BLOCK with the standard GHC runtime
/// message, classified `Runtime`/`Run` (never `UserHaskell`/`Compile`) — same
/// failure shape as any other Haskell `error` call (the `runtime_error`
/// machinery, `tidepool-codegen/CLAUDE.md`). The total `headMay`/`atMay` stay
/// the recommended control-flow idiom (`works_safe_idiom_family` above).
#[test]
fn base_partials_fail_loudly_as_runtime() {
    fails_loudly_as_runtime(r#"pure (head ([] :: [Int]))"#, "Prelude.head: empty list");
    fails_loudly_as_runtime(
        r#"pure ([1,2,3 :: Int] !! 99)"#,
        "Prelude.!!: index too large",
    );
}

// --- (d) DISTINCT-MECHANISM probes — pin a compiler/runtime mechanism, not
// a stdlib function's JIT-safety. ---

/// `Data.Tree`'s `Node` constructor collides on the UNQUALIFIED name with
/// freer-simple's FTCQueue continuation `Node` — both arity 2. Before the fix,
/// `ConTags::try_from` resolved the freer continuations via `get_by_name`, which
/// returns `None` for a 2-entry unqualified collision, so any eval importing
/// `Data.Tree (Tree(..))` and using `Node` died at effect-machine setup with
/// `missing freer-simple constructor 'Node' in DataConTable`. The fix resolves
/// the freer continuations by their fixed MODULE-QUALIFIED name
/// (`Data.FTCQueue.Node`), which is unambiguous regardless of user imports.
/// Asserts the documented repro: `treeDepth` over a depth-3 tree returns 3.
#[test]
fn works_data_tree_node_no_freer_collision() {
    // Data.Tree's `Node` constructor collides with the freer continuation
    // `Node` (both in scope in every eval), so an unqualified `Node` is a
    // legitimate ambiguity. Qualify Data.Tree — the point is its `Node`
    // resolves + `treeDepth` runs (the freer `Node` must not shadow the
    // qualified constructor).
    works_with_imports(
        "qualified Data.Tree as DTree",
        "pure (treeDepth t) where { \
         t = DTree.Node (1::Int) [DTree.Node 2 [], DTree.Node 3 [DTree.Node 4 []]]; \
         treeDepth (DTree.Node _ []) = 1::Int; \
         treeDepth (DTree.Node _ cs) = 1 + maximum (map treeDepth cs) }",
        serde_json::json!(3),
    );
}

/// Companion to the collision guard: a NORMAL eval (no `Data.Tree`) must still
/// resolve the freer continuation `Node` and run the effect machine. This is the
/// no-regression half — the qualified-name resolution must not break the common
/// (no-collision) case. `pure` exercises `Val`; running at all exercises the
/// full ConTags resolution (Val/E/Union/Leaf/Node) at machine setup.
#[test]
fn works_freer_node_resolves_without_data_tree() {
    works("pure (sum [1..10::Int])", serde_json::json!(55));
}

/// DuplicateRecordFields shared selector — the `Hit`/`FileRead` library records
/// both define a `path` field (legal under `DuplicateRecordFields`). In GHC
/// 9.2+ the selectors keep the bare occ name `path` (record-field namespace,
/// no `$sel:` mangling), so
/// `stableVarId` fingerprinted identical `<Module>:path` strings → ONE varId
/// for two distinct selectors. The DataConTable / external resolver coalesced
/// them: `getField @"path" @Hit` bound to whichever selector won, and applying
/// the other record's selector to a `Hit` value (or vice-versa) hit a CASE TRAP
/// ("scrutinee constructor not among case alternatives"). Type-checks, then
/// traps at runtime = compiler bug. Fixed in `Translate.hs` by folding the
/// record selector's parent tycon into its varId (`stableVarIdWith`), so
/// `path`@Hit ≠ `path`@FileRead. BOTH accesses must now return the right field.
#[test]
fn works_dup_record_fields_shared_selector() {
    // Hit.path (shared field) via OverloadedRecordDot — the trapping case.
    works("pure ((Hit \"a\" 1 \"b\").path)", serde_json::json!("a"));
    // FileRead.path (the OTHER record sharing `path`) — must resolve to FileRead's field.
    works(
        "pure ((FileRead \"p\" (Right \"body\")).path)",
        serde_json::json!("p"),
    );
    // Both in one eval, plus a non-shared field each, to prove no cross-wiring:
    // Hit.line (unique to Hit) and FileRead.contents (unique to FileRead) still resolve.
    works(
        "pure (object [\"hp\" .= (Hit \"a\" 1 \"b\").path, \"dp\" .= (FileRead \"p\" (Right \"body\")).path, \
         \"hl\" .= (Hit \"a\" 1 \"b\").line, \"db\" .= fromRight \"\" (FileRead \"p\" (Right \"body\")).contents])",
        serde_json::json!({"hp":"a","dp":"p","hl":1,"db":"body"}),
    );
}

/// CANARY (not a regression pin) — dup-field record selectors stay at 0 audit
/// collisions through the production extract path. Two records sharing a field
/// label (`color` on both `Foo` and `Bar`, legal under `DuplicateRecordFields`)
/// must not hash to one VarId, or the JIT's flat emit env would alias one
/// selector onto the other (wrong field / case trap).
///
/// HONEST SCOPE: this exercises the HOME-MODULE path only — the selectors are
/// compiled from source in this extraction, so they carry `RecSelId` details.
/// BOTH the current `FldName`-namespace disambiguator AND b4e0f8c's older
/// `idDetails`-based one handle that case, so this test PASSES PRE-FIX too and
/// does NOT by itself pin the b4e0f8c→FldName change. It is a stays-green canary:
/// it goes red if a future varId-scheme change reintroduces field-name
/// collisions wholesale. The mechanism contract of the current fix
/// (`fieldParentDisamb`: distinct parents → distinct disambiguators; non-field →
/// empty) is pinned directly in `haskell/test-varid/VarIdMechanismTest.hs`.
///
/// The originally-reported collision (0xfea90eccc07baa0f, two TOP `path` sites in
/// Tidepool.Records) came from a stale deployed extract binary predating b4e0f8c;
/// it could not be reproduced on current source in any configuration (home
/// source, or records as a compiled-package dependency with fat Core / no
/// unfoldings). The module below does NOT import `Tidepool.Prelude`, keeping the
/// audit small and the two `color` selectors unambiguously its own TOP binders.
///
/// Structurally unique (spawns `tidepool-extract` directly, not `eval_raw`) —
/// kept standalone.
#[test]
fn varid_audit_dup_record_fields_zero_collisions() {
    // Probe: two records, one shared field name.
    let probe_src = r#"
{-# LANGUAGE NoImplicitPrelude, DuplicateRecordFields, OverloadedRecordDot #-}
module DupFieldAudit where

import Prelude (String, (++))

data Foo = Foo { color :: String }
data Bar = Bar { color :: String }

target :: String
target = (Foo "red").color ++ (Bar "blue").color
"#;

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let probe_path = tmp.path().join("DupFieldAudit.hs");
    {
        let mut f = std::fs::File::create(&probe_path).expect("write probe");
        f.write_all(probe_src.as_bytes())
            .expect("write probe bytes");
    }
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).expect("out dir");

    let extract_bin =
        std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string());

    let output = std::process::Command::new(&extract_bin)
        .arg(probe_path.to_str().unwrap())
        .args(["--target", "target"])
        .args(["--output-dir", out_dir.to_str().unwrap()])
        .env("TIDEPOOL_VARID_AUDIT", "1")
        .output()
        .unwrap_or_else(|e| panic!("failed to run {extract_bin}: {e}"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Extract the [VARID AUDIT] summary line.
    let audit_line = stderr
        .lines()
        .find(|l| l.starts_with("[VARID AUDIT]"))
        .unwrap_or_else(|| panic!("no [VARID AUDIT] line in stderr:\n{stderr}"));

    // Assert 0 collisions — the two `color` selectors get distinct VarIds
    // (pre-fix via b4e0f8c's RecSelId key, post-fix via the FldName namespace).
    assert!(
        audit_line.ends_with("0 collisions"),
        "expected 0 collisions from [VARID AUDIT], got: {audit_line}\nfull stderr:\n{stderr}"
    );

    // Belt-and-suspenders: no [VARID COLLISION] line naming two TOP sites.
    let collision_line = stderr
        .lines()
        .find(|l| l.starts_with("[VARID COLLISION]") && l.contains("sites=2"));
    assert!(
        collision_line.is_none(),
        "unexpected collision: {}\nfull stderr:\n{stderr}",
        collision_line.unwrap_or("")
    );
}

/// `Tidepool.Swarm`'s `PlanF`/`hyloM` (PRD 20, "The hylo core") — the swarm
/// engine's recursion-scheme core surviving derivation and extraction on the
/// JIT. Standalone per exclusion class (d) DISTINCT-MECHANISM: this pins
/// whether GHC-derived `Functor`/`Foldable`/`Traversable` dictionaries for a
/// user-defined recursive-shaped data type, plus a higher-order recursive
/// fold (`hyloM`'s `traverse go`), survive the Core extract → Cranelift JIT
/// pipeline — a compiler/runtime mechanism, not a stdlib function's
/// JIT-safety, so it does not belong in a family bundle. `Tidepool.Swarm` is
/// imported qualified: this repo's own `.tidepool/lib/Schemes.hs` (the
/// project-local dev verb library, auto-imported unqualified as `Library` in
/// every eval run from this worktree) already defines an unrelated `hyloM`,
/// so an unqualified import here would be ambiguous.
///
/// Three parts: (a) `PlanF` construction plus `fmap`/`sum` (derived
/// `Functor`/`Foldable`) and `traverse` (derived `Traversable`) directly on a
/// value — each acts on `kids` only, `task` is untouched. (b) `hyloM`
/// unfolding a numeric range `(lo, hi)` by repeated midpoint split down to
/// singleton leaves, then folding the SAME coalgebra's tree back up two
/// ways — concatenating leaf values (order-preserving) and summing them —
/// asserting the exact reconstructed `[1..10]` and its sum `55`. (c) the four
/// policy wrappers (`budgeted`/`capped`/`gated`/`receipted`) actually applied
/// over that same coalgebra: each is a higher-order transformer whose slot is
/// an `M`-effectful function, so this pins that a wrapped seam still runs —
/// coalgebra-side refusal TRUNCATES to a childless `PlanF` (`[0]`, the
/// refusal task's leaf) rather than throwing, `budgeted` truncates only the
/// subtree its slot vetoes, and `receipted`'s stamp is applied at every fold
/// (three folds over `(1,2)`: `1+1`, `2+1`, then `(2+3)+1` = `6`).
///
/// They join this probe rather than getting their own `#[test]`: the wrappers
/// are ordinary functions over the same derived dictionaries part (b) already
/// exercises, so a second extract compile would buy no distinct mechanism.
#[test]
fn works_swarm_planf_hylo_on_jit() {
    works_with_imports(
        "qualified Tidepool.Swarm as Swarm\nData.Traversable (traverse)",
        r#"Swarm.hyloM algLeaves coalg ((1,10) :: (Int,Int)) >>= \leaves ->
            Swarm.hyloM algSum coalg ((1,10) :: (Int,Int)) >>= \total ->
            Swarm.hyloM algLeaves (Swarm.capped spanOf 4 refuse coalg) ((1,10) :: (Int,Int)) >>= \cappedOut ->
            Swarm.hyloM algLeaves (Swarm.budgeted vetoHigh coalg) ((1,10) :: (Int,Int)) >>= \budgetOut ->
            Swarm.hyloM algLeaves (Swarm.gated refuseRoot coalg) ((1,10) :: (Int,Int)) >>= \gatedOut ->
            Swarm.hyloM (Swarm.receipted bump algSum) coalg ((1,2) :: (Int,Int)) >>= \stamped ->
            pure (concat
                [ check "planf_fmap_kids" (Swarm.kids (fmap (+1) (Swarm.PlanF ("root"::Text) [1,2,3::Int])) == [2,3,4::Int])
                , check "planf_foldable_sum" (sum (Swarm.PlanF ("root"::Text) [1,2,3::Int]) == (6::Int))
                , check "planf_traverse_maybe" (fmap Swarm.kids (traverse (\x -> if x > (0::Int) then Just (x*10) else Nothing) (Swarm.PlanF ("root"::Text) [1,2,3::Int])) == Just [10,20,30::Int])
                , check "hylo_range_split_leaves" (leaves == ([1..10] :: [Int]))
                , check "hylo_range_split_sum" (total == (55 :: Int))
                , check "swarm_capped_truncates_to_refusal_leaf" (cappedOut == ([0] :: [Int]))
                , check "swarm_budgeted_truncates_only_the_vetoed_subtree" (budgetOut == ([1,2,3,4,5,0] :: [Int]))
                , check "swarm_gated_refuses_the_layer_after_unfolding_it" (gatedOut == ([0] :: [Int]))
                , check "swarm_receipted_stamps_every_fold" (stamped == (6 :: Int))
                ])
             where {
               check nm ok = if ok then [] else [nm];
               coalg :: (Int,Int) -> M (Swarm.PlanF (Int,Int) (Int,Int));
               coalg (lo,hi) = if lo == hi then pure (Swarm.PlanF (lo,hi) []) else let { mid = (lo+hi) `div` 2 } in pure (Swarm.PlanF (lo,hi) [(lo,mid),(mid+1,hi)]);
               algLeaves :: Swarm.PlanF (Int,Int) [Int] -> M [Int];
               algLeaves (Swarm.PlanF (lo,_) []) = pure [lo];
               algLeaves (Swarm.PlanF _ xss) = pure (concat xss);
               algSum :: Swarm.PlanF (Int,Int) Int -> M Int;
               algSum (Swarm.PlanF (lo,_) []) = pure lo;
               algSum (Swarm.PlanF _ xs) = pure (sum xs);
               spanOf :: (Int,Int) -> Int;
               spanOf (lo,hi) = hi - lo;
               refuse :: (Int,Int) -> M (Maybe (Int,Int));
               refuse _ = pure (Just (0,0));
               vetoHigh :: (Int,Int) -> M (Maybe (Int,Int));
               vetoHigh (lo,_) = pure (if lo > 5 then Just (0,0) else Nothing);
               refuseRoot :: Swarm.PlanF (Int,Int) (Int,Int) -> M (Maybe (Int,Int));
               refuseRoot (Swarm.PlanF (lo,_) _) = pure (if lo == 1 then Just (0,0) else Nothing);
               bump :: Swarm.PlanF (Int,Int) Int -> Int -> M Int;
               bump _ b = pure (b + 1)
             }"#,
        serde_json::json!([]),
    );
}

// --- (e) `works_stdlib_quoter_survives_extract` — the QQ-survival proof,
// load-bearing coverage, kept standalone by explicit instruction. ---

/// `[uri|...|]` (`haskell/lib/Tidepool/QQ/Validate.hs`) — a QuasiQuoter
/// DEFINED IN THE STDLIB (not shipped with GHC/base) compiles and runs a
/// splice through the real extract pipeline, exactly like the other
/// `Tidepool.QQ` quoters (`[fmt|]`/`[j|]`/`[patch|]`). The check
/// (`uriCheck`) runs entirely at COMPILE time inside the splice evaluator;
/// on success the expansion is a plain `Text` literal — exercised here.
#[test]
fn works_stdlib_quoter_survives_extract() {
    works_with_imports(
        "Tidepool.QQ (uri)",
        r#"pure [uri|https://example.com/x|]"#,
        serde_json::json!("https://example.com/x"),
    );
}

// --- (c) EFFECTS/DISPATCH probes — exercise a REAL dispatcher (not
// NullDispatcher); bundling would change dispatch interleaving. ---

// ---------------------------------------------------------------------------
// Tidepool.Fork (Wave C) — forkFilter compiles and runs on the JIT.
//
// `forkMap`/`forkCata` (a CALLER-chosen answer type `b`) are NOT shipped:
// extract statically rejects a `runLLMTurnFanout` occurrence whose
// answer type still carries a free type variable
// (`Tidepool.Translate.checkRunLLMTurnType`), and closing that gap for a
// library-defined generic wrapper would require GHC to duplicate the
// wrapper's definition (type-substituted) into every call site before
// extract ever sees the Core — empirically, neither `{-# INLINE #-}` nor an
// explicit `{-# SPECIALIZE #-}` at the call site makes that happen in this
// pipeline (`TIDEPOOL_DUMP_CLOSED` shows the call site still applying the
// generic, un-inlined top-level binding). See `Tidepool.Fork`'s module
// haddock for the full finding. `forkFilter` has no such requirement — its
// fanout always answers a fixed `Bool` — so it's the one combinator that
// composes over `runLLMTurnFanout` cleanly.
//
// Unlike `works`/`works_with_imports` (NullDispatcher), a
// `runLLMTurnFanout` site genuinely dispatches an `Ask` effect (tag 8,
// same as `run_llm_turn_sidecar.rs`'s `ASK_TAG`) — this probe answers it
// with a scripted `DispatchEffect` so the eval runs straight through to a
// final value, exactly as a harness-driven fanout resume would.
// ---------------------------------------------------------------------------

/// Same shape as `eval_raw_with_imports`, generic over the dispatcher so a
/// scripted `Ask` responder can stand in for the calling agent.
fn eval_with_dispatch<H: DispatchEffect<()>>(
    imports: &str,
    code: &str,
    dispatcher: &mut H,
) -> Result<serde_json::Value, String> {
    // `standard_decls()` no longer carries `Fork` (vestigial-subsystems
    // review §4 — the ordinary session engine never services it); the two
    // callers of this helper (`works_fork`, `works_fork_map`) genuinely
    // exercise `Tidepool.Fork`'s JIT dispatch, so widen the roster here,
    // explicitly, the same way the harness's `agent_decls` does. Fork lands
    // at the same tag (10) it always did — it was always the tail element.
    let mut decls = tidepool_mcp::standard_decls();
    decls.push(tidepool_mcp::fork_decl());
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, imports, "", None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    let include = [
        hs.as_path(),
        lib.as_path(),
        effects_dir.core.as_path(),
        effects_dir.shim.as_path(),
    ];
    match compile_and_run(&src, "result", &include, dispatcher, &()) {
        Ok(v) => Ok(v.to_json()),
        Err(e) => Err(tidepool_runtime::classify(&e).message),
    }
}

/// Answers ONE fanout dispatch with a fixed `[Bool]` list.
struct BoolListOnce {
    answer: Vec<bool>,
}

impl DispatchEffect<()> for BoolListOnce {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        assert_eq!(tag, FORK_TAG, "expected the fanout's Fork dispatch");
        cx.respond_list(self.answer.clone())
    }
}

/// `forkFilter` (`Tidepool.Fork`, Wave C) runs on the JIT: answers a REAL
/// `runLLMTurnFanout` dispatch (not a NullDispatcher stub), keeping only
/// the elements whose scripted verdict is `True`, in original order.
#[test]
fn works_fork() {
    let mut filter_d = BoolListOnce {
        answer: vec![true, false, true, false],
    };
    let got = eval_with_dispatch(
        "Tidepool.Fork",
        "do { ys <- forkFilter (\\x -> T.pack (show (x :: Int))) [1, 2, 3, 4 :: Int]; \
         pure (toJSON (ys :: [Int])) }",
        &mut filter_d,
    )
    .unwrap_or_else(|e| panic!("forkFilter probe failed: {e}"));
    assert_eq!(got, serde_json::json!([1, 3]));
}

/// Answers ONE fanout dispatch with a fixed `[Int]` list.
struct IntListOnce {
    answer: Vec<i64>,
}

impl DispatchEffect<()> for IntListOnce {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        assert_eq!(tag, FORK_TAG, "expected the fanout's Fork dispatch");
        cx.respond_list(self.answer.clone())
    }
}

/// `forkMap` (`Tidepool.Fork`, combinator-sites widen) runs end to end on the
/// JIT: a CALLER-chosen answer type (`@Int`, not forkFilter's fixed `Bool`)
/// reaches a REAL `runLLMTurnFanout`-shaped dispatch — the mechanism
/// `Tidepool.Fork`'s module haddock and `works_fork`'s doc comment describe
/// as the previously-blocked wall, closed by the combinator-sites extract
/// pass.
#[test]
fn works_fork_map() {
    let mut map_d = IntListOnce {
        answer: vec![10, 20, 30, 40],
    };
    let got = eval_with_dispatch(
        "Tidepool.Fork",
        "do { ys <- forkMap @Int (\\x -> T.pack (show (x :: Int))) [1, 2, 3, 4 :: Int]; \
         pure (toJSON (ys :: [Int])) }",
        &mut map_d,
    )
    .unwrap_or_else(|e| panic!("forkMap probe failed: {e}"));
    assert_eq!(got, serde_json::json!([10, 20, 30, 40]));
}

// --- Lone family — nothing else to bundle it with, so it stays its own test. ---

/// `fmtInt`/`fmtFrac`/`fmtStr`/`fmtChar`/`fmtSigned`/`fmtPlain`
/// (`Tidepool.QQ.Fmt.Runtime`) — called in the exact argument shape
/// `[fmt|...|]` generates (`Tidepool.QQ.Fmt`'s `emitInt`/`emitFrac`/
/// `emitStr`/`emitChar`/`emitDefault`: sign, then type-specific flags, then
/// `grp`/width/fill/align, then the value last).
///
/// `fmtInt FMinus 10 .. 6 '0' FRight (-42)` = "000-42": `fpad` lays fill
/// BEFORE the sign for right-alignment (not sign-aware zero-padding like
/// printf's `%06d`) — pre="-", body="42", pad=6-3=3 chars of "0" first.
/// `fmtInt FPlus 16 True True .. 0 ' ' FRight 255` = "+0XFF": explicit `+`
/// sign, uppercase hex, `0x`/`0X` alt-form prefix. `fmtInt .. (Just ',') ..
/// 1234567` groups every 3 digits: "1,234,567".
///
/// `fmtFrac` pins the rounding-tie boundary at 2 decimal places with EXACT
/// dyadic-fraction ties, so no floating-point rounding noise reaches `round`:
/// 0.125 (1/8) * 100 = 12.5 exactly, ties DOWN to the even 12 -> "0.12";
/// 0.375 (3/8) * 100 = 37.5 exactly, ties UP to the even 38 -> "0.38"; at 0
/// decimal places, 2.5 ties to the even 2 -> "2" (same banker's-rounding
/// `round` primop as `works_round_bankers`). A
/// negative value through a width/zero-fill: `fmtFrac .. 2 8 '0' FRight
/// (-3.14159)` = "000-3.14". Percent mode pre-multiplies by 100 and appends
/// "%": `fmtFrac FMinus True 1 .. 0.4567` = "45.7%".
///
/// `fmtStr (Just 3) .. FLeft "hello"` truncates to "hel"; `fmtStr Nothing 6
/// '.' FRight "hi"` pads to "....hi". `fmtChar 3 '*' FLeft 65` treats 65 as
/// a code point ('A') and left-pads: "A**".
///
/// `fmtSigned` recovers the sign from a leading '-' in the ALREADY-RENDERED
/// text: `fmtSigned FPlus 6 '0' FRight "-42"` = "000-42" (negative, sign
/// from the text, not from `FPlus`); `fmtSigned FPlus 6 '0' FRight "42"` =
/// "000+42" (non-negative, so `FPlus`'s explicit "+" is used).
/// `fmtPlain 8 '-' FCenter "hi"` centers with no sign logic: "---hi---".
#[test]
fn works_fmt_runtime_helpers_pinned() {
    works(
        "pure (object [\"int_neg_zero_pad\" .= fmtInt FMinus 10 False False Nothing 6 '0' FRight (-42), \
         \"int_hex_alt_plus\" .= fmtInt FPlus 16 True True Nothing 0 ' ' FRight 255, \
         \"int_group_commas\" .= fmtInt FMinus 10 False False (Just ',') 0 ' ' FRight 1234567, \
         \"frac_tie_even_down\" .= fmtFrac FMinus False 2 0 ' ' FRight 0.125, \
         \"frac_tie_even_up\" .= fmtFrac FMinus False 2 0 ' ' FRight 0.375, \
         \"frac_zero_prec_round\" .= fmtFrac FMinus False 0 0 ' ' FRight 2.5, \
         \"frac_neg_padded\" .= fmtFrac FMinus False 2 8 '0' FRight (-3.14159), \
         \"frac_percent\" .= fmtFrac FMinus True 1 0 ' ' FRight 0.4567, \
         \"str_truncate\" .= fmtStr (Just 3) 0 ' ' FLeft \"hello\", \
         \"str_pad_right\" .= fmtStr Nothing 6 '.' FRight \"hi\", \
         \"char_left_pad\" .= fmtChar 3 '*' FLeft 65, \
         \"signed_neg\" .= fmtSigned FPlus 6 '0' FRight \"-42\", \
         \"signed_pos\" .= fmtSigned FPlus 6 '0' FRight \"42\", \
         \"plain_center\" .= fmtPlain 8 '-' FCenter \"hi\"])",
        serde_json::json!({
            "int_neg_zero_pad": "000-42",
            "int_hex_alt_plus": "+0XFF",
            "int_group_commas": "1,234,567",
            "frac_tie_even_down": "0.12",
            "frac_tie_even_up": "0.38",
            "frac_zero_prec_round": "2",
            "frac_neg_padded": "000-3.14",
            "frac_percent": "45.7%",
            "str_truncate": "hel",
            "str_pad_right": "....hi",
            "char_left_pad": "A**",
            "signed_neg": "000-42",
            "signed_pos": "000+42",
            "plain_center": "---hi---"
        }),
    );
}

// --- (d) DISTINCT-MECHANISM: quoter bracket-depth tracking, not a stdlib
// function's JIT-safety. ---

/// MUST-NOT-BREAK companion: a `}` INSIDE a hole's expression that is NOT in
/// a string — an explicit-brace `let { … }` block (the same construct the
/// module haddock cites for bracket-depth tracking) — also stays legal. The
/// hole's own closing `}` is only recognized at bracket depth 0, so the
/// nested `{ y = 1 }`'s `}` decrements depth instead of ending the hole.
///
/// Formerly SANCTIONED-RED (`plans/post-restart/gate-runbook.md`):
/// `Tidepool.QQ.HsMeta.Translate.toExp` had no case for `HsLet`, so any
/// hole containing `let ... in ...` hit the catch-all `todo`/`noTH` at
/// compile time. Fixed by giving `toExp` an `HsLet` case backed by a new
/// `toDecs`/`toDec` (translating the two binding shapes an ordinary `let`
/// can introduce: a simple variable binding and a single-clause function
/// binding). Kept standalone as class (d) — it pins the quoter's
/// bracket-depth parsing, not a stdlib function.
#[test]
fn qq_fmt_brace_inside_hole_non_string_expr_still_works() {
    works_with_imports(
        QQ_IMPORTS,
        "pure [fmt|{let { y = 1 :: Int } in y}|]",
        serde_json::json!("1"),
    );
}

/// A let-bound TYPE SIGNATURE (`y :: Int` as its own declaration, distinct
/// from the expression-level ascription `y = 1 :: Int` pinned above) used to
/// be silently dropped by `toDecs` — it destructured `ValBinds`' `_sigs`
/// field and threw it away instead of translating or failing loudly. Fixed
/// by `toDecs` translating `ValBinds`' sigs into `TH.SigD`s (`toSigDecs`).
/// This pins that the signature is honored (the binding still evaluates
/// correctly) rather than silently vanishing.
#[test]
fn qq_fmt_let_bound_type_signature_translated() {
    works_with_imports(
        QQ_IMPORTS,
        "pure [fmt|{let { y :: Int; y = 1 } in y}|]",
        serde_json::json!("1"),
    );
}
