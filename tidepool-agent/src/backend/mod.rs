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
/// A STEP function, not run-to-completion: a turn that stops mid-flight to
/// ask the parent something cannot be expressed by a single blocking call,
/// because the parent's tool handlers are authored Haskell
/// (`Tidepool.Agent.Contract`'s `Tool` carries `handler :: input -> m
/// output`), which no Rust closure can run. So the driving loop lives in
/// Haskell, and each step here returns normally, keeping every effect call
/// across this seam synchronous — a backend that is internally async (the
/// codex adapter drives a tokio stdio transport) owns its own runtime rather
/// than making every caller async.
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
    /// `Vec<String>` and NOT a typed frame, deliberately: the seam may not
    /// name a backend's protocol types. To everything above the seam these
    /// are bytes to write to a file.
    ///
    /// It exists so ONE bounded live run can buy repeatable protocol
    /// coverage: record once, replay in CI forever.
    fn transcript_jsonl(&self) -> Vec<String> {
        Vec::new()
    }

    /// A handle that reaps this backend FROM ANOTHER THREAD, making an
    /// in-flight seam call return.
    ///
    /// Take it BEFORE the cycle starts running. Once a cycle thread is inside
    /// [`start_turn`](AgentBackend::start_turn) it holds `&mut` on the backend
    /// and nothing else can reach it — which is the whole reason a canceller is
    /// a separate, `Send + Sync` object rather than a `&mut self` method. A
    /// flag the blocked thread would have to check is not cancellation; it is a
    /// request the blocked thread cannot read.
    ///
    /// Default: a no-op canceller. Correct for a backend with nothing to reap
    /// (an in-process one, and [`codex::replay`], which pumps a transcript with
    /// no process behind it) — and what keeps every such backend compiling.
    fn canceller(&self) -> Box<dyn BackendCanceller> {
        Box::new(NoopCanceller)
    }
}

/// Reaps a backend's underlying process/session FROM ANOTHER THREAD, making
/// its in-flight seam call return.
///
/// `Send + Sync`, because the whole point is that it is held by someone other
/// than the cycle thread — a supervisor that decided to cancel while the cycle
/// thread is blocked inside the seam.
///
/// `cancel` is total and idempotent: cancelling twice, or cancelling a backend
/// that already finished, does nothing and reports nothing. There is no
/// "cancel failed" a caller could act on differently — a process that is
/// already gone is the outcome cancellation wanted.
pub trait BackendCanceller: Send + Sync {
    fn cancel(&self);
}

/// The default [`AgentBackend::canceller`]: a backend with no process behind
/// it has nothing to reap, and saying so honestly is better than pretending.
///
/// This is NOT a stand-in for an unimplemented canceller on a backend that
/// DOES own a process — a canceller that returns without reaping anything is
/// worse than no canceller, because a supervisor would then join a thread that
/// never returns.
struct NoopCanceller;

impl BackendCanceller for NoopCanceller {
    fn cancel(&self) {}
}

/// Makes one backend instance per cycle.
///
/// Concurrent cycles never share a backend: an [`AgentBackend`] is a step
/// function over ONE live thread, so two cycles sharing one would interleave
/// `start_turn`/`resume` on the same session and misroute each other's replies.
/// A factory is what lets a cycle table hold N cycles without ever holding N
/// references to one backend.
///
/// `&mut self` because a factory may legitimately carry state (a one-shot
/// factory that yields a pre-built instance once, a factory that counts).
pub trait AgentBackendFactory: Send {
    fn create(&mut self) -> Result<Box<dyn AgentBackend + Send>, AgentBackendError>;
}

/// A closure as a factory, so
/// `|| CodexAgentBackend::new().map(|b| Box::new(b) as _)` is usable without a
/// named type per call site.
pub struct ClosureBackendFactory<F>(F);

impl<F> ClosureBackendFactory<F>
where
    F: FnMut() -> Result<Box<dyn AgentBackend + Send>, AgentBackendError> + Send,
{
    pub fn new(f: F) -> Self {
        Self(f)
    }
}

impl<F> AgentBackendFactory for ClosureBackendFactory<F>
where
    F: FnMut() -> Result<Box<dyn AgentBackend + Send>, AgentBackendError> + Send,
{
    fn create(&mut self) -> Result<Box<dyn AgentBackend + Send>, AgentBackendError> {
        (self.0)()
    }
}

/// Run one turn to completion, refusing every tool call it makes.
///
/// A thread created with no declarations should make no calls at all; if one
/// arrives anyway it is refused rather than ignored, because an unanswered
/// call parks the child until its timeout.
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
