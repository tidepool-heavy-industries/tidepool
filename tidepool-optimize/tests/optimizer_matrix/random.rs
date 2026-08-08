//! Random cells: each pass (or the full pipeline) driven directly by an
//! unconstrained generator, no pinned shape.

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use serial_test::serial;
use std::cell::Cell;
use tidepool_eval::{deep_force, eval, Env, VecHeap};
use tidepool_optimize::beta::BetaReduce;
use tidepool_optimize::case_reduce::CaseReduce;
use tidepool_optimize::dce::Dce;
use tidepool_optimize::inline::Inline;
use tidepool_optimize::partial::PartialEval;
use tidepool_optimize::pipeline::optimize;
use tidepool_optimize::Pass;
use tidepool_testing::gen::{arb_core_expr, arb_ground_expr};
use tidepool_testing::proptest::check_pass_preserves_eval;

use crate::support::run_matrix;

/// `BetaReduce` x `arb_core_expr`.
#[test]
#[serial]
fn random_beta_reduce_preserves_eval() {
    run_matrix(16 * 1024 * 1024, 200, arb_core_expr, |expr| {
        check_pass_preserves_eval(&BetaReduce, expr)
    });
}

/// `Dce` x `arb_core_expr`.
#[test]
#[serial]
fn random_dce_preserves_eval() {
    run_matrix(16 * 1024 * 1024, 200, arb_core_expr, |expr| {
        check_pass_preserves_eval(&Dce, expr)
    });
}

/// `Inline` x `arb_core_expr`.
#[test]
#[serial]
fn random_inline_preserves_eval() {
    run_matrix(16 * 1024 * 1024, 200, arb_core_expr, |expr| {
        check_pass_preserves_eval(&Inline, expr)
    });
}

/// `CaseReduce` x `arb_core_expr`.
#[test]
#[serial]
fn random_case_reduce_preserves_eval() {
    run_matrix(16 * 1024 * 1024, 200, arb_core_expr, |expr| {
        check_pass_preserves_eval(&CaseReduce, expr)
    });
}

/// `PartialEval` x `arb_core_expr`.
#[test]
fn random_partial_eval_preserves_eval() {
    run_matrix(8 * 1024 * 1024, 200, arb_core_expr, |expr| {
        check_pass_preserves_eval(&PartialEval, expr)
    });
}

/// `Dce` is size-non-increasing on `arb_core_expr`.
#[test]
#[serial]
fn dce_does_not_increase_size() {
    run_matrix(16 * 1024 * 1024, 200, arb_core_expr, |expr| {
        let mut optimized = expr.clone();
        Dce.run(&mut optimized);

        prop_assert!(
            optimized.nodes.len() <= expr.nodes.len(),
            "DCE increased expression size.
Original size: {}
Optimized size: {}
Expr: {:#?}
Optimized Expr: {:#?}",
            expr.nodes.len(),
            optimized.nodes.len(),
            expr,
            optimized
        );
        Ok(())
    });
}

/// `FullPipeline` x `arb_core_expr`: eval preservation (WHNF).
#[test]
#[serial]
fn multiple_passes_preserve_eval() {
    run_matrix(16 * 1024 * 1024, 200, arb_core_expr, |expr| {
        crate::support::check_pipeline_preserves_eval(expr)
    });
}

/// `FullPipeline` x `arb_core_expr`: running `optimize` twice is the same as
/// running it once.
#[test]
#[serial]
fn optimization_is_idempotent() {
    run_matrix(16 * 1024 * 1024, 200, arb_core_expr, |expr| {
        let mut optimized1 = expr.clone();
        optimize(&mut optimized1).unwrap();

        let mut optimized2 = optimized1.clone();
        let stats = optimize(&mut optimized2).unwrap();

        prop_assert_eq!(
            &optimized1,
            &optimized2,
            "Optimization was not idempotent (expressions differ).
Expr: {:#?}
Once: {:#?}
Twice: {:#?}",
            expr,
            optimized1,
            optimized2
        );

        prop_assert_eq!(
            stats.iterations,
            1,
            "Optimization was not idempotent (reported changes on second run).
Stats: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
            stats,
            expr,
            optimized1
        );

        Ok(())
    });
}

/// `FullPipeline` x `arb_ground_expr`: eval preservation with a reach floor.
/// Ground-typed results are always structurally comparable, and the optimizer
/// may legitimately make a lazy error strict (case-of-known-constructor
/// inlining a previously-thunked error path) — both engines erroring after
/// `deep_force` is then an accepted strictness change, not a bug. The floor
/// (`compared >= 50` of 200) guards against a change that silently stops this
/// cell from ever reaching a real value comparison.
#[test]
fn optimization_preserves_semantics() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let compared = Cell::new(0u64);
            let both_error = Cell::new(0u64);
            let deep_force_fail = Cell::new(0u64);
            let eval_only_error = Cell::new(0u64);

            runner
                .run(&arb_ground_expr(), |expr| {
                    let mut heap1 = VecHeap::new();
                    let before = eval(&expr, &Env::new(), &mut heap1);

                    let mut optimized = expr.clone();
                    let _ = optimize(&mut optimized);

                    let mut heap2 = VecHeap::new();
                    let after = eval(&optimized, &Env::new(), &mut heap2);

                    match (before, after) {
                        (Ok(v1), Ok(v2)) => {
                            let f1 = deep_force(v1, &mut heap1);
                            let f2 = deep_force(v2, &mut heap2);
                            match (f1, f2) {
                                (Ok(fv1), Ok(fv2)) => {
                                    prop_assert!(
                                        tidepool_testing::proptest::values_equal(&fv1, &fv2),
                                        "optimize changed the deep-forced value.
Original: {:?}
Optimized: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                                        fv1,
                                        fv2,
                                        expr,
                                        optimized
                                    );
                                    compared.set(compared.get() + 1);
                                }
                                (Err(_), Err(_)) => {
                                    deep_force_fail.set(deep_force_fail.get() + 1);
                                }
                                (Ok(v), Err(e)) => {
                                    prop_assert!(
                                        false,
                                        "optimize broke deep_force: before Ok({}) after Err({:?})",
                                        v,
                                        e
                                    );
                                }
                                (Err(_), Ok(_)) => {
                                    deep_force_fail.set(deep_force_fail.get() + 1);
                                }
                            }
                        }
                        (Err(_), Err(_)) => {
                            both_error.set(both_error.get() + 1);
                        }
                        (Ok(v1), Err(e)) => {
                            let forced = deep_force(v1, &mut heap1);
                            match forced {
                                Err(_) => {
                                    both_error.set(both_error.get() + 1);
                                }
                                Ok(_) => {
                                    prop_assert!(
                                        false,
                                        "optimize broke eval: original deep_forces Ok but optimized errors: {:?}",
                                        e
                                    );
                                }
                            }
                        }
                        (Err(_), Ok(_)) => {
                            eval_only_error.set(eval_only_error.get() + 1);
                        }
                    }
                    Ok(())
                })
                .unwrap();

            let compared = compared.get();
            eprintln!(
                "optimization_preserves_semantics: compared={compared}, both_error={}, \
                 eval_only_error={}, deep_force_fail={}",
                both_error.get(),
                eval_only_error.get(),
                deep_force_fail.get()
            );
            assert!(
                compared >= 50,
                "Only {compared} of 200 cases reached value comparison"
            );
        })
        .unwrap();
    handle.join().unwrap();
}
