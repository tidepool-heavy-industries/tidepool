//! Pre-emit lowering passes over `CoreExpr` — structural rewrites that run
//! after `normalize`/`wrap_with_datacon_env` and before Cranelift emission
//! (see `jit_machine::compile_inner`).

use rustc_hash::{FxHashMap, FxHashSet};
use tidepool_repr::tree::get_children;
use tidepool_repr::{CoreExpr, CoreFrame, JoinId, MapLayer, TreeBuilder, VarId};

/// Rewrites `Join`/`Jump` pairs where a `Jump` reaches its label only by
/// crossing a `Lam` boundary — a shape codegen cannot compile. Each `Lam`
/// compiles as its own Cranelift function; a join's block is registered only
/// in the function emitting its `Join`, so a `Jump` inside a nested `Lam`'s
/// body hits a label unregistered in THAT function and codegen aborts
/// (`emit::JoinPointRegistry::get`'s "Jump to unregistered join").
///
/// Mirrors `Translate.jumpCrossesLam` (`haskell/src/Tidepool/Translate.hs:2427`,
/// applied at `:1385`): the real GHC→Core pipeline never emits this shape
/// because Translate.hs performs the same rewrite before the `Join`/`Jump`
/// nodes are built. This pass exists for `CoreExpr` producers that skip that
/// step — hand-built IR (fuzzer/proptest regressions) and any future non-GHC
/// frontend — so codegen's precondition holds regardless of producer.
///
/// A crossing `Join { label, params, rhs, body }` becomes
/// `LetNonRec { binder = VarId(label.0), rhs = \params -> rhs, body }` (a join
/// binder and a value binder share one numeric id space, same as
/// `Translate.hs`'s `Id`/`varId`), and every `Jump { label, args }` anywhere in
/// the tree becomes the application chain `Var(label) args...`. Converting
/// every occurrence (not just those inside `body`, as `Translate.hs` does) is
/// a strict superset: once a label's `Join` is gone, any surviving raw `Jump`
/// to it — wherever it lives — would hit the same unregistered-join error.
pub fn lower_jump_crosses_lam(tree: &CoreExpr) -> CoreExpr {
    if tree.nodes.is_empty() {
        return tree.clone();
    }
    let root = tree.nodes.len() - 1;
    let crosses = compute_crossing_joins(tree, root);
    if crosses.values().all(|&c| !c) {
        return tree.clone();
    }
    rewrite(tree, root, &crosses)
}

/// For every `Join` node reachable from `root`, whether a `Jump` to its own
/// label reaches it by crossing a `Lam`. Computed bottom-up (postorder) so an
/// outer join's check can treat an inner join that ITSELF crosses as if its
/// body were already wrapped in a `Lam` — mirroring `jumpCrossesLam`'s
/// recursive `rhsUnderLam` computation without rescanning from scratch for
/// each join.
fn compute_crossing_joins(tree: &CoreExpr, root: usize) -> FxHashMap<JoinId, bool> {
    let order = postorder(tree, root);
    let mut crosses: FxHashMap<JoinId, bool> = FxHashMap::default();
    for idx in order {
        if let CoreFrame::Join { label, body, .. } = &tree.nodes[idx] {
            let c = reaches_under_lam(tree, false, *body, *label, &crosses);
            crosses.insert(*label, c);
        }
    }
    crosses
}

/// Explicit-stack postorder traversal (children before parents), so a
/// DAG-shared node is visited once and dependency order (inner joins decided
/// before the joins that contain them) holds regardless of the input's raw
/// node ordering.
fn postorder(tree: &CoreExpr, root: usize) -> Vec<usize> {
    enum Step {
        Enter(usize),
        Exit(usize),
    }
    let mut order = Vec::new();
    let mut visited = FxHashSet::default();
    let mut stack = vec![Step::Enter(root)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(i) => {
                if visited.contains(&i) {
                    continue;
                }
                visited.insert(i);
                stack.push(Step::Exit(i));
                for c in get_children(&tree.nodes[i]) {
                    if !visited.contains(&c) {
                        stack.push(Step::Enter(c));
                    }
                }
            }
            Step::Exit(i) => order.push(i),
        }
    }
    order
}

