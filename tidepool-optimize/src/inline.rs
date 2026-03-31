//! Inlining pass for Core expressions.

use crate::occ::{get_occ, occ_analysis, Occ};
use tidepool_eval::{Changed, Pass};
use tidepool_repr::{get_children, replace_subtree, CoreExpr, CoreFrame};

/// Inlining pass: eliminates single-use `LetNonRec` bindings by substituting the RHS directly at the use site.
pub struct Inline;

impl Pass for Inline {
    fn run(&self, expr: &mut CoreExpr) -> Changed {
        if expr.nodes.is_empty() {
            return false;
        }
        let occ_map = occ_analysis(expr);
        match try_inline(expr, &occ_map) {
            Some(new_expr) => {
                *expr = new_expr;
                true
            }
            None => false,
        }
    }

    fn name(&self) -> &str {
        "Inline"
    }
}

fn try_inline(expr: &CoreExpr, occ_map: &crate::occ::OccMap) -> Option<CoreExpr> {
    try_inline_at(expr, expr.nodes.len() - 1, occ_map)
}

fn try_inline_at(expr: &CoreExpr, idx: usize, occ_map: &crate::occ::OccMap) -> Option<CoreExpr> {
    match &expr.nodes[idx] {
        CoreFrame::LetNonRec { binder, rhs, body } => {
            if get_occ(occ_map, *binder) == Occ::Once {
                // Inline: substitute binder -> rhs in body
                let body_tree = expr.extract_subtree(*body);
                let rhs_tree = expr.extract_subtree(*rhs);
                let inlined = tidepool_repr::subst::subst(&body_tree, *binder, &rhs_tree);
                Some(replace_subtree(expr, idx, &inlined))
            } else {
                // Try children
                try_inline_at(expr, *rhs, occ_map).or_else(|| try_inline_at(expr, *body, occ_map))
            }
        }
        // Never inline LetRec, even if Once (it might be recursive via own RHS)
        _ => try_children(expr, idx, occ_map),
    }
}

fn try_children(expr: &CoreExpr, idx: usize, occ_map: &crate::occ::OccMap) -> Option<CoreExpr> {
    get_children(&expr.nodes[idx])
        .into_iter()
        .find_map(|child| try_inline_at(expr, child, occ_map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_eval::{eval, Env, VecHeap};
    use tidepool_repr::{Literal, PrimOpKind, VarId};

    fn tree(nodes: Vec<CoreFrame<usize>>) -> CoreExpr {
        CoreExpr { nodes }
    }

    // 1. let x = 42 in x -> 42. Binder Once, inlined.
    #[test]
    fn test_inline_single_use() {
        let x = VarId(1);
        let mut expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Var(x),                   // 1
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            }, // 2
        ]);
        let pass = Inline;
        let changed = pass.run(&mut expr);
        assert!(changed);
        assert_eq!(expr.nodes.len(), 1);
        assert_eq!(expr.nodes[0], CoreFrame::Lit(Literal::LitInt(42)));
    }

    // 2. let x = 42 in x + x -> unchanged. Binder Many, not inlined.
    #[test]
    fn test_inline_multi_use_preserved() {
        let x = VarId(1);
        let mut expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Var(x),                   // 1
            CoreFrame::Var(x),                   // 2
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![1, 2],
            }, // 3
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 3,
            }, // 4
        ]);
        let pass = Inline;
        let changed = pass.run(&mut expr);
        assert!(!changed);
    }

    // 3. let x = 42 in 0 -> unchanged by inline (DCE will handle dead bindings).
    #[test]
    fn test_inline_dead_preserved() {
        let x = VarId(1);
        let mut expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Lit(Literal::LitInt(0)),  // 1
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            }, // 2
        ]);
        let pass = Inline;
        let changed = pass.run(&mut expr);
        assert!(!changed);
    }

    // 4. let x = 1 in let y = x in y -> after two passes: 1.
    #[test]
    fn test_inline_nested() {
        let x = VarId(1);
        let y = VarId(2);
        let mut expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Var(x),                  // 1
            CoreFrame::Var(y),                  // 2
            CoreFrame::LetNonRec {
                binder: y,
                rhs: 1,
                body: 2,
            }, // 3
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 3,
            }, // 4
        ]);
        let pass = Inline;

        // Pass 1: inline x = 1 (outer let), producing: let y = 1 in y
        assert!(pass.run(&mut expr));
        // Pass 2: inline y = 1 (inner let), producing: 1
        assert!(pass.run(&mut expr));
        // Result should be 1
        assert_eq!(expr.nodes.len(), 1);
        assert_eq!(expr.nodes[0], CoreFrame::Lit(Literal::LitInt(1)));
    }

    // 5. letrec f = f in f -> unchanged. LetRec binder Once but must NOT inline.
    #[test]
    fn test_inline_letrec_not_inlined() {
        let f = VarId(1);
        let mut expr = tree(vec![
            CoreFrame::Var(f), // 0
            CoreFrame::Var(f), // 1
            CoreFrame::LetRec {
                bindings: vec![(f, 0)],
                body: 1,
            }, // 2
        ]);
        let pass = Inline;
        let changed = pass.run(&mut expr);
        assert!(!changed);
    }

    // 6. let x = y in \y. x -> \y'. y (fresh y').
    #[test]
    fn test_inline_capture_avoiding() {
        let x = VarId(1);
        let y = VarId(2);
        let mut expr = tree(vec![
            CoreFrame::Var(y),                     // 0: rhs
            CoreFrame::Var(x),                     // 1
            CoreFrame::Lam { binder: y, body: 1 }, // 2: body
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 2,
            }, // 3
        ]);
        let pass = Inline;
        let changed = pass.run(&mut expr);
        assert!(changed);

        // Result should be \y'. y
        let root = expr.nodes.len() - 1;
        let CoreFrame::Lam { binder, body } = &expr.nodes[root] else {
            panic!("Result should be Lam");
        };
        assert_ne!(*binder, y);
        let CoreFrame::Var(v) = &expr.nodes[*body] else {
            panic!("Body should be Var(y)");
        };
        assert_eq!(*v, y);
    }

    // 7. test_inline_preserves_eval: Build let x = 21 in x + x (Many, no inline) and let x = 21 in x (Once, inline). Eval before/after, verify match.
    #[test]
    fn test_inline_preserves_eval() {
        let x = VarId(1);

        // Case A: Once (should inline)
        let expr_once = tree(vec![
            CoreFrame::Lit(Literal::LitInt(21)),
            CoreFrame::Var(x),
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            },
        ]);
        let mut expr_once_reduced = expr_once.clone();
        Inline.run(&mut expr_once_reduced);

        let mut heap = VecHeap::new();
        let env = Env::new();
        let v1 = eval(&expr_once, &env, &mut heap).unwrap();
        let v2 = eval(&expr_once_reduced, &env, &mut heap).unwrap();
        match (v1, v2) {
            (tidepool_eval::Value::Lit(l1), tidepool_eval::Value::Lit(l2)) => assert_eq!(l1, l2),
            _ => panic!("Expected literals"),
        }

        // Case B: Many (should NOT inline)
        let mut expr_many = tree(vec![
            CoreFrame::Lit(Literal::LitInt(21)),
            CoreFrame::Var(x),
            CoreFrame::Var(x),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![1, 2],
            },
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 3,
            },
        ]);
        let expr_many_orig = expr_many.clone();
        Inline.run(&mut expr_many);
        assert_eq!(expr_many, expr_many_orig);
    }
}
