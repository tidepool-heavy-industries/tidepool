#[path = "../support/mod.rs"]
mod support;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../minimal_watch_list.rs"]
mod minimal_watch_list;
#[path = "../nested_async_repro.rs"]
mod nested_async_repro;
#[path = "../node_mailboxes.rs"]
mod node_mailboxes;
#[path = "../reinterpret_rowchange_repro.rs"]
mod reinterpret_rowchange_repro;
#[path = "../selfharness_decl_plane_replay.rs"]
mod selfharness_decl_plane_replay;
#[path = "../stable_effects_core_decl_plane.rs"]
mod stable_effects_core_decl_plane;
#[path = "../state_injection_memo_hit.rs"]
mod state_injection_memo_hit;
