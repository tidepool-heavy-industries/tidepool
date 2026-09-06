//! Host functions for the JIT runtime — the Rust side of the JIT↔host ABI.
//! Cranelift-emitted code calls into these symbols, registered via
//! `host_fn_symbols()` below. Every item is re-exported here so callers
//! outside this module see one flat surface regardless of the internal
//! module split ([`cancel`], [`gc`], [`errors`], [`force`], [`primops`],
//! [`list_materialize`]).
//!
//! ## Reaching per-machine state
//!
//! Ambient per-machine state (cancel flag, JSON con ids, stack-map registry,
//! call depth, first-cause runtime error, diagnostics, GC state and root
//! registries) lives on [`crate::machine_state::MachineState`]; see that
//! module's doc for the full reach-path invariant. Host fns without a
//! `vmctx` use the per-thread `CURRENT_MACHINE` slot; the GC cluster is
//! reached via `vmctx` only. The signal handler's `EXEC_CONTEXT`/
//! `SIGNAL_SAFE_CTX` stay thread-scoped rather than per-machine.

mod cancel;
mod errors;
mod force;
mod gc;
mod list_materialize;
mod primops;

pub(crate) use cancel::check_cancel_and_set_error;
pub use cancel::runtime_cancel_check;

pub use gc::{
    arm_gc_fault, arm_write_barrier, clear_gc_poison_override, clear_heap_verify_override,
    clear_rust_roots, gc_doubling_run_count, gc_trigger, gc_trigger_call_count,
    gc_trigger_last_vmctx, heap_verify_run_count, persistent_roots_count, register_old_space_arena,
    register_persistent_root, register_rust_root, remembered_slots_count, reset_test_counters,
    rust_roots_mark, set_gc_poison, set_heap_verify, set_write_barrier_disabled_for_test,
    truncate_rust_roots, write_barrier, GcFaultPoint,
};
pub(crate) use gc::{run_minor_collection_for_tenure_fixup, GcState};

use errors::unresolved_var_trap;
pub use errors::{
    debug_app_check, debug_app_return, drain_diagnostics, error_poison_ptr, error_poison_ptr_lazy,
    error_poison_ptr_lazy_msg, error_poison_ptr_lazy_named, get_exec_context, has_runtime_error,
    is_lazy_poison, poisoned_external_name, push_diagnostic, raise_lazy_poison,
    register_poisoned_externals, register_var_names, runtime_bad_thunk_state_trap,
    runtime_blackhole_trap, runtime_error, runtime_error_dynamic, runtime_error_with_msg,
    runtime_oom, runtime_shape_trap, set_exec_context, set_first_cause, surface_error,
    take_runtime_error, RuntimeError, RuntimeErrorKind, ShapeTrapKind,
};
pub(crate) use errors::{MIN_VALID_ADDR, SIGNAL_SAFE_CTX, SIGNAL_SAFE_CTX_LEN};

pub use force::{deep_force, heap_demand, heap_force, trampoline_resolve};

pub use primops::{
    runtime_cas_boxed_array, runtime_clone_boxed_array, runtime_compare_byte_arrays,
    runtime_copy_addr_to_byte_array, runtime_copy_boxed_array, runtime_copy_byte_array,
    runtime_decode_double_exponent, runtime_decode_double_mantissa, runtime_decode_float_exponent,
    runtime_decode_float_mantissa, runtime_double_acos, runtime_double_acosh, runtime_double_asin,
    runtime_double_asinh, runtime_double_atan, runtime_double_atanh, runtime_double_cos,
    runtime_double_cosh, runtime_double_exp, runtime_double_expm1, runtime_double_log,
    runtime_double_log1p, runtime_double_power, runtime_double_sin, runtime_double_sinh,
    runtime_double_tan, runtime_double_tanh, runtime_int_encode_double, runtime_json_decode,
    runtime_new_boxed_array, runtime_new_byte_array, runtime_parse_iso8601,
    runtime_render_double_prec_text, runtime_render_double_text, runtime_resize_byte_array,
    runtime_set_byte_array, runtime_shrink_boxed_array, runtime_shrink_byte_array, runtime_strlen,
    runtime_text_measure_off, runtime_text_memchr, runtime_text_reverse, runtime_word2_quot,
    runtime_word2_rem, runtime_word_encode_double,
};

pub(crate) use list_materialize::materialize_cons_list;

/// Return the list of host function symbols for JIT registration.
///
/// Usage: `CodegenPipeline::new(&host_fn_symbols())`
pub fn host_fn_symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("gc_trigger", gc_trigger as *const u8),
        ("write_barrier", write_barrier as *const u8),
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
        ("heap_demand", heap_demand as *const u8),
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
            "runtime_decode_float_mantissa",
            runtime_decode_float_mantissa as *const u8,
        ),
        (
            "runtime_decode_float_exponent",
            runtime_decode_float_exponent as *const u8,
        ),
        (
            "runtime_text_measure_off",
            runtime_text_measure_off as *const u8,
        ),
        ("runtime_text_memchr", runtime_text_memchr as *const u8),
        ("runtime_text_reverse", runtime_text_reverse as *const u8),
        (
            "runtime_render_double_text",
            runtime_render_double_text as *const u8,
        ),
        (
            "runtime_render_double_prec_text",
            runtime_render_double_prec_text as *const u8,
        ),
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
