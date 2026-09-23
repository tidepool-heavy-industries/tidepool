//! Host functions for the JIT runtime — the Rust side of the JIT↔host ABI.
//! Cranelift-emitted code calls the prepared module's explicit symbol set.
//! Every item is re-exported here so callers
//! outside this module see one flat surface regardless of the internal
//! module split ([`gc`] and [`errors`]).
//!
//! ## Reaching per-machine state
//!
//! Ambient per-machine state (cancel flag, JSON con ids, stack-map registry,
//! call depth, first-cause runtime error, diagnostics, GC state and root
//! registries) lives on [`crate::machine_state::MachineState`]; see that
//! module's doc for the full reach-path invariant. Host fns without a
//! `vmctx` use the per-thread `CURRENT_MACHINE` slot; the GC cluster is
//! reached via `vmctx` only.

mod errors;
mod gc;

pub(crate) use gc::remembered_set_disabled_for_test;
pub use gc::{
    arm_write_barrier, clear_gc_poison_override, clear_heap_verify_override,
    clear_max_heap_bytes_override, clear_rust_roots, persistent_roots_count,
    register_old_space_arena, register_persistent_root, register_rust_root, remembered_slots_count,
    rust_roots_mark, set_gc_poison, set_heap_verify, set_max_heap_bytes_for_test,
    set_remembered_set_disabled_for_test, truncate_rust_roots, write_barrier,
};
pub(crate) use gc::{prepared_gc_trigger, GcState, PreparedHeap};

pub(crate) use errors::MIN_VALID_ADDR;
pub use errors::{bad_pointer, drain_diagnostics, push_diagnostic, RuntimeError};
