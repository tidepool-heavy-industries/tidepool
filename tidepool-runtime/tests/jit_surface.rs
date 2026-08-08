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

/// `Fork`'s position in the standard effect stack: 9 base effects at tags
/// 0..8, then the interposed `Ask` (9), `RunLLMTurn` (10), and `Fork` (11) —
/// the roster order in `tidepool-mcp`'s `standard_decls()`.
///
/// `Tidepool.Fork`'s `forkFilter`/`forkMap` reach the machine through
/// `forkAllSited`, which sends on `Fork` — so their fanout dispatch arrives
/// at THIS tag, not `RunLLMTurn`'s and not `Ask`'s.
const FORK_TAG: u64 = 11;

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
/// `eitherDecode`. Typeclass-dictionary dispatch over `Value` constructor
/// matches, running on the JIT: `[a]` traverses, `Int` reads a `Number`, the
/// polymorphic `fromJSON` round-trips a `toJSON`-built `Value`. (The text→Value
/// half is the `JsonDecode` primop — see `works_either_decode` below.)
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

/// `eitherDecode` — the pure, aeson-flavored JSON decoder and the single decode
/// entry point. Text→Value parse is the `JsonDecode` primop (serde_json
/// Rust-side); `Value` has the identity `FromJSON` instance, so `eitherDecode
/// @Value` is the raw parse. Malformed input is `Left <serde msg>`, never an
/// abort and never a silently-discarded error.
#[test]
fn works_either_decode() {
    // Right on valid input (a = Value via the identity FromJSON instance).
    works(
        r#"pure (case (eitherDecode "[1,2,3]" :: Either Text Value) of { Right v -> v; Left _ -> Null })"#,
        serde_json::json!([1, 2, 3]),
    );
    // Left on malformed input, with the serde error message preserved (non-empty).
    works(
        r#"pure (case (eitherDecode "{oops" :: Either Text Value) of { Left e -> Bool (T.length e > 0); Right _ -> Bool False })"#,
        serde_json::json!(true),
    );
}

