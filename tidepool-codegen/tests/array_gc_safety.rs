//! Finding 2 (repo-review-2026-07-06/01-gc-memory-safety.md): boxed
//! `SmallArray#` element pointers were invisible to `for_each_pointer_field`
//! (`TAG_LIT` fell into the `_ => {}` catch-all), so a GC that landed after a
//! `newSmallArray#` with a heap-pointer element — but before anything else kept
//! that element reachable — collected it as garbage. The next
//! `readSmallArray#`/`indexSmallArray#` then dereferenced freed nursery memory.
//!
//! This is a RED-FIRST test per the plan's anti-patterns section: the existing
//! `proptest_host_arrays.rs` suite explicitly models boxed slots as opaque
//! tokens the host never dereferences (see that file's module doc), so it
//! structurally cannot catch this. This test drives `newSmallArray#` /
//! `indexSmallArray#` through the real CoreExpr -> JIT path with a REAL heap
//! `Con` element and a tiny nursery, forcing many collections between the
//! array's creation and its read while the stored element's only remaining
//! reference is the array slot itself (the binding that built the element is
//! never referenced again, so nothing else roots it).
//!
//! Before the fix: `for_each_pointer_field`'s `TAG_LIT` case did nothing, so
//! the element `Con` was never evacuated, the nursery buffer holding it was
//! freed on the next GC's buffer swap (`host_fns/gc.rs` `perform_gc`), and the
//! final `indexSmallArray#` read dereferenced freed memory (SIGSEGV or garbage
//! under ASAN-free conditions here, since Rust's allocator doesn't unmap).
//! After the fix, `for_each_pointer_field`'s new `TAG_LIT` arm walks the
//! array's payload slots, so the element is evacuated with everything else and
//! the read is intact.

use tidepool_codegen::host_fns::{heap_verify_run_count, set_heap_verify};
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::value::Value;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};
use tidepool_testing::proptest::build_table_for_expr;

// Matches the standard table used across tidepool-codegen's hand-built-tree
// tests (see heap_verify_lane.rs / proptest_gc_recursion.rs).
const I_HASH: DataConId = DataConId(7); // I# single-field Int box wrapper
const JUST: DataConId = DataConId(1);

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

/// Build:
///
/// ```text
/// let x   = I# 999                              -- heap Con, referenced ONLY below
/// in let arr = newSmallArray# 1# x               -- array's only ref to x
///    in letrec go = \acc -> \i ->
///         case (i ># LIMIT) of
///           1# -> case indexSmallArray# arr 0# of I# n -> n + acc   -- read AFTER the loop
///           _  -> let box  = I# (acc +# i)
///                 in let junk = Just (Just (I# i))    -- pure allocation pressure
///                    in case box of I# n -> go n (i +# 1)
///       in go 0 0
/// ```
///
/// `x` is never referenced again after the `newSmallArray#` call — the array
/// slot is its ONLY remaining reference — while `arr` stays live (captured by
/// `go`'s closure) across `limit` allocating loop iterations under a tiny
/// nursery, forcing several collections between the array's creation and its
/// final read.
fn build_array_survives_gc(limit: i64) -> (CoreExpr, i64) {
    reset_ctr();
    let expected_sum: i64 = (0..=limit).sum();
    let expected = expected_sum + 999;

    let mut b = TreeBuilder::new();

    let x = fresh_var();
    let lit999 = b.push(CoreFrame::Lit(Literal::LitInt(999)));
    let x_con = b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![lit999],
    });

    let arr = fresh_var();
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let x_var_for_arr = b.push(CoreFrame::Var(x));
    let arr_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::NewSmallArray,
        args: vec![one, x_var_for_arr],
    });

    let go = fresh_var();
    let acc = fresh_var();
    let i = fresh_var();

    let iv = b.push(CoreFrame::Var(i));
    let lim = b.push(CoreFrame::Lit(Literal::LitInt(limit)));
    let cond = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntGt,
        args: vec![iv, lim],
    });

    // Terminal: case indexSmallArray# arr 0# of I# n -> n + acc
    let arr_v_term = b.push(CoreFrame::Var(arr));
    let zero_idx = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let idx_read = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IndexSmallArray,
        args: vec![arr_v_term, zero_idx],
    });
    let n = fresh_var();
    let n_v = b.push(CoreFrame::Var(n));
    let acc_v_term = b.push(CoreFrame::Var(acc));
    let sum_final = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![n_v, acc_v_term],
    });
    let extract_binder = fresh_var();
    let done = b.push(CoreFrame::Case {
        scrutinee: idx_read,
        binder: extract_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(I_HASH),
            binders: vec![n],
            body: sum_final,
        }],
    });

    // Non-terminal: accumulate + allocate garbage + recurse.
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

    let iv3 = b.push(CoreFrame::Var(i));
    let inner = b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![iv3],
    });
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
    let nn = fresh_var();
    let nn_v = b.push(CoreFrame::Var(nn));
    let iv4 = b.push(CoreFrame::Var(i));
    let one_lit = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let new_i = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![iv4, one_lit],
    });
    let go_v = b.push(CoreFrame::Var(go));
    let app1 = b.push(CoreFrame::App {
        fun: go_v,
        arg: nn_v,
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
            binders: vec![nn],
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
    let zero2 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let call2 = b.push(CoreFrame::App {
        fun: call1,
        arg: zero2,
    });

    let letrec = b.push(CoreFrame::LetRec {
        bindings: vec![(go, go_lam)],
        body: call2,
    });

    let let_arr = b.push(CoreFrame::LetNonRec {
        binder: arr,
        rhs: arr_rhs,
        body: letrec,
    });
    let let_x = b.push(CoreFrame::LetNonRec {
        binder: x,
        rhs: x_con,
        body: let_arr,
    });

    let mut tree = b.build();
    (fixup_root(&mut tree, let_x), expected)
}

#[test]
fn small_array_element_survives_gc_under_tiny_nursery() {
    set_heap_verify(true);
    let before = heap_verify_run_count();

    let (expr, expected) = build_array_survives_gc(2000);
    let table = build_table_for_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, 2 * 1024)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    let result = machine
        .run_pure()
        .unwrap_or_else(|e| panic!("run failed: {e:?}"));

    match result {
        Value::Lit(Literal::LitInt(n)) => {
            assert_eq!(
                n, expected,
                "SmallArray# element did not survive GC intact (Finding 2 UAF)"
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
