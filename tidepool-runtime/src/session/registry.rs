//! `SessionRegistry` — tidepool's ONE resident-machine ownership/lifecycle
//! registry (see the root `CLAUDE.md` Mechanism Index: "session
//! checkout/ownership").
//!
//! A `HashMap<SessionId, Entry<M, H>>` where [`Slot`] is `Idle(M) |
//! Running{holes} | Suspended{machine,holes} | Wedged{since}`, generic over
//! the machine handle `M` (so this module stays free of any concrete JIT/
//! session dependency) AND the hole/continuation identity type `H` (so a
//! keyed, multi-hole consumer — `tidepool-harness`'s `HoleId` — and a
//! single-hole consumer — `tidepool-repl`'s `ContinuationId` — share the same
//! mechanism without unifying those two newtypes, which is a separate,
//! not-yet-proposed duplicate). Every machine access goes through this map —
//! the stowed-XOR-running discipline that justifies `unsafe impl Send` on a
//! resident machine maps directly onto the slot variants: a machine is in
//! EXACTLY one slot, and `Running` means it is out on a turn (no side-channel
//! access while running).
//!
//! MULTI-HOLE: a suspended session carries a SET of parked holes, each
//! resumable by identity in ANY order (the machine's own continuation
//! registry imposes none), and a NEW top-level run over parked frames is an
//! ordinary checkout, not a refusal. The SESSION is the ground truth for its
//! own hole set; the slot mirrors it at restore time — a caller restores with
//! the hole set the session actually reports, never a guess from a turn's
//! domain result.
//!
//! # Lifecycle transitions are atomic at the dispatch boundary
//!
//! The transition — inspect the slot, decide, move the owned machine out —
//! happens under one short lock; the TURN itself (compile + run, which
//! blocks) runs with the lock RELEASED (the machine owned on the caller's
//! stack), then a second short lock restores the machine as `Idle`,
//! `Suspended`, or `Wedged`. The `parking_lot::Mutex` is NEVER held across the
//! turn.
//!
//! A [`Checkout`] is the RAII proof that a machine is out on a turn: it owns
//! the machine and, on settlement, moves it back under the lock. Dropping a
//! `Checkout` without settling is a bug (the session is left `Running`
//! forever) — the `#[must_use]` and the panic-safety `Drop` (which restores
//! the CARRIED hole set, not a bare `Idle`) make the exit explicit.
//!
//! # Epoch guard — stale-checkout-after-replace
//!
//! Every entry carries a monotonic epoch, minted fresh on
//! [`SessionRegistry::insert_idle`]; a [`Checkout`] carries the epoch it read
//! at checkout time. A settlement (`restore_suspended`/`mark_wedged`, or the
//! panic-safety `Drop`) against an entry whose CURRENT epoch does not match —
//! because [`SessionRegistry::remove`] deleted it (a wedged/crashed turn
//! never coming back, or a caller retiring the session out from under an
//! in-flight checkout) — DROPS the machine instead of resurrecting or
//! clobbering whatever now occupies (or no longer occupies) that id. Harness
//! `SessionId`s and `SingleSlot`'s minted ids are never reused, so this is
//! belt-and-suspenders on top of that invariant, not a substitute for it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use tidepool_repr::SessionId;

/// Why a machine checkout was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckoutError<H> {
    /// No session registered under this id.
    #[error("no session {0}")]
    Unknown(SessionId),
    /// A single-slot facade ([`SingleSlot`]) has no current entry at all —
    /// distinct from [`Self::Unknown`], which names a specific stale id.
    #[error("no session is open")]
    NoSession,
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
        attempted: H,
        parked: Vec<H>,
    },
    /// The session is in a TERMINAL slot state (`Wedged`) that refuses every
    /// checkout. `label` is [`Slot::label`]'s string.
    #[error("session {session}: {label}")]
    Terminal { session: SessionId, label: String },
}

