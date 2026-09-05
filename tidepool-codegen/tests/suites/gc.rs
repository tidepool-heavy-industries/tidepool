#[path = "../support/gc_scaffold.rs"]
mod gc_scaffold;
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
#[path = "../support/mod.rs"]
mod support;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../apply_cont_heap_composition_gc.rs"]
mod apply_cont_heap_composition_gc;
#[path = "../array_gc_safety.rs"]
mod array_gc_safety;
#[path = "../con_midfill_gc_safety.rs"]
mod con_midfill_gc_safety;
#[path = "../continuation_gc_root.rs"]
mod continuation_gc_root;
#[path = "../gc_audit.rs"]
mod gc_audit;
#[path = "../gc_fault_recovery.rs"]
mod gc_fault_recovery;
#[path = "../gc_frame_walker.rs"]
mod gc_frame_walker;
#[path = "../gc_write_barrier.rs"]
mod gc_write_barrier;
#[path = "../heap_bridge_tests.rs"]
mod heap_bridge_tests;
#[path = "../heap_force_tests.rs"]
mod heap_force_tests;
#[path = "../heap_verify_lane.rs"]
mod heap_verify_lane;
#[path = "../nested_child_gc_rooting.rs"]
mod nested_child_gc_rooting;
#[path = "../nested_child_response_materialization_gc.rs"]
mod nested_child_response_materialization_gc;
#[path = "../stackmap_join_param_gc_coverage.rs"]
mod stackmap_join_param_gc_coverage;
