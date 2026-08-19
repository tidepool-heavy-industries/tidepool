//! Heap object layout and the copying-GC core (Cheney scan, pointer-field
//! walking) for Tidepool's JIT runtime. This crate provides the shared
//! HeapObject layout and `gc::raw`'s Cheney-copy primitives; the JIT's own
//! nursery and frame walker (in `tidepool-codegen`) drive collection on top
//! of them.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod gc;
pub mod layout;

pub use layout::*;
