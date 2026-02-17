use crate::frame::{CoreFrame, MapLayer};
use crate::types::NodeId;

/// A recursive tree stored as a flat vector of frames.
///
/// Each frame's child positions hold `NodeId` indices into the `nodes` vec.
/// This enables O(1) node access and cache-friendly traversal.
#[derive(Debug, Clone, PartialEq)]
pub struct RecursiveTree<F> {
    nodes: Vec<F>,
    root: NodeId,
}

impl<F> RecursiveTree<F> {
    /// Create a tree with a single root node.
    pub fn singleton(frame: F) -> Self {
        RecursiveTree {
            nodes: vec![frame],
            root: NodeId(0),
        }
    }

    /// Add a node to the tree, returning its NodeId.
    pub fn add_node(&mut self, frame: F) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(frame);
        id
    }

    /// Set the root node.
    pub fn set_root(&mut self, id: NodeId) {
        self.root = id;
    }

    /// Get the root NodeId.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Get a reference to the frame at the given NodeId.
    pub fn node(&self, id: NodeId) -> &F {
        &self.nodes[id.0 as usize]
    }

    /// Get a mutable reference to the frame at the given NodeId.
    pub fn node_mut(&mut self, id: NodeId) -> &mut F {
        &mut self.nodes[id.0 as usize]
    }

    /// Number of nodes in the tree.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

impl RecursiveTree<CoreFrame<NodeId>> {
    /// Catamorphism: fold the tree bottom-up.
    ///
    /// Visits every node exactly once in bottom-up order, replacing
    /// each `CoreFrame<NodeId>` with the result of the algebra `alg`.
    pub fn cata<T: Clone>(&self, mut alg: impl FnMut(CoreFrame<T>) -> T) -> T {
        let mut results: Vec<Option<T>> = vec![None; self.nodes.len()];
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut visited = vec![false; self.nodes.len()];
        let mut stack = vec![self.root.0 as usize];

        // Compute post-order traversal
        while let Some(&idx) = stack.last() {
            if visited[idx] {
                stack.pop();
                order.push(idx);
            } else {
                visited[idx] = true;
                // Push children onto stack
                self.nodes[idx].clone().map_layer(|child: NodeId| {
                    let ci = child.0 as usize;
                    if !visited[ci] {
                        stack.push(ci);
                    }
                    child
                });
            }
        }

        for idx in order {
            let frame = self.nodes[idx].clone().map_layer(|child: NodeId| {
                results[child.0 as usize].clone().expect("child not computed")
            });
            results[idx] = Some(alg(frame));
        }

        results[self.root.0 as usize].clone().expect("root not computed")
    }

    /// Anamorphism: unfold a tree top-down from a seed.
    ///
    /// The coalgebra `coalg` produces a `CoreFrame<Seed>` from a seed.
    /// Each `Seed` in the frame becomes a child node via recursive unfolding.
    pub fn ana<Seed>(seed: Seed, mut coalg: impl FnMut(Seed) -> CoreFrame<Seed>) -> Self {
        let mut nodes: Vec<CoreFrame<NodeId>> = Vec::new();
        // Use a work queue. Process seeds breadth-first so parents get lower indices.
        let mut queue: std::collections::VecDeque<(NodeId, Seed)> = std::collections::VecDeque::new();

        // Reserve root
        nodes.push(CoreFrame::Var(crate::types::VarId(0))); // placeholder
        queue.push_back((NodeId(0), seed));

        while let Some((node_id, seed)) = queue.pop_front() {
            let frame = coalg(seed);
            let frame_mapped = frame.map_layer(|child_seed| {
                let child_id = NodeId(nodes.len() as u32);
                nodes.push(CoreFrame::Var(crate::types::VarId(0))); // placeholder
                queue.push_back((child_id, child_seed));
                child_id
            });
            nodes[node_id.0 as usize] = frame_mapped;
        }

        RecursiveTree { nodes, root: NodeId(0) }
    }
}

/// Hylomorphism: unfold then fold without materializing the full tree.
pub fn hylo<Seed, T: Clone>(
    seed: Seed,
    mut coalg: impl FnMut(Seed) -> CoreFrame<Seed>,
    alg: impl FnMut(CoreFrame<T>) -> T,
) -> T {
    // Unfold into a temporary flat buffer (like ana), then fold (like cata).
    // This allocates the intermediate tree but frees it when done.
    let tree = RecursiveTree::ana(seed, &mut coalg);
    tree.cata(alg)
}
