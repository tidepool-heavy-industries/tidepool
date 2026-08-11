//! Behavioral coverage for the boxed `SmallArray#`/`Array#` primop family
//! (`emit/primop.rs:2179` onward). The JIT implements the whole family, but
//! `tidepool-eval`'s tree-walker has no boxed-array `Value` variant, so these
//! primops sit entirely outside the eval-vs-JIT differential oracle. Every
//! case here therefore asserts an explicit expected value against the real
//! JIT — never against an eval oracle — driven through the real CoreExpr ->
//! JIT path (hand-built `CoreExpr`, tiny nursery, filler allocations to
//! force collections).
//!
//! All cases run under `TIDEPOOL_GC_POISON`/`TIDEPOOL_HEAP_VERIFY` so a
//! dangling read is deterministic rather than sometimes-working.

use tidepool_codegen::host_fns::{heap_verify_run_count, set_gc_poison, set_heap_verify};
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_eval::value::Value;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};
use tidepool_testing::proptest::build_table_for_expr;

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

fn compile_and_run(expr: &CoreExpr, nursery: usize) -> Result<Value, JitError> {
    let table = build_table_for_expr(expr);
    let mut machine = JitEffectMachine::compile(expr, &table, nursery)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    machine.run_pure()
}

fn expect_int(expr: &CoreExpr, nursery: usize) -> i64 {
    match compile_and_run(expr, nursery).unwrap_or_else(|e| panic!("run failed: {e:?}")) {
        Value::Lit(Literal::LitInt(n)) => n,
        other => panic!("expected LitInt, got {other:?}"),
    }
}

/// `I# n`.
fn con_int(b: &mut TreeBuilder, n: i64) -> usize {
    let lit = b.push(CoreFrame::Lit(Literal::LitInt(n)));
    b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![lit],
    })
}

/// `letrec go = \i -> case i ># limit of
///    1# -> <done>
///    _  -> let junk = Just (Just (I# i)) in go (i +# 1)
///  in go 0`
///
/// Pure allocation pressure under a tiny nursery, forcing several
/// collections before `done` (already built, referencing outer bindings) is
/// reached.
fn build_gc_forcing_loop(b: &mut TreeBuilder, limit: i64, done: usize) -> usize {
    let go = fresh_var();
    let i = fresh_var();

    let iv = b.push(CoreFrame::Var(i));
    let lim = b.push(CoreFrame::Lit(Literal::LitInt(limit)));
    let cond = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntGt,
        args: vec![iv, lim],
    });

    let iv2 = b.push(CoreFrame::Var(i));
    let inner = b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![iv2],
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

    let iv3 = b.push(CoreFrame::Var(i));
    let one_lit = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let new_i = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![iv3, one_lit],
    });
    let go_v = b.push(CoreFrame::Var(go));
    let recur_call = b.push(CoreFrame::App {
        fun: go_v,
        arg: new_i,
    });
    let let_junk = b.push(CoreFrame::LetNonRec {
        binder: junk_binder,
        rhs: junk,
        body: recur_call,
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
                body: let_junk,
            },
        ],
    });

    let lam = b.push(CoreFrame::Lam {
        binder: i,
        body: body_case,
    });
    let go_v2 = b.push(CoreFrame::Var(go));
    let start = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let call = b.push(CoreFrame::App {
        fun: go_v2,
        arg: start,
    });
    b.push(CoreFrame::LetRec {
        bindings: vec![(go, lam)],
        body: call,
    })
}

/// Sequences `writeSmallArray# arr idx (I# values[idx])` for each `idx` in
/// `0..values.len()`, then `cont` (already built). Written outermost-first
/// (last write closest to the array-construction site) so the tree's
/// index-ordering invariant (every child index precedes its parent) holds.
fn build_write_chain(b: &mut TreeBuilder, arr: VarId, values: &[i64], cont: usize) -> usize {
    let mut rest = cont;
    for (idx, &val) in values.iter().enumerate().rev() {
        let arr_v = b.push(CoreFrame::Var(arr));
        let idx_lit = b.push(CoreFrame::Lit(Literal::LitInt(idx as i64)));
        let val_con = con_int(b, val);
        let write = b.push(CoreFrame::PrimOp {
            op: PrimOpKind::WriteSmallArray,
            args: vec![arr_v, idx_lit, val_con],
        });
        let case_binder = fresh_var();
        rest = b.push(CoreFrame::Case {
            scrutinee: write,
            binder: case_binder,
            alts: vec![Alt {
                con: AltCon::Default,
                binders: vec![],
                body: rest,
            }],
        });
    }
    rest
}

