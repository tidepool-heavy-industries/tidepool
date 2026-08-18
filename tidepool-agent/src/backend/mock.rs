//! The scripted seam backend — the ONLY backend any committed test may drive.
//!
//! Standing rule: no live-model turns in tests or automated code.
//!
//! # This mock implements the SEAM, never the protocol
//!
//! It answers seam-trait calls from a fixed script and records what it was
//! asked. It knows nothing about JSON-RPC, frame ordering, session lifecycle,
//! `item/tool/call`, or any Codex error shape — and it must stay that way. A
//! test that can only pass by teaching this type protocol behavior is telling
//! you one of two things: the seam is in the wrong place and the behavior
//! belongs in the adapter where the real path exercises it, or the test wants a
//! RECORDED transcript replayed through the real adapter
//! ([`super::codex::replay`]) rather than an imitation.
//!
//! A script is arrange-step input — "given exactly these events, the loop does
//! X" — not a simulation of a model. Realistic backend behavior comes from
//! recordings, never from hand-written guesses about what a model would do.

use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use crate::backend::{AgentBackend, BackendCanceller};
use crate::seam::{
    AgentActivity, AgentBackendError, BackendThreadId, CycleOutcome, CycleResultPayload, CycleSpec,
    ThreadSpec, ToolCall, ToolCallId, ToolReply, TurnEvent, TurnId,
};

/// One scripted stop in a turn.
#[derive(Debug, Clone)]
pub enum MockStep {
    /// The turn parks, claiming the child called `tool` with `arguments`.
    Calls {
        tool: String,
        arguments: serde_json::Value,
    },
    /// The turn ends with this payload.
    Completes(CycleResultPayload),
    /// The step fails.
    Fails(AgentBackendError),
    /// Block until [`MockControl::release`], then continue to the NEXT step.
    /// A [`MockControl::cancel`] while blocked returns
    /// `RunFailed { detail: "cancelled" }`.
    ///
    /// SCHEDULING, not protocol — see [`MockControl`].
    Blocks,
}

/// A test-controlled gate: a scripted turn can BLOCK at a stop until the test
/// releases it, or until the backend is cancelled.
///
/// # This is seam-level SCHEDULING, not protocol behavior
///
/// The standing rule for this module (see the module docs) is that the mock
/// implements the SEAM and never the protocol: no JSON-RPC, no frame ordering,
/// no session lifecycle, no error shapes. This gate does not bend that rule,
/// because WHEN a seam call returns is a property of the seam itself — the
/// trait's own docs say an implementation must survive an arbitrary gap while
/// a call is parked, and a real backend's `start_turn` blocks for as long as
/// the model takes. `Blocks` lets a test choose that duration explicitly
/// instead of guessing at it.
///
/// It exists because concurrency tests must pin completion ORDER without
/// sleeping. A sleep-based ordering assertion is a race that usually passes;
/// this is a rendezvous that always does.
///
/// It teaches the mock nothing about what a model would do, and no assertion
/// about protocol behavior can be written with it.
pub struct MockControl {
    state: Mutex<ControlState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct ControlState {
    /// Releases granted but not yet consumed. Counted rather than a boolean so
    /// a test may release BEFORE the backend reaches its gate — a rendezvous
    /// that depended on which side arrived first would be the race this type
    /// exists to remove.
    releases: usize,
    cancelled: bool,
}

impl MockControl {
    fn new() -> Self {
        Self {
            state: Mutex::new(ControlState::default()),
            changed: Condvar::new(),
        }
    }

    /// Let one blocked (or one future) step through.
    pub fn release(&self) {
        let mut state = self.state.lock();
        state.releases += 1;
        self.changed.notify_all();
    }

    /// Fail every blocked and future step. Total and idempotent, like every
    /// [`BackendCanceller`].
    pub fn cancel(&self) {
        let mut state = self.state.lock();
        state.cancelled = true;
        self.changed.notify_all();
    }

    /// Whether [`cancel`](Self::cancel) has been called.
    pub fn is_cancelled(&self) -> bool {
        self.state.lock().cancelled
    }

