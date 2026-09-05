#[path = "../common/mod.rs"]
mod common;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../gc_field_replay.rs"]
mod gc_field_replay;
#[path = "../gc_heap_verify_stress.rs"]
mod gc_heap_verify_stress;
#[path = "../info_introspect.rs"]
mod info_introspect;
#[path = "../lifecycle_state.rs"]
mod lifecycle_state;
#[path = "../lost_session.rs"]
mod lost_session;
#[path = "../multi_binder.rs"]
mod multi_binder;
