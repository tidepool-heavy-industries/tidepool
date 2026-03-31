//! Analysis to identify free variables in Tidepool IR expressions.

use crate::{CoreExpr, CoreFrame, VarId};
use std::collections::HashSet;

/// Collect all free variables in the expression rooted at the given node.
pub fn free_vars(tree: &CoreExpr) -> HashSet<VarId> {
    if tree.nodes.is_empty() {
        return HashSet::new();
    }
    free_vars_at(tree, tree.nodes.len() - 1)
}

fn free_vars_at(tree: &CoreExpr, idx: usize) -> HashSet<VarId> {
    match &tree.nodes[idx] {
        CoreFrame::Var(v) => {
            let mut s = HashSet::new();
            s.insert(*v);
            s
        }
        CoreFrame::Lit(_) => HashSet::new(),
        CoreFrame::App { fun, arg } => {
            let mut s = free_vars_at(tree, *fun);
            s.extend(free_vars_at(tree, *arg));
            s
        }
        CoreFrame::Lam { binder, body } => {
            let mut s = free_vars_at(tree, *body);
            s.remove(binder);
            s
        }
        CoreFrame::LetNonRec { binder, rhs, body } => {
            let mut s = free_vars_at(tree, *rhs);
            let mut body_fvs = free_vars_at(tree, *body);
            body_fvs.remove(binder);
            s.extend(body_fvs);
            s
        }
        CoreFrame::LetRec { bindings, body } => {
            let bound: HashSet<VarId> = bindings.iter().map(|(v, _)| *v).collect();
            bindings
                .iter()
                .map(|(_, rhs)| *rhs)
                .chain(std::iter::once(*body))
                .flat_map(|idx| free_vars_at(tree, idx))
                .filter(|v| !bound.contains(v))
                .collect()
        }
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            let mut s = free_vars_at(tree, *scrutinee);
            let alt_fvs = alts.iter().flat_map(|alt| {
                let mut fvs = free_vars_at(tree, alt.body);
                fvs.remove(binder);
                for b in &alt.binders {
                    fvs.remove(b);
                }
                fvs
            });
            s.extend(alt_fvs);
            s
        }
        CoreFrame::Con { fields, .. } => {
            fields.iter().flat_map(|f| free_vars_at(tree, *f)).collect()
        }
        CoreFrame::Join {
            label: _,
            params,
            rhs,
            body,
        } => {
            let param_set: HashSet<VarId> = params.iter().copied().collect();
            let mut rhs_fvs = free_vars_at(tree, *rhs);
            for p in &param_set {
                rhs_fvs.remove(p);
            }
            // Join label scopes over body (and rhs references label via Jump, not as free var)
            let body_fvs = free_vars_at(tree, *body);
            let mut s = rhs_fvs;
            s.extend(body_fvs);
            s
        }
        CoreFrame::Jump { args, .. } => {
            args.iter().flat_map(|a| free_vars_at(tree, *a)).collect()
        }
        CoreFrame::PrimOp { args, .. } => {
            args.iter().flat_map(|a| free_vars_at(tree, *a)).collect()
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
        let mut expected = HashSet::new();
        expected.insert(x);
        assert_eq!(free_vars(&expr), expected);
    }

    #[test]
    fn test_free_vars_lam_bound() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Var(x),                     // 0
            CoreFrame::Lam { binder: x, body: 0 }, // 1
        ]);
        assert_eq!(free_vars(&expr), HashSet::new());
    }

    #[test]
    fn test_free_vars_lam_free() {
        let x = VarId(1);
        let y = VarId(2);
        let expr = tree(vec![
            CoreFrame::Var(y),                     // 0
            CoreFrame::Lam { binder: x, body: 0 }, // 1
        ]);
        let mut expected = HashSet::new();
        expected.insert(y);
        assert_eq!(free_vars(&expr), expected);
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
        let mut expected = HashSet::new();
        expected.insert(y);
        assert_eq!(free_vars(&expr), expected);
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
        assert_eq!(free_vars(&expr), HashSet::new());
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
        let mut expected = HashSet::new();
        expected.insert(a);
        assert_eq!(free_vars(&expr), expected);
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
        let mut expected = HashSet::new();
        expected.insert(x);
        expected.insert(y);
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
        let mut expected = HashSet::new();
        expected.insert(y);
        expected.insert(x);
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
        let mut expected = HashSet::new();
        expected.insert(x);
        assert_eq!(free_vars(&expr), expected);
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
        assert!(fvs.contains(&y), "y should be free");
        assert!(fvs.contains(&z), "z should be free");
        assert!(!fvs.contains(&x), "x should be bound by join param");
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
        assert!(fvs.contains(&x));
        assert!(fvs.contains(&y));
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
        assert!(fvs.contains(&x));
        assert!(fvs.contains(&y));
    }
}
