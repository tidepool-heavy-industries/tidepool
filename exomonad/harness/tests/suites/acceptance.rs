#[path = "../support/mod.rs"]
mod support;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../acceptance_askuser.rs"]
mod acceptance_askuser;
#[path = "../acceptance_boot_compile_count.rs"]
mod acceptance_boot_compile_count;
#[path = "../acceptance_cross_turn.rs"]
mod acceptance_cross_turn;
#[path = "../acceptance_finalize.rs"]
mod acceptance_finalize;
#[path = "../acceptance_lazy_boot.rs"]
mod acceptance_lazy_boot;
#[path = "../acceptance_multi_target.rs"]
mod acceptance_multi_target;
#[path = "../acceptance_selfharness.rs"]
mod acceptance_selfharness;
