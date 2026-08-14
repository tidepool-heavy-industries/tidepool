//! The Tidepool-owned agent seam: the vocabulary everything above the backend
//! speaks.
//!
//! Every type here is deliberately backend-neutral. If adding a field here
//! requires naming a `codex-codes` type, the field belongs in
//! [`crate::backend::codex`] instead, projected into a neutral shape on the way
//! out.

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

/// One dynamic tool as declared to a backend at agent creation, frozen for
/// the lifetime of the agent thread — Codex dynamic tools are thread-scoped,
/// not turn-scoped, so this is not re-negotiable mid-agent.
///
/// `input_schema` is JSON Schema because that is what every current backend
/// speaks, not because the authored surface knows about JSON Schema; it is
/// derived from the endpoint's input type by the structural interpreter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DynamicToolDeclaration {
    /// The normalized wire name (snake_case of the record selector).
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A backend's identity for one parked tool call. Opaque, as above — it is the
/// correlation token a reply must carry back, and Tidepool only ever echoes it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ToolCallId(pub String);

/// One tool call the child made, parked awaiting the parent's answer.
///
/// The correlation triple (`thread`, `turn`, `call`) rides every call because
/// it is what makes a cross-agent misroute DETECTABLE rather than a silent
/// wrong answer.
///
/// While a call is parked the child's turn is stopped: nothing is spent, and
/// nothing times out except by the backend's own clock. The parent is free to
/// take as long as answering honestly requires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub call: ToolCallId,
    pub thread: BackendThreadId,
    pub turn: TurnId,
    /// The wire name the child invoked — one of the declared
    /// [`DynamicToolDeclaration::name`]s, or something else entirely, which is
    /// a fact the parent must be able to refuse rather than a fact to assume.
    pub tool: String,
    /// The child's arguments, as JSON. Named-field objects: the model is the
    /// encoder here and it knows only the declared `input_schema`.
    pub arguments: serde_json::Value,
}

/// What the parent answered a [`ToolCall`] with.
///
/// The two cases are the protocol's own (`success: true/false` plus content),
/// NOT a JSON-RPC error — a failing tool is an ordinary conversational fact the
/// child can react to, and sending a transport error instead would strand the
/// call. Every parked call gets one of these; there is no third "ignore" case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToolOutcome {
    /// The handler ran and produced a value the child should read.
    Answered(serde_json::Value),
    /// The handler refused or failed. The text is what the child sees, so it
    /// is written for the child, not for a log.
    Refused(String),
}

/// A parent's answer bound to the call it answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolReply {
    pub call: ToolCallId,
    pub outcome: ToolOutcome,
}

/// Why the turn pump stopped.
///
/// A turn either parks on a tool call or ends. There is deliberately no
/// "still running" variant: the pump blocks until one of these two is true, so
/// a caller can never observe a turn mid-flight and has nothing to poll.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TurnEvent {
    /// The child called a tool. The request is PARKED — the backend has
    /// written no response — until [`AgentBackend::resume`](crate::backend::AgentBackend::resume).
    ToolCall(ToolCall),
    /// The turn reached its terminal state.
    Completed(CycleOutcome),
}

/// How hard the model should think. Distinct from [`ModelPolicy`]: the model
/// is WHICH engine, this is HOW MUCH of it to spend, and the two move
/// independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

/// Tokens one turn actually consumed, as the backend reported them.
///
/// `Option`-free on purpose once present: a backend that reports usage reports
/// all of it. Whether it reported any is [`CycleOutcome::usage`]'s `Option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

/// Receipt-bearing observations. Model prose is never the source of any
/// field here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentActivity {
    /// A command the backend actually ran, with what it actually returned.
    Command {
        command: String,
        exit_code: Option<i32>,
    },
    /// A path the backend actually changed.
    FileChanged { path: String },
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

