//! Compilation-wide indexed free-variable analysis.
//!
//! Computes the free-variable set of the subtree rooted at EVERY node index
//! of a `CoreExpr`, once, in a single forward pass, and serves every query
//! as an O(1) index lookup (plus a cheap sort of that node's own set, to
//! preserve the exact `free_vars` contract). Avoids the quadratic cost of
//! extracting and re-walking a fresh subtree per query, which matters
//! because free-var queries happen repeatedly inside per-binding loops.
//!
//! # Traversal order
//!
//! `RecursiveTree`'s flat-vector invariant — enforced by `debug_assert!(c <
//! i, ...)` at every whole-tree walk in `tree.rs` and `free_vars.rs` — is that
//! every child index is strictly less than its parent's index. A single
//! forward pass over `0..nodes.len()` therefore visits every node's children
//! before the node itself; this module's whole one-pass approach depends on
//! that invariant holding.

use rustc_hash::FxHashSet;
use std::rc::Rc;
use tidepool_repr::{CoreExpr, CoreFrame, VarId};

pub struct FreeVarsIndex {
    sets: Vec<Rc<FxHashSet<VarId>>>,
}

impl FreeVarsIndex {
    /// See the module doc ("Traversal order") for why a single forward pass
    /// suffices.
    pub fn compute(tree: &CoreExpr) -> Self {
        let empty: Rc<FxHashSet<VarId>> = Rc::new(FxHashSet::default());
        let mut sets: Vec<Rc<FxHashSet<VarId>>> = Vec::with_capacity(tree.nodes.len());
        for frame in &tree.nodes {
            sets.push(node_free_vars(frame, &sets, &empty));
        }
        FreeVarsIndex { sets }
    }

    /// Free variables of the subtree rooted at `idx`: a sorted, deduplicated
    /// `Vec<VarId>` — the exact contract `tidepool_repr::free_vars::free_vars`
    /// returns, so existing callers (`binary_search`, sorted iteration) need
    /// no change beyond swapping the call.
    pub fn free_vars_at(&self, idx: usize) -> Vec<VarId> {
        let mut v: Vec<VarId> = self.sets[idx].iter().copied().collect();
        v.sort_unstable();
        v
    }

    pub fn free_vars_set_at(&self, idx: usize) -> &FxHashSet<VarId> {
        &self.sets[idx]
    }
}

fn child(sets: &[Rc<FxHashSet<VarId>>], idx: usize) -> &Rc<FxHashSet<VarId>> {
    &sets[idx]
}

fn union_children(
    children: &[&Rc<FxHashSet<VarId>>],
    empty: &Rc<FxHashSet<VarId>>,
) -> Rc<FxHashSet<VarId>> {
    let nonempty: Vec<&Rc<FxHashSet<VarId>>> =
        children.iter().copied().filter(|s| !s.is_empty()).collect();
    match nonempty.as_slice() {
        [] => Rc::clone(empty),
        [only] => Rc::clone(only),
        many => {
            let mut merged: FxHashSet<VarId> = FxHashSet::default();
            for s in many {
                merged.extend(s.iter().copied());
            }
            Rc::new(merged)
        }
    }
}

fn remove_binders(
    set: &Rc<FxHashSet<VarId>>,
    binders: &[VarId],
    empty: &Rc<FxHashSet<VarId>>,
) -> Rc<FxHashSet<VarId>> {
    if binders.iter().all(|b| !set.contains(b)) {
        return Rc::clone(set);
    }
    let mut s: FxHashSet<VarId> = (**set).clone();
    for b in binders {
        s.remove(b);
    }
    if s.is_empty() {
        Rc::clone(empty)
    } else {
        Rc::new(s)
    }
}

