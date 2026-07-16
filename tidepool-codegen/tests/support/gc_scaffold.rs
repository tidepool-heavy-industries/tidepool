//! Shared GC-pressure test scaffolding between `heap_verify_lane.rs` and
//! `proptest_gc_recursion.rs`: standard DataCon tags, the fresh-VarId supply,
//! root fixup, and the cons-spine / balanced Pair-tree `CoreExpr` builders.
//! `tests/*.rs` files are separate crates, so this is included via `#[path]`
//! rather than shared as an ordinary library module.

use std::cell::Cell;

use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

// Standard DataCon tags (must match `standard_datacon_table` in tidepool-testing).
pub const PAIR: DataConId = DataConId(4);
pub const NIL: DataConId = DataConId(5); // []
pub const CONS: DataConId = DataConId(6); // :
pub const I_HASH: DataConId = DataConId(7); // I# single-field Int box wrapper

thread_local! {
    static VAR_CTR: Cell<u64> = const { Cell::new(1000) };
}

pub fn fresh_var() -> VarId {
    VAR_CTR.with(|c| {
        let v = c.get();
        c.set(v + 1);
        VarId(v)
    })
}

pub fn reset_ctr() {
    VAR_CTR.with(|c| c.set(1000));
}

/// eval/compile treat the LAST node as the root; wrap in `let v = <root> in v`
/// when a helper appended nodes after the structural root.
pub fn fixup_root(tree: &mut CoreExpr, root: usize) -> CoreExpr {
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

/// Build a literal cons-spine `e0 : e1 : ... : []`, return its root index.
pub fn push_spine(b: &mut TreeBuilder, elems: &[i64]) -> usize {
    let mut tail = b.push(CoreFrame::Con {
        tag: NIL,
        fields: vec![],
    });
    for &e in elems.iter().rev() {
        let head = b.push(CoreFrame::Lit(Literal::LitInt(e)));
        tail = b.push(CoreFrame::Con {
            tag: CONS,
            fields: vec![head, tail],
        });
    }
    tail
}

/// Build a balanced Pair-tree of `depth` over `leaves` (len must be 2^depth).
pub fn push_pairtree(b: &mut TreeBuilder, depth: u32, leaves: &[i64]) -> usize {
    if depth == 0 {
        return b.push(CoreFrame::Lit(Literal::LitInt(leaves[0])));
    }
    let half = leaves.len() / 2;
    let l = push_pairtree(b, depth - 1, &leaves[..half]);
    let r = push_pairtree(b, depth - 1, &leaves[half..]);
    b.push(CoreFrame::Con {
        tag: PAIR,
        fields: vec![l, r],
    })
}

/// Checksum walk over a balanced Pair-tree of `depth`, folding all leaves into
/// one Int# sum.
pub fn push_pairtree_sum(b: &mut TreeBuilder, depth: u32, tree: usize) -> usize {
    if depth == 0 {
        return tree;
    }
    let case_binder = fresh_var();
    let l = fresh_var();
    let r = fresh_var();
    let l_v = b.push(CoreFrame::Var(l));
    let l_sum = push_pairtree_sum(b, depth - 1, l_v);
    let r_v = b.push(CoreFrame::Var(r));
    let r_sum = push_pairtree_sum(b, depth - 1, r_v);
    let sum = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![l_sum, r_sum],
    });
    b.push(CoreFrame::Case {
        scrutinee: tree,
        binder: case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(PAIR),
            binders: vec![l, r],
            body: sum,
        }],
    })
}
