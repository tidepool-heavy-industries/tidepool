use tidepool_eval::{Changed, Pass};
use tidepool_repr::{replace_subtree, CoreExpr, CoreFrame};

/// Beta reduction pass: find `App { fun, arg }` where `fun` is a `Lam { binder, body }`.
/// Replaces it with `subst(body, binder, arg)`.
pub struct BetaReduce;

impl Pass for BetaReduce {
    fn run(&self, expr: &mut CoreExpr) -> Changed {
        if expr.nodes.is_empty() {
            return false;
        }
        match try_beta_reduce(expr) {
            Some(new_expr) => {
                *expr = new_expr;
                true
            }
            None => false,
        }
    }

    fn name(&self) -> &str {
        "BetaReduce"
    }
}

fn try_beta_reduce(expr: &CoreExpr) -> Option<CoreExpr> {
    // Start from root (last node)
    try_beta_at(expr, expr.nodes.len() - 1)
}

fn try_beta_at(expr: &CoreExpr, idx: usize) -> Option<CoreExpr> {
    match &expr.nodes[idx] {
        CoreFrame::App { fun, arg } => {
            // Check if fun is a Lam
            if let CoreFrame::Lam { binder, body } = &expr.nodes[*fun] {
                // Found a manifest beta redex!
                let body_tree = expr.extract_subtree(*body);
                let arg_tree = expr.extract_subtree(*arg);
                let substituted = tidepool_repr::subst::subst(&body_tree, *binder, &arg_tree);
                Some(replace_subtree(expr, idx, &substituted))
            } else {
                // Try to find redex in children
                try_beta_at(expr, *fun).or_else(|| try_beta_at(expr, *arg))
            }
        }
        // For other nodes, try each child
        other => {
            let mut result = None;
            // We need to visit children. Since map_layer is for remapping indices,
            // we can use it to "visit" indices if we are careful.
            // But it's easier to just match on the frame and visit children.
            match other {
                CoreFrame::Var(_) | CoreFrame::Lit(_) => {}
                CoreFrame::App { .. } => {
                    // App nodes are handled in the outer match — this arm should never fire.
                    return None;
                }
                CoreFrame::Lam { body, .. } => {
                    result = try_beta_at(expr, *body);
                }
                CoreFrame::LetNonRec { rhs, body, .. } => {
                    result = try_beta_at(expr, *rhs).or_else(|| try_beta_at(expr, *body));
                }
                CoreFrame::LetRec { bindings, body } => {
                    for (_, rhs) in bindings {
                        result = try_beta_at(expr, *rhs);
                        if result.is_some() {
                            break;
                        }
                    }
                    if result.is_none() {
                        result = try_beta_at(expr, *body);
                    }
                }
                CoreFrame::Case {
                    scrutinee, alts, ..
                } => {
                    result = try_beta_at(expr, *scrutinee);
                    if result.is_none() {
                        for alt in alts {
                            result = try_beta_at(expr, alt.body);
                            if result.is_some() {
                                break;
                            }
                        }
                    }
                }
                CoreFrame::Con { fields, .. } => {
                    for field in fields {
                        result = try_beta_at(expr, *field);
                        if result.is_some() {
                            break;
                        }
                    }
                }
                CoreFrame::Join { rhs, body, .. } => {
                    result = try_beta_at(expr, *rhs).or_else(|| try_beta_at(expr, *body));
                }
                CoreFrame::Jump { args, .. } => {
                    for arg in args {
                        result = try_beta_at(expr, *arg);
                        if result.is_some() {
                            break;
                        }
                    }
                }
                CoreFrame::PrimOp { args, .. } => {
                    for arg in args {
                        result = try_beta_at(expr, *arg);
                        if result.is_some() {
                            break;
                        }
                    }
                }
            }
            result
        }
    }
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
        if let CoreFrame::Lam { binder, body } = &expr.nodes[root] {
            assert_eq!(*binder, y);
            if let CoreFrame::Lit(Literal::LitInt(1)) = &expr.nodes[*body] {
                // OK
            } else {
                panic!("Body should be 1, got {:?}", expr.nodes[*body]);
            }
        } else {
            panic!("Result should be Lam, got {:?}", expr.nodes[root]);
        }
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
        if let CoreFrame::Lam { binder, body } = &expr.nodes[root] {
            assert_ne!(*binder, y); // Should be renamed
            if let CoreFrame::Var(v) = &expr.nodes[*body] {
                assert_eq!(*v, y); // Should refer to the free y
            } else {
                panic!("Body should be Var(y)");
            }
        } else {
            panic!("Result should be Lam");
        }
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

        if let (tidepool_eval::Value::Lit(l1), tidepool_eval::Value::Lit(l2)) =
            (&val_orig, &val_reduced)
        {
            assert_eq!(l1, l2);
        } else {
            panic!(
                "Expected literal results, got {:?} and {:?}",
                val_orig, val_reduced
            );
        }

        if let tidepool_eval::Value::Lit(Literal::LitInt(n)) = val_orig {
            assert_eq!(n, 42);
        } else {
            panic!("Expected 42");
        }
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