/// `case indexSmallArray# arr indices[0] of I# n0 -> n0*1000^0 +
///  (case indexSmallArray# arr indices[1] of I# n1 -> n1*1000^1 + ...)`
///
/// Every element is individually forced (deep-inspected, not just
/// non-null); the positional weighting makes the final checksum a unique
/// encoding of the whole sequence, provided every value stays under 1000.
fn build_checksum_read(b: &mut TreeBuilder, arr: VarId, indices: &[i64]) -> usize {
    build_checksum_read_at(b, arr, indices, 0)
}

fn build_checksum_read_at(b: &mut TreeBuilder, arr: VarId, indices: &[i64], pos: u32) -> usize {
    match indices.split_first() {
        None => b.push(CoreFrame::Lit(Literal::LitInt(0))),
        Some((&idx, tail)) => {
            let arr_v = b.push(CoreFrame::Var(arr));
            let idx_lit = b.push(CoreFrame::Lit(Literal::LitInt(idx)));
            let read = b.push(CoreFrame::PrimOp {
                op: PrimOpKind::IndexSmallArray,
                args: vec![arr_v, idx_lit],
            });
            let n = fresh_var();
            let n_v = b.push(CoreFrame::Var(n));
            let weight_lit = b.push(CoreFrame::Lit(Literal::LitInt(1000i64.pow(pos))));
            let weighted = b.push(CoreFrame::PrimOp {
                op: PrimOpKind::IntMul,
                args: vec![n_v, weight_lit],
            });
            let rest_sum = build_checksum_read_at(b, arr, tail, pos + 1);
            let total = b.push(CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![weighted, rest_sum],
            });
            let case_binder = fresh_var();
            b.push(CoreFrame::Case {
                scrutinee: read,
                binder: case_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(I_HASH),
                    binders: vec![n],
                    body: total,
                }],
            })
        }
    }
}

fn expected_checksum(values: &[i64]) -> i64 {
    values
        .iter()
        .enumerate()
        .map(|(i, &v)| v * 1000i64.pow(i as u32))
        .sum()
}

// ---------------------------------------------------------------------------
// Case 1: new / write / forced-GC / read, deep-inspected.
// ---------------------------------------------------------------------------

fn build_new_write_gc_read(limit: i64) -> (CoreExpr, i64) {
    reset_ctr();
    let mut b = TreeBuilder::new();

    let placeholder = con_int(&mut b, 0);
    let arr = fresh_var();
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let arr_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::NewSmallArray,
        args: vec![one, placeholder],
    });

    // done: read AFTER the write and the forcing loop.
    let arr_v_done = b.push(CoreFrame::Var(arr));
    let zero_idx = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let read = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IndexSmallArray,
        args: vec![arr_v_done, zero_idx],
    });
    let n = fresh_var();
    let n_v = b.push(CoreFrame::Var(n));
    let case_binder = fresh_var();
    let done = b.push(CoreFrame::Case {
        scrutinee: read,
        binder: case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(I_HASH),
            binders: vec![n],
            body: n_v,
        }],
    });

    let loop_expr = build_gc_forcing_loop(&mut b, limit, done);

    // Write a REAL heap Con into the array, sequenced before the loop.
    let arr_v_write = b.push(CoreFrame::Var(arr));
    let zero_idx2 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let x_val = con_int(&mut b, 4242);
    let write = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::WriteSmallArray,
        args: vec![arr_v_write, zero_idx2, x_val],
    });
    let write_binder = fresh_var();
    let after_write = b.push(CoreFrame::Case {
        scrutinee: write,
        binder: write_binder,
        alts: vec![Alt {
            con: AltCon::Default,
            binders: vec![],
            body: loop_expr,
        }],
    });

    let let_arr = b.push(CoreFrame::LetNonRec {
        binder: arr,
        rhs: arr_rhs,
        body: after_write,
    });

    let mut tree = b.build();
    (fixup_root(&mut tree, let_arr), 4242)
}

