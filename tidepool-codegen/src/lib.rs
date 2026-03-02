//! Cranelift-based JIT compiler for Tidepool Core expressions.
//!
//! Compiles `CoreExpr` to native code via Cranelift, with effect machine support
//! for yielding on algebraic effects and resuming with handler responses.

pub mod alloc;
pub mod context;
pub mod datacon_env;
pub mod debug;
pub mod effect_machine;
pub mod emit;
pub mod gc;
pub mod heap_bridge;
pub mod host_fns;
pub mod jit_machine;
pub mod nursery;
pub mod pipeline;
pub mod signal_safety;
pub mod stack_map;
pub mod yield_type;
