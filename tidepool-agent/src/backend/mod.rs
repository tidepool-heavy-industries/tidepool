//! Backend adapters.
//!
//! One module per backend. A backend module owns its wire types, its process
//! lifecycle, and its correlation bookkeeping, and exposes only
//! [`crate::seam`] vocabulary.

use crate::seam::{AgentBackendError, BackendThreadId, CycleOutcome, CycleSpec, ThreadSpec};

pub mod codex;
pub mod mock;

/// The lane-1 backend seam: create one thread, run one cycle on it.
///
/// Deliberately this narrow — no steer, no interrupt, no event subscription,
/// no reattach. Those are lanes 2–5 surfaces, each gated on its own design;
/// widening this trait is a design act, not a convenience.
///
/// Sync by design: effect handlers are sync, and a backend that is internally
/// async (the codex adapter drives a tokio stdio transport) owns its own
/// runtime the way `LlmHandler` does, rather than making every caller async.
///
/// Failures are [`AgentBackendError`] — the seam's own projection
/// (retryable-vs-not, mine-vs-theirs), never a backend wire error.
pub trait OneCycleBackend {
    /// Create a thread per `spec`. Success means the backend ACCEPTED the
    /// thread (the saga's `ThreadAccepted` stage), not that any work ran.
    fn start_thread(&mut self, spec: &ThreadSpec) -> Result<BackendThreadId, AgentBackendError>;

    /// Run one turn to completion on `thread` and report what happened.
    /// Blocks for the whole cycle — lane 1's call is synchronous
    /// run-to-completion by design (the async handle surface is later lanes).
    fn run_cycle(
        &mut self,
        thread: &BackendThreadId,
        spec: &CycleSpec,
    ) -> Result<CycleOutcome, AgentBackendError>;
}
