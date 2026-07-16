//! Default-tier lane: makes the post-GC heap invariant verifier
//! (`verify_heap_post_gc`, `tidepool-codegen/src/host_fns/gc.rs`) actually run.
//!
//! Before this lane, nothing in the repo ever set `TIDEPOOL_HEAP_VERIFY=1` —
//! `grep -rn TIDEPOOL_HEAP_VERIFY` found only comments — so the verifier never
//! guarded a single regression. This lane force-enables it via
//! `host_fns::set_heap_verify` (a process-global atomic override; `env::set_var`
//! would race the `OnceLock`-cached env read `heap_verify_enabled` uses, and is
//! unsafe on edition 2024) and, via `host_fns::heap_verify_run_count`, asserts
//! the verifier actually fired rather than silently no-op'ing.
//!
//! Programs mirror the allocation shapes from `proptest_gc_recursion.rs` (a
//! long cons-spine, many simultaneously-live bindings, one big nested
//! constructor, an allocate-every-iteration loop) but are FIXED and small, run
//! once each at a tiny nursery, and checked against a hand-computed expected
//! value. GC *correctness* (the JIT-vs-eval differential across a nursery
//! ladder) is `proptest_gc_recursion`'s job; this lane only needs the verifier
//! to see real GC-forcing traffic and stay silent on a healthy heap. A verifier
//! violation surfaces as a panic, which is a hard test failure — that's the
//! whole point.

use tidepool_codegen::host_fns::{heap_verify_run_count, set_heap_verify};
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::value::Value;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};
use tidepool_testing::proptest::build_table_for_expr;

#[path = "support/gc_scaffold.rs"]
mod gc_scaffold;
use gc_scaffold::{fixup_root, fresh_var, push_pairtree, push_pairtree_sum, push_spine, reset_ctr};
use gc_scaffold::{CONS, I_HASH, NIL, PAIR};

/// Tail-recursive sum fold over a cons-spine, expressed as a self-recursive
/// `LetRec` lambda (JIT tail-call-optimizes it, PR #154):
///   letrec go = \acc -> \xs -> case xs of { [] -> acc ; (h:t) -> go (acc+h) t }
///   in go 0 <list_var>
/// Returns the root index (a `LetRec`).
fn push_sum_fold(b: &mut TreeBuilder, list_var: VarId) -> usize {
    let go = fresh_var();
    let acc = fresh_var();
    let xs = fresh_var();

    let xs_v = b.push(CoreFrame::Var(xs));
    let case_binder = fresh_var();
    let h = fresh_var();
    let t = fresh_var();

    let nil_body = b.push(CoreFrame::Var(acc));

    let av = b.push(CoreFrame::Var(acc));
    let hv = b.push(CoreFrame::Var(h));
    let combined = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![av, hv],
    });
    let go_v = b.push(CoreFrame::Var(go));
    let app1 = b.push(CoreFrame::App {
        fun: go_v,
        arg: combined,
    });
    let t_v = b.push(CoreFrame::Var(t));
    let recur = b.push(CoreFrame::App {
        fun: app1,
        arg: t_v,
    });

    let case_node = b.push(CoreFrame::Case {
        scrutinee: xs_v,
        binder: case_binder,
        alts: vec![
            Alt {
                con: AltCon::DataAlt(NIL),
                binders: vec![],
                body: nil_body,
            },
            Alt {
                con: AltCon::DataAlt(CONS),
                binders: vec![h, t],
                body: recur,
            },
        ],
    });

    let inner_lam = b.push(CoreFrame::Lam {
        binder: xs,
        body: case_node,
    });
    let go_lam = b.push(CoreFrame::Lam {
        binder: acc,
        body: inner_lam,
    });

    let go_b = b.push(CoreFrame::Var(go));
    let seed = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let call1 = b.push(CoreFrame::App {
        fun: go_b,
        arg: seed,
    });
    let list_v = b.push(CoreFrame::Var(list_var));
    let call2 = b.push(CoreFrame::App {
        fun: call1,
        arg: list_v,
    });

    b.push(CoreFrame::LetRec {
        bindings: vec![(go, go_lam)],
        body: call2,
    })
}

/// (a) Long cons-spine, tail-recursively summed. Forces repeated forwarding of
/// a single long live chain as the tiny nursery collects mid-walk.
fn build_cons_spine_sum(n: i64) -> (CoreExpr, i64) {
    reset_ctr();
    let elems: Vec<i64> = (0..n).collect();
    let expected = elems.iter().sum();

    let mut b = TreeBuilder::new();
    let spine = push_spine(&mut b, &elems);
    let lst = fresh_var();
    let fold = push_sum_fold(&mut b, lst);
    let root = b.push(CoreFrame::LetNonRec {
        binder: lst,
        rhs: spine,
        body: fold,
    });
    let mut tree = b.build();
    (fixup_root(&mut tree, root), expected)
}