/// Scoping must stay identical to `tidepool_repr::free_vars::node_free_vars`.
fn node_free_vars(
    frame: &CoreFrame<usize>,
    sets: &[Rc<FxHashSet<VarId>>],
    empty: &Rc<FxHashSet<VarId>>,
) -> Rc<FxHashSet<VarId>> {
    match frame {
        CoreFrame::Var(v) => {
            let mut s = FxHashSet::default();
            s.insert(*v);
            Rc::new(s)
        }
        CoreFrame::Lit(_) => Rc::clone(empty),
        CoreFrame::App { fun, arg } => {
            union_children(&[child(sets, *fun), child(sets, *arg)], empty)
        }
        CoreFrame::Lam { binder, body } => {
            remove_binders(child(sets, *body), std::slice::from_ref(binder), empty)
        }
        CoreFrame::LetNonRec { binder, rhs, body } => {
            let body_bound =
                remove_binders(child(sets, *body), std::slice::from_ref(binder), empty);
            union_children(&[child(sets, *rhs), &body_bound], empty)
        }
        CoreFrame::LetRec { bindings, body } => {
            let bound: Vec<VarId> = bindings.iter().map(|(v, _)| *v).collect();
            let rhs_sets: Vec<Rc<FxHashSet<VarId>>> = bindings
                .iter()
                .map(|(_, rhs)| remove_binders(child(sets, *rhs), &bound, empty))
                .collect();
            let body_bound = remove_binders(child(sets, *body), &bound, empty);
            let mut refs: Vec<&Rc<FxHashSet<VarId>>> = rhs_sets.iter().collect();
            refs.push(&body_bound);
            union_children(&refs, empty)
        }
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            let mut alt_sets: Vec<Rc<FxHashSet<VarId>>> = Vec::with_capacity(alts.len());
            for alt in alts {
                let mut binders: Vec<VarId> = Vec::with_capacity(alt.binders.len() + 1);
                binders.push(*binder);
                binders.extend(alt.binders.iter().copied());
                alt_sets.push(remove_binders(child(sets, alt.body), &binders, empty));
            }
            let mut refs: Vec<&Rc<FxHashSet<VarId>>> = Vec::with_capacity(alt_sets.len() + 1);
            refs.push(child(sets, *scrutinee));
            refs.extend(alt_sets.iter());
            union_children(&refs, empty)
        }
        CoreFrame::Con { fields, .. } => {
            let refs: Vec<&Rc<FxHashSet<VarId>>> = fields.iter().map(|f| child(sets, *f)).collect();
            union_children(&refs, empty)
        }
        CoreFrame::Join {
            label: _,
            params,
            rhs,
            body,
        } => {
            let rhs_bound = remove_binders(child(sets, *rhs), params, empty);
            union_children(&[&rhs_bound, child(sets, *body)], empty)
        }
        CoreFrame::Jump { args, .. } => {
            let refs: Vec<&Rc<FxHashSet<VarId>>> = args.iter().map(|a| child(sets, *a)).collect();
            union_children(&refs, empty)
        }
        CoreFrame::PrimOp { args, .. } => {
            let refs: Vec<&Rc<FxHashSet<VarId>>> = args.iter().map(|a| child(sets, *a)).collect();
            union_children(&refs, empty)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::types::*;
    use tidepool_repr::RecursiveTree;

    fn tree(nodes: Vec<CoreFrame<usize>>) -> CoreExpr {
        RecursiveTree { nodes }
    }

    #[test]
    fn matches_reference_on_lam_bound_and_free() {
        let x = VarId(1);
        let y = VarId(2);
        let expr = tree(vec![
            CoreFrame::Var(y),                     // 0
            CoreFrame::Lam { binder: x, body: 0 }, // 1: free = {y}
        ]);
        let idx = FreeVarsIndex::compute(&expr);
        assert_eq!(idx.free_vars_at(1), vec![y]);
        assert_eq!(idx.free_vars_at(0), vec![y]);
    }

    #[test]
    fn matches_reference_on_let_rec_self_recursive() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Var(x), // 0: rhs/body
            CoreFrame::LetRec {
                bindings: vec![(x, 0)],
                body: 0,
            }, // 1
        ]);
        let idx = FreeVarsIndex::compute(&expr);
        assert_eq!(idx.free_vars_at(1), Vec::<VarId>::new());
    }

    #[test]
    fn matches_reference_on_case_binders() {
        let a = VarId(1);
        let b = VarId(2);
        let expr = tree(vec![
            CoreFrame::Var(a), // 0: scrutinee
            CoreFrame::Var(b), // 1: alt body
            CoreFrame::Case {
                scrutinee: 0,
                binder: b,
                alts: vec![Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: 1,
                }],
            }, // 2
        ]);
        let idx = FreeVarsIndex::compute(&expr);
        assert_eq!(idx.free_vars_at(2), vec![a]);
    }

    #[test]
    fn matches_reference_on_join_jump() {
        let x = VarId(1);
        let y = VarId(2);
        let expr = tree(vec![
            CoreFrame::Var(y), // 0: Jump arg
            CoreFrame::Jump {
                label: JoinId(1),
                args: vec![0],
            }, // 1: rhs
            CoreFrame::Var(x), // 2: body
            CoreFrame::Join {
                label: JoinId(1),
                params: vec![x],
                rhs: 1,
                body: 2,
            }, // 3
        ]);
        let idx = FreeVarsIndex::compute(&expr);
        let mut expected = vec![x, y];
        expected.sort();
        assert_eq!(idx.free_vars_at(3), expected);
    }

    #[test]
    fn empty_sets_are_shared() {
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Lit(Literal::LitInt(2)), // 1
            CoreFrame::App { fun: 0, arg: 1 },  // 2
        ]);
        let idx = FreeVarsIndex::compute(&expr);
        assert!(idx.free_vars_set_at(0).is_empty());
        assert!(idx.free_vars_set_at(1).is_empty());
        assert!(idx.free_vars_set_at(2).is_empty());
        assert!(Rc::ptr_eq(&idx.sets[0], &idx.sets[1]));
    }

    #[test]
    fn single_nonempty_child_shares_rc() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Var(x),                  // 0
            CoreFrame::Lit(Literal::LitInt(1)), // 1
            CoreFrame::App { fun: 0, arg: 1 },  // 2: only fun contributes
        ]);
        let idx = FreeVarsIndex::compute(&expr);
        assert!(Rc::ptr_eq(&idx.sets[0], &idx.sets[2]));
    }
}
