//! Differential coverage for RECURSIVE join points (`joinrec`).
//!
//! GHC emits recursive join points constantly under -O2 (loops compile to
//! them). The tree-walking interpreter (`tidepool-eval`) historically could not
//! evaluate a recursive self-`Jump` — the join label was not in scope inside its
//! own rhs, so the second jump died with `UnboundJoin`. Because eval errored,
//! the differential oracle `check_jit_vs_eval` routed every recursive-join case
//! to its skip arm: a whole common class of Core silently escaped differential
//! testing.
//!
//! With the eval knot-tying fix in place these cases now evaluate, so this file
//! pins the recursive-join class to the JIT differentially (eval == JIT) AND
//! against the hand-computed arithmetic answer.
//!
//! Construction style mirrors `proptest_ghc_idioms.rs`: hand-built
//! `RecursiveTree<CoreFrame<usize>>` IR, total and ground by construction.

use tidepool_repr::types::{Alt, AltCon, JoinId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::{env_from_datacon_table, eval, VecHeap};
use tidepool_testing::proptest::{build_table_for_expr, check_jit_vs_eval};

/// Build a recursive-join counting loop with `n_lead` extra leading params
/// shaped like GHC type-args (always passed `0`, never used):
///
/// ```text
/// join go (lead..., acc, i) = case (i ># limit) of
///                               1# -> acc
///                               _  -> jump go (lead..., acc +# i, i +# 1)
/// in jump go (0..., start_acc, 0)
/// ```
///
/// Evaluates to `start_acc + sum [0..limit]`.
fn build_sum_joinrec(limit: i64, n_lead: usize, start_acc: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let mut next_var: u64 = 1000;
    let mut fresh = || {
        let v = VarId(next_var);
        next_var += 1;
        v
    };

    let go = JoinId(0);
    let leads: Vec<VarId> = (0..n_lead).map(|_| fresh()).collect();
    let acc = fresh();
    let i = fresh();
    let mut params = leads.clone();
    params.push(acc);
    params.push(i);

    // rhs: case (i ># limit) of { 1# -> acc ; _ -> jump go (leads..., acc+#i, i+#1) }
    let iv = b.push(CoreFrame::Var(i));
    let lim = b.push(CoreFrame::Lit(Literal::LitInt(limit)));
    let cond = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntGt,
        args: vec![iv, lim],
    });
    let acc_done = b.push(CoreFrame::Var(acc));
    let acc_r = b.push(CoreFrame::Var(acc));
    let i_r = b.push(CoreFrame::Var(i));
    let new_acc = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![acc_r, i_r],
    });
    let i_r2 = b.push(CoreFrame::Var(i));
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let new_i = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![i_r2, one],
    });
    let mut jargs: Vec<usize> = leads.iter().map(|v| b.push(CoreFrame::Var(*v))).collect();
    jargs.push(new_acc);
    jargs.push(new_i);
    let recur = b.push(CoreFrame::Jump {
        label: go,
        args: jargs,
    });
    let cbind = fresh();
    let rhs = b.push(CoreFrame::Case {
        scrutinee: cond,
        binder: cbind,
        alts: vec![
            Alt {
                con: AltCon::LitAlt(Literal::LitInt(1)),
                binders: vec![],
                body: acc_done,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: recur,
            },
        ],
    });

    // body: jump go (0..., start_acc, 0)
    let mut init: Vec<usize> = leads
        .iter()
        .map(|_| b.push(CoreFrame::Lit(Literal::LitInt(0))))
        .collect();
    let sa = b.push(CoreFrame::Lit(Literal::LitInt(start_acc)));
    let zero = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    init.push(sa);
    init.push(zero);
    let body = b.push(CoreFrame::Jump {
        label: go,
        args: init,
    });

    // root must be the LAST node (eval / compile root at nodes.len() - 1).
    let _root = b.push(CoreFrame::Join {
        label: go,
        params,
        rhs,
        body,
    });
    b.build()
}

fn eval_int(expr: &CoreExpr) -> i64 {
    let table = build_table_for_expr(expr);
    let env = env_from_datacon_table(&table);
    let mut heap = VecHeap::new();
    let v = eval(expr, &env, &mut heap).expect("eval should resolve the recursive join");
    match v {
        tidepool_eval::Value::Lit(Literal::LitInt(n)) => n,
        other => panic!("expected LitInt, got {other:?}"),
    }
}

fn jit_int(expr: &CoreExpr) -> i64 {
    let table = build_table_for_expr(expr);
    let mut machine = JitEffectMachine::compile(expr, &table, 64 * 1024).expect("JIT compile");
    let v = machine.run_pure().expect("JIT run");
    match v {
        tidepool_eval::Value::Lit(Literal::LitInt(n)) => n,
        other => panic!("expected LitInt from JIT, got {other:?}"),
    }
}

#[test]
fn joinrec_eval_matches_arithmetic() {
    // sum [0..limit] = limit*(limit+1)/2
    for limit in [0i64, 1, 5, 100, 200] {
        let expr = build_sum_joinrec(limit, 0, 0);
        let expected = limit * (limit + 1) / 2;
        assert_eq!(eval_int(&expr), expected, "eval sum 0..{limit}");
    }
}

#[test]
fn joinrec_eval_equals_jit() {
    // Directly cross-check eval against the JIT (the differential oracle was
    // previously blind to this whole class).
    for limit in [0i64, 1, 5, 100, 200] {
        let expr = build_sum_joinrec(limit, 0, 7); // start_acc = 7
        let expected = 7 + limit * (limit + 1) / 2;
        let ev = eval_int(&expr);
        let jit = jit_int(&expr);
        assert_eq!(ev, expected, "eval start=7 sum 0..{limit}");
        assert_eq!(jit, expected, "jit start=7 sum 0..{limit}");
        assert_eq!(ev, jit, "eval/jit divergence at limit {limit}");
    }
}

#[test]
fn joinrec_with_type_arg_leads_eval_equals_jit() {
    // n_lead extra leading params shaped like type-args — stresses join arity
    // counting on both sides.
    for n_lead in 0..3usize {
        for limit in [0i64, 5, 100] {
            let expr = build_sum_joinrec(limit, n_lead, 0);
            let expected = limit * (limit + 1) / 2;
            assert_eq!(
                eval_int(&expr),
                expected,
                "eval n_lead={n_lead} limit={limit}"
            );
            assert_eq!(
                jit_int(&expr),
                expected,
                "jit n_lead={n_lead} limit={limit}"
            );
        }
    }
}

#[test]
fn joinrec_through_differential_oracle() {
    // Run the now-un-blinded class through the actual differential oracle used by
    // the proptest fuzzers. Pre-fix, eval errored and these cases were silently
    // skipped via the oracle's catch-all arm.
    for limit in [0i64, 1, 5, 100, 200] {
        let expr = build_sum_joinrec(limit, 1, 3);
        check_jit_vs_eval(expr.clone(), 64 * 1024).unwrap_or_else(|e| {
            panic!("differential oracle (64KB) failed at limit {limit}: {e:?}")
        });
        check_jit_vs_eval(expr, 4 * 1024)
            .unwrap_or_else(|e| panic!("differential oracle (4KB) failed at limit {limit}: {e:?}"));
    }
}
