//! Stress constructor fields and recursive closure captures under a tiny nursery.
//! Materialized fields must remain rooted until their constructor is allocated;
//! preallocated recursive objects must zero every pointer slot before any GC.

use tidepool_codegen::host_fns::{heap_verify_run_count, set_heap_verify};
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::value::Value;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};
use tidepool_testing::proptest::build_table_for_expr;

const I_HASH: DataConId = DataConId(7); // I# single-field Int box wrapper
const JUST: DataConId = DataConId(1); // Just single-field Con wrapper

thread_local! {
    static VAR_CTR: std::cell::Cell<u64> = const { std::cell::Cell::new(1000) };
}
fn fresh_var() -> VarId {
    VAR_CTR.with(|c| {
        let v = c.get();
        c.set(v + 1);
        VarId(v)
    })
}
fn reset_ctr() {
    VAR_CTR.with(|c| c.set(1000));
}

fn fixup_root(tree: &mut CoreExpr, root: usize) -> CoreExpr {
    if root == tree.nodes.len() - 1 {
        return tree.clone();
    }
    let binder = fresh_var();
    let var_idx = tree.nodes.len();
    tree.nodes.push(CoreFrame::Var(binder));
    tree.nodes.push(CoreFrame::LetNonRec {
        binder,
        rhs: root,
        body: var_idx,
    });
    tree.clone()
}

/// A small self-recursive strict sum `sumTo n = go 0 n`, forced to WHNF as
/// the (non-trivial, thunkified) field of a `Just`. Returns the root index of
/// `letrec go = ... in go 0 <n>`.
fn push_strict_sum(b: &mut TreeBuilder, n: i64) -> usize {
    let go = fresh_var();
    let acc = fresh_var();
    let i = fresh_var();

    let iv = b.push(CoreFrame::Var(i));
    let lim = b.push(CoreFrame::Lit(Literal::LitInt(n)));
    let cond = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntGt,
        args: vec![iv, lim],
    });

    let done = b.push(CoreFrame::Var(acc));

    let av = b.push(CoreFrame::Var(acc));
    let iv2 = b.push(CoreFrame::Var(i));
    let new_acc = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![av, iv2],
    });
    let iv3 = b.push(CoreFrame::Var(i));
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let new_i = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![iv3, one],
    });
    let go_v = b.push(CoreFrame::Var(go));
    let app1 = b.push(CoreFrame::App {
        fun: go_v,
        arg: new_acc,
    });
    let recur = b.push(CoreFrame::App {
        fun: app1,
        arg: new_i,
    });

    let case_binder = fresh_var();
    let body_case = b.push(CoreFrame::Case {
        scrutinee: cond,
        binder: case_binder,
        alts: vec![
            Alt {
                con: AltCon::LitAlt(Literal::LitInt(1)),
                binders: vec![],
                body: done,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: recur,
            },
        ],
    });

    let inner_lam = b.push(CoreFrame::Lam {
        binder: i,
        body: body_case,
    });
    let go_lam = b.push(CoreFrame::Lam {
        binder: acc,
        body: inner_lam,
    });

    let go_b = b.push(CoreFrame::Var(go));
    let zero1 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let call1 = b.push(CoreFrame::App {
        fun: go_b,
        arg: zero1,
    });
    let zero2 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let call2 = b.push(CoreFrame::App {
        fun: call1,
        arg: zero2,
    });

    b.push(CoreFrame::LetRec {
        bindings: vec![(go, go_lam)],
        body: call2,
    })
}

/// `let j0 = Just (sumTo n0) in let j1 = Just (sumTo n1) in ... in`
/// `case j0 of Just x0 -> case x0 of I# v0 -> case j1 of Just x1 -> ... -> v0+v1+...`
///
/// Each `Just (sumTo n)` keeps its computed field lazy. Repeated allocation
/// and forcing exercises roots across constructor and thunk allocation.
fn build_many_lazy_justs(count: usize, n: i64) -> (CoreExpr, i64) {
    reset_ctr();
    let mut b = TreeBuilder::new();

    let mut binders = Vec::with_capacity(count);
    for k in 0..count {
        let sum_root = push_strict_sum(&mut b, n + (k as i64 % 7));
        let just_rhs = b.push(CoreFrame::Con {
            tag: JUST,
            fields: vec![sum_root],
        });
        binders.push((fresh_var(), just_rhs));
    }

    // Unwrap every Just/I# and add up the sums, nested innermost-first so the
    // outermost binder (j0) is evaluated (forced) first.
    let mut acc: Option<usize> = None;
    for &(binder, _) in &binders {
        let jv = b.push(CoreFrame::Var(binder));
        let x = fresh_var();
        let x_v = b.push(CoreFrame::Var(x));
        let vbind = fresh_var();
        let v_v = b.push(CoreFrame::Var(vbind));
        let unwrap_i = b.push(CoreFrame::Case {
            scrutinee: x_v,
            binder: fresh_var(),
            alts: vec![Alt {
                con: AltCon::DataAlt(I_HASH),
                binders: vec![vbind],
                body: v_v,
            }],
        });
        let combined = match acc {
            None => unwrap_i,
            Some(prev) => {
                let prev_v = prev;
                b.push(CoreFrame::PrimOp {
                    op: PrimOpKind::IntAdd,
                    args: vec![unwrap_i, prev_v],
                })
            }
        };
        let unwrap_just = b.push(CoreFrame::Case {
            scrutinee: jv,
            binder: fresh_var(),
            alts: vec![Alt {
                con: AltCon::DataAlt(JUST),
                binders: vec![x],
                body: combined,
            }],
        });
        acc = Some(unwrap_just);
    }
    let sum_expr = acc.expect("count > 0");

    let mut body = sum_expr;
    for &(binder, rhs) in binders.iter().rev() {
        body = b.push(CoreFrame::LetNonRec { binder, rhs, body });
    }

    let expected: i64 = (0..count as i64)
        .map(|k| {
            let m = n + (k % 7);
            (0..=m).sum::<i64>()
        })
        .sum();

    let mut tree = b.build();
    (fixup_root(&mut tree, body), expected)
}

#[test]
fn lazy_constructor_fields_survive_tiny_nursery() {
    set_heap_verify(true);
    let before = heap_verify_run_count();

    let (expr, expected) = build_many_lazy_justs(40, 60);
    let table = build_table_for_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, 512)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    let result = machine
        .run_pure()
        .unwrap_or_else(|e| panic!("run failed: {e:?}"));

    match result {
        Value::Lit(Literal::LitInt(n)) => {
            assert_eq!(
                n, expected,
                "Just (sumTo n) result corrupted (Finding 1 mid-fill GC)"
            );
        }
        other => panic!("expected LitInt, got {other:?}"),
    }

    let after = heap_verify_run_count();
    assert!(
        after > before,
        "heap_verify_run_count did not increase ({before} -> {after}) — \
         the verifier never ran, so this test guarded nothing"
    );
}
