//! Session registry — the resident-machine lifecycle guardian.
//!
//! A `HashMap<SessionId, Slot<M>>` where `Slot` (defined in [`crate::tree`]) is
//! `Idle(M) | Running { holes } | Suspended { machine, holes }`. Every machine
//! access goes through this map — the stowed-XOR-running discipline that
//! justifies `unsafe impl Send for JitEffectMachine` maps directly onto the
//! slot variants: a machine is in EXACTLY one slot, and `Running` means it is
//! out on a turn (no side-channel access while running).
//!
//! MULTI-HOLE (one-session plan, Phase 2): a suspended session carries a SET
//! of parked holes, each resumable by identity in ANY order (the machine's
//! continuation registry imposes none — locked decision 2), and a NEW
//! top-level run over parked frames is an ordinary checkout, not a refusal.
//! The SESSION (`M`) is the ground truth for its own hole set; the slot
//! mirrors it at restore time — a caller restores with the hole set the
//! session actually reports (`read the machine, don't guess`), never a guess
//! from the turn's domain result.
//!
//! Generic over the machine handle `M` so this crate stays free of the JIT
//! dependency — `tidepool-runtime::session::ResidentSession` is the `M` the
//! harness instantiates.
//!
//! # Lifecycle transitions are atomic at the dispatch boundary
//!
//! The transition — inspect the slot, decide, move the owned machine out
//! — happens under one short lock; the TURN itself (compile + run, which blocks)
//! runs with the lock RELEASED (the machine owned on the caller's stack), then a
//! second short lock restores the machine as `Idle` or `Suspended`. The
//! `parking_lot::Mutex` is NEVER held across the turn.
//!
//! A [`Checkout`] is the RAII proof that a machine is out on a turn: it owns the
//! machine and, on restore, moves it back under the lock. Dropping a `Checkout`
//! without restoring is a bug (the session is left `Running` forever) — the
//! `#[must_use]` and the panic-safety `Drop` (which restores the CARRIED hole
//! set, not a bare `Idle`) make the exit explicit. A machine that is
//! genuinely gone (e.g. a `JoinError` off the blocking pool) is retired via
//! [`Self::remove`] directly, never through a `Checkout`.

use std::collections::HashMap;

use parking_lot::Mutex;
use tidepool_repr::SessionId;

use crate::tree::{HoleId, Slot};

/// Why a machine checkout was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckoutError {
    /// No session registered under this id.
    #[error("no session {0}")]
    Unknown(SessionId),
    /// A turn is already executing on this session (its machine is out —
    /// `Slot::Running`). Turns on one session are strictly sequential,
    /// whatever kind of turn it is.
    #[error("session {0} is already running a turn")]
    Running(SessionId),
    /// A child-run checkout was attempted on a session with no parked hole
    /// (a child reads a suspended parent's world by construction).
    #[error("session {0} has no parked hole; a child run requires a suspended parent")]
    NotSuspended(SessionId),
    /// A resume/abort referenced a hole that is not among this session's
    /// parked holes. Nothing is consumed — the caller can retry with a
    /// member hole (validate-before-consume, at the registry layer).
    #[error("session {session}: no parked hole {attempted:?}{}", if .parked.is_empty() {
        " (session has no parked holes)".to_string()
    } else {
        format!(" (parked: {:?})", .parked)
    })]
    WrongHole {
        session: SessionId,
        attempted: HoleId,
        parked: Vec<HoleId>,
    },
}

/// The resident-session registry: owns every session's [`Slot`] and gates all
/// machine access through atomic checkout/restore transitions.
pub struct SessionRegistry<M> {
    slots: Mutex<HashMap<SessionId, Slot<M>>>,
}

impl<M> Default for SessionRegistry<M> {
    fn default() -> Self {
        SessionRegistry {
            slots: Mutex::new(HashMap::new()),
        }
    }
}

impl<M> SessionRegistry<M> {
    /// A fresh, empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a freshly-bootstrapped session as `Idle`. Returns the previous
    /// slot if the id was already present (the caller decides whether that is a
    /// reset or a collision).
    pub fn insert_idle(&self, id: SessionId, machine: M) -> Option<Slot<M>> {
        self.slots.lock().insert(id, Slot::Idle(machine))
    }

