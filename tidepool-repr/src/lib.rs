//! Core representation types for Tidepool's GHC Core IR.
//!
//! Provides the primary intermediate representation (IR) used by Tidepool.
//! The IR is a recursive tree of [`CoreFrame`] nodes, typically manipulated
//! as a [`CoreExpr`]. It also defines identifiers, literals, and a
//! [`DataConTable`] for constructor metadata.

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

/// Core IR expression: a recursion scheme over [`CoreFrame`] nodes.
///
/// This is the primary interchange format between the Haskell frontend
/// (which translates GHC Core into this tree) and the Rust backend
/// (which optimizes it or compiles it into machine code).
pub type CoreExpr = RecursiveTree<CoreFrame<usize>>;
