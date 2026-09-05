#[path = "../common/mod.rs"]
mod common;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../batch_turns_spawn_census.rs"]
mod batch_turns_spawn_census;
