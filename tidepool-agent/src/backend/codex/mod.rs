//! The Codex `app-server` backend.
//!
//! # Containment
//!
//! This module and its children are the ONLY place `codex-codes`, app-server
//! JSON-RPC, thread/turn wire types, or the word "Codex" may appear. Anything
//! crossing out of here is [`crate::seam`] vocabulary.
//!
//! # Pinning
//!
//! The CLI and the client crate are pinned TOGETHER; see `PINNED_CLI_VERSION`.
//! Dynamic tools are an experimental app-server surface, so a version bump is a
//! deliberate act with a fixture re-run behind it, not a lockfile refresh.
//!
//! # Protocol truth
//!
//! `codex-codes` 0.146.4 drops `dynamicTools` and its spec types entirely —
//! experimental-gated fields are cut from schema generation — so this module
//! hand-rolls `DynamicToolSpec` and friends and sends `thread/start` through
//! the crate's raw `request()` escape hatch. Full offline sourcing:
//! `fixtures/app-server-0.146.0/PROTOCOL-NOTES.md`.
//!
//! # Config isolation
//!
//! No normal worker run may mutate the operator's Codex user configuration.
//! The operator's `~/.codex` holds a live
//! ChatGPT authentication; credentials are never copied or rewritten into an
//! isolated `CODEX_HOME` to route around this. The shape that avoids the
//! documented project-trust write is to omit `cwd` from thread start and supply
//! it at turn start — proving that is sufficient is the first thing this
//! adapter does, before any run that spends a token.

pub mod driver;
pub mod dynamic_tools;
pub mod isolation;
pub mod process;
pub mod replay;
pub mod transport;

pub use driver::{
    CodexAgentBackend, CodexBackendFactory, CHEAPEST_GPT56_PREFERENCE, CHEAP_PLUMBING_PREFERENCE,
    DEFAULT_TURN_TIMEOUT,
};
pub use replay::{ReplayError, TranscriptTransport};
pub use transport::Transport;

/// The Codex CLI version this adapter is pinned to and its fixtures were
/// recorded against.
pub const PINNED_CLI_VERSION: &str = "0.146.0";
