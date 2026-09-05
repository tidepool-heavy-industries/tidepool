#[path = "../common/mod.rs"]
mod common;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../decl_plane.rs"]
mod decl_plane;
