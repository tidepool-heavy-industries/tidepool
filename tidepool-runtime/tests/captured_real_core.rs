//! Captured-real-Core differential: closing the meta-hole that the synthetic
//! JIT-vs-eval proptest net could never reach.
//!
//! The widened synthetic generator (generic N-way dispatch, mixed unboxed/boxed
//! sums, closure-in-field-then-apply) found ZERO divergence — because the bugs
//! live in the SPECIFIC `-O2`-optimized base-library Core that only the real
//! extractor emits (the synthetic `standard_datacon_table` can't even mint the
//! `Integer` `IS`/`IP`/`IN` repr). So we capture that real Core as CBOR fixtures
//! (extracted once via the native-bignum `tidepool-extract-bin --all-closed`,
//! checked in) and run it through the SAME `check_jit_vs_eval` oracle with the
//! REAL `meta.cbor` `DataConTable`.
//!
//! Source (`haskell/.../Repro.hs`, native-bignum GHC):
//! ```haskell
//! {-# NOINLINE nVal #-}
//! nVal :: Integer ; nVal = 1025
//! reproRoundIN :: Double ; reproRoundIN = fromIntegral nVal   -- #1
//! reproReadInt :: Int    ; reproReadInt = read "42"           -- #2
//! ```
//!
//! These fixtures pin two DISTINCT, currently-unfixed bugs. The asserts encode
//! the present (buggy) behaviour; when a fix lands, the relevant assert flips and
//! this test fails loudly — that is the signal to update it.
use tidepool_codegen::jit_machine::JitError;
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::value::Value;
use tidepool_repr::serial::read::{read_cbor, read_metadata};
use tidepool_repr::{CoreExpr, DataConTable, Literal};
use tidepool_testing::proptest::{check_jit_vs_eval_captured, CapturedOutcome};

static META: &[u8] = include_bytes!("captured_core/meta.cbor");
static ROUND_IN: &[u8] = include_bytes!("captured_core/reproRoundIN.cbor");
static READ_INT: &[u8] = include_bytes!("captured_core/reproReadInt.cbor");

const NURSERY: usize = 8 * 1024 * 1024;

fn load(node: &[u8]) -> (CoreExpr, DataConTable) {
    (read_cbor(node).unwrap(), read_metadata(META).unwrap().0)
}

/// Unwrap a boxed `D# d` (a `Con` with one `LitDouble` field) to its `f64`.
fn boxed_double(v: &Value) -> Option<f64> {
    match v {
        Value::Con(_, fields) => match fields.first() {
            Some(Value::Lit(Literal::LitDouble(bits))) => Some(f64::from_bits(*bits)),
            _ => None,
        },
        Value::Lit(Literal::LitDouble(bits)) => Some(f64::from_bits(*bits)),
        _ => None,
    }
}

/// Run on a large stack: the real conversion / ReadP Core recurses deeper than
/// the 2 MiB default test stack.
fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

// #1 — a TRUE JIT-vs-eval differential. `fromIntegral (1025 :: Integer) :: Double`
// is trivially exact (eval returns 1025.0), but the JIT mis-dispatches the inlined
// `roundingMode#` Integer case (IS → IN) and raises `roundingMode#: IN`. This is
// the captured target a fix for the constructor-tag-misread bug must clear.
#[test]
fn captured_round_in_is_jit_only_failure() {
    on_big_stack(|| {
        let (expr, table) = load(ROUND_IN);
        match check_jit_vs_eval_captured(&expr, &table, NURSERY) {
            CapturedOutcome::JitOnlyFailure { eval, jit } => {
                // Eval is correct: exactly 1025.0.
                assert_eq!(
                    boxed_double(&eval),
                    Some(1025.0),
                    "eval must compute the exact value; got {eval:?}"
                );
                // JIT diverges with roundingMode#: IN.
                match jit {
                    JitError::Yield(YieldError::UserErrorMsg(msg)) => assert!(
                        msg.contains("roundingMode#"),
                        "expected the roundingMode# divergence, got: {msg}"
                    ),
                    other => panic!("expected JIT roundingMode#: IN, got {other:?}"),
                }
            }
            other => panic!("expected #1 to be a JitOnlyFailure differential, got {other:?}"),
        }
    });
}

// #2 — NOT a JIT-vs-eval differential. `read "42" :: Int` fails in BOTH engines:
// the tree-walker yields `NotAFunction` and the JIT `BadFunPtrTag` (a constructor
// in function position, reached through ReadP's newtype coercion). Because eval
// also fails, the differential oracle structurally CANNOT catch this — it is a
// translation / shared-repr bug, a different class from #1. Captured (as the
// explicit `BothFail` outcome) so that class is on record rather than swallowed.
#[test]
fn captured_read_int_both_engines_fail() {
    on_big_stack(|| {
        let (expr, table) = load(READ_INT);
        match check_jit_vs_eval_captured(&expr, &table, NURSERY) {
            CapturedOutcome::BothFail { eval, jit } => {
                // Tree-walker also fails — proves this is NOT JIT-only.
                assert!(
                    format!("{eval:?}").contains("NotAFunction"),
                    "expected eval NotAFunction, got {eval:?}"
                );
                assert!(
                    matches!(jit, JitError::Yield(YieldError::BadFunPtrTag(_))),
                    "expected JIT BadFunPtrTag (non-function applied), got {jit:?}"
                );
            }
            other => panic!("expected #2 to be BothFail (not a differential), got {other:?}"),
        }
    });
}
