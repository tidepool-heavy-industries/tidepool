//! Heap object layout and the copying-GC core (Cheney scan, pointer-field
//! walking) for Tidepool's JIT runtime. The interpreter-plane arena/mark-
//! compact GC (`ArenaHeap`, `gc::trace`, `gc::compact`) was retired as
//! production-dead — the JIT's own nursery + frame walker (in
//! `tidepool-codegen`) drive collection; this crate now only provides the
//! shared object layout and `gc::raw`'s Cheney-copy primitives it's built on.

pub mod gc;
pub mod layout;

pub use layout::*;
