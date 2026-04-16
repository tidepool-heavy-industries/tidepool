//! Flat-vector representation of expression trees.

use crate::frame::CoreFrame;
use crate::types::Alt;
use std::collections::HashMap;

/// A tree stored as a flat vector of frames.
///
/// Children are stored as `usize` indices into the `nodes` vector.
/// This flat layout improves cache locality and allows for efficient
/// serialization without pointer-based traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecursiveTree<F> {
    /// The nodes of the tree. The root is typically the last node.
    pub nodes: Vec<F>,
}

impl<F> RecursiveTree<F>
where
    F: MapLayer<usize, usize, Output = F> + Clone,
{
    /// Extract a subtree rooted at `idx` into a new standalone tree.
    pub fn extract_subtree(&self, idx: usize) -> Self {
        let mut new_nodes = Vec::new();
        let mut old_to_new = HashMap::new();

        fn collect<F>(
            idx: usize,
            tree: &RecursiveTree<F>,
            new_nodes: &mut Vec<F>,
            old_to_new: &mut HashMap<usize, usize>,
        ) -> usize
        where
            F: MapLayer<usize, usize, Output = F> + Clone,
        {
            if let Some(&new_idx) = old_to_new.get(&idx) {
                return new_idx;
            }

            let frame = &tree.nodes[idx];
            let mapped = frame
                .clone()
                .map_layer(|child| collect(child, tree, new_nodes, old_to_new));
            let new_idx = new_nodes.len();
            new_nodes.push(mapped);
            old_to_new.insert(idx, new_idx);
            new_idx
        }

        collect(idx, self, &mut new_nodes, &mut old_to_new);
        RecursiveTree { nodes: new_nodes }
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

    let mut new_nodes = Vec::new();
    let mut old_to_new = HashMap::new();
    rebuild(
        expr,
        expr.nodes.len() - 1,
        target_idx,
        replacement,
        &mut new_nodes,
        &mut old_to_new,
    );
    RecursiveTree { nodes: new_nodes }
}

fn rebuild(
    expr: &RecursiveTree<CoreFrame<usize>>,
    idx: usize,
    target: usize,
    replacement: &RecursiveTree<CoreFrame<usize>>,
    new_nodes: &mut Vec<CoreFrame<usize>>,
    old_to_new: &mut HashMap<usize, usize>,
) -> usize {
    if let Some(&ni) = old_to_new.get(&idx) {
        return ni;
    }
    if idx == target {
        let offset = new_nodes.len();
        for node in &replacement.nodes {
            new_nodes.push(node.clone().map_layer(|i| i + offset));
        }
        let root = new_nodes
            .len()
            .checked_sub(1)
            .expect("replacement tree must not be empty");
        old_to_new.insert(idx, root);
        return root;
    }
    let mapped = expr.nodes[idx]
        .clone()
        .map_layer(|child| rebuild(expr, child, target, replacement, new_nodes, old_to_new));
    let new_idx = new_nodes.len();
    new_nodes.push(mapped);
    old_to_new.insert(idx, new_idx);
    new_idx
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
