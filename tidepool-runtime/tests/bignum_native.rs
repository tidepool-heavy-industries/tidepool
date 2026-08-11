//! Native ghc-bignum backend: Integer/Natural operations are now pure Core over
//! Word#/ByteArray# primops (no __gmpn_*/integer_gmp_* FFI), which the JIT
//! compiles directly — correct by construction. These validate end-to-end VALUES
//! through the JIT for the whole "Integer/GMP arithmetic is unsupported" class
//! the pivot retires: big literals, read, multi-limb +/*//mod/gcd, show.
//!
//! Plain modules (base Prelude only — no MCP preamble, no lens) run via
//! `compile_and_run_pure`. Needs the worktree extract binary built against the
//! native-bignum GHC (TIDEPOOL_EXTRACT) + that GHC's libdir (TIDEPOOL_GHC_LIBDIR).
//!
//! 15 of the 16 probes are VALUE-class (a regression here is a wrong-but-
//! obtained show/toJSON string, never a crash — this file's own harness has
//! no dedicated-thread/signal-safety/timeout scaffolding at all, unlike
//! `jit_surface.rs`'s and `stdlib_regressions_02*.rs`'s, which is itself
//! evidence none of these were ever expected to corrupt the process) and
//! bundle into three `#[test]` fns via the check-list idiom
//! (`generic_form_roundtrip.rs`/`jit_surface.rs`). `read_integer` stays
//! standalone: unlike `read_int`'s undocumented, trivial-input read, its own
//! comment names a HANG-class root cause ("lazy-let thunkification... no
//! longer force-recurses" — corecursion that used to force-eval eagerly
//! instead of staying lazy), so it keeps its own probe rather than risking
//! taking sibling checks down with it if that regresses.
use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

fn show_pure(body: &str) -> serde_json::Value {
    let src = format!("module M where\nx :: String\nx = {body}\n");
    EvalHarness::new()
        .run_pure(&src, "x")
        .expect(&format!("eval failed for `{body}`"))
        .to_json()
}

/// Run a check-list module (base Prelude only, no `Text`/`Tidepool.Prelude`)
/// and return the rendered `[String]` of failed check names.
fn checks_pure(body: &str) -> serde_json::Value {
    let src = format!(
        "module M where\n\
         check :: String -> Bool -> [String]\n\
         check nm ok = if ok then [] else [nm]\n\
         result :: [String]\n\
         result = {body}\n"
    );
    EvalHarness::new()
        .run_pure(&src, "result")
        .expect(&format!("eval failed for `{body}`"))
        .to_json()
}

// The ORIGINAL trigger: large base-10-exponent Double literals desugar to a
// runtime `rationalToDouble` computation. FIXED across two related root causes:
// (a) the roundingMode#:IN eager-CAF-eval (#1, see `from_integral_to_double`), and
// (b) this path's eager-eval of a `raise# exc` LetRec binding (rationalToDouble's
// overflow/ratio-error throw) — the error-deferral check did not recognise a
// `PrimOp Raise` RHS, so the strict spine threw it regardless of control flow.
// Fix: the error-call walkers also treat `raise#` as a bottoming, deferred RHS.
#[test]
fn double_literal_and_fromintegral_family() {
    let v = checks_pure(
        r#"concat
    [ check "big_double_e308" ((1.0e308 :: Double) == 1.0e308)
    , check "big_double_max_finite" ((1.7976931348623157e308 :: Double) == 1.7976931348623157e308)
    , check "big_double_neg_exp" ((1.0e-300 :: Double) == 1.0e-300)
      -- fromIntegral :: Integer -> Double, COMPUTED big Integer. This was the
      -- canonical #1 case (roundingMode#: IN). FIXED: the bug was eager-eval
      -- of GHC's bottoming `case error "roundingMode#: IN" of {}` CAF — the
      -- JIT's error-deferral check did not see the error through the forced
      -- case scrutinee, so it evaluated the CAF at LetRec setup and raised
      -- the error regardless of the (correct) case dispatch. Fix: the
      -- error-call walkers follow the case scrutinee (tidepool-codegen emit).
    , check "from_integral_to_double" (show (fromIntegral (2 ^ (100 :: Int) :: Integer) :: Double) == "1.2676506002282294e30")
    ]"#,
    );
    assert_eq!(v, json!([]), "failed checks: {v}");
}

#[test]
fn integer_literal_and_dispatch_family() {
    let v = checks_pure(
        r#"concat
      -- 2^100 > 0 — exercises the computed-BigNat dispatch (IS x / DEFAULT).
    [ check "diag_computed_pos" (show ((2 ^ (100 :: Int) :: Integer) > 0) == "True")
      -- Smallest 2-limb Integer, COMPUTED via powImpl.
    , check "diag_computed_t64" (show (2 ^ (64 :: Int) :: Integer) == "18446744073709551616")
      -- Constant-folded to `IP 2^64` (a literal) — like big_integer_literal.
    , check "diag_literal_ip" (show (18446744073709551615 + (1 :: Integer)) == "18446744073709551616")
      -- computed 2^64 == literal 2^64 — isolates compute vs show.
    , check "diag_computed_eq_literal" (show ((2 ^ (64 :: Int) :: Integer) == 18446744073709551616) == "True")
      -- The original "Integer/GMP arithmetic is unsupported" repro: a big
      -- Integer literal, shown.
    , check "big_integer_literal" (show (123456789012345678901234567890 :: Integer) == "123456789012345678901234567890")
    , check "pow_2_100" (show (2 ^ (100 :: Int) :: Integer) == "1267650600228229401496703205376")
    ]"#,
    );
    assert_eq!(v, json!([]), "failed checks: {v}");
}

#[test]
fn integer_multilimb_ops_and_read_family() {
    let v = checks_pure(
        r#"concat
    [ check "factorial_30" (show (product [1..30] :: Integer) == "265252859812191058636308480000000")
      -- 35! / 20! = 21*22*...*35
    , check "big_div" (show (product [1..35] `div` product [1..20] :: Integer) == "4247252019052922880000")
      -- (2^200 + 7) mod (10^18)
    , check "big_mod" (show ((2 ^ (200 :: Int) + 7) `mod` (10 ^ (18 :: Int)) :: Integer) == "993782792835301383")
      -- gcd(2^100, 2^60 * 3) = 2^60
    , check "big_gcd" (show (gcd (2 ^ (100 :: Int)) (3 * 2 ^ (60 :: Int)) :: Integer) == "1152921504606846976")
      -- read "42" :: Int — the same Read/ReadP CPS-parser machinery as
      -- read_integer (below), at a trivial, non-recursion-stressing input
      -- size; undocumented crash history, so it bundles as VALUE-class.
    , check "read_int" (show (read "42" :: Int) == "42")
    ]"#,
    );
    assert_eq!(v, json!([]), "failed checks: {v}");
}

// read pulls in the Read/ReadP CPS-parser machinery. Both root causes are now
// fixed: the unboxed-1-tuple (`MkSolo#`) build erasure in Translate.hs, and
// lazy-let thunkification in the JIT (ReadP `expect`'s `let x = F k` corecursion
// no longer force-recurses). The Integer arithmetic the lexer accumulates was
// already correct.
//
// HANG/CRASH-class — STANDALONE: "no longer force-recurses" names a
// corecursion mechanism that used to force-eval eagerly instead of staying
// lazy — a regression here risks a hang or stack blowup, not a wrong value,
// which would take out any check sharing its probe.
#[test]
fn read_integer() {
    assert_eq!(
        show_pure("show (read \"123456789012345678901234567890\" :: Integer)"),
        json!("123456789012345678901234567890")
    );
}
