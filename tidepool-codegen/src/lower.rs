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
/// Mirrors `Translate.jumpCrossesLam` (`haskell/src/Tidepool/Translate.hs`, grep
/// `jumpCrossesLam`; def ~L2602, applied ~L1545): the real GHC→Core pipeline never emits this shape
/// because Translate.hs performs the same rewrite before the `Join`/`Jump`
/// nodes are built. This pass exists for `CoreExpr` producers that skip that
/// step — hand-built IR (fuzzer/proptest regressions) and any future non-GHC
/// frontend — so codegen's precondition holds regardless of producer.
///
/// A crossing `Join { label, params, rhs, body }` becomes
/// `LetNonRec { binder = fresh, rhs = \params -> rhs, body }`, and every
/// `Jump { label, args }` anywhere in the tree becomes the application chain
/// `Var(fresh) args...`. Converting every occurrence (not just those inside
/// `body`, as `Translate.hs` does) is a strict superset: once a label's `Join`
/// is gone, any surviving raw `Jump` to it — wherever it lives — would hit the
/// same unregistered-join error.
///
/// `fresh` is a `VarId` minted strictly above every `VarId` in the tree (see
/// `max_var_id`), one per crossing label. `Translate.hs` can reuse the join's
/// own numeric id as the value binder because GHC's `Id`/join uniques share one
/// disjoint space; a hand-built producer that seeds a 0-based `JoinId`/`VarId`
/// space could otherwise have `VarId(label.0)` silently shadow a live value
/// binder across the join body. Minting fresh sidesteps that regardless of
/// producer.
///
/// PRECONDITION — crossing joins must be non-recursive: no `Jump` back to a
/// join's own `label` (under a `Lam`) inside its own `rhs`. GHC's `NonRec`
/// joins and the fuzzer's generators both guarantee this. A self-recursive
/// join whose back-edge crosses a `Lam` would need a `LetRec` wrapper (the
/// non-recursive `LetNonRec` here would leave the recursive `Var(fresh)`
/// unbound in the wrapped `rhs`); that shape is produced by no current
/// frontend and is not handled — `compute_crossing_joins` scans only `body`,
/// so it is left untouched rather than mis-lowered.
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
///
/// Explicit-stack DFS (L5, repo-review-2026-07-06/01-gc-memory-safety.md):
/// every sibling traversal in this file (`postorder`, `rewrite`) already uses
/// an explicit stack for deep-tower safety; this one was still host-
/// recursive — a deep tower with any `Join` in it could overflow the
/// COMPILER's own stack, outside any signal protection. A pure "does any
/// reachable `Jump{label==vid}` occur with `under_lam` true" search doesn't
/// depend on traversal order, so pushing every child (with its own computed
/// `under_lam`) and returning as soon as one matches is exactly equivalent
/// to the original short-circuiting recursion.
fn reaches_under_lam(
    tree: &CoreExpr,
    under_lam: bool,
    idx: usize,
    vid: JoinId,
    crosses: &FxHashMap<JoinId, bool>,
) -> bool {
    let mut stack = vec![(idx, under_lam)];
    while let Some((idx, under_lam)) = stack.pop() {
        match &tree.nodes[idx] {
            CoreFrame::Var(_) | CoreFrame::Lit(_) => {}
            CoreFrame::App { fun, arg } => {
                stack.push((*fun, under_lam));
                stack.push((*arg, under_lam));
            }
            CoreFrame::Lam { body, .. } => stack.push((*body, true)),
            CoreFrame::LetNonRec { rhs, body, .. } => {
                stack.push((*rhs, under_lam));
                stack.push((*body, under_lam));
            }
            CoreFrame::LetRec { bindings, body } => {
                for (_, r) in bindings {
                    stack.push((*r, under_lam));
                }
                stack.push((*body, under_lam));
            }
            CoreFrame::Case {
                scrutinee, alts, ..
            } => {
                stack.push((*scrutinee, under_lam));
                for a in alts {
                    stack.push((a.body, under_lam));
                }
            }
            CoreFrame::Con { fields, .. } => {
                for &f in fields {
                    stack.push((f, under_lam));
                }
            }
            CoreFrame::Join {
                label: inner_label,
                rhs,
                body,
                ..
            } => {
                let inner_crosses = crosses.get(inner_label).copied().unwrap_or(false);
                let rhs_under_lam = under_lam || inner_crosses;
                stack.push((*rhs, rhs_under_lam));
                stack.push((*body, under_lam));
            }
            CoreFrame::Jump { label, args } => {
                if under_lam && *label == vid {
                    return true;
                }
                for &a in args {
                    stack.push((a, under_lam));
                }
            }
            CoreFrame::PrimOp { args, .. } => {
                for &a in args {
                    stack.push((a, under_lam));
                }
            }
        }
    }
    false
}

