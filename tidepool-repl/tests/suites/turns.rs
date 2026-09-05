#[path = "../common/mod.rs"]
mod common;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../ask_resume.rs"]
mod ask_resume;
#[path = "../auto_verdict_dispatch.rs"]
mod auto_verdict_dispatch;
#[path = "../bare_expr_retry_census.rs"]
mod bare_expr_retry_census;
#[path = "../block_value_semantics.rs"]
mod block_value_semantics;
#[path = "../cancel_lifecycle.rs"]
mod cancel_lifecycle;
#[path = "../do_block_invariant.rs"]
mod do_block_invariant;
#[path = "../effects_smoke.rs"]
mod effects_smoke;
