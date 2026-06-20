//! Phase-1 multi-limb Integer (ghc-bignum gmp-backend) support: the JIT now
//! satisfies ghc-bignum's `__gmpn_*` / `integer_gmp_*` FFI surface via Rust
//! (`tidepool-bignum`), plus the `LitNumBigNat` literal path. This retires the
//! whole "Integer/GMP arithmetic is unsupported" class: `read :: Integer`,
//! Integer-defaulted big arithmetic, big literals, show, div/mod/gcd.
//!
//! Pre-fix these died at COMPILE time with "Unsupported FFI call:
//! ghc-bignum:__gmpn_*". Run against the worktree extract binary
//! (TIDEPOOL_EXTRACT + TIDEPOOL_GHC_LIBDIR), which carries the fix.
use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

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

/// Compile + run `pure (<hole> :: Text)` with the given top-level `helpers`,
/// returning the JSON result.
fn eval_text(helpers: &str, hole: &str) -> serde_json::Value {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code = format!("pure (({hole}) :: Text)");
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do(&code),
        "",
        helpers,
        None,
        None,
    );
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .leak() as &Path;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib").leak() as &Path;
    let lib = root.join(".tidepool/lib").leak() as &Path;
    let include = [hs, lib, effects_dir];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => v.to_json(),
        Err(e) => panic!("eval failed for `{hole}`: {e}"),
    }
}

const HELPERS: &str = "\
bigReadI :: Integer\n\
bigReadI = P.read \"123456789012345678901234567890\"\n\
readSmallI :: Int\n\
readSmallI = P.read \"42\"\n\
factBig :: Integer\n\
factBig = product [1..30]\n\
pow100 :: Integer\n\
pow100 = 2 P.^ (100 :: Int)\n\
bigLit :: Integer\n\
bigLit = 123456789012345678901234567890\n\
a35 :: Integer\n\
a35 = product [1..35]\n\
b20 :: Integer\n\
b20 = product [1..20]\n\
divIdentity :: Bool\n\
divIdentity = (a35 `P.div` b20) P.* b20 P.+ (a35 `P.mod` b20) == a35\n\
gcdDivides :: Bool\n\
gcdDivides = let g = P.gcd a35 b20 in a35 `P.mod` g == 0 && b20 `P.mod` g == 0\n\
toDblOk :: Bool\n\
toDblOk = (fromIntegral (2 P.^ (60 :: Int) :: Integer) :: Double) == 1152921504606846976.0\n";

#[test]
fn read_integer_roundtrips() {
    // `read :: Integer` used to die at compile time (GMP Read lexer).
    assert_eq!(
        eval_text(HELPERS, "show bigReadI"),
        serde_json::json!("123456789012345678901234567890")
    );
}

#[test]
fn read_int_works() {
    assert_eq!(eval_text(HELPERS, "show readSmallI"), serde_json::json!("42"));
}

#[test]
fn factorial_30_exact() {
    // 30! overflows a single limb; exercises multi-limb integerMul.
    assert_eq!(
        eval_text(HELPERS, "show factBig"),
        serde_json::json!("265252859812191058636308480000000")
    );
}

#[test]
fn pow_2_100_exact() {
    assert_eq!(
        eval_text(HELPERS, "show pow100"),
        serde_json::json!("1267650600228229401496703205376")
    );
}

#[test]
fn big_integer_literal() {
    // LitNumBigNat literal materialization.
    assert_eq!(
        eval_text(HELPERS, "show bigLit"),
        serde_json::json!("123456789012345678901234567890")
    );
}

#[test]
fn big_div_mod_identity() {
    // q*d + r == n over multi-limb operands (tdiv_qr / mpn division).
    assert_eq!(
        eval_text(HELPERS, "show divIdentity"),
        serde_json::json!("True")
    );
}

#[test]
fn big_gcd_divides_both() {
    assert_eq!(
        eval_text(HELPERS, "show gcdDivides"),
        serde_json::json!("True")
    );
}

#[test]
fn from_integral_to_double() {
    // fromIntegral :: Integer -> Double via integer_gmp_mpn_get_d. 2^60 is exact.
    assert_eq!(
        eval_text(HELPERS, "show toDblOk"),
        serde_json::json!("True")
    );
}
