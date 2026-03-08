use crate::frame::CoreFrame;
use crate::types::Alt;
use std::collections::HashMap;

/// A tree stored as a flat vector of frames. Children are `usize` indices into `nodes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecursiveTree<F> {
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
        let root = new_nodes.len() - 1;
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
    type Output;
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
            CoreFrame::Var(VarId(1)),
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::App { fun: 0, arg: 1 },
            CoreFrame::Lam {
                binder: VarId(2),
                body: 0,
            },
            CoreFrame::LetNonRec {
                binder: VarId(3),
                rhs: 1,
                body: 2,
            },
            CoreFrame::LetRec {
                bindings: vec![(VarId(4), 0), (VarId(5), 1)],
                body: 2,
            },
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(6),
                alts: vec![Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: 1,
                }],
            },
            CoreFrame::Con {
                tag: DataConId(7),
                fields: vec![0, 1],
            },
            CoreFrame::Join {
                label: JoinId(8),
                params: vec![VarId(9)],
                rhs: 0,
                body: 1,
            },
            CoreFrame::Jump {
                label: JoinId(10),
                args: vec![0, 1],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            },
        ]
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
