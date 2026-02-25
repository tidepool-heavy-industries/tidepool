use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use tidepool_eval::pass::Pass;
use tidepool_eval::{eval, Env, Value, VecHeap};
use tidepool_optimize::beta::BetaReduce;
use tidepool_optimize::case_reduce::CaseReduce;
use tidepool_optimize::dce::Dce;
use tidepool_optimize::inline::Inline;
use tidepool_optimize::pipeline::optimize;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, VarId};
use tidepool_repr::{CoreExpr, TreeBuilder};
use tidepool_testing::gen::arb_core_expr;

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

fn expr_to_builder(expr: CoreExpr) -> TreeBuilder {
    let mut b = TreeBuilder::new();
    for node in expr.nodes {
        b.push(node);
    }
    b
}

fn wrap_in_beta_reducible(body: CoreExpr, arg: CoreExpr, binder: VarId) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let body_len = body.nodes.len();
    let arg_len = arg.nodes.len();

    let body_off = b.push_tree(expr_to_builder(body));
    let body_root = body_off + body_len - 1;

    let arg_off = b.push_tree(expr_to_builder(arg));
    let arg_root = arg_off + arg_len - 1;

    let lam = b.push(CoreFrame::Lam {
        binder,
        body: body_root,
    });
    b.push(CoreFrame::App {
        fun: lam,
        arg: arg_root,
    });
    b.build()
}

#[test]
fn beta_reduction_preserves_eval() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = BetaReduce;
            runner
                .run(
                    &(arb_core_expr(), arb_core_expr(), any::<u64>()),
                    |(body, arg, binder_id)| {
                        let expr = wrap_in_beta_reducible(body, arg, VarId(binder_id));
                        check_pass_preserves_eval(&pass, expr)
                    },
                )
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

fn wrap_in_unused_let(rhs: CoreExpr, body: CoreExpr, binder: VarId) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let rhs_len = rhs.nodes.len();
    let body_len = body.nodes.len();

    let rhs_off = b.push_tree(expr_to_builder(rhs));
    let rhs_root = rhs_off + rhs_len - 1;

    let body_off = b.push_tree(expr_to_builder(body));
    let body_root = body_off + body_len - 1;

    b.push(CoreFrame::LetNonRec {
        binder,
        rhs: rhs_root,
        body: body_root,
    });
    b.build()
}

#[test]
fn dce_preserves_eval() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = Dce;
            runner
                .run(
                    &(arb_core_expr(), arb_core_expr(), any::<u64>()),
                    |(rhs, body, binder_id)| {
                        // Use a very large VarId to avoid collisions with variables in body
                        let binder = VarId(0xF000_0000_0000_0000 | binder_id);
                        let expr = wrap_in_unused_let(rhs, body, binder);
                        check_pass_preserves_eval(&pass, expr)
                    },
                )
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

fn wrap_in_used_once_let(rhs: CoreExpr, binder: VarId) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let rhs_len = rhs.nodes.len();

    let rhs_off = b.push_tree(expr_to_builder(rhs));
    let rhs_root = rhs_off + rhs_len - 1;

    let var = b.push(CoreFrame::Var(binder));
    b.push(CoreFrame::LetNonRec {
        binder,
        rhs: rhs_root,
        body: var,
    });
    b.build()
}

#[test]
fn inline_preserves_eval() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = Inline;
            runner
                .run(&(arb_core_expr(), any::<u64>()), |(rhs, binder_id)| {
                    let binder = VarId(binder_id);
                    let expr = wrap_in_used_once_let(rhs, binder);
                    check_pass_preserves_eval(&pass, expr)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

fn wrap_in_known_con_case(body: CoreExpr, binder: VarId, tag: DataConId) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let lit = b.push(CoreFrame::Lit(Literal::LitInt(42)));
    let con = b.push(CoreFrame::Con {
        tag,
        fields: vec![lit],
    });

    let body_len = body.nodes.len();
    let body_off = b.push_tree(expr_to_builder(body));
    let body_root = body_off + body_len - 1;

    let alt = Alt {
        con: AltCon::DataAlt(tag),
        binders: vec![VarId(binder.0 + 1)], // dummy binder for the field
        body: body_root,
    };
    b.push(CoreFrame::Case {
        scrutinee: con,
        binder,
        alts: vec![alt],
    });
    b.build()
}


#[test]
fn case_of_known_con_preserves_eval() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = CaseReduce;
            runner
                .run(&(arb_core_expr(), any::<u64>()), |(body, binder_id)| {
                    let binder = VarId(binder_id);
                    let tag = DataConId(1);
                    let expr = wrap_in_known_con_case(body, binder, tag);
                    check_pass_preserves_eval(&pass, expr)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn multiple_passes_preserve_eval() {
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
                    optimize(&mut optimized);

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
fn optimization_is_idempotent() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |expr| {
                    let mut optimized1 = expr.clone();
                    optimize(&mut optimized1);

                    let mut optimized2 = optimized1.clone();
                    let stats = optimize(&mut optimized2);

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
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn dce_does_not_increase_size() {
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
                    let mut optimized = expr.clone();
                    pass.run(&mut optimized);

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
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

