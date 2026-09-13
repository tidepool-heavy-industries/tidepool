#[path = "../support/engine_observation.rs"]
mod engine_observation;
#[path = "../support/session_scaffold.rs"]
mod session_scaffold;
#[path = "../support/session_scaffold_expect.rs"]
mod session_scaffold_expect;
#[path = "../support/session_scaffold_gc_forcing.rs"]
mod session_scaffold_gc_forcing;
#[path = "../support/session_scaffold_reference.rs"]
mod session_scaffold_reference;
#[path = "../support/session_scaffold_value.rs"]
mod session_scaffold_value;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../addr_deref_unbox_hardening.rs"]
mod addr_deref_unbox_hardening;
#[path = "../apply_acceptance.rs"]
mod apply_acceptance;
#[path = "../bind_error_then_allocate.rs"]
mod bind_error_then_allocate;
#[path = "../boxed_array_behavior.rs"]
mod boxed_array_behavior;
#[path = "../call_depth_sequential_vs_nested.rs"]
mod call_depth_sequential_vs_nested;
#[path = "../case_trap_scrut_ptr.rs"]
mod case_trap_scrut_ptr;
#[path = "../contags_refresh_on_add_function.rs"]
mod contags_refresh_on_add_function;
#[path = "../datacon_never_used_as_value.rs"]
mod datacon_never_used_as_value;
#[path = "../deep_force_nf.rs"]
mod deep_force_nf;
#[path = "../e6_no_rules_pragma.rs"]
mod e6_no_rules_pragma;
#[path = "../effect_machine.rs"]
mod effect_machine;
#[path = "../emit_case.rs"]
mod emit_case;
#[path = "../emit_expr.rs"]
mod emit_expr;
#[path = "../emit_join.rs"]
mod emit_join;
#[path = "../emit_join_advanced.rs"]
mod emit_join_advanced;
#[path = "../emit_letrec_advanced.rs"]
mod emit_letrec_advanced;
#[path = "../emit_letrec_con.rs"]
mod emit_letrec_con;
#[path = "../external_cancellation.rs"]
mod external_cancellation;
#[path = "../external_env_resolution.rs"]
mod external_env_resolution;
#[path = "../ffi_bytearray_unbox_hardening.rs"]
mod ffi_bytearray_unbox_hardening;
#[path = "../ffi_strlen_unbox_hardening.rs"]
mod ffi_strlen_unbox_hardening;
#[path = "../frame_walker_hardening.rs"]
mod frame_walker_hardening;
#[path = "../free_vars_index_equivalence.rs"]
mod free_vars_index_equivalence;
#[path = "../lazy_let_guard.rs"]
mod lazy_let_guard;
#[path = "../letrec_field_freevar_deps.rs"]
mod letrec_field_freevar_deps;
#[path = "../letrec_lazy_guard.rs"]
mod letrec_lazy_guard;
#[path = "../normalize_differential.rs"]
mod normalize_differential;
#[path = "../raise_lazy_trivial_guard.rs"]
mod raise_lazy_trivial_guard;
#[path = "../scaffold.rs"]
mod scaffold;
#[path = "../shape_trap_dump_oob.rs"]
mod shape_trap_dump_oob;
#[path = "../signal_safety.rs"]
mod signal_safety;
#[path = "../stack_safety.rs"]
mod stack_safety;
#[path = "../stackmap_case_branch_point_creation_mark.rs"]
mod stackmap_case_branch_point_creation_mark;
#[path = "../tco.rs"]
mod tco;
#[path = "../tco_advanced.rs"]
mod tco_advanced;
