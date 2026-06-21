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
use tidepool_eval::value::Value;
use tidepool_repr::serial::read::{read_cbor, read_metadata};
use tidepool_repr::{CoreExpr, DataConTable, Literal};
use tidepool_testing::proptest::{check_jit_vs_eval_captured, CapturedOutcome};

static META: &[u8] = include_bytes!("captured_core/meta.cbor");
static ROUND_IN: &[u8] = include_bytes!("captured_core/reproRoundIN.cbor");
static ROUND_IN_MIN: &[u8] = include_bytes!("captured_core/reproRoundINMin.cbor");
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

/// Unwrap a boxed `I# n` (a `Con` with one `LitInt` field) to its `i64`.
fn boxed_int(v: &Value) -> Option<i64> {
    match v {
        Value::Con(_, fields) => match fields.first() {
            Some(Value::Lit(Literal::LitInt(n))) => Some(*n),
            _ => None,
        },
        Value::Lit(Literal::LitInt(n)) => Some(*n),
        _ => None,
    }
}

/// The golden value `read "42" :: Int` must produce once #2 is fixed.
const READ_INT_EXPECTED: i64 = 42;

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

// #1 — FIXED. `fromIntegral (1025 :: Integer) :: Double` now AGREES at 1025.0 in
// both engines. The bug was NOT a constructor-tag misread (the dispatch reads IS
// correctly): GHC lowers roundingMode#'s `IN -> error` to a bottoming unlifted
// CAF shaped `case error "roundingMode#: IN" of {}`, and the JIT's error-deferral
// check only saw `error ...` directly — not through the forced case scrutinee — so
// it evaluated the CAF eagerly and raised the error regardless of which branch the
// case took. Fix: the error-call walkers follow the case scrutinee (expr.rs).
#[test]
fn captured_round_in_agrees_1025() {
    on_big_stack(|| {
        let (expr, table) = load(ROUND_IN);
        match check_jit_vs_eval_captured(&expr, &table, NURSERY) {
            CapturedOutcome::Agree(v) => assert_eq!(
                boxed_double(&v),
                Some(1025.0),
                "fromIntegral 1025 :: Double must be 1025.0 in both engines; got {v:?}"
            ),
            other => panic!("expected #1 to be FIXED (Agree 1025.0), got {other:?}"),
        }
    });
}

// #1 MINIMIZED — the 84-node delta-debugged fixture (the precise fix target).
// Confirms the fix on the minimal failing subtree: the surviving live path is the
// inlined integerToBinaryFloat'/roundingMode# dispatch plus the bottoming error
// CAF (`case error … of {}`) as a LetRec binding — the eager-eval of which was the
// real bug. Now Agree at 1025.0.
#[test]
fn captured_round_in_minimized_agrees_1025() {
    on_big_stack(|| {
        let (expr, table) = load(ROUND_IN_MIN);
        assert!(
            expr.nodes.len() <= 100,
            "minimized fixture should be ~84 nodes, got {}",
            expr.nodes.len()
        );
        match check_jit_vs_eval_captured(&expr, &table, NURSERY) {
            CapturedOutcome::Agree(v) => {
                assert_eq!(boxed_double(&v), Some(1025.0), "got {v:?}")
            }
            other => panic!("minimized #1 must now Agree at 1025.0 (FIXED), got {other:?}"),
        }
    });
}

// #2 — `read "42" :: Int` is now FIXED in BOTH engines (was a `BothFail`: the
// tree-walker yielded `NotAFunction`, the JIT `BadFunPtrTag`). The diagnosis was
// NOT a ReadP `~R#` newtype coercion; it was two distinct root causes, both
// landed:
//   1. The unboxed-1-tuple build asymmetry (Translate.hs): GHC wraps the ReadP
//      CPS function in `MkSolo#`, the unboxed 1-tuple `(# f #)` (no runtime rep);
//      the Con-build path boxed it into a heap Con instead of erasing it to its
//      field → a constructor in function position. The `reproReadInt` fixture was
//      regenerated with this fix (meta.cbor + the #1 fixtures are unchanged).
//   2. Eager-let in the JIT (emit/expr.rs): ReadP `expect` is a productive
//      corecursion `F = \k -> let x = F k in <Get parser using x>`; the strict
//      LetNonRec spine force-evaluated `let x = F k` into infinite recursion.
//      Non-trivial LetNonRec RHS is now thunkified (GHC Core `let` is non-strict).
// The golden below is the live eval-vs-expected guard.
#[test]
fn captured_read_int_golden_expects_42() {
    on_big_stack(|| {
        let (expr, table) = load(READ_INT);
        match check_jit_vs_eval_captured(&expr, &table, NURSERY) {
            CapturedOutcome::Agree(v) => assert_eq!(
                boxed_int(&v),
                Some(READ_INT_EXPECTED),
                "read \"42\" must evaluate to 42 in both engines; got {v:?}"
            ),
            other => panic!(
                "golden: read \"42\" must be {READ_INT_EXPECTED} in BOTH engines, got {other:?}"
            ),
        }
    });
}
