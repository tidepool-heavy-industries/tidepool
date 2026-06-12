//! Flat-vector representation of expression trees.

use crate::frame::CoreFrame;
use crate::types::Alt;
use std::collections::HashMap;

/// A tree stored as a flat vector of frames.
///
/// In Tidepool's IR, [`crate::CoreExpr`] is a `RecursiveTree<CoreFrame<usize>>`
/// where children are stored as `usize` indices into the `nodes` vector.
/// This flat layout improves cache locality and allows for efficient
/// serialization without pointer-based traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecursiveTree<F> {
    /// The nodes of the tree. The root is typically the last node.
    pub nodes: Vec<F>,
}

/// A single step of an explicit-stack post-order walk over a flat tree.
/// `Enter` discovers a node's children (scheduling them first); `Exit` rebuilds
/// the node once all its children have new indices. Two `usize` per frame keeps
/// the work item pointer-sized.
enum WalkStep {
    Enter(usize),
    Exit(usize),
}

// The work item must stay small: a fat step type would defeat the point of
// moving the recursion onto the heap. (One discriminant + one index.)
const _: () = assert!(std::mem::size_of::<WalkStep>() <= 16);

impl RecursiveTree<CoreFrame<usize>> {
    /// Extract a subtree rooted at `idx` into a new standalone tree.
    ///
    /// Stack-safe: an explicit-stack post-order walk (children before parents,
    /// root emitted last) with index memoization — no call-stack growth per
    /// tree level, so arbitrarily deep towers extract without overflowing.
    ///
    /// Output is identical to the former recursive `collect`: nodes land in
    /// left-to-right post-order, and DAG sharing is preserved (a node reachable
    /// from several parents is emitted exactly once, via `old_to_new`).
    pub fn extract_subtree(&self, idx: usize) -> Self {
        let mut new_nodes: Vec<CoreFrame<usize>> = Vec::new();
        // old index -> new index, written at Exit; presence marks "emitted"
        // and is the sole bookkeeping (no separate "seen" set): an Enter or
        // Exit for an already-emitted index is skipped, which both preserves
        // DAG sharing and — for the common pure-tree case (in-degree 1) — costs
        // exactly the one map probe the recursive `collect` did.
        let mut old_to_new: HashMap<usize, usize> = HashMap::new();

        let mut stack = vec![WalkStep::Enter(idx)];
        while let Some(step) = stack.pop() {
            match step {
                WalkStep::Enter(i) => {
                    if old_to_new.contains_key(&i) {
                        continue;
                    }
                    stack.push(WalkStep::Exit(i));
                    // Children pop left-to-right, matching the recursive walk's
                    // child order (and thus node ordering).
                    for_each_child_rev(&self.nodes[i], |c| {
                        if !old_to_new.contains_key(&c) {
                            stack.push(WalkStep::Enter(c));
                        }
                    });
                }
                WalkStep::Exit(i) => {
                    if old_to_new.contains_key(&i) {
                        continue; // already emitted via another (shared) path
                    }
                    // Every child has an `old_to_new` entry by post-order
                    // discipline (acyclic IR ⇒ no in-progress child here).
                    let mapped = self.nodes[i].clone().map_layer(|c| old_to_new[&c]);
                    let new_idx = new_nodes.len();
                    new_nodes.push(mapped);
                    old_to_new.insert(i, new_idx);
                }
            }
        }
        RecursiveTree { nodes: new_nodes }
    }
}

