pub mod frame;
pub mod pretty;
pub mod tree;
pub mod types;

pub use frame::*;
pub use tree::*;
pub use types::*;

/// A complete Core expression, stored as a flat recursive tree.
pub type CoreExpr = RecursiveTree<CoreFrame<usize>>;