/// Resident-session registry slot. `Idle`/`Running`/`Suspended` are the
/// live-machine states every consumer shares; `Wedged` is a terminal
/// placeholder some consumers (REPL's single implicit session) use to keep a
/// reason visible for a reaper's TTL window instead of removing the entry
/// outright — a consumer that never constructs it (the harness, which
/// retires the whole node via a different mechanism instead) simply never
/// sees it.
#[derive(Debug)]
pub enum Slot<M, H> {
    Idle(M),
    /// The machine is out on a turn — a fresh run, a resume, or a child run
    /// over parked frames. `holes` are the parked holes the session had when
    /// it left, carried so reads and errors stay truthful while the machine
    /// is out, and so the panic-safety `Drop` can restore them instead of
    /// losing them.
    Running {
        holes: Vec<H>,
    },
    /// The machine is present with one or more parked holes, each resumable
    /// by identity in any order. Newest last.
    Suspended {
        machine: M,
        holes: Vec<H>,
    },
    /// The machine is irrecoverably gone (a turn thread that outran its
    /// abort grace, or crashed off the blocking pool) — nothing to restore,
    /// but the reason stays visible until an explicit
    /// [`SessionRegistry::remove`]/reinstall reclaims the slot.
    Wedged {
        since: Instant,
    },
}

impl<M, H: std::fmt::Debug> Slot<M, H> {
    /// Human-facing summary of this slot's state — a busy-guard rejection
    /// message, a diagnostic log line, or [`CheckoutError::Terminal`]'s
    /// display all read this one string, so REPL's caller-facing label and
    /// the registry's own error text cannot drift apart.
    pub fn label(&self) -> String {
        match self {
            Slot::Idle(_) => "idle".to_string(),
            Slot::Running { .. } => "running".to_string(),
            Slot::Suspended { holes, .. } => match holes.last() {
                Some(h) => format!("suspended (continuation {h:?})"),
                None => "suspended".to_string(),
            },
            Slot::Wedged { .. } => "wedged (a turn timed out)".to_string(),
        }
    }
}

struct Entry<M, H> {
    epoch: u64,
    slot: Slot<M, H>,
}

/// The resident-session registry: owns every session's [`Slot`] and gates all
/// machine access through atomic checkout/settlement transitions.
pub struct SessionRegistry<M, H> {
    slots: Mutex<HashMap<SessionId, Entry<M, H>>>,
    next_epoch: AtomicU64,
}

impl<M, H> Default for SessionRegistry<M, H> {
    fn default() -> Self {
        SessionRegistry {
            slots: Mutex::new(HashMap::new()),
            next_epoch: AtomicU64::new(1),
        }
    }
}

impl<M, H: Clone + PartialEq + std::fmt::Debug> SessionRegistry<M, H> {
    /// A fresh, empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a freshly-bootstrapped session as `Idle`, minting a fresh
    /// epoch. Returns the previous slot if the id was already present (the
    /// caller decides whether that is a reset or a collision) — this is the
    /// ONE place an entry's epoch is minted, so a caller that never reuses an
    /// id (every consumer today) never needs to think about epochs at all.
    pub fn insert_idle(&self, id: SessionId, machine: M) -> Option<Slot<M, H>> {
        let epoch = self.next_epoch.fetch_add(1, Ordering::Relaxed);
        self.slots
            .lock()
            .insert(
                id,
                Entry {
                    epoch,
                    slot: Slot::Idle(machine),
                },
            )
            .map(|e| e.slot)
    }

    /// Remove a session entirely, returning its slot (drops the machine when
    /// the returned slot is dropped). The get-unstuck / teardown path. A
    /// checkout still outstanding for `id` finds its epoch stale on
    /// settlement and drops its machine instead of resurrecting this entry.
    pub fn remove(&self, id: SessionId) -> Option<Slot<M, H>> {
        self.slots.lock().remove(&id).map(|e| e.slot)
    }

    /// Read-only access to the machine WITHOUT checking it out — only
    /// succeeds when the machine is actually present in its slot (`Idle` or
    /// `Suspended`). Used for cheap metadata reads that must not disturb the
    /// checkout discipline or race a real checkout — the lock is held only
    /// for the duration of `f`.
    pub fn peek<R>(&self, id: SessionId, f: impl FnOnce(&M) -> R) -> Option<R> {
        match self.slots.lock().get(&id) {
            Some(Entry {
                slot: Slot::Idle(m),
                ..
            }) => Some(f(m)),
            Some(Entry {
                slot: Slot::Suspended { machine, .. },
                ..
            }) => Some(f(machine)),
            _ => None,
        }
    }

    /// This session's current [`Slot::label`], if it has an entry at all.
    pub fn label(&self, id: SessionId) -> Option<String> {
        self.slots.lock().get(&id).map(|e| e.slot.label())
    }

