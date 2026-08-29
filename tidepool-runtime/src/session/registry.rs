//! Atomic ownership and lifecycle registry for resident machines.
//!
//! A session is idle, checked out and running, suspended with one or more
//! parked holes, or terminally wedged. Checkout moves the machine out under a
//! short lock; compilation and execution happen after the lock is released.
//! Settlement restores the machine together with the hole set reported by the
//! session itself.
//!
//! [`Checkout`] is the RAII proof of exclusive machine ownership. Each
//! checkout also carries the entry's monotonic epoch. If an entry is removed
//! or replaced while work is in flight, stale settlement drops the returned
//! machine instead of resurrecting or overwriting the newer entry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use tidepool_repr::SessionId;
use tokio::sync::Notify;

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
    /// A notification-driven checkout wait reached its caller-supplied bound.
    #[error("timed out after {waited:?} waiting to check out session {session}")]
    WaitTimeout {
        session: SessionId,
        waited: std::time::Duration,
    },
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

/// A [`Slot`]'s shape, without its payload — enough for a caller's own
/// admission policy (e.g. "refuse a fresh run while suspended", a policy the
/// shared `checkout_run` deliberately does NOT enforce since the keyed
/// harness registry treats a run over parked frames as ordinary) without
/// exposing the machine or hole set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    Idle,
    Running,
    Suspended,
    Wedged,
}

/// The structural checkout a caller wants notification-driven admission for.
#[derive(Debug, Clone, Copy)]
pub enum CheckoutRequest<'h, H> {
    Run,
    Resume(&'h H),
    Child,
}

impl<M, H> Slot<M, H> {
    pub fn kind(&self) -> SlotKind {
        match self {
            Slot::Idle(_) => SlotKind::Idle,
            Slot::Running { .. } => SlotKind::Running,
            Slot::Suspended { .. } => SlotKind::Suspended,
            Slot::Wedged { .. } => SlotKind::Wedged,
        }
    }
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
    availability: Notify,
}

