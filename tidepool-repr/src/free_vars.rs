//! Analysis to identify free variables in Tidepool IR expressions.

use crate::tree::for_each_child_rev;
use crate::{CoreExpr, CoreFrame, VarId};
use rustc_hash::{FxHashMap, FxHashSet};

/// Collect all free variables in the expression rooted at this tree's root node.
/// Returns a sorted, deduplicated `Vec<VarId>` for efficient access and minimal allocation.
///
/// Stack-safe: an explicit-stack post-order walk memoizes each subtree's
/// free-variable set by node index, so arbitrarily deep trees (this runs in the
/// emit hot path) are analyzed without per-level call-stack growth. A subtree's
/// free-var set is intrinsic to that subtree (binder removal happens at the
/// binding node, never propagated inward), so memoizing by index yields the
/// same result as the former recursive walk — and is strictly cheaper on shared
/// (DAG) subtrees, which the recursive version recomputed per occurrence.
pub fn free_vars(tree: &CoreExpr) -> Vec<VarId> {
    if tree.nodes.is_empty() {
        return Vec::new();
    }
    let root = tree.nodes.len() - 1;
    // memo[i] = free vars of the subtree rooted at i, filled at Exit. Presence
    // is the sole bookkeeping (no separate "seen" set): an Enter/Exit for an
    // already-computed index is skipped, so shared subtrees are analyzed once.
    let mut memo: FxHashMap<usize, FxHashSet<VarId>> = FxHashMap::default();

    enum Step {
        Enter(usize),
        Exit(usize),
    }
    let mut stack = vec![Step::Enter(root)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(i) => {
                if memo.contains_key(&i) {
                    continue;
                }
                stack.push(Step::Exit(i));
                for_each_child_rev(&tree.nodes[i], |c| {
                    if !memo.contains_key(&c) {
                        stack.push(Step::Enter(c));
                    }
                });
            }
            Step::Exit(i) => {
                if memo.contains_key(&i) {
                    continue; // already computed via another (shared) path
                }
                let s = node_free_vars(tree, i, &memo);
                memo.insert(i, s);
            }
        }
    }

    let mut fvs: Vec<VarId> = memo.remove(&root).unwrap_or_default().into_iter().collect();
    fvs.sort_unstable();
    fvs
}

/// Compute one node's free-variable set from its already-computed children's
/// sets (`memo`). Scoping is identical to the former recursive `free_vars_at`.
fn node_free_vars(
    tree: &CoreExpr,
    idx: usize,
    memo: &FxHashMap<usize, FxHashSet<VarId>>,
) -> FxHashSet<VarId> {
    let child = |i: &usize| memo.get(i).cloned().unwrap_or_default();
    match &tree.nodes[idx] {
        CoreFrame::Var(v) => {
            let mut s = FxHashSet::default();
            s.insert(*v);
            s
        }
        CoreFrame::Lit(_) => FxHashSet::default(),
        CoreFrame::App { fun, arg } => {
            let mut s = child(fun);
            s.extend(child(arg));
            s
        }
        CoreFrame::Lam { binder, body } => {
            let mut s = child(body);
            s.remove(binder);
            s
        }
        CoreFrame::LetNonRec { binder, rhs, body } => {
            let mut s = child(rhs);
            let mut body_fvs = child(body);
            body_fvs.remove(binder);
            s.extend(body_fvs);
            s
        }
        CoreFrame::LetRec { bindings, body } => {
            let bound: FxHashSet<VarId> = bindings.iter().map(|(v, _)| *v).collect();
            let mut s: FxHashSet<VarId> = bindings
                .iter()
                .flat_map(|(_, rhs)| child(rhs))
                .filter(|v| !bound.contains(v))
                .collect();

            let body_fvs = child(body);
            s.extend(body_fvs.difference(&bound));
            s
        }
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            let mut s = child(scrutinee);
            for alt in alts {
                let mut alt_fvs = child(&alt.body);
                alt_fvs.remove(binder); // case binder
                for b in &alt.binders {
                    alt_fvs.remove(b); // pattern binders
                }
                s.extend(alt_fvs);
            }
            s
        }
        CoreFrame::Con { fields, .. } => fields.iter().flat_map(child).collect(),
        CoreFrame::Join {
            label: _,
            params,
            rhs,
            body,
        } => {
            let param_set: FxHashSet<VarId> = params.iter().copied().collect();
            let mut rhs_fvs = child(rhs);
            for p in &param_set {
                rhs_fvs.remove(p);
            }
            // Join label scopes over body (and rhs references label via Jump, not as free var)
            let body_fvs = child(body);
            let mut s = rhs_fvs;
            s.extend(body_fvs);
            s
        }
        CoreFrame::Jump { args, .. } => args.iter().flat_map(child).collect(),
        CoreFrame::PrimOp { args, .. } => args.iter().flat_map(child).collect(),
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
}
