//! Backend adapters.
//!
//! One module per backend. A backend module owns its wire types, its process
//! lifecycle, and its correlation bookkeeping, and exposes only
//! [`crate::seam`] vocabulary.

use crate::seam::{
    AgentBackendError, BackendThreadId, CycleOutcome, CycleSpec, ThreadSpec, ToolReply, TurnEvent,
};

pub mod codex;
pub mod mock;

/// The backend seam: create a thread, start a turn, answer parked tool calls
/// until the turn ends.
///
/// # Why this REPLACED `OneCycleBackend` rather than widening it
///
/// Lane 1's trait was `start_thread` + `run_cycle`, where `run_cycle` ran a
/// whole turn to completion behind one blocking call. That shape cannot
/// express a turn that STOPS in the middle to ask the parent something, and a
/// turn that stops in the middle is this lane's entire subject. Widening it
/// with an optional callback would have put the parent's tool handlers inside
/// a Rust closure — and the parent's tool handlers are authored Haskell
/// (`Tidepool.Agent.Contract`'s `Tool` carries `handler :: input -> m output`),
/// which no Rust closure can run.
///
/// So the seam is a STEP function, and the driving loop lives in Haskell:
///
/// ```text
/// start_turn ──► ToolCall ──► (parent's Haskell handler runs) ──► resume ──► ToolCall ──► …
///        └────► Completed                                            └────► Completed
/// ```
///
/// Each step returns normally, so every effect call across this seam stays
/// synchronous and run-to-completion — the property the whole handler layer is
/// built on. What is parked between steps is the CHILD's request on the far
/// side of the backend, which costs nothing but an unwritten response.
///
/// Sync by design, as before: effect handlers are sync, and a backend that is
/// internally async (the codex adapter drives a tokio stdio transport) owns its
/// own runtime the way `LlmHandler` does, rather than making every caller async.
///
/// # The obligation a parked call creates
///
/// Between a [`TurnEvent::ToolCall`] and its [`resume`](AgentBackend::resume)
/// the child's turn is stopped and its request unanswered. An implementation
/// must survive an arbitrary gap there — the parent may be running a long
/// handler — and a caller that abandons a parked call leaves the child parked
/// until the backend's own timeout. Ownership is what bounds that: dropping the
/// backend must take the child down with it.
pub trait AgentBackend {
    /// Create a thread per `spec`, including its dynamic tool declarations.
    /// Success means the backend ACCEPTED the thread (the saga's
    /// `ThreadAccepted` stage), not that any work ran.
    ///
    /// Declarations are frozen here because they are thread-scoped, not
    /// turn-scoped — an agent cannot be handed a new tool mid-life.
    fn start_thread(&mut self, spec: &ThreadSpec) -> Result<BackendThreadId, AgentBackendError>;

    /// Start one turn and run it until it parks on a tool call or completes.
    fn start_turn(
        &mut self,
        thread: &BackendThreadId,
        spec: &CycleSpec,
    ) -> Result<TurnEvent, AgentBackendError>;

    /// Answer the parked call named by `reply.call` and run on to the next
    /// stop.
    ///
    /// Answering a call that is not the parked one is a caller bug, and
    /// implementations report it as [`AgentBackendError::ProtocolRejected`]
    /// rather than answering the wrong call — a misrouted reply is exactly the
    /// failure the correlation triple exists to make detectable.
    fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, AgentBackendError>;

    /// This backend's own record of its wire traffic so far, as opaque JSONL
    /// lines — empty when it keeps none.
    ///
    /// `Vec<String>` and NOT a typed frame, deliberately: the seam may not name
    /// a backend's protocol types (`lib.rs`'s containment rule), and the only
    /// consumer of a recording is a replay transport living inside the same
    /// backend module, which can parse its own format. To everything above the
    /// seam these are bytes to write to a file.
    ///
    /// It exists so ONE bounded live run can buy repeatable protocol coverage:
    /// record once, replay in CI forever. A recording is evidence; a
    /// hand-written imitation of a backend is drift waiting to happen.
    fn transcript_jsonl(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Run one turn to completion, refusing every tool call it makes.
///
/// The no-tools path, expressed as a COMBINATOR over the step seam rather than
/// as a second primitive — PRD 18's own rule for synchronous delegation. A
/// thread created with no declarations should make no calls at all; if one
/// arrives anyway it is refused rather than ignored, because an unanswered call
/// parks the child until its timeout.
pub fn run_turn_to_completion(
    backend: &mut dyn AgentBackend,
    thread: &BackendThreadId,
    spec: &CycleSpec,
) -> Result<CycleOutcome, AgentBackendError> {
    let mut event = backend.start_turn(thread, spec)?;
    loop {
        match event {
            TurnEvent::Completed(outcome) => return Ok(outcome),
            TurnEvent::ToolCall(call) => {
                let reply = ToolReply {
                    call: call.call.clone(),
                    outcome: crate::seam::ToolOutcome::Refused(format!(
                        "no such tool: {} — this agent was created with no dynamic tools",
                        call.tool
                    )),
                };
                event = backend.resume(reply)?;
            }
        }
    }
}
