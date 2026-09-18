// Each module remains a separate source file; nextest isolates each test process.
#[path = "../introspection_public_api.rs"]
mod introspection_public_api;
#[path = "../prepared_reply_types.rs"]
mod prepared_reply_types;
#[path = "../prepared_residency.rs"]
mod prepared_residency;
#[path = "../prepared_turn.rs"]
mod prepared_turn;
#[path = "../prepared_unit_codegen_cost.rs"]
mod prepared_unit_codegen_cost;
#[path = "../run_llm_turn_sidecar.rs"]
mod run_llm_turn_sidecar;
#[path = "../session_decl_accum.rs"]
mod session_decl_accum;
#[path = "../session_decl_recovery.rs"]
mod session_decl_recovery;
#[path = "../session_decl_scope_tree.rs"]
mod session_decl_scope_tree;
#[path = "../session_scope_retirement.rs"]
mod session_scope_retirement;
#[path = "../session_table_qualified_identity.rs"]
mod session_table_qualified_identity;
#[path = "../sweep_repoint_smoke.rs"]
mod sweep_repoint_smoke;
#[path = "../user_library.rs"]
mod user_library;
