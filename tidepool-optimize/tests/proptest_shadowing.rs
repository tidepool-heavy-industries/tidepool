//! F4 (plan 04-optimizer-shadowing): the shared proptest generator used a
//! single `Rc<Cell<u64>>` fresh-var counter across all generation contexts,
//! so no binder id was EVER reused — the entire differential net was
//! structurally blind to the shadowing bug class that F1/F2/F3 fixed
//! (PartialEval's `Lam`/`Join` arms, `normalize.rs`'s `var_map`). This file
//! runs the pass-preservation and JIT-vs-eval differential suites with
//! `arb_core_expr_shadowing` — which deliberately reuses in-scope binder ids
//! for a fraction of Lam/Let/Case/Join binders — so that bug class stays
//! covered going forward instead of silently un-testable again.

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use tidepool_eval::{eval, Env, VecHeap};
use tidepool_optimize::partial::PartialEval;
use tidepool_optimize::pipeline::optimize;
use tidepool_repr::check_toplevel_varids;
use tidepool_testing::gen::arb_core_expr_shadowing;
use tidepool_testing::proptest::{check_jit_vs_eval, check_pass_preserves_eval, values_equal};

/// F1/F2 regression at scale: `PartialEval` must preserve evaluation even
/// when Lam/Let/Case/Join binders shadow an outer same-named binder.
#[test]
fn partial_eval_preserves_eval_with_shadowing() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 800,
                ..Config::default()
            });
            let pass = PartialEval;
            runner
                .run(&arb_core_expr_shadowing(4, 60), |expr| {
                    check_pass_preserves_eval(&pass, expr)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

/// F3 regression at scale: `JitEffectMachine::compile` runs `normalize` on
/// every program before Cranelift emission (the production path), so this
/// exercises normalize.rs's scoped `var_map` directly against shadowed-binder
/// input — no `PartialEval` involved.
///
/// `check_toplevel_varids` gates out an UNRELATED, expected false positive:
/// it defends against the #313 bug class (two DISTINCT real GHC top-level
/// bindings colliding on the same VarId) by rejecting a duplicate binder on
/// the tree's outermost Let-chain — a precondition that holds for real
/// extractor output but not for this synthetic generator, which can (by
/// design) place a reused binder id at the very top of the generated tree.
/// That's legitimate local shadowing, not a #313-class collision, so those
/// cases are skipped here rather than tripping an unrelated guard.
#[test]
fn jit_agrees_with_eval_with_shadowing() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 800,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr_shadowing(4, 60), |expr| {
                    prop_assume!(check_toplevel_varids(&expr).is_ok());
                    check_jit_vs_eval(expr, 64 * 1024)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

/// F1/F2/F3 combined: the full `optimize` pipeline (PartialEval + Beta +
/// Inline + DCE + CaseReduce, to fixpoint) must preserve evaluation on
/// shadowed-binder input too — not just PartialEval in isolation.
#[test]
fn full_pipeline_preserves_eval_with_shadowing() {
    let handle = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 800,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr_shadowing(4, 60), |expr| {
                    let mut heap1 = VecHeap::new();
                    let env = Env::new();
                    let original_res = eval(&expr, &env, &mut heap1);

                    let mut optimized = expr.clone();
                    optimize(&mut optimized).unwrap();

                    let mut heap2 = VecHeap::new();
                    let optimized_res = eval(&optimized, &env, &mut heap2);

                    match (original_res, optimized_res) {
                        (Ok(v1), Ok(v2)) => {
                            prop_assert!(
                                values_equal(&v1, &v2),
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
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}
