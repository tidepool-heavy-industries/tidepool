//! The per-session lifecycle state machine.
//!
//! Session lifecycle was previously smeared across three disjoint
//! representations — the `SessionManager` map (present ⇒ open), the server's
//! `continuations` map (present ⇒ parked on an `ask`), and the worker-local
//! `Option<SessionHandle<Open>>` — plus an implicit fourth: which channel the
//! worker thread is blocked on. Composite states like "Suspended ∧ Closing" had
//! no representation, so they went unhandled (deadlock on close-while-suspended,
//! leak on abandon, wedge on timeout, stale mutation on a concurrent run).
//!
//! This module makes the lifecycle a SINGLE owned value, transitioned
//! atomically by the server at the dispatch boundary. The suspension payload is
//! folded INTO [`SessionState::Suspended`] (it was a separate `continuations`
//! map) so a suspension cannot exist untracked by state — teardown is forced to
//! decide its fate.

use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tidepool_mcp::CapturedOutput;

use crate::ask::{PauseGate, ResumeMsg, WorkerMessage};

/// An in-turn `ask` continuation id (`scont_<n>`). A minted-once identity, not a
/// free-form string: it is compared and routed as this newtype rather than a
/// bare `String`. `#[serde(transparent)]` keeps the wire form a plain string, so
/// `continuation_id` request/response JSON is byte-identical.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct ContinuationId(pub String);

impl std::fmt::Display for ContinuationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Shared, lockable per-session state. One per session, owned by the
/// `SessionManager` entry; the server clones the `Arc` out and transitions under
/// the short `Mutex`.
///
/// INVARIANT (load-bearing): this `Mutex` is NEVER held across an `.await`.
/// Every transition is: lock → inspect/guard → move owned values out → unlock →
/// then `.await` (`drive`). `parking_lot::Mutex` is not async-aware, and holding
/// it across an await would risk deadlock + block the executor.
pub type SharedState = Arc<Mutex<SessionState>>;

/// Wrap an initial state in a fresh [`SharedState`].
pub fn shared(state: SessionState) -> SharedState {
    Arc::new(Mutex::new(state))
}

/// The lifecycle state of one resident session.
pub enum SessionState {
    /// Worker parked on its command channel; ready for a turn.
    Idle,
    /// A turn is executing on the worker thread.
    Busy,
    /// The turn hit an `ask`; the worker is parked on `response_rx`. The
    /// suspension payload (incl. the live `response_tx`) lives HERE, so teardown
    /// can't forget to release it.
    Suspended(Box<Suspension>),
    /// A turn timed out; `request_abort` was sent so it unwinds at its next
    /// effect checkpoint (a pure computation with no effect dispatch is
    /// uninterruptible via the gate; the JIT cancel handles that case). A
    /// follow-up op errors clearly; the reaper or `close` reclaims it.
    Wedged { since: Instant },
    /// Teardown in progress — every op is rejected.
    Closing,
}

/// Everything needed to resume (or reclaim) a parked `ask`. Folded into
/// [`SessionState::Suspended`]; this is exactly the payload the retired
/// `ReplContinuation` carried.
pub struct Suspension {
    pub cont_id: ContinuationId,
    pub response_tx: std::sync::mpsc::Sender<ResumeMsg>,
    pub session_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerMessage>,
    pub gate: Arc<PauseGate>,
    pub captured: CapturedOutput,
    /// The `ask`'s schema, used to validate + canonicalize the resume reply
    /// before it reaches the worker. `None` ⇒ accept any JSON.
    pub expected_schema: Option<serde_json::Value>,
    /// When the session entered (or last refreshed) this suspension — the
    /// reaper's TTL clock.
    pub since: Instant,
}

impl SessionState {
    pub fn is_idle(&self) -> bool {
        matches!(self, SessionState::Idle)
    }

    /// Short label for the "session busy" rejection message (M5 guard).
    pub fn busy_label(&self) -> String {
        match self {
            SessionState::Idle => "idle".into(),
            SessionState::Busy => "busy with a running turn".into(),
            SessionState::Suspended(s) => format!("suspended (continuation {})", s.cont_id),
            SessionState::Wedged { .. } => "wedged (a turn timed out)".into(),
            SessionState::Closing => "closing".into(),
        }
    }
}

/// If `state` is `Suspended`, transition it to `Busy` and return the owned
/// [`Suspension`]; otherwise leave it untouched and return `None`.
///
/// The resume/abort paths call this AFTER confirming `Suspended` under the same
/// lock, so the `None` case is unreachable there — but returning an `Option`
/// (rather than a re-`match` + `unreachable!`) makes "already checked" a type
/// guarantee at the call site instead of a panic waiting to fire.
pub fn take_suspension(state: &mut SessionState) -> Option<Box<Suspension>> {
    match std::mem::replace(state, SessionState::Busy) {
        SessionState::Suspended(s) => Some(s),
        other => {
            // Not suspended — restore what we displaced and report the miss.
            *state = other;
            None
        }
    }
}
