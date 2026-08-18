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

pub mod backend;
pub mod seam;
pub mod spawn;

pub use backend::{
    run_turn_to_completion, AgentBackend, AgentBackendFactory, BackendCanceller,
    ClosureBackendFactory,
};
pub use seam::{
    AgentBackendError, AgentId, BackendThreadId, CycleOutcome, CycleResultPayload, CycleSpec,
    DynamicToolDeclaration, ModelPolicy, ReasoningEffort, ThreadSpec, TokenUsage, ToolCall,
    ToolCallId, ToolOutcome, ToolReply, TurnEvent, TurnId,
};
pub use spawn::{
    AnswerFailure, CoupledSpawner, CycleProgress, CycleSaga, OneCycleRun, ParkedCycle, SpawnError,
    SpawnReceipt, SpawnRequest, SpawnStage, SpawnStep, SpawnSubstrate, SpawnWorkspace, WorkerRun,
    MAX_TOOL_ROUNDS,
};
