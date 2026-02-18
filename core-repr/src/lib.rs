pub mod alpha;
pub mod builder;
pub mod datacon;
pub mod datacon_table;
pub mod frame;
pub mod free_vars;
pub mod pretty;
pub mod serial;
pub mod subst;
pub mod tree;
pub mod types;

pub use builder::TreeBuilder;
pub use datacon::*;
pub use datacon_table::*;
pub use frame::*;
pub use tree::*;
pub use types::*;

/// A complete Core expression, stored as a flat recursive tree.
pub type CoreExpr = RecursiveTree<CoreFrame<usize>>;
