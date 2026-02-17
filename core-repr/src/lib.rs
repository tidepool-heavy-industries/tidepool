pub mod types;
pub mod frame;
pub mod tree;

pub use frame::{CoreFrame, MapLayer};
pub use tree::{RecursiveTree, hylo};
pub use types::*;

/// A complete GHC Core expression.
pub type CoreExpr = RecursiveTree<CoreFrame<NodeId>>;