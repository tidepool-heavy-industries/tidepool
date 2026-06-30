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
use tidepool_mcp::CapturedOutput;

use crate::ask::{PauseGate, ResumeMsg, WorkerMessage};

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
    /// effect checkpoint (a pure infinite loop is uninterruptible). A follow-up
    /// op errors clearly; the reaper or `close` reclaims it.
    Wedged { since: Instant },
    /// Teardown in progress — every op is rejected.
    Closing,
}

/// Everything needed to resume (or reclaim) a parked `ask`. Folded into
/// [`SessionState::Suspended`]; this is exactly the payload the retired
/// `ReplContinuation` carried.
pub struct Suspension {
    pub cont_id: String,
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
