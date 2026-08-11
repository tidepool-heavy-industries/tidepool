//! The scripted seam backend — the ONLY backend any committed test may drive.
//!
//! Standing rule (Inanna, 2026-08-09): no live-model turns in tests or
//! automated code.
//!
//! # This mock implements the SEAM, never the protocol (root/human, 2026-08-11)
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

use crate::backend::AgentBackend;
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

    /// Take the next scripted stop and project it into a seam event.
    fn step(&mut self) -> Result<TurnEvent, AgentBackendError> {
        let Some(step) = self.script.pop_front() else {
            return Err(AgentBackendError::RunFailed {
                detail: "mock backend script exhausted: the turn was driven further than the \
                         test scripted it"
                    .to_string(),
            });
        };
        match step {
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
}
