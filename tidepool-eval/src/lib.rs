//! Tree-walking interpreter for Tidepool Core expressions.
//!
//! Provides a lazy, big-step evaluator for [`tidepool_repr::CoreExpr`].
//! Includes runtime representations ([`Value`]), environment management ([`Env`]),
//! and thunk storage ([`Heap`]).

pub mod env;
pub mod error;
pub mod eval;
pub mod heap;
pub mod pass;
pub mod value;

pub use env::*;
pub use error::*;
pub use eval::*;
pub use heap::*;
pub use pass::*;
pub use value::*;