    /// Check a machine OUT for a new turn: `Idle | Suspended → Running`. A
    /// new turn over PARKED frames is ordinary (the machine's continuation
    /// registry keeps every parked frame rooted while unrelated fragments
    /// run). Refuses a session already running, one that is `Wedged`, or an
    /// unknown id.
    pub fn checkout_run(&self, id: SessionId) -> Result<Checkout<'_, M, H>, CheckoutError<H>> {
        let mut slots = self.slots.lock();
        match slots.get_mut(&id) {
            None => Err(CheckoutError::Unknown(id)),
            Some(entry) => match &entry.slot {
                Slot::Running { .. } => Err(CheckoutError::Running(id)),
                Slot::Wedged { .. } => Err(CheckoutError::Terminal {
                    session: id,
                    label: entry.slot.label(),
                }),
                Slot::Idle(_) | Slot::Suspended { .. } => {
                    let holes = slot_holes(&entry.slot);
                    let epoch = entry.epoch;
                    let machine = take_machine(&mut entry.slot, holes.clone());
                    Ok(Checkout {
                        registry: self,
                        id,
                        epoch,
                        machine: Some(machine),
                        holes,
                    })
                }
            },
        }
    }

    /// Check a machine OUT to resume/abort one of its parked holes:
    /// `Suspended → Running`, validating `hole` is a MEMBER of the parked
    /// set (any order). A mismatch leaves the slot untouched
    /// ([`CheckoutError::WrongHole`]) — validate-before-consume.
    pub fn checkout_resume(
        &self,
        id: SessionId,
        hole: &H,
    ) -> Result<Checkout<'_, M, H>, CheckoutError<H>> {
        let mut slots = self.slots.lock();
        match slots.get_mut(&id) {
            None => Err(CheckoutError::Unknown(id)),
            Some(entry) => match &entry.slot {
                Slot::Running { .. } => Err(CheckoutError::Running(id)),
                Slot::Wedged { .. } => Err(CheckoutError::Terminal {
                    session: id,
                    label: entry.slot.label(),
                }),
                Slot::Idle(_) => Err(CheckoutError::WrongHole {
                    session: id,
                    attempted: hole.clone(),
                    parked: Vec::new(),
                }),
                Slot::Suspended { holes, .. } if !holes.contains(hole) => {
                    Err(CheckoutError::WrongHole {
                        session: id,
                        attempted: hole.clone(),
                        parked: holes.clone(),
                    })
                }
                Slot::Suspended { .. } => {
                    let holes = slot_holes(&entry.slot);
                    let epoch = entry.epoch;
                    let machine = take_machine(&mut entry.slot, holes.clone());
                    Ok(Checkout {
                        registry: self,
                        id,
                        epoch,
                        machine: Some(machine),
                        holes,
                    })
                }
            },
        }
    }

    /// Check a machine OUT for a CHILD run over its parked frames:
    /// `Suspended → Running`, requiring at least one parked hole. Otherwise
    /// identical to [`Self::checkout_run`].
    pub fn checkout_child(&self, id: SessionId) -> Result<Checkout<'_, M, H>, CheckoutError<H>> {
        let mut slots = self.slots.lock();
        match slots.get_mut(&id) {
            None => Err(CheckoutError::Unknown(id)),
            Some(entry) => match &entry.slot {
                Slot::Running { .. } => Err(CheckoutError::Running(id)),
                Slot::Wedged { .. } => Err(CheckoutError::Terminal {
                    session: id,
                    label: entry.slot.label(),
                }),
                Slot::Idle(_) => Err(CheckoutError::NotSuspended(id)),
                Slot::Suspended { .. } => {
                    let holes = slot_holes(&entry.slot);
                    let epoch = entry.epoch;
                    let machine = take_machine(&mut entry.slot, holes.clone());
                    Ok(Checkout {
                        registry: self,
                        id,
                        epoch,
                        machine: Some(machine),
                        holes,
                    })
                }
            },
        }
    }

    /// Settle a checked-out machine with its post-turn parked hole set under
    /// the lock. An empty set restores `Idle`. A stale epoch (the entry was
    /// removed or replaced while this checkout was outstanding) drops the
    /// machine instead of writing it back.
    fn restore_suspended(&self, id: SessionId, epoch: u64, machine: M, holes: Vec<H>) {
        let mut slots = self.slots.lock();
        match slots.get_mut(&id) {
            Some(entry) if entry.epoch == epoch => {
                entry.slot = if holes.is_empty() {
                    Slot::Idle(machine)
                } else {
                    Slot::Suspended { machine, holes }
                };
            }
            _ => drop(machine),
        }
    }

    /// Settle a checked-out machine as `Wedged{since}` — the turn never gave
    /// the machine back (it was moved onto a blocking task that outran its
    /// abort grace or crashed), so there is nothing to restore; this keeps
    /// the reason visible until a reaper/`remove` reclaims the slot. Same
    /// stale-epoch drop rule as [`Self::restore_suspended`] (there is no
    /// machine to drop here — a stale mark is simply a no-op on the entry).
    fn mark_wedged(&self, id: SessionId, epoch: u64, since: Instant) {
        let mut slots = self.slots.lock();
        if let Some(entry) = slots.get_mut(&id) {
            if entry.epoch == epoch {
                entry.slot = Slot::Wedged { since };
            }
        }
    }
}