    /// Block until released or cancelled. `Err` is the cancellation.
    fn wait(&self) -> Result<(), AgentBackendError> {
        let mut state = self.state.lock();
        loop {
            if state.cancelled {
                return Err(AgentBackendError::RunFailed {
                    detail: "cancelled".to_string(),
                });
            }
            if state.releases > 0 {
                state.releases -= 1;
                return Ok(());
            }
            self.changed.wait(&mut state);
        }
    }
}

impl BackendCanceller for Arc<MockControl> {
    fn cancel(&self) {
        MockControl::cancel(self);
    }
}

/// Where an injected failure fires, for the saga's rollback rows.
#[derive(Debug, Clone)]
pub enum MockFailure {
    /// `start_thread` fails — the saga must roll back a binding it just took
    /// (the `Bound → ThreadAccepted` edge).
    AtThreadStart(AgentBackendError),
    /// The first `start_turn` fails — the saga must roll back with the thread
    /// already accepted (the `ThreadAccepted → Running` edge).
    AtCycle(AgentBackendError),
}

/// A scripted backend.
///
/// The exact model a receipt records for a mock run is [`MockBackend::MODEL`] —
/// the "record the resolved model, not the tier" rule applies to mocks too, so
/// receipt assertions stay literal.
pub struct MockBackend {
    /// Remaining scripted stops, consumed front to back.
    script: std::collections::VecDeque<MockStep>,
    next_thread: u64,
    next_call: u64,
    turn: TurnId,
    thread: Option<BackendThreadId>,
    /// The call currently parked, if any.
    parked: Option<ToolCallId>,
    /// Fails `start_thread` instead of accepting it. Separate from the script
    /// because thread start is not a turn stop.
    thread_start_failure: Option<AgentBackendError>,
    /// Activity reported on the terminal outcome. Set explicitly; empty by
    /// default, because a scripted turn did nothing.
    activity: Vec<AgentActivity>,
    /// Every `ThreadSpec` this backend was asked to start, in order.
    pub started: Vec<ThreadSpec>,
    /// Every `(thread, CycleSpec)` this backend was asked to run, in order.
    pub cycles: Vec<(BackendThreadId, CycleSpec)>,
    /// Every reply the parent sent, in order — what a test asserts the parent's
    /// handlers actually answered.
    pub replies: Vec<ToolReply>,
    /// The gate [`MockStep::Blocks`] waits on, and the thing
    /// [`MockBackend::canceller`] hands out. Always present so a canceller can
    /// be taken from any backend, scripted with `Blocks` or not.
    control: Arc<MockControl>,
}

impl MockBackend {
    /// The mock's "exact resolved model" string.
    pub const MODEL: &'static str = "mock-model-0";

    /// A backend that runs `script` in order.
    pub fn scripted(script: impl IntoIterator<Item = MockStep>) -> Self {
        Self {
            script: script.into_iter().collect(),
            next_thread: 0,
            next_call: 0,
            turn: TurnId("mock-turn-0".to_string()),
            thread: None,
            parked: None,
            thread_start_failure: None,
            activity: Vec::new(),
            started: Vec::new(),
            cycles: Vec::new(),
            replies: Vec::new(),
            control: Arc::new(MockControl::new()),
        }
    }

    /// A backend whose one turn completes with `payload` and makes no calls.
    pub fn completing(payload: CycleResultPayload) -> Self {
        Self::scripted([MockStep::Completes(payload)])
    }

    /// A backend that fails per `failure`.
    pub fn failing(failure: MockFailure) -> Self {
        match failure {
            MockFailure::AtThreadStart(e) => {
                let mut b = Self::scripted([]);
                b.thread_start_failure = Some(e);
                b
            }
            MockFailure::AtCycle(e) => Self::scripted([MockStep::Fails(e)]),
        }
    }

    /// Report `activity` on the terminal outcome.
    #[must_use]
    pub fn with_activity(mut self, activity: Vec<AgentActivity>) -> Self {
        self.activity = activity;
        self
    }

