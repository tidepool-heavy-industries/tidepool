//! Flat-vector representation of expression trees.

/// A tree stored as a flat vector of frames. Children are indices into `nodes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecursiveTree<F> {
    /// Nodes in post-order; the root is the last node.
    pub nodes: Vec<F>,
}
