//! Shaped cells: each pins one exact transformation shape that a random
//! generator hits rarely or never. Every cell keeps its original assertion on
//! the optimized shape, then still routes the unoptimized input through the
//! pass-preservation oracle.

use proptest::prelude::*;
use serial_test::serial;
use tidepool_optimize::beta::BetaReduce;
use tidepool_optimize::case_reduce::CaseReduce;
use tidepool_optimize::dce::Dce;
use tidepool_optimize::inline::Inline;
use tidepool_optimize::partial::PartialEval;
use tidepool_optimize::Pass;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::TreeBuilder;
use tidepool_testing::gen::arb_core_expr;
use tidepool_testing::proptest::check_pass_preserves_eval;

use crate::support::{
    run_matrix, wrap_in_beta_reducible, wrap_in_known_con_case, wrap_in_unused_let,
    wrap_in_used_once_let,
};

/// `BetaReduce` x `wrap_in_beta_reducible(arb_core_expr, arb_core_expr, _)`.
#[test]
#[serial]
fn beta_reduction_preserves_eval() {
    run_matrix(
        16 * 1024 * 1024,
        200,
        || (arb_core_expr(), arb_core_expr(), any::<u64>()),
        |(body, arg, binder_id)| {
            let expr = wrap_in_beta_reducible(body, arg, VarId(binder_id));
            check_pass_preserves_eval(&BetaReduce, expr)
        },
    );
}

/// `Dce` x `wrap_in_unused_let(arb_core_expr, arb_core_expr, _)`.
#[test]
#[serial]
fn dce_preserves_eval() {
    run_matrix(
        16 * 1024 * 1024,
        200,
        || (arb_core_expr(), arb_core_expr(), any::<u64>()),
        |(rhs, body, binder_id)| {
            // A very large VarId avoids collisions with variables in body.
            let binder = VarId(0xF000_0000_0000_0000 | binder_id);
            let expr = wrap_in_unused_let(rhs, body, binder);
            check_pass_preserves_eval(&Dce, expr)
        },
    );
}

/// `Inline` x `wrap_in_used_once_let(arb_core_expr, _)`.
#[test]
#[serial]
fn inline_preserves_eval() {
    run_matrix(
        16 * 1024 * 1024,
        200,
        || (arb_core_expr(), any::<u64>()),
        |(rhs, binder_id)| {
            let expr = wrap_in_used_once_let(rhs, VarId(binder_id));
            check_pass_preserves_eval(&Inline, expr)
        },
    );
}

/// `CaseReduce` x `wrap_in_known_con_case(arb_core_expr, _, _)`.
#[test]
#[serial]
fn case_of_known_con_preserves_eval() {
    run_matrix(
        16 * 1024 * 1024,
        200,
        || (arb_core_expr(), any::<u64>()),
        |(body, binder_id)| {
            let expr = wrap_in_known_con_case(body, VarId(binder_id), DataConId(1));
            check_pass_preserves_eval(&CaseReduce, expr)
        },
    );
}

/// `PartialEval` x a `Case(Con(tag, [Lit(val)]), Alt(tag, [x]) -> x)` shape:
/// case-of-known-constructor must fold to the literal, not merely evaluate to
/// the same value.
#[test]
fn nested_known_con_case_reduces() {
    run_matrix(8 * 1024 * 1024, 200, any::<i64>, |val| {
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

        let mut optimized = expr.clone();
        PartialEval.run(&mut optimized);
        prop_assert_eq!(
            optimized.nodes.len(),
            1,
            "Should have reduced to a single node"
        );
        prop_assert!(matches!(optimized.nodes[0], CoreFrame::Lit(Literal::LitInt(v)) if v == val));

        check_pass_preserves_eval(&PartialEval, expr)
    });
}

/// `PartialEval` x a `let a = .. in let b = .. in a + b * c` chain: nested
/// let propagation plus primop folding must fold the whole chain to one
/// literal.
#[test]
fn nested_let_propagation() {
    run_matrix(
        8 * 1024 * 1024,
        200,
        || (any::<i64>(), any::<i64>(), any::<i64>()),
        |(a, b, c)| {
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

            let expected = (a.wrapping_add(b)).wrapping_mul(c);

            let mut optimized = expr.clone();
            PartialEval.run(&mut optimized);

            prop_assert_eq!(optimized.nodes.len(), 1, "Should have folded completely");
            prop_assert!(
                matches!(optimized.nodes[0], CoreFrame::Lit(Literal::LitInt(v)) if v == expected)
            );

            check_pass_preserves_eval(&PartialEval, expr)
        },
    );
}

/// `PartialEval` x every foldable binary `PrimOpKind` (arith + comparisons)
/// applied to two literals: each must fold to the literal result, not merely
/// evaluate to it.
#[test]
fn primop_fold_all_foldable_ops() {
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

    let ops_len = ops.len();
    run_matrix(
        8 * 1024 * 1024,
        200,
        move || (0..ops_len, any::<i64>(), any::<i64>()),
        move |(op_idx, a, b)| {
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
                PrimOpKind::IntEq => {
                    if a == b {
                        1
                    } else {
                        0
                    }
                }
                PrimOpKind::IntNe => {
                    if a != b {
                        1
                    } else {
                        0
                    }
                }
                PrimOpKind::IntLt => {
                    if a < b {
                        1
                    } else {
                        0
                    }
                }
                PrimOpKind::IntLe => {
                    if a <= b {
                        1
                    } else {
                        0
                    }
                }
                PrimOpKind::IntGt => {
                    if a > b {
                        1
                    } else {
                        0
                    }
                }
                PrimOpKind::IntGe => {
                    if a >= b {
                        1
                    } else {
                        0
                    }
                }
                _ => unreachable!(),
            };

            let mut optimized = expr.clone();
            PartialEval.run(&mut optimized);

            prop_assert_eq!(
                optimized.nodes.len(),
                1,
                "Should have folded {:?} completely",
                op
            );
            prop_assert!(
                matches!(optimized.nodes[0], CoreFrame::Lit(Literal::LitInt(v)) if v == expected)
            );

            check_pass_preserves_eval(&PartialEval, expr)
        },
    );
}

/// `PartialEval` x `IntNegate(Lit(a))`: unary primop folding.
#[test]
fn primop_fold_negate() {
    run_matrix(8 * 1024 * 1024, 200, any::<i64>, |a| {
        let mut builder = TreeBuilder::new();
        let lit_a = builder.push(CoreFrame::Lit(Literal::LitInt(a)));
        builder.push(CoreFrame::PrimOp {
            op: PrimOpKind::IntNegate,
            args: vec![lit_a],
        });
        let expr = builder.build();

        let expected = a.wrapping_neg();

        let mut optimized = expr.clone();
        PartialEval.run(&mut optimized);

        prop_assert_eq!(
            optimized.nodes.len(),
            1,
            "Should have folded IntNegate completely"
        );
        prop_assert!(
            matches!(optimized.nodes[0], CoreFrame::Lit(Literal::LitInt(v)) if v == expected)
        );

        check_pass_preserves_eval(&PartialEval, expr)
    });
}