    /// This backend's scheduling gate — how a test releases a
    /// [`MockStep::Blocks`] stop, or cancels a blocked one.
    pub fn control(&self) -> Arc<MockControl> {
        Arc::clone(&self.control)
    }

    /// Take the next scripted stop and project it into a seam event.
    ///
    /// A [`MockStep::Blocks`] stop is not a stop the SAGA sees: it blocks and
    /// then continues to the next scripted step, so the gate changes when a
    /// seam call returns and never what it returns.
    fn step(&mut self) -> Result<TurnEvent, AgentBackendError> {
        loop {
            let Some(step) = self.script.pop_front() else {
                return Err(AgentBackendError::RunFailed {
                    detail: "mock backend script exhausted: the turn was driven further than the \
                             test scripted it"
                        .to_string(),
                });
            };
            if matches!(step, MockStep::Blocks) {
                self.control.wait()?;
                continue;
            }
            return self.project(step);
        }
    }

    /// Project one non-blocking scripted stop into a seam event.
    fn project(&mut self, step: MockStep) -> Result<TurnEvent, AgentBackendError> {
        match step {
            MockStep::Blocks => unreachable!("`step` handles the gate before projecting"),
            MockStep::Fails(e) => Err(e),
            MockStep::Completes(payload) => {
                self.parked = None;
                Ok(TurnEvent::Completed(CycleOutcome {
                    turn: self.turn.clone(),
                    payload,
                    activity: std::mem::take(&mut self.activity),
                    resolved_model: Self::MODEL.to_string(),
                    usage: None,
                }))
            }
            MockStep::Calls { tool, arguments } => {
                let call = ToolCallId(format!("mock-call-{}", self.next_call));
                self.next_call += 1;
                self.parked = Some(call.clone());
                Ok(TurnEvent::ToolCall(ToolCall {
                    call,
                    thread: self
                        .thread
                        .clone()
                        .unwrap_or_else(|| BackendThreadId("mock-thread-unstarted".to_string())),
                    turn: self.turn.clone(),
                    tool,
                    arguments,
                }))
            }
        }
    }
}

impl AgentBackend for MockBackend {
    fn start_thread(&mut self, spec: &ThreadSpec) -> Result<BackendThreadId, AgentBackendError> {
        self.started.push(spec.clone());
        if let Some(e) = &self.thread_start_failure {
            return Err(e.clone());
        }
        let id = BackendThreadId(format!("mock-thread-{}", self.next_thread));
        self.next_thread += 1;
        self.thread = Some(id.clone());
        Ok(id)
    }

    fn start_turn(
        &mut self,
        thread: &BackendThreadId,
        spec: &CycleSpec,
    ) -> Result<TurnEvent, AgentBackendError> {
        self.cycles.push((thread.clone(), spec.clone()));
        self.turn = TurnId(format!("mock-turn-{}", self.cycles.len()));
        self.step()
    }

    fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, AgentBackendError> {
        // The one contract check the mock keeps: a reply must answer the call
        // that is actually parked. This is a SEAM rule (the trait states it),
        // not protocol knowledge — and a mock that silently accepted a
        // misrouted reply would let a correlation bug reach the live leg, where
        // it costs real tokens to discover.
        match &self.parked {
            Some(parked) if *parked == reply.call => {}
            Some(parked) => {
                return Err(AgentBackendError::ProtocolRejected {
                    detail: format!(
                        "reply names call {} but {} is the parked call",
                        reply.call.0, parked.0
                    ),
                })
            }
            None => {
                return Err(AgentBackendError::ProtocolRejected {
                    detail: format!("reply names call {} but no call is parked", reply.call.0),
                })
            }
        }
        self.replies.push(reply);
        self.parked = None;
        self.step()
    }

    /// The scheduling gate, as a canceller.
    ///
    /// Reaping a mock means failing whatever it is blocked on — there is no
    /// process — so this is the honest analogue of killing the app-server
    /// child, not a stand-in for it: a cycle thread parked on
    /// [`MockStep::Blocks`] really does return from its seam call when another
    /// thread calls this.
    fn canceller(&self) -> Box<dyn BackendCanceller> {
        Box::new(self.control())
    }
}
