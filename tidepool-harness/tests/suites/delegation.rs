#[path = "../support/mod.rs"]
mod support;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../delegate_positive_path.rs"]
mod delegate_positive_path;
#[path = "../delegate_type_pinning.rs"]
mod delegate_type_pinning;