/// Port of `Translate.jumpCrossesLam`'s `go`: does a `Jump` to `vid` occur
/// under a `Lam` when descending from `idx`? `under_lam` tracks whether an
/// enclosing (real, or already-decided-to-convert) `Lam` boundary has been
/// crossed since the join's body. `crosses` supplies already-decided inner
/// joins (postorder guarantees they were computed first), so a nested join
/// that itself converts is treated as a `Lam` boundary without a fresh
/// recursive re-scan of its body.
fn reaches_under_lam(
    tree: &CoreExpr,
    under_lam: bool,
    idx: usize,
    vid: JoinId,
    crosses: &FxHashMap<JoinId, bool>,
) -> bool {
    match &tree.nodes[idx] {
        CoreFrame::Var(_) | CoreFrame::Lit(_) => false,
        CoreFrame::App { fun, arg } => {
            reaches_under_lam(tree, under_lam, *fun, vid, crosses)
                || reaches_under_lam(tree, under_lam, *arg, vid, crosses)
        }
        CoreFrame::Lam { body, .. } => reaches_under_lam(tree, true, *body, vid, crosses),
        CoreFrame::LetNonRec { rhs, body, .. } => {
            reaches_under_lam(tree, under_lam, *rhs, vid, crosses)
                || reaches_under_lam(tree, under_lam, *body, vid, crosses)
        }
        CoreFrame::LetRec { bindings, body } => {
            bindings
                .iter()
                .any(|(_, r)| reaches_under_lam(tree, under_lam, *r, vid, crosses))
                || reaches_under_lam(tree, under_lam, *body, vid, crosses)
        }
        CoreFrame::Case {
            scrutinee, alts, ..
        } => {
            reaches_under_lam(tree, under_lam, *scrutinee, vid, crosses)
                || alts
                    .iter()
                    .any(|a| reaches_under_lam(tree, under_lam, a.body, vid, crosses))
        }
        CoreFrame::Con { fields, .. } => fields
            .iter()
            .any(|&f| reaches_under_lam(tree, under_lam, f, vid, crosses)),
        CoreFrame::Join {
            label: inner_label,
            rhs,
            body,
            ..
        } => {
            let inner_crosses = crosses.get(inner_label).copied().unwrap_or(false);
            let rhs_under_lam = under_lam || inner_crosses;
            reaches_under_lam(tree, rhs_under_lam, *rhs, vid, crosses)
                || reaches_under_lam(tree, under_lam, *body, vid, crosses)
        }
        CoreFrame::Jump { label, args } => {
            (under_lam && *label == vid)
                || args
                    .iter()
                    .any(|&a| reaches_under_lam(tree, under_lam, a, vid, crosses))
        }
        CoreFrame::PrimOp { args, .. } => args
            .iter()
            .any(|&a| reaches_under_lam(tree, under_lam, a, vid, crosses)),
    }
}

