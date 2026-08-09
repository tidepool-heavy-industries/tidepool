//! The Tidepool-owned agent seam: the vocabulary everything above the backend
//! speaks.
//!
//! Every type here is deliberately backend-neutral. If adding a field here
//! requires naming a `codex-codes` type, the field belongs in
//! [`crate::backend::codex`] instead, projected into a neutral shape on the way
//! out.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Tidepool's identity for one agent. Minted by the registry, never by a
/// backend — a backend thread id may be reassigned or absent (an ephemeral
/// thread that failed to start still needs an identity to report the failure
/// against), so the two cannot be the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentId(pub u64);

/// A backend's identity for the thread hosting an agent. Opaque: Tidepool
/// stores, compares and echoes it, and never parses it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BackendThreadId(pub String);

/// A backend's identity for one turn on a thread. Opaque, as above.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TurnId(pub String);

/// A backend's identity for one outstanding server-initiated tool call.
///
/// This is the correlation token for the park/reply primitive: the backend
/// blocks the child's turn until the host answers *this* id. Losing one strands
/// a child turn forever, so it is carried end to end rather than reconstructed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ToolCallId(pub String);

/// One dynamic tool as declared to a backend at agent creation.
///
/// Emitted by the Haskell Generic tool compiler (`compileTools`), one per
/// record selector, and frozen for the lifetime of the agent thread — Codex
/// dynamic tools are thread-scoped, not turn-scoped, so this is not
/// re-negotiable mid-agent.
///
/// `input_schema` is JSON Schema because that is what every current backend
/// speaks, NOT because the authored surface knows about JSON Schema. It is
/// derived from the endpoint's input type by the structural interpreter; no
/// authored Haskell writes one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DynamicToolDeclaration {
    /// The normalized wire name (snake_case of the record selector).
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// What a backend observed, in Tidepool's vocabulary.
///
/// Deliberately coarse: the registry needs enough to route, park, and build a
/// receipt. Rich per-backend activity detail is summarized into
/// [`RuntimeAgentEvent::Activity`] rather than modeled variant-by-variant,
/// because modeling it exactly is how backend vocabulary leaks upward one
/// harmless-looking field at a time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RuntimeAgentEvent {
    /// The thread accepted a turn.
    TurnStarted { turn: TurnId },

    /// The child called one of its generated tools and is now parked until the
    /// host answers `call`. The registry must resolve every one of these
    /// exactly once, including on handler failure — an unresolved call is a
    /// permanently stranded child.
    ToolCall {
        turn: TurnId,
        call: ToolCallId,
        /// The normalized wire name, matching a [`DynamicToolDeclaration::name`].
        tool: String,
        arguments: serde_json::Value,
    },

    /// Observable work that belongs in a receipt but does not change lifecycle.
    Activity(AgentActivity),

    /// The turn ended normally. `output` is the structured terminal value when
    /// the backend produced one; decoding it against the requested result type
    /// is the Haskell side's job, and a decode failure is NOT a success.
    TurnCompleted {
        turn: TurnId,
        output: Option<serde_json::Value>,
    },

    /// The turn ended by interruption. Distinct from `Failed`: the receipt is
    /// still authoritative for what happened before the interrupt.
    TurnInterrupted { turn: TurnId },

    /// The backend reported an error against this agent.
    Failed { error: AgentBackendError },
}

/// Receipt-bearing observations. Model prose is never the source of any field
/// here — PRD 18: "Runtime receipts are authoritative."
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentActivity {
    /// A command the backend actually ran, with what it actually returned.
    Command {
        command: String,
        exit_code: Option<i32>,
    },
    /// A path the backend actually changed.
    FileChanged { path: String },
    /// Token/usage counters as reported by the backend, kept as a flat map so a
    /// backend adding a counter does not change this type.
    Usage { counters: BTreeMap<String, i64> },
    /// Anything else worth journaling but not worth a variant yet.
    Other {
        kind: String,
        detail: serde_json::Value,
    },
}

/// Backend failures, projected into causes Tidepool can act on.
///
/// The distinction that matters to a caller is retryable-vs-not and
/// mine-vs-theirs; a backend's own error code is preserved in `detail` for
/// diagnosis without becoming a matchable part of this type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
pub enum AgentBackendError {
    /// The backend process is gone. Every agent it hosted is lost.
    #[error("agent backend process unavailable: {detail}")]
    BackendUnavailable { detail: String },

    /// The backend refused the request as malformed or unsupported. This is a
    /// Tidepool bug or a version skew, never a user error.
    #[error("agent backend rejected request: {detail}")]
    ProtocolRejected { detail: String },

    /// The backend accepted the request and reported a runtime failure against
    /// the agent (rate limit, model error, sandbox denial).
    #[error("agent run failed: {detail}")]
    RunFailed { detail: String },
}

/// What a caller-assigned workspace grants a worker.
///
/// TRANSITIONAL (PRD 19 revision, Inanna 2026-08-08): agent creation is being
/// coupled to worktree allocation — one managed worktree per agent, all agents
/// isolated, and once PRD 19 lands a managed worktree is the ONLY workspace an
/// agent can receive. This free-form shape is valid for pre-PRD-19 spikes and
/// dogfood only. Do not build a writer-lease or shared-directory model on it;
/// the coupling dissolves that problem rather than solving it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub cwd: String,
    pub access: WorkspaceAccess,
}

/// TRANSITIONAL — see [`Workspace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceAccess {
    ReadOnly,
    WorkspaceWrite,
}