#[test]
fn new_write_forced_gc_read_deep_inspects_element() {
    set_heap_verify(true);
    set_gc_poison(true);
    let before = heap_verify_run_count();

    let (expr, expected) = build_new_write_gc_read(2000);
    let n = expect_int(&expr, 2 * 1024);
    assert_eq!(
        n, expected,
        "a real heap Con written into the array did not survive forced GC intact"
    );

    let after = heap_verify_run_count();
    assert!(
        after > before,
        "heap_verify_run_count did not increase ({before} -> {after}) — \
         this test forced no collection, so it guarded nothing"
    );
}

// ---------------------------------------------------------------------------
// Case 2: alias through unsafeFreeze — freeze/thaw are identity, so a write
// through the pre-freeze mutable reference is visible through the frozen
// alias. This documents that as the intended representation-sharing
// semantics, not a bug.
// ---------------------------------------------------------------------------

fn build_freeze_alias() -> CoreExpr {
    reset_ctr();
    let mut b = TreeBuilder::new();

    let placeholder = con_int(&mut b, 0);
    let arr = fresh_var();
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let arr_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::NewSmallArray,
        args: vec![one, placeholder],
    });

    let frozen = fresh_var();
    let arr_v_for_freeze = b.push(CoreFrame::Var(arr));
    let freeze_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::UnsafeFreezeSmallArray,
        args: vec![arr_v_for_freeze],
    });

    // Read through the FROZEN alias, forced after the write below.
    let frozen_v = b.push(CoreFrame::Var(frozen));
    let zero_idx = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let read = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IndexSmallArray,
        args: vec![frozen_v, zero_idx],
    });
    let n = fresh_var();
    let n_v = b.push(CoreFrame::Var(n));
    let case_binder = fresh_var();
    let done = b.push(CoreFrame::Case {
        scrutinee: read,
        binder: case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(I_HASH),
            binders: vec![n],
            body: n_v,
        }],
    });

    // Write through the ORIGINAL mutable var, sequenced before the frozen read.
    let arr_v_write = b.push(CoreFrame::Var(arr));
    let zero_idx2 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let val = con_int(&mut b, 777);
    let write = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::WriteSmallArray,
        args: vec![arr_v_write, zero_idx2, val],
    });
    let write_binder = fresh_var();
    let after_write = b.push(CoreFrame::Case {
        scrutinee: write,
        binder: write_binder,
        alts: vec![Alt {
            con: AltCon::Default,
            binders: vec![],
            body: done,
        }],
    });

    let let_frozen = b.push(CoreFrame::LetNonRec {
        binder: frozen,
        rhs: freeze_rhs,
        body: after_write,
    });
    let let_arr = b.push(CoreFrame::LetNonRec {
        binder: arr,
        rhs: arr_rhs,
        body: let_frozen,
    });

    let mut tree = b.build();
    fixup_root(&mut tree, let_arr)
}

#[test]
fn unsafe_freeze_is_identity_write_through_mutable_visible_via_frozen_alias() {
    set_heap_verify(true);
    set_gc_poison(true);

    let expr = build_freeze_alias();
    let n = expect_int(&expr, 64 * 1024);
    assert_eq!(
        n, 777,
        "unsafeFreezeSmallArray# is identity in this implementation: a write \
         through the mutable reference must be visible via the frozen alias"
    );
}

// ---------------------------------------------------------------------------
// Case 3: overlapping copySmallArray# (src == dest) in both directions,
// asserted against memmove semantics.
// ---------------------------------------------------------------------------

