//! Dead code elimination pass for Core expressions.

use crate::occ::{get_occ, occ_analysis, Occ};
use tidepool_eval::{Changed, Pass};
use tidepool_repr::{get_children, replace_subtree, CoreExpr, CoreFrame};

/// Dead Code Elimination pass.
/// Removes `LetNonRec` bindings where the binder is unused.
/// Removes `LetRec` groups where all binders are unused.
pub struct Dce;

impl Pass for Dce {
    fn run(&self, expr: &mut CoreExpr) -> Changed {
        if expr.nodes.is_empty() {
            return false;
        }
        let occ_map = occ_analysis(expr);
        match try_dce(expr, &occ_map) {
            Some(new_expr) => {
                *expr = new_expr;
                true
            }
            None => false,
        }
    }

    fn name(&self) -> &str {
        "Dce"
    }
}

fn try_dce(expr: &CoreExpr, occ_map: &crate::occ::OccMap) -> Option<CoreExpr> {
    try_dce_at(expr, expr.nodes.len() - 1, occ_map)
}

fn try_dce_at(expr: &CoreExpr, idx: usize, occ_map: &crate::occ::OccMap) -> Option<CoreExpr> {
    match &expr.nodes[idx] {
        CoreFrame::LetNonRec { binder, body, .. } => {
            if get_occ(occ_map, *binder) == Occ::Dead {
                // Drop the binding, keep just body
                let body_tree = expr.extract_subtree(*body);
                Some(replace_subtree(expr, idx, &body_tree))
            } else {
                try_children(expr, idx, occ_map)
            }
        }
        CoreFrame::LetRec { bindings, body } => {
            let all_dead = bindings
                .iter()
                .all(|(binder, _)| get_occ(occ_map, *binder) == Occ::Dead);
            if all_dead {
                // Drop the entire group, keep just body
                let body_tree = expr.extract_subtree(*body);
                Some(replace_subtree(expr, idx, &body_tree))
            } else {
                try_children(expr, idx, occ_map)
            }
        }
        _ => try_children(expr, idx, occ_map),
    }
}

