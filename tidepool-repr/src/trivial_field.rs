//! Shared shallow safety test for materializing a value in a lazy position.
//!
//! Constructors are values even when their fields contain computations. Both
//! backends must materialize their fields lazily; this predicate never grants
//! permission to evaluate a constructor's descendants. Primitive applications
//! remain computations, including those whose arguments happen to be literals.

use crate::{CoreExpr, CoreFrame};

/// Whether a node can be materialized without demanding a computation.
/// References reuse an existing value, lambdas allocate closures, and
/// constructors allocate data with separately suspended computation fields.
pub fn is_trivial_field(idx: usize, expr: &CoreExpr) -> bool {
    matches!(
        expr.nodes[idx],
        CoreFrame::Var(_) | CoreFrame::Lit(_) | CoreFrame::Lam { .. } | CoreFrame::Con { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Literal, PrimOpKind, VarId};
    use crate::RecursiveTree;

    fn tree(nodes: Vec<CoreFrame<usize>>) -> CoreExpr {
        RecursiveTree { nodes }
    }

    #[test]
    fn var_lit_lam_are_trivial() {
        let expr = tree(vec![CoreFrame::Var(VarId(1))]);
        assert!(is_trivial_field(0, &expr));

        let expr = tree(vec![CoreFrame::Lit(Literal::LitInt(1))]);
        assert!(is_trivial_field(0, &expr));

        let expr = tree(vec![CoreFrame::Lam {
            binder: VarId(1),
            body: 0,
        }]);
        assert!(is_trivial_field(0, &expr));
    }

    #[test]
    fn raise_is_never_trivial_even_with_trivial_args() {
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(5)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Raise,
                args: vec![0],
            },
        ]);
        assert!(!is_trivial_field(1, &expr));

        let expr = tree(vec![CoreFrame::PrimOp {
            op: PrimOpKind::Raise,
            args: vec![],
        }]);
        assert!(!is_trivial_field(0, &expr));
    }

    #[test]
    fn arithmetic_computation_is_not_trivial_even_with_literal_args() {
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            },
        ]);
        assert!(!is_trivial_field(2, &expr));
    }

    #[test]
    fn constructor_materialization_does_not_demand_fields() {
        use crate::types::DataConId;

        let expr = tree(vec![
            CoreFrame::Var(VarId(1)),
            CoreFrame::Con {
                tag: DataConId(0),
                fields: vec![0],
            },
        ]);
        assert!(is_trivial_field(1, &expr));

        let expr = tree(vec![
            CoreFrame::PrimOp {
                op: PrimOpKind::Raise,
                args: vec![],
            },
            CoreFrame::Con {
                tag: DataConId(0),
                fields: vec![0],
            },
        ]);
        assert!(is_trivial_field(1, &expr));
    }

    #[test]
    fn app_case_let_join_jump_are_never_trivial() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Var(x),
            CoreFrame::Var(x),
            CoreFrame::App { fun: 0, arg: 1 },
        ]);
        assert!(!is_trivial_field(2, &expr));
    }

    #[test]
    fn deeply_nested_trivial_cons_are_stack_safe() {
        use crate::types::DataConId;

        let depth = 50_000;
        let mut nodes = vec![CoreFrame::Lit(Literal::LitInt(0))];
        for i in 0..depth {
            nodes.push(CoreFrame::Con {
                tag: DataConId(0),
                fields: vec![i],
            });
        }
        let expr = tree(nodes);
        let root = expr.nodes.len() - 1;
        assert!(is_trivial_field(root, &expr));
    }
}
