#[path = "../cross_mode_harness/mod.rs"]
mod cross_mode_harness;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../agent_mode_encoding.rs"]
mod agent_mode_encoding;
#[path = "../bignum_native.rs"]
mod bignum_native;
#[path = "../bridged_records_extract.rs"]
mod bridged_records_extract;
#[path = "../cache_tests.rs"]
mod cache_tests;
#[path = "../captured_real_core.rs"]
mod captured_real_core;
#[path = "../case_trap_graceful.rs"]
mod case_trap_graceful;
#[path = "../constructors_of_type.rs"]
mod constructors_of_type;
#[path = "../cross_mode_existing.rs"]
mod cross_mode_existing;
#[path = "../cross_mode_targeted.rs"]
mod cross_mode_targeted;
#[path = "../cross_mode_tests.rs"]
mod cross_mode_tests;
#[path = "../eager_list_responses.rs"]
mod eager_list_responses;
#[path = "../extract_poison_diagnostic.rs"]
mod extract_poison_diagnostic;
#[path = "../flinch_katas.rs"]
mod flinch_katas;
#[path = "../gc_and_errors.rs"]
mod gc_and_errors;
#[path = "../gc_stress_text_fold.rs"]
mod gc_stress_text_fold;
