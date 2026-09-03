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

pub use backend::codex::trust_interactive_project;
pub use backend::{AgentBackend, AgentBackendFactory, BackendCanceller};
pub use interactive::{
    InteractiveAgentBackend, InteractiveAgentCommand, InteractiveAgentInstallation,
    InteractiveAgentSpec, InteractiveFuture, InteractiveLaunchMode, InteractiveNativeSandbox,
};
pub use seam::{
    AgentBackendError, AgentId, BackendThreadId, CycleOutcome, CycleResultPayload, CycleSpec,
    ModelPolicy, ReasoningEffort, ThreadSpec, TokenUsage, ToolCall, ToolCallId, ToolDeclaration,
    ToolKind, ToolOutcome, ToolReply, TurnEvent, TurnId,
};
pub use spawn::{
    AnswerFailure, CoupledSpawner, CycleProgress, CycleSaga, OneCycleRun, ParkedCycle, SpawnError,
    SpawnReceipt, SpawnRequest, SpawnStage, SpawnStep, SpawnSubstrate, SpawnWorkspace, WorkerRun,
    MAX_TOOL_ROUNDS,
};

/// Construct the installed native interactive-agent adapter behind its
/// backend-neutral seam.
#[must_use]
pub fn native_interactive_backend(
    installation: InteractiveAgentInstallation,
) -> std::sync::Arc<dyn InteractiveAgentBackend> {
    std::sync::Arc::new(backend::codex::CodexInteractiveBackend::new(installation))
}

/// Resolve and behaviorally verify the interactive agent installed for Shoal.
pub async fn resolve_native_interactive_agent(
) -> Result<InteractiveAgentInstallation, AgentBackendError> {
    backend::codex::node::resolve_installation().await
}

/// Restore the exact installation passed through Shoal's private host launch.
pub fn native_interactive_agent_from_parts(
    executable: std::path::PathBuf,
    version: String,
) -> Result<InteractiveAgentInstallation, AgentBackendError> {
    backend::codex::node::installation_from_parts(executable, version)
}

/// Read and validate the opaque conversation binding established by the
/// interactive host-tools session callback.
pub async fn read_interactive_binding(
    path: &std::path::Path,
) -> Result<BackendThreadId, AgentBackendError> {
    backend::codex::node::read_binding(path)
        .await
        .map(|binding| binding.thread)
}

/// Durably retain the exact conversation binding selected by a composition
/// root after the newly launched interactive process has proved readiness.
pub async fn persist_interactive_binding(
    path: &std::path::Path,
    thread: BackendThreadId,
) -> Result<(), AgentBackendError> {
    backend::codex::node::write_binding(path, thread).await
}
