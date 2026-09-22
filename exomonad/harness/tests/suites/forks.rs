#[path = "../support/mod.rs"]
mod support;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../answerer_async_fork.rs"]
mod answerer_async_fork;
#[path = "../companion_collapsed_slice.rs"]
mod companion_collapsed_slice;
#[path = "../fork_child_decl_plane_type.rs"]
mod fork_child_decl_plane_type;
#[path = "../listen_channel.rs"]
mod listen_channel;