/// Whole-tree rewrite: convert every crossing `Join` into `LetNonRec` plus a
/// `Lam` wrapper, and every `Jump` to a converted label into an application
/// chain. Explicit-stack postorder copy (same shape as
/// `RecursiveTree::extract_subtree`) so the rewrite doesn't grow the host
/// stack on a deep tower.
fn rewrite(tree: &CoreExpr, root: usize, crosses: &FxHashMap<JoinId, bool>) -> CoreExpr {
    enum Step {
        Enter(usize),
        Exit(usize),
    }
    let mut b = TreeBuilder::new();
    let mut old_to_new: FxHashMap<usize, usize> = FxHashMap::default();
    let mut stack = vec![Step::Enter(root)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(i) => {
                if old_to_new.contains_key(&i) {
                    continue;
                }
                stack.push(Step::Exit(i));
                for c in get_children(&tree.nodes[i]) {
                    if !old_to_new.contains_key(&c) {
                        stack.push(Step::Enter(c));
                    }
                }
            }
            Step::Exit(i) => {
                if old_to_new.contains_key(&i) {
                    continue;
                }
                let new_idx = match &tree.nodes[i] {
                    CoreFrame::Jump { label, args }
                        if crosses.get(label).copied().unwrap_or(false) =>
                    {
                        // Converted join: a Jump becomes a call to the closure
                        // now bound at `VarId(label.0)` — Var applied to each
                        // (already-mapped) arg in order.
                        let head = b.push(CoreFrame::Var(VarId(label.0)));
                        args.iter().fold(head, |fun, &a| {
                            b.push(CoreFrame::App {
                                fun,
                                arg: old_to_new[&a],
                            })
                        })
                    }
                    CoreFrame::Join {
                        label,
                        params,
                        rhs,
                        body,
                    } if crosses.get(label).copied().unwrap_or(false) => {
                        let new_rhs = old_to_new[rhs];
                        let new_body = old_to_new[body];
                        // \p0 -> \p1 -> ... -> rhs (identity when params is empty).
                        let wrapped = params.iter().rev().fold(new_rhs, |acc, &p| {
                            b.push(CoreFrame::Lam { binder: p, body: acc })
                        });
                        b.push(CoreFrame::LetNonRec {
                            binder: VarId(label.0),
                            rhs: wrapped,
                            body: new_body,
                        })
                    }
                    frame => b.push(frame.clone().map_layer(|c| old_to_new[&c])),
                };
                old_to_new.insert(i, new_idx);
            }
        }
    }
    b.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{Literal, PrimOpKind};

    /// `join k(p) = p +# 0 in (\x -> jump k (x +# 0)) 0` — the Jump crosses the
    /// value Lam's boundary (bug1_join_crosses_lambda's fixture). Must convert.
    #[test]
    fn crossing_join_converts_to_let_non_rec_lambda() {
        let p = VarId(1);
        let x = VarId(2);
        let k = JoinId(0);
        let mut b = TreeBuilder::new();
        let pv = b.push(CoreFrame::Var(p));
        let l0 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
        let rhs = b.push(CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![pv, l0],
        });
        let xv = b.push(CoreFrame::Var(x));
        let l0b = b.push(CoreFrame::Lit(Literal::LitInt(0)));
        let xsum = b.push(CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![xv, l0b],
        });
        let jmp = b.push(CoreFrame::Jump {
            label: k,
            args: vec![xsum],
        });
        let lam = b.push(CoreFrame::Lam { binder: x, body: jmp });
        let arg = b.push(CoreFrame::Lit(Literal::LitInt(0)));
        let app = b.push(CoreFrame::App { fun: lam, arg });
        b.push(CoreFrame::Join {
            label: k,
            params: vec![p],
            rhs,
            body: app,
        });
        let tree = b.build();

        let lowered = lower_jump_crosses_lam(&tree);

        // No Join/Jump survives — every one was converted.
        for node in &lowered.nodes {
            assert!(
                !matches!(node, CoreFrame::Join { .. } | CoreFrame::Jump { .. }),
                "expected no Join/Jump to survive conversion, found {node:?}"
            );
        }
        let root = &lowered.nodes[lowered.nodes.len() - 1];
        match root {
            CoreFrame::LetNonRec { binder, .. } => assert_eq!(*binder, VarId(k.0)),
            other => panic!("expected root LetNonRec, got {other:?}"),
        }
    }

    /// A recursive join whose back-edge stays inside `rhs` (no Lam crossing)
    /// must be left completely untouched — this is join.rs's #325 loop shape.
    #[test]
    fn non_crossing_recursive_join_is_unchanged() {
        let n = VarId(2);
        let go = JoinId(1);
        let mut b = TreeBuilder::new();
        let var_n = b.push(CoreFrame::Var(n));
        let recur = b.push(CoreFrame::Jump {
            label: go,
            args: vec![var_n],
        });
        let lit_n = b.push(CoreFrame::Lit(Literal::LitInt(5)));
        let body = b.push(CoreFrame::Jump {
            label: go,
            args: vec![lit_n],
        });
        b.push(CoreFrame::Join {
            label: go,
            params: vec![n],
            rhs: recur,
            body,
        });
        let tree = b.build();

        let lowered = lower_jump_crosses_lam(&tree);
        assert_eq!(lowered, tree, "non-crossing join must pass through unchanged");
    }

    /// A tree with no Join at all is returned unchanged (fast path).
    #[test]
    fn no_joins_is_a_no_op() {
        let mut b = TreeBuilder::new();
        b.push(CoreFrame::Lit(Literal::LitInt(1)));
        let tree = b.build();
        assert_eq!(lower_jump_crosses_lam(&tree), tree);
    }
}
