// Each module remains a separate source file; nextest isolates each test process.
#[path = "../build_products_dir_differential.rs"]
mod build_products_dir_differential;
#[path = "../exact_export_facade.rs"]
mod exact_export_facade;
#[path = "../green_thread_representation.rs"]
mod green_thread_representation;
#[path = "../managed_exit_cell.rs"]
mod managed_exit_cell;
#[path = "../tenure_resume_gc_repro.rs"]
mod tenure_resume_gc_repro;
#[path = "../word64_primops_random_probe.rs"]
mod word64_primops_random_probe;
