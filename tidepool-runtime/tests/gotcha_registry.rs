//! Executable gotcha registry — the documented footguns, Known-Limits, and
//! Dangerous-Patterns turned into live eval probes that PIN current behavior.
//!
//! Purpose (three jobs):
//!   1. REGRESSION GUARD — a "stale fear that now works" (sum/Floating/round/
//!      even-odd/nub/Integer-defaulting/insertWith, and the once-forbidden
//!      infinite-list idioms) must keep working. If one flips to an error, the
//!      named test fails loud.
//!   2. CLEAN-FAILURE GUARD — an unsupported thing (read/Integer-GMP, a
//!      near-DBL_MAX Double literal, `cycle`, non-tail-recursion overflow,
//!      `let`-in-braced-`do`, a giant lens fold) must fail with a CLEAN, named
//!      error — never a silent SIGILL/SIGSEGV or silent-wrong output. Each
//!      LOUD-FAIL probe asserts the error text carries the expected marker.
//!   3. STALE-DOC FLAG — several probes prove the docs OVERSTATE danger. Those
//!      are pinned here AND the doc lines are trued up (CLAUDE.md "Known
//!      Limits", memory `tidepool-style-guide.md`). See the per-probe comments.
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

// `1.41421…` (sqrt 2) and `3.14` are pinned round-trip eval outputs, not math
// constants — assert them verbatim.
#![allow(clippy::approx_constant)]

use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

