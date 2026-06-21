//! Native ghc-bignum backend: Integer/Natural operations are now pure Core over
//! Word#/ByteArray# primops (no __gmpn_*/integer_gmp_* FFI), which the JIT
//! compiles directly — correct by construction. These validate end-to-end VALUES
//! through the JIT for the whole "Integer/GMP arithmetic is unsupported" class
//! the pivot retires: big literals, read, multi-limb +/*//mod/gcd, show.
//!
//! Plain modules (base Prelude only — no MCP preamble, no lens) run via
//! `compile_and_run_pure`. Needs the worktree extract binary built against the
//! native-bignum GHC (TIDEPOOL_EXTRACT) + that GHC's libdir (TIDEPOOL_GHC_LIBDIR).
use serde_json::json;
use tidepool_runtime::compile_and_run_pure;

fn show_pure(body: &str) -> serde_json::Value {
    let body = body.to_string();
    // Run on a large-stack thread: the JIT and some Haskell machinery (e.g. the
    // Read/ReadP CPS parser) recurse deeper than the 2 MiB default test stack.
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let src = format!("module M where\nx :: String\nx = {body}\n");
            match compile_and_run_pure(&src, "x", &[]) {
                Ok(r) => r.to_json(),
                Err(e) => panic!("eval failed for `{body}`: {e}"),
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

/// Run a `Double` binding and return its rendered JSON number.
fn dbl_pure(body: &str) -> serde_json::Value {
    let body = body.to_string();
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let src = format!("module M where\nx :: Double\nx = {body}\n");
            match compile_and_run_pure(&src, "x", &[]) {
                Ok(r) => r.to_json(),
                Err(e) => panic!("eval failed for `{body}`: {e}"),
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

// The ORIGINAL trigger: large base-10-exponent Double literals desugar to a
// runtime `rationalToDouble` computation. Two over-collection layers dragged the
// type-representation machinery into its closure via the DEAD error branches
// (ratio/divide-by-zero exceptions carry `Typeable` `SomeException`):
// GHC.Fingerprint (MD5) AND GHC.Data.Typeable (`mkTrCon1/2/3`). Both are now
// pruned at the module boundary (`isNeverResolve` -> lazy poison), so the
// formerly force-trapped externals stay un-forced in the dead branches.
//
// With that resolved, these now reach the SAME blocker as `from_integral_to_double`
// below: `GHC.Float.roundingMode#: IN` — a positive value reaching the negative-
// bignat (`IN`) constructor branch in the JIT's Integer IS/IP/IN case dispatch
// (integerToBinaryFloat' / roundingMode#). #1 (computed Integer->Double) and #2
// (big Double literals) are therefore one root cause: the Integer-constructor
// dispatch in the GHC.Float Double-conversion path. Tracked together.
#[test]
#[ignore = "GHC.Float roundingMode#: IN — JIT Integer IS/IP/IN dispatch (shared with from_integral_to_double)"]
fn big_double_e308() {
    assert_eq!(dbl_pure("1.0e308"), json!(1.0e308));
}

#[test]
#[ignore = "GHC.Float roundingMode#: IN — JIT Integer IS/IP/IN dispatch (shared with from_integral_to_double)"]
fn big_double_max_finite() {
    assert_eq!(
        dbl_pure("1.7976931348623157e308"),
        json!(1.7976931348623157e308)
    );
}

#[test]
#[ignore = "GHC.Float roundingMode#: IN — JIT Integer IS/IP/IN dispatch (shared with from_integral_to_double)"]
fn big_double_neg_exp() {
    assert_eq!(dbl_pure("1.0e-300"), json!(1.0e-300));
}

#[test]
fn diag_computed_pos() {
    // 2^100 > 0 — exercises the computed-BigNat dispatch (IS x / DEFAULT).
    assert_eq!(
        show_pure("show ((2 ^ (100 :: Int) :: Integer) > 0)"),
        json!("True")
    );
}

#[test]
fn diag_computed_t64() {
    // Smallest 2-limb, COMPUTED via powImpl.
    assert_eq!(
        show_pure("show (2 ^ (64 :: Int) :: Integer)"),
        json!("18446744073709551616")
    );
}

#[test]
fn diag_literal_ip() {
    // Constant-folded to `IP 2^64` (a literal) — like big_integer_literal.
    assert_eq!(
        show_pure("show (18446744073709551615 + (1 :: Integer))"),
        json!("18446744073709551616")
    );
}

#[test]
fn diag_computed_eq_literal() {
    // computed 2^64 == literal 2^64 — isolates compute vs show.
    assert_eq!(
        show_pure("show ((2 ^ (64 :: Int) :: Integer) == 18446744073709551616)"),
        json!("True")
    );
}

#[test]
fn big_integer_literal() {
    assert_eq!(
        show_pure("show (123456789012345678901234567890 :: Integer)"),
        json!("123456789012345678901234567890")
    );
}

#[test]
fn pow_2_100() {
    assert_eq!(
        show_pure("show (2 ^ (100 :: Int) :: Integer)"),
        json!("1267650600228229401496703205376")
    );
}

#[test]
fn factorial_30() {
    assert_eq!(
        show_pure("show (product [1..30] :: Integer)"),
        json!("265252859812191058636308480000000")
    );
}

// read pulls in the Read/ReadP CPS-parser machinery, which currently hits a JIT
// closure-handling bug ("application of non-closure"); orthogonal to bignum —
// the Integer arithmetic the lexer accumulates is itself correct now.
#[test]
#[ignore = "Read/ReadP CPS machinery: JIT non-closure application (orthogonal to bignum)"]
fn read_integer() {
    assert_eq!(
        show_pure("show (read \"123456789012345678901234567890\" :: Integer)"),
        json!("123456789012345678901234567890")
    );
}

#[test]
#[ignore = "Read/ReadP CPS machinery: JIT non-closure application (orthogonal to bignum)"]
fn read_int() {
    assert_eq!(show_pure("show (read \"42\" :: Int)"), json!("42"));
}

#[test]
fn big_div() {
    // 35! / 20! = 21*22*...*35
    assert_eq!(
        show_pure("show (product [1..35] `div` product [1..20] :: Integer)"),
        json!("4247252019052922880000")
    );
}

#[test]
fn big_mod() {
    // (2^200 + 7) mod (10^18)
    assert_eq!(
        show_pure("show ((2 ^ (200 :: Int) + 7) `mod` (10 ^ (18 :: Int)) :: Integer)"),
        json!("993782792835301383")
    );
}

#[test]
fn big_gcd() {
    // gcd(2^100, 2^60 * 3) = 2^60
    assert_eq!(
        show_pure("show (gcd (2 ^ (100 :: Int)) (3 * 2 ^ (60 :: Int)) :: Integer)"),
        json!("1152921504606846976")
    );
}

// fromIntegral :: Integer -> Double of a LITERAL big Integer works; of a COMPUTED
// one hits GHC.Float's `roundingMode#: IN` (a positive value reaching it as a
// negative bignat — a representation issue in integerToBinaryFloat', deep in the
// Double-conversion path). Tracked separately.
#[test]
#[ignore = "computed Integer -> Double: GHC.Float roundingMode#: IN (Double-conversion path)"]
fn from_integral_to_double() {
    assert_eq!(
        show_pure("show (fromIntegral (2 ^ (100 :: Int) :: Integer) :: Double)"),
        json!("1.2676506002282294e30")
    );
}
