//! The per-session lifecycle state machine.
//!
//! The session lifecycle is a SINGLE owned value ([`SessionState`]),
//! transitioned atomically by the server at the dispatch boundary. The
//! suspension payload lives INSIDE [`SessionState::Suspended`] rather than a
//! side map, so a suspension cannot exist untracked by state — teardown is
//! always forced to decide its fate. This is what rules out composite states
//! like "Suspended ∧ Closing" going unhandled (deadlock on
//! close-while-suspended, leak on abandon, wedge on timeout, stale mutation
//! on a concurrent run).

use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tidepool_mcp::CapturedOutput;

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
    /// No turn in flight; ready for a new one.
    Idle,
    /// A turn is executing (the session is checked out of its manager slot and
    /// owned by the turn's blocking task).
    Busy,
    /// The turn hit an `ask` and stowed its continuation as data. Nothing is
    /// blocked — the session is sitting back in its manager slot holding the
    /// stowed continuation. The suspension's caller-facing payload lives HERE
    /// so a suspension can't exist untracked by state.
    Suspended(Box<Suspension>),
    /// A turn timed out; `request_abort` was sent so it unwinds at its next
    /// effect checkpoint (a pure computation with no effect dispatch is
    /// uninterruptible via the gate; the JIT cancel handles that case). A
    /// follow-up op errors clearly; the reaper or `session_reset` reclaims it.
    Wedged { since: Instant },
    /// Teardown in progress — every op is rejected.
    Closing,
}

/// Everything the SERVER needs to answer (or reclaim) a suspended `ask`. Lives
/// inside [`SessionState::Suspended`] so a suspension can't exist untracked by
/// state.
///
/// The continuation itself is NOT here: it is stowed as data on the session's
/// JIT machine, which sits in the manager's `Suspended { session, cont_id }`
/// slot. This struct carries only what the request/response path needs — the
/// identity to route a resume by, the schema to validate it against, the output
/// buffer the resumed turn keeps appending to, and the reaper's clock.
pub struct Suspension {
    /// The minted id a `session_resume` must name (validate-before-consume:
    /// a mismatch never touches the pending continuation).
    pub cont_id: ContinuationId,
    /// The console output captured so far, carried across the suspension so the
    /// resumed turn's drain includes everything the pre-ask items printed.
    pub captured: CapturedOutput,
    /// The `ask`'s schema, used to validate + canonicalize the resume reply
    /// before the continuation is consumed. `None` ⇒ accept any JSON.
    pub expected_schema: Option<serde_json::Value>,
    /// When the session entered (or last refreshed) this suspension — the
    /// reaper's TTL clock.
    pub since: Instant,
}

impl SessionState {
    pub fn is_idle(&self) -> bool {
        matches!(self, SessionState::Idle)
    }

    /// Short label used in the "session busy" rejection message.
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