/// Apply `f` to each child index of `frame` in REVERSE of [`get_children`]
/// order, allocation-free. Reversed so that pushing onto a LIFO work stack
/// yields left-to-right pop order — the explicit-stack walks
/// (`extract_subtree`, `replace_subtree`, `free_vars`) use this in their Enter
/// phase to schedule children without a per-node `Vec` allocation.
pub(crate) fn for_each_child_rev(frame: &CoreFrame<usize>, mut f: impl FnMut(usize)) {
    match frame {
        CoreFrame::Var(_) | CoreFrame::Lit(_) => {}
        CoreFrame::App { fun, arg } => {
            f(*arg);
            f(*fun);
        }
        CoreFrame::Lam { body, .. } => f(*body),
        CoreFrame::LetNonRec { rhs, body, .. } => {
            f(*body);
            f(*rhs);
        }
        CoreFrame::LetRec { bindings, body } => {
            f(*body);
            for (_, r) in bindings.iter().rev() {
                f(*r);
            }
        }
        CoreFrame::Case {
            scrutinee, alts, ..
        } => {
            for alt in alts.iter().rev() {
                f(alt.body);
            }
            f(*scrutinee);
        }
        CoreFrame::Con { fields, .. } => {
            for &x in fields.iter().rev() {
                f(x);
            }
        }
        CoreFrame::Join { rhs, body, .. } => {
            f(*body);
            f(*rhs);
        }
        CoreFrame::Jump { args, .. } => {
            for &a in args.iter().rev() {
                f(a);
            }
        }
        CoreFrame::PrimOp { args, .. } => {
            for &a in args.iter().rev() {
                f(a);
            }
        }
    }
}

/// Get all child indices of a CoreFrame node.
pub fn get_children(frame: &CoreFrame<usize>) -> Vec<usize> {
    match frame {
        CoreFrame::Var(_) | CoreFrame::Lit(_) => vec![],
        CoreFrame::App { fun, arg } => vec![*fun, *arg],
        CoreFrame::Lam { body, .. } => vec![*body],
        CoreFrame::LetNonRec { rhs, body, .. } => vec![*rhs, *body],
        CoreFrame::LetRec { bindings, body } => {
            let mut c: Vec<usize> = bindings.iter().map(|(_, r)| *r).collect();
            c.push(*body);
            c
        }
        CoreFrame::Case {
            scrutinee,
            alts,
            binder: _,
        } => {
            let mut c = vec![*scrutinee];
            for alt in alts {
                c.push(alt.body);
            }
            c
        }
        CoreFrame::Con { fields, .. } => fields.clone(),
        CoreFrame::Join { rhs, body, .. } => vec![*rhs, *body],
        CoreFrame::Jump { args, .. } => args.clone(),
        CoreFrame::PrimOp { args, .. } => args.clone(),
    }
}

/// Replace the subtree rooted at `target_idx` with `replacement`.
pub fn replace_subtree(
    expr: &RecursiveTree<CoreFrame<usize>>,
    target_idx: usize,
    replacement: &RecursiveTree<CoreFrame<usize>>,
) -> RecursiveTree<CoreFrame<usize>> {
    if expr.nodes.is_empty() {
        return expr.clone();
    }
    if replacement.nodes.is_empty() {
        // Replacing with an empty tree is not valid for CoreExpr, but we avoid panicking.
        return expr.clone();
    }
    assert!(
        target_idx < expr.nodes.len(),
        "target_idx {} out of bounds (len {})",
        target_idx,
        expr.nodes.len()
    );

    let mut new_nodes: Vec<CoreFrame<usize>> = Vec::new();
    let mut old_to_new: HashMap<usize, usize> = HashMap::new();

    // Stack-safe rebuild: explicit-stack post-order copy of `expr`, splicing
    // `replacement` wholesale wherever the walk reaches `target_idx`. Mirrors
    // the former recursive `rebuild` (same node ordering, same memoized DAG
    // sharing) without per-level call-stack growth. `old_to_new` is the sole
    // bookkeeping — an Enter/Exit for an already-emitted index is skipped.
    let mut stack = vec![WalkStep::Enter(expr.nodes.len() - 1)];
    while let Some(step) = stack.pop() {
        match step {
            WalkStep::Enter(i) => {
                if old_to_new.contains_key(&i) {
                    continue;
                }
                if i == target_idx {
                    // Splice the replacement here and stop — the original
                    // subtree at `target_idx` is discarded (no Exit, no
                    // descent), exactly as the recursive version returned early.
                    let offset = new_nodes.len();
                    for node in &replacement.nodes {
                        new_nodes.push(node.clone().map_layer(|j| j + offset));
                    }
                    let root = new_nodes
                        .len()
                        .checked_sub(1)
                        .expect("replacement tree must not be empty");
                    old_to_new.insert(i, root);
                    continue;
                }
                stack.push(WalkStep::Exit(i));
                for_each_child_rev(&expr.nodes[i], |c| {
                    if !old_to_new.contains_key(&c) {
                        stack.push(WalkStep::Enter(c));
                    }
                });
            }
            WalkStep::Exit(i) => {
                if old_to_new.contains_key(&i) {
                    continue; // already emitted via another (shared) path
                }
                let mapped = expr.nodes[i].clone().map_layer(|c| old_to_new[&c]);
                let new_idx = new_nodes.len();
                new_nodes.push(mapped);
                old_to_new.insert(i, new_idx);
            }
        }
    }
    RecursiveTree { nodes: new_nodes }
}