fn try_children(expr: &CoreExpr, idx: usize, occ_map: &crate::occ::OccMap) -> Option<CoreExpr> {
    get_children(&expr.nodes[idx])
        .into_iter()
        .find_map(|child| try_dce_at(expr, child, occ_map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_eval::{eval, Env, VecHeap};
    use tidepool_repr::{Literal, VarId};

    // Helper to build a tree
    fn tree(nodes: Vec<CoreFrame<usize>>) -> CoreExpr {
        CoreExpr { nodes }
    }

    // 1. test_dce_dead_let: let x = 42 in 0 -> 0. Binder Dead, dropped.
    #[test]
    fn test_dce_dead_let() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0: rhs
            CoreFrame::Lit(Literal::LitInt(0)),  // 1: body
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            }, // 2: root
        ]);
        let mut dce_expr = expr;
        let changed = Dce.run(&mut dce_expr);
        assert!(changed);
        assert_eq!(dce_expr.nodes.len(), 1);
        assert_eq!(dce_expr.nodes[0], CoreFrame::Lit(Literal::LitInt(0)));
    }

    // 2. test_dce_live_let_preserved: let x = 42 in x -> unchanged. Binder Once, kept.
    #[test]
    fn test_dce_live_let_preserved() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0: rhs
            CoreFrame::Var(x),                   // 1: body
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            }, // 2: root
        ]);
        let mut dce_expr = expr.clone();
        let changed = Dce.run(&mut dce_expr);
        assert!(!changed);
        assert_eq!(dce_expr, expr);
    }

    // 3. test_dce_letrec_all_dead: letrec { f = 1; g = 2 } in 0 -> 0. All Dead, drop entire group.
    #[test]
    fn test_dce_letrec_all_dead() {
        let f = VarId(1);
        let g = VarId(2);
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0: f's rhs
            CoreFrame::Lit(Literal::LitInt(2)), // 1: g's rhs
            CoreFrame::Lit(Literal::LitInt(0)), // 2: body
            CoreFrame::LetRec {
                bindings: vec![(f, 0), (g, 1)],
                body: 2,
            }, // 3: root
        ]);
        let mut dce_expr = expr;
        let changed = Dce.run(&mut dce_expr);
        assert!(changed);
        assert_eq!(dce_expr.nodes.len(), 1);
        assert_eq!(dce_expr.nodes[0], CoreFrame::Lit(Literal::LitInt(0)));
    }

    // 4. test_dce_letrec_one_live: letrec { f = g; g = 1 } in f -> unchanged.
    // f is Once (live), keep entire group even though g might be referenced only by f.
    #[test]
    fn test_dce_letrec_one_live() {
        let f = VarId(1);
        let g = VarId(2);
        let expr = tree(vec![
            CoreFrame::Var(g),                  // 0: f's rhs
            CoreFrame::Lit(Literal::LitInt(1)), // 1: g's rhs
            CoreFrame::Var(f),                  // 2: body
            CoreFrame::LetRec {
                bindings: vec![(f, 0), (g, 1)],
                body: 2,
            }, // 3: root
        ]);
        let mut dce_expr = expr.clone();
        let changed = Dce.run(&mut dce_expr);
        assert!(!changed);
        assert_eq!(dce_expr, expr);
    }

    // 5. test_dce_nested: let x = 42 in let y = 0 in x -> after DCE drops y's let, result is let x = 42 in x.
    #[test]
    fn test_dce_nested() {
        let x = VarId(1);
        let y = VarId(2);
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0: x's rhs
            CoreFrame::Lit(Literal::LitInt(0)),  // 1: y's rhs
            CoreFrame::Var(x),                   // 2: y's body
            CoreFrame::LetNonRec {
                binder: y,
                rhs: 1,
                body: 2,
            }, // 3: x's body
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 3,
            }, // 4: root
        ]);
        let mut dce_expr = expr;
        let changed = Dce.run(&mut dce_expr);
        assert!(changed);
        // Should have dropped y
        // let x = 42 in x
        assert_eq!(dce_expr.nodes.len(), 3);
        // The root should be a LetNonRec for x
        let root_idx = dce_expr.nodes.len() - 1;
        let CoreFrame::LetNonRec { binder, .. } = &dce_expr.nodes[root_idx] else {
            panic!(
                "Root should be LetNonRec for x, got {:?}",
                dce_expr.nodes[root_idx]
            );
        };
        assert_eq!(*binder, x);
    }

    // 6. test_dce_preserves_eval: let x = 42 in let y = 99 in x -> eval before/after, verify match.
    #[test]
    fn test_dce_preserves_eval() {
        let x = VarId(1);
        let y = VarId(2);
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0: x's rhs
            CoreFrame::Lit(Literal::LitInt(99)), // 1: y's rhs
            CoreFrame::Var(x),                   // 2: y's body
            CoreFrame::LetNonRec {
                binder: y,
                rhs: 1,
                body: 2,
            }, // 3: x's body
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 3,
            }, // 4: root
        ]);
        let mut dce_expr = expr.clone();

        let mut heap = VecHeap::new();
        let env = Env::new();

        let val_orig = eval(&expr, &env, &mut heap).expect("Original eval failed");

        let changed = Dce.run(&mut dce_expr);
        assert!(changed);

        let val_dce = eval(&dce_expr, &env, &mut heap).expect("DCE eval failed");

        match (val_orig, val_dce) {
            (tidepool_eval::Value::Lit(l1), tidepool_eval::Value::Lit(l2)) => assert_eq!(l1, l2),
            _ => panic!("Expected literals"),
        }
    }

    // 7. test_dce_letrec_mixed_liveness: letrec { f = 100; g = 200 } in f -> unchanged.
    // g is Dead, but f is Once. The entire group must be kept because DCE currently
    // only drops the entire LetRec group if ALL binders are dead.
    // If it used .any() instead of .all(), it would incorrectly drop the whole group.
    #[test]
    fn test_dce_letrec_mixed_liveness() {
        let f = VarId(1);
        let g = VarId(2);
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(100)), // 0: f's rhs
            CoreFrame::Lit(Literal::LitInt(200)), // 1: g's rhs
            CoreFrame::Var(f),                    // 2: body
            CoreFrame::LetRec {
                bindings: vec![(f, 0), (g, 1)],
                body: 2,
            }, // 3: root
        ]);
        let mut dce_expr = expr.clone();

        let mut heap = VecHeap::new();
        let env = Env::new();

        // 1. Original evaluates to 100
        let val_orig = eval(&expr, &env, &mut heap).expect("Original eval failed");
        let tidepool_eval::Value::Lit(Literal::LitInt(n)) = val_orig else {
            panic!("Original should eval to 100, got {:?}", val_orig);
        };
        assert_eq!(n, 100);

        // 2. DCE should NOT drop the group because f is live
        let changed = Dce.run(&mut dce_expr);
        assert!(
            !changed,
            "DCE should not have changed the expression because f is live"
        );
        assert_eq!(dce_expr, expr);

        // 3. Evaluates correctly after (no-op) DCE
        let val_dce = eval(&dce_expr, &env, &mut heap).expect("DCE eval failed");
        let tidepool_eval::Value::Lit(Literal::LitInt(n2)) = val_dce else {
            panic!(
                "Result after DCE should still eval to 100, got {:?}",
                val_dce
            );
        };
        assert_eq!(n2, 100);
    }
}
