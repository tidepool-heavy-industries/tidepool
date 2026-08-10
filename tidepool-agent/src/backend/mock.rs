//! The replay/mock backend — the ONLY backend any committed test may drive.
//!
//! Standing rule (Inanna, 2026-08-09): no live-model turns in tests or
//! automated code. This mock is deliberately boring: it answers from a script
//! and records what it was asked, so a saga test can inject a failure at an
//! exact stage and then assert the rollback from disk state. It never spawns
//! a process, never touches `~/.codex`, never spends a token.

use crate::backend::OneCycleBackend;
use crate::seam::{
    AgentBackendError, BackendThreadId, CycleOutcome, CycleResultPayload, CycleSpec, ThreadSpec,
    TurnId,
};

/// Where an injected failure fires.
#[derive(Debug, Clone)]
pub enum MockFailure {
    /// `start_thread` fails — the saga must roll back a binding it just took
    /// (the `Bound → ThreadAccepted` edge).
    AtThreadStart(AgentBackendError),
    /// `run_cycle` fails — the saga must roll back with the thread already
    /// accepted (the `ThreadAccepted → Running` edge).
    AtCycle(AgentBackendError),
}

/// A scripted one-cycle backend.
///
/// The exact model a receipt records for a mock run is
/// [`MockBackend::MODEL`] — the "record the resolved model, not the tier"
/// rule applies to mocks too, so receipt assertions stay literal.
pub struct MockBackend {
    payload: CycleResultPayload,
    failure: Option<MockFailure>,
    next_thread: u64,
    /// Every `ThreadSpec` this backend was asked to start, in order.
    pub started: Vec<ThreadSpec>,
    /// Every `(thread, CycleSpec)` this backend was asked to run, in order.
    pub cycles: Vec<(BackendThreadId, CycleSpec)>,
}

impl MockBackend {
    /// The mock's "exact resolved model" string.
    pub const MODEL: &'static str = "mock-model-0";

    /// A backend whose one cycle completes with `payload`.
    pub fn completing(payload: CycleResultPayload) -> Self {
        Self {
            payload,
            failure: None,
            next_thread: 0,
            started: Vec::new(),
            cycles: Vec::new(),
        }
    }

    /// A backend that fails per `failure` (and completes with `Absent` on any
    /// path the failure does not cover).
    pub fn failing(failure: MockFailure) -> Self {
        Self {
            payload: CycleResultPayload::Absent,
            failure: Some(failure),
            next_thread: 0,
            started: Vec::new(),
            cycles: Vec::new(),
        }
    }
}

impl OneCycleBackend for MockBackend {
    fn start_thread(&mut self, spec: &ThreadSpec) -> Result<BackendThreadId, AgentBackendError> {
        self.started.push(spec.clone());
        if let Some(MockFailure::AtThreadStart(e)) = &self.failure {
            return Err(e.clone());
        }
        let id = BackendThreadId(format!("mock-thread-{}", self.next_thread));
        self.next_thread += 1;
        Ok(id)
    }

    fn run_cycle(
        &mut self,
        thread: &BackendThreadId,
        spec: &CycleSpec,
    ) -> Result<CycleOutcome, AgentBackendError> {
        self.cycles.push((thread.clone(), spec.clone()));
        if let Some(MockFailure::AtCycle(e)) = &self.failure {
            return Err(e.clone());
        }
        Ok(CycleOutcome {
            turn: TurnId(format!("mock-turn-{}", self.cycles.len())),
            payload: self.payload.clone(),
            activity: Vec::new(),
            resolved_model: Self::MODEL.to_string(),
        })
    }
}