    /// Remove a session entirely, returning its slot (drops the machine when the
    /// returned slot is dropped). The get-unstuck / teardown path.
    pub fn remove(&self, id: SessionId) -> Option<Slot<M>> {
        self.slots.lock().remove(&id)
    }

    /// Read-only access to the machine WITHOUT checking it out — only
    /// succeeds when the machine is actually present in its slot (`Idle` or
    /// `Suspended`; a `Running` machine is out on a turn, so there is nothing
    /// here to borrow). Used for cheap metadata reads (decl-plane context,
    /// heap stats) that must not disturb the checkout discipline or race a
    /// real checkout — the lock is held only for the duration of `f`.
    pub fn peek<R>(&self, id: SessionId, f: impl FnOnce(&M) -> R) -> Option<R> {
        match self.slots.lock().get(&id) {
            Some(Slot::Idle(m)) => Some(f(m)),
            Some(Slot::Suspended { machine, .. }) => Some(f(machine)),
            _ => None,
        }
    }

    /// Check a machine OUT for a new turn: `Idle | Suspended → Running`,
    /// moving the machine onto the returned [`Checkout`]. A new turn over
    /// PARKED frames is ordinary (the machine's continuation registry keeps
    /// every parked frame rooted while unrelated fragments run) — the old
    /// reject-while-suspended refusal is gone with the slot path. Refuses
    /// only a session whose machine is already out.
    pub fn checkout_run(&self, id: SessionId) -> Result<Checkout<'_, M>, CheckoutError> {
        let mut slots = self.slots.lock();
        match slots.get_mut(&id) {
            None => Err(CheckoutError::Unknown(id)),
            Some(Slot::Running { .. }) => Err(CheckoutError::Running(id)),
            Some(slot) => {
                let holes = slot_holes(slot);
                let machine = take_machine(slot, holes.clone());
                Ok(Checkout {
                    registry: self,
                    id,
                    machine: Some(machine),
                    holes,
                })
            }
        }
    }

    /// Check a machine OUT to resume/abort one of its parked holes:
    /// `Suspended → Running`, validating `hole` is a MEMBER of the parked
    /// set (any order — the machine resumes by identity). A mismatch leaves
    /// the slot untouched ([`CheckoutError::WrongHole`]) —
    /// validate-before-consume. The lock is released `Running`; the caller
    /// drives the resume, then restores with the session's OWN post-turn
    /// hole set.
    pub fn checkout_resume(
        &self,
        id: SessionId,
        hole: &HoleId,
    ) -> Result<Checkout<'_, M>, CheckoutError> {
        let mut slots = self.slots.lock();
        match slots.get_mut(&id) {
            None => Err(CheckoutError::Unknown(id)),
            Some(Slot::Running { .. }) => Err(CheckoutError::Running(id)),
            Some(Slot::Idle(_)) => Err(CheckoutError::WrongHole {
                session: id,
                attempted: hole.clone(),
                parked: Vec::new(),
            }),
            Some(Slot::Suspended { holes, .. }) if !holes.contains(hole) => {
                Err(CheckoutError::WrongHole {
                    session: id,
                    attempted: hole.clone(),
                    parked: holes.clone(),
                })
            }
            Some(slot) => {
                // Matched Suspended with a member hole.
                let holes = slot_holes(slot);
                let machine = take_machine(slot, holes.clone());
                Ok(Checkout {
                    registry: self,
                    id,
                    machine: Some(machine),
                    holes,
                })
            }
        }
    }

    /// Check a machine OUT for a CHILD run over its parked frames:
    /// `Suspended → Running`, requiring at least one parked hole (a child
    /// reads a suspended parent's world by construction — the semantic
    /// distinction [`CheckoutError::NotSuspended`] preserves). Otherwise
    /// identical to [`Self::checkout_run`]: with the continuation registry
    /// there is no special child window, and the parked holes ride the
    /// checkout like any other turn's.
    pub fn checkout_child(&self, id: SessionId) -> Result<Checkout<'_, M>, CheckoutError> {
        let mut slots = self.slots.lock();
        match slots.get_mut(&id) {
            None => Err(CheckoutError::Unknown(id)),
            Some(Slot::Running { .. }) => Err(CheckoutError::Running(id)),
            Some(Slot::Idle(_)) => Err(CheckoutError::NotSuspended(id)),
            Some(slot) => {
                let holes = slot_holes(slot);
                let machine = take_machine(slot, holes.clone());
                Ok(Checkout {
                    registry: self,
                    id,
                    machine: Some(machine),
                    holes,
                })
            }
        }
    }

    /// Restore a checked-out machine with its post-turn parked hole set under
    /// the lock. An empty set restores `Idle` (the two restores are one
    /// operation with two spellings, so a caller passing the session's own
    /// reported holes cannot desync the slot from the machine).
    fn restore_suspended(&self, id: SessionId, machine: M, holes: Vec<HoleId>) {
        let slot = if holes.is_empty() {
            Slot::Idle(machine)
        } else {
            Slot::Suspended { machine, holes }
        };
        self.slots.lock().insert(id, slot);
    }
}

