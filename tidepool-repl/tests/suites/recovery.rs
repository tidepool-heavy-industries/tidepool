#[path = "../common/mod.rs"]
mod common;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../error_recovery.rs"]
mod error_recovery;
#[path = "../it_binding.rs"]
mod it_binding;
#[path = "../lifecycle_meta.rs"]
mod lifecycle_meta;
