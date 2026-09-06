//! Shared conservative safety test for eager materialization in a lazy position.
//!
//! Eval constructor fields and JIT constructor fields, bindings, and lazy
//! primitive operands use this owner. Primitive applications are computations,
//! even with literal operands: they may fail, have effects, or demand a bottom
//! through a variable. Keep them thunked rather than maintaining a second
//! registry of supposedly total operations.
//!
//! The explicit work stack keeps deeply nested constructor spines stack-safe.

use crate::{CoreExpr, CoreFrame};

/// Returns true if the expression at `idx` is trivial (safe to evaluate
/// eagerly). Trivial expressions are already in WHNF or produce values with
/// no computation.
pub fn is_trivial_field(idx: usize, expr: &CoreExpr) -> bool {
    enum Work {
        Visit(usize),
        Combine(usize), // number of children just visited, to AND together
    }
    let mut stack = vec![Work::Visit(idx)];
    let mut results: Vec<bool> = Vec::new();
    while let Some(w) = stack.pop() {
        match w {
            Work::Visit(i) => match &expr.nodes[i] {
                CoreFrame::Var(_) | CoreFrame::Lit(_) | CoreFrame::Lam { .. } => results.push(true),
                CoreFrame::Con { fields, .. } => {
                    stack.push(Work::Combine(fields.len()));
                    for &f in fields.iter().rev() {
                        stack.push(Work::Visit(f));
                    }
                }
                CoreFrame::PrimOp { .. } => results.push(false),
                _ => results.push(false), // App, Case, LetNonRec, LetRec, Join, Jump
            },
            Work::Combine(n) => {
                let start = results.len() - n;
                let all = results[start..].iter().all(|&b| b);
                results.truncate(start);
                results.push(all);
            }
        }
    }
    results.pop().unwrap_or(false)
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
    fn con_is_trivial_iff_all_fields_are() {
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
        assert!(!is_trivial_field(1, &expr));
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