fn build_overlapping_copy(src_off: i64, dest_off: i64, len: i64, initial: &[i64]) -> CoreExpr {
    reset_ctr();
    let mut b = TreeBuilder::new();

    let placeholder = con_int(&mut b, 0);
    let arr = fresh_var();
    let size_lit = b.push(CoreFrame::Lit(Literal::LitInt(initial.len() as i64)));
    let arr_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::NewSmallArray,
        args: vec![size_lit, placeholder],
    });

    let indices: Vec<i64> = (0..initial.len() as i64).collect();
    let checksum = build_checksum_read(&mut b, arr, &indices);

    let arr_v_src = b.push(CoreFrame::Var(arr));
    let src_off_lit = b.push(CoreFrame::Lit(Literal::LitInt(src_off)));
    let arr_v_dest = b.push(CoreFrame::Var(arr));
    let dest_off_lit = b.push(CoreFrame::Lit(Literal::LitInt(dest_off)));
    let len_lit = b.push(CoreFrame::Lit(Literal::LitInt(len)));
    let copy = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::CopySmallArray,
        args: vec![arr_v_src, src_off_lit, arr_v_dest, dest_off_lit, len_lit],
    });
    let copy_binder = fresh_var();
    let after_copy = b.push(CoreFrame::Case {
        scrutinee: copy,
        binder: copy_binder,
        alts: vec![Alt {
            con: AltCon::Default,
            binders: vec![],
            body: checksum,
        }],
    });

    let write_all = build_write_chain(&mut b, arr, initial, after_copy);

    let let_arr = b.push(CoreFrame::LetNonRec {
        binder: arr,
        rhs: arr_rhs,
        body: write_all,
    });

    let mut tree = b.build();
    fixup_root(&mut tree, let_arr)
}

#[test]
fn overlapping_copy_shift_right_matches_memmove() {
    set_heap_verify(true);
    set_gc_poison(true);

    let initial = [10, 20, 30, 40, 50];
    // src=[0..4) -> dest starting at 1: elements shift right, overlapping.
    let expr = build_overlapping_copy(0, 1, 4, &initial);
    let n = expect_int(&expr, 64 * 1024);
    let expected = expected_checksum(&[10, 10, 20, 30, 40]);
    assert_eq!(
        n, expected,
        "copySmallArray# with src == dest, shifting right, must match memmove semantics"
    );
}

#[test]
fn overlapping_copy_shift_left_matches_memmove() {
    set_heap_verify(true);
    set_gc_poison(true);

    let initial = [10, 20, 30, 40, 50];
    // src=[1..5) -> dest starting at 0: elements shift left, overlapping.
    let expr = build_overlapping_copy(1, 0, 4, &initial);
    let n = expect_int(&expr, 64 * 1024);
    let expected = expected_checksum(&[20, 30, 40, 50, 50]);
    assert_eq!(
        n, expected,
        "copySmallArray# with src == dest, shifting left, must match memmove semantics"
    );
}

// ---------------------------------------------------------------------------
// Cases 4 & 5: negative index / index == len, for both read and write —
// clean domain errors, no out-of-bounds access, and the runtime stays
// usable afterward (proven by a fresh, independent, valid program).
// ---------------------------------------------------------------------------

fn build_index_read(len: i64, idx: i64) -> CoreExpr {
    reset_ctr();
    let mut b = TreeBuilder::new();

    let placeholder = con_int(&mut b, 0);
    let arr = fresh_var();
    let len_lit = b.push(CoreFrame::Lit(Literal::LitInt(len)));
    let arr_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::NewSmallArray,
        args: vec![len_lit, placeholder],
    });

    let arr_v = b.push(CoreFrame::Var(arr));
    let idx_lit = b.push(CoreFrame::Lit(Literal::LitInt(idx)));
    let read = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IndexSmallArray,
        args: vec![arr_v, idx_lit],
    });
    let n = fresh_var();
    let n_v = b.push(CoreFrame::Var(n));
    let case_binder = fresh_var();
    let done = b.push(CoreFrame::Case {
        scrutinee: read,
        binder: case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(I_HASH),
            binders: vec![n],
            body: n_v,
        }],
    });

    let let_arr = b.push(CoreFrame::LetNonRec {
        binder: arr,
        rhs: arr_rhs,
        body: done,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, let_arr)
}