/// Never invoked — every probe is pure (no effect is `send`ed). Present only so
/// the full effectful preamble compiles.
struct NullDispatcher;
impl DispatchEffect<()> for NullDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        cx.respond(serde_json::json!(0))
    }
}

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
    let include = [hs.as_path(), lib.as_path(), effects_dir.as_path()];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        Err(e) => Err(format!("{e}")),
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
fn loud_fail(code: &str, marker: &str) {
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

// =========================================================================
// CLASS 1 — WORKS: "stale fears, verified gone". Assert the correct value.
// A failure here is a regression in a feature the docs already call fixed.
// =========================================================================

/// `sum`/`product`/`maximum`/`minimum`/`foldr1` — the error-worker-sentinel
/// class (CLAUDE.md "Stale fears, verified gone", commit 4273c51). The lazy
/// poison closure defers the dictionary `error` branches. `foldr1` is not
/// re-exported unqualified, so it rides `P.foldr1`.
#[test]
fn works_error_worker_folds() {
    works(
        "pure (object [\"sum\" .= sum [1..100::Int], \"product\" .= product [1..5::Int], \
         \"maximum\" .= maximum [3,1,4,1,5,9,2,6::Int], \"minimum\" .= minimum [3,1,4,1,5,9::Int], \
         \"foldr1\" .= P.foldr1 (+) [1,2,3,4::Int]])",
        serde_json::json!({"sum":5050,"product":120,"maximum":9,"minimum":1,"foldr1":10}),
    );
}

/// `nub` — works (O(n²) but correct), no longer a SIGILL fear.
#[test]
fn works_nub_dedup() {
    works(
        "pure (nub [1,1,2,3,3,2::Int])",
        serde_json::json!([1, 2, 3]),
    );
}

/// `Floating` ops (`sqrt`/`exp`/`log`) — the lazy poison closure fix defused
/// the Floating-dictionary error branches. The style-guide "Dangerous Patterns"
/// row claiming `sqrt`/`sin`/`cos`/`exp`/`log` have "no workaround" is STALE.
/// (`exp 0.0` and `log 1.0` are integral Doubles → rendered as integers.)
#[test]
fn works_floating_ops() {
    works(
        "pure (object [\"sqrt2\" .= (sqrt 2.0 :: Double), \"exp0\" .= (exp 0.0 :: Double), \
         \"log1\" .= (log 1.0 :: Double)])",
        serde_json::json!({"sqrt2":1.4142135623730951,"exp0":1,"log1":0}),
    );
}

/// `round :: Double -> Int` — banker's rounding (ties to even) via the
/// monomorphic shadow (`rintDouble` FFI is unsupported). [0.5,1.5,2.5,3.5] →
/// [0,2,2,4], NOT [1,2,3,4].
#[test]
fn works_round_bankers() {
    works(
        "pure (map (\\d -> round d :: Int) [0.5, 1.5, 2.5, 3.5 :: Double])",
        serde_json::json!([0, 2, 2, 4]),
    );
}

/// `even`/`odd` — GHC specialization removed the need for the old monomorphic
/// shadows; the Integral dictionary is specialized away.
#[test]
fn works_even_odd() {
    works(
        "pure (object [\"evens\" .= map even [1,2,3,4::Int], \"odds\" .= map odd [1,2,3,4::Int]])",
        serde_json::json!({"evens":[false,true,false,true],"odds":[true,false,true,false]}),
    );
}

/// Integer defaulting in an UNTYPED local recursive helper — once feared to
/// pull GMP `integerAdd`/`integerSub`, now resolved by the load-bearing Integer
/// shims + `default (Int, Text)`. `fac 10` with no signature → 3628800.
#[test]
fn works_integer_defaulting_untyped_helper() {
    works(
        "pure (fac 10) where { fac n = if n <= 1 then 1 else n * fac (n-1) }",
        serde_json::json!(3628800),
    );
}

/// `Map.insertWith (+)` — the combining-insert shadow is correct (recent
/// ledger note: "insertWith retirable"). {a:10,b:2} + (a,1) → {a:11,b:2}.
#[test]
fn works_map_insertwith() {
    works(
        "pure (toJSON (Map.insertWith (+) (\"a\"::Text) (1::Int) \
         (Map.fromList [(\"a\",10),(\"b\",2)])))",
        serde_json::json!({"a":11,"b":2}),
    );
}

/// `Map.fromListWith (+) [("k", 1)]` — style-guide gotcha #15 says this needs
/// an explicit key annotation to dodge an ambiguous-type error. In the MCP eval
/// context that is OVERSTATED: `default (Int, Text)` resolves key→Text,
/// value→Int, so it compiles and runs unannotated. (Pinned to flag the doc.)
#[test]
fn works_map_fromlistwith_default_resolves() {
    works(
        "pure (toJSON (Map.fromListWith (+) [(\"k\", 1)]))",
        serde_json::json!({"k":1}),
    );
}

/// `takeWhile`/`span` are lazy-safe (jit-eager-argument-position memo). Bounded
/// inputs here; the infinite-input laziness is covered by the STALE-DOC class.
#[test]
fn works_lazy_safe_combinators() {
    works(
        "pure (object [\"takeWhile\" .= takeWhile (< 5) (enumFromTo 1 100 :: [Int]), \
         \"span\" .= span (< 3) [1,2,3,4,5::Int]])",
        serde_json::json!({"takeWhile":[1,2,3,4],"span":[[1,2],[3,4,5]]}),
    );
}

/// TCO: a deep TAIL-recursive loop (500k frames) returns cleanly — contrast
/// with `loud_fail_nontail_recursion_overflow`. Pins "tail recursion is
/// unbounded", retiring the long-dead "recursion depth ~20 max" rule.
#[test]
fn works_tco_deep_tail_recursion() {
    works(
        "pure (go 500000 0 :: Int) where { go n acc = if n == 0 then acc else go (n-1) (acc+1) }",
        serde_json::json!(500000),
    );
}

/// Moderate Double literals render fine — documents the boundary for the
/// near-DBL_MAX GMP trap below (3.14, 1.0e10, 1.23e100 all stay within the
/// integerAdd/integerSub shims). 1.0e10 is integral → integer JSON.
#[test]
fn works_moderate_double_literals() {
    works(
        "pure (object [\"a\" .= (3.14 :: Double), \"b\" .= (1.0e10 :: Double), \
         \"c\" .= (1.234567890123456e100 :: Double)])",
        serde_json::json!({"a":3.14,"b":10000000000i64,"c":1.234567890123456e100}),
    );
}

// =========================================================================
// CLASS 2 — STALE-DOC, now WORKS: probes that prove the "Dangerous Patterns"
// table OVERSTATES danger. These idioms were documented SIGILL/SIGSEGV; the
// lazy infrastructure (thunked Con fields, guarded-corecursion filter/nubBy,
// shipped lazy effect results) made bounded consumption correct. The matching
// doc lines are trued up in tidepool-style-guide.md.
// =========================================================================

/// `take n [0..]` / `take n (repeat x)` / `take n (iterate f x)` — bounded
/// consumption of an "infinite" producer now works. Style-guide table claims
/// these SIGILL ("Eager Con fields, no thunks"); STALE.
#[test]
fn stale_doc_infinite_list_take_now_works() {
    works(
        "pure (object [\"enumFrom\" .= (take 3 [0..] :: [Int]), \
         \"repeat\" .= (take 3 (repeat (7::Int)) :: [Int]), \
         \"iterate\" .= (take 4 (iterate (*2) (1::Int)))])",
        serde_json::json!({"enumFrom":[0,1,2],"repeat":[7,7,7],"iterate":[1,2,4,8]}),
    );
}

/// `zipWith f xs [0..]` / `take n (filter p [0..])` / `take n (map f [0..])` —
/// lazy transforms over an infinite list now work. Style-guide table claims
/// `zipWith f xs [0..]` "doesn't fuse, infinite list" → crash; STALE. Confirms
/// jit-eager-argument-position's filter/nubBy lazy-safety claim.
#[test]
fn stale_doc_infinite_list_transform_now_works() {
    works(
        "pure (object [\"zipWith\" .= zipWith (\\a b -> a + b) [10,20,30::Int] [0..], \
         \"filter\" .= take 3 (filter even [0..] :: [Int]), \
         \"map\" .= take 4 (map (*2) [0..] :: [Int])])",
        serde_json::json!({"zipWith":[10,21,32],"filter":[0,2,4],"map":[0,2,4,6]}),
    );
}

// =========================================================================
// CLASS 3 — LOUD-FAIL: unsupported things that must fail with a CLEAN, named
// error (never a silent SIGILL/SIGSEGV / wrong output). Assert Err + marker.
// =========================================================================

/// `read` now WORKS on the native-bignum toolchain — the `__gmpn_*` wall is gone
/// (was a clean gmp COMPILE error). Flipped from loud_fail 2026-06-22; the
/// deployed extract uses GHC's native ghc-bignum.
#[test]
fn stale_doc_read_now_works() {
    works("pure (P.read \"42\" :: Int)", serde_json::json!(42));
}

/// A near-DBL_MAX Double LITERAL now WORKS on the native-bignum toolchain (was a
/// clean gmp COMPILE error via the integerAdd/integerSub shims). Flipped from
/// loud_fail 2026-06-22. Moderate literals were always fine (see
/// `works_moderate_double_literals`).
#[test]
fn stale_doc_large_double_literal_now_works() {
    works("pure (1.79e308 :: Double)", serde_json::json!(1.79e308));
}

/// `cycle` is an unresolved external — clean yield error ("unresolved
/// variable"), NOT the silent SIGSEGV the style-guide table implies. Use manual
/// recursion / `replicate`.
#[test]
fn loud_fail_cycle_unresolved_external() {
    loud_fail("pure (take 3 (cycle [1,2,3]) :: [Int])", "unresolved");
}

/// NON-tail recursion overflows (~10-20K frames) with a CLEAN "stack overflow"
/// yield error — never SIGSEGV. (500k non-tail frames here.) Contrast
/// `works_tco_deep_tail_recursion`.
///
/// BUG-FIND (2026-06-21), committed RED to track: this is the CORRECT behavior
/// and the MCP server path delivers it (verified live — clean "stack overflow").
/// But this test's `compile_and_run` + `NullDispatcher` path instead corrupts to
/// "unexpected heap tag: 0" — the JIT's clean recursion-overflow guard does NOT
/// fire on the library path, so deep non-tail recursion blows past it into heap
/// corruption. A real server-vs-library eval divergence, not a test artifact
/// (same stack size now via EVAL_STACK_SIZE; size was ruled out). FIX PENDING.
#[test]
fn loud_fail_nontail_recursion_overflow() {
    loud_fail(
        "pure (go 500000 :: Int) where { go n = if n == 0 then 0 else 1 + go (n-1) }",
        "stack overflow",
    );
}

/// `let` in braced `do` (`do { let x = e; stmt }`) is a GHC PARSE error
/// (style-guide gotcha #10/#207). Clean compile-time failure.
#[test]
fn loud_fail_let_in_braced_do_parse_error() {
    loud_fail("do { let x = 1 :: Int; pure x }", "parse error");
}

/// A large Value tree folded by a lens (`toJSON [1..20000] ^.. values`)
/// overflows with a CLEAN "stack overflow" yield error — NOT the SIGILL the
/// style-guide #18 claims. The cause is a non-tail lens fold, not "complex
/// traversal". Doc trued up in tidepool-style-guide.md.
///
/// BUG-FIND (2026-06-21), committed RED to track: same divergence as
/// `loud_fail_nontail_recursion_overflow` — the MCP server path yields clean
/// "stack overflow" (verified live), but this test's `compile_and_run` +
/// `NullDispatcher` path corrupts to "unexpected heap tag: 0". FIX PENDING.
#[test]
fn loud_fail_large_value_lens_fold_overflow() {
    loud_fail(
        "pure (len (toJSON [1..20000::Int] ^.. values) :: Int)",
        "stack overflow",
    );
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
    let include = [hs.as_path(), lib.as_path(), effects_dir.as_path()];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        Err(e) => Err(format!("{e}")),
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

/// Control.Lens `_last`/`_init`/`unsnoc` on a LIST. With -O2 + cross-module
/// specialization, GHC's `INLINE _Snoc` compiles `xs ^? _last` to a worker that
/// passes the bottoming `lastError "last"` thunk into a demand-analysis-DEAD
/// fallback arg slot. The JIT evaluates App args eagerly, so before the fix
/// `[10,20,30] ^? _last` died with `yield error: Haskell error: last` instead
/// of returning `Just 30`. The fix (a) tags `lastError`/`initError` as error
/// vars and (b) routes an error call in App-argument position through a LAZY
/// poison closure (`EmitFrame::RaiseLazy`) rather than an eager `Raise`. `_head`
/// always worked (Cons, no dead-arg fallback); the empty-list cases must stay
/// `Nothing` (never raise).
#[test]
fn works_lens_last_init_unsnoc_on_list() {
    works_with_imports(
        "Control.Lens (_last, _head, _init, unsnoc)",
        "pure (object \
         [ \"last\" .= ([10,20,30::Int] ^? _last) \
         , \"head\" .= ([10,20,30::Int] ^? _head) \
         , \"init\" .= ([10,20,30::Int] ^? _init) \
         , \"unsnoc\" .= (unsnoc [10,20,30::Int]) \
         , \"last_empty\" .= (([]::[Int]) ^? _last) \
         , \"init_empty\" .= (([]::[Int]) ^? _init) \
         ])",
        serde_json::json!({
            "last": 30,
            "head": 10,
            "init": [10, 20],
            "unsnoc": [[10, 20], 30],
            "last_empty": null,
            "init_empty": null,
        }),
    );
}

/// `"abc" ^? _last` rides the Text `Snoc` instance, NOT the list dead-arg path —
/// it always worked and MUST keep working after the fix.
#[test]
fn works_lens_last_on_text() {
    works_with_imports(
        "Control.Lens (_last)",
        "pure ((\"abc\" :: Text) ^? _last)",
        serde_json::json!("c"),
    );
}
