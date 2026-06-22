//! Beta reduction pass for Core expressions.

use tidepool_eval::{Changed, Pass};
use tidepool_repr::{replace_subtree, CoreExpr, CoreFrame};

/// Optimization pass: beta reduction.
///
/// Replaces function applications `(\x -> body) arg` with the body where
/// `x` is substituted for `arg`.
pub struct BetaReduce;

impl Pass for BetaReduce {
    fn run(&self, expr: &mut CoreExpr) -> Changed {
        crate::apply_rewrite(expr, try_beta_reduce)
    }

    fn name(&self) -> &str {
        "BetaReduce"
    }
}

fn try_beta_reduce(expr: &CoreExpr) -> Option<CoreExpr> {
    crate::rewrite::find_redex(expr, try_beta_at)
}

/// Beta redex test for a single node: an `App` whose function is a manifest
/// `Lam`. Non-redex nodes return `None`; the search driver handles descent.
fn try_beta_at(expr: &CoreExpr, idx: usize) -> Option<CoreExpr> {
    let CoreFrame::App { fun, arg } = &expr.nodes[idx] else {
        return None;
    };
    let CoreFrame::Lam { binder, body } = &expr.nodes[*fun] else {
        return None;
    };
    // Found a manifest beta redex!
    let body_tree = expr.extract_subtree(*body);
    let arg_tree = expr.extract_subtree(*arg);
    let substituted = tidepool_repr::subst::subst(&body_tree, *binder, &arg_tree);
    Some(replace_subtree(expr, idx, &substituted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_eval::{eval, Env, VecHeap};
    use tidepool_repr::{Literal, VarId};

    #[test]
    fn test_beta_identity() {
        // (λx.x) 42 → 42
        let x = VarId(1);
        let nodes = vec![
            CoreFrame::Var(x),                     // 0: x
            CoreFrame::Lam { binder: x, body: 0 }, // 1: λx.x
            CoreFrame::Lit(Literal::LitInt(42)),   // 2: 42
            CoreFrame::App { fun: 1, arg: 2 },     // 3: (λx.x) 42
        ];
        let mut expr = CoreExpr { nodes };
        let pass = BetaReduce;
        let changed = pass.run(&mut expr);

        assert!(changed);
        assert_eq!(expr.nodes.len(), 1);
        assert_eq!(expr.nodes[0], CoreFrame::Lit(Literal::LitInt(42)));
    }

    #[test]
    fn test_beta_const() {
        // (λx.λy.x) 1 → λy.1
        let x = VarId(1);
        let y = VarId(2);
        let nodes = vec![
            CoreFrame::Var(x),                     // 0: x
            CoreFrame::Lam { binder: y, body: 0 }, // 1: λy.x
            CoreFrame::Lam { binder: x, body: 1 }, // 2: λx.λy.x
            CoreFrame::Lit(Literal::LitInt(1)),    // 3: 1
            CoreFrame::App { fun: 2, arg: 3 },     // 4: (λx.λy.x) 1
        ];
        let mut expr = CoreExpr { nodes };
        let pass = BetaReduce;
        let changed = pass.run(&mut expr);

        assert!(changed);
        // Result should be λy.1
        let root = expr.nodes.len() - 1;
        let CoreFrame::Lam { binder, body } = &expr.nodes[root] else {
            panic!("Result should be Lam, got {:?}", expr.nodes[root]);
        };
        assert_eq!(*binder, y);
        let CoreFrame::Lit(Literal::LitInt(1)) = &expr.nodes[*body] else {
            panic!("Body should be 1, got {:?}", expr.nodes[*body]);
        };
    }

    #[test]
    fn test_beta_no_redex() {
        // (λx.x)
        let x = VarId(1);
        let nodes = vec![
            CoreFrame::Var(x),                     // 0: x
            CoreFrame::Lam { binder: x, body: 0 }, // 1: λx.x
        ];
        let mut expr = CoreExpr { nodes };
        let pass = BetaReduce;
        let changed = pass.run(&mut expr);
        assert!(!changed);
    }

    #[test]
    fn test_beta_capture_avoiding() {
        // (λx.λy.x) y → λy'.y (y' fresh)
        let x = VarId(1);
        let y = VarId(2);
        let nodes = vec![
            CoreFrame::Var(x),                     // 0: x
            CoreFrame::Lam { binder: y, body: 0 }, // 1: λy.x
            CoreFrame::Lam { binder: x, body: 1 }, // 2: λx.λy.x
            CoreFrame::Var(y),                     // 3: y
            CoreFrame::App { fun: 2, arg: 3 },     // 4: (λx.λy.x) y
        ];
        let mut expr = CoreExpr { nodes };
        let pass = BetaReduce;
        let changed = pass.run(&mut expr);

        assert!(changed);
        let root = expr.nodes.len() - 1;
        let CoreFrame::Lam { binder, body } = &expr.nodes[root] else {
            panic!("Result should be Lam");
        };
        assert_ne!(*binder, y); // Should be renamed
        let CoreFrame::Var(v) = &expr.nodes[*body] else {
            panic!("Body should be Var(y)");
        };
        assert_eq!(*v, y); // Should refer to the free y
    }

    #[test]
    fn test_beta_preserves_eval() {
        // (λx. x + x) 21
        let x = VarId(1);
        let nodes = vec![
            CoreFrame::Var(x), // 0: x
            CoreFrame::PrimOp {
                op: tidepool_repr::PrimOpKind::IntAdd,
                args: vec![0, 0],
            }, // 1: x + x
            CoreFrame::Lam { binder: x, body: 1 }, // 2: λx. x + x
            CoreFrame::Lit(Literal::LitInt(21)), // 3: 21
            CoreFrame::App { fun: 2, arg: 3 }, // 4: (λx. x + x) 21
        ];
        let expr_orig = CoreExpr { nodes };
        let mut expr_reduced = expr_orig.clone();
        let pass = BetaReduce;
        pass.run(&mut expr_reduced);

        let mut heap = VecHeap::new();
        let env = Env::new();

        let val_orig = eval(&expr_orig, &env, &mut heap).expect("Original eval failed");
        let val_reduced = eval(&expr_reduced, &env, &mut heap).expect("Reduced eval failed");

        let (tidepool_eval::Value::Lit(l1), tidepool_eval::Value::Lit(l2)) =
            (&val_orig, &val_reduced)
        else {
            panic!(
                "Expected literal results, got {:?} and {:?}",
                val_orig, val_reduced
            );
        };
        assert_eq!(l1, l2);

        let tidepool_eval::Value::Lit(Literal::LitInt(n)) = val_orig else {
            panic!("Expected 42");
        };
        assert_eq!(n, 42);
    }

    #[test]
    fn test_beta_nested() {
        // (λx.x) ((λy.y) 42)
        let x = VarId(1);
        let y = VarId(2);
        let nodes = vec![
            CoreFrame::Var(y),                     // 0: y
            CoreFrame::Lam { binder: y, body: 0 }, // 1: λy.y
            CoreFrame::Lit(Literal::LitInt(42)),   // 2: 42
            CoreFrame::App { fun: 1, arg: 2 },     // 3: (λy.y) 42
            CoreFrame::Var(x),                     // 4: x
            CoreFrame::Lam { binder: x, body: 4 }, // 5: λx.x
            CoreFrame::App { fun: 5, arg: 3 },     // 6: (λx.x) ((λy.y) 42)
        ];
        let mut expr = CoreExpr { nodes };
        let pass = BetaReduce;

        // Run until fixpoint
        while pass.run(&mut expr) {}

        assert_eq!(expr.nodes.len(), 1);
        assert_eq!(expr.nodes[0], CoreFrame::Lit(Literal::LitInt(42)));
    }
}