/// Largest `VarId.0` mentioned anywhere in the tree (0 if none), scanning both
/// references (`Var`) and every binder site (`Lam`/`LetNonRec`/`LetRec`/`Case`
/// binders, `Join` params). Converted-join binders are minted above this so
/// they cannot shadow a live value binder — see [`lower_jump_crosses_lam`].
fn max_var_id(tree: &CoreExpr) -> u64 {
    let mut max = 0u64;
    for node in &tree.nodes {
        match node {
            CoreFrame::Var(v) => max = max.max(v.0),
            CoreFrame::Lam { binder, .. } | CoreFrame::LetNonRec { binder, .. } => {
                max = max.max(binder.0)
            }
            CoreFrame::Case { binder, alts, .. } => {
                max = max.max(binder.0);
                // Alt pattern binders are VarIds too (M6) — omitting them let a
                // converted Jump's fresh binder silently capture one.
                for alt in alts {
                    for b in &alt.binders {
                        max = max.max(b.0);
                    }
                }
            }
            CoreFrame::LetRec { bindings, .. } => {
                for (b, _) in bindings {
                    max = max.max(b.0);
                }
            }
            CoreFrame::Join { params, .. } => {
                for p in params {
                    max = max.max(p.0);
                }
            }
            _ => {}
        }
    }
    max
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
    // Fresh binder per crossing label, minted strictly above every VarId in
    // the tree (sorted for deterministic assignment) so a converted join's
    // value binder never shadows a live in-scope binder.
    let base = max_var_id(tree) + 1;
    let mut crossing: Vec<JoinId> = crosses
        .iter()
        .filter_map(|(&k, &c)| c.then_some(k))
        .collect();
    crossing.sort_by_key(|k| k.0);
    let fresh: FxHashMap<JoinId, VarId> = crossing
        .into_iter()
        .enumerate()
        .map(|(i, k)| (k, VarId(base + i as u64)))
        .collect();

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
                        // now bound at `fresh[label]` — Var applied to each
                        // (already-mapped) arg in order.
                        let head = b.push(CoreFrame::Var(fresh[label]));
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
                            b.push(CoreFrame::Lam {
                                binder: p,
                                body: acc,
                            })
                        });
                        b.push(CoreFrame::LetNonRec {
                            binder: fresh[label],
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
        let lam = b.push(CoreFrame::Lam {
            binder: x,
            body: jmp,
        });
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
        // The converted binder is minted fresh, strictly above every VarId in
        // the tree (p=VarId(1), x=VarId(2) ⇒ max 2 ⇒ base 3), not reused from
        // the join's own id.
        let root = &lowered.nodes[lowered.nodes.len() - 1];
        match root {
            CoreFrame::LetNonRec { binder, .. } => {
                assert!(
                    binder.0 >= 3,
                    "converted binder must be fresh, got {binder:?}"
                );
            }
            other => panic!("expected root LetNonRec, got {other:?}"),
        }
    }

    /// A crossing join whose numeric label collides with a live in-scope
    /// `VarId` — the shape a producer that seeds a 0-based `JoinId`/`VarId`
    /// space would emit — must mint a FRESH binder, never shadow the value.
    /// Guards the id-space-collision hazard.
    #[test]
    fn crossing_join_binder_does_not_shadow_colliding_varid() {
        // let v0 = 99 in (join 0(p) = p in (\x -> jump 0 v0) 0)
        // The Jump passes `Var(VarId(0))`, which must keep resolving to the
        // outer `let v0` — not be captured by a converted binder reusing id 0.
        let v0 = VarId(0); // collides numerically with JoinId(0)
        let p = VarId(5);
        let x = VarId(6);
        let k = JoinId(0);
        let mut b = TreeBuilder::new();
        let rhs = b.push(CoreFrame::Var(p));
        let ref_v0 = b.push(CoreFrame::Var(v0));
        let jmp = b.push(CoreFrame::Jump {
            label: k,
            args: vec![ref_v0],
        });
        let lam = b.push(CoreFrame::Lam {
            binder: x,
            body: jmp,
        });
        let arg = b.push(CoreFrame::Lit(Literal::LitInt(0)));
        let app = b.push(CoreFrame::App { fun: lam, arg });
        let join = b.push(CoreFrame::Join {
            label: k,
            params: vec![p],
            rhs,
            body: app,
        });
        let outer_rhs = b.push(CoreFrame::Lit(Literal::LitInt(99)));
        b.push(CoreFrame::LetNonRec {
            binder: v0,
            rhs: outer_rhs,
            body: join,
        });
        let tree = b.build();

        let lowered = lower_jump_crosses_lam(&tree);

        // The converted join is the LetNonRec whose rhs is a Lam wrapper; its
        // binder must be fresh (>= max var 6 ⇒ base 7), never VarId(0).
        let converted = lowered.nodes.iter().find_map(|n| match n {
            CoreFrame::LetNonRec { binder, rhs, .. }
                if matches!(lowered.nodes[*rhs], CoreFrame::Lam { .. }) =>
            {
                Some(*binder)
            }
            _ => None,
        });
        let binder = converted.expect("a converted-join LetNonRec with a Lam rhs");
        assert!(
            binder.0 >= 7,
            "converted binder must be fresh, got {binder:?}"
        );
        assert_ne!(binder, v0, "must not reuse the colliding label id 0");
        // The outer `let v0 = 99` binder is untouched.
        assert!(
            lowered
                .nodes
                .iter()
                .any(|n| matches!(n, CoreFrame::LetNonRec { binder, .. } if *binder == v0)),
            "outer VarId(0) binding must survive"
        );
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
        assert_eq!(
            lowered, tree,
            "non-crossing join must pass through unchanged"
        );
    }

    /// A tree with no Join at all is returned unchanged (fast path).
    #[test]
    fn no_joins_is_a_no_op() {
        let mut b = TreeBuilder::new();
        b.push(CoreFrame::Lit(Literal::LitInt(1)));
        let tree = b.build();
        assert_eq!(lower_jump_crosses_lam(&tree), tree);
    }

    /// M6: `max_var_id` must scan `Case` ALT pattern binders, not just the
    /// case's own top-level `binder`. A `Case` alt binder holding the
    /// LARGEST `VarId` in the tree, with everything else (join label,
    /// params, lambda binder) small, reproduces the exact undercount: if alt
    /// binders aren't scanned, the converted join's freshly-minted binder can
    /// come out `<=` the alt binder instead of strictly above every VarId.
    #[test]
    fn crossing_join_binder_is_fresh_above_case_alt_binder() {
        // case Con_5(0#) of { Con_5 big -> big }    (big = VarId(10), the
        // largest VarId anywhere in the tree, reachable ONLY via Alt::binders)
        // join k(p) = p in (\x -> jump k 0) 0        (everything else small)
        let big = VarId(10);
        let p = VarId(1);
        let x = VarId(2);
        let case_scrut_binder = VarId(3);
        let k = JoinId(0);
        const SOME_CON: tidepool_repr::DataConId = tidepool_repr::DataConId(5);

        let mut b = TreeBuilder::new();
        let field = b.push(CoreFrame::Lit(Literal::LitInt(0)));
        let scrutinee = b.push(CoreFrame::Con {
            tag: SOME_CON,
            fields: vec![field],
        });
        // The alt binder `big` is bound but UNUSED in the body — it must
        // still count toward `max_var_id` via `Alt::binders`, not just via a
        // `Var(big)` reference (which would mask the M6 bug: `max_var_id`
        // already scans every `Var` reference node, so an unused binder is
        // the only shape that isolates "scans Alt::binders" from "scans Var
        // references").
        let unused_body = b.push(CoreFrame::Lit(Literal::LitInt(0)));
        let case = b.push(CoreFrame::Case {
            scrutinee,
            binder: case_scrut_binder,
            alts: vec![tidepool_repr::Alt {
                con: tidepool_repr::AltCon::DataAlt(SOME_CON),
                binders: vec![big],
                body: unused_body,
            }],
        });

        let pv = b.push(CoreFrame::Var(p));
        let l0 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
        let jmp = b.push(CoreFrame::Jump {
            label: k,
            args: vec![l0],
        });
        let lam = b.push(CoreFrame::Lam {
            binder: x,
            body: jmp,
        });
        let arg = b.push(CoreFrame::Lit(Literal::LitInt(0)));
        let app = b.push(CoreFrame::App { fun: lam, arg });
        let join = b.push(CoreFrame::Join {
            label: k,
            params: vec![p],
            rhs: pv,
            body: app,
        });
        // Root ties both together so the whole tree is reachable from one root.
        b.push(CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![join, case],
        });
        let tree = b.build();

        let lowered = lower_jump_crosses_lam(&tree);

        let converted = lowered.nodes.iter().find_map(|n| match n {
            CoreFrame::LetNonRec { binder, rhs, .. }
                if matches!(lowered.nodes[*rhs], CoreFrame::Lam { .. }) =>
            {
                Some(*binder)
            }
            _ => None,
        });
        let binder = converted.expect("a converted-join LetNonRec with a Lam rhs");
        assert!(
            binder.0 > big.0,
            "converted binder ({binder:?}) must be strictly fresher than the \
             Case alt binder ({big:?}) — max_var_id must scan Alt::binders"
        );
    }

    /// L5: `reaches_under_lam` was host-recursive while its siblings
    /// (`postorder`, `rewrite`) already use an explicit stack — a deep tower
    /// with a `Join` in it could overflow the COMPILER's own stack, outside
    /// any signal protection. A `Join` whose `body` is a very deep `App`
    /// chain (`App{fun: <deeper App>, arg: ...}`) exercises this: `fun` sits
    /// on the LEFT of the `reaches_under_lam(fun) || reaches_under_lam(arg)`
    /// short-circuit, so — unlike a chain built through a position that's
    /// effectively in tail position (e.g. `LetNonRec::body`, which rustc/LLVM
    /// can and does turn into a loop even in a dev build) — each level's
    /// stack frame must stay live while the `fun` side resolves, forcing
    /// genuine non-tail recursion in the old implementation.
    #[test]
    fn reaches_under_lam_handles_a_deep_tower_without_host_stack_overflow() {
        const DEPTH: usize = 500_000;
        let k = JoinId(0);
        let p = VarId(0);

        let mut b = TreeBuilder::new();
        let rhs = b.push(CoreFrame::Var(p)); // join k(p) = p

        // Deep App chain: (...((p 0) 0) ... 0) — DEPTH nested applications,
        // no actual Jump anywhere, so the search must walk the whole chain.
        let mut fun = b.push(CoreFrame::Var(p));
        for _ in 0..DEPTH {
            let arg = b.push(CoreFrame::Lit(Literal::LitInt(0)));
            fun = b.push(CoreFrame::App { fun, arg });
        }

        b.push(CoreFrame::Join {
            label: k,
            params: vec![p],
            rhs,
            body: fun,
        });
        let tree = b.build();

        // No Jump anywhere in `body`, so nothing crosses — the whole DEPTH
        // must be walked (not short-circuited) to determine that, and the
        // tree comes back unchanged.
        let lowered = lower_jump_crosses_lam(&tree);
        assert_eq!(lowered, tree);
    }
}
