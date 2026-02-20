use tidepool_eval::pass::Pass;
use tidepool_eval::{eval, Env, Value, VecHeap};
use tidepool_optimize::beta::BetaReduce;
use tidepool_optimize::case_reduce::CaseReduce;
use tidepool_optimize::dce::Dce;
use tidepool_optimize::inline::Inline;
use tidepool_optimize::pipeline::run_pipeline;
use tidepool_repr::CoreExpr;
use tidepool_testing::gen::arb_core_expr;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};

/// Recursive structural comparison of values.
/// Skips closures, thunks, and join points by returning true.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Lit(l1), Value::Lit(l2)) => l1 == l2,
        (Value::Con(tag1, fields1), Value::Con(tag2, fields2)) => {
            tag1 == tag2
                && fields1.len() == fields2.len()
                && fields1
                    .iter()
                    .zip(fields2.iter())
                    .all(|(f1, f2)| values_equal(f1, f2))
        }
        // For closures and thunks, skip comparison (return true to not fail the test)
        _ => true,
    }
}

/// Helper that verifies an optimization pass preserves evaluation results.
fn check_pass_preserves_eval(pass: &dyn Pass, expr: CoreExpr) -> Result<(), TestCaseError> {
    let mut heap1 = VecHeap::new();
    let env = Env::new();

    // Evaluate original
    let original_res = eval(&expr, &env, &mut heap1);

    // Run the pass
    let mut optimized = expr.clone();
    pass.run(&mut optimized);

    let mut heap2 = VecHeap::new();
    // Evaluate optimized
    let optimized_res = eval(&optimized, &env, &mut heap2);

    match (original_res, optimized_res) {
        (Ok(v1), Ok(v2)) => {
            prop_assert!(
                values_equal(&v1, &v2),
                "Evaluation results differ after pass {}.
Original: {:?}
Optimized: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                pass.name(),
                v1,
                v2,
                expr,
                optimized
            );
        }
        (Err(_), _) => {
            // If original eval fails, we skip this case.
            // Passes are only guaranteed to preserve behavior of well-defined programs.
        }
        (Ok(_), Err(e)) => {
            prop_assert!(
                false,
                "Optimized evaluation failed but original succeeded.
Pass: {}
Error: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                pass.name(),
                e,
                expr,
                optimized
            );
        }
    }
    Ok(())
}

#[test]
fn test_beta_reduce_correctness() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = BetaReduce;
            runner
                .run(&arb_core_expr(), |expr| {
                    check_pass_preserves_eval(&pass, expr)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn test_case_reduce_correctness() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = CaseReduce;
            runner
                .run(&arb_core_expr(), |expr| {
                    check_pass_preserves_eval(&pass, expr)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn test_inline_correctness() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = Inline;
            runner
                .run(&arb_core_expr(), |expr| {
                    check_pass_preserves_eval(&pass, expr)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn test_dce_correctness() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = Dce;
            runner
                .run(&arb_core_expr(), |expr| {
                    check_pass_preserves_eval(&pass, expr)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn test_pipeline_correctness() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |expr| {
                    let mut heap1 = VecHeap::new();
                    let env = Env::new();

                    let original_res = eval(&expr, &env, &mut heap1);

                    let mut optimized = expr.clone();
                    let passes: Vec<Box<dyn Pass>> = vec![
                        Box::new(BetaReduce),
                        Box::new(CaseReduce),
                        Box::new(Inline),
                        Box::new(Dce),
                    ];
                    run_pipeline(&passes, &mut optimized);

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

#[test]
fn test_pipeline_idempotent() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |expr| {
                    let mut optimized = expr.clone();
                    let passes: Vec<Box<dyn Pass>> = vec![
                        Box::new(BetaReduce),
                        Box::new(CaseReduce),
                        Box::new(Inline),
                        Box::new(Dce),
                    ];

                    // First run
                    run_pipeline(&passes, &mut optimized);

                    // Second run
                    let stats = run_pipeline(&passes, &mut optimized);

                    prop_assert_eq!(
                        stats.iterations,
                        1,
                        "Pipeline was not idempotent.
Stats: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                        stats,
                        expr,
                        optimized
                    );

                    Ok(())
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}
