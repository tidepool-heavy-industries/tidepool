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
//! # Config isolation
//!
//! No normal worker run may mutate the operator's Codex user configuration
//! (PRD 18 acceptance criterion 11). The operator's `~/.codex` holds a live
//! ChatGPT authentication; credentials are never copied or rewritten into an
//! isolated `CODEX_HOME` to route around this. The shape that avoids the
//! documented project-trust write is to omit `cwd` from thread start and supply
//! it at turn start — proving that is sufficient is the first thing this
//! adapter does, before any run that spends a token.

/// The Codex CLI version this adapter is pinned to and its fixtures were
/// recorded against.
pub const PINNED_CLI_VERSION: &str = "0.146.0";

// Bring-up lands here: process lifecycle, initialize handshake, thread/turn
// requests, `item/tool/call` correlation, and the projection into
// `crate::seam::RuntimeAgentEvent`.
