#[path = "../support/gc_scaffold.rs"]
mod gc_scaffold;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../proptest_boundary_roundtrip.rs"]
mod proptest_boundary_roundtrip;
#[path = "../proptest_case_dispatch.rs"]
mod proptest_case_dispatch;
#[path = "../proptest_compile.rs"]
mod proptest_compile;
#[path = "../proptest_gc_recursion.rs"]
mod proptest_gc_recursion;
#[path = "../proptest_ghc_idioms.rs"]
mod proptest_ghc_idioms;
#[path = "../proptest_ghc_idioms_widen.rs"]
mod proptest_ghc_idioms_widen;
#[path = "../proptest_heap_layout.rs"]
mod proptest_heap_layout;
#[path = "../proptest_host_arrays.rs"]
mod proptest_host_arrays;
#[path = "../proptest_host_fns.rs"]
mod proptest_host_fns;
#[path = "../proptest_jit_dispatch.rs"]
mod proptest_jit_dispatch;
#[path = "../proptest_numeric_conversions.rs"]
mod proptest_numeric_conversions;
#[path = "../proptest_primops_differential.rs"]
mod proptest_primops_differential;
