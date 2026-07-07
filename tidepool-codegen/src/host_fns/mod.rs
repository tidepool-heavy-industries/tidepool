//! Host functions for the JIT runtime — the Rust side of the JIT↔host ABI.
//!
//! Cranelift-emitted code calls into these symbols, which are handed to the JIT
//! module via `host_fn_symbols()` (the registration table near the bottom of the
//! file). Four concerns live here:
//!
//! - **Primop dispatch** — runtime implementations for primops not inlined as
//!   Cranelift IR (the bignum / `__gmpn_*` / `integer_gmp_*` intercepts, etc.).
//! - **GC trigger** — allocation slow-path callbacks into the copying collector,
//!   driven through the `VMContext` + stack-map registry.
//! - **Error poisoning** — `RuntimeError` raising: case traps, bad pointers,
//!   division by zero, and forced `error`/`undefined` sentinels.
//! - **Lazy-result streaming** — the `ValueSource` / `ValueStream` machinery that
//!   parks an effect-result iterator and serves it element-at-a-time.
//!
//! Always-on stderr breadcrumbs (`[CASE TRAP]`/`[SHAPE TRAP: …]`, `[BUG]`) fire
//! only on genuine compiler bugs and must stay loud.
//!
//! This module is split along system boundaries: [`cancel`] (external
//! cancellation), [`gc`] (roots + the copying collector), [`errors`]
//! (`RuntimeError` + poison machinery), [`force`] (WHNF/NF forcing + the tail
//! trampoline), [`primops`] (byte/boxed-array, Double, JSON primops), and
//! [`streaming`] (lazy effect-result materialization). Every item previously
//! reachable at the top level of this module is re-exported here so no
//! caller outside this module needs to change.
//!
//! ## The per-machine-state boundary (multi-machine / parMapM seam)
//!
//! Per-machine state now lives entirely on [`crate::machine_state::MachineState`],
//! owned by `JitEffectMachine`: the cancel flag, JSON con ids, stack-map
//! registry, and call depth (T6 leaf 1); the first-cause runtime error,
//! diagnostics, and parked-stream registry (T6 leaf 2); and the GC state plus
//! the run-scoped/session-scoped GC root registries (T6 leaf 3). Two reach
//! paths exist, and the GC cluster uses only the first:
//!
//! - **vmctx reach** (`(*vmctx).machine_state`) — the GC cluster
//!   (`gc.rs`: `GcState`, run-scoped roots, persistent roots) is reached this
//!   way EXCLUSIVELY, never via the per-thread slot below. A write (root
//!   register) and the read that later traces it (`perform_gc`) must key on
//!   the identical machine; see the "GC-cluster reach" note on
//!   `machine_state.rs`.
//! - **`CURRENT_MACHINE`** (a per-thread slot, `crate::machine_state`) — used
//!   by the cancel/JSON/stack-map/call-depth/runtime-error/diagnostics/
//!   parked-stream ambient shims for host fns that receive no `vmctx`.
//!
//! Only the signal handler (`EXEC_CONTEXT` / `SIGNAL_SAFE_CTX`, read from
//! async-signal context) must stay thread-scoped rather than per-machine.
//! Full host-fn vmctx-reach for the remaining ambient shims
//! (`runtime_error`/`runtime_error_with_msg`/`unresolved_var_trap`/
//! `runtime_shape_trap`/`runtime_oom`/the array primops) is #329.

mod cancel;
mod errors;
mod force;
mod gc;
mod primops;
mod streaming;

pub(crate) use cancel::check_cancel_and_set_error;
pub use cancel::runtime_cancel_check;

pub(crate) use gc::GcState;
pub use gc::{
    clear_rust_roots, gc_trigger, gc_trigger_call_count, gc_trigger_last_vmctx,
    heap_verify_run_count, persistent_roots_count, register_persistent_root, register_rust_root,
    reset_test_counters, rust_roots_mark, set_heap_verify, truncate_rust_roots,
};

use errors::unresolved_var_trap;
pub use errors::{
    debug_app_check, debug_app_return, drain_diagnostics, error_poison_ptr, error_poison_ptr_lazy,
    error_poison_ptr_lazy_msg, get_exec_context, has_runtime_error, is_lazy_poison,
    push_diagnostic, raise_lazy_poison, register_var_names, runtime_bad_thunk_state_trap,
    runtime_blackhole_trap, runtime_error, runtime_error_dynamic, runtime_error_with_msg,
    runtime_oom, runtime_shape_trap, set_exec_context, set_first_cause, surface_error,
    take_runtime_error, RuntimeError, RuntimeErrorKind, ShapeTrapKind,
};
pub(crate) use errors::{SIGNAL_SAFE_CTX, SIGNAL_SAFE_CTX_LEN};

pub use force::{deep_force, heap_force, trampoline_resolve};

pub use primops::{
    runtime_cas_boxed_array, runtime_clone_boxed_array, runtime_compare_byte_arrays,
    runtime_copy_addr_to_byte_array, runtime_copy_boxed_array, runtime_copy_byte_array,
    runtime_decode_double_exponent, runtime_decode_double_mantissa, runtime_double_acos,
    runtime_double_acosh, runtime_double_asin, runtime_double_asinh, runtime_double_atan,
    runtime_double_atanh, runtime_double_cos, runtime_double_cosh, runtime_double_exp,
    runtime_double_expm1, runtime_double_log, runtime_double_log1p, runtime_double_power,
    runtime_double_sin, runtime_double_sinh, runtime_double_tan, runtime_double_tanh,
    runtime_int_encode_double, runtime_json_decode, runtime_new_boxed_array,
    runtime_new_byte_array, runtime_parse_iso8601, runtime_resize_byte_array,
    runtime_set_byte_array, runtime_show_double_addr, runtime_show_signed_double_addr,
    runtime_shrink_boxed_array, runtime_shrink_byte_array, runtime_strlen,
    runtime_text_measure_off, runtime_text_memchr, runtime_text_reverse, runtime_word2_quot,
    runtime_word2_rem, runtime_word_encode_double,
};

