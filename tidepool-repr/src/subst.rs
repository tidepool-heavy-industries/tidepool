//! Capture-avoiding substitution for Tidepool IR.

use crate::free_vars::free_vars;
use crate::tree::MapLayer;
use crate::{CoreExpr, CoreFrame, RecursiveTree, VarId};
use rustc_hash::{FxHashMap, FxHashSet};

/// Substitute `replacement` for `target` in `tree`. Returns a new tree.
/// Capture-avoiding: renames binders that would capture free vars in the replacement.
pub fn subst(tree: &CoreExpr, target: VarId, replacement: &CoreExpr) -> CoreExpr {
    if tree.nodes.is_empty() {
        return tree.clone();
    }

    let fvs_replacement = free_vars(replacement);
    let max_tree = find_max_var_id(tree);
    let max_replacement = find_max_var_id(replacement);
    let mut max_id = VarId(max_tree.0.max(max_replacement.0));
    let mut next_id = move || {
        max_id.0 += 1;
        max_id
    };

    let mut new_nodes = Vec::new();
    let mut ctx = SubstCtx {
        target,
        replacement,
        fvs_replacement: &fvs_replacement,
        next_id: &mut next_id,
        new_nodes: &mut new_nodes,
    };

    subst_at(tree, tree.nodes.len() - 1, &mut ctx, &FxHashMap::default());

    RecursiveTree { nodes: new_nodes }
}

struct SubstCtx<'a> {
    target: VarId,
    replacement: &'a CoreExpr,
    fvs_replacement: &'a FxHashSet<VarId>,
    next_id: &'a mut dyn FnMut() -> VarId,
    new_nodes: &'a mut Vec<CoreFrame<usize>>,
}

fn find_max_var_id(tree: &CoreExpr) -> VarId {
    let mut max = VarId(0);
    for node in &tree.nodes {
        match node {
            CoreFrame::Var(v) => max = VarId(max.0.max(v.0)),
            CoreFrame::Lam { binder, .. } => max = VarId(max.0.max(binder.0)),
            CoreFrame::LetNonRec { binder, .. } => max = VarId(max.0.max(binder.0)),
            CoreFrame::LetRec { bindings, .. } => {
                for (v, _) in bindings {
                    max = VarId(max.0.max(v.0));
                }
            }
            CoreFrame::Case { binder, alts, .. } => {
                max = VarId(max.0.max(binder.0));
                for alt in alts {
                    for b in &alt.binders {
                        max = VarId(max.0.max(b.0));
                    }
                }
            }
            CoreFrame::Join { params, .. } => {
                for p in params {
                    max = VarId(max.0.max(p.0));
                }
            }
            _ => {}
        }
    }
    max
}

