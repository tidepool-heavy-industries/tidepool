#[path = "../common/mod.rs"]
mod common;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../bindings_dedup.rs"]
mod bindings_dedup;
#[path = "../shadow_rebind.rs"]
mod shadow_rebind;
#[path = "../value_fidelity.rs"]
mod value_fidelity;