pub(crate) use streaming::{
    alloc_stream_tail_thunk, materialize_cons_list, park_stream, ParkedStream, ReadySource,
    StreamId,
};

/// Return the list of host function symbols for JIT registration.
///
/// Usage: `CodegenPipeline::new(&host_fn_symbols())`
pub fn host_fn_symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("gc_trigger", gc_trigger as *const u8),
        ("runtime_json_decode", runtime_json_decode as *const u8),
        ("runtime_parse_iso8601", runtime_parse_iso8601 as *const u8),
        ("runtime_oom", runtime_oom as *const u8),
        (
            "runtime_blackhole_trap",
            runtime_blackhole_trap as *const u8,
        ),
        (
            "runtime_bad_thunk_state_trap",
            runtime_bad_thunk_state_trap as *const u8,
        ),
        ("heap_force", heap_force as *const u8),
        ("unresolved_var_trap", unresolved_var_trap as *const u8),
        ("runtime_error", runtime_error as *const u8),
        (
            "runtime_error_with_msg",
            runtime_error_with_msg as *const u8,
        ),
        ("runtime_error_dynamic", runtime_error_dynamic as *const u8),
        ("debug_app_check", debug_app_check as *const u8),
        ("debug_app_return", debug_app_return as *const u8),
        ("trampoline_resolve", trampoline_resolve as *const u8),
        ("runtime_cancel_check", runtime_cancel_check as *const u8),
        (
            "runtime_new_byte_array",
            runtime_new_byte_array as *const u8,
        ),
        (
            "runtime_copy_addr_to_byte_array",
            runtime_copy_addr_to_byte_array as *const u8,
        ),
        (
            "runtime_set_byte_array",
            runtime_set_byte_array as *const u8,
        ),
        (
            "runtime_shrink_byte_array",
            runtime_shrink_byte_array as *const u8,
        ),
        (
            "runtime_resize_byte_array",
            runtime_resize_byte_array as *const u8,
        ),
        (
            "runtime_copy_byte_array",
            runtime_copy_byte_array as *const u8,
        ),
        (
            "runtime_compare_byte_arrays",
            runtime_compare_byte_arrays as *const u8,
        ),
        ("runtime_strlen", runtime_strlen as *const u8),
        (
            "runtime_decode_double_mantissa",
            runtime_decode_double_mantissa as *const u8,
        ),
        (
            "runtime_decode_double_exponent",
            runtime_decode_double_exponent as *const u8,
        ),
        (
            "runtime_text_measure_off",
            runtime_text_measure_off as *const u8,
        ),
        ("runtime_text_memchr", runtime_text_memchr as *const u8),
        ("runtime_text_reverse", runtime_text_reverse as *const u8),
        ("runtime_word2_quot", runtime_word2_quot as *const u8),
        ("runtime_word2_rem", runtime_word2_rem as *const u8),
        // ghc-bignum Integer->Double FFI (the only mpn-adjacent FFI under the
        // native backend; mantissa * 2^exp via tidepool-bignum).
        (
            "runtime_int_encode_double",
            runtime_int_encode_double as *const u8,
        ),
        (
            "runtime_word_encode_double",
            runtime_word_encode_double as *const u8,
        ),
        (
            "runtime_new_boxed_array",
            runtime_new_boxed_array as *const u8,
        ),
        (
            "runtime_clone_boxed_array",
            runtime_clone_boxed_array as *const u8,
        ),
        (
            "runtime_copy_boxed_array",
            runtime_copy_boxed_array as *const u8,
        ),
        (
            "runtime_shrink_boxed_array",
            runtime_shrink_boxed_array as *const u8,
        ),
        (
            "runtime_cas_boxed_array",
            runtime_cas_boxed_array as *const u8,
        ),
        ("runtime_shape_trap", runtime_shape_trap as *const u8),
        (
            "runtime_show_double_addr",
            runtime_show_double_addr as *const u8,
        ),
        (
            "runtime_show_signed_double_addr",
            runtime_show_signed_double_addr as *const u8,
        ),
        // Double math (libm wrappers)
        ("runtime_double_exp", runtime_double_exp as *const u8),
        ("runtime_double_expm1", runtime_double_expm1 as *const u8),
        ("runtime_double_log", runtime_double_log as *const u8),
        ("runtime_double_log1p", runtime_double_log1p as *const u8),
        ("runtime_double_sin", runtime_double_sin as *const u8),
        ("runtime_double_cos", runtime_double_cos as *const u8),
        ("runtime_double_tan", runtime_double_tan as *const u8),
        ("runtime_double_asin", runtime_double_asin as *const u8),
        ("runtime_double_acos", runtime_double_acos as *const u8),
        ("runtime_double_atan", runtime_double_atan as *const u8),
        ("runtime_double_sinh", runtime_double_sinh as *const u8),
        ("runtime_double_cosh", runtime_double_cosh as *const u8),
        ("runtime_double_tanh", runtime_double_tanh as *const u8),
        ("runtime_double_asinh", runtime_double_asinh as *const u8),
        ("runtime_double_acosh", runtime_double_acosh as *const u8),
        ("runtime_double_atanh", runtime_double_atanh as *const u8),
        ("runtime_double_power", runtime_double_power as *const u8),
    ]
}