/// How a spawn names the model it wants. Runtime-RESOLVED, never a hardcoded
/// slug: the backend queries its own model list and picks the concrete
/// model, and [`CycleOutcome::resolved_model`] records EXACTLY what it got —
/// a receipt naming a tier rather than the model it actually ran is not
/// checkable.
///
/// Each variant names an ALLOWLIST, in preference order. The allowlist is the
/// mechanism by which a banned model is unreachable: resolution takes the first
/// listed slug the backend actually offers and FAILS otherwise, so no slug
/// outside the list can be selected however the catalogue changes. A denylist
/// would have to anticipate every future name; this does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelPolicy {
    /// Cheap plumbing: prefer `gpt-5.4-mini`, else `gpt-5.6-luna`.
    CheapPlumbing,
    /// The cheapest gpt-5.6 tier, pinned: `gpt-5.6-luna` and nothing else.
    ///
    /// Distinct from [`ModelPolicy::CheapPlumbing`], which would resolve to
    /// the cheaper `gpt-5.4-mini` — a specific budget grant names this exact
    /// tier, and cheaper is not the same as granted.
    CheapestGpt56,
}

/// What one thread is created with. Frozen for the thread's lifetime — dynamic
/// tools are thread-scoped, not turn-scoped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadSpec {
    pub ephemeral: bool,
    pub dynamic_tools: Vec<DynamicToolDeclaration>,
}

/// One work cycle: one turn on one thread, in one workspace.
///
/// `cwd` is supplied HERE and not at thread creation — the request shape that
/// avoids the documented Codex project-trust config write (PRD 18 acceptance
/// criterion 11; see `backend::codex`'s module docs). Backends that don't
/// share that hazard still honor the same split, so the seam has one shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CycleSpec {
    /// Absolute path of the (bound, managed) worktree the worker runs in.
    pub cwd: String,
    /// The initial task prompt, ordinary text.
    pub task: String,
    /// JSON Schema constraining the terminal message. Derived from the
    /// caller's result type by the structural interpreter — no authored
    /// Haskell writes one, same rule as [`DynamicToolDeclaration::input_schema`].
    pub output_schema: Option<serde_json::Value>,
    pub model: ModelPolicy,
    pub effort: ReasoningEffort,
    /// Extra directories the backend's write sandbox must admit beyond
    /// `cwd`. In practice: the source repository's `.git` — a LINKED
    /// worktree's git metadata (objects, refs, `worktrees/<id>/`) lives
    /// there, so without it every `git commit` inside the worktree is
    /// blocked by the sandbox (the curator's first committed run failed
    /// exactly this way; dogfood 2026-08-14). `serde(default)` keeps
    /// recorded fixtures decodable.
    #[serde(default)]
    pub extra_writable_roots: Vec<String>,
}

/// What the terminal message actually was. Typed rather than `Option<Value>`
/// because "the model wrote non-JSON text" and "the model wrote nothing" are
/// different facts a caller acts on differently, and collapsing either into
/// a decode failure would hide which side broke the contract.
///
/// `Structured` is NOT yet a typed success: decoding it against the caller's
/// requested type happens on the Haskell side, and a decode failure there is
/// a typed error, never a success.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CycleResultPayload {
    /// The final agent message parsed as JSON (the `outputSchema` contract:
    /// the message TEXT is the schema-conforming JSON — there is no separate
    /// structured-output field).
    Structured(serde_json::Value),
    /// The final agent message, present but not parseable as JSON.
    Unstructured(String),
    /// The turn completed without any agent message.
    Absent,
}

/// Everything one completed cycle reports back through the seam.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CycleOutcome {
    pub turn: TurnId,
    pub payload: CycleResultPayload,
    /// Receipt-bearing observations, in the order the backend reported them.
    pub activity: Vec<AgentActivity>,
    /// The EXACT model the backend resolved and ran — recorded per the
    /// [`ModelPolicy`] rule, never the tier name.
    pub resolved_model: String,
    /// What the turn actually cost, when the backend reported it. `None` means
    /// the backend said nothing about usage — never "it was free".
    pub usage: Option<TokenUsage>,
}
