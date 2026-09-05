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
#[path = "../binding_table_realm_isolation.rs"]
mod binding_table_realm_isolation;
#[path = "../binding_tip_lazy_sharing.rs"]
mod binding_tip_lazy_sharing;
#[path = "../converge_proof.rs"]
mod converge_proof;
#[path = "../error_binding_guard.rs"]
mod error_binding_guard;
#[path = "../populated_session_second_fragment.rs"]
mod populated_session_second_fragment;
#[path = "../realm_cycle_scoped_drop.rs"]
mod realm_cycle_scoped_drop;
#[path = "../realm_global_id_isolation.rs"]
mod realm_global_id_isolation;
#[path = "../realm_handles.rs"]
mod realm_handles;
#[path = "../realm_leak_comparison.rs"]
mod realm_leak_comparison;
#[path = "../realm_module_growth.rs"]
mod realm_module_growth;
#[path = "../realm_multi_continuation.rs"]
mod realm_multi_continuation;
#[path = "../realm_per_realm_fields.rs"]
mod realm_per_realm_fields;
#[path = "../realm_root_growth.rs"]
mod realm_root_growth;
#[path = "../session_seed_external_env_root_retention.rs"]
mod session_seed_external_env_root_retention;
#[path = "../suspendable_materialization.rs"]
mod suspendable_materialization;
