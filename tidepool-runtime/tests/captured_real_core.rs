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
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::value::Value;
use tidepool_eval::{env_from_datacon_table, eval, VecHeap};
use tidepool_repr::serial::read::{read_cbor, read_metadata};
use tidepool_repr::{CoreExpr, DataConTable, Literal};
use tidepool_testing::proptest::check_jit_vs_eval_with_table;

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
fn captured_round_in_is_jit_vs_eval_divergence() {
    on_big_stack(|| {
        let (expr, table) = load(ROUND_IN);
        let env = env_from_datacon_table(&table);
        let mut heap = VecHeap::new();

        // Eval is correct: 1025.0.
        let ev = eval(&expr, &env, &mut heap).expect("eval should succeed");
        assert_eq!(
            boxed_double(&ev),
            Some(1025.0),
            "eval must compute the exact value; got {ev:?}"
        );

        // JIT diverges: roundingMode#: IN.
        let jit = JitEffectMachine::compile(&expr, &table, NURSERY)
            .expect("compile should succeed")
            .run_pure();
        match jit {
            Err(JitError::Yield(YieldError::UserErrorMsg(msg))) => {
                assert!(
                    msg.contains("roundingMode#"),
                    "expected the roundingMode# divergence, got: {msg}"
                );
            }
            other => panic!("expected JIT roundingMode#: IN, got {other:?}"),
        }

        // The oracle itself flags the divergence (eval Ok, JIT Err).
        let (expr2, table2) = load(ROUND_IN);
        assert!(
            check_jit_vs_eval_with_table(expr2, &table2, NURSERY).is_err(),
            "oracle must detect the #1 JIT-vs-eval divergence"
        );
    });
}

// #2 — NOT a JIT-vs-eval differential. `read "42" :: Int` fails in BOTH engines:
// the tree-walker yields `NotAFunction` and the JIT `BadFunPtrTag` (a constructor
// in function position, reached through ReadP's newtype coercion). Because eval
// also fails, the differential oracle CANNOT catch this — it is a translation /
// shared-repr bug, a different class from #1. Captured so that class is on record.
#[test]
fn captured_read_int_both_engines_fail() {
    on_big_stack(|| {
        let (expr, table) = load(READ_INT);
        let env = env_from_datacon_table(&table);
        let mut heap = VecHeap::new();

        let ev = eval(&expr, &env, &mut heap);
        assert!(
            ev.is_err(),
            "tree-walker also fails (not JIT-only); got {ev:?}"
        );

        let jit = JitEffectMachine::compile(&expr, &table, NURSERY)
            .expect("compile should succeed")
            .run_pure();
        assert!(
            matches!(jit, Err(JitError::Yield(YieldError::BadFunPtrTag(_)))),
            "expected JIT BadFunPtrTag (non-function applied), got {jit:?}"
        );

        // Because BOTH fail, the differential oracle does NOT flag #2 — proving
        // it is out of the JIT-vs-eval net's reach (translation-level, not a
        // JIT-only value-repr bug).
        let (expr2, table2) = load(READ_INT);
        assert!(
            check_jit_vs_eval_with_table(expr2, &table2, NURSERY).is_ok(),
            "oracle cannot flag #2: both engines fail, so it is not a differential"
        );
    });
}