/// The parked holes a present-machine slot carries (`Idle` → none).
fn slot_holes<M, H: Clone>(slot: &Slot<M, H>) -> Vec<H> {
    match slot {
        Slot::Idle(_) => Vec::new(),
        Slot::Suspended { holes, .. } => holes.clone(),
        Slot::Running { .. } | Slot::Wedged { .. } => {
            unreachable!("caller matched a present-machine slot")
        }
    }
}

/// Move the machine out of a present-machine slot, leaving `Running{holes}`.
fn take_machine<M, H>(slot: &mut Slot<M, H>, holes: Vec<H>) -> M {
    match std::mem::replace(slot, Slot::Running { holes }) {
        Slot::Idle(machine) => machine,
        Slot::Suspended { machine, .. } => machine,
        Slot::Running { .. } | Slot::Wedged { .. } => {
            unreachable!("caller matched a present-machine slot")
        }
    }
}

/// RAII proof that a session's machine is OUT on a turn (`Slot::Running`).
/// Owns the machine for the turn; settle it exactly once via
/// [`Self::restore_suspended`] or [`Self::mark_wedged`].
///
/// Dropping a `Checkout` without settling leaves the slot `Running` (the
/// session is wedged) — the panic-safety `Drop` below is the only exit that
/// does not require an explicit settlement call.
#[must_use = "a checked-out machine must be restored (or marked wedged), or the session is left \
              Running forever"]
pub struct Checkout<'r, M, H: Clone + PartialEq + std::fmt::Debug> {
    registry: &'r SessionRegistry<M, H>,
    id: SessionId,
    epoch: u64,
    machine: Option<M>,
    /// The parked holes carried OUT with the machine — what the panic-safety
    /// `Drop` restores (an unwound turn must not lose the session's parked
    /// frames; they are still rooted in the machine's continuation
    /// registry).
    holes: Vec<H>,
}

// Whole point of this type: a checked-out machine is settled by exactly one
// call, which consumes `self` by value. A future `#[derive(Clone)]` would let
// a caller settle the SAME checkout twice, silently reviving the
// double-settle bug this type exists to make a compile error. Pinned at
// `M = H = ()` — the struct has no `Clone`/`Copy` impl for any `M`/`H`, so a
// fixed stand-in is enough to catch a derive that would apply uniformly.
static_assertions::assert_not_impl_any!(Checkout<'static, (), ()>: Clone, Copy);

impl<M, H: Clone + PartialEq + std::fmt::Debug> Checkout<'_, M, H> {
    /// The session id this checkout is for.
    pub fn session_id(&self) -> SessionId {
        self.id
    }

    /// Borrow the checked-out machine for the turn.
    pub fn machine(&mut self) -> &mut M {
        #[allow(clippy::expect_used, reason = "machine present until settled")]
        self.machine
            .as_mut()
            .expect("machine present until settled")
    }

    /// Take ownership of the machine off the checkout (e.g. to move it onto
    /// an eval thread). The caller MUST return it via [`Self::restore_suspended`]
    /// or settle via [`Self::mark_wedged`].
    pub fn take(&mut self) -> M {
        #[allow(clippy::expect_used, reason = "machine present until settled")]
        self.machine.take().expect("machine present until settled")
    }

    /// Put the machine back after a `take`, ahead of [`Self::restore_suspended`].
    pub fn put(&mut self, machine: M) {
        self.machine = Some(machine);
    }
}

