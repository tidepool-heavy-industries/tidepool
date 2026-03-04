//! Manual heap and copying garbage collector for Tidepool's JIT runtime.
//!
//! Provides a nursery arena with bump allocation, heap object layout, and a
//! copying GC with RBP frame walking for root discovery.

pub mod arena;
pub mod gc;
pub mod layout;

pub use arena::*;
pub use layout::*;
pub use gc::trace::GcError;