/// Recursive helper for substitution.
/// `env` maps binders that have been renamed (due to capture avoidance) to their new IDs.
fn subst_at(tree: &CoreExpr, idx: usize, ctx: &mut SubstCtx, env: &FxHashMap<VarId, VarId>) -> usize {
    match &tree.nodes[idx] {
        CoreFrame::Var(v) => {
            let actual_v = env.get(v).copied().unwrap_or(*v);
            if actual_v == ctx.target {
                // Splice replacement
                splice_tree(ctx.replacement, ctx.new_nodes)
            } else {
                let new_idx = ctx.new_nodes.len();
                ctx.new_nodes.push(CoreFrame::Var(actual_v));
                new_idx
            }
        }
        CoreFrame::Lit(l) => {
            let new_idx = ctx.new_nodes.len();
            ctx.new_nodes.push(CoreFrame::Lit(l.clone()));
            new_idx
        }
        CoreFrame::Lam { binder, body } => {
            let actual_binder = env.get(binder).copied().unwrap_or(*binder);
            if actual_binder == ctx.target {
                // target is shadowed, just copy the subtree with renamed binders (if any)
                copy_with_env(tree, idx, ctx.new_nodes, env)
            } else if ctx.fvs_replacement.contains(&actual_binder) {
                // Capture would occur, rename binder
                let fresh = (ctx.next_id)();
                let mut new_env = env.clone();
                new_env.insert(*binder, fresh);
                let new_body = subst_at(tree, *body, ctx, &new_env);
                let new_idx = ctx.new_nodes.len();
                ctx.new_nodes.push(CoreFrame::Lam {
                    binder: fresh,
                    body: new_body,
                });
                new_idx
            } else {
                let new_body = subst_at(tree, *body, ctx, env);
                let new_idx = ctx.new_nodes.len();
                ctx.new_nodes.push(CoreFrame::Lam {
                    binder: actual_binder,
                    body: new_body,
                });
                new_idx
            }
        }
        CoreFrame::LetNonRec { binder, rhs, body } => {
            let actual_binder = env.get(binder).copied().unwrap_or(*binder);
            let new_rhs = subst_at(tree, *rhs, ctx, env);

            if actual_binder == ctx.target {
                // target is shadowed in body
                let new_body = copy_with_env(tree, *body, ctx.new_nodes, env);
                let new_idx = ctx.new_nodes.len();
                ctx.new_nodes.push(CoreFrame::LetNonRec {
                    binder: actual_binder,
                    rhs: new_rhs,
                    body: new_body,
                });
                new_idx
            } else if ctx.fvs_replacement.contains(&actual_binder) {
                let fresh = (ctx.next_id)();
                let mut new_env = env.clone();
                new_env.insert(*binder, fresh);
                let new_body = subst_at(tree, *body, ctx, &new_env);
                let new_idx = ctx.new_nodes.len();
                ctx.new_nodes.push(CoreFrame::LetNonRec {
                    binder: fresh,
                    rhs: new_rhs,
                    body: new_body,
                });
                new_idx
            } else {
                let new_body = subst_at(tree, *body, ctx, env);
                let new_idx = ctx.new_nodes.len();
                ctx.new_nodes.push(CoreFrame::LetNonRec {
                    binder: actual_binder,
                    rhs: new_rhs,
                    body: new_body,
                });
                new_idx
            }
        }
        CoreFrame::LetRec { bindings, body } => {
            let mut binders: Vec<VarId> = bindings.iter().map(|(v, _)| *v).collect();
            let mut shadow = false;
            let mut needs_rename = false;
            let mut new_env = env.clone();

            for b in &binders {
                let actual_b = env.get(b).copied().unwrap_or(*b);
                if actual_b == ctx.target {
                    shadow = true;
                }
                if ctx.fvs_replacement.contains(&actual_b) {
                    needs_rename = true;
                }
            }

            if shadow {
                // Just copy the whole LetRec with environment renaming
                copy_with_env(tree, idx, ctx.new_nodes, env)
            } else if needs_rename {
                for b in &mut binders {
                    let actual_b = env.get(b).copied().unwrap_or(*b);
                    if ctx.fvs_replacement.contains(&actual_b) {
                        let fresh = (ctx.next_id)();
                        new_env.insert(*b, fresh);
                        *b = fresh;
                    } else {
                        *b = actual_b;
                    }
                }
                let mut new_bindings = Vec::new();
                for (i, (_, rhs)) in bindings.iter().enumerate() {
                    let new_rhs = subst_at(tree, *rhs, ctx, &new_env);
                    new_bindings.push((binders[i], new_rhs));
                }
                let new_body = subst_at(tree, *body, ctx, &new_env);
                let new_idx = ctx.new_nodes.len();
                ctx.new_nodes.push(CoreFrame::LetRec {
                    bindings: new_bindings,
                    body: new_body,
                });
                new_idx
            } else {
                let mut new_bindings = Vec::new();
                for (v, rhs) in bindings {
                    let new_rhs = subst_at(tree, *rhs, ctx, env);
                    new_bindings.push((*v, new_rhs));
                }
                let new_body = subst_at(tree, *body, ctx, env);
                let new_idx = ctx.new_nodes.len();
                ctx.new_nodes.push(CoreFrame::LetRec {
                    bindings: new_bindings,
                    body: new_body,
                });
                new_idx
            }
        }
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            let actual_binder = env.get(binder).copied().unwrap_or(*binder);
            let new_scrutinee = subst_at(tree, *scrutinee, ctx, env);

            let mut case_env = env.clone();
            let mut final_case_binder = actual_binder;
            if ctx.fvs_replacement.contains(&actual_binder) {
                final_case_binder = (ctx.next_id)();
                case_env.insert(*binder, final_case_binder);
            }

            let mut final_alts = Vec::new();
            for alt in alts {
                let mut alt_shadow = actual_binder == ctx.target;
                let mut alt_env = case_env.clone();
                let mut new_pattern_binders = Vec::new();
                for b in &alt.binders {
                    let actual_b = case_env.get(b).copied().unwrap_or(*b);
                    if actual_b == ctx.target {
                        alt_shadow = true;
                    }
                    if ctx.fvs_replacement.contains(&actual_b) {
                        let fresh = (ctx.next_id)();
                        alt_env.insert(*b, fresh);
                        new_pattern_binders.push(fresh);
                    } else {
                        new_pattern_binders.push(actual_b);
                    }
                }
                let new_body = if alt_shadow {
                    copy_with_env(tree, alt.body, ctx.new_nodes, &alt_env)
                } else {
                    subst_at(tree, alt.body, ctx, &alt_env)
                };
                final_alts.push(crate::types::Alt {
                    con: alt.con.clone(),
                    binders: new_pattern_binders,
                    body: new_body,
                });
            }

            let new_idx = ctx.new_nodes.len();
            ctx.new_nodes.push(CoreFrame::Case {
                scrutinee: new_scrutinee,
                binder: final_case_binder,
                alts: final_alts,
            });
            new_idx
        }
        CoreFrame::Join {
            label,
            params,
            rhs,
            body,
        } => {
            let mut join_env = env.clone();
            let mut new_params = Vec::new();
            let mut shadow = false;
            for p in params {
                let actual_p = env.get(p).copied().unwrap_or(*p);
                if actual_p == ctx.target {
                    shadow = true;
                }
                if ctx.fvs_replacement.contains(&actual_p) {
                    let fresh = (ctx.next_id)();
                    join_env.insert(*p, fresh);
                    new_params.push(fresh);
                } else {
                    new_params.push(actual_p);
                }
            }

            let new_rhs = if shadow {
                copy_with_env(tree, *rhs, ctx.new_nodes, &join_env)
            } else {
                subst_at(tree, *rhs, ctx, &join_env)
            };

            let new_body = subst_at(tree, *body, ctx, env); // label doesn't shadow target

            let new_idx = ctx.new_nodes.len();
            ctx.new_nodes.push(CoreFrame::Join {
                label: *label,
                params: new_params,
                rhs: new_rhs,
                body: new_body,
            });
            new_idx
        }
        other => {
            // App, Con, Jump, PrimOp
            let mapped = other
                .clone()
                .map_layer(|child_idx| subst_at(tree, child_idx, ctx, env));
            let new_idx = ctx.new_nodes.len();
            ctx.new_nodes.push(mapped);
            new_idx
        }
    }
}