impl<M, H: Clone + PartialEq + std::fmt::Debug> Checkout<'_, M, H> {
    /// Restore the machine with its post-turn parked hole set — pass the
    /// session's OWN reported holes, never a guess from the turn's domain
    /// result. An empty set restores `Idle`.
    pub fn restore_suspended(mut self, holes: Vec<H>) {
        #[allow(clippy::expect_used, reason = "machine present until settled")]
        let machine = self.machine.take().expect("machine present until settled");
        self.registry
            .restore_suspended(self.id, self.epoch, machine, holes);
    }

    /// Settle this checkout as `Wedged{since}` — the machine was moved off
    /// this checkout (via [`Self::take`]) onto a task that never gave it
    /// back. There is nothing left to restore; this just records the reason
    /// where a reaper/caller can find it instead of leaving the slot
    /// `Running` forever.
    pub fn mark_wedged(mut self, since: Instant) {
        self.machine = None;
        self.registry.mark_wedged(self.id, self.epoch, since);
    }
}

/// Panic safety net: if a `Checkout` is dropped while it still owns the
/// machine (an explicit settlement never ran — e.g. a panic unwound through
/// the turn between checkout and settle), restore it with the hole set it
/// CARRIED OUT rather than leaving the slot `Running` forever — or silently
/// dropping parked frames to a bare `Idle`.
///
/// This does NOT cover [`Checkout::take`]: once the machine has been moved
/// off the checkout, `Drop` has nothing to restore — a caller that loses the
/// machine that way must settle explicitly instead (`mark_wedged`, or the
/// harness's `terminate_node`).
impl<M, H: Clone + PartialEq + std::fmt::Debug> Drop for Checkout<'_, M, H> {
    fn drop(&mut self) {
        if let Some(machine) = self.machine.take() {
            self.registry.restore_suspended(
                self.id,
                self.epoch,
                machine,
                std::mem::take(&mut self.holes),
            );
        }
    }
}

/// A [`SessionRegistry`] restricted to holding AT MOST one entry at a time —
/// `tidepool-repl`'s "one implicit session, no name" shape, built on the SAME
/// primitive the keyed (harness) registry uses rather than a second
/// implementation. Mints a fresh [`SessionId`] per [`Self::install`] (never
/// reused), tracking which id is "current" — a stale checkout against a
/// replaced entry finds its epoch stale on settlement (see the module doc)
/// and drops its machine rather than clobbering the fresh one.
pub struct SingleSlot<M, H> {
    registry: SessionRegistry<M, H>,
    current: Mutex<Option<SessionId>>,
    next_id: AtomicU64,
}

impl<M, H> Default for SingleSlot<M, H> {
    fn default() -> Self {
        SingleSlot {
            registry: SessionRegistry::default(),
            current: Mutex::new(None),
            next_id: AtomicU64::new(0),
        }
    }
}

