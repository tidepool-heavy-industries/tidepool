#[path = "../support/mod.rs"]
mod support;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../agent_stack_scoping.rs"]
mod agent_stack_scoping;
#[path = "../decl_plane_run_scoping.rs"]
mod decl_plane_run_scoping;
#[path = "../dogfood_harness_typecheck.rs"]
mod dogfood_harness_typecheck;
#[path = "../dogfood_observability.rs"]
mod dogfood_observability;
#[path = "../finalize_type_pinning.rs"]
mod finalize_type_pinning;
#[path = "../operator_gate_lifecycle.rs"]
mod operator_gate_lifecycle;
#[path = "../outer_effects.rs"]
mod outer_effects;
#[path = "../outer_fanout.rs"]
mod outer_fanout;
#[path = "../outer_subagent.rs"]
mod outer_subagent;
#[path = "../timing_emission_pin.rs"]
mod timing_emission_pin;
#[path = "../turn_lease.rs"]
mod turn_lease;
