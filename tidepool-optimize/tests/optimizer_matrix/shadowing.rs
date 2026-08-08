//! Shadowing cells, all against `arb_core_expr_shadowing`.
//!
//! The shared generator's single fresh-var counter meant no binder id was
//! ever reused, leaving every other cell in this matrix structurally blind to
//! the shadowing bug class (`PartialEval`'s `Lam`/`Join` arms,
//! `normalize.rs`'s scoped `var_map`). `arb_core_expr_shadowing` deliberately
//! reuses an in-scope binder id for a fraction of Lam/Let/Case/Join binders,
//! so these three cells are the only ones in the matrix that can catch that
//! class going forward.

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use tidepool_eval::{eval, Env, VecHeap};
use tidepool_optimize::partial::PartialEval;
use tidepool_repr::check_toplevel_varids;
use tidepool_testing::differential::{
    check, DiffConfig, EvalErrorClass, JitErrorClass, ReachCounter,
};
use tidepool_testing::gen::arb_core_expr_shadowing;
use tidepool_testing::proptest::check_pass_preserves_eval;

use crate::support::run_matrix;

/// `PartialEval` x `arb_core_expr_shadowing`.
#[test]
fn partial_eval_preserves_eval_with_shadowing() {
    run_matrix(
        8 * 1024 * 1024,
        800,
        || arb_core_expr_shadowing(4, 60),
        |expr| check_pass_preserves_eval(&PartialEval, expr),
    );
}

/// `FullPipeline` x `arb_core_expr_shadowing`: eval preservation (WHNF).
#[test]
fn full_pipeline_preserves_eval_with_shadowing() {
    run_matrix(
        16 * 1024 * 1024,
        800,
        || arb_core_expr_shadowing(4, 60),
        |expr| {
            let mut heap1 = VecHeap::new();
            let env = Env::new();
            let original_res = eval(&expr, &env, &mut heap1);

            let mut optimized = expr.clone();
            tidepool_optimize::pipeline::optimize(&mut optimized).unwrap();

            let mut heap2 = VecHeap::new();
            let optimized_res = eval(&optimized, &env, &mut heap2);

            match (original_res, optimized_res) {
                (Ok(v1), Ok(v2)) => {
                    prop_assert!(
                        tidepool_testing::proptest::values_equal(&v1, &v2),
                        "Pipeline evaluation results differ.
Original: {:?}
Optimized: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                        v1,
                        v2,
                        expr,
                        optimized
                    );
                }
                (Err(_), _) => {}
                (Ok(_), Err(e)) => {
                    prop_assert!(
                        false,
                        "Pipeline optimized evaluation failed but original succeeded.
Error: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                        e,
                        expr,
                        optimized
                    );
                }
            }
            Ok(())
        },
    );
}

/// The tolerated-class policy for the shadowing JIT differential.
///
/// `arb_core_expr_shadowing` shares `arb_core_expr`'s generation machinery
/// (same `gen_leaf`/`gen_prim_op`/... functions, just with a nonzero shadow
/// weight), so it inherits the same partiality: it can produce `LetRec`
/// shapes with inter-dependent simple bindings the interpreter thunks but the
/// JIT evaluates sequentially (`UnresolvedVar`), those unresolved vars can
/// leave garbage heap objects behind a later read (`HeapBridge`), a tiny
/// nursery can legitimately overflow (`HeapOverflow`), and a numeric
/// conversion chain can feed an out-of-range `Int#` through `Chr`
/// (`TypeMismatch` on the eval side). Both engines also correctly detect a
/// generator-built self-referencing thunk — eval as `InfiniteLoop`, the JIT
/// as `BlackHole` under its own name; only the eval side is named here
/// (`BlackHole` deliberately stays unnamed on the JIT side, so a JIT-only
/// blackhole with eval succeeding still fails as a real divergence).
fn shadow_jit_policy() -> DiffConfig {
    DiffConfig::new("optimizer_matrix/jit_vs_eval_shadowing")
        .expect_jit(&[
            JitErrorClass::HeapOverflow,
            JitErrorClass::UnresolvedVar,
            JitErrorClass::HeapBridge,
        ])
        .expect_eval(&[EvalErrorClass::TypeMismatch, EvalErrorClass::InfiniteLoop])
}

/// `JitCompile` (raw `normalize` + Cranelift emission, no optimizer pass) x
/// `arb_core_expr_shadowing`: `JitEffectMachine::compile` runs `normalize` on
/// every program before emission (the production path), so this exercises
/// `normalize.rs`'s scoped `var_map` directly against shadowed-binder input.
///
/// `check_toplevel_varids` gates out an unrelated, expected false positive:
/// it defends against a distinct bug class (two DISTINCT real GHC top-level
/// bindings colliding on the same `VarId`) by rejecting a duplicate binder on
/// the tree's outermost Let-chain — a precondition that holds for real
/// extractor output but not for this synthetic generator, which can (by
/// design) place a reused binder id at the very top of the generated tree.
/// That is legitimate local shadowing, not a top-level collision, so those
/// cases are skipped here rather than tripping an unrelated guard.
///
/// The reach counter is local to this test function (not a `static`):
/// nextest gives every test its own process, so a floor asserted from a
/// separate test would always observe a zero counter.
///
/// The floor is 0.5: an observed run reaches ~62.6% (501/800), comfortably
/// above it, so the floor absorbs ordinary seed-to-seed variance without
/// tripping on it, but still catches a real regression that guts how often
/// this cell reaches an actual value comparison.
#[test]
fn jit_agrees_with_eval_with_shadowing() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let reach = ReachCounter::new("optimizer_matrix/jit_vs_eval_shadowing");
            let cfg = shadow_jit_policy();
            let mut runner = TestRunner::new(Config {
                cases: 800,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr_shadowing(4, 60), |expr| {
                    prop_assume!(check_toplevel_varids(&expr).is_ok());
                    check(expr, &cfg, &reach)
                })
                .unwrap();
            reach.assert_floor(0.5);
        })
        .unwrap();
    handle.join().unwrap();
}
