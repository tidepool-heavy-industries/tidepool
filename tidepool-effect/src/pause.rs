//! The shared [`PauseGate`]: the timeout-as-yield-point latch used by both
//! ask/suspend dispatchers.
//!
//! An eval only computes during an MCP call. When the caller's window expires,
//! the server requests a pause (or, for a hard timeout, an abort) and the eval
//! thread parks itself at its NEXT effect dispatch — we own every dispatch, so
//! every effect is a yield point. Between MCP calls: no compute, no LLM spend,
//! nothing unobserved. Pure JIT stretches can't be interrupted — a thread that
//! reaches no effect within a grace period is treated as a runaway and detached
//! (the old timeout behavior, reserved for exactly that case).
//!
//! Two dispatchers share this ONE gate:
//!
//! - `tidepool-mcp`'s per-eval `AskDispatcher` drives the full state machine —
//!   pause → park → resume/abort — plus the compile-phase flag (`set_compiling`)
//!   so a slow cold-cache GHC compile isn't misdiagnosed as a runaway, and
//!   `parked_or_in_effect` to distinguish "will park at the next boundary" from
//!   "pure-compute runaway" at the grace deadline.
//! - `tidepool-repl`'s resident-worker `ReplAskDispatcher` uses the abort latch
//!   only: on a turn timeout it requests an abort and reads `is_in_effect` at the
//!   grace deadline (a long external Exec/Http call, not a pure loop). It never
//!   requests a pause — the pause states and grace machinery are simply unused
//!   there, but the gate is one type.
//!
//! Only the gate is shared; the DISPATCHERS and worker/thread-parking mechanics
//! stay crate-local (see each `ask.rs`).
//!
//! ## The two cancellation channels (deliberate layering, one known race)
//!
//! The gate is the COOPERATIVE channel: it can park-and-resume, and its abort
//! is observed at effect-dispatch checkpoints, surfacing as
//! `EffectError::Handler(reason)`. The JIT `cancel_flag` (an `Arc<AtomicBool>`
//! polled at the trampoline / GC / join-back-edge / dispatch safepoints) is
//! the FORCED-UNWIND backstop for pure compute that never reaches a
//! checkpoint, surfacing as `YieldError::Cancelled`. The server timeout paths
//! set BOTH (a gate-only abort leaves a pure loop spinning; a flag-only cancel
//! cannot wake a thread parked in an ask). Consequence: the SAME timeout races
//! between the two surface shapes — whichever safepoint fires first wins.
//! Both terminate the turn, so callers must treat either shape as
//! cancellation; do not string-match one of them.

use std::sync::Arc;

/// The pause gate: timeout-as-yield-point latch shared by the ask dispatchers.
///
/// See the module docs for the full contract. Backed by a `parking_lot` mutex +
/// condvar so the server side can wait for the eval thread to park.
pub struct PauseGate {
    inner: parking_lot::Mutex<GateInner>,
    cv: parking_lot::Condvar,
}

struct GateInner {
    state: GateState,
    /// True while the thread is inside an effect handler (incl. blocked on an
    /// ask): between [`PauseGate::checkpoint`] returning `Ok` and
    /// [`PauseGate::exit_effect`]. Read at the grace deadline to distinguish
    /// "blocked waiting on an external Exec/Http/LLM call (will park at the next
    /// boundary)" from "pure JIT compute runaway". (#324)
    in_effect: bool,
    /// True from eval-thread start until the JIT machine is created (the
    /// cancel-handle installer callback fires at exactly that boundary). A
    /// timeout during this phase is a slow GHC COMPILE, not a pure runaway — the
    /// message must not blame user code for a cold cache. (#324)
    compiling: bool,
}

/// Gate state machine. The repl dispatcher only ever moves between `Run` and
/// `AbortRequested`; the mcp dispatcher exercises the pause states too.
#[derive(Clone, PartialEq)]
pub enum GateState {
    Run,
    PauseRequested,
    Paused,
    AbortRequested(String),
}

impl PauseGate {
    pub fn new() -> Arc<Self> {
        Arc::new(PauseGate {
            inner: parking_lot::Mutex::new(GateInner {
                state: GateState::Run,
                in_effect: false,
                compiling: false,
            }),
            cv: parking_lot::Condvar::new(),
        })
    }

    /// Eval/worker side, at every effect dispatch entry. Parks while paused;
    /// returns `Err(reason)` on abort (the turn then unwinds). On `Ok`, marks
    /// `in_effect = true` — the caller MUST pair with [`Self::exit_effect`].
    pub fn checkpoint(&self) -> Result<(), String> {
        let mut g = self.inner.lock();
        loop {
            match &g.state {
                GateState::Run => {
                    g.in_effect = true;
                    return Ok(());
                }
                GateState::AbortRequested(r) => {
                    let r = r.clone();
                    g.state = GateState::Run;
                    return Err(r);
                }
                GateState::PauseRequested => {
                    g.state = GateState::Paused;
                    self.cv.notify_all(); // tell the server side we parked
                }
                GateState::Paused => {
                    self.cv.wait(&mut g);
                }
            }
        }
    }

    /// Eval/worker side, on effect handler return (success or error): clear
    /// `in_effect`. Must be called after every successful [`Self::checkpoint`].
    pub fn exit_effect(&self) {
        self.inner.lock().in_effect = false;
    }

    /// Mark the compile phase (eval-thread start → JIT machine creation).
    pub fn set_compiling(&self, on: bool) {
        self.inner.lock().compiling = on;
    }