/// (b) N simultaneously-live boxed/pair bindings, all folded into one sum.
/// Built as a right-nested chain of `LetNonRec`, so every earlier binding is a
/// live root while later ones are still being allocated.
fn build_wide_live(n: usize) -> (CoreExpr, i64) {
    reset_ctr();
    // Alternate Boxed(i) / Pair(i, i+1) cells.
    let mut expected: i64 = 0;
    let mut b = TreeBuilder::new();
    let binders: Vec<VarId> = (0..n).map(|_| fresh_var()).collect();

    let mut acc = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    for (i, &binder) in binders.iter().enumerate() {
        let bv = b.push(CoreFrame::Var(binder));
        let case_binder = fresh_var();
        if i % 2 == 0 {
            let v = i as i64;
            expected += v;
            let fld = fresh_var();
            let fld_v = b.push(CoreFrame::Var(fld));
            let extracted = b.push(CoreFrame::Case {
                scrutinee: bv,
                binder: case_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(I_HASH),
                    binders: vec![fld],
                    body: fld_v,
                }],
            });
            acc = b.push(CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![acc, extracted],
            });
        } else {
            let (a, c) = (i as i64, i as i64 + 1);
            expected += a + c;
            let f1 = fresh_var();
            let f2 = fresh_var();
            let f1_v = b.push(CoreFrame::Var(f1));
            let f2_v = b.push(CoreFrame::Var(f2));
            let sum = b.push(CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![f1_v, f2_v],
            });
            let extracted = b.push(CoreFrame::Case {
                scrutinee: bv,
                binder: case_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(PAIR),
                    binders: vec![f1, f2],
                    body: sum,
                }],
            });
            acc = b.push(CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![acc, extracted],
            });
        }
    }

    // Wrap LetNonRec bindings around `acc`, last cell inward, so the first
    // cell's let is outermost (and therefore the root).
    let mut body = acc;
    for i in (0..n).rev() {
        let rhs = if i % 2 == 0 {
            let lit = b.push(CoreFrame::Lit(Literal::LitInt(i as i64)));
            b.push(CoreFrame::Con {
                tag: I_HASH,
                fields: vec![lit],
            })
        } else {
            let la = b.push(CoreFrame::Lit(Literal::LitInt(i as i64)));
            let lc = b.push(CoreFrame::Lit(Literal::LitInt(i as i64 + 1)));
            b.push(CoreFrame::Con {
                tag: PAIR,
                fields: vec![la, lc],
            })
        };
        body = b.push(CoreFrame::LetNonRec {
            binder: binders[i],
            rhs,
            body,
        });
    }

    let mut tree = b.build();
    (fixup_root(&mut tree, body), expected)
}

/// (c) One large nested constructor (a balanced Pair-tree) returned WHOLE
/// before being summed, so the entire object is the live root through a GC
/// firing mid- or post-construction.
fn build_big_con(depth: u32) -> (CoreExpr, i64) {
    reset_ctr();
    let n = 1usize << depth;
    let leaves: Vec<i64> = (0..n as i64).collect();
    let expected = leaves.iter().sum();

    let mut b = TreeBuilder::new();
    let tree_root = push_pairtree(&mut b, depth, &leaves);
    let bound = fresh_var();
    let bound_v = b.push(CoreFrame::Var(bound));
    let sum = push_pairtree_sum(&mut b, depth, bound_v);
    let root = b.push(CoreFrame::LetNonRec {
        binder: bound,
        rhs: tree_root,
        body: sum,
    });
    let mut tree = b.build();
    (fixup_root(&mut tree, root), expected)
}

