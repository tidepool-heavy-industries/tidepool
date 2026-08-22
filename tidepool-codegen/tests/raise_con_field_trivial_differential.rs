//! Differential fixture for the "trivial constructor field" predicate drift
//! (duplication survey finding 5): a `Con` field that is `raise#` applied to
//! a trivial (already-WHNF) argument.
//!
//! The JIT's `is_trivial_field` (`tidepool-codegen/src/emit/expr.rs`)
//! explicitly classifies `PrimOpKind::Raise` as non-trivial regardless of its
//! arguments, so `Con(tag, [raise# 5])` thunks the field and never raises
//! just from constructing the value — matching GHC Core's non-strict `let`/
//! constructor-application semantics (`Just (error "boom")` does not raise
//! until the `Just` is pattern-matched OPEN, i.e. the field itself is
//! demanded, not merely the outer tag).
//!
//! The oracle's copy of the predicate (`tidepool-eval/src/eval.rs`) instead
//! classified every `PrimOp` by its arguments alone, so a `Raise` with a
//! trivial argument was misclassified trivial and evaluated EAGERLY while
//! merely constructing the `Con` — raising before anything ever forced the
//! field.
//!
//! `case (Con tag [raise# 5]) of { tag y -> 42 }` matches only the outer tag
//! (WHNF of the scrutinee), never touching the field `y` — so the correct
//! answer is `42`. A top-level `run_pure`/`eval` return would deep-force the
//! *whole* result including any unforced field (that's `heap_to_value_forcing`
//! doing its normal job of crossing the JIT/Rust boundary as concrete data),
//! which would mask this bug by forcing the field anyway — so the outer
//! `Case` here is load-bearing: it is what lets a correct implementation
//! throw the field away unforced.

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::{env_from_datacon_table, eval, Value, VecHeap};
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};
use tidepool_testing::proptest::build_table_for_expr;

/// `case (Con tag=0 [raise# 5]) of { DataAlt(tag=0) y -> 42 }`
fn build_tree() -> CoreExpr {
    let tag = DataConId(0);
    let field_binder = VarId(1);
    let case_binder = VarId(2);

    let mut b = TreeBuilder::new();
    let raise_arg = b.push(CoreFrame::Lit(Literal::LitInt(5)));
    let raise_field = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::Raise,
        args: vec![raise_arg],
    });
    let scrutinee = b.push(CoreFrame::Con {
        tag,
        fields: vec![raise_field],
    });
    let alt_body = b.push(CoreFrame::Lit(Literal::LitInt(42)));
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
        other => panic!(
            "matching only the outer tag of Con(tag, [raise# 5]) must not raise: {other:?}"
        ),
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
        other => panic!(
            "matching only the outer tag of Con(tag, [raise# 5]) must not raise: {other:?}"
        ),
    }
}