/// `decode` — the Maybe-flavored aeson decode over the same primop path.
#[test]
fn works_decode() {
    works(
        r#"pure (case (decode "{\"a\":1}" :: Maybe Value) of { Just v -> v; Nothing -> Null })"#,
        serde_json::json!({"a": 1}),
    );
    works(
        r#"pure (case (decode "nope" :: Maybe Value) of { Nothing -> "rejected"::Text; Just _ -> "wat" })"#,
        serde_json::json!("rejected"),
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

/// SHOW precedence: a NEGATIVE Double in constructor-arg position is
/// parenthesized (`Just (-2.5)`) — the `showParen (p > 6)` is decided by the
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

/// Ledger #34 is DEAD, re-pinned after the call-depth fix (plan 01 finding
/// 5): the old counter tripped at ~20k TOTAL calls, so a 12k sorted build
/// "overflowed" falsely. `fromDistinctAscList` builds by halving — real
/// depth is O(log n) — so with depth counted properly the sorted fast-path
/// is fine at any practical scale (verified to 200k). Genuine deep recursion
/// still failing CLEAN is pinned in tidepool-codegen's call-depth tests.
#[test]
fn works_map_fromlist_large_sorted_after_depth_fix() {
    works(
        "pure (Map.size (Map.fromList [(i, i) | i <- [1..12000 :: Int]]))",
        serde_json::json!(12000),
    );
}

/// `Map.fromListWith` at scale: an `insertWith` fold (no ascending
/// fast-path), safe on large sorted input — historically the #34 workaround,
/// now just the natural spelling for combining duplicates.
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
/// with `nontail_recursion_fails_loudly`. Pins "tail recursion is
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
/// integerAdd/integerSub shims). `a`=3.14 renders compact (serde-exact); `b`
/// and `c` are integral → integer JSON. `c` (1.234…e100) is an extreme
/// whole-number magnitude, so it EXPANDS to the exact 101-digit integer
/// (16 significant digits × 10^85) rather than serde's compact `…e+100` — a
/// small coefficient with a large positive exponent is an integer, and the
/// erased `Value` carries nothing to mark it as double-origin. See
/// `large_double_literal_on_jit`.
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
fn infinite_list_take_on_jit() {
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
fn infinite_list_transform_on_jit() {
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

/// `read :: Int` works on the JIT: the deployed extract uses GHC's native
/// ghc-bignum, so the integer Read path carries no `__gmpn_*` dependency.
#[test]
fn read_on_jit() {
    works("pure (P.read \"42\" :: Int)", serde_json::json!(42));
}

/// `read :: Double` also WORKS on the native-bignum toolchain. Root CLAUDE.md
/// item 0 claimed BOTH `:: Int` AND `:: Double` die at compile time with
/// "__gmpn_add_1". The `:: Int` case is covered by read_on_jit;
/// this probe pins the `:: Double` variant. The Read lexer for Double goes through
/// the same native-bignum integer path so the __gmpn_* wall is gone for both.
#[test]
fn read_double_on_jit() {
    works("pure (P.read \"42.5\" :: Double)", serde_json::json!(42.5));
}

/// A near-DBL_MAX Double LITERAL works on the JIT: the native-bignum
/// integerAdd/integerSub shims handle it. Moderate literals are also fine (see
/// `works_moderate_double_literals`). The value renders as the exact expanded
/// integer (179×10^306), not serde's compact `1.79e+308` — extreme
/// whole-number doubles expand (see `works_moderate_double_literals`). Assert
/// the value round-trips rather than pinning the 309-digit string.
#[test]
fn large_double_literal_on_jit() {
    let got = run_probe("pure (1.79e308 :: Double)").expect("large double literal should eval");
    assert_eq!(
        got.as_f64(),
        Some(1.79e308),
        "value must round-trip through the expanded-integer render; got {got}"
    );
}

/// `cycle` works: base's inlined body floats its
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

/// Integers survive the JSON path exactly. The vendored aeson `Number` carries a
/// `Scientific` (exact `Integer` coefficient × 10^exponent), so an Int/Integer
/// rides its coefficient with no Double rounding past 2^53 — exact end-to-end
/// (ToJSON instances, [j|] integral literals, serde bridge, render arm, optics
/// `_Int`).
#[test]
fn works_exact_int_json() {
    works(
        "pure (object [\"a\" .= (912345678901234567 :: Int), \"b\" .= (9007199254740993 :: Int), \"neg\" .= (-42 :: Int), \"rt\" .= (toJSON (9007199254740993 :: Int) ^? _Int)])",
        serde_json::json!({"a": 912345678901234567_i64, "b": 9007199254740993_i64, "neg": -42, "rt": 9007199254740993_i64}),
    );
}

/// Vendored `lines`/`words`: guarded corecursion.
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
/// Guards the masked-StackOverflow case in effect_machine `parse_result`:
/// when the top-level result is itself a thunk,
/// the deep recursion runs inside `force_ptr`, so the depth guard sets
/// StackOverflow AFTER `parse_result`'s pre-force error check — the tag-0 poison
/// closure was then mis-reported as "unexpected heap tag: 0". `parse_result` now
/// re-checks `take_runtime_error()` after forcing. (The bug was invisible on the
/// MCP server, which surfaced the still-pending error via a later teardown path;
/// only the library/`compile_and_run` path exposed it.)
#[test]
fn nontail_recursion_fails_loudly() {
    fails_loudly(
        "pure (go 500000 :: Int) where { go n = if n == 0 then 0 else 1 + go (n-1) }",
        "stack overflow",
    );
}

/// `let` in braced `do` (`do { let x = e; stmt }`) is a GHC PARSE error
/// (style-guide gotcha #10/#207). Clean compile-time failure.
#[test]
fn let_in_braced_do_fails_loudly() {
    fails_loudly("do { let x = 1 :: Int; pure x }", "parse error");
}

/// A large Value tree folded by a lens (`toJSON [1..20000] ^.. values`)
/// overflows with a CLEAN "stack overflow" yield error — NOT the SIGILL the
/// style-guide #18 claims. The cause is a non-tail lens fold, not "complex
/// traversal". Doc trued up in tidepool-style-guide.md.
///
/// Guards the same masked-StackOverflow case as
/// `nontail_recursion_fails_loudly` (in `parse_result`).
#[test]
fn large_value_lens_fold_fails_loudly() {
    fails_loudly(
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
        Err(e) => Err(tidepool_runtime::classify(&e).message),
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

/// DuplicateRecordFields shared selector — the `Hit`/`FileRead` library records
/// both define a `path` field (legal under `DuplicateRecordFields`; `FileRead`
/// is the #335 successor to the old `Doc` record this probe originally pinned
/// against, same shared-field shape). In GHC 9.2+ the selectors keep the bare
/// occ name `path` (record-field namespace, no `$sel:` mangling), so
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
// Claims from root CLAUDE.md "Monomorphic Shadows (Prelude)" and
// haskell/CLAUDE.md "Known Limits", pinned as probes.
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

/// Empty `Text` operations work correctly on the JIT: the `LitString([])`
/// choke is oracle-only, not a JIT issue. Regression
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

// =========================================================================
// The safe-idiom surface: the total forms that stand in for the partials
// (headMay/lastMay/initMay/tailMay/atMay, note/hush, wither/filterA/ordNub,
// readMaybe, (>>>)/(<<<)). Each PIN proves the `Tidepool.Prelude` re-export
// JIT-runs end-to-end. Add a `works_*` probe here when you add a Prelude fn.
// =========================================================================

/// `headMay`/`lastMay`/`initMay`/`tailMay` (safe package) — `Just` on a
/// non-empty list, `Nothing` on empty. Total replacements for bare
/// head/tail/last/init.
#[test]
fn works_safe_list_may_functions() {
    works(
        "pure (object [\"head_some\" .= headMay [1,2,3::Int], \"head_none\" .= headMay ([]::[Int]), \
         \"last_some\" .= lastMay [1,2,3::Int], \"last_none\" .= lastMay ([]::[Int]), \
         \"init_some\" .= initMay [1,2,3::Int], \"init_none\" .= initMay ([]::[Int]), \
         \"tail_some\" .= tailMay [1,2,3::Int], \"tail_none\" .= tailMay ([]::[Int])])",
        serde_json::json!({
            "head_some": 1, "head_none": null,
            "last_some": 3, "last_none": null,
            "init_some": [1,2], "init_none": null,
            "tail_some": [2,3], "tail_none": null
        }),
    );
}

/// `atMay` (safe package) — `Just` at a valid index, `Nothing` out of range.
/// Total replacement for `xs !! i`.
#[test]
fn works_safe_at_may() {
    works(
        "pure (object [\"hit\" .= atMay [10,20,30::Int] 1, \"miss\" .= atMay [10,20,30::Int] 5])",
        serde_json::json!({"hit": 20, "miss": null}),
    );
}

/// `maximumMay`/`minimumMay` (safe package) — `Just` on non-empty, `Nothing`
/// on empty (contrast the always-partial `maximum`/`minimum`, still bare-
/// exported and pinned by `works_error_worker_folds`).
#[test]
fn works_safe_maximum_minimum_may() {
    works(
        "pure (object [\"max_some\" .= maximumMay [3,1,4,1,5::Int], \"max_none\" .= maximumMay ([]::[Int]), \
         \"min_some\" .= minimumMay [3,1,4,1,5::Int], \"min_none\" .= minimumMay ([]::[Int])])",
        serde_json::json!({"max_some": 5, "max_none": null, "min_some": 1, "min_none": null}),
    );
}

/// `readMaybe` (Text.Read) — `Just` on a parseable literal, `Nothing` on
/// garbage. Bare `read` (partial) is still exported/pinned separately
/// (`read_on_jit`); this is the total sibling.
#[test]
fn works_read_maybe() {
    works(
        "pure (object [\"ok\" .= (readMaybe \"42\" :: Maybe Int), \"bad\" .= (readMaybe \"abc\" :: Maybe Int)])",
        serde_json::json!({"ok": 42, "bad": null}),
    );
}

/// `note`/`hush` (errors package) railway helpers. `note` tags a `Nothing`
/// into a `Left e`; `hush` forgets a `Left` back to `Nothing`. `Either` renders
/// via the `{"Left":_}`/`{"Right":_}` `ToJSON` instance (`Tidepool.Aeson.Value`).
#[test]
fn works_note_hush() {
    works(
        "pure (object [\"note_some\" .= (note (\"e\"::Text) (Just (5::Int)) :: Either Text Int), \
         \"note_none\" .= (note (\"e\"::Text) (Nothing :: Maybe Int) :: Either Text Int), \
         \"hush_right\" .= (hush (Right (5::Int) :: Either Text Int)), \
         \"hush_left\" .= (hush (Left (\"e\"::Text) :: Either Text Int))])",
        serde_json::json!({
            "note_some": {"Right": 5}, "note_none": {"Left": "e"},
            "hush_right": 5, "hush_left": null
        }),
    );
}

/// `wither`/`filterA` (witherable package) — effectful filter-map/filter fused
/// through an `Applicative` (here `Maybe`). `wither` is `mapMaybe` with
/// effects; `filterA` is `filter` with effects.
#[test]
fn works_wither_filter_a() {
    works(
        "pure (object [\"wither\" .= fromMaybe [] (wither (\\x -> Just (if even x then Just x else Nothing)) [1,2,3,4,5,6::Int]), \
         \"filterA\" .= fromMaybe [] (filterA (\\x -> Just (even x)) [1,2,3,4::Int])])",
        serde_json::json!({"wither": [2,4,6], "filterA": [2,4]}),
    );
}

/// `ordNub` (witherable package) — the O(n log n) `Ord`-based nub, preserving
/// first-occurrence order like `nub` (pinned separately by `works_nub_dedup`).
#[test]
fn works_ord_nub() {
    works(
        "pure (ordNub [3,1,2,3,1::Int])",
        serde_json::json!([3, 1, 2]),
    );
}

/// `(>>>)`/`(<<<)` (Control.Category) point-free composition. `f >>> g` runs
/// `f` then `g`; `g <<< f` is the same pipeline written in `(.)` order.
#[test]
fn works_category_compose() {
    works(
        "pure (object [\"gt\" .= (((subtract 1) :: Int -> Int) >>> (* 10)) 5, \
         \"lt\" .= (((* 10) :: Int -> Int) <<< subtract 1) 5])",
        serde_json::json!({"gt": 40, "lt": 40}),
    );
}

/// `toGregorian` (Tidepool.Data.Time) — canonical `Data.Time` decomposition,
/// `UTCTime -> (year, month, day)`. Pinned against the same leap-day fixture
/// `formatISO8601`'s own haddock uses (`1709164800000` == 2024-02-29), so this
/// probe and the doc example can't silently drift apart.
#[test]
fn works_to_gregorian() {
    works(
        "pure (toGregorian (UTCTime 1709164800000))",
        serde_json::json!([2024, 2, 29]),
    );
}

/// `formatDay` (Tidepool.Data.Time) — zero-padded `YYYY-MM-DD` date prefix of
/// `formatISO8601`, same fixture as `works_to_gregorian`.
#[test]
fn works_format_day() {
    works(
        "pure (formatDay (UTCTime 1709164800000))",
        serde_json::json!("2024-02-29"),
    );
}

/// Round-trip `parseISO8601 -> formatDay` for a single-digit month+day date —
/// catches a dropped zero-pad (must render "2024-03-05", not "2024-3-5").
#[test]
fn works_parse_iso8601_format_day_roundtrip() {
    works(
        r#"pure (case parseISO8601 "2024-03-05T10:00:00Z" of { Right t -> formatDay t; Left e -> e })"#,
        serde_json::json!("2024-03-05"),
    );
}

/// `Ui` (`haskell/lib/Tidepool/Ui.hs`) constructs on the JIT and serializes
/// to the same JSON contract as `tidepool-harness/src/ui.rs`'s
/// `wire_shape_is_stable` test (externally tagged by `"ui"`, snake_case
/// tags, `options` as 2-element arrays). Compared structurally, not as a
/// literal string — the vendored `object` is `Data.Map.Strict`-backed and
/// always emits keys in ascending order, so it cannot reproduce the Rust
/// struct's declaration-order field sequence; JSON object member order
/// carries no semantics and neither side depends on it.
#[test]
fn works_ui() {
    // `Tidepool.Form` (`Text -> Text -> Form ()`) is auto-imported unqualified
    // by the standard preamble whenever `Ask` is in the effect stack (always,
    // here), and it exports its own `code`/`prose` display combinators of the
    // same bare name as `Tidepool.Ui`'s (`Text -> Text -> Ui`) — importing
    // `Tidepool.Ui` unqualified alone makes bare `code` an "Ambiguous
    // occurrence". `Tidepool.Ui.code` stays reachable qualified, exactly the
    // pattern `tidepool_harness::engine::split_imports` documents for a turn
    // building a raw `Ui` tree by hand.
    works_with_imports(
        "Tidepool.Ui\nqualified Tidepool.Ui as U",
        r#"pure (card "hole"
             [ U.code "haskell" "resume :: Verdict -> M ()"
             , choice "verdict?" [("approve", "Approve")]
             , badge "Exec, Fs" EffectRow
             ])"#,
        serde_json::json!({
            "ui": "card",
            "title": "hole",
            "body": [
                {"ui": "code", "lang": "haskell", "source": "resume :: Verdict -> M ()"},
                {"ui": "choice", "prompt": "verdict?", "options": [["approve", "Approve"]]},
                {"ui": "badge", "label": "Exec, Fs", "kind": "effect_row"}
            ]
        }),
    );
}

/// `[form|...|]` (`haskell/lib/Tidepool/FormQQ.hs`) — the GO half of the
/// Wave-C QQ gate: a QuasiQuoter DEFINED IN THE STDLIB (not shipped with
/// GHC/base) compiles and runs a splice through the real extract pipeline,
/// exactly like the already-shipped `Tidepool.QQ` quoters
/// (`[fmt|]`/`[j|]`/`[patch|]`/`[uri|]`). Parsing (blank-line skip, `choice`/
/// `text`/`multiline`/prose dispatch) happens entirely at COMPILE time inside
/// the splice evaluator; the expansion is a plain `[Ui]` list built from
/// `Tidepool.Ui`'s smart constructors, so `card title [form|...|]` is the
/// idiomatic use — exercised here.
#[test]
fn works_form_qq() {
    works_with_imports(
        "Tidepool.Ui\nTidepool.FormQQ (form)",
        r#"pure (card "setup"
             [form|
Welcome! Fill this in.
choice env: dev prod
text token
multiline notes
|])"#,
        serde_json::json!({
            "ui": "card",
            "title": "setup",
            "body": [
                {"ui": "prose", "text": "Welcome! Fill this in."},
                {"ui": "choice", "prompt": "env", "options": [["dev", "dev"], ["prod", "prod"]]},
                {"ui": "text_in", "prompt": "token", "multiline": false},
                {"ui": "text_in", "prompt": "notes", "multiline": true}
            ]
        }),
    );
}

/// A malformed `[form|...|]` line is a COMPILE-TIME error naming the
/// offending 1-indexed line number — never a silent misparse. Line 2 here
/// (`choice missing colon`) has no `:`.
#[test]
fn form_qq_bad_line_fails_loudly_with_line_number() {
    let imports = "Tidepool.Ui\nTidepool.FormQQ (form)".to_string();
    let code = r#"pure (card "setup"
             [form|
choice missing colon
|])"#
        .to_string();
    let got = std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            eval_raw_with_imports(&imports, &code)
        })
        .unwrap()
        .join()
        .map_err(|_| "thread panicked (HARD crash / uncaught signal)".to_string())
        .and_then(|r| r);
    match got {
        Ok(v) => panic!("expected a compile-time failure naming the bad line, got Ok: {v}"),
        Err(e) => assert!(
            e.contains("form: line 2") && e.contains("requires ': key key ...'"),
            "error must name the offending line and reason, got: {e}"
        ),
    }
}

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
// `runLLMTurnFanout` site genuinely dispatches an `Ask` effect (tag 9,
// same as `run_llm_turn_sidecar.rs`'s `ASK_TAG`) — this probe answers it
// with a scripted `DispatchEffect` so the eval runs straight through to a
// final value, exactly as a harness-driven `answer_fanout` would.
// ---------------------------------------------------------------------------

/// Same shape as `eval_raw_with_imports`, generic over the dispatcher so a
/// scripted `Ask` responder can stand in for the calling agent.
fn eval_with_dispatch<H: DispatchEffect<()>>(
    imports: &str,
    code: &str,
    dispatcher: &mut H,
) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, imports, "", None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    let include = [hs.as_path(), lib.as_path(), effects_dir.as_path()];
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

// =========================================================================
// Aeson numeric fidelity — `FromJSON Int` bounded-integral decoding and the
// `_Int`/`_Integer` prisms, plus the `Data.Text`/`Data.Char` shadows whose
// canonical names must carry canonical semantics (`splitOn`, `digitToInt`).
// =========================================================================

/// `FromJSON Int` decodes an exact integer within `Int` range.
#[test]
fn works_from_json_int_exact() {
    works(
        r#"pure (case (eitherDecode "42" :: Either Text Int) of { Right i -> i; Left _ -> -999 })"#,
        serde_json::json!(42),
    );
}

/// `FromJSON Int` REJECTS a fractional `Scientific` rather than truncating it,
/// mirroring aeson's bounded-integral parse (aeson `FromJSON` source,
/// `parseBoundedIntegralFromScientific` —
/// https://hackage.haskell.org/package/aeson/docs/src/Data.Aeson.Types.FromJSON.html).
#[test]
fn works_from_json_int_rejects_fraction() {
    works(
        r#"pure (either (const True) (const False) (eitherDecode "-3.7" :: Either Text Int))"#,
        serde_json::json!(true),
    );
    works(
        r#"pure (either (const True) (const False) (eitherDecode "3.7" :: Either Text Int))"#,
        serde_json::json!(true),
    );
}

/// `FromJSON Int` REJECTS an exact integer outside `Int` range rather than
/// silently wrapping (same `toBoundedInteger` grounding as above).
#[test]
fn works_from_json_int_rejects_out_of_range() {
    works(
        r#"pure (either (const True) (const False) (eitherDecode "99999999999999999999999999" :: Either Text Int))"#,
        serde_json::json!(true),
    );
}

/// `_Int` truncates toward zero, matching upstream `Data.Aeson.Lens._Int`'s
/// integral conversion (NOT floor) —
/// https://hackage.haskell.org/package/lens-aeson/docs/Data-Aeson-Lens.html
#[test]
fn works_lens_int_truncates_toward_zero() {
    works(
        r#"pure (fromMaybe (-999) ((decode "-3.7" :: Maybe Value) >>= (^? _Int)))"#,
        serde_json::json!(-3),
    );
    works(
        r#"pure (fromMaybe (-999) ((decode "10.5" :: Maybe Value) >>= (^? _Int)))"#,
        serde_json::json!(10),
    );
}

/// `_Int` stays `Nothing` — never a silent wraparound — for an exact integer
/// outside `Int` range.
#[test]
fn works_lens_int_out_of_range_is_nothing() {
    works(
        r#"pure (isJust ((decode "99999999999999999999999999" :: Maybe Value) >>= (^? _Int)))"#,
        serde_json::json!(false),
    );
}

/// `_Integer` truncates toward zero for a fractional number (same upstream
/// lens-aeson grounding as `_Int`); unlike `_Int`, `Integer` is unbounded, so
/// an exact integer far beyond `Int` range still decodes —
/// https://hackage.haskell.org/package/lens-aeson/docs/Data-Aeson-Lens.html
#[test]
fn works_lens_integer_truncates_and_is_unbounded() {
    works(
        r#"pure (fromMaybe (-999) ((decode "-3.7" :: Maybe Value) >>= (^? _Integer)))"#,
        serde_json::json!(-3),
    );
    works(
        r#"pure (fromMaybe "MISSING" (show <$> ((decode "123456789012345678901234567890" :: Maybe Value) >>= (^? _Integer))))"#,
        serde_json::json!("123456789012345678901234567890"),
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

/// `digitToInt` decodes a hex digit exactly, matching `Data.Char.digitToInt`.
#[test]
fn works_digit_to_int_hex() {
    works(
        r#"pure (object ["a" .= digitToInt 'a', "nine" .= digitToInt '9', "big" .= digitToInt 'F'])"#,
        serde_json::json!({"a": 10, "nine": 9, "big": 15}),
    );
}

/// `digitToInt` THROWS on a non-hex-digit character, mirroring
/// `Data.Char.digitToInt` —
/// https://hackage.haskell.org/package/base/docs/Data-Char.html#v:digitToInt
#[test]
fn digit_to_int_non_hex_fails_loudly() {
    fails_loudly(r#"pure (digitToInt 'z')"#, "not a digit");
}

/// `digitToIntMay` is the total form of `digitToInt`: `Nothing` for a
/// non-hex-digit character instead of throwing.
#[test]
fn works_digit_to_int_may() {
    works(
        r#"pure (object ["hit" .= digitToIntMay 'a', "miss" .= digitToIntMay 'z'])"#,
        serde_json::json!({"hit": 10, "miss": null}),
    );
}

/// `Tidepool.Data.Text`'s polymorphic `Pack` dialect, pinned against the
/// standing criterion for every dialect choice in this repo: some
/// canonical-INVALID case may now work (the win the dialect exists for), but
/// NO canonical-VALID case may change or fail. `T.pack (s :: String)` is
/// VALID-CANONICAL — `Data.Text.pack :: String -> Text` accepts exactly this;
/// it must keep meaning "pack the String".
#[test]
fn works_pack_valid_canonical_string() {
    works(
        r#"pure (T.pack ("abc" :: String))"#,
        serde_json::json!("abc"),
    );
}

/// VALID-CANONICAL: `T.pack (t :: Text)` — not canonical `Data.Text.pack`
/// (which only accepts `String`), but canonical under `Pack`'s own contract
/// (identity on `Text`); pinned since the dialect's exported behavior for a
/// `Text` argument must stay identity.
#[test]
fn works_pack_identity_on_text() {
    works(
        r#"pure (T.pack ("already text" :: Text))"#,
        serde_json::json!("already text"),
    );
}

/// AT-RISK VALID-CANONICAL: `T.pack "lit"` — a bare string literal is the
/// case `Tidepool.Data.Text`'s own module comment flags as now ambiguous
/// between the `Pack String` and `Pack Text` instances. `ExtendedDefaultRules`
/// plus the preamble's `default (Int, Double, Text)` must resolve it (to
/// `Text`, an identity pack) so it still means the obvious `Text "lit"` —
/// if this probe fails to compile, the dialect criterion is violated.
#[test]
fn works_pack_string_literal_defaults() {
    works(r#"pure (T.pack "lit")"#, serde_json::json!("lit"));
}

/// INVALID-CANONICAL, NOW WORKS: `T.pack (show x)` — canonically a type
/// error (`Data.Text.pack :: String -> Text` cannot accept `show x :: Text`,
/// since this stdlib's `show` returns `Text`, not `String`). This is the win
/// the `Pack` dialect exists for: `T.pack (show x)` is identity instead of a
/// trap. (`works_fork` already exercises this shape in production; this pins
/// it directly.)
#[test]
fn works_pack_show_output_dialect_win() {
    works(
        r#"pure (T.pack (show (42 :: Int)))"#,
        serde_json::json!("42"),
    );
}

// =========================================================================
// Quasiquoter strictness — `[j|…|]` exact-number and control-character
// handling, `[fmt|…|]` brace-escape discipline. Rejection paths are
// compile-time failures, pinned with `fails_loudly_with_imports` (defined
// near `fails_loudly`, top of file — shared plumbing, not local to this
// section).
//
// `[j|…|]`/`[fmt|…|]` are NOT auto-imported on this raw `template_haskell`
// path (that injection is the live MCP server request handler's job, which
// this harness bypasses) — every probe below needs an explicit
// `Tidepool.QQ (fmt, j)` import, same as `works_form_qq` and
// `render/fmt_spec_reject.rs`/`render/fmt_nonfinite.rs` elsewhere in this
// suite.
// =========================================================================

const QQ_IMPORTS: &str = "Tidepool.QQ (fmt, j)";

/// `[j|…|]` integer literals beyond `Double`'s 53-bit mantissa parse EXACT:
/// the literal-syntax number path is integer digit accumulation only, never
/// `read :: Double`. Before the fix, `read "123456789012345678" :: Double`
/// rounds to the nearest representable double and loses the low digits.
#[test]
fn qq_json_exact_large_integer_literal() {
    works_with_imports(
        QQ_IMPORTS,
        "pure [j|123456789012345678|]",
        serde_json::json!(123456789012345678_i64),
    );
}

/// A fractional literal with more significant digits than a `Double` mantissa
/// can carry (19 digits) round-trips EXACTLY through `renderJson` — the
/// coefficient/exponent are assembled arithmetically from the literal's
/// digits, not recovered from a lossy `Double` parse.
#[test]
fn qq_json_exact_fraction_literal_beyond_double_precision() {
    works_with_imports(
        QQ_IMPORTS,
        "pure (renderJson [j|1.234567890123456789|])",
        serde_json::json!("1.234567890123456789"),
    );
}

/// The exact-number literal path also drives `[j|…|]` PATTERN matching
/// (`buildMatch`'s `NNumber` arm): a beyond-`Double`-precision integer
/// matches itself exactly.
#[test]
fn qq_json_pattern_matches_exact_large_integer() {
    works_with_imports(
        QQ_IMPORTS,
        "pure (case [j|123456789012345678|] of { [j|123456789012345678|] -> True; _ -> False })",
        serde_json::json!(true),
    );
}

/// A raw (unescaped) control character inside a `[j|…|]` string literal is a
/// compile-time error naming the offending code point — the JSON grammar the
/// quoter advertises never allowed a literal control byte inside a string.
#[test]
fn qq_json_string_rejects_unescaped_control_char() {
    fails_loudly_with_imports(QQ_IMPORTS, "pure [j|\"a\u{1}b\"|]", "control character");
}

/// A JSON escape sequence for the SAME code point still works — only the
/// raw, unescaped byte is rejected.
#[test]
fn qq_json_string_allows_escaped_control_char() {
    works_with_imports(
        QQ_IMPORTS,
        "pure [j|\"a\\u0001b\"|]",
        serde_json::json!("a\u{1}b"),
    );
}

/// A bare, unmatched `}` outside a hole is a compile-time error in
/// `[fmt|…|]` — matching the Python f-string grammar the module advertises
/// (a lone `}` is not allowed; `}}` is the literal-`}` escape).
#[test]
fn qq_fmt_rejects_bare_unmatched_brace() {
    fails_loudly_with_imports(QQ_IMPORTS, "pure [fmt|value } here|]", "unmatched '}'");
}

/// MUST-NOT-BREAK: a `}` INSIDE a hole's expression, inside a string literal
/// (`T.pack "a}b"`), is legal and must stay legal — only a `}` OUTSIDE a hole
/// is rejected. `scanHole`/`scanLiteral` (bracket-depth + literal-aware) reach
/// the real closing `}` without the new bare-`}` rule ever seeing the one
/// inside the string, because it never leaves `scanLiteral`'s string-body scan.
#[test]
fn qq_fmt_brace_inside_hole_string_literal_still_works() {
    works_with_imports(
        QQ_IMPORTS,
        r#"pure [fmt|{T.pack "a}b"}|]"#,
        serde_json::json!("a}b"),
    );
}

/// MUST-NOT-BREAK companion: a `}` INSIDE a hole's expression that is NOT in
/// a string — an explicit-brace `let { … }` block (the same construct the
/// module haddock cites for bracket-depth tracking) — also stays legal. The
/// hole's own closing `}` is only recognized at bracket depth 0, so the
/// nested `{ y = 1 }`'s `}` decrements depth instead of ending the hole.
#[test]
fn qq_fmt_brace_inside_hole_non_string_expr_still_works() {
    works_with_imports(
        QQ_IMPORTS,
        "pure [fmt|{let { y = 1 :: Int } in y}|]",
        serde_json::json!("1"),
    );
}

/// The doubled-brace `}}` literal-`}` escape still works after the bare-`}`
/// rejection lands (regression guard: only the UNDOUBLED case is rejected).
#[test]
fn qq_fmt_doubled_brace_still_literal() {
    works_with_imports(
        QQ_IMPORTS,
        "pure [fmt|literal }} brace|]",
        serde_json::json!("literal } brace"),
    );
}

/// An unclosed `{` in `[fmt|…|]` now names the offset at which the quote body
/// ran out of input — previously this lexer error carried no position.
#[test]
fn qq_fmt_unclosed_brace_carries_offset() {
    fails_loudly_with_imports(QQ_IMPORTS, "pure [fmt|hello {name|]", "at offset");
}

// =========================================================================
// `Tidepool.FilePath` POSIX fidelity — the upstream `filepath` test vectors
// for `normalise` and the sibling path functions.
// =========================================================================

/// `normalise` ported from `System.FilePath.Posix.normalise`
/// (filepath-1.5.2.0, BSD-3-Clause) — trailing-separator and leading-`/`
/// vectors, the ones the HIGH finding was about (the old splitOn-based
/// shadow dropped a meaningful trailing separator and collapsed `"./"`).
/// Vectors are upstream's own doctests, plus the finding's own repro shapes.
/// <https://hackage.haskell.org/package/filepath-1.5.2.0/docs/System-FilePath-Posix.html>
#[test]
fn works_filepath_normalise_trailing_and_leading_separators() {
    works(
        r#"pure (object
            [ "a_slash" .= normalise "a/"
            , "test_many_slash" .= normalise "/test////"
            , "dot_slash" .= normalise "./"
            , "file_test_many_slash" .= normalise "/file/test////"
            , "dotdot_bob_fred_slash" .= normalise "../bob/fred/"
            , "bob_fred_dot" .= normalise "bob/fred/."
            , "dot_bob_fred_slash" .= normalise "./bob/fred/"
            , "empty" .= normalise ""
            , "double_leading_slash_home" .= normalise "//home"
            , "backslash_literal" .= normalise "/file/\\test////"
            ])"#,
        serde_json::json!({
            "a_slash": "a/",
            "test_many_slash": "/test/",
            "dot_slash": "./",
            "file_test_many_slash": "/file/test/",
            "dotdot_bob_fred_slash": "../bob/fred/",
            "bob_fred_dot": "bob/fred/",
            "dot_bob_fred_slash": "bob/fred/",
            "empty": ".",
            "double_leading_slash_home": "/home",
            "backslash_literal": "/file/\\test/",
        }),
    );
}

/// `normalise` — vectors upstream documents as UNCHANGED by normalisation
/// (interior `.` collapsed, `..` left alone, no trailing separator to add).
/// Confirms the port doesn't touch what the old shadow already got right.
/// Same upstream URL as `works_filepath_normalise_trailing_and_leading_separators`.
#[test]
fn works_filepath_normalise_interior_dots_and_clean_paths() {
    works(
        r#"pure (object
            [ "a_dot_b_dotdot_c" .= normalise "a/./b/../c"
            , "test_dot_file" .= normalise "/test/./file"
            , "file_dot_test" .= normalise "/file/./test"
            , "test_file_dotdot_bob_fred_slash" .= normalise "/test/file/../bob/fred/"
            , "a_dotdot_c" .= normalise "/a/../c"
            , "dot" .= normalise "."
            , "dot_dot" .= normalise "./."
            , "slash_dot_slash" .= normalise "/./"
            , "root" .= normalise "/"
            ])"#,
        serde_json::json!({
            "a_dot_b_dotdot_c": "a/b/../c",
            "test_dot_file": "/test/file",
            "file_dot_test": "/file/test",
            "test_file_dotdot_bob_fred_slash": "/test/file/../bob/fred/",
            "a_dotdot_c": "/a/../c",
            "dot": ".",
            "dot_dot": "./",
            "slash_dot_slash": "/",
            "root": "/",
        }),
    );
}

/// Sibling-diff fix: `splitExtension`/`takeExtension`/`takeBaseName`/
/// `hasExtension` no longer special-case a leading `.` (a hidden file like
/// `.bashrc`) as "no extension" — upstream's `System.FilePath.Posix.splitExtension`
/// (filepath-1.5.2.0) finds the extension from the LAST `.` in the whole
/// path with no hidden-file exception, so a name that begins with `.` and
/// has no other `.` splits as an EMPTY base name and an ALL-extension. The
/// old Tidepool shadow silently gave the opposite (canonical-name,
/// non-canonical semantics) answer for every dotfile.
/// <https://hackage.haskell.org/package/filepath-1.5.2.0/docs/System-FilePath-Posix.html>
#[test]
fn works_filepath_extension_dotfile_fidelity() {
    works(
        r#"pure (object
            [ "take_extension_bashrc" .= takeExtension ".bashrc"
            , "take_extension_dot" .= takeExtension "."
            , "split_extension_bashrc" .= (let (b, e) = splitExtension ".bashrc" in object ["base" .= b, "ext" .= e])
            , "take_base_name_bashrc" .= takeBaseName ".bashrc"
            , "has_extension_bashrc" .= hasExtension ".bashrc"
            , "split_extension_crossing_slash" .= (let (b, e) = splitExtension "file.txt/boris" in object ["base" .= b, "ext" .= e])
            , "take_extension_regular" .= takeExtension "file.txt"
            ])"#,
        serde_json::json!({
            "take_extension_bashrc": ".bashrc",
            "take_extension_dot": ".",
            "split_extension_bashrc": {"base": "", "ext": ".bashrc"},
            "take_base_name_bashrc": "",
            "has_extension_bashrc": true,
            "split_extension_crossing_slash": {"base": "file.txt/boris", "ext": ""},
            "take_extension_regular": ".txt",
        }),
    );
}

// =========================================================================
// `Tidepool.Data.Time` and the Prelude/Fmt-runtime shadows: the names the
// stdlib claims are JIT-safe, each pinned by a probe that calls it.
// =========================================================================

/// `formatISO8601` on a known instant and a PRE-EPOCH instant, plus both
/// directions of `parseISO8601` and the full parse-then-format round trip.
///
/// `UTCTime 1700000000000` is the well-known Unix instant `1700000000`s ->
/// 2023-11-14T22:13:20Z. `UTCTime (-1000)` is 1000ms BEFORE the epoch —
/// exactly one second earlier, which floors into the END of the preceding
/// day: 1969-12-31T23:59:59Z, not a negative time-of-day. `parseISO8601
/// "2026-07-01T19:24:22-07:00"` normalizes the `-07:00` offset to UTC (add
/// 7h): 19:24:22 on the 1st becomes 02:24:22 on the 2nd — the same fixture
/// the module's own haddock uses — pinned as epoch-ms, and round-tripped
/// back through `formatISO8601` to the reformatted UTC string.
#[test]
fn works_time_formatting_pinned() {
    works(
        "pure (object [\"known\" .= formatISO8601 (UTCTime 1700000000000), \
         \"pre_epoch\" .= formatISO8601 (UTCTime (-1000)), \
         \"roundtrip_tz\" .= (case parseISO8601 \"2026-07-01T19:24:22-07:00\" of { Right t -> formatISO8601 t; Left e -> e }), \
         \"parse_tz_ms\" .= (case parseISO8601 \"2026-07-01T19:24:22-07:00\" of { Right t -> epochMillis t; Left _ -> (-1) }), \
         \"parse_epoch\" .= (case parseISO8601 \"1970-01-01T00:00:00Z\" of { Right t -> epochMillis t; Left _ -> (-1) })])",
        serde_json::json!({
            "known": "2023-11-14T22:13:20Z",
            "pre_epoch": "1969-12-31T23:59:59Z",
            "roundtrip_tz": "2026-07-02T02:24:22Z",
            "parse_tz_ms": 1782959062000_i64,
            "parse_epoch": 0
        }),
    );
}

/// `daysFromCivil`/`diffUTCTime`/`addUTCTime`/`epochMillis` — the Int-only
/// civil-date arithmetic the module header claims is fully JIT-safe.
///
/// `daysFromCivil 2024 2 29` = 19782, the same epoch-day `toGregorian`'s
/// fixture (`UTCTime 1709164800000`) decomposes to (`19782 * 86400 * 1000 ==
/// 1709164800000`) — `daysFromCivil` is `civilFromDays`'s pinned inverse.
/// `daysFromCivil 1970 1 1` = 0 (the epoch). `daysFromCivil 1969 12 31` =
/// -1: one day before the epoch is epoch-day -1, not an off-by-one wrap.
///
/// `diffUTCTime (UTCTime 1000) (UTCTime (-500))` crosses the epoch boundary
/// (one operand pre-epoch, one post-epoch): (1000 - (-500)) / 1000 = 1.5s.
/// `diffUTCTime (UTCTime 0) (UTCTime 5000)` is a negative delta: (0 - 5000)
/// / 1000 = -5s (an integral Double renders as a bare JSON integer, see
/// `works_moderate_double_literals`).
///
/// `addUTCTime (-1.5) (UTCTime 1000)` both crosses the epoch boundary and
/// applies a negative delta: 1000 + round(-1.5 * 1000) = 1000 - 1500 = -500.
/// `addUTCTime 0.0625 (UTCTime 0)` and `addUTCTime 0.1875 (UTCTime 0)` pin
/// the millisecond-rounding boundary at an EXACT tie: 1/16 and 3/16 are
/// exactly representable in binary64, and so are their *1000 products (62.5
/// and 187.5) — no floating-point rounding noise before `round` ever sees
/// them, unlike a decimal literal such as 0.0005 whose stored double isn't
/// provably exactly 0.0005. `round` is banker's rounding (ties to even, see
/// `works_round_bankers`): 62.5 ties DOWN to 62 (even), 187.5 ties UP to 188
/// (even) — not simple round-half-up.
///
/// `epochMillis (UTCTime (-500))` is the plain pre-epoch accessor: -500.
#[test]
fn works_time_arithmetic_pinned() {
    works(
        "pure (object [\"days_modern\" .= daysFromCivil 2024 2 29, \
         \"days_epoch\" .= daysFromCivil 1970 1 1, \
         \"days_pre_epoch\" .= daysFromCivil 1969 12 31, \
         \"diff_cross_epoch\" .= diffUTCTime (UTCTime 1000) (UTCTime (-500)), \
         \"diff_negative\" .= diffUTCTime (UTCTime 0) (UTCTime 5000), \
         \"add_cross_epoch_neg\" .= epochMillis (addUTCTime (-1.5) (UTCTime 1000)), \
         \"add_round_tie_down\" .= epochMillis (addUTCTime 0.0625 (UTCTime 0)), \
         \"add_round_tie_up\" .= epochMillis (addUTCTime 0.1875 (UTCTime 0)), \
         \"epoch_millis_pre_epoch\" .= epochMillis (UTCTime (-500))])",
        serde_json::json!({
            "days_modern": 19782,
            "days_epoch": 0,
            "days_pre_epoch": -1,
            "diff_cross_epoch": 1.5,
            "diff_negative": -5,
            "add_cross_epoch_neg": -500,
            "add_round_tie_down": 62,
            "add_round_tie_up": 188,
            "epoch_millis_pre_epoch": -500
        }),
    );
}

/// `replace`/`isSuffixOf`/`isInfixOf`/`takeWhileT`/`dropWhileT` — the
/// `Tidepool.Prelude` Text shadows, called through the unqualified surface
/// exactly as an eval user writes them.
///
/// `takeWhileT`/`dropWhileT` are pinned with an OPERATOR SECTION predicate
/// (`(/= ',')`, `(< 'c')`), the shape a retired String-detour workaround
/// existed for (a cross-module operator-section predicate reaching an
/// external `Data.Text` unfolding once corrupted; `T` now points at the
/// vendored home-module `Tidepool.Data.Text`) — plus partial application
/// (`takeWhileT (/= ',')` passed to `map`) and use inside `filter`/`map`
/// together, and empty-input/no-match cases for every one of the five.
#[test]
fn works_prelude_text_shadows_pinned() {
    works(
        "pure (object [\"replace_basic\" .= replace \"a\" \"o\" \"banana\", \
         \"replace_no_match\" .= replace \"z\" \"o\" \"banana\", \
         \"replace_empty_haystack\" .= replace \"a\" \"o\" \"\", \
         \"is_suffix_true\" .= isSuffixOf \"ana\" \"banana\", \
         \"is_suffix_false\" .= isSuffixOf \"xyz\" \"banana\", \
         \"is_suffix_empty\" .= isSuffixOf \"\" \"banana\", \
         \"is_infix_true\" .= isInfixOf \"nan\" \"banana\", \
         \"is_infix_false\" .= isInfixOf \"xyz\" \"banana\", \
         \"is_infix_empty\" .= isInfixOf \"\" \"banana\", \
         \"take_while_section\" .= takeWhileT (/= ',') \"a,b,c\", \
         \"take_while_no_match\" .= takeWhileT (== 'z') \"abc\", \
         \"take_while_empty\" .= takeWhileT (/= ',') \"\", \
         \"drop_while_section\" .= dropWhileT (< 'c') \"abcdef\", \
         \"drop_while_no_match\" .= dropWhileT (== 'z') \"abc\", \
         \"drop_while_empty\" .= dropWhileT (/= ',') \"\", \
         \"take_while_partial_map\" .= map (takeWhileT (/= ',')) [\"a,b\", \"c,d\", \"nocomma\"], \
         \"drop_while_filter_map\" .= map (dropWhileT (< 'c')) (filter (/= \"\") [\"abcdef\", \"\", \"cba\"])])",
        serde_json::json!({
            "replace_basic": "bonono",
            "replace_no_match": "banana",
            "replace_empty_haystack": "",
            "is_suffix_true": true,
            "is_suffix_false": false,
            "is_suffix_empty": true,
            "is_infix_true": true,
            "is_infix_false": false,
            "is_infix_empty": true,
            "take_while_section": "a",
            "take_while_no_match": "",
            "take_while_empty": "",
            "drop_while_section": "cdef",
            "drop_while_no_match": "abc",
            "drop_while_empty": "",
            "take_while_partial_map": ["a", "c", "nocomma"],
            "drop_while_filter_map": ["cdef", "cba"]
        }),
    );
}

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

/// The `0`-flag's SIGN-AWARE zero-padding, pinned end to end through the
/// real `[fmt|...|]` quasiquoter — not the runtime helpers called directly
/// (contrast `works_fmt_runtime_helpers_pinned`, which calls `fmtInt` with
/// an EXPLICIT `FRight`, a different and also-correct path).
///
/// Python's format-spec grammar: preceding the width field by a `0` enables
/// sign-aware zero-padding for numeric types — equivalent to a fill
/// character of `0` with an alignment type of `=`. `Tidepool.QQ.PyF.Spec`'s
/// `overrideAlignmentIfZero` implements exactly this rule (a bare `0` flag
/// with no explicit alignment maps to fill `'0'` plus `AlignInside`), and
/// `Tidepool.QQ.Fmt.Runtime`'s `fpad FInside = pre ++ pad need ++ body`
/// places the sign BEFORE the padding, between it and the digits.
///
/// The only existing zero-pad coverage (`haskell/test/Suite.hs`'s
/// `qq_fmt_spec_zero_pad`, `[fmt|{n:04d}|]` on `n = 42`) runs on a POSITIVE
/// number, which has no sign to place — `AlignInside` and plain `AlignRight`
/// produce IDENTICAL output for a positive value, so that probe cannot tell
/// the two apart. If the zero-flag override, `extractPad`, `alignE`, or
/// `fpad`'s `FInside` case silently fell back to `AlignRight`, nothing
/// already pinned would catch it. `[fmt|{n:06d}|]` on a NEGATIVE `n` is the
/// shape that can: Python's `f"{-42:06d}"` is `"-00042"`, never `"000-42"`.
///
/// `[fmt|{d:08.2f}|]` on a negative `Double` pins the same wiring for the
/// fractional presentation type: `-3.14159` rounds (non-tie) to `3.14` at 2
/// decimal places, and sign-aware zero-padding to width 8 gives `"-0003.14"`
/// — sign, three zero-fill digits, then the 4-character body `3.14`.
#[test]
fn works_fmt_qq_sign_aware_zero_pad() {
    works_with_imports(
        "Tidepool.QQ.Fmt (fmt)",
        "pure (object [\"int_neg\" .= [fmt|{n:06d}|], \"frac_neg\" .= [fmt|{d:08.2f}|]]) \
         where { n = (-42) :: Int; d = (-3.14159) :: Double }",
        serde_json::json!({
            "int_neg": "-00042",
            "frac_neg": "-0003.14"
        }),
    );
}
