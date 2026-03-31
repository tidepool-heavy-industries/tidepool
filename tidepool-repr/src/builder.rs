//! Helper for constructing Tidepool IR trees.

use crate::frame::CoreFrame;
use crate::tree::{MapLayer, RecursiveTree};

/// Builds a RecursiveTree by appending nodes bottom-up.
///
/// Using TreeBuilder avoids manual index arithmetic when constructing CoreExpr trees.
///
/// # Example
/// ```
/// use tidepool_repr::{TreeBuilder, CoreFrame, Literal, VarId};
///
/// let mut b = TreeBuilder::new();
/// let x = b.push(CoreFrame::Var(VarId(1)));
/// let lit = b.push(CoreFrame::Lit(Literal::LitInt(42)));
/// let app = b.push(CoreFrame::App { fun: x, arg: lit });
/// let expr = b.build();
/// assert_eq!(expr.nodes.len(), 3);
/// ```
#[derive(Debug, Clone)]
pub struct TreeBuilder {
    nodes: Vec<CoreFrame<usize>>,
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

    /// Append all nodes from another builder, offsetting indices.
    /// Returns the offset of the first added node.
    pub fn push_tree(&mut self, other: TreeBuilder) -> usize {
        let offset = self.nodes.len();
        for node in other.nodes {
            self.nodes.push(node.map_layer(|idx| idx + offset));
        }
        offset
    }

    /// Add multiple nodes, return the index of the last added node (or 0 if empty).
    pub fn extend<I>(&mut self, iter: I) -> usize
    where
        I: IntoIterator<Item = CoreFrame<usize>>,
    {
        let mut last_idx = self.nodes.len().saturating_sub(1);
        for frame in iter {
            last_idx = self.push(frame);
        }
        last_idx
    }

    /// Finish building, return the tree.
    pub fn build(self) -> RecursiveTree<CoreFrame<usize>> {
        RecursiveTree { nodes: self.nodes }
    }
}

impl Default for TreeBuilder {
    fn default() -> Self {
        Self::new()
    }
}
