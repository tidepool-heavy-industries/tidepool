//! Session registry — the resident-machine lifecycle guardian (segment 20).
//!
//! A `HashMap<SessionId, Slot<M>>` where `Slot` (defined in [`crate::tree`]) is
//! `Idle(M) | Running | Suspended { machine, hole }`. Every machine access goes
//! through this map — the stowed-XOR-running discipline that justifies
//! `unsafe impl Send for JitEffectMachine` maps directly onto the slot variants:
//! a machine is in EXACTLY one slot, and `Running` means it is out on a turn (no
//! side-channel access while running).
//!
//! Generic over the machine handle `M` so this crate stays free of the JIT
//! dependency — segment 20's `tidepool-runtime::session::ResidentSession` is the
//! `M` the harness instantiates.
//!
//! # Lifecycle transitions are atomic at the dispatch boundary
//!
//! Ported from `tidepool-repl`'s `state.rs` discipline (the pattern, not the
//! code): the transition — inspect the slot, decide, move the owned machine out
//! — happens under one short lock; the TURN itself (compile + run, which blocks)
//! runs with the lock RELEASED (the machine owned on the caller's stack), then a
//! second short lock restores the machine as `Idle` or `Suspended`. The
//! `parking_lot::Mutex` is NEVER held across the turn.
//!
//! A [`Checkout`] is the RAII proof that a machine is out on a turn: it owns the
//! machine and, on restore, moves it back under the lock. Dropping a `Checkout`
//! without restoring is a bug (the session is left `Running` forever) — the
//! `#[must_use]` and the [`Checkout::abandon`] escape hatch make that explicit.
//!
//! # Segment boundary
//!
//! A `Suspended` session REJECTS a new-turn checkout ([`CheckoutError::Suspended`])
//! — nested child runs on a stowed continuation are segment 40's job. Here a
//! suspended session accepts only a resume/abort checkout keyed by its hole.

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
    /// `Slot::Running` or `Slot::RunningChild`). Turns on one session are
    /// strictly sequential.
    #[error("session {0} is already running a turn")]
    Running(SessionId),
    /// A resume/abort or new-turn checkout was attempted while a nested CHILD
    /// run is executing against the suspended parent (`Slot::RunningChild`).
    /// The parent stays suspended on `hole`; retry after the child completes.
    #[error("session {session} is running a nested child against hole {hole:?}; wait for it")]
    RunningChild { session: SessionId, hole: HoleId },
    /// A child-run checkout was attempted on a session that is NOT suspended
    /// (a child requires a suspended parent by construction).
    #[error("session {0} is not suspended; a nested child requires a suspended parent")]
    NotSuspended(SessionId),
    /// A new turn was attempted on a suspended session. Until segment 40 lands
    /// nested child runs, a suspended session accepts only a resume/abort of its
    /// pending hole.
    #[error("session {session} is suspended on {hole:?}; resume or abort it first")]
    Suspended { session: SessionId, hole: HoleId },
    /// A resume/abort referenced a hole that is not the one this session is
    /// suspended on (or the session is idle, not suspended). The pending
    /// continuation is NOT consumed — the caller can retry with the right hole
    /// (the engine's validate-before-consume semantics, at the registry layer).
    #[error("session {session}: no pending hole {attempted:?}{}", match .pending {
        Some(h) => format!(" (suspended on {h:?})"),
        None => " (session is idle)".to_string(),
    })]
    WrongHole {
        session: SessionId,
        attempted: HoleId,
        pending: Option<HoleId>,
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

    /// Whether a session exists and is idle (ready for a new turn).
    pub fn is_idle(&self, id: SessionId) -> bool {
        matches!(self.slots.lock().get(&id), Some(Slot::Idle(_)))
    }

    /// The hole a session is suspended on, if any. A session with a nested
    /// child mid-run (`RunningChild`) is still suspended on its hole.
    pub fn pending_hole(&self, id: SessionId) -> Option<HoleId> {
        match self.slots.lock().get(&id) {
            Some(Slot::Suspended { hole, .. }) => Some(hole.clone()),
            Some(Slot::RunningChild { hole }) => Some(hole.clone()),
            _ => None,
        }
    }

    /// Check a machine OUT for a new turn: `Idle → Running`, moving the machine
    /// onto the returned [`Checkout`]. Refuses a running or suspended session
    /// (the latter is segment 40's boundary). The lock is released with the slot
    /// left `Running`; the caller runs the turn, then restores via the
    /// `Checkout`.
    pub fn checkout_run(&self, id: SessionId) -> Result<Checkout<'_, M>, CheckoutError> {
        let mut slots = self.slots.lock();
        match slots.get_mut(&id) {
            None => Err(CheckoutError::Unknown(id)),
            Some(Slot::Running) => Err(CheckoutError::Running(id)),
            Some(Slot::RunningChild { hole }) => Err(CheckoutError::RunningChild {
                session: id,
                hole: hole.clone(),
            }),
            Some(Slot::Suspended { hole, .. }) => Err(CheckoutError::Suspended {
                session: id,
                hole: hole.clone(),
            }),
            Some(slot @ Slot::Idle(_)) => {
                let Slot::Idle(machine) = std::mem::replace(slot, Slot::Running) else {
                    unreachable!("matched Idle above")
                };
                Ok(Checkout {
                    registry: self,
                    id,
                    machine: Some(machine),
                })
            }
        }
    }

    /// Check a machine OUT to resume/abort its pending hole: `Suspended{hole} →
    /// Running`, validating `hole` matches. A mismatch leaves the slot untouched
    /// ([`CheckoutError::WrongHole`]) — validate-before-consume. The lock is
    /// released `Running`; the caller drives the resume, then restores.
    pub fn checkout_resume(
        &self,
        id: SessionId,
        hole: &HoleId,
    ) -> Result<Checkout<'_, M>, CheckoutError> {
        let mut slots = self.slots.lock();
        match slots.get_mut(&id) {
            None => Err(CheckoutError::Unknown(id)),
            Some(Slot::Running) => Err(CheckoutError::Running(id)),
            Some(Slot::RunningChild { hole: pending }) => Err(CheckoutError::RunningChild {
                session: id,
                hole: pending.clone(),
            }),
            Some(Slot::Idle(_)) => Err(CheckoutError::WrongHole {
                session: id,
                attempted: hole.clone(),
                pending: None,
            }),
            Some(Slot::Suspended {
                hole: pending_hole, ..
            }) if pending_hole != hole => Err(CheckoutError::WrongHole {
                session: id,
                attempted: hole.clone(),
                pending: Some(pending_hole.clone()),
            }),
            Some(slot) => {
                // Matched Suspended with the right hole.
                let Slot::Suspended { machine, .. } = std::mem::replace(slot, Slot::Running) else {
                    unreachable!("matched Suspended above")
                };
                Ok(Checkout {
                    registry: self,
                    id,
                    machine: Some(machine),
                })
            }
        }
    }

    /// Check a machine OUT for a NESTED CHILD run against its suspended parent
    /// (segment 40): `Suspended{hole} → RunningChild{hole}`, keeping the hole so
    /// the parent stays suspended. The child restores via
    /// [`Checkout::restore_suspended`] with the SAME hole. Refuses a session
    /// that is not suspended, already running, or running another child.
    ///
    /// The `hole` need not be validated here the way `checkout_resume` does —
    /// a child run does not consume the parent's continuation (the parent's
    /// stowed continuation is GC-rooted, not fed) — but the caller passes it so
    /// the slot can carry it through `RunningChild` back to `Suspended`.
    pub fn checkout_child(&self, id: SessionId) -> Result<Checkout<'_, M>, CheckoutError> {
        let mut slots = self.slots.lock();
        match slots.get_mut(&id) {
            None => Err(CheckoutError::Unknown(id)),
            Some(Slot::Running) => Err(CheckoutError::Running(id)),
            Some(Slot::RunningChild { hole }) => Err(CheckoutError::RunningChild {
                session: id,
                hole: hole.clone(),
            }),
            Some(Slot::Idle(_)) => Err(CheckoutError::NotSuspended(id)),
            Some(slot @ Slot::Suspended { .. }) => {
                let hole = match slot {
                    Slot::Suspended { hole, .. } => hole.clone(),
                    _ => unreachable!("matched Suspended above"),
                };
                let Slot::Suspended { machine, .. } =
                    std::mem::replace(slot, Slot::RunningChild { hole })
                else {
                    unreachable!("matched Suspended above")
                };
                Ok(Checkout {
                    registry: self,
                    id,
                    machine: Some(machine),
                })
            }
        }
    }

    /// Restore a checked-out machine as `Idle` (turn completed) under the lock.
    fn restore_idle(&self, id: SessionId, machine: M) {
        self.slots.lock().insert(id, Slot::Idle(machine));
    }

    /// Restore a checked-out machine as `Suspended{hole}` (turn suspended at an
    /// ask) under the lock.
    fn restore_suspended(&self, id: SessionId, machine: M, hole: HoleId) {
        self.slots
            .lock()
            .insert(id, Slot::Suspended { machine, hole });
    }
}

