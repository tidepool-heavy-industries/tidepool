//! Free-variable analysis for Tidepool IR expressions.
//!
//! [`FreeVarsIndex`] is the ONE engine: a single forward pass over every node
//! of a `CoreExpr` that computes the free-variable set of the subtree rooted
//! at EVERY index, once, and serves each query as an O(1) index lookup (sets
//! are `Rc`-shared, so a query allocates only the final sorted `Vec`). This
//! also makes it trivially stack-safe — no explicit-stack walk needed at all:
//! `RecursiveTree`'s flat-vector invariant (every child index is strictly
//! less than its parent's, enforced by `debug_assert!(c < i, ...)` at every
//! whole-tree walk in `tree.rs`) means a single forward pass over
//! `0..nodes.len()` visits every node's children before the node itself.
//!
//! [`free_vars`] — the whole-tree root query most callers want — is just
//! `FreeVarsIndex::compute(tree).free_vars_at(root)`. Reach for
//! [`FreeVarsIndex`] directly when you need free variables at more than one
//! node of the same tree (e.g. a per-binding loop over a `LetRec` group): one
//! `compute` call amortizes across every query, instead of each query
//! re-walking (and in the old per-root-only API, re-extracting) its own
//! subtree.

use crate::{CoreExpr, CoreFrame, VarId};
use rustc_hash::FxHashSet;
use std::rc::Rc;

/// Free-variable sets for every node index of a `CoreExpr`, computed once.
pub struct FreeVarsIndex {
    sets: Vec<Rc<FxHashSet<VarId>>>,
}

impl FreeVarsIndex {
    /// See the module doc for why a single forward pass suffices.
    pub fn compute(tree: &CoreExpr) -> Self {
        let empty: Rc<FxHashSet<VarId>> = Rc::new(FxHashSet::default());
        let mut sets: Vec<Rc<FxHashSet<VarId>>> = Vec::with_capacity(tree.nodes.len());
        for frame in &tree.nodes {
            sets.push(node_free_vars(frame, &sets, &empty));
        }
        FreeVarsIndex { sets }
    }

    /// Free variables of the subtree rooted at `idx`: a sorted, deduplicated
    /// `Vec<VarId>` — the contract every caller (`binary_search`, sorted
    /// iteration) relies on.
    pub fn free_vars_at(&self, idx: usize) -> Vec<VarId> {
        let mut v: Vec<VarId> = self.sets[idx].iter().copied().collect();
        v.sort_unstable();
        v
    }

    pub fn free_vars_set_at(&self, idx: usize) -> &FxHashSet<VarId> {
        &self.sets[idx]
    }
}

