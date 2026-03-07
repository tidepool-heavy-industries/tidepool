//! Tail-call optimization tests.
//!
//! These tests verify that tail-position function applications in lambda bodies
//! use the TCO path (VMContext store + null return → trampoline resolution)
//! instead of `call_indirect`, enabling deep recursion without stack overflow.

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::value::Value;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, Literal, TreeBuilder};

fn assert_lit_int(val: &Value, expected: i64) {
    match val {
        Value::Lit(Literal::LitInt(n)) => assert_eq!(*n, expected),
        other => panic!("expected Lit(Int({})), got {:?}", expected, other),
    }
}

fn empty_table() -> DataConTable {
    DataConTable::new()
}

/// Build: `let go = \n -> case n ==# 0# of { 1# -> Lit(result); _ -> go (n -# 1#) } in go N`
///
/// This is a tail-recursive countdown. Without TCO, N=100000 overflows the stack.
fn build_tail_recursive_countdown(n: i64, result: i64) -> CoreExpr {
    let go = VarId(1);
    let param_n = VarId(2);
    let case_binder = VarId(3);

    let mut bld = TreeBuilder::new();

    // Leaf nodes used in the lambda body:
    // 0: Var(param_n) — the parameter
    let var_n = bld.push(CoreFrame::Var(param_n));
    // 1: Lit(0) — comparison target
    let lit_0 = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
    // 2: param_n ==# 0# → produces 1# (true) or 0# (false)
    let cmp = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntEq,
        args: vec![var_n, lit_0],
    });

    // True branch: return result literal
    let lit_result = bld.push(CoreFrame::Lit(Literal::LitInt(result)));

    // False branch: go (n -# 1#)
    let var_n2 = bld.push(CoreFrame::Var(param_n));
    let lit_1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
    let sub = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntSub,
        args: vec![var_n2, lit_1],
    });
    let var_go = bld.push(CoreFrame::Var(go));
    let tail_call = bld.push(CoreFrame::App {
        fun: var_go,
        arg: sub,
    });

    // Case on the comparison result (unboxed Int#)
    let case_node = bld.push(CoreFrame::Case {
        scrutinee: cmp,
        binder: case_binder,
        alts: vec![
            Alt {
                con: AltCon::LitAlt(Literal::LitInt(1)),
                binders: vec![],
                body: lit_result,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: tail_call,
            },
        ],
    });

    // Lambda: \n -> case ...
    let lam = bld.push(CoreFrame::Lam {
        binder: param_n,
        body: case_node,
    });

    // Application: go N
    let lit_n = bld.push(CoreFrame::Lit(Literal::LitInt(n)));
    let var_go2 = bld.push(CoreFrame::Var(go));
    let app = bld.push(CoreFrame::App {
        fun: var_go2,
        arg: lit_n,
    });

    // LetRec: let go = \n -> ... in go N
    bld.push(CoreFrame::LetRec {
        bindings: vec![(go, lam)],
        body: app,
    });

    bld.build()
}

/// Deep self-recursion: `go 100000` should complete without stack overflow.
/// Uses default thread stack size so this would overflow without TCO.
#[test]
fn test_tco_deep_self_recursion() {
    std::thread::spawn(|| {
        let expr = build_tail_recursive_countdown(100_000, 42);
        let table = empty_table();
        let mut machine =
            JitEffectMachine::compile(&expr, &table, 1 << 20).unwrap();
        let result = machine.run_pure().unwrap();
        assert_lit_int(&result, 42);
    })
    .join()
    .unwrap();
}

/// Moderate recursion to verify basic correctness with a smaller depth.
#[test]
fn test_tco_moderate_recursion() {
    let expr = build_tail_recursive_countdown(1000, 99);
    let table = empty_table();
    let mut machine = JitEffectMachine::compile(&expr, &table, 1 << 20).unwrap();
    let result = machine.run_pure().unwrap();
    assert_lit_int(&result, 99);
}

/// Non-tail call to a function that internally does tail calls.
/// `let go = \n -> case n ==# 0# of { 1# -> 7; _ -> go (n -# 1#) } in go 50 +# go 50`
#[test]
fn test_tco_non_tail_calling_tail_function() {
    let go = VarId(1);
    let param_n = VarId(2);
    let case_binder = VarId(3);

    let mut bld = TreeBuilder::new();

    // Lambda body: case n ==# 0# of { 1# -> 7; _ -> go (n-1) }
    let var_n = bld.push(CoreFrame::Var(param_n));
    let lit_0 = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
    let cmp = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntEq,
        args: vec![var_n, lit_0],
    });
    let lit_7 = bld.push(CoreFrame::Lit(Literal::LitInt(7)));
    let var_n2 = bld.push(CoreFrame::Var(param_n));
    let lit_1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
    let sub = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntSub,
        args: vec![var_n2, lit_1],
    });
    let var_go = bld.push(CoreFrame::Var(go));
    let tail_call = bld.push(CoreFrame::App {
        fun: var_go,
        arg: sub,
    });
    let case_node = bld.push(CoreFrame::Case {
        scrutinee: cmp,
        binder: case_binder,
        alts: vec![
            Alt {
                con: AltCon::LitAlt(Literal::LitInt(1)),
                binders: vec![],
                body: lit_7,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: tail_call,
            },
        ],
    });
    let lam = bld.push(CoreFrame::Lam {
        binder: param_n,
        body: case_node,
    });

    // Body: go 50 +# go 50
    let lit_50a = bld.push(CoreFrame::Lit(Literal::LitInt(50)));
    let var_go_a = bld.push(CoreFrame::Var(go));
    let app_a = bld.push(CoreFrame::App {
        fun: var_go_a,
        arg: lit_50a,
    });
    let lit_50b = bld.push(CoreFrame::Lit(Literal::LitInt(50)));
    let var_go_b = bld.push(CoreFrame::Var(go));
    let app_b = bld.push(CoreFrame::App {
        fun: var_go_b,
        arg: lit_50b,
    });
    let add = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![app_a, app_b],
    });

    bld.push(CoreFrame::LetRec {
        bindings: vec![(go, lam)],
        body: add,
    });

    let expr = bld.build();
    let table = empty_table();
    let mut machine = JitEffectMachine::compile(&expr, &table, 1 << 20).unwrap();
    let result = machine.run_pure().unwrap();
    // go 50 returns 7, so 7 + 7 = 14
    assert_lit_int(&result, 14);
}