/// The parked holes a present-machine slot carries (`Idle` → none).
fn slot_holes<M>(slot: &Slot<M>) -> Vec<HoleId> {
    match slot {
        Slot::Idle(_) => Vec::new(),
        Slot::Suspended { holes, .. } => holes.clone(),
        Slot::Running { .. } => unreachable!("caller matched a present-machine slot"),
    }
}

/// Move the machine out of a present-machine slot, leaving `Running{holes}`.
fn take_machine<M>(slot: &mut Slot<M>, holes: Vec<HoleId>) -> M {
    match std::mem::replace(slot, Slot::Running { holes }) {
        Slot::Idle(machine) => machine,
        Slot::Suspended { machine, .. } => machine,
        Slot::Running { .. } => unreachable!("caller matched a present-machine slot"),
    }
}

/// RAII proof that a session's machine is OUT on a turn (`Slot::Running`). Owns
/// the machine for the turn; restore it exactly once via
/// [`Self::restore_suspended`].
///
/// Dropping a `Checkout` without restoring leaves the slot `Running` (the
/// session is wedged) — the panic-safety `Drop` below is the only exit that
/// does not require an explicit restore call.
#[must_use = "a checked-out machine must be restored, or the session is left Running forever"]
pub struct Checkout<'r, M> {
    registry: &'r SessionRegistry<M>,
    id: SessionId,
    machine: Option<M>,
    /// The parked holes carried OUT with the machine — what the panic-safety
    /// `Drop` restores (an unwound turn must not lose the session's parked
    /// frames; they are still rooted in the machine's continuation registry).
    holes: Vec<HoleId>,
}

// Whole point of this type: a checked-out machine is settled by exactly one
// call to `restore_suspended`, which consumes `self` by
// value. A future `#[derive(Clone)]` would let a caller settle the SAME
// checkout twice (or settle a clone while the panic-safety `Drop` still
// thinks the original is unsettled), silently reviving the double-settle bug
// this type exists to make a compile error. Pinned at `M = ()` — the struct
// has no `Clone`/`Copy` impl for any `M`, so a fixed stand-in is enough to
// catch a derive that would apply uniformly.
static_assertions::assert_not_impl_any!(Checkout<'static, ()>: Clone, Copy);

impl<M> Checkout<'_, M> {
    /// The session id this checkout is for.
    pub fn session_id(&self) -> SessionId {
        self.id
    }

    /// Borrow the checked-out machine for the turn.
    pub fn machine(&mut self) -> &mut M {
        #[allow(clippy::expect_used, reason = "machine present until restore")]
        self.machine
            .as_mut()
            .expect("machine present until restore")
    }

    /// Take ownership of the machine off the checkout (e.g. to move it onto an
    /// eval thread). The caller MUST return it via [`Self::restore_suspended`].
    pub fn take(&mut self) -> M {
        #[allow(clippy::expect_used, reason = "machine present until restore")]
        self.machine.take().expect("machine present until restore")
    }

    /// Put the machine back via [`Self::restore_suspended`] after a `take`.
    pub fn put(&mut self, machine: M) {
        self.machine = Some(machine);
    }

    /// Restore the machine with its post-turn parked hole set — pass the
    /// session's OWN reported holes (`parked_holes()`), never a guess from
    /// the turn's domain result. An empty set restores `Idle`.
    pub fn restore_suspended(mut self, holes: Vec<HoleId>) {
        #[allow(clippy::expect_used, reason = "machine present until restore")]
        let machine = self.machine.take().expect("machine present until restore");
        self.registry.restore_suspended(self.id, machine, holes);
    }
}