impl<M, H> Default for SessionRegistry<M, H> {
    fn default() -> Self {
        SessionRegistry {
            slots: Mutex::new(HashMap::new()),
            next_epoch: AtomicU64::new(1),
            availability: Notify::new(),
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
        let previous = self
            .slots
            .lock()
            .insert(
                id,
                Entry {
                    epoch,
                    slot: Slot::Idle(machine),
                },
            )
            .map(|e| e.slot);
        self.availability.notify_waiters();
        previous
    }

    /// Remove a session entirely, returning its slot (drops the machine when
    /// the returned slot is dropped). The get-unstuck / teardown path. A
    /// checkout still outstanding for `id` finds its epoch stale on
    /// settlement and drops its machine instead of resurrecting this entry.
    pub fn remove(&self, id: SessionId) -> Option<Slot<M, H>> {
        let removed = self.slots.lock().remove(&id).map(|e| e.slot);
        if removed.is_some() {
            self.availability.notify_waiters();
        }
        removed
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

    /// This session's current [`SlotKind`], if it has an entry at all.
    pub fn kind(&self, id: SessionId) -> Option<SlotKind> {
        self.slots.lock().get(&id).map(|e| e.slot.kind())
    }

    /// The `since` timestamp of a `Wedged` entry — `None` if the session has
    /// no entry, or its entry is not `Wedged`.
    pub fn wedged_since(&self, id: SessionId) -> Option<Instant> {
        match self.slots.lock().get(&id) {
            Some(Entry {
                slot: Slot::Wedged { since },
                ..
            }) => Some(*since),
            _ => None,
        }
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

    /// Wait until `id`'s machine can be checked out for a run.
    ///
    /// Only [`CheckoutError::Running`] waits. Unknown, terminal, and other
    /// structural refusals return immediately. Settlement wakes one waiter;
    /// no polling interval or caller-maintained retry flag is involved.
    pub async fn checkout_wait(
        &self,
        id: SessionId,
        request: CheckoutRequest<'_, H>,
        max_wait: std::time::Duration,
    ) -> Result<Checkout<'_, M, H>, CheckoutError<H>> {
        let wait = async {
            loop {
                let available = self.availability.notified();
                let checkout = match request {
                    CheckoutRequest::Run => self.checkout_run(id),
                    CheckoutRequest::Resume(hole) => self.checkout_resume(id, hole),
                    CheckoutRequest::Child => self.checkout_child(id),
                };
                match checkout {
                    Err(CheckoutError::Running(_)) => available.await,
                    result => return result,
                }
            }
        };
        match tokio::time::timeout(max_wait, wait).await {
            Ok(result) => result,
            Err(_) => Err(CheckoutError::WaitTimeout {
                session: id,
                waited: max_wait,
            }),
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
        drop(slots);
        self.availability.notify_one();
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
        drop(slots);
        self.availability.notify_waiters();
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

    /// The parked holes this checkout found when it took the machine out
    /// (i.e. the slot's state BEFORE this checkout — empty means it was
    /// `Idle`). Read-only: a caller whose own admission policy is stricter
    /// than the registry's (e.g. "refuse a fresh run over a suspended
    /// session", which `checkout_run` itself does not enforce) reads this to
    /// decide whether to hand the machine straight back via
    /// [`Self::restore_suspended`] instead of running a turn on it.
    pub fn holes_at_checkout(&self) -> &[H] {
        &self.holes
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

    /// Split into the machine (to drive the turn on, e.g. move onto a
    /// blocking task) and an OWNED [`CheckoutReceipt`] carrying just enough
    /// (`session`, `epoch`) to settle LATER against a fresh registry borrow —
    /// for a caller whose settlement must run from a DIFFERENT async task
    /// than the one that checked out (a detached `tokio::spawn`, decoupled
    /// from the original request future, cannot hold a `Checkout<'r, ..>`
    /// whose `'r` is tied to that original call). Mirrors
    /// `tidepool-repl`'s pre-promotion `Checkout::into_parts`/
    /// `CheckoutCustody` split, now shared. Harness never needs this — its
    /// `run_checked_out` holds the borrowed `Checkout` across an `.await`
    /// within the SAME async fn instead.
    pub fn into_parts(mut self) -> (M, CheckoutReceipt) {
        #[allow(clippy::expect_used, reason = "machine present until settled")]
        let machine = self.machine.take().expect("machine present until settled");
        let receipt = CheckoutReceipt {
            id: self.id,
            epoch: Some(self.epoch),
        };
        (machine, receipt)
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

/// Owned, lifetime-free proof that a session machine is checked out.
///
/// [`Checkout::into_parts`] returns this when work must outlive the borrowed
/// checkout. It must be settled exactly once through the registry. A receipt
/// cannot restore on drop because it does not own the machine; an unconsumed
/// receipt is reported loudly in debug builds.
#[must_use = "a checkout receipt must be settled, or the session is left Running forever"]
pub struct CheckoutReceipt {
    id: SessionId,
    epoch: Option<u64>,
}

impl CheckoutReceipt {
    /// The session id this receipt is for.
    pub fn session_id(&self) -> SessionId {
        self.id
    }

    /// Consume the receipt and return its fencing epoch.
    fn into_epoch(mut self) -> u64 {
        #[allow(
            clippy::expect_used,
            reason = "CheckoutReceipt always holds an epoch until into_epoch consumes it"
        )]
        self.epoch
            .take()
            .expect("CheckoutReceipt always holds an epoch until into_epoch consumes it")
    }
}

impl Drop for CheckoutReceipt {
    fn drop(&mut self) {
        let Some(epoch) = self.epoch else { return };
        let detail = format!(
            "CheckoutReceipt dropped without being consumed — session {}'s epoch {epoch} \
             checkout was never settled (no settle_suspended/settle_wedged). The registry \
             entry this checkout came from is left stuck Running: no future checkout can \
             check the session back out.",
            self.id
        );
        // Preserve the original diagnosis if another failure is unwinding.
        if std::thread::panicking() {
            tracing::error!("{detail} (reported during an active unwind, so not raised)");
            return;
        }
        debug_assert!(false, "{}", detail);
    }
}

// A clone would permit two settlements for one checkout.
static_assertions::assert_not_impl_any!(CheckoutReceipt: Clone, Copy);

impl<M, H: Clone + PartialEq + std::fmt::Debug> SessionRegistry<M, H> {
    /// Settle an OWNED [`CheckoutReceipt`] (from [`Checkout::into_parts`])
    /// with its post-turn parked hole set — the SAME settlement as
    /// [`Checkout::restore_suspended`], reachable without the borrowed
    /// `Checkout` still in hand.
    pub fn settle_suspended(&self, receipt: CheckoutReceipt, machine: M, holes: Vec<H>) {
        let id = receipt.session_id();
        self.restore_suspended(id, receipt.into_epoch(), machine, holes);
    }

    /// Settle an OWNED [`CheckoutReceipt`] as `Wedged{since}` — see
    /// [`Checkout::mark_wedged`].
    pub fn settle_wedged(&self, receipt: CheckoutReceipt, since: Instant) {
        let id = receipt.session_id();
        self.mark_wedged(id, receipt.into_epoch(), since);
    }

    /// Settle an OWNED [`CheckoutReceipt`] by REMOVING the entry outright —
    /// for a caller with nothing worth restoring and no reason to keep a
    /// `Wedged` placeholder visible (e.g. an abort that unexpectedly
    /// re-suspended, with no caller left waiting on that hole). Same epoch
    /// guard as every other settlement: a stale receipt (the entry was
    /// already replaced or removed) removes nothing.
    pub fn settle_retire(&self, receipt: CheckoutReceipt) {
        let id = receipt.session_id();
        let epoch = receipt.into_epoch();
        let mut slots = self.slots.lock();
        if slots.get(&id).is_some_and(|e| e.epoch == epoch) {
            slots.remove(&id);
        }
        drop(slots);
        self.availability.notify_waiters();
    }
}

/// A [`SessionRegistry`] restricted to holding AT MOST one entry at a time —
/// `tidepool-repl`'s "one implicit session, no name" shape, built on the SAME
/// primitive the keyed (harness) registry uses rather than a second
/// implementation. The caller mints the [`SessionId`] passed to
/// [`Self::install`] (so it can stay the SAME id the caller's own
/// include-tree/session-config bookkeeping already uses — this facade does
/// not maintain a second counter); tracking which id is "current" means a
/// stale checkout against a replaced entry finds its epoch stale on
/// settlement (see the module doc) and drops its machine rather than
/// clobbering the fresh one.
pub struct SingleSlot<M, H> {
    registry: SessionRegistry<M, H>,
    current: Mutex<Option<SessionId>>,
}

impl<M, H> Default for SingleSlot<M, H> {
    fn default() -> Self {
        SingleSlot {
            registry: SessionRegistry::default(),
            current: Mutex::new(None),
        }
    }
}

impl<M, H: Clone + PartialEq + std::fmt::Debug> SingleSlot<M, H> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a freshly-opened machine as the current entry under `id`.
    /// Errors (handing the machine back) if one is already present — the
    /// caller lost an auto-open race and should drop this one and use the
    /// existing session.
    pub fn install(&self, id: SessionId, machine: M) -> Result<(), M> {
        let mut current = self.current.lock();
        if current.is_some() {
            return Err(machine);
        }
        self.registry.insert_idle(id, machine);
        *current = Some(id);
        Ok(())
    }

    /// The current entry's id, if one is installed.
    pub fn current_id(&self) -> Option<SessionId> {
        *self.current.lock()
    }

    /// The current entry's [`Slot::label`], if one is installed.
    pub fn label(&self) -> Option<String> {
        self.current_id().and_then(|id| self.registry.label(id))
    }

    /// The current entry's [`SlotKind`], if one is installed.
    pub fn kind(&self) -> Option<SlotKind> {
        self.current_id().and_then(|id| self.registry.kind(id))
    }

    /// The current entry's `Wedged` `since` timestamp — `None` unless the
    /// current entry is actually `Wedged`.
    pub fn wedged_since(&self) -> Option<Instant> {
        self.current_id()
            .and_then(|id| self.registry.wedged_since(id))
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

    /// [`SessionRegistry::settle_suspended`] — settle a [`CheckoutReceipt`]
    /// obtained from a checkout this facade produced.
    pub fn settle_suspended(&self, receipt: CheckoutReceipt, machine: M, holes: Vec<H>) {
        self.registry.settle_suspended(receipt, machine, holes);
    }

    /// [`SessionRegistry::settle_wedged`] — settle a [`CheckoutReceipt`] as
    /// `Wedged{since}`.
    pub fn settle_wedged(&self, receipt: CheckoutReceipt, since: Instant) {
        self.registry.settle_wedged(receipt, since);
    }

    /// [`SessionRegistry::settle_retire`] — settle a [`CheckoutReceipt`] by
    /// removing the entry outright. If the receipt's session is still the
    /// CURRENT one (nothing replaced it since checkout), clears `current`
    /// too, so the next `install` succeeds instead of finding a phantom
    /// entry a stale `current` still points at.
    pub fn settle_retire(&self, receipt: CheckoutReceipt) {
        let id = receipt.session_id();
        self.registry.settle_retire(receipt);
        let mut current = self.current.lock();
        if *current == Some(id) {
            *current = None;
        }
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

    /// A [`CheckoutReceipt`] settles the SAME entry a borrowed `Checkout`
    /// would, and honors the same epoch guard — the shape a detached
    /// `tokio::spawn`'d settlement (REPL's `drive`) actually uses.
    #[test]
    fn receipt_settles_like_a_checkout_and_respects_the_epoch_guard() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = SessionId(40);
        reg.insert_idle(id, FakeMachine { turns: 0 });

        let co = reg.checkout_run(id).expect("idle -> run");
        let (mut machine, receipt) = co.into_parts();
        assert_eq!(receipt.session_id(), id);
        machine.turns += 1;
        reg.settle_suspended(receipt, machine, vec![Hole("h1")]);

        let mut co = reg.checkout_resume(id, &Hole("h1")).expect("resume");
        assert_eq!(co.machine().turns, 1);
        let (machine, receipt) = co.into_parts();
        reg.settle_wedged(receipt, Instant::now());

        assert!(matches!(
            err(reg.checkout_run(id)),
            CheckoutError::Terminal { session, .. } if session == id
        ));
        let _ = machine;

        // A stale receipt from BEFORE a remove+reinstall must not resurrect
        // or clobber — same epoch guard as a borrowed `Checkout`.
        reg.remove(id);
        reg.insert_idle(id, FakeMachine { turns: 9 });
        let stale_co = reg.checkout_run(id).expect("fresh entry checkoutable");
        let (stale_machine, stale_receipt) = stale_co.into_parts();
        reg.remove(id);
        reg.insert_idle(id, FakeMachine { turns: 99 });
        reg.settle_suspended(stale_receipt, stale_machine, Vec::new());
        let mut co = reg.checkout_run(id).expect("the fresh entry survives");
        assert_eq!(co.machine().turns, 99);
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

    #[tokio::test]
    async fn waiting_checkout_wakes_when_the_running_owner_settles() {
        let reg: std::sync::Arc<SessionRegistry<FakeMachine, Hole>> =
            std::sync::Arc::new(SessionRegistry::new());
        let id = SessionId(31);
        reg.insert_idle(id, FakeMachine { turns: 7 });
        let running = reg.checkout_run(id).expect("first owner");

        let waiter_registry = std::sync::Arc::clone(&reg);
        let waiter = tokio::spawn(async move {
            let mut checkout = waiter_registry
                .checkout_wait(id, CheckoutRequest::Run, std::time::Duration::from_secs(1))
                .await
                .expect("settlement wakes the waiter");
            assert_eq!(checkout.machine().turns, 7);
            checkout.restore_suspended(Vec::new());
        });

        tokio::task::yield_now().await;
        running.restore_suspended(Vec::new());
        waiter.await.expect("waiter task");
    }

    #[tokio::test]
    async fn waiting_checkout_reports_its_timeout_distinctly() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = SessionId(32);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        let running = reg.checkout_run(id).expect("hold the machine");
        let waited = std::time::Duration::from_millis(1);

        let error = err(reg.checkout_wait(id, CheckoutRequest::Run, waited).await);
        assert_eq!(
            error,
            CheckoutError::WaitTimeout {
                session: id,
                waited
            }
        );
        running.restore_suspended(Vec::new());
    }

    #[test]
    fn single_slot_install_refuses_a_second_entry_and_checks_out_the_current_one() {
        let slot: SingleSlot<FakeMachine, Hole> = SingleSlot::new();
        assert!(matches!(err(slot.checkout_run()), CheckoutError::NoSession));

        slot.install(SessionId(1), FakeMachine { turns: 0 })
            .expect("first install");
        assert!(
            slot.install(SessionId(2), FakeMachine { turns: 9 })
                .is_err(),
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
        slot.install(SessionId(1), FakeMachine { turns: 1 })
            .expect("install");
        let stale = slot.checkout_run().expect("checkout");

        slot.remove();
        slot.install(SessionId(2), FakeMachine { turns: 2 })
            .expect("fresh install");

        stale.restore_suspended(Vec::new());
        let mut co = slot
            .checkout_run()
            .expect("fresh session still checkoutable");
        assert_eq!(co.machine().turns, 2);
        co.restore_suspended(Vec::new());
    }
}
