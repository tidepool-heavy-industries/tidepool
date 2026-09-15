// Each module remains a separate source file; nextest isolates each test process.
#[path = "../proptest_boundary_roundtrip.rs"]
mod proptest_boundary_roundtrip;
#[path = "../proptest_compile.rs"]
mod proptest_compile;
#[path = "../proptest_heap_layout.rs"]
mod proptest_heap_layout;
#[path = "../proptest_host_arrays.rs"]
mod proptest_host_arrays;
#[path = "../proptest_host_fns.rs"]
mod proptest_host_fns;