/// RAII proof that a session's machine is OUT on a turn (`Slot::Running`). Owns
/// the machine for the turn; restore it exactly once via [`Self::restore_idle`]
/// or [`Self::restore_suspended`].
///
/// Dropping a `Checkout` without restoring leaves the slot `Running` (the
/// session is wedged). That is a caller bug; [`Self::abandon`] is the explicit
/// "the machine is gone, drop the session" path for teardown.
#[must_use = "a checked-out machine must be restored (or abandoned), or the session is left Running forever"]
pub struct Checkout<'r, M> {
    registry: &'r SessionRegistry<M>,
    id: SessionId,
    machine: Option<M>,
}

impl<M> Checkout<'_, M> {
    /// The session id this checkout is for.
    pub fn session_id(&self) -> SessionId {
        self.id
    }

    /// Borrow the checked-out machine for the turn.
    pub fn machine(&mut self) -> &mut M {
        self.machine
            .as_mut()
            .expect("machine present until restore/abandon")
    }

    /// Take ownership of the machine off the checkout (e.g. to move it onto an
    /// eval thread). The caller MUST return it via [`Self::restore_idle`] /
    /// [`Self::restore_suspended`].
    pub fn take(&mut self) -> M {
        self.machine
            .take()
            .expect("machine present until restore/abandon")
    }

    /// Put the machine back via [`Self::restore_idle`] after a `take`.
    pub fn put(&mut self, machine: M) {
        self.machine = Some(machine);
    }

    /// Restore the machine as `Idle` (the turn completed): `Running → Idle`.
    pub fn restore_idle(mut self) {
        let machine = self
            .machine
            .take()
            .expect("machine present until restore/abandon");
        self.registry.restore_idle(self.id, machine);
    }

    /// Restore the machine as `Suspended{hole}` (the turn suspended at an ask):
    /// `Running → Suspended`.
    pub fn restore_suspended(mut self, hole: HoleId) {
        let machine = self
            .machine
            .take()
            .expect("machine present until restore/abandon");
        self.registry.restore_suspended(self.id, machine, hole);
    }

    /// The turn faulted irrecoverably: drop the session outright (`Running →
    /// gone`) rather than leaving it wedged. Teardown path.
    pub fn abandon(mut self) {
        self.machine.take();
        self.registry.remove(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial stand-in for the machine handle `M` — the registry is pure
    /// lifecycle bookkeeping, so a counter proves the transitions without the
    /// JIT.
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

    #[test]
    fn idle_run_completes_back_to_idle() {
        let reg = SessionRegistry::new();
        let id = SessionId(1);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        assert!(reg.is_idle(id));

        let mut co = reg.checkout_run(id).expect("idle → run");
        // While running, the slot rejects a second turn and reads not-idle.
        assert!(!reg.is_idle(id));
        assert_eq!(err(reg.checkout_run(id)), CheckoutError::Running(id));
        co.machine().turns += 1;
        co.restore_idle();

        assert!(reg.is_idle(id));
        // The machine (and its accumulated turn count) survived the round-trip.
        let mut co = reg.checkout_run(id).expect("idle again");
        assert_eq!(co.machine().turns, 1);
        co.restore_idle();
    }

    #[test]
    fn suspend_rejects_new_run_then_resumes_on_matching_hole() {
        let reg = SessionRegistry::new();
        let id = SessionId(2);
        reg.insert_idle(id, FakeMachine { turns: 0 });

        // Run → suspend.
        let co = reg.checkout_run(id).expect("idle → run");
        co.restore_suspended(hole("scont_1"));
        assert_eq!(reg.pending_hole(id), Some(hole("scont_1")));

        // A NEW run is rejected while suspended (segment-20 boundary).
        assert_eq!(
            err(reg.checkout_run(id)),
            CheckoutError::Suspended {
                session: id,
                hole: hole("scont_1")
            }
        );

        // Resume on the WRONG hole is rejected WITHOUT consuming (retryable).
        assert_eq!(
            err(reg.checkout_resume(id, &hole("scont_9"))),
            CheckoutError::WrongHole {
                session: id,
                attempted: hole("scont_9"),
                pending: Some(hole("scont_1")),
            }
        );
        assert_eq!(reg.pending_hole(id), Some(hole("scont_1")));

        // Resume on the RIGHT hole checks the machine out and completes.
        let co = reg
            .checkout_resume(id, &hole("scont_1"))
            .expect("right hole");
        co.restore_idle();
        assert!(reg.is_idle(id));
    }

    #[test]
    fn resume_on_idle_session_is_wrong_hole_not_running() {
        let reg = SessionRegistry::new();
        let id = SessionId(3);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        assert_eq!(
            err(reg.checkout_resume(id, &hole("scont_1"))),
            CheckoutError::WrongHole {
                session: id,
                attempted: hole("scont_1"),
                pending: None,
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

    /// Segment 40: a suspended session hosts a nested child run
    /// (`Suspended → RunningChild → Suspended`), and while the child is mid-run
    /// the parent's resume/abort and a new top-level run are all rejected
    /// cleanly (sequential-isolated). After the child restores, the parent is
    /// suspended on the SAME hole and resumes normally.
    #[test]
    fn nested_child_run_keeps_parent_suspended_and_blocks_resume() {
        let reg = SessionRegistry::new();
        let id = SessionId(5);
        reg.insert_idle(id, FakeMachine { turns: 0 });

        // Run → suspend.
        let co = reg.checkout_run(id).expect("idle → run");
        co.restore_suspended(hole("scont_1"));
        assert_eq!(reg.pending_hole(id), Some(hole("scont_1")));

        // Check a child out: Suspended → RunningChild, parent still suspended.
        let mut child = reg.checkout_child(id).expect("suspended → child");
        assert_eq!(
            reg.pending_hole(id),
            Some(hole("scont_1")),
            "parent stays suspended on its hole while a child runs"
        );

        // While the child runs, EVERYTHING else is rejected.
        assert_eq!(
            err(reg.checkout_run(id)),
            CheckoutError::RunningChild {
                session: id,
                hole: hole("scont_1")
            }
        );
        assert_eq!(
            err(reg.checkout_resume(id, &hole("scont_1"))),
            CheckoutError::RunningChild {
                session: id,
                hole: hole("scont_1")
            },
            "parent resume must be rejected while a child is mid-run"
        );
        assert_eq!(
            err(reg.checkout_child(id)),
            CheckoutError::RunningChild {
                session: id,
                hole: hole("scont_1")
            },
            "a second concurrent child must be rejected"
        );

        // The child ran a turn on the machine; restore back to Suspended.
        child.machine().turns += 1;
        child.restore_suspended(hole("scont_1"));
        assert_eq!(reg.pending_hole(id), Some(hole("scont_1")));

        // The parent now resumes on its (untouched) hole; the child's turn count
        // survived (it ran on the same machine).
        let mut co = reg
            .checkout_resume(id, &hole("scont_1"))
            .expect("parent resumes after the child completes");
        assert_eq!(co.machine().turns, 1, "the child's turn ran on the machine");
        co.restore_idle();
        assert!(reg.is_idle(id));
    }

    /// A nested child requires a suspended parent — checking a child out on an
    /// idle session is `NotSuspended`, not a silent mis-transition.
    #[test]
    fn child_checkout_on_idle_is_not_suspended() {
        let reg = SessionRegistry::new();
        let id = SessionId(6);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        assert_eq!(
            err(reg.checkout_child(id)),
            CheckoutError::NotSuspended(id)
        );
    }

    #[test]
    fn abandon_drops_a_wedged_turn() {
        let reg = SessionRegistry::new();
        let id = SessionId(4);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        let co = reg.checkout_run(id).expect("idle → run");
        co.abandon();
        // The session is gone, not left Running.
        assert_eq!(err(reg.checkout_run(id)), CheckoutError::Unknown(id));
    }
}