/// Functor map over the recursive positions of a frame.
pub trait MapLayer<A, B> {
    /// The resulting type after mapping.
    type Output;
    /// Apply `f` to each child index in the frame.
    fn map_layer(self, f: impl FnMut(A) -> B) -> Self::Output;
}

impl<A, B> MapLayer<A, B> for CoreFrame<A> {
    type Output = CoreFrame<B>;
    fn map_layer(self, mut f: impl FnMut(A) -> B) -> CoreFrame<B> {
        match self {
            CoreFrame::Var(v) => CoreFrame::Var(v),
            CoreFrame::Lit(l) => CoreFrame::Lit(l),
            CoreFrame::App { fun, arg } => CoreFrame::App {
                fun: f(fun),
                arg: f(arg),
            },
            CoreFrame::Lam { binder, body } => CoreFrame::Lam {
                binder,
                body: f(body),
            },
            CoreFrame::LetNonRec { binder, rhs, body } => CoreFrame::LetNonRec {
                binder,
                rhs: f(rhs),
                body: f(body),
            },
            CoreFrame::LetRec { bindings, body } => CoreFrame::LetRec {
                bindings: bindings.into_iter().map(|(id, rhs)| (id, f(rhs))).collect(),
                body: f(body),
            },
            CoreFrame::Case {
                scrutinee,
                binder,
                alts,
            } => CoreFrame::Case {
                scrutinee: f(scrutinee),
                binder,
                alts: alts
                    .into_iter()
                    .map(|alt| Alt {
                        con: alt.con,
                        binders: alt.binders,
                        body: f(alt.body),
                    })
                    .collect(),
            },
            CoreFrame::Con { tag, fields } => CoreFrame::Con {
                tag,
                fields: fields.into_iter().map(f).collect(),
            },
            CoreFrame::Join {
                label,
                params,
                rhs,
                body,
            } => CoreFrame::Join {
                label,
                params,
                rhs: f(rhs),
                body: f(body),
            },
            CoreFrame::Jump { label, args } => CoreFrame::Jump {
                label,
                args: args.into_iter().map(f).collect(),
            },
            CoreFrame::PrimOp { op, args } => CoreFrame::PrimOp {
                op,
                args: args.into_iter().map(f).collect(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn sample_frames() -> Vec<CoreFrame<usize>> {
        vec![
            CoreFrame::Var(VarId(1)),            // 0
            CoreFrame::Lit(Literal::LitInt(42)), // 1
            CoreFrame::App { fun: 0, arg: 1 },   // 2
            CoreFrame::Lam {
                binder: VarId(2),
                body: 0,
            }, // 3
            CoreFrame::LetNonRec {
                binder: VarId(3),
                rhs: 1,
                body: 2,
            }, // 4
            CoreFrame::LetRec {
                bindings: vec![(VarId(4), 0), (VarId(5), 1)],
                body: 2,
            }, // 5
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(6),
                alts: vec![Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: 1,
                }],
            }, // 6
            CoreFrame::Con {
                tag: DataConId(7),
                fields: vec![0, 1],
            }, // 7
            CoreFrame::Join {
                label: JoinId(8),
                params: vec![VarId(9)],
                rhs: 0,
                body: 1,
            }, // 8
            CoreFrame::Jump {
                label: JoinId(10),
                args: vec![0, 1],
            }, // 9
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 10
        ]
    }

    #[test]
    fn test_get_children() {
        let frames = sample_frames();
        assert_eq!(get_children(&frames[0]), Vec::<usize>::new()); // Var
        assert_eq!(get_children(&frames[1]), Vec::<usize>::new()); // Lit
        assert_eq!(get_children(&frames[2]), vec![0, 1]); // App
        assert_eq!(get_children(&frames[3]), vec![0]); // Lam
        assert_eq!(get_children(&frames[4]), vec![1, 2]); // LetNonRec
        assert_eq!(get_children(&frames[5]), vec![0, 1, 2]); // LetRec
        assert_eq!(get_children(&frames[6]), vec![0, 1]); // Case
        assert_eq!(get_children(&frames[7]), vec![0, 1]); // Con
        assert_eq!(get_children(&frames[8]), vec![0, 1]); // Join
        assert_eq!(get_children(&frames[9]), vec![0, 1]); // Jump
        assert_eq!(get_children(&frames[10]), vec![0, 1]); // PrimOp
    }

    #[test]
    fn test_replace_subtree_root() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
        ];
        let expr = RecursiveTree { nodes };
        let replacement = RecursiveTree {
            nodes: vec![CoreFrame::Lit(Literal::LitInt(2))],
        };
        let result = replace_subtree(&expr, 0, &replacement);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0], CoreFrame::Lit(Literal::LitInt(2)));
    }

    #[test]
    fn test_replace_subtree_nested() {
        // App(Var(x), Lit(1))
        let nodes = vec![
            CoreFrame::Var(VarId(1)),           // 0: x
            CoreFrame::Lit(Literal::LitInt(1)), // 1: 1
            CoreFrame::App { fun: 0, arg: 1 },  // 2: x 1
        ];
        let expr = RecursiveTree { nodes };

        // Replace Lit(1) with Lit(2)
        let replacement = RecursiveTree {
            nodes: vec![CoreFrame::Lit(Literal::LitInt(2))],
        };
        let result = replace_subtree(&expr, 1, &replacement);

        // Result should be App(Var(x), Lit(2))
        // The order might change depending on implementation, but let's check structure.
        let root_idx = result.nodes.len() - 1;
        if let CoreFrame::App { fun, arg } = &result.nodes[root_idx] {
            assert_eq!(result.nodes[*fun], CoreFrame::Var(VarId(1)));
            assert_eq!(result.nodes[*arg], CoreFrame::Lit(Literal::LitInt(2)));
        } else {
            panic!("Root should be App");
        }
    }

    #[test]
    fn test_map_layer_identity() {
        for frame in sample_frames() {
            let mapped = frame.clone().map_layer(|x| x);
            assert_eq!(frame, mapped);
        }
    }

    #[test]
    fn test_map_layer_composition() {
        let f = |x: usize| x + 10;
        let g = |x: usize| x * 2;

        for frame in sample_frames() {
            let direct = frame.clone().map_layer(|x| g(f(x)));
            let composed = frame.map_layer(f).map_layer(g);
            assert_eq!(direct, composed);
        }
    }

    #[test]
    fn test_recursive_tree_construction() {
        // App { fun: Lit(42), arg: Var(x) }
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Var(VarId(1)),            // 1
            CoreFrame::App { fun: 0, arg: 1 },   // 2 (root)
        ];
        let tree = RecursiveTree { nodes };

        assert_eq!(tree.nodes.len(), 3);
        if let CoreFrame::App { fun, arg } = &tree.nodes[2] {
            assert_eq!(*fun, 0);
            assert_eq!(*arg, 1);
        } else {
            panic!("Root should be an App");
        }
    }
}
