//! Typed headless-subagent backends.
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

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod backend;
pub mod interactive;
pub mod seam;
pub mod spawn;

pub use backend::{AgentBackend, AgentBackendFactory, BackendCanceller};
pub use interactive::{
    InteractiveAgentBackend, InteractiveAgentProcess, InteractiveAgentSpec, InteractiveFuture,
    InteractiveLaunchMode, InteractiveMcpServer,
};
pub use seam::{
    AgentBackendError, AgentId, BackendThreadId, CycleOutcome, CycleResultPayload, CycleSpec,
    ModelPolicy, ReasoningEffort, ThreadSpec, TokenUsage, ToolCall, ToolCallId, ToolDeclaration,
    ToolOutcome, ToolReply, TurnEvent, TurnId,
};
pub use spawn::{
    AnswerFailure, CoupledSpawner, CycleProgress, CycleSaga, OneCycleRun, ParkedCycle, SpawnError,
    SpawnReceipt, SpawnRequest, SpawnStage, SpawnStep, SpawnSubstrate, SpawnWorkspace, WorkerRun,
    MAX_TOOL_ROUNDS,
};

/// Run the installed interactive-agent MCP sidecar.
///
/// Backend vocabulary remains contained in its adapter; the small binary calls
/// this neutral entry point and therefore does not learn the concrete backend.
pub async fn run_interactive_node() -> Result<(), Box<dyn std::error::Error>> {
    backend::codex::node::run_sidecar_from_env().await?;
    Ok(())
}