fn splice_tree(replacement: &CoreExpr, new_nodes: &mut Vec<CoreFrame<usize>>) -> usize {
    if replacement.nodes.is_empty() {
        // Empty replacement tree is a programming error; emit a dummy Var(0) node.
        eprintln!("WARNING: splice_tree called with empty replacement tree");
        let idx = new_nodes.len();
        new_nodes.push(CoreFrame::Var(VarId(0)));
        return idx;
    }
    let offset = new_nodes.len();
    for node in &replacement.nodes {
        let mapped = node.clone().map_layer(|idx| idx + offset);
        new_nodes.push(mapped);
    }
    new_nodes.len() - 1
}

fn copy_with_env(
    tree: &CoreExpr,
    idx: usize,
    new_nodes: &mut Vec<CoreFrame<usize>>,
    env: &FxHashMap<VarId, VarId>,
) -> usize {
    match &tree.nodes[idx] {
        CoreFrame::Var(v) => {
            let actual_v = env.get(v).copied().unwrap_or(*v);
            let new_idx = new_nodes.len();
            new_nodes.push(CoreFrame::Var(actual_v));
            new_idx
        }
        CoreFrame::Lam { binder, body } => {
            let actual_binder = env.get(binder).copied().unwrap_or(*binder);
            let new_body = copy_with_env(tree, *body, new_nodes, env);
            let new_idx = new_nodes.len();
            new_nodes.push(CoreFrame::Lam {
                binder: actual_binder,
                body: new_body,
            });
            new_idx
        }
        CoreFrame::LetNonRec { binder, rhs, body } => {
            let actual_binder = env.get(binder).copied().unwrap_or(*binder);
            let new_rhs = copy_with_env(tree, *rhs, new_nodes, env);
            let new_body = copy_with_env(tree, *body, new_nodes, env);
            let new_idx = new_nodes.len();
            new_nodes.push(CoreFrame::LetNonRec {
                binder: actual_binder,
                rhs: new_rhs,
                body: new_body,
            });
            new_idx
        }
        CoreFrame::LetRec { bindings, body } => {
            let mut new_bindings = Vec::new();
            for (v, rhs) in bindings {
                let actual_v = env.get(v).copied().unwrap_or(*v);
                let new_rhs = copy_with_env(tree, *rhs, new_nodes, env);
                new_bindings.push((actual_v, new_rhs));
            }
            let new_body = copy_with_env(tree, *body, new_nodes, env);
            let new_idx = new_nodes.len();
            new_nodes.push(CoreFrame::LetRec {
                bindings: new_bindings,
                body: new_body,
            });
            new_idx
        }
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            let actual_binder = env.get(binder).copied().unwrap_or(*binder);
            let new_scrutinee = copy_with_env(tree, *scrutinee, new_nodes, env);
            let mut new_alts = Vec::new();
            for alt in alts {
                let mut new_pattern_binders = Vec::new();
                for b in &alt.binders {
                    new_pattern_binders.push(env.get(b).copied().unwrap_or(*b));
                }
                let new_body = copy_with_env(tree, alt.body, new_nodes, env);
                new_alts.push(crate::types::Alt {
                    con: alt.con.clone(),
                    binders: new_pattern_binders,
                    body: new_body,
                });
            }
            let new_idx = new_nodes.len();
            new_nodes.push(CoreFrame::Case {
                scrutinee: new_scrutinee,
                binder: actual_binder,
                alts: new_alts,
            });
            new_idx
        }
        CoreFrame::Join {
            label,
            params,
            rhs,
            body,
        } => {
            let mut new_params = Vec::new();
            for p in params {
                new_params.push(env.get(p).copied().unwrap_or(*p));
            }
            let new_rhs = copy_with_env(tree, *rhs, new_nodes, env);
            let new_body = copy_with_env(tree, *body, new_nodes, env);
            let new_idx = new_nodes.len();
            new_nodes.push(CoreFrame::Join {
                label: *label,
                params: new_params,
                rhs: new_rhs,
                body: new_body,
            });
            new_idx
        }
        other => {
            let mapped = other
                .clone()
                .map_layer(|child_idx| copy_with_env(tree, child_idx, new_nodes, env));
            let new_idx = new_nodes.len();
            new_nodes.push(mapped);
            new_idx
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn leaf(frame: CoreFrame<usize>) -> CoreExpr {
        RecursiveTree { nodes: vec![frame] }
    }

    #[test]
    fn test_subst_simple() {
        let x = VarId(1);
        let tree = leaf(CoreFrame::Var(x));
        let replacement = leaf(CoreFrame::Lit(Literal::LitInt(42)));
        let result = subst(&tree, x, &replacement);
        assert_eq!(result, replacement);
    }

    #[test]
    fn test_subst_no_op() {
        let x = VarId(1);
        let y = VarId(2);
        let tree = leaf(CoreFrame::Var(y));
        let replacement = leaf(CoreFrame::Lit(Literal::LitInt(42)));
        let result = subst(&tree, x, &replacement);
        assert_eq!(result, tree);
    }

    #[test]
    fn test_subst_shadowing() {
        let x = VarId(1);
        let body = leaf(CoreFrame::Var(x));
        let mut nodes = body.nodes;
        let body_idx = nodes.len() - 1;
        nodes.push(CoreFrame::Lam {
            binder: x,
            body: body_idx,
        });
        let tree = RecursiveTree { nodes };

        let replacement = leaf(CoreFrame::Lit(Literal::LitInt(42)));
        let result = subst(&tree, x, &replacement);
        assert_eq!(result, tree);
    }

    #[test]
    fn test_subst_capture_avoiding() {
        let x = VarId(1);
        let y = VarId(2);
        // Lam(y, x)
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(x),                     // 0
                CoreFrame::Lam { binder: y, body: 0 }, // 1
            ],
        };
        // Var(y)
        let replacement = leaf(CoreFrame::Var(y));

        let result = subst(&tree, x, &replacement);

        // Result should be Lam(y', y) where y' is fresh
        if let CoreFrame::Lam { binder, body } = &result.nodes[result.nodes.len() - 1] {
            assert_ne!(*binder, y);
            assert_ne!(*binder, x);
            if let CoreFrame::Var(v) = &result.nodes[*body] {
                assert_eq!(*v, y);
            } else {
                panic!("Body should be Var(y)");
            }
        } else {
            panic!("Result should be Lam");
        }
    }

    #[test]
    fn test_subst_let_rec() {
        let x = VarId(1);
        let y = VarId(2);
        // LetRec [(x, Var(y))] Var(x)
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(y), // 0: rhs
                CoreFrame::Var(x), // 1: body
                CoreFrame::LetRec {
                    bindings: vec![(x, 0)],
                    body: 1,
                }, // 2
            ],
        };
        // Var(42)
        let replacement = leaf(CoreFrame::Lit(Literal::LitInt(42)));

        // Substitute x -> 42. Since x is bound in LetRec, it should be a no-op for body.
        // But y is free in rhs, let's substitute y -> 42.
        let result = subst(&tree, y, &replacement);

        if let CoreFrame::LetRec { bindings, .. } = &result.nodes[result.nodes.len() - 1] {
            if let CoreFrame::Lit(Literal::LitInt(42)) = &result.nodes[bindings[0].1] {
                // OK
            } else {
                panic!("rhs should be substituted");
            }
        } else {
            panic!("Result should be LetRec");
        }
    }

    #[test]
    fn test_subst_case_capture() {
        let x = VarId(1);
        let y = VarId(2);
        let z = VarId(3);
        // Case(x, y, [Default => z])
        // Let's substitute z -> Var(y). Case binder y should be renamed.
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(x), // 0: scrutinee
                CoreFrame::Var(z), // 1: alt body
                CoreFrame::Case {
                    scrutinee: 0,
                    binder: y,
                    alts: vec![Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 1,
                    }],
                }, // 2
            ],
        };
        let replacement = leaf(CoreFrame::Var(y));

        let result = subst(&tree, z, &replacement);

        if let CoreFrame::Case { binder, alts, .. } = &result.nodes[result.nodes.len() - 1] {
            assert_ne!(*binder, y);
            if let CoreFrame::Var(v) = &result.nodes[alts[0].body] {
                assert_eq!(*v, y);
            } else {
                panic!("Body should be Var(y)");
            }
        } else {
            panic!("Result should be Case");
        }
    }

    #[test]
    fn test_subst_join() {
        let x = VarId(1);
        // Join(j, [x], x, x)
        // Substitute x -> 42. x is bound in rhs, but NOT in body (if we follow Join scoping rules: params bound in rhs).
        // Wait, spec says: "Join label scopes over body (and rhs references label via Jump, not as free var)".
        // "params" are bound in RHS.
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(x), // 0: rhs
                CoreFrame::Var(x), // 1: body
                CoreFrame::Join {
                    label: JoinId(1),
                    params: vec![x],
                    rhs: 0,
                    body: 1,
                },
            ],
        };
        let replacement = leaf(CoreFrame::Lit(Literal::LitInt(42)));

        let result = subst(&tree, x, &replacement);

        if let CoreFrame::Join { rhs, body, .. } = &result.nodes[result.nodes.len() - 1] {
            // x in rhs is shadowed, should NOT be substituted
            if let CoreFrame::Var(v) = &result.nodes[*rhs] {
                assert_eq!(*v, x);
            } else {
                panic!("RHS x should be shadowed");
            }
            // x in body is NOT shadowed by params, should be substituted
            if let CoreFrame::Lit(Literal::LitInt(42)) = &result.nodes[*body] {
                // OK
            } else {
                panic!("Body x should be substituted");
            }
        } else {
            panic!("Result should be Join");
        }
    }

    #[test]
    fn test_subst_join_env_leak() {
        let x = VarId(1);
        let p = VarId(2);
        let j = JoinId(1);

        // Tree: Join j(p) = p in p
        // We substitute x -> p.
        // Param p MUST be renamed in RHS to avoid potentially capturing p in the replacement
        // (the substitution algorithm renames all binders that conflict with free variables of the replacement).
        // But it MUST NOT be renamed in the body, as the body is outside the scope of Join params.

        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(p), // 0: rhs
                CoreFrame::Var(p), // 1: body
                CoreFrame::Join {
                    label: j,
                    params: vec![p],
                    rhs: 0,
                    body: 1,
                },
            ],
        };
        let replacement = leaf(CoreFrame::Var(p));

        let result = subst(&tree, x, &replacement);

        if let CoreFrame::Join {
            params, rhs, body, ..
        } = &result.nodes[result.nodes.len() - 1]
        {
            let p_fresh = params[0];
            assert_ne!(
                p_fresh, p,
                "Parameter p should have been renamed because it exists in fvs_replacement"
            );

            // RHS: Var(p) should have become Var(p_fresh)
            if let CoreFrame::Var(v) = &result.nodes[*rhs] {
                assert_eq!(*v, p_fresh, "RHS should use renamed parameter");
            } else {
                panic!("RHS should be Var");
            }

            // Body: Var(p) should REMAIN Var(p)
            if let CoreFrame::Var(v) = &result.nodes[*body] {
                assert_eq!(
                    *v, p,
                    "Body should NOT use renamed parameter (it is outside Join scope)"
                );
            } else {
                panic!("Body should be Var");
            }
        } else {
            panic!("Result should be Join");
        }
    }

    #[test]
    fn test_subst_lambda_shadow_exact() {
        let x = VarId(1);
        let y = VarId(2);
        // \x -> x
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(x),                     // 0
                CoreFrame::Lam { binder: x, body: 0 }, // 1
            ],
        };
        // Var(y)
        let replacement = leaf(CoreFrame::Var(y));

        let result = subst(&tree, x, &replacement);

        // Result should be \x -> x (shadowed)
        assert_eq!(result.nodes.len(), 2);
        if let CoreFrame::Lam { binder, body } = &result.nodes[1] {
            assert_eq!(*binder, x);
            if let CoreFrame::Var(v) = &result.nodes[*body] {
                assert_eq!(*v, x, "Shadowed variable should not be substituted");
            } else {
                panic!("Body should be Var(x)");
            }
        } else {
            panic!("Result should be Lam");
        }
    }
}