/// Collect all free variables in the expression rooted at this tree's root node.
/// Returns a sorted, deduplicated `Vec<VarId>` for efficient access and minimal allocation.
pub fn free_vars(tree: &CoreExpr) -> Vec<VarId> {
    if tree.nodes.is_empty() {
        return Vec::new();
    }
    let root = tree.nodes.len() - 1;
    FreeVarsIndex::compute(tree).free_vars_at(root)
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

/// One node's free-variable set from its already-computed children's sets
/// (`sets`, filled by the forward pass in [`FreeVarsIndex::compute`] — every
/// child index is strictly less than `idx`, so it is already present).
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
    use crate::types::*;
    use crate::RecursiveTree;

    /// Helper to build a single-node tree
    fn leaf(frame: CoreFrame<usize>) -> CoreExpr {
        RecursiveTree { nodes: vec![frame] }
    }

    /// Helper to build a tree with given nodes (root is last)
    fn tree(nodes: Vec<CoreFrame<usize>>) -> CoreExpr {
        RecursiveTree { nodes }
    }

    #[test]
    fn test_free_vars_var() {
        let x = VarId(1);
        let expr = leaf(CoreFrame::Var(x));
        assert_eq!(free_vars(&expr), vec![x]);
    }

    #[test]
    fn test_free_vars_lam_bound() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Var(x),                     // 0
            CoreFrame::Lam { binder: x, body: 0 }, // 1
        ]);
        assert_eq!(free_vars(&expr), Vec::<VarId>::new());
    }

    #[test]
    fn test_free_vars_lam_free() {
        let x = VarId(1);
        let y = VarId(2);
        let expr = tree(vec![
            CoreFrame::Var(y),                     // 0
            CoreFrame::Lam { binder: x, body: 0 }, // 1
        ]);
        assert_eq!(free_vars(&expr), vec![y]);
    }

    #[test]
    fn test_free_vars_let_non_rec() {
        let x = VarId(1);
        let y = VarId(2);
        let expr = tree(vec![
            CoreFrame::Var(y), // 0: rhs
            CoreFrame::Var(x), // 1: body
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            }, // 2
        ]);
        assert_eq!(free_vars(&expr), vec![y]);
    }

    #[test]
    fn test_free_vars_let_rec() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Var(x), // 0: rhs/body
            CoreFrame::LetRec {
                bindings: vec![(x, 0)],
                body: 0,
            }, // 1
        ]);
        assert_eq!(free_vars(&expr), Vec::<VarId>::new());
    }

    #[test]
    fn test_free_vars_case() {
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
        assert_eq!(free_vars(&expr), vec![a]);
    }

    #[test]
    fn test_free_vars_con() {
        let x = VarId(1);
        let y = VarId(2);
        let expr = tree(vec![
            CoreFrame::Var(x),
            CoreFrame::Var(y),
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0, 1],
            },
        ]);
        let mut expected = vec![x, y];
        expected.sort();
        assert_eq!(free_vars(&expr), expected);
    }

    #[test]
    fn test_free_vars_join_jump() {
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
        // x is bound in rhs by Join params, but NOT in body. y is free in rhs.
        let mut expected = vec![x, y];
        expected.sort();
        assert_eq!(free_vars(&expr), expected);
    }

    #[test]
    fn test_free_vars_primop() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Var(x),
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            },
        ]);
        assert_eq!(free_vars(&expr), vec![x]);
    }

    #[test]
    fn test_free_vars_join_spec() {
        // join j(x) = x + y in jump j(z)
        // Free vars should include y and z but NOT x (bound by join param)
        let y = VarId(1);
        let x = VarId(2);
        let z = VarId(3);
        let j = JoinId(1);
        let tree_expr = tree(vec![
            CoreFrame::Var(x), // 0: x (in rhs)
            CoreFrame::Var(y), // 1: y (in rhs)
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: x + y (rhs)
            CoreFrame::Var(z), // 3: z (jump arg)
            CoreFrame::Jump {
                label: j,
                args: vec![3],
            }, // 4: jump j(z) (body)
            CoreFrame::Join {
                label: j,
                params: vec![x],
                rhs: 2,
                body: 4,
            }, // 5: root
        ]);
        let fvs = free_vars(&tree_expr);
        assert!(fvs.binary_search(&y).is_ok(), "y should be free");
        assert!(fvs.binary_search(&z).is_ok(), "z should be free");
        assert!(
            fvs.binary_search(&x).is_err(),
            "x should be bound by join param"
        );
    }

    #[test]
    fn test_free_vars_primop_free() {
        // x + y where both are free
        let x = VarId(1);
        let y = VarId(2);
        let tree_expr = tree(vec![
            CoreFrame::Var(x),
            CoreFrame::Var(y),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            },
        ]);
        let fvs = free_vars(&tree_expr);
        assert!(fvs.binary_search(&x).is_ok());
        assert!(fvs.binary_search(&y).is_ok());
        assert_eq!(fvs.len(), 2);
    }

    #[test]
    fn test_free_vars_con_fields_spec() {
        // Con(tag=0, [x, y]) — both x and y should be free
        let x = VarId(1);
        let y = VarId(2);
        let tree_expr = tree(vec![
            CoreFrame::Var(x),
            CoreFrame::Var(y),
            CoreFrame::Con {
                tag: DataConId(0),
                fields: vec![0, 1],
            },
        ]);
        let fvs = free_vars(&tree_expr);
        assert!(fvs.binary_search(&x).is_ok());
        assert!(fvs.binary_search(&y).is_ok());
    }

    // -- Per-node FreeVarsIndex queries (moved from
    // tidepool-codegen/src/emit/free_vars_index.rs on consolidation) --

    #[test]
    fn index_matches_reference_on_lam_bound_and_free() {
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
    fn index_matches_reference_on_let_rec_self_recursive() {
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
    fn index_matches_reference_on_case_binders() {
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
    fn index_matches_reference_on_join_jump() {
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
    fn index_empty_sets_are_shared() {
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
    fn index_single_nonempty_child_shares_rc() {
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