/// Panic safety net: if a `Checkout` is dropped while it still owns the
/// machine (an explicit `restore_suspended` never ran — e.g. a panic unwound
/// through the turn between checkout and restore), restore it with the hole
/// set it CARRIED OUT rather than leaving the slot `Running` forever — or
/// silently dropping parked frames to a bare `Idle` (they are still rooted in
/// the machine; the slot must keep saying
/// so). `restore_suspended` `take()`s the machine first, so `Drop` sees
/// `None` and does nothing on that explicit exit path.
///
/// This does NOT cover [`Checkout::take`]: once the machine has been moved
/// off the checkout (e.g. onto a blocking thread), `Drop` has nothing to
/// restore — a caller that loses the machine that way (a `JoinError`) must
/// retire the session explicitly instead (the harness's `terminate_node`).
impl<M> Drop for Checkout<'_, M> {
    fn drop(&mut self) {
        if let Some(machine) = self.machine.take() {
            self.registry
                .restore_suspended(self.id, machine, std::mem::take(&mut self.holes));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial stand-in for the machine handle `M` — the registry is pure
    /// lifecycle bookkeeping, so a counter is enough to exercise the
    /// transitions without the JIT.
    #[derive(Debug, PartialEq, Eq)]
    struct FakeMachine {
        turns: u32,
    }

    fn hole(s: &str) -> HoleId {
        HoleId(s.to_string())
    }

    /// Extract the error from a checkout `Result` without requiring
    /// `Checkout: Debug` (which `Result::unwrap_err` would).
    fn err<M>(r: Result<Checkout<'_, M>, CheckoutError>) -> CheckoutError {
        match r {
            Ok(_) => panic!("expected a checkout error, got an Ok(Checkout)"),
            Err(e) => e,
        }
    }

    // Test-only slot inspection: no production caller needs `is_idle`/
    // `pending_hole`/`pending_holes` (production always restores through
    // `restore_suspended` and reads holes off the SESSION, never the slot),
    // so these read the private `slots` field directly rather than carrying
    // dead API on `SessionRegistry`.
    fn is_idle<M>(reg: &SessionRegistry<M>, id: SessionId) -> bool {
        matches!(reg.slots.lock().get(&id), Some(Slot::Idle(_)))
    }

    fn pending_hole<M>(reg: &SessionRegistry<M>, id: SessionId) -> Option<HoleId> {
        pending_holes(reg, id).last().cloned()
    }

    fn pending_holes<M>(reg: &SessionRegistry<M>, id: SessionId) -> Vec<HoleId> {
        match reg.slots.lock().get(&id) {
            Some(Slot::Suspended { holes, .. }) | Some(Slot::Running { holes }) => holes.clone(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn idle_run_completes_back_to_idle() {
        let reg = SessionRegistry::new();
        let id = SessionId(1);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        assert!(is_idle(&reg, id));

        let mut co = reg.checkout_run(id).expect("idle → run");
        // While running, the slot rejects a second turn and reads not-idle.
        assert!(!is_idle(&reg, id));
        assert_eq!(err(reg.checkout_run(id)), CheckoutError::Running(id));
        co.machine().turns += 1;
        co.restore_suspended(Vec::new());

        assert!(is_idle(&reg, id));
        // The machine (and its accumulated turn count) survived the round-trip.
        let mut co = reg.checkout_run(id).expect("idle again");
        assert_eq!(co.machine().turns, 1);
        co.restore_suspended(Vec::new());
    }

    /// MULTI-HOLE: a session parks two holes across two runs; both are
    /// visible; either resumes FIRST (any-order); a new run over the parked
    /// frames is ordinary; each resume retires only its own hole.
    #[test]
    fn multi_hole_any_order_resume_and_run_over_parked() {
        let reg = SessionRegistry::new();
        let id = SessionId(2);
        reg.insert_idle(id, FakeMachine { turns: 0 });

        // Run → park hole 1.
        let co = reg.checkout_run(id).expect("idle → run");
        co.restore_suspended(vec![hole("scont_1")]);
        assert_eq!(pending_hole(&reg, id), Some(hole("scont_1")));

        // A NEW run over the parked frame is an ordinary checkout — it
        // carries the holes out, and while running they still read.
        let co = reg.checkout_run(id).expect("run over parked frame");
        assert_eq!(
            pending_holes(&reg, id),
            vec![hole("scont_1")],
            "parked holes stay visible while the machine is out"
        );
        // …this second run parks another hole.
        co.restore_suspended(vec![hole("scont_1"), hole("scont_2")]);
        assert_eq!(
            pending_holes(&reg, id),
            vec![hole("scont_1"), hole("scont_2")]
        );
        assert_eq!(
            pending_hole(&reg, id),
            Some(hole("scont_2")),
            "the single-hole view is the newest"
        );

        // Resume the OLDER hole first (any-order — the machine imposes none).
        let co = reg
            .checkout_resume(id, &hole("scont_1"))
            .expect("older hole is a member");
        co.restore_suspended(vec![hole("scont_2")]);
        assert_eq!(pending_holes(&reg, id), vec![hole("scont_2")]);

        // Then the newer; the session goes idle via the unified restore.
        let co = reg.checkout_resume(id, &hole("scont_2")).expect("newer");
        co.restore_suspended(Vec::new());
        assert!(is_idle(&reg, id));
    }

    #[test]
    fn resume_on_non_member_hole_is_wrong_hole_and_consumes_nothing() {
        let reg = SessionRegistry::new();
        let id = SessionId(3);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        let co = reg.checkout_run(id).expect("idle → run");
        co.restore_suspended(vec![hole("scont_1")]);

        assert_eq!(
            err(reg.checkout_resume(id, &hole("scont_9"))),
            CheckoutError::WrongHole {
                session: id,
                attempted: hole("scont_9"),
                parked: vec![hole("scont_1")],
            }
        );
        assert_eq!(pending_holes(&reg, id), vec![hole("scont_1")]);

        // Idle session: WrongHole with an empty parked set, not Running.
        let co = reg.checkout_resume(id, &hole("scont_1")).expect("member");
        co.restore_suspended(Vec::new());
        assert_eq!(
            err(reg.checkout_resume(id, &hole("scont_1"))),
            CheckoutError::WrongHole {
                session: id,
                attempted: hole("scont_1"),
                parked: Vec::new(),
            }
        );
    }

    #[test]
    fn unknown_session_is_unknown() {
        let reg: SessionRegistry<FakeMachine> = SessionRegistry::new();
        assert_eq!(
            err(reg.checkout_run(SessionId(9))),
            CheckoutError::Unknown(SessionId(9))
        );
    }

    /// A child checkout requires a parked hole, carries the holes through the
    /// run, and while the machine is out EVERY other checkout is rejected
    /// (one machine, one computation at a time).
    #[test]
    fn child_checkout_over_parked_frames_and_exclusivity() {
        let reg = SessionRegistry::new();
        let id = SessionId(5);
        reg.insert_idle(id, FakeMachine { turns: 0 });

        let co = reg.checkout_run(id).expect("idle → run");
        co.restore_suspended(vec![hole("scont_1")]);

        let mut child = reg.checkout_child(id).expect("child over parked frame");
        assert_eq!(
            pending_hole(&reg, id),
            Some(hole("scont_1")),
            "parent's hole stays visible while a child runs"
        );

        // While the child runs, EVERYTHING else is rejected.
        assert_eq!(err(reg.checkout_run(id)), CheckoutError::Running(id));
        assert_eq!(
            err(reg.checkout_resume(id, &hole("scont_1"))),
            CheckoutError::Running(id),
            "parent resume must be rejected while a child is mid-run"
        );
        assert_eq!(
            err(reg.checkout_child(id)),
            CheckoutError::Running(id),
            "a second concurrent child must be rejected"
        );

        // The child ran a turn on the machine; restore with the same holes.
        child.machine().turns += 1;
        child.restore_suspended(vec![hole("scont_1")]);
        assert_eq!(pending_hole(&reg, id), Some(hole("scont_1")));

        // The parent now resumes on its (untouched) hole; the child's turn count
        // survived (it ran on the same machine).
        let mut co = reg
            .checkout_resume(id, &hole("scont_1"))
            .expect("parent resumes after the child completes");
        assert_eq!(co.machine().turns, 1, "the child's turn ran on the machine");
        co.restore_suspended(Vec::new());
        assert!(is_idle(&reg, id));
    }

    /// A child requires a parked hole — checking a child out on an idle
    /// session is `NotSuspended`, not a silent mis-transition.
    #[test]
    fn child_checkout_on_idle_is_not_suspended() {
        let reg = SessionRegistry::new();
        let id = SessionId(6);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        assert_eq!(err(reg.checkout_child(id)), CheckoutError::NotSuspended(id));
    }

    #[test]
    fn peek_reads_idle_and_suspended_but_not_running() {
        let reg = SessionRegistry::new();
        let id = SessionId(10);
        reg.insert_idle(id, FakeMachine { turns: 3 });
        assert_eq!(reg.peek(id, |m| m.turns), Some(3));

        let co = reg.checkout_run(id).expect("idle -> run");
        assert_eq!(
            reg.peek(id, |m| m.turns),
            None,
            "a checked-out (Running) machine has nothing to peek"
        );
        co.restore_suspended(vec![hole("scont_peek")]);
        assert_eq!(
            reg.peek(id, |m| m.turns),
            Some(3),
            "a suspended machine is still present in its slot"
        );

        assert_eq!(reg.peek(SessionId(999), |m: &FakeMachine| m.turns), None);
    }

    /// A `Checkout` dropped WITHOUT an explicit restore (a panic unwinding
    /// between checkout and restore) must not leave the slot `Running`
    /// forever — AND must not lose the parked holes it carried out: `Drop`
    /// restores `Suspended` with the carried set (the frames are still
    /// rooted in the machine), `Idle` only when there were none.
    #[test]
    fn dropping_a_checkout_restores_the_carried_hole_set() {
        let reg = SessionRegistry::new();
        let id = SessionId(11);
        reg.insert_idle(id, FakeMachine { turns: 0 });

        // No holes: unwound turn restores Idle (the original pin).
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut co = reg.checkout_run(id).expect("idle -> run");
            co.machine().turns += 1;
            panic!("simulated turn panic between checkout and restore");
        }));
        assert!(result.is_err(), "the closure must have panicked");
        assert!(
            is_idle(&reg, id),
            "a holeless Checkout dropped by a panic must restore Idle"
        );

        // With a parked hole: the unwound turn restores Suspended{holes}.
        let co = reg.checkout_run(id).expect("run");
        co.restore_suspended(vec![hole("scont_1")]);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _co = reg.checkout_run(id).expect("run over parked");
            panic!("simulated panic with a parked hole carried out");
        }));
        assert!(result.is_err());
        assert_eq!(
            pending_holes(&reg, id),
            vec![hole("scont_1")],
            "the carried hole set survives an unwound turn"
        );
        let mut co = reg
            .checkout_resume(id, &hole("scont_1"))
            .expect("the parked hole is still resumable after the panic");
        assert_eq!(
            co.machine().turns,
            1,
            "the machine's pre-panic mutation survived the Drop-recovery round trip"
        );
        co.restore_suspended(Vec::new());
    }
}
