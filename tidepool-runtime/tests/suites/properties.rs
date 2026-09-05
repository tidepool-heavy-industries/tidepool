// Each module remains a separate source file; nextest isolates each test process.
#[path = "../proptest_cache_layer.rs"]
mod proptest_cache_layer;
#[path = "../proptest_gc_pressure.rs"]
mod proptest_gc_pressure;
#[path = "../proptest_haskell_pipeline.rs"]
mod proptest_haskell_pipeline;
#[path = "../proptest_jit_vs_eval.rs"]
mod proptest_jit_vs_eval;
#[path = "../proptest_letrec.rs"]
mod proptest_letrec;
#[path = "../proptest_render_json.rs"]
mod proptest_render_json;
