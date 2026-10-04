#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
#[path = "../emitted_rust_is_rustfmt_stable.rs"]
mod emitted_rust_is_rustfmt_stable;
#[path = "../event_haskell_contract.rs"]
mod event_haskell_contract;
#[path = "../event_wire_rust.rs"]
mod event_wire_rust;
#[path = "../exomonad_control_contract.rs"]
mod exomonad_control_contract;
#[path = "../polymorphism_validation.rs"]
mod polymorphism_validation;
#[path = "../schema_validation.rs"]
mod schema_validation;
#[path = "../worktree_adapters.rs"]
mod worktree_adapters;
#[path = "../worktree_haskell_contract.rs"]
mod worktree_haskell_contract;
#[path = "../worktree_wire_rust.rs"]
mod worktree_wire_rust;

#[path = "../external_references.rs"]
mod external_references;
