//! Typed headless-subagent backends (PRD 18,
//! `plans/self-iterating-harness/18-typed-subagent-spawning-prd.md`).
//!
//! # The containment boundary
//!
//! This crate exists to be the ONLY place in the workspace that knows a coding
//! backend exists. Two rules make that structural rather than aspirational:
//!
//! 1. `codex-codes`, app-server JSON-RPC types, and the word "Codex" appear
//!    ONLY under [`backend::codex`]. Every other module — and every other
//!    crate — speaks the vocabulary in [`seam`].
//! 2. Nothing in [`seam`] may be defined in terms of a backend type. A seam
//!    type that is a re-export or a newtype of a `codex-codes` type has already
//!    broken the boundary, because a backend version bump then reaches the
//!    Haskell surface.
//!
//! PRD 18 non-goal, quoted: "Making `codex-codes` types part of Tidepool's
//! public Rust or Haskell API." The adapter is pinned, compatibility-tested,
//! and replaceable — vendoring or rewriting it must be a change confined to
//! [`backend::codex`].
//!
//! # What lives here and what does not
//!
//! Here: process lifecycle, request correlation, protocol fixtures, and the
//! translation between a backend's wire events and [`seam::RuntimeAgentEvent`].
//!
//! Not here: the authored Haskell surface (`Call`/`Notify`/`Tool`/`AsServerT`),
//! the Generic tool compiler, the agent registry, and realm parking. Those
//! consume this crate; they do not live in it.

pub mod backend;
pub mod seam;
pub mod spawn;

pub use backend::OneCycleBackend;
pub use seam::{
    AgentBackendError, AgentId, BackendThreadId, CycleOutcome, CycleResultPayload, CycleSpec,
    DynamicToolDeclaration, ModelPolicy, RuntimeAgentEvent, ThreadSpec, ToolCallId, TurnId,
};
pub use spawn::{
    CoupledSpawner, OneCycleRun, SpawnError, SpawnReceipt, SpawnRequest, SpawnStage,
    SpawnWorkspace, WorkerRun,
};
