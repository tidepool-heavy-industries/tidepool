use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use tidepool_eval::pass::Pass;
use tidepool_eval::{eval, Env, Value, VecHeap};
use tidepool_optimize::partial::PartialEval;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
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

#[allow(dead_code)]
fn expr_to_builder(expr: CoreExpr) -> TreeBuilder {
    let mut b = TreeBuilder::new();
    for node in expr.nodes {
        b.push(node);
    }
    b
}

#[test]
fn random_partial_eval_preserves_eval() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = PartialEval;
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
fn nested_known_con_case_reduces() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = PartialEval;
            runner
                .run(&any::<i64>(), |val| {
                    let mut b = TreeBuilder::new();
                    let lit = b.push(CoreFrame::Lit(Literal::LitInt(val)));
                    let con = b.push(CoreFrame::Con {
                        tag: DataConId(1),
                        fields: vec![lit],
                    });
                    let var = b.push(CoreFrame::Var(VarId(200)));
                    let alt = Alt {
                        con: AltCon::DataAlt(DataConId(1)),
                        binders: vec![VarId(200)],
                        body: var,
                    };
                    b.push(CoreFrame::Case {
                        scrutinee: con,
                        binder: VarId(100),
                        alts: vec![alt],
                    });
                    let expr = b.build();
                    
                    // Verify it actually reduces to just the literal
                    let mut optimized = expr.clone();
                    pass.run(&mut optimized);
                    prop_assert_eq!(optimized.nodes.len(), 1, "Should have reduced to a single node");
                    prop_assert!(matches!(optimized.nodes[0], CoreFrame::Lit(Literal::LitInt(v)) if v == val));

                    check_pass_preserves_eval(&pass, expr)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn nested_let_propagation() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = PartialEval;
            runner
                .run(&(any::<i64>(), any::<i64>(), any::<i64>()), |(a, b, c)| {
                    let mut builder = TreeBuilder::new();
                    
                    // let VarId(1) = Lit(a) in 
                    // let VarId(2) = PrimOp(IntAdd, [Var(VarId(1)), Lit(b)]) in 
                    // PrimOp(IntMul, [Var(VarId(2)), Lit(c)])
                    
                    let lit_a = builder.push(CoreFrame::Lit(Literal::LitInt(a)));
                    let lit_b = builder.push(CoreFrame::Lit(Literal::LitInt(b)));
                    let lit_c = builder.push(CoreFrame::Lit(Literal::LitInt(c)));
                    
                    let var1 = builder.push(CoreFrame::Var(VarId(1)));
                    let add = builder.push(CoreFrame::PrimOp {
                        op: PrimOpKind::IntAdd,
                        args: vec![var1, lit_b],
                    });
                    
                    let var2 = builder.push(CoreFrame::Var(VarId(2)));
                    let mul = builder.push(CoreFrame::PrimOp {
                        op: PrimOpKind::IntMul,
                        args: vec![var2, lit_c],
                    });
                    
                    let let2 = builder.push(CoreFrame::LetNonRec {
                        binder: VarId(2),
                        rhs: add,
                        body: mul,
                    });
                    
                    builder.push(CoreFrame::LetNonRec {
                        binder: VarId(1),
                        rhs: lit_a,
                        body: let2,
                    });
                    
                    let expr = builder.build();
                    
                    // Expected value: (a.wrapping_add(b)).wrapping_mul(c)
                    let expected = (a.wrapping_add(b)).wrapping_mul(c);
                    
                    let mut optimized = expr.clone();
                    pass.run(&mut optimized);
                    
                    prop_assert_eq!(optimized.nodes.len(), 1, "Should have folded completely");
                    prop_assert!(matches!(optimized.nodes[0], CoreFrame::Lit(Literal::LitInt(v)) if v == expected));
                    
                    check_pass_preserves_eval(&pass, expr)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn primop_fold_all_foldable_ops() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = PartialEval;
            
            let ops = vec![
                PrimOpKind::IntAdd,
                PrimOpKind::IntSub,
                PrimOpKind::IntMul,
                PrimOpKind::IntEq,
                PrimOpKind::IntNe,
                PrimOpKind::IntLt,
                PrimOpKind::IntLe,
                PrimOpKind::IntGt,
                PrimOpKind::IntGe,
            ];

            runner
                .run(&(0..ops.len(), any::<i64>(), any::<i64>()), |(op_idx, a, b)| {
                    let op = ops[op_idx];
                    let mut builder = TreeBuilder::new();
                    let lit_a = builder.push(CoreFrame::Lit(Literal::LitInt(a)));
                    let lit_b = builder.push(CoreFrame::Lit(Literal::LitInt(b)));
                    builder.push(CoreFrame::PrimOp {
                        op,
                        args: vec![lit_a, lit_b],
                    });
                    let expr = builder.build();
                    
                    let expected = match op {
                        PrimOpKind::IntAdd => a.wrapping_add(b),
                        PrimOpKind::IntSub => a.wrapping_sub(b),
                        PrimOpKind::IntMul => a.wrapping_mul(b),
                        PrimOpKind::IntEq => if a == b { 1 } else { 0 },
                        PrimOpKind::IntNe => if a != b { 1 } else { 0 },
                        PrimOpKind::IntLt => if a < b { 1 } else { 0 },
                        PrimOpKind::IntLe => if a <= b { 1 } else { 0 },
                        PrimOpKind::IntGt => if a > b { 1 } else { 0 },
                        PrimOpKind::IntGe => if a >= b { 1 } else { 0 },
                        _ => unreachable!(),
                    };
                    
                    let mut optimized = expr.clone();
                    pass.run(&mut optimized);
                    
                    prop_assert_eq!(optimized.nodes.len(), 1, "Should have folded {:?} completely", op);
                    prop_assert!(matches!(optimized.nodes[0], CoreFrame::Lit(Literal::LitInt(v)) if v == expected));
                    
                    check_pass_preserves_eval(&pass, expr)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn primop_fold_negate() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            let pass = PartialEval;
            runner
                .run(&any::<i64>(), |a| {
                    let mut builder = TreeBuilder::new();
                    let lit_a = builder.push(CoreFrame::Lit(Literal::LitInt(a)));
                    builder.push(CoreFrame::PrimOp {
                        op: PrimOpKind::IntNegate,
                        args: vec![lit_a],
                    });
                    let expr = builder.build();
                    
                    let expected = a.wrapping_neg();
                    
                    let mut optimized = expr.clone();
                    pass.run(&mut optimized);
                    
                    prop_assert_eq!(optimized.nodes.len(), 1, "Should have folded IntNegate completely");
                    prop_assert!(matches!(optimized.nodes[0], CoreFrame::Lit(Literal::LitInt(v)) if v == expected));
                    
                    check_pass_preserves_eval(&pass, expr)
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}