fn build_index_write(len: i64, idx: i64) -> CoreExpr {
    reset_ctr();
    let mut b = TreeBuilder::new();

    let placeholder = con_int(&mut b, 0);
    let arr = fresh_var();
    let len_lit = b.push(CoreFrame::Lit(Literal::LitInt(len)));
    let arr_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::NewSmallArray,
        args: vec![len_lit, placeholder],
    });

    let arr_v = b.push(CoreFrame::Var(arr));
    let idx_lit = b.push(CoreFrame::Lit(Literal::LitInt(idx)));
    let val = con_int(&mut b, 1);
    let write = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::WriteSmallArray,
        args: vec![arr_v, idx_lit, val],
    });

    let let_arr = b.push(CoreFrame::LetNonRec {
        binder: arr,
        rhs: arr_rhs,
        body: write,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, let_arr)
}

/// A trivially-valid array program, used to prove the runtime is still
/// functional after a previously-triggered clean domain error.
fn build_healthy_probe() -> (CoreExpr, i64) {
    reset_ctr();
    let mut b = TreeBuilder::new();

    let val = con_int(&mut b, 55);
    let arr = fresh_var();
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let arr_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::NewSmallArray,
        args: vec![one, val],
    });

    let arr_v = b.push(CoreFrame::Var(arr));
    let zero_idx = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let read = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IndexSmallArray,
        args: vec![arr_v, zero_idx],
    });
    let n = fresh_var();
    let n_v = b.push(CoreFrame::Var(n));
    let case_binder = fresh_var();
    let done = b.push(CoreFrame::Case {
        scrutinee: read,
        binder: case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(I_HASH),
            binders: vec![n],
            body: n_v,
        }],
    });

    let let_arr = b.push(CoreFrame::LetNonRec {
        binder: arr,
        rhs: arr_rhs,
        body: done,
    });
    let mut tree = b.build();
    (fixup_root(&mut tree, let_arr), 55)
}

fn assert_machine_still_usable() {
    let (expr, expected) = build_healthy_probe();
    let n = expect_int(&expr, 64 * 1024);
    assert_eq!(
        n, expected,
        "the runtime did not recover cleanly after the prior domain error"
    );
}

#[test]
fn negative_index_read_and_write_are_clean_errors_and_machine_stays_usable() {
    set_heap_verify(true);
    set_gc_poison(true);

    let read_expr = build_index_read(4, -1);
    let read_result = compile_and_run(&read_expr, 64 * 1024);
    assert!(
        read_result.is_err(),
        "a negative index read must raise a clean error, got {read_result:?}"
    );

    let write_expr = build_index_write(4, -1);
    let write_result = compile_and_run(&write_expr, 64 * 1024);
    assert!(
        write_result.is_err(),
        "a negative index write must raise a clean error, got {write_result:?}"
    );

    assert_machine_still_usable();
}

#[test]
fn index_at_len_read_and_write_are_clean_errors_and_machine_stays_usable() {
    set_heap_verify(true);
    set_gc_poison(true);

    let read_expr = build_index_read(4, 4);
    let read_result = compile_and_run(&read_expr, 64 * 1024);
    assert!(
        read_result.is_err(),
        "an idx == len read must raise a clean error, got {read_result:?}"
    );

    let write_expr = build_index_write(4, 4);
    let write_result = compile_and_run(&write_expr, 64 * 1024);
    assert!(
        write_result.is_err(),
        "an idx == len write must raise a clean error, got {write_result:?}"
    );

    assert_machine_still_usable();
}

// ---------------------------------------------------------------------------
// Case 6: zero-length array.
// ---------------------------------------------------------------------------