impl<M, H: Clone + PartialEq + std::fmt::Debug> SingleSlot<M, H> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a freshly-opened machine as the current entry, minting a
    /// fresh id. Errors (handing the machine back) if one is already present
    /// — the caller lost an auto-open race and should drop this one and use
    /// the existing session.
    pub fn install(&self, machine: M) -> Result<SessionId, M> {
        let mut current = self.current.lock();
        if current.is_some() {
            return Err(machine);
        }
        let id = SessionId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.registry.insert_idle(id, machine);
        *current = Some(id);
        Ok(id)
    }

    /// The current entry's id, if one is installed.
    pub fn current_id(&self) -> Option<SessionId> {
        *self.current.lock()
    }

    /// The current entry's [`Slot::label`], if one is installed.
    pub fn label(&self) -> Option<String> {
        self.current_id().and_then(|id| self.registry.label(id))
    }

    /// Read-only access to the machine WITHOUT checking it out — see
    /// [`SessionRegistry::peek`].
    pub fn peek<R>(&self, f: impl FnOnce(&M) -> R) -> Option<R> {
        let id = self.current_id()?;
        self.registry.peek(id, f)
    }

    /// [`SessionRegistry::checkout_run`] against the current entry.
    pub fn checkout_run(&self) -> Result<Checkout<'_, M, H>, CheckoutError<H>> {
        let id = self.current_id().ok_or(CheckoutError::NoSession)?;
        self.registry.checkout_run(id)
    }

    /// [`SessionRegistry::checkout_resume`] against the current entry.
    pub fn checkout_resume(&self, hole: &H) -> Result<Checkout<'_, M, H>, CheckoutError<H>> {
        let id = self.current_id().ok_or(CheckoutError::NoSession)?;
        self.registry.checkout_resume(id, hole)
    }

    /// Remove the current entry wholesale (drops the machine and, with it,
    /// any stowed continuation). A turn still checked out finds its epoch
    /// stale on settlement.
    pub fn remove(&self) {
        if let Some(id) = self.current.lock().take() {
            self.registry.remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial stand-in for the machine handle `M` — the registry is pure
    /// lifecycle bookkeeping, so a counter is enough to exercise the
    /// transitions without a real JIT machine.
    #[derive(Debug, PartialEq, Eq)]
    struct FakeMachine {
        turns: u32,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Hole(&'static str);

    fn err<M, H: Clone + PartialEq + std::fmt::Debug>(
        r: Result<Checkout<'_, M, H>, CheckoutError<H>>,
    ) -> CheckoutError<H> {
        match r {
            Ok(_) => panic!("expected a checkout error, got an Ok(Checkout)"),
            Err(e) => e,
        }
    }

    fn is_idle<M, H>(reg: &SessionRegistry<M, H>, id: SessionId) -> bool {
        matches!(
            reg.slots.lock().get(&id),
            Some(Entry {
                slot: Slot::Idle(_),
                ..
            })
        )
    }

    #[test]
    fn idle_run_completes_back_to_idle() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = SessionId(1);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        assert!(is_idle(&reg, id));

        let mut co = reg.checkout_run(id).expect("idle → run");
        assert!(!is_idle(&reg, id));
        assert_eq!(err(reg.checkout_run(id)), CheckoutError::Running(id));
        co.machine().turns += 1;
        co.restore_suspended(Vec::new());

        assert!(is_idle(&reg, id));
        let mut co = reg.checkout_run(id).expect("idle again");
        assert_eq!(co.machine().turns, 1);
        co.restore_suspended(Vec::new());
    }

    #[test]
    fn multi_hole_any_order_resume_and_run_over_parked() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = SessionId(2);
        reg.insert_idle(id, FakeMachine { turns: 0 });

        let co = reg.checkout_run(id).expect("idle → run");
        co.restore_suspended(vec![Hole("h1")]);

        let co = reg.checkout_run(id).expect("run over parked frame");
        co.restore_suspended(vec![Hole("h1"), Hole("h2")]);

        let co = reg
            .checkout_resume(id, &Hole("h1"))
            .expect("older hole is a member");
        co.restore_suspended(vec![Hole("h2")]);

        let co = reg.checkout_resume(id, &Hole("h2")).expect("newer");
        co.restore_suspended(Vec::new());
        assert!(is_idle(&reg, id));
    }

    #[test]
    fn resume_on_non_member_hole_is_wrong_hole_and_consumes_nothing() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = SessionId(3);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        let co = reg.checkout_run(id).expect("idle → run");
        co.restore_suspended(vec![Hole("h1")]);

        assert_eq!(
            err(reg.checkout_resume(id, &Hole("h9"))),
            CheckoutError::WrongHole {
                session: id,
                attempted: Hole("h9"),
                parked: vec![Hole("h1")],
            }
        );
    }

    #[test]
    fn unknown_session_is_unknown() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        assert_eq!(
            err(reg.checkout_run(SessionId(9))),
            CheckoutError::Unknown(SessionId(9))
        );
    }

    #[test]
    fn child_checkout_on_idle_is_not_suspended() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = SessionId(6);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        assert_eq!(err(reg.checkout_child(id)), CheckoutError::NotSuspended(id));
    }

    #[test]
    fn dropping_a_checkout_restores_the_carried_hole_set() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = SessionId(11);
        reg.insert_idle(id, FakeMachine { turns: 0 });

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut co = reg.checkout_run(id).expect("idle -> run");
            co.machine().turns += 1;
            panic!("simulated turn panic between checkout and restore");
        }));
        assert!(result.is_err());
        assert!(is_idle(&reg, id));

        let co = reg.checkout_run(id).expect("run");
        co.restore_suspended(vec![Hole("h1")]);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _co = reg.checkout_run(id).expect("run over parked");
            panic!("simulated panic with a parked hole carried out");
        }));
        assert!(result.is_err());
        let mut co = reg
            .checkout_resume(id, &Hole("h1"))
            .expect("the parked hole is still resumable after the panic");
        assert_eq!(co.machine().turns, 1);
        co.restore_suspended(Vec::new());
    }

    /// THE EPOCH GUARD — a stale checkout must not resurrect a session that
    /// was removed while it was still checked out.
    #[test]
    fn a_stale_checkout_cannot_resurrect_a_removed_entry() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = SessionId(20);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        let co = reg.checkout_run(id).expect("idle -> run");

        // The entry is removed while `co` is still outstanding (e.g. a
        // concurrent terminate/reset).
        reg.remove(id);
        assert!(reg.peek(id, |_| ()).is_none());

        // The stale checkout's restore must not bring it back.
        co.restore_suspended(Vec::new());
        assert!(
            reg.peek(id, |_| ()).is_none(),
            "a stale restore must not resurrect a removed entry"
        );
    }

    /// A stale checkout must not clobber a FRESH entry either, if the same id
    /// were ever reinstalled (never happens in practice — ids are minted
    /// monotonically — but the epoch guard does not rely on that).
    #[test]
    fn a_stale_checkout_cannot_clobber_a_reinstalled_entry() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = SessionId(21);
        reg.insert_idle(id, FakeMachine { turns: 1 });
        let stale = reg.checkout_run(id).expect("idle -> run");

        reg.remove(id);
        reg.insert_idle(id, FakeMachine { turns: 2 });

        stale.restore_suspended(Vec::new());
        let mut co = reg
            .checkout_run(id)
            .expect("the fresh entry is still checkoutable");
        assert_eq!(
            co.machine().turns,
            2,
            "the stale checkout must not have overwritten the fresh entry"
        );
        co.restore_suspended(Vec::new());
    }

    #[test]
    fn wedged_refuses_every_checkout_until_removed() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = SessionId(30);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        let co = reg.checkout_run(id).expect("idle -> run");
        co.mark_wedged(Instant::now());

        assert!(matches!(
            err(reg.checkout_run(id)),
            CheckoutError::Terminal { session, .. } if session == id
        ));
        assert_eq!(reg.label(id), Some("wedged (a turn timed out)".to_string()));

        reg.remove(id);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        assert!(is_idle(&reg, id), "reinstalling reclaims the slot");
    }

    #[test]
    fn single_slot_install_refuses_a_second_entry_and_checks_out_the_current_one() {
        let slot: SingleSlot<FakeMachine, Hole> = SingleSlot::new();
        assert!(matches!(err(slot.checkout_run()), CheckoutError::NoSession));

        slot.install(FakeMachine { turns: 0 })
            .expect("first install");
        assert!(
            slot.install(FakeMachine { turns: 9 }).is_err(),
            "second install refused"
        );

        let co = slot.checkout_run().expect("checkout the installed machine");
        co.restore_suspended(Vec::new());

        slot.remove();
        assert!(slot.current_id().is_none());
        assert!(matches!(err(slot.checkout_run()), CheckoutError::NoSession));
    }

    /// A stale turn from BEFORE a `remove`+reinstall must not clobber the
    /// fresh session `SingleSlot` now holds — the single-slot analogue of
    /// `a_stale_checkout_cannot_clobber_a_reinstalled_entry`, driven through
    /// the facade the way `tidepool-repl`'s `session_reset` actually does it.
    #[test]
    fn single_slot_stale_turn_cannot_clobber_a_session_installed_after_reset() {
        let slot: SingleSlot<FakeMachine, Hole> = SingleSlot::new();
        slot.install(FakeMachine { turns: 1 }).expect("install");
        let stale = slot.checkout_run().expect("checkout");

        slot.remove();
        slot.install(FakeMachine { turns: 2 })
            .expect("fresh install");

        stale.restore_suspended(Vec::new());
        let mut co = slot
            .checkout_run()
            .expect("fresh session still checkoutable");
        assert_eq!(co.machine().turns, 2);
        co.restore_suspended(Vec::new());
    }
}
