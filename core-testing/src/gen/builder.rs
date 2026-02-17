use core_repr::{CoreFrame, RecursiveTree, MapLayer};

/// Builds a RecursiveTree by appending nodes bottom-up.
#[derive(Debug, Clone)]
pub(crate) struct TreeBuilder {
    pub nodes: Vec<CoreFrame<usize>>,
}

impl TreeBuilder {
    /// Create a new empty builder.
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Add a node, return its index.
    pub fn push(&mut self, frame: CoreFrame<usize>) -> usize {
        let idx = self.nodes.len();
        self.nodes.push(frame);
        idx
    }

    /// Appends all nodes from another tree to this builder, returning the offset.
    pub fn push_tree(&mut self, other: TreeBuilder) -> usize {
        let offset = self.nodes.len();
        for node in other.nodes {
            self.nodes.push(node.map_layer(|idx| idx + offset));
        }
        offset
    }

    /// Finish building, return the tree.
    pub fn build(self) -> RecursiveTree<CoreFrame<usize>> {
        RecursiveTree { nodes: self.nodes }
    }
}