fn build_sizeof_zero() -> CoreExpr {
    reset_ctr();
    let mut b = TreeBuilder::new();

    let placeholder = con_int(&mut b, 0);
    let arr = fresh_var();
    let zero = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let arr_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::NewSmallArray,
        args: vec![zero, placeholder],
    });

    let arr_v = b.push(CoreFrame::Var(arr));
    let sizeof = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::SizeofSmallArray,
        args: vec![arr_v],
    });

    let let_arr = b.push(CoreFrame::LetNonRec {
        binder: arr,
        rhs: arr_rhs,
        body: sizeof,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, let_arr)
}

#[test]
fn zero_length_array_sizeof_is_zero_and_any_index_errors_cleanly() {
    set_heap_verify(true);
    set_gc_poison(true);

    let sizeof_expr = build_sizeof_zero();
    let n = expect_int(&sizeof_expr, 64 * 1024);
    assert_eq!(n, 0, "sizeofSmallArray# of a zero-length array must be 0");

    let read_expr = build_index_read(0, 0);
    let read_result = compile_and_run(&read_expr, 64 * 1024);
    assert!(
        read_result.is_err(),
        "indexing a zero-length array must raise a clean error, got {read_result:?}"
    );

    let write_expr = build_index_write(0, 0);
    let write_result = compile_and_run(&write_expr, 64 * 1024);
    assert!(
        write_result.is_err(),
        "writing a zero-length array must raise a clean error, got {write_result:?}"
    );
}

// ---------------------------------------------------------------------------
// Case 7: an element kept live SOLELY by an array payload slot in the
// NURSERY (never tenured — the tenured version is a separate acceptance
// test for the old-space write barrier).
// ---------------------------------------------------------------------------

fn build_sole_array_reference(limit: i64) -> (CoreExpr, i64) {
    reset_ctr();
    let mut b = TreeBuilder::new();

    // The element: Just (I# 9001). After this point it is referenced ONLY
    // by the array slot — nothing else roots it.
    let inner_lit = b.push(CoreFrame::Lit(Literal::LitInt(9001)));
    let inner_con = b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![inner_lit],
    });
    let elem = b.push(CoreFrame::Con {
        tag: JUST,
        fields: vec![inner_con],
    });

    let arr = fresh_var();
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let arr_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::NewSmallArray,
        args: vec![one, elem],
    });

    // done: case arr[0] of Just x -> case x of I# n -> n — both constructor
    // layers forced, a genuine deep inspection rather than a non-null check.
    let arr_v = b.push(CoreFrame::Var(arr));
    let zero_idx = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let read = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IndexSmallArray,
        args: vec![arr_v, zero_idx],
    });
    let inner_binder = fresh_var();
    let inner_v = b.push(CoreFrame::Var(inner_binder));
    let n = fresh_var();
    let n_v = b.push(CoreFrame::Var(n));
    let unbox_case_binder = fresh_var();
    let unbox_case = b.push(CoreFrame::Case {
        scrutinee: inner_v,
        binder: unbox_case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(I_HASH),
            binders: vec![n],
            body: n_v,
        }],
    });
    let just_case_binder = fresh_var();
    let done = b.push(CoreFrame::Case {
        scrutinee: read,
        binder: just_case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(JUST),
            binders: vec![inner_binder],
            body: unbox_case,
        }],
    });

    let loop_expr = build_gc_forcing_loop(&mut b, limit, done);

    let let_arr = b.push(CoreFrame::LetNonRec {
        binder: arr,
        rhs: arr_rhs,
        body: loop_expr,
    });
    let mut tree = b.build();
    (fixup_root(&mut tree, let_arr), 9001)
}

#[test]
fn element_kept_live_solely_by_array_payload_slot_survives_nursery_gc() {
    set_heap_verify(true);
    set_gc_poison(true);
    let before = heap_verify_run_count();

    let (expr, expected) = build_sole_array_reference(2000);
    let n = expect_int(&expr, 2 * 1024);
    assert_eq!(
        n, expected,
        "an element rooted solely by the array payload slot did not survive nursery GC intact"
    );

    let after = heap_verify_run_count();
    assert!(
        after > before,
        "heap_verify_run_count did not increase ({before} -> {after}) — \
         this test forced no collection, so it guarded nothing"
    );
}