    /// Whether the eval thread was still in the compile phase (never reached
    /// execution) — consulted by the timeout path to avoid misdiagnosing a slow
    /// cold-cache compile as a pure infinite loop.
    pub fn is_compiling(&self) -> bool {
        self.inner.lock().compiling
    }

    /// Server side: whether the thread is currently blocked inside an effect
    /// handler. When true, the timeout is due to a slow external call
    /// (Exec/Http/LLM/…), NOT a pure infinite loop. (The simple grace-less read
    /// the repl worker uses; the mcp side uses [`Self::parked_or_in_effect`].)
    pub fn is_in_effect(&self) -> bool {
        self.inner.lock().in_effect
    }

    /// Server side: request the thread pause at its next effect boundary.
    pub fn request_pause(&self) {
        let mut g = self.inner.lock();
        if g.state == GateState::Run {
            g.state = GateState::PauseRequested;
        }
    }

    /// Server side: wake a paused (or pause-pending) thread back into `Run`.
    pub fn resume_run(&self) {
        let mut g = self.inner.lock();
        g.state = GateState::Run;
        self.cv.notify_all();
    }

    /// Server side: wake the thread with an abort. Its current/next checkpoint
    /// returns `Err` and the eval terminates as a normal error.
    pub fn request_abort(&self, reason: String) {
        let mut g = self.inner.lock();
        g.state = GateState::AbortRequested(reason);
        self.cv.notify_all();
    }

    /// Server side, after [`Self::request_pause`]: wait up to `grace` for the
    /// thread to park. Returns `true` if it parked OR is inside an effect (it
    /// will park at the next boundary — long LLM/IO calls must not be mistaken
    /// for runaways); `false` = pure-compute runaway.
    pub fn parked_or_in_effect(&self, grace: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + grace;
        let mut g = self.inner.lock();
        loop {
            if g.state == GateState::Paused {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return g.in_effect;
            }
            self.cv.wait_for(&mut g, deadline - now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gate state machine: pause parks a checkpointing thread, resume
    /// releases it, abort errors it out; in_effect threads are not runaways.
    #[test]
    fn pause_gate_park_resume_abort() {
        // pause → thread parks at checkpoint → resume releases it
        let gate = PauseGate::new();
        gate.request_pause();
        let g2 = Arc::clone(&gate);
        let t = std::thread::spawn(move || g2.checkpoint());
        assert!(gate.parked_or_in_effect(Duration::from_secs(2)));
        gate.resume_run();
        assert!(t.join().unwrap().is_ok());
        gate.exit_effect();

        // pause → park → abort errors the checkpoint
        gate.request_pause();
        let g3 = Arc::clone(&gate);
        let t = std::thread::spawn(move || g3.checkpoint());
        assert!(gate.parked_or_in_effect(Duration::from_secs(2)));
        gate.request_abort("killed".into());
        let err = t.join().unwrap().unwrap_err();
        assert!(err.contains("killed"));

        // a running gate with no checkpointing thread = runaway
        let lone = PauseGate::new();
        lone.request_pause();
        assert!(!lone.parked_or_in_effect(Duration::from_millis(50)));

        // ...unless the thread is inside an effect (e.g. a long LLM
        // call): it will park at the NEXT boundary — not a runaway.
        let busy = PauseGate::new();
        busy.checkpoint().unwrap(); // enter effect (in_effect = true)
        busy.request_pause();
        assert!(busy.parked_or_in_effect(Duration::from_millis(50)));
    }

    /// `is_in_effect()` is false initially, true after a successful
    /// `checkpoint()`, and false again after `exit_effect()`. (repl #324)
    #[test]
    fn pause_gate_in_effect_flag() {
        let gate = PauseGate::new();
        assert!(!gate.is_in_effect(), "fresh gate: not in effect");

        gate.checkpoint().expect("first checkpoint ok");
        assert!(gate.is_in_effect(), "after checkpoint: in effect");

        gate.exit_effect();
        assert!(!gate.is_in_effect(), "after exit_effect: not in effect");
    }

    /// When an abort was requested, `checkpoint()` returns `Err` and does NOT
    /// set `in_effect` — there is no effect to exit. (repl #324)
    #[test]
    fn pause_gate_abort_does_not_set_in_effect() {
        let gate = PauseGate::new();
        gate.request_abort("timed out".into());
        let result = gate.checkpoint();
        assert!(result.is_err(), "aborted checkpoint returns Err");
        assert!(
            !gate.is_in_effect(),
            "aborted checkpoint must not set in_effect"
        );
    }

    /// After `request_abort` is consumed by `checkpoint()`, the gate resets to
    /// `Run` and a subsequent `checkpoint()` succeeds and sets `in_effect`.
    /// (repl #324)
    #[test]
    fn pause_gate_abort_consumed_then_next_checkpoint_ok() {
        let gate = PauseGate::new();
        gate.request_abort("first abort".into());
        let _ = gate.checkpoint(); // consumes the abort
                                   // Gate is now Run again — next checkpoint should succeed.
        gate.checkpoint()
            .expect("second checkpoint ok after abort consumed");
        assert!(gate.is_in_effect());
        gate.exit_effect();
    }

    /// The compile-phase flag round-trips (#324): a timeout during compile must
    /// be distinguishable from a pure-compute runaway.
    #[test]
    fn pause_gate_compiling_flag() {
        let gate = PauseGate::new();
        assert!(!gate.is_compiling(), "fresh gate: not compiling");
        gate.set_compiling(true);
        assert!(gate.is_compiling(), "after set_compiling(true)");
        gate.set_compiling(false);
        assert!(!gate.is_compiling(), "after set_compiling(false)");
    }
}