/// (d) Self-recursive accumulate loop that allocates a fresh boxed Con every
/// iteration (immediate garbage) plus deeper `Just`-nested junk, while
/// threading one live accumulator:
///   letrec go = \acc -> \i ->
///     case (i ># LIMIT) of
///       1# -> acc
///       _  -> let box  = I# (acc +# i)
///             in let junk = Just (Just (I# i))   -- never used
///                in case box of I# n -> go n (i +# 1)
///   in go 0 0
fn build_accum_loop(limit: i64) -> (CoreExpr, i64) {
    reset_ctr();
    let expected = (0..=limit).sum();

    let mut b = TreeBuilder::new();
    let go = fresh_var();
    let acc = fresh_var();
    let i = fresh_var();

    let iv = b.push(CoreFrame::Var(i));
    let lim = b.push(CoreFrame::Lit(Literal::LitInt(limit)));
    let cond = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntGt,
        args: vec![iv, lim],
    });

    let done = b.push(CoreFrame::Var(acc));

    let av = b.push(CoreFrame::Var(acc));
    let iv2 = b.push(CoreFrame::Var(i));
    let new_val = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![av, iv2],
    });
    let box_rhs = b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![new_val],
    });
    let box_binder = fresh_var();

    // junk: Just (Just (I# i)) — allocated, never inspected.
    let iv3 = b.push(CoreFrame::Var(i));
    let inner = b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![iv3],
    });
    const JUST: DataConId = DataConId(1);
    let just1 = b.push(CoreFrame::Con {
        tag: JUST,
        fields: vec![inner],
    });
    let junk = b.push(CoreFrame::Con {
        tag: JUST,
        fields: vec![just1],
    });
    let junk_binder = fresh_var();

    let box_v = b.push(CoreFrame::Var(box_binder));
    let case_binder = fresh_var();
    let n = fresh_var();
    let n_v = b.push(CoreFrame::Var(n));
    let iv4 = b.push(CoreFrame::Var(i));
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let new_i = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![iv4, one],
    });
    let go_v = b.push(CoreFrame::Var(go));
    let app1 = b.push(CoreFrame::App {
        fun: go_v,
        arg: n_v,
    });
    let recur_call = b.push(CoreFrame::App {
        fun: app1,
        arg: new_i,
    });
    let unbox_case = b.push(CoreFrame::Case {
        scrutinee: box_v,
        binder: case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(I_HASH),
            binders: vec![n],
            body: recur_call,
        }],
    });
    let let_junk = b.push(CoreFrame::LetNonRec {
        binder: junk_binder,
        rhs: junk,
        body: unbox_case,
    });
    let recur = b.push(CoreFrame::LetNonRec {
        binder: box_binder,
        rhs: box_rhs,
        body: let_junk,
    });

    let case_binder2 = fresh_var();
    let body_case = b.push(CoreFrame::Case {
        scrutinee: cond,
        binder: case_binder2,
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
    let start = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let call1 = b.push(CoreFrame::App {
        fun: go_b,
        arg: start,
    });
    let zero = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let call2 = b.push(CoreFrame::App {
        fun: call1,
        arg: zero,
    });

    let root = b.push(CoreFrame::LetRec {
        bindings: vec![(go, go_lam)],
        body: call2,
    });
    let mut tree = b.build();
    (fixup_root(&mut tree, root), expected)
}

/// Run `expr` under a tiny nursery with the heap verifier forced on, and
/// assert both the correct result AND that the verifier actually fired at
/// least once more than before this call.
fn run_verified(expr: CoreExpr, nursery_size: usize, expected: i64, label: &str) {
    set_heap_verify(true);
    let before = heap_verify_run_count();

    let table = build_table_for_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, nursery_size).unwrap_or_else(|e| {
        panic!("{label}: compile failed: {e:?}");
    });
    let result = machine
        .run_pure()
        .unwrap_or_else(|e| panic!("{label}: run failed: {e:?}"));

    match result {
        Value::Lit(Literal::LitInt(n)) => {
            assert_eq!(n, expected, "{label}: wrong result");
        }
        other => panic!("{label}: expected LitInt, got {other:?}"),
    }

    let after = heap_verify_run_count();
    assert!(
        after > before,
        "{label}: heap_verify_run_count did not increase ({before} -> {after}) \
         — the verifier never ran, so this lane guarded nothing"
    );
}

#[test]
fn heap_verify_fires_on_cons_spine_sum() {
    let (expr, expected) = build_cons_spine_sum(300);
    run_verified(expr, 2 * 1024, expected, "cons_spine_sum");
}

#[test]
fn heap_verify_fires_on_wide_live() {
    let (expr, expected) = build_wide_live(24);
    run_verified(expr, 1024, expected, "wide_live");
}

#[test]
fn heap_verify_fires_on_big_con() {
    let (expr, expected) = build_big_con(5);
    run_verified(expr, 2 * 1024, expected, "big_con");
}

#[test]
fn heap_verify_fires_on_accum_loop() {
    let (expr, expected) = build_accum_loop(600);
    run_verified(expr, 4 * 1024, expected, "accum_loop");
}
