//! Both backends preserve partial constructor fields until the field is demanded.
//! Matching only the outer tag must not enter Raise or division by zero.

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::{env_from_datacon_table, eval, Value, VecHeap};
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};
use tidepool_testing::proptest::build_table_for_expr;

/// `case (Con tag=0 [raise# 5]) of { DataAlt(tag=0) y -> 42 }`
fn build_tree() -> CoreExpr {
    build_partial_tree(PrimOpKind::Raise, false)
}

fn build_partial_tree(op: PrimOpKind, demand: bool) -> CoreExpr {
    let tag = DataConId(0);
    let field_binder = VarId(1);
    let case_binder = VarId(2);

    let mut b = TreeBuilder::new();
    let raise_arg = b.push(CoreFrame::Lit(Literal::LitInt(5)));
    let zero = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let raise_field = b.push(CoreFrame::PrimOp {
        op,
        args: if op == PrimOpKind::Raise {
            vec![raise_arg]
        } else {
            vec![raise_arg, zero]
        },
    });
    let scrutinee = b.push(CoreFrame::Con {
        tag,
        fields: vec![raise_field],
    });
    let field_ref = b.push(CoreFrame::Var(field_binder));
    let alt_body = if demand {
        b.push(CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![field_ref, zero],
        })
    } else {
        b.push(CoreFrame::Lit(Literal::LitInt(42)))
    };
    b.push(CoreFrame::Case {
        scrutinee,
        binder: case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(tag),
            binders: vec![field_binder],
            body: alt_body,
        }],
    });
    b.build()
}

#[test]
fn eval_does_not_raise_when_con_field_is_unforced() {
    let expr = build_tree();
    let table = build_table_for_expr(&expr);
    let env = env_from_datacon_table(&table);
    let mut heap = VecHeap::new();
    let result = eval(&expr, &env, &mut heap);
    match result {
        Ok(Value::Lit(Literal::LitInt(n))) => assert_eq!(n, 42),
        other => {
            panic!("matching only the outer tag of Con(tag, [raise# 5]) must not raise: {other:?}")
        }
    }
}

#[test]
fn jit_does_not_raise_when_con_field_is_unforced() {
    let expr = build_tree();
    let table = build_table_for_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, 64 * 1024).expect("JIT compile");
    let result = machine.run_pure();
    match result {
        Ok(Value::Lit(Literal::LitInt(n))) => assert_eq!(n, 42),
        other => {
            panic!("matching only the outer tag of Con(tag, [raise# 5]) must not raise: {other:?}")
        }
    }
}

#[test]
fn partial_constructor_field_is_lazy_in_both_backends() {
    for demand in [false, true] {
        let expr = build_partial_tree(PrimOpKind::IntQuot, demand);
        let table = build_table_for_expr(&expr);
        let env = env_from_datacon_table(&table);
        let evaluated = eval(&expr, &env, &mut VecHeap::new());
        let mut machine = JitEffectMachine::compile(&expr, &table, 64 * 1024).expect("JIT compile");
        let jitted = machine.run_pure();
        if demand {
            assert!(evaluated.is_err(), "selected quotient must fail in eval");
            assert!(jitted.is_err(), "selected quotient must fail in JIT");
        } else {
            assert!(matches!(evaluated, Ok(Value::Lit(Literal::LitInt(42)))));
            assert!(matches!(jitted, Ok(Value::Lit(Literal::LitInt(42)))));
        }
    }
}
