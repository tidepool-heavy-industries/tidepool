//! "Trivial constructor field" analysis: is the expression at a node index
//! already in WHNF, or built entirely from trivial parts, so it is safe for a
//! backend to evaluate it EAGERLY when constructing a `Con` instead of
//! allocating a thunk?
//!
//! Both `tidepool-eval` (the oracle) and `tidepool-codegen` (the JIT) call
//! this ONE predicate and must get the same answer for every node, or a Con
//! field is forced by one backend and left lazy by the other — a real
//! semantic divergence, not just duplicated code (see
//! `tidepool-codegen/tests/raise_con_field_trivial_differential.rs`, which
//! pins the case that actually diverged: `PrimOp Raise` with a trivial
//! argument was classified trivial by an args-only rule, forcing the field —
//! and raising — merely from constructing the `Con`).
//!
//! Explicit work-stack, not recursion: a chain of nested trivial
//! `Con`/`PrimOp` wrappers (e.g. deep tree spines built by proptests) walks
//! this predicate before any evaluator/emitter touches the node, so it must
//! be stack-safe on its own.

use crate::types::PrimOpKind;
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
                // A `raise#` must stay lazy even with a trivial arg: `let x =
                // raise# e in if False then x else 0` must return 0, not raise
                // eagerly (M2).
                CoreFrame::PrimOp {
                    op: PrimOpKind::Raise,
                    ..
                } => results.push(false),
                CoreFrame::PrimOp { args, .. } => {
                    stack.push(Work::Combine(args.len()));
                    for &a in args.iter().rev() {
                        stack.push(Work::Visit(a));
                    }
                }
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
    use crate::types::{Literal, VarId};
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
    fn non_raise_primop_is_trivial_iff_args_are() {
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            },
        ]);
        assert!(is_trivial_field(2, &expr));
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
