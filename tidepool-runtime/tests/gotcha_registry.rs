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

use std::io::Write;
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

/// `FromJSON` — the pure `Value -> a` structural-decode layer that backs
/// `parseJson`. Typeclass-dictionary dispatch over `Value` constructor matches,
/// running on the JIT: `[a]` traverses, `Int` reads a `Number`, the polymorphic
/// `fromJSON` round-trips a `toJSON`-built `Value`. (The `ParseJson` *effect*
/// half — serde_json text→Value — is exercised by live eval + the FromCore
/// roundtrip; the `NullDispatcher` here returns `0` for all effects, so only the
/// pure layer is probeable.) Feature shipped 2026-06-22.
#[test]
fn works_from_json() {
    // FromJSON [Int]: build an Array via toJSON, decode it back, sum it.
    works(
        r#"pure (case (fromJSON (toJSON [1,2,3::Int]) :: Result [Int]) of { Success xs -> sum xs; Error _ -> (-1) })"#,
        serde_json::json!(6),
    );
    // FromJSON Value is identity; mismatches surface as Error, not a crash.
    works(
        r#"pure (case (fromJSON (toJSON ("hi"::Text)) :: Result Int) of { Success _ -> "wrong"::Text; Error _ -> "mismatch-ok" })"#,
        serde_json::json!("mismatch-ok"),
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

/// SHOW precedence (ledger #30, FIXED): a NEGATIVE Double in constructor-arg
/// position is parenthesized (`Just (-2.5)`) — the `showParen (p > 6)` that the
/// JIT-safe `showSignedFloat` replacement used to drop, now restored via the
/// `ShowSignedDoubleAddr` primop (parens decided in the Rust host, no Core
/// compare; the precedence — `appPrec1`=11 nested, `minExpt`=0 top-level —
/// resolves after unblocking `minExpt` in Resolve.hs). Top-level (prec 0) and
/// positive values are NOT parenthesized; the Int path is unaffected; nesting
/// inside a list threads the precedence too.
#[test]
fn works_show_negative_double_parens() {
    works(
        "pure (object [ \"nested\" .= (show (Just (-2.5 :: Double)) :: Text) \
         , \"top\" .= (show (-2.5 :: Double) :: Text) \
         , \"pos\" .= (show (Just (1.5 :: Double)) :: Text) \
         , \"int\" .= (show (Just (-1 :: Int)) :: Text) \
         , \"list\" .= (show [Just (-2.5 :: Double), Nothing] :: Text) ])",
        serde_json::json!({
            "nested": "Just (-2.5)",
            "top": "-2.5",
            "pos": "Just 1.5",
            "int": "Just (-1)",
            "list": "[Just (-2.5),Nothing]"
        }),
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

/// FOOTGUN (ledger #34): `Map.fromList` on a LARGE ASCENDING list hits GHC's
/// `fromDistinctAscList` fast-path — a non-tail balanced-tree build that
/// overflows the JIT's ~10-20k non-tail limit. Counterintuitive: the sorted
/// case (GHC's *fast* path) is the one that breaks. Must fail CLEAN (a named
/// yield), never a silent SIGSEGV. Unsorted input takes the tail-safe
/// `foldl' insert` path — covered by the WORKS probe below.
#[test]
fn loud_fail_map_fromlist_large_sorted_overflows() {
    loud_fail(
        "pure (Map.size (Map.fromList [(i, i) | i <- [1..12000 :: Int]]))",
        "stack overflow",
    );
}

/// The #34 WORKAROUND is safe at scale: `Map.fromListWith` uses an `insertWith`
/// fold (no ascending fast-path), so a large SORTED input is fine — and
/// `Map.fromList` on UNSORTED input takes the same tail-safe path.
#[test]
fn works_map_fromlistwith_large_sorted_safe() {
    works(
        "pure (Map.size (Map.fromListWith const [(i, i) | i <- [1..12000 :: Int]]))",
        serde_json::json!(12000),
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

/// `read :: Double` also WORKS on the native-bignum toolchain. Root CLAUDE.md
/// item 0 claimed BOTH `:: Int` AND `:: Double` die at compile time with
/// "__gmpn_add_1". The `:: Int` case was already flipped (stale_doc_read_now_works);
/// this probe pins the `:: Double` variant. The Read lexer for Double goes through
/// the same native-bignum integer path so the __gmpn_* wall is gone for both.
#[test]
fn stale_doc_read_double_now_works() {
    works("pure (P.read \"42.5\" :: Double)", serde_json::json!(42.5));
}

/// A near-DBL_MAX Double LITERAL now WORKS on the native-bignum toolchain (was a
/// clean gmp COMPILE error via the integerAdd/integerSub shims). Flipped from
/// loud_fail 2026-06-22. Moderate literals were always fine (see
/// `works_moderate_double_literals`).
#[test]
fn stale_doc_large_double_literal_now_works() {
    works("pure (1.79e308 :: Double)", serde_json::json!(1.79e308));
}

/// `cycle` WORKS (fixed 2026-07-01): base's inlined body floats its
/// corecursive knot (`xs' = xs ++ xs'`) to a top-level self-recursive simple
/// binding, which the LetRec emit now knot-ties (promised captures → null
/// placeholder slot → pending_capture_updates patch) instead of silently
/// dropping the self-capture → unresolved_var_trap on force. Pins BOTH the
/// re-exported name and the qualified base path.
#[test]
fn works_cycle_value_knot() {
    works(
        "pure (object [\"cyc\" .= (take 5 (cycle [1,2,3]) :: [Int]), \"qual\" .= (take 4 (P.cycle \"ab\") :: String)])",
        serde_json::json!({"cyc": [1,2,3,1,2], "qual": "abab"}),
    );
}

/// BUG-8 dead (2026-07-02): integers survive the JSON path exactly. The
/// vendored aeson `Number` is Double-backed, so `toJSON` on Int/Integer
/// LOST precision past 2^53 at CONSTRUCTION (silent wrong DATA — found by
/// the kata debt-ledger sweep). Ints now ride the exact `NumberI` carrier
/// end-to-end (ToJSON instances, [j|] integral literals, serde bridge,
/// render arm, optics `_Int`).
#[test]
fn works_exact_int_json() {
    works(
        "pure (object [\"a\" .= (912345678901234567 :: Int), \"b\" .= (9007199254740993 :: Int), \"neg\" .= (-42 :: Int), \"rt\" .= (toJSON (9007199254740993 :: Int) ^? _Int)])",
        serde_json::json!({"a": 912345678901234567_i64, "b": 9007199254740993_i64, "neg": -42, "rt": 9007199254740993_i64}),
    );
}

/// Vendored `lines`/`words` (friction #10, 2026-07-02): guarded corecursion.
/// The external Data.Text bodies overflowed the JIT stack when the list was
/// built without a fused consumer; semantics must stay Data.Text-exact
/// (`lines "a\nb\n" == ["a","b"]` — no empty final segment).
#[test]
fn works_lines_words_vendored() {
    works(
        "pure (object [\"n\" .= length (lines (T.replicate 20000 \"x\\n\")), \"trail\" .= lines \"a\\nb\\n\", \"mid\" .= lines \"a\\n\\nb\", \"ws\" .= words \" a b\\tc \"])",
        serde_json::json!({"n": 20000, "trail": ["a","b"], "mid": ["a","","b"], "ws": ["a","b","c"]}),
    );
}

/// NON-tail recursion overflows (~10-20K frames) with a CLEAN "stack overflow"
/// yield error — never SIGSEGV. (500k non-tail frames here.) Contrast
/// `works_tco_deep_tail_recursion`.
///
/// Regression guard for the masked-StackOverflow bug (fixed 2026-06-21,
/// effect_machine `parse_result`): when the top-level result is itself a thunk,
/// the deep recursion runs inside `force_ptr`, so the depth guard sets
/// StackOverflow AFTER `parse_result`'s pre-force error check — the tag-0 poison
/// closure was then mis-reported as "unexpected heap tag: 0". `parse_result` now
/// re-checks `take_runtime_error()` after forcing. (The bug was invisible on the
/// MCP server, which surfaced the still-pending error via a later teardown path;
/// only the library/`compile_and_run` path exposed it.)
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
/// Regression guard for the same masked-StackOverflow bug as
/// `loud_fail_nontail_recursion_overflow` (fixed 2026-06-21 in `parse_result`).
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
    // legitimate ambiguity. (The Lsp effect's node type is `LspNode`, so it no
    // longer contributes to this collision.) Qualify Data.Tree — the point is
    // its `Node` resolves + `treeDepth` runs (the freer `Node` must not shadow
    // the qualified constructor).
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

/// DuplicateRecordFields shared selector — the `Hit`/`Doc` library records both
/// define a `path` field (legal under `DuplicateRecordFields`). In GHC 9.2+ the
/// selectors keep the bare occ name `path` (record-field namespace, no `$sel:`
/// mangling), so `stableVarId` fingerprinted identical `Tidepool.Records:path`
/// strings → ONE varId for two distinct selectors. The DataConTable / external
/// resolver coalesced them: `getField @"path" @Hit` bound to whichever selector
/// won, and applying `Doc`'s selector to a `Hit` value (or vice-versa) hit a
/// CASE TRAP ("scrutinee constructor not among case alternatives"). Type-checks,
/// then traps at runtime = compiler bug. Fixed in `Translate.hs` by folding the
/// record selector's parent tycon into its varId (`stableVarIdWith`), so
/// `path`@Hit ≠ `path`@Doc. BOTH accesses must now return the right field.
#[test]
fn works_dup_record_fields_shared_selector() {
    // Hit.path (shared field) via OverloadedRecordDot — the trapping case.
    works("pure ((Hit \"a\" 1 \"b\").path)", serde_json::json!("a"));
    // Doc.path (the OTHER record sharing `path`) — must resolve to Doc's field.
    works("pure ((Doc \"p\" \"body\").path)", serde_json::json!("p"));
    // Both in one eval, plus a non-shared field each, to prove no cross-wiring:
    // Hit.line (unique to Hit) and Doc.body (unique to Doc) still resolve.
    works(
        "pure (object [\"hp\" .= (Hit \"a\" 1 \"b\").path, \"dp\" .= (Doc \"p\" \"body\").path, \
         \"hl\" .= (Hit \"a\" 1 \"b\").line, \"db\" .= (Doc \"p\" \"body\").body])",
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

// =========================================================================
// NEWLY ADDED (2026-07-01 gotcha-claims-sweep): claims from root CLAUDE.md
// "Monomorphic Shadows (Prelude)" and haskell/CLAUDE.md "Known Limits" that
// were testable but had no probe in this registry.
// =========================================================================

/// `showDouble` monomorphic shadow: `Translate.hs` intercepts `showDouble` /
/// `$fShowDouble_$sshowSignedFloat` and emits the `ShowDoubleAddr` primop
/// (avoids `floatToDigits`/Integer, which are unsupported FFI). `show ::
/// Show a => a -> Text` in the MCP preamble calls through this shadow for
/// `Double`. Decimal notation for 0.1 ≤ |x| < 1e7, scientific notation
/// otherwise; Haskell-style mantissa always includes ".".
///
/// Doc claim: "intercepted at binding level by Translate.hs, emits
/// `ShowDoubleAddr` primop (avoids `floatToDigits`/Integer)" (root CLAUDE.md
/// "Monomorphic Shadows (Prelude)"). Regression guard: if the interception
/// breaks, `show (3.14 :: Double)` raises a runtime error (the fallback body
/// is `error "showDouble: should be intercepted by Translate"`).
#[test]
fn works_show_double_monomorphic_shadow() {
    works(
        "pure (object [\"pi\" .= show (3.14 :: Double), \"one\" .= show (1.0 :: Double), \
         \"big\" .= show (1.0e10 :: Double), \"neg\" .= show (-2.5 :: Double)])",
        serde_json::json!({"pi":"3.14","one":"1.0","big":"1.0e10","neg":"-2.5"}),
    );
}

/// Empty `Text` operations work correctly on the JIT. The memory entry
/// `empty-text-interp-string-space.md` notes these were verified 2026-06-22 —
/// the `LitString([])` choke is oracle-only, not a JIT issue. Regression
/// guard: the `LetRec` sibling-capture fix (`emit_letrec_phases`) touched
/// these paths and originally manifested as `T.split` on empty text returning
/// `<closure>` instead of `[""]`.
///
/// Doc claim: "JIT handles empty Text correctly (verified live:
/// null/uncons/unpack/Eq/length)" (memory `empty-text-interp-string-space`).
#[test]
fn works_empty_text_ops() {
    works(
        "pure (object [\"isnull\" .= isNull (\"\" :: Text), \
         \"len\" .= len (\"\" :: Text), \
         \"eq\" .= (\"\" == (\"\" :: Text)), \
         \"split\" .= splitOn \"/\" \"\"])",
        serde_json::json!({"isnull":true,"len":0,"eq":true,"split":[""]}),
    );
}

/// GHC -O2 loopifies a no-base-case non-tail recursion (`go n = n + go
/// (n+1)`) into a tight spin — it runs until the eval *timeout*, NOT until
/// "stack overflow". Contrast `loud_fail_nontail_recursion_overflow` where
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
