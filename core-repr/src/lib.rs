pub mod datacon;
pub mod datacon_table;
pub mod frame;
pub mod pretty;
pub mod tree;
pub mod types;
pub mod serial;
pub mod free_vars;
pub mod subst;
pub mod alpha;

pub use datacon::*;
pub use datacon_table::*;
pub use frame::*;
pub use tree::*;
pub use types::*;

/// A complete Core expression, stored as a flat recursive tree.
pub type CoreExpr = RecursiveTree<CoreFrame<usize>>